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
use std::io::{ErrorKind, Write};
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
///
/// Exposed `pub(crate)` for the launchd domain target `gui/<uid>` the background
/// service installer builds (issue #166); every other caller is in-module.
pub(crate) fn current_uid() -> u32 {
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

/// The current user's login name from the password database
/// (`getpwuid(getuid())->pw_name`), resolved the same way as [`home_dir`] — never
/// from `$USER`, which may be unset or spoofed.
///
/// This is the `acct` attribute Claude Code stores its credential item under (CC's
/// `vO()` == `whoami`; `build/version-compat.md`). The isolated-refresh engine
/// (issue #102) seeds and reads its isolated keychain item under the SAME `acct`,
/// so a `claude` it spawns locates the seeded item.
pub(crate) fn username() -> Result<OsString> {
    let uid = current_uid();
    // SAFETY: `getpwuid` returns a pointer into a libc-owned static buffer. The crate
    // runs on a single-threaded executor (`#[tokio::main(flavor = "current_thread")]`)
    // and `getpwuid` (here and in [`home_dir`]) is the crate's ONLY `getpw*` caller, so
    // no concurrent `getpw*` can race or invalidate the shared buffer — this holds for
    // this function's mid-runtime callers too (the #102 refresh engine resolves the
    // `acct` per cycle), not only at startup. `pw_name` is copied into an owned
    // `OsString` before any later `getpw*` (e.g. a subsequent `home_dir`) could run.
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return Err(Error::UserUnresolved);
        }
        let name = (*pw).pw_name;
        if name.is_null() {
            return Err(Error::UserUnresolved);
        }
        let bytes = CStr::from_ptr(name).to_bytes().to_vec();
        if bytes.is_empty() {
            return Err(Error::UserUnresolved);
        }
        Ok(OsString::from_vec(bytes))
    }
}

/// The ephemeral isolated-refresh directory for account `uuid`:
/// `<support_dir>/refresh/<uuid>` (issue #102). Native-local under [`support_dir`]
/// (not the XDG-overridable [`config_dir`]) — it is the isolated `CLAUDE_CONFIG_DIR`
/// whose path-hash names the isolated keychain item, so it must resolve identically
/// for the engine and the `claude` it spawns regardless of a per-shell
/// `XDG_CONFIG_HOME`.
pub(crate) fn isolated_refresh_dir(uuid: &str) -> Result<PathBuf> {
    Ok(support_dir()?.join("refresh").join(uuid))
}

/// The ephemeral isolated interactive-login directory: `<support_dir>/login` (issue
/// #132). Native-local under [`support_dir`] (like [`isolated_refresh_dir`]) — it is
/// the isolated `CLAUDE_CONFIG_DIR` the captured `claude /login` runs under, whose
/// path-hash names the suffixed isolated keychain item CC writes the fresh credential
/// to, so it must resolve identically for the engine and the `claude` it spawns.
///
/// Unlike the refresh dir, this is NOT keyed by an account uuid: a fresh login capture
/// discovers the account only AFTER the login completes (from the isolated
/// `.claude.json` `oauthAccount`), so there is no uuid to key on up front. A single
/// fixed `login` leaf suffices — the capture-then-`/login` loop is sequential (one
/// login at a time), and [`create_isolated_dir`] removes any stale leaf a crashed
/// prior capture left behind before each run.
///
/// Reachable in production via the daemon startup / `login`-start orphan reaper (issue #133), which
/// derives the isolated login item's #100 service from this path; the login-capture engine's own
/// production entry is wired by a later issue (#134).
pub(crate) fn isolated_login_dir() -> Result<PathBuf> {
    Ok(support_dir()?.join("login"))
}

