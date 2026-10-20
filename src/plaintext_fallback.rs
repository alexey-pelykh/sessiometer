// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Claude Code's plaintext credential fallback — `<config dir>/.credentials.json` — and what
//! sessiometer does about it after each canonical write (issue #1540).
//!
//! Claude Code's credential store is keychain-PRIMARY with a plaintext FALLBACK
//! (`build/version-compat.md`, the AC-6 finding): it reads the `Claude Code-credentials` item and
//! falls back to the file only when the keychain returns nothing, and it writes the file only when
//! a keychain write fails non-transiently. On macOS the file is therefore the leftover of a failed
//! write, never the live credential while the keychain item exists.
//!
//! A leftover is not harmless. Claude Code's per-request "did the credentials change?" check is
//! file-FIRST (observed on 2.1.285): while the file exists it compares only the file's mtime and
//! never looks at the keychain, so a swap — which rewrites only the keychain item — is invisible
//! to every RUNNING session until its in-memory token fails. And if the keychain item is ever
//! removed (the ADR-0018 scrub), sessions fall back to the file's stale login.
//!
//! [`settle`] acts on the file per [`PlaintextFallbackMode`] without ever reading it: it stats,
//! removes, or re-stamps the path, nothing more.
//!
//! macOS only. On Linux the same file IS the canonical store (#26), so the host wiring resolves no
//! path there and nothing in this module runs.

use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::error::Result;
use crate::keychain::{Credential, CredentialStore};
use crate::observability::{Event, EventLog, PlaintextFallbackAction};
use crate::paths;

/// The fallback file's name inside Claude Code's config directory.
pub(crate) const FALLBACK_FILE_NAME: &str = ".credentials.json";

/// What a canonical write does about a present fallback file — the
/// `[credential].plaintext_fallback` setting (issue #1540).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PlaintextFallbackMode {
    /// Delete the file. The keychain item sessiometer just wrote is authoritative; the file is a
    /// stale fallback whose only remaining effects are hiding swaps and resurrecting an old login.
    #[default]
    Remove,
    /// Leave the contents alone and bump the mtime, so live sessions re-read storage
    /// (keychain-first) on their next request. The file — and its stale login — stays.
    Touch,
    /// Do nothing beyond logging that the file is present.
    Off,
}

