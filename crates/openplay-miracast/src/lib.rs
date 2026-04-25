pub mod rtsp_server;
pub mod session;
pub mod wfd_params;

/// Wi-Fi Direct P2P discovery via wpa_supplicant D-Bus.
/// Only available on Linux where wpa_supplicant is present.
#[cfg(target_os = "linux")]
pub mod wifi_direct;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum MiracastError {
    #[error("RTSP error: {0}")]
    Rtsp(String),

    #[error("WFD negotiation failed: {0}")]
    Negotiation(String),

    #[error("Connection failed: {0}")]
    Connection(String),

    #[error("No compatible video format")]
    NoCompatibleFormat,

    #[error("Pipeline error: {0}")]
    Pipeline(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Session closed")]
    SessionClosed,
}
