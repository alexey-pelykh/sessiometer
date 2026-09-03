// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The dedicated config-write lock (issue #1445, design D-8) — the one thing that makes
//! [`Config::save_to`](crate::config::Config::save_to) a serialized read-modify-write instead
//! of a race.
//!
//! # Why not the swap lock
//!
//! [`reconcile_login`](crate::capture::reconcile_login) places the roster save deliberately
//! OUTSIDE `swap.lock`, and its stated reason is correct and remains correct: a swap contends
//! on the keychain and `~/.claude.json`, never on `config.toml`, so no concurrent swap can race
//! the roster write. What that reasoning does not address is **two config writers racing each
//! other** — two CLI invocations, or a CLI and the daemon's `perform_config_set`. The swap lock
//! was never the instrument for that pair; widening it would break a documented invariant to fix
//! an unrelated one, and would put the roster write back inside a lock the stash-before-roster
//! ordering deliberately keeps it outside of. This lock is the instrument for that pair, and it
//! leaves `swap.lock`'s contention set exactly as it was.
//!
//! # Why the lock is a SIBLING of the config file, not a fixed support-dir path
//!
//! Two reasons, and the second is the one that bites.
//!
//! It must not be the config file's own inode. [`write_private_file`](crate::paths::write_private_file)
//! publishes by `rename`, so a lock held on `config.toml` would not carry across the swap and a
//! later writer opening the new file by path would not contend with the holder — the same reason
//! [`crate::usage_store`] gives for its own `usage.lock`.
//!
//! And it is derived from the config path this crate was asked to write, NOT from
//! [`paths::config_file`](crate::paths::config_file). `config_dir` is `XDG_CONFIG_HOME`-overridable
//! and [`Config::save_to`](crate::config::Config::save_to) is an injectable-path seam, so a fixed
//! native-local lock would make two shells writing two DIFFERENT config files contend for no
//! reason, and — worse — would make every hermetic test in this crate contend on the real
//! machine's lock. Deriving it makes the rule exact: same config file ⇒ same lock, different
//! config files ⇒ no contention. `swap.lock` and `daemon.lock` are native-local for the opposite
//! reason: the resource THEY guard (the keychain, the daemon instance) is machine-global and has
//! no per-config identity to key on.
//!
//! # Fail-closed, and never nested inside the swap lock
//!
//! A contended acquire fails with [`Error::ConfigWriteLockBusy`] after a bounded wait rather than
//! blocking forever or — the outcome that would make the lock decorative — writing anyway.
//! `save_to` acquires BEFORE it reads the file it is about to replace, so a refusal is a true
//! no-op: nothing retained, nothing written, nothing evicted.
//!
//! The wait is bounded AND the two locks are never nested, which is what keeps a CLI writer and
//! a daemon holding `swap.lock` from deadlocking. Every `save_to` call in this crate sits
//! strictly AFTER its verb's swap-locked section has returned and released
//! (`capture` / `reconcile_login` / `apply_import` all save outside the lock, by the same
//! documented intent this module opens with), so the two critical sections are disjoint rather
//! than ordered. `config_write_lock_is_never_taken_inside_the_swap_lock` in
//! [`crate::config`] pins that.

use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// How long a contended [`ConfigWriteLock::acquire`] waits before failing closed.
///
/// Comfortably exceeds one config write — parse a small TOML file, copy it into the backup ring,
/// render and `fsync` a few KB, `rename`, prune — which is milliseconds. Shorter than
/// [`SWAP_LOCK_MAX_WAIT`](crate::swap::SWAP_LOCK_MAX_WAIT)'s 10 s on purpose: that budget is
/// sized for a swap's `security` subprocesses, and there is nothing comparably slow inside this
/// critical section, so a wait that long here would only lengthen the report of a wedged holder.
pub(crate) const CONFIG_WRITE_LOCK_MAX_WAIT: Duration = Duration::from_secs(5);

