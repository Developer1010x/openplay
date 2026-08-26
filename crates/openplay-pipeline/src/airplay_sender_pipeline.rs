use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use tracing::{error, info};

use crate::capture_config::CaptureConfig;
use crate::encoder::{build_capture_element, configure_encoder, EncoderType};
use crate::PipelineError;

/// GStreamer pipeline for AirPlay sending.
///
/// Captures the screen, encodes to H.264, and outputs raw NALUs via `appsink`
/// for the AirPlay mirror stream protocol.
///
/// Pipeline: [capture src] → capsfilter(fps) → queue → encoder → h264parse → capsfilter(h264,byte-stream) → appsink
pub struct AirPlaySenderPipeline {
    pipeline: gst::Pipeline,
    appsink: gst_app::AppSink,
    encoder_type: EncoderType,
}

impl AirPlaySenderPipeline {
    /// Creates a new AirPlay sender pipeline.
    pub fn new(
        capture: &CaptureConfig,
        encoder_type: EncoderType,
        bitrate_kbps: u32,
    ) -> Result<Self> {
        let pipeline = gst::Pipeline::with_name("openplay-airplay-sender");

        // Platform-specific screen capture source
        let src = build_capture_element(
            #[cfg(target_os = "linux")]
            capture.pw_fd,
            #[cfg(target_os = "linux")]
            capture.node_id,
        )?;

        // Rate limiting capsfilter
        let capsfilter = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("framerate", gst::Fraction::new(capture.framerate as i32, 1))
                    .build(),
            )
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("capsfilter: {e}")))?;

        // Video queue (leaky to avoid stalling on slow encode)
        let video_queue = gst::ElementFactory::make("queue")
            .name("video_queue")
            .property("max-size-buffers", 1u32)
            .property_from_str("leaky", "downstream")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("queue: {e}")))?;

        // Encoder
        let encoder = gst::ElementFactory::make(encoder_type.factory_name())
            .build()
            .map_err(|e| {
                PipelineError::MissingElement(format!("{}: {e}", encoder_type.factory_name()))
            })?;
        configure_encoder(&encoder, encoder_type, bitrate_kbps);

        // H.264 output caps (byte-stream for raw NALUs).
        //
        // This is linked *after* h264parse, not before it. `vtenc_h264` offers
        // only `stream-format=avc` on its src pad, so demanding byte-stream
        // straight out of the encoder leaves an empty caps intersection and the
        // link fails outright on macOS. h264parse is the element that converts
        // avc to byte-stream. `x264enc` advertises both formats, which is why
        // the old order linked fine on Linux and hid this.
        //
        // `profile` is deliberately not constrained: `vtenc_h264` has no profile
        // property and does not advertise the field, so pinning it here would
        // just move the negotiation failure one element downstream.
        let h264_caps = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-h264")
                    .field("stream-format", "byte-stream")
                    .field("alignment", "au")
                    .build(),
            )
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("h264 capsfilter: {e}")))?;

        // H.264 parser (normalizes NALUs)
        let h264parse = gst::ElementFactory::make("h264parse")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("h264parse: {e}")))?;

        // App sink — pull buffers and send over AirPlay mirror stream
        let appsink = gst_app::AppSink::builder()
            .name("airplay_sink")
            .max_buffers(2)
            .drop(true)
            .sync(false)
            .build();

        appsink.set_caps(Some(
            &gst::Caps::builder("video/x-h264")
                .field("stream-format", "byte-stream")
                .field("alignment", "au")
                .build(),
        ));

        pipeline
            .add_many([
                &src,
                &capsfilter,
                &video_queue,
                &encoder,
                &h264parse,
                &h264_caps,
                appsink.upcast_ref(),
            ])
            .map_err(|e| PipelineError::Gstreamer(format!("Failed to add elements: {e}")))?;

        gst::Element::link_many([
            &src,
            &capsfilter,
            &video_queue,
            &encoder,
            &h264parse,
            &h264_caps,
            appsink.upcast_ref(),
        ])
        .map_err(|e| PipelineError::Gstreamer(format!("Failed to link elements: {e}")))?;

        info!(
            encoder = encoder_type.factory_name(),
            bitrate_kbps,
            framerate = capture.framerate,
            "AirPlay sender pipeline created"
        );

        Ok(Self {
            pipeline,
            appsink,
            encoder_type,
        })
    }

    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }

    pub fn appsink(&self) -> &gst_app::AppSink {
        &self.appsink
    }

    pub fn encoder_type(&self) -> EncoderType {
        self.encoder_type
    }

    pub fn start(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| PipelineError::StateChange(format!("Failed to start pipeline: {e}")))?;
        info!("AirPlay sender pipeline started");
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Null)
            .map_err(|e| PipelineError::StateChange(format!("Failed to stop pipeline: {e}")))?;
        info!("AirPlay sender pipeline stopped");
        Ok(())
    }

    pub fn setup_bus_watch<F>(&self, callback: F) -> Result<()>
    where
        F: Fn(&gst::Bus, &gst::Message) -> gst::BusSyncReply + Send + Sync + 'static,
    {
        let bus = self.pipeline.bus().context("Pipeline has no bus")?;
        bus.set_sync_handler(callback);
        Ok(())
    }
}

impl Drop for AirPlaySenderPipeline {
    fn drop(&mut self) {
        if let Err(e) = self.pipeline.set_state(gst::State::Null) {
            error!("Failed to stop AirPlay pipeline on drop: {e}");
        }
    }
}
