use anyhow::{Context, Result};
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
use std::path::{Path, PathBuf};
use tracing::{debug, info};

use crate::certificate_fingerprint;

const CERT_FILENAME: &str = "openplay.crt.pem";
const KEY_FILENAME: &str = "openplay.key.pem";

/// Manages TLS certificates for OpenPlay.
///
/// On first run, generates a self-signed ECDSA P-256 certificate and persists it.
/// On subsequent runs, loads the existing certificate from disk.
pub struct CertificateManager {
    cert_pem: String,
    key_pem: String,
    cert_der: Vec<u8>,
    fingerprint: String,
}

impl CertificateManager {
    /// Loads existing certificate from `data_dir` or generates a new one.
    pub fn load_or_generate(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("Failed to create data dir: {}", data_dir.display()))?;

        let cert_path = data_dir.join(CERT_FILENAME);
        let key_path = data_dir.join(KEY_FILENAME);

        if cert_path.exists() && key_path.exists() {
            debug!("Loading existing certificate from {}", cert_path.display());
            Self::load_from_files(&cert_path, &key_path)
        } else {
            info!("Generating new ECDSA P-256 certificate");
            let manager = Self::generate()?;
            manager.save_to_files(&cert_path, &key_path)?;
            Ok(manager)
        }
    }

    /// Generates a fresh self-signed ECDSA P-256 certificate.
    pub fn generate() -> Result<Self> {
        let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
            .context("Failed to generate ECDSA key pair")?;

        let mut params = CertificateParams::new(vec!["openplay.local".to_string()])
            .context("Failed to create certificate params")?;
        params.not_before = rcgen::date_time_ymd(2024, 1, 1);
        params.not_after = rcgen::date_time_ymd(2034, 1, 1);
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "OpenPlay Device");

        let cert = params
            .self_signed(&key_pair)
            .context("Failed to sign certificate")?;

        let cert_pem = cert.pem();
        let key_pem = key_pair.serialize_pem();
        let cert_der = cert.der().to_vec();
        let fingerprint = certificate_fingerprint(&cert_der);

        info!(fingerprint = %fingerprint, "Generated new certificate");

        Ok(Self {
            cert_pem,
            key_pem,
            cert_der,
            fingerprint,
        })
    }

    fn load_from_files(cert_path: &Path, key_path: &Path) -> Result<Self> {
        let cert_pem =
            std::fs::read_to_string(cert_path).context("Failed to read certificate file")?;
        let key_pem = std::fs::read_to_string(key_path).context("Failed to read key file")?;

        // Parse the PEM to extract DER for fingerprinting
        let cert_der = {
            let mut reader = std::io::BufReader::new(cert_pem.as_bytes());
            let certs = rustls_pemfile::certs(&mut reader)
                .collect::<Result<Vec<_>, _>>()
                .context("Failed to parse certificate PEM")?;
            certs
                .into_iter()
                .next()
                .context("No certificates found in PEM file")?
                .to_vec()
        };

        let fingerprint = certificate_fingerprint(&cert_der);
        debug!(fingerprint = %fingerprint, "Loaded existing certificate");

        Ok(Self {
            cert_pem,
            key_pem,
            cert_der,
            fingerprint,
        })
    }

    fn save_to_files(&self, cert_path: &Path, key_path: &Path) -> Result<()> {
        std::fs::write(cert_path, &self.cert_pem).context("Failed to write certificate file")?;

        // Set restrictive permissions on the key file
        std::fs::write(key_path, &self.key_pem).context("Failed to write key file")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))?;
        }

        info!(
            cert = %cert_path.display(),
            key = %key_path.display(),
            "Saved certificate and key"
        );
        Ok(())
    }

    /// Returns the certificate in PEM format.
    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// Returns the private key in PEM format.
    pub fn key_pem(&self) -> &str {
        &self.key_pem
    }

    /// Returns the certificate in DER format.
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// Returns the SHA-256 fingerprint of the certificate.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Returns the path where the certificate is stored.
    pub fn cert_path(data_dir: &Path) -> PathBuf {
        data_dir.join(CERT_FILENAME)
    }

    /// Returns the path where the key is stored.
    pub fn key_path(data_dir: &Path) -> PathBuf {
        data_dir.join(KEY_FILENAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_generate_certificate() {
        let mgr = CertificateManager::generate().unwrap();
        assert!(!mgr.cert_pem().is_empty());
        assert!(!mgr.key_pem().is_empty());
        assert!(!mgr.cert_der().is_empty());
        assert!(!mgr.fingerprint().is_empty());
        assert!(mgr.cert_pem().contains("BEGIN CERTIFICATE"));
        assert!(mgr.key_pem().contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn test_load_or_generate_creates_files() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = CertificateManager::load_or_generate(tmp.path()).unwrap();
        let fp1 = mgr.fingerprint().to_string();

        // Loading again should return the same cert
        let mgr2 = CertificateManager::load_or_generate(tmp.path()).unwrap();
        assert_eq!(fp1, mgr2.fingerprint());
    }

    #[test]
    fn test_key_file_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let _mgr = CertificateManager::load_or_generate(tmp.path()).unwrap();

        let key_path = tmp.path().join(KEY_FILENAME);
        let metadata = fs::metadata(&key_path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
