//! Screen capture abstraction.
//!
//! On Linux, [`CaptureSession`] requests a PipeWire
//! screencast through the XDG Desktop Portal using `ashpd`, and exposes a file
//! descriptor and node ID for GStreamer's `pipewiresrc` to consume. The portal
//! raises the desktop's own permission dialog, so the first capture always
//! requires user consent; the choice is then remembered across runs (see
//! [`forget_screen_choice`] for how to get the dialog back).
//!
//! **On macOS and Windows this crate compiles as a stub.** There is no capture
//! backend on those platforms, so the applications build and start but cannot
//! capture a screen.

/// Platform-specific capture implementations.
#[cfg(target_os = "linux")]
mod portal;

#[cfg(target_os = "linux")]
mod restore_token;

#[cfg(not(target_os = "linux"))]
mod desktop;

// Re-export the platform-appropriate CaptureSession and helpers.
#[cfg(target_os = "linux")]
pub use portal::{CaptureSession, CaptureSource, SessionType};

#[cfg(not(target_os = "linux"))]
pub use desktop::{CaptureSession, CaptureSource, SessionType};

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

    #[error("Platform capture error: {0}")]
    Platform(String),
}

/// Forgets the screen the portal was told to remember, so the next capture
/// shows the desktop's screen-selection dialog again.
///
/// Returns `Ok(true)` when a remembered choice existed and was removed, and
/// `Ok(false)` when there was nothing to forget — which is always the case on
/// platforms that never remember one.
pub fn forget_screen_choice() -> std::io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        restore_token::clear()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(false)
    }
}

/// Detects the current display session type (Linux only; returns Unknown on other platforms).
pub fn detect_session_type() -> SessionType {
    #[cfg(target_os = "linux")]
    {
        match std::env::var("XDG_SESSION_TYPE").as_deref() {
            Ok("wayland") => SessionType::Wayland,
            Ok("x11") => SessionType::X11,
            _ => SessionType::Unknown,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        SessionType::Native
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_session_type_returns_value() {
        let _session_type = detect_session_type();
    }
}
