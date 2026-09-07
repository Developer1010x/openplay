use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gstreamer_app as gst_app;
use tokio::runtime::Handle as TokioHandle;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use openplay_airplay::session::{AirPlaySession, SessionEvent as AirPlayEvent};
use openplay_capture::CaptureSession;
use openplay_miracast::session::{MiracastSession, SessionEvent as MiracastEvent};
use openplay_pipeline::{
    probe_best_encoder, AirPlaySenderPipeline, CaptureConfig, EncoderType, MiracastSenderPipeline,
    Role, SdpKind, SenderPipeline, WebRtcEvent, WebRtcPeer,
};
use openplay_protocol::{
    Capabilities, RejectReason, Resolution, SessionEndReason, SignalingMessage,
};
use openplay_signaling::SignalingClient;
use url::Url;

/// Handle that allows signalling an active cast to stop.
#[derive(Clone)]
pub struct CastStopHandle {
    stop_flag: Arc<AtomicBool>,
}

impl CastStopHandle {
    pub fn new() -> Self {
        Self {
            stop_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }

    /// The read side of [`CastStopHandle::stop`]. The UI polls the shared flag
    /// via [`CastStopHandle::flag`] instead, so this is only exercised by the
    /// unit tests below.
    #[allow(dead_code)]
    pub fn is_stopped(&self) -> bool {
        self.stop_flag.load(Ordering::Relaxed)
    }

    pub fn flag(&self) -> Arc<AtomicBool> {
        self.stop_flag.clone()
    }
}

// ─── AirPlay ──────────────────────────────────────────────────────────────────

pub async fn start_airplay_cast(
    receiver_addr: SocketAddr,
    bitrate_kbps: u32,
    framerate: u32,
    force_sw_encode: bool,
    tokio_handle: TokioHandle,
    stop_handle: CastStopHandle,
    status_callback: impl Fn(&str) + 'static,
) {
    status_callback("Starting screen capture...");

    let capture = match CaptureSession::start().await {
        Ok(c) => c,
        Err(e) => {
            error!(%e, "Screen capture failed");
            status_callback(&format!("Capture failed: {e}"));
            return;
        }
    };

    let capture_config = make_capture_config(&capture, framerate);
    let width = capture_config.width;
    let height = capture_config.height;

    info!(width, height, "Capture session started");
    status_callback("Connecting to AirPlay receiver...");

    let stop_flag = stop_handle.flag();
    let result = tokio_handle
        .spawn(async move {
            run_airplay_pipeline(
                receiver_addr,
                capture_config,
                width,
                height,
                bitrate_kbps,
                force_sw_encode,
                stop_flag,
            )
            .await
        })
        .await;

    match result {
        Ok(Ok(())) => status_callback("Casting ended"),
        Ok(Err(e)) => {
            let msg = e.to_string();
            if msg.contains("stopped by user") {
                status_callback("Casting stopped");
            } else {
                error!(%e, "AirPlay casting failed");
                status_callback(&format!("Casting failed: {e}"));
            }
        }
        Err(e) => {
            if e.is_cancelled() {
                status_callback("Casting stopped");
            } else {
                error!(%e, "AirPlay task panicked");
                status_callback(&format!("Casting error: {e}"));
            }
        }
    }
}

// ─── Miracast (Infrastructure / MICE) ─────────────────────────────────────────

/// `sink_addr` carries the port the receiver was discovered or entered with.
/// This used to take a bare `IpAddr` and re-attach a hardcoded 7236, which
/// silently discarded the port from a manually added sink.
pub async fn start_miracast_cast(
    sink_addr: SocketAddr,
    bitrate_kbps: u32,
    framerate: u32,
    force_sw_encode: bool,
    tokio_handle: TokioHandle,
    stop_handle: CastStopHandle,
    status_callback: impl Fn(&str) + 'static,
) {
    status_callback("Starting screen capture...");

    let capture = match CaptureSession::start().await {
        Ok(c) => c,
        Err(e) => {
            error!(%e, "Screen capture failed");
            status_callback(&format!("Capture failed: {e}"));
            return;
        }
    };

    let capture_config = make_capture_config(&capture, framerate);
    let stop_flag = stop_handle.flag();

    status_callback("Connecting via Miracast...");

    let result = tokio_handle
        .spawn(async move {
            run_miracast_pipeline(
                sink_addr,
                capture_config,
                bitrate_kbps,
                force_sw_encode,
                stop_flag,
            )
            .await
        })
        .await;

    match result {
        Ok(Ok(())) => status_callback("Casting ended"),
        Ok(Err(e)) => {
            let msg = e.to_string();
            if msg.contains("stopped by user") {
                status_callback("Casting stopped");
            } else {
                error!(%e, "Miracast casting failed");
                status_callback(&format!("Casting failed: {e}"));
            }
        }
        Err(e) => {
            if e.is_cancelled() {
                status_callback("Casting stopped");
            } else {
                error!(%e, "Miracast task panicked");
                status_callback(&format!("Casting error: {e}"));
            }
        }
    }
}

// ─── Miracast P2P (Wi-Fi Direct, Linux only) ──────────────────────────────────

#[cfg(target_os = "linux")]
pub async fn start_miracast_p2p_cast(
    peer_mac: &str,
    bitrate_kbps: u32,
    framerate: u32,
    force_sw_encode: bool,
    tokio_handle: TokioHandle,
    stop_handle: CastStopHandle,
    status_callback: impl Fn(&str) + 'static,
) {
    status_callback("Starting screen capture...");

    let capture = match CaptureSession::start().await {
        Ok(c) => c,
        Err(e) => {
            error!(%e, "Screen capture failed");
            status_callback(&format!("Capture failed: {e}"));
            return;
        }
    };

    let capture_config = make_capture_config(&capture, framerate);
    let stop_flag = stop_handle.flag();
    let mac = peer_mac.to_string();

    status_callback("Forming Wi-Fi Direct P2P group...");

    let result = tokio_handle
        .spawn(async move {
            run_miracast_p2p_pipeline(
                &mac,
                capture_config,
                bitrate_kbps,
                force_sw_encode,
                stop_flag,
            )
            .await
        })
        .await;

    match result {
        Ok(Ok(())) => status_callback("Casting ended"),
        Ok(Err(e)) => {
            error!(%e, "P2P Miracast casting failed");
            status_callback(&format!("Casting failed: {e}"));
        }
        Err(e) => {
            error!(%e, "P2P Miracast task panicked");
            status_callback(&format!("Casting error: {e}"));
        }
    }
}

// ─── Internal pipeline runners ────────────────────────────────────────────────

/// Picks the H.264 encoder for a cast.
///
/// `force_sw_encode` comes straight from `config.toml` and exists to debug
/// hardware-encoder problems: it skips registry probing entirely rather than
/// probing and then discarding the result. Otherwise we probe, and fall back to
/// x264 if nothing hardware-accelerated is available.
fn select_encoder(force_sw_encode: bool) -> EncoderType {
    if force_sw_encode {
        info!("force_sw_encode is set, skipping hardware encoder probe");
        return EncoderType::X264;
    }

    probe_best_encoder().unwrap_or_else(|_| {
        warn!("No HW encoder found, falling back to x264");
        EncoderType::X264
    })
}

async fn run_airplay_pipeline(
    receiver_addr: SocketAddr,
    capture_config: CaptureConfig,
    width: u32,
    height: u32,
    bitrate_kbps: u32,
    force_sw_encode: bool,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let encoder_type = select_encoder(force_sw_encode);
    info!(
        encoder = encoder_type.factory_name(),
        "Using encoder for AirPlay"
    );

    let pipeline = AirPlaySenderPipeline::new(&capture_config, encoder_type, bitrate_kbps)?;

    let (frame_tx, mut frame_rx) = mpsc::channel::<Vec<u8>>(32);

    pipeline.appsink().set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |appsink| {
                let sample = appsink
                    .pull_sample()
                    .map_err(|_| gstreamer::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gstreamer::FlowError::Error)?;
                let map = buffer
                    .map_readable()
                    .map_err(|_| gstreamer::FlowError::Error)?;
                let data = map.as_slice().to_vec();
                let _ = frame_tx.try_send(data);
                Ok(gstreamer::FlowSuccess::Ok)
            })
            .build(),
    );

    info!(%receiver_addr, width, height, "Starting AirPlay session");
    let framerate = capture_config.framerate;
    let mut session = AirPlaySession::start(receiver_addr, width, height, framerate)
        .await
        .map_err(|e| anyhow::anyhow!("AirPlay session start failed: {e}"))?;

    match session.events().recv().await {
        Some(AirPlayEvent::Ready) => info!("AirPlay session ready"),
        Some(AirPlayEvent::Ended(Some(e))) => {
            return Err(anyhow::anyhow!("AirPlay setup failed: {e}"));
        }
        _ => return Err(anyhow::anyhow!("AirPlay session closed unexpectedly")),
    }

    pipeline.start()?;
    info!("AirPlay pipeline started — streaming");

    let mut sent_codec_data = false;
    let mut frame_count: u64 = 0;

    while let Some(data) = frame_rx.recv().await {
        if stop_flag.load(Ordering::Relaxed) {
            info!("AirPlay casting stopped by user");
            break;
        }
        if data.is_empty() {
            continue;
        }
        if !sent_codec_data {
            if let Some(sps_pps) = extract_sps_pps(&data) {
                if let Err(e) = session.send_codec_data(sps_pps).await {
                    error!(%e, "Failed to send codec data");
                    break;
                }
                sent_codec_data = true;
            }
        }
        if let Err(e) = session.send_video_frame(data).await {
            error!(%e, "Failed to send video frame");
            break;
        }
        frame_count += 1;
        // `is_multiple_of` is stable only since 1.87; the workspace MSRV is 1.80.
        if frame_count.is_multiple_of(300) {
            info!(frame_count, "AirPlay streaming...");
        }
    }

    info!(frame_count, "AirPlay casting ended");
    pipeline.stop()?;
    let _ = session.stop().await;
    Ok(())
}

