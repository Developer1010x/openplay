# openplay-receiver

The receiver binary. Advertises itself on the local network, accepts incoming screen cast connections from OpenPlay senders, and displays the video stream in a window.

## Running

```bash
cargo run -p openplay-receiver
# or after build:
./target/release/openplay-receiver

# Options:
openplay-receiver --name "Living Room TV"   # override advertised display name
openplay-receiver --port 9000               # override signaling port
openplay-receiver --config /path/to/config.toml
```

## Architecture

**`main.rs`** — parses CLI arguments, loads `AppConfig`, applies overrides for name and port, and calls `app::run(config)`.

**`app.rs`** — the main application loop. Starts:
1. `ReceiverAdvertiser` — publishes the device on mDNS so senders can find it.
2. `SignalingServer` — listens on the configured port for incoming WebSocket connections from senders.
3. The egui window via `eframe::run_native`.

When a sender connects, `app.rs` drives the `ReceiverStateMachine` through pairing/authentication and then hands off to the WebRTC pipeline.

**`window.rs`** — the egui window. Renders the incoming video as a texture updated from the `ReceiverPipeline`'s appsink. Also shows the device name, current connection status, and sender identity.

## WebRTC path

The receiver uses `openplay-signaling` (`SignalingServer`) for ICE/SDP exchange and `openplay-pipeline` (`ReceiverPipeline`) for decoding. The pipeline outputs RGBA frames to an appsink; `window.rs` uploads each frame to an egui texture for display.

## Planned

AirPlay receiver mode and Miracast receiver mode are not yet implemented. The stubs live in `receiver/airplay/` and `receiver/miracast/` in the planned repository layout.
