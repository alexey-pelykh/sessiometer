// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Filesystem locations and their permission discipline.
//!
//! Base directories resolve at the platform's native location (issue #24):
//! macOS keeps its long-pinned `~/Library/…` layout exactly as before; Linux
//! (and other non-Apple Unix) follows the XDG Base Directory spec; Windows
//! resolves everything under the `%LOCALAPPDATA%` Known Folder — Local, never
//! Roaming: credential-adjacent state must not roam across a domain profile.
//! The precedence ladder for the overridable dirs is `--config`/`--log`
//! override ([`config_dir_with_override`] / [`logs_dir_with_override`]) >
//! `$XDG_*` opt-in override (where the platform honors one) > native default —
//! except the runtime state dir ([`support_dir`]: lock, socket, usage store),
//! which is ALWAYS native-fixed so contention stays machine-global (issue #7).
//!
//! On Unix the home directory is resolved from the password database via
//! `getpwuid(getuid())` rather than `$HOME`: the process may be launched in an
//! environment where `$HOME` is unset or spoofed, yet the state and credential
//! files this tool manages must land in the real user's home. On Windows the
//! base resolves through `etcetera`'s Windows strategy, which is env-first —
//! `%LOCALAPPDATA%` when set, the `SHGetKnownFolderPath` Known-Folder API as
//! its fallback; pinning it to the API alone (the analog of this `getpwuid`
//! discipline) is a requirement on the Windows-enablement work, which is also
//! where that branch first compiles.
//!
//! That same password-database discipline extends past *locations* to the user's
//! login shell (issue #783): under launchd the daemon inherits a bare
//! `PATH=/usr/bin:/bin:/usr/sbin:/sbin` with no `~/.local/bin`, so the user-level
//! `PATH` has to be reconstructed by running the login shell — resolved from
//! `pw_shell`, never from `$SHELL`, for the same reason the home directory is never
//! read from `$HOME`.
//!
//! Directories are created `0700` and files `0600`, and every directory we
//! create is asserted to be owned by the current uid before use.

use std::ffi::{CStr, OsStr, OsString};
use std::fs::{self, File, OpenOptions, Permissions};
use std::future::Future;
use std::io::{ErrorKind, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::Command;
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use crate::isolated_spawn::SPAWN_ENV_REMOVE;

/// `0700` — owner `rwx`, nothing for group/other.
const DIR_MODE: u32 = 0o700;
/// `0600` — owner `rw`, nothing for group/other.
///
/// `pub(crate)` since issue #1439: the roster backup ring reads back the mode of every entry it
/// writes rather than assuming the writer's, and AC-5 names THIS constant as the one an entry
/// must match, so the check has to compare against it rather than against a second copy.
pub(crate) const FILE_MODE: u32 = 0o600;
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
    // SAFETY: `getpwuid` returns a pointer into a libc-owned static buffer. Exactly
    // THREE functions read it — this one, [`username`], and [`login_shell`] (issue
    // #783) — and they are the crate's only `getpw*` callers, so nothing else can
    // invalidate the buffer between the call and the copy. What rules out a
    // concurrent `getpw*` is the single-threaded executor
    // (`#[tokio::main(flavor = "current_thread")]` in `src/main.rs`, ADR-0001) — NOT
    // "single-threaded at startup", which would be the wrong argument: all three are
    // reachable mid-runtime (the #102 refresh engine resolves the `acct` per cycle,
    // and the #783 harvest is built for that same per-cycle path). `pw_dir` is copied
    // into an owned `OsString` before any later `getpw*` could run.
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
/// This is the **fallback source** for the `acct` attribute Claude Code stores its
/// credential item under — not the `acct` itself. CC's own derivation prefers `$USER`
/// and only falls back to this passwd name (`keychain::claude_code_acct_from`, issue
/// #711; `build/version-compat.md`), so the isolated-refresh engine (issue #102) seeds
/// its item under CC's *derived* `acct` rather than calling this directly. The `$USER`
/// preference is deliberately confined to that mirror: everything else this module
/// resolves — the home directory above all — must key off the real user regardless of
/// a spoofed environment, which is exactly what this function guarantees.
pub(crate) fn username() -> Result<OsString> {
    let uid = current_uid();
    // SAFETY: `getpwuid` returns a pointer into a libc-owned static buffer. Exactly
    // THREE functions read it — this one, [`home_dir`], and [`login_shell`] (issue
    // #783) — and they are the crate's only `getpw*` callers, so no concurrent
    // `getpw*` can race or invalidate the shared buffer; the guarantee comes from the
    // single-threaded executor (`#[tokio::main(flavor = "current_thread")]`,
    // ADR-0001), and it holds for every mid-runtime caller (the #102 refresh engine
    // resolves the `acct` per cycle; the #783 harvest is built for that same per-cycle
    // path), not only at startup. `pw_name` is copied into an owned `OsString` before
    // any later `getpw*` (e.g. a subsequent `home_dir`) could run.
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

/// The current user's login shell from the password database
/// (`getpwuid(getuid())->pw_shell`), resolved exactly like [`home_dir`] and
/// [`username`] — never from `$SHELL`.
///
/// `$SHELL` is unusable here for two independent reasons: under launchd it is simply
/// absent (the same impoverished environment that motivates issue #783 at all), and
/// where it *is* set it is an inherited, spoofable value rather than the user's
/// registered login shell. The password database is the same identity source the rest
/// of this module already keys off.
///
/// The THIRD `getpw*` accessor in the crate, and deliberately a third *function*
/// rather than a multi-field read folded into [`home_dir`]: both existing accessors
/// have callers of their own, and merging them would be a refactor buying nothing the
/// immediate-copy discipline (see the SAFETY note) does not already provide.
///
/// A `pw_shell` that is absent, or that does not name an absolute path, is
/// [`Error::LoginShellUnresolved`] — a `nologin`-style account may legitimately carry an
/// empty one, and the caller degrades rather than guessing a shell. See
/// [`login_shell_from`] for why a *relative* entry is refused on the same footing. Not
/// yet wired into production (the harvest's resolution-chain wiring is issue #784), so —
/// like [`usage_samples`] — it is `allow(dead_code)` off the test path.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn login_shell() -> Result<PathBuf> {
    let uid = current_uid();
    // SAFETY: `getpwuid` returns a pointer into a libc-owned static buffer. Exactly
    // THREE functions read it — this one, [`home_dir`], and [`username`] — and they
    // are the crate's only `getpw*` callers, so nothing else can invalidate the buffer
    // between the call and the copy. What rules out a concurrent `getpw*` is the
    // single-threaded executor (`#[tokio::main(flavor = "current_thread")]`,
    // ADR-0001); this function in particular is built for the per-cycle refresh path
    // (#784), so "single-threaded at startup" would NOT be a sufficient argument for
    // it. `pw_shell` is copied into an owned `OsString` before any later `getpw*`
    // could run.
    let bytes = unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return Err(Error::LoginShellUnresolved);
        }
        let shell = (*pw).pw_shell;
        if shell.is_null() {
            return Err(Error::LoginShellUnresolved);
        }
        CStr::from_ptr(shell).to_bytes().to_vec()
    };
    login_shell_from(&bytes)
}

/// The pure validation half of [`login_shell`], taking the raw `pw_shell` bytes so both
/// rejections are testable without a passwd entry to forge — the same
/// argument-threading the [`claude_binary_from`] and [`harvest_path_from`] seams in this
/// module already use.
///
/// A single predicate covers both rejections, because an EMPTY path is not an absolute
/// one: `pw_shell` must be an absolute path. Empty is the `nologin`-class passwd entry
/// (there is nothing to execute); relative is the transport rule's discipline #1
/// (`CONTRIBUTING.md`: "Absolute path …, never `$PATH`-resolved"), which applies with
/// more force to the binary actually exec'd than to the `/usr/bin/env` it runs — a
/// relative `pw_shell` would otherwise be resolved against the very `PATH` whose absence
/// is the reason this harvest exists.
fn login_shell_from(pw_shell: &[u8]) -> Result<PathBuf> {
    let path = PathBuf::from(OsString::from_vec(pw_shell.to_vec()));
    if !path.is_absolute() {
        return Err(Error::LoginShellUnresolved);
    }
    Ok(path)
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

// --- Platform path strategies (issue #24) --------------------------------------
//
// Three pure derivation families — Apple (macOS), XDG (Linux and other
// non-Apple Unix), Windows — so every platform's path policy is unit-testable
// on any host without touching process-global state. The live accessors
// ([`config_dir`], [`support_dir`], [`logs_dir`]) select the family for the
// compile target and feed it the platform-resolved base: the `getpwuid` home
// on Unix, the `%LOCALAPPDATA%` Known Folder on Windows. Each family is live
// on its own target and test-exercised on every host, so the off-target
// families carry the usual test-only `allow(dead_code)`.

/// Pure derivation of the macOS config directory, so the env/home policy is
/// testable without touching process-global state: `$XDG_CONFIG_HOME/sessiometer`
/// when that override is set and non-empty, else the native
/// `~/Library/Application Support/sessiometer`. The long-pinned macOS behavior —
/// note it predates the XDG-spec-strict [`xdg_dir_from`] ladder and accepts any
/// non-empty override, relative included.
#[cfg_attr(not(test), allow(dead_code))]
fn apple_config_dir_from(home: &Path, xdg_config_home: Option<OsString>) -> PathBuf {
    match xdg_config_home {
        Some(xdg) if !xdg.is_empty() => Path::new(&xdg).join(APP),
        _ => home.join("Library/Application Support").join(APP),
    }
}

/// Pure derivation of the native-local macOS application-support directory,
/// `~/Library/Application Support/sessiometer` — the fixed macOS home of
/// [`support_dir`], never env-overridden.
#[cfg_attr(not(test), allow(dead_code))]
fn apple_support_dir_from(home: &Path) -> PathBuf {
    home.join("Library/Application Support").join(APP)
}

/// Pure derivation of the native macOS log directory,
/// `~/Library/Logs/sessiometer` (Console.app reads here).
#[cfg_attr(not(test), allow(dead_code))]
fn apple_logs_dir_from(home: &Path) -> PathBuf {
    home.join("Library/Logs").join(APP)
}

/// Pure derivation of the XDG config directory (Linux and other non-Apple
/// Unix): `$XDG_CONFIG_HOME/sessiometer` when the override is set to an
/// absolute path, else the spec default `~/.config/sessiometer`.
#[cfg_attr(not(test), allow(dead_code))]
fn xdg_config_dir_from(home: &Path, xdg_config_home: Option<OsString>) -> PathBuf {
    xdg_dir_from(home, xdg_config_home, ".config")
}

/// Pure derivation of the XDG state directory (Linux and other non-Apple
/// Unix): `$XDG_STATE_HOME/sessiometer` when the override is set to an
/// absolute path, else the spec default [`xdg_state_default_from`]. Logs live
/// here off-macOS ([`logs_dir`]); the runtime state dir deliberately does NOT
/// read the override ([`support_dir`]).
#[cfg_attr(not(test), allow(dead_code))]
fn xdg_state_dir_from(home: &Path, xdg_state_home: Option<OsString>) -> PathBuf {
    xdg_dir_from(home, xdg_state_home, ".local/state")
}

/// The fixed XDG state-home default, `~/.local/state/sessiometer` — the
/// off-macOS home of [`support_dir`]. Split from [`xdg_state_dir_from`] so the
/// never-overridden runtime state dir cannot accidentally grow the env ladder.
#[cfg_attr(not(test), allow(dead_code))]
fn xdg_state_default_from(home: &Path) -> PathBuf {
    home.join(".local/state").join(APP)
}

/// The shared XDG ladder: an absolute non-empty `$XDG_*` override wins;
/// anything else — unset, empty, or relative (invalid per the XDG Base
/// Directory spec: "All paths … must be absolute. If … a relative path … it
/// should consider the path invalid and ignore it") — falls back to
/// `<home>/<spec_default>/sessiometer`.
fn xdg_dir_from(home: &Path, xdg_override: Option<OsString>, spec_default: &str) -> PathBuf {
    match xdg_override {
        Some(xdg) if !xdg.is_empty() && Path::new(&xdg).is_absolute() => Path::new(&xdg).join(APP),
        _ => home.join(spec_default).join(APP),
    }
}

/// Windows app folder segment — capitalized per the platform's convention:
/// `%LOCALAPPDATA%\Sessiometer` (issue #24).
#[cfg_attr(not(test), allow(dead_code))]
const APP_WINDOWS: &str = "Sessiometer";

/// Pure derivation of the Windows config directory,
/// `<local-app-data>\Sessiometer`. Config, state, and logs all live under the
/// LOCAL app-data root — never the Roaming profile.
#[cfg_attr(not(test), allow(dead_code))]
fn windows_config_dir_from(local_app_data: &Path) -> PathBuf {
    local_app_data.join(APP_WINDOWS)
}

/// Pure derivation of the Windows state directory,
/// `<local-app-data>\Sessiometer` — today byte-identical to
/// [`windows_config_dir_from`], kept a separate derivation so either policy
/// can move without silently dragging the other; like every platform, the
/// lock lives here, native-fixed.
#[cfg_attr(not(test), allow(dead_code))]
fn windows_state_dir_from(local_app_data: &Path) -> PathBuf {
    local_app_data.join(APP_WINDOWS)
}

/// Pure derivation of the Windows log directory,
/// `<local-app-data>\Sessiometer\logs`.
#[cfg_attr(not(test), allow(dead_code))]
fn windows_logs_dir_from(local_app_data: &Path) -> PathBuf {
    local_app_data.join(APP_WINDOWS).join("logs")
}

/// The `%LOCALAPPDATA%` root, resolved through `etcetera`'s Windows strategy.
/// On `etcetera` 0.11 it is `cache_dir()` that maps to the LOCAL app-data root
/// (`%LOCALAPPDATA%` when set and non-empty, the
/// `SHGetKnownFolderPath(FOLDERID_LocalAppData)` Known-Folder API as its
/// fallback, a `%USERPROFILE%`-derived last resort); its `config_dir()`/
/// `data_dir()` map to the ROAMING profile and are deliberately not used here
/// (Local, never Roaming). Being env-first, this does NOT yet mirror the Unix
/// `getpwuid`-over-`$HOME` spoof-resistance of [`home_dir`] — hardening to the
/// Known-Folder API alone is pinned on the Windows-enablement work (which is
/// also where this branch first compiles; nothing builds it today).
#[cfg(windows)]
fn windows_local_app_data() -> Result<PathBuf> {
    use etcetera::base_strategy::{BaseStrategy, Windows};
    let strategy = Windows::new().map_err(|_| Error::HomeUnresolved)?;
    Ok(strategy.cache_dir())
}

/// The config directory, at the platform's native location with the XDG
/// opt-in override where the platform honors one:
///
/// - **macOS**: `$XDG_CONFIG_HOME/sessiometer` if set and non-empty, otherwise
///   `~/Library/Application Support/sessiometer` — the long-pinned behavior.
/// - **Linux** (and other non-Apple Unix): `$XDG_CONFIG_HOME/sessiometer` if
///   set to an absolute path, otherwise `~/.config/sessiometer`.
/// - **Windows**: `%LOCALAPPDATA%\Sessiometer` (Local, never Roaming).
pub(crate) fn config_dir() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Ok(apple_config_dir_from(
            &home_dir()?,
            std::env::var_os("XDG_CONFIG_HOME"),
        ))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Ok(xdg_config_dir_from(
            &home_dir()?,
            std::env::var_os("XDG_CONFIG_HOME"),
        ))
    }
    #[cfg(windows)]
    {
        Ok(windows_config_dir_from(&windows_local_app_data()?))
    }
}

/// The `--config` tier of the precedence ladder (issue #24): an explicit
/// directory from the operator wins over both the `$XDG_CONFIG_HOME` opt-in
/// override and the platform-native default, and is taken exactly as given —
/// no `sessiometer` leaf is appended (the operator names the final directory;
/// the env tiers name a parent). The CLI flag itself is not wired
/// yet — argv surface stays with the per-OS daemon UX — so until then this is
/// the resolution seam that wiring lands on; like [`usage_samples`], it is
/// `allow(dead_code)` off the test path.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn config_dir_with_override(flag: Option<&Path>) -> Result<PathBuf> {
    match flag {
        Some(dir) => Ok(dir.to_path_buf()),
        None => config_dir(),
    }
}

/// The `--log` tier of the precedence ladder (issue #24), for [`logs_dir`] —
/// see [`config_dir_with_override`]. There is deliberately NO such seam for
/// [`support_dir`]: the lock/socket/state dir is never overridable, on any
/// platform — machine-global contention (issue #7) breaks the moment an
/// override can split it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn logs_dir_with_override(flag: Option<&Path>) -> Result<PathBuf> {
    match flag {
        Some(dir) => Ok(dir.to_path_buf()),
        None => logs_dir(),
    }
}

/// The config file: `<config_dir>/config.toml` — the daemon's source of truth
/// (roster + tunables), read at start and written by `capture` (issue #3).
pub(crate) fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// The launchd agent's `StandardErrorPath`: `<logs_dir>/daemon.err.log` — where a
/// MANAGED daemon's stderr lands, and therefore where its diagnostic channel
/// (issue #77) lands once `[tunables].verbose` arms it.
///
/// Lifted here by issue #775 because it now has TWO consumers that must agree: the
/// installer renders it into the plist ([`crate::service::install`]) and the reader
/// reads it back (`log --channel diag`). Computed independently in each, a rename in
/// one would leave the other reading a file nothing writes — silently, since an
/// absent diagnostic file is a legitimate state (the knob is off). One function
/// makes that drift unrepresentable.
///
/// Unlike [`crate::observability::log_path`]'s event log, this file is written by
/// LAUNCHD, not by this crate: it is raw process stderr, so it also carries panic
/// payloads and anything else the process writes there. That is why the reader
/// treats it as an UNGOVERNED channel and keeps it strictly opt-in.
pub(crate) fn daemon_stderr_log() -> Result<PathBuf> {
    Ok(logs_dir()?.join("daemon.err.log"))
}

