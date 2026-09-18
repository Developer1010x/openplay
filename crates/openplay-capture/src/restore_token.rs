//! Where the portal's screencast *restore token* lives between runs.
//!
//! When `select_sources` asks the XDG Desktop Portal to remember the user's
//! choice, the `Start` response carries an opaque token. Handing that token
//! back on the next session lets the portal skip its screen-selection dialog
//! and reuse the same screen. Without it, every cast begins with the picker.
//!
//! The token is tied to this application by the portal's permission store and
//! means nothing to anyone else, but it is still the thing that lets a capture
//! begin without a dialog, so it is written owner-only like the rest of the
//! data directory. If the portal ever rejects it (revoked, expired, a different
//! compositor), the portal shows the picker again and returns a fresh token,
//! which overwrites this one — a stale token costs one dialog, never a failure.
#![cfg(target_os = "linux")]

use std::fs;
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// File name inside the data directory.
const FILE_NAME: &str = "screencast-restore-token";

/// Where the token is kept: `$XDG_DATA_HOME/openplay/screencast-restore-token`.
pub fn path() -> PathBuf {
    openplay_common::data_dir().join(FILE_NAME)
}

/// The remembered token, if there is one.
pub fn load() -> Option<String> {
    load_from(&path())
}

/// Remembers `token` for the next session.
pub fn save(token: &str) -> io::Result<()> {
    save_to(&path(), token)
}

/// Forgets the remembered token. `Ok(true)` if there was one to forget.
pub fn clear() -> io::Result<bool> {
    clear_at(&path())
}

fn load_from(path: &Path) -> Option<String> {
    let token = fs::read_to_string(path).ok()?;
    let token = token.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

fn save_to(path: &Path, token: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(token.as_bytes())
}

fn clear_at(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(FILE_NAME);
        (dir, path)
    }

    #[test]
    fn a_saved_token_is_loaded_back() {
        let (_dir, path) = scratch();
        save_to(&path, "abc-123").unwrap();
        assert_eq!(load_from(&path).as_deref(), Some("abc-123"));
    }

    #[test]
    fn saving_creates_the_directory_and_keeps_the_file_private() {
        let (_dir, path) = scratch();
        save_to(&path, "t").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file must be owner-only, got {mode:o}");
    }

    #[test]
    fn a_missing_or_blank_file_means_no_token() {
        let (_dir, path) = scratch();
        assert_eq!(load_from(&path), None, "missing file");
        save_to(&path, "  \n").unwrap();
        assert_eq!(load_from(&path), None, "blank file");
    }

    #[test]
    fn clearing_reports_whether_there_was_anything_to_clear() {
        let (_dir, path) = scratch();
        assert!(!clear_at(&path).unwrap(), "nothing there yet");
        save_to(&path, "t").unwrap();
        assert!(clear_at(&path).unwrap(), "removed the token");
        assert_eq!(load_from(&path), None);
        assert!(!clear_at(&path).unwrap(), "already gone");
    }

    #[test]
    fn saving_again_overwrites_rather_than_appends() {
        let (_dir, path) = scratch();
        save_to(&path, "first-token-longer").unwrap();
        save_to(&path, "second").unwrap();
        assert_eq!(load_from(&path).as_deref(), Some("second"));
    }
}