/// Create the ephemeral isolated-refresh directory `path` (issue #102) as a fresh,
/// private (`0700`, owner-checked) directory, REFUSING a pre-existing symlink.
///
/// Stricter than [`ensure_private_dir`]: a spawned `claude` writes its `.claude.json`
/// into this dir, and the dir's path-hash names the keychain item it refreshes, so a
/// symlink planted at this path could redirect those writes outside our `0700` tree.
/// The leaf is therefore created FRESH — any pre-existing *real* directory (a stale
/// dir left by a crashed prior cycle) is removed first, and a pre-existing *symlink*
/// is refused ([`Error::UnsafeIsolatedDir`]) rather than followed. After creation the
/// leaf is re-checked with `symlink_metadata` (`lstat` — never follows a link) to be a
/// real directory owned by the current uid. The parent (`<support>/refresh`) is
/// ensured private first.
pub(crate) fn create_isolated_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    // `symlink_metadata` (lstat) classifies the leaf itself, not a link's target.
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(Error::UnsafeIsolatedDir {
                path: path.to_path_buf(),
            });
        }
        // A stale real directory from a prior crashed cycle — remove it so the seed
        // and `.claude.json` start from a clean, owner-fresh state.
        Ok(_) => fs::remove_dir_all(path)?,
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(Error::Io(err)),
    }
    // `create_dir` (not `_all`) makes the leaf fresh and fails if it reappeared; it
    // never follows a symlink (a TOCTOU-planted link at this point fails the create
    // or is caught by the post-create lstat below).
    fs::create_dir(path)?;
    fs::set_permissions(path, Permissions::from_mode(DIR_MODE))?;
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        return Err(Error::UnsafeIsolatedDir {
            path: path.to_path_buf(),
        });
    }
    if meta.uid() != current_uid() {
        return Err(Error::ForeignOwnership(path.to_path_buf()));
    }
    Ok(())
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

/// The per-user LaunchAgents directory: `~/Library/LaunchAgents`.
///
/// Where the background service's launchd plist lives (issue #166). Unlike this
/// crate's private state dirs, it is a shared, system-defined location
/// (conventionally `0755`), so the installer creates it with `create_dir_all` —
/// NOT [`ensure_private_dir`], which would narrow it to `0700` and assert sole
/// ownership. Native-local (never XDG-relative): the login-session launchd domain
/// reads agents only from here.
pub(crate) fn launch_agents_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join("Library/LaunchAgents"))
}

/// The native-local application-support directory, **always**
/// `~/Library/Application Support/sessiometer` — even when `$XDG_CONFIG_HOME`
/// redirects [`config_dir`].
///
/// The daemon's runtime files (the single-instance lock and the control socket)
/// live here rather than under the XDG-overridable config dir so that a second
/// `run` contends on the *same* lock regardless of a per-shell `XDG_CONFIG_HOME`
/// — the lock's job is to serialize Sessiometer against itself on one machine,
/// which an env-var-relative path would defeat (issue #7).
pub(crate) fn support_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join("Library/Application Support").join(APP))
}

/// The single-instance lock file: `<support_dir>/daemon.lock` (`0600`).
///
/// A kernel advisory `flock` is held on this for the daemon's whole lifetime; a
/// second `run` fails to acquire it and exits `3` (issue #7). Native-local (via
/// [`support_dir`]) so the contention is machine-global, not XDG-relative.
pub(crate) fn daemon_lock() -> Result<PathBuf> {
    Ok(support_dir()?.join("daemon.lock"))
}

/// The control socket: `<support_dir>/daemon.sock` (`0600`).
///
/// The newline-delimited-JSON Unix-domain control channel a running daemon
/// serves `status` on (issue #7). Native-local (via [`support_dir`]) and a Unix
/// domain socket — never a TCP port.
pub(crate) fn control_socket() -> Result<PathBuf> {
    Ok(support_dir()?.join("daemon.sock"))
}

/// The single-WRITER swap lock file: `<support_dir>/swap.lock` (`0600`).
///
/// A kernel advisory `flock` held only for the DURATION of a swap (not the
/// process lifetime) by BOTH the manual `use` swap and the daemon's swap routine,
/// so the two-step swap (canonical keychain write → `~/.claude.json` co-write)
/// runs as a mutually-exclusive critical section and the two writers can never
/// interleave into a split state (issue #64). DISTINCT from [`daemon_lock`]: that
/// one is held non-blocking for the daemon's whole lifetime (a single-INSTANCE
/// gate), so reusing it would either hang `use` or misreport "already running".
/// Native-local (via [`support_dir`]) so the contention is machine-global, not
/// XDG-relative — exactly like the single-instance lock.
pub(crate) fn swap_lock() -> Result<PathBuf> {
    Ok(support_dir()?.join("swap.lock"))
}

