mod portal;

pub use portal::{CaptureSession, CaptureSource, SessionType};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum CaptureError {
    #[error("Portal error: {0}")]
    Portal(String),

    #[error("User cancelled the screen selection dialog")]
    Cancelled,

    #[error("No screens available for capture")]
    NoScreens,

    #[error("Session type not supported: {0}")]
    UnsupportedSession(String),
}

/// Detects the current display session type.
pub fn detect_session_type() -> SessionType {
    match std::env::var("XDG_SESSION_TYPE").as_deref() {
        Ok("wayland") => SessionType::Wayland,
        Ok("x11") => SessionType::X11,
        _ => SessionType::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_session_type_returns_value() {
        // Just verify it doesn't panic — actual result depends on environment
        let _session_type = detect_session_type();
    }
}
