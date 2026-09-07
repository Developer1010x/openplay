use std::net::SocketAddr;
// Only the Wi-Fi Direct path listens for an inbound sink, warns, or has a P2P
// group to hand to a Drop guard.
#[cfg(target_os = "linux")]
use std::sync::Arc;
use std::time::Duration;

#[cfg(target_os = "linux")]
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
#[cfg(target_os = "linux")]
use tracing::warn;
use tracing::{error, info};

use crate::rtsp_server;
use crate::wfd_params::WfdVideoFormats;
#[cfg(target_os = "linux")]
use crate::wifi_direct::{self, WifiDirectEvent, WifiDirectManager};
use crate::MiracastError;

/// How long to wait for the sink to accept the RTSP connection.
const SINK_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

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

    /// Stops the session, ending the cast.
    ///
    /// The session task holds the RTSP control connection open for as long as
    /// the cast lasts, so it never ends on its own: this — or dropping the
    /// session, which calls it — is how the sender ends one. Both make the task
    /// emit [`SessionEvent::Ended`] and let go of the socket, which is the sink's
    /// signal that the session is over.
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
    ///
    /// Only available on Linux, where peer discovery and P2P group formation go
    /// through wpa_supplicant over D-Bus. See [`crate::wifi_direct`].
    #[cfg(target_os = "linux")]
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

    // A wrong address on a network that drops rather than refuses leaves the
    // kernel retrying for minutes, and the UI says "Connecting" for all of them.
    // Ten seconds is a long time to reach a device on the same LAN.
    let stream = tokio::time::timeout(SINK_CONNECT_TIMEOUT, TcpStream::connect(sink_addr))
        .await
        .map_err(|_| {
            MiracastError::Connection(format!(
                "No answer from {sink_addr} within {SINK_CONNECT_TIMEOUT:?}"
            ))
        })?
        .map_err(|e| MiracastError::Connection(format!("Failed to connect to {sink_addr}: {e}")))?;

    let source_formats = WfdVideoFormats::default();

    let mut conn = rtsp_server::RtspConnection::new(stream);
    let result = rtsp_server::negotiate(&mut conn, sink_addr, &source_formats).await?;

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

    // The caller starts its GStreamer pipeline (mpegtsmux → rtpmp2tpay →
    // udpsink) on that Ready and streams over UDP, which tells the sink nothing
    // about whether we are still here. The session lives on this RTSP
    // connection, so the cast lasts exactly as long as the call below: it pings
    // the sink, answers what the sink asks, and returns only when the sink ends
    // the session or the connection breaks.
    //
    // Anything else here ends the cast within milliseconds. Sending Ended is the
    // obvious way, but so is simply returning: that drops `conn` (the sink sees
    // a FIN on a session it was promised timeout=30 on) and drops `evt_tx`, and
    // a closed channel reads as `None` in run_miracast_pipeline's select, which
    // breaks the loop exactly like an Ended would.
    rtsp_server::serve_control_channel(&mut conn).await?;

    info!("Miracast session ended by the sink");
    let _ = evt_tx.send(SessionEvent::Ended(None)).await;

    Ok(())
}

/// Run a Miracast session over Wi-Fi Direct P2P.
///
/// Uses wpa_supplicant D-Bus directly (not NetworkManager) with GO intent=0
/// (prefer client role, like miraclecast). After P2P group forms, we listen
/// on port 7236 for the sink to connect (WFD spec: source is the RTSP server).
#[cfg(target_os = "linux")]
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
    // Shared so P2PGroupGuard can still reach it from a Drop that outlives this
    // function's stack.
    let manager = Arc::new(manager);

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

    // From here on there is a group to clean up, whichever way this ends.
    let _group_guard = P2PGroupGuard {
        manager: manager.clone(),
    };

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
    let mut conn = rtsp_server::RtspConnection::new(stream);
    let result = rtsp_server::negotiate(&mut conn, sink_addr, &source_formats).await?;

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

    // Step 8: hold the control connection open for the life of the cast. See
    // run_session — returning from here, by any route, ends the cast.
    rtsp_server::serve_control_channel(&mut conn).await?;

    info!("Miracast (Wi-Fi Direct) session ended by the sink");
    let _ = evt_tx.send(SessionEvent::Ended(None)).await;

    // The P2P group comes down in P2PGroupGuard::drop, which runs on every exit
    // from here — including the cancellation that stop() causes.
    Ok(())
}

