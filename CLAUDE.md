# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Longer-form documentation lives in `docs/`. Start with `docs/README.md`.
`docs/architecture.md` and `docs/contributing.md` cover most of what follows in
more detail; this file is the quick reference.

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
cargo clippy --all-targets --all-features -- -D warnings

# Format
cargo fmt --all

# Check without producing binaries
cargo check --all-targets

# Probe a real AirPlay receiver (hardware check, see docs/crypto.md)
cargo run -p openplay-airplay --example pair_probe -- 192.168.1.11:7000
```

GStreamer must be installed before building. On Linux, PipeWire and XDG Desktop Portal are also required. See `docs/install.md` for platform-specific install commands.

CI runs `fmt --check`, then `clippy -D warnings`, then `cargo test --all`, then a release build. Formatting gates linting — a fmt failure hides every clippy finding behind it.

## Project status

Do not assume a feature works because a type for it exists. As of this writing:

- **Miracast sending** works, including Wi-Fi Direct P2P on Linux.
- **AirPlay sending** is partly working. HAP pairing used a fabricated SRP group and could never succeed; that is fixed and now uses the real RFC 5054 3072-bit group. FairPlay is written but **not wired into the session flow at all** — `fp_setup` has no callers, and Apple TV 2nd/3rd generation are refused by model string instead. See `docs/crypto.md` and issue #8.
- **OpenPlay (WebRTC)** is **not wired to either binary**. `SenderPipeline`, `ReceiverPipeline`, `SignalingServer`, `SignalingClient` and `ReceiverAdvertiser` are implemented and have no callers. The sender's `Protocol::OpenPlay` arm sets a status string and stops; the receiver window is static.
- **Screen capture is only exercised on Linux.** On macOS and Windows `CaptureSession` just reports the display size and capture is left to GStreamer's own elements; that path is untested. The Windows build was broken outright until `openplay-capture` declared the `windows` crate it uses.
- **`CertificateManager`** is never constructed outside its own tests, and `openplay-crypto` no longer depends on `rustls`.
- **Unused dependencies were removed** from every crate. If you wire up a path that needs `openplay-signaling`, `-protocol` or `-crypto`, re-add the declaration.

## Architecture

OpenPlay is a Rust workspace. The two application binaries are `openplay-sender` and `openplay-receiver`. All other crates are libraries used by one or both.

### Protocol flow — OpenPlay (WebRTC) — designed, not connected

1. **openplay-receiver** starts `ReceiverAdvertiser` (discovery) to publish itself via mDNS and `SignalingServer` (signaling) to listen on a WebSocket port.
2. **openplay-sender** runs `ReceiverBrowser` (discovery) to find receivers on the LAN, then connects via `SignalingClient`.
3. Signaling exchanges: session negotiation → pairing/auth → SDP offer/answer → ICE candidates.
4. Once ICE connects, the sender builds a `SenderPipeline`: screen capture via `CaptureSession` → H.264 encode → GStreamer WebRTC → receiver.
5. The receiver builds a `ReceiverPipeline`: GStreamer WebRTC → decode → display in the egui window.

Steps 1–5 describe the design. Neither binary performs any of it yet.

### Protocol flow — AirPlay

1. Sender discovers AirPlay receivers via `AirPlayBrowser` (discovery).
2. User selects a receiver; `start_airplay_cast` in `casting.rs` is called.
3. `AirPlaySession` (airplay/session.rs) spawns `run_session`, which starts an NTP server on port 7010, then tries `http_session::negotiate` (`GET /info` → `POST /stream`). On 501/403 it falls back to `negotiate_with_auth`: HAP **transient** pair-setup + pair-verify (airplay/hap_pairing.rs, SRP-6a math in airplay/srp.rs), then `POST /stream`. **There is no FairPlay phase** — `fairplay.rs` has no callers, and `AppleTV2,*`/`AppleTV3,*` are refused up front instead.
4. `AirPlaySenderPipeline` (pipeline) captures screen → encodes H.264 → emits NAL units to an appsink.
5. The casting loop reads NAL units, copies the SPS/PPS out of the first frame and sends them once as codec data (`send_codec_data`), then forwards every access unit unmodified via `AirPlaySession::send_video_frame` (airplay/mirror_stream.rs).

### Protocol flow — Miracast

1. Sender discovers Miracast receivers via `MiracastBrowser`. On Linux, Wi-Fi Direct peers are found via `wifi_direct.rs` through wpa_supplicant D-Bus.
2. `MiracastSession` (miracast/session.rs) performs Wi-Fi Display negotiation over RTSP M1–M7 (miracast/rtsp_server.rs, miracast/wfd_params.rs). After a P2P group forms, **the source is the RTSP server** — OpenPlay listens on 7236 for the sink.
3. Once negotiated, `MiracastSenderPipeline` captures screen → H.264 → RTP → UDP to the sink.

### Key design patterns

**Session events via channels**: `AirPlaySession` and `MiracastSession` report status back to the casting loop through a `tokio::sync::mpsc` channel of `SessionEvent` enums (`Ready`, `Ended`). The casting code awaits `Ready` before starting the pipeline, because the negotiated resolution and port are not known until the handshake completes.

**Encoder probing**: `probe_best_encoder()` in pipeline/encoder.rs queries the GStreamer registry at runtime *and* tries to instantiate each candidate, since a factory can be registered while the hardware is absent. Priority is VA-API → NVENC on Linux, VideoToolbox on macOS, Media Foundation → NVENC on Windows, x264 as universal fallback. **Never hardcode an encoder type.** The one bypass is the `force_sw_encode` config flag, handled by `select_encoder()` in sender/src/casting.rs.

**Config validation runs twice**: `AppConfig::load_or_create_at()` writes defaults on first run and validates. Both binaries then call `validate()` **again** after applying CLI overrides, because `--port 0` and `--name ""` bypass the first check.

**Capture abstraction**: `CaptureSession` wraps platform-specific capture. On Linux it uses ashpd to request a PipeWire screencast via XDG Desktop Portal, exposing a file descriptor + node ID to `pipewiresrc`. On macOS and Windows it only reports the primary display size and leaves capture to GStreamer's own elements (untested). `CaptureConfig` carries node ID, fd, resolution and framerate into the pipeline constructors.

**Workspace dependencies**: All internal crate references use `{ workspace = true }`. Path mappings are defined once in the root `Cargo.toml`. When moving or adding a crate, update only the root `Cargo.toml`.

**Platform gating**: Miracast Wi-Fi Direct (`wifi_direct.rs`, and the Wi-Fi Direct paths in `session.rs`) and PipeWire capture are gated with `#[cfg(target_os = "linux")]`. Gating the module is not enough — gate every import it needs and every caller, or the fix trades a hard error for unused-import warnings, which are errors under `-D warnings`. A `cross-platform-check` CI job on macOS and Windows covers the eight crates that build without GStreamer or PipeWire. Do not substitute `cargo check --target` from Linux for it on crates with C dependencies (`ring`, `rusqlite`) — those fail for unrelated reasons.