async fn run_miracast_pipeline(
    sink_addr: SocketAddr,
    capture_config: CaptureConfig,
    bitrate_kbps: u32,
    force_sw_encode: bool,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let encoder_type = select_encoder(force_sw_encode);
    info!(
        encoder = encoder_type.factory_name(),
        "Using encoder for Miracast"
    );

    info!(%sink_addr, "Starting Miracast WFD negotiation");
    let mut session = MiracastSession::start(sink_addr)
        .await
        .map_err(|e| anyhow::anyhow!("Miracast session failed: {e}"))?;

    let (_width, _height, _fps, rtp_port, rtp_addr) = match session.events().recv().await {
        Some(MiracastEvent::Ready {
            width,
            height,
            fps,
            rtp_port,
            sink_addr,
        }) => {
            info!(
                width,
                height, fps, rtp_port, "Miracast negotiation complete"
            );
            (width, height, fps, rtp_port, sink_addr)
        }
        Some(MiracastEvent::Ended(Some(e))) => {
            return Err(anyhow::anyhow!("Miracast negotiation failed: {e}"));
        }
        _ => return Err(anyhow::anyhow!("Miracast session closed unexpectedly")),
    };

    let sink_ip = rtp_addr.ip().to_string();
    let pipeline = MiracastSenderPipeline::new(
        &capture_config,
        encoder_type,
        bitrate_kbps,
        &sink_ip,
        rtp_port,
    )?;

    pipeline.start()?;
    info!("Miracast pipeline started — streaming to {sink_ip}:{rtp_port}");

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            info!("Miracast casting stopped by user");
            break;
        }
        tokio::select! {
            event = session.events().recv() => {
                match event {
                    Some(MiracastEvent::Ended(err)) => {
                        if let Some(e) = err { warn!(%e, "Miracast ended with error"); }
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
        }
    }

    pipeline.stop()?;
    info!("Miracast casting ended");
    Ok(())
}

#[cfg(target_os = "linux")]
async fn run_miracast_p2p_pipeline(
    peer_mac: &str,
    capture_config: CaptureConfig,
    bitrate_kbps: u32,
    force_sw_encode: bool,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let encoder_type = select_encoder(force_sw_encode);
    info!(
        encoder = encoder_type.factory_name(),
        "Using encoder for P2P Miracast"
    );

    let mut session = MiracastSession::start_wifi_direct(peer_mac, 7236)
        .await
        .map_err(|e| anyhow::anyhow!("P2P Miracast session failed: {e}"))?;

    let (_w, _h, _fps, rtp_port, rtp_addr) = match session.events().recv().await {
        Some(MiracastEvent::Ready {
            width,
            height,
            fps,
            rtp_port,
            sink_addr,
        }) => (width, height, fps, rtp_port, sink_addr),
        Some(MiracastEvent::Ended(Some(e))) => {
            return Err(anyhow::anyhow!("P2P negotiation failed: {e}"));
        }
        _ => return Err(anyhow::anyhow!("P2P Miracast closed unexpectedly")),
    };

    let sink_ip = rtp_addr.ip().to_string();
    let pipeline = MiracastSenderPipeline::new(
        &capture_config,
        encoder_type,
        bitrate_kbps,
        &sink_ip,
        rtp_port,
    )?;
    pipeline.start()?;

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }
        tokio::select! {
            event = session.events().recv() => {
                match event {
                    Some(MiracastEvent::Ended(_)) | None => break,
                    _ => {}
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
        }
    }

    pipeline.stop()?;
    Ok(())
}

// ─── Capture config builder ────────────────────────────────────────────────────

fn make_capture_config(capture: &CaptureSession, framerate: u32) -> CaptureConfig {
    #[cfg(target_os = "linux")]
    {
        let node_id = capture.primary_source().map(|s| s.node_id).unwrap_or(0);
        let width = capture
            .primary_source()
            .and_then(|s| s.width)
            .unwrap_or(1920);
        let height = capture
            .primary_source()
            .and_then(|s| s.height)
            .unwrap_or(1080);
        CaptureConfig::new(capture.pipewire_fd(), node_id, width, height, framerate)
    }
    #[cfg(not(target_os = "linux"))]
    {
        CaptureConfig::new(capture.width(), capture.height(), framerate)
    }
}

// ─── H.264 NALU helpers ───────────────────────────────────────────────────────

fn extract_sps_pps(data: &[u8]) -> Option<Vec<u8>> {
    let mut sps: Option<&[u8]> = None;
    let mut pps: Option<&[u8]> = None;
    let nalu_positions = find_nalu_starts(data);

    for i in 0..nalu_positions.len() {
        let start = nalu_positions[i];
        let end = if i + 1 < nalu_positions.len() {
            nalu_positions[i + 1]
        } else {
            data.len()
        };
        let nalu = &data[start..end];
        let header_offset = if nalu.starts_with(&[0, 0, 0, 1]) {
            4
        } else if nalu.starts_with(&[0, 0, 1]) {
            3
        } else {
            continue;
        };
        if header_offset >= nalu.len() {
            continue;
        }
        match nalu[header_offset] & 0x1F {
            7 => sps = Some(nalu),
            8 => pps = Some(nalu),
            _ => {}
        }
    }

    match (sps, pps) {
        (Some(s), Some(p)) => {
            let mut result = Vec::with_capacity(s.len() + p.len());
            result.extend_from_slice(s);
            result.extend_from_slice(p);
            Some(result)
        }
        (Some(s), None) => Some(s.to_vec()),
        _ => None,
    }
}

fn find_nalu_starts(data: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                starts.push(i);
                i += 3;
                continue;
            } else if i + 3 < data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                starts.push(i);
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    starts
}

