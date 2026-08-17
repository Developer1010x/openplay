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
/// Device name advertised to the receiver.
const OPENPLAY_DEVICE_NAME: &str = "OpenPlay";

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
) -> Result<NegotiatedStream, AirPlayError> {
    let mut stream = TcpStream::connect(addr)
        .await
        .map_err(|e| AirPlayError::Connection(format!("Failed to connect to {addr}: {e}")))?;

    info!(%addr, "Connected to AirPlay receiver");

    // Step 1: GET /info
    let server_info = get_info(&mut stream, session_id).await?;
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
    post_stream(&mut stream, width, height, fps, session_id).await?;

    Ok(NegotiatedStream {
        stream,
        server_info,
    })
}

/// Sends GET /info with proper AirPlay headers and parses the binary plist response.
async fn get_info(stream: &mut TcpStream, session_id: &str) -> Result<ServerInfo, AirPlayError> {
    let request = format!(
        "GET /info HTTP/1.1\r\n\
         User-Agent: {AIRPLAY_USER_AGENT}\r\n\
         X-Apple-Device-Name: {OPENPLAY_DEVICE_NAME}\r\n\
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
) -> Result<(), AirPlayError> {
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
         X-Apple-Device-Name: {OPENPLAY_DEVICE_NAME}\r\n\
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

    // Read response status
    let (status, _body) = read_http_response(stream).await?;
    if !status.contains("200") {
        return Err(AirPlayError::Negotiation(format!(
            "POST /stream failed: {status}"
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
) -> Result<(String, Vec<u8>), AirPlayError> {
    let request = format!(
        "GET /info HTTP/1.1\r\n\
         User-Agent: {AIRPLAY_USER_AGENT}\r\n\
         X-Apple-Device-Name: {OPENPLAY_DEVICE_NAME}\r\n\
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
pub async fn post_stream_on(
    stream: &mut TcpStream,
    width: u32,
    height: u32,
    fps: u32,
    session_id: &str,
) -> Result<(), AirPlayError> {
    post_stream(stream, width, height, fps, session_id).await
}
