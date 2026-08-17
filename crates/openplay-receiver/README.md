# openplay-receiver

The receiver binary.

> **Status: not wired up.** Today this starts an egui window showing a static
> "Waiting for a sender to connect…" page and nothing else. mDNS advertisement,
> the signaling server and the receive pipeline are all unimplemented in this
> binary, and it declares only `openplay-common` as a dependency. The sections
> below describe the intended design. See the [Status section of the root
> README](../../README.md) and [docs/protocols.md](../../docs/protocols.md).

## Running

```bash
cargo run -p openplay-receiver
# or after build:
./target/release/openplay-receiver

# Options:
openplay-receiver --name "Living Room TV"   # display name (not yet advertised)
openplay-receiver --port 9000               # override signaling port
openplay-receiver --config /path/to/config.toml
```

## Architecture

**`main.rs`** — parses CLI arguments, loads `AppConfig`, applies overrides for name and port, and calls `app::run(config)`.

**`app.rs`** — builds the eframe native options and runs the egui window. That is
all it does today.

*Intended:* start `ReceiverAdvertiser` (mDNS) and `SignalingServer`, then drive the
`ReceiverStateMachine` through pairing/authentication and hand off to the WebRTC
pipeline.

**`window.rs`** — the egui window. Currently a static waiting page with a `TODO`
where the video widget belongs.

*Intended:* render incoming video as a texture updated from the
`ReceiverPipeline`'s appsink, alongside device name, connection status and sender
identity.

## WebRTC path (designed, not connected)

The design is for the receiver to use `openplay-signaling` (`SignalingServer`) for
ICE/SDP exchange and `openplay-pipeline` (`ReceiverPipeline`) for decoding, with
the pipeline emitting RGBA frames to an appsink that `window.rs` uploads to an
egui texture.

Both types exist and are unreferenced here. This crate previously declared
dependencies on `openplay-pipeline`, `-signaling`, `-protocol`, `-crypto`,
`-discovery`, `gstreamer`, `gstreamer-app` and `tokio` without importing any of
them; those declarations have been removed, so wiring this up starts by re-adding
the ones you need.

## Planned

AirPlay receiver mode and Miracast receiver mode are not implemented, and no
receiver-side stubs exist in the tree. For AirPlay specifically there is a design
document: [docs/airplay-receiver-design.md](../../docs/airplay-receiver-design.md).
