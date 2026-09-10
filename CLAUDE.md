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

**The `nice` GStreamer plugin is a hard requirement for anything WebRTC**, and it ships separately from `gst-plugins-bad` everywhere (`gstreamer1.0-nice` on Debian/Ubuntu, `libnice-gstreamer1` on Fedora, part of `libnice` on Arch). Without it `webrtcbin` constructs successfully and then refuses every `sink_%u` pad request, so a cast fails with no useful error. `crates/openplay-pipeline/tests/webrtc_loopback.rs` asserts the plugin is present up front rather than failing obscurely, which means `cargo test --all` fails on a machine that lacks it. Check with `gst-inspect-1.0 nice`.

CI runs `fmt --check`, then `clippy -D warnings`, then `cargo test --all`, then a release build, plus a `cross-platform-check` on macOS/Windows and an `MSRV` job pinned to 1.88.0. Formatting gates linting — a fmt failure hides every clippy finding behind it.

## Project status

Do not assume a feature works because a type for it exists. Equally, do not assume it is unwired because this file once said so. As of this writing:

- **OpenPlay (WebRTC) is wired to both binaries.** `crates/openplay-receiver/src/net.rs` starts `ReceiverAdvertiser` and a TLS `SignalingServer`, gates every session behind a consent prompt, then builds a `ReceiverPipeline` and answers the SDP offer. `start_openplay_cast` in `crates/openplay-sender/src/casting.rs` is called from the `Protocol::OpenPlay` arm of `crates/openplay-sender/src/app.rs`. `crates/openplay-pipeline/src/webrtc.rs` drives offer/answer and trickle ICE, turning `webrtcbin` signals into a `WebRtcEvent` channel. **It has never been reported working between two separate machines** — the coverage is in-process: `crates/openplay-signaling/tests/loopback.rs` (3 tests over real loopback TLS) and `crates/openplay-pipeline/tests/webrtc_loopback.rs` (two `webrtcbin`s negotiating and carrying decoded frames, driven by a `videotestsrc` stand-in for `SenderPipeline`, since the real one needs a desktop portal and a human click).
- **The receiver's security model is the consent prompt, and nothing else.** No sender is accepted until a human presses **Allow** in `window.rs`'s consent page. There is no pairing and no authentication behind it: the mDNS TXT fingerprint is unauthenticated, so anyone on the LAN can advertise their own, and the `PairingChallenge`/`Auth*` messages in `openplay-protocol` have no senders or handlers. Do not weaken the prompt into a timeout that accepts, and do not describe pinning as authentication. A prompt is exclusive: while one is on screen, a second sender is refused with `RejectReason::Busy` rather than replacing it, because overwriting the displayed name and connection id would let a sender that keeps asking inherit an Allow meant for someone else. The name on the prompt is the sender's configured `display_name`, sanitised (control characters stripped, truncated to `MAX_NAME_CHARS`, blank falling back to "OpenPlay Sender") — it is the whole basis on which a person decides, so treat it as a security-relevant value rather than a cosmetic one.
- **Miracast sending is wired end to end but has never been verified against a real sink.** Do not call it working. The session now holds the RTSP control connection open for the life of the cast (`rtsp_server::serve_control_channel`, keepalive pings, `Ended` only on shutdown or when the sink leaves) — an earlier version sent `Ended` on the line after `Ready` and dropped the socket right after M7, which ended every cast in milliseconds. Note the shape of that bug when touching this code: **returning from `run_session` by any route ends the cast**, because dropping `conn` sends the sink a FIN and dropping `evt_tx` reads as `None` in the caller's select.
- **AirPlay sending** is partly working. HAP pairing used a fabricated SRP group and could never succeed; that is fixed and now uses the real RFC 5054 3072-bit group. Whether the handshake works end to end is still unconfirmed against hardware (issue #27). FairPlay **will not be implemented** — `fp_setup` has no callers and must not acquire any, and Apple TV 2nd/3rd generation are refused by model string by design. See the decision in `docs/crypto.md`.
- **Screen capture is only exercised on Linux.** On macOS and Windows capture is left to GStreamer's own elements and is untested. Note that `query_primary_display_size()` in `openplay-capture/src/desktop.rs` has a real implementation only for Windows: the macOS branch is an empty block with a comment, so macOS gets the hardcoded `(1920, 1080)` fallback. The Windows build was broken outright until #25 made `openplay-capture` declare the `windows` crate it uses.
- **`CertificateManager` and the `tls.rs` builders now have callers.** The receiver calls `CertificateManager::load_or_generate` and serves `server_config()`; the sender pins with `client_config_pinned`. A certificate *is* generated on the receiver's first launch, in the data directory.
- **Crate dependencies changed with the wiring.** `openplay-receiver` now depends on `-crypto`, `-discovery`, `-pipeline`, `-protocol` and `-signaling` as well as `-common`; `openplay-sender` additionally on `-signaling`, `-protocol`, `-crypto` and `url`. Manifests are still expected to describe what is actually used — if you drop a call path, drop the declaration.

## Architecture

OpenPlay is a Rust workspace. The two application binaries are `openplay-sender` and `openplay-receiver`. All other crates are libraries used by one or both.

### Protocol flow — OpenPlay (WebRTC)

Implemented in `receiver/src/net.rs` and `sender/src/casting.rs`. Untested between two machines.

1. **openplay-receiver** (`net::start`) generates or loads its certificate, registers `ReceiverAdvertiser` over mDNS with the SHA-256 fingerprint in the `fp` TXT key, then binds `SignalingServer` on `[::]:port` (falling back to `0.0.0.0` if the dual-stack bind fails) with the matching `ServerConfig`. Advertisement failing is degraded, not fatal — the server still accepts a sender given the address by hand.
2. **openplay-sender** runs `ReceiverBrowser`, and the `Protocol::OpenPlay` arm requires **both** an address and a fingerprint before connecting; missing either is a hard failure, never a fall-back to unpinned TLS. It pins with `client_config_pinned` and connects `SignalingClient` to `wss://addr`.
3. `SessionRequest` → the receiver checks protocol version, H.264 support and whether it is already busy, then raises `Status::PendingConsent` and **waits for a human**. `SessionAccept` is sent only from `NetHandle::decide`. The sender waits without a timeout, polling its stop flag every 250 ms so Stop still works during the silence.
4. The sender builds `SenderPipeline` (only after acceptance — no point touching the GPU sooner) and plays it, which makes `webrtcbin` raise `on-negotiation-needed`. `WebRtcPeer` (`pipeline/webrtc.rs`, `Role::Offerer`) produces the offer; SDP and trickle ICE cross in both directions over signaling.
5. The receiver builds `ReceiverPipeline` on the offer, starts it **before** answering (an unstarted `webrtcbin` has no clock and gathers no candidates), answers as `Role::Answerer`, and paints decoded RGBA frames in the egui window.
6. Teardown: `SessionEnd` from the owning connection, `WebRtcEvent::Disconnected`, or a 2-second liveness tick that notices the reply channel closed. `SessionEnd` from a non-owning connection is ignored, so a stranger cannot stop someone's cast.

**Pairing and authentication are skipped entirely.** Step 3 in the protocol design has a pairing/auth phase; the code goes straight from session negotiation to SDP.

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

**GStreamer talks back over an unbounded channel**: `WebRtcPeer` in pipeline/webrtc.rs turns `webrtcbin`'s GObject signals into `WebRtcEvent`s on a `tokio::sync::mpsc::UnboundedSender`. Unbounded is not laziness — these fire on GStreamer streaming threads, and `Sender::blocking_send` panics outright when called from inside a runtime thread, while `UnboundedSender::send` is synchronous and needs no reactor. Traffic is a handful of messages per session.

**Exactly one peer offers**: `Role::Offerer` answers `on-negotiation-needed` by building an offer; `Role::Answerer` ignores that signal and produces an answer only once `set_remote_description` has fed it an offer. Both offering, or neither, gives a session that negotiates forever and carries no video.

**No STUN or TURN is configured**, so only host candidates are gathered. That is deliberate — casts are LAN-local and contacting a third-party STUN server would leak that a cast is happening. `WebRtcPeer::set_stun_server` exists for deployments that need it.

**Encoder probing**: `probe_best_encoder()` in pipeline/encoder.rs queries the GStreamer registry at runtime *and* tries to instantiate each candidate, since a factory can be registered while the hardware is absent. Priority is VA-API → NVENC on Linux, VideoToolbox on macOS, Media Foundation → NVENC on Windows, x264 as universal fallback. **Never hardcode an encoder type.** The one bypass is the `force_sw_encode` config flag, handled by `select_encoder()` in sender/src/casting.rs.

**Config validation runs twice**: `AppConfig::load_or_create_at()` writes defaults on first run and validates. Both binaries then call `validate()` **again** after applying CLI overrides, because `--port 0` and `--name ""` bypass the first check.

**Capture abstraction**: `CaptureSession` wraps platform-specific capture. On Linux it uses ashpd to request a PipeWire screencast via XDG Desktop Portal, exposing a file descriptor + node ID to `pipewiresrc`. On macOS and Windows it reports a display size and leaves capture to GStreamer's own elements (untested) — and only the Windows size is real (`GetSystemMetrics`); the macOS branch of `query_primary_display_size()` is empty, so macOS falls through to a hardcoded 1920x1080. `CaptureConfig` carries node ID, fd, resolution and framerate into the pipeline constructors.

**Workspace dependencies**: All internal crate references use `{ workspace = true }`. Path mappings are defined once in the root `Cargo.toml`. When moving or adding a crate, update only the root `Cargo.toml`.

**Platform gating**: Miracast Wi-Fi Direct (`wifi_direct.rs`, and the Wi-Fi Direct paths in `session.rs`) and PipeWire capture are gated with `#[cfg(target_os = "linux")]`. Gating the module is not enough — gate every import it needs and every caller, or the fix trades a hard error for unused-import warnings, which are errors under `-D warnings`. A `cross-platform-check` CI job on macOS and Windows, and an `MSRV` job on 1.88.0, both cover `--workspace` minus `openplay-pipeline`, `-sender` and `-receiver` — so a new crate is covered automatically and only a GStreamer/PipeWire-dependent one belongs in the exclusions. Do not substitute `cargo check --target` from Linux for the cross-platform job on crates with C dependencies (`ring`, `rusqlite`) — those fail for unrelated reasons.

**The MSRV is 1.88, not the 1.80 in `Cargo.toml`.** That field is wrong by eight minor versions: 1.80 fails at dependency *resolution* (`zvariant_utils` needs the `edition2024` cargo feature, `time` and `zbus` 5.x need 1.87–1.88). CI's `MSRV` job pin is the number actually enforced; if you correct the manifest, move the pin in the same change.

**Crypto constants must be verifiable**: `srp.rs` pins the SRP group's properties (bit length, primality, safe-primality, generator) *separately* from the protocol round-trip, and checks it against a rearrangement of RFC 3526's formula in a test (which pins the top and bottom of the value, not the middle — the primality tests cover that). A round-trip test alone passes happily with a wrong shared constant — that is how the original fabricated group survived. Read `docs/crypto.md` before touching `openplay-airplay`.

### Crate responsibilities at a glance

| Crate | What it owns |
|---|---|
| openplay-sender | Binary: UI (egui), receiver list, calls into casting.rs |
| openplay-receiver | Binary: egui window (waiting page, consent prompt, video), plus `net.rs` — mDNS advertisement, TLS signaling server and the session loop |
| openplay-airplay | AirPlay protocol: HAP pairing, SRP-6a, NTP, mirror stream, TLV8, plus uncalled `fairplay.rs` |
| openplay-miracast | Miracast protocol: RTSP, WFD params, Wi-Fi Direct (Linux) |
| openplay-pipeline | GStreamer pipeline construction for all three protocols, encoder probing, and `webrtc.rs` — SDP offer/answer and trickle ICE over `webrtcbin` |
| openplay-signaling | TLS WebSocket signaling client (sender side) and server (receiver side) |
| openplay-discovery | mDNS advertisement and browsing for OpenPlay, AirPlay, and Miracast |
| openplay-protocol | `SignalingMessage` enum and state machines — the wire format for OpenPlay signaling |
| openplay-crypto | Self-signed ECDSA P-256 cert lifecycle (generate, persist, load, fingerprint) plus `tls.rs`, the rustls configs: a server config for the receiver and a fingerprint-pinning client config for the sender |
| openplay-capture | Screen capture abstraction; PipeWire/XDG Portal on Linux |
| openplay-common | `AppConfig` (TOML), XDG paths, logging init, shared constants |

Non-crate directories: `data/` (desktop entry, AppStream metainfo, icon, and the D-Bus and polkit files Wi-Fi Direct needs), `flatpak/` (manifest), `packaging/` (`build-deb.sh`, which
CI runs in the release job), `docs/`, `.github/workflows/`.
