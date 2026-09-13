use std::net::SocketAddr;
use std::time::Duration;

use bytes::BytesMut;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::{debug, info, warn};

use crate::wfd_params::{WfdClientRtpPorts, WfdVideoFormats};
use crate::MiracastError;

/// Default WFD RTSP port.
pub const WFD_RTSP_PORT: u16 = 7236;

/// How long any single M1–M7 message may take to arrive.
///
/// A read on an open but silent socket never returns on its own, so without a
/// bound here a sink that completes the TCP handshake and then says nothing
/// wedges the cast forever with the UI stuck on "Connecting". Fifteen seconds is
/// far longer than a sink needs to answer a request it has already received.
pub const RTSP_READ_TIMEOUT: Duration = Duration::from_secs(15);

/// Session timeout promised to the sink in the M6 SETUP response, in seconds.
const SESSION_TIMEOUT_SECS: u64 = 30;

/// Session id for the one session a WFD control connection ever carries.
const SESSION_ID: &str = "1";

/// How often the source pings the sink over the control connection while
/// streaming.
///
/// M6 promised `timeout=30`, and a sink is entitled to drop the session once the
/// control connection has been silent that long. Pinging at a third of the
/// timeout leaves room for two lost round trips before the sink gives up on us.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(SESSION_TIMEOUT_SECS / 3);

/// How many keep-alives may go unanswered before the sink is declared gone.
///
/// Three pings with no reply is longer than the session timeout we promised the
/// sink, so by then it has stopped believing in us too and there is nothing left
/// to stream to.
const MAX_UNANSWERED_KEEPALIVES: u32 = 3;

/// Largest RTSP message accepted, as a guard against a peer that never sends the
/// header terminator.
const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// WFD RTSP negotiation state (M1 through M7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WfdState {
    /// Initial state.
    Init,
    /// M1: Source sent OPTIONS.
    M1OptionsSent,
    /// M2: Sink responded with OPTIONS.
    M2SinkOptions,
    /// M3: Source sent GET_PARAMETER.
    M3GetParameter,
    /// M4: Source sent SET_PARAMETER.
    M4SetParameter,
    /// M5: Source sent SETUP trigger.
    M5Trigger,
    /// M6: Sink sent SETUP with transport.
    M6Setup,
    /// M7: Sink sent PLAY.
    M7Play,
    /// Streaming active.
    Streaming,
}

/// Result of WFD RTSP negotiation.
#[derive(Debug)]
pub struct WfdNegotiationResult {
    /// The negotiated video width.
    pub width: u32,
    /// The negotiated video height.
    pub height: u32,
    /// The negotiated framerate.
    pub fps: u32,
    /// The RTP port the sink wants to receive on.
    pub rtp_port: u16,
    /// The sink's address.
    pub sink_addr: SocketAddr,
}

/// An RTSP control connection: the socket, plus the bytes read from it that have
/// not been consumed yet.
///
/// The buffer has to outlive a single message read, for two reasons. A sink is
/// free to put M6 SETUP and M7 PLAY in one TCP segment, and
/// [`tokio::time::timeout`] cancels the read it wraps — both lose whatever the
/// read had already accumulated if the buffer is a local inside the read.
///
/// Generic over the transport so the negotiation can be driven end to end in a
/// test over [`tokio::io::duplex`]; in production `S` is always a `TcpStream`.
pub struct RtspConnection<S> {
    stream: S,
    buf: BytesMut,
    cseq: u32,
}

impl<S: AsyncRead + AsyncWrite + Unpin> RtspConnection<S> {
    /// Wraps an already-connected stream.
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            buf: BytesMut::with_capacity(4096),
            cseq: 0,
        }
    }

    /// Returns the CSeq to use for the next request we originate.
    fn next_cseq(&mut self) -> u32 {
        self.cseq += 1;
        self.cseq
    }

    /// Writes one complete RTSP message.
    ///
    /// `what` names the message for the error, since every send site otherwise
    /// produces the same "Broken pipe" with no clue which exchange broke.
    async fn send(&mut self, msg: &str, what: &str) -> Result<(), MiracastError> {
        self.stream
            .write_all(msg.as_bytes())
            .await
            .map_err(|e| MiracastError::Rtsp(format!("Send {what}: {e}")))
    }

    /// Reads one RTSP message, waiting no longer than `timeout` for it.
    async fn read_message_within(&mut self, timeout: Duration) -> Result<String, MiracastError> {
        tokio::time::timeout(timeout, self.read_message())
            .await
            .map_err(|_| MiracastError::RtspTimeout(timeout))?
    }

    /// Reads one RTSP message, blocking until the sink sends one.
    ///
    /// Cancel-safe: everything read stays in `self.buf`, so a caller that races
    /// this against a timer and loses resumes where it left off.
    async fn read_message(&mut self) -> Result<String, MiracastError> {
        loop {
            if let Some(len) = framed_message_len(&self.buf) {
                let raw = self.buf.split_to(len);
                return Ok(String::from_utf8_lossy(&raw).into_owned());
            }

            if self.buf.len() > MAX_MESSAGE_BYTES {
                return Err(MiracastError::Rtsp("RTSP message too large".to_string()));
            }

            let n = self
                .stream
                .read_buf(&mut self.buf)
                .await
                .map_err(|e| MiracastError::Rtsp(format!("Read error: {e}")))?;
            if n == 0 {
                return Err(MiracastError::Rtsp("Connection closed".to_string()));
            }
        }
    }

    /// Reads one message and parses it as a response.
    async fn read_response(&mut self, timeout: Duration) -> Result<RtspResponse, MiracastError> {
        let raw = self.read_message_within(timeout).await?;
        Ok(parse_response(&raw))
    }

    /// Reads one message and parses it as a request.
    async fn read_request(&mut self, timeout: Duration) -> Result<RtspRequest, MiracastError> {
        let raw = self.read_message_within(timeout).await?;
        Ok(parse_request(&raw))
    }
}

