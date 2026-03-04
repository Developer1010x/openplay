mod client;
mod server;

pub use client::SignalingClient;
pub use server::SignalingServer;

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
