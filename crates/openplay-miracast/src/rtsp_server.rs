use std::net::SocketAddr;

use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info};

use crate::wfd_params::{WfdClientRtpPorts, WfdVideoFormats};
use crate::MiracastError;

/// Default WFD RTSP port.
pub const WFD_RTSP_PORT: u16 = 7236;

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

/// Performs WFD RTSP M1-M7 negotiation with a Miracast sink.
///
/// The source (us) drives the negotiation by sending RTSP requests
/// and the sink responds.
pub async fn negotiate(
    mut stream: TcpStream,
    sink_addr: SocketAddr,
    source_formats: &WfdVideoFormats,
) -> Result<WfdNegotiationResult, MiracastError> {
    let mut cseq: u32 = 0;
    // M1: Source sends OPTIONS
    cseq += 1;
    send_options(&mut stream, cseq).await?;
    let m1_resp = read_rtsp_response(&mut stream).await?;
    if !m1_resp.status.starts_with("200") {
        return Err(MiracastError::Rtsp(format!("M1 OPTIONS rejected: {}", m1_resp.status)));
    }
    debug!(state = ?WfdState::M1OptionsSent, "M1 complete");

    // M2: Wait for sink's OPTIONS request
    let m2_req = read_rtsp_request(&mut stream).await?;
    if m2_req.method != "OPTIONS" {
        return Err(MiracastError::Rtsp(format!("Expected M2 OPTIONS, got {}", m2_req.method)));
    }
    send_options_response(&mut stream, &m2_req).await?;
    debug!(state = ?WfdState::M2SinkOptions, "M2 complete");

    // M3: Source sends GET_PARAMETER to query sink capabilities
    cseq += 1;
    send_get_parameter(&mut stream, cseq).await?;
    let m3_resp = read_rtsp_response(&mut stream).await?;
    if !m3_resp.status.starts_with("200") {
        return Err(MiracastError::Rtsp(format!("M3 GET_PARAMETER rejected: {}", m3_resp.status)));
    }

    // Parse sink capabilities
    let sink_formats = parse_sink_video_formats(&m3_resp.body);
    let _sink_rtp_ports = parse_sink_rtp_ports(&m3_resp.body);

    // Negotiate resolution
    let (width, height, fps) = crate::wfd_params::negotiate_resolution(
        source_formats,
        &sink_formats.unwrap_or_default(),
    );
    info!(width, height, fps, "Negotiated resolution");

    // M4: Source sends SET_PARAMETER with negotiated params
    cseq += 1;
    let negotiated = WfdVideoFormats {
        cea_resolutions: crate::wfd_params::CeaResolutions(
            match (width, height, fps) {
                (1920, 1080, 60) => crate::wfd_params::CeaResolutions::RES_1920X1080_P60,
                (1920, 1080, _) => crate::wfd_params::CeaResolutions::RES_1920X1080_P30,
                (1280, 720, 60) => crate::wfd_params::CeaResolutions::RES_1280X720_P60,
                _ => crate::wfd_params::CeaResolutions::RES_1280X720_P30,
            }
        ),
        ..source_formats.clone()
    };
    send_set_parameter(&mut stream, cseq, &negotiated).await?;
    let m4_resp = read_rtsp_response(&mut stream).await?;
    if !m4_resp.status.starts_with("200") {
        return Err(MiracastError::Rtsp(format!("M4 SET_PARAMETER rejected: {}", m4_resp.status)));
    }
    debug!(state = ?WfdState::M4SetParameter, "M4 complete");

    // M5: Source sends SET_PARAMETER with trigger SETUP
    cseq += 1;
    send_trigger_setup(&mut stream, cseq).await?;
    let m5_resp = read_rtsp_response(&mut stream).await?;
    if !m5_resp.status.starts_with("200") {
        return Err(MiracastError::Rtsp(format!("M5 trigger rejected: {}", m5_resp.status)));
    }
    debug!(state = ?WfdState::M5Trigger, "M5 complete");

    // M6: Wait for sink's SETUP request
    let m6_req = read_rtsp_request(&mut stream).await?;
    if m6_req.method != "SETUP" {
        return Err(MiracastError::Rtsp(format!("Expected M6 SETUP, got {}", m6_req.method)));
    }
    let rtp_port = parse_transport_port(&m6_req.headers).unwrap_or(1028);
    send_setup_response(&mut stream, &m6_req, rtp_port).await?;
    debug!(state = ?WfdState::M6Setup, rtp_port, "M6 complete");

    // M7: Wait for sink's PLAY request
    let m7_req = read_rtsp_request(&mut stream).await?;
    if m7_req.method != "PLAY" {
        return Err(MiracastError::Rtsp(format!("Expected M7 PLAY, got {}", m7_req.method)));
    }
    send_play_response(&mut stream, &m7_req).await?;
    info!(state = ?WfdState::M7Play, "WFD negotiation complete — ready to stream");

    Ok(WfdNegotiationResult {
        width,
        height,
        fps,
        rtp_port,
        sink_addr,
    })
}

// --- RTSP message types ---

