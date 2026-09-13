use std::collections::BTreeMap;
use std::net::SocketAddr;

use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use crate::features::AirPlayFeatures;
use crate::AirPlayError;

/// AirPlay client version string — must match a known AirPlay version to avoid
/// rejection by third-party receivers (LG TVs, Samsung TVs, NOVO boards, etc.).
const AIRPLAY_USER_AGENT: &str = "AirPlay/550.10";
/// Device name sent to the receiver when the caller has no usable one.
pub const DEFAULT_DEVICE_NAME: &str = "OpenPlay";

/// Makes a display name safe to send as the `X-Apple-Device-Name` header.
///
/// The name comes from `config.toml` or `--name`. An HTTP header ends at the
/// first line break, so a name containing one would cut the request short and
/// let the rest of the name be read as further headers. Control characters
/// are dropped, surrounding whitespace is trimmed, and a name with nothing
/// left falls back to [`DEFAULT_DEVICE_NAME`] rather than sending an empty
/// header. Anything else — including non-ASCII, which Apple's own senders
/// use — is kept as typed.
pub fn header_safe_device_name(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        DEFAULT_DEVICE_NAME.to_string()
    } else {
        cleaned.to_string()
    }
}

/// Result of AirPlay HTTP negotiation.
pub struct NegotiatedStream {
    /// The TCP stream, now in binary mirror mode.
    pub stream: TcpStream,
    /// Server info from GET /info.
    pub server_info: ServerInfo,
}

/// Parsed server info from AirPlay GET /info response.
#[derive(Debug, Clone, Default)]
pub struct ServerInfo {
    /// Device model (e.g. "AppleTV5,3", "LG Smart TV", etc.).
    pub model: String,
    /// Device name.
    pub device_name: String,
    /// Feature bitmask.
    pub features: AirPlayFeatures,
    /// Source version string.
    pub source_version: String,
    /// MAC address.
    pub mac_address: String,
}

/// Performs AirPlay HTTP negotiation with a receiver.
///
/// 1. GET /info — query capabilities
/// 2. POST /stream — send binary plist with mirror parameters, transitions connection to raw binary
///
/// Includes proper AirPlay headers required by third-party receivers (LG, Samsung, NOVO boards).
/// The mirroring capability check is lenient: if the device returns features=0 or unknown,
/// we proceed anyway rather than rejecting — many commercial and education displays don't
/// populate the features bitmask correctly.
pub async fn negotiate(
    addr: SocketAddr,
    width: u32,
    height: u32,
    fps: u32,
    session_id: &str,
    device_name: &str,
) -> Result<NegotiatedStream, AirPlayError> {
    let mut stream = TcpStream::connect(addr)
        .await
        .map_err(|e| AirPlayError::Connection(format!("Failed to connect to {addr}: {e}")))?;

    info!(%addr, "Connected to AirPlay receiver");

    // Step 1: GET /info
    let server_info = get_info(&mut stream, session_id, device_name).await?;
    info!(
        model = %server_info.model,
        name = %server_info.device_name,
        features = server_info.features.raw(),
        "AirPlay receiver info"
    );

    // Lenient mirroring check: only reject if features are explicitly non-zero AND
    // mirroring is absent. Many third-party receivers (LG, NOVO, etc.) return features=0
    // or skip the /info response body entirely — we should still attempt mirroring.
    if server_info.features.raw() != 0 && !server_info.features.supports_mirroring() {
        warn!(
            model = %server_info.model,
            features = server_info.features.raw(),
            "Receiver does not report mirroring support (bit 7 unset) — \
             attempting anyway as this is common with third-party AirPlay devices"
        );
    }

    // Step 2: POST /stream
    post_stream(&mut stream, width, height, fps, session_id, device_name).await?;

    Ok(NegotiatedStream {
        stream,
        server_info,
    })
}

