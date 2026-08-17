//! AirPlay sender protocol.
//!
//! Covers the HTTP/plist session layer, feature negotiation, TLV8, NTP timing,
//! the mirror stream, HAP pairing (SRP-6a over the RFC 5054 3072-bit group with
//! SHA-512, then Ed25519/X25519 and ChaCha20-Poly1305), and FairPlay.
//!
//! # Interoperability status
//!
//! Read `docs/crypto.md` before debugging a receiver that rejects a connection.
//!
//! - **HAP pairing** used a fabricated SRP group and could never succeed. That
//!   is fixed — see [`srp`] — but is **not confirmed against physical Apple
//!   hardware**.
//! - **FairPlay** still derives its AES key from an invented seed and cannot
//!   interoperate. Receivers that require it, such as Apple TV 3rd gen, will
//!   reject the handshake. See [`fairplay`].

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
