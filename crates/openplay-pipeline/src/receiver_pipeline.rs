use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use tracing::{debug, error, info};

use crate::encoder::build_decoder_element;
use crate::PipelineError;

/// GStreamer pipeline for the receiver (WebRTC → decode → appsink).
///
/// Uses `appsink` with RGBA output for cross-platform video display.
/// The consuming application (egui, etc.) reads frames from the appsink
/// and renders them using platform-appropriate methods.
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

        let webrtcbin = gst::ElementFactory::make("webrtcbin")
            .name("recv")
            .property_from_str("bundle-policy", "max-bundle")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("webrtcbin: {e}")))?;

        pipeline
            .add(&webrtcbin)
            .map_err(|e| PipelineError::Gstreamer(format!("Failed to add webrtcbin: {e}")))?;

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

    /// Links the video decoding chain when a video pad appears on webrtcbin.
    ///
    /// Chain: webrtcbin → rtph264depay → h264parse → decoder → videoconvert
    ///         → capsfilter(RGBA) → appsink
    ///
    /// The appsink outputs RGBA frames for rendering in egui (cross-platform).
    fn link_video_chain(pipeline: &gst::Pipeline, src_pad: &gst::Pad) -> Result<()> {
        let depay = gst::ElementFactory::make("rtph264depay")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("rtph264depay: {e}")))?;

        let parse = gst::ElementFactory::make("h264parse")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("h264parse: {e}")))?;

        let decoder = build_decoder_element()?;

        let convert = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("videoconvert: {e}")))?;

        // Force RGBA output so egui can display it directly as a texture
        let rgba_caps = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("format", "RGBA")
                    .build(),
            )
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("rgba capsfilter: {e}")))?;

        // AppSink: consumer reads frames and uploads to egui texture
        let appsink = gst_app::AppSink::builder()
            .name("video_sink")
            .max_buffers(2)
            .drop(true)
            .sync(true)
            .build();

        pipeline
            .add_many([
                &depay,
                &parse,
                &decoder,
                &convert,
                &rgba_caps,
                appsink.upcast_ref(),
            ])
            .map_err(|e| PipelineError::Gstreamer(format!("Failed to add decode chain: {e}")))?;

        gst::Element::link_many([
            &depay,
            &parse,
            &decoder,
            &convert,
            &rgba_caps,
            appsink.upcast_ref(),
        ])
        .map_err(|e| PipelineError::Gstreamer(format!("Failed to link decode chain: {e}")))?;

        for elem in [
            &depay,
            &parse,
            &decoder,
            &convert,
            &rgba_caps,
            appsink.upcast_ref(),
        ] {
            elem.sync_state_with_parent()
                .map_err(|e| PipelineError::StateChange(format!("Failed to sync state: {e}")))?;
        }

        let depay_sink = depay
            .static_pad("sink")
            .context("No sink pad on depayloader")?;
        src_pad
            .link(&depay_sink)
            .map_err(|e| PipelineError::Gstreamer(format!("Failed to link to depayloader: {e}")))?;

        info!("Video decoding chain linked (RGBA appsink)");
        Ok(())
    }

    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }

    pub fn webrtcbin(&self) -> &gst::Element {
        &self.webrtcbin
    }

    /// Returns the video appsink for reading RGBA frames.
    pub fn video_appsink(&self) -> Option<gst_app::AppSink> {
        self.pipeline
            .by_name("video_sink")
            .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
    }

    pub fn start(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| PipelineError::StateChange(format!("Failed to start pipeline: {e}")))?;
        info!("Receiver pipeline started");
        Ok(())
    }

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
