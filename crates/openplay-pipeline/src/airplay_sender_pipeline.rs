use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use std::os::unix::io::RawFd;
use tracing::{error, info};

use crate::encoder::{configure_encoder, EncoderType};
use crate::PipelineError;

/// GStreamer pipeline for AirPlay sending.
///
/// Same capture + encode chain as `SenderPipeline`, but outputs to `appsink`
/// instead of WebRTC. AirPlay uses raw H.264 NALUs with custom framing,
/// not RTP.
///
/// Pipeline: pipewiresrc → capsfilter(fps) → queue → encoder → capsfilter(h264,byte-stream) → h264parse → appsink
pub struct AirPlaySenderPipeline {
    pipeline: gst::Pipeline,
    appsink: gst_app::AppSink,
    encoder_type: EncoderType,
}

impl AirPlaySenderPipeline {
    /// Creates a new AirPlay sender pipeline.
    ///
    /// # Arguments
    /// * `pw_fd` - PipeWire file descriptor from the portal
    /// * `node_id` - PipeWire node ID for the capture source
    /// * `encoder_type` - Which encoder to use
    /// * `bitrate_kbps` - Target bitrate in kbps
    /// * `framerate` - Target framerate
    pub fn new(
        pw_fd: RawFd,
        node_id: u32,
        encoder_type: EncoderType,
        bitrate_kbps: u32,
        framerate: u32,
    ) -> Result<Self> {
        let pipeline = gst::Pipeline::with_name("openplay-airplay-sender");

        // Source: PipeWire screen capture
        let src = gst::ElementFactory::make("pipewiresrc")
            .property("fd", pw_fd)
            .property("path", node_id.to_string())
            .property("do-timestamp", true)
            .property("always-copy", false)
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("pipewiresrc: {e}")))?;

        // Rate limiting capsfilter
        let capsfilter = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("framerate", gst::Fraction::new(framerate as i32, 1))
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

        // H.264 output caps (byte-stream for raw NALUs)
        let h264_caps = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-h264")
                    .field("profile", "high")
                    .field("stream-format", "byte-stream")
                    .build(),
            )
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("h264 capsfilter: {e}")))?;

        // H.264 parser
        let h264parse = gst::ElementFactory::make("h264parse")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("h264parse: {e}")))?;

        // App sink — we pull buffers from here and send over AirPlay mirror stream
        let appsink = gst_app::AppSink::builder()
            .name("airplay_sink")
            .max_buffers(2)
            .drop(true) // Drop old buffers if not consumed fast enough
            .sync(false)
            .build();

        // Set caps on appsink
        appsink.set_caps(Some(
            &gst::Caps::builder("video/x-h264")
                .field("stream-format", "byte-stream")
                .field("alignment", "au")
                .build(),
        ));

        // Add all elements to pipeline
        pipeline
            .add_many([
                &src,
                &capsfilter,
                &video_queue,
                &encoder,
                &h264_caps,
                &h264parse,
                appsink.upcast_ref(),
            ])
            .map_err(|e| PipelineError::Gstreamer(format!("Failed to add elements: {e}")))?;

        // Link: src → capsfilter → queue → encoder → h264caps → h264parse → appsink
        gst::Element::link_many([
            &src,
            &capsfilter,
            &video_queue,
            &encoder,
            &h264_caps,
            &h264parse,
            appsink.upcast_ref(),
        ])
        .map_err(|e| PipelineError::Gstreamer(format!("Failed to link elements: {e}")))?;

        info!(
            encoder = encoder_type.factory_name(),
            bitrate_kbps,
            framerate,
            "AirPlay sender pipeline created"
        );

        Ok(Self {
            pipeline,
            appsink,
            encoder_type,
        })
    }

    /// Returns a reference to the underlying GStreamer pipeline.
    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }

    /// Returns a reference to the appsink element.
    pub fn appsink(&self) -> &gst_app::AppSink {
        &self.appsink
    }

    /// Returns the encoder type in use.
    pub fn encoder_type(&self) -> EncoderType {
        self.encoder_type
    }

    /// Sets the pipeline to Playing state.
    pub fn start(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| PipelineError::StateChange(format!("Failed to start pipeline: {e}")))?;
        info!("AirPlay sender pipeline started");
        Ok(())
    }

    /// Stops the pipeline.
    pub fn stop(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Null)
            .map_err(|e| PipelineError::StateChange(format!("Failed to stop pipeline: {e}")))?;
        info!("AirPlay sender pipeline stopped");
        Ok(())
    }

    /// Sets up the bus watch for pipeline events.
    pub fn setup_bus_watch<F>(&self, callback: F) -> Result<()>
    where
        F: Fn(&gst::Bus, &gst::Message) -> gst::BusSyncReply + Send + Sync + 'static,
    {
        let bus = self
            .pipeline
            .bus()
            .context("Pipeline has no bus")?;
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
