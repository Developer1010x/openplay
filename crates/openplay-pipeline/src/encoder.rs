use gstreamer as gst;
use gstreamer::prelude::*;
use tracing::{debug, info, warn};

use crate::PipelineError;

/// Supported encoder types, in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderType {
    /// VA-API hardware encoder (Intel + AMD via Mesa).
    VaH264,
    /// NVIDIA NVENC hardware encoder.
    NvH264,
    /// Software x264 encoder (fallback).
    X264,
}

impl EncoderType {
    /// Returns the GStreamer element factory name.
    pub fn factory_name(&self) -> &'static str {
        match self {
            EncoderType::VaH264 => "vah264enc",
            EncoderType::NvH264 => "nvh264enc",
            EncoderType::X264 => "x264enc",
        }
    }

    /// Returns human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            EncoderType::VaH264 => "VA-API H.264 (Hardware)",
            EncoderType::NvH264 => "NVENC H.264 (Hardware)",
            EncoderType::X264 => "x264 (Software)",
        }
    }

    /// Whether this is a hardware encoder.
    pub fn is_hardware(&self) -> bool {
        !matches!(self, EncoderType::X264)
    }
}

/// Probes the GStreamer registry to find the best available H.264 encoder.
///
/// Priority: vah264enc > nvh264enc > x264enc
///
/// Returns the best encoder type, or an error if none are available.
pub fn probe_best_encoder() -> Result<EncoderType, PipelineError> {
    let candidates = [EncoderType::VaH264, EncoderType::NvH264, EncoderType::X264];

    let registry = gst::Registry::get();

    for encoder in &candidates {
        let factory_name = encoder.factory_name();
        if let Some(factory) = registry.find_feature(factory_name, gst::ElementFactory::static_type()) {
            debug!(encoder = factory_name, "Found encoder in registry");
            // Try to actually create an instance to verify it works
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
        EncoderType::X264 => {
            encoder.set_property("bitrate", bitrate_kbps);
            encoder.set_property("key-int-max", 60u32);
            encoder.set_property("bframes", 0u32);
            encoder.set_property_from_str("tune", "zerolatency");
            encoder.set_property_from_str("speed-preset", "ultrafast");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encoder_type_properties() {
        assert_eq!(EncoderType::VaH264.factory_name(), "vah264enc");
        assert_eq!(EncoderType::NvH264.factory_name(), "nvh264enc");
        assert_eq!(EncoderType::X264.factory_name(), "x264enc");
        assert!(EncoderType::VaH264.is_hardware());
        assert!(EncoderType::NvH264.is_hardware());
        assert!(!EncoderType::X264.is_hardware());
    }
}
