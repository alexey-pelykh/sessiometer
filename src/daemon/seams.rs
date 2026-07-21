// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Daemon seams: the shutdown / poll / external-login / poll-refresh / keep-warm trait boundaries the
//! poll/swap loop is driven through, plus their production implementations (the seam wiring) and the
//! two concrete helpers extracted alongside — [`InstanceLock`] (the `flock` single-instance lock) and
//! the private `StashCredentialStore` adapter. The generic `Daemon<P, C, S, K>` bounds and every
//! construction site (cli / use_account / service) resolve against these; a hermetic test swaps a
//! fake in for each trait seam.
//!
//! The family: [`Shutdown`] / [`RealShutdown`] (the SIGINT/SIGTERM stop), [`RosterPoller`] /
//! [`RealRosterPoller`] (per-account usage poll, canonical for the active account vs a stash-backed
//! [`StashCredentialStore`] for any other), [`ExternalLoginWatch`] / [`ExternalLoginWatcher`] (the
//! #140 short-cadence canonical probe), [`PollRefresh`] (the #162 on-demand poll-path refresh, whose
//! production impl rides `refresh_tick`'s `RealRefreshEngine`), [`KeepWarm`] / [`RealKeepWarmEngine`]
//! (the #282 active-account keep-warm mint), and [`InstanceLock`] (the `flock` single-instance lock).
//!
//! Extracted verbatim from `daemon` per the God-module decomposition (issue #637 step 2, issue
//! #657) — a behavior-preserving move, re-exported under `crate::daemon::*` so every call and
//! construction site resolves unchanged.

use super::*;

/// Shutdown seam: resolves when a graceful stop has been requested. Behind a seam
/// so the loop's stop path is driven deterministically in tests (a real
/// implementation waits on SIGINT / SIGTERM).
pub(crate) trait Shutdown {
    /// Resolve when a graceful shutdown has been requested.
    async fn requested(&mut self);
}

/// Real shutdown: resolves on the first SIGINT or SIGTERM.
pub(crate) struct RealShutdown {
    sigint: Signal,
    sigterm: Signal,
}

impl RealShutdown {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            sigint: signal(SignalKind::interrupt())?,
            sigterm: signal(SignalKind::terminate())?,
        })
    }
}

impl Shutdown for RealShutdown {
    async fn requested(&mut self) {
        tokio::select! {
            _ = self.sigint.recv() => {}
            _ = self.sigterm.recv() => {}
        }
    }
}

/// Per-account usage seam: poll one roster account, routing the active account
/// through the canonical credential and every other through its stash. The test
/// fake (`FakeRosterPoller`) returns scripted per-account readings.
pub(crate) trait RosterPoller {
    /// Poll `account`'s usage. `active` selects the token source: the canonical
    /// keychain item for the active account (whose token is the freshest), or the
    /// account's stash for any other. Returns the full [`PolledReading`] — the
    /// swap-decision [`Usage`] plus the sample-only `severity` — from a single API
    /// call; the caller projects to `Usage` for the decision and records the sample
    /// from the same reading (issue #156, no extra call).
    async fn poll(&self, account: &Account, active: bool) -> Result<PolledReading>;
}

/// Production poller: build a [`CurlTransport`]-backed [`RealUsageSource`] per
/// call — over the canonical store for the active account, or a stash-backed
/// [`StashCredentialStore`] for any other. Stateless: the consecutive-401 streak
/// that drives dead-credential detection lives in the daemon's per-account health
/// state (issue #42), not in this per-poll source.
pub(crate) struct RealRosterPoller {
    stash: RealAccountStash,
}

impl RealRosterPoller {
    pub(crate) fn new() -> Self {
        Self {
            stash: RealAccountStash::new(),
        }
    }
}

