# Architecture

OpenPlay is a flat Cargo workspace of eleven crates. Two are binaries; the other
nine are libraries — though three of them (`openplay-signaling`, `-protocol`
and `-crypto`) are not reached from either binary yet.

For what actually works today versus what is designed but unwired, see the
Status section of the [README](../README.md). This document describes the shape
of the code, including the parts that are not yet connected — where that is the
case, it says so.

## Crate graph

Solid arrows are dependencies the code actually uses today.

```
openplay-sender (bin)                    openplay-receiver (bin)
      │                                            │
      ├── casting.rs                               └── app.rs, window.rs
      │                                                     │
      ▼                                                     ▼
 openplay-airplay  openplay-miracast              openplay-common
 openplay-pipeline openplay-capture
 openplay-discovery
      │
      └── openplay-common

not reached from either binary:
 openplay-signaling ── openplay-protocol      openplay-crypto
 (SignalingServer/Client)                     (CertificateManager)
```

The receiver's only dependency is `openplay-common` — it is a window and nothing
more. `openplay-signaling`, `-protocol` and `-crypto` are workspace members with
no consumer in either binary; the WebRTC path that would use them is unwired.

| Crate | Owns |
|---|---|
| `openplay-sender` | Binary. egui UI, receiver list, `casting.rs` orchestration |
| `openplay-receiver` | Binary. egui window showing a static "waiting" page |
| `openplay-airplay` | AirPlay: HAP pairing, SRP, NTP, mirror stream, TLV8. Also `fairplay.rs`, which has no callers |
| `openplay-miracast` | Miracast/WFD: RTSP, WFD params, Wi-Fi Direct (Linux) |
| `openplay-pipeline` | GStreamer pipeline construction, encoder probing |
| `openplay-signaling` | WebSocket signaling client and server. Never constructed |
| `openplay-discovery` | mDNS advertisement and browsing |
| `openplay-protocol` | `SignalingMessage` wire format and connection state machines |
| `openplay-crypto` | Self-signed certificate lifecycle. Never constructed |
| `openplay-capture` | Screen capture abstraction |
| `openplay-common` | `AppConfig`, XDG paths, logging, shared constants |

Internal dependencies are declared once in the root `Cargo.toml` under
`[workspace.dependencies]` and referenced as `{ workspace = true }`. When adding
or moving a crate, edit only the root manifest.

## The three casting paths

OpenPlay supports three protocols, and they share less than you might expect.
Only capture and encoding are common; discovery, session setup and transport
all differ.

```
                    ┌─────────────────────────────┐
                    │  CaptureSession (capture)   │
                    │  XDG Portal → PipeWire fd   │
                    └──────────────┬──────────────┘
                                   │ CaptureConfig
                    ┌──────────────▼──────────────┐
                    │  probe_best_encoder()       │
                    │  VA-API / NVENC / VT / MF   │
                    │  → x264 fallback            │
                    └──────────────┬──────────────┘
          ┌────────────────────────┼────────────────────────┐
          ▼                        ▼                        ▼
   AirPlaySender-           MiracastSender-           SenderPipeline
   Pipeline                 Pipeline                  (WebRTC)
          │                        │                        │
   H.264 NAL units          RTP/MPEG2-TS over UDP     GStreamer webrtcbin
   over mirror stream       to negotiated port        via SDP + ICE
```

The three protocol paths are documented in detail in
[protocols.md](protocols.md).

**There is no audio path at all.** No crate captures, encodes or transports
audio — `openplay-pipeline` builds video-only pipelines throughout. This is
easy to miss because two layers advertise audio anyway: `Capabilities` in
`openplay-protocol` defaults `audio_codecs` to `["opus"]`, and Miracast's
`WfdAudioCodecs` is negotiated during M3/M4. Both are declarations nothing
honours. Adding audio means new elements in every sender pipeline plus a
transport for each protocol (RTP for Miracast, the AirPlay audio channel,
a WebRTC audio track).

## Key design patterns

These are the conventions that are easy to violate by accident.

### Encoder probing is always runtime

`probe_best_encoder()` in `pipeline/encoder.rs` queries the GStreamer registry
and, for each candidate, additionally tries to *instantiate* it — a factory can
be registered but fail to build when the underlying hardware is absent. Platform
candidate order is VA-API → NVENC on Linux, VideoToolbox on macOS, Media
Foundation → NVENC on Windows, with x264 as the universal fallback.

**Never hardcode an encoder type.** The one legitimate bypass is
`force_sw_encode` in the config, which is handled by `select_encoder()` in
`sender/src/casting.rs` and exists for debugging hardware-encoder problems.

### Session events travel over channels

`AirPlaySession` and `MiracastSession` both report status back to the casting
loop through a `tokio::sync::mpsc` channel of session events. There is **no
shared type** — each crate defines its own, and they differ in what `Ready`
carries:

