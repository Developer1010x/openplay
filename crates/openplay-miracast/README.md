# openplay-miracast

Miracast / Wi-Fi Display (WFD) protocol implementation for the sender side. Supports both infrastructure mode (MICE — Miracast over Existing Network Infrastructure) and Wi-Fi Direct P2P mode on Linux.

## Protocol flow

Miracast uses RTSP for capability negotiation (M1–M7 messages) and RTP/UDP for the actual media stream.

1. **M1–M2** — OPTIONS exchange to confirm WFD support.
2. **M3** — GET_PARAMETER: sender requests sink's supported video/audio formats.
3. **M4** — SET_PARAMETER: sender announces its own capabilities and selects negotiated resolution.
4. **M5** — SETUP trigger: sender tells sink to prepare for transport setup.
5. **M6** — SETUP: establishes the RTP session; sink replies with the RTP destination port.
6. **M7** — PLAY: streaming begins.

Once M7 completes, `MiracastSession` emits `SessionEvent::Ready` with the negotiated resolution, framerate, and RTP destination. The sender then builds a `MiracastSenderPipeline` and streams H.264 in MPEG-TS over RTP/UDP.

## Key types

**`MiracastSession`** — orchestrates the connection lifecycle. Created with `start(sink_addr)` for MICE or `start_wifi_direct(peer_mac, port)` for P2P. Events emitted via `SessionEvent::Ready` / `SessionEvent::Ended`.

**`WfdVideoFormats`** — encodes and parses the `wfd-video-formats` parameter string. Carries H.264 profile, level, and CEA/VESA/HH resolution bitmasks.

**`CeaResolutions`** — u32 bitmask of supported CEA resolutions. `negotiate_resolution()` finds the highest common resolution between source and sink.

**`WfdAudioCodecs`** — LPCM, AAC, AC3 codec bitmasks formatted for WFD parameter exchange.

**`WfdClientRtpPorts`** — transport profile and port numbers for `wfd-client-rtp-ports`.

## Wi-Fi Direct (Linux only)

`wifi_direct.rs` uses `zbus` to communicate with `wpa_supplicant` over D-Bus. Handles P2P peer discovery, group formation, and IP address resolution for the peer device.

## Tests

```bash
cargo test -p openplay-miracast
```
