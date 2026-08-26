use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{interval, Duration};
use tracing::{error, info, warn};

use crate::control_channel;
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
            if let Err(e) = run_session(
                receiver_addr,
                width,
                height,
                fps,
                session_id,
                cmd_rx,
                evt_tx.clone(),
            )
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
    let negotiated = match http_session::negotiate(receiver_addr, width, height, fps, &session_id)
        .await
    {
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
    let session = hap_pairing::pair_setup_transient(receiver_addr)
        .await
        .map_err(|e| AirPlayError::Pairing(format!("Transient pairing failed: {e}")))?;

    info!("Transient pair-setup succeeded");

    // Step 4: everything after M4 is encrypted. There is no pair-verify in the
    // transient flow — that belongs to PIN pairing, which exchanges long-term
    // identities in M5/M6. Transient has none; the SRP session key keys the
    // channel directly, and only on the connection it was negotiated over.
    let mut control = control_channel::ControlChannel::new(session.stream, &session.session_key)
        .map_err(|e| AirPlayError::Pairing(format!("Control channel setup failed: {e}")))?;

    info!("Encrypted control channel established, sending POST /stream");

    let request = http_session::build_stream_request(width, height, fps, session_id)?;
    let response = control
        .request(&request)
        .await
        .map_err(|e| AirPlayError::Negotiation(format!("Encrypted POST /stream failed: {e}")))?;

    let status = String::from_utf8_lossy(&response);
    let status_line = status.lines().next().unwrap_or("<no status line>");
    if !status_line.contains("200") {
        return Err(AirPlayError::Negotiation(format!(
            "POST /stream over the encrypted channel returned: {status_line}"
        )));
    }

    info!("POST /stream accepted over the encrypted control channel");

    // Step 5: and here the implemented path ends. `MirrorStream` writes NAL
    // units straight to a `TcpStream`, but every byte on this connection must
    // now be wrapped in control-channel frames, so handing it the raw socket
    // would emit plaintext into an encrypted stream and the receiver would drop
    // the connection. Making the mirror stream encryption-aware is the
    // remaining work; failing here with an explicit message beats returning a
    // connection that cannot carry video.
    let _ = server_info;
    Err(AirPlayError::Negotiation(
        "Transient pairing and the encrypted control channel now succeed, but the \
         mirror stream cannot yet send video over an encrypted connection. See the \
         AirPlay status in docs/crypto.md."
            .to_string(),
    ))
}