// ─── OpenPlay (WebRTC) ────────────────────────────────────────────────────────

/// Where to cast, the identity to hold the far end to, and who we say we are.
///
/// The fingerprint travels with the address because the two are only meaningful
/// together: the receiver serves a self-signed certificate, so the fingerprint
/// from its mDNS `fp` TXT key is the sole thing distinguishing it from any
/// other host that answers on that address.
#[derive(Debug, Clone)]
pub struct OpenPlayTarget {
    pub addr: SocketAddr,
    pub fingerprint: String,
    /// Shown in the approval prompt on the receiver's screen.
    ///
    /// This is the whole basis on which somebody decides to allow the cast, so
    /// it comes from the user's configured display name rather than being
    /// derived here — "OpenPlay Sender" for every device would make the
    /// decision meaningless.
    pub display_name: String,
}

/// Casts to an OpenPlay receiver over WebRTC.
pub async fn start_openplay_cast(
    target: OpenPlayTarget,
    bitrate_kbps: u32,
    framerate: u32,
    force_sw_encode: bool,
    tokio_handle: TokioHandle,
    stop_handle: CastStopHandle,
    status_callback: impl Fn(&str) + 'static,
) {
    status_callback("Starting screen capture...");

    let capture = match CaptureSession::start().await {
        Ok(c) => c,
        Err(e) => {
            error!(%e, "Screen capture failed");
            status_callback(&format!("Capture failed: {e}"));
            return;
        }
    };

    let capture_config = make_capture_config(&capture, framerate);
    info!(
        width = capture_config.width,
        height = capture_config.height,
        "Capture session started"
    );
    status_callback("Connecting to OpenPlay receiver...");

    let stop_flag = stop_handle.flag();
    let result = tokio_handle
        .spawn(async move {
            run_openplay_pipeline(
                target,
                capture_config,
                bitrate_kbps,
                force_sw_encode,
                stop_flag,
            )
            .await
        })
        .await;

    // Every branch must end in a message containing one of "ended", "failed",
    // "stopped" or "error": that substring is how the UI learns the cast is
    // over, and without it the Stop button never goes away.
    match result {
        Ok(Ok(())) => status_callback("Casting ended"),
        Ok(Err(e)) => {
            let msg = e.to_string();
            if msg.contains("stopped by user") {
                status_callback("Casting stopped");
            } else {
                error!(%e, "OpenPlay casting failed");
                status_callback(&format!("Casting failed: {e}"));
            }
        }
        Err(e) => {
            if e.is_cancelled() {
                status_callback("Casting stopped");
            } else {
                error!(%e, "OpenPlay task panicked");
                status_callback(&format!("Casting error: {e}"));
            }
        }
    }
}

