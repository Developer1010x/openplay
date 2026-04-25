use openplay_miracast::wfd_params::{
    CeaResolutions, WfdAudioCodecs, WfdClientRtpPorts, WfdVideoFormats, negotiate_resolution,
};

// ── CeaResolutions ────────────────────────────────────────────────────────────

#[test]
fn cea_zero_supports_nothing() {
    let cea = CeaResolutions(0);
    assert!(!cea.supports(CeaResolutions::RES_1920X1080_P60));
    assert!(!cea.supports(CeaResolutions::RES_640X480_P60));
}

#[test]
fn cea_all_defined_constants_are_distinct_bits() {
    let constants = [
        CeaResolutions::RES_640X480_P60,
        CeaResolutions::RES_720X480_P60,
        CeaResolutions::RES_720X576_P50,
        CeaResolutions::RES_1280X720_P30,
        CeaResolutions::RES_1280X720_P60,
        CeaResolutions::RES_1920X1080_P30,
        CeaResolutions::RES_1920X1080_P60,
    ];
    for i in 0..constants.len() {
        for j in (i + 1)..constants.len() {
            assert_ne!(constants[i], constants[j], "constants[{i}] and constants[{j}] must differ");
            assert_eq!(
                constants[i] & constants[j],
                0,
                "constants[{i}] and constants[{j}] must not share bits"
            );
        }
    }
}

#[test]
fn cea_combined_bitmask_supports_all_set_bits() {
    let mask = CeaResolutions::RES_1920X1080_P60
        | CeaResolutions::RES_1280X720_P60
        | CeaResolutions::RES_720X480_P60;
    let cea = CeaResolutions(mask);
    assert!(cea.supports(CeaResolutions::RES_1920X1080_P60));
    assert!(cea.supports(CeaResolutions::RES_1280X720_P60));
    assert!(cea.supports(CeaResolutions::RES_720X480_P60));
    assert!(!cea.supports(CeaResolutions::RES_640X480_P60));
    assert!(!cea.supports(CeaResolutions::RES_1920X1080_P30));
}

// ── WfdVideoFormats ───────────────────────────────────────────────────────────

#[test]
fn video_formats_default_encodes_and_parses() {
    let vf = WfdVideoFormats::default();
    let encoded = vf.encode();
    let parsed = WfdVideoFormats::parse(&encoded).unwrap();
    assert_eq!(parsed.profile, vf.profile);
    assert_eq!(parsed.level, vf.level);
    assert_eq!(parsed.cea_resolutions.0, vf.cea_resolutions.0);
    assert_eq!(parsed.frame_rate_ctrl, vf.frame_rate_ctrl);
}

#[test]
fn video_formats_parse_returns_none_for_short_string() {
    assert!(WfdVideoFormats::parse("00 00").is_none());
    assert!(WfdVideoFormats::parse("").is_none());
}

#[test]
fn video_formats_encode_has_11_parts() {
    let vf = WfdVideoFormats::default();
    let encoded = vf.encode();
    assert_eq!(encoded.split_whitespace().count(), 11);
}

#[test]
fn video_formats_custom_values_roundtrip() {
    let vf = WfdVideoFormats {
        native: 0x02,
        preferred_display_mode: 0,
        profile: 0x02, // CHP
        level: 0x20,   // Level 4
        cea_resolutions: CeaResolutions(CeaResolutions::RES_1920X1080_P60),
        vesa_resolutions: 0,
        hh_resolutions: 0,
        latency: 0,
        min_slice_size: 0,
        slice_enc_params: 0,
        frame_rate_ctrl: 0x11,
    };
    let encoded = vf.encode();
    let parsed = WfdVideoFormats::parse(&encoded).unwrap();
    assert_eq!(parsed.profile, 0x02);
    assert_eq!(parsed.level, 0x20);
    assert!(parsed.cea_resolutions.supports(CeaResolutions::RES_1920X1080_P60));
}

// ── negotiate_resolution ──────────────────────────────────────────────────────

#[test]
fn negotiate_prefers_1080p60_when_both_support_it() {
    let both = WfdVideoFormats {
        cea_resolutions: CeaResolutions(
            CeaResolutions::RES_1920X1080_P60 | CeaResolutions::RES_1920X1080_P30,
        ),
        ..Default::default()
    };
    assert_eq!(negotiate_resolution(&both, &both), (1920, 1080, 60));
}

