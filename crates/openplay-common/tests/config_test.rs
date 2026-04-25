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

    let mut original = AppConfig::default();
    original.display_name = "TestDevice".to_string();
    original.port = 9999;
    original.max_bitrate_kbps = 8000;
    original.framerate = 60;
    original.force_sw_encode = true;
    original.airplay_enabled = false;
    original.miracast_enabled = false;

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
fn constants_have_expected_values() {
    assert_eq!(DEFAULT_PORT, 7290);
    assert_eq!(openplay_common::PROTOCOL_VERSION, 1);
    assert!(openplay_common::MDNS_SERVICE_TYPE.contains("openplay"));
    assert!(openplay_common::AIRPLAY_MDNS_SERVICE_TYPE.contains("airplay"));
}
