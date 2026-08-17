pub mod fairplay;
pub mod features;
pub mod hap_pairing;
pub mod http_session;
pub mod mirror_header;
pub mod mirror_stream;
pub mod ntp;
pub mod session;
pub mod srp;
pub mod tlv8;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum AirPlayError {
    #[error("Connection failed: {0}")]
    Connection(String),

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("NTP server error: {0}")]
    Ntp(String),

    #[error("Mirror stream error: {0}")]
    MirrorStream(String),

    #[error("Receiver does not support mirroring")]
    MirroringNotSupported,

    #[error("Negotiation failed: {0}")]
    Negotiation(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Plist error: {0}")]
    Plist(String),

    #[error("FairPlay error: {0}")]
    FairPlay(String),

    #[error("Pairing error: {0}")]
    Pairing(String),

    #[error("Session closed")]
    SessionClosed,
}
