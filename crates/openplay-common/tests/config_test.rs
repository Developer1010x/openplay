use openplay_common::{AppConfig, OpenPlayError, DEFAULT_PORT};
use std::fs;

#[test]
fn default_config_values() {
    let cfg = AppConfig::default();
    assert_eq!(cfg.port, DEFAULT_PORT);
    assert_eq!(cfg.max_bitrate_kbps, 6000);
    assert_eq!(cfg.framerate, 30);
    assert!(!cfg.force_sw_encode);
    assert!(cfg.airplay_enabled);
    assert!(cfg.miracast_enabled);
    assert!(!cfg.display_name.is_empty());
}

#[test]
fn save_and_load_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    let original = AppConfig {
        display_name: "TestDevice".to_string(),
        port: 9999,
        max_bitrate_kbps: 8000,
        framerate: 60,
        force_sw_encode: true,
        airplay_enabled: false,
        miracast_enabled: false,
    };

    original.save_to(&path).unwrap();
    assert!(path.exists());

    let loaded = AppConfig::load_from(&path).unwrap();
    assert_eq!(loaded.display_name, "TestDevice");
    assert_eq!(loaded.port, 9999);
    assert_eq!(loaded.max_bitrate_kbps, 8000);
    assert_eq!(loaded.framerate, 60);
    assert!(loaded.force_sw_encode);
    assert!(!loaded.airplay_enabled);
    assert!(!loaded.miracast_enabled);
}

#[test]
fn load_from_nonexistent_returns_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does_not_exist.toml");

    let cfg = AppConfig::load_from(&path).unwrap();
    assert_eq!(cfg.port, DEFAULT_PORT);
}

#[test]
fn load_from_invalid_toml_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.toml");
    fs::write(&path, b"not valid toml ][").unwrap();

    assert!(AppConfig::load_from(&path).is_err());
}

#[test]
fn save_creates_parent_directories() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("deep").join("config.toml");

    AppConfig::default().save_to(&path).unwrap();
    assert!(path.exists());
}

#[test]
fn config_serializes_all_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    let cfg = AppConfig::default();
    cfg.save_to(&path).unwrap();

    let contents = fs::read_to_string(&path).unwrap();
    assert!(contents.contains("display_name"));
    assert!(contents.contains("port"));
    assert!(contents.contains("max_bitrate_kbps"));
    assert!(contents.contains("framerate"));
    assert!(contents.contains("force_sw_encode"));
    assert!(contents.contains("airplay_enabled"));
    assert!(contents.contains("miracast_enabled"));
}

#[test]
fn error_messages_include_context() {
    let e = OpenPlayError::Config("bad field".to_string());
    assert!(e.to_string().contains("bad field"));

    let e = OpenPlayError::Pipeline("element not found".to_string());
    assert!(e.to_string().contains("element not found"));

    let e = OpenPlayError::Timeout("no response".to_string());
    assert!(e.to_string().contains("no response"));
}

#[test]
fn error_from_io() {
    let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
    let e: OpenPlayError = io_err.into();
    assert!(e.to_string().contains("denied"));
}

#[test]
fn default_config_is_valid() {
    assert!(AppConfig::default().validate().is_ok());
}

#[test]
fn validate_rejects_empty_display_name() {
    let cfg = AppConfig {
        display_name: "   ".to_string(),
        ..AppConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(err.to_string().contains("display_name"));
}

#[test]
fn validate_rejects_zero_port() {
    let cfg = AppConfig {
        port: 0,
        ..AppConfig::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn validate_rejects_out_of_range_bitrate() {
    let mut cfg = AppConfig {
        max_bitrate_kbps: 0,
        ..AppConfig::default()
    };
    assert!(cfg.validate().is_err());

    cfg.max_bitrate_kbps = openplay_common::MAX_BITRATE_KBPS + 1;
    assert!(cfg.validate().is_err());
}

#[test]
fn validate_rejects_out_of_range_framerate() {
    let mut cfg = AppConfig {
        framerate: 0,
        ..AppConfig::default()
    };
    assert!(cfg.validate().is_err());

    cfg.framerate = openplay_common::MAX_FRAMERATE + 1;
    assert!(cfg.validate().is_err());
}

#[test]
fn validate_accepts_boundary_values() {
    let mut cfg = AppConfig {
        max_bitrate_kbps: openplay_common::MIN_BITRATE_KBPS,
        framerate: openplay_common::MIN_FRAMERATE,
        port: 1,
        ..AppConfig::default()
    };
    assert!(cfg.validate().is_ok());

    cfg.max_bitrate_kbps = openplay_common::MAX_BITRATE_KBPS;
    cfg.framerate = openplay_common::MAX_FRAMERATE;
    cfg.port = u16::MAX;
    assert!(cfg.validate().is_ok());
}

#[test]
fn load_validated_from_rejects_bad_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        b"display_name = \"X\"\nport = 7290\nmax_bitrate_kbps = 0\nframerate = 30\n\
          force_sw_encode = false\nairplay_enabled = true\nmiracast_enabled = true\n",
    )
    .unwrap();

    assert!(AppConfig::load_validated_from(&path).is_err());
}

#[test]
fn load_validated_from_accepts_good_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    AppConfig::default().save_to(&path).unwrap();

    let cfg = AppConfig::load_validated_from(&path).unwrap();
    assert!(cfg.validate().is_ok());
}

#[test]
fn constants_have_expected_values() {
    assert_eq!(DEFAULT_PORT, 7290);
    assert_eq!(openplay_common::PROTOCOL_VERSION, 1);
    assert!(openplay_common::MDNS_SERVICE_TYPE.contains("openplay"));
    assert!(openplay_common::AIRPLAY_MDNS_SERVICE_TYPE.contains("airplay"));
}

#[test]
fn load_or_create_at_writes_the_file_on_first_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("config.toml");
    assert!(!path.exists());

    let cfg = AppConfig::load_or_create_at(&path).unwrap();

    assert!(
        path.exists(),
        "first run should materialise the config file"
    );
    assert_eq!(cfg.port, DEFAULT_PORT);

    // What was written must be what we returned, and must round-trip.
    let reloaded = AppConfig::load_from(&path).unwrap();
    assert_eq!(reloaded.port, cfg.port);
    assert_eq!(reloaded.display_name, cfg.display_name);
    assert_eq!(reloaded.max_bitrate_kbps, cfg.max_bitrate_kbps);
}

#[test]
fn load_or_create_at_does_not_overwrite_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "port = 12345\n").unwrap();

    let cfg = AppConfig::load_or_create_at(&path).unwrap();

    assert_eq!(cfg.port, 12345);
    assert_eq!(fs::read_to_string(&path).unwrap(), "port = 12345\n");
}

#[test]
fn load_or_create_at_rejects_an_out_of_range_value() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "max_bitrate_kbps = 0\n").unwrap();

    let err = AppConfig::load_or_create_at(&path).unwrap_err();
    assert!(
        matches!(err, OpenPlayError::Config(_)),
        "expected a config error, got {err:?}"
    );
}
