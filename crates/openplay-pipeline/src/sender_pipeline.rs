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

        let rtppay = gst::ElementFactory::make("rtph264pay")
            .property("config-interval", -1i32)
            .property("aggregate-mode", 1i32)
            .property("mtu", 1200u32)
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("rtph264pay: {e}")))?;

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

        pipeline
            .add_many([
                &src,
                &capsfilter,
                &video_queue,
                &encoder,
                &h264_caps,
                &rtppay,
                &rtp_queue,
                &webrtcbin,
            ])
            .map_err(|e| PipelineError::Gstreamer(format!("Failed to add elements: {e}")))?;

        gst::Element::link_many([
            &src,
            &capsfilter,
            &video_queue,
            &encoder,
            &h264_caps,
            &rtppay,
            &rtp_queue,
        ])
        .map_err(|e| PipelineError::Gstreamer(format!("Failed to link elements: {e}")))?;

        let rtp_queue_src = rtp_queue
            .static_pad("src")
            .context("No src pad on rtp_queue")?;
        let webrtc_sink = webrtcbin
            .request_pad_simple("sink_%u")
            .context("Failed to request webrtcbin sink pad")?;
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
