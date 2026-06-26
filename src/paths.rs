// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Filesystem locations and their permission discipline (macOS).
//!
//! The home directory is resolved from the password database via
//! `getpwuid(getuid())` rather than `$HOME`: the process may be launched in an
//! environment where `$HOME` is unset or spoofed, yet the state and credential
//! files this tool manages must land in the real user's home. Directories are
//! created `0700` and files `0600`, and every directory we create is asserted
//! to be owned by the current uid before use.

use std::ffi::{CStr, OsString};
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// `0700` — owner `rwx`, nothing for group/other.
const DIR_MODE: u32 = 0o700;
/// `0600` — owner `rw`, nothing for group/other.
const FILE_MODE: u32 = 0o600;
/// Application name segment used in every derived path.
const APP: &str = "sessiometer";

/// The current real user id (`getuid(2)`).
fn current_uid() -> u32 {
    // SAFETY: `getuid` cannot fail and has no preconditions.
    unsafe { libc::getuid() }
}

/// Resolve the current user's home directory from the password database.
///
/// Uses `getpwuid(getuid())` and copies `pw_dir` out immediately; the `$HOME`
/// environment variable is intentionally ignored.
fn home_dir() -> Result<PathBuf> {
    let uid = current_uid();
    // SAFETY: `getpwuid` returns a pointer into a static buffer owned by libc.
    // The process is single-threaded at startup (the only caller path), makes
    // no other `getpw*` call before reading the result, and copies `pw_dir`
    // into an owned `OsString` before the pointer can be invalidated.
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return Err(Error::HomeUnresolved);
        }
        let dir = (*pw).pw_dir;
        if dir.is_null() {
            return Err(Error::HomeUnresolved);
        }
        let bytes = CStr::from_ptr(dir).to_bytes().to_vec();
        if bytes.is_empty() {
            return Err(Error::HomeUnresolved);
        }
        Ok(PathBuf::from(OsString::from_vec(bytes)))
    }
}

/// Pure derivation of the config directory, so the env/home policy is testable
/// without touching process-global state.
fn config_dir_from(home: &Path, xdg_config_home: Option<OsString>) -> PathBuf {
    match xdg_config_home {
        Some(xdg) if !xdg.is_empty() => Path::new(&xdg).join(APP),
        _ => home.join("Library/Application Support").join(APP),
    }
}

/// The config directory: `$XDG_CONFIG_HOME/sessiometer` if that variable is
/// set and non-empty, otherwise `~/Library/Application Support/sessiometer`.
pub(crate) fn config_dir() -> Result<PathBuf> {
    Ok(config_dir_from(
        &home_dir()?,
        std::env::var_os("XDG_CONFIG_HOME"),
    ))
}

/// The config file: `<config_dir>/config.toml` — the daemon's source of truth
/// (roster + tunables), read at start and written by `capture` (issue #3).
pub(crate) fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// The log directory: `~/Library/Logs/sessiometer`.
pub(crate) fn logs_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join("Library/Logs").join(APP))
}

/// The login keychain file: `~/Library/Keychains/login.keychain-db`.
///
/// Where Claude Code stores its `Claude Code-credentials` item (the legacy
/// file-based keychain, confirmed in `build/version-compat.md`). Every keychain
/// operation pins this path explicitly via the `security` CLI — it keeps the
/// item on the classic-ACL path (issue #2).
pub(crate) fn login_keychain() -> Result<PathBuf> {
    Ok(home_dir()?.join("Library/Keychains/login.keychain-db"))
}

/// Create `path` (and any missing parents) `0700` and assert it is owned by the
/// current uid. Idempotent: if the directory already exists it re-tightens the
/// mode and re-checks ownership.
pub(crate) fn ensure_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, Permissions::from_mode(DIR_MODE))?;
    if fs::metadata(path)?.uid() != current_uid() {
        return Err(Error::ForeignOwnership(path.to_path_buf()));
    }
    Ok(())
}

/// Open (creating if needed, then append) `path` with `0600` permissions. The
/// mode is applied only when the file is created; an existing file keeps its
/// permissions (standard Unix `open` semantics).
pub(crate) fn create_private_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(FILE_MODE)
        .open(path)?;
    Ok(file)
}

/// Atomically (over)write `path` with `contents`, leaving it `0600`.
///
/// Writes a sibling `<path>.tmp` (created fresh `0600`), `fsync`s it, then
/// renames it over `path`. The rename is atomic within the directory, so a
/// concurrent reader (the daemon loading config) never observes a half-written
/// file, and `path` ends up `0600` regardless of any prior mode — unlike
/// [`create_private_file`], whose mode applies only on creation. The parent
/// directory must already exist and be private (caller runs
/// [`ensure_private_dir`] first).
pub(crate) fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);

    // A stale temp from a prior crashed write would make `create_new` fail;
    // remove it best-effort so we always start from a fresh `0600` file.
    let _ = fs::remove_file(&tmp);
    {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(FILE_MODE)
            .open(&tmp)?;
        file.write_all(contents)?;
        // Durable before the rename, so a crash can't leave an empty config in
        // place of the old one.
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_prefers_xdg_when_set() {
        let got = config_dir_from(Path::new("/Users/x"), Some(OsString::from("/cfg")));
        assert_eq!(got, PathBuf::from("/cfg/sessiometer"));
    }

    #[test]
    fn config_dir_falls_back_to_library_when_xdg_unset() {
        let got = config_dir_from(Path::new("/Users/x"), None);
        assert_eq!(
            got,
            PathBuf::from("/Users/x/Library/Application Support/sessiometer")
        );
    }

    #[test]
    fn config_dir_falls_back_when_xdg_empty() {
        let got = config_dir_from(Path::new("/Users/x"), Some(OsString::new()));
        assert_eq!(
            got,
            PathBuf::from("/Users/x/Library/Application Support/sessiometer")
        );
    }

    #[test]
    fn ensure_private_dir_sets_0700_and_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested/private");
        ensure_private_dir(&dir).unwrap();

        let meta = fs::metadata(&dir).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, DIR_MODE);
        assert_eq!(meta.uid(), current_uid());
    }

    #[test]
    fn create_private_file_is_0600() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state");
        let _file = create_private_file(&path).unwrap();

        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, FILE_MODE);
    }

    #[test]
    fn write_private_file_writes_contents_0600() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        write_private_file(&path, b"hello = 1\n").unwrap();

        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, FILE_MODE);
        assert_eq!(fs::read(&path).unwrap(), b"hello = 1\n");
        // No temp file left behind.
        assert!(!tmp.path().join("config.toml.tmp").exists());
    }

    #[test]
    fn write_private_file_overwrites_and_stays_0600() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        write_private_file(&path, b"first").unwrap();
        // Loosen the mode to prove the second write re-tightens it (the rename
        // installs the fresh 0600 temp, regardless of the old file's mode).
        fs::set_permissions(&path, Permissions::from_mode(0o644)).unwrap();

        write_private_file(&path, b"second").unwrap();

        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, FILE_MODE);
        assert_eq!(fs::read(&path).unwrap(), b"second");
    }
}
