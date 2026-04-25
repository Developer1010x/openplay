use openplay_discovery::{
    airplay_record::AirPlayTxtRecord,
    record::TxtRecord,
    AirPlayReceiverInfo, MiracastReceiverInfo, ReceiverInfo,
    SERVICE_TYPE, AIRPLAY_SERVICE_TYPE,
};

// ── TxtRecord (OpenPlay) ───────────────────────────────────────────────────────

#[test]
fn txt_record_roundtrip_all_fields() {
    let record = TxtRecord {
        version: 1,
        display_name: "Living Room TV".to_string(),
        capabilities: "audio,cursor".to_string(),
        video_codecs: "h264,h265".to_string(),
        audio_codecs: "opus".to_string(),
        resolution: "3840x2160".to_string(),
        max_fps: 60,
        fingerprint: "AA:BB:CC:DD".to_string(),
        port: 7290,
    };
    let props = record.to_properties();
    let parsed = TxtRecord::from_properties(&props).unwrap();

    assert_eq!(parsed.version, 1);
    assert_eq!(parsed.display_name, "Living Room TV");
    assert_eq!(parsed.capabilities, "audio,cursor");
    assert_eq!(parsed.video_codecs, "h264,h265");
    assert_eq!(parsed.audio_codecs, "opus");
    assert_eq!(parsed.resolution, "3840x2160");
    assert_eq!(parsed.max_fps, 60);
    assert_eq!(parsed.fingerprint, "AA:BB:CC:DD");
    assert_eq!(parsed.port, 7290);
}

#[test]
fn txt_record_missing_version_returns_none() {
    // from_properties requires "v" key for version
    let props: Vec<(String, String)> = vec![
        ("dn".to_string(), "TV".to_string()),
    ];
    assert!(TxtRecord::from_properties(&props).is_none());
}

#[test]
fn txt_record_missing_display_name_returns_none() {
    let props: Vec<(String, String)> = vec![
        ("v".to_string(), "1".to_string()),
    ];
    assert!(TxtRecord::from_properties(&props).is_none());
}

#[test]
fn txt_record_optional_fields_default_when_absent() {
    let props = vec![
        ("v".to_string(), "1".to_string()),
        ("dn".to_string(), "Device".to_string()),
    ];
    let record = TxtRecord::from_properties(&props).unwrap();
    // Optional fields should have defaults
    assert_eq!(record.video_codecs, "h264");
    assert_eq!(record.audio_codecs, "opus");
    assert_eq!(record.resolution, "1920x1080");
    assert_eq!(record.max_fps, 30);
}

#[test]
fn txt_record_to_properties_has_expected_keys() {
    let record = TxtRecord {
        version: 1,
        display_name: "Test".to_string(),
        capabilities: "".to_string(),
        video_codecs: "h264".to_string(),
        audio_codecs: "opus".to_string(),
        resolution: "1920x1080".to_string(),
        max_fps: 30,
        fingerprint: "AA:BB".to_string(),
        port: 7290,
    };
    let props = record.to_properties();
    let keys: Vec<&str> = props.iter().map(|(k, _)| k.as_str()).collect();
    assert!(keys.contains(&"v"));
    assert!(keys.contains(&"dn"));
    assert!(keys.contains(&"fp"));
    assert!(keys.contains(&"port"));
}

// ── AirPlayTxtRecord ───────────────────────────────────────────────────────────

#[test]
fn airplay_txt_record_full_parse() {
    let props = vec![
        ("deviceid".to_string(), "AA:BB:CC:DD:EE:FF".to_string()),
        ("features".to_string(), "0x5A7FFFF7,0x1E".to_string()),
        ("model".to_string(), "AppleTV6,2".to_string()),
        ("pk".to_string(), "aabbccdd".to_string()),
        ("flags".to_string(), "0x4".to_string()),
        ("srcvers".to_string(), "550.10".to_string()),
        ("protovers".to_string(), "1.1".to_string()),
    ];
    let record = AirPlayTxtRecord::from_properties(&props).unwrap();
    assert_eq!(record.device_id, "AA:BB:CC:DD:EE:FF");
    assert_eq!(record.features, "0x5A7FFFF7,0x1E");
    assert_eq!(record.model, "AppleTV6,2");
    assert_eq!(record.pk, "aabbccdd");
    assert_eq!(record.source_version, "550.10");
    assert_eq!(record.protocol_version, "1.1");
}

#[test]
fn airplay_txt_record_minimal_only_deviceid() {
    let props = vec![
        ("deviceid".to_string(), "11:22:33:44:55:66".to_string()),
    ];
    let record = AirPlayTxtRecord::from_properties(&props).unwrap();
    assert_eq!(record.device_id, "11:22:33:44:55:66");
    assert!(record.features.is_empty());
    assert!(record.model.is_empty());
}

#[test]
fn airplay_txt_record_missing_deviceid_returns_none() {
    let props = vec![
        ("features".to_string(), "0x5A7FFFF7".to_string()),
    ];
    assert!(AirPlayTxtRecord::from_properties(&props).is_none());
}

#[test]
fn airplay_txt_record_empty_props_returns_none() {
    let props: Vec<(String, String)> = vec![];
    assert!(AirPlayTxtRecord::from_properties(&props).is_none());
}

// ── Service type constants ─────────────────────────────────────────────────────

#[test]
fn service_type_constants_are_valid_mdns() {
    assert!(SERVICE_TYPE.ends_with(".local."));
    assert!(SERVICE_TYPE.contains("openplay"));
    assert!(AIRPLAY_SERVICE_TYPE.ends_with(".local."));
    assert!(AIRPLAY_SERVICE_TYPE.contains("airplay"));
}

// ── ReceiverInfo construction ──────────────────────────────────────────────────

#[test]
fn receiver_info_fields_accessible() {
    let info = ReceiverInfo {
        name: "openplay-tv._openplay._tcp.local.".to_string(),
        display_name: "TV".to_string(),
        addresses: vec!["192.168.1.5".parse().unwrap()],
        port: 7290,
        fingerprint: "AA:BB".to_string(),
        video_codecs: vec!["h264".to_string()],
        resolution: "1920x1080".to_string(),
        max_fps: 30,
    };
    assert_eq!(info.port, 7290);
    assert_eq!(info.display_name, "TV");
    assert!(!info.addresses.is_empty());
}

#[test]
fn airplay_receiver_info_fields() {
    let info = AirPlayReceiverInfo {
        name: "AppleTV._airplay._tcp.local.".to_string(),
        display_name: "Apple TV".to_string(),
        addresses: vec!["10.0.0.1".parse().unwrap()],
        port: 7000,
        device_id: "AA:BB:CC:DD:EE:FF".to_string(),
        features: "0x5A7FFFF7".to_string(),
        model: "AppleTV6,2".to_string(),
    };
    assert_eq!(info.port, 7000);
    assert_eq!(info.device_id, "AA:BB:CC:DD:EE:FF");
    assert_eq!(info.model, "AppleTV6,2");
}

#[test]
fn miracast_receiver_info_fields() {
    let info = MiracastReceiverInfo {
        name: "WFD-Display._display._tcp.local.".to_string(),
        display_name: "Smart TV".to_string(),
        addresses: vec!["192.168.1.10".parse().unwrap()],
        port: 7236,
        device_info: "miracast-device".to_string(),
    };
    assert_eq!(info.port, 7236);
    assert_eq!(info.display_name, "Smart TV");
}