async fn run_openplay_pipeline(
    target: OpenPlayTarget,
    capture_config: CaptureConfig,
    bitrate_kbps: u32,
    force_sw_encode: bool,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let encoder_type = select_encoder(force_sw_encode);
    info!(
        encoder = encoder_type.factory_name(),
        "Using encoder for OpenPlay"
    );

    let tls_config = openplay_crypto::client_config_pinned(&target.fingerprint)
        .map_err(|e| anyhow::anyhow!("Receiver certificate is not usable: {e}"))?;

    let addr = target.addr;
    let url = Url::parse(&format!("wss://{addr}"))
        .map_err(|e| anyhow::anyhow!("Bad receiver address {addr}: {e}"))?;

    let (outgoing, mut incoming) = SignalingClient::new(url, tls_config)
        .connect()
        .await
        .map_err(|e| anyhow::anyhow!("Could not reach the receiver: {e}"))?;

    // Ask for a session and wait to be let in. The receiver shows the user a
    // prompt, so this can sit here for as long as it takes somebody to walk
    // over and press a button — there is deliberately no timeout.
    outgoing
        .send(SignalingMessage::SessionRequest {
            sender_id: sender_id(),
            display_name: sanitise_display_name(&target.display_name),
            protocol_version: openplay_common::PROTOCOL_VERSION,
            capabilities: Capabilities {
                video_codecs: vec!["h264".to_string()],
                audio_codecs: vec![],
                max_resolution: Some(Resolution {
                    width: capture_config.width,
                    height: capture_config.height,
                }),
                max_framerate: Some(capture_config.framerate),
                supports_cursor: true,
            },
        })
        .await
        .map_err(|_| anyhow::anyhow!("Signaling closed before the session request was sent"))?;

    wait_for_session_accept(&mut incoming, &stop_flag).await?;

    // Only now is it worth touching the GPU.
    let pipeline = SenderPipeline::new(&capture_config, encoder_type, bitrate_kbps)?;
    if let Err(e) = pipeline.setup_bus_watch(openplay_pipeline::log_bus_message) {
        warn!(%e, "Could not watch the sender pipeline bus");
    }

    let (webrtc_tx, mut webrtc_rx) = mpsc::unbounded_channel::<WebRtcEvent>();
    let peer = WebRtcPeer::new(pipeline.webrtcbin(), Role::Offerer, webrtc_tx);

    // Playing the pipeline is what makes webrtcbin ask for negotiation, which
    // produces the offer.
    pipeline.start()?;
    info!("OpenPlay pipeline started — negotiating");

    let outcome = drive_session(&peer, &outgoing, &mut incoming, &mut webrtc_rx, &stop_flag).await;

    let _ = outgoing
        .send(SignalingMessage::SessionEnd {
            reason: SessionEndReason::UserStopped,
        })
        .await;
    pipeline.stop()?;

    outcome
}

