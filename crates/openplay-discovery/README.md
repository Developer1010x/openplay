# openplay-discovery

mDNS service advertisement and browsing for all three protocols supported by OpenPlay. Used by both the sender (to find receivers) and the receiver (to announce itself).

## What it contains

### OpenPlay receivers

**`ReceiverAdvertiser`** — registers an `_openplay._tcp.local.` mDNS service. **Nothing constructs this yet** — the receiver binary does not call it, so no OpenPlay instance is currently discoverable. The sender does browse for the service.

**`ReceiverBrowser`** — browses for `_openplay._tcp.local.` services and sends `DiscoveryEvent::ReceiverFound` / `ReceiverLost` over a channel.

**`TxtRecord`** — the mDNS TXT record for OpenPlay services. Carries: `display_name`, `fingerprint` (cert SHA-256), `video_codecs`, `max_width`, `max_height`, `max_fps`. Serialized with `to_properties()` / `from_properties()`.

### AirPlay receivers

**`AirPlayBrowser`** — browses `_airplay._tcp.local.`. Emits `DiscoveryEvent::AirPlayReceiverFound` / `AirPlayReceiverLost`.

**`AirPlayTxtRecord`** — parses AirPlay mDNS TXT records. Required field: `deviceid`. Optional: `features`, `model`, `pk`, `flags`, `srcvers`, `protovers`.

**`AirPlayFeatures`** is in `openplay-airplay`; the feature bits are linked from the record.

### Miracast receivers

**`MiracastBrowser`** — browses three service types simultaneously: `_display._tcp.local.`, `_miracast._tcp.local.`, `_wfd._tcp.local.`. Emits `DiscoveryEvent::MiracastReceiverFound` / `MiracastReceiverLost`.

## DiscoveryEvent

All three browsers emit events through the same `DiscoveryEvent` enum, which the sender UI consumes from a single channel to populate its receiver list.

## Tests

```bash
cargo test -p openplay-discovery
```