impl PlaintextFallbackMode {
    /// Every config token, in the order the docs and the validation message list them.
    pub(crate) const TOKENS: [&'static str; 3] = ["remove", "touch", "off"];

    /// The config token for this mode — what `Config::render` writes and
    /// [`from_token`](Self::from_token) reads back.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Remove => "remove",
            Self::Touch => "touch",
            Self::Off => "off",
        }
    }

    /// The mode a config token names, or `None` for anything outside [`TOKENS`](Self::TOKENS)
    /// (case-sensitive, like every other enumerated key).
    pub(crate) fn from_token(token: &str) -> Option<Self> {
        match token {
            "remove" => Some(Self::Remove),
            "touch" => Some(Self::Touch),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// The fallback file paired with the canonical item sessiometer addresses, resolved from the
/// same two environment values as the keychain service name (`keychain::canonical_service_from`)
/// so the file and the item can never name different config directories:
///
/// - `CLAUDE_SECURESTORAGE_CONFIG_DIR`, when DEFINED, wins outright — a defined-empty value means
///   the default directory, exactly as Claude Code's own resolver treats it;
/// - otherwise a non-empty `CLAUDE_CONFIG_DIR`;
/// - otherwise `default_dir` (`~/.claude`).
///
/// `None` when the resolved directory is relative (Claude Code would resolve it against ITS
/// working directory, which sessiometer cannot know) or non-ASCII (the keychain side refuses
/// those too — `Error::NonAsciiConfigDir` — so no canonical write can succeed there anyway).
/// Pure: the env read lives with the caller, so every arm is testable without process-global env.
pub(crate) fn fallback_path_from(
    securestorage: Option<&OsStr>,
    config_dir: Option<&OsStr>,
    default_dir: &Path,
) -> Option<PathBuf> {
    let dir = match securestorage {
        Some(s) if s.is_empty() => default_dir,
        Some(s) => Path::new(s),
        None => match config_dir {
            Some(c) if !c.is_empty() => Path::new(c),
            _ => default_dir,
        },
    };
    (dir.is_absolute() && dir.as_os_str().as_encoded_bytes().is_ascii())
        .then(|| dir.join(FALLBACK_FILE_NAME))
}

/// Act on the fallback file at `path` per `mode`, stamping a re-touched file with `now`.
///
/// `None` when nothing is there — the normal state, and silent. Otherwise the action taken, for
/// the caller to log. Something other than a regular file at the path is [`Refused`] in every
/// mode: a symlink could point anywhere, and a directory is not Claude Code's fallback. The file's
/// contents are never read.
///
/// [`Refused`]: PlaintextFallbackAction::Refused
pub(crate) fn settle(
    mode: PlaintextFallbackMode,
    path: &Path,
    now: SystemTime,
) -> Option<PlaintextFallbackAction> {
    // `symlink_metadata`, not `metadata`: a symlink must be seen AS a symlink, never followed.
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => return None,
        Err(_) => return Some(PlaintextFallbackAction::Failed),
    };
    if !meta.file_type().is_file() {
        return Some(PlaintextFallbackAction::Refused);
    }
    let outcome = match mode {
        PlaintextFallbackMode::Off => return Some(PlaintextFallbackAction::Ignored),
        PlaintextFallbackMode::Remove => {
            fs::remove_file(path).map(|()| PlaintextFallbackAction::Removed)
        }
        // Opened read-only: re-stamping needs only ownership, and nothing is ever written to it.
        PlaintextFallbackMode::Touch => fs::File::open(path)
            .and_then(|file| file.set_modified(now))
            .map(|()| PlaintextFallbackAction::Touched),
    };
    match outcome {
        Ok(action) => Some(action),
        // Gone between the stat and the act (Claude Code cleaned it up itself): nothing to report.
        Err(err) if err.kind() == ErrorKind::NotFound => None,
        Err(_) => Some(PlaintextFallbackAction::Failed),
    }
}

/// Where a [`FallbackSettlingStore`] records its [`Event::PlaintextFallback`] lines.
enum LogTarget {
    /// The native event log (`~/Library/Logs/sessiometer/sessiometer.log`), opened per line in
    /// append mode, so it interleaves safely with the daemon's and the CLI's own handle.
    Native,
    /// An explicit log file — the hermetic test path.
    #[cfg(test)]
    At(PathBuf),
}

/// The canonical [`CredentialStore`], plus the plaintext-fallback settlement after every
/// SUCCESSFUL write (issue #1540).
///
/// Wrapping the store rather than each write site is what makes the guarantee total: every
/// production canonical write — a swap, an adopt-target recovery, an activating `login`, keep-warm's
/// promotion — goes through a store built at one of three construction sites, so wrapping there
/// covers all four paths and any written later. Reads and the resolution probe pass straight
/// through. A failed write settles nothing: the keychain item is then not known to hold the
/// credential, so the fallback is not known to be stale.
///
/// Best-effort like the `~/.claude.json` co-write beside it: the canonical write already
/// succeeded, so neither a failed settlement nor a failed log line ever fails the write.
pub(crate) struct FallbackSettlingStore<C> {
    inner: C,
    mode: PlaintextFallbackMode,
    /// The fallback file to settle; `None` disables settlement (no macOS keychain to be primary
    /// to, or a config dir the file cannot be located under).
    path: Option<PathBuf>,
    log: LogTarget,
}

impl<C> FallbackSettlingStore<C> {
    /// Wrap `inner` for this host: the fallback paired with the canonical item this process
    /// addresses, on macOS only — elsewhere the file is Claude Code's PRIMARY store (#26) and must
    /// never be touched, so the wrapper is a pure pass-through.
    pub(crate) fn for_host(inner: C, mode: PlaintextFallbackMode) -> Self {
        Self {
            inner,
            mode,
            path: host_fallback_path(),
            log: LogTarget::Native,
        }
    }

    /// Wrap `inner` against an explicit fallback `path` and event `log` (tests only).
    #[cfg(test)]
    fn at(inner: C, mode: PlaintextFallbackMode, path: Option<PathBuf>, log: PathBuf) -> Self {
        Self {
            inner,
            mode,
            path,
            log: LogTarget::At(log),
        }
    }

    /// Settle the fallback after a successful canonical write, logging any action taken.
    fn settle_after_write(&self) {
        let Some(path) = &self.path else { return };
        let Some(action) = settle(self.mode, path, SystemTime::now()) else {
            return;
        };
        let log = match &self.log {
            LogTarget::Native => EventLog::open(),
            #[cfg(test)]
            LogTarget::At(path) => EventLog::at(path),
        };
        if let Ok(mut log) = log {
            let _ = log.emit(&Event::PlaintextFallback { action });
        }
    }
}

/// The fallback paired with this process's canonical item: macOS only (see
/// [`FallbackSettlingStore::for_host`]), resolved from the same environment the keychain service
/// name is. `cfg!` rather than `#[cfg]` keeps the resolution compiled — and linted — everywhere.
fn host_fallback_path() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let default_dir = paths::claude_config_dir().ok()?;
    fallback_path_from(
        std::env::var_os("CLAUDE_SECURESTORAGE_CONFIG_DIR").as_deref(),
        std::env::var_os("CLAUDE_CONFIG_DIR").as_deref(),
        &default_dir,
    )
}

