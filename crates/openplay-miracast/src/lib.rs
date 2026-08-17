//! Miracast / Wi-Fi Display sender protocol.
//!
//! Two transports:
//!
//! - **MICE** (Miracast over Infrastructure) — both devices already share a
//!   network. Available on every platform.
//! - **Wi-Fi Direct P2P** — Linux only, driving wpa_supplicant over D-Bus. See
//!   [`wifi_direct`]; the code paths in [`session`] are gated to match.
//!
//! Either way, WFD negotiation is RTSP M1 through M7 ([`rtsp_server`]) with
//! video parameters from [`wfd_params`], after which H.264 is streamed as
//! MPEG2-TS over RTP/UDP.
//!
//! Note the role inversion: once connected, the **source is the RTSP server**,
//! listening for the sink to connect to it.

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
