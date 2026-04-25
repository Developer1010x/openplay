use gstreamer as gst;
use gstreamer::prelude::*;
use tracing::{debug, info, warn};

use crate::PipelineError;

/// Supported encoder types, in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderType {
    /// VA-API hardware encoder (Intel + AMD via Mesa — Linux).
    VaH264,
    /// NVIDIA NVENC hardware encoder (Linux/Windows).
    NvH264,
    /// Apple VideoToolbox hardware encoder (macOS).
    VtH264,
    /// Windows Media Foundation hardware encoder (Windows).
    MfH264,
    /// Software x264 encoder (all platforms — fallback).
    X264,
}

impl EncoderType {
    /// Returns the GStreamer element factory name.
    pub fn factory_name(&self) -> &'static str {
        match self {
            EncoderType::VaH264 => "vah264enc",
            EncoderType::NvH264 => "nvh264enc",
            EncoderType::VtH264 => "vtenc_h264",
            EncoderType::MfH264 => "mfh264enc",
            EncoderType::X264 => "x264enc",
        }
    }

    /// Returns human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            EncoderType::VaH264 => "VA-API H.264 (Hardware)",
            EncoderType::NvH264 => "NVENC H.264 (Hardware)",
            EncoderType::VtH264 => "VideoToolbox H.264 (Hardware)",
            EncoderType::MfH264 => "Media Foundation H.264 (Hardware)",
            EncoderType::X264 => "x264 (Software)",
        }
    }

    /// Whether this is a hardware encoder.
    pub fn is_hardware(&self) -> bool {
        !matches!(self, EncoderType::X264)
    }
}

/// Probes the GStreamer registry to find the best available H.264 encoder
/// for the current platform.
///
/// Platform priority order:
/// - **Linux**: vah264enc (VA-API) → nvh264enc (NVENC) → x264enc
/// - **macOS**: vtenc_h264 (VideoToolbox) → x264enc
/// - **Windows**: mfh264enc (Media Foundation) → nvh264enc (NVENC) → x264enc
pub fn probe_best_encoder() -> Result<EncoderType, PipelineError> {
    let candidates = platform_encoder_candidates();
    let registry = gst::Registry::get();

    for encoder in candidates {
        let factory_name = encoder.factory_name();
        if registry
            .find_feature(factory_name, gst::ElementFactory::static_type())
            .is_some()
        {
            debug!(encoder = factory_name, "Found encoder in registry");
            if gst::ElementFactory::make(factory_name).build().is_ok() {
                info!(
                    encoder = factory_name,
                    label = encoder.label(),
                    hw = encoder.is_hardware(),
                    "Selected encoder"
                );
                return Ok(*encoder);
            } else {
                warn!(
                    encoder = factory_name,
                    "Encoder found in registry but failed to instantiate"
                );
            }
        }
    }

    Err(PipelineError::NoEncoder)
}

/// Returns the ordered list of encoder candidates for the current platform.
fn platform_encoder_candidates() -> &'static [EncoderType] {
    #[cfg(target_os = "linux")]
    {
        &[EncoderType::VaH264, EncoderType::NvH264, EncoderType::X264]
    }
    #[cfg(target_os = "macos")]
    {
        &[EncoderType::VtH264, EncoderType::X264]
    }
    #[cfg(target_os = "windows")]
    {
        &[EncoderType::MfH264, EncoderType::NvH264, EncoderType::X264]
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        &[EncoderType::X264]
    }
}

/// Configures encoder properties for low-latency streaming.
pub fn configure_encoder(
    encoder: &gst::Element,
    encoder_type: EncoderType,
    bitrate_kbps: u32,
) {
    match encoder_type {
        EncoderType::VaH264 => {
            encoder.set_property_from_str("rate-control", "cbr");
            encoder.set_property("bitrate", bitrate_kbps);
            encoder.set_property("key-int-max", 60u32);
            encoder.set_property_from_str("b-frames", "0");
            encoder.set_property("ref-frames", 1u32);
            encoder.set_property_from_str("target-usage", "6");
        }
        EncoderType::NvH264 => {
            encoder.set_property_from_str("rc-mode", "cbr");
            encoder.set_property("bitrate", bitrate_kbps);
            encoder.set_property("gop-size", 60i32);
            encoder.set_property("bframes", 0u32);
            encoder.set_property("zerolatency", true);
        }
        EncoderType::VtH264 => {
            // VideoToolbox (macOS)
            encoder.set_property("bitrate", bitrate_kbps * 1000); // VT uses bits/s
            encoder.set_property("max-keyframe-interval", 60i32);
            encoder.set_property("realtime", true);
            encoder.set_property("allow-frame-reordering", false);
        }
        EncoderType::MfH264 => {
            // Windows Media Foundation
            encoder.set_property("bitrate", bitrate_kbps * 1000); // MF uses bits/s
            encoder.set_property_from_str("rc-mode", "cbr");
            encoder.set_property("low-latency", true);
        }
        EncoderType::X264 => {
            encoder.set_property("bitrate", bitrate_kbps);
            encoder.set_property("key-int-max", 60u32);
            encoder.set_property("bframes", 0u32);
            encoder.set_property_from_str("tune", "zerolatency");
            encoder.set_property_from_str("speed-preset", "ultrafast");
        }
    }
}