/// Sends GET /info with proper AirPlay headers and parses the binary plist response.
async fn get_info(
    stream: &mut TcpStream,
    session_id: &str,
    device_name: &str,
) -> Result<ServerInfo, AirPlayError> {
    let device_name = header_safe_device_name(device_name);
    let request = format!(
        "GET /info HTTP/1.1\r\n\
         User-Agent: {AIRPLAY_USER_AGENT}\r\n\
         X-Apple-Device-Name: {device_name}\r\n\
         X-Apple-Session-ID: {session_id}\r\n\
         X-Apple-ProtocolVersion: 1\r\n\
         Content-Length: 0\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| AirPlayError::Http(format!("Failed to send GET /info: {e}")))?;

    let (headers, body) = read_http_response(stream).await?;
    debug!(status = %headers, body_len = body.len(), "GET /info response");

    parse_info_response(&body)
}

/// Sends POST /stream with binary plist body and proper AirPlay headers.
async fn post_stream(
    stream: &mut TcpStream,
    width: u32,
    height: u32,
    fps: u32,
    session_id: &str,
    device_name: &str,
) -> Result<(), AirPlayError> {
    let device_name = header_safe_device_name(device_name);
    let mut params = BTreeMap::new();
    params.insert("width".to_string(), plist::Value::Integer(width.into()));
    params.insert("height".to_string(), plist::Value::Integer(height.into()));
    params.insert("fps".to_string(), plist::Value::Integer(fps.into()));
    params.insert("overscanned".to_string(), plist::Value::Boolean(false));
    params.insert("refreshRate".to_string(), plist::Value::Real(fps as f64));
    params.insert(
        "sessionID".to_string(),
        plist::Value::String(session_id.to_string()),
    );
    params.insert(
        "version".to_string(),
        plist::Value::String("1.0".to_string()),
    );

    let plist_value = plist::Value::Dictionary(params.into_iter().collect());
    let mut body = Vec::new();
    plist_value
        .to_writer_binary(&mut body)
        .map_err(|e| AirPlayError::Plist(format!("Failed to encode plist: {e}")))?;

    let request = format!(
        "POST /stream HTTP/1.1\r\n\
         User-Agent: {AIRPLAY_USER_AGENT}\r\n\
         X-Apple-Device-Name: {device_name}\r\n\
         X-Apple-Session-ID: {session_id}\r\n\
         X-Apple-ProtocolVersion: 1\r\n\
         Content-Type: application/x-apple-binary-plist\r\n\
         Content-Length: {}\r\n\r\n",
        body.len()
    );

    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| AirPlayError::Http(format!("Failed to send POST /stream header: {e}")))?;
    stream
        .write_all(&body)
        .await
        .map_err(|e| AirPlayError::Http(format!("Failed to send POST /stream body: {e}")))?;

    // Read response status.
    //
    // The status has to be read off the first line and nowhere else: the whole
    // header block of a 500 can contain "200" three times over, and treating
    // that as success would hand the caller a failed connection to write video
    // into. The status line is also what the caller's retry logic parses, so it
    // has to lead the message.
    let (headers, _body) = read_http_response(stream).await?;
    if !status_is_success(&headers) {
        let status_line = headers.lines().next().unwrap_or("<no status line>");
        return Err(AirPlayError::Negotiation(format!(
            "POST /stream failed: {status_line}"
        )));
    }

    info!("POST /stream accepted — mirror stream active");
    Ok(())
}

/// Reads an HTTP response (headers + body).
async fn read_http_response(stream: &mut TcpStream) -> Result<(String, Vec<u8>), AirPlayError> {
    let mut buf = BytesMut::with_capacity(4096);

    // Read until we find \r\n\r\n
    let header_end = loop {
        let n = stream
            .read_buf(&mut buf)
            .await
            .map_err(|e| AirPlayError::Http(format!("Read error: {e}")))?;
        if n == 0 {
            return Err(AirPlayError::Http(
                "Connection closed during HTTP response".to_string(),
            ));
        }

        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }

        if buf.len() > 64 * 1024 {
            return Err(AirPlayError::Http("HTTP headers too large".to_string()));
        }
    };
    let headers_bytes = &buf[..header_end];
    let headers = String::from_utf8_lossy(headers_bytes).to_string();

    // Parse Content-Length
    let content_length = parse_content_length(&headers).unwrap_or(0);

    // Body starts after \r\n\r\n
    let body_start = header_end + 4;
    let already_read = buf.len().saturating_sub(body_start);

    let mut body = Vec::with_capacity(content_length);
    if body_start < buf.len() {
        body.extend_from_slice(&buf[body_start..]);
    }

    // Read remaining body
    if already_read < content_length {
        let remaining = content_length - already_read;
        let mut rest = vec![0u8; remaining];
        stream
            .read_exact(&mut rest)
            .await
            .map_err(|e| AirPlayError::Http(format!("Failed to read body: {e}")))?;
        body.extend_from_slice(&rest);
    }

    Ok((headers, body))
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Whether a response's **status line** reports success.
///
/// `headers` is the whole header block, so the status has to be read off the
/// first line and nowhere else. This was `headers.contains("200")`, which every
/// one of `Content-Length: 1200`, `Server: AirTunes/200.20` and a `Date`
/// containing "200" satisfies — so a 500 or a 403 was accepted as a working
/// mirror session, and the failure surfaced later as an unexplained stall.
///
/// `hap_pairing::check_http_status` already parsed the status line correctly;
/// this is the same parse, kept separate only because the two report failures
/// differently.
///
/// An unparseable first line is not success: if the status cannot be read there
/// is nothing to justify sending video down the connection.
fn status_is_success(headers: &str) -> bool {
    headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .is_some_and(|code| (200..300).contains(&code))
}

