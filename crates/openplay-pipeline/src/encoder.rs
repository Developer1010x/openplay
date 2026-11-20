use gstreamer as gst;
use gstreamer::prelude::*;
use tracing::{debug, info, warn};

use crate::PipelineError;

/// Supported encoder types, in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderType {
    /// VA-API hardware encoder (Intel + AMD via Mesa — Linux).
    VaH264,
    /// VA-API low-power hardware encoder (Linux).
    ///
    /// Several Intel generations — Tiger Lake among them — expose only the
    /// low-power H.264 entrypoint, so the `va` plugin registers `vah264lpenc`
    /// and never `vah264enc`. Probing for the latter alone drops those machines
    /// to software encoding while a working hardware encoder sits unused.
    VaH264Lp,
    /// Legacy `gstreamer-vaapi` hardware encoder (Linux).
    ///
    /// Superseded by the `va` plugin, but still the only VA-API encoder present
    /// on distributions that ship `gstreamer1.0-vaapi` without a new enough
    /// `gst-plugins-bad`.
    VaapiH264,
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
            EncoderType::VaH264Lp => "vah264lpenc",
            EncoderType::VaapiH264 => "vaapih264enc",
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
            EncoderType::VaH264Lp => "VA-API H.264 low-power (Hardware)",
            EncoderType::VaapiH264 => "VA-API H.264 legacy (Hardware)",
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
        &[
            EncoderType::VaH264,
            EncoderType::VaH264Lp,
            EncoderType::VaapiH264,
            EncoderType::NvH264,
            EncoderType::X264,
        ]
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

/// Sets an enum-valued property to the first nick the element actually accepts.
///
/// `set_property_from_str` panics on an unknown property *and* on a nick the
/// property's enum does not define, which is not safe for the VA encoders: the
/// `va` plugin builds `rate-control` as a driver-specific enum — the type is
/// literally named `GstVaEncoderRateControl_H264_LP_renderD128` — and populates
/// it from what the hardware reports. A Tiger Lake iGPU offers only `cqp` on
/// `vah264lpenc`, so the hardcoded `"cbr"` took down the whole cast:
///
/// ```text
/// property 'rate-control' of type 'GstVaH264LPEnc' can't be set from string 'cbr'
/// ```
///
/// `probe_best_encoder` could not have caught it. It instantiates each
/// candidate, and instantiation succeeds — it is configuring the element that
/// fails, one step later.
///
/// Returns the nick that was set, or `None` if the property is absent or none
/// of `preferred` is available, in which case the element keeps its default.
fn set_enum_property(element: &gst::Element, property: &str, preferred: &[&str]) -> Option<String> {
    let Some(pspec) = element.find_property(property) else {
        debug!(property, "Encoder has no such property — leaving it unset");
        return None;
    };

    let Some(enum_class) = gst::glib::EnumClass::with_type(pspec.value_type()) else {
        // Not an enum: fall back to the string setter, which is safe now that
        // the property is known to exist.
        if let Some(first) = preferred.first() {
            element.set_property_from_str(property, first);
            return Some((*first).to_string());
        }
        return None;
    };

    for nick in preferred {
        if enum_class.value_by_nick(nick).is_some() {
            element.set_property_from_str(property, nick);
            debug!(property, nick, "Set encoder property");
            return Some((*nick).to_string());
        }
    }

    let available: Vec<&str> = enum_class
        .values()
        .iter()
        .filter_map(|v| v.nick().into())
        .collect();
    warn!(
        property,
        wanted = ?preferred,
        ?available,
        "Encoder supports none of the preferred values — keeping its default"
    );
    None
}

/// Configures encoder properties for low-latency streaming.
pub fn configure_encoder(encoder: &gst::Element, encoder_type: EncoderType, bitrate_kbps: u32) {
    match encoder_type {
        // Both `va` plugin encoders take the same property set.
        EncoderType::VaH264 | EncoderType::VaH264Lp => {
            // Constant bitrate is what a live cast wants; a driver that only
            // offers cqp gets vbr if it has it, and otherwise keeps its default
            // rather than bringing the cast down.
            set_enum_property(encoder, "rate-control", &["cbr", "vbr", "cqp"]);
            encoder.set_property("bitrate", bitrate_kbps);
            encoder.set_property("key-int-max", 60u32);
            encoder.set_property_from_str("b-frames", "0");
            encoder.set_property("ref-frames", 1u32);
            encoder.set_property_from_str("target-usage", "6");
        }
        EncoderType::VaapiH264 => {
            // The legacy plugin spells these differently to the `va` one and has
            // no target-usage or ref-frames property.
            set_enum_property(encoder, "rate-control", &["cbr", "vbr"]);
            encoder.set_property("bitrate", bitrate_kbps);
            encoder.set_property("keyframe-period", 60u32);
            encoder.set_property("max-bframes", 0u32);
        }
        EncoderType::NvH264 => {
            set_enum_property(encoder, "rc-mode", &["cbr", "vbr"]);
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
            set_enum_property(encoder, "rc-mode", &["cbr", "vbr"]);
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
        assert_eq!(EncoderType::VaH264Lp.factory_name(), "vah264lpenc");
        assert_eq!(EncoderType::VaapiH264.factory_name(), "vaapih264enc");
        assert_eq!(EncoderType::NvH264.factory_name(), "nvh264enc");
        assert_eq!(EncoderType::VtH264.factory_name(), "vtenc_h264");
        assert_eq!(EncoderType::MfH264.factory_name(), "mfh264enc");
        assert_eq!(EncoderType::X264.factory_name(), "x264enc");
        assert!(EncoderType::VaH264.is_hardware());
        assert!(EncoderType::VaH264Lp.is_hardware());
        assert!(EncoderType::VaapiH264.is_hardware());
        assert!(EncoderType::NvH264.is_hardware());
        assert!(EncoderType::VtH264.is_hardware());
        assert!(EncoderType::MfH264.is_hardware());
        assert!(!EncoderType::X264.is_hardware());
    }
}