impl RosterPoller for RealRosterPoller {
    async fn poll(&self, account: &Account, active: bool) -> Result<PolledReading> {
        if active {
            // The active account's token refreshes in place, so the canonical
            // item is the freshest bearer — poll through it.
            RealUsageSource::new(CurlTransport::new(RealCredentialStore::new()))
                .usage()
                .await
        } else {
            // A non-active account is polled with its stashed token — the seam #5
            // anticipated: `CurlTransport` is generic over `CredentialStore`.
            RealUsageSource::new(CurlTransport::new(StashCredentialStore {
                stash: &self.stash,
                service: account.stash(),
            }))
            .usage()
            .await
        }
    }
}

/// A read-only [`CredentialStore`] whose token comes from a per-account stash —
/// the adapter that lets the usage poller read a non-active account through the
/// same transport seam as the active one.
struct StashCredentialStore<'a, S> {
    stash: &'a S,
    service: String,
}

impl<S: AccountStash> CredentialStore for StashCredentialStore<'_, S> {
    async fn read(&self) -> Result<Credential> {
        Ok(self.stash.read(&self.service).await?.credential)
    }

    async fn write(&self, _credential: &Credential) -> Result<()> {
        // Polling never writes the canonical item through a stash adapter; the
        // swap engine writes the canonical item directly.
        Err(Error::Unimplemented(
            "stash-backed credential store is read-only",
        ))
    }
}

/// The external-login watch cadence (issue #140): how often the run loop probes the canonical
/// credential item for an OUT-OF-BAND change (a manual `claude /login`), DECOUPLED from the
/// usage-poll cadence (`poll_secs`, default 300 s). The probe is a LOCAL keychain read — no
/// network, no rate-limit — so a short cadence is cheap; 15 s bounds the worst-case
/// active-account re-auth latency to seconds instead of a full poll interval. A named constant
/// (not config) keeps issue #140 scoped to the reactivity change; operator-tunable config is a
/// deliberate future option. Chosen over event-driven keychain watching (kqueue / FSEvents /
/// `Sec*` callbacks): the macOS keychain DB is fragile to watch and would add substantial
/// unsafe / FFI surface for little gain over a cheap local poll on the established idle-seam
/// pattern (#105).
const EXTERNAL_LOGIN_WATCH_SECS: u64 = 15;

/// External-login watch seam (issue #140): the run loop drives this from its idle path to
/// notice a manual `claude /login` (or any out-of-band canonical rewrite) on the ACTIVE
/// account FASTER than the usage-poll cadence. Distinct from [`RefreshTicker`] (#105, the
/// periodic parked-account refresh) and [`CanonicalWatch`] (the per-tick change classifier):
/// this is purely a shorter-cadence TRIGGER — it reads the canonical and, when it differs from
/// the daemon's last-committed baseline ([`Daemon::canonical_baseline`]), the run loop breaks
/// the idle to re-tick so the existing [`Daemon::reconcile_canonical_change`] does the
/// authoritative re-stash / re-resolve / surface. It NEVER mutates daemon state itself.
///
/// The seam owns its OWN [`CredentialStore`] because the daemon's is borrowed by the idle
/// `wait` future; both read the SAME canonical item in production. Wholly inert when a hermetic
/// test wires the no-op watch: [`until_due`](ExternalLoginWatch::until_due) never resolves, so
/// the arm never wins the idle select and the loop behaves exactly as before #140.
pub(crate) trait ExternalLoginWatch {
    /// Resolve when the next canonical probe is due (the watch's own cadence). MUST never
    /// resolve when disabled, so it never wins the idle select. Re-armable: the run loop awaits
    /// it afresh each idle iteration.
    async fn until_due(&mut self);
    /// Read the canonical credential item via the watch's OWN store. `None` on ANY
    /// unreadable / locked / absent read — a probe that cannot read simply detects nothing and
    /// the run loop keeps idling (fail-safe: detection never stalls or crashes the loop).
    async fn read_canonical(&mut self) -> Option<Credential>;
}

