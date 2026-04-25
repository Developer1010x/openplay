use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{interval, Duration};
use tracing::{error, info, warn};

use crate::hap_pairing;
use crate::http_session;
use crate::mirror_stream::MirrorStream;
use crate::ntp::NtpServer;
use crate::AirPlayError;
use openplay_common::AIRPLAY_NTP_PORT;

/// Commands that can be sent to an active AirPlay session.
#[derive(Debug)]
pub enum SessionCommand {
    /// Send codec configuration (SPS/PPS).
    SendCodecData(Vec<u8>),
    /// Send an H.264 video frame.
    SendVideoFrame(Vec<u8>),
    /// Stop the session.
    Stop,
}

/// Events emitted by an AirPlay session.
#[derive(Debug)]
pub enum SessionEvent {
    /// Session is connected and ready for frames.
    Ready,
    /// Session ended (normally or with error).
    Ended(Option<AirPlayError>),
}

/// Orchestrates the full AirPlay mirroring session lifecycle:
/// 1. Start NTP server
/// 2. HTTP negotiate (GET /info → POST /stream)
/// 3. Stream H.264 frames with heartbeats
pub struct AirPlaySession {
    command_tx: mpsc::Sender<SessionCommand>,
    event_rx: mpsc::Receiver<SessionEvent>,
}

impl AirPlaySession {
    /// Starts a new AirPlay session to the given receiver.
    ///
    /// # Arguments
    /// * `receiver_addr` - Address of the AirPlay receiver (IP:port)
    /// * `width` - Video width
    /// * `height` - Video height
    /// * `fps` - Target framerate
    pub async fn start(
        receiver_addr: SocketAddr,
        width: u32,
        height: u32,
        fps: u32,
    ) -> Result<Self, AirPlayError> {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (evt_tx, evt_rx) = mpsc::channel(16);

        let session_id = uuid::Uuid::new_v4().to_string();

        tokio::spawn(async move {
            if let Err(e) =
                run_session(receiver_addr, width, height, fps, session_id, cmd_rx, evt_tx.clone())
                    .await
            {
                error!(%e, "AirPlay session error");
                let _ = evt_tx.send(SessionEvent::Ended(Some(e))).await;
            }
        });

        Ok(Self {
            command_tx: cmd_tx,
            event_rx: evt_rx,
        })
    }

    /// Sends a command to the session.
    pub async fn send(&self, cmd: SessionCommand) -> Result<(), AirPlayError> {
        self.command_tx
            .send(cmd)
            .await
            .map_err(|_| AirPlayError::SessionClosed)
    }

    /// Sends codec configuration data (SPS/PPS).
    pub async fn send_codec_data(&self, data: Vec<u8>) -> Result<(), AirPlayError> {
        self.send(SessionCommand::SendCodecData(data)).await
    }

    /// Sends an H.264 video frame.
    pub async fn send_video_frame(&self, data: Vec<u8>) -> Result<(), AirPlayError> {
        self.send(SessionCommand::SendVideoFrame(data)).await
    }

    /// Stops the session.
    pub async fn stop(&self) -> Result<(), AirPlayError> {
        self.send(SessionCommand::Stop).await
    }

    /// Returns the event receiver for session lifecycle events.
    pub fn events(&mut self) -> &mut mpsc::Receiver<SessionEvent> {
        &mut self.event_rx
    }
}

async fn run_session(
    receiver_addr: SocketAddr,
    width: u32,
    height: u32,
    fps: u32,
    session_id: String,
    mut cmd_rx: mpsc::Receiver<SessionCommand>,
    evt_tx: mpsc::Sender<SessionEvent>,
) -> Result<(), AirPlayError> {
    // Step 1: Start NTP server
    let mut ntp_server = NtpServer::start(AIRPLAY_NTP_PORT).await?;
    info!("NTP server running on port {}", AIRPLAY_NTP_PORT);

    // Step 2: HTTP negotiate — try basic first, then authenticated if needed
    let negotiated = match http_session::negotiate(receiver_addr, width, height, fps, &session_id).await {
        Ok(n) => {
            info!("AirPlay negotiation complete (no auth required)");
            n
        }
        Err(AirPlayError::Negotiation(ref msg)) if msg.contains("501") || msg.contains("403") => {
            // Server requires authentication — try HAP pairing
            info!("Receiver requires authentication, attempting HAP pairing");
            negotiate_with_auth(receiver_addr, width, height, fps, &session_id).await?
        }
        Err(e) => return Err(e),
    };

    // Step 3: Create mirror stream (optionally with FairPlay encryption)
    let mirror_stream = Arc::new(Mutex::new(MirrorStream::new(negotiated.stream)));

    // Notify ready
    let _ = evt_tx.send(SessionEvent::Ready).await;

    // Start heartbeat task
    let heartbeat_stream = mirror_stream.clone();
    let heartbeat_handle = tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(2));
        loop {
            ticker.tick().await;
            let mut stream = heartbeat_stream.lock().await;
            if let Err(e) = stream.send_heartbeat().await {
                warn!("Heartbeat failed: {e}");
                break;
            }
        }
    });

    // Process commands
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            SessionCommand::SendCodecData(data) => {
                let mut stream = mirror_stream.lock().await;
                stream.send_codec_data(&data).await?;
            }
            SessionCommand::SendVideoFrame(data) => {
                let mut stream = mirror_stream.lock().await;
                stream.send_video_frame(&data).await?;
            }
            SessionCommand::Stop => {
                info!("AirPlay session stop requested");
                break;
            }
        }
    }

    // Cleanup
    heartbeat_handle.abort();
    ntp_server.stop();
    let _ = evt_tx.send(SessionEvent::Ended(None)).await;
    info!("AirPlay session ended");

    Ok(())
}

