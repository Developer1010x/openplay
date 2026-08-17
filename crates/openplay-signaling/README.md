# openplay-signaling

WebSocket-based signaling channel for OpenPlay's native WebRTC protocol. Provides a
TLS-secured client (sender side) and server (receiver side) that exchange
`SignalingMessage` values.

> **Status: never constructed, and untested.** Neither binary calls
> `SignalingClient` or `SignalingServer`, and this crate has no tests. Both take
> their rustls configuration from the caller, and there is no caller. See the
> [Status section of the root README](../../README.md).

## What it contains

**`SignalingClient`** — connects to a receiver's signaling server. `connect()` returns a pair of tokio channels: one for sending messages to the server and one for receiving messages from it. Runs two background tasks (inbound reader and outbound forwarder) that terminate when either channel is closed.

**`SignalingServer`** — binds a TLS WebSocket server. `run()` accepts connections indefinitely and routes each connection's messages through a shared channel with per-client reply senders. The receiver application creates one server and handles all incoming OpenPlay connections through it.

Both types accept a `rustls::ClientConfig` / `rustls::ServerConfig` populated from the certificates in `openplay-crypto`.

## Protocol transport

Messages are serialized to JSON text frames via `serde_json`. Binary frames and non-text frames are silently ignored. Message ordering within a single connection matches WebSocket frame order.

## Error types

`SignalingError` — `Connection`, `Tls`, `WebSocket`, `Message`, `Bind`, `Timeout`.

## Tests

```bash
cargo test -p openplay-signaling
```
