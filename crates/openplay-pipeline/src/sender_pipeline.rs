use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use std::os::unix::io::RawFd;
use tracing::{debug, error, info};

use crate::encoder::{configure_encoder, EncoderType};
use crate::PipelineError;

/// GStreamer pipeline for the sender (screen capture → encode → WebRTC).
pub struct SenderPipeline {
    pipeline: gst::Pipeline,
    webrtcbin: gst::Element,
    encoder_type: EncoderType,
}

impl SenderPipeline {
    /// Creates a new sender pipeline.
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
        let pipeline = gst::Pipeline::with_name("openplay-sender");

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

        // H.264 output caps
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

        // RTP payloader
        let rtppay = gst::ElementFactory::make("rtph264pay")
            .property("config-interval", -1i32)
            .property("aggregate-mode", 1i32) // zero-latency
            .property("mtu", 1200u32)
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("rtph264pay: {e}")))?;

        // Output queue
        let rtp_queue = gst::ElementFactory::make("queue")
            .name("rtp_queue")
            .property("max-size-buffers", 1u32)
            .property_from_str("leaky", "downstream")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("rtp queue: {e}")))?;

        // WebRTC output
        let webrtcbin = gst::ElementFactory::make("webrtcbin")
            .name("send")
            .property_from_str("bundle-policy", "max-bundle")
            .property("latency", 40u32)
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("webrtcbin: {e}")))?;

        // Add all elements to pipeline
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

        // Link: src → capsfilter → queue → encoder → h264caps → rtppay → queue → webrtcbin
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

        // Link RTP queue to webrtcbin via request pad
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
            framerate,
            "Sender pipeline created"
        );

        Ok(Self {
            pipeline,
            webrtcbin,
            encoder_type,
        })
    }

    /// Returns a reference to the underlying GStreamer pipeline.
    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }

    /// Returns a reference to the webrtcbin element.
    pub fn webrtcbin(&self) -> &gst::Element {
        &self.webrtcbin
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
        info!("Sender pipeline started");
        Ok(())
    }

    /// Stops the pipeline.
    pub fn stop(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Null)
            .map_err(|e| PipelineError::StateChange(format!("Failed to stop pipeline: {e}")))?;
        info!("Sender pipeline stopped");
        Ok(())
    }

    /// Sets up the bus watch for pipeline events.
    /// The callback is invoked on the GLib main thread.
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

impl Drop for SenderPipeline {
    fn drop(&mut self) {
        if let Err(e) = self.pipeline.set_state(gst::State::Null) {
            error!("Failed to stop pipeline on drop: {e}");
        }
    }
}
