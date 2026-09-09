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
    /// ([`RefreshOutcome::Refreshed`]); `(report, None)` for `NoChange` / `Dead` / `Error` (the daemon
    /// then leaves the canonical item untouched — a `Dead` outcome flows to the #42 streak). `Err` is a
    /// could-not-run (locked keychain / unresolvable binary / FS) failure the daemon treats fail-safe.
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

    /// Resolve the `claude` binary to spawn THIS keep-warm cycle (issue #375) via the shared
    /// policy ([`crate::paths::claude_binary_with_override`]: `[refresh].claude_bin` →
    /// `$CLAUDE_BIN` → the harvested user `PATH`). #375 moved the timing to per-cycle; #784
    /// changed only tier 3's PATH source (the login-shell harvest, so a launchd-started daemon
    /// resolves what the user's terminal would). Which binary a given `PATH` yields is unchanged
    /// — first match in the user's own order, no canonicalization, no validation (a wrapper
    /// symlink spawns as-is). A failure surfaces as the mint's `Err`, which the daemon treats
    /// non-fatally: the canonical item is left untouched and the mint is retried next cycle.
    ///
    /// `async` since #784 (tier 3's harvest spawns the login shell); the harvested PATH is
    /// memoized process-wide, so this shares one harvest with the periodic tick rather than
    /// paying its own.
    async fn resolve_binary(&self) -> Result<PathBuf> {
        crate::paths::claude_binary_with_override(self.claude_bin.as_deref()).await
    }
}

impl KeepWarm for RealKeepWarmEngine {
    fn keep_warm<'a>(
        &'a self,
        account: &'a Account,
        canonical: &'a Credential,
    ) -> Pin<Box<dyn Future<Output = Result<KeepWarmMint>> + 'a>> {
        // Own the non-borrowed inputs so the future needs only the `'a` borrows. The resolve
        // itself moved INSIDE the future with #784 — it is `async` now (tier 3 harvests the login
        // shell), and `&'a self` is already captured, so awaiting it here is free of any bridge.
        // Still per-cycle (issue #375), and a resolution failure is still carried as the `Err`
        // the daemon handles fail-safe (canonical left untouched).
        let uuid = account.account_uuid.clone();
        Box::pin(async move {
            let binary = self.resolve_binary().await?;
            crate::refresh::keep_warm_account(canonical.expose(), &uuid, binary).await
        })
    }
}

