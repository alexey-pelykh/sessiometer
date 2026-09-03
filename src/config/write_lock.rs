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
//! ordering deliberately keeps it outside of. This lock serializes those writers' PUBLISHES — see
//! § The span, exactly for what that does and does not buy — and it leaves `swap.lock`'s
//! contention set exactly as it was.
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
//! machine's lock. Deriving it makes the rule: same config PATH ⇒ same lock, different config
//! paths ⇒ no contention. Path, not file — the derivation is lexical, so two spellings of one
//! file (a symlinked home, `/tmp` vs `/private/tmp`) key two different locks and do not contend,
//! and a lock file deleted between two acquires gives each writer a fresh inode of its own. Both
//! are silent. Neither is reachable through the production path — `paths::config_file` returns
//! one spelling and nothing removes the file — and both are shared with `swap.lock` and
//! `usage.lock`, which derive theirs the same way; hardening all three is issue #1483.
//! `swap.lock` and `daemon.lock` are native-local for the opposite
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
//! What rules out a config-vs-swap deadlock is the BOUNDED wait, and it would rule one out even
//! if the locks were nested: a cycle needs both hold-and-wait directions, and this critical
//! section acquires nothing else, so only one direction is even constructible — the worst a
//! nesting could cost is a swap-lock hold extended by `CONFIG_WRITE_LOCK_MAX_WAIT`.
//!
//! Non-nesting is kept anyway, for that latency and because it survives the wait becoming
//! unbounded (issue #257 plans to replace this raw `flock` with `File::try_lock`). Every
//! `save_to` call in this crate sits strictly AFTER its verb's swap-locked section has returned
//! and released — `capture` / `reconcile_login` / `perform_socket_capture` / `apply_import` all
//! save outside the lock, by the same documented intent this module opens with — so the two
//! critical sections are disjoint rather than ordered.
//! `capture::tests::the_roster_save_is_reached_only_after_the_swap_lock_has_been_released`
//! pins that over all four.
//!
//! # The span, exactly — and what it does NOT cover
//!
//! State the span rather than the aspiration, because "a config-write lock exists" invites a
//! reader to assume more than this one gives. It is held across what
//! [`Config::save_to`](crate::config::Config::save_to) itself does: retain the file being
//! replaced into the backup ring, publish the replacement, then prune. That is a genuine
//! read-modify-write OF `config.toml` — the ring reads the very file it is about to displace —
//! and serializing it is what makes a concurrent publish impossible (R-16), leaves the file on
//! disk as exactly one writer's complete valid output (AC-8), and closes the three degradations
//! [`crate::roster_backup`] previously had to describe rather than prevent.
//!
//! It does NOT cover a CALLER that reads `config.toml`, mutates the parsed value, and only then
//! saves. Two such callers can each publish a complete, valid file with one of them losing its
//! change, because only the last step of each is serialized. That wider span is deliberately out
//! of scope here — D-8's runtime view annotates the lock on the SAVE step alone, and AC-8 forbids
//! two of the obvious widenings outright — and it is tracked as its own issue (#1482), which also
//! records why `capture` and `login` cannot simply be widened: their reads straddle a
//! multi-minute interactive spawn.

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
    /// a busy-spin or a blocked OS thread), so the runtime keeps turning while it WAITS and a CLI
    /// verb stays interruptible — the same discipline
    /// [`SwapLock::acquire`](crate::swap::SwapLock::acquire) keeps, and the reason this is async
    /// where [`crate::usage_store`]'s blocking store lock is not.
    ///
    /// Scoped to the wait deliberately. The daemon awaits both its `save_to` calls inline in the
    /// run loop's post-idle, so the TICK and every command routed through it are delayed by up to
    /// `max_wait` regardless; and the critical section the wait leads to is synchronous
    /// `std::fs` on a `current_thread` runtime. Yielding here buys an interruptible, non-spinning
    /// wait — not a daemon that keeps working through it.
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
                // Interrupted by a signal — not contention; retry immediately, but only
                // after the deadline check below. A bare `continue` here would skip it, so a
                // signal arriving faster than the loop could turn the one function whose whole
                // contract is "bounded" into an unbounded spin.
                Some(libc::EINTR) => {
                    if Instant::now() >= deadline {
                        return Err(Error::ConfigWriteLockBusy);
                    }
                    continue;
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    /// The lock is the config file's OWN sibling, so two shells pointed at two different config
    /// files (an `XDG_CONFIG_HOME` override, or two hermetic tests) never contend for no reason —
    /// and one config file always resolves to one lock however it was reached.
    #[test]
    fn the_lock_is_the_config_files_own_sibling_and_is_per_config_file() {
        let a = Path::new("/cfg/a/config.toml");
        let b = Path::new("/cfg/b/config.toml");

        assert_eq!(lock_path(a), PathBuf::from("/cfg/a/config.toml.lock"));
        assert_eq!(lock_path(a), lock_path(Path::new("/cfg/a/config.toml")));
        assert_ne!(lock_path(a), lock_path(b));

        // Never the config file itself: `write_private_file` publishes by `rename`, so a lock on
        // that inode would not carry across the swap and a later writer opening the new file by
        // path would not contend with the holder.
        assert_ne!(lock_path(a), a.to_path_buf());
    }

    /// The lock is DISTINCT from `swap.lock` — AC-3's structural half. Sharing a file with the
    /// swap lock is the one implementation that would satisfy "a lock exists" while silently
    /// widening `swap.lock`'s contention set, which the issue forbids outright.
    #[test]
    fn the_lock_file_is_never_the_swap_or_daemon_lock() {
        let support = Path::new("/support");
        let config_lock = lock_path(Path::new("/cfg/config.toml"));

        assert_ne!(config_lock, support.join("swap.lock"));
        assert_ne!(config_lock, support.join("daemon.lock"));
        assert_ne!(config_lock, support.join("usage.lock"));
    }

    /// FAIL-CLOSED while held, and available again the moment the holder releases — the property
    /// AC-5 rests on. A busy lock is REPORTED as [`Error::ConfigWriteLockBusy`] within the bounded
    /// wait; it neither blocks forever (which is the deadlock AC-5 forbids) nor silently proceeds
    /// without the lock (which would make the lock decorative).
    ///
    /// Mirrors `swap::tests::the_swap_lock_fails_closed_while_held_then_recovers_on_release`.
    #[tokio::test]
    async fn a_contended_acquire_fails_closed_and_recovers_on_release() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml.lock");

        let held = ConfigWriteLock::acquire(&path, Duration::from_millis(50))
            .await
            .expect("the first acquire takes an uncontended lock");

        // A short budget so the refusal is observed without waiting out the production one; the
        // BEHAVIOUR under test is the fail-closed verdict, not the length of the wait.
        let refused = ConfigWriteLock::acquire(&path, Duration::from_millis(50)).await;
        assert!(
            matches!(refused, Err(Error::ConfigWriteLockBusy)),
            "a contended acquire must report `ConfigWriteLockBusy`, got {refused:?}"
        );

        drop(held);
        ConfigWriteLock::acquire(&path, Duration::from_millis(50))
            .await
            .expect("the lock is available again once the holder releases");
    }

    /// Two DIFFERENT config files do not serialize against each other. The per-config-file keying
    /// is not cosmetic: a fixed native-local lock would make every hermetic test in this crate,
    /// and any two shells under different `XDG_CONFIG_HOME` values, queue behind one another.
    #[tokio::test]
    async fn two_different_config_files_do_not_contend() {
        let dir = tempfile::tempdir().unwrap();
        let a = lock_path(&dir.path().join("a/config.toml"));
        let b = lock_path(&dir.path().join("b/config.toml"));
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(b.parent().unwrap()).unwrap();

        let _held_a = ConfigWriteLock::acquire(&a, Duration::from_millis(50))
            .await
            .unwrap();
        ConfigWriteLock::acquire(&b, Duration::from_millis(50))
            .await
            .expect("a different config file's lock is a different lock");
    }

    /// The lock file is created `0600`: it sits beside a `0600` config in a `0700` directory, and
    /// a world-writable lock would let any local user wedge every config write on the machine.
    #[tokio::test]
    async fn the_lock_file_is_created_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml.lock");
        let _held = ConfigWriteLock::acquire(&path, Duration::from_millis(50))
            .await
            .unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the lock file is created private");
    }
}