/// The log directory:
///
/// - **macOS**: `~/Library/Logs/sessiometer` (Console.app reads here) — fixed,
///   no env override, the long-pinned behavior.
/// - **Linux** (and other non-Apple Unix): `$XDG_STATE_HOME/sessiometer` if
///   set to an absolute path, otherwise `~/.local/state/sessiometer` — logs
///   are state per the XDG spec ("actions history (logs, history, …)").
/// - **Windows**: `%LOCALAPPDATA%\Sessiometer\logs`.
pub(crate) fn logs_dir() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Ok(apple_logs_dir_from(&home_dir()?))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Ok(xdg_state_dir_from(
            &home_dir()?,
            std::env::var_os("XDG_STATE_HOME"),
        ))
    }
    #[cfg(windows)]
    {
        Ok(windows_logs_dir_from(&windows_local_app_data()?))
    }
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

/// The native-local runtime state directory, **always** at the platform's
/// fixed native location — never an env-var override:
///
/// - **macOS**: `~/Library/Application Support/sessiometer` — even when
///   `$XDG_CONFIG_HOME` redirects [`config_dir`].
/// - **Linux** (and other non-Apple Unix): `~/.local/state/sessiometer` —
///   even when `$XDG_STATE_HOME` redirects [`logs_dir`].
/// - **Windows**: `%LOCALAPPDATA%\Sessiometer`. Caveat: `etcetera`'s resolver
///   is env-first (see `windows_local_app_data`), so the never-overridable
///   invariant is NOT yet delivered on that target — hardening to the
///   Known-Folder API alone is pinned on the Windows-enablement work.
///
/// The daemon's runtime files (the single-instance lock and the control socket)
/// live here rather than under an env-overridable dir so that a second `run`
/// contends on the *same* lock regardless of a per-shell override — the lock's
/// job is to serialize Sessiometer against itself on one machine, which an
/// env-var-relative path would defeat (issue #7); and `flock` on the network
/// filesystem an override could point at is unreliable besides.
pub(crate) fn support_dir() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Ok(apple_support_dir_from(&home_dir()?))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Ok(xdg_state_default_from(&home_dir()?))
    }
    #[cfg(windows)]
    {
        Ok(windows_state_dir_from(&windows_local_app_data()?))
    }
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
/// `$CLAUDE_BIN` if it names an existing file, else the first `claude` found on the
/// **harvested user-level `PATH`** (issue #784 — see [`tier3_path`]). The result is
/// absolute (the spawn pins an absolute binary — a PATH entry may be a wrapper that execs
/// a patched copy, the #101 provenance note), so a caller can validate it once before
/// spawning. [`Error::ClaudeBinaryNotFound`] if neither yields an existing file. Used by
/// the one-shot `poke` (issue #104) and the periodic refresh tick (#105).
pub(crate) async fn claude_binary() -> Result<PathBuf> {
    claude_binary_with_override(None).await
}

/// Resolve the `claude` binary the isolated-refresh engine spawns, honoring the
/// `[refresh].claude_bin` config override (issue #105) ahead of the `$CLAUDE_BIN` /
/// harvested-`PATH` resolution.
///
/// The three tiers, in order — only tier 3 changed in #784:
///
/// 1. `[refresh].claude_bin` — the explicit operator pin (`config_bin`)
/// 2. `$CLAUDE_BIN` — the explicit env override
/// 3. the **harvested user-level `PATH`**, scanned in the user's own order
///
/// `config_bin` is `Some` only when the operator set `[refresh].claude_bin` (an empty value
/// is collapsed to `None` at config-load). When set it WINS and is validated exactly like a
/// `$CLAUDE_BIN` override — absolutized against the current dir, then required to name an
/// existing file — so a configured-but-missing binary is [`Error::ClaudeBinaryNotFound`],
/// never a silent fall-through to a different `claude` (the operator named a specific
/// binary; honor it or fail). A pin also suppresses the harvest entirely: [`tier3_path`]
/// returns `None` before any login shell is spawned.
///
/// **`async` because tier 3 now genuinely does I/O.** The harvest ([`harvest_login_shell_path`])
/// spawns the user's login shell, and the daemon runs on a `current_thread` runtime (ADR-0001)
/// where a `block_on` from inside the runtime panics. Rather than bridge, the resolver itself
/// became `async` — every caller (`refresh_tick`'s `RealRefreshEngine::refresh`, `seams`'
/// `RealKeepWarmEngine::keep_warm`, `poke`, `login`) was already inside an async context, so
/// there is no bridge to build. The PURE policy below ([`claude_binary_from`]) stays sync and
/// argument-threaded; only this composing entry point awaits.
pub(crate) async fn claude_binary_with_override(config_bin: Option<&Path>) -> Result<PathBuf> {
    claude_binary_ambient(config_bin, harvest_login_shell_path).await
}

/// [`claude_binary_with_override`]'s body with the harvest — and ONLY the harvest —
/// injected. The ambient-read seam, added by issue #785.
///
/// Every other input stays genuinely ambient: `$CLAUDE_BIN`, `$PATH` and `cwd` are read
/// from the REAL process environment and the memo is the REAL process-wide static. That is
/// the whole point. The #784 seam below ([`claude_binary_tiered`]) threads those in as
/// arguments, which is right for testing the POLICY but is exactly why the launchd outage
/// was invisible to the suite: every test proved "the resolver works when HANDED a good
/// `PATH`", and none proved "the daemon, running in the environment launchd actually gives
/// it, resolves the binary". A test that re-execs itself under
/// `PATH=/usr/bin:/bin:/usr/sbin:/sbin` needs to drive the daemon's OWN reads rather than a
/// hand-rolled mirror of them, and this is the entry point that lets it.
///
/// `harvest` is the one input that cannot stay ambient, because it is the one a test cannot
/// stage: [`harvest_login_shell_path`] resolves the shell from the PASSWORD DATABASE, so
/// leaving it fixed would make the guard depend on whichever shell the running user happens
/// to have — and, through it, on whether their real `~/.local/bin/claude` exists.
async fn claude_binary_ambient<F, Fut>(config_bin: Option<&Path>, harvest: F) -> Result<PathBuf>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<OsString>>,
{
    claude_binary_tiered(
        config_bin,
        std::env::var_os("CLAUDE_BIN"),
        std::env::var_os("PATH"),
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        &HARVESTED_PATH,
        Instant::now(),
        harvest,
    )
    .await
}

/// The three-tier composition behind [`claude_binary_with_override`], with every ambient
/// input threaded in as an argument — the process env, `cwd`, the memo, the clock and the
/// harvest itself.
///
/// This is the composition seam, and it exists so the tests drive the REAL tier ordering
/// rather than a hand-rolled mirror of it: precedence, the "a pin suppresses the harvest"
/// short-circuit, the harvest-replaces-inherited rule and the degrade-on-failure path are
/// all observable here without mutating process-global env or spawning a login shell.
async fn claude_binary_tiered<F, Fut>(
    config_bin: Option<&Path>,
    claude_bin_env: Option<OsString>,
    inherited: Option<OsString>,
    cwd: &Path,
    memo: &HarvestedPathMemo,
    now: Instant,
    harvest: F,
) -> Result<PathBuf>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<OsString>>,
{
    // Tiers 1 and 2 collapse into one "explicit override" value because they are validated
    // IDENTICALLY (absolutize, then require an existing file); only their precedence differs,
    // and tier 1 winning is expressed by it shadowing the env read here.
    //
    // "Omit OR LEAVE EMPTY to resolve normally": an empty override is NOT an override. The
    // emptiness is filtered at BOTH tiers and BEFORE tier 3 is consulted, so an empty value
    // falls through to the tier below *and* leaves tier 3 reachable — rather than merely
    // failing an existence check with the lower tiers already suppressed. (Config-load
    // collapses an empty `claude_bin` to `None` too; filtering here keeps the resolver's own
    // contract from drifting from it.)
    let explicit = match config_bin.filter(|bin| !bin.as_os_str().is_empty()) {
        Some(bin) => Some(bin.as_os_str().to_owned()),
        None => claude_bin_env,
    }
    .filter(|bin| !bin.is_empty());
    // Tier 3 is consulted only when NO explicit override decided — gated on the EFFECTIVE
    // override rather than on `config_bin` alone. An operator who sets `$CLAUDE_BIN` has named
    // a specific binary just as surely as one who pins `[refresh].claude_bin`, and often does
    // so *because* their login shell is slow, prompts, or is broken — running it anyway would
    // execute their rc files in the daemon's process tree on a timer for a value that is then
    // discarded 100% of the time.
    let path = tier3_path(explicit.as_deref(), memo, now, inherited, harvest).await;
    claude_binary_from(explicit, path, cwd)
}

/// The pure resolution policy, taking the tier-1/2 override, the tier-3 scan `PATH` and
/// `cwd` as arguments so the override / PATH-scan / not-found branches are testable without
/// mutating process-global env. An empty / unset override falls through to the PATH scan; an
/// override that is set but does NOT name an existing file is an error (the operator pointed
/// us at a specific binary — don't silently substitute a different one).
///
/// The scan honors `path`'s ORDER and returns the FIRST match — no re-ranking, no preference
/// among several `claude` binaries. That is the binding #784 constraint: a `claude` the user
/// deliberately shadows earlier on their `PATH` must be the one the daemon spawns, exactly as
/// their own shell would resolve it.
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

// --- Login-shell PATH harvest (issue #783) -------------------------------------
//
// Under launchd the daemon inherits `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, which does
// not contain `~/.local/bin/claude` — so once the daemon moved under launchd (issue
// #171) every automatic refresh failed to resolve a `claude` at all. The daemon must
// spawn the SAME binary the user's terminal would, which means reconstructing the
// user-level PATH that launchd never handed it.
//
// Issue #783 built the harvest capability (`harvest_login_shell_path` and its seams);
// issue #784 feeds it into the resolution chain as tier 3 (`tier3_path`, memoized by
// `HarvestedPathMemo`), which is what gave every function here a production caller.

/// The absolute `env(1)` run inside the login shell to read its environment.
///
/// Absolute per the transport rule (`CONTRIBUTING.md` § "System CLIs, not client
/// crates"): a hijacked `PATH` must not be able to substitute a different binary —
/// which matters doubly here, because the entire purpose of the call is to obtain a
/// `PATH` we do not yet have.
///
/// `env` and NOT `echo $PATH`: fish and nu print `$PATH` as a space-separated LIST, so
/// an `echo`-based harvest would silently corrupt the value on exactly the shells whose
/// users are most likely to have a customized `PATH`. `env` emits the colon-joined
/// variable verbatim regardless of which shell runs it.
const ENV_BIN: &str = "/usr/bin/env";

/// How long the login-shell PATH harvest may run before the child is killed.
///
/// **5 s.** The measured cost of `zsh -l -c /usr/bin/env` on the reference machine is
/// **~38 ms**, so this leaves ~130× headroom — a pathologically slow rc file still
/// finishes comfortably.
///
/// The value is fixed from ABOVE, not below: `[refresh].timeout_secs` bounds the whole
/// refresh cycle and defaults to 90 s ([`crate::config::RefreshConfig`]), emitting
/// `RefreshEventReason::Timeout` when it fires. A harvest allowed to approach that
/// number could hang long enough to trip the cycle bound instead, and the operator
/// would be pointed at the `claude` spawn rather than at their own shell. At 5 s the
/// harvest is 18× below the cycle bound, so a hang always surfaces as a harvest
/// failure. `harvest_bound_stays_far_below_the_refresh_cycle_bound` pins the ratio so a
/// future tuner cannot silently close the gap.
pub(crate) const LOGIN_SHELL_HARVEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Build the login-shell PATH-harvest command — **the single site this spawn's env
/// scrub is applied**, mirroring [`crate::isolated_spawn::SpawnPlan::build_command`].
///
/// `<shell> -l -c /usr/bin/env`: a LOGIN shell (`-l`) because the user's `PATH`
/// additions live in the login profile, and non-interactive (`-c`, never `-i`) because
/// an interactive shell was measured at ~284 ms for a byte-identical result — 7.5× the
/// non-interactive ~38 ms — while additionally sourcing `.zshrc`.
///
/// stdout is captured (it is the answer); stdin is nulled so a shell that reads from it
/// gets EOF instead of blocking the daemon forever; stderr is nulled because rc chatter
/// is noise the parser does not need and must not reach the daemon's own streams.
///
/// The [`SPAWN_ENV_REMOVE`] scrub is applied LAST, exactly as on the `claude` spawn
/// paths and for the same reason, which is sharper here than anywhere else: a login
/// shell sources ARBITRARY user rc files, so it must never inherit ANY name on that
/// list. The names are deliberately NOT repeated here — this sentence spelled out three
/// of them until issue #1009 grew the list to six, and a prose copy of a shared list
/// goes half-complete the moment the list grows, at the seam this same doc calls the
/// sharpest.
/// This command is the THIRD parametrization
/// `isolated_spawn::tests::all_parametrizations_apply_the_full_scrub_set` asserts the
/// complete set against — a dropped entry there is a silent isolation regression here.
pub(crate) fn build_login_shell_env_command(shell: &Path) -> Command {
    let mut command = Command::new(shell);
    command
        .arg("-l")
        .arg("-c")
        .arg(ENV_BIN)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // A child that outlives its bound must not outlive this process: on timeout the
        // `wait_with_output` future is dropped, which drops the `Child`, which kills it.
        .kill_on_drop(true);
    // The security-critical scrub, applied LAST so no earlier `.env` can resurrect a
    // scrubbed var (see [`SPAWN_ENV_REMOVE`]).
    for var in SPAWN_ENV_REMOVE {
        command.env_remove(var);
    }
    command
}

/// Extract the `PATH` value from `/usr/bin/env` output.
///
/// Scans line by line for the FIRST line beginning exactly with `PATH=` and returns the
/// remainder of that line verbatim.
///
/// Byte-oriented, never `str`: a `PATH` entry is an arbitrary OS byte string, and a
/// lossy UTF-8 conversion would silently rewrite a non-UTF-8 directory into replacement
/// characters — the same reason [`home_dir`] builds its result with `OsString::from_vec`.
///
/// Matching an anchored whole-line prefix (rather than splitting each line on `=`) is
/// what makes the awkward cases fall out for free: a value that itself contains `=`
/// (`FOO=a=b`) cannot split wrong, because only the text after the leading `PATH=`
/// is ever taken; a key merely *ending* in `PATH` (`XPATH=`, `MYPATH=`) never matches,
/// because the prefix is anchored at the line start; and rc chatter a login shell
/// prints to stdout is skipped, because it does not carry the prefix.
///
/// `env`'s newline-separated format is inherently ambiguous once a value contains a
/// newline — a continuation line is indistinguishable from a fresh entry. Continuation
/// lines of an ordinary multi-line value are simply skipped (they do not start with
/// `PATH=`), which is the case that matters. A value deliberately embedding a literal
/// `"\nPATH="` could shadow the real entry; that is accepted rather than defended
/// against, because the environment being read is the user's OWN login shell and anyone
/// able to plant such a variable can already replace the `claude` on the user's
/// interactive `PATH` — a daemon-side defense would be stricter than the terminal it
/// exists to imitate.
///
/// An absent `PATH=` line and a present-but-EMPTY one are both errors. An empty `PATH`
/// is not a usable answer, and returning it as success would present a failed harvest
/// as a successful one.
fn path_from_env_output(shell: &Path, output: &[u8]) -> Result<OsString> {
    for line in output.split(|byte| *byte == b'\n') {
        if let Some(value) = line.strip_prefix(b"PATH=") {
            if value.is_empty() {
                return Err(Error::LoginShellPathUnharvested {
                    shell: shell.to_path_buf(),
                    reason: "it reported an empty PATH",
                });
            }
            return Ok(OsString::from_vec(value.to_vec()));
        }
    }
    Err(Error::LoginShellPathUnharvested {
        shell: shell.to_path_buf(),
        reason: "its environment contained no PATH= line",
    })
}

/// Harvest the user-level `PATH` by running the current user's login shell (issue #783).
///
/// The production entry point: resolves the shell from the password database via
/// [`login_shell`] and hands it to [`harvest_path_from`] under
/// [`LOGIN_SHELL_HARVEST_TIMEOUT`]. Wired into the resolution chain as tier 3 by issue
/// #784 — it reaches production through [`tier3_path`], never called directly.
pub(crate) async fn harvest_login_shell_path() -> Result<OsString> {
    harvest_path_from(&login_shell()?, LOGIN_SHELL_HARVEST_TIMEOUT).await
}

