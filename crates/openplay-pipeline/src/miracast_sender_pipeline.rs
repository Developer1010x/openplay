use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use tracing::{error, info};

use crate::capture_config::CaptureConfig;
use crate::encoder::{build_capture_element, configure_encoder, EncoderType};
use crate::PipelineError;

/// GStreamer pipeline for Miracast sending.
///
/// Captures the screen, encodes to H.264, muxes into MPEG2-TS,
/// wraps in RTP, and sends via UDP to the Miracast sink.
///
/// Pipeline: [capture src] → capsfilter(fps) → queue → encoder → h264parse → mpegtsmux → rtpmp2tpay → udpsink
pub struct MiracastSenderPipeline {
    pipeline: gst::Pipeline,
    encoder_type: EncoderType,
}

impl MiracastSenderPipeline {
    /// Creates a new Miracast sender pipeline.
    pub fn new(
        capture: &CaptureConfig,
        encoder_type: EncoderType,
        bitrate_kbps: u32,
        sink_host: &str,
        sink_port: u16,
    ) -> Result<Self> {
        let pipeline = gst::Pipeline::with_name("openplay-miracast-sender");

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

        let h264parse = gst::ElementFactory::make("h264parse")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("h264parse: {e}")))?;

        // MPEG-TS muxer (align=7 → 1316-byte packets for WFD compliance)
        let mpegtsmux = gst::ElementFactory::make("mpegtsmux")
            .property("alignment", 7i32)
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("mpegtsmux: {e}")))?;

        let rtppay = gst::ElementFactory::make("rtpmp2tpay")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("rtpmp2tpay: {e}")))?;

        let udpsink = gst::ElementFactory::make("udpsink")
            .property("host", sink_host)
            .property("port", sink_port as i32)
            .property("sync", false)
            .property("async", false)
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("udpsink: {e}")))?;

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
            framerate = capture.framerate,
            sink_host,
            sink_port,
            "Miracast sender pipeline created"
        );

        Ok(Self {
            pipeline,
            encoder_type,
        })
    }

    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }

    pub fn encoder_type(&self) -> EncoderType {
        self.encoder_type
    }

    pub fn start(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| PipelineError::StateChange(format!("Failed to start pipeline: {e}")))?;
        info!("Miracast sender pipeline started");
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Null)
            .map_err(|e| PipelineError::StateChange(format!("Failed to stop pipeline: {e}")))?;
        info!("Miracast sender pipeline stopped");
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

impl Drop for MiracastSenderPipeline {
    fn drop(&mut self) {
        if let Err(e) = self.pipeline.set_state(gst::State::Null) {
            error!("Failed to stop Miracast pipeline on drop: {e}");
        }
    }
}