/// The raw usage-sample log: `<support_dir>/usage-samples.jsonl` (`0600`).
///
/// The append-only rolling window the daemon writes one JSON line per poll to, and
/// read-only tools read (issue #155, via [`crate::usage_store`]). Native-local (via
/// [`support_dir`]) alongside the lock/socket/config, so a single machine has one
/// store regardless of a per-shell `XDG_CONFIG_HOME`.
///
/// Consumed in production by the daemon's per-poll collector (issue #156) and the
/// read-only reporting tools (issue #157); until they land the store is a
/// not-yet-wired seam ([`crate::usage_store`]), so — like [`write_preserving_mode`]
/// — this is `allow(dead_code)` off the test path.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn usage_samples() -> Result<PathBuf> {
    Ok(support_dir()?.join("usage-samples.jsonl"))
}

/// The rolled usage aggregates: `<support_dir>/usage-rollup.json` (`0600`).
///
/// The single atomically-rewritten object holding the hourly + daily tiers and the
/// roll watermark (issue #155, via [`crate::usage_store`]). Sibling to
/// [`usage_samples`] under the native-local support dir; wired into production by
/// the same later work items, hence the matching `allow(dead_code)`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn usage_rollup() -> Result<PathBuf> {
    Ok(support_dir()?.join("usage-rollup.json"))
}