/// Production external-login watch (issue #140): a short-cadence LOCAL probe of the canonical
/// item over a [`RealCredentialStore`]. Always-on — the probe is a cheap local keychain read
/// with no network / rate-limit cost and a strictly better active-account re-auth latency, so
/// there is no feature gate; a hermetic test that must NOT probe wires the inert no-op watch
/// instead. Its own store is a second [`RealCredentialStore`] (stateless, resolves the same
/// canonical item as the daemon's) so the idle `wait`'s `&mut Daemon` borrow is untouched.
pub(crate) struct ExternalLoginWatcher<C> {
    store: C,
}

impl<C> ExternalLoginWatcher<C> {
    pub(crate) fn new(store: C) -> Self {
        Self { store }
    }
}

impl<C: CredentialStore> ExternalLoginWatch for ExternalLoginWatcher<C> {
    async fn until_due(&mut self) {
        tokio::time::sleep(Duration::from_secs(EXTERNAL_LOGIN_WATCH_SECS)).await;
    }

    async fn read_canonical(&mut self) -> Option<Credential> {
        // Best-effort: a locked / not-found / transient keychain read yields `None`, so the run
        // loop detects nothing this probe and keeps idling — a detection failure must never
        // break the poll/swap loop (mirrors #156's fail-open collector, #162's fail-safe
        // refresh).
        self.store.read().await.ok()
    }
}

/// Per-account refresh seam the POLL path uses to revive an expired-but-refreshable
/// access token BEFORE a usage 401 counts toward the #42 dead-credential streak (issue
/// #162). Distinct from [`RefreshTicker`] (the periodic parked-account sweep, #105): this
/// is a single, on-demand, one-account refresh composed into the poll→streak seam that a
/// 401 previously fell straight through.
///
/// Carried as an OPTIONAL [`Daemon`] field (`Option<Box<dyn PollRefresh>>`, like
/// `swap_lock_path`) rather than a 5th generic seam: the retry re-polls through the
/// account's EXISTING [`RosterPoller`], so only the refresh needs injecting, and the boxed
/// option leaves every hermetic-test `Daemon::new` site — and `tick`'s many call sites —
/// untouched (a scoped change that composes with the queued #140 daemon work). `None` (the
/// default) is the pre-#162 behaviour: a 401 flows straight to the streak. Production wires
/// the #102 engine ([`RealRefreshEngine`]); the seam tests wire a scripted fake.
///
/// A hand-desugared `async fn` (a boxed future) so the trait is `dyn`-compatible; the
/// current-thread runtime keeps the returned future free of a `Send` bound.
pub(crate) trait PollRefresh {
    /// Run ONE isolated refresh cycle for `account` (the #102 engine), yielding the
    /// classified [`RefreshReport`] so the caller can distinguish a revived / still-alive
    /// token from a `Dead` one (the refresh token cleared in place).
    fn refresh<'a>(
        &'a self,
        account: &'a Account,
    ) -> Pin<Box<dyn Future<Output = Result<RefreshReport>> + 'a>>;
}

impl PollRefresh for RealRefreshEngine {
    fn refresh<'a>(
        &'a self,
        account: &'a Account,
    ) -> Pin<Box<dyn Future<Output = Result<RefreshReport>> + 'a>> {
        // Reuse the SAME #102 engine the periodic tick drives — the poll path and the
        // sweep now compose over one refresh implementation (issue #162 root cause: they
        // were scoped as separate issues and never composed).
        Box::pin(RefreshEngine::refresh(self, account))
    }
}

