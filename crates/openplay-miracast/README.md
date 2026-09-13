# openplay-miracast

Miracast / Wi-Fi Display (WFD) protocol implementation for the sender side. Supports both infrastructure mode (MICE — Miracast over Existing Network Infrastructure) and Wi-Fi Direct P2P mode on Linux.

## Status: unverified against hardware

**No part of this crate has ever been run against a real Miracast sink.** The M1–M7 code path is complete and it is exercised end to end by tests against a scripted fake sink, but a fake sink agrees with whatever the source does. Treat "Miracast sending works" as unproven until someone casts to an actual display and reports back.

What that gap has already hidden — each of these was found by reading the code, not by a failing cast, and each one alone was enough to make every cast fail:

| Bug | Symptom on real hardware |
|---|---|
| The M1 status check tested the status *line* (`RTSP/1.0 200 OK`) with `starts_with("200")` | Every negotiation rejected at M1, on a perfectly good 200 OK |
| The session sent `Ended` immediately after `Ready` | The cast stopped milliseconds after the pipeline started |
| `negotiate` took the socket by value and dropped it after M7 PLAY | FIN on a session the source had just promised `timeout=30`; the sink tears down |
| No keep-alives on the control connection | The sink drops the session after its 30-second timeout even when nothing else is wrong |
| `client_port=19000-19001` did not parse, and the caller fell back to port 1028 | RTP sent where nothing is listening: no error anywhere, black screen |
| No read timeouts in the RTSP exchange | A sink that stalls hangs the cast permanently, UI stuck |
| `disconnect()` named the root D-Bus interface | Every P2P teardown returned UnknownMethod, was swallowed, and was logged as a success |

They are fixed. The point of the table is that a path nobody has run is not a path that works, and the next unverified assumption in here has not been found yet.

Verified by tests: message framing (including a sink that pipelines SETUP and PLAY into one segment), `client_port` parsing, status-code parsing, the full M1–M7 exchange, keep-alive and teardown handling on the control channel, and the session lifecycle — Ready, staying alive, stop, sink teardown, sink disappearance.

Not verified by anything: that a real sink accepts our `wfd_video_formats`, that the resolution negotiation picks a mode the sink can actually decode, that MPEG2-TS over RTP arrives in a form it can play, and every line of `wifi_direct.rs`, which needs wpa_supplicant, a P2P-capable radio and a real display in the room.

## Protocol flow

Miracast uses RTSP for capability negotiation (M1–M7 messages) and RTP/UDP for the actual media stream.

1. **M1–M2** — OPTIONS exchange to confirm WFD support.
2. **M3** — GET_PARAMETER: sender requests sink's supported video/audio formats.
3. **M4** — SET_PARAMETER: sender announces its own capabilities and selects negotiated resolution.
4. **M5** — SETUP trigger: sender tells sink to prepare for transport setup.
5. **M6** — SETUP: establishes the RTP session; sink replies with the RTP destination port.
6. **M7** — PLAY: streaming begins.

Once M7 completes, `MiracastSession` emits `SessionEvent::Ready` with the negotiated resolution, framerate, and RTP destination. The sender then builds a `MiracastSenderPipeline` and streams H.264 in MPEG-TS over RTP/UDP.

M7 is not the end of the RTSP conversation. The media leaves over UDP and tells the sink nothing about our liveness, so the session lives on the control connection: `serve_control_channel` holds it open for the whole cast, pings the sink every 10 seconds (`GET_PARAMETER` with no body, the RFC 2326 §10.8 ping), answers the sink's own requests, and returns only when the sink sends TEARDOWN, stops answering, or drops the connection.

## Key types

**`MiracastSession`** — orchestrates the connection lifecycle. Created with `start(sink_addr)` for MICE or `start_wifi_direct(peer_mac, port)` for P2P. Events emitted via `SessionEvent::Ready` / `SessionEvent::Ended`. The session task runs for the life of the cast and holds the event sender; `stop()` (also called on drop) is the only way to end it from the sender side.

**`RtspConnection`** — the control connection: the socket plus its framing buffer. Generic over the transport so the negotiation can be driven over a pipe in tests.

**`WfdVideoFormats`** — encodes and parses the `wfd-video-formats` parameter string. Carries H.264 profile, level, and CEA/VESA/HH resolution bitmasks.

**`CeaResolutions`** — u32 bitmask of supported CEA resolutions. `negotiate_resolution()` finds the highest common resolution between source and sink.

**`WfdAudioCodecs`** — LPCM, AAC, AC3 codec bitmasks formatted for WFD parameter exchange.

**`WfdClientRtpPorts`** — transport profile and port numbers for `wfd-client-rtp-ports`.

## Wi-Fi Direct (Linux only)

`wifi_direct.rs` uses `zbus` to communicate with `wpa_supplicant` over D-Bus. Handles P2P peer discovery, group formation, and IP address resolution for the peer device.

Two things to know before editing it. `Connect`, `StopFind` and `Disconnect` live on `fi.w1.wpa_supplicant1.Interface.P2PDevice` and are called on an *interface* object path; the root interface name differs only by a suffix and answers UnknownMethod on that path. And the group teardown runs from `P2PGroupGuard::drop` in `session.rs`, not from a cleanup line at the end of the session function — stopping a cast cancels that function wherever it is parked, so anything written after the streaming phase never runs.

## Tests

```bash
cargo test -p openplay-miracast
```

The tests that matter most drive a scripted fake sink: `rtsp_server::tests` over `tokio::io::duplex`, `session::tests` over a loopback TCP socket. `test_sink::FakePeer` is shared between them — a real sink script, not a mock — but it agrees with the source about anything the WFD spec leaves ambiguous, which is precisely where hardware will disagree.