/// Waits for the receiver to accept the session, or for the user to give up.
///
/// The receiver asks a human before answering, so the only bounds here are the
/// user at the far end and the Stop button at this one.
async fn wait_for_session_accept(
    incoming: &mut mpsc::Receiver<SignalingMessage>,
    stop_flag: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    loop {
        if stop_flag.load(Ordering::Relaxed) {
            return Err(anyhow::anyhow!("Casting stopped by user"));
        }

        tokio::select! {
            message = incoming.recv() => match message {
                Some(SignalingMessage::SessionAccept { receiver_id, negotiated }) => {
                    info!(%receiver_id, codec = %negotiated.video_codec, "Session accepted");
                    return Ok(());
                }
                Some(SignalingMessage::SessionReject { reason }) => {
                    return Err(anyhow::anyhow!("{}", describe_rejection(&reason)));
                }
                Some(other) => {
                    debug!(msg = ?std::mem::discriminant(&other), "Ignoring pre-session message");
                }
                None => return Err(anyhow::anyhow!("Receiver closed the connection")),
            },
            // Poll the stop flag on a timer as well as on message arrival: a
            // receiver waiting on its consent prompt sends nothing at all, and
            // Stop has to work during that silence.
            _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
        }
    }
}

/// Turns a rejection into something worth showing a person.
fn describe_rejection(reason: &RejectReason) -> String {
    match reason {
        RejectReason::Busy => "Receiver is already showing another device".to_string(),
        RejectReason::VersionMismatch => {
            "Receiver runs a different OpenPlay version — update both ends".to_string()
        }
        RejectReason::NoCompatibleCodecs => "Receiver does not support H.264".to_string(),
        RejectReason::NotPaired => "This device is not paired with the receiver".to_string(),
        RejectReason::Denied => "The person at the receiver declined the cast".to_string(),
    }
}