fn parse_content_length(headers: &str) -> Option<usize> {
    for line in headers.lines() {
        if let Some(val) = line
            .strip_prefix("Content-Length:")
            .or_else(|| line.strip_prefix("content-length:"))
        {
            return val.trim().parse().ok();
        }
    }
    None
}

/// Public alias for use in FairPlay negotiation flow.
pub fn parse_info_response_pub(body: &[u8]) -> Result<ServerInfo, AirPlayError> {
    parse_info_response(body)
}

fn parse_info_response(body: &[u8]) -> Result<ServerInfo, AirPlayError> {
    if body.is_empty() {
        // Many third-party AirPlay receivers (LG, NOVO, etc.) return empty /info bodies.
        // Return a default ServerInfo rather than failing — we still try to mirror.
        debug!("GET /info returned empty body — using default ServerInfo (third-party device)");
        return Ok(ServerInfo::default());
    }

    let value: plist::Value = match plist::from_bytes(body) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse /info plist: {e} — using default ServerInfo");
            return Ok(ServerInfo::default());
        }
    };

    let dict = value
        .as_dictionary()
        .ok_or_else(|| AirPlayError::Plist("Expected dictionary in /info response".to_string()))?;

    let get_str = |key: &str| -> String {
        dict.get(key)
            .and_then(|v| v.as_string())
            .unwrap_or("")
            .to_string()
    };

    let features_str = get_str("features");
    let features = if features_str.is_empty() {
        dict.get("features")
            .and_then(|v| v.as_unsigned_integer())
            .and_then(|v| AirPlayFeatures::parse(&format!("0x{v:X}")))
            .unwrap_or_default()
    } else {
        // A receiver advertising a malformed `features` string is treated as
        // advertising nothing, rather than failing the whole /info parse.
        AirPlayFeatures::parse(&features_str).unwrap_or_default()
    };

    Ok(ServerInfo {
        model: get_str("model"),
        device_name: get_str("deviceName"),
        features,
        source_version: get_str("sourceVersion"),
        mac_address: get_str("macAddress"),
    })
}

