// Non-Linux capture stub.
//
// On Windows and macOS, screen capture is handled directly by GStreamer capture
// elements (d3d11screencapturesrc on Windows, avfvideosrc/screencapturesrc on macOS).
// This module provides a lightweight CaptureSession that queries the screen resolution
// so the pipeline can be configured correctly before capture starts.

use crate::CaptureError;
use tracing::info;

/// Session type placeholder for non-Linux platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionType {
    /// Native platform capture (no portal needed).
    Native,
}

/// Information about a display to capture.
#[derive(Debug, Clone)]
pub struct CaptureSource {
    /// Display index (0 = primary).
    pub display_index: u32,
    /// Width in pixels.
    pub width: Option<u32>,
    /// Height in pixels.
    pub height: Option<u32>,
}

/// A capture session handle for non-Linux platforms.
///
/// On Windows/macOS, GStreamer's built-in capture elements (d3d11screencapturesrc,
/// avfvideosrc) are used directly — no portal or PipeWire fd is needed. This struct
/// just carries the screen resolution so the pipeline can be configured.
pub struct CaptureSession {
    sources: Vec<CaptureSource>,
}

impl CaptureSession {
    /// Starts a capture session by querying the primary display resolution.
    pub async fn start() -> Result<Self, CaptureError> {
        let (width, height) = query_primary_display_size();
        info!(width, height, "Desktop capture session ready (native GStreamer source)");

        Ok(Self {
            sources: vec![CaptureSource {
                display_index: 0,
                width: Some(width),
                height: Some(height),
            }],
        })
    }

    /// Returns the primary capture source.
    pub fn primary_source(&self) -> Option<&CaptureSource> {
        self.sources.first()
    }

    /// Returns all capture sources.
    pub fn sources(&self) -> &[CaptureSource] {
        &self.sources
    }

    /// Width of the primary display.
    pub fn width(&self) -> u32 {
        self.primary_source()
            .and_then(|s| s.width)
            .unwrap_or(1920)
    }

    /// Height of the primary display.
    pub fn height(&self) -> u32 {
        self.primary_source()
            .and_then(|s| s.height)
            .unwrap_or(1080)
    }
}

/// Queries the primary display resolution using platform APIs.
fn query_primary_display_size() -> (u32, u32) {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
        unsafe {
            let w = GetSystemMetrics(SM_CXSCREEN);
            let h = GetSystemMetrics(SM_CYSCREEN);
            if w > 0 && h > 0 {
                return (w as u32, h as u32);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        // Use CoreGraphics to get display size
        // If not available at compile time, fall through to default
    }

    // Default fallback — GStreamer source will use actual resolution
    (1920, 1080)
}
