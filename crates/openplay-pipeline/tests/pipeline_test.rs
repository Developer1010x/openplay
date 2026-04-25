use openplay_pipeline::{CaptureConfig, EncoderType, PipelineError};

// ── CaptureConfig ──────────────────────────────────────────────────────────────

#[test]
#[cfg(not(target_os = "linux"))]
fn capture_config_stores_dimensions() {
    let cfg = CaptureConfig::new(1920, 1080, 30);
    assert_eq!(cfg.width, 1920);
    assert_eq!(cfg.height, 1080);
    assert_eq!(cfg.framerate, 30);
}

#[test]
#[cfg(not(target_os = "linux"))]
fn capture_config_4k() {
    let cfg = CaptureConfig::new(3840, 2160, 60);
    assert_eq!(cfg.width, 3840);
    assert_eq!(cfg.height, 2160);
    assert_eq!(cfg.framerate, 60);
}

#[test]
#[cfg(target_os = "linux")]
fn capture_config_linux_stores_pw_fields() {
    let cfg = CaptureConfig::new(5, 42, 1920, 1080, 30);
    assert_eq!(cfg.pw_fd, 5);
    assert_eq!(cfg.node_id, 42);
    assert_eq!(cfg.width, 1920);
    assert_eq!(cfg.height, 1080);
    assert_eq!(cfg.framerate, 30);
}

// ── EncoderType ────────────────────────────────────────────────────────────────

#[test]
fn encoder_type_factory_names_are_nonempty() {
    for enc in [
        EncoderType::VaH264,
        EncoderType::NvH264,
        EncoderType::VtH264,
        EncoderType::MfH264,
        EncoderType::X264,
    ] {
        assert!(!enc.factory_name().is_empty());
    }
}

#[test]
fn encoder_type_labels_are_nonempty() {
    for enc in [
        EncoderType::VaH264,
        EncoderType::NvH264,
        EncoderType::VtH264,
        EncoderType::MfH264,
        EncoderType::X264,
    ] {
        assert!(!enc.label().is_empty());
    }
}

#[test]
fn x264_is_software() {
    assert!(!EncoderType::X264.is_hardware());
}

#[test]
fn hardware_encoders_are_hardware() {
    for enc in [
        EncoderType::VaH264,
        EncoderType::NvH264,
        EncoderType::VtH264,
        EncoderType::MfH264,
    ] {
        assert!(enc.is_hardware(), "{:?} should be hardware", enc);
    }
}

#[test]
fn encoder_factory_names_match_gstreamer_conventions() {
    assert_eq!(EncoderType::VaH264.factory_name(), "vah264enc");
    assert_eq!(EncoderType::NvH264.factory_name(), "nvh264enc");
    assert_eq!(EncoderType::VtH264.factory_name(), "vtenc_h264");
    assert_eq!(EncoderType::MfH264.factory_name(), "mfh264enc");
    assert_eq!(EncoderType::X264.factory_name(), "x264enc");
}

#[test]
fn encoder_labels_contain_codec_name() {
    for enc in [
        EncoderType::VaH264,
        EncoderType::NvH264,
        EncoderType::VtH264,
        EncoderType::MfH264,
        EncoderType::X264,
    ] {
        assert!(
            enc.label().contains("H.264") || enc.label().contains("x264"),
            "{:?} label should mention codec",
            enc
        );
    }
}

// ── PipelineError ──────────────────────────────────────────────────────────────

#[test]
fn pipeline_error_messages_contain_context() {
    let e = PipelineError::Gstreamer("init failed".to_string());
    assert!(e.to_string().contains("init failed"));

    let e = PipelineError::MissingElement("vah264enc".to_string());
    assert!(e.to_string().contains("vah264enc"));

    let e = PipelineError::StateChange("NULL -> PLAYING".to_string());
    assert!(e.to_string().contains("NULL -> PLAYING"));

    let e = PipelineError::Sdp("invalid SDP".to_string());
    assert!(e.to_string().contains("invalid SDP"));
}

#[test]
fn pipeline_error_no_encoder_message() {
    let e = PipelineError::NoEncoder;
    assert!(!e.to_string().is_empty());
}
