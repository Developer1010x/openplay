# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
# Build everything
cargo build

# Build release binaries
cargo build --release

# Run the sender
cargo run -p openplay-sender

# Run the receiver
cargo run -p openplay-receiver

# Run all tests
cargo test

# Run tests for a specific crate
cargo test -p openplay-protocol
cargo test -p openplay-airplay

# Run a single test by name
cargo test -p openplay-protocol test_serialize_session_request

# Lint
cargo clippy --all-targets --all-features

# Format
cargo fmt --all

# Check without producing binaries
cargo check --all-targets
```

GStreamer must be installed before building. On Linux, PipeWire and XDG Desktop Portal are also required. See README.md for platform-specific install commands.

## Architecture

OpenPlay is a Rust workspace. The two application binaries are `openplay-sender` and `openplay-receiver`. All other crates are libraries used by one or both.

### Protocol flow — OpenPlay (WebRTC)

1. **openplay-receiver** starts `ReceiverAdvertiser` (discovery) to publish itself via mDNS and `SignalingServer` (signaling) to listen on a WebSocket port.
2. **openplay-sender** runs `ReceiverBrowser` (discovery) to find receivers on the LAN, then connects via `SignalingClient` (signaling).
3. Signaling exchanges: session negotiation → pairing/auth (ECDH + HMAC proof using keys from `CertificateManager` in crypto) → SDP offer/answer → ICE candidates.
4. Once ICE connects, the sender builds a `SenderPipeline` (pipeline): screen capture via `CaptureSession` (capture) → H.264 encode → GStreamer WebRTC → receiver.
5. The receiver builds a `ReceiverPipeline` (pipeline): GStreamer WebRTC → decode → display in the egui window.

### Protocol flow — AirPlay

1. Sender discovers AirPlay receivers via `AirPlayBrowser` (discovery).
2. User selects a receiver; `start_airplay_cast` in `casting.rs` is called.
3. `AirPlaySession` (airplay/session.rs) performs multi-phase handshake over HTTP: feature negotiation → HAP pairing (airplay/hap_pairing.rs, Ed25519/X25519) → FairPlay (airplay/fairplay.rs, AES-CTR) → mirror stream setup.
4. `AirPlaySenderPipeline` (pipeline) captures screen → encodes H.264 → emits NAL units to an appsink.
5. The casting loop reads NAL units, strips SPS/PPS for codec data, and sends frames via `AirPlaySession::send_video_frame` over the mirror stream (airplay/mirror_stream.rs).

### Protocol flow — Miracast

1. Sender discovers Miracast receivers via `MiracastBrowser` (discovery). On Linux, Wi-Fi Direct peers are found via `wifi_direct.rs` through wpa_supplicant D-Bus.
2. `MiracastSession` (miracast/session.rs) runs an RTSP server (miracast/rtsp_server.rs) and performs Wi-Fi Display (WFD) parameter negotiation (miracast/wfd_params.rs).
3. Once negotiated, `MiracastSenderPipeline` (pipeline) captures screen → H.264 → RTP → UDP to the receiver's IP/port.

### Key design patterns

**Session events via channels**: `AirPlaySession` and `MiracastSession` both communicate status back to the casting loop through a `tokio::sync::mpsc` channel of `SessionEvent` enums (`Ready`, `Ended`). The casting code awaits the first event before starting the pipeline.

**Encoder probing**: `probe_best_encoder()` in pipeline/encoder.rs queries the GStreamer registry at runtime. Platform priority is VA-API → NVENC on Linux, VideoToolbox on macOS, Media Foundation → NVENC on Windows, with x264 as universal fallback. Never hardcode an encoder type.

**Capture abstraction**: `CaptureSession` in the capture crate wraps platform-specific capture. On Linux it uses ashpd to request a PipeWire screencast via XDG Desktop Portal, and exposes a file descriptor + PipeWire node ID to GStreamer's `pipewiresrc`. On other platforms the struct is a stub. The `CaptureConfig` (pipeline/capture_config.rs) carries the node ID, fd, resolution, and framerate into the pipeline constructors.

**Workspace dependencies**: All internal crate references use `{ workspace = true }`. Path mappings are defined once in the root `Cargo.toml` under `[workspace.dependencies]`. When moving or adding a crate, update only the root `Cargo.toml`.

**Platform gating**: Miracast Wi-Fi Direct (`wifi_direct.rs`, zbus D-Bus) and PipeWire capture are gated with `#[cfg(target_os = "linux")]`. Windows and macOS capture backends are not yet implemented — the capture crate compiles stubs on those platforms.

### Crate responsibilities at a glance

| Crate | What it owns |
|---|---|
| openplay-sender | Binary: UI (egui), receiver list, calls into casting.rs |
| openplay-receiver | Binary: UI (egui), hosts signaling server and display window |
| openplay-airplay | AirPlay protocol: HAP pairing, FairPlay, NTP, mirror stream, TLV8 |
| openplay-miracast | Miracast protocol: RTSP, WFD params, Wi-Fi Direct (Linux) |
| openplay-pipeline | GStreamer pipeline construction for all three protocols + encoder probing |
| openplay-signaling | WebSocket signaling client (sender side) and server (receiver side) |
| openplay-discovery | mDNS advertisement and browsing for OpenPlay, AirPlay, and Miracast |
| openplay-protocol | `SignalingMessage` enum and state machine — the wire format for OpenPlay signaling |
| openplay-crypto | Self-signed ECDSA P-256 cert lifecycle (generate, persist, load, fingerprint) |
| openplay-capture | Screen capture abstraction; PipeWire/XDG Portal on Linux |
| openplay-common | `AppConfig` (TOML), XDG paths, logging init, shared constants |