/// Performs WFD RTSP M1-M7 negotiation with a Miracast sink.
///
/// The source (us) drives the negotiation by sending RTSP requests
/// and the sink responds.
///
/// Takes the connection by reference and leaves it open: the M6 response
/// promises the sink `timeout=30`, so the caller has to hold this socket and
/// keep answering on it for as long as the cast lasts. See
/// [`serve_control_channel`].
pub async fn negotiate<S: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut RtspConnection<S>,
    sink_addr: SocketAddr,
    source_formats: &WfdVideoFormats,
) -> Result<WfdNegotiationResult, MiracastError> {
    // M1: Source sends OPTIONS
    let cseq = conn.next_cseq();
    send_options(conn, cseq).await?;
    let m1_resp = conn.read_response(RTSP_READ_TIMEOUT).await?;
    if m1_resp.code != 200 {
        return Err(MiracastError::Rtsp(format!(
            "M1 OPTIONS rejected: {}",
            m1_resp.status
        )));
    }
    debug!(state = ?WfdState::M1OptionsSent, "M1 complete");

    // M2: Wait for sink's OPTIONS request
    let m2_req = conn.read_request(RTSP_READ_TIMEOUT).await?;
    if m2_req.method != "OPTIONS" {
        return Err(MiracastError::Rtsp(format!(
            "Expected M2 OPTIONS, got {}",
            m2_req.method
        )));
    }
    send_options_response(conn, &m2_req).await?;
    debug!(state = ?WfdState::M2SinkOptions, "M2 complete");

    // M3: Source sends GET_PARAMETER to query sink capabilities
    let cseq = conn.next_cseq();
    send_get_parameter(conn, cseq).await?;
    let m3_resp = conn.read_response(RTSP_READ_TIMEOUT).await?;
    if m3_resp.code != 200 {
        return Err(MiracastError::Rtsp(format!(
            "M3 GET_PARAMETER rejected: {}",
            m3_resp.status
        )));
    }

    // Parse sink capabilities
    let sink_formats = parse_sink_video_formats(&m3_resp.body);
    let _sink_rtp_ports = parse_sink_rtp_ports(&m3_resp.body);

    // Negotiate resolution
    let (width, height, fps) =
        crate::wfd_params::negotiate_resolution(source_formats, &sink_formats.unwrap_or_default());
    info!(width, height, fps, "Negotiated resolution");

    // M4: Source sends SET_PARAMETER with negotiated params
    let cseq = conn.next_cseq();
    let negotiated = WfdVideoFormats {
        cea_resolutions: crate::wfd_params::CeaResolutions(match (width, height, fps) {
            (1920, 1080, 60) => crate::wfd_params::CeaResolutions::RES_1920X1080_P60,
            (1920, 1080, _) => crate::wfd_params::CeaResolutions::RES_1920X1080_P30,
            (1280, 720, 60) => crate::wfd_params::CeaResolutions::RES_1280X720_P60,
            _ => crate::wfd_params::CeaResolutions::RES_1280X720_P30,
        }),
        ..source_formats.clone()
    };
    send_set_parameter(conn, cseq, &negotiated).await?;
    let m4_resp = conn.read_response(RTSP_READ_TIMEOUT).await?;
    if m4_resp.code != 200 {
        return Err(MiracastError::Rtsp(format!(
            "M4 SET_PARAMETER rejected: {}",
            m4_resp.status
        )));
    }
    debug!(state = ?WfdState::M4SetParameter, "M4 complete");

    // M5: Source sends SET_PARAMETER with trigger SETUP
    let cseq = conn.next_cseq();
    send_trigger_setup(conn, cseq).await?;
    let m5_resp = conn.read_response(RTSP_READ_TIMEOUT).await?;
    if m5_resp.code != 200 {
        return Err(MiracastError::Rtsp(format!(
            "M5 trigger rejected: {}",
            m5_resp.status
        )));
    }
    debug!(state = ?WfdState::M5Trigger, "M5 complete");

    // M6: Wait for sink's SETUP request
    let m6_req = conn.read_request(RTSP_READ_TIMEOUT).await?;
    if m6_req.method != "SETUP" {
        return Err(MiracastError::Rtsp(format!(
            "Expected M6 SETUP, got {}",
            m6_req.method
        )));
    }
    // No usable client_port is fatal on purpose. Falling back to a default here
    // sends every RTP packet to a port nobody is bound to, which the sink cannot
    // report and the user sees only as a permanently black screen.
    let rtp_port = parse_transport_port(&m6_req.headers).ok_or_else(|| {
        MiracastError::Rtsp(format!(
            "M6 SETUP carries no usable client_port, nowhere to send RTP: {}",
            m6_req.headers.replace("\r\n", " | ")
        ))
    })?;
    send_setup_response(conn, &m6_req, rtp_port).await?;
    debug!(state = ?WfdState::M6Setup, rtp_port, "M6 complete");

    // M7: Wait for sink's PLAY request
    let m7_req = conn.read_request(RTSP_READ_TIMEOUT).await?;
    if m7_req.method != "PLAY" {
        return Err(MiracastError::Rtsp(format!(
            "Expected M7 PLAY, got {}",
            m7_req.method
        )));
    }
    send_play_response(conn, &m7_req).await?;
    info!(state = ?WfdState::M7Play, "WFD negotiation complete — ready to stream");

    Ok(WfdNegotiationResult {
        width,
        height,
        fps,
        rtp_port,
        sink_addr,
    })
}

