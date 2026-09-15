use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use tracing::{debug, error, info};

use crate::capture_config::CaptureConfig;
use crate::encoder::{build_capture_element, configure_encoder, EncoderType};
use crate::PipelineError;

/// GStreamer pipeline for the OpenPlay sender (screen capture → encode → WebRTC).
pub struct SenderPipeline {
    pipeline: gst::Pipeline,
    webrtcbin: gst::Element,
    encoder_type: EncoderType,
}

impl SenderPipeline {
    /// Creates a new sender pipeline.
    pub fn new(
        capture: &CaptureConfig,
        encoder_type: EncoderType,
        bitrate_kbps: u32,
    ) -> Result<Self> {
        let pipeline = gst::Pipeline::with_name("openplay-sender");

        // Platform-specific screen capture source
        let src = build_capture_element(
            #[cfg(target_os = "linux")]
            capture.pw_fd,
            #[cfg(target_os = "linux")]
            capture.node_id,
        )?;

        // `pipewiresrc` hands over whatever the portal negotiated — the pixel
        // format is the compositor's choice, and a portal stream commonly
        // advertises a *maximum* framerate rather than a fixed one. Feeding
        // that straight into a hardware encoder, which accepts only a few
        // formats, leaves nothing in the intersection and the source gives up:
        //
        // ```text
        // pipewiresrc0: stream error: no more input formats
        // streaming stopped, reason not-negotiated (-4)
        // ```
        //
        // `videoconvert` widens the acceptable format set to everything raw,
        // and `videorate` is what actually turns a variable-rate stream into
        // the fixed framerate the capsfilter below asks for — without it that
        // filter is a demand the source cannot meet.
        let videoconvert = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("videoconvert: {e}")))?;

        let videorate = gst::ElementFactory::make("videorate")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("videorate: {e}")))?;

        let capsfilter = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("framerate", gst::Fraction::new(capture.framerate as i32, 1))
                    .build(),
            )
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("capsfilter: {e}")))?;

        let video_queue = gst::ElementFactory::make("queue")
            .name("video_queue")
            .property("max-size-buffers", 1u32)
            .property_from_str("leaky", "downstream")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("queue: {e}")))?;

        let encoder = gst::ElementFactory::make(encoder_type.factory_name())
            .build()
            .map_err(|e| {
                PipelineError::MissingElement(format!("{}: {e}", encoder_type.factory_name()))
            })?;
        configure_encoder(&encoder, encoder_type, bitrate_kbps);

        // Constrain alignment only. `vtenc_h264` offers just
        // `stream-format=avc` and advertises no `profile` field, so requiring
        // byte-stream or high profile straight out of the encoder makes this
        // link fail on macOS — the same defect fixed in the AirPlay pipeline.
        // Unlike that one this chain has no h264parse to convert with, but it
        // does not need one: `rtph264pay` accepts avc and byte-stream alike, so
        // letting the two negotiate is both correct and simpler.
        let h264_caps = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-h264")
                    .field("alignment", "au")
                    .build(),
            )
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("h264 capsfilter: {e}")))?;

        // `aggregate-mode` is a GEnum. Setting it with `.property(_, 1i32)`
        // panics inside `.build()` — "can't be set from the given type" — so it
        // has to go through `property_from_str`.
        let rtppay = gst::ElementFactory::make("rtph264pay")
            .property("config-interval", -1i32)
            .property_from_str("aggregate-mode", "zero-latency")
            .property("mtu", 1200u32)
            .property("pt", 96u32)
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("rtph264pay: {e}")))?;

        // Without fixed RTP caps the payloader offers `payload` as a *range*,
        // and webrtcbin cannot build an m-line from unfixed caps: `create-offer`
        // then succeeds and returns an SDP with no media section at all. That
        // is the quietest way to get a session that negotiates and carries no
        // video, so these four fields are mandatory — and their types are load
        // bearing (`payload` and `clock-rate` are gint, `encoding-name` is
        // upper-case).
        let rtp_caps = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("application/x-rtp")
                    .field("media", "video")
                    .field("encoding-name", "H264")
                    .field("payload", 96i32)
                    .field("clock-rate", 90000i32)
                    .build(),
            )
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("rtp capsfilter: {e}")))?;

        let rtp_queue = gst::ElementFactory::make("queue")
            .name("rtp_queue")
            .property("max-size-buffers", 1u32)
            .property_from_str("leaky", "downstream")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("rtp queue: {e}")))?;

        let webrtcbin = gst::ElementFactory::make("webrtcbin")
            .name("send")
            .property_from_str("bundle-policy", "max-bundle")
            .property("latency", 40u32)
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("webrtcbin: {e}")))?;

        // Diagnostic tap: with OPENPLAY_DUMP_CAPTURE set, report what the
        // capture source is actually producing, before anything else touches
        // it. A uniformly green picture at the far end is either a blank
        // capture or a mangled conversion, and nothing downstream can tell
        // those apart — the encode/decode chain is provably fine on a
        // videotestsrc.
        if std::env::var("OPENPLAY_DUMP_CAPTURE").is_ok() {
            if let Some(pad) = src.static_pad("src") {
                let seen = std::sync::atomic::AtomicUsize::new(0);
                pad.add_probe(gst::PadProbeType::BUFFER, move |_, probe_info| {
                    let n = seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if n < 5 {
                        if let Some(gst::PadProbeData::Buffer(buf)) = &probe_info.data {
                            if let Ok(map) = buf.map_readable() {
                                let d = map.as_slice();
                                let nonzero = d.iter().filter(|b| **b != 0).count();
                                let max = d.iter().copied().max().unwrap_or(0);
                                let sum: u64 = d.iter().map(|b| *b as u64).sum();
                                tracing::warn!(
                                    frame = n,
                                    bytes = d.len(),
                                    nonzero,
                                    pct_nonzero = (nonzero * 100) / d.len().max(1),
                                    max,
                                    mean = sum / d.len().max(1) as u64,
                                    "Captured buffer contents"
                                );
                            }
                        }
                    }
                    gst::PadProbeReturn::Ok
                });
            }
        }

        pipeline
            .add_many([
                &src,
                &videoconvert,
                &videorate,
                &capsfilter,
                &video_queue,
                &encoder,
                &h264_caps,
                &rtppay,
                &rtp_caps,
                &rtp_queue,
                &webrtcbin,
            ])
            .map_err(|e| PipelineError::Gstreamer(format!("Failed to add elements: {e}")))?;

        gst::Element::link_many([
            &src,
            &videoconvert,
            &videorate,
            &capsfilter,
            &video_queue,
            &encoder,
            &h264_caps,
            &rtppay,
            &rtp_caps,
            &rtp_queue,
        ])
        .map_err(|e| PipelineError::Gstreamer(format!("Failed to link elements: {e}")))?;

        let rtp_queue_src = rtp_queue
            .static_pad("src")
            .context("No src pad on rtp_queue")?;
        let webrtc_sink = webrtcbin.request_pad_simple("sink_%u").context(
            "webrtcbin refused a sink pad. This is what a missing libnice plugin \
             looks like — install gstreamer1.0-nice and check `gst-inspect-1.0 nicesrc`",
        )?;
        rtp_queue_src
            .link(&webrtc_sink)
            .map_err(|e| PipelineError::Gstreamer(format!("Failed to link to webrtcbin: {e}")))?;

        info!(
            encoder = encoder_type.factory_name(),
            bitrate_kbps,
            framerate = capture.framerate,
            "Sender pipeline created"
        );

        Ok(Self {
            pipeline,
            webrtcbin,
            encoder_type,
        })
    }

    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }

    pub fn webrtcbin(&self) -> &gst::Element {
        &self.webrtcbin
    }

    pub fn encoder_type(&self) -> EncoderType {
        self.encoder_type
    }

    pub fn start(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| PipelineError::StateChange(format!("Failed to start pipeline: {e}")))?;
        info!("Sender pipeline started");
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Null)
            .map_err(|e| PipelineError::StateChange(format!("Failed to stop pipeline: {e}")))?;
        info!("Sender pipeline stopped");
        Ok(())
    }

    pub fn setup_bus_watch<F>(&self, callback: F) -> Result<()>
    where
        F: Fn(&gst::Bus, &gst::Message) -> gst::BusSyncReply + Send + Sync + 'static,
    {
        let bus = self.pipeline.bus().context("Pipeline has no bus")?;
        bus.set_sync_handler(callback);
        debug!("Bus watch set up");
        Ok(())
    }
}

impl Drop for SenderPipeline {
    fn drop(&mut self) {
        if let Err(e) = self.pipeline.set_state(gst::State::Null) {
            error!("Failed to stop pipeline on drop: {e}");
        }
    }
}