/// Sends a GET /info request with AirPlay headers and returns raw headers + body.
/// Used by the auth flow in session.rs.
pub async fn get_info_raw(
    stream: &mut TcpStream,
    session_id: &str,
    device_name: &str,
) -> Result<(String, Vec<u8>), AirPlayError> {
    let device_name = header_safe_device_name(device_name);
    let request = format!(
        "GET /info HTTP/1.1\r\n\
         User-Agent: {AIRPLAY_USER_AGENT}\r\n\
         X-Apple-Device-Name: {device_name}\r\n\
         X-Apple-Session-ID: {session_id}\r\n\
         X-Apple-ProtocolVersion: 1\r\n\
         Content-Length: 0\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| AirPlayError::Http(format!("Failed to send GET /info: {e}")))?;
    read_http_response(stream).await
}

/// Sends POST /stream with AirPlay headers on an already-open stream.
/// Used by the auth flow after pair-verify.
/// Builds a complete `POST /stream` request — headers and binary-plist body.
///
/// Split out so the same request can be sent either straight down a socket or
/// wrapped in encrypted control-channel frames after a transient pair-setup.
pub fn build_stream_request(
    width: u32,
    height: u32,
    fps: u32,
    session_id: &str,
    device_name: &str,
) -> Result<Vec<u8>, AirPlayError> {
    let device_name = header_safe_device_name(device_name);
    let mut params = BTreeMap::new();
    params.insert("width".to_string(), plist::Value::Integer(width.into()));
    params.insert("height".to_string(), plist::Value::Integer(height.into()));
    params.insert("fps".to_string(), plist::Value::Integer(fps.into()));
    params.insert("overscanned".to_string(), plist::Value::Boolean(false));
    params.insert("refreshRate".to_string(), plist::Value::Real(fps as f64));
    params.insert(
        "sessionID".to_string(),
        plist::Value::String(session_id.to_string()),
    );
    params.insert(
        "version".to_string(),
        plist::Value::String("1.0".to_string()),
    );

    let plist_value = plist::Value::Dictionary(params.into_iter().collect());
    let mut body = Vec::new();
    plist_value
        .to_writer_binary(&mut body)
        .map_err(|e| AirPlayError::Plist(format!("Failed to encode plist: {e}")))?;

    let mut request = format!(
        "POST /stream HTTP/1.1\r\n\
         User-Agent: {AIRPLAY_USER_AGENT}\r\n\
         X-Apple-Device-Name: {device_name}\r\n\
         X-Apple-Session-ID: {session_id}\r\n\
         X-Apple-ProtocolVersion: 1\r\n\
         Content-Type: application/x-apple-binary-plist\r\n\
         Content-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(&body);
    Ok(request)
}

pub async fn post_stream_on(
    stream: &mut TcpStream,
    width: u32,
    height: u32,
    fps: u32,
    session_id: &str,
    device_name: &str,
) -> Result<(), AirPlayError> {
    post_stream(stream, width, height, fps, session_id, device_name).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything before the blank line that ends the headers.
    fn header_block(request: &[u8]) -> String {
        let text = String::from_utf8_lossy(request);
        text.split("\r\n\r\n").next().unwrap_or("").to_string()
    }

    #[test]
    fn stream_request_carries_the_configured_device_name() {
        let request = build_stream_request(1920, 1080, 30, "S1", "Nikhil's Laptop").unwrap();
        let headers = header_block(&request);
        assert!(
            headers.contains("X-Apple-Device-Name: Nikhil's Laptop\r\n"),
            "{headers}"
        );
    }

    #[test]
    fn a_line_break_in_the_name_cannot_add_a_header() {
        let request =
            build_stream_request(1920, 1080, 30, "S1", "Laptop\r\nX-Injected: yes").unwrap();
        let headers = header_block(&request);
        assert!(!headers.contains("\r\nX-Injected"), "{headers}");
        assert!(
            headers.contains("X-Apple-Device-Name: LaptopX-Injected: yes\r\n"),
            "{headers}"
        );
    }

    #[test]
    fn a_blank_name_falls_back_to_the_default() {
        assert_eq!(header_safe_device_name(""), DEFAULT_DEVICE_NAME);
        assert_eq!(header_safe_device_name("   "), DEFAULT_DEVICE_NAME);
        assert_eq!(header_safe_device_name("\r\n\t"), DEFAULT_DEVICE_NAME);
    }

    #[test]
    fn whitespace_is_trimmed_and_non_ascii_is_kept() {
        assert_eq!(
            header_safe_device_name("  Nikhil’s Laptop  "),
            "Nikhil’s Laptop"
        );
    }

    /// A real 500, with three separate headers that contain "200".
    const FIVE_HUNDRED: &str = "HTTP/1.1 500 Internal Server Error\r\n\
         Server: AirTunes/200.20\r\n\
         Date: Mon, 06 Jan 2003 12:00:14 GMT\r\n\
         Content-Length: 1200";

    #[test]
    fn a_failure_status_is_rejected_however_the_headers_read() {
        assert!(
            !status_is_success(FIVE_HUNDRED),
            "the status line says 500; only the status line counts"
        );

        // The check this replaced. Kept as an assertion rather than a comment
        // so the reason for the rewrite cannot quietly stop being true.
        assert!(
            FIVE_HUNDRED.contains("200"),
            "substring matching accepted this response as success"
        );

        assert!(!status_is_success(
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0"
        ));
        assert!(!status_is_success(
            "HTTP/1.1 501 Not Implemented\r\nContent-Length: 200"
        ));
    }

    #[test]
    fn a_success_status_is_accepted() {
        assert!(status_is_success(
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n"
        ));
        assert!(status_is_success("HTTP/1.1 204 No Content"));
        assert!(status_is_success("RTSP/1.0 200 OK\r\nCSeq: 1"));
    }

    #[test]
    fn an_unreadable_status_line_is_not_success() {
        assert!(!status_is_success(""));
        assert!(!status_is_success("garbage\r\nContent-Length: 200"));
    }

    /// `session.rs` decides whether to retry with HAP pairing by looking for
    /// "501"/"403" in this error text, so the status line has to survive into it.
    #[test]
    fn the_rejection_message_keeps_the_status_code() {
        let status_line = FIVE_HUNDRED.lines().next().unwrap();
        assert!(status_line.contains("500"));
        assert!(
            !status_line.contains("1200"),
            "the Content-Length must not reach the auth-fallback match"
        );
    }
}