/// Pumps SDP and ICE in both directions until the cast ends.
async fn drive_session(
    peer: &WebRtcPeer,
    outgoing: &mpsc::Sender<SignalingMessage>,
    incoming: &mut mpsc::Receiver<SignalingMessage>,
    webrtc_rx: &mut mpsc::UnboundedReceiver<WebRtcEvent>,
    stop_flag: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    loop {
        if stop_flag.load(Ordering::Relaxed) {
            info!("OpenPlay casting stopped by user");
            return Err(anyhow::anyhow!("Casting stopped by user"));
        }

        tokio::select! {
            event = webrtc_rx.recv() => {
                let Some(event) = event else {
                    return Err(anyhow::anyhow!("WebRTC event channel closed"));
                };
                match event {
                    WebRtcEvent::LocalDescription { kind, sdp } => {
                        if kind != SdpKind::Offer {
                            warn!(?kind, "Sender produced a description that is not an offer");
                            continue;
                        }
                        send_or_fail(outgoing, SignalingMessage::SdpOffer { sdp }).await?;
                    }
                    WebRtcEvent::IceCandidate { sdp_mline_index, candidate } => {
                        send_or_fail(outgoing, SignalingMessage::IceCandidate {
                            candidate,
                            sdp_mid: None,
                            sdp_mline_index: Some(sdp_mline_index),
                        }).await?;
                    }
                    WebRtcEvent::IceGatheringComplete => {
                        send_or_fail(outgoing, SignalingMessage::IceComplete).await?;
                    }
                    WebRtcEvent::Connected => info!("Media session connected — streaming"),
                    WebRtcEvent::Disconnected => return Ok(()),
                    WebRtcEvent::Failed(reason) => {
                        return Err(anyhow::anyhow!("Media session failed: {reason}"));
                    }
                }
            }

            message = incoming.recv() => match message {
                Some(SignalingMessage::SdpAnswer { sdp }) => {
                    peer.set_remote_description(SdpKind::Answer, &sdp)
                        .map_err(|e| anyhow::anyhow!("Receiver sent an unusable answer: {e}"))?;
                }
                Some(SignalingMessage::IceCandidate { candidate, sdp_mline_index, .. }) => {
                    peer.add_ice_candidate(sdp_mline_index.unwrap_or(0), &candidate);
                }
                Some(SignalingMessage::IceComplete) => {
                    debug!("Receiver finished gathering ICE candidates");
                }
                Some(SignalingMessage::SessionEnd { reason }) => {
                    info!(?reason, "Receiver ended the session");
                    return Ok(());
                }
                Some(other) => {
                    debug!(msg = ?std::mem::discriminant(&other), "Unhandled signaling message");
                }
                None => return Err(anyhow::anyhow!("Receiver closed the connection")),
            },

            _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
        }
    }
}

