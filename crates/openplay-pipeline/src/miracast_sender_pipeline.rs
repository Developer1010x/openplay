use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use std::os::unix::io::RawFd;
use tracing::{error, info};

use crate::encoder::{configure_encoder, EncoderType};
use crate::PipelineError;

/// GStreamer pipeline for Miracast sending.
///
/// Captures screen via PipeWire, encodes to H.264, muxes into MPEG2-TS,
/// wraps in RTP, and sends via UDP to the Miracast sink.
///
/// Pipeline: pipewiresrc → capsfilter(fps) → queue → encoder → h264parse → mpegtsmux → rtpmp2tpay → udpsink
pub struct MiracastSenderPipeline {
    pipeline: gst::Pipeline,
    encoder_type: EncoderType,
}

impl MiracastSenderPipeline {
    /// Creates a new Miracast sender pipeline.
    ///
    /// # Arguments
    /// * `pw_fd` - PipeWire file descriptor from the portal
    /// * `node_id` - PipeWire node ID for the capture source
    /// * `encoder_type` - Which encoder to use
    /// * `bitrate_kbps` - Target bitrate in kbps
    /// * `framerate` - Target framerate
    /// * `sink_host` - Destination IP address for RTP/UDP
    /// * `sink_port` - Destination UDP port for RTP
    pub fn new(
        pw_fd: RawFd,
        node_id: u32,
        encoder_type: EncoderType,
        bitrate_kbps: u32,
        framerate: u32,
        sink_host: &str,
        sink_port: u16,
    ) -> Result<Self> {
        let pipeline = gst::Pipeline::with_name("openplay-miracast-sender");

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

        // Video queue
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

        // H.264 parser
        let h264parse = gst::ElementFactory::make("h264parse")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("h264parse: {e}")))?;

        // MPEG-TS muxer
        let mpegtsmux = gst::ElementFactory::make("mpegtsmux")
            .property("alignment", 7i32) // Align to 7 TS packets (1316 bytes)
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("mpegtsmux: {e}")))?;

        // RTP MPEG-TS payloader
        let rtppay = gst::ElementFactory::make("rtpmp2tpay")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("rtpmp2tpay: {e}")))?;

        // UDP sink
        let udpsink = gst::ElementFactory::make("udpsink")
            .property("host", sink_host)
            .property("port", sink_port as i32)
            .property("sync", false)
            .property("async", false)
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("udpsink: {e}")))?;

        // Add all elements to pipeline
        pipeline
            .add_many([
                &src,
                &capsfilter,
                &video_queue,
                &encoder,
                &h264parse,
                &mpegtsmux,
                &rtppay,
                &udpsink,
            ])
            .map_err(|e| PipelineError::Gstreamer(format!("Failed to add elements: {e}")))?;

        // Link the chain
        gst::Element::link_many([
            &src,
            &capsfilter,
            &video_queue,
            &encoder,
            &h264parse,
            &mpegtsmux,
            &rtppay,
            &udpsink,
        ])
        .map_err(|e| PipelineError::Gstreamer(format!("Failed to link elements: {e}")))?;

        info!(
            encoder = encoder_type.factory_name(),
            bitrate_kbps,
            framerate,
            sink_host,
            sink_port,
            "Miracast sender pipeline created"
        );

        Ok(Self {
            pipeline,
            encoder_type,
        })
    }

    /// Returns a reference to the underlying GStreamer pipeline.
    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
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
        info!("Miracast sender pipeline started");
        Ok(())
    }

    /// Stops the pipeline.
    pub fn stop(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Null)
            .map_err(|e| PipelineError::StateChange(format!("Failed to stop pipeline: {e}")))?;
        info!("Miracast sender pipeline stopped");
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

impl Drop for MiracastSenderPipeline {
    fn drop(&mut self) {
        if let Err(e) = self.pipeline.set_state(gst::State::Null) {
            error!("Failed to stop Miracast pipeline on drop: {e}");
        }
    }
}
