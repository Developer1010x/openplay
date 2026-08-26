//! Self-signed certificate lifecycle for securing OpenPlay connections.
//!
//! [`CertificateManager`] generates, persists, loads
//! and fingerprints a self-signed ECDSA P-256 certificate and key.
//!
//! **Nothing constructs this outside its own tests yet.** It is the identity the
//! WebRTC signaling path is designed to use, and that path is not yet connected.
//! See `docs/crypto.md`.

mod certs;
mod tls;

pub use certs::CertificateManager;
pub use tls::client_config_pinned;

use sha2::{Digest, Sha256};

/// Computes a SHA-256 fingerprint of DER-encoded certificate bytes.
/// Returns a colon-separated hex string like "A3:B2:C1:...".
pub fn certificate_fingerprint(der_bytes: &[u8]) -> String {
    let hash = Sha256::digest(der_bytes);
    hash.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint_format() {
        let fake_cert = b"fake certificate data for testing";
        let fp = certificate_fingerprint(fake_cert);
        // Should be 32 bytes = 64 hex chars + 31 colons = 95 chars total
        assert_eq!(fp.len(), 95);
        assert!(fp.contains(':'));
        // Each segment should be 2 hex chars
        for segment in fp.split(':') {
            assert_eq!(segment.len(), 2);
            assert!(segment.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
}