/// The parametrized harvest behind [`harvest_login_shell_path`], taking `shell` and the
/// run `bound` as arguments so every branch — success, missing shell, non-zero exit,
/// hang — is testable against a deterministic stand-in without touching process-global
/// state or depending on whichever shell the test host's user happens to have.
///
/// Every failure is [`Error::LoginShellPathUnharvested`], a single variant the caller
/// matches once to degrade non-fatally (issue #375's contract: record it and retry next
/// cycle, never permanently disable the tick). The one exception is a `shell` that is not
/// an ABSOLUTE path, which is [`Error::LoginShellUnresolved`] and short-circuits before
/// any spawn: empty means the passwd entry named nothing to execute, and relative would
/// be `PATH`-resolved by `Command` against the very `PATH` whose absence is the reason
/// this function exists (the transport rule's discipline #1). Both are passwd-entry
/// problems rather than failed harvests, and the gate is repeated here — [`login_shell`]
/// already applies it — because THIS is the boundary that execs.
///
/// The exit status is checked BEFORE the output is parsed, which is load-bearing rather
/// than stylistic: a `nologin`-class shell prints its refusal to stdout and exits
/// non-zero, and a refusal message must never be mistaken for environment output.
async fn harvest_path_from(shell: &Path, bound: Duration) -> Result<OsString> {
    if !shell.is_absolute() {
        return Err(Error::LoginShellUnresolved);
    }
    let child = build_login_shell_env_command(shell).spawn().map_err(|_| {
        // The io error is classified, never embedded: `Display` on it is safe, but the
        // single-variant contract above is what the caller actually needs.
        Error::LoginShellPathUnharvested {
            shell: shell.to_path_buf(),
            reason: "it could not be spawned",
        }
    })?;
    let output = match tokio::time::timeout(bound, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(_)) => {
            return Err(Error::LoginShellPathUnharvested {
                shell: shell.to_path_buf(),
                reason: "it could not be run to completion",
            })
        }
        // The bound fired: the future above is dropped here, and with it the `Child`,
        // whose `kill_on_drop` reaps the hung shell rather than leaking it.
        Err(_) => {
            return Err(Error::LoginShellPathUnharvested {
                shell: shell.to_path_buf(),
                reason: "it did not exit within the harvest timeout",
            })
        }
    };
    if !output.status.success() {
        return Err(Error::LoginShellPathUnharvested {
            shell: shell.to_path_buf(),
            reason: "it exited non-zero without producing an environment",
        });
    }
    path_from_env_output(shell, &output.stdout)
}

// --- Tier-3 PATH memoization (issue #784) --------------------------------------
//
// The harvest above is tier 3's EXPENSIVE half (~38 ms of login shell); scanning its
// result for a `claude` is the cheap half. Only the former is memoized, which is what
// lets tier 3 run per-cycle (issue #375) without paying one login shell per account.

/// How long one harvested `PATH` is reused before the login shell is re-run (issue #784).
///
/// **60 s**, pinned to [`crate::config::DEFAULT_REFRESH_IDLE_AFTER_SECS`] rather than an
/// invented constant: an interval this daemon already treats as meaningful.
///
/// **What is memoized is the PATH STRING — never the resolution.** Cache the stable,
/// expensive thing (running a login shell, measured at ~38 ms); never the volatile, cheap
/// thing (a directory scan for `claude`). That split is exactly what preserves issue #375:
/// the scan still runs EVERY cycle, against the memoized PATH, so a `claude` that appears,
/// moves, or disappears inside an already-known directory is picked up on the very next
/// cycle with no daemon restart. Only the login-shell spawn is amortized, and the TTL
/// bounds even that — a later cycle re-harvests, so a `PATH` the user themselves changes
/// is observed within 60 s, again with no restart. A memo that outlived the process would
/// be a start-up freeze by another name, which is the thing #375 removed.
///
/// **Why a TTL rather than a per-sweep memo.** [`claude_binary_with_override`] serves TWO
/// callers that share one engine — the periodic sweep (issue #105) and the #162 poll-refresh
/// — so a memo scoped to "one sweep" would leave the poll path unbounded. The lifetime is
/// therefore keyed on wall-clock, process-wide, covering both.
///
/// **Cost, for the record.** The sweep resolves once per ACCOUNT, so on the reference
/// 6-account roster the naive wiring would be 6 login-shell spawns per sweep (~230 ms).
/// Memoized it is one (~38 ms) — on the SUCCESS path. A persistently FAILING harvest is
/// still one attempt per account, because a failure is deliberately not cached (below); the
/// worst case is bounded by [`LOGIN_SHELL_HARVEST_TIMEOUT`] × accounts (~30 s on the
/// reference roster) and is accepted, since the alternative — caching the failure — is the
/// self-inflicted outage that tradeoff exists to avoid. Resolution stays CORRECT throughout
/// (it degrades to the inherited `$PATH`); only the cost regresses. At the default 3600 s
/// cadence either number is negligible (~228 ms/hour unmemoized) — this is NOT a hot-path
/// optimization. The TTL exists to bound the POLL path and to keep the cost independent of
/// roster size.
///
/// `pub(crate)` for the doc link alone (`refresh_tick` cites the bound its per-cycle resolve
/// inherits); no code outside this module reads it.
pub(crate) const HARVESTED_PATH_TTL: Duration =
    Duration::from_secs(crate::config::DEFAULT_REFRESH_IDLE_AFTER_SECS);

/// A time-bounded memo of one harvested `PATH`, with failure EXCLUDED from the cache.
///
/// The `Mutex` is `tokio`'s (not `std`'s) because the harvest is awaited while the slot is
/// held: holding it across the await is deliberate, so N concurrent callers perform ONE
/// harvest between them rather than racing N login shells.
struct HarvestedPathMemo {
    /// `Some((harvested_at, path))` once a harvest has succeeded. A FAILED harvest never
    /// writes here — see [`HarvestedPathMemo::get_or_harvest`].
    slot: Mutex<Option<(Instant, OsString)>>,
}

impl HarvestedPathMemo {
    const fn new() -> Self {
        Self {
            slot: Mutex::const_new(None),
        }
    }

    /// The memoized value if it was harvested less than [`HARVESTED_PATH_TTL`] before `now`,
    /// otherwise a fresh `harvest()` — whose result is stored ONLY on success.
    ///
    /// **Failures are never memoized.** Caching a failed harvest would convert a transient
    /// hiccup (a momentarily wedged rc file, a shell mid-upgrade) into a self-inflicted
    /// outage lasting the whole TTL; instead the very next cycle retries. The corollary is
    /// that a still-fresh SUCCESS shields the caller from a transient failure entirely,
    /// because the harvest is not attempted at all while the memo is warm.
    ///
    /// `now` is threaded in rather than read here so the TTL boundary is testable without a
    /// clock or a sleep — the same argument-threading the rest of this module uses.
    async fn get_or_harvest<F, Fut>(&self, now: Instant, harvest: F) -> Result<OsString>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<OsString>>,
    {
        let mut slot = self.slot.lock().await;
        if let Some((harvested_at, path)) = slot.as_ref() {
            if now.saturating_duration_since(*harvested_at) < HARVESTED_PATH_TTL {
                return Ok(path.clone());
            }
        }
        // On failure the `?` returns with the slot UNTOUCHED. A stale entry left behind is
        // inert — it is already past its TTL, so it can never be served — and the next call
        // retries the harvest rather than inheriting a cached error.
        let harvested = harvest().await?;
        *slot = Some((now, harvested.clone()));
        Ok(harvested)
    }
}

/// The process-wide memo backing tier 3 — shared by the periodic sweep, the poll-refresh,
/// the keep-warm engine and the one-shot CLI verbs, so the harvest cost is paid once per
/// [`HARVESTED_PATH_TTL`] for the whole process rather than once per caller.
static HARVESTED_PATH: HarvestedPathMemo = HarvestedPathMemo::new();

