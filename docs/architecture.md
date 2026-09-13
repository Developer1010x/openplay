# Architecture

OpenPlay is a flat Cargo workspace of eleven crates. Two are binaries; the other
nine are libraries, and every one of them is now reached from at least one
binary.

For what actually works today versus what is merely connected, see the Status
section of the [README](../README.md). This document describes the shape of the
code. Where a path is wired but unverified — which is most of the OpenPlay and
Miracast paths — it says so.

## Crate graph

Arrows are dependencies the code actually uses today.

```
openplay-sender (bin)                     openplay-receiver (bin)
      │                                             │
      ├── app.rs, receiver_list.rs                  ├── app.rs, window.rs
      └── casting.rs                                └── net.rs
      │                                             │
      ▼                                             ▼
 openplay-airplay   openplay-capture         openplay-discovery
 openplay-miracast  openplay-discovery       openplay-signaling
 openplay-pipeline  openplay-signaling       openplay-protocol
 openplay-protocol  openplay-crypto          openplay-crypto
      │                                      openplay-pipeline
      └── openplay-common                    openplay-common
```

The receiver is no longer a window with one dependency: `net.rs` owns mDNS
advertisement, the TLS signaling server and the session loop, so it pulls in
`-crypto`, `-discovery`, `-pipeline`, `-protocol` and `-signaling`. The sender
gained `-signaling`, `-protocol`, `-crypto` and `url` for the same reason.

Only `openplay-capture` is asymmetric: the sender captures a screen, the
receiver displays one, so the receiver does not depend on it.

| Crate | Owns |
|---|---|
| `openplay-sender` | Binary. egui UI, receiver list, `casting.rs` orchestration for all three protocols |
| `openplay-receiver` | Binary. egui window (waiting page, consent prompt, video) plus `net.rs` — advertisement, signaling server, session loop |
| `openplay-airplay` | AirPlay: HAP pairing, SRP, NTP, mirror stream, TLV8. Also `fairplay.rs`, which has no callers |
| `openplay-miracast` | Miracast/WFD: RTSP, WFD params, Wi-Fi Direct (Linux) |
| `openplay-pipeline` | GStreamer pipeline construction, encoder probing, and `webrtc.rs` — SDP and trickle ICE over `webrtcbin` |
| `openplay-signaling` | TLS WebSocket signaling client and server |
| `openplay-discovery` | mDNS advertisement and browsing |
| `openplay-protocol` | `SignalingMessage` wire format and connection state machines. The messages are used; **the state machines still have no callers** |
| `openplay-crypto` | Self-signed certificate lifecycle plus `tls.rs`, the rustls server and pinning-client configs |
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
reports a display size, and capture itself is left to GStreamer's own elements —
`d3d11screencapturesrc` on Windows, `screencapturesrc` (GStreamer 1.22+) or
`avfvideosrc` on macOS, selected in `pipeline/encoder.rs`. Neither has been
exercised, so treat them as untested rather than working. See the README Status
section.

Be precise about "reports the display size", because only Windows does.
`query_primary_display_size()` calls `GetSystemMetrics(SM_CXSCREEN/SM_CYSCREEN)`
under `#[cfg(target_os = "windows")]`; the `#[cfg(target_os = "macos")]` branch
is an empty block containing a comment about using CoreGraphics, so control
falls through to the shared default and **macOS always reports a hardcoded
1920x1080**, whatever the panel actually is.

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

CI now guards this with a `cross-platform-check` job on macOS and Windows, and
an `MSRV` job that uses the same crate selection. Both are expressed as
`--workspace` minus `openplay-pipeline`, `-sender` and `-receiver`, so they cover
every crate that builds without GStreamer or PipeWire — today
`openplay-common`, `-protocol`, `-crypto`, `-capture`, `-discovery`,
`-signaling`, `-airplay` and `-miracast` — and a new crate is picked up
automatically rather than being silently uncovered.

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

This is the native protocol, and it is now driven by both binaries. What it has
never had is a report of working between two separate machines — the coverage is
all in-process. Read this section as "here is the call path", not "here is a
proven feature".

### Receiver: `receiver/src/net.rs`

`net::start` runs before the window opens, so the mDNS service is registered by
the time the waiting page appears, and returns a `NetHandle` the window holds
for the life of the process. Dropping it unregisters the service and stops the
runtime. In order:

1. `CertificateManager::load_or_generate(data_dir)` — this is the first launch
   at which a certificate exists on disk.
2. `ReceiverAdvertiser::new(txt_record(config, certs.fingerprint()))`, inside
   `runtime.enter()` because the mDNS daemon spawns work expecting a reactor.
   Advertising **first** is deliberate: the fingerprint in the TXT record must
   match the certificate the server is about to present.
3. `SignalingServer::bind` on `[::]:port` with `certs.server_config()`, falling
   back to `0.0.0.0` on hosts with IPv6 disabled. Dual-stack matters because
   mDNS advertises every interface address including IPv6 ones, and an
   IPv4-only bind would publish addresses nothing listens on.
