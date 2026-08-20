//! rustls configuration for the OpenPlay signaling channel.
//!
//! The receiver's certificate is self-signed and its address is a bare LAN IP,
//! so neither end can use webpki's usual path: there is no CA to chain to and
//! no hostname to match. Instead the sender pins the SHA-256 fingerprint of the
//! receiver's certificate, which mDNS already advertises in the `fp` TXT key.
//!
//! # What pinning does and does not buy
//!
//! The TXT record is unauthenticated, so an attacker on the same LAN can
//! advertise a receiver with their own fingerprint and a sender that has never
//! seen the real one will pin the attacker's certificate. Pinning therefore
//! gives confidentiality against a passive eavesdropper, and detects a
//! substituted certificate on any *later* connection, but it is not by itself
//! authentication of the receiver. That requires the user to confirm a code
//! shown on both screens — which is what the `PairingChallenge` /
//! `PairingConfirm` messages in `openplay-protocol` are for. They are not
//! wired up yet.

use anyhow::{anyhow, Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, DigitallySignedStruct, Error as TlsError, ServerConfig, SignatureScheme,
};
use std::sync::Arc;

use crate::{certificate_fingerprint, CertificateManager};

impl CertificateManager {
    /// Builds a rustls [`ServerConfig`] presenting this manager's certificate.
    ///
    /// Client certificates are not requested: the sender is authenticated by the
    /// pairing exchange, not by TLS.
    pub fn server_config(&self) -> Result<Arc<ServerConfig>> {
        let certs = rustls_pemfile::certs(&mut self.cert_pem().as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to parse certificate PEM")?;
        if certs.is_empty() {
            return Err(anyhow!("Certificate PEM contained no certificates"));
        }

        let key = rustls_pemfile::private_key(&mut self.key_pem().as_bytes())
            .context("Failed to parse private key PEM")?
            .ok_or_else(|| anyhow!("Private key PEM contained no key"))?;

        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("Failed to build rustls ServerConfig")?;

        Ok(Arc::new(config))
    }
}

/// Builds a rustls [`ClientConfig`] that accepts exactly one certificate: the
/// one whose SHA-256 fingerprint matches `expected_fingerprint`.
///
/// `expected_fingerprint` is the value from the receiver's mDNS `fp` TXT key.
/// Comparison ignores case and separators, so `A3:B2:...`, `a3b2...` and
/// `a3-b2-...` are equivalent.
///
/// See the module documentation for what this does not protect against.
pub fn client_config_pinned(expected_fingerprint: &str) -> Result<Arc<ClientConfig>> {
    let normalised = normalise_fingerprint(expected_fingerprint);
    if normalised.len() != 64 {
        return Err(anyhow!(
            "Expected a 64-hex-digit SHA-256 fingerprint, got {} digits",
            normalised.len()
        ));
    }

    let provider = CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));

    let verifier = PinnedCertVerifier {
        expected: normalised,
        algorithms: provider.signature_verification_algorithms,
    };

    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("Failed to select TLS protocol versions")?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();

    Ok(Arc::new(config))
}

/// Lowercases and strips separators so fingerprints compare by value.
fn normalise_fingerprint(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// A [`ServerCertVerifier`] that accepts one specific certificate by fingerprint.
///
/// Signature verification is still delegated to the crypto provider — pinning
/// replaces *identity* checking only. A pinned certificate that cannot sign the
/// handshake is still rejected.
#[derive(Debug)]
struct PinnedCertVerifier {
    expected: String,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let presented = normalise_fingerprint(&certificate_fingerprint(end_entity.as_ref()));

        if presented == self.expected {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(TlsError::General(format!(
                "certificate fingerprint mismatch: receiver presented {presented}, \
                 mDNS advertised {}",
                self.expected
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives the verifier directly, which is the decision pinning actually makes.
    fn verify_against(pinned: &str, cert_der: &[u8]) -> Result<ServerCertVerified, TlsError> {
        let provider = CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
        let verifier = PinnedCertVerifier {
            expected: normalise_fingerprint(pinned),
            algorithms: provider.signature_verification_algorithms,
        };
        verifier.verify_server_cert(
            &CertificateDer::from(cert_der.to_vec()),
            &[],
            &ServerName::try_from("192.168.1.10").unwrap(),
            &[],
            UnixTime::now(),
        )
    }

    #[test]
    fn server_config_builds_from_a_generated_certificate() {
        let mgr = CertificateManager::generate().unwrap();
        assert!(mgr.server_config().is_ok());
    }

    #[test]
    fn verifier_accepts_the_pinned_certificate() {
        let mgr = CertificateManager::generate().unwrap();
        assert!(verify_against(mgr.fingerprint(), mgr.cert_der()).is_ok());
    }

    #[test]
    fn verifier_rejects_a_different_certificate() {
        let receiver = CertificateManager::generate().unwrap();
        let impostor = CertificateManager::generate().unwrap();
        assert_ne!(receiver.fingerprint(), impostor.fingerprint());

        // Sender pinned the real receiver; an impostor answers instead.
        let err = verify_against(receiver.fingerprint(), impostor.cert_der()).unwrap_err();
        assert!(
            err.to_string().contains("fingerprint mismatch"),
            "expected a mismatch error, got {err}"
        );
    }

    #[test]
    fn fingerprint_comparison_ignores_case_and_separators() {
        let mgr = CertificateManager::generate().unwrap();
        let colonned = mgr.fingerprint().to_string();
        let bare = colonned.replace(':', "");

        assert!(verify_against(&colonned, mgr.cert_der()).is_ok());
        assert!(verify_against(&bare, mgr.cert_der()).is_ok());
        assert!(verify_against(&bare.to_lowercase(), mgr.cert_der()).is_ok());
    }

    #[test]
    fn client_config_rejects_a_malformed_fingerprint() {
        assert!(client_config_pinned("not-a-fingerprint").is_err());
        assert!(client_config_pinned("").is_err());
        // 63 digits: one short of SHA-256.
        assert!(client_config_pinned(&"a".repeat(63)).is_err());
    }

    #[test]
    fn client_config_accepts_the_advertised_form() {
        let mgr = CertificateManager::generate().unwrap();
        assert!(client_config_pinned(mgr.fingerprint()).is_ok());
    }
}
