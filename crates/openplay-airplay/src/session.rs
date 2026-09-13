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
    let negotiated =
        match http_session::negotiate(receiver_addr, width, height, fps, &session_id).await {
            Ok(n) => {
                info!("AirPlay negotiation complete (no auth required)");
                n
            }
            Err(AirPlayError::Negotiation(ref msg)) if wants_authentication(msg) => {
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

/// Whether an unauthenticated `POST /stream` failure is worth retrying behind
/// pairing.
///
/// This was `msg.contains("501") || msg.contains("403")`, which missed the
/// statuses real receivers actually answer with. A Mac with AirPlay Receiver
/// set to *Everyone* and no password returns **404** — the legacy AirPlay 1
/// endpoint simply does not exist there — and one with a password returns
/// **470**. Neither matched, so casting gave up without ever attempting to
/// pair, while `pair_probe` reached M4 happily by calling pair-setup directly.
///
/// The code is read from the **status line only**. Substring-matching the whole
/// message cannot work: `AirPlayError::Negotiation` carries the entire header
/// block, so `Content-Length: 1401` in a permanent `500` would be read as a
/// `401` and provoke a full SRP-6a pair-setup against a receiver that was never
/// going to authenticate. `Server: AirTunes/470.x` does the same. Anything past
/// the first line is a header, and headers are full of digits.
fn wants_authentication(msg: &str) -> bool {
    matches!(status_code(msg), Some(401 | 403 | 404 | 470 | 501))
}

/// Extracts the HTTP status code from a message whose first line is a status
/// line, e.g. `POST /stream failed: HTTP/1.1 404 Not Found`.
///
/// Returns `None` for anything that is not a status line at all — a connection
/// error, a timeout — so those never provoke a retry.
fn status_code(msg: &str) -> Option<u16> {
    let status_line = msg.lines().next()?;
    let after_version = status_line
        .split("HTTP/1.1")
        .nth(1)
        .or_else(|| status_line.split("HTTP/1.0").nth(1))?;
    after_version.split_whitespace().next()?.parse().ok()
}

/// Negotiate AirPlay connection with authentication.
///
/// Strategy:
/// 1. GET /info to identify the device
/// 2. Warn — but do not refuse — on models that normally require FairPlay
/// 3. Refuse only on the advertised feature bit: no transient pairing support
///    means the handshake below cannot succeed, whatever the model string says
/// 4. HAP transient pair-setup
/// 5. POST /stream over the encrypted control channel
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

    // Step 2: warn about hardware known to need FairPlay, but do not refuse on
    // the model string alone.
    //
    // The string is self-reported and third-party receivers borrow it freely for
    // client compatibility. A Vivitek NovoConnect on the test network advertises
    // `AppleTV3,1` over mDNS and reports `AppleTV3,2` from GET /info while being
    // neither: `rmodel=AirReceiver3,1`, no HomeKit pairing, and a `/fp-setup`
    // that answers 200 to an empty body — nothing like the Apple hardware this
    // check was written for. Refusing it up front meant never discovering that
    // it authenticates fine and simply does not implement mirroring.
    //
    // Genuine Apple TV 2/3 still cannot work; the feature-bit check below is
    // what turns them away, with a reason the user can act on.
    if model.starts_with("AppleTV3") || model.starts_with("AppleTV2") {
        warn!(
            %model,
            "This model normally requires FairPlay, which is not implemented. \
             Continuing anyway — the string is self-reported and third-party \
             receivers reuse it. Expect failure from the receiver if it is genuine."
        );
    }

    // Step 3: refuse on evidence rather than on the model string.
    //
    // Feature bit 48 is the receiver's own statement about whether it supports
    // transient pairing, and it is the only thing here that actually predicts
    // whether step 4 can work. Checking it is what the model-string refusal was
    // reaching for: a genuine Apple TV 2/3 does not set it and is turned away
    // with a specific reason, while the Vivitek-style third-party boxes that
    // merely borrow the model string are not.
    if !server_info.features.supports_transient_pairing() {
        let hint = if model.starts_with("AppleTV3") || model.starts_with("AppleTV2") {
            " Apple TV 2nd and 3rd generation need FairPlay, which OpenPlay does \
             not implement and will not — see docs/crypto.md. Use a 4th generation \
             or later, or an AirPlay-compatible smart TV."
        } else {
            ""
        };
        return Err(AirPlayError::Pairing(format!(
            "{model} does not advertise transient pairing support (feature bit 48), \
             so the no-PIN handshake OpenPlay implements cannot authenticate to it.{hint}"
        )));
    }

    // Step 4: Try HAP transient pair-setup (no PIN, for AirPlay 2 devices)
    info!("Attempting HAP transient pairing (no PIN)");
    let session = hap_pairing::pair_setup_transient(receiver_addr)
        .await
        .map_err(|e| AirPlayError::Pairing(format!("Transient pairing failed: {e}")))?;

    info!("Transient pair-setup succeeded");

    // Step 5: everything after M4 is encrypted. There is no pair-verify in the
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

    // Step 6: and here the implemented path ends. `MirrorStream` writes NAL
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

#[cfg(test)]
mod auth_fallback_tests {
    use super::{status_code, wants_authentication};

    /// What `AirPlayError::Negotiation` actually carries: the whole header
    /// block, not just the status line. Every test below uses this shape,
    /// because the single-line strings the first version of these tests used
    /// were the reason the header-matching bug survived them.
    fn negotiation_failure(status_line: &str, headers: &[&str]) -> String {
        let mut msg = format!("POST /stream failed: {status_line}");
        for header in headers {
            msg.push_str("\r\n");
            msg.push_str(header);
        }
        msg
    }

    /// The statuses observed from real receivers that the original
    /// `501 || 403` test missed, and which caused casting to skip pairing.
    #[test]
    fn retries_on_statuses_real_receivers_actually_send() {
        assert!(
            wants_authentication(&negotiation_failure(
                "HTTP/1.1 404 Not Found",
                &["Content-Length: 0", "Server: AirTunes/950.7.1"]
            )),
            "a Mac set to Everyone with no password answers 404"
        );
        assert!(
            wants_authentication(&negotiation_failure(
                "HTTP/1.1 470 ",
                &["Content-Length: 32"]
            )),
            "a Mac with Require Password answers 470"
        );
        assert!(
            wants_authentication(&negotiation_failure(
                "HTTP/1.1 401 Unauthorized",
                &["WWW-Authenticate: Digest realm=\"airplay\""]
            )),
            "a receiver behind HTTP Digest answers 401"
        );
    }

    #[test]
    fn still_retries_on_the_original_two() {
        assert!(wants_authentication(&negotiation_failure(
            "HTTP/1.1 501 Not Implemented",
            &["Content-Length: 0"]
        )));
        assert!(wants_authentication(&negotiation_failure(
            "HTTP/1.1 403 Forbidden",
            &["Content-Length: 0"]
        )));
    }

    /// The regression this rewrite exists for.
    ///
    /// Every one of these is a permanent failure whose *headers* contain a
    /// retryable code. Substring-matching the message provoked a full SRP-6a
    /// pair-setup against a receiver that was never going to authenticate, and
    /// replaced an accurate error with a misleading pairing one.
    #[test]
    fn a_retryable_code_inside_a_header_does_not_trigger_a_retry() {
        assert!(
            !wants_authentication(&negotiation_failure(
                "HTTP/1.1 500 Internal Server Error",
                &["Content-Length: 1401", "Connection: close"]
            )),
            "Content-Length: 1401 contains 401"
        );
        assert!(
            !wants_authentication(&negotiation_failure(
                "HTTP/1.1 400 Bad Request",
                &["Content-Length: 404"]
            )),
            "Content-Length: 404 contains 404"
        );
        assert!(
            !wants_authentication(&negotiation_failure(
                "HTTP/1.1 500 Internal Server Error",
                &["Server: AirTunes/470.8.1"]
            )),
            "a version string can contain 470"
        );
        assert!(
            !wants_authentication(&negotiation_failure(
                "HTTP/1.1 200 OK",
                &["Date: Mon, 01 Jan 2024 05:01:03 GMT"]
            )),
            "a date can contain 501"
        );
    }

    #[test]
    fn does_not_retry_on_unrelated_failures() {
        assert!(!wants_authentication("Connection refused (os error 61)"));
        assert!(!wants_authentication("connection closed while reading"));
        assert!(!wants_authentication(""));
    }

    #[test]
    fn status_code_reads_only_the_first_line() {
        assert_eq!(
            status_code("POST /stream failed: HTTP/1.1 404 Not Found\r\nContent-Length: 401"),
            Some(404)
        );
        assert_eq!(status_code("HTTP/1.0 501 Not Implemented"), Some(501));
        assert_eq!(status_code("Connection refused"), None);
    }
}