/// Builds the platform-appropriate screen capture source element.
///
/// - **Linux**: `pipewiresrc` using the PipeWire fd + node_id from the XDG portal.
/// - **Windows**: `d3d11screencapturesrc` (GStreamer Windows plugin).
/// - **macOS**: `screencapturesrc` (GStreamer 1.22+) or `avfvideosrc` (older).
pub fn build_capture_element(
    #[cfg(target_os = "linux")] pw_fd: i32,
    #[cfg(target_os = "linux")] node_id: u32,
) -> Result<gst::Element, PipelineError> {
    #[cfg(target_os = "linux")]
    {
        gst::ElementFactory::make("pipewiresrc")
            .property("fd", pw_fd)
            .property("path", node_id.to_string())
            .property("do-timestamp", true)
            .property("always-copy", false)
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("pipewiresrc: {e}")))
    }

    #[cfg(target_os = "windows")]
    {
        gst::ElementFactory::make("d3d11screencapturesrc")
            .property("show-cursor", true)
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("d3d11screencapturesrc: {e}")))
    }

    #[cfg(target_os = "macos")]
    {
        // Try screencapturesrc (GStreamer 1.22+) first, fall back to avfvideosrc
        if let Ok(elem) = gst::ElementFactory::make("screencapturesrc")
            .property("show-cursor", true)
            .build()
        {
            return Ok(elem);
        }
        gst::ElementFactory::make("avfvideosrc")
            .property("capture-screen", true)
            .property("capture-screen-cursor", true)
            .build()
            .map_err(|e| PipelineError::MissingElement(format!("avfvideosrc: {e}")))
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Err(PipelineError::MissingElement(
            "No screen capture element available for this platform".to_string(),
        ))
    }
}

/// Builds the platform-appropriate H.264 decoder element.
///
/// Priority: hardware decoders first, software fallback.
pub fn build_decoder_element() -> Result<gst::Element, PipelineError> {
    // Platform-specific hardware decoders first
    #[cfg(target_os = "linux")]
    let hw_candidates = &["vah264dec", "nvh264dec"];
    #[cfg(target_os = "macos")]
    let hw_candidates = &["vtdec_hw", "vtdec"];
    #[cfg(target_os = "windows")]
    let hw_candidates = &["d3d11h264dec", "nvh264dec", "mfdec"];
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let hw_candidates: &[&str] = &[];

    for name in hw_candidates {
        if let Ok(elem) = gst::ElementFactory::make(name).build() {
            info!(decoder = name, "Using hardware decoder");
            return Ok(elem);
        }
    }

    // Software fallback (all platforms)
    gst::ElementFactory::make("avdec_h264")
        .build()
        .map_err(|e| PipelineError::MissingElement(format!("avdec_h264 (software): {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encoder_type_properties() {
        assert_eq!(EncoderType::VaH264.factory_name(), "vah264enc");
        assert_eq!(EncoderType::NvH264.factory_name(), "nvh264enc");
        assert_eq!(EncoderType::VtH264.factory_name(), "vtenc_h264");
        assert_eq!(EncoderType::MfH264.factory_name(), "mfh264enc");
        assert_eq!(EncoderType::X264.factory_name(), "x264enc");
        assert!(EncoderType::VaH264.is_hardware());
        assert!(EncoderType::NvH264.is_hardware());
        assert!(EncoderType::VtH264.is_hardware());
        assert!(EncoderType::MfH264.is_hardware());
        assert!(!EncoderType::X264.is_hardware());
    }
}