/// Tier 3's `PATH` — and the gate on whether tier 3 is consulted at all (issue #784).
///
/// `explicit_override` being `Some` means a HIGHER tier already decided — either
/// `[refresh].claude_bin` or `$CLAUDE_BIN`, after empty values were collapsed away. The
/// operator named a specific binary, so there is no scan (a missing override must ERROR,
/// never fall through to some other `claude`) and therefore no harvest either: an
/// explicitly-pointed daemon never spawns a login shell. That matters beyond the wasted
/// ~38 ms — the override is the documented escape hatch, and an operator who reaches for it
/// because their login shell is slow or broken must not have it run anyway.
///
/// Otherwise the harvested user-level `PATH` REPLACES the process-inherited one. Replaces,
/// never unions: a union would let the launchd-inherited `/usr/bin:/bin:/usr/sbin:/sbin`
/// contribute an entry that outranks the user's own, which is precisely the shadowing the
/// scan order exists to honor. A successful harvest is authoritative — if it yields no
/// `claude`, that is [`Error::ClaudeBinaryNotFound`], not a silent retry against the
/// daemon's `PATH`.
///
/// A FAILED harvest degrades to the inherited `$PATH` — the pre-#784 tier 3. The failure is
/// non-fatal by contract (issue #783) and deliberately silent here: the resolver is a pure
/// policy, and the outcome the operator needs is already surfaced downstream as the sweep's
/// `outcome=error` refresh event. Degrading this way is what makes the change strictly
/// additive on the failure path — it can only add resolutions, never remove one that works
/// today.
async fn tier3_path<F, Fut>(
    explicit_override: Option<&OsStr>,
    memo: &HarvestedPathMemo,
    now: Instant,
    inherited: Option<OsString>,
    harvest: F,
) -> Option<OsString>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<OsString>>,
{
    if explicit_override.is_some() {
        return None;
    }
    match memo.get_or_harvest(now, harvest).await {
        Ok(harvested) => Some(harvested),
        Err(_) => inherited,
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

    use std::collections::BTreeSet;
    use std::ffi::OsStr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn apple_config_prefers_xdg_when_set() {
        let got = apple_config_dir_from(Path::new("/Users/x"), Some(OsString::from("/cfg")));
        assert_eq!(got, PathBuf::from("/cfg/sessiometer"));
    }

    #[test]
    fn apple_config_falls_back_to_library_when_xdg_unset() {
        let got = apple_config_dir_from(Path::new("/Users/x"), None);
        assert_eq!(
            got,
            PathBuf::from("/Users/x/Library/Application Support/sessiometer")
        );
    }

    #[test]
    fn apple_config_falls_back_when_xdg_empty() {
        let got = apple_config_dir_from(Path::new("/Users/x"), Some(OsString::new()));
        assert_eq!(
            got,
            PathBuf::from("/Users/x/Library/Application Support/sessiometer")
        );
    }

    #[test]
    fn macos_resolution_is_pinned_byte_identical() {
        // Issue #24 extended this module to other platforms; the macOS
        // resolution is pinned here byte-for-byte so the extension (and any
        // future one) can never drift it. These are the exact pre-#24 paths.
        let home = Path::new("/Users/x");
        assert_eq!(
            apple_config_dir_from(home, None),
            PathBuf::from("/Users/x/Library/Application Support/sessiometer")
        );
        assert_eq!(
            apple_support_dir_from(home),
            PathBuf::from("/Users/x/Library/Application Support/sessiometer")
        );
        assert_eq!(
            apple_logs_dir_from(home),
            PathBuf::from("/Users/x/Library/Logs/sessiometer")
        );
        // The macOS XDG override predates the spec-strict XDG ladder and keeps
        // its historical any-non-empty acceptance — relative values included.
        assert_eq!(
            apple_config_dir_from(home, Some(OsString::from("rel/cfg"))),
            PathBuf::from("rel/cfg/sessiometer")
        );
    }

    #[cfg(target_os = "macos")]
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

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_live_logs_dir_is_library_logs() {
        // `logs_dir` reads no env on macOS; its tail is fixed (pre-#24 pin).
        let dir = logs_dir().unwrap();
        assert!(
            dir.ends_with("Library/Logs/sessiometer"),
            "macOS logs_dir must be ~/Library/Logs/sessiometer, got {dir:?}"
        );
    }

    /// The launchd stderr path is ONE function with two callers (issue #775): the installer
    /// renders it into the plist, the `log --channel diag` reader reads it back. Computed
    /// separately they could drift, and the drift would be SILENT — an absent diagnostic file is
    /// a legitimate state (the knob is off), so the reader would simply say "no diagnostics yet"
    /// about a file the daemon was busily writing somewhere else.
    #[test]
    fn the_daemon_stderr_log_sits_in_the_log_dir_beside_the_event_log() {
        let path = daemon_stderr_log().unwrap();
        assert_eq!(path.file_name().unwrap(), "daemon.err.log");
        assert_eq!(
            path.parent().unwrap(),
            logs_dir().unwrap(),
            "the reader and the installer must resolve the same directory"
        );
        // Beside, not the same as: the durable event log is a different file with different
        // guarantees, and conflating them would route ungoverned stderr into a metered channel.
        assert_ne!(path, crate::observability::log_path().unwrap());
    }

    // --- Cross-platform strategies (issue #24) ------------------------------

    #[test]
    fn xdg_config_prefers_an_absolute_override() {
        let got = xdg_config_dir_from(Path::new("/home/x"), Some(OsString::from("/cfg")));
        assert_eq!(got, PathBuf::from("/cfg/sessiometer"));
    }

    #[test]
    fn xdg_config_defaults_to_dot_config() {
        let got = xdg_config_dir_from(Path::new("/home/x"), None);
        assert_eq!(got, PathBuf::from("/home/x/.config/sessiometer"));
    }

    #[test]
    fn xdg_config_ignores_an_empty_override() {
        let got = xdg_config_dir_from(Path::new("/home/x"), Some(OsString::new()));
        assert_eq!(got, PathBuf::from("/home/x/.config/sessiometer"));
    }

    #[test]
    fn xdg_config_ignores_a_relative_override() {
        // XDG Base Directory spec: a relative `$XDG_*` value is invalid and
        // ignored (unlike the pinned macOS behavior, which predates this).
        let got = xdg_config_dir_from(Path::new("/home/x"), Some(OsString::from("rel/cfg")));
        assert_eq!(got, PathBuf::from("/home/x/.config/sessiometer"));
    }

    #[test]
    fn xdg_state_prefers_an_absolute_override() {
        let got = xdg_state_dir_from(Path::new("/home/x"), Some(OsString::from("/state")));
        assert_eq!(got, PathBuf::from("/state/sessiometer"));
    }

    #[test]
    fn xdg_state_defaults_to_local_state() {
        let got = xdg_state_dir_from(Path::new("/home/x"), None);
        assert_eq!(got, PathBuf::from("/home/x/.local/state/sessiometer"));
    }

    #[test]
    fn xdg_state_default_never_reads_the_override() {
        // The off-macOS `support_dir` home: the fixed spec default, structurally
        // incapable of following `$XDG_STATE_HOME` — the lock/socket/state dir
        // is never env-overridable on any platform (issue #7).
        let home = Path::new("/home/x");
        assert_eq!(
            xdg_state_default_from(home),
            PathBuf::from("/home/x/.local/state/sessiometer")
        );
        assert_eq!(
            xdg_state_default_from(home),
            xdg_state_dir_from(home, None),
            "the default must be exactly the no-override state dir"
        );
    }

    #[test]
    fn windows_dirs_all_live_under_the_local_sessiometer_root() {
        // Byte-exact Windows separators cannot be asserted on a Unix host (the
        // `\` rendering is the OS's join behavior), so these pin the structure:
        // config and state share `<local-app-data>\Sessiometer`, logs nest one
        // `logs` segment below it, and everything stays under the LOCAL root.
        let local = Path::new(r"C:\Users\x\AppData\Local");
        let config = windows_config_dir_from(local);
        let state = windows_state_dir_from(local);
        let logs = windows_logs_dir_from(local);

        assert_eq!(
            config, state,
            "config and state resolve to the same directory"
        );
        assert!(config.starts_with(local));
        assert_eq!(config.file_name().unwrap(), APP_WINDOWS);
        assert_eq!(logs.parent().unwrap(), config.as_path());
        assert_eq!(logs.file_name().unwrap(), "logs");
    }

    #[test]
    fn explicit_override_wins_over_env_and_native() {
        // The `--config`/`--log` tier of the precedence ladder: an explicit
        // directory short-circuits before any env or native resolution runs.
        let flag = Path::new("/etc/custom-sessiometer");
        assert_eq!(
            config_dir_with_override(Some(flag)).unwrap(),
            PathBuf::from("/etc/custom-sessiometer")
        );
        assert_eq!(
            logs_dir_with_override(Some(flag)).unwrap(),
            PathBuf::from("/etc/custom-sessiometer")
        );
    }

    #[test]
    fn no_override_falls_through_to_the_native_resolution() {
        // Both sides read the same live environment, so the equality holds
        // regardless of any `$XDG_*` the host session carries.
        assert_eq!(
            config_dir_with_override(None).unwrap(),
            config_dir().unwrap()
        );
        assert_eq!(logs_dir_with_override(None).unwrap(), logs_dir().unwrap());
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
        // The login name is the FALLBACK source for the isolated item's `acct`
        // (#102; CC's `$USER`-first derivation is pinned in `keychain`, issue #711).
        // It must resolve to a non-empty value from the password database — never
        // from `$USER`, which the rest of this module must not trust.
        let name = username().unwrap();
        assert!(!name.is_empty());
    }

    #[test]
    fn isolated_refresh_dir_is_native_local_under_refresh() {
        // The isolated CLAUDE_CONFIG_DIR (#102) lives under the native-local support
        // dir, never the XDG-overridable config dir, so its path-hash is stable.
        let dir = isolated_refresh_dir("11111111-1111-1111-1111-111111111111").unwrap();
        // The macOS-literal tail; on other targets the support-relative asserts
        // below carry the same invariant against that platform's fixed state dir.
        #[cfg(target_os = "macos")]
        assert!(dir.ends_with(
            "Library/Application Support/sessiometer/refresh/11111111-1111-1111-1111-111111111111"
        ));
        assert!(dir.starts_with(support_dir().unwrap()));
        assert!(dir.ends_with("refresh/11111111-1111-1111-1111-111111111111"));
    }

    #[test]
    fn isolated_login_dir_is_native_local_under_login() {
        // The isolated login CLAUDE_CONFIG_DIR (#132) is a single fixed leaf under the
        // native-local support dir (not uuid-keyed — the account is unknown until the
        // login completes), so its path-hash names the suffixed isolated item stably.
        let dir = isolated_login_dir().unwrap();
        // The macOS-literal tail; the support-relative asserts below are the
        // target-agnostic form of the same invariant.
        #[cfg(target_os = "macos")]
        assert!(dir.ends_with("Library/Application Support/sessiometer/login"));
        assert!(dir.starts_with(support_dir().unwrap()));
        assert!(dir.ends_with("login"));
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

    #[tokio::test]
    async fn override_prefers_a_present_config_bin() {
        // A `[refresh].claude_bin` pointing at an existing absolute file resolves to it,
        // ahead of any `$CLAUDE_BIN` / harvested PATH (absolute, so cwd-independent).
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("claude");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();
        let got = claude_binary_with_override(Some(&bin)).await.unwrap();
        assert_eq!(got, bin);
    }

    #[tokio::test]
    async fn override_errors_on_a_missing_config_bin() {
        // A configured-but-missing override fails rather than silently scanning a PATH
        // for a different `claude` — the operator named a specific binary.
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("no-such-claude");
        let err = claude_binary_with_override(Some(&missing))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::ClaudeBinaryNotFound));
    }

    // --- Tier-3 resolution against the harvested PATH (issue #784) ----------
    //
    // Every test here drives the REAL composition seam (`claude_binary_tiered`) or the
    // real memo, never a mirror of them, and none touches the ambient process environment
    // or spawns a login shell: the `$CLAUDE_BIN` / `$PATH` values, `cwd`, the clock and the
    // harvest itself are all threaded in as arguments — the same discipline the
    // `claude_binary_from` and `harvest_path_from` suites above already model. Test names
    // carry their issue-#784 T-number so each maps back to the specification.

    /// The launchd environment this whole item exists to fix: the bare `PATH` a
    /// `launchd`-started daemon inherits, which contains no `claude` at all.
    const LAUNCHD_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

    /// Create a directory holding a `claude` file and return the directory, ready to be
    /// `join`ed into a `PATH`. Not marked executable — the resolver's gate is `is_file`.
    fn dir_with_claude(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("claude"), b"#!/bin/sh\n").unwrap();
        dir
    }

    /// Join directories into a `PATH`-shaped value, preserving order.
    fn join(dirs: &[&Path]) -> OsString {
        std::env::join_paths(dirs.iter().map(|d| d.as_os_str())).unwrap()
    }

    /// A stand-in harvest that counts its invocations and yields `outcome`: `Some(path)`
    /// for a success, `None` for the failure #783 reports on an unusable login shell.
    ///
    /// One helper rather than a success/failure pair, so the call count — the property most
    /// of these tests actually assert on — is produced by ONE piece of code either way.
    fn harvest_yielding(
        outcome: Option<OsString>,
        calls: &AtomicUsize,
    ) -> impl FnOnce() -> std::future::Ready<Result<OsString>> + '_ {
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            std::future::ready(outcome.ok_or(Error::LoginShellUnresolved))
        }
    }

    /// Drive `memo` once at `now` against a stand-in harvest yielding `outcome` — the whole
    /// vocabulary of the memoization suite, which asserts on the value served and on how many
    /// harvests it took to serve it.
    async fn memo_get(
        memo: &HarvestedPathMemo,
        now: Instant,
        outcome: Option<&str>,
        calls: &AtomicUsize,
    ) -> Result<OsString> {
        memo.get_or_harvest(now, harvest_yielding(outcome.map(OsString::from), calls))
            .await
    }

    /// Resolve through the real composition seam with a fresh memo and no clock pressure,
    /// returning the resolution alongside how many harvests it took — the shape every
    /// precedence test wants. `harvested` is the stand-in harvest's outcome (`None` = it
    /// fails), threaded through [`harvest_yielding`].
    async fn resolve(
        config_bin: Option<&Path>,
        claude_bin_env: Option<OsString>,
        inherited: Option<OsString>,
        harvested: Option<OsString>,
        cwd: &Path,
    ) -> (Result<PathBuf>, usize) {
        let memo = HarvestedPathMemo::new();
        let calls = AtomicUsize::new(0);
        let got = claude_binary_tiered(
            config_bin,
            claude_bin_env,
            inherited,
            cwd,
            &memo,
            Instant::now(),
            harvest_yielding(harvested, &calls),
        )
        .await;
        (got, calls.load(Ordering::SeqCst))
    }

    // -- Precedence (T1-T7) --------------------------------------------------

    #[tokio::test]
    async fn t1_config_bin_wins_over_claude_bin_env_and_over_a_harvested_match() {
        // Tier 1 beats tiers 2 AND 3 — and, since #784, suppresses the harvest entirely:
        // a pinned daemon must never pay a login-shell spawn it cannot use.
        let tmp = tempfile::tempdir().unwrap();
        let pinned = dir_with_claude(tmp.path(), "pinned").join("claude");
        let env_bin = dir_with_claude(tmp.path(), "env").join("claude");
        let harvested = dir_with_claude(tmp.path(), "harvested");
        let (got, harvests) = resolve(
            Some(&pinned),
            Some(env_bin.as_os_str().to_owned()),
            Some(OsString::from(LAUNCHD_PATH)),
            Some(join(&[&harvested])),
            tmp.path(),
        )
        .await;
        assert_eq!(got.unwrap(), pinned);
        assert_eq!(harvests, 0, "a config pin must not spawn a login shell");
    }

    #[tokio::test]
    async fn t2_claude_bin_env_wins_over_a_harvested_match() {
        let tmp = tempfile::tempdir().unwrap();
        let env_bin = dir_with_claude(tmp.path(), "env").join("claude");
        let harvested = dir_with_claude(tmp.path(), "harvested");
        let (got, harvests) = resolve(
            None,
            Some(env_bin.as_os_str().to_owned()),
            Some(OsString::from(LAUNCHD_PATH)),
            Some(join(&[&harvested])),
            tmp.path(),
        )
        .await;
        assert_eq!(got.unwrap(), env_bin);
        // The same property T1 asserts for tier 1, one tier down: an operator who named a
        // binary must not have their login shell run for a value that is then discarded. The
        // gate is on the EFFECTIVE override, not on `[refresh].claude_bin` alone — `$CLAUDE_BIN`
        // is the documented escape hatch, often reached for BECAUSE the login shell misbehaves.
        assert_eq!(
            harvests, 0,
            "an explicit $CLAUDE_BIN must not spawn a login shell"
        );
    }

    #[tokio::test]
    async fn t3_both_overrides_unset_uses_the_harvested_path() {
        let tmp = tempfile::tempdir().unwrap();
        let harvested = dir_with_claude(tmp.path(), "harvested");
        let (got, harvests) = resolve(
            None,
            None,
            Some(OsString::from(LAUNCHD_PATH)),
            Some(join(&[&harvested])),
            tmp.path(),
        )
        .await;
        assert_eq!(got.unwrap(), harvested.join("claude"));
        assert_eq!(harvests, 1, "tier 3 must actually have been consulted");
    }

    #[tokio::test]
    async fn t4_missing_config_bin_errors_with_no_harvested_substitution() {
        // The operator named a specific binary. A wrong pin must fail LOUDLY — the harvested
        // PATH holding a perfectly good `claude` must not rescue it.
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("no-such-claude");
        let harvested = dir_with_claude(tmp.path(), "harvested");
        let (got, harvests) = resolve(
            Some(&missing),
            None,
            None,
            Some(join(&[&harvested])),
            tmp.path(),
        )
        .await;
        assert!(matches!(got, Err(Error::ClaudeBinaryNotFound)));
        assert_eq!(
            harvests, 0,
            "a pin that fails must still not have spawned a login shell"
        );
    }

    #[tokio::test]
    async fn t5_missing_claude_bin_env_errors_with_no_harvested_substitution() {
        // The same contract one tier down — the pre-#784 behavior, unchanged.
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("no-such-claude");
        let harvested = dir_with_claude(tmp.path(), "harvested");
        let (got, harvests) = resolve(
            None,
            Some(missing.as_os_str().to_owned()),
            None,
            Some(join(&[&harvested])),
            tmp.path(),
        )
        .await;
        assert!(matches!(got, Err(Error::ClaudeBinaryNotFound)));
        assert_eq!(
            harvests, 0,
            "a $CLAUDE_BIN that fails must still not have spawned a login shell"
        );
    }

    #[tokio::test]
    async fn t6_empty_config_bin_falls_through_to_claude_bin_env() {
        // "Omit OR LEAVE EMPTY to resolve normally" — the documented contract. (Config-load
        // collapses an empty value to `None`; this pins the resolver's own behavior for a
        // literal empty string, so the two layers cannot drift apart silently.)
        let tmp = tempfile::tempdir().unwrap();
        let env_bin = dir_with_claude(tmp.path(), "env").join("claude");
        let (got, _) = resolve(
            Some(Path::new("")),
            Some(env_bin.as_os_str().to_owned()),
            None,
            Some(OsString::from(LAUNCHD_PATH)),
            tmp.path(),
        )
        .await;
        assert_eq!(got.unwrap(), env_bin);
    }

    #[tokio::test]
    async fn t7_empty_claude_bin_env_falls_through_to_the_harvested_path() {
        let tmp = tempfile::tempdir().unwrap();
        let harvested = dir_with_claude(tmp.path(), "harvested");
        let (got, _) = resolve(
            None,
            Some(OsString::new()),
            Some(OsString::from(LAUNCHD_PATH)),
            Some(join(&[&harvested])),
            tmp.path(),
        )
        .await;
        assert_eq!(got.unwrap(), harvested.join("claude"));
    }

    // -- Ordering / shadowing: the binding constraint (T8-T11) ---------------

    #[tokio::test]
    async fn t8_the_earlier_harvested_entry_shadows_the_later_one() {
        // "It's important that we catch $PATH-shadowed `claude` binary whenever possible."
        // First on the user's PATH wins — exactly as their own shell resolves it.
        let tmp = tempfile::tempdir().unwrap();
        let first = dir_with_claude(tmp.path(), "first");
        let second = dir_with_claude(tmp.path(), "second");
        let (got, _) = resolve(None, None, None, Some(join(&[&first, &second])), tmp.path()).await;
        assert_eq!(got.unwrap(), first.join("claude"));
    }

    #[tokio::test]
    async fn t9_reversing_the_harvested_order_reverses_the_winner() {
        // The twin of T8 over the SAME two directories: proves the winner tracks ORDER
        // rather than some incidental property (name, mtime, creation order).
        let tmp = tempfile::tempdir().unwrap();
        let first = dir_with_claude(tmp.path(), "first");
        let second = dir_with_claude(tmp.path(), "second");
        let (got, _) = resolve(None, None, None, Some(join(&[&second, &first])), tmp.path()).await;
        assert_eq!(got.unwrap(), second.join("claude"));
    }

    #[tokio::test]
    async fn t10_a_claude_only_on_the_daemon_path_is_not_selected_when_harvest_succeeds() {
        // The harvest REPLACES the inherited `$PATH`; it never unions with it. A union would
        // let a launchd-inherited entry outrank the user's own — defeating the shadowing T8
        // exists to guarantee.
        let tmp = tempfile::tempdir().unwrap();
        let inherited_only = dir_with_claude(tmp.path(), "inherited-only");
        let harvested = dir_with_claude(tmp.path(), "harvested");
        let (got, _) = resolve(
            None,
            None,
            Some(join(&[&inherited_only])),
            Some(join(&[&harvested])),
            tmp.path(),
        )
        .await;
        assert_eq!(got.unwrap(), harvested.join("claude"));
    }

    #[tokio::test]
    async fn t11_empty_entries_in_the_harvested_path_are_skipped() {
        // A trailing / leading / doubled `:` is a legal (and common) `PATH`; the empty entry
        // means "cwd" to some shells, and the existing `is_empty` guard skips it rather than
        // probing `cwd/claude`. Guarded here because a cwd that happens to hold a `claude`
        // would otherwise win over the user's real one.
        let tmp = tempfile::tempdir().unwrap();
        let cwd = dir_with_claude(tmp.path(), "cwd");
        let harvested = dir_with_claude(tmp.path(), "harvested");
        let mut path = OsString::from(":");
        path.push(harvested.as_os_str());
        path.push(":");
        let (got, _) = resolve(None, None, None, Some(path), &cwd).await;
        assert_eq!(got.unwrap(), harvested.join("claude"));
    }

    // -- Fallback (T12-T14) --------------------------------------------------

    #[tokio::test]
    async fn t12_a_failed_harvest_degrades_to_the_daemon_path() {
        // Strictly additive on the failure path: the change can only ADD resolutions, never
        // remove one that works today.
        let tmp = tempfile::tempdir().unwrap();
        let inherited = dir_with_claude(tmp.path(), "inherited");
        let (got, harvests) =
            resolve(None, None, Some(join(&[&inherited])), None, tmp.path()).await;
        assert_eq!(got.unwrap(), inherited.join("claude"));
        assert_eq!(harvests, 1, "the failure must have been a real attempt");
    }

    #[tokio::test]
    async fn t13_a_failed_harvest_with_no_claude_on_the_daemon_path_is_not_found() {
        // T12's other half: the degrade is a fall-BACK, not a rescue — when the daemon's own
        // `$PATH` has no `claude` either, the failure is reported rather than papered over.
        // The inherited `$PATH` is a controlled empty directory rather than [`LAUNCHD_PATH`],
        // both so this stays independent of what the host happens to have in `/usr/bin` and
        // so it does not restate T17, which pins the literal launchd value on purpose.
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        let (got, harvests) = resolve(None, None, Some(join(&[&empty])), None, tmp.path()).await;
        assert!(matches!(got, Err(Error::ClaudeBinaryNotFound)));
        assert_eq!(harvests, 1, "the failure must have been a real attempt");
    }

    #[tokio::test]
    async fn t14_a_successful_harvest_without_claude_does_not_retry_the_daemon_path() {
        // A successful harvest is AUTHORITATIVE. Silently retrying the daemon's `$PATH` here
        // would resurrect the union T10 forbids, so "the user has no `claude`" is reported as
        // such rather than papered over with a launchd-inherited one.
        let tmp = tempfile::tempdir().unwrap();
        let inherited = dir_with_claude(tmp.path(), "inherited");
        let empty = tmp.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        let (got, _) = resolve(
            None,
            None,
            Some(join(&[&inherited])),
            Some(join(&[&empty])),
            tmp.path(),
        )
        .await;
        assert!(matches!(got, Err(Error::ClaudeBinaryNotFound)));
    }

    // -- Regression guards (T15-T18) -----------------------------------------

    #[tokio::test]
    async fn t15_a_symlinked_claude_resolves_to_the_symlink_not_its_target() {
        // Issue #101: a `claude` wrapper on PATH must be spawned AS-IS. `absolutize` performs
        // no canonicalization, and #784 did not change that.
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("claude-real");
        fs::write(&real, b"#!/bin/sh\n").unwrap();
        let dir = tmp.path().join("harvested");
        fs::create_dir_all(&dir).unwrap();
        let link = dir.join("claude");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (got, _) = resolve(None, None, None, Some(join(&[&dir])), tmp.path()).await;
        let got = got.unwrap();
        assert_eq!(got, link);
        assert_ne!(got, real);
    }

    #[tokio::test]
    async fn t16_a_relative_harvested_entry_is_absolutized_against_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let abs = dir_with_claude(tmp.path(), "relbin");
        let (got, _) = resolve(None, None, None, Some(OsString::from("relbin")), tmp.path()).await;
        let got = got.unwrap();
        assert_eq!(got, abs.join("claude"));
        assert!(got.is_absolute());
    }

    #[tokio::test]
    async fn t17_the_literal_launchd_path_without_a_harvest_is_the_production_outage() {
        // The bug, encoded: `PATH=/usr/bin:/bin:/usr/sbin:/sbin` holds no `claude`, so every
        // refresh errored BEFORE any spawn (83/83 `outcome=error window_secs=0`). Named so the
        // regression stays legible; T18 is its inverse.
        let tmp = tempfile::tempdir().unwrap();
        let (got, _) = resolve(
            None,
            None,
            Some(OsString::from(LAUNCHD_PATH)),
            None,
            tmp.path(),
        )
        .await;
        assert!(matches!(got, Err(Error::ClaudeBinaryNotFound)));
    }

    #[tokio::test]
    async fn t18_the_same_launchd_path_with_a_harvest_resolves() {
        // The fix, expressed as T17's inverse over an IDENTICAL inherited `$PATH`: the only
        // difference is that the harvest succeeded.
        let tmp = tempfile::tempdir().unwrap();
        let harvested = dir_with_claude(tmp.path(), "user-local-bin");
        let (got, _) = resolve(
            None,
            None,
            Some(OsString::from(LAUNCHD_PATH)),
            Some(join(&[&harvested])),
            tmp.path(),
        )
        .await;
        assert_eq!(got.unwrap(), harvested.join("claude"));
    }

    // -- Cross-platform (T19) ------------------------------------------------

    /// T19 (per issue #797's premise correction): the resolver must gain NO platform
    /// conditional. There is no Linux CI to catch one — `test` and `msrv` are both
    /// `runs-on: macos-latest` — so the guard is a source assertion rather than a build.
    /// No Linux claim is made or implied by this test.
    #[test]
    fn t19_the_resolver_introduces_no_platform_conditional() {
        let source = include_str!("paths.rs");
        // Bound the window to the resolution chain + harvest section, so an unrelated
        // `cfg(target_os)` elsewhere in this module could never mask a regression here.
        let start = source
            .find("pub(crate) async fn claude_binary()")
            .expect("resolution chain moved — re-anchor this guard");
        let end = source
            .find("pub(crate) fn ensure_private_dir")
            .expect("harvest section moved — re-anchor this guard");
        assert!(
            start < end,
            "the anchors were reordered — re-anchor this guard rather than let it slice backwards"
        );
        let window = &source[start..end];
        assert!(
            !window.contains("target_os"),
            "the tier-3 resolver must stay platform-unconditional (issue #784 AC9)"
        );
    }

    // -- Harvest memoization (T20-T23) ---------------------------------------
    //
    // What is memoized is the PATH STRING, never the resolution — so #375 holds: the
    // directory scan still runs on every call, against the memoized PATH.

    #[tokio::test]
    async fn t20_one_sweep_over_many_accounts_performs_one_harvest() {
        // `resolve_binary` runs once per ACCOUNT. Unmemoized, the reference 6-account roster
        // would spawn 6 login shells per sweep (~230 ms at the measured ~38 ms each). The
        // spawn COUNT is asserted directly; a timing assertion would be flaky.
        let memo = HarvestedPathMemo::new();
        let calls = AtomicUsize::new(0);
        let now = Instant::now();
        for _ in 0..6 {
            let got = memo_get(&memo, now, Some("/user/bin"), &calls)
                .await
                .unwrap();
            assert_eq!(got, OsString::from("/user/bin"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn t21_a_later_sweep_past_the_ttl_re_harvests() {
        // The memo must not become a start-up freeze by another name (issue #375).
        let memo = HarvestedPathMemo::new();
        let calls = AtomicUsize::new(0);
        let first = Instant::now();
        memo_get(&memo, first, Some("/a"), &calls).await.unwrap();
        // Still inside the TTL: served from the memo.
        let inside_ttl = first + HARVESTED_PATH_TTL - Duration::from_secs(1);
        memo_get(&memo, inside_ttl, Some("/a"), &calls)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Past it: re-harvested.
        memo_get(&memo, first + HARVESTED_PATH_TTL, Some("/a"), &calls)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn t22_a_path_change_between_sweeps_is_observed_without_a_restart() {
        // The user edits their profile and the daemon picks it up within one TTL — no restart.
        let memo = HarvestedPathMemo::new();
        let calls = AtomicUsize::new(0);
        let first = Instant::now();
        let before = memo_get(&memo, first, Some("/before"), &calls)
            .await
            .unwrap();
        assert_eq!(before, OsString::from("/before"));
        let after = memo_get(&memo, first + HARVESTED_PATH_TTL, Some("/after"), &calls)
            .await
            .unwrap();
        assert_eq!(after, OsString::from("/after"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn t23_a_failed_harvest_is_not_cached_as_a_permanent_failure() {
        // A cached failure would convert a transient hiccup into a self-inflicted outage
        // lasting the whole TTL. The retry is immediate — no TTL wait — because nothing was
        // written on the failing call.
        let memo = HarvestedPathMemo::new();
        let calls = AtomicUsize::new(0);
        let now = Instant::now();
        let err = memo_get(&memo, now, None, &calls).await.unwrap_err();
        assert!(matches!(err, Error::LoginShellUnresolved));
        let recovered = memo_get(&memo, now, Some("/user/bin"), &calls)
            .await
            .unwrap();
        assert_eq!(recovered, OsString::from("/user/bin"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_claude_appearing_under_a_warm_memo_is_still_found_the_next_cycle() {
        // Issue #375's decisive scenario, end-to-end through the REAL composition — the one
        // thing the memo could plausibly have broken, and the reason what is memoized is the
        // PATH string rather than the resolution.
        //
        // T20-T23 exercise the memo in isolation (they only count harvests) and every
        // precedence test builds a FRESH memo, so each of their resolutions is a first, live
        // one. Neither shape can catch a future refactor that memoizes the RESOLUTION: it
        // would leave the whole suite green while a `claude` installed after the daemon
        // started stayed invisible for a TTL. This is that guard.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("user-local-bin");
        fs::create_dir_all(&dir).unwrap();
        let memo = HarvestedPathMemo::new();
        let calls = AtomicUsize::new(0);
        let now = Instant::now();
        let path = join(&[&dir]);

        // Cycle 1: the directory is on the harvested PATH but holds no `claude` yet.
        let first = claude_binary_tiered(
            None,
            None,
            None,
            tmp.path(),
            &memo,
            now,
            harvest_yielding(Some(path.clone()), &calls),
        )
        .await;
        assert!(matches!(first, Err(Error::ClaudeBinaryNotFound)));

        // Claude Code installs itself into that ALREADY-HARVESTED directory.
        fs::write(dir.join("claude"), b"#!/bin/sh\n").unwrap();

        // Cycle 2, one second later — still deep inside the TTL, so the harvest is NOT
        // re-run. The scan is, and it finds the new binary: no restart, no TTL wait.
        let second = claude_binary_tiered(
            None,
            None,
            None,
            tmp.path(),
            &memo,
            now + Duration::from_secs(1),
            harvest_yielding(Some(path), &calls),
        )
        .await;
        assert_eq!(second.unwrap(), dir.join("claude"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the memo must still have been warm — otherwise this proves nothing about it"
        );
    }

    #[tokio::test]
    async fn a_warm_memo_shields_the_caller_from_a_transient_harvest_failure() {
        // The corollary of T23: while the memo is warm the harvest is not attempted AT ALL,
        // so a login shell that breaks mid-TTL cannot disturb resolution until the TTL lapses.
        let memo = HarvestedPathMemo::new();
        let calls = AtomicUsize::new(0);
        let now = Instant::now();
        memo_get(&memo, now, Some("/user/bin"), &calls)
            .await
            .unwrap();
        let still = memo_get(&memo, now + Duration::from_secs(1), None, &calls)
            .await
            .unwrap();
        assert_eq!(still, OsString::from("/user/bin"));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "harvest was not attempted");
    }

    #[test]
    fn the_harvested_path_ttl_is_pinned_to_the_refresh_idle_floor() {
        // AC11's settled lifetime, asserted against its SOURCE rather than a literal: the TTL
        // is `[refresh].idle_after_secs`'s default, an interval this daemon already treats as
        // meaningful — not an invented constant. Re-pointing one must not silently orphan the
        // other.
        assert_eq!(
            HARVESTED_PATH_TTL,
            Duration::from_secs(crate::config::DEFAULT_REFRESH_IDLE_AFTER_SECS)
        );
        assert_eq!(HARVESTED_PATH_TTL, Duration::from_secs(60));
    }

    // --- Login-shell PATH harvest (issue #783) ------------------------------
    //
    // Split in two, matching the seam: the PARSE tests feed captured `env` output
    // straight to `path_from_env_output` (no spawn at all), and the SPAWN tests drive
    // `harvest_path_from` against deterministic `/bin/sh` stand-ins rather than the
    // test host user's real shell. Neither half mutates the ambient process
    // environment: every value the code under test reads is threaded in as an
    // argument, exactly as the `claude_binary_from` tests above already model, so the
    // suite cannot race another test that happens to touch the same variable.

    /// A stand-in login shell: an executable `/bin/sh` script that IGNORES its
    /// `-l -c /usr/bin/env` arguments and instead runs `body`. That substitution is
    /// what makes the spawn tests hermetic — the harvested bytes are whatever `body`
    /// prints, rather than whatever environment the test runner happens to carry.
    fn fake_shell(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A real `false`(1), which is the concrete `pw_shell` value AC10 names. Merged-`/usr`
    /// systems (macOS, current Debian/Ubuntu) carry `/usr/bin/false`; the `/bin/false`
    /// fallback keeps the test honest on hosts that only have the unmerged location,
    /// rather than silently skipping.
    fn real_false_binary() -> PathBuf {
        for candidate in ["/usr/bin/false", "/bin/false"] {
            let path = PathBuf::from(candidate);
            if path.is_file() {
                return path;
            }
        }
        panic!("no `false` binary at /usr/bin/false or /bin/false");
    }

    fn unharvested_reason(err: &Error) -> &'static str {
        match err {
            Error::LoginShellPathUnharvested { reason, .. } => reason,
            other => panic!("expected LoginShellPathUnharvested, got {other:?}"),
        }
    }

    /// [`path_from_env_output`] against a fixed stand-in shell. Every parse test asserts on
    /// the parsed value or the classified reason and never on which shell was named, so the
    /// `shell` argument is pure carrier for the error payload — naming it once here keeps it
    /// from reading like a variable of the parse.
    fn parse_env_output(output: &[u8]) -> Result<OsString> {
        path_from_env_output(Path::new("/bin/zsh"), output)
    }

    // --- Pure parse (T1-T9): no spawn, captured `env` output only -----------

    /// T1: a well-formed environment among many other variables.
    #[test]
    fn parses_the_path_line_out_of_a_well_formed_environment() {
        let output = b"SHELL=/bin/zsh\nPATH=/opt/homebrew/bin:/usr/bin:/bin\nTERM=xterm\n";
        let got = parse_env_output(output).unwrap();
        assert_eq!(got, OsString::from("/opt/homebrew/bin:/usr/bin:/bin"));
    }

    /// T2: no `PATH=` line at all is a typed error, NOT an empty-string success — the
    /// caller must be able to tell "no answer" from "the answer is nothing".
    #[test]
    fn an_absent_path_line_is_a_typed_error() {
        let err = parse_env_output(b"HOME=/Users/x\nTERM=xterm\n").unwrap_err();
        assert!(matches!(err, Error::LoginShellPathUnharvested { .. }));
        assert_eq!(
            unharvested_reason(&err),
            "its environment contained no PATH= line"
        );
    }

    /// T3: `PATH=` present but EMPTY is likewise no usable answer. Distinct from T2 by
    /// reason string, so an operator can tell an unset PATH from an emptied one.
    #[test]
    fn a_present_but_empty_path_is_not_a_success() {
        let err = parse_env_output(b"HOME=/Users/x\nPATH=\nTERM=xterm\n").unwrap_err();
        assert!(matches!(err, Error::LoginShellPathUnharvested { .. }));
        assert_eq!(unharvested_reason(&err), "it reported an empty PATH");
    }

    /// T4: a value containing `=` must not corrupt the match. Both directions are
    /// checked — an `=`-bearing OTHER variable is skipped, and an `=` inside PATH's own
    /// value survives verbatim (a naive `split('=')` would truncate it).
    #[test]
    fn a_value_containing_an_equals_sign_does_not_corrupt_the_match() {
        let output = b"FOO=a=b\nPATH=/usr/bin:/opt/x=y/bin\nBAR=1\n";
        let got = parse_env_output(output).unwrap();
        assert_eq!(got, OsString::from("/usr/bin:/opt/x=y/bin"));
    }

    /// T5: a value containing a NEWLINE must not cause the following line to be
    /// mis-parsed. `FOO`'s value spans two lines; its continuation carries no `PATH=`
    /// prefix, so it is skipped and the real entry below it is still found.
    #[test]
    fn a_value_containing_a_newline_does_not_misparse_the_next_line() {
        let output = b"FOO=first\nsecond-line-of-foo\nPATH=/usr/bin:/bin\nBAR=1\n";
        let got = parse_env_output(output).unwrap();
        assert_eq!(got, OsString::from("/usr/bin:/bin"));
    }

    /// T6: a key merely ENDING in `PATH` is not the `PATH` variable. The prefix is
    /// anchored at the line start, so `XPATH=` / `MYPATH=` cannot match — and with no
    /// real `PATH` line present the result must be the typed error, never `XPATH`'s value.
    #[test]
    fn a_key_merely_ending_in_path_is_not_matched() {
        let output = b"XPATH=/wrong/one\nMYPATH=/also/wrong\nCLASSPATH=/nope\n";
        let err = parse_env_output(output).unwrap_err();
        assert_eq!(
            unharvested_reason(&err),
            "its environment contained no PATH= line"
        );

        // ...and the real entry is still found when it sits among those decoys.
        let with_real = b"XPATH=/wrong/one\nPATH=/usr/bin\nMYPATH=/also/wrong\n";
        assert_eq!(
            parse_env_output(with_real).unwrap(),
            OsString::from("/usr/bin")
        );
    }

    /// T7: a trailing newline and its absence parse identically — the last line of
    /// `env` output is as valid a carrier as any other.
    #[test]
    fn parses_with_and_without_a_trailing_newline() {
        let with = b"HOME=/Users/x\nPATH=/usr/bin:/bin\n";
        let without = b"HOME=/Users/x\nPATH=/usr/bin:/bin";
        assert_eq!(
            parse_env_output(with).unwrap(),
            OsString::from("/usr/bin:/bin")
        );
        assert_eq!(
            parse_env_output(without).unwrap(),
            OsString::from("/usr/bin:/bin")
        );
    }

    /// T8: completely empty output is a typed error, not an empty success.
    #[test]
    fn empty_output_is_a_typed_error() {
        let err = parse_env_output(b"").unwrap_err();
        assert!(matches!(err, Error::LoginShellPathUnharvested { .. }));
        assert_eq!(
            unharvested_reason(&err),
            "its environment contained no PATH= line"
        );
    }

    /// T9: rc chatter — a login shell printing banners/warnings to stdout — is ignored,
    /// and the `PATH=` line is still found among it. This is the realistic shape of a
    /// customized login profile, not a synthetic edge case.
    #[test]
    fn rc_chatter_is_ignored_and_the_path_is_still_found() {
        let output = b"Welcome back!\n  nvm: using v22\n\nPATH=/usr/local/bin:/usr/bin\ndone.\n";
        let got = parse_env_output(output).unwrap();
        assert_eq!(got, OsString::from("/usr/local/bin:/usr/bin"));
    }

    /// The harvested value is preserved BYTE-for-byte, including bytes that are not
    /// valid UTF-8. A `String`-based parse would lossily rewrite such a directory into
    /// replacement characters and resolve `claude` against a path that does not exist.
    #[test]
    fn a_non_utf8_path_entry_survives_verbatim() {
        let mut output = b"PATH=/usr/bin:/opt/".to_vec();
        output.push(0xff);
        output.extend_from_slice(b"dir/bin\n");
        let got = parse_env_output(&output).unwrap();

        let mut expected = b"/usr/bin:/opt/".to_vec();
        expected.push(0xff);
        expected.extend_from_slice(b"dir/bin");
        assert_eq!(got, OsString::from_vec(expected));
    }

    // --- Spawn (T10-T14, T17-T19) -------------------------------------------

    /// T10: a shell that prints a known environment has its PATH harvested verbatim.
    #[tokio::test]
    async fn harvest_reads_the_path_the_shell_prints() {
        let tmp = tempfile::tempdir().unwrap();
        let shell = fake_shell(
            tmp.path(),
            "shell",
            "printf 'HOME=/Users/x\\nPATH=/harvested/bin:/usr/bin\\nTERM=xterm\\n'",
        );
        let got = harvest_path_from(&shell, Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(got, OsString::from("/harvested/bin:/usr/bin"));
    }

    /// T11: a `pw_shell` naming a file that does not exist is a typed error — the spawn
    /// fails, and it must not panic or propagate a bare io error.
    #[tokio::test]
    async fn a_nonexistent_shell_is_a_typed_error() {
        let err = harvest_path_from(Path::new("/no/such/shell"), Duration::from_secs(10))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::LoginShellPathUnharvested { .. }));
        assert_eq!(unharvested_reason(&err), "it could not be spawned");
    }

    /// T12: a shell that exits non-zero is a typed error even when it printed nothing.
    #[tokio::test]
    async fn a_shell_exiting_non_zero_is_a_typed_error() {
        let tmp = tempfile::tempdir().unwrap();
        let shell = fake_shell(tmp.path(), "shell", "exit 1");
        let err = harvest_path_from(&shell, Duration::from_secs(10))
            .await
            .unwrap_err();
        assert_eq!(
            unharvested_reason(&err),
            "it exited non-zero without producing an environment"
        );
    }

    /// T13 (and the live half of AC3): a shell that never exits is cut off by the bound
    /// and yields the typed error rather than hanging the caller. Driven with a short
    /// injected bound so the suite does not pay the real 5 s; the constant's own
    /// relationship to the refresh cycle is pinned separately by
    /// `harvest_bound_stays_far_below_the_refresh_cycle_bound` (T20).
    #[tokio::test]
    async fn a_hanging_shell_is_cut_off_by_the_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let shell = fake_shell(tmp.path(), "shell", "sleep 120");
        let bound = Duration::from_millis(250);
        // The ceiling sits below `LOGIN_SHELL_HARVEST_TIMEOUT` on purpose: a looser one
        // would also pass if `bound` were ignored and the 5 s constant used instead, so
        // the assertion would no longer prove the bound is the parameter. Still 8x slack
        // over the 250 ms injected value, so a loaded CI runner cannot flake it.
        let ceiling = Duration::from_secs(2);

        let started = std::time::Instant::now();
        let err = harvest_path_from(&shell, bound).await.unwrap_err();
        let elapsed = started.elapsed();

        assert_eq!(
            unharvested_reason(&err),
            "it did not exit within the harvest timeout"
        );
        assert!(
            elapsed < ceiling,
            "harvest must return on the INJECTED bound, took {elapsed:?}"
        );
        assert!(
            ceiling < LOGIN_SHELL_HARVEST_TIMEOUT,
            "this ceiling only discriminates while it stays below the constant"
        );
    }

    /// T14: the child's environment carries none of the scrubbed credential vars —
    /// asserted on the built command, which is where the scrub actually lives and the
    /// only place it can be checked EXHAUSTIVELY. A live child can only ever
    /// demonstrate the absence of variables the ambient environment happened to set,
    /// and setting them would mean mutating process-global state this suite refuses to
    /// touch. The live-child corroboration is the test below; the parity assertion
    /// against the two `claude` spawn plans lives in
    /// `isolated_spawn::tests::all_parametrizations_apply_the_full_scrub_set`.
    #[test]
    fn the_harvest_command_scrubs_the_full_credential_set() {
        let command = build_login_shell_env_command(Path::new("/bin/zsh"));
        let removed: BTreeSet<OsString> = command
            .as_std()
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_os_string())
            .collect();
        let expected: BTreeSet<OsString> = SPAWN_ENV_REMOVE
            .iter()
            .map(|s| OsString::from(*s))
            .collect();
        assert_eq!(
            removed, expected,
            "the login-shell harvest must scrub the full credential/config-override set — \
             a login shell sources arbitrary rc files"
        );
        // The argv shape the whole mechanism rests on: a LOGIN shell, non-interactive,
        // running `env` at its absolute path (never `echo $PATH`).
        let args: Vec<&OsStr> = command.as_std().get_args().collect();
        assert_eq!(
            args,
            [
                OsStr::new("-l"),
                OsStr::new("-c"),
                OsStr::new("/usr/bin/env")
            ]
        );
        assert!(Path::new(ENV_BIN).is_absolute());
    }

    /// T14, live half: a real child prints its real environment, and no scrubbed name
    /// appears among its keys. Deliberately paired with the exhaustive command-level
    /// assertion above rather than replacing it — this half proves the scrub survives an
    /// actual spawn, and its subject is verified non-degenerate (the child really did
    /// emit a populated environment) before the absence claim is made.
    ///
    /// **macOS-only assumption — documented, not `cfg`-gated (issue #797, ADR-0029).**
    /// This spawns the real `/bin/sh -l -c /usr/bin/env` and asserts it succeeded, which
    /// assumes `/bin/sh` accepts `-l` and still emits an environment. macOS's `/bin/sh`
    /// (bash in `sh` mode) does; `dash` — the Debian/Ubuntu `/bin/sh` — does not treat
    /// `-l` the same, so the success assertion could fail there for a reason unrelated to
    /// the scrub under test. It stays a comment rather than a `#[cfg(target_os = "macos")]`
    /// because macOS is the only supported build target: the crate does not compile for
    /// Linux at all, so the gate would be inert today AND would falsely imply the rest of
    /// this suite is portable. A future porter (#26 / #29) re-verifies it against the
    /// target's real `/bin/sh`.
    #[tokio::test]
    async fn a_live_harvest_child_emits_no_scrubbed_variable() {
        // The real shape — `/bin/sh -l -c /usr/bin/env` — so the child's own `env`
        // output is what gets inspected.
        let shell = PathBuf::from("/bin/sh");
        let mut command = build_login_shell_env_command(&shell);
        let output = command.spawn().unwrap().wait_with_output().await.unwrap();
        assert!(output.status.success(), "/bin/sh -l -c env must succeed");

        let keys: Vec<&[u8]> = output
            .stdout
            .split(|byte| *byte == b'\n')
            .filter_map(|line| line.iter().position(|b| *b == b'=').map(|i| &line[..i]))
            .collect();
        // Non-degenerate subject: an empty or unparsed corpus would make the absence
        // assertion below vacuously true, so prove the corpus is real first.
        assert!(
            keys.contains(&b"PATH".as_slice()),
            "the child's environment must be populated for the absence check to mean anything"
        );
        for scrubbed in SPAWN_ENV_REMOVE {
            assert!(
                !keys.contains(&scrubbed.as_bytes()),
                "{scrubbed} must not survive into the harvest child"
            );
        }
    }

    /// T17: the concrete `pw_shell` AC10 names — a real `false`(1), a valid passwd entry
    /// for a service-ish account. It exits non-zero immediately and must degrade to the
    /// typed error, never a hang or a bogus empty PATH treated as success.
    #[tokio::test]
    async fn a_false_login_shell_is_a_typed_error() {
        let err = harvest_path_from(&real_false_binary(), Duration::from_secs(10))
            .await
            .unwrap_err();
        assert_eq!(
            unharvested_reason(&err),
            "it exited non-zero without producing an environment"
        );
    }

    /// T18: a `nologin`-style shell prints its refusal to STDOUT and exits non-zero. The
    /// refusal must not be parsed as environment output — so the stand-in prints text
    /// that WOULD parse as a valid PATH line, proving the exit-status check runs first
    /// and this is not merely passing because the message happened to be unparseable.
    #[tokio::test]
    async fn a_nologin_refusal_on_stdout_is_never_parsed_as_an_environment() {
        let tmp = tempfile::tempdir().unwrap();
        let shell = fake_shell(
            tmp.path(),
            "nologin",
            "printf 'This account is currently not available.\\nPATH=/refused/nonsense\\n'\nexit 1",
        );
        let err = harvest_path_from(&shell, Duration::from_secs(10))
            .await
            .unwrap_err();
        assert_eq!(
            unharvested_reason(&err),
            "it exited non-zero without producing an environment"
        );
        // The parseable-looking refusal really was on stdout — otherwise this test would
        // pass for the wrong reason (nothing to mis-parse in the first place).
        let raw = Command::new(&shell)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap()
            .wait_with_output()
            .await
            .unwrap();
        assert!(path_from_env_output(&shell, &raw.stdout).is_ok());
    }

    /// T19, exec-boundary side: an EMPTY shell is `LoginShellUnresolved` — a passwd-entry
    /// problem, not a failed harvest — and short-circuits before any spawn is attempted.
    /// The passwd-side guard that production actually reaches is covered by
    /// `an_empty_pw_shell_entry_is_unresolved` below.
    #[tokio::test]
    async fn an_empty_shell_is_unresolved_and_never_spawned() {
        let err = harvest_path_from(Path::new(""), Duration::from_secs(10))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::LoginShellUnresolved),
            "an empty pw_shell is a passwd problem, not a harvest failure; got {err:?}"
        );
    }

    // --- The passwd accessors together (T16) + the bound (T20, AC11/AC12) ----

    /// T16: all three passwd-derived accessors return their OWN field, and re-reading in
    /// the reverse order returns identical values. Each copies out of the same
    /// libc-owned static buffer, so a borrow left dangling by one would be clobbered by
    /// the next `getpwuid` — this is the test that would catch it.
    ///
    /// **macOS-only assumption — documented, not `cfg`-gated (issue #797, ADR-0029; same
    /// reasoning as `a_live_harvest_child_emits_no_scrubbed_variable`).** The absoluteness
    /// assertions below read the HOST's live passwd entry and require it to be populated:
    /// an absolute `pw_dir`, an absolute `pw_shell`, a non-empty name. Every macOS account
    /// satisfies that; a minimal Linux container image need not — a uid with no passwd
    /// entry makes `login_shell()` return `LoginShellUnresolved`, and a `nologin`-class
    /// empty `pw_shell` does the same, so the `unwrap()` panics before the absoluteness
    /// claim is even reached. A future porter (#26 / #29) re-verifies it against the
    /// target's passwd database.
    #[test]
    fn every_passwd_accessor_returns_its_own_field() {
        let home_first = home_dir().unwrap();
        let name_first = username().unwrap();
        let shell_first = login_shell().unwrap();
        // Reverse order: an accessor that returned a view into the shared buffer rather
        // than an owned copy would now disagree with itself.
        let shell_second = login_shell().unwrap();
        let name_second = username().unwrap();
        let home_second = home_dir().unwrap();

        assert_eq!(
            home_first, home_second,
            "home_dir clobbered by a later getpw*"
        );
        assert_eq!(
            name_first, name_second,
            "username clobbered by a later getpw*"
        );
        assert_eq!(
            shell_first, shell_second,
            "login_shell clobbered by a later getpw*"
        );

        assert!(!name_first.is_empty());
        assert!(
            home_first.is_absolute(),
            "home must be absolute, got {home_first:?}"
        );
        assert!(
            shell_first.is_absolute(),
            "the login shell must be an absolute path, got {shell_first:?}"
        );
        // Three DISTINCT fields, not one value read three ways.
        assert_ne!(home_first.as_os_str(), shell_first.as_os_str());
        assert_ne!(home_first.as_os_str(), name_first.as_os_str());
        assert_ne!(shell_first.as_os_str(), name_first.as_os_str());
    }

    /// The login shell comes from the password database, NOT `$SHELL` (AC2). Asserted
    /// without touching the ambient environment: `login_shell` takes no input and reads
    /// no env var, so whatever `$SHELL` says cannot reach it — and the value it does
    /// return is a real executable, which a spoofed `$SHELL` need not be.
    ///
    /// **macOS-only assumption — documented, not `cfg`-gated (issue #797, ADR-0029; same
    /// reasoning as `every_passwd_accessor_returns_its_own_field`).** This reads the HOST's
    /// live passwd entry, and it is the STRICTER of the two: `is_file()` demands the named
    /// shell exist on disk, where the sibling only demands `is_absolute()`. A minimal Linux
    /// container image naming an absent `/usr/sbin/nologin` would pass the sibling and fail
    /// here. A future porter (#26 / #29) re-verifies it against the target's passwd database.
    #[test]
    fn the_login_shell_comes_from_the_password_database() {
        let shell = login_shell().unwrap();
        assert!(
            shell.is_file(),
            "pw_shell must name a real executable, got {shell:?}"
        );
        // The passwd answer is independent of `$SHELL`: reading it twice under whatever
        // environment this process carries yields the same value, and the function has
        // no parameter or env read through which `$SHELL` could influence it.
        assert_eq!(shell, login_shell().unwrap());
    }

    /// The production entry point draws its shell from [`login_shell`] — asserted as an
    /// EQUIVALENCE against the explicit composition rather than against a fixed expected
    /// PATH, which keeps it host-independent: the two sides agree whether this machine's
    /// login shell yields a PATH, refuses to run, or has no passwd entry at all. Without
    /// it the entry point would be the one unexercised piece of the mechanism.
    ///
    /// Scope, stated precisely because an overclaiming test comment is exactly the hazard
    /// AC8 exists to prevent: this pins the shell SOURCE, not the bound. A healthy login
    /// shell returns in ~40 ms under any plausible timeout, so no equivalence test can
    /// observe which `Duration` the entry point passed. The bound is pinned separately —
    /// structurally by `harvest_bound_stays_far_below_the_refresh_cycle_bound`, and
    /// behaviorally by `a_hanging_shell_is_cut_off_by_the_bound`.
    #[tokio::test]
    async fn the_entry_point_draws_its_shell_from_the_password_database() {
        let via_entry_point = harvest_login_shell_path().await;
        let via_composition = match login_shell() {
            Ok(shell) => harvest_path_from(&shell, LOGIN_SHELL_HARVEST_TIMEOUT).await,
            Err(err) => Err(err),
        };

        match (&via_entry_point, &via_composition) {
            (Ok(entry), Ok(composed)) => assert_eq!(entry, composed),
            // Errors compare by variant + reason: both sides must fail the SAME way, and
            // the payload is secret-free by construction (see `LoginShellPathUnharvested`).
            (
                Err(Error::LoginShellPathUnharvested { reason: a, .. }),
                Err(Error::LoginShellPathUnharvested { reason: b, .. }),
            ) => assert_eq!(a, b),
            (Err(Error::LoginShellUnresolved), Err(Error::LoginShellUnresolved)) => {}
            (entry, composed) => panic!(
                "the entry point must draw its shell from `login_shell()`, but it diverged \
                 from that composition: {entry:?} vs {composed:?}"
            ),
        }
    }

    /// T19, passwd side: the guard that production actually reaches. `login_shell()`
    /// rejects a `nologin`-class EMPTY `pw_shell` before `harvest_path_from` is ever
    /// called, so testing only the latter's gate would leave the reachable branch
    /// uncovered — the seam exists so the raw passwd bytes can be threaded in.
    #[test]
    fn an_empty_pw_shell_entry_is_unresolved() {
        assert!(matches!(
            login_shell_from(b"").unwrap_err(),
            Error::LoginShellUnresolved
        ));
    }

    /// A RELATIVE `pw_shell` is refused rather than `PATH`-resolved. `Command::new` would
    /// otherwise search `$PATH` for it — resolving the shell against the very `PATH` this
    /// harvest exists because the daemon does not have, and violating the transport rule's
    /// discipline #1 (absolute path, never `$PATH`-resolved) that this module's own
    /// `/usr/bin/env` comment invokes.
    #[test]
    fn a_relative_pw_shell_is_refused_not_path_resolved() {
        assert!(matches!(
            login_shell_from(b"sh").unwrap_err(),
            Error::LoginShellUnresolved
        ));
        assert!(matches!(
            login_shell_from(b"bin/zsh").unwrap_err(),
            Error::LoginShellUnresolved
        ));
        // ...and an absolute one is accepted, so the guard is not simply rejecting all.
        assert_eq!(
            login_shell_from(b"/bin/zsh").unwrap(),
            PathBuf::from("/bin/zsh")
        );
    }

    /// The same refusal at the exec boundary: `harvest_path_from` must not spawn a
    /// relative shell even when handed one directly, because that is the call that execs.
    #[tokio::test]
    async fn a_relative_shell_is_never_spawned() {
        // `sh` really is resolvable on this host's PATH — otherwise the test would pass
        // for the wrong reason (nothing to find), and the guard would be unproven.
        assert!(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .any(|dir| dir.join("sh").is_file()),
            "expected a resolvable `sh` on PATH for this test to be meaningful"
        );
        let err = harvest_path_from(Path::new("sh"), Duration::from_secs(10))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::LoginShellUnresolved),
            "a relative shell must be refused, not PATH-resolved; got {err:?}"
        );
    }

    /// T20 / AC11 / AC12: the harvest bound sits far enough below the whole-cycle
    /// `[refresh].timeout_secs` that a hung harvest can only surface as a HARVEST
    /// failure — never as `RefreshEventReason::Timeout`, which would point an operator
    /// at the `claude` spawn instead of at their own login shell.
    ///
    /// Read off `RefreshConfig::default()` rather than restating 90 s, so retuning the
    /// cycle bound in `config.rs` re-checks this relationship instead of silently
    /// invalidating it.
    #[test]
    fn harvest_bound_stays_far_below_the_refresh_cycle_bound() {
        let cycle = crate::config::RefreshConfig::default().timeout();
        assert!(
            LOGIN_SHELL_HARVEST_TIMEOUT < cycle,
            "harvest bound {LOGIN_SHELL_HARVEST_TIMEOUT:?} must be below the cycle bound {cycle:?}"
        );
        // "Meaningfully shorter" made falsifiable: an order of magnitude of clearance,
        // not a hair. At the shipped values this is 5 s against 90 s (18x).
        assert!(
            LOGIN_SHELL_HARVEST_TIMEOUT * 10 < cycle,
            "harvest bound {LOGIN_SHELL_HARVEST_TIMEOUT:?} leaves too little clearance under \
             the cycle bound {cycle:?} — a hung harvest could be misattributed to reason=timeout"
        );
        // And it is generous against the measured ~38 ms cost the comment cites: two
        // orders of magnitude of headroom for a slow rc file.
        assert!(LOGIN_SHELL_HARVEST_TIMEOUT >= Duration::from_secs(1));
    }

    // --- The literal launchd environment, reproduced (issue #785) -----------
    //
    // Every test above threads the environment in as an ARGUMENT — the right shape for
    // testing the POLICY, and exactly the gap the outage lived in (`claude_binary_ambient`
    // states it in full): the resolver was always correct when GIVEN a good PATH; the daemon
    // was never given one.
    //
    // This section closes it by reproducing the environment for real, in a CHILD PROCESS:
    // the test binary re-execs itself with its environment cleared to the single literal
    // variable
    //
    //     PATH=/usr/bin:/bin:/usr/sbin:/sbin
    //
    // which is the environment measured on the affected machine — no `CLAUDE_BIN`, no
    // `HOME`, nothing else. `apps/menubar/LaunchAgents/org.sessiometer.agent.plist` carries
    // NO `EnvironmentVariables` key, which is *why* the daemon inherits that bare set; the
    // reproduction matches the job's actual (absent-key) shape rather than an invented one.
    //
    // A child process rather than `std::env::set_var`: mutating this process's environment
    // would race the rest of the suite sharing it (other tests read `$PATH` and `$TMPDIR`
    // concurrently), no internal mutex can serialize readers it does not own, and the call
    // is `unsafe` from edition 2024 on. A child's environment, by contrast, is exactly what
    // we hand it — `HOME` is genuinely ABSENT rather than merely overwritten, which is the
    // one thing an in-process approach cannot safely arrange.
    //
    // Nothing here introduces a platform conditional (issue #797's premise correction, now
    // recorded as ADR-0029): no `cfg(target_os)`, and no claim — implicit or explicit —
    // about Linux.
    //
    // Cases carry their issue-#785 T-number, exactly as the tier-3 tests above carry their
    // #784 ones. The two numberings are independent: a bare "T2" in this section is #785's.

    /// The interpreter every staged login shell in this section is written against — and the
    /// one host property the skip gate keys on, since without it no staged shell can run.
    const POSIX_SH: &str = "/bin/sh";

    /// Marks a child invocation AND names the case it must run; absent means "not a child".
    const LAUNCHD_CASE_VAR: &str = "SESSIOMETER_TEST_LAUNCHD_CASE";

    /// Case-name prefix selecting the issue-#802 TERMINAL environment over the launchd one.
    /// The payload dispatches its environment assertion on this, so the two families cannot
    /// assert against each other's `PATH`. Terminal cases carry a `cli` number rather than a
    /// #785-style T-number *because* of this dispatch — and nothing links this constant to
    /// the `"cli1"` / `"cli2"` literals in the match arms, so a new terminal case has to be
    /// named to match it (one that is not fails loudly, against the wrong `PATH`).
    const TERMINAL_CASE_PREFIX: &str = "cli";

    /// Carries the parent's staged fixture root into the child.
    const LAUNCHD_FIXTURE_VAR: &str = "SESSIOMETER_TEST_LAUNCHD_FIXTURE";

    /// The `#[ignore]`d child payload, named once so the re-exec filter and the function it
    /// selects cannot drift apart silently.
    const LAUNCHD_CHILD_FN: &str = "launchd_env_child_payload";

    /// Printed by the child once a case's assertions have all run, and asserted by the
    /// parent — see [`run_child_case`] for why an exit code alone is not evidence.
    const LAUNCHD_CASE_OK: &str = "launchd-case-ok:";

    /// Printed instead of running, when the mechanism cannot apply.
    const LAUNCHD_SKIP: &str = "launchd-case-skipped:";

    /// The fixture's stand-in for `~/.local/bin`: the directory holding the `claude` that is
    /// reachable ONLY through the harvest, never from the child's own `$PATH`.
    const FIXTURE_HARVESTED_DIR: &str = "user-local-bin";
    /// The fixture's stand-in for the `/custom/bin` of `PATH=/custom/bin:$PATH` (issue #802):
    /// a SECOND `claude`, reachable only from the child's own inherited `$PATH` and never
    /// through the harvest — the mirror image of [`FIXTURE_HARVESTED_DIR`]. Inert for the
    /// launchd cases, whose `PATH` is the bare [`LAUNCHD_PATH`] and so never names the
    /// fixture root at all.
    const FIXTURE_SHELL_LOCAL_DIR: &str = "shell-local-bin";
    /// The fixture login shell whose harvest SUCCEEDS, yielding [`FIXTURE_HARVESTED_DIR`].
    const FIXTURE_LOGIN_SHELL: &str = "login-shell";
    /// The fixture login shell whose harvest FAILS — the negative control's mechanism.
    const FIXTURE_BROKEN_LOGIN_SHELL: &str = "broken-login-shell";
    /// The environment dump the fixture login shell prints, staged as a FILE so a tempdir
    /// path never has to be interpolated into shell source.
    const FIXTURE_HARVESTED_ENV: &str = "harvested-env";

    /// Stage the child fixture and return its (parent-owned) root.
    ///
    /// Every `claude` staged here lives under the fixture root — never on
    /// `/usr/bin:/bin:/usr/sbin:/sbin`, and never the developer's real
    /// `~/.local/bin/claude`. That is what makes the guard deterministic on CI and on any
    /// contributor's machine. There are TWO of them, reachable by disjoint routes:
    /// [`FIXTURE_HARVESTED_DIR`] only through the harvest, and [`FIXTURE_SHELL_LOCAL_DIR`]
    /// only through the child's own `$PATH` (issue #802). The launchd cases see just the
    /// first, because their `PATH` never names the fixture root.
    ///
    /// The login shell prints a staged FILE rather than an interpolated string, so no
    /// tempdir path ever needs shell-quoting: `${0%/*}` is POSIX parameter expansion (the
    /// script's own directory) and `/bin/cat` is absolute, per the transport rule.
    fn stage_child_fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let harvested = dir_with_claude(tmp.path(), FIXTURE_HARVESTED_DIR);
        // The issue-#802 half: a second `claude` reachable ONLY from the child's own
        // inherited `$PATH`. Staged unconditionally — it is unreachable from the launchd
        // cases' bare `PATH`, and it is what makes the terminal cases' "not selected" a real
        // shadowing rather than an assertion over a directory nothing could have found.
        dir_with_claude(tmp.path(), FIXTURE_SHELL_LOCAL_DIR);
        // Shaped like a real `env` dump — other variables around the `PATH=` line, including
        // a `HOME` the harvest must ignore — so the real parser does real work.
        let mut dump = OsString::from("SHELL=/bin/zsh\nHOME=/harvest/must/ignore/this\nPATH=");
        dump.push(harvested.as_os_str());
        dump.push("\nTERM=xterm\n");
        fs::write(tmp.path().join(FIXTURE_HARVESTED_ENV), dump.into_vec()).unwrap();
        fake_shell(
            tmp.path(),
            FIXTURE_LOGIN_SHELL,
            &format!("/bin/cat \"${{0%/*}}/{FIXTURE_HARVESTED_ENV}\""),
        );
        fake_shell(tmp.path(), FIXTURE_BROKEN_LOGIN_SHELL, "exit 1");
        tmp
    }

    /// The child payload's libtest name. libtest names a test by its module path with the
    /// CRATE ROOT stripped, so this strips it too.
    ///
    /// A drifted name would make the re-exec filter select NOTHING — and libtest exits 0 on
    /// an empty selection, which is a silently green, entirely degenerate gate. That is why
    /// [`run_child_case`] asserts the child's completion marker and not merely its status.
    fn launchd_child_test_name() -> String {
        let module = module_path!();
        let module = module.split_once("::").map_or(module, |(_, rest)| rest);
        format!("{module}::{LAUNCHD_CHILD_FN}")
    }

    /// Emit a skip reason that libtest's default output capture cannot swallow.
    ///
    /// `println!` and `eprintln!` route through the thread-local capture libtest installs, so
    /// on a PASSING test their output is discarded unless `--nocapture` is passed — which
    /// would make a skip indistinguishable from a real pass, the exact "a silently-skipped
    /// test is a degenerate gate" outcome the skip requirement exists to prevent. A write
    /// straight to the `stderr` HANDLE goes to fd 2 and is always seen. (Measured both ways:
    /// the macro output vanishes on a passing test, the handle write does not.)
    fn announce_skip(reason: &str) {
        let _ = writeln!(std::io::stderr(), "{LAUNCHD_SKIP} {reason}");
    }

    /// The `PATH` a launchd case runs under: the bare inherited set, verbatim.
    fn launchd_path(_fixture: &Path) -> OsString {
        OsString::from(LAUNCHD_PATH)
    }

    /// The `PATH` a terminal case runs under: [`FIXTURE_SHELL_LOCAL_DIR`] PREPENDED to the
    /// same bare set — the literal `PATH=/custom/bin:$PATH` shape issue #802 is about.
    ///
    /// The tail is [`LAUNCHD_PATH`] rather than the runner's real `$PATH` so the child's
    /// entire search space stays enumerable: exactly two `claude` binaries can decide the
    /// outcome (the shell-local one and the harvested one), and neither the CI image nor a
    /// contributor's machine can contribute a third.
    fn shell_local_prefixed_path(fixture: &Path) -> OsString {
        let mut path = fixture.join(FIXTURE_SHELL_LOCAL_DIR).into_os_string();
        path.push(":");
        path.push(LAUNCHD_PATH);
        path
    }

    /// Run one launchd case — the child's `PATH` is the bare inherited set (see
    /// [`run_child_case`], which this and [`run_terminal_case`] both wrap).
    fn run_launchd_case(case: &str, with_claude_bin: bool) {
        run_child_case(case, launchd_path, with_claude_bin);
    }

    /// Run one issue-#802 terminal case — the child's `PATH` LEADS with a directory holding
    /// a `claude`, reproducing `PATH=/custom/bin:$PATH sessiometer poke`.
    ///
    /// No `with_claude_bin` parameter: an override decides at tier 2 and short-circuits the
    /// harvest, so it could only make these cases prove less than they do.
    fn run_terminal_case(case: &str) {
        run_child_case(case, shell_local_prefixed_path, false);
    }

    /// Run one child case: re-exec THIS test binary, with its environment cleared to exactly
    /// the set `child_path` describes, running only the `#[ignore]`d child payload.
    ///
    /// `child_path` builds the child's `PATH` from the staged fixture root — the one input
    /// that cannot be a constant, since the terminal cases prepend a fixture directory whose
    /// path is not known until it is staged.
    ///
    /// `with_claude_bin` additionally exports `$CLAUDE_BIN` pointing at the fixture binary —
    /// the tier-2 escape hatch, which only one case exercises.
    fn run_child_case(case: &str, child_path: fn(&Path) -> OsString, with_claude_bin: bool) {
        // A parent test running INSIDE a child invocation would re-exec again, forever.
        // `--exact` plus `--ignored` already makes that unreachable; failing loudly here
        // means a future drift in either flag surfaces as one clear panic, not a fork bomb.
        assert!(
            std::env::var_os(LAUNCHD_CASE_VAR).is_none(),
            "{LAUNCHD_CASE_VAR} is set — a parent test is running inside a child invocation"
        );
        if !Path::new(POSIX_SH).is_file() {
            // SKIPPED, never failed: a host with no POSIX shell cannot run the staged login
            // shell, which is a property of the host rather than of the resolver.
            announce_skip(&format!(
                "{case}: no POSIX shell at {POSIX_SH}, so the staged login shell cannot run \
                 on this host"
            ));
            return;
        }
        let fixture = stage_child_fixture();
        let mut command = std::process::Command::new(
            std::env::current_exe().expect("the test binary must know its own path"),
        );
        command
            .arg(launchd_child_test_name())
            .arg("--exact")
            .arg("--ignored")
            .arg("--nocapture")
            .arg("--test-threads=1")
            // THE reproduction. After `env_clear` the child's ENTIRE environment is what
            // follows: the `PATH` this case runs under, plus the two variables telling the
            // payload which case to run and where its fixture lives. No `HOME`, no
            // `CLAUDE_BIN` unless the case asks for one, nothing else.
            .env_clear()
            .env("PATH", child_path(fixture.path()))
            .env(LAUNCHD_CASE_VAR, case)
            .env(LAUNCHD_FIXTURE_VAR, fixture.path())
            // `cwd` is the other genuinely-ambient input [`claude_binary_ambient`] reads, so
            // it is reproduced too rather than inherited from cargo. The plist carries no
            // `WorkingDirectory` key either, so a launchd-started daemon runs at `/`.
            .current_dir("/");
        if with_claude_bin {
            command.env(
                "CLAUDE_BIN",
                fixture.path().join(FIXTURE_HARVESTED_DIR).join("claude"),
            );
        }
        let output = command
            .output()
            .unwrap_or_else(|err| panic!("could not re-exec the test binary for {case}: {err}"));
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "the launchd child failed for {case} ({}):\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
            output.status
        );
        // Non-degenerate subject: libtest exits 0 when its filter selects NOTHING, so a
        // renamed payload or a drifted module path would otherwise read as a pass. The marker
        // is printed by the case body itself, only after every assertion in it has run.
        assert!(
            stdout.contains(&format!("{LAUNCHD_CASE_OK} {case}")),
            "the launchd child exited 0 for {case} without completing the case — the filter \
             `{}` may have selected no test:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
            launchd_child_test_name()
        );
    }

    /// The fixture root the parent staged (child side).
    fn launchd_fixture_root() -> PathBuf {
        PathBuf::from(
            std::env::var_os(LAUNCHD_FIXTURE_VAR).expect("the parent must pass the fixture root"),
        )
    }

    /// Assert the child really is running under the reproduced LAUNCHD environment. Every
    /// launchd case runs this FIRST, so no such case can assert against an environment that
    /// was not actually reduced to `PATH=/usr/bin:/bin:/usr/sbin:/sbin`. The issue-#802
    /// terminal cases run [`assert_terminal_environment`] instead — the dispatch lives in
    /// [`launchd_env_child_payload`], so neither family can skip its own check.
    fn assert_launchd_environment() {
        assert_eq!(
            std::env::var_os("PATH").as_deref(),
            Some(OsStr::new(LAUNCHD_PATH)),
            "the child must run under the literal launchd PATH"
        );
        assert!(
            std::env::var_os("HOME").is_none(),
            "the launchd environment carries no HOME — the passwd discipline in `home_dir` \
             exists for exactly this"
        );
    }

    /// Assert the child really is running under the reproduced TERMINAL environment (issue
    /// #802): a `PATH` whose FIRST entry holds a `claude`, exactly as `PATH=/custom/bin:$PATH`
    /// produces. The terminal cases run this FIRST for the same reason the launchd cases run
    /// [`assert_launchd_environment`] — no case may assert against an environment that was
    /// not actually reproduced.
    ///
    /// The tail check is what bounds the search space: with everything after the prefix
    /// pinned to [`LAUNCHD_PATH`], the only two `claude` binaries the child can possibly
    /// resolve are the two the fixture staged.
    fn assert_terminal_environment(shell_local_dir: &Path) {
        let path = std::env::var_os("PATH").expect("the child must run under a PATH");
        let mut entries = std::env::split_paths(&path);
        assert_eq!(
            entries.next().as_deref(),
            Some(shell_local_dir),
            "the shell-local directory must be the FIRST `PATH` entry — that is what \
             `PATH=/custom/bin:$PATH` produces, and what the resolver must decline to consult"
        );
        assert!(
            shell_local_dir.join("claude").is_file(),
            "the shell-local `claude` must exist, or 'it was not selected' proves nothing"
        );
        assert_eq!(
            entries.collect::<Vec<_>>(),
            std::env::split_paths(LAUNCHD_PATH).collect::<Vec<_>>(),
            "everything after the prefix must be the bare launchd set, so nothing this host \
             happens to carry can decide the outcome"
        );
    }

    /// Resolve exactly as production does — ambient `$CLAUDE_BIN`, `$PATH` and `cwd`, and the
    /// real process-wide memo — with only the login shell staged, because that one input
    /// comes from the password database and cannot be staged any other way.
    ///
    /// The daemon and the CLI verbs share this entry point (`poke` via
    /// [`claude_binary`], `login` via [`claude_binary_with_override`]), which is exactly the
    /// ADR-0030 property the issue-#802 cases below exercise — so the helper is named for the
    /// seam it drives rather than for either environment it is driven under.
    async fn resolve_ambient(config_bin: Option<&Path>, shell: &Path) -> Result<PathBuf> {
        claude_binary_ambient(config_bin, || {
            harvest_path_from(shell, LOGIN_SHELL_HARVEST_TIMEOUT)
        })
        .await
    }

    /// The child half of the launchd guard, `#[ignore]`d because it is meaningless outside a
    /// re-exec: the PARENT is what supplies the reduced environment, so an ordinary
    /// `cargo test` run must not select it.
    ///
    /// One payload dispatching on the case rather than one `#[ignore]`d test per case: a
    /// hand-run `cargo test -- --ignored` would otherwise execute several tests with no
    /// parent environment behind them, and the "not a child" branch would have to be
    /// repeated in each.
    #[tokio::test]
    #[ignore = "child half of the re-exec environment guards — driven by run_child_case"]
    async fn launchd_env_child_payload() {
        let Some(case) = std::env::var_os(LAUNCHD_CASE_VAR) else {
            // Reachable only by running the ignored set by hand — which means WITHOUT the
            // `--nocapture` the parent passes, so this has to go through [`announce_skip`]'s
            // handle write for the reason documented there. There is nothing to assert
            // without the parent's environment, so say so rather than pass silently.
            announce_skip(&format!(
                "(no case): {LAUNCHD_CASE_VAR} is unset — this payload is driven by the \
                 launchd parent tests, never run directly"
            ));
            return;
        };
        let case = case.to_str().expect("case names are ASCII").to_owned();

        let fixture = launchd_fixture_root();
        let harvested_dir = fixture.join(FIXTURE_HARVESTED_DIR);
        let fixture_claude = harvested_dir.join("claude");
        let shell_local_dir = fixture.join(FIXTURE_SHELL_LOCAL_DIR);
        let shell_local_claude = shell_local_dir.join("claude");
        let good_shell = fixture.join(FIXTURE_LOGIN_SHELL);
        let broken_shell = fixture.join(FIXTURE_BROKEN_LOGIN_SHELL);

        // Every case asserts the environment it was actually given, FIRST and before
        // anything else — the launchd cases the bare `PATH` with no `HOME`, the issue-#802
        // terminal cases the shell-local-prefixed one. Dispatching here rather than per-case
        // keeps that unconditional: a case cannot forget to check what it is running under.
        if case.starts_with(TERMINAL_CASE_PREFIX) {
            assert_terminal_environment(&shell_local_dir);
        } else {
            assert_launchd_environment();
        }

        match case.as_str() {
            // T1: the fix under the real environment. `PATH=/usr/bin:/bin:/usr/sbin:/sbin`
            // holds no `claude` at all, yet the one reachable via the harvested user PATH
            // resolves. This is the assertion that fails on pre-#784 `main`.
            "t1" => {
                assert!(
                    std::env::var_os("CLAUDE_BIN").is_none(),
                    "t1 must resolve through the harvest, never through an override"
                );
                let got = resolve_ambient(None, &good_shell)
                    .await
                    .expect("a harvested `claude` must resolve under the launchd environment");
                assert_eq!(got, fixture_claude);
            }
            // T2: the negative control, and the reason a green result here MEANS something.
            // The only difference from T1 is that the harvest FAILS: the same fixture
            // `claude` is on disk, simply unreachable from the launchd PATH, so resolution
            // reports `ClaudeBinaryNotFound` — the current-`main` behavior, pinned as the
            // regression baseline. Stub the harvest out and T1 collapses into this outcome
            // and fails; without this case a no-op harvest would leave the suite green.
            "t2" => {
                assert!(
                    fixture_claude.is_file(),
                    "the fixture `claude` must exist, or T2 proves nothing about the harvest"
                );
                for dir in std::env::split_paths(LAUNCHD_PATH) {
                    let candidate = dir.join("claude");
                    assert!(
                        !candidate.is_file(),
                        "this host has a `claude` at {candidate:?}, on the literal launchd \
                         PATH — the negative control cannot hold here"
                    );
                }
                let err = resolve_ambient(None, &broken_shell)
                    .await
                    .expect_err("a failed harvest under the launchd PATH must not resolve");
                assert!(matches!(err, Error::ClaudeBinaryNotFound), "got {err:?}");
            }
            // T3: the documented escape hatch, through the REAL production entry point —
            // `claude_binary_with_override`, harvest and all. With `$CLAUDE_BIN` set, tier 2
            // decides and `tier3_path` short-circuits before any login shell is spawned, so
            // this case never touches the running user's shell. It is what an operator would
            // be told to do when the harvest cannot work for them.
            "t3" => {
                let claude_bin =
                    std::env::var_os("CLAUDE_BIN").expect("the parent must set $CLAUDE_BIN");
                let got = claude_binary_with_override(None)
                    .await
                    .expect("$CLAUDE_BIN must resolve under the launchd environment");
                assert_eq!(got, PathBuf::from(&claude_bin));
                assert_eq!(got, fixture_claude);
            }
            // T4: the tier-1 `[refresh].claude_bin` pin, likewise through the REAL entry
            // point and likewise harvest-free.
            "t4" => {
                assert!(
                    std::env::var_os("CLAUDE_BIN").is_none(),
                    "t4 exercises tier 1; a `$CLAUDE_BIN` would confuse which tier answered"
                );
                let got = claude_binary_with_override(Some(&fixture_claude))
                    .await
                    .expect("a config pin must resolve under the launchd environment");
                assert_eq!(got, fixture_claude);
            }
            // T5: `HOME` is genuinely ABSENT (asserted above, and true by construction — the
            // child's environment is `env_clear()` plus what the parent set). The
            // password-database discipline this module is built on must hold anyway, and
            // resolution must still work.
            "t5" => {
                let home =
                    home_dir().expect("getpwuid must resolve the home dir with $HOME absent");
                assert!(home.is_absolute(), "got {home:?}");
                let name = username().expect("getpwuid must resolve the user with $USER absent");
                assert!(!name.is_empty());
                let got = resolve_ambient(None, &good_shell)
                    .await
                    .expect("resolution must survive an absent $HOME");
                assert_eq!(got, fixture_claude);
            }
            // T6: the harvest genuinely produced a DIFFERENT `PATH` than the launchd one.
            // Without this, T1 could pass by accident — a harvest that silently echoed the
            // inherited value would still "resolve", just never through the user's own PATH.
            "t6" => {
                let harvested = harvest_path_from(&good_shell, LOGIN_SHELL_HARVEST_TIMEOUT)
                    .await
                    .expect("the staged login shell must harvest");
                assert_eq!(harvested, OsString::from(harvested_dir.as_os_str()));
                assert_ne!(
                    harvested,
                    OsString::from(LAUNCHD_PATH),
                    "the harvested PATH must DIFFER from the launchd PATH, or T1 could pass \
                     by accident"
                );
                assert_ne!(
                    std::env::var_os("PATH"),
                    Some(harvested),
                    "the harvest must not merely echo the inherited environment"
                );
            }
            // CLI1 (issue #802) — the CLI delta, reproduced and pinned. The child's OWN
            // `$PATH` LEADS with a directory holding a `claude` (the literal
            // `PATH=/custom/bin:$PATH sessiometer poke` shape) while the harvest succeeds
            // with a DIFFERENT one. The HARVESTED binary must win: a successful harvest
            // REPLACES the inherited `$PATH` rather than unioning with it, so a shell-local
            // prefix is never consulted.
            //
            // THE discriminating case for ADR-0030. Reintroduce the inherited `$PATH` as a
            // tier-3 probe — alternative 1, "try inherited first, harvest only on a miss" —
            // and this fails outright: the shell-local `claude` sits FIRST on `$PATH`, so an
            // inherited-first ladder returns it. A case that merely asserted "a `claude`
            // resolves" would stay green under that change and would pin nothing at all.
            "cli1" => {
                assert!(
                    std::env::var_os("CLAUDE_BIN").is_none(),
                    "cli1 must be decided at tier 3, never by an override"
                );
                assert_ne!(
                    shell_local_claude, fixture_claude,
                    "the two staged binaries must be DISTINCT, or the winner is unreadable"
                );
                let got = resolve_ambient(None, &good_shell)
                    .await
                    .expect("a harvested `claude` must resolve under a terminal `PATH`");
                assert_eq!(
                    got, fixture_claude,
                    "the harvested `claude` must outrank the shell-local one"
                );
                assert_ne!(
                    got, shell_local_claude,
                    "a shell-local `PATH` entry must NOT be consulted when the harvest \
                     succeeds (issue #802, ADR-0030) — resolving to it means tier 3 probed \
                     the inherited `$PATH`"
                );
            }
            // CLI2 (issue #802) — the negative control, and what makes CLI1's green mean
            // something. Same environment, same two staged binaries; the ONLY difference is
            // that the harvest FAILS. Resolution then degrades to the inherited `$PATH` and
            // finds the shell-local `claude` — proving that binary was reachable all along,
            // so CLI1's "not selected" is a real shadowing rather than an assertion over a
            // directory nothing could have found.
            //
            // It also pins the degrade itself at the AMBIENT seam: T12 proves it against
            // threaded-in arguments, this proves it against the real process environment.
            "cli2" => {
                let got = resolve_ambient(None, &broken_shell)
                    .await
                    .expect("a failed harvest must degrade to the inherited `$PATH`");
                assert_eq!(
                    got, shell_local_claude,
                    "a FAILED harvest degrades to the inherited `$PATH`, where the \
                     shell-local `claude` is the first match"
                );
            }
            other => panic!("unknown child case {other:?}"),
        }

        println!("{LAUNCHD_CASE_OK} {case}");
    }

    /// T1 — under `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, with no `CLAUDE_BIN` and no `HOME`, a
    /// `claude` reachable only via the harvested user PATH RESOLVES. The launchd outage,
    /// inverted: this is what pre-#784 `main` could not do.
    ///
    /// The name spells the `PATH` out instead of naming it, which is deliberate (AC5): ONE
    /// test here has to carry the literal environment, so that a `cargo test` line on its own
    /// tells a reader what is being reproduced. Don't shorten it to match its siblings.
    #[test]
    fn launchd_env_usr_bin_bin_usr_sbin_sbin_resolves_a_harvested_claude() {
        run_launchd_case("t1", false);
    }

    /// T2 — the negative control. Same environment, same fixture, harvest FAILING: no
    /// `claude` is reachable from `/usr/bin:/bin:/usr/sbin:/sbin`, so resolution is
    /// `ClaudeBinaryNotFound`. This is the current-`main` behavior pinned as the baseline, and
    /// it is what makes T1's green non-degenerate.
    #[test]
    fn launchd_env_without_a_working_harvest_is_claude_binary_not_found() {
        run_launchd_case("t2", false);
    }

    /// T3 — `$CLAUDE_BIN` resolves through the real production entry point under
    /// `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, without needing a harvest at all: the documented
    /// escape hatch actually works under launchd.
    #[test]
    fn launchd_env_with_claude_bin_resolves_via_tier_2() {
        run_launchd_case("t3", true);
    }

    /// T4 — a `[refresh].claude_bin` pin resolves through the real production entry point
    /// under `PATH=/usr/bin:/bin:/usr/sbin:/sbin`.
    #[test]
    fn launchd_env_with_a_config_pin_resolves_via_tier_1() {
        run_launchd_case("t4", false);
    }

    /// T5 — with `HOME` entirely ABSENT (as under launchd), the passwd-database accessors
    /// still answer and resolution still succeeds.
    #[test]
    fn launchd_env_without_home_still_resolves_from_the_password_database() {
        run_launchd_case("t5", false);
    }

    /// T6 — the harvested PATH is genuinely different from
    /// `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, so T1 cannot pass by accident if the harvest
    /// silently no-ops.
    #[test]
    fn launchd_env_harvested_path_differs_from_the_launchd_path() {
        run_launchd_case("t6", false);
    }

    /// The executable text of the `fn` whose signature starts at `signature`: comments and
    /// blank lines dropped, the rest trimmed and joined by single spaces.
    ///
    /// Panics rather than returning an `Option`, so a moved or renamed function re-anchors
    /// its guard LOUDLY instead of silently matching nothing — the degenerate-gate failure
    /// [`run_child_case`]'s completion marker guards against one layer down.
    fn stripped_fn_body(source: &str, signature: &str) -> String {
        let start = source
            .find(signature)
            .unwrap_or_else(|| panic!("`{signature}` moved — re-anchor this guard"));
        let open = source[start..]
            .find('{')
            .map(|offset| start + offset + 1)
            .expect("the signature has no body");
        let end = source[open..]
            .find("\n}")
            .map(|offset| open + offset)
            .expect("the body is unterminated");
        source[open..end]
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("//"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Both production entry points must be BARE delegations to the one shared policy.
    ///
    /// [`claude_binary_ambient`] exists so the cases above can drive production's own env
    /// reads against a STAGED login shell. That seam opens a gap one level up — and it is the
    /// very shape of bug this section exists to guard, recursed: every launchd case either
    /// injects its own harvest (T1/T2/T5/T6) or is decided at tier 1/2 before the harvest is
    /// reached (T3/T4), and the issue-#802 cases inject too. So any TIER-3 policy inserted
    /// above the delegation — in [`claude_binary`] (whose sole production caller is `poke`) or in
    /// [`claude_binary_with_override`] (`login`, plus the daemon) — leaves every behavioural
    /// case in this file green while silently changing what production resolves.
    ///
    /// Confirmed by mutation, twice, which is why this guard asserts EQUALITY over both
    /// entry points rather than `contains` over one. A caller-scoped tier 3 planted inside
    /// [`claude_binary`] is the least creative implementation of ADR-0030 alternative 2
    /// available — one function, one production caller — and a `contains` assertion accepts
    /// it, because the delegation it looks for is still there, just no longer alone.
    ///
    /// A source assertion rather than a behavioural one, for the same reason
    /// `t19_the_resolver_introduces_no_platform_conditional` is: reaching these entry points
    /// behaviourally means spawning the RUNNING USER's login shell and scanning their real
    /// `PATH` — exactly the machine-specific state every case here stages away. An
    /// equivalence test against the ambient seam would be actively misleading, since the
    /// process-wide [`HARVESTED_PATH`] memo would serve the second call from the first
    /// call's result and agree no matter what the wiring said.
    #[test]
    fn the_production_entry_points_are_bare_delegations_to_the_one_policy() {
        let source = include_str!("paths.rs");
        // Each definition precedes this test, so `find` reaches it rather than the literal
        // in the table below — the same anchoring trick T19 uses.
        for (signature, delegation) in [
            (
                "pub(crate) async fn claude_binary()",
                "claude_binary_with_override(None).await",
            ),
            (
                "pub(crate) async fn claude_binary_with_override(",
                "claude_binary_ambient(config_bin, harvest_login_shell_path).await",
            ),
        ] {
            assert_eq!(
                stripped_fn_body(source, signature),
                delegation,
                "`{signature}` must be NOTHING BUT `{delegation}` — anything else is \
                 resolution policy living above the one shared ladder, which every \
                 behavioural case in this file is structurally blind to (they all inject at \
                 `claude_binary_ambient`, below here). A caller-scoped tier planted here is \
                 ADR-0030 alternative 2 arriving by the shortest available route."
            );
        }
    }

    // --- The invoking shell's environment, reproduced (issue #802) -----------
    //
    // The section above reproduces the environment `launchd` gives the DAEMON — bare `PATH`,
    // no `claude` anywhere on it. This one reproduces the opposite environment, on the same
    // harness: a TERMINAL whose `PATH` already leads with a perfectly good `claude`.
    //
    //     PATH=/custom/bin:$PATH sessiometer poke
    //
    // `poke` and `login` share one resolver with the daemon, so a successful harvest REPLACES
    // that inherited `PATH` and the `/custom/bin` prefix is never consulted. ADR-0030 records
    // that as the decision (issue #802 branch (a)) rather than as a defect; these cases are
    // its AC4 pin, so it cannot drift back unnoticed.
    //
    // THREE legs, because a caller-scoped tier can be planted at three layers and each leg
    // is blind to the other two:
    //
    //   * BELOW the entry points — CLI1 / CLI2 pin the RESOLUTION, and fail if tier 3 ever
    //     probes the inherited `$PATH` again (ADR-0030 alternative 1).
    //   * ABOVE them — `the_cli_verbs_share_the_one_resolution_policy` pins the CALL SITES,
    //     and fails if a verb grows its own resolver (ADR-0030 alternative 2), which the
    //     behavioural legs cannot see, since they drive the seam both callers share.
    //   * AT them — `the_production_entry_points_are_bare_delegations_to_the_one_policy`
    //     (defined above, with the #785 guards it also serves) pins the ENTRY POINTS: the
    //     layer the other two bracket without covering, and the shortest route to
    //     alternative 2 — one tier planted inside `claude_binary`, one function, one
    //     production caller. Both mutations that reach it left the whole suite green until
    //     that guard existed.

    /// CLI1 — with a `claude` sitting FIRST on the invoking shell's `PATH` and a different
    /// one reachable through the harvest, the HARVESTED binary resolves. The shell-local
    /// entry is not consulted.
    ///
    /// The name spells out the property rather than the environment: this is the assertion
    /// that fails the moment the inherited `$PATH` becomes a tier-3 probe again.
    #[test]
    fn terminal_env_a_shell_local_path_entry_is_not_consulted_when_the_harvest_succeeds() {
        run_terminal_case("cli1");
    }

    /// CLI2 — the negative control. The SAME shell-local `claude`, in the SAME environment,
    /// IS resolved once the harvest fails. Without it CLI1 could be passing over a binary
    /// nothing could have reached, which would prove nothing about shadowing.
    #[test]
    fn terminal_env_the_shell_local_entry_is_reachable_once_the_harvest_fails() {
        run_terminal_case("cli2");
    }

    /// The CLI verbs must resolve through the ONE shared policy — no caller-scoped tier 3.
    ///
    /// ADR-0030 alternative 2 is "scope tier 3 by caller: daemon paths harvest, CLI verbs use
    /// the inherited `$PATH`". Every behavioural case here drives [`claude_binary_ambient`],
    /// which is the DAEMON's entry point as much as the CLI's — so splitting the policy in
    /// two would leave all of them green while `poke` and `login` quietly stopped predicting
    /// the daemon. This is the guard that fails instead.
    ///
    /// A source assertion for the same reason
    /// [`the_production_entry_points_are_bare_delegations_to_the_one_policy`] is one: the only
    /// behavioural difference a caller-scoped tier produces depends on the running
    /// developer's OWN login shell and their real `~/.local/bin/claude` — precisely the
    /// machine-specific state every case here stages away. Driving `poke` or `login`
    /// end-to-end would additionally need a roster, a keychain and a live daemon.
    ///
    /// This guard binds ABOVE the entry points (on the verbs' call sites) and the behavioural
    /// cases bind BELOW them (at `claude_binary_ambient`); the sibling guard closes the
    /// bracket by pinning the entry points THEMSELVES. All three are needed — a caller-scoped
    /// tier can be planted at any of the three layers, and each guard is blind to the other
    /// two.
    ///
    /// Two assertions per verb, because the split can arrive from either direction: the verb
    /// stops calling the shared entry point, or it starts consulting `$PATH` on its own.
    #[test]
    fn the_cli_verbs_share_the_one_resolution_policy() {
        for (verb, source, shared_entry_point) in [
            ("poke", include_str!("poke.rs"), "paths::claude_binary()"),
            (
                "login",
                include_str!("login.rs"),
                "paths::claude_binary_with_override(",
            ),
        ] {
            assert!(
                source.contains(shared_entry_point),
                "`{verb}` must resolve `claude` through the shared `{shared_entry_point}` \
                 entry point — a caller-scoped tier 3 (ADR-0030 alternative 2) splits one \
                 resolution policy into two and stops `poke` predicting the daemon"
            );
            assert!(
                !source.contains("var_os(\"PATH\")") && !source.contains("var(\"PATH\")"),
                "`{verb}` must not read the invoking shell's `$PATH` itself — reconstructing \
                 a caller-scoped tier there is exactly the split ADR-0030 declined"
            );
        }
    }
}