/// Holds the control connection open for the life of the cast.
///
/// M7 only starts the stream; the session lives on the control connection.
/// The media itself leaves over UDP and tells the sink nothing about our
/// liveness, so this pings every [`KEEPALIVE_INTERVAL`] and answers whatever the
/// sink sends (its own keep-alives, and `SET_PARAMETER` requests such as
/// `wfd_idr_request`). Returning from here — or worse, dropping the socket —
/// tells the sink the session is over.
///
/// Returns `Ok(())` when the sink ends the session cleanly with TEARDOWN, and an
/// error when the connection breaks or the sink stops answering.
pub async fn serve_control_channel<S: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut RtspConnection<S>,
) -> Result<(), MiracastError> {
    serve_control_channel_every(conn, KEEPALIVE_INTERVAL).await
}

/// The body of [`serve_control_channel`], with the ping period as an argument so
/// a test can exercise the give-up path in milliseconds instead of sitting
/// through three real ten-second silences.
async fn serve_control_channel_every<S: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut RtspConnection<S>,
    ping_every: Duration,
) -> Result<(), MiracastError> {
    debug!(state = ?WfdState::Streaming, "Holding RTSP control connection open");
    let mut unanswered = 0u32;

    loop {
        // The timeout doubles as the ping timer: nothing from the sink for a
        // whole period is the cue to ping it. `read_message` is cancel-safe, so
        // the half-read message a busy sink was in the middle of survives the
        // timeout and completes on the next pass.
        match tokio::time::timeout(ping_every, conn.read_message()).await {
            Ok(Ok(raw)) => {
                unanswered = 0;
                if handle_control_message(conn, &raw).await? {
                    return Ok(());
                }
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                unanswered += 1;
                if unanswered > MAX_UNANSWERED_KEEPALIVES {
                    let silence = ping_every * unanswered;
                    warn!(?silence, "Sink stopped answering keep-alives");
                    return Err(MiracastError::RtspTimeout(silence));
                }
                let cseq = conn.next_cseq();
                send_keepalive(conn, cseq).await?;
            }
        }
    }
}

/// Handles one message that arrived on the control connection while streaming.
///
/// Returns `true` when the message ended the session.
async fn handle_control_message<S: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut RtspConnection<S>,
    raw: &str,
) -> Result<bool, MiracastError> {
    // A response can only be to a keep-alive we sent; anything else here is a
    // request the sink expects an answer to.
    if raw.starts_with("RTSP/") {
        let resp = parse_response(raw);
        if resp.code != 200 {
            warn!(status = %resp.status, "Sink rejected a keep-alive");
        }
        return Ok(false);
    }

    let req = parse_request(raw);
    match req.method.as_str() {
        "TEARDOWN" => {
            send_ok(conn, &req, "TEARDOWN response").await?;
            info!("Sink ended the session (TEARDOWN)");
            Ok(true)
        }
        "OPTIONS" => {
            send_options_response(conn, &req).await?;
            Ok(false)
        }
        // GET_PARAMETER is the sink's own keep-alive; SET_PARAMETER during
        // streaming is usually wfd_idr_request. Neither needs anything from us
        // beyond an acknowledgement — the encoder is not steerable from here.
        _ => {
            debug!(method = %req.method, "Acknowledging sink request during streaming");
            send_ok(conn, &req, "control response").await?;
            Ok(false)
        }
    }
}

// --- RTSP message types ---

#[allow(dead_code)]
#[derive(Debug)]
struct RtspResponse {
    /// Numeric status code, parsed out of the status line.
    ///
    /// Kept separately because the status line is `RTSP/1.0 200 OK` — testing it
    /// with `starts_with("200")` matches the version, never the code, and so
    /// rejects every response a sink can send.
    code: u16,
    status: String,
    headers: String,
    body: String,
}

#[allow(dead_code)]
struct RtspRequest {
    method: String,
    uri: String,
    headers: String,
    body: String,
    cseq: u32,
}

// --- Send helpers ---

async fn send_options<S: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut RtspConnection<S>,
    cseq: u32,
) -> Result<(), MiracastError> {
    let msg = format!("OPTIONS * RTSP/1.0\r\nCSeq: {cseq}\r\nRequire: org.wfa.wfd1.0\r\n\r\n");
    conn.send(&msg, "M1").await
}

