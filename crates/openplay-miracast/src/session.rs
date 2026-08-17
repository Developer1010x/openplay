use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::rtsp_server;
use crate::wfd_params::WfdVideoFormats;
use crate::wifi_direct::{self, WifiDirectEvent, WifiDirectManager};
use crate::MiracastError;

/// Events emitted by a Miracast session.
#[derive(Debug)]
pub enum SessionEvent {
    /// Negotiation complete, ready to stream.
    Ready {
        width: u32,
        height: u32,
        fps: u32,
        rtp_port: u16,
        sink_addr: SocketAddr,
    },
    /// Session ended.
    Ended(Option<MiracastError>),
}

/// Orchestrates a Miracast (WFD) casting session.
///
/// For MICE (Miracast over Infrastructure), the user provides the sink IP.
/// We connect via TCP for RTSP negotiation (M1-M7), then start
/// streaming MPEG2-TS over RTP/UDP.
pub struct MiracastSession {
    event_rx: mpsc::Receiver<SessionEvent>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl MiracastSession {
    /// Starts a Miracast session by connecting to the sink via MICE (infrastructure).
    ///
    /// # Arguments
    /// * `sink_addr` - Sink's RTSP address (IP:7236)
    pub async fn start(sink_addr: SocketAddr) -> Result<Self, MiracastError> {
        let (evt_tx, evt_rx) = mpsc::channel(16);
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            tokio::select! {
                result = run_session(sink_addr, evt_tx.clone()) => {
                    if let Err(e) = result {
                        error!(%e, "Miracast session error");
                        let _ = evt_tx.send(SessionEvent::Ended(Some(e))).await;
                    }
                }
                _ = &mut shutdown_rx => {
                    info!("Miracast session shutdown requested");
                    let _ = evt_tx.send(SessionEvent::Ended(None)).await;
                }
            }
        });

        Ok(Self {
            event_rx: evt_rx,
            shutdown_tx: Some(shutdown_tx),
        })
    }

    /// Returns the event receiver for session lifecycle events.
    pub fn events(&mut self) -> &mut mpsc::Receiver<SessionEvent> {
        &mut self.event_rx
    }

    /// Stops the session.
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }

    /// Starts a Miracast session via Wi-Fi Direct P2P connection.
    ///
    /// This discovers the peer, forms a P2P group, resolves the peer IP,
    /// then proceeds with RTSP negotiation.
    ///
    /// # Arguments
    /// * `peer_address` - Wi-Fi Direct device address (MAC) of the sink
    /// * `rtsp_port` - RTSP port (default 7236)
    pub async fn start_wifi_direct(
        peer_address: &str,
        rtsp_port: u16,
    ) -> Result<Self, MiracastError> {
        let (evt_tx, evt_rx) = mpsc::channel(16);
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();

        let peer_addr = peer_address.to_string();

        tokio::spawn(async move {
            tokio::select! {
                result = run_wifi_direct_session(&peer_addr, rtsp_port, evt_tx.clone()) => {
                    if let Err(e) = result {
                        error!(%e, "Miracast Wi-Fi Direct session error");
                        let _ = evt_tx.send(SessionEvent::Ended(Some(e))).await;
                    }
                }
                _ = &mut shutdown_rx => {
                    info!("Miracast Wi-Fi Direct session shutdown requested");
                    let _ = evt_tx.send(SessionEvent::Ended(None)).await;
                }
            }
        });

        Ok(Self {
            event_rx: evt_rx,
            shutdown_tx: Some(shutdown_tx),
        })
    }
}

