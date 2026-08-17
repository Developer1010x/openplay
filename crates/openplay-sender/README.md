# openplay-sender

The sender binary. Discovers receivers on the local network and casts the screen to one of them using AirPlay, Miracast, or the native OpenPlay WebRTC protocol.

## Running

```bash
cargo run -p openplay-sender
# or after build:
./target/release/openplay-sender

# Options:
openplay-sender --name "My Laptop"   # override advertised display name
openplay-sender --config /path/to/config.toml
```

## Architecture

The binary is structured in four modules (`main.rs`, `app.rs`, `casting.rs`,
`receiver_list.rs`):

**`main.rs`** — parses CLI arguments, initializes GStreamer and logging, loads `AppConfig`, and launches the egui window.

**`app.rs`** (`SenderApp`) — the egui application. Runs three mDNS browsers (OpenPlay, AirPlay, Miracast) and maintains the receiver list. Handles UI state: bitrate/framerate sliders and the Start/Stop Casting button. There is no protocol selector — the protocol comes from each receiver's discovery record. Bridges the egui main thread to the tokio runtime for async casting operations.

**`casting.rs`** — the three async casting entry points:
- `start_airplay_cast()` — starts `CaptureSession`, builds `AirPlaySenderPipeline`, runs `AirPlaySession` handshake, then feeds NAL units frame-by-frame.
- `start_miracast_cast()` — starts `CaptureSession`, runs RTSP WFD negotiation via `MiracastSession`, then builds `MiracastSenderPipeline` and streams RTP/UDP.
- `start_miracast_p2p_cast()` (Linux only) — same as above but forms a Wi-Fi Direct group first.

`CastStopHandle` — an `Arc<AtomicBool>` wrapper that the UI uses to signal the casting loop to stop.

**`receiver_list.rs`** — a thin data structure that merges events from all three browsers into a single displayable list.

## Configuration

All runtime settings come from `AppConfig` (see `openplay-common`). The most relevant fields for the sender are `max_bitrate_kbps`, `framerate`, `airplay_enabled`, and `miracast_enabled`.