/// The in-place ACTIVE-account keep-warm seam (issue #282) — the FOURTH refresh mechanism.
/// Given the active account and its current CANONICAL blob it mints a fresh token by driving
/// `claude` through the isolated back-dating spawn (there is no first-party OAuth exchange —
/// a fresh token comes only from Claude Code, see [`crate::refresh`]) and RETURNS it, so the
/// DAEMON promotes it to the canonical `Claude Code-credentials` item (atomic `-U`, under the
/// swap lock, baseline-committed). It never writes the canonical item itself, keeping the
/// daemon the single canonical writer (ADR-0003). Distinct from [`PollRefresh`] (the
/// #253-excluded isolated engine that writes the STASH): this is the ONE refresh path that
/// legitimately targets the active account, because its result lands where a live session reads.
///
/// Carried as an OPTIONAL [`Daemon`] field (`Option<Box<dyn KeepWarm>>`, like `poll_refresh`)
/// so every hermetic-test `Daemon::new` site is untouched; `None` (the default) is the pre-#282
/// behaviour. A hand-desugared `async fn` (a boxed future) so the trait is `dyn`-compatible; the
/// current-thread runtime keeps the returned future free of a `Send` bound.
pub(crate) trait KeepWarm {
    /// Mint a fresh token for `account` from its `canonical` blob and return it for the daemon
    /// to promote to the canonical item. `Ok((report, Some(credential)))` ONLY on a real refresh
    /// ([`RefreshOutcome::Refreshed`]); `(report, None)` for `NoChange` / `Dead` / `Error` (the
    /// daemon then leaves the canonical item untouched — a `Dead` outcome flows to the #42
    /// streak). `Err` is a could-not-run (spawn / FS) failure the daemon treats fail-safe.
    fn keep_warm<'a>(
        &'a self,
        account: &'a Account,
        canonical: &'a Credential,
    ) -> Pin<Box<dyn Future<Output = Result<KeepWarmMint>> + 'a>>;
}

/// The keep-warm mint result: the classified [`RefreshReport`] plus the fresh credential the
/// daemon promotes to the canonical item — `Some` ONLY on a real [`RefreshOutcome::Refreshed`],
/// `None` for `NoChange` / `Dead` / `Error`. Aliased so the `dyn`-compatible boxed-future
/// signatures on the [`KeepWarm`] trait stay readable (`clippy::type_complexity`); the same tuple
/// [`crate::refresh::keep_warm_cycle`] returns.
pub(crate) type KeepWarmMint = (RefreshReport, Option<Credential>);

/// The production [`KeepWarm`]: mints via [`crate::refresh::keep_warm_account`], which reuses
/// the #102 isolated back-dating spawn on a COPY of the canonical blob and hands the fresh
/// token back. Holds the `[refresh].claude_bin` OVERRIDE (issue #375), NOT a resolved path:
/// like the periodic tick's [`RealRefreshEngine`] it resolves `claude` PER CYCLE at the spawn
/// site via [`resolve_binary`](Self::resolve_binary), so a symlink / `$PATH` / version change
/// after the daemon started is picked up on the next keep-warm with no restart. The ephemeral
/// isolated dir + keychain are derived per-call from the account uuid.
pub(crate) struct RealKeepWarmEngine {
    claude_bin: Option<PathBuf>,
}

impl RealKeepWarmEngine {
    pub(crate) fn new(claude_bin: Option<PathBuf>) -> Self {
        Self { claude_bin }
    }

    /// Resolve the `claude` binary to spawn THIS keep-warm cycle (issue #375) via the UNCHANGED
    /// policy ([`crate::paths::claude_binary_with_override`]: `[refresh].claude_bin` →
    /// `$CLAUDE_BIN` → `$PATH`) — only the timing moved to per-cycle; which binary is chosen is
    /// identical to before (no canonicalization, no validation — a wrapper symlink spawns as-is).
    /// A failure surfaces as the mint's `Err`, which the daemon treats non-fatally: the canonical
    /// item is left untouched and the mint is retried next cycle.
    fn resolve_binary(&self) -> Result<PathBuf> {
        crate::paths::claude_binary_with_override(self.claude_bin.as_deref())
    }
}

impl KeepWarm for RealKeepWarmEngine {
    fn keep_warm<'a>(
        &'a self,
        account: &'a Account,
        canonical: &'a Credential,
    ) -> Pin<Box<dyn Future<Output = Result<KeepWarmMint>> + 'a>> {
        // Resolve THIS cycle (issue #375), not from a frozen field, then own the non-borrowed
        // inputs so the future needs only `canonical`'s lifetime. A resolution failure is carried
        // into the future as the `Err` the daemon handles fail-safe (canonical left untouched).
        let resolved = self.resolve_binary();
        let uuid = account.account_uuid.clone();
        Box::pin(async move {
            let binary = resolved?;
            crate::refresh::keep_warm_account(canonical.expose(), &uuid, binary).await
        })
    }
}