**Crypto constants must be verifiable**: `srp.rs` pins the SRP group's properties (bit length, primality, safe-primality, generator) *separately* from the protocol round-trip, and checks it against a rearrangement of RFC 3526's formula in a test (which pins the top and bottom of the value, not the middle — the primality tests cover that). A round-trip test alone passes happily with a wrong shared constant — that is how the original fabricated group survived. Read `docs/crypto.md` before touching `openplay-airplay`.

### Crate responsibilities at a glance

| Crate | What it owns |
|---|---|
| openplay-sender | Binary: UI (egui), receiver list, calls into casting.rs |
| openplay-receiver | Binary: egui window showing a static "waiting" page. Depends only on openplay-common |
| openplay-airplay | AirPlay protocol: HAP pairing, SRP-6a, NTP, mirror stream, TLV8, plus uncalled `fairplay.rs` |
| openplay-miracast | Miracast protocol: RTSP, WFD params, Wi-Fi Direct (Linux) |
| openplay-pipeline | GStreamer pipeline construction for all three protocols + encoder probing |
| openplay-signaling | WebSocket signaling client (sender side) and server (receiver side) |
| openplay-discovery | mDNS advertisement and browsing for OpenPlay, AirPlay, and Miracast |
| openplay-protocol | `SignalingMessage` enum and state machines — the wire format for OpenPlay signaling |
| openplay-crypto | Self-signed ECDSA P-256 cert lifecycle (generate, persist, load, fingerprint) |
| openplay-capture | Screen capture abstraction; PipeWire/XDG Portal on Linux |
| openplay-common | `AppConfig` (TOML), XDG paths, logging init, shared constants |

Non-crate directories: `data/` (desktop entry, AppStream metainfo, icon, and the D-Bus and polkit files Wi-Fi Direct needs), `flatpak/` (manifest), `docs/`, `.github/workflows/`.