async fn send_options_response<S: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut RtspConnection<S>,
    req: &RtspRequest,
) -> Result<(), MiracastError> {
    let msg = format!(
        "RTSP/1.0 200 OK\r\nCSeq: {}\r\nPublic: org.wfa.wfd1.0, GET_PARAMETER, SET_PARAMETER\r\n\r\n",
        req.cseq
    );
    conn.send(&msg, "OPTIONS response").await
}

async fn send_get_parameter<S: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut RtspConnection<S>,
    cseq: u32,
) -> Result<(), MiracastError> {
    let body = "wfd_video_formats\r\nwfd_audio_codecs\r\nwfd_client_rtp_ports\r\n";
    let msg = format!(
        "GET_PARAMETER rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: {cseq}\r\nContent-Type: text/parameters\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    conn.send(&msg, "M3").await
}

async fn send_set_parameter<S: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut RtspConnection<S>,
    cseq: u32,
    video_formats: &WfdVideoFormats,
) -> Result<(), MiracastError> {
    let body = format!(
        "wfd_video_formats: {}\r\nwfd_client_rtp_ports: RTP/AVP/UDP;unicast 1028 0 mode=play\r\n",
        video_formats.encode()
    );
    let msg = format!(
        "SET_PARAMETER rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: {cseq}\r\nContent-Type: text/parameters\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    conn.send(&msg, "M4").await
}

async fn send_trigger_setup<S: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut RtspConnection<S>,
    cseq: u32,
) -> Result<(), MiracastError> {
    let body = "wfd_trigger_method: SETUP\r\n";
    let msg = format!(
        "SET_PARAMETER rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: {cseq}\r\nContent-Type: text/parameters\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    conn.send(&msg, "M5").await
}

async fn send_setup_response<S: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut RtspConnection<S>,
    req: &RtspRequest,
    rtp_port: u16,
) -> Result<(), MiracastError> {
    let msg = format!(
        "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: {SESSION_ID};timeout={SESSION_TIMEOUT_SECS}\r\nTransport: RTP/AVP/UDP;unicast;client_port={rtp_port};server_port={rtp_port}\r\n\r\n",
        req.cseq
    );
    conn.send(&msg, "M6 response").await
}

async fn send_play_response<S: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut RtspConnection<S>,
    req: &RtspRequest,
) -> Result<(), MiracastError> {
    let msg = format!(
        "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: {SESSION_ID}\r\n\r\n",
        req.cseq
    );
    conn.send(&msg, "M7 response").await
}

async fn send_ok<S: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut RtspConnection<S>,
    req: &RtspRequest,
    what: &str,
) -> Result<(), MiracastError> {
    let msg = format!(
        "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: {SESSION_ID}\r\n\r\n",
        req.cseq
    );
    conn.send(&msg, what).await
}

/// Sends the RTSP ping: a `GET_PARAMETER` with no body, which RFC 2326 §10.8
/// defines as the liveness check.
async fn send_keepalive<S: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut RtspConnection<S>,
    cseq: u32,
) -> Result<(), MiracastError> {
    let msg = format!(
        "GET_PARAMETER rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: {cseq}\r\nSession: {SESSION_ID}\r\n\r\n"
    );
    conn.send(&msg, "keep-alive").await
}

// --- Parse helpers ---