/// A held single-instance lock: a kernel advisory `flock(LOCK_EX|LOCK_NB)` on the
/// native-local `daemon.lock`. The file is held open for the process lifetime —
/// the kernel releases the lock on death (or on drop), so there is no stale-PID
/// reaping. A second `run` cannot acquire it and gets [`Error::AlreadyRunning`]
/// (process exit `3`).
pub(crate) struct InstanceLock {
    // Held open purely to keep the lock; dropping it (or the process dying)
    // releases it.
    _file: File,
}

impl InstanceLock {
    /// Acquire the lock at `path`, creating the file `0600` if needed.
    /// [`Error::AlreadyRunning`] if another instance already holds it.
    pub(crate) fn acquire(path: &Path) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;

        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        // Raw `flock` FFI, kept un-wrapped by ADR-0004: kept raw rather than
        // adding a `rustix` production dependency; the std wheel
        // (`File::try_lock`, stable 1.89) is the planned replacement once MSRV
        // reaches 1.89 (see #257).
        // SAFETY: `flock` takes a valid open fd (owned by `file`, which outlives
        // the call) and the two flag constants; it has no other preconditions.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(Self { _file: file });
        }
        let err = std::io::Error::last_os_error();
        // EWOULDBLOCK (== EAGAIN) means another instance holds the lock; anything
        // else is a genuine I/O failure.
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Err(Error::AlreadyRunning)
        } else {
            Err(Error::Io(err))
        }
    }

    /// Probe whether the single-instance lock at `path` is currently held by a LIVE daemon,
    /// WITHOUT disturbing it (issue #396) — the lock-fallback half of the `daemon status`
    /// liveness projection (socket-primary, lock-fallback). A non-blocking
    /// `flock(LOCK_EX|LOCK_NB)` over a fresh read-only open (a separate open file description,
    /// so it contends with the daemon's held lock exactly as a second `run` would):
    /// - `EWOULDBLOCK` ⇒ another process holds it — a daemon is alive even if its control
    ///   socket is not answering yet (the honest startup / wedged case; NOT "not running").
    /// - a successful acquire ⇒ no live holder; the lock is released the instant `file` drops
    ///   at the end of this scope — nothing is started, stopped, or signalled.
    /// - an absent lock file ⇒ the daemon has never created it ⇒ not running.
    ///
    /// Read-only by construction (the `daemon status` AC: no process is started/stopped/
    /// signalled). Kept beside [`Self::acquire`] so the raw `flock` FFI stays localized
    /// (ADR-0004).
    ///
    /// Note the one inherent tradeoff: probing a FREE lock necessarily acquires it for the
    /// ~microseconds until `file` drops — `flock` has no test-without-acquire mode, so this
    /// acquire-then-release is the canonical liveness-probe shape. It is benign here because
    /// the caller runs this ONLY as the socket-primary fallback (a real startup already holds
    /// the lock, so the probe fails to acquire and never contends); the sole residual race is
    /// a `run` whose own `acquire` lands in that microsecond window and self-refuses (exit 3),
    /// which is vanishingly unlikely and self-correcting on retry.
    pub(crate) fn is_held(path: &Path) -> Result<bool> {
        use std::os::unix::io::AsRawFd;

        let file = match OpenOptions::new().read(true).open(path) {
            Ok(file) => file,
            // No lock file at all ⇒ the daemon has never created it ⇒ not held.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(Error::Io(err)),
        };
        // SAFETY: as in `acquire` — a valid open fd (owned by `file`, which outlives the
        // call) plus the two flag constants; `flock` has no other preconditions.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            // Acquired ⇒ no live holder; `file` drops at the end of this scope, releasing the
            // lock at once.
            return Ok(false);
        }
        let err = std::io::Error::last_os_error();
        // EWOULDBLOCK (== EAGAIN): another instance holds the lock — a live daemon.
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(true)
        } else {
            Err(Error::Io(err))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_keep_warm_engine_resolves_the_binary_per_cycle_not_frozen_at_construction() {
        // Issue #375, the #282 keep-warm engine's half of the fix (sibling to `refresh_tick`'s
        // `RealRefreshEngine` test). `RealKeepWarmEngine` holds the `[refresh].claude_bin` OVERRIDE
        // and resolves the spawn binary PER CYCLE, so a mid-run symlink re-point is picked up on the
        // next keep-warm with no daemon restart. Built ONCE, resolved across a re-point: the
        // frozen-at-startup design this fixes could only ever return its first result.
        let tmp = tempfile::tempdir().unwrap();
        let installed = tmp.path().join("claude-installed");
        std::fs::write(&installed, b"#!/bin/sh\n").unwrap();
        let link = tmp.path().join("claude");
        std::os::unix::fs::symlink(&installed, &link).unwrap();

        let engine = RealKeepWarmEngine::new(Some(link.clone()));

        // Cycle 1: link → installed (exists) → Ok, returning the symlink path UNCANONICALIZED
        // (issue constraint [C1]: a wrapper symlink is spawned as-is, never resolved to its target).
        assert_eq!(engine.resolve_binary().unwrap(), link);

        // The updater removes the pointed-at binary: the SAME engine resolves to a NON-FATAL error
        // on its next cycle (the daemon leaves the canonical item untouched, retried next cycle),
        // never a reuse of a stale frozen path.
        std::fs::remove_file(&installed).unwrap();
        assert!(matches!(
            engine.resolve_binary(),
            Err(crate::error::Error::ClaudeBinaryNotFound)
        ));
    }

    // --- single-instance lock ----------------------------------------------

    #[test]
    fn instance_lock_blocks_a_second_acquisition_then_frees_on_drop() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");

        let lock = InstanceLock::acquire(&path).expect("first acquisition succeeds");
        // A second acquisition while the first is held is refused — the exit-3
        // signal a second `run` exits on, without disturbing the first.
        assert!(matches!(
            InstanceLock::acquire(&path),
            Err(Error::AlreadyRunning)
        ));
        // The lock file is 0600.
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // Dropping the holder releases the lock (kernel-released on close).
        drop(lock);
        let _reacquired =
            InstanceLock::acquire(&path).expect("the lock is free after the first is dropped");
    }

    #[test]
    fn instance_lock_is_held_probe_reports_absent_held_and_freed() {
        // Issue #396: the read-only lock-fallback probe behind `daemon status`. It must never
        // disturb a live holder (non-blocking flock over a separate open), and it distinguishes
        // three states: absent lock file, held-by-a-live-daemon, and present-but-free.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");

        // Absent lock file ⇒ the daemon has never created it ⇒ not held (⇒ "not running").
        assert!(!InstanceLock::is_held(&path).expect("an absent lock probes cleanly as not-held"));

        // A held single-instance lock ⇒ the probe (a SEPARATE open + non-blocking flock, as a
        // `daemon status` in another process would do) sees it held — without disturbing the
        // holder, which is still live below.
        let lock = InstanceLock::acquire(&path).expect("acquire the single-instance lock");
        assert!(InstanceLock::is_held(&path).expect("a held lock probes as held"));
        // The probe did not steal the lock: a second real acquisition is still refused.
        assert!(matches!(
            InstanceLock::acquire(&path),
            Err(Error::AlreadyRunning)
        ));

        // Released (holder dropped) ⇒ present-but-free ⇒ not held. The file now EXISTS
        // (acquire created it), so this is the stale-lock-file path — distinct from the
        // absent path above, and the probe's own acquire+release leaves nothing signalled.
        drop(lock);
        assert!(
            !InstanceLock::is_held(&path).expect("a released lock probes as not-held"),
            "a present-but-unlocked file must read as not-held (stale lock file)",
        );
    }
}