/// Tears the P2P group down when the Wi-Fi Direct session ends, however it ends.
///
/// The common ending is cancellation: `stop()` fires the shutdown branch of the
/// select in [`MiracastSession::start_wifi_direct`], which drops the session
/// future wherever it is parked — normally in the control channel, ahead of any
/// cleanup line written after it. A group left standing keeps the sink attached
/// to a source that has stopped sending, and the next cast has to fight it.
#[cfg(target_os = "linux")]
struct P2PGroupGuard {
    manager: Arc<WifiDirectManager>,
}

#[cfg(target_os = "linux")]
impl Drop for P2PGroupGuard {
    fn drop(&mut self) {
        // Drop cannot await and the teardown is a D-Bus round trip, so it goes
        // to the runtime instead. If there is no runtime left the group stays up
        // until wpa_supplicant is told otherwise, which is worth a line in the
        // log rather than a panic out of a destructor.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!("No async runtime left to tear the P2P group down on");
            return;
        };
        let manager = self.manager.clone();
        handle.spawn(async move {
            if let Err(e) = manager.disconnect().await {
                warn!(%e, "P2P group teardown failed");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtsp_server::test_sink::{run_negotiation, FakePeer};
    use std::time::Duration as StdDuration;
    use tokio::net::TcpListener;

    /// Binds a loopback listener and plays the sink's half of M1–M7 on the first
    /// connection it gets.
    ///
    /// The handle yields the sink connection *still open* once PLAY is answered,
    /// because that is the state the whole cast happens in.
    async fn fake_sink() -> (SocketAddr, tokio::task::JoinHandle<FakePeer<TcpStream>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut peer = FakePeer::new(stream);
            run_negotiation(&mut peer, "19000-19001").await;
            peer
        });
        (addr, task)
    }

    async fn expect_ready(session: &mut MiracastSession) -> u16 {
        match tokio::time::timeout(StdDuration::from_secs(5), session.events().recv()).await {
            Ok(Some(SessionEvent::Ready { rtp_port, .. })) => rtp_port,
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    async fn next_event(session: &mut MiracastSession) -> Option<SessionEvent> {
        tokio::time::timeout(StdDuration::from_secs(5), session.events().recv())
            .await
            .expect("session should have reported an event")
    }

    #[tokio::test]
    async fn session_stays_alive_after_ready() {
        let (addr, sink) = fake_sink().await;
        let mut session = MiracastSession::start(addr).await.unwrap();

        assert_eq!(expect_ready(&mut session).await, 19000);
        let _peer = sink.await.unwrap();

        // The regression this guards is the whole cast: an Ended queued straight
        // behind Ready, or the session task returning and dropping evt_tx, both
        // stop run_miracast_pipeline on its first loop iteration, milliseconds
        // after the pipeline starts.
        let next =
            tokio::time::timeout(StdDuration::from_millis(300), session.events().recv()).await;
        assert!(
            next.is_err(),
            "the session ended itself right after Ready: {next:?}"
        );
    }

    #[tokio::test]
    async fn stop_ends_the_session_and_closes_the_control_connection() {
        let (addr, sink) = fake_sink().await;
        let mut session = MiracastSession::start(addr).await.unwrap();
        expect_ready(&mut session).await;
        let mut peer = sink.await.unwrap();

        session.stop();

        assert!(
            matches!(
                next_event(&mut session).await,
                Some(SessionEvent::Ended(None))
            ),
            "stop() should end the session cleanly"
        );
        tokio::time::timeout(StdDuration::from_secs(5), peer.wait_for_close())
            .await
            .expect("the source should let go of the control connection on stop");
    }

    #[tokio::test]
    async fn a_sink_teardown_ends_the_session() {
        let (addr, sink) = fake_sink().await;
        let mut session = MiracastSession::start(addr).await.unwrap();
        expect_ready(&mut session).await;
        let mut peer = sink.await.unwrap();

        peer.send("TEARDOWN rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: 9\r\nSession: 1\r\n\r\n")
            .await;
        assert!(peer.recv().await.starts_with("RTSP/1.0 200"));

        assert!(matches!(
            next_event(&mut session).await,
            Some(SessionEvent::Ended(None))
        ));
    }

    #[tokio::test]
    async fn a_sink_that_vanishes_ends_the_session_with_an_error() {
        let (addr, sink) = fake_sink().await;
        let mut session = MiracastSession::start(addr).await.unwrap();
        expect_ready(&mut session).await;
        drop(sink.await.unwrap());

        match next_event(&mut session).await {
            Some(SessionEvent::Ended(Some(e))) => {
                assert!(e.to_string().contains("Connection closed"), "{e}");
            }
            other => panic!("expected Ended(Some(_)), got {other:?}"),
        }
    }
}
