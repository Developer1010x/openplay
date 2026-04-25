use openplay_crypto::{certificate_fingerprint, CertificateManager};

#[test]
fn generate_produces_valid_pem() {
    let mgr = CertificateManager::generate().unwrap();
    assert!(mgr.cert_pem().contains("-----BEGIN CERTIFICATE-----"));
    assert!(mgr.cert_pem().contains("-----END CERTIFICATE-----"));
    assert!(mgr.key_pem().contains("-----BEGIN PRIVATE KEY-----"));
    assert!(mgr.key_pem().contains("-----END PRIVATE KEY-----"));
}

#[test]
fn generate_produces_non_empty_der() {
    let mgr = CertificateManager::generate().unwrap();
    assert!(!mgr.cert_der().is_empty());
}

#[test]
fn fingerprint_is_sha256_format() {
    let mgr = CertificateManager::generate().unwrap();
    let fp = mgr.fingerprint();
    // SHA-256 = 32 bytes = 64 hex chars + 31 colons = 95 chars
    assert_eq!(fp.len(), 95);
    for segment in fp.split(':') {
        assert_eq!(segment.len(), 2, "each segment must be 2 hex chars");
        assert!(segment.chars().all(|c| c.is_ascii_hexdigit()));
    }
}

#[test]
fn two_generated_certs_have_different_fingerprints() {
    let a = CertificateManager::generate().unwrap();
    let b = CertificateManager::generate().unwrap();
    assert_ne!(a.fingerprint(), b.fingerprint());
}

#[test]
fn load_or_generate_persists_cert() {
    let dir = tempfile::tempdir().unwrap();
    let mgr1 = CertificateManager::load_or_generate(dir.path()).unwrap();
    let fp1 = mgr1.fingerprint().to_string();

    let mgr2 = CertificateManager::load_or_generate(dir.path()).unwrap();
    assert_eq!(fp1, mgr2.fingerprint(), "reloaded cert must match original");
}

#[test]
fn load_or_generate_creates_both_files() {
    let dir = tempfile::tempdir().unwrap();
    CertificateManager::load_or_generate(dir.path()).unwrap();

    assert!(CertificateManager::cert_path(dir.path()).exists());
    assert!(CertificateManager::key_path(dir.path()).exists());
}

#[test]
fn fingerprint_stable_across_reload() {
    let dir = tempfile::tempdir().unwrap();
    let fp1 = CertificateManager::load_or_generate(dir.path())
        .unwrap()
        .fingerprint()
        .to_string();
    let fp2 = CertificateManager::load_or_generate(dir.path())
        .unwrap()
        .fingerprint()
        .to_string();
    assert_eq!(fp1, fp2);
}

#[test]
fn fingerprint_function_format() {
    let data = b"hello openplay";
    let fp = certificate_fingerprint(data);
    assert_eq!(fp.len(), 95);
    assert!(fp.contains(':'));
}

#[test]
fn fingerprint_is_deterministic() {
    let data = b"test bytes";
    assert_eq!(certificate_fingerprint(data), certificate_fingerprint(data));
}

#[test]
fn fingerprint_differs_for_different_input() {
    assert_ne!(
        certificate_fingerprint(b"input-a"),
        certificate_fingerprint(b"input-b")
    );
}

#[cfg(unix)]
#[test]
fn key_file_is_owner_read_write_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    CertificateManager::load_or_generate(dir.path()).unwrap();

    let key_path = CertificateManager::key_path(dir.path());
    let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "key file must be 0600");
}