```rust
// openplay-miracast/src/session.rs
pub enum SessionEvent {
    Ready { width: u32, height: u32, fps: u32, rtp_port: u16, sink_addr: SocketAddr },
    Ended(Option<MiracastError>),
}

// openplay-airplay/src/session.rs — Ready carries nothing
pub enum SessionEvent {
    Ready,
    Ended(Option<AirPlayError>),
}
```

The Miracast path must await `Ready` before constructing its pipeline, because
the negotiated resolution and RTP port are not known until M1–M7 completes. The
AirPlay path already knows its parameters, so its `Ready` only signals that the
mirror stream is open. Either way, a session that ends without ever emitting
`Ready` is a failed handshake.

### Capture is a Linux implementation behind a cross-platform type

`CaptureSession` in `openplay-capture` wraps platform-specific capture. On Linux
it uses `ashpd` to request a PipeWire screencast through the XDG Desktop Portal,
then exposes a file descriptor and a PipeWire node ID that GStreamer's
`pipewiresrc` consumes.

On macOS and Windows there is no portal: `CaptureSession` in `desktop.rs` only
reports the primary display size, and capture itself is left to GStreamer's own
elements — `d3d11screencapturesrc` on Windows, `screencapturesrc` (GStreamer
1.22+) or `avfvideosrc` on macOS, selected in `pipeline/encoder.rs`. Neither has
been exercised, so treat them as untested rather than working. See the README
Status section.

`CaptureConfig` (`pipeline/capture_config.rs`) carries the node ID, fd,
resolution and framerate into the pipeline constructors.

### Platform gating

Two things are Linux-only and must stay behind `#[cfg(target_os = "linux")]`:

- Miracast Wi-Fi Direct (`miracast/wifi_direct.rs`, and the Wi-Fi Direct code
  paths in `miracast/session.rs`), which talks to wpa_supplicant over D-Bus
- PipeWire capture

Gating a module is not enough — every import and every caller needs gating too,
including helper imports that only the gated code uses. This is what issue #6
was: `wifi_direct` was correctly gated in `lib.rs` but imported unconditionally
in `session.rs`, so the crate did not compile off Linux for months without
anyone noticing.

CI now guards this with a `cross-platform-check` job on macOS and Windows. It
covers every crate that builds without GStreamer or PipeWire —
`openplay-common`, `-protocol`, `-crypto`, `-capture`, `-discovery`,
`-signaling`, `-airplay` and `-miracast` — so it needs no system packages.

`openplay-capture` is on that list deliberately: its Windows build was broken by
exactly this class of mistake (`desktop.rs` used the `windows` crate without
declaring it), and an earlier five-crate version of this job did not cover it.
Both the dependency and this list entry landed in #25.

`openplay-pipeline`, `-sender` and `-receiver` need GStreamer and are still only
built on Linux — the same blind spot, one layer up.

These crates cannot be usefully cross-checked from Linux with
`cargo check --target`: `ring` and `rusqlite` compile C that a Linux `cc` will
not build for those hosts, so such a run fails for reasons unrelated to the
code. Native runners are the only reliable signal.

### Configuration is validated once, after overrides

`AppConfig::load_or_create_at()` writes defaults on first run, then validates.
Both binaries call it and then call `validate()` **again** after applying CLI
overrides, because `--port 0` and `--name ""` bypass the first check. See
[configuration.md](configuration.md).

## The OpenPlay/WebRTC path

This is the native protocol, and it is the part that is **not yet wired to
either binary**. The libraries exist but have no test coverage: nothing in the
workspace constructs `SenderPipeline`, `ReceiverPipeline`, `SignalingServer`,
`SignalingClient` or `ReceiverAdvertiser`, and `openplay-signaling` has no tests
at all.

One piece *is* wired: the sender starts `ReceiverBrowser`, so it browses
`_openplay._tcp.local.`. Nothing advertises that service, so it never finds
anything.

Designed flow:

1. The receiver starts `ReceiverAdvertiser` (mDNS, `_openplay._tcp.local.`) and
   `SignalingServer` (WebSocket).
2. The sender runs `ReceiverBrowser` to find receivers, then connects with
   `SignalingClient`.
3. Signaling exchanges, in order: session negotiation → pairing or
   authentication → SDP offer/answer → ICE candidates.
4. On ICE connect, the sender builds a `SenderPipeline`; the receiver builds a
   `ReceiverPipeline`.

The wire format is `SignalingMessage` in `openplay-protocol`, with
`SenderStateMachine` and `ReceiverStateMachine` enforcing legal transitions. See
[protocols.md](protocols.md#openplay-webrtc) for the message list.

What is missing is the glue: `sender/src/app.rs` has a `Protocol::OpenPlay` arm
that sets a status string and stops, and `receiver/src/window.rs` is a static
page.

Both binaries also used to *declare* dependencies on `openplay-signaling`,
`-protocol` and `-crypto` without importing them — the receiver declared eight
unused dependencies in total, including `gstreamer` and `openplay-pipeline`.
Those declarations have been removed, so the manifests now describe what is
actually used, and re-adding one is the first step of wiring this path up.

Tracked in issue #11's follow-up work.