/// A held single-instance lock on the native-local `daemon.lock`: a kernel advisory
/// `flock(LOCK_EX|LOCK_NB)` on Unix, a `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK |
/// LOCKFILE_FAIL_IMMEDIATELY)` byte-range lock on Windows (issue #976). The file is held open
/// for the process lifetime — the kernel releases the lock on death (or on drop) on BOTH targets,
/// so there is no stale-PID reaping. A second `run` cannot acquire it and gets
/// [`Error::AlreadyRunning`] (process exit `3`). The two targets are not equivalent on the TIMING
/// of that release, and the § What the file lock does NOT carry over section below records the
/// difference rather than leaving the word "death" to carry it.
///
/// # Why a FILE lock on Windows and not a named mutex (issue #976 AC3)
///
/// The issue names both candidates and requires the choice to be made against the
/// ANY-PROVENANCE requirement: [`InstanceLock::is_held`] is mirrored by the menu-bar's
/// `DaemonLockProbe`, which must detect a daemon started by any means — launchd, manual,
/// app-managed — and a lock that only detects self-started daemons is a regression in kind even
/// though the menu-bar is macOS-only.
///
/// **A named mutex fails that requirement on its namespace alone.** Windows kernel object names
/// are per-SESSION unless prefixed: an unprefixed (or `Local\`) name resolves inside the caller's
/// session, so a daemon started in a service context and a CLI started from an interactive shell
/// would create two different mutexes and each would report the other absent. `Global\` fixes the
/// namespace and introduces a worse problem: creating a global object needs
/// `SeCreateGlobalPrivilege`, which services and administrators hold by default and a standard
/// interactive user does not — so the very user this per-user daemon exists for could fail to
/// take its own lock. A byte-range lock has no namespace at all: the lock file is reached by
/// PATH, which is already per-user and already the same path both ends compute.
///
/// **Three further reasons, each independent of that one.** A mutex is invisible to anything that
/// checks the lock FILE, which is what the issue says outright and what the file-based
/// `is_held` probe reads. An abandoned mutex reports `WAIT_ABANDONED`, an extra state with no
/// `flock` analogue and no meaning here, where a byte-range lock simply becomes free. And
/// `File::try_lock` — stable since 1.89 and the planned replacement for the raw `flock` FFI once
/// MSRV reaches it (#257) — is implemented over `LockFileEx` on Windows, so this choice puts both
/// arms on one future convergence point instead of stranding the Windows arm off it.
///
/// **What the file lock does NOT carry over.** Two differences, and the second is the one with a
/// consumer.
///
/// `flock` is ADVISORY; a Windows byte-range lock is MANDATORY, so a third-party reader of
/// `daemon.lock` is refused rather than ignored. Nothing in this crate reads the file's CONTENT —
/// it is zero bytes and exists only to be locked — so that one has no consumer here. It is
/// recorded because it is a real semantic difference and the next reader should not have to
/// rediscover it.
///
/// RELEASE IS NOT PROMISED TO BE PROMPT ON WINDOWS, where `flock`'s is, and the caveat covers more
/// than a crash. `LockFileEx`'s documented Remarks put both paths in ONE clause — a process that
/// "terminates with a portion of a file locked **or closes a file that has outstanding locks**" —
/// say the time the system takes to unlock them "depends upon available system resources", and
/// recommend that a process explicitly unlock what it locked. So the ORDINARY drop is inside the
/// caveat too, not only the crash, which is why the recommendation is TAKEN here rather than
/// dismissed: [`unlock_exclusive`] runs before the handle closes, on `Drop` and on the probe in
/// [`InstanceLock::is_held`]. Both paths matter to this crate. `is_held` acquires and releases in
/// one breath, and the AC2 test drops a lock and immediately re-acquires it — the two places an
/// unpromised release latency would bite first, and both are on the target the test is committed
/// to grade.
///
/// What the explicit unlock CANNOT reach is a kill or a crash: the process is gone before any
/// `Drop` runs, so an indeterminate window remains in which [`InstanceLock::is_held`] answers
/// `true` and a restart takes [`Error::AlreadyRunning`]. That is a startup-latency and
/// operator-confusion hazard rather than a correctness one — the lock still cannot be held by two
/// live daemons — and the no-reaping conclusion above still holds, because a stale LOCK clears
/// itself eventually where a stale PID FILE never would. It is named rather than fixed because
/// nothing in this process can fix it, and it is UNMEASURED like the rest of this arm.
///
/// UNMEASURED on Windows, like every other line of that arm: no CI job compiles this target
/// (**#978**). The reasoning above is from the documented API contract, not from a run.
pub(crate) struct InstanceLock {
    // Held open purely to keep the lock. Dropping it releases the lock — explicitly first, via the
    // `Drop` below, then implicitly as the handle closes; the process dying releases it too, but
    // only on the operating system's own schedule (see the doc above).
    file: File,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        unlock_exclusive(&self.file);
    }
}

/// What one non-blocking exclusive-lock attempt did — the three-way outcome both targets narrow
/// to, so the state machine [`InstanceLock`] runs on top of it is target-neutral and its
/// fail-closed shape is readable in one place (issue #976).
///
/// The same split [`super::peer_auth::is_same_user`] makes for peer identity: the per-target
/// syscall yields a value, and the DECISION over that value is written once.
enum LockAttempt {
    /// The lock is now held on the passed handle, and is released when it closes.
    Acquired,
    /// Another open file description holds it — a live daemon.
    Contended,
    /// The lock could not be attempted or failed for any other reason.
    Failed(std::io::Error),
}

/// Open (creating if needed) the lock file at `path` for the lock attempts below.
///
/// `0600` from the start, so the file is never briefly world-readable. It carries no content —
/// it exists only to be locked — but a predictable per-user path under the support dir should
/// not be a file anybody else can open either.
#[cfg(unix)]
fn open_lock_file(path: &Path, create: bool) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    if !create {
        return OpenOptions::new().read(true).open(path);
    }
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
}

