use anyhow::{Context, Result};
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
use std::path::{Path, PathBuf};
use tracing::{debug, info};

#[cfg(unix)]
use tracing::warn;

use crate::certificate_fingerprint;

const CERT_FILENAME: &str = "openplay.crt.pem";
const KEY_FILENAME: &str = "openplay.key.pem";

/// Owner-only mode for the private key file.
///
/// The certificate fingerprint is the only identity in the whole system, so a
/// copy of this key is a complete impersonation of the receiver. No group bits,
/// no other bits, at any instant — not even the instant between creating the
/// file and writing to it.
#[cfg(unix)]
const KEY_MODE: u32 = 0o600;

/// Owner-only mode for the data directory.
///
/// A directory other users can write is a directory in which the key file can
/// be replaced by a symlink before it is ever created, so the directory's mode
/// carries part of the key's protection. This also matches the XDG Base
/// Directory spec, which asks for 0700 on a data directory created on demand.
#[cfg(unix)]
const DATA_DIR_MODE: u32 = 0o700;

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
        create_data_dir(data_dir)?;

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

        // A key this process did not write — restored from a backup, unpacked
        // from an archive, or left behind by an older build — can be sitting at
        // 0644. Check before using it, or an exposed identity is reused
        // silently for the life of the install.
        enforce_key_permissions(key_path)?;

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
        // The certificate is public — it is handed to every peer that connects
        // — so the default mode is correct for it.
        std::fs::write(cert_path, &self.cert_pem).context("Failed to write certificate file")?;

        write_key_file(key_path, &self.key_pem)?;

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

/// Creates `data_dir`, owner-only where the platform has modes.
///
/// On Unix the mode applies to every component this call creates. They are all
/// inside the user's own data home, so 0700 is right for each of them.
///
/// On Windows the directory inherits its parent's ACL, which under
/// `%LOCALAPPDATA%` already grants the user alone. No owner-only ACL is set
/// explicitly: doing that correctly needs the `windows` crate's security APIs
/// and this crate does not depend on them. Saying so is better than implying a
/// guarantee that is not there.
fn create_data_dir(data_dir: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(DATA_DIR_MODE);
    }

    builder
        .create(data_dir)
        .with_context(|| format!("Failed to create data dir: {}", data_dir.display()))
}

/// Opens `path` truncated for writing, readable only by its owner *first*.
///
/// The ordering is the whole point. `std::fs::write` creates with
/// `0o666 & !umask` — 0644 under the usual umask — so a chmod afterwards leaves
/// a window in which any local user can open the key, and a descriptor opened
/// in that window survives the chmod that follows. Handing the mode to `open`
/// closes the window on creation; the `set_permissions` below closes it for a
/// file that already existed, and both happen before the caller writes a byte.
///
/// `create_new` would be stronger still, since `O_EXCL` refuses to follow a
/// symlink someone else planted at this path. It is not usable here: this path
/// must also overwrite the key a previous run wrote, and exclusive creation
/// fails on exactly that. [`create_data_dir`] denies other users write access
/// to the directory, which closes the same hole from the other side.
///
/// On Windows the file inherits the directory's ACL and no owner-only ACL is
/// set — see [`create_data_dir`] for why.
fn open_private_file(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(KEY_MODE);
    }

    let file = options
        .open(path)
        .with_context(|| format!("Failed to open {} for writing", path.display()))?;

    // `mode` above is consulted only when this call creates the file. A key
    // restored from a backup at 0644 keeps that mode through `open`, so tighten
    // the descriptor itself — fchmod, no second path lookup, nothing to race.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(KEY_MODE))
            .with_context(|| format!("Failed to restrict permissions on {}", path.display()))?;
    }

    Ok(file)
}

