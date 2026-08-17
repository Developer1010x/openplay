# Protocols

Three casting paths, in decreasing order of how well they work today.

All three are **video only**. Nothing in the workspace captures or transmits
audio, even where the protocol layer advertises it — see
[architecture.md](architecture.md#the-three-casting-paths).

| Protocol | Discovery | Session setup | Transport | State |
|---|---|---|---|---|
| Miracast | mDNS + Wi-Fi Direct | RTSP M1–M7 | RTP/MPEG2-TS over UDP | Works |
| AirPlay | mDNS `_airplay._tcp` | HTTP/plist + HAP | Mirror stream (TCP) | Pairing unconfirmed; FairPlay not wired in |
| OpenPlay | mDNS `_openplay._tcp` | WebSocket signaling | WebRTC | Sender browses; everything else is libraries only |

---

## Miracast / Wi-Fi Display

The most complete path.

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

The native protocol. Treat this section as a design description.

`openplay-protocol` is implemented and well tested. `openplay-signaling` has no
tests at all, and the WebRTC pipelines are only covered at the config/encoder
level. Neither binary calls any of it, except that the sender does browse for
`_openplay._tcp.local.` — a service nothing advertises.

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

### What is missing

The transport and state layers are done. The glue is not:

- `sender/src/app.rs` — the `Protocol::OpenPlay` match arm sets a status string
  and clears `is_casting`
- `receiver/src/window.rs` — a static "waiting for a sender" page
- `CertificateManager` is never constructed outside its own tests, so the
  identity the signaling layer expects is not generated. `openplay-crypto` no
  longer even depends on `rustls` — it only produces the certificate, and
  nothing consumes it

`SenderPipeline`, `ReceiverPipeline`, `SignalingServer`, `SignalingClient` and
`ReceiverAdvertiser` all exist and have no callers in either binary.