/// Open (creating if needed) the lock file at `path` for the lock attempts below.
///
/// The `0600` the Unix arm sets has NO analogue here and is deliberately not faked with one: a
/// new file inherits the enclosing directory's ACL, and the file-mode-to-ACL layer is **#974**'s,
/// not this item's. So on Windows this file's reachability is whatever the support directory
/// grants — stated rather than papered over, because it is the same asymmetry ADR-0037
/// § Decision 2 records for the control channel, where the `0700` directory also has no analogue.
///
/// The lock's own guarantee does not rest on the mode either way: the file carries no content —
/// it exists only to be locked — and a foreign user who can OPEN it still cannot make our
/// `LockFileEx` succeed while we hold it, because a byte-range lock is contended per handle by
/// the kernel rather than by anything the file's ACL says.
#[cfg(windows)]
fn open_lock_file(path: &Path, create: bool) -> std::io::Result<File> {
    if !create {
        return OpenOptions::new().read(true).open(path);
    }
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
}

/// One non-blocking exclusive `flock(LOCK_EX|LOCK_NB)` on `file`.
#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> LockAttempt {
    use std::os::unix::io::AsRawFd;

    // Raw `flock` FFI, kept un-wrapped by ADR-0004: kept raw rather than
    // adding a `rustix` production dependency; the std wheel
    // (`File::try_lock`, stable 1.89) is the planned replacement once MSRV
    // reaches 1.89 (see #257).
    // SAFETY: `flock` takes a valid open fd (owned by `file`, which outlives
    // the call) and the two flag constants; it has no other preconditions.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return LockAttempt::Acquired;
    }
    let err = std::io::Error::last_os_error();
    // EWOULDBLOCK (== EAGAIN) means another instance holds the lock; anything
    // else is a genuine I/O failure.
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        LockAttempt::Contended
    } else {
        LockAttempt::Failed(err)
    }
}

/// One non-blocking exclusive `LockFileEx` over the first byte of `file` (issue #976).
///
/// `LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY` is the direct analogue of
/// `LOCK_EX | LOCK_NB`: exclusive, and returning at once rather than blocking. Contention is
/// `ERROR_LOCK_VIOLATION`, which is this arm's `EWOULDBLOCK`.
///
/// **One byte at offset zero**, and the range is arbitrary only in the sense that any range would
/// do provided BOTH the acquire and the probe use the same one — which is why there is one
/// function rather than two. Windows documents locking a range beyond the current end-of-file as
/// legal, so the file staying empty is not a problem.
///
/// **Locks conflict between HANDLES, not between processes**, which is the property both callers
/// depend on and the one a reader coming from `flock` should confirm rather than assume. A second
/// `LockFileEx` against a separately-opened handle is refused even inside the same process — so
/// the second-`acquire` refusal and the separate-open `is_held` probe both behave as their Unix
/// counterparts do over distinct open file descriptions. (A DUPLICATED handle shares its locks;
/// nothing here duplicates one.)
/// Release the exclusive lock [`try_lock_exclusive`] took, before the handle is closed.
///
/// A no-op on Unix, and deliberately: `flock` releases at close with no documented latency caveat,
/// so an explicit `LOCK_UN` would buy nothing and would add a syscall to every drop. On Windows it
/// is `UnlockFileEx` over the same one-byte range, which is what that API's own Remarks recommend
/// — see [`InstanceLock`] for the sentence and for the case this still cannot reach.
///
/// Best-effort by construction: it runs on a drop path and on a probe that has already decided its
/// answer, so there is no caller left to return a failure to. A failure leaves exactly the state
/// the close would have left anyway.
#[cfg(unix)]
fn unlock_exclusive(_file: &File) {}

#[cfg(windows)]
fn unlock_exclusive(file: &File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
    use windows_sys::Win32::System::IO::OVERLAPPED;

    // SAFETY: the same contract `try_lock_exclusive` documents for the matching lock call — a
    // zeroed `OVERLAPPED` carrying offset 0 with a null `hEvent`, a live handle owned by `file`,
    // and the one-byte length that call took. The result is discarded on purpose (see above).
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    unsafe {
        UnlockFileEx(file.as_raw_handle() as HANDLE, 0, 1, 0, &mut overlapped);
    }
}

