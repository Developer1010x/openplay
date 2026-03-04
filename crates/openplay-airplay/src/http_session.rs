use std::collections::BTreeMap;
use std::net::SocketAddr;

use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info};

use crate::features::AirPlayFeatures;
use crate::AirPlayError;

/// Result of AirPlay HTTP negotiation.
pub struct NegotiatedStream {
    /// The TCP stream, now in binary mirror mode.
    pub stream: TcpStream,
    /// Server info from GET /info.
    pub server_info: ServerInfo,
}

/// Parsed server info from AirPlay GET /info response.
#[derive(Debug, Clone)]
pub struct ServerInfo {
    /// Device model (e.g. "AppleTV5,3").
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

impl Default for ServerInfo {
    fn default() -> Self {
        Self {
            model: String::new(),
            device_name: String::new(),
            features: AirPlayFeatures::default(),
            source_version: String::new(),
            mac_address: String::new(),
        }
    }
}

/// Performs AirPlay HTTP negotiation with a receiver.
///
/// 1. GET /info — query capabilities
/// 2. POST /stream — send binary plist with mirror parameters, transitions connection to raw binary
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
    let server_info = get_info(&mut stream).await?;
    info!(
        model = %server_info.model,
        name = %server_info.device_name,
        "AirPlay receiver info"
    );

    if !server_info.features.supports_mirroring() {
        return Err(AirPlayError::MirroringNotSupported);
    }

    // Step 2: POST /stream
    post_stream(&mut stream, width, height, fps, session_id).await?;

    Ok(NegotiatedStream {
        stream,
        server_info,
    })
}

/// Sends GET /info and parses the binary plist response.
async fn get_info(stream: &mut TcpStream) -> Result<ServerInfo, AirPlayError> {
    let request = "GET /info HTTP/1.1\r\nContent-Length: 0\r\n\r\n";
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| AirPlayError::Http(format!("Failed to send GET /info: {e}")))?;

    let (headers, body) = read_http_response(stream).await?;
    debug!(status = %headers, body_len = body.len(), "GET /info response");

    parse_info_response(&body)
}

/// Sends POST /stream with binary plist body.
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
    params.insert(
        "overscanned".to_string(),
        plist::Value::Boolean(false),
    );
    params.insert(
        "refreshRate".to_string(),
        plist::Value::Real(fps as f64),
    );
    params.insert(
        "sessionID".to_string(),
        plist::Value::String(session_id.to_string()),
    );
    params.insert("version".to_string(), plist::Value::String("1.0".to_string()));

    let plist_value = plist::Value::Dictionary(params.into_iter().collect());
    let mut body = Vec::new();
    plist_value
        .to_writer_binary(&mut body)
        .map_err(|e| AirPlayError::Plist(format!("Failed to encode plist: {e}")))?;

    let request = format!(
        "POST /stream HTTP/1.1\r\nContent-Type: application/x-apple-binary-plist\r\nContent-Length: {}\r\n\r\n",
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
            return Err(AirPlayError::Http("Connection closed during HTTP response".to_string()));
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
    let already_read = buf.len() - body_start;

    let mut body = Vec::with_capacity(content_length);
    body.extend_from_slice(&buf[body_start..]);

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
        if let Some(val) = line.strip_prefix("Content-Length:").or_else(|| line.strip_prefix("content-length:")) {
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
        return Ok(ServerInfo::default());
    }

    let value: plist::Value = plist::from_bytes(body)
        .map_err(|e| AirPlayError::Plist(format!("Failed to parse /info plist: {e}")))?;

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
        // Try numeric features field
        dict.get("features")
            .and_then(|v| v.as_unsigned_integer())
            .map(|v| AirPlayFeatures::parse(&format!("0x{v:X}")))
            .unwrap_or_default()
    } else {
        AirPlayFeatures::parse(&features_str)
    };

    Ok(ServerInfo {
        model: get_str("model"),
        device_name: get_str("deviceName").to_string(),
        features,
        source_version: get_str("sourceVersion"),
        mac_address: get_str("macAddress"),
    })
}
