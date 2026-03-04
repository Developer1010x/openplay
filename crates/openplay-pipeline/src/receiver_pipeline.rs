use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use tracing::{debug, error, info, warn};

use crate::PipelineError;

/// GStreamer pipeline for the receiver (WebRTC → decode → display).
pub struct ReceiverPipeline {
    pipeline: gst::Pipeline,
    webrtcbin: gst::Element,
}

impl ReceiverPipeline {
    /// Creates a new receiver pipeline with webrtcbin as the entry point.
    ///
    /// The decoding chain is connected dynamically when pads appear on webrtcbin.
    pub fn new() -> Result<Self> {
        let pipeline = gst::Pipeline::with_name("openplay-receiver");

        // WebRTC input
        let webrtcbin = gst::ElementFactory::make("webrtcbin")
            .name("recv")
            .property_from_str("bundle-policy", "max-bundle")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("webrtcbin: {e}")))?;

        pipeline
            .add(&webrtcbin)
            .map_err(|e| PipelineError::Gstreamer(format!("Failed to add webrtcbin: {e}")))?;

        // Connect the pad-added signal to dynamically link the decoding chain
        let pipeline_weak = pipeline.downgrade();
        webrtcbin.connect_pad_added(move |_webrtcbin, pad| {
            let Some(pipeline) = pipeline_weak.upgrade() else {
                return;
            };

            let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
            let caps_str = caps.to_string();
            debug!(caps = %caps_str, "WebRTC pad added");

            if caps_str.contains("video") {
                if let Err(e) = Self::link_video_chain(&pipeline, pad) {
                    error!("Failed to link video chain: {e}");
                }
            } else {
                debug!("Ignoring non-video pad: {caps_str}");
            }
        });

        info!("Receiver pipeline created");

        Ok(Self {
            pipeline,
            webrtcbin,
        })
    }

    /// Links the video decoding chain when a video pad appears.
    fn link_video_chain(pipeline: &gst::Pipeline, src_pad: &gst::Pad) -> Result<()> {
        let depay = gst::ElementFactory::make("rtph264depay")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("rtph264depay: {e}")))?;

        let parse = gst::ElementFactory::make("h264parse")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("h264parse: {e}")))?;

        // Try hardware decoder first, fall back to software
        let decoder = Self::create_decoder()?;

        let convert = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("videoconvert: {e}")))?;

        let sink = gst::ElementFactory::make("gtk4paintablesink")
            .name("video_sink")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("gtk4paintablesink: {e}")))?;

        pipeline
            .add_many([&depay, &parse, &decoder, &convert, &sink])
            .map_err(|e| PipelineError::Gstreamer(format!("Failed to add decode chain: {e}")))?;

        gst::Element::link_many([&depay, &parse, &decoder, &convert, &sink])
            .map_err(|e| PipelineError::Gstreamer(format!("Failed to link decode chain: {e}")))?;

        // Sync element states with the pipeline
        for elem in [&depay, &parse, &decoder, &convert, &sink] {
            elem.sync_state_with_parent()
                .map_err(|e| PipelineError::StateChange(format!("Failed to sync state: {e}")))?;
        }

        // Link the webrtcbin pad to the depayloader
        let depay_sink = depay
            .static_pad("sink")
            .context("No sink pad on depayloader")?;
        src_pad
            .link(&depay_sink)
            .map_err(|e| PipelineError::Gstreamer(format!("Failed to link to depayloader: {e}")))?;

        info!("Video decoding chain linked");
        Ok(())
    }

    /// Creates the best available H.264 decoder.
    fn create_decoder() -> Result<gst::Element> {
        // Try hardware decoders first
        let candidates = ["vah264dec", "nvh264dec", "avdec_h264"];
        for name in &candidates {
            if let Ok(elem) = gst::ElementFactory::make(name).build() {
                info!(decoder = name, "Using decoder");
                return Ok(elem);
            }
        }
        Err(PipelineError::MissingElement("No H.264 decoder found".to_string()).into())
    }

    /// Returns a reference to the underlying GStreamer pipeline.
    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }

    /// Returns a reference to the webrtcbin element.
    pub fn webrtcbin(&self) -> &gst::Element {
        &self.webrtcbin
    }

    /// Returns the gtk4paintablesink element, if the video chain has been linked.
    pub fn video_sink(&self) -> Option<gst::Element> {
        self.pipeline.by_name("video_sink")
    }

    /// Sets the pipeline to Playing state.
    pub fn start(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| PipelineError::StateChange(format!("Failed to start pipeline: {e}")))?;
        info!("Receiver pipeline started");
        Ok(())
    }

    /// Stops the pipeline.
    pub fn stop(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Null)
            .map_err(|e| PipelineError::StateChange(format!("Failed to stop pipeline: {e}")))?;
        info!("Receiver pipeline stopped");
        Ok(())
    }
}

impl Drop for ReceiverPipeline {
    fn drop(&mut self) {
        if let Err(e) = self.pipeline.set_state(gst::State::Null) {
            error!("Failed to stop pipeline on drop: {e}");
        }
    }
}
