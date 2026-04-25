# openplay-capture

Platform abstraction for screen capture. Exposes a single `CaptureSession` type regardless of operating system; the underlying mechanism differs per platform.

## Platform implementations

**Linux** (`portal.rs`) — uses the XDG Desktop Portal via `ashpd`. Opens a `org.freedesktop.portal.ScreenCast` session, prompts the user to select a screen or window, and returns a PipeWire file descriptor and node ID. GStreamer's `pipewiresrc` element consumes these directly.

**Windows / macOS** (`desktop.rs`) — stub implementation. On Windows, `GetSystemMetrics` is used to query the primary display resolution. On macOS, a CoreGraphics query is planned but not yet implemented. GStreamer's `d3d11screencapturesrc` (Windows) and `avfvideosrc` (macOS) handle capture natively within the pipeline.

## Public API

```rust
// Start a capture session (shows platform UI on Linux)
let session = CaptureSession::start().await?;

// Linux: access the PipeWire fd and node id
let fd = session.pipewire_fd();
let source = session.primary_source(); // Option<CaptureSource>

// All platforms: query resolution
let (w, h) = (session.width(), session.height());
```

**`detect_session_type()`** — returns `SessionType::Wayland`, `X11`, or `Unknown` based on `XDG_SESSION_TYPE` on Linux; `SessionType::Native` on other platforms.

**`CaptureError`** — `Portal`, `Cancelled`, `NoScreens`, `UnsupportedSession`, `Platform`.

## Tests

```bash
cargo test -p openplay-capture
```
