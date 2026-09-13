# Protocols

Three casting paths. All three are connected to the binaries; none of them has a
confirmed success against the hardware or second machine it is aimed at, so the
"State" column below describes evidence, not confidence.

All three are **video only**. Nothing in the workspace captures or transmits
audio, even where the protocol layer advertises it — see
[architecture.md](architecture.md#the-three-casting-paths).

| Protocol | Discovery | Session setup | Transport | State |
|---|---|---|---|---|
| OpenPlay | mDNS `_openplay._tcp` | TLS WebSocket signaling + consent prompt | WebRTC | Wired both ends, covered by loopback tests; never run between two machines |
| Miracast | mDNS + Wi-Fi Direct | RTSP M1–M7, then a control channel held open | RTP/MPEG2-TS over UDP | Wired end to end; never verified against a real sink |
| AirPlay | mDNS `_airplay._tcp` | HTTP/plist + HAP | Mirror stream (TCP) | Pairing unconfirmed against hardware; FairPlay not wired in and will not be |

---

## Miracast / Wi-Fi Display

The most fully specified path, and the least evidenced: nothing in this
repository has ever been confirmed to drive a real Miracast sink. Treat a cast
that fails as an OpenPlay bug until proven otherwise — that is not politeness,
it is the base rate. A defect that ended every cast within milliseconds of a
successful M1–M7 lived here undetected, because passing the handshake looks like
success in a log and nobody was watching a screen.

### Discovery

`MiracastBrowser` browses three service types, because sinks disagree about
which to advertise:

- `_display._tcp.local.` — the Wi-Fi Alliance MICE spec
- `_miracast._tcp.local.`
- `_wfd._tcp.local.`

On Linux, Wi-Fi Direct peers are found separately through `wifi_direct.rs`,
which talks to wpa_supplicant over D-Bus rather than NetworkManager.

### Two transports

**MICE (Miracast over Infrastructure).** Both machines are already on the same
network. The user supplies or discovers the sink IP, and `MiracastSession::start`
connects to it over TCP for RTSP negotiation. Available on all platforms.

**Wi-Fi Direct P2P.** Linux only. `MiracastSession::start_wifi_direct` drives
wpa_supplicant to form a P2P group with GO intent 0 — preferring the client
role, as miraclecast does — then resolves the peer IP, by ARP if the
`GroupStarted` signal did not carry one.

Note the role inversion here, which surprises people: after the group forms, the
**source is the RTSP server**. OpenPlay listens on port 7236 for the sink to
connect to it, with a fallback to connecting outbound after 30 seconds if the
sink never does.

### RTSP negotiation (M1–M7)

`rtsp_server.rs` drives the WFD handshake as a fixed request/response sequence in
`negotiate()`. The `WfdState` enum labels the steps for log output; it is never
stored or transitioned:

| Step | Direction | Message |
|---|---|---|
| M1 | Source → Sink | `OPTIONS` |
| M2 | Sink → Source | `OPTIONS` request — the source replies `200 OK` with `Public:` |
| M3 | Source → Sink | `GET_PARAMETER` — capability query |
| M4 | Source → Sink | `SET_PARAMETER` — chosen parameters |
| M5 | Source → Sink | `SET_PARAMETER` with body `wfd_trigger_method: SETUP` |
| M6 | Sink → Source | `SETUP` with transport |
| M7 | Sink → Source | `PLAY` |

Video format negotiation lives in `wfd_params.rs` (`WfdVideoFormats`). On
success the session emits `SessionEvent::Ready` with the agreed resolution,
framerate and RTP port, and `MiracastSenderPipeline` starts streaming
H.264 in MPEG2-TS over RTP/UDP.

### After M7: the control channel is the session

Media goes out over UDP, which tells the sink nothing about whether the source
is still there. The session therefore lives on the RTSP connection, and
`serve_control_channel` holds it open for the whole cast: it answers what the
sink asks, pings on a keepalive timer when the sink has been silent, and returns
only when the sink ends the session or the connection breaks.

This is the part to be careful with when editing `run_session`, because the
failure is silent and looks like the sink's fault. **Returning early by any
route ends the cast**, not only by sending `Ended`: dropping `conn` shows the
sink a FIN on a session it was promised a timeout on, and dropping `evt_tx`
closes the channel, which the casting loop's select reads as `None` and treats
exactly like an `Ended`. An earlier version of this code sent `Ended` on the
line after `Ready` and every cast died in milliseconds.

---

## AirPlay

Sender only. Discovery, the session layer, TLV8, NTP, the mirror stream and HAP
pairing are implemented. FairPlay is written but **not wired into the session
flow at all** — see [crypto.md](crypto.md).

### Flow

1. `AirPlayBrowser` discovers receivers via mDNS `_airplay._tcp.local.`
2. `start_airplay_cast` in `sender/src/casting.rs` is called with the address
3. `AirPlaySession::start` spawns `run_session`, which:
   - starts the **NTP server** on port 7010 (`ntp.rs`)
   - tries an unauthenticated `http_session::negotiate` — `GET /info`, then
     `POST /stream`
   - if that fails with **501 or 403**, falls back to `negotiate_with_auth`:
     `GET /info` to identify the model, then HAP **transient** pair-setup
     followed by pair-verify (`hap_pairing.rs`), then `POST /stream` on the
     verified connection
   - wraps the resulting connection in a `MirrorStream` and starts a 2-second
     heartbeat
4. `AirPlaySenderPipeline` captures and encodes, emitting H.264 NAL units to an
   appsink
5. The casting loop copies the SPS/PPS out of the first frame and sends them once
   as codec data, then forwards each access unit unmodified via
   `AirPlaySession::send_video_frame`

Two things this flow does **not** do, both worth knowing:

- **There is no FairPlay phase.** `session.rs` never references `fairplay.rs`,
  and `fp_setup` has no callers anywhere. Instead `negotiate_with_auth` inspects
  the model string from `/info` and refuses `AppleTV2,*` / `AppleTV3,*` up front
  with an explicit "requires FairPlay authentication which is not supported"
  error. The comment in `run_session` about "optionally with FairPlay
  encryption" describes an intention, not behaviour.
- **Only transient pairing is attempted.** The PIN flow (`pair_setup`) exists and
  is exercised by the `pair_probe` example, but the session path only ever calls
  `pair_setup_transient`.

On feature parsing: `AirPlayFeatures::parse` returns `Option<Self>` so a
malformed `features` string stays distinguishable from an absent one, but the
`/info` path deliberately discards that distinction with `unwrap_or_default()`,
treating a malformed value as "advertises nothing" rather than failing the whole
`/info` parse.

`ntp.rs` implements the timing channel; `mirror_header.rs` the per-frame header;
`tlv8.rs` the TLV8 encoding HAP uses throughout. The mirror connection itself is
established by `POST /stream` in `http_session.rs`; `mirror_stream.rs` then
frames video, codec data and heartbeats onto it.

### Pairing modes

- `pair_setup_transient(addr)` — no PIN. Used when the receiver is set to
  "Everyone on the Same Network". Sends flags `0x02` and uses the standard
  transient PIN `3939`. **This is the only mode the session path uses.**
- `pair_setup(addr, pin)` — first-time pairing with a 4-digit PIN. Reachable via
  the `pair_probe` example, not from the session flow.
- `pair_verify(...)` — subsequent connections, using stored Ed25519 keys.

Ed25519 signing and ChaCha20-Poly1305 are used in pair-setup M5/M6; X25519 ECDH
is used separately in `pair_verify`.

`hap_pairing.rs` provides SQLite helpers for paired devices (`init_paired_db`,
`store_paired_device`, `load_paired_device`), but nothing outside the module's
own unit tests calls them — pairings are not actually persisted between runs.

---

## OpenPlay (WebRTC)

The native protocol, and the only one where both ends are OpenPlay.

Both binaries drive it: `receiver/src/net.rs` advertises, listens, prompts for
consent and answers; `sender/src/casting.rs` browses, pins, requests a session
and offers. `openplay-protocol` is well tested, `openplay-signaling` is covered
by a loopback TLS test, and `openplay-pipeline` has a test in which two
`webrtcbin`s negotiate and carry real decoded frames. What does **not** exist is
a report of it working between two separate machines. See
[architecture.md](architecture.md#the-openplaywebrtc-path) for the call path and
for what the tests do and do not exercise.

### Message flow as implemented

The design below has a pairing/authentication phase. The code does not.

```
sender                                           receiver
  │                                                 │
  │  (browses _openplay._tcp, reads addr + fp)      │  (advertises, fp in TXT)
  │                                                 │
  │────── TLS handshake, cert pinned to fp ────────▶│
  │────── SessionRequest ──────────────────────────▶│
  │                                                 │  ⟵ human presses Allow
  │◀───── SessionAccept { negotiated } ─────────────│
  │                                                 │
  │────── SdpOffer ────────────────────────────────▶│  builds ReceiverPipeline,
  │◀───── SdpAnswer ────────────────────────────────│  starts it, then answers
  │◀──┬── IceCandidate (trickle, both ways) ───┬───▶│
  │   └── IceComplete ────────────────────────┘     │
  │                                                 │
  │═════════ H.264 over webrtcbin ═════════════════▶│  decode → RGBA → egui
  │                                                 │
  │────── SessionEnd ──────────────────────────────▶│
```

Rejections a sender can receive: `VersionMismatch` (protocol versions differ),
`NoCompatibleCodecs` (the sender did not offer H.264), `Busy` (the receiver is
already showing another device), and `Denied` (the person pressed Deny).
`NotPaired` is defined and never sent, because there is no pairing.

Not shown, because they are implemented but incidental: `Ping`/`Pong`, where the
receiver answers with a real clock reading rather than echoing the sender's
timestamp — an echo would make every clock-offset calculation come out as
exactly `-rtt/2`, a plausible-looking number that is always wrong and that no
test would catch.

### Wire format

`SignalingMessage` in `openplay-protocol/src/message.rs`, serialised as JSON
over a WebSocket.

**Session negotiation**
- `SessionRequest { sender_id, display_name, protocol_version, capabilities }`
- `SessionAccept { receiver_id, negotiated }`
- `SessionReject { reason }`

**Pairing** (first connection)
- `PairingChallenge { receiver_pub_ecdh }`
- `PairingResponse { sender_pub_ecdh, pin_proof }`
- `PairingConfirm { confirm, receiver_cert_fingerprint }`

**Authentication** (subsequent connections)
- `AuthChallenge { nonce }` / `AuthResponse { nonce, proof }` / `AuthConfirm { proof }`

**WebRTC signaling**
- `SdpOffer { sdp }` / `SdpAnswer { sdp }`
- `IceCandidate { candidate, sdp_mid, sdp_mline_index }` / `IceComplete`

**Session control**
- `BitrateHint { target_kbps, reason }`
- `Ping { timestamp_ms }` / `Pong { timestamp_ms, receiver_timestamp_ms }`
- `SessionEnd { reason }`

`Capabilities` defaults to H.264 video, Opus audio, 60 fps and cursor support.

### State machines

`openplay-protocol/src/state.rs` holds two machines that reject illegal
transitions rather than letting the session drift into an undefined state.

```
Sender:   Idle → Discovering → Connecting → Pairing ─┐
                                          → Authenticating → Signaling → Streaming → Disconnecting
Receiver: Idle → Advertising → PendingConnection → Pairing ─┐
                                                  → Authenticating → Signaling → Receiving → Disconnecting
```

`sender_event_from_message` and `receiver_event_from_message` map an incoming
`SignalingMessage` to the event that should drive the machine, so transport and
state logic stay separate.

**Neither machine is wired up.** `SenderStateMachine` and `ReceiverStateMachine`
have no callers in either binary or in `openplay-signaling`; the two session
loops enforce their own ordering directly instead — the receiver by matching on
message plus current state in `net.rs`, the sender by its
`wait_for_session_accept` → `drive_session` sequence. Using them would be an
improvement, not a rewrite.

### Transport security

The signaling channel is TLS from the first byte. The receiver serves
`CertificateManager::server_config()`; the sender builds
`client_config_pinned(fingerprint)` from the `fp` TXT key and refuses to connect
at all if the receiver advertised no fingerprint.

Both ends deviate from webpki's usual path because there is nothing to chain to:
the certificate is self-signed and the address is a bare LAN IP, so there is no
CA and no hostname to match. The pin replaces both.

**Pinning is not authentication**, and `openplay-crypto/src/tls.rs` says so in
its own module docs. The TXT record is unauthenticated, so an attacker on the
LAN can advertise a receiver carrying their own fingerprint, and a sender that
has never seen the real one will pin the attacker's certificate quite happily.
What pinning buys is confidentiality against a passive eavesdropper, and
detection of a *substituted* certificate on any later connection.

Real authentication is what the `PairingChallenge` / `PairingResponse` /
`PairingConfirm` messages above are for — a code confirmed on both screens.
Those messages are defined and **nothing sends or handles them**. Until they
exist, the receiver's consent prompt is the only thing standing between a
stranger on the network and the screen; see
[architecture.md](architecture.md#consent-is-the-security-model).

### What is still missing

- **Pairing and authentication.** The messages are defined; no code path uses
  them. `RejectReason::NotPaired` is therefore never sent.
- **Renegotiation.** A second `SdpOffer` on an established session is logged and
  ignored.
- **Audio.** `Capabilities` advertises Opus by default; the pipelines are
  video-only, and the receiver negotiates `audio_codec: None`.
- **A cross-machine run.** Everything above is exercised in one process, over
  loopback, against a `videotestsrc` stand-in for the real capture pipeline.