/// Negotiate AirPlay connection with authentication.
///
/// Strategy:
/// 1. GET /info to identify the device
/// 2. If Apple TV 3rd gen (AppleTV3,x) → error (requires proprietary FairPlay)
/// 3. If device supports transient pairing → HAP transient pair-setup + pair-verify
/// 4. POST /stream on the verified connection
async fn negotiate_with_auth(
    receiver_addr: SocketAddr,
    width: u32,
    height: u32,
    fps: u32,
    session_id: &str,
) -> Result<http_session::NegotiatedStream, AirPlayError> {
    // Step 1: Reconnect and GET /info to check device type (with proper AirPlay headers).
    let mut stream = TcpStream::connect(receiver_addr)
        .await
        .map_err(|e| AirPlayError::Connection(format!("Failed to connect: {e}")))?;

    let (_headers, body) = http_session::get_info_raw(&mut stream, session_id).await?;
    let server_info = http_session::parse_info_response_pub(&body)?;
    drop(stream); // Close the info connection

    let model = &server_info.model;
    info!(model = %model, features = server_info.features.raw(), "Checking auth strategy");

    // Step 2: Check if this is an Apple TV 3rd gen (FairPlay-only, not supported)
    if model.starts_with("AppleTV3") || model.starts_with("AppleTV2") {
        return Err(AirPlayError::Negotiation(format!(
            "{model} requires FairPlay authentication which is not supported. \
             Use a newer Apple TV (4th gen+) or an AirPlay-compatible smart TV instead."
        )));
    }

    // Step 3: Try HAP transient pair-setup (no PIN, for AirPlay 2 devices)
    info!("Attempting HAP transient pairing (no PIN)");
    let pair_result = hap_pairing::pair_setup_transient(receiver_addr).await
        .map_err(|e| AirPlayError::Pairing(format!("Transient pairing failed: {e}")))?;

    info!("Transient pair-setup succeeded, starting pair-verify");

    // Step 4: pair-verify to establish encrypted session
    let paired_device = hap_pairing::PairedDevice {
        device_id: pair_result.accessory_id.clone(),
        accessory_ltpk: pair_result.accessory_ltpk,
        client_ltsk: pair_result.client_ltsk,
        client_ltpk: pair_result.client_ltpk,
    };

    let (mut verified_stream, _verify_result) = hap_pairing::pair_verify(receiver_addr, &paired_device).await
        .map_err(|e| AirPlayError::Pairing(format!("Pair-verify failed: {e}")))?;

    info!("Pair-verify succeeded, sending POST /stream");

    // Step 5: POST /stream on the verified connection (with proper AirPlay headers)
    http_session::post_stream_on(&mut verified_stream, width, height, fps, session_id).await?;
    info!("POST /stream accepted after HAP pairing — mirror stream active");

    Ok(http_session::NegotiatedStream {
        stream: verified_stream,
        server_info,
    })
}

/// Simple HTTP response reader for the FairPlay flow.
async fn read_http_response_simple(stream: &mut TcpStream) -> Result<(String, Vec<u8>), AirPlayError> {
    let mut buf = vec![0u8; 8192];
    let mut total = 0;

    loop {
        let n = stream.read(&mut buf[total..]).await
            .map_err(|e| AirPlayError::Http(format!("Read failed: {e}")))?;
        if n == 0 {
            return Err(AirPlayError::Http("Connection closed".to_string()));
        }
        total += n;

        if let Some(pos) = buf[..total].windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&buf[..pos]).to_string();
            let body_start = pos + 4;

            let content_length = headers.lines()
                .find_map(|line| {
                    line.strip_prefix("Content-Length:")
                        .or_else(|| line.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);

            let mut body = buf[body_start..total].to_vec();
            while body.len() < content_length {
                let mut tmp = vec![0u8; content_length - body.len()];
                let n = stream.read(&mut tmp).await
                    .map_err(|e| AirPlayError::Http(format!("Read body failed: {e}")))?;
                if n == 0 { break; }
                body.extend_from_slice(&tmp[..n]);
            }

            return Ok((headers, body));
        }

        if total >= buf.len() {
            buf.resize(buf.len() * 2, 0);
        }
    }
}