#[cfg(windows)]
fn try_lock_exclusive(file: &File) -> LockAttempt {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    // The offset the lock starts at, carried in the OVERLAPPED rather than as an argument. Zeroed
    // whole: `hEvent` must be null for a synchronous handle, and every other member is reserved.
    // SAFETY: `OVERLAPPED` is a plain `#[repr(C)]` struct of integers and pointers, for which the
    // all-zero bit pattern is valid (a null `hEvent` is exactly what this call wants).
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    // SAFETY: `LockFileEx` takes a valid open handle (owned by `file`, which outlives the call),
    // the two flag constants, a reserved zero, the two halves of a 1-byte length, and a live
    // `OVERLAPPED` on this stack frame which also outlives the call. The handle is synchronous
    // (Rust's `File` opens without `FILE_FLAG_OVERLAPPED`), so the call completes before it
    // returns and the structure is not retained.
    let ok = unsafe {
        LockFileEx(
            file.as_raw_handle() as HANDLE,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if ok != 0 {
        return LockAttempt::Acquired;
    }
    let err = std::io::Error::last_os_error();
    // ERROR_LOCK_VIOLATION is what LOCKFILE_FAIL_IMMEDIATELY reports for a range another handle
    // already holds — another instance is alive. Anything else is a genuine I/O failure.
    if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        LockAttempt::Contended
    } else {
        LockAttempt::Failed(err)
    }
}

impl InstanceLock {
    /// Acquire the lock at `path`, creating the file if needed (`0600` on Unix; see
    /// [`open_lock_file`] for why Windows has no analogue). [`Error::AlreadyRunning`] if another
    /// instance already holds it.
    pub(crate) fn acquire(path: &Path) -> Result<Self> {
        let file = open_lock_file(path, true)?;
        match try_lock_exclusive(&file) {
            LockAttempt::Acquired => Ok(Self { file }),
            LockAttempt::Contended => Err(Error::AlreadyRunning),
            LockAttempt::Failed(err) => Err(Error::Io(err)),
        }
    }

    /// Probe whether the single-instance lock at `path` is currently held by a LIVE daemon,
    /// WITHOUT disturbing it (issue #396) — the lock-fallback half of the `daemon status`
    /// liveness projection (socket-primary, lock-fallback). A non-blocking exclusive lock over a
    /// fresh read-only open (a separate open file description / handle, so it contends with the
    /// daemon's held lock exactly as a second `run` would):
    /// - contention ⇒ another process holds it — a daemon is alive even if its control
    ///   socket is not answering yet (the honest startup / wedged case; NOT "not running").
    /// - a successful acquire ⇒ no live holder; the lock is released the instant `file` drops
    ///   at the end of this scope — nothing is started, stopped, or signalled.
    /// - an absent lock file ⇒ the daemon has never created it ⇒ not running.
    ///
    /// ANY-PROVENANCE by construction, on every target (issue #976 AC3): it asks the KERNEL who
    /// holds a lock on a path, so it answers about a daemon started by any means — launchd, a
    /// Windows service, a shell, the menu-bar app — and never only about one this code started.
    /// That is the property that ruled a named mutex out on Windows; see [`InstanceLock`].
    ///
    /// Read-only by construction (the `daemon status` AC: no process is started/stopped/
    /// signalled). Kept beside [`Self::acquire`] so the raw locking FFI stays localized
    /// (ADR-0004) and so both ends lock the same range.
    ///
    /// Note the one inherent tradeoff: probing a FREE lock necessarily acquires it for the
    /// ~microseconds until `file` drops — neither `flock` nor `LockFileEx` has a
    /// test-without-acquire mode, so this acquire-then-release is the canonical liveness-probe
    /// shape. It is benign here because the caller runs this ONLY as the socket-primary fallback
    /// (a real startup already holds the lock, so the probe fails to acquire and never contends);
    /// the sole residual race is a `run` whose own `acquire` lands in that microsecond window and
    /// self-refuses (exit 3), which is vanishingly unlikely and self-correcting on retry.
    pub(crate) fn is_held(path: &Path) -> Result<bool> {
        let file = match open_lock_file(path, false) {
            Ok(file) => file,
            // No lock file at all ⇒ the daemon has never created it ⇒ not held.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(Error::Io(err)),
        };
        match try_lock_exclusive(&file) {
            // Acquired ⇒ no live holder. Unlock EXPLICITLY before `file` drops: closing a handle
            // with an outstanding lock is inside the same "depends upon available system
            // resources" caveat as termination on Windows, and this probe is the one path that
            // takes a lock only to hand it straight back (`InstanceLock`'s own doc).
            LockAttempt::Acquired => {
                unlock_exclusive(&file);
                Ok(false)
            }
            // Another instance holds the lock — a live daemon.
            LockAttempt::Contended => Ok(true),
            LockAttempt::Failed(err) => Err(Error::Io(err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unix-only because the scenario IS a symlink re-point: `std::os::windows::fs::symlink_file`
    // needs either Developer Mode or `SeCreateSymbolicLinkPrivilege`, so the same test on Windows
    // would fail for a reason that has nothing to do with what it asserts. The BEHAVIOUR it pins —
    // resolve per cycle, never freeze at construction — is target-neutral and lives in
    // `RealKeepWarmEngine::resolve_binary`; only the fixture is not (issue #976).
    #[cfg(unix)]
    #[tokio::test]
    async fn real_keep_warm_engine_resolves_the_binary_per_cycle_not_frozen_at_construction() {
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
        assert_eq!(engine.resolve_binary().await.unwrap(), link);

        // The updater removes the pointed-at binary: the SAME engine resolves to a NON-FATAL error
        // on its next cycle (the daemon leaves the canonical item untouched, retried next cycle),
        // never a reuse of a stale frozen path.
        std::fs::remove_file(&installed).unwrap();
        assert!(matches!(
            engine.resolve_binary().await,
            Err(crate::error::Error::ClaudeBinaryNotFound)
        ));
    }

    // --- single-instance lock ----------------------------------------------

    /// The single-instance guarantee itself, and issue #976's AC2 on every target that compiles.
    ///
    /// TARGET-NEUTRAL since #976, where it was Unix-only because of the mode assertion it used to
    /// carry (now `the_lock_file_is_owner_only_on_unix` below). The claim — a second acquisition
    /// while the first is held is REFUSED, and the refusal is `AlreadyRunning` rather than a
    /// generic I/O error — is the same on `flock` and on `LockFileEx`, and running it everywhere
    /// is what makes the shared state machine in `InstanceLock` more than an assertion about one
    /// arm. AC2 asks for this "verified by a test on the Windows CI job"; that job is **#978** and
    /// does not exist yet, so what this commit can deliver is the test, committed and unskipped,
    /// which grades the Windows arm the moment the job turns on.
    ///
    /// The two acquisitions are separate OPENS in one process, which is what both mechanisms
    /// contend on: `flock` locks the open file description, and `LockFileEx` locks per handle.
    #[test]
    fn instance_lock_blocks_a_second_acquisition_then_frees_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");

        let lock = InstanceLock::acquire(&path).expect("first acquisition succeeds");
        // A second acquisition while the first is held is refused — the exit-3
        // signal a second `run` exits on, without disturbing the first.
        assert!(matches!(
            InstanceLock::acquire(&path),
            Err(Error::AlreadyRunning)
        ));
        // Dropping the holder releases the lock (kernel-released on close).
        drop(lock);
        let _reacquired =
            InstanceLock::acquire(&path).expect("the lock is free after the first is dropped");
    }

    /// The lock file's mode, split out of the test above by issue #976 because it is the one part
    /// of it that does NOT generalize: `0600` has no Windows analogue and `open_lock_file`'s
    /// Windows arm says so rather than faking one.
    #[cfg(unix)]
    #[test]
    fn the_lock_file_is_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");
        let _lock = InstanceLock::acquire(&path).expect("acquire creates the lock file");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn instance_lock_is_held_probe_reports_absent_held_and_freed() {
        // Issue #396: the read-only lock-fallback probe behind `daemon status`. It must never
        // disturb a live holder (a non-blocking exclusive lock over a SEPARATE open — `flock` on
        // Unix, `LockFileEx` on Windows), and it distinguishes three states: absent lock file,
        // held-by-a-live-daemon, and present-but-free.
        //
        // Issue #976 AC3 (any-provenance) is what the separate open buys: the probe asks the
        // KERNEL who holds a lock on this PATH, so it answers about a holder started by any means
        // rather than about one this code started. That is what a named mutex could not do, and
        // this test is the executable half of it — the half it cannot reach is a holder in another
        // PROCESS, which needs a second binary; neither lock primitive distinguishes the caller,
        // so the separate open is the same contention a second process presents.
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
