use std::sync::Arc;

use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::VideoFrameExt;
use tracing::{debug, error, info, trace, warn};

use crate::encoder::build_decoder_element;
use crate::PipelineError;

/// One decoded frame, packed tightly as RGBA and ready for upload.
///
/// GStreamer pads rows out to a hardware-friendly stride, which is rarely
/// `width * 4`. Passing the mapped buffer straight to a texture uploader
/// therefore produces the classic diagonal-shear image, so the rows are
/// repacked here — once, at the only place that knows the stride.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, RGBA order, no row padding.
    pub rgba: Vec<u8>,
}

/// Called on a GStreamer streaming thread for every decoded frame.
pub type FrameHandler = Arc<dyn Fn(VideoFrame) + Send + Sync>;

/// GStreamer pipeline for the receiver (WebRTC → decode → appsink).
///
/// Uses `appsink` with RGBA output for cross-platform video display.
/// The consuming application (egui, etc.) receives frames through the handler
/// passed to [`Self::with_frame_handler`] and renders them however it likes.
pub struct ReceiverPipeline {
    pipeline: gst::Pipeline,
    webrtcbin: gst::Element,
}

impl ReceiverPipeline {
    /// Creates a receiver pipeline that decodes but discards every frame.
    ///
    /// Useful for tests and for negotiating a session before a display exists;
    /// real consumers want [`Self::with_frame_handler`].
    pub fn new() -> Result<Self> {
        Self::with_frame_handler(Arc::new(|_| {}))
    }

    /// Creates a receiver pipeline that hands every decoded frame to `on_frame`.
    ///
    /// The handler is installed on the appsink at the moment the decode chain is
    /// built, which is the only workable time: the appsink does not exist until
    /// `webrtcbin` produces a pad, so a caller that tried to fetch it up front
    /// would get `None` and silently never render.
    ///
    /// `on_frame` runs on a GStreamer streaming thread. It must not block — the
    /// appsink is configured to drop rather than queue, so a slow handler costs
    /// frames rather than latency.
    pub fn with_frame_handler(on_frame: FrameHandler) -> Result<Self> {
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

            // `current_caps` is the only reliable source here: querying a
            // webrtcbin src pad returns the generic `application/x-rtp`
            // template, which carries no media field, so a caps-less pad must
            // be treated as video rather than discarded — dropping it is how a
            // session ends up connected with a permanently black screen.
            // Match the `media` field rather than searching the whole caps
            // string: "video" also occurs inside the base64 of
            // sprop-parameter-sets, so a substring test both false-positives
            // and — when caps are absent — false-negatives into a black screen.
            let is_video = match pad
                .current_caps()
                .and_then(|c| c.structure(0).map(|s| s.to_owned()))
            {
                Some(structure) => {
                    debug!(caps = %structure, "WebRTC pad added");
                    structure.get::<String>("media").as_deref() == Ok("video")
                }
                None => {
                    debug!("WebRTC pad added with no caps yet — assuming video");
                    true
                }
            };

            if is_video {
                if let Err(e) = Self::link_video_chain(&pipeline, pad, on_frame.clone()) {
                    error!("Failed to link video chain: {e}");
                }
            } else {
                debug!("Ignoring non-video pad");
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
    fn link_video_chain(
        pipeline: &gst::Pipeline,
        src_pad: &gst::Pad,
        on_frame: FrameHandler,
    ) -> Result<()> {
        // Decoding must not run on webrtcbin's RTP receive thread: a slow
        // decode there stalls RTCP and ICE keepalives as well as video.
        let queue = gst::ElementFactory::make("queue")
            .property("max-size-buffers", 3u32)
            .property_from_str("leaky", "downstream")
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("queue: {e}")))?;

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
            // egui paces its own redraws, so letting the appsink block on the
            // pipeline clock would only add latency to a live cast.
            .sync(false)
            .build();

        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    match Self::frame_from_sample(&sample) {
                        Some(frame) => {
                            trace!(width = frame.width, height = frame.height, "Decoded frame");
                            on_frame(frame);
                        }
                        None => {
                            // A sample we cannot describe is a bug rather than a
                            // stream condition, but dropping one frame is far
                            // better than tearing the pipeline down mid-cast.
                            warn!("Dropped a frame that could not be read as RGBA");
                        }
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        pipeline
            .add_many([
                &queue,
                &depay,
                &parse,
                &decoder,
                &convert,
                &rgba_caps,
                appsink.upcast_ref(),
            ])
            .map_err(|e| PipelineError::Gstreamer(format!("Failed to add decode chain: {e}")))?;

        gst::Element::link_many([
            &queue,
            &depay,
            &parse,
            &decoder,
            &convert,
            &rgba_caps,
            appsink.upcast_ref(),
        ])
        .map_err(|e| PipelineError::Gstreamer(format!("Failed to link decode chain: {e}")))?;

        for elem in [
            &queue,
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

        let queue_sink = queue.static_pad("sink").context("No sink pad on queue")?;
        src_pad.link(&queue_sink).map_err(|e| {
            PipelineError::Gstreamer(format!("Failed to link to decode chain: {e}"))
        })?;

        info!("Video decoding chain linked (RGBA appsink)");
        Ok(())
    }

    /// Converts one appsink sample into a tightly packed RGBA frame.
    ///
    /// Returns `None` if the sample carries no caps, no buffer, or caps that do
    /// not describe raw video — all of which mean the chain is misconfigured
    /// rather than that the stream ended.
    fn frame_from_sample(sample: &gst::Sample) -> Option<VideoFrame> {
        let caps = sample.caps()?;
        let info = gst_video::VideoInfo::from_caps(caps).ok()?;
        let buffer = sample.buffer()?;
        let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info).ok()?;

        let width = frame.width();
        let height = frame.height();
        let stride = frame.plane_stride()[0] as usize;
        let data = frame.plane_data(0).ok()?;

        let row_bytes = width as usize * 4;
        let mut rgba = Vec::with_capacity(row_bytes * height as usize);

        if stride == row_bytes {
            // Already tightly packed — the common case for x264/avdec output.
            rgba.extend_from_slice(&data[..row_bytes * height as usize]);
        } else {
            for row in 0..height as usize {
                let start = row * stride;
                rgba.extend_from_slice(data.get(start..start + row_bytes)?);
            }
        }

        Some(VideoFrame {
            width,
            height,
            rgba,
        })
    }

    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }

    pub fn webrtcbin(&self) -> &gst::Element {
        &self.webrtcbin
    }

    /// Installs a handler for GStreamer bus errors and warnings.
    ///
    /// The sender pipelines all have this; the receiver had none, which meant a
    /// decode failure produced a black window and no explanation anywhere.
    pub fn setup_bus_watch<F>(&self, callback: F) -> Result<()>
    where
        F: Fn(&gst::Bus, &gst::Message) -> gst::BusSyncReply + Send + Sync + 'static,
    {
        let bus = self.pipeline.bus().context("Pipeline has no bus")?;
        bus.set_sync_handler(callback);
        debug!("Receiver bus watch set up");
        Ok(())
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