impl<C: CredentialStore> CredentialStore for FallbackSettlingStore<C> {
    async fn read(&self) -> Result<Credential> {
        self.inner.read().await
    }

    async fn write(&self, credential: &Credential) -> Result<()> {
        self.inner.write(credential).await?;
        self.settle_after_write();
        Ok(())
    }

    async fn probe_resolution(&self) -> Result<()> {
        self.inner.probe_resolution().await
    }
}

// Unix-only: the fixtures use symlinks and permission bits to stage each case.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    fn os(s: &str) -> Option<&OsStr> {
        Some(OsStr::new(s))
    }

    const DEFAULT_DIR: &str = "/Users/someone/.claude";

    fn default_file() -> Option<PathBuf> {
        Some(Path::new(DEFAULT_DIR).join(FALLBACK_FILE_NAME))
    }

    // --- PlaintextFallbackMode tokens --------------------------------------------------------

    #[test]
    fn every_mode_round_trips_through_its_token() {
        for (token, mode) in [
            ("remove", PlaintextFallbackMode::Remove),
            ("touch", PlaintextFallbackMode::Touch),
            ("off", PlaintextFallbackMode::Off),
        ] {
            assert_eq!(mode.as_str(), token);
            assert_eq!(PlaintextFallbackMode::from_token(token), Some(mode));
        }
        assert_eq!(
            PlaintextFallbackMode::TOKENS
                .map(|t| PlaintextFallbackMode::from_token(t).unwrap().as_str()),
            PlaintextFallbackMode::TOKENS
        );
    }

    #[test]
    fn a_token_outside_the_vocabulary_names_no_mode() {
        for bad in ["", "Remove", "delete", "on", " remove"] {
            assert_eq!(PlaintextFallbackMode::from_token(bad), None, "{bad:?}");
        }
    }

    // --- fallback_path_from ------------------------------------------------------------------

    #[test]
    fn both_unset_resolves_the_default_config_dir() {
        assert_eq!(
            fallback_path_from(None, None, Path::new(DEFAULT_DIR)),
            default_file()
        );
    }

    #[test]
    fn a_non_empty_config_dir_relocates_the_file() {
        assert_eq!(
            fallback_path_from(None, os("/opt/cc"), Path::new(DEFAULT_DIR)),
            Some(PathBuf::from("/opt/cc/.credentials.json"))
        );
    }

    #[test]
    fn an_empty_config_dir_is_the_default_like_the_bare_service_name() {
        assert_eq!(
            fallback_path_from(None, os(""), Path::new(DEFAULT_DIR)),
            default_file()
        );
    }

    #[test]
    fn a_defined_securestorage_dir_wins_over_config_dir() {
        assert_eq!(
            fallback_path_from(os("/srv/secure"), os("/opt/cc"), Path::new(DEFAULT_DIR)),
            Some(PathBuf::from("/srv/secure/.credentials.json"))
        );
    }

    #[test]
    fn a_defined_empty_securestorage_dir_means_the_default_even_with_config_dir_set() {
        // Claude Code: `(CLAUDE_SECURESTORAGE_CONFIG_DIR || ~/.claude)` once it is defined —
        // CLAUDE_CONFIG_DIR is never consulted, matching the bare service name.
        assert_eq!(
            fallback_path_from(os(""), os("/opt/cc"), Path::new(DEFAULT_DIR)),
            default_file()
        );
    }

    #[test]
    fn a_relative_dir_resolves_nothing() {
        assert_eq!(
            fallback_path_from(None, os("relative/cc"), Path::new(DEFAULT_DIR)),
            None
        );
        assert_eq!(
            fallback_path_from(os("relative/secure"), None, Path::new(DEFAULT_DIR)),
            None
        );
    }

    #[test]
    fn a_non_ascii_dir_resolves_nothing() {
        assert_eq!(
            fallback_path_from(None, os("/opt/résumé"), Path::new(DEFAULT_DIR)),
            None
        );
    }

    // --- settle ------------------------------------------------------------------------------

    const ALL_MODES: [PlaintextFallbackMode; 3] = [
        PlaintextFallbackMode::Remove,
        PlaintextFallbackMode::Touch,
        PlaintextFallbackMode::Off,
    ];

    /// A present fallback file holding `body`, its mtime pinned a day in the past so a re-stamp
    /// is observable.
    fn present_file(dir: &Path, body: &[u8]) -> (PathBuf, SystemTime) {
        let path = dir.join(FALLBACK_FILE_NAME);
        fs::write(&path, body).unwrap();
        let old = SystemTime::now() - Duration::from_secs(86_400);
        fs::File::open(&path).unwrap().set_modified(old).unwrap();
        let mtime = fs::metadata(&path).unwrap().modified().unwrap();
        (path, mtime)
    }

    fn later() -> SystemTime {
        // A whole-second instant, so filesystem timestamp granularity cannot blur the comparison.
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    #[test]
    fn an_absent_file_is_silent_in_every_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FALLBACK_FILE_NAME);
        for mode in ALL_MODES {
            assert_eq!(settle(mode, &path, later()), None, "{mode:?}");
        }
    }

    #[test]
    fn remove_deletes_a_present_file() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = present_file(dir.path(), b"{}");
        assert_eq!(
            settle(PlaintextFallbackMode::Remove, &path, later()),
            Some(PlaintextFallbackAction::Removed)
        );
        assert!(!path.exists());
    }

    #[test]
    fn touch_restamps_the_mtime_and_leaves_the_contents() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = present_file(dir.path(), b"{\"claudeAiOauth\":{}}");
        assert_eq!(
            settle(PlaintextFallbackMode::Touch, &path, later()),
            Some(PlaintextFallbackAction::Touched)
        );
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), later());
        assert_eq!(fs::read(&path).unwrap(), b"{\"claudeAiOauth\":{}}");
    }

    #[test]
    fn off_leaves_the_file_exactly_as_it_was_but_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mtime) = present_file(dir.path(), b"{}");
        assert_eq!(
            settle(PlaintextFallbackMode::Off, &path, later()),
            Some(PlaintextFallbackAction::Ignored)
        );
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), mtime);
        assert_eq!(fs::read(&path).unwrap(), b"{}");
    }

    #[test]
    fn a_symlink_is_refused_in_every_mode_and_neither_link_nor_target_moves() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere.json");
        fs::write(&target, b"keep me").unwrap();
        let link = dir.path().join(FALLBACK_FILE_NAME);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let target_mtime = fs::metadata(&target).unwrap().modified().unwrap();
        for mode in ALL_MODES {
            assert_eq!(
                settle(mode, &link, later()),
                Some(PlaintextFallbackAction::Refused),
                "{mode:?}"
            );
            assert!(fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(fs::read(&target).unwrap(), b"keep me");
            assert_eq!(
                fs::metadata(&target).unwrap().modified().unwrap(),
                target_mtime
            );
        }
    }

    #[test]
    fn a_directory_is_refused_in_every_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FALLBACK_FILE_NAME);
        fs::create_dir(&path).unwrap();
        for mode in ALL_MODES {
            assert_eq!(
                settle(mode, &path, later()),
                Some(PlaintextFallbackAction::Refused),
                "{mode:?}"
            );
            assert!(path.is_dir());
        }
    }

    #[test]
    fn a_remove_the_filesystem_denies_is_reported_and_leaves_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = present_file(dir.path(), b"{}");
        // Unlinking needs write permission on the DIRECTORY, so a read-only parent denies it.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
        let action = settle(PlaintextFallbackMode::Remove, &path, later());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(action, Some(PlaintextFallbackAction::Failed));
        assert!(path.exists());
    }

    #[test]
    fn a_touch_the_filesystem_denies_is_reported_and_leaves_the_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mtime) = present_file(dir.path(), b"{}");
        // A mode-000 file cannot be opened even by its owner, so the re-stamp is denied.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        let action = settle(PlaintextFallbackMode::Touch, &path, later());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(action, Some(PlaintextFallbackAction::Failed));
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), mtime);
    }

    #[test]
    fn a_stat_the_filesystem_denies_is_reported_in_every_mode_and_moves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mtime) = present_file(dir.path(), b"{}");
        // Without search permission on the parent even the stat is denied — an error that is NOT
        // `NotFound`, so it must surface as a failure rather than read as "nothing there".
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o000)).unwrap();
        let actions = ALL_MODES.map(|mode| settle(mode, &path, later()));
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(actions, [Some(PlaintextFallbackAction::Failed); 3]);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), mtime);
    }

    // --- FallbackSettlingStore ---------------------------------------------------------------

    use crate::error::Error;
    use crate::keychain::FakeCredentialStore;

    fn cred(blob: &[u8]) -> Credential {
        Credential::new(blob.to_vec())
    }

    fn log_lines(log: &Path) -> Vec<String> {
        fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// A canonical store whose every write fails — a locked keychain at write time.
    struct FailingStore;

    impl CredentialStore for FailingStore {
        async fn read(&self) -> Result<Credential> {
            Err(Error::KeychainLocked { op: "read" })
        }

        async fn write(&self, _credential: &Credential) -> Result<()> {
            Err(Error::KeychainLocked { op: "write" })
        }
    }

    #[tokio::test]
    async fn a_successful_write_settles_the_fallback_and_logs_one_line() {
        for (mode, token, removed) in [
            (PlaintextFallbackMode::Remove, "removed", true),
            (PlaintextFallbackMode::Touch, "touched", false),
            (PlaintextFallbackMode::Off, "ignored", false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (path, _) = present_file(dir.path(), b"{}");
            let log = dir.path().join("events.log");
            let store = FallbackSettlingStore::at(
                FakeCredentialStore::empty(),
                mode,
                Some(path.clone()),
                log.clone(),
            );

            store.write(&cred(b"incoming")).await.unwrap();

            assert!(store.read().await.unwrap().matches(&cred(b"incoming")));
            assert_eq!(!path.exists(), removed, "{mode:?}");
            let lines = log_lines(&log);
            assert_eq!(lines.len(), 1, "{mode:?}: {lines:?}");
            assert!(
                lines[0].ends_with(&format!(" event=plaintext_fallback action={token}")),
                "{mode:?}: {lines:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_absent_fallback_writes_through_and_logs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.log");
        let store = FallbackSettlingStore::at(
            FakeCredentialStore::empty(),
            PlaintextFallbackMode::Remove,
            Some(dir.path().join(FALLBACK_FILE_NAME)),
            log.clone(),
        );

        store.write(&cred(b"incoming")).await.unwrap();

        assert!(store.read().await.unwrap().matches(&cred(b"incoming")));
        assert_eq!(log_lines(&log), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_failed_write_leaves_the_fallback_alone_and_logs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mtime) = present_file(dir.path(), b"{}");
        let log = dir.path().join("events.log");
        let store = FallbackSettlingStore::at(
            FailingStore,
            PlaintextFallbackMode::Remove,
            Some(path.clone()),
            log.clone(),
        );

        let result = store.write(&cred(b"incoming")).await;

        assert!(
            matches!(result, Err(Error::KeychainLocked { op: "write" })),
            "{result:?}"
        );
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), mtime);
        assert_eq!(log_lines(&log), Vec::<String>::new());
    }

    #[tokio::test]
    async fn without_a_resolved_path_the_store_is_a_pure_pass_through() {
        // The off-macOS shape: no path, so a file sitting where macOS would keep the fallback —
        // on Linux, the LIVE credential — is never touched.
        let dir = tempfile::tempdir().unwrap();
        let (path, mtime) = present_file(dir.path(), b"{}");
        let log = dir.path().join("events.log");
        let store = FallbackSettlingStore::at(
            FakeCredentialStore::empty(),
            PlaintextFallbackMode::Remove,
            None,
            log.clone(),
        );

        store.write(&cred(b"incoming")).await.unwrap();

        assert!(store.read().await.unwrap().matches(&cred(b"incoming")));
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), mtime);
        assert_eq!(log_lines(&log), Vec::<String>::new());
    }

    #[tokio::test]
    async fn reads_and_the_resolution_probe_pass_straight_through() {
        let dir = tempfile::tempdir().unwrap();
        let inner = FakeCredentialStore::empty();
        inner.set_not_found(true);
        let store = FallbackSettlingStore::at(
            inner,
            PlaintextFallbackMode::Remove,
            None,
            dir.path().join("events.log"),
        );

        assert!(matches!(store.read().await, Err(Error::CredentialNotFound)));
        assert!(matches!(
            store.probe_resolution().await,
            Err(Error::CredentialNotFound)
        ));
    }

    // --- wiring: the production writers of the canonical item ------------------------------

    /// Everything above `source`'s `#[cfg(test)] mod …` block. A lone `#[cfg(test)]` ITEM earlier
    /// in the file (use_account.rs carries one on an import) does not end the region; only the
    /// attribute on a module does.
    fn non_test_region(source: &str) -> String {
        let lines: Vec<&str> = source.lines().collect();
        let end = (0..lines.len())
            .find(|&i| {
                lines[i].starts_with("#[cfg(test)]")
                    && lines
                        .get(i + 1)
                        .is_some_and(|n| n.starts_with("mod ") && n.trim_end().ends_with('{'))
            })
            .unwrap_or(lines.len());
        lines[..end].join("\n")
    }

    /// The argument list of the first `FallbackSettlingStore::for_host(` call after `anchor` in
    /// `region`, verbatim between its balanced parentheses. Panics unless `anchor` occurs exactly
    /// once, so a renamed or duplicated site cannot leave the gate reading the wrong call.
    fn wrap_arguments_after(region: &str, anchor: &str) -> String {
        assert_eq!(
            region.matches(anchor).count(),
            1,
            "expected `{anchor}` exactly once in the non-test region (zero: the site moved or the \
             region was cut short; two: a second writer needs its own row)"
        );
        let after = &region[region.find(anchor).unwrap()..];
        const CALL: &str = "FallbackSettlingStore::for_host(";
        let call = after
            .find(CALL)
            .unwrap_or_else(|| panic!("no `{CALL}` after `{anchor}`"));
        // A bare store handed to the site BEFORE any wrap is the regression this gate exists for;
        // without this, a wrap further down the file would satisfy it.
        assert!(
            !after[..call].contains("RealCredentialStore::new()"),
            "a bare `RealCredentialStore::new()` reaches `{anchor}` ahead of the wrapper"
        );
        let start = call + CALL.len();
        let mut depth = 1usize;
        for (i, ch) in after[start..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' if depth == 1 => return after[start..start + i].to_owned(),
                ')' => depth -= 1,
                _ => {}
            }
        }
        panic!("unbalanced `{CALL}` after `{anchor}`")
    }

    /// Issue #1540: the three production paths that WRITE the canonical item — the daemon (its
    /// auto-swaps and keep-warm promotion), `use` (manual swap and adopt), and `login`'s
    /// activation — each build their store through the settling wrapper, over the real keychain
    /// store, with the mode the operator configured. A site that regresses to a bare
    /// `RealCredentialStore::new()` compiles and passes every behavioural test above, and silently
    /// brings back the swap a live session never sees.
    #[test]
    fn every_canonical_writer_settles_the_fallback_with_the_configured_mode() {
        let sites = [
            (
                "src/cli.rs",
                include_str!("cli.rs"),
                "let mut daemon = Daemon::new(",
            ),
            (
                "src/use_account.rs",
                include_str!("use_account.rs"),
                "run_use(\n        &config,",
            ),
            (
                "src/capture.rs",
                include_str!("capture.rs"),
                "let report = run_login_locked(",
            ),
        ];
        for (file, source, anchor) in sites {
            let args = wrap_arguments_after(&non_test_region(source), anchor);
            let args = args.trim();
            assert!(
                args.starts_with("RealCredentialStore::new()"),
                "{file}: the store after `{anchor}` must wrap the real keychain store, got `{args}`"
            );
            assert!(
                args.contains("plaintext_fallback"),
                "{file}: the mode must come from `[credential].plaintext_fallback`, got `{args}`"
            );
        }
    }
}