#[test]
fn negotiate_prefers_1080p30_when_no_60() {
    let source = WfdVideoFormats {
        cea_resolutions: CeaResolutions(
            CeaResolutions::RES_1920X1080_P30 | CeaResolutions::RES_1280X720_P60,
        ),
        ..Default::default()
    };
    let sink = WfdVideoFormats {
        cea_resolutions: CeaResolutions(CeaResolutions::RES_1920X1080_P30),
        ..Default::default()
    };
    assert_eq!(negotiate_resolution(&source, &sink), (1920, 1080, 30));
}

#[test]
fn negotiate_720p60_when_no_1080p() {
    let source = WfdVideoFormats {
        cea_resolutions: CeaResolutions(
            CeaResolutions::RES_1280X720_P60 | CeaResolutions::RES_1280X720_P30,
        ),
        ..Default::default()
    };
    let sink = WfdVideoFormats {
        cea_resolutions: CeaResolutions(CeaResolutions::RES_1280X720_P60),
        ..Default::default()
    };
    assert_eq!(negotiate_resolution(&source, &sink), (1280, 720, 60));
}

#[test]
fn negotiate_720p30_when_only_30() {
    let source = WfdVideoFormats {
        cea_resolutions: CeaResolutions(CeaResolutions::RES_1280X720_P30),
        ..Default::default()
    };
    let sink = WfdVideoFormats {
        cea_resolutions: CeaResolutions(
            CeaResolutions::RES_1280X720_P30 | CeaResolutions::RES_1920X1080_P60,
        ),
        ..Default::default()
    };
    assert_eq!(negotiate_resolution(&source, &sink), (1280, 720, 30));
}

#[test]
fn negotiate_480p_when_lowest_common() {
    let source = WfdVideoFormats {
        cea_resolutions: CeaResolutions(CeaResolutions::RES_720X480_P60),
        ..Default::default()
    };
    let sink = WfdVideoFormats {
        cea_resolutions: CeaResolutions(
            CeaResolutions::RES_720X480_P60 | CeaResolutions::RES_1920X1080_P60,
        ),
        ..Default::default()
    };
    assert_eq!(negotiate_resolution(&source, &sink), (720, 480, 60));
}

#[test]
fn negotiate_fallback_when_no_common_cea() {
    let source = WfdVideoFormats {
        cea_resolutions: CeaResolutions(CeaResolutions::RES_640X480_P60),
        ..Default::default()
    };
    let sink = WfdVideoFormats {
        cea_resolutions: CeaResolutions(CeaResolutions::RES_1920X1080_P60),
        ..Default::default()
    };
    assert_eq!(negotiate_resolution(&source, &sink), (1280, 720, 30));
}

// ── WfdClientRtpPorts ─────────────────────────────────────────────────────────

#[test]
fn rtp_ports_default_display() {
    let s = WfdClientRtpPorts::default().to_string();
    assert!(s.contains("RTP/AVP/UDP;unicast"));
    assert!(s.contains("1028"));
    assert!(s.contains("mode=play"));
}

#[test]
fn rtp_ports_roundtrip() {
    let p = WfdClientRtpPorts {
        profile: "RTP/AVP/UDP;unicast".to_string(),
        rtp_port0: 4096,
        rtp_port1: 0,
        mode: "mode=play".to_string(),
    };
    let s = p.to_string();
    let parsed = WfdClientRtpPorts::parse(&s).unwrap();
    assert_eq!(parsed.rtp_port0, 4096);
    assert_eq!(parsed.rtp_port1, 0);
    assert_eq!(parsed.mode, "mode=play");
}

#[test]
fn rtp_ports_parse_returns_none_for_short_string() {
    assert!(WfdClientRtpPorts::parse("RTP/AVP/UDP;unicast 1028").is_none());
    assert!(WfdClientRtpPorts::parse("").is_none());
}

// ── WfdAudioCodecs ────────────────────────────────────────────────────────────

#[test]
fn audio_codecs_default_display_has_all_codecs() {
    let s = WfdAudioCodecs::default().to_string();
    assert!(s.contains("LPCM"));
    assert!(s.contains("AAC"));
    assert!(s.contains("AC3"));
}

#[test]
fn audio_codecs_zero_ac3() {
    let ac = WfdAudioCodecs::default();
    assert_eq!(ac.ac3, 0);
}

#[test]
fn audio_codecs_display_format_has_correct_segments() {
    let s = WfdAudioCodecs::default().to_string();
    // Expect "LPCM XXXXXXXX XXXXXXXX, AAC XXXXXXXX XXXXXXXX, AC3 XXXXXXXX XXXXXXXX"
    let parts: Vec<&str> = s.split(',').collect();
    assert_eq!(parts.len(), 3, "expected 3 codec entries");
}