4. A `SessionLoop` task, which owns all session state and handles one message at
   a time.

A failed advertisement is **degraded, not fatal**: the window says the receiver
is not discoverable, and the server still accepts a sender given the address by
hand. A failed bind is fatal, and is reported before the window claims to be
listening.

### Sender: `sender/src/casting.rs`

`start_openplay_cast` is called from the `Protocol::OpenPlay` arm of
`sender/src/app.rs`, which requires **both** an address and a fingerprint from
the discovered receiver. Missing either is a visible failure — there is no
fall-back to unpinned TLS, because connecting anyway would mean trusting
whichever host answered.

It captures the screen, pins with `openplay_crypto::client_config_pinned`,
connects `SignalingClient` to `wss://addr`, sends `SessionRequest`, and then
**waits with no timeout** for the human at the far end. Only after
`SessionAccept` does it build a `SenderPipeline` — there is no point touching
the GPU before that. `drive_session` then pumps SDP and ICE in both directions
until the cast ends.

### Consent is the security model

The receiver refuses to display anything until a person presses **Allow** on the
prompt in `window.rs`. `handle_session_request` sets `Status::PendingConsent` and
stops; only `NetHandle::decide` sends `SessionAccept`. The decision carries the
`ConnectionId` from the prompt, so an answer cannot land on a different sender
that connected in between, and a prompt whose sender disappeared is cleared by
the liveness tick rather than left on screen.

Be clear about why the prompt has to exist. **Nothing on this path authenticates
a sender**: the mDNS TXT record is unauthenticated, so pinning its fingerprint
gives confidentiality against a passive eavesdropper and detects a substituted
certificate later, but is not proof of identity. `openplay-protocol` defines
`PairingChallenge` / `PairingResponse` / `PairingConfirm` and the `Auth*`
messages; **nothing sends or handles them**, and the session goes straight from
negotiation to SDP. Without the prompt, any device on the network could put
pixels on the screen unprompted.

One session runs at a time, because there is one screen; a second sender is
refused with `RejectReason::Busy` rather than silently replacing the first. Only
the connection that owns a session may end it.

### `pipeline/webrtc.rs`

The pipelines build element graphs and never talk to a peer. `WebRtcPeer` is the
part in between: it drives `webrtcbin`'s offer/answer dance and converts its
GObject signals into `WebRtcEvent`s.

Three constraints shape it, and each is easy to get wrong:

- **The channel is unbounded on purpose.** `webrtcbin` delivers promise replies,
  ICE candidates and state changes on GStreamer streaming threads, while
  signaling lives in tokio. `UnboundedSender::send` is synchronous and needs no
  reactor; `Sender::blocking_send` panics outright when called from inside a
  runtime thread. Traffic is a handful of messages per session.
- **Exactly one peer offers.** `Role::Offerer` answers `on-negotiation-needed`
  by building an offer; `Role::Answerer` ignores that signal and only answers
  once `set_remote_description` has fed it an offer. Both offering, or neither,
  is the classic way to get a session that negotiates forever and carries no
  video.
- **The pipeline must be playing before negotiation.** An unstarted `webrtcbin`
  has no clock and produces no ICE candidates, which is why both ends start
  their pipeline before creating a description.

No STUN or TURN server is configured, so only host candidates are gathered.
That is deliberate — casts are LAN-local, and contacting a third-party STUN
server would leak that a cast is happening. `WebRtcPeer::set_stun_server` exists
for deployments that need it.

### `webrtcbin` needs the `nice` plugin

Without libnice's GStreamer plugin, `webrtcbin` constructs successfully and then
refuses every `sink_%u` pad request, so nothing links and the failure names no
cause. It is packaged separately from `gst-plugins-bad` everywhere — see
[install.md](install.md#the-nice-plugin-specifically).

### What covers it, and what does not

- `crates/openplay-signaling/tests/loopback.rs` — three tests over real loopback
  TLS: a full session exchange, a client pinning the wrong certificate being
  refused, and the server dropping messages that fail validation.
- `crates/openplay-pipeline/tests/webrtc_loopback.rs` — two `webrtcbin`s
  negotiating in one process and carrying decoded frames, asserting the RGBA
  packing contract. It builds a `videotestsrc`-fed **look-alike** of
  `SenderPipeline` rather than the real one, which is hardwired to a PipeWire
  source needing a desktop portal and a user click. So the real sender graph is
  not what this test exercises.
- `crates/openplay-crypto/tests/tls_handshake_test.rs` — three tests on pinning,
  including that the default verifier would reject the same certificate.

Not covered: two machines, a real network, mDNS between hosts, and the real
capture-fed sender pipeline. `SenderStateMachine` and `ReceiverStateMachine` in
`openplay-protocol` are still unused by both binaries and by the signaling
crate — the loops enforce their own ordering instead. The wire format is
`SignalingMessage`; see [protocols.md](protocols.md#openplay-webrtc) for the
message list.
