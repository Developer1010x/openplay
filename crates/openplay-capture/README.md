# openplay-capture

Platform abstraction for screen capture. Exposes a single `CaptureSession` type regardless of operating system; the underlying mechanism differs per platform.

## Platform implementations

**Linux** (`portal.rs`) — uses the XDG Desktop Portal via `ashpd`. Opens a `org.freedesktop.portal.ScreenCast` session, prompts the user to select a screen or window, and returns a PipeWire file descriptor and node ID. GStreamer's `pipewiresrc` element consumes these directly.

**Windows / macOS** (`desktop.rs`) — reports the primary display size only; capture itself is left to GStreamer's `d3d11screencapturesrc` (Windows) or `screencapturesrc`/`avfvideosrc` (macOS) inside the pipeline. On Windows the size comes from `GetSystemMetrics`; on macOS a CoreGraphics query is planned, so the 1920x1080 fallback is used. **Neither path has been exercised** — and the Windows build did not compile at all until #25 made this crate declare the `windows` crate it uses.

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

**`detect_session_type()`** — returns `SessionType::Wayland`, `X11`, or `Unknown` based on `XDG_SESSION_TYPE` on Linux; `SessionType::Native` on other platforms. Only called from tests; nothing uses it at runtime.

**`CaptureError`** — `Portal`, `Cancelled`, `NoScreens`, `UnsupportedSession`, `Platform`.

## Tests

```bash
cargo test -p openplay-capture
```