/// Returns the total length of the message at the front of `buf`, or `None`
/// while it is still incomplete.
///
/// Works on bytes rather than on a lossy string: a non-UTF-8 byte anywhere in
/// the buffer would otherwise shift every offset by the length difference of the
/// replacement character and cut the message in the wrong place.
fn framed_message_len(buf: &[u8]) -> Option<usize> {
    let header_end = find_subslice(buf, b"\r\n\r\n")?;
    let body_start = header_end + 4;

    let headers = String::from_utf8_lossy(&buf[..header_end]);
    let content_length = headers
        .lines()
        .find(|l| l.to_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);

    let total = body_start + content_length;
    (buf.len() >= total).then_some(total)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_response(raw: &str) -> RtspResponse {
    let (header_part, body) = split_header_body(raw);
    let status = header_part.lines().next().unwrap_or("").to_string();
    // "RTSP/1.0 200 OK" → 200. An unparseable status line leaves 0, which fails
    // every check below, which is the right way for a garbled response to end.
    let code = status
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    RtspResponse {
        code,
        status,
        headers: header_part.to_string(),
        body,
    }
}

fn parse_request(raw: &str) -> RtspRequest {
    let (header_part, body) = split_header_body(raw);
    let first_line = header_part.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    let method = parts.first().unwrap_or(&"").to_string();
    let uri = parts.get(1).unwrap_or(&"").to_string();

    let cseq = header_part
        .lines()
        .find(|l| l.to_lowercase().starts_with("cseq:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);

    RtspRequest {
        method,
        uri,
        headers: header_part.to_string(),
        body,
        cseq,
    }
}

fn split_header_body(raw: &str) -> (&str, String) {
    if let Some(pos) = raw.find("\r\n\r\n") {
        (&raw[..pos], raw[pos + 4..].to_string())
    } else {
        (raw, String::new())
    }
}

fn parse_sink_video_formats(body: &str) -> Option<WfdVideoFormats> {
    for line in body.lines() {
        if let Some(val) = line.strip_prefix("wfd_video_formats:") {
            return WfdVideoFormats::parse(val.trim());
        }
    }
    None
}

fn parse_sink_rtp_ports(body: &str) -> Option<WfdClientRtpPorts> {
    for line in body.lines() {
        if let Some(val) = line.strip_prefix("wfd_client_rtp_ports:") {
            return WfdClientRtpPorts::parse(val.trim());
        }
    }
    None
}

/// Extracts the RTP port the sink wants from a SETUP request's Transport header.
///
/// `client_port` is a range in RFC 2326 — `client_port=19000-19001`, the RTP port
/// and its RTCP partner — and sinks do send it that way. Parsing the whole token
/// as a `u16` fails on every one of those, and the port the source then streams
/// to is whatever the caller's fallback was: no error anywhere, just a sink that
/// receives nothing. Take the first number of the range and tolerate the
/// whitespace RFC 2326 allows around the parameters.
fn parse_transport_port(headers: &str) -> Option<u16> {
    for line in headers.lines() {
        let transport = match line
            .strip_prefix("Transport:")
            .or_else(|| line.strip_prefix("transport:"))
        {
            Some(t) => t,
            None => continue,
        };

        for part in transport.split(';') {
            if let Some(value) = part.trim().strip_prefix("client_port=") {
                // "19000-19001" → 19000, "19000" → 19000.
                let first = value.split('-').next().unwrap_or("").trim();
                if let Ok(port) = first.parse::<u16>() {
                    return Some(port);
                }
            }
        }
    }
    None
}

/// A stand-in for the sink half of a control connection.
///
/// Lives outside `mod tests` so [`crate::session`] can drive a whole negotiation
/// over a loopback socket with it. It buffers exactly like the real reader does,
/// because the source pipelines — it answers the sink's OPTIONS and asks its own
/// GET_PARAMETER back to back — and a fake sink that assumes one message per
/// read loses half the script.
#[cfg(test)]
pub(crate) mod test_sink {
    use super::*;

    pub(crate) struct FakePeer<S> {
        stream: S,
        buf: Vec<u8>,
    }

    impl<S: AsyncRead + AsyncWrite + Unpin> FakePeer<S> {
        pub(crate) fn new(stream: S) -> Self {
            Self {
                stream,
                buf: Vec::new(),
            }
        }

        /// Reads one whole RTSP message from the source.
        pub(crate) async fn recv(&mut self) -> String {
            loop {
                if let Some(len) = framed_message_len(&self.buf) {
                    let msg = String::from_utf8_lossy(&self.buf[..len]).into_owned();
                    self.buf.drain(..len);
                    return msg;
                }
                let mut chunk = [0u8; 1024];
                let n = self
                    .stream
                    .read(&mut chunk)
                    .await
                    .expect("read from source");
                assert_ne!(
                    n,
                    0,
                    "source closed the connection, buffer held {:?}",
                    String::from_utf8_lossy(&self.buf)
                );
                self.buf.extend_from_slice(&chunk[..n]);
            }
        }

        pub(crate) async fn send(&mut self, msg: &str) {
            self.stream
                .write_all(msg.as_bytes())
                .await
                .expect("write to source");
        }

        /// Answers a request with a bare 200, echoing its CSeq.
        pub(crate) async fn reply_ok(&mut self, req: &str) {
            let cseq = cseq_of(req);
            self.send(&format!("RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\n\r\n"))
                .await;
        }

        /// Answers a request with a 200 carrying a `text/parameters` body.
        pub(crate) async fn reply_with(&mut self, req: &str, body: &str) {
            let cseq = cseq_of(req);
            self.send(&format!(
                "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nContent-Type: text/parameters\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ))
            .await;
        }

        /// Blocks until the source closes the connection.
        pub(crate) async fn wait_for_close(&mut self) {
            loop {
                let mut chunk = [0u8; 1024];
                match self.stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => continue,
                }
            }
        }
    }

    pub(crate) fn cseq_of(msg: &str) -> u32 {
        msg.lines()
            .find(|l| l.to_lowercase().starts_with("cseq:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or_else(|| panic!("no CSeq in {msg:?}"))
    }

    /// What the sink claims to support in M3: 1080p60 on top of the two modes
    /// the source offers by default, so the best common mode is 1080p30.
    pub(crate) fn sink_capabilities() -> String {
        let formats = WfdVideoFormats {
            cea_resolutions: crate::wfd_params::CeaResolutions(
                crate::wfd_params::CeaResolutions::RES_1920X1080_P60
                    | crate::wfd_params::CeaResolutions::RES_1920X1080_P30
                    | crate::wfd_params::CeaResolutions::RES_1280X720_P30,
            ),
            ..Default::default()
        };
        format!(
            "wfd_video_formats: {}\r\nwfd_client_rtp_ports: RTP/AVP/UDP;unicast 1028 0 mode=play\r\n",
            formats.encode()
        )
    }

    /// Plays the sink's half of M1–M7, asking for `client_port` in the SETUP.
    ///
    /// Leaves the connection open on return, since that is what a real sink does
    /// and what [`serve_control_channel`] has to cope with.
    pub(crate) async fn run_negotiation<S: AsyncRead + AsyncWrite + Unpin>(
        peer: &mut FakePeer<S>,
        client_port: &str,
    ) {
        let m1 = peer.recv().await;
        assert!(m1.starts_with("OPTIONS"), "M1 was {m1:?}");
        peer.reply_ok(&m1).await;

        // M2 is the sink's own OPTIONS, which the source must answer.
        peer.send("OPTIONS * RTSP/1.0\r\nCSeq: 100\r\nRequire: org.wfa.wfd1.0\r\n\r\n")
            .await;
        let m2_resp = peer.recv().await;
        assert!(
            m2_resp.starts_with("RTSP/1.0 200"),
            "M2 answer was {m2_resp:?}"
        );

        let m3 = peer.recv().await;
        assert!(m3.starts_with("GET_PARAMETER"), "M3 was {m3:?}");
        peer.reply_with(&m3, &sink_capabilities()).await;

        // M4 and M5 are both SET_PARAMETER; acknowledge each.
        for expected in ["wfd_video_formats", "wfd_trigger_method: SETUP"] {
            let req = peer.recv().await;
            assert!(
                req.starts_with("SET_PARAMETER") && req.contains(expected),
                "expected a SET_PARAMETER carrying {expected}, got {req:?}"
            );
            peer.reply_ok(&req).await;
        }

        peer.send(&format!(
            "SETUP rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: 101\r\n\
             Transport: RTP/AVP/UDP;unicast;client_port={client_port}\r\n\r\n"
        ))
        .await;
        let m6_resp = peer.recv().await;
        assert!(
            m6_resp.contains(&format!(
                "Session: {SESSION_ID};timeout={SESSION_TIMEOUT_SECS}"
            )),
            "M6 answer was {m6_resp:?}"
        );

        peer.send("PLAY rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: 102\r\nSession: 1\r\n\r\n")
            .await;
        let m7_resp = peer.recv().await;
        assert!(
            m7_resp.starts_with("RTSP/1.0 200"),
            "M7 answer was {m7_resp:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::test_sink::{cseq_of, run_negotiation, FakePeer};
    use super::*;

    // ── parse_transport_port ──────────────────────────────────────────────────

    #[test]
    fn transport_port_parses_a_range() {
        let headers = "SETUP rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: 4\r\n\
                       Transport: RTP/AVP/UDP;unicast;client_port=19000-19001";
        assert_eq!(parse_transport_port(headers), Some(19000));
    }

    #[test]
    fn transport_port_parses_a_single_port() {
        let headers = "Transport: RTP/AVP/UDP;unicast;client_port=19000";
        assert_eq!(parse_transport_port(headers), Some(19000));
    }

    #[test]
    fn transport_port_tolerates_leading_space() {
        let headers = "Transport: RTP/AVP/UDP;unicast; client_port=19000-19001;mode=play";
        assert_eq!(parse_transport_port(headers), Some(19000));
    }

    #[test]
    fn transport_port_accepts_lowercase_header() {
        let headers = "transport: RTP/AVP/UDP;unicast;client_port=15550-15551";
        assert_eq!(parse_transport_port(headers), Some(15550));
    }

    #[test]
    fn transport_port_ignores_server_port() {
        let headers =
            "Transport: RTP/AVP/UDP;unicast;server_port=5000-5001;client_port=19000-19001";
        assert_eq!(parse_transport_port(headers), Some(19000));
    }

    #[test]
    fn transport_port_is_none_without_a_transport_header() {
        let headers = "SETUP rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: 4\r\nSession: 1";
        assert_eq!(parse_transport_port(headers), None);
    }

    #[test]
    fn transport_port_is_none_when_unparseable() {
        assert_eq!(
            parse_transport_port("Transport: RTP/AVP/UDP;unicast;client_port=abc-def"),
            None
        );
        assert_eq!(
            parse_transport_port("Transport: RTP/AVP/UDP;unicast;client_port="),
            None
        );
        // 70000 does not fit in a u16 and must not wrap round to something valid.
        assert_eq!(
            parse_transport_port("Transport: RTP/AVP/UDP;unicast;client_port=70000-70001"),
            None
        );
    }

    // ── status lines ──────────────────────────────────────────────────────────

    #[test]
    fn response_code_comes_from_the_status_line_not_its_prefix() {
        // The status line starts with the version, so a prefix test against
        // "200" rejects every response a sink can send.
        let ok = parse_response("RTSP/1.0 200 OK\r\nCSeq: 1\r\n\r\n");
        assert_eq!(ok.code, 200);
        assert!(!ok.status.starts_with("200"));

        assert_eq!(
            parse_response("RTSP/1.0 501 Not Implemented\r\n\r\n").code,
            501
        );
        assert_eq!(parse_response("garbage\r\n\r\n").code, 0);
    }

    #[test]
    fn request_parsing_pulls_method_and_cseq() {
        let req = parse_request(
            "SETUP rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: 42\r\nSession: 1\r\n\r\n",
        );
        assert_eq!(req.method, "SETUP");
        assert_eq!(req.uri, "rtsp://localhost/wfd1.0");
        assert_eq!(req.cseq, 42);
    }

    // ── message framing ───────────────────────────────────────────────────────

    #[test]
    fn framing_needs_the_header_terminator() {
        assert_eq!(
            framed_message_len(b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n"),
            None
        );
    }

    #[test]
    fn framing_measures_a_bodyless_message() {
        let msg = b"RTSP/1.0 200 OK\r\nCSeq: 1\r\n\r\n";
        assert_eq!(framed_message_len(msg), Some(msg.len()));
    }

    #[test]
    fn framing_waits_for_the_declared_body() {
        let partial = b"RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Length: 10\r\n\r\nabc";
        assert_eq!(framed_message_len(partial), None);
        let complete = b"RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Length: 10\r\n\r\nabcdefghij";
        assert_eq!(framed_message_len(complete), Some(complete.len()));
    }

    #[test]
    fn framing_stops_at_the_end_of_the_first_message() {
        // A sink is free to pipeline; the second message must be left in the buffer.
        let first = "RTSP/1.0 200 OK\r\nCSeq: 1\r\n\r\n";
        let two = format!("{first}RTSP/1.0 200 OK\r\nCSeq: 2\r\n\r\n");
        assert_eq!(framed_message_len(two.as_bytes()), Some(first.len()));
    }

    #[tokio::test]
    async fn pipelined_messages_are_both_read() {
        let (mut sink, source) = tokio::io::duplex(1024);
        sink.write_all(b"RTSP/1.0 200 OK\r\nCSeq: 1\r\n\r\nRTSP/1.0 200 OK\r\nCSeq: 2\r\n\r\n")
            .await
            .unwrap();

        let mut conn = RtspConnection::new(source);
        let first = conn.read_message().await.unwrap();
        let second = conn.read_message().await.unwrap();
        assert_eq!(cseq_of(&first), 1);
        assert_eq!(cseq_of(&second), 2);
    }

    #[tokio::test]
    async fn a_message_split_across_writes_is_reassembled() {
        let (mut sink, source) = tokio::io::duplex(1024);
        let mut conn = RtspConnection::new(source);

        sink.write_all(b"RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Le")
            .await
            .unwrap();
        let read = tokio::spawn(async move { conn.read_message().await.unwrap() });
        sink.write_all(b"ngth: 5\r\n\r\nhello").await.unwrap();

        assert!(read.await.unwrap().ends_with("hello"));
    }

    #[tokio::test]
    async fn a_silent_sink_times_out_instead_of_hanging() {
        let (_sink, source) = tokio::io::duplex(1024);
        let mut conn = RtspConnection::new(source);

        let err = conn
            .read_message_within(Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(
            matches!(err, MiracastError::RtspTimeout(_)),
            "expected a timeout, got {err}"
        );
    }

    #[tokio::test]
    async fn a_closed_connection_is_an_error_not_a_hang() {
        let (sink, source) = tokio::io::duplex(1024);
        drop(sink);
        let mut conn = RtspConnection::new(source);

        let err = conn
            .read_message_within(Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Connection closed"));
    }

    // ── negotiation ───────────────────────────────────────────────────────────

    fn sink_addr() -> SocketAddr {
        "192.168.1.50:7236".parse().unwrap()
    }

    #[tokio::test]
    async fn negotiation_runs_m1_to_m7_and_takes_the_port_from_the_range() {
        let (sink, source) = tokio::io::duplex(8192);
        let sink_task = tokio::spawn(async move {
            let mut peer = FakePeer::new(sink);
            run_negotiation(&mut peer, "19000-19001").await;
            peer
        });

        let mut conn = RtspConnection::new(source);
        let result = negotiate(&mut conn, sink_addr(), &WfdVideoFormats::default())
            .await
            .expect("negotiation should complete against a well-behaved sink");

        sink_task.await.unwrap();
        // 19000, not the 1028 fallback the unparsed range used to produce —
        // that fallback is why the sink received no RTP at all.
        assert_eq!(result.rtp_port, 19000);
        assert_eq!((result.width, result.height, result.fps), (1920, 1080, 30));
        assert_eq!(result.sink_addr, sink_addr());
    }

    #[tokio::test]
    async fn negotiation_survives_a_sink_that_pipelines_setup_and_play() {
        let (sink, source) = tokio::io::duplex(8192);
        let sink_task = tokio::spawn(async move {
            let mut peer = FakePeer::new(sink);
            let m1 = peer.recv().await;
            peer.reply_ok(&m1).await;
            peer.send("OPTIONS * RTSP/1.0\r\nCSeq: 100\r\n\r\n").await;
            peer.recv().await;
            let m3 = peer.recv().await;
            peer.reply_with(&m3, &super::test_sink::sink_capabilities())
                .await;
            for _ in 0..2 {
                let req = peer.recv().await;
                peer.reply_ok(&req).await;
            }
            // SETUP and PLAY in a single write, which a sink is entitled to do.
            peer.send(
                "SETUP rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: 101\r\n\
                 Transport: RTP/AVP/UDP;unicast;client_port=15550-15551\r\n\r\n\
                 PLAY rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: 102\r\nSession: 1\r\n\r\n",
            )
            .await;
            peer.recv().await; // M6 response
            peer.recv().await; // M7 response
        });

        let mut conn = RtspConnection::new(source);
        let result = negotiate(&mut conn, sink_addr(), &WfdVideoFormats::default())
            .await
            .expect("a pipelined SETUP/PLAY must not lose the PLAY");
        sink_task.await.unwrap();
        assert_eq!(result.rtp_port, 15550);
    }

    #[tokio::test]
    async fn setup_without_a_client_port_fails_instead_of_guessing() {
        let (sink, source) = tokio::io::duplex(8192);
        let sink_task = tokio::spawn(async move {
            let mut peer = FakePeer::new(sink);
            let m1 = peer.recv().await;
            peer.reply_ok(&m1).await;
            peer.send("OPTIONS * RTSP/1.0\r\nCSeq: 100\r\n\r\n").await;
            peer.recv().await;
            let m3 = peer.recv().await;
            peer.reply_with(&m3, &super::test_sink::sink_capabilities())
                .await;
            for _ in 0..2 {
                let req = peer.recv().await;
                peer.reply_ok(&req).await;
            }
            peer.send(
                "SETUP rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: 101\r\n\
                 Transport: RTP/AVP/UDP;unicast\r\n\r\n",
            )
            .await;
            // Hold the connection open so the source fails on the header rather
            // than on an EOF that would prove nothing.
            peer.wait_for_close().await;
        });

        let mut conn = RtspConnection::new(source);
        let err = negotiate(&mut conn, sink_addr(), &WfdVideoFormats::default())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("client_port"),
            "expected a client_port complaint, got {err}"
        );
        drop(conn);
        sink_task.await.unwrap();
    }

    #[tokio::test]
    async fn negotiation_gives_up_on_a_sink_that_stops_answering() {
        let (sink, source) = tokio::io::duplex(8192);
        let sink_task = tokio::spawn(async move {
            let mut peer = FakePeer::new(sink);
            peer.recv().await; // Take M1 and never answer it.
            peer.wait_for_close().await;
        });

        let mut conn = RtspConnection::new(source);
        // The real RTSP_READ_TIMEOUT is 15s; drive the same path faster.
        send_options(&mut conn, 1).await.unwrap();
        let err = conn
            .read_response(Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(err, MiracastError::RtspTimeout(_)));
        drop(conn);
        sink_task.await.unwrap();
    }

    // ── control channel ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn control_channel_answers_the_sink_and_ends_on_teardown() {
        let (sink, source) = tokio::io::duplex(4096);
        let sink_task = tokio::spawn(async move {
            let mut peer = FakePeer::new(sink);
            // The sink's own keep-alive must come back as a 200 with its CSeq.
            peer.send(
                "GET_PARAMETER rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: 200\r\nSession: 1\r\n\r\n",
            )
            .await;
            let ack = peer.recv().await;
            assert!(ack.starts_with("RTSP/1.0 200"), "ack was {ack:?}");
            assert_eq!(cseq_of(&ack), 200);

            peer.send(
                "TEARDOWN rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: 201\r\nSession: 1\r\n\r\n",
            )
            .await;
            assert_eq!(cseq_of(&peer.recv().await), 201);
        });

        let mut conn = RtspConnection::new(source);
        serve_control_channel(&mut conn)
            .await
            .expect("TEARDOWN is a clean end, not an error");
        sink_task.await.unwrap();
    }

    #[tokio::test]
    async fn control_channel_reports_a_sink_that_disappears() {
        let (sink, source) = tokio::io::duplex(4096);
        drop(sink);

        let mut conn = RtspConnection::new(source);
        let err = serve_control_channel(&mut conn).await.unwrap_err();
        assert!(err.to_string().contains("Connection closed"));
    }

    #[tokio::test]
    async fn control_channel_pings_a_quiet_sink_and_gives_up_on_silence() {
        let (sink, source) = tokio::io::duplex(4096);
        let sink_task = tokio::spawn(async move {
            let mut peer = FakePeer::new(sink);
            // Never answer. All three pings still have to arrive first.
            for _ in 0..MAX_UNANSWERED_KEEPALIVES {
                let ping = peer.recv().await;
                assert!(ping.starts_with("GET_PARAMETER"), "ping was {ping:?}");
                assert!(ping.contains("Session: 1"), "ping was {ping:?}");
            }
            peer.wait_for_close().await;
        });

        let mut conn = RtspConnection::new(source);
        let err = serve_control_channel_every(&mut conn, Duration::from_millis(20))
            .await
            .unwrap_err();
        assert!(
            matches!(err, MiracastError::RtspTimeout(_)),
            "expected a timeout, got {err}"
        );
        drop(conn);
        sink_task.await.unwrap();
    }

    #[tokio::test]
    async fn control_channel_keeps_going_while_the_sink_answers() {
        let (sink, source) = tokio::io::duplex(4096);
        let sink_task = tokio::spawn(async move {
            let mut peer = FakePeer::new(sink);
            // Answer twice as many pings as it takes to be declared dead, to
            // show an answered keep-alive resets the count.
            for _ in 0..(MAX_UNANSWERED_KEEPALIVES * 2 + 2) {
                let ping = peer.recv().await;
                peer.reply_ok(&ping).await;
            }
            peer.send("TEARDOWN rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: 9\r\n\r\n")
                .await;
            peer.recv().await;
        });

        let mut conn = RtspConnection::new(source);
        serve_control_channel_every(&mut conn, Duration::from_millis(10))
            .await
            .expect("a sink that answers must not be declared gone");
        sink_task.await.unwrap();
    }
}
