//! WebSocket signaling transport for the OpenPlay protocol.
//!
//! [`SignalingServer`] is hosted by the receiver;
//! [`SignalingClient`] is used by the sender. Both
//! carry [`SignalingMessage`](openplay_protocol::SignalingMessage)
//! values over TLS, taking their rustls configuration from the caller.
//!
//! The receiver hosts a server in `openplay-receiver`'s `net` module; the
//! sender dials it from `openplay-sender`'s `casting` module.

mod client;
mod server;

pub use client::SignalingClient;
pub use server::{ConnectionHandle, ConnectionId, IncomingMessage, SignalingServer};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum SignalingError {
    #[error("Connection failed: {0}")]
    Connection(String),

    #[error("TLS error: {0}")]
    Tls(String),

    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("Message error: {0}")]
    Message(String),

    #[error("Server bind error: {0}")]
    Bind(String),

    #[error("Timeout")]
    Timeout,
}