async fn send_or_fail(
    outgoing: &mpsc::Sender<SignalingMessage>,
    message: SignalingMessage,
) -> anyhow::Result<()> {
    outgoing
        .send(message)
        .await
        .map_err(|_| anyhow::anyhow!("Receiver closed the connection"))
}

/// Makes a configured name safe to put in a `SessionRequest`.
///
/// The receiver validates names and rejects the whole request if one is empty,
/// over-long, or carries control characters — so trimming here turns a
/// misconfigured name into a slightly shortened one rather than a cast that
/// fails with an unexplained rejection.
fn sanitise_display_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(openplay_protocol::MAX_NAME_CHARS)
        .collect();

    if cleaned.trim().is_empty() {
        "OpenPlay Sender".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_nalu_starts() {
        let data = [
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x00, 0x00, 0x01, 0x68, 0xce,
            0x38, 0x80,
        ];
        assert_eq!(find_nalu_starts(&data), vec![0, 8]);
    }

    #[test]
    fn test_stop_handle() {
        let h = CastStopHandle::new();
        assert!(!h.is_stopped());
        h.stop();
        assert!(h.is_stopped());
    }
}

/// A stable identifier for this install.
///
/// Nothing authenticates it today, so it is only useful for correlating log
/// lines — but it must at least be stable across runs, because it is what a
/// future paired-device store would key on.
fn sender_id() -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    openplay_common::data_dir().hash(&mut hasher);
    format!("openplay-{:016x}", hasher.finish())
}

#[cfg(test)]
mod openplay_tests {
    use super::*;

    #[test]
    fn a_configured_name_is_passed_through_unchanged() {
        assert_eq!(
            sanitise_display_name("Sandeepa's Laptop"),
            "Sandeepa's Laptop"
        );
    }

    #[test]
    fn an_empty_or_blank_name_falls_back_rather_than_being_rejected() {
        assert_eq!(sanitise_display_name(""), "OpenPlay Sender");
        assert_eq!(sanitise_display_name("   "), "OpenPlay Sender");
    }

    #[test]
    fn control_characters_are_stripped_so_the_receiver_does_not_reject_us() {
        assert_eq!(
            sanitise_display_name("Laptop\nWARN forged"),
            "LaptopWARN forged"
        );
    }

    #[test]
    fn an_over_long_name_is_trimmed_to_the_protocol_bound() {
        let long = "n".repeat(openplay_protocol::MAX_NAME_CHARS + 50);
        let out = sanitise_display_name(&long);
        assert_eq!(out.chars().count(), openplay_protocol::MAX_NAME_CHARS);
    }

    #[test]
    fn the_sender_id_is_stable_across_calls() {
        assert_eq!(sender_id(), sender_id());
        assert!(sender_id().starts_with("openplay-"));
    }
}