impl Drop for MiracastSession {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn run_session(
    sink_addr: SocketAddr,
    evt_tx: mpsc::Sender<SessionEvent>,
) -> Result<(), MiracastError> {
    info!(%sink_addr, "Connecting to Miracast sink");

    let stream = TcpStream::connect(sink_addr)
        .await
        .map_err(|e| MiracastError::Connection(format!("Failed to connect to {sink_addr}: {e}")))?;

    let source_formats = WfdVideoFormats::default();

    let result = rtsp_server::negotiate(stream, sink_addr, &source_formats).await?;

    info!(
        width = result.width,
        height = result.height,
        fps = result.fps,
        rtp_port = result.rtp_port,
        "Miracast negotiation complete"
    );

    let _ = evt_tx
        .send(SessionEvent::Ready {
            width: result.width,
            height: result.height,
            fps: result.fps,
            rtp_port: result.rtp_port,
            sink_addr: result.sink_addr,
        })
        .await;

    // The GStreamer pipeline (mpegtsmux → rtpmp2tpay → udpsink) will be started
    // by the caller after receiving the Ready event with the negotiated params.
    // Keep session alive until shutdown.
    let _ = evt_tx.send(SessionEvent::Ended(None)).await;

    Ok(())
}

/// Run a Miracast session over Wi-Fi Direct P2P.
///
/// Uses wpa_supplicant D-Bus directly (not NetworkManager) with GO intent=0
/// (prefer client role, like miraclecast). After P2P group forms, we listen
/// on port 7236 for the sink to connect (WFD spec: source is the RTSP server).
async fn run_wifi_direct_session(
    peer_address: &str,
    rtsp_port: u16,
    evt_tx: mpsc::Sender<SessionEvent>,
) -> Result<(), MiracastError> {
    info!(peer = %peer_address, "Starting Wi-Fi Direct Miracast session");

    // Step 1: Start WifiDirectManager with P2P Find active (needed for Connect)
    let (manager, mut wfd_events) = WifiDirectManager::start_for_connect()
        .await
        .map_err(|e| MiracastError::Connection(format!("Failed to start P2P manager: {e}")))?;

    // Step 2: Wait a few seconds for the peer to be discovered
    info!("Waiting for peer discovery...");
    let discovery_timeout = Duration::from_secs(15);
    let discovery_start = tokio::time::Instant::now();
    let mut peer_found = false;

    while discovery_start.elapsed() < discovery_timeout {
        tokio::select! {
            Some(event) = wfd_events.recv() => {
                match event {
                    WifiDirectEvent::PeerFound(peer) => {
                        if peer.device_address.eq_ignore_ascii_case(peer_address) {
                            info!(name = %peer.name, wfd = peer.wfd_supported, "Target peer discovered");
                            peer_found = true;
                            break;
                        }
                    }
                    WifiDirectEvent::Error(e) => {
                        warn!(error = %e, "P2P discovery error");
                    }
                    _ => {}
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }

    if !peer_found {
        info!("Peer not seen in discovery, attempting connect anyway (may already be cached)");
    }

    // Step 3: Initiate P2P Connect with GO intent=0 (prefer client role)
    info!(peer = %peer_address, "Initiating P2P Connect (go_intent=0, pbc)");
    manager
        .connect(peer_address)
        .await
        .map_err(|e| MiracastError::Connection(format!("P2P Connect failed: {e}")))?;

    // Step 4: Wait for P2P group formation
    info!("Waiting for P2P group formation...");
    let group_timeout = Duration::from_secs(60);
    let group_start = tokio::time::Instant::now();
    let mut group_info = None;

    while group_start.elapsed() < group_timeout {
        match tokio::time::timeout(Duration::from_secs(5), wfd_events.recv()).await {
            Ok(Some(event)) => match event {
                WifiDirectEvent::GroupFormed(info) => {
                    info!(
                        iface = %info.interface_name,
                        peer_ip = %info.peer_ip,
                        "P2P group formed!"
                    );
                    group_info = Some(info);
                    break;
                }
                WifiDirectEvent::Error(e) => {
                    error!(error = %e, "P2P group formation error");
                    manager.disconnect().await.ok();
                    return Err(MiracastError::Connection(format!(
                        "P2P group formation failed: {e}"
                    )));
                }
                WifiDirectEvent::GroupRemoved => {
                    error!("P2P group removed unexpectedly");
                    return Err(MiracastError::Connection(
                        "P2P group removed before session started".to_string(),
                    ));
                }
                other => {
                    info!(event = ?other, "P2P event during group formation");
                }
            },
            Ok(None) => {
                error!("P2P event channel closed");
                return Err(MiracastError::Connection(
                    "P2P event channel closed".to_string(),
                ));
            }
            Err(_) => {
                // Timeout on this recv, keep waiting
                info!(
                    elapsed_s = group_start.elapsed().as_secs(),
                    "Still waiting for P2P group..."
                );
            }
        }
    }

    let group = group_info.ok_or_else(|| {
        MiracastError::Connection("Timeout (60s) waiting for P2P group formation".to_string())
    })?;

    // Step 5: Resolve peer IP address
    // If the GroupStarted signal didn't include the IP, resolve via ARP/DHCP
    let peer_ip = if group.peer_ip.is_unspecified() {
        info!(iface = %group.interface_name, "Resolving peer IP via ARP...");
        // Wait for DHCP to assign addresses first
        tokio::time::sleep(Duration::from_secs(3)).await;

        wifi_direct::resolve_peer_ip(&group.interface_name, Duration::from_secs(30))
            .await
            .map_err(|e| MiracastError::Connection(format!("Failed to resolve peer IP: {e}")))?
    } else {
        group.peer_ip
    };

    info!(peer_ip = %peer_ip, "Peer IP resolved");

    // Step 6: Start RTSP server and wait for sink connection
    // WFD spec: the SOURCE is the RTSP server, the SINK connects to us on port 7236
    let bind_addr = format!("0.0.0.0:{rtsp_port}");
    info!(bind = %bind_addr, "Starting RTSP server, waiting for sink...");

    let listener = TcpListener::bind(&bind_addr).await.map_err(|e| {
        MiracastError::Connection(format!("RTSP listen on {bind_addr} failed: {e}"))
    })?;

    // Wait for sink to connect, with fallback to connect to sink
    let (stream, sink_tcp_addr) = tokio::select! {
        result = async {
            tokio::time::timeout(Duration::from_secs(30), listener.accept()).await
        } => {
            match result {
                Ok(Ok((stream, addr))) => {
                    info!(sink = %addr, "Sink connected to our RTSP server");
                    Ok((stream, addr))
                }
                Ok(Err(e)) => Err(MiracastError::Connection(format!("Accept failed: {e}"))),
                Err(_) => {
                    // Timeout — try connecting to the sink as fallback
                    info!(peer_ip = %peer_ip, "No incoming connection after 30s, connecting to sink");
                    let sink_addr = SocketAddr::new(peer_ip, rtsp_port);
                    let stream = TcpStream::connect(sink_addr).await
                        .map_err(|e| MiracastError::Connection(
                            format!("Both listen and connect failed. Connect error: {e}")
                        ))?;
                    info!(sink = %sink_addr, "Connected to sink (fallback)");
                    Ok((stream, sink_addr))
                }
            }
        }
    }?;

    let sink_addr = SocketAddr::new(sink_tcp_addr.ip(), rtsp_port);

    // Step 7: RTSP negotiation (M1-M7)
    info!("Starting WFD RTSP negotiation (M1-M7)");
    let source_formats = WfdVideoFormats::default();
    let result = rtsp_server::negotiate(stream, sink_addr, &source_formats).await?;

    info!(
        width = result.width,
        height = result.height,
        fps = result.fps,
        rtp_port = result.rtp_port,
        "Miracast (Wi-Fi Direct) negotiation complete"
    );

    let _ = evt_tx
        .send(SessionEvent::Ready {
            width: result.width,
            height: result.height,
            fps: result.fps,
            rtp_port: result.rtp_port,
            sink_addr: result.sink_addr,
        })
        .await;

    // Keep alive until shutdown
    let _ = evt_tx.send(SessionEvent::Ended(None)).await;

    // Cleanup: disconnect P2P group
    manager.disconnect().await.ok();

    Ok(())
}
