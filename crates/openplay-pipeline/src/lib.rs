//! GStreamer pipeline construction and encoder selection.
//!
//! One pipeline type per casting path — `AirPlaySenderPipeline`,
//! `MiracastSenderPipeline`, `SenderPipeline` (WebRTC) and `ReceiverPipeline` —
//! all fed by a [`CaptureConfig`] carrying the
//! PipeWire node ID, file descriptor, resolution and framerate.
//!
//! [`probe_best_encoder`] queries the GStreamer
//! registry at runtime and additionally tries to instantiate each candidate,
//! because a factory can be registered while the hardware is unavailable.
//! Candidate order is VA-API then NVENC on Linux, VideoToolbox on macOS, Media
//! Foundation then NVENC on Windows, with x264 as the universal fallback.
//!
//! **Never hardcode an encoder type.**

mod airplay_sender_pipeline;
mod capture_config;
mod encoder;
mod miracast_sender_pipeline;
mod receiver_pipeline;
mod sender_pipeline;

pub use airplay_sender_pipeline::AirPlaySenderPipeline;
pub use capture_config::CaptureConfig;
pub use encoder::{probe_best_encoder, EncoderType};
pub use miracast_sender_pipeline::MiracastSenderPipeline;
pub use receiver_pipeline::ReceiverPipeline;
pub use sender_pipeline::SenderPipeline;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum PipelineError {
    #[error("GStreamer error: {0}")]
    Gstreamer(String),

    #[error("No suitable encoder found")]
    NoEncoder,

    #[error("Pipeline element not found: {0}")]
    MissingElement(String),

    #[error("Pipeline state change failed: {0}")]
    StateChange(String),

    #[error("SDP error: {0}")]
    Sdp(String),
}

/// Initializes GStreamer. Must be called before any pipeline operations.
pub fn init() -> Result<(), PipelineError> {
    gstreamer::init().map_err(|e| PipelineError::Gstreamer(e.to_string()))
}