/// Claude Code's per-user state file: `~/.claude.json`.
///
/// Holds the active account's `oauthAccount` identity block, which `capture`
/// (issue #4) records alongside the keychain credential. Resolved from the
/// password database like every other path here — never from `$HOME`.
pub(crate) fn claude_json() -> Result<PathBuf> {
    Ok(home_dir()?.join(".claude.json"))
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

/// Resolve the `claude` binary to spawn for an isolated refresh (issue #102 step 4):
/// `$CLAUDE_BIN` if it names an existing file, else the first `claude` found on
/// `$PATH`. The result is absolute (the spawn pins an absolute binary — a PATH entry
/// may be a wrapper that execs a patched copy, the #101 provenance note), so a caller
/// can validate it once before spawning. [`Error::ClaudeBinaryNotFound`] if neither
/// yields an existing file. Used by the one-shot `poke` (issue #104) and, later, the
/// periodic refresh tick (#105).
pub(crate) fn claude_binary() -> Result<PathBuf> {
    claude_binary_from(
        std::env::var_os("CLAUDE_BIN"),
        std::env::var_os("PATH"),
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    )
}

/// The pure resolution policy for [`claude_binary`], taking the `$CLAUDE_BIN` /
/// `$PATH` values + `cwd` as arguments so the override / PATH-scan / not-found
/// branches are testable without mutating process-global env. An empty / unset
/// `$CLAUDE_BIN` falls through to the PATH scan; a `$CLAUDE_BIN` that is set but does
/// NOT name an existing file is an error (the operator pointed us at a specific
/// binary — don't silently substitute a different one).
/// Resolve the `claude` binary the isolated-refresh engine spawns, honoring the
/// `[refresh].claude_bin` config override (issue #105) ahead of the `$CLAUDE_BIN` / `$PATH`
/// resolution [`claude_binary`] performs.
///
/// `config_bin` is `Some` only when the operator set `[refresh].claude_bin` (an empty value
/// is collapsed to `None` at config-load). When set it WINS and is validated exactly like a
/// `$CLAUDE_BIN` override — absolutized against the current dir, then required to name an
/// existing file — so a configured-but-missing binary is [`Error::ClaudeBinaryNotFound`],
/// never a silent fall-through to a different `claude` on `$PATH` (the operator named a
/// specific binary; honor it or fail). When `None`, defers to [`claude_binary`].
pub(crate) fn claude_binary_with_override(config_bin: Option<&Path>) -> Result<PathBuf> {
    match config_bin {
        // A configured override is the sole candidate: pass no `$PATH`, so a missing one is
        // an error rather than a scan that substitutes a different binary.
        Some(bin) => claude_binary_from(
            Some(bin.as_os_str().to_owned()),
            None,
            &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        ),
        None => claude_binary(),
    }
}

fn claude_binary_from(
    claude_bin: Option<OsString>,
    path: Option<OsString>,
    cwd: &Path,
) -> Result<PathBuf> {
    if let Some(bin) = claude_bin {
        if !bin.is_empty() {
            let candidate = absolutize(PathBuf::from(bin), cwd);
            return if candidate.is_file() {
                Ok(candidate)
            } else {
                Err(Error::ClaudeBinaryNotFound)
            };
        }
    }
    if let Some(path) = path {
        for dir in std::env::split_paths(&path) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            // Absolutize BEFORE the existence check: a relative PATH entry must resolve
            // against `cwd` (the engine pins an absolute binary), and `is_file` on a
            // relative path would otherwise probe the process cwd, not `cwd`.
            let candidate = absolutize(dir.join("claude"), cwd);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(Error::ClaudeBinaryNotFound)
}

/// Make `path` absolute against `cwd` (a `$PATH` entry or `$CLAUDE_BIN` may be
/// relative); an already-absolute path is returned unchanged. Deliberately NO
/// symlink resolution — a `claude` wrapper on PATH must be spawned as-is (#101).
fn absolutize(path: PathBuf, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
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

/// Atomically (over)write an **existing** `path` with `contents`, preserving its
/// current permission mode rather than forcing `0600`.
///
/// Same atomic shape as [`write_private_file`] — a same-directory `<path>.tmp`,
/// `fsync`, then `rename` over `path`, so a concurrent reader never observes a
/// half-written file — but for a file whose permission policy is **not ours to
/// set**. The swap engine (#6) co-writes the `oauthAccount` block into
/// `~/.claude.json`, a file owned by Claude Code; the existing file's mode is
/// copied onto the replacement so the co-write never widens (nor narrows) the
/// user's chosen permissions. `path` must already exist — its mode is the very
/// thing being preserved, so an absent file is an error, never a silent create at
/// our default mode. Wired into the swap loop in #7 (via [`crate::claude_state`]).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn write_preserving_mode(path: &Path, contents: &[u8]) -> Result<()> {
    // The existing file's permission bits (including any setuid/setgid/sticky),
    // copied verbatim onto the replacement. Reading metadata first also surfaces
    // an absent file here rather than fabricating one at `FILE_MODE`.
    let mode = fs::metadata(path)?.permissions().mode() & 0o7777;

    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);

    // A stale temp from a prior crashed write would make `create_new` fail; remove
    // it best-effort so we always start from a fresh file.
    let _ = fs::remove_file(&tmp);
    {
        // Created `0600` so the temp is never *more* permissive than the file it
        // replaces while it is being written; the source mode is copied on just
        // before the rename.
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(FILE_MODE)
            .open(&tmp)?;
        file.write_all(contents)?;
        file.set_permissions(Permissions::from_mode(mode))?;
        // Durable (data + the copied mode) before the rename, so a crash can't
        // leave a truncated file in place of the old one.
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
    fn support_dir_is_native_local_application_support() {
        // The daemon's lock/socket dir is always native-local — it reads no
        // XDG override (unlike `config_dir`), so its tail is fixed.
        let dir = support_dir().unwrap();
        assert!(
            dir.ends_with("Library/Application Support/sessiometer"),
            "support_dir must be native-local, got {dir:?}"
        );
    }

    #[test]
    fn lock_and_socket_live_directly_under_support_dir() {
        let support = support_dir().unwrap();
        assert_eq!(daemon_lock().unwrap(), support.join("daemon.lock"));
        assert_eq!(control_socket().unwrap(), support.join("daemon.sock"));
    }

    #[test]
    fn usage_store_files_live_directly_under_support_dir() {
        // The usage-sample store (issue #155) is native-local alongside the
        // lock/socket/config, with the two fixed leaf names, so a machine has one
        // store regardless of `XDG_CONFIG_HOME`.
        let support = support_dir().unwrap();
        assert_eq!(
            usage_samples().unwrap(),
            support.join("usage-samples.jsonl")
        );
        assert_eq!(usage_rollup().unwrap(), support.join("usage-rollup.json"));
        assert_ne!(usage_samples().unwrap(), usage_rollup().unwrap());
    }

    #[test]
    fn swap_lock_is_distinct_from_the_single_instance_lock() {
        // The single-WRITER swap lock (issue #64) is native-local like the rest of
        // the runtime files, and a DISTINCT file from the single-instance lock —
        // reusing `daemon.lock` would hang `use` or misreport "already running".
        let support = support_dir().unwrap();
        assert_eq!(swap_lock().unwrap(), support.join("swap.lock"));
        assert_ne!(swap_lock().unwrap(), daemon_lock().unwrap());
    }

    #[test]
    fn username_resolves_a_non_empty_login_name() {
        // The login name backs the isolated item's `acct` (#102); it must resolve
        // to a non-empty value from the password database (never `$USER`).
        let name = username().unwrap();
        assert!(!name.is_empty());
    }

    #[test]
    fn isolated_refresh_dir_is_native_local_under_refresh() {
        // The isolated CLAUDE_CONFIG_DIR (#102) lives under the native-local support
        // dir, never the XDG-overridable config dir, so its path-hash is stable.
        let dir = isolated_refresh_dir("11111111-1111-1111-1111-111111111111").unwrap();
        assert!(dir.ends_with(
            "Library/Application Support/sessiometer/refresh/11111111-1111-1111-1111-111111111111"
        ));
        assert!(dir.starts_with(support_dir().unwrap()));
    }

    #[test]
    fn isolated_login_dir_is_native_local_under_login() {
        // The isolated login CLAUDE_CONFIG_DIR (#132) is a single fixed leaf under the
        // native-local support dir (not uuid-keyed — the account is unknown until the
        // login completes), so its path-hash names the suffixed isolated item stably.
        let dir = isolated_login_dir().unwrap();
        assert!(dir.ends_with("Library/Application Support/sessiometer/login"));
        assert!(dir.starts_with(support_dir().unwrap()));
        // Distinct from the refresh tree — the two engines never share an isolated dir.
        assert_ne!(dir, isolated_refresh_dir("login").unwrap());
    }

    #[test]
    fn create_isolated_dir_makes_a_fresh_0700_owned_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("refresh/u-1");
        create_isolated_dir(&dir).unwrap();

        let meta = fs::symlink_metadata(&dir).unwrap();
        assert!(meta.file_type().is_dir());
        assert_eq!(meta.permissions().mode() & 0o777, DIR_MODE);
        assert_eq!(meta.uid(), current_uid());
    }

    #[test]
    fn create_isolated_dir_replaces_a_stale_real_directory() {
        // A crashed prior cycle can leave a stale dir (possibly with leftover files);
        // the next cycle must start clean — the stale dir is removed and recreated.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("refresh/u-1");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("stale.json"), b"leftover").unwrap();

        create_isolated_dir(&dir).unwrap();

        assert!(dir.exists());
        assert!(
            !dir.join("stale.json").exists(),
            "stale contents must be cleared"
        );
    }

    #[test]
    fn create_isolated_dir_refuses_a_pre_existing_symlink() {
        // A symlink planted at the leaf path is REFUSED, not followed — it could
        // redirect the seeded .claude.json / the spawn's writes out of our 0700 tree.
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("refresh");
        fs::create_dir_all(&parent).unwrap();
        let target = tmp.path().join("elsewhere");
        fs::create_dir_all(&target).unwrap();
        let link = parent.join("u-1");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let err = create_isolated_dir(&link).unwrap_err();
        assert!(matches!(err, Error::UnsafeIsolatedDir { .. }));
        // The symlink (and its target) are untouched — refused, never followed.
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(target.exists());
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

    #[test]
    fn write_preserving_mode_keeps_an_existing_non_0600_mode() {
        // The co-write target (~/.claude.json) is Claude Code's; a non-0600 mode
        // must survive the co-write — the opposite of `write_private_file`, which
        // forces 0600 on our own files.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o644)).unwrap();

        write_preserving_mode(&path, b"new-contents").unwrap();

        let meta = fs::metadata(&path).unwrap();
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o644,
            "must preserve the existing mode, not force 0600"
        );
        assert_eq!(fs::read(&path).unwrap(), b"new-contents");
        // No temp file left behind.
        assert!(!tmp.path().join("state.json.tmp").exists());
    }

    #[test]
    fn write_preserving_mode_keeps_a_0600_mode_too() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o600)).unwrap();

        write_preserving_mode(&path, b"new").unwrap();

        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn write_preserving_mode_requires_an_existing_file() {
        // The mode being preserved is the existing file's, so an absent file is an
        // error — never a silent create at our default mode.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("absent.json");
        assert!(write_preserving_mode(&path, b"x").is_err());
        assert!(!path.exists());
    }

    // --- claude_binary_from --------------------------------------------------

    #[test]
    fn claude_binary_prefers_an_existing_claude_bin_override() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("claude");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();
        let got = claude_binary_from(
            Some(bin.as_os_str().to_owned()),
            Some(OsString::from("/nonexistent")),
            Path::new("/cwd"),
        )
        .unwrap();
        assert_eq!(got, bin);
    }

    #[test]
    fn claude_binary_errors_when_the_override_is_missing() {
        // Set but not an existing file — don't silently substitute a PATH `claude`.
        let tmp = tempfile::tempdir().unwrap();
        let path_dir = tmp.path().join("bin");
        fs::create_dir(&path_dir).unwrap();
        fs::write(path_dir.join("claude"), b"#!/bin/sh\n").unwrap();
        let err = claude_binary_from(
            Some(OsString::from("/no/such/claude")),
            Some(path_dir.as_os_str().to_owned()),
            Path::new("/cwd"),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ClaudeBinaryNotFound));
    }

    #[test]
    fn claude_binary_scans_path_when_no_override() {
        let tmp = tempfile::tempdir().unwrap();
        let dir_a = tmp.path().join("a");
        let dir_b = tmp.path().join("b");
        fs::create_dir(&dir_a).unwrap();
        fs::create_dir(&dir_b).unwrap();
        let bin = dir_b.join("claude");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();
        // `a` has no `claude`; the scan finds it in `b`.
        let path = std::env::join_paths([dir_a.as_os_str(), dir_b.as_os_str()]).unwrap();
        let got = claude_binary_from(None, Some(path), Path::new("/cwd")).unwrap();
        assert_eq!(got, bin);
    }

    #[test]
    fn claude_binary_errors_when_absent_everywhere() {
        let tmp = tempfile::tempdir().unwrap();
        let empty_dir = tmp.path().join("empty");
        fs::create_dir(&empty_dir).unwrap();
        let err = claude_binary_from(
            None,
            Some(empty_dir.as_os_str().to_owned()),
            Path::new("/cwd"),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ClaudeBinaryNotFound));
    }

    #[test]
    fn claude_binary_absolutizes_a_relative_path_entry() {
        // A relative PATH dir resolves against cwd — the engine pins an absolute binary.
        let tmp = tempfile::tempdir().unwrap();
        let rel = std::path::PathBuf::from("relbin");
        let abs = tmp.path().join("relbin");
        fs::create_dir(&abs).unwrap();
        fs::write(abs.join("claude"), b"#!/bin/sh\n").unwrap();
        let got = claude_binary_from(None, Some(rel.as_os_str().to_owned()), tmp.path()).unwrap();
        assert_eq!(got, abs.join("claude"));
        assert!(got.is_absolute());
    }

    // --- claude_binary_with_override (issue #105) ---------------------------

    #[test]
    fn override_prefers_a_present_config_bin() {
        // A `[refresh].claude_bin` pointing at an existing absolute file resolves to it,
        // ahead of any `$CLAUDE_BIN` / `$PATH` (absolute, so cwd-independent).
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("claude");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();
        let got = claude_binary_with_override(Some(&bin)).unwrap();
        assert_eq!(got, bin);
    }

    #[test]
    fn override_errors_on_a_missing_config_bin() {
        // A configured-but-missing override fails rather than silently scanning `$PATH`
        // for a different `claude` — the operator named a specific binary.
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("no-such-claude");
        let err = claude_binary_with_override(Some(&missing)).unwrap_err();
        assert!(matches!(err, Error::ClaudeBinaryNotFound));
    }
}
