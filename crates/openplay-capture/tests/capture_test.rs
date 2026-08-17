use openplay_capture::{detect_session_type, CaptureError};

#[test]
fn detect_session_type_returns_without_panic() {
    let _ = detect_session_type();
}

#[test]
fn capture_error_portal_display() {
    let e = CaptureError::Portal("D-Bus timeout".to_string());
    assert!(e.to_string().contains("D-Bus timeout"));
}

#[test]
fn capture_error_cancelled_display() {
    let e = CaptureError::Cancelled;
    assert!(e.to_string().contains("cancelled"));
}

#[test]
fn capture_error_no_screens_display() {
    let e = CaptureError::NoScreens;
    assert!(e.to_string().contains("No screens"));
}

#[test]
fn capture_error_unsupported_session_display() {
    let e = CaptureError::UnsupportedSession("mir".to_string());
    assert!(e.to_string().contains("mir"));
}

#[test]
fn capture_error_platform_display() {
    let e = CaptureError::Platform("access denied".to_string());
    assert!(e.to_string().contains("access denied"));
}

#[cfg(target_os = "linux")]
#[test]
fn session_type_reflects_xdg_env() {
    use openplay_capture::SessionType;
    std::env::set_var("XDG_SESSION_TYPE", "wayland");
    assert_eq!(detect_session_type(), SessionType::Wayland);

    std::env::set_var("XDG_SESSION_TYPE", "x11");
    assert_eq!(detect_session_type(), SessionType::X11);

    std::env::remove_var("XDG_SESSION_TYPE");
    assert_eq!(detect_session_type(), SessionType::Unknown);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn non_linux_session_type_is_native() {
    use openplay_capture::SessionType;
    assert_eq!(detect_session_type(), SessionType::Native);
}