/// Writes the private key without ever leaving it readable by anyone else.
fn write_key_file(path: &Path, key_pem: &str) -> Result<()> {
    use std::io::Write;

    let mut file = open_private_file(path)?;
    file.write_all(key_pem.as_bytes())
        .with_context(|| format!("Failed to write key file: {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("Failed to flush key file: {}", path.display()))
}

/// Tightens a key file other users can read, and says loudly that it happened.
///
/// Repairing rather than refusing keeps a working receiver working — a refusal
/// would strand a user whose only fault was restoring a backup, with no
/// recovery but a manual `chmod`. The warning carries the part a chmod cannot:
/// the key may already have been copied, and only regenerating it fixes that.
#[cfg(unix)]
fn enforce_key_permissions(key_path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::metadata(key_path)
        .with_context(|| format!("Failed to stat key file: {}", key_path.display()))?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 == 0 {
        return Ok(());
    }

    warn!(
        path = %key_path.display(),
        mode = %format!("{mode:04o}"),
        "Private key is accessible to other users. Tightening it to 0600, but \
         it may already have been copied — anyone holding a copy can impersonate \
         this receiver. Delete the key and restart OpenPlay to generate a new \
         identity if this machine has other users."
    );

    std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(KEY_MODE))
        .with_context(|| format!("Failed to restrict permissions on {}", key_path.display()))
}

/// Windows has no mode bits to check; see [`create_data_dir`].
#[cfg(not(unix))]
fn enforce_key_permissions(_key_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// The permission bits of an existing path.
    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path)
            .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
            .permissions()
            .mode()
            & 0o777
    }

    /// Leaves a file at `mode`, standing in for a backup restore, an unpacked
    /// archive, or a key written by an older build of OpenPlay.
    #[cfg(unix)]
    fn place_file_with_mode(path: &Path, contents: &str, mode: u32) {
        std::fs::write(path, contents).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(mode_of(path), mode, "test setup did not take effect");
    }

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

    #[cfg(unix)]
    #[test]
    fn test_key_file_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let _mgr = CertificateManager::load_or_generate(tmp.path()).unwrap();

        let key_path = tmp.path().join(KEY_FILENAME);
        let mode = mode_of(&key_path);

        assert_eq!(
            mode & 0o077,
            0,
            "the key is readable or writable outside its owner: {mode:04o}"
        );
        assert_eq!(mode, 0o600, "the key should be exactly 0600");
    }

    /// The mode the key file is *created* with, read off the descriptor before
    /// a single byte of key material has gone through it.
    ///
    /// This is the instant the old write-then-chmod code left the key at 0644.
    /// A local user watching the directory could open it there and keep that
    /// descriptor across the chmod that followed, so the chmod revoked nothing.
    /// Asserting the *final* mode cannot catch that — the old code reached 0600
    /// too. Asserting it on an empty, freshly created file is what pins the
    /// ordering, and it does so deterministically, with no race to lose.
    #[cfg(unix)]
    #[test]
    fn the_key_file_is_owner_only_before_any_key_material_reaches_it() {
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join(KEY_FILENAME);

        let file = open_private_file(&key_path).unwrap();
        let meta = file.metadata().unwrap();

        assert_eq!(meta.len(), 0, "nothing should have been written yet");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "the key file must be private from the moment it exists, not from \
             the moment a later chmod runs"
        );
    }

    /// `mode` on `open` only applies when `open` creates the file, so a key
    /// left at 0644 by something else must be tightened on the descriptor
    /// before the new key is written into it.
    #[cfg(unix)]
    #[test]
    fn a_pre_existing_world_readable_key_is_tightened_before_it_is_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join(KEY_FILENAME);
        place_file_with_mode(&key_path, "stale key material\n", 0o644);

        let file = open_private_file(&key_path).unwrap();
        let meta = file.metadata().unwrap();

        assert_eq!(meta.len(), 0, "the stale contents should be gone");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "an inherited 0644 must not survive into the file the new key is \
             written to"
        );
    }

    /// Loading is the path that ran forever without ever looking at the mode: a
    /// key exposed once stayed exposed for the life of the install.
    #[cfg(unix)]
    #[test]
    fn loading_a_world_readable_key_repairs_it_and_keeps_the_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let fingerprint = CertificateManager::load_or_generate(tmp.path())
            .unwrap()
            .fingerprint()
            .to_string();

        let key_path = tmp.path().join(KEY_FILENAME);
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let reloaded = CertificateManager::load_or_generate(tmp.path()).unwrap();

        assert_eq!(
            reloaded.fingerprint(),
            fingerprint,
            "repairing the mode must not change the device identity"
        );
        assert_eq!(
            mode_of(&key_path),
            0o600,
            "a key found group/world readable must not be left that way"
        );
    }

    /// A 0600 key inside a directory anyone can write is not protected: the
    /// file can be swapped for a symlink before it is created.
    #[cfg(unix)]
    #[test]
    fn the_data_dir_is_created_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        // Nested and absent, so this exercises creation rather than whatever
        // mode `tempfile` happens to give the directory it makes itself.
        let data_dir = tmp.path().join("state").join("openplay");

        CertificateManager::load_or_generate(&data_dir).unwrap();

        assert_eq!(mode_of(&data_dir), 0o700);
        assert_eq!(
            mode_of(data_dir.parent().unwrap()),
            0o700,
            "every component created on the way is inside the user's own data \
             home, so each one gets the same mode"
        );
    }
}
