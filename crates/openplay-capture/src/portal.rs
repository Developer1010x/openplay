// This module is Linux-only — screen capture via XDG Desktop Portal + PipeWire.
#![cfg(target_os = "linux")]

use crate::CaptureError;
use tracing::{debug, info};

/// The type of display session detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionType {
    Wayland,
    X11,
    Unknown,
}

/// Information about a captured source from the portal.
#[derive(Debug, Clone)]
pub struct CaptureSource {
    /// PipeWire node ID for this source.
    pub node_id: u32,
    /// Width in pixels (if known).
    pub width: Option<u32>,
    /// Height in pixels (if known).
    pub height: Option<u32>,
}

/// Manages a portal-based screen capture session.
///
/// Wraps the ashpd `Screencast` portal to provide PipeWire fd and node IDs
/// for feeding into a GStreamer `pipewiresrc` element.
pub struct CaptureSession {
    /// The PipeWire fd (as raw fd) for GStreamer's pipewiresrc.
    pw_fd: std::os::unix::io::RawFd,
    /// Captured sources (screens/windows).
    sources: Vec<CaptureSource>,
}

impl CaptureSession {
    /// Starts a new screen capture session via the XDG Desktop Portal.
    ///
    /// This will trigger the compositor's screen selection dialog.
    /// The user must grant permission for screen capture.
    ///
    /// Returns a `CaptureSession` with PipeWire fd and source info.
    pub async fn start() -> Result<Self, CaptureError> {
        info!("Starting portal screen capture session");

        let proxy = ashpd::desktop::screencast::Screencast::new()
            .await
            .map_err(|e| CaptureError::Portal(format!("Failed to create screencast proxy: {e}")))?;

        let session = proxy
            .create_session()
            .await
            .map_err(|e| CaptureError::Portal(format!("Failed to create session: {e}")))?;

        proxy
            .select_sources(
                &session,
                ashpd::desktop::screencast::CursorMode::Embedded,
                ashpd::desktop::screencast::SourceType::Monitor.into(),
                false,
                None,
                ashpd::desktop::PersistMode::DoNot,
            )
            .await
            .map_err(|e| CaptureError::Portal(format!("Failed to select sources: {e}")))?;

        let response = proxy
            .start(&session, None)
            .await
            .map_err(|e| {
                if e.to_string().contains("Cancelled") {
                    CaptureError::Cancelled
                } else {
                    CaptureError::Portal(format!("Failed to start session: {e}"))
                }
            })?
            .response()
            .map_err(|e| CaptureError::Portal(format!("Failed to get start response: {e}")))?;

        let streams = response.streams();
        if streams.is_empty() {
            return Err(CaptureError::NoScreens);
        }

        let sources: Vec<CaptureSource> = streams
            .iter()
            .map(|stream: &ashpd::desktop::screencast::Stream| {
                let (width, height) = stream
                    .size()
                    .map(|s| (Some(s.0 as u32), Some(s.1 as u32)))
                    .unwrap_or((None, None));
                CaptureSource {
                    node_id: stream.pipe_wire_node_id(),
                    width,
                    height,
                }
            })
            .collect();

        debug!(sources = ?sources, "Portal returned capture sources");

        let pw_fd = proxy
            .open_pipe_wire_remote(&session)
            .await
            .map_err(|e| CaptureError::Portal(format!("Failed to open PipeWire remote: {e}")))?;

        use std::os::unix::io::IntoRawFd;
        let raw_fd = pw_fd.into_raw_fd();

        info!(
            num_sources = sources.len(),
            node_id = sources[0].node_id,
            "Screen capture session started"
        );

        Ok(Self {
            pw_fd: raw_fd,
            sources,
        })
    }

    /// Returns the PipeWire fd for use with GStreamer's `pipewiresrc`.
    pub fn pipewire_fd(&self) -> std::os::unix::io::RawFd {
        self.pw_fd
    }

    /// Returns the first (primary) capture source.
    pub fn primary_source(&self) -> Option<&CaptureSource> {
        self.sources.first()
    }

    /// Returns all capture sources.
    pub fn sources(&self) -> &[CaptureSource] {
        &self.sources
    }
}
