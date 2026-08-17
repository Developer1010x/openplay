use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gstreamer_app as gst_app;
use tokio::runtime::Handle as TokioHandle;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use openplay_airplay::session::{AirPlaySession, SessionEvent as AirPlayEvent};
use openplay_capture::CaptureSession;
use openplay_miracast::session::{MiracastSession, SessionEvent as MiracastEvent};
use openplay_pipeline::{
    probe_best_encoder, AirPlaySenderPipeline, CaptureConfig, EncoderType, MiracastSenderPipeline,
};

const MIRACAST_RTSP_PORT: u16 = 7236;

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

pub async fn start_miracast_cast(
    receiver_ip: std::net::IpAddr,
    bitrate_kbps: u32,
    framerate: u32,
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
    let sink_addr = SocketAddr::new(receiver_ip, MIRACAST_RTSP_PORT);
    let stop_flag = stop_handle.flag();

    status_callback("Connecting via Miracast...");

    let result = tokio_handle
        .spawn(async move {
            run_miracast_pipeline(sink_addr, capture_config, bitrate_kbps, stop_flag).await
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
            run_miracast_p2p_pipeline(&mac, capture_config, bitrate_kbps, stop_flag).await
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

async fn run_airplay_pipeline(
    receiver_addr: SocketAddr,
    capture_config: CaptureConfig,
    width: u32,
    height: u32,
    bitrate_kbps: u32,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let encoder_type = probe_best_encoder().unwrap_or_else(|_| {
        warn!("No HW encoder found, falling back to x264");
        EncoderType::X264
    });
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
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let encoder_type = probe_best_encoder().unwrap_or_else(|_| {
        warn!("No HW encoder found, falling back to x264");
        EncoderType::X264
    });
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
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let encoder_type = probe_best_encoder().unwrap_or(EncoderType::X264);
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