/// Poll interval while waiting on a contended config-write lock. Short enough that the wait ends
/// within ~one interval of the holder releasing, and the few polls spanning a typical
/// millisecond-scale write are negligible.
const CONFIG_WRITE_LOCK_RETRY: Duration = Duration::from_millis(20);

/// The suffix appended to the config path to name its lock file.
const LOCK_SUFFIX: &str = ".lock";

/// The config-write lock file for `config_path`: its `<config.toml>.lock` sibling, `0600`.
///
/// Pure and path-derived — see the module docs for why it is keyed on the config path rather
/// than on a fixed native-local location.
pub(crate) fn lock_path(config_path: &Path) -> PathBuf {
    let mut path = config_path.as_os_str().to_owned();
    path.push(LOCK_SUFFIX);
    PathBuf::from(path)
}

/// A held single-WRITER config lock: a kernel advisory `flock(LOCK_EX)` on the config file's
/// [`lock_path`] sibling, held only for the DURATION of one config read-modify-write (issue
/// #1445).
///
/// The file is held open for the critical section; the kernel releases the lock on drop (or on
/// process death), so there is no stale-lock reaping and an operator who kills a wedged writer
/// does not have to clean up after it.
///
/// DISTINCT from [`crate::swap::SwapLock`], which guards the keychain + `~/.claude.json` pair and
/// whose contention set deliberately excludes `config.toml`; and from
/// [`crate::daemon::InstanceLock`], which is held non-blocking for the daemon's whole lifetime to
/// reject a second `run`. This one is BLOCKING (bounded) and per-write.
#[derive(Debug)]
pub(crate) struct ConfigWriteLock {
    // Held open purely to keep the lock; dropping it (or the process dying) releases it.
    _file: std::fs::File,
}

impl ConfigWriteLock {
    /// Acquire the config-write lock at `path` (creating the file `0600` if needed),
    /// bounded-blocking up to `max_wait`.
    ///
    /// FAIL-CLOSED: if the lock cannot be taken within `max_wait` — another config writer held it
    /// the whole time — returns [`Error::ConfigWriteLockBusy`] so the caller aborts with ZERO
    /// writes, rather than writing without it and reopening the interleave this exists to
    /// prevent. A busy lock is REPORTED, never silently skipped.
    ///
    /// Polls `flock(LOCK_EX|LOCK_NB)` and yields the runtime between tries (an async sleep, never
    /// a busy-spin or a blocked OS thread), so the daemon keeps serving its control socket while
    /// it waits and a CLI verb stays interruptible — the same discipline
    /// [`SwapLock::acquire`](crate::swap::SwapLock::acquire) keeps, and the reason this is async
    /// where [`crate::usage_store`]'s blocking store lock is not.
    pub(crate) async fn acquire(path: &Path, max_wait: Duration) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        let deadline = Instant::now() + max_wait;
        loop {
            // Raw `flock` FFI, kept un-wrapped by ADR-0004: kept raw rather than adding a
            // `rustix` production dependency; the std wheel (`File::try_lock`, stable 1.89) is
            // the planned replacement once MSRV reaches 1.89 (see #257).
            // SAFETY: `flock` takes a valid open fd (owned by `file`, which outlives the call)
            // and the two flag constants; it has no other preconditions.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(Self { _file: file });
            }
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                // EWOULDBLOCK (== EAGAIN): another config writer holds it — wait, retry.
                Some(libc::EWOULDBLOCK) => {}
                // Interrupted by a signal — not contention; retry immediately.
                Some(libc::EINTR) => continue,
                // A genuine I/O failure (a broken fd / filesystem), surfaced as itself rather
                // than masqueraded as contention.
                _ => return Err(Error::Io(err)),
            }
            // Out of patience: fail closed (the caller aborts with ZERO writes).
            if Instant::now() >= deadline {
                return Err(Error::ConfigWriteLockBusy);
            }
            tokio::time::sleep(CONFIG_WRITE_LOCK_RETRY).await;
        }
    }
}