#[allow(dead_code)]
struct RtspResponse {
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

async fn send_options(stream: &mut TcpStream, cseq: u32) -> Result<(), MiracastError> {
    let msg = format!(
        "OPTIONS * RTSP/1.0\r\nCSeq: {cseq}\r\nRequire: org.wfa.wfd1.0\r\n\r\n"
    );
    stream.write_all(msg.as_bytes()).await.map_err(|e| MiracastError::Rtsp(format!("Send M1: {e}")))
}

async fn send_options_response(stream: &mut TcpStream, req: &RtspRequest) -> Result<(), MiracastError> {
    let msg = format!(
        "RTSP/1.0 200 OK\r\nCSeq: {}\r\nPublic: org.wfa.wfd1.0, GET_PARAMETER, SET_PARAMETER\r\n\r\n",
        req.cseq
    );
    stream.write_all(msg.as_bytes()).await.map_err(|e| MiracastError::Rtsp(format!("Send M2 response: {e}")))
}

async fn send_get_parameter(stream: &mut TcpStream, cseq: u32) -> Result<(), MiracastError> {
    let body = "wfd_video_formats\r\nwfd_audio_codecs\r\nwfd_client_rtp_ports\r\n";
    let msg = format!(
        "GET_PARAMETER rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: {cseq}\r\nContent-Type: text/parameters\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(msg.as_bytes()).await.map_err(|e| MiracastError::Rtsp(format!("Send M3: {e}")))
}

async fn send_set_parameter(
    stream: &mut TcpStream,
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
    stream.write_all(msg.as_bytes()).await.map_err(|e| MiracastError::Rtsp(format!("Send M4: {e}")))
}

async fn send_trigger_setup(stream: &mut TcpStream, cseq: u32) -> Result<(), MiracastError> {
    let body = "wfd_trigger_method: SETUP\r\n";
    let msg = format!(
        "SET_PARAMETER rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: {cseq}\r\nContent-Type: text/parameters\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(msg.as_bytes()).await.map_err(|e| MiracastError::Rtsp(format!("Send M5: {e}")))
}

async fn send_setup_response(
    stream: &mut TcpStream,
    req: &RtspRequest,
    rtp_port: u16,
) -> Result<(), MiracastError> {
    let msg = format!(
        "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: 1;timeout=30\r\nTransport: RTP/AVP/UDP;unicast;client_port={rtp_port};server_port={rtp_port}\r\n\r\n",
        req.cseq
    );
    stream.write_all(msg.as_bytes()).await.map_err(|e| MiracastError::Rtsp(format!("Send M6 response: {e}")))
}

async fn send_play_response(stream: &mut TcpStream, req: &RtspRequest) -> Result<(), MiracastError> {
    let msg = format!(
        "RTSP/1.0 200 OK\r\nCSeq: {}\r\nSession: 1\r\n\r\n",
        req.cseq
    );
    stream.write_all(msg.as_bytes()).await.map_err(|e| MiracastError::Rtsp(format!("Send M7 response: {e}")))
}

// --- Read helpers ---

async fn read_rtsp_response(stream: &mut TcpStream) -> Result<RtspResponse, MiracastError> {
    let raw = read_rtsp_message(stream).await?;
    let (header_part, body) = split_header_body(&raw);
    let status = header_part
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    Ok(RtspResponse {
        status,
        headers: header_part.to_string(),
        body,
    })
}

async fn read_rtsp_request(stream: &mut TcpStream) -> Result<RtspRequest, MiracastError> {
    let raw = read_rtsp_message(stream).await?;
    let (header_part, body) = split_header_body(&raw);
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

    Ok(RtspRequest {
        method,
        uri,
        headers: header_part.to_string(),
        body,
        cseq,
    })
}

async fn read_rtsp_message(stream: &mut TcpStream) -> Result<String, MiracastError> {
    let mut buf = BytesMut::with_capacity(4096);

    loop {
        let n = stream
            .read_buf(&mut buf)
            .await
            .map_err(|e| MiracastError::Rtsp(format!("Read error: {e}")))?;
        if n == 0 {
            return Err(MiracastError::Rtsp("Connection closed".to_string()));
        }

        let s = String::from_utf8_lossy(&buf);
        if let Some(header_end) = s.find("\r\n\r\n") {
            // Check if we have the full body
            let header_part = &s[..header_end];
            let content_length = header_part
                .lines()
                .find(|l| l.to_lowercase().starts_with("content-length:"))
                .and_then(|l| l.split(':').nth(1))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);

            let body_start = header_end + 4;
            let body_len = s.len() - body_start;

            if body_len >= content_length {
                return Ok(s[..body_start + content_length].to_string());
            }
        }

        if buf.len() > 64 * 1024 {
            return Err(MiracastError::Rtsp("RTSP message too large".to_string()));
        }
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

fn parse_transport_port(headers: &str) -> Option<u16> {
    for line in headers.lines() {
        if let Some(transport) = line.strip_prefix("Transport:").or_else(|| line.strip_prefix("transport:")) {
            for part in transport.split(';') {
                if let Some(port_str) = part.strip_prefix("client_port=") {
                    return port_str.trim().parse().ok();
                }
            }
        }
    }
    None
}
