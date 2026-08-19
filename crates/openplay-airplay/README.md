# openplay-airplay

AirPlay mirroring protocol implementation. Handles the sender-side flow from
device discovery through stream setup to per-frame video delivery.

> **Interoperability status.** HAP pairing used a fabricated SRP group and could
> never succeed; that is fixed (see `srp.rs`) but is **unconfirmed against physical
> Apple hardware**. `fairplay.rs` uses an invented key derivation and **has no
> callers** — Apple TV 2nd/3rd generation are refused by model string instead.
> Read [docs/crypto.md](../../docs/crypto.md) before debugging a receiver that
> rejects a connection.

## Protocol phases

The AirPlay mirroring handshake proceeds in four phases, all handled by `AirPlaySession`:

1. **Feature negotiation** (`http_session.rs`) — GET `/info` to read server capabilities, POST `/stream` with SDP-like parameters. Checks `AirPlayFeatures` for mirroring support.

2. **HAP pairing** (`hap_pairing.rs`) — HomeKit Accessory Protocol pairing over TLV8-encoded HTTP POST requests to `/pair-setup` and `/pair-verify`. Two modes:
   - Transient pairing (PIN `3939`) — no stored credentials.
   - Full SRP-6a pairing with a 4-digit PIN, followed by Ed25519 signing and X25519 ECDH. Credentials are stored in a SQLite database via `rusqlite`.

3. **FairPlay** (`fairplay.rs`) — AES-CTR content protection negotiation.

4. **Mirror stream** (`mirror_stream.rs`) — raw TCP stream carrying fixed-size 128-byte `MirrorHeader` frames followed by H.264 NAL unit payloads. The casting loop sends SPS/PPS codec data first, then individual video frames.

## Key types

| Type | File | Purpose |
|---|---|---|
| `AirPlaySession` | session.rs | High-level orchestration; emits `SessionEvent::Ready` / `SessionEvent::Ended` |
| `AirPlayFeatures` | features.rs | Bitfield parsed from `0xHEX` or `0xLO,0xHI` mDNS TXT |
| `MirrorHeader` | mirror_header.rs | 128-byte fixed header: payload size (LE u32), packet type, NTP timestamp (BE u64) |
| `MirrorStream` | mirror_stream.rs | Writes codec data, video frames, and heartbeats over TCP |
| `NtpServer` | ntp.rs | Local NTP v4 server (stratum 1, reference "AIRP") required by some receivers |
| `Tlv8Item` | tlv8.rs | TLV8 encode/decode with automatic fragmentation for values > 255 bytes |

## Tests

```bash
cargo test -p openplay-airplay
```
