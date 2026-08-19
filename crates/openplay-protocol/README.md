# openplay-protocol

Defines the wire format and state machines for the OpenPlay WebRTC signaling
protocol.

> **Status: no consumers yet.** This crate is implemented and well tested, but the
> WebRTC path it describes is not wired to either binary, so nothing outside this
> crate's tests uses it. See the [Status section of the root
> README](../../README.md).

## What it contains

**`SignalingMessage`** — the single enum that covers every message exchanged over the WebSocket signaling channel. Tagged JSON with `"type": "snake_case_variant"`. Variants fall into five groups:

- Session negotiation: `SessionRequest`, `SessionAccept`, `SessionReject`
- Pairing (first connection): `PairingChallenge`, `PairingResponse`, `PairingConfirm`
- Authentication (subsequent connections): `AuthChallenge`, `AuthResponse`, `AuthConfirm`
- WebRTC: `SdpOffer`, `SdpAnswer`, `IceCandidate`, `IceComplete`
- Control: `BitrateHint`, `Ping`, `Pong`, `SessionEnd`

**`SenderStateMachine` / `ReceiverStateMachine`** — enforce valid state transitions for each side. Invalid transitions return `StateError::InvalidTransition` containing the from-state, intended to-state, and triggering event.

**`sender_event_from_message()` / `receiver_event_from_message()`** — map incoming `SignalingMessage` values to the appropriate state event. Returns `None` for messages handled inline rather than through the state machine.

## State flows

Sender: `Idle → Discovering → Connecting → [Pairing →] Authenticating → Signaling → Streaming → Disconnecting → Idle`

Receiver: `Idle → Advertising → PendingConnection → [Pairing →] Authenticating → Signaling → Receiving → Disconnecting → Advertising`

## Tests

```bash
cargo test -p openplay-protocol
```
