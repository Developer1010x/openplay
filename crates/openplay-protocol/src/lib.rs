//! The OpenPlay signaling wire format and connection state machines.
//!
//! [`SignalingMessage`] is the JSON message set
//! exchanged over the WebSocket connection between a sender and a receiver:
//! session negotiation, pairing or authentication, SDP and ICE, then session
//! control. [`SenderStateMachine`] and
//! [`ReceiverStateMachine`] reject illegal
//! transitions so a session cannot drift into an undefined state.
//!
//! This crate is pure data and logic — no I/O. The transport lives in
//! `openplay-signaling`.
//!
//! Note that the OpenPlay/WebRTC path is not yet wired to either binary; see the
//! README Status section and `docs/protocols.md`.

mod message;
mod state;

pub use message::*;
pub use state::*;
