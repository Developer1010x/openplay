/// Platform-agnostic capture configuration passed to GStreamer pipeline builders.
///
/// On Linux, screen capture goes through the XDG Desktop Portal → PipeWire,
/// so the pipeline source element (`pipewiresrc`) needs the PipeWire fd and node ID.
///
/// On Windows and macOS, GStreamer's built-in capture elements
/// (`d3d11screencapturesrc` / `avfvideosrc`) handle capture directly — no fd needed.
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Video width in pixels.
    pub width: u32,
    /// Video height in pixels.
    pub height: u32,
    /// Target framerate.
    pub framerate: u32,
    /// PipeWire file descriptor (Linux only — from XDG Desktop Portal).
    #[cfg(target_os = "linux")]
    pub pw_fd: i32,
    /// PipeWire node ID (Linux only — identifies the capture source stream).
    #[cfg(target_os = "linux")]
    pub node_id: u32,
}

impl CaptureConfig {
    /// Creates a CaptureConfig for Linux (with PipeWire fd and node_id).
    #[cfg(target_os = "linux")]
    pub fn new(pw_fd: i32, node_id: u32, width: u32, height: u32, framerate: u32) -> Self {
        Self {
            width,
            height,
            framerate,
            pw_fd,
            node_id,
        }
    }

    /// Creates a CaptureConfig for non-Linux platforms (Windows/macOS).
    /// Width/height are used to configure pipeline caps; the actual GStreamer source
    /// element will use the real screen resolution, potentially overriding these.
    #[cfg(not(target_os = "linux"))]
    pub fn new(width: u32, height: u32, framerate: u32) -> Self {
        Self {
            width,
            height,
            framerate,
        }
    }
}
