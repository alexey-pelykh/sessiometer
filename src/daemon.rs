// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The poll loop, its decision state, and the daemon lifecycle.
//!
//! [`Daemon`] is generic over its four seams — [`RosterPoller`],
//! [`CredentialStore`], [`AccountStash`] and [`Clock`] — so the whole loop runs
//! hermetically against in-memory fakes in tests: no live quota, no keychain, no
//! real time, no signals, no socket. The current-thread runtime (see `main`) is
//! what lets the seams stay free of `Send` bounds.
//!
//! ## One cycle ([`Daemon::tick`])
//!
//! 1. **Identify the active account.** Resolved once and cached, updated on each
//!    swap — see [`Daemon::resolve_active`]. `None` (un-identifiable) → poll-only,
//!    never swap.
//! 2. **Poll every account.** The active account through the canonical credential
//!    (its token is the freshest), every other through its stash — the per-account
//!    seam #5 anticipated (`CurlTransport` is generic over [`CredentialStore`]). A
//!    failed poll just drops that account from this cycle; the loop never swaps on
//!    missing data.
//! 3. **Decide and swap.** If the active account's SESSION usage is at/above the
//!    session swap-away trigger, OR its WEEKLY usage is at/above the separate
//!    (typically higher) weekly trigger — each drawn this cycle from its own
//!    timing strategy and clamped to range (issues #38, #41) — pick the viable
//!    target whose weekly quota resets soonest (issue #37, [`pick_target`]) and run
//!    the out-of-band [`swap::swap`]. A per-cycle jittered post-swap cooldown (issue
//!    #10) refuses a re-swap until it has elapsed, bounding oscillation between two
//!    near-exhausted accounts.
//!
//! The session trigger, the weekly trigger (#41), the cooldown, and the
//! inter-poll interval are each a
//! [`Strategy`] (base + optional jitter, issue #38): a fresh value is drawn and
//! clamped to the parameter's range every cycle through the [`SplitMix64`] seam,
//! so polling/swaps decorrelate across accounts and cycles instead of running in
//! lockstep. The seam is seeded from entropy in production and from a fixed seed
//! in tests (`Daemon::with_seed`), keeping the draws deterministic under test.
//!
//! ## Lifecycle (the run loop, [`run_loop`])
//!
//! - **Single-instance lock** ([`InstanceLock`]) — a kernel advisory `flock` held
//!   for the process lifetime; a second `run` exits `3`.
//! - **Reconcile-on-start** ([`Daemon::reconcile_on_start`]) — heal a crash /
//!   third-writer `oauthAccount`↔canonical mismatch before the first poll.
//! - **Control socket** ([`UnixControl`]) — a `0600` Unix-domain socket serving
//!   newline-delimited JSON `status`, carrying handles + percentages only, never a
//!   token (issue #15).
//! - **Graceful shutdown** ([`Shutdown`]) — SIGINT / SIGTERM is observed only
//!   *between* ticks, so an in-flight swap always runs to completion (#6 is
//!   no-half-swap): complete-or-abort, never a torn swap.
//!
//! The minimal `last_swap` shown by `status` (the handle swapped to + a relative
//! age) is surfaced here (#8), and every swap / all-exhausted / token-rejection /
//! lock-wait is recorded to the structured event log (#9, via
//! [`crate::observability`]). The post-swap cooldown that bounds oscillation (#10)
//! is wired into the decision below — a re-swap is refused until the per-cycle
//! jittered cooldown has elapsed, and the swap-target session floor is opt-in (off
//! by default). When EVERY account is weekly-exhausted there is no viable target
//! ([`TickAction::NoViableTarget`], #11): the loop enters the all-exhausted
//! terminal state — it HOLDS (no swap, so no thrash) and emits a single
//! edge-triggered `all_exhausted` event naming the least-bad account (the soonest
//! weekly `resets_at`), which now fills the event log's `resets_at=` field.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::net::UnixListener;
use tokio::signal::unix::{signal, Signal, SignalKind};

use crate::claude_state;
use crate::config::{Account, Tunables};
use crate::error::{Error, Result};
use crate::keychain::{
    CanonicalChange, CanonicalWatch, Credential, CredentialStore, RealCredentialStore,
};
use crate::observability::{Event, EventLog, SwapReason};
use crate::stash::{AccountStash, RealAccountStash, StashedAccount};
use crate::swap::{self, SwapDecision};
use crate::timing::{SplitMix64, Strategy};
use crate::usage::{CurlTransport, RealUsageSource, Usage, UsageSource};

/// Per-cycle clamp bounds for the swap-away trigger draw, in PERCENT — mirrors
/// config's `session_trigger` range so a jittered draw can never escape it.
const TRIGGER_PCT_LO: f64 = 50.0;
const TRIGGER_PCT_HI: f64 = 99.0;
/// Per-cycle clamp bounds for the WEEKLY swap-away trigger draw, in PERCENT
/// (issue #41) — mirrors config's `weekly_trigger` range. Its own constants
/// (numerically equal to the session bounds today) so the two triggers stay
/// independently bounded.
const WEEKLY_TRIGGER_PCT_LO: f64 = 50.0;
const WEEKLY_TRIGGER_PCT_HI: f64 = 99.0;
/// Per-cycle clamp bounds for the cooldown draw, in seconds (config range).
const COOLDOWN_SECS_LO: f64 = 0.0;
const COOLDOWN_SECS_HI: f64 = 3600.0;
/// Per-cycle clamp bounds for the poll-interval draw, in seconds (config range).
const POLL_SECS_LO: f64 = 5.0;
const POLL_SECS_HI: f64 = 3600.0;

/// First back-off after a cycle finds the keychain LOCKED (issue #13) — short, so
/// a brief lock (the operator mid-unlock) is recovered from within a second.
const LOCK_BACKOFF_BASE: Duration = Duration::from_secs(1);
/// Ceiling for the locked-keychain back-off (issue #13). The wait doubles each
/// locked cycle from [`LOCK_BACKOFF_BASE`] but never exceeds this, settling at one
/// read attempt per minute — prompt to resume on unlock, yet not a busy-spin on a
/// keychain that stays locked. The daemon NEVER auto-unlocks or prompts; a locked
/// keychain is the operator's to open (a non-interactive read just fails, exit 36).
const LOCK_BACKOFF_CAP: Duration = Duration::from_secs(60);

/// Time seam: the daemon reads "now" and sleeps until the next poll through
/// this, so a fake can drive time and make the loop run instantly in tests.
pub(crate) trait Clock {
    /// The current instant.
    fn now(&self) -> Instant;
    /// Sleep for `interval` — the (jittered) wait until the next poll, computed
    /// per cycle by the daemon (issue #38). The clock no longer owns the
    /// interval; it just sleeps the duration it is handed.
    async fn tick(&self, interval: Duration);
}

/// Real clock: monotonic `Instant::now` and a Tokio sleep of the handed interval.
#[derive(Default)]
pub(crate) struct RealClock;

impl RealClock {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    async fn tick(&self, interval: Duration) {
        tokio::time::sleep(interval).await;
    }
}

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
/// fake ([`tests::FakeRosterPoller`]) returns scripted per-account readings.
pub(crate) trait RosterPoller {
    /// Poll `account`'s usage. `active` selects the token source: the canonical
    /// keychain item for the active account (whose token is the freshest), or the
    /// account's stash for any other.
    async fn poll(&self, account: &Account, active: bool) -> Result<Usage>;
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
    async fn poll(&self, account: &Account, active: bool) -> Result<Usage> {
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
                service: account.stash.clone(),
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

/// Control seam: serve control-socket connections. The production impl
/// ([`UnixControl`]) accepts on a `UnixListener`; the run loop's idle select
/// drives it between polls. The test no-op never resolves, so it never wins the
/// select.
pub(crate) trait Control {
    /// Serve at most one control connection from `snapshot`, then resolve.
    async fn serve(&self, snapshot: &StatusSnapshot);
}

/// Production control: accept one client at a time on the bound socket and answer
/// from the latest snapshot.
pub(crate) struct UnixControl {
    listener: UnixListener,
}

impl UnixControl {
    pub(crate) fn new(listener: UnixListener) -> Self {
        Self { listener }
    }
}

impl Control for UnixControl {
    async fn serve(&self, snapshot: &StatusSnapshot) {
        if let Ok((stream, _addr)) = self.listener.accept().await {
            // Best-effort: a malformed or disconnected client must never crash the
            // daemon — drop the exchange (the reply carries nothing secret anyway).
            let _ = serve_control(stream, snapshot).await;
        }
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
}

/// The latest per-account reading the daemon exposes — over the control socket
/// and in the event log. Non-secret by construction: a handle (label), the active
/// flag, and percentages — never a token or email (issue #15).
#[derive(Debug, Clone, Default)]
pub(crate) struct StatusSnapshot {
    pub(crate) accounts: Vec<AccountReading>,
    /// The most recent swap as of this cycle (issue #8), or `None` until the
    /// first. Already projected to a relative age; [`status_response`] copies it
    /// straight onto the wire.
    pub(crate) last_swap: Option<LastSwapLine>,
}

/// One account's latest reading.
#[derive(Debug, Clone)]
pub(crate) struct AccountReading {
    pub(crate) label: String,
    pub(crate) active: bool,
    /// Whether the account is in the rotation (issue #36) — surfaced so `status`
    /// can mark a parked account. A disabled account is shown but never swapped to.
    pub(crate) enabled: bool,
    /// Whether the account is QUARANTINED — its credential is dead and needs a
    /// re-login (issue #42). The durable "needs re-login" status `status` surfaces;
    /// non-secret (a plain flag on the account's handle).
    pub(crate) quarantined: bool,
    pub(crate) usage: Option<Usage>,
}

/// The control socket's `status` reply — handles + percentages + a minimal
/// `last_swap`, and nothing else (issue #15: never a token or email). Derives
/// both `Serialize` (the daemon writes it) and `Deserialize` (the `status` client
/// reads it), so this one definition is the whole wire contract. The richer
/// swap-history event-log view is #9.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StatusResponse {
    pub(crate) accounts: Vec<AccountStatusLine>,
    /// The most recent swap, or `null` if none has happened this run.
    pub(crate) last_swap: Option<LastSwapLine>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AccountStatusLine {
    /// The operator-chosen handle (label) — never the email (issue #15).
    pub(crate) label: String,
    pub(crate) active: bool,
    /// Whether the account is in the rotation (issue #36); `false` for a parked
    /// account, which `status` marks. Non-secret — a plain flag.
    pub(crate) enabled: bool,
    /// Whether the account is QUARANTINED — its credential is dead and needs a
    /// re-login (issue #42). The durable "needs re-login" status; `false` for a
    /// healthy account. Non-secret — a plain flag.
    pub(crate) quarantined: bool,
    /// Last-polled session-window usage percent (`0..=100`); `null` if the last
    /// poll for this account failed (never a fabricated `0`).
    pub(crate) session_pct: Option<u8>,
    /// Last-polled weekly-window usage percent (`0..=100`).
    pub(crate) weekly_pct: Option<u8>,
}

/// The minimal `last_swap` shown by `status` (issue #8): the handle swapped TO
/// and a relative age (`secs_ago`, computed as of the last poll). Non-secret by
/// construction — a label + an integer, never a token or email (issue #15). The
/// swap *history* (richer records) is #9. One serializable type for both
/// [`StatusSnapshot`] (built each cycle) and [`StatusResponse`] (the wire).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LastSwapLine {
    /// The label of the account swapped TO — never the email (issue #15).
    pub(crate) to: String,
    /// Whole seconds since the swap completed, as of the last poll.
    pub(crate) secs_ago: u64,
}

/// The `{"cmd": "..."}` control request.
#[derive(Deserialize)]
struct ControlRequest {
    cmd: String,
}

/// Project a [`StatusSnapshot`] into the wire [`StatusResponse`]. Sourced solely
/// from non-secret fields, so it can never carry a token or email (issue #15).
fn status_response(snapshot: &StatusSnapshot) -> StatusResponse {
    StatusResponse {
        accounts: snapshot
            .accounts
            .iter()
            .map(|account| AccountStatusLine {
                label: account.label.clone(),
                active: account.active,
                enabled: account.enabled,
                quarantined: account.quarantined,
                session_pct: account.usage.map(|u| to_pct(u.session)),
                weekly_pct: account.usage.map(|u| to_pct(u.weekly)),
            })
            .collect(),
        // Already computed (a label + a relative age) at snapshot build; copy it
        // to the wire (issue #8).
        last_swap: snapshot.last_swap.clone(),
    }
}

/// A usage fraction in `[0.0, 1.0]` as a rounded, clamped `0..=100` percent.
fn to_pct(fraction: f64) -> u8 {
    (fraction * 100.0).round().clamp(0.0, 100.0) as u8
}

/// Build the one-line reply to a control request line.
fn control_reply(line: &str, snapshot: &StatusSnapshot) -> String {
    match serde_json::from_str::<ControlRequest>(line) {
        Ok(request) if request.cmd == "status" => serde_json::to_string(&status_response(snapshot))
            .unwrap_or_else(|_| r#"{"error":"encode failed"}"#.to_owned()),
        Ok(_) => r#"{"error":"unknown command"}"#.to_owned(),
        Err(_) => r#"{"error":"malformed request"}"#.to_owned(),
    }
}

/// Serve one control exchange: read one newline-delimited JSON request and write
/// one newline-delimited JSON reply. Generic over the stream so it is testable
/// over an in-memory duplex without binding a real socket.
async fn serve_control<RW>(stream: RW, snapshot: &StatusSnapshot) -> Result<()>
where
    RW: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let mut buffered = tokio::io::BufReader::new(stream);
    let mut line = String::new();
    buffered.read_line(&mut line).await?;
    let reply = control_reply(line.trim_end(), snapshot);
    buffered.write_all(reply.as_bytes()).await?;
    buffered.write_all(b"\n").await?;
    buffered.flush().await?;
    Ok(())
}

/// What the loop decided to do this cycle — logged, and asserted on in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TickAction {
    /// Active account is below the swap-away trigger — stay put.
    Held,
    /// Swapped the active credential from roster index `from` to `to`.
    Swapped { from: usize, to: usize },
    /// EMERGENCY-swapped from a confirmed-DEAD active account `from` to `to`, the
    /// soonest-reset viable target (issue #42) — bypassing the swap-away trigger and
    /// the cooldown. Distinct from [`Swapped`](Self::Swapped) so a forced
    /// dead-credential escape is visible in tests and outcomes.
    EmergencySwapped { from: usize, to: usize },
    /// The active account's credential is DEAD (quarantined, #42) but no other
    /// account is a viable swap target — the daemon holds on the dead active, unable
    /// to escape. The `credential_dead` signal already fired on the death transition,
    /// so this state is silent (no repeat-spam). The dead-credential cousin of
    /// [`NoViableTarget`](Self::NoViableTarget).
    ActiveDeadNoTarget,
    /// Active is over the trigger but no other account is a viable target: every
    /// other account is weekly-exhausted (or, with the opt-in session floor
    /// enabled, all over it). The all-exhausted terminal state (#11) — the loop
    /// holds and emits one edge-triggered `all_exhausted` signal, never swapping.
    NoViableTarget,
    /// The active account could not be identified — poll-only, no swap.
    SkippedActiveUnknown,
    /// The active account's reading was unavailable this cycle (transient / 401 /
    /// unreadable) — never swap on missing data.
    SkippedActiveUnavailable,
    /// Over the trigger but within the post-swap cooldown — the re-swap is
    /// refused to bound oscillation (issue #10).
    SkippedCooldown,
    /// A swap was attempted but the engine returned an error; #6 is no-half-swap,
    /// so the state is coherent and the loop retries next cycle.
    SwapFailed,
    /// The keychain was LOCKED when this cycle went to read the canonical
    /// credential (issue #13). All work is deferred — no resolve, no poll, no swap
    /// — and the loop backs off (the wait is carried in
    /// [`TickOutcome::next_wait`]). The daemon never auto-unlocks or prompts.
    KeychainLocked,
}

/// The result of one poll iteration.
#[derive(Debug)]
pub(crate) struct TickOutcome {
    /// What the loop decided to do.
    pub(crate) action: TickAction,
    /// The structured log events this cycle generated (issue #9): the
    /// poll-outcome events (401 / keychain-locked / 403) in roster order, then the
    /// decision event (swap / all-exhausted) if any. `run_loop` emits each to the
    /// event log; a Hold or a skip generates none.
    pub(crate) events: Vec<Event>,
    /// The per-account readings this cycle, for the control socket (`status`).
    pub(crate) snapshot: StatusSnapshot,
    /// How long the run loop should wait before the next tick. `None` = the normal
    /// jittered poll interval (issue #38); `Some(d)` = an explicit wait — the
    /// locked-keychain back-off (issue #13), which grows the gap between retries
    /// while the keychain stays locked.
    pub(crate) next_wait: Option<Duration>,
}

/// The last swap the loop performed: the handle swapped TO and when. One record
/// serves two readers — the cooldown floor (its `at`) and the `status` display
/// (#8, projected to a [`LastSwapLine`] at snapshot time).
#[derive(Debug, Clone)]
struct LastSwap {
    /// Label of the account swapped TO (non-secret; issue #15).
    to: String,
    /// When the swap completed — monotonic, so it is both the cooldown floor and
    /// the base for the `status` "seconds ago". Process-local: never serialized
    /// directly (an [`Instant`] is meaningless across the socket).
    at: Instant,
}

/// Per-account health carried ACROSS ticks — the dead-credential lifecycle state
/// (issue #42), indexed by roster position. Daemon-level (not per-poll) because the
/// 401 streak and the recovery probe must accumulate across ticks: a per-poll
/// counter is rebuilt every cycle and never observes a streak (the prerequisite the
/// issue fixed). Sized to the roster in [`Daemon::new`].
#[derive(Default, Clone)]
struct AccountHealth {
    /// Consecutive non-scope 401s on this account's stored token. Incremented on a
    /// 401, reset to 0 on ANY non-401 outcome (success, 403, transient, locked). The
    /// `consecutive=` field of a `monitor_401` event while still healthy; reaching
    /// `monitor_401_n` declares the account DEAD ([`quarantined`](Self::quarantined)).
    consec_401: u32,
    /// Whether this account is QUARANTINED — its credential is dead (rejected
    /// `monitor_401_n` times in a row), so the daemon stops polling and selecting it
    /// for the rotation until the operator re-logs-in. The durable "needs re-login"
    /// status surfaced by `status` (issue #42), and the edge that fires the
    /// [`Event::CredentialDead`] / [`Event::CredentialRestored`] signals exactly once
    /// per transition.
    quarantined: bool,
    /// Consecutive successful recovery probes after a quarantined account is
    /// re-logged-in (issue #42). A quarantined account is polled only when it is the
    /// active account — which it becomes only after the operator re-logs-in it (a
    /// canonical-change re-stash, #13) — so each `Live` poll here is a recovery
    /// probe; reaching `monitor_recovery_m` consecutive un-quarantines it. Reset to 0
    /// on any non-success, so the M successes must be consecutive.
    recovery_successes: u32,
}

/// Per-loop decision state carried across polls.
#[derive(Default)]
struct DecisionState {
    /// 1-based count of polls taken.
    ticks: u64,
    /// Roster index of the active account, resolved once and updated on each
    /// swap. `None` until first resolved (then the loop polls but never swaps).
    active: Option<usize>,
    /// The last swap performed, or `None` until the first. Drives both the
    /// post-swap cooldown (anti-oscillation, #10): a re-swap is refused until this
    /// cycle's jittered `cooldown` has elapsed since this swap, so two
    /// near-exhausted accounts cannot ping-pong — and the minimal `last_swap`
    /// shown by `status` (#8).
    last_swap: Option<LastSwap>,
    /// Per-account health carried across ticks (issue #42), indexed by roster
    /// position: the consecutive-401 streak (feeding the `monitor_401` log event and
    /// the dead-credential threshold), the quarantine flag, and the recovery-probe
    /// count. Sized to the roster in [`Daemon::new`]. See [`AccountHealth`].
    health: Vec<AccountHealth>,
    /// Edge-trigger guard for the all-exhausted signal (issue #11): set when an
    /// `all_exhausted` event is emitted, and cleared by [`Daemon::tick`] on any
    /// cycle that is NOT the no-viable-target state. So the signal fires exactly
    /// ONCE per all-exhausted episode — not once per poll while every account
    /// stays exhausted — and fires afresh if the state clears and is re-entered.
    signaled_all_exhausted: bool,
    /// The out-of-band canonical-change detector (issue #13 re-auth re-stash):
    /// tracks the last *committed* canonical credential so a rewrite by something
    /// other than the daemon — a `claude /login` re-auth, or a silent in-place
    /// token refresh — is detected and the owning account's stash refreshed. The
    /// daemon's OWN canonical writes (a swap) are committed into it so they are not
    /// re-detected as external. The *type* lives in [`crate::keychain`] so the
    /// dead-credential path (#42) reuses it; the daemon owns this instance.
    canonical_watch: CanonicalWatch,
    /// Current locked-keychain back-off (issue #13): `None` while the keychain is
    /// readable, `Some(d)` while locked — grown from [`LOCK_BACKOFF_BASE`] toward
    /// [`LOCK_BACKOFF_CAP`] each locked cycle and returned as
    /// [`TickOutcome::next_wait`]. Reset to `None` on the first readable cycle, so
    /// a later lock episode starts the climb afresh.
    lock_backoff: Option<Duration>,
    /// Edge-trigger guard for the keychain-locked signal (issue #13): set when a
    /// `keychain_locked_wait` event is emitted, cleared on the first readable
    /// cycle. So the signal fires exactly ONCE per lock episode — not once per
    /// backed-off retry while the keychain stays locked — mirroring
    /// `signaled_all_exhausted`.
    signaled_keychain_locked: bool,
}

/// The poll loop, generic over its four injectable seams.
pub(crate) struct Daemon<P, C, S, K> {
    roster: Vec<Account>,
    poller: P,
    store: C,
    stash: S,
    clock: K,
    claude_json: PathBuf,
    /// Per-cycle swap-away trigger strategy (issue #38): drawn + clamped to
    /// `50..=99` percent each cycle, then `/100` for the swap decision. Replaces
    /// the former fixed `session_trigger` fraction.
    trigger_strategy: Strategy,
    /// Per-cycle WEEKLY swap-away trigger strategy (issue #41): drawn + clamped to
    /// `50..=99` percent each cycle, then `/100` for the swap decision — the
    /// weekly-dimension counterpart of `trigger_strategy`, independent of it.
    weekly_trigger_strategy: Strategy,
    /// Opt-in swap-target session guard (#10): `Some(fraction)` only swaps TO an
    /// account whose session usage is below it (`session_floor / 100`); `None` (the
    /// default) disables the guard, leaving target choice to the soonest-reset rule
    /// alone (issue #37) — the configuration under which the cooldown alone bounds
    /// oscillation.
    session_floor: Option<f64>,
    /// Per-cycle post-swap cooldown strategy (issue #38; the #10 seam — see
    /// [`DecisionState`]): drawn + clamped to `0..=3600` s each cycle. Replaces
    /// the former fixed `cooldown` duration.
    cooldown_strategy: Strategy,
    /// Per-cycle poll-interval strategy (issue #38): drawn + clamped to
    /// `5..=3600` s each loop iteration by
    /// [`next_poll_interval`](Self::next_poll_interval).
    poll_strategy: Strategy,
    /// Jitter RNG seam — process entropy in production, a fixed seed in tests
    /// ([`with_seed`](Self::with_seed)) so per-cycle draws are deterministic.
    rng: SplitMix64,
    /// Consecutive non-scope 401s before an account's stored credential is treated
    /// as DEAD and quarantined (issue #42; config `monitor_401_n`, `1..=20`).
    monitor_401_n: u8,
    /// Consecutive successful recovery probes before a quarantined account is
    /// restored to the rotation (issue #42; config `monitor_recovery_m`, `1..=20`).
    monitor_recovery_m: u8,
    state: DecisionState,
}

impl<P, C, S, K> Daemon<P, C, S, K>
where
    P: RosterPoller,
    C: CredentialStore,
    S: AccountStash,
    K: Clock,
{
    pub(crate) fn new(
        roster: Vec<Account>,
        poller: P,
        store: C,
        stash: S,
        clock: K,
        claude_json: PathBuf,
        tunables: &Tunables,
    ) -> Self {
        // Per-account health carried across ticks (issue #42), one slot per account.
        let health = vec![AccountHealth::default(); roster.len()];
        Self {
            roster,
            poller,
            store,
            stash,
            clock,
            claude_json,
            trigger_strategy: tunables.trigger_strategy,
            weekly_trigger_strategy: tunables.weekly_trigger_strategy,
            session_floor: tunables.session_floor.map(|floor| f64::from(floor) / 100.0),
            cooldown_strategy: tunables.cooldown_strategy,
            poll_strategy: tunables.poll_strategy,
            rng: SplitMix64::from_entropy(),
            monitor_401_n: tunables.monitor_401_n,
            monitor_recovery_m: tunables.monitor_recovery_m,
            state: DecisionState {
                health,
                ..DecisionState::default()
            },
        }
    }

    /// Replace the jitter RNG with a deterministically-seeded one — the test seam
    /// for reproducible per-cycle draws (issue #38 AC).
    #[cfg(test)]
    pub(crate) fn with_seed(mut self, seed: u64) -> Self {
        self.rng = SplitMix64::new(seed);
        self
    }

    /// Reconcile `~/.claude.json` to the canonical credential on startup.
    ///
    /// Heals the post-swap crash window: a swap writes the incoming token to the
    /// canonical item (the commit) and then co-writes `~/.claude.json` (best
    /// effort); a crash in between leaves the canonical holding the incoming token
    /// while the display still shows the outgoing account. Here we find the roster
    /// account whose stash matches the canonical token and, if the display
    /// disagrees, co-write that account's `oauthAccount`. When the canonical
    /// matches no stash — the active account's token has merely drifted (refreshed
    /// in place) on a normal restart, or it belongs to an un-captured account —
    /// `~/.claude.json` is left untouched (there is nothing to heal). Best-effort
    /// and idempotent.
    ///
    /// This IS the issue #13 process-death-mid-swap recovery: the swap commits the
    /// canonical token before co-writing the display, so a crash in that window
    /// leaves the keychain authoritative and the display stale — exactly the
    /// mismatch healed here on the next start. No separate mechanism is needed; the
    /// keychain-first ordering plus this reconcile make a torn swap self-healing.
    pub(crate) async fn reconcile_on_start(&self) -> Result<()> {
        let canonical = self.store.read().await?;
        for account in &self.roster {
            let Ok(stashed) = self.stash.read(&account.stash).await else {
                continue;
            };
            if !stashed.credential.matches(&canonical) {
                continue;
            }
            // The canonical belongs to this account; ensure the display agrees.
            let displayed = claude_state::read_oauth_account_from(&self.claude_json)
                .ok()
                .map(|o| o.account_uuid().to_owned());
            if displayed.as_deref() != Some(stashed.oauth_account.account_uuid()) {
                claude_state::write_oauth_account(&self.claude_json, &stashed.oauth_account)?;
            }
            return Ok(());
        }
        // No stash matched the canonical token — leave ~/.claude.json untouched.
        Ok(())
    }

    /// Identify the active account: the roster index whose credential the
    /// canonical keychain item currently holds.
    ///
    /// Delegates to [`resolve_account_for`](Self::resolve_account_for) when the
    /// canonical is readable (token-match, then the `~/.claude.json` display
    /// fallback); when the canonical is unreadable (locked / not-found) it uses the
    /// displayed identity alone — the same json signal, the only one available
    /// without a token to match. `None` if neither resolves; the caller then polls
    /// but never swaps.
    async fn resolve_active(&self) -> Option<usize> {
        match self.store.read().await {
            Ok(canonical) => self.resolve_account_for(&canonical).await,
            Err(_) => {
                let oauth = claude_state::read_oauth_account_from(&self.claude_json).ok()?;
                self.roster
                    .iter()
                    .position(|a| a.account_uuid == oauth.account_uuid())
            }
        }
    }

    /// Run one poll iteration: resolve the active account, poll every roster
    /// account, then decide and (if warranted) swap.
    pub(crate) async fn tick(&mut self) -> TickOutcome {
        self.state.ticks += 1;
        let at = self.clock.now();
        let mut events: Vec<Event> = Vec::new();

        // Read the canonical credential ONCE at the top of the cycle. It drives
        // three things, all from this single read: lock detection (defer the whole
        // cycle and back off, #13), re-auth re-stash detection (the canonical
        // changed out-of-band, #13), and the active resolution below. A locked
        // keychain is the one outcome that short-circuits the entire tick.
        let canonical = match self.store.read().await {
            Err(Error::KeychainLocked { .. }) => return self.locked_tick(at),
            Ok(canonical) => {
                // Readable: clear any lock back-off and re-arm the edge-triggered
                // lock signal, then heal an out-of-band canonical change (#13).
                self.state.lock_backoff = None;
                self.state.signaled_keychain_locked = false;
                self.reconcile_canonical_change(&canonical, &mut events)
                    .await;
                Some(canonical)
            }
            Err(_) => {
                // Unreadable for a non-lock reason (not-found / transient): no
                // change-detection is possible, but it is not a lock — clear the
                // back-off and fall through to poll (the loop never swaps on missing
                // data, so an unknown active simply holds).
                self.state.lock_backoff = None;
                self.state.signaled_keychain_locked = false;
                None
            }
        };

        // Resolve the active account once; cached, updated on each swap, and dropped
        // by a re-auth re-stash above so it is re-resolved here. Reuse the canonical
        // already read above (the "read ONCE" intent) rather than re-reading it; only
        // the non-lock unreadable case falls back to the display-only resolve, whose
        // own store read has just failed anyway.
        if self.state.active.is_none() {
            self.state.active = match &canonical {
                Some(canonical) => self.resolve_account_for(canonical).await,
                None => self.resolve_active().await,
            };
        }
        let active = self.state.active;

        // Poll every account: the active one via the canonical credential (its
        // token is the freshest), every other via its stash. A failed poll
        // (transient / 401 / unreadable) leaves that account's reading absent — it
        // is simply not a candidate this cycle, and the loop never swaps on
        // missing data. The poll OUTCOME also feeds the event log (issue #9): a 401
        // or a 403 each emits one line, in roster order, and the per-account 401
        // streak is maintained here. (A locked keychain is handled at top-of-tick,
        // not per-account — see `locked_tick`.)
        let mut readings: Vec<Option<Usage>> = Vec::with_capacity(self.roster.len());
        for i in 0..self.roster.len() {
            // Skip polling a disabled (parked, #36) or QUARANTINED (dead, #42)
            // non-active account: neither is a swap target and a poll would waste a
            // `curl` (a quarantined account's stored token is dead). Its reading
            // stays absent (`None`), keeping `readings` indexed by roster position
            // and auto-excluding it from `pick_target`. The ACTIVE account is ALWAYS
            // polled — even when disabled (so its swap-AWAY trigger still fires) or
            // quarantined (so a dead active is re-probed: a 401 keeps it dead and
            // drives the emergency swap; a success is a recovery probe, #42).
            if active != Some(i) && (!self.roster[i].enabled || self.state.health[i].quarantined) {
                readings.push(None);
                continue;
            }
            let result = self.poller.poll(&self.roster[i], active == Some(i)).await;
            self.note_poll_outcome(i, &result, &mut events);
            readings.push(result.ok());
        }

        let action = self.decide_action(at, active, &readings, &mut events).await;
        // Edge-trigger the all-exhausted signal (issue #11): clear the guard
        // whenever this cycle is NOT the no-viable-target state, so a later
        // re-entry signals afresh. `decide_action` sets the guard (and emits once)
        // while in the state; this is the matching reset on the way out.
        if !matches!(action, TickAction::NoViableTarget) {
            self.state.signaled_all_exhausted = false;
        }
        let snapshot = self.snapshot(at, active, &readings);
        TickOutcome {
            action,
            events,
            snapshot,
            // A normal cycle waits the regular jittered poll interval (#38); only
            // the locked path (`locked_tick`) overrides this with a back-off.
            next_wait: None,
        }
    }

    /// The keychain was LOCKED when this cycle went to read the canonical
    /// credential (issue #13). Defer ALL work — no resolve, no poll, no swap — and
    /// back off so the daemon does not hammer a locked keychain. The back-off grows
    /// exponentially from [`LOCK_BACKOFF_BASE`], doubling each consecutive locked
    /// cycle up to [`LOCK_BACKOFF_CAP`]. The `keychain_locked_wait` event is
    /// edge-triggered: emitted ONCE when the lock is first observed (guarded by
    /// `signaled_keychain_locked`), not every backed-off retry. The daemon NEVER
    /// auto-unlocks or prompts — a locked keychain is the operator's to open; a
    /// non-interactive read just fails (exit 36), and the daemon waits it out.
    fn locked_tick(&mut self, at: Instant) -> TickOutcome {
        let mut events = Vec::new();
        if !self.state.signaled_keychain_locked {
            events.push(Event::KeychainLockedWait);
            self.state.signaled_keychain_locked = true;
        }
        // Grow the back-off: first locked cycle waits BASE, each subsequent one
        // doubles up to CAP. Stored so the next locked cycle continues the climb.
        let backoff = match self.state.lock_backoff {
            None => LOCK_BACKOFF_BASE,
            Some(prev) => (prev * 2).min(LOCK_BACKOFF_CAP),
        };
        self.state.lock_backoff = Some(backoff);
        // Build an all-absent snapshot so the control socket keeps answering while
        // locked: every reading is unavailable (the keychain is unreadable), but
        // `status` still lists the roster and the last swap rather than going dark.
        let readings = vec![None; self.roster.len()];
        let snapshot = self.snapshot(at, self.state.active, &readings);
        TickOutcome {
            action: TickAction::KeychainLocked,
            events,
            snapshot,
            next_wait: Some(backoff),
        }
    }

    /// Detect and heal an OUT-OF-BAND canonical change (issue #13 re-auth re-stash):
    /// the operator ran `claude /login` (or the active token silently refreshed in
    /// place), rewriting the canonical credential underneath the daemon. Classify
    /// the freshly-read `canonical` against the watch; on a `Changed` verdict, find
    /// the account it now belongs to and refresh that account's stash to the new
    /// token — so a later swap AWAY and back restores the re-authenticated
    /// credential, not the stale stashed one.
    ///
    /// The watch's two-step protocol (classify, then commit) makes this re-fire
    /// safe: the baseline advances only once the re-stash SUCCEEDS, so a failure
    /// (e.g. the keychain locks mid-write) leaves the change to be re-detected and
    /// retried next cycle. After a successful re-stash the cached active index is
    /// dropped so it is re-resolved against the new canonical (a `/login` may have
    /// switched to a different account).
    async fn reconcile_canonical_change(
        &mut self,
        canonical: &Credential,
        events: &mut Vec<Event>,
    ) {
        match self.state.canonical_watch.classify(canonical) {
            // First observation this run: prime the baseline, detect nothing.
            CanonicalChange::Primed => self.state.canonical_watch.commit(canonical),
            // No out-of-band write since we last looked.
            CanonicalChange::Unchanged => {}
            CanonicalChange::Changed => match self.resolve_account_for(canonical).await {
                Some(idx) => {
                    if self.restash_account(idx, canonical).await {
                        events.push(Event::ReStash {
                            account: self.roster[idx].label.clone(),
                        });
                        // Handled: advance the baseline so this write is not
                        // re-detected, and drop the cached active so it is
                        // re-resolved against the new canonical below.
                        self.state.canonical_watch.commit(canonical);
                        self.state.active = None;
                    }
                    // else: the re-stash failed (e.g. a locked keychain) — do NOT
                    // commit; leave the change to re-fire and catch up next cycle.
                }
                None => {
                    // The new canonical maps to no roster account (an un-captured
                    // login, or an identity we cannot resolve). Nothing to
                    // re-stash; commit anyway so we do not spin on it every cycle.
                    self.state.canonical_watch.commit(canonical);
                }
            },
        }
    }

    /// Identify which roster account the given `canonical` credential belongs to,
    /// using two signals in order: (1) the canonical token byte-matches an account's
    /// stash — exact right after a swap or a re-stash; (2) the displayed
    /// `~/.claude.json` `accountUuid` maps to a roster account — the signal when the
    /// token has changed in place and no stash matches it yet (a fresh `/login` or
    /// an in-place refresh). `None` if neither resolves. Shared by
    /// [`resolve_active`](Self::resolve_active) and the re-auth re-stash path (#13).
    async fn resolve_account_for(&self, canonical: &Credential) -> Option<usize> {
        for (i, account) in self.roster.iter().enumerate() {
            if let Ok(stashed) = self.stash.read(&account.stash).await {
                if stashed.credential.matches(canonical) {
                    return Some(i);
                }
            }
        }
        if let Ok(oauth) = claude_state::read_oauth_account_from(&self.claude_json) {
            return self
                .roster
                .iter()
                .position(|a| a.account_uuid == oauth.account_uuid());
        }
        None
    }

    /// Refresh account `idx`'s stash to the new `canonical` token (issue #13 re-auth
    /// re-stash), PRESERVING its `oauthAccount` identity half. The identity is taken
    /// from the existing stash if present; otherwise from `~/.claude.json` — but
    /// only when the displayed identity actually belongs to account `idx` (its
    /// `accountUuid` matches the roster entry), so a wrong identity is never stapled
    /// onto the refreshed token. Returns `false` (re-stash not performed) when no
    /// usable identity is available or the stash write fails — the caller then
    /// leaves the change to re-fire rather than committing the baseline.
    async fn restash_account(&self, idx: usize, canonical: &Credential) -> bool {
        let account = &self.roster[idx];
        // Prefer the identity already stashed for this account: it is authoritative
        // and does not depend on the best-effort display file.
        let oauth_account = if let Ok(existing) = self.stash.read(&account.stash).await {
            existing.oauth_account
        } else if let Ok(displayed) = claude_state::read_oauth_account_from(&self.claude_json) {
            // No existing stash: fall back to the displayed identity, but only if it
            // is THIS account's — never staple a different account's identity on.
            if account.account_uuid != displayed.account_uuid() {
                return false;
            }
            displayed
        } else {
            return false;
        };
        let refreshed = StashedAccount {
            credential: canonical.clone(),
            oauth_account,
        };
        self.stash.write(&account.stash, &refreshed).await.is_ok()
    }

    /// Fold account `i`'s poll `result` into its per-account health (issue #42) and
    /// push any poll-outcome event. Classifies the result into a [`PollOutcome`]:
    ///
    /// - **Unauthorized** (401): increment the consecutive-401 streak and reset any
    ///   recovery probe. While the account is still healthy, emit `monitor_401` with
    ///   the climbing count; the Nth consecutive (`monitor_401_n`) QUARANTINES it (a
    ///   dead credential) and emits [`Event::CredentialDead`] — edge-triggered, ONCE
    ///   on the transition. Once quarantined, further 401s are silent (no spam): the
    ///   dead state is a durable status, not a repeated log line.
    /// - **Live** (success): reset the streak. If the account is quarantined, this is
    ///   a recovery probe — count consecutive successes and, at `monitor_recovery_m`,
    ///   un-quarantine it and emit [`Event::CredentialRestored`] (edge-triggered,
    ///   ONCE). A quarantined account is polled only while it is active, and the
    ///   COMMON way a dead account becomes active is the operator re-logging-in (the
    ///   #13 canonical-change re-stash) — so recovery normally follows a genuine
    ///   re-login. The one exception is a dead ACTIVE account with no viable swap
    ///   target (it stays active and is re-probed): if its OWN token starts answering
    ///   again, M live polls restore it without a re-login. That is intended — a token
    ///   returning success M times in a row is a working credential, and leaving such
    ///   an account stranded in `needs re-login` would make the durable status lie.
    /// - **ScopeMissing** (403): reset the streak — a 403 token authenticates, so it
    ///   is NOT dead — and emit `usage_scope_fail` (#5). Resets any recovery probe.
    /// - **Transient** (5xx / network / 429 / other 4xx / locked / unreadable): reset
    ///   the streak silently — no liveness signal either way — and reset any recovery
    ///   probe (only a `Live` poll advances recovery). A locked keychain is
    ///   process-global and signaled once at top-of-tick (#13), never here.
    fn note_poll_outcome(&mut self, i: usize, result: &Result<Usage>, events: &mut Vec<Event>) {
        match classify_poll(result) {
            PollOutcome::Unauthorized => {
                let consecutive = self.state.health[i].consec_401.saturating_add(1);
                self.state.health[i].consec_401 = consecutive;
                // A 401 breaks any in-progress recovery probe.
                self.state.health[i].recovery_successes = 0;
                // Already dead → stay silent: the durable status carries the dead
                // state; CredentialDead already fired on the transition (no spam).
                if self.state.health[i].quarantined {
                    return;
                }
                events.push(Event::Monitor401 {
                    account: self.roster[i].label.clone(),
                    consecutive,
                });
                // The Nth consecutive non-scope 401 declares the credential DEAD.
                if consecutive >= u32::from(self.monitor_401_n) {
                    self.state.health[i].quarantined = true;
                    events.push(Event::CredentialDead {
                        account: self.roster[i].label.clone(),
                    });
                }
            }
            PollOutcome::Live => {
                self.state.health[i].consec_401 = 0;
                if self.state.health[i].quarantined {
                    let m = self.state.health[i].recovery_successes.saturating_add(1);
                    self.state.health[i].recovery_successes = m;
                    if m >= u32::from(self.monitor_recovery_m) {
                        self.state.health[i].quarantined = false;
                        self.state.health[i].recovery_successes = 0;
                        events.push(Event::CredentialRestored {
                            account: self.roster[i].label.clone(),
                        });
                    }
                }
            }
            PollOutcome::ScopeMissing => {
                self.state.health[i].consec_401 = 0;
                self.state.health[i].recovery_successes = 0;
                events.push(Event::UsageScopeFail {
                    account: self.roster[i].label.clone(),
                });
            }
            PollOutcome::Transient => {
                self.state.health[i].consec_401 = 0;
                self.state.health[i].recovery_successes = 0;
            }
        }
    }

    /// The per-roster-index enabled (in-rotation, issue #36) mask `pick_target`
    /// consumes — a disabled account is never a viable swap target. Rebuilt per call
    /// (the roster is small); shared by the normal and the #42 emergency swap path.
    fn enabled_mask(&self) -> Vec<bool> {
        self.roster.iter().map(|account| account.enabled).collect()
    }

    /// Decide what to do about the active account this cycle, performing the swap
    /// if one is warranted. Returns the per-cycle verdict.
    async fn decide_action(
        &mut self,
        at: Instant,
        active: Option<usize>,
        readings: &[Option<Usage>],
        events: &mut Vec<Event>,
    ) -> TickAction {
        // No identifiable active account → poll-only (never swap on an unknown
        // active account: it is missing data about WHO to swap away from).
        let Some(active_idx) = active else {
            return TickAction::SkippedActiveUnknown;
        };
        // The active account's credential is DEAD (quarantined, #42) — distinct from
        // a transient skip below. Two sub-cases, by whether it polled this cycle:
        if self.state.health[active_idx].quarantined {
            match readings[active_idx] {
                // Still failing (no reading) → the live session is blocked. Escape it
                // with an emergency swap, bypassing the swap-away trigger AND cooldown.
                None => return self.emergency_swap(at, active_idx, readings, events).await,
                // Polling live again → the credential is recovering (normally the
                // operator's re-login; note_poll_outcome counts toward restore). Hold:
                // never swap away mid-recovery, never emergency-swap one that now works.
                Some(_) => return TickAction::Held,
            }
        }
        // The active account's own reading is unavailable (transient / a 401 below the
        // dead threshold / unreadable) → skip; never swap on missing data.
        let Some(active_usage) = readings[active_idx] else {
            return TickAction::SkippedActiveUnavailable;
        };
        // Draw this cycle's swap-away triggers (issues #38, #41): each jittered +
        // clamped to 50..=99 percent, then to a fraction for the decision. The
        // session and weekly triggers are independent — swap when EITHER dimension
        // reaches its own; below BOTH → hold. Both are drawn every cycle (a fixed
        // strategy consumes no RNG), keeping the per-cycle draw order deterministic.
        let session_trigger =
            self.trigger_strategy
                .draw(&mut self.rng, TRIGGER_PCT_LO, TRIGGER_PCT_HI)
                / 100.0;
        let weekly_trigger = self.weekly_trigger_strategy.draw(
            &mut self.rng,
            WEEKLY_TRIGGER_PCT_LO,
            WEEKLY_TRIGGER_PCT_HI,
        ) / 100.0;
        if swap::decide(&active_usage, session_trigger, weekly_trigger) == SwapDecision::Hold {
            return TickAction::Held;
        }
        // Over the trigger. Cooldown (anti-oscillation, #10): refuse a re-swap
        // until this cycle's (jittered) cooldown has elapsed since the last swap,
        // so two near-exhausted accounts cannot ping-pong.
        let cooldown = Duration::from_secs_f64(self.cooldown_strategy.draw(
            &mut self.rng,
            COOLDOWN_SECS_LO,
            COOLDOWN_SECS_HI,
        ));
        if let Some(last) = &self.state.last_swap {
            if at.saturating_duration_since(last.at) < cooldown {
                return TickAction::SkippedCooldown;
            }
        }
        // Pick the viable target whose weekly quota resets soonest (issue #37). A
        // disabled (parked) account is not viable (issue #36), and a weekly-exhausted
        // account is not viable (#11) — so when every ENABLED other account is
        // weekly-exhausted this returns `None`. A disabled account, even with weekly
        // headroom, never counts, so it cannot hold the daemon out of the
        // all-exhausted terminal state (#11).
        let Some(target_idx) = pick_target(
            active_idx,
            readings,
            &self.enabled_mask(),
            self.session_floor,
            weekly_trigger,
        ) else {
            // No viable target — every other account is weekly-exhausted (or, with
            // the opt-in floor enabled, over it). The all-exhausted TERMINAL state
            // (issue #11): HOLD, do NOT swap (swapping among exhausted accounts only
            // thrashes), and emit ONE edge-triggered signal naming the least-bad
            // account — the one whose weekly window resets soonest, so the operator
            // knows when relief arrives. The active account is left exactly as is.
            // The signal is edge-triggered: emit only on ENTERING the state, so the
            // payload is computed once per episode, not every poll while it holds.
            if !self.state.signaled_all_exhausted {
                let (hold_idx, resets_at) = match soonest_weekly_reset(readings) {
                    Some((idx, at)) => (idx, Some(at)),
                    // No account reported a parseable weekly reset: fall back to the
                    // active account, timestamp omitted (forward-compatible).
                    None => (active_idx, None),
                };
                events.push(Event::AllExhausted {
                    hold: self.roster[hold_idx].label.clone(),
                    resets_at,
                });
                self.state.signaled_all_exhausted = true;
            }
            return TickAction::NoViableTarget;
        };
        // Run the out-of-band swap. #6 is no-half-swap: an error leaves the
        // canonical item and both stashes coherent, so we simply retry next cycle.
        let outgoing = self.roster[active_idx].stash.clone();
        let incoming = self.roster[target_idx].stash.clone();
        match swap::swap(
            &self.store,
            &self.stash,
            &outgoing,
            &incoming,
            &self.claude_json,
        )
        .await
        {
            Ok(_report) => {
                self.record_swap(target_idx, &incoming, at).await;
                // Log the swap (issue #9). `swap::decide` returns only a binary
                // verdict, so the reason is re-derived here from the active reading:
                // session-first when BOTH dimensions are over their (this-cycle)
                // triggers. `session_pct` reuses `to_pct` so the log agrees with the
                // percentage `status` shows for the same reading.
                let reason = if active_usage.session >= session_trigger {
                    SwapReason::Session
                } else {
                    SwapReason::Weekly
                };
                events.push(Event::Swap {
                    from: self.roster[active_idx].label.clone(),
                    to: self.roster[target_idx].label.clone(),
                    reason,
                    session_pct: to_pct(active_usage.session),
                });
                TickAction::Swapped {
                    from: active_idx,
                    to: target_idx,
                }
            }
            Err(_) => TickAction::SwapFailed,
        }
    }

    /// Record a completed swap to `target_idx` (its incoming stash named `incoming`):
    /// update the cached active index, the post-swap cooldown floor + `status`
    /// display (#8), and prime the canonical watch with the token just promoted, so
    /// this OWN write is not re-detected as an out-of-band change next cycle (#13).
    /// Read the token back from the incoming stash (which still holds it) rather than
    /// re-reading the canonical: if a third writer changed the canonical after our
    /// write, committing the token we INTENDED leaves that change to be detected and
    /// re-stashed next cycle, instead of silently adopting the intruder. Shared by
    /// the normal swap and the emergency swap (#42).
    async fn record_swap(&mut self, target_idx: usize, incoming: &str, at: Instant) {
        self.state.active = Some(target_idx);
        self.state.last_swap = Some(LastSwap {
            to: self.roster[target_idx].label.clone(),
            at,
        });
        if let Ok(incoming_stashed) = self.stash.read(incoming).await {
            self.state
                .canonical_watch
                .commit(&incoming_stashed.credential);
        }
    }

    /// Emergency-swap away from a confirmed-DEAD active account (issue #42): the live
    /// session is blocked, so rotate to the soonest-reset viable target IMMEDIATELY —
    /// bypassing the swap-away trigger and the post-swap cooldown that gate a normal
    /// swap. Thrash-safe by construction: it fires ONLY on a quarantined active
    /// account, and a quarantined account is never itself a viable target (it is
    /// skipped in polling, so its reading is absent), so there is no ping-pong.
    /// `pick_target` (the #37 soonest-reset rule) still excludes disabled and
    /// weekly-exhausted accounts; with no viable target the daemon holds on the dead
    /// active ([`TickAction::ActiveDeadNoTarget`]) — the `CredentialDead` signal
    /// already fired, so this stuck state is silent (no repeat-spam).
    async fn emergency_swap(
        &mut self,
        at: Instant,
        active_idx: usize,
        readings: &[Option<Usage>],
        events: &mut Vec<Event>,
    ) -> TickAction {
        // The weekly-exhaustion viability filter for `pick_target` — drawn like the
        // normal path (a fixed strategy consumes no RNG). The session swap-away
        // trigger and the cooldown are deliberately NOT consulted: an emergency swap
        // bypasses both (the active credential is dead, not merely over a trigger).
        let weekly_trigger = self.weekly_trigger_strategy.draw(
            &mut self.rng,
            WEEKLY_TRIGGER_PCT_LO,
            WEEKLY_TRIGGER_PCT_HI,
        ) / 100.0;
        let Some(target_idx) = pick_target(
            active_idx,
            readings,
            &self.enabled_mask(),
            self.session_floor,
            weekly_trigger,
        ) else {
            return TickAction::ActiveDeadNoTarget;
        };
        // #6 is no-half-swap: an error leaves the canonical item and both stashes
        // coherent — the dead active stays quarantined and the emergency swap retries
        // next cycle.
        let outgoing = self.roster[active_idx].stash.clone();
        let incoming = self.roster[target_idx].stash.clone();
        match swap::swap(
            &self.store,
            &self.stash,
            &outgoing,
            &incoming,
            &self.claude_json,
        )
        .await
        {
            Ok(_report) => {
                self.record_swap(target_idx, &incoming, at).await;
                events.push(Event::EmergencySwap {
                    from: self.roster[active_idx].label.clone(),
                    to: self.roster[target_idx].label.clone(),
                });
                TickAction::EmergencySwapped {
                    from: active_idx,
                    to: target_idx,
                }
            }
            Err(_) => TickAction::SwapFailed,
        }
    }

    /// Build the non-secret per-account snapshot for the event log and the socket.
    /// `at` (this cycle's instant) is the base for the `last_swap` relative age.
    fn snapshot(
        &self,
        at: Instant,
        active: Option<usize>,
        readings: &[Option<Usage>],
    ) -> StatusSnapshot {
        StatusSnapshot {
            accounts: self
                .roster
                .iter()
                .enumerate()
                .map(|(i, account)| AccountReading {
                    label: account.label.clone(),
                    active: active == Some(i),
                    enabled: account.enabled,
                    quarantined: self.state.health[i].quarantined,
                    usage: readings[i],
                })
                .collect(),
            // Project the monotonic last-swap record to a relative age as of this
            // cycle (issue #8); sourced from a label only, so no token/email can
            // reach it (issue #15).
            last_swap: self.state.last_swap.as_ref().map(|swap| LastSwapLine {
                to: swap.to.clone(),
                secs_ago: at.saturating_duration_since(swap.at).as_secs(),
            }),
        }
    }

    /// Draw this cycle's poll interval from the poll strategy (issue #38),
    /// clamped to the valid `5..=3600` s range. The fixed (no-jitter) case
    /// returns the base verbatim; deterministic under a seeded RNG.
    pub(crate) fn next_poll_interval(&mut self) -> Duration {
        Duration::from_secs_f64(
            self.poll_strategy
                .draw(&mut self.rng, POLL_SECS_LO, POLL_SECS_HI),
        )
    }

    /// Sleep until the next poll is due — a freshly drawn, jittered interval
    /// (issue #38) handed to the [`Clock`] seam.
    pub(crate) async fn wait_for_next_poll(&mut self) {
        let interval = self.next_poll_interval();
        self.clock.tick(interval).await;
    }

    /// Sleep until the next tick is due. `next_wait` is the just-finished tick's
    /// requested wait: `None` → the normal jittered poll interval (issue #38);
    /// `Some(d)` → an explicit duration, the locked-keychain back-off (issue #13).
    /// Behind the [`Clock`] seam, so tests drive both paths deterministically.
    pub(crate) async fn wait_after_tick(&mut self, next_wait: Option<Duration>) {
        match next_wait {
            Some(backoff) => self.clock.tick(backoff).await,
            None => self.wait_for_next_poll().await,
        }
    }
}

/// The health-relevant classification of ONE account's poll this tick — the typed
/// poll outcome (issue #42) the per-account health state machine consumes. Derived
/// from the poll `Result` by [`classify_poll`]; distinct from the raw HTTP taxonomy
/// (`usage`'s status classes) in that it folds every non-liveness-bearing error into
/// one `Transient` class and separates the two liveness signals — `Live` (the
/// credential works) from `Unauthorized` (the token was rejected). "Dead" and
/// "exhausted" are not single-poll outcomes: death is the ACCUMULATION of
/// `Unauthorized` across ticks (the per-account 401 streak reaching `monitor_401_n`),
/// and exhaustion is derived from a `Live` reading's usage against the swap triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollOutcome {
    /// A successful usage reading — the credential is alive. Resets the death streak;
    /// for a quarantined account, advances the recovery probe.
    Live,
    /// HTTP 401 — the stored token was rejected. Advances the consecutive-401 death
    /// streak; the Nth (`monitor_401_n`) quarantines the account.
    Unauthorized,
    /// HTTP 403 — the token authenticated but lacks the usage scope (a non-interactive
    /// setup token). NON-dead (it authenticated), surfaced distinctly (#5).
    ScopeMissing,
    /// Any other failure (5xx / network / 429 / other 4xx / keychain-locked /
    /// unreadable token / unparseable body): no liveness signal — neither advances
    /// nor, by itself, distinguishes death. Resets the death streak (a 401 streak
    /// must be unbroken).
    Transient,
}

/// Classify a poll `Result` into its [`PollOutcome`] — the typed poll outcome the
/// dead-credential health state machine consumes (issue #42). Pure: the single place
/// the HTTP error taxonomy is mapped onto the liveness/death axis, so the policy is
/// testable in isolation and `note_poll_outcome` stays a state-transition.
fn classify_poll(result: &Result<Usage>) -> PollOutcome {
    match result {
        Ok(_) => PollOutcome::Live,
        Err(Error::UsageUnauthorized) => PollOutcome::Unauthorized,
        Err(Error::UsageScopeMissing) => PollOutcome::ScopeMissing,
        Err(_) => PollOutcome::Transient,
    }
}

/// Pick the viable swap target whose weekly window resets SOONEST (issue #37):
/// among accounts other than `active` that are enabled (issue #36), whose reading
/// is available, that are NOT weekly-exhausted (weekly usage below `weekly_trigger`,
/// issue #11) — and, when the opt-in `floor` is `Some`, whose session usage is
/// below it (#10) — the one with the earliest weekly `resets_at`. An account with a
/// known reset is preferred over one without (an unknown reset sorts last); an
/// exact tie — or an all-unknown field — keeps the earliest roster index. `None`
/// when none qualifies: with every enabled other account weekly-exhausted that is
/// the all-exhausted terminal state (#11). `enabled` is indexed by roster position,
/// parallel to `readings`.
///
/// Soonest-reset (issue #37) SUPERSEDES the former most-weekly-headroom rule.
/// Swapping TO the account whose quota refills first burns an allowance that is
/// about to reset anyway and preserves the longer-runway account, raising total
/// roster utilization. It also UNIFIES normal selection with the #11 terminal hold,
/// which already holds on the soonest-`resets_at` account
/// ([`soonest_weekly_reset`]) — so, when resets are known, the daemon prefers the
/// same least-time-to-relief account whether or not a viable target exists. The two
/// differ deliberately only on the degenerate `None` case: this fn keeps an
/// unknown-reset account as a last-resort eligible target (selection must pick
/// SOMETHING viable), whereas [`soonest_weekly_reset`] excludes `None` outright (the
/// hold then omits a timestamp). The viability FILTER is unchanged; only the choice
/// AMONG viable accounts changed.
///
/// Two exclusions are load-bearing. The weekly-exhaustion exclusion: a target
/// at/above its weekly trigger would re-trip [`swap::decide`] next cycle and
/// thrash, so it can never be a useful destination — excluding it is what turns
/// "all enabled accounts weekly-exhausted" into a no-viable-target verdict instead
/// of a swap. The disabled exclusion (#36): a parked account is never a destination
/// even with ample headroom, and — being excluded here rather than relying on its
/// (skipped) poll — it can never hold the daemon out of the #11 terminal state.
fn pick_target(
    active: usize,
    readings: &[Option<Usage>],
    enabled: &[bool],
    floor: Option<f64>,
    weekly_trigger: f64,
) -> Option<usize> {
    readings
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != active)
        .filter(|&(i, _)| enabled[i])
        .filter_map(|(i, reading)| reading.map(|usage| (i, usage)))
        .filter(|&(_, usage)| usage.weekly < weekly_trigger)
        .filter(|&(_, usage)| floor.is_none_or(|f| usage.session < f))
        // Soonest weekly reset (issue #37). The key sorts a known reset ahead of an
        // unknown one (`false` < `true`), then by the reset epoch ascending;
        // `min_by_key` keeps the first of equal keys, so an exact tie — or an
        // all-unknown field — falls to the earliest roster index, matching
        // [`soonest_weekly_reset`]'s tie-break (#11).
        .min_by_key(|&(_, usage)| match usage.weekly_resets_at {
            Some(resets_at) => (false, resets_at),
            None => (true, i64::MAX),
        })
        .map(|(i, _)| i)
}

/// The roster index (and its epoch) of the account whose WEEKLY window resets
/// soonest, among readings that reported a parseable reset (issue #11). The
/// all-exhausted terminal state holds on this least-bad account. Accounts without
/// a known reset are skipped; an exact tie keeps the earliest roster index. `None`
/// when no account reported a reset, leaving the caller to fall back.
fn soonest_weekly_reset(readings: &[Option<Usage>]) -> Option<(usize, i64)> {
    let mut soonest: Option<(usize, i64)> = None;
    for (i, reading) in readings.iter().enumerate() {
        if let Some(at) = reading.as_ref().and_then(|usage| usage.weekly_resets_at) {
            if soonest.is_none_or(|(_, best)| at < best) {
                soonest = Some((i, at));
            }
        }
    }
    soonest
}

/// The console line for a swap this cycle, or `None` for any non-swap outcome.
/// Surfaced to the operator watching the foreground `run` (issue #8) — the file
/// event log records every cycle separately. Both swap kinds echo: a normal swap
/// and the #42 emergency swap away from a dead active credential (the latter named
/// distinctly, since it means a credential just died and the daemon force-rotated).
/// Sourced solely from labels, so it can never carry a token or email (issue #15).
fn swap_report(outcome: &TickOutcome) -> Option<String> {
    match outcome.action {
        TickAction::Swapped { from, to } => Some(format!(
            "swapped: {} → {}",
            label_at(&outcome.snapshot, from),
            label_at(&outcome.snapshot, to),
        )),
        TickAction::EmergencySwapped { from, to } => Some(format!(
            "emergency swap (dead credential): {} → {}",
            label_at(&outcome.snapshot, from),
            label_at(&outcome.snapshot, to),
        )),
        _ => None,
    }
}

/// The label of the roster account at `index` in `snapshot`, or `"?"` if out of
/// range. A swap's indices are always valid, but the long-running daemon must
/// never panic on a display path, so this stays total.
fn label_at(snapshot: &StatusSnapshot, index: usize) -> &str {
    snapshot
        .accounts
        .get(index)
        .map_or("?", |account| account.label.as_str())
}

/// Drive the poll loop until shutdown.
///
/// Reconcile-on-start, then forever: tick, log, and idle until the next poll —
/// meanwhile serving control requests and watching for shutdown. Shutdown is
/// observed only HERE (between ticks), never mid-tick: a swap inside [`Daemon::tick`]
/// always runs to completion, so a shutdown can never tear a swap
/// (complete-or-abort; #6 is no-half-swap).
pub(crate) async fn run_loop<P, C, S, K, Sh, Ctl>(
    daemon: &mut Daemon<P, C, S, K>,
    log: &mut EventLog,
    shutdown: &mut Sh,
    control: &Ctl,
) -> Result<()>
where
    P: RosterPoller,
    C: CredentialStore,
    S: AccountStash,
    K: Clock,
    Sh: Shutdown,
    Ctl: Control,
{
    // Reconcile-on-start is best-effort: a failure is logged and the loop still
    // starts — the next swap re-establishes consistency anyway.
    if let Err(err) = daemon.reconcile_on_start().await {
        eprintln!("sessiometer: reconcile-on-start skipped: {err}");
    }

    loop {
        let outcome = daemon.tick().await;
        // Best-effort logging (issue #9): emit each event the tick produced. A
        // write failure must not kill the daemon, and one failed event must not
        // drop the rest of the tick's events — so log and continue, never return.
        for event in &outcome.events {
            if let Err(err) = log.emit(event) {
                eprintln!("sessiometer: event log write failed: {err}");
            }
        }
        // Echo a swap to the operator watching the foreground process (issue #8).
        // The file event log (above) records every cycle; the console gets just
        // swaps, sourced solely from labels (issue #15).
        if let Some(report) = swap_report(&outcome) {
            eprintln!("sessiometer: {report}");
        }
        // The wait this tick requested — a locked-keychain back-off overrides the
        // normal interval (issue #13); captured before the snapshot is moved.
        let next_wait = outcome.next_wait;
        // The snapshot the control socket answers from until the next poll.
        let snapshot = outcome.snapshot;

        // Idle until the next tick is due, serving control requests and watching
        // for shutdown. A swap (if any) already completed inside `tick`, so a
        // shutdown observed here aborts cleanly before the next tick — no half-swap.
        let wait = daemon.wait_after_tick(next_wait);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                biased;
                _ = shutdown.requested() => return Ok(()),
                _ = control.serve(&snapshot) => continue,
                _ = &mut wait => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_state::OauthAccount;
    use crate::config::Tunables;
    use crate::keychain::FakeCredentialStore;
    use crate::stash::{FakeAccountStash, StashedAccount};
    use crate::timing::Jitter;
    use std::cell::Cell;
    use std::collections::HashMap;

    // --- Fakes -------------------------------------------------------------

    /// A clock whose `now` starts at construction and advances by `step` on each
    /// `tick` — so a loop's cadence is deterministic and runs in zero real time.
    /// `frozen` makes `tick` a no-op (constant `now`).
    struct FakeClock {
        now: Cell<Instant>,
        step: Duration,
    }

    impl FakeClock {
        fn new(step: Duration) -> Self {
            Self {
                now: Cell::new(Instant::now()),
                step,
            }
        }
        fn frozen() -> Self {
            Self::new(Duration::ZERO)
        }
        fn advance(&self, by: Duration) {
            self.now.set(self.now.get() + by);
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.now.get()
        }
        // Advances by its own `step`, independent of the daemon's drawn interval,
        // so the existing run-loop/cooldown tests keep their deterministic
        // cadence. The poll-interval jitter (issue #38) is covered directly via
        // `Daemon::next_poll_interval`.
        async fn tick(&self, _interval: Duration) {
            self.now.set(self.now.get() + self.step);
        }
    }

    /// A scripted per-account poll outcome. `Ok` yields a reading; each error
    /// variant drives one of [`Daemon::note_poll_outcome`]'s issue-#9 arms, so the
    /// 401 / keychain-lock / 403 event paths and the 401 streak become testable.
    #[derive(Clone, Copy)]
    enum Scripted {
        Ok(Usage),
        Transient,
        Unauthorized,
        Locked,
        ScopeMissing,
    }

    /// Scripts each account's poll outcome keyed by `account_uuid`: `ok` yields a
    /// reading, the error builders inject the issue-#9 conditions, and an
    /// unscripted account returns a transient error (unavailable).
    struct FakeRosterPoller {
        readings: HashMap<String, Scripted>,
    }

    impl FakeRosterPoller {
        fn new() -> Self {
            Self {
                readings: HashMap::new(),
            }
        }
        fn ok(mut self, uuid: &str, session: f64, weekly: f64) -> Self {
            self.readings.insert(
                uuid.to_owned(),
                Scripted::Ok(Usage {
                    session,
                    weekly,
                    weekly_resets_at: None,
                }),
            );
            self
        }
        /// Like [`ok`](Self::ok) but with a known weekly `resets_at` (epoch
        /// seconds) — the all-exhausted tests (#11) script which account resets
        /// soonest through this.
        fn ok_resets(
            mut self,
            uuid: &str,
            session: f64,
            weekly: f64,
            weekly_resets_at: i64,
        ) -> Self {
            self.readings.insert(
                uuid.to_owned(),
                Scripted::Ok(Usage {
                    session,
                    weekly,
                    weekly_resets_at: Some(weekly_resets_at),
                }),
            );
            self
        }
        fn failing(mut self, uuid: &str) -> Self {
            self.readings.insert(uuid.to_owned(), Scripted::Transient);
            self
        }
        fn unauthorized(mut self, uuid: &str) -> Self {
            self.readings
                .insert(uuid.to_owned(), Scripted::Unauthorized);
            self
        }
        fn keychain_locked(mut self, uuid: &str) -> Self {
            self.readings.insert(uuid.to_owned(), Scripted::Locked);
            self
        }
        fn scope_missing(mut self, uuid: &str) -> Self {
            self.readings
                .insert(uuid.to_owned(), Scripted::ScopeMissing);
            self
        }
    }

    impl RosterPoller for FakeRosterPoller {
        async fn poll(&self, account: &Account, _active: bool) -> Result<Usage> {
            match self.readings.get(&account.account_uuid) {
                Some(Scripted::Ok(usage)) => Ok(*usage),
                Some(Scripted::Unauthorized) => Err(Error::UsageUnauthorized),
                Some(Scripted::Locked) => Err(Error::KeychainLocked { op: "read" }),
                Some(Scripted::ScopeMissing) => Err(Error::UsageScopeMissing),
                // Explicit `Transient` and any unscripted account both land here.
                _ => Err(Error::UsageTransient { status: 0 }),
            }
        }
    }

    /// Resolves on its `stop_at`-th `requested()` call (the run loop calls it once
    /// per tick), so the loop stops after exactly `stop_at` ticks.
    struct FakeShutdown {
        calls: Cell<u32>,
        stop_at: u32,
    }

    impl FakeShutdown {
        fn after(stop_at: u32) -> Self {
            Self {
                calls: Cell::new(0),
                stop_at,
            }
        }
    }

    impl Shutdown for FakeShutdown {
        async fn requested(&mut self) {
            let n = self.calls.get() + 1;
            self.calls.set(n);
            if n >= self.stop_at {
                return;
            }
            std::future::pending::<()>().await;
        }
    }

    /// A control seam that never serves (its future never resolves), so it never
    /// wins the run loop's idle select.
    struct NoControl;

    impl Control for NoControl {
        async fn serve(&self, _snapshot: &StatusSnapshot) {
            std::future::pending::<()>().await;
        }
    }

    // --- builders ----------------------------------------------------------

    fn account(uuid: &str, stash: &str, label: &str) -> Account {
        Account {
            account_uuid: uuid.to_owned(),
            stash: stash.to_owned(),
            label: label.to_owned(),
            enabled: true,
        }
    }

    /// A roster account that starts parked (issue #36) — for the disable paths.
    fn disabled_account(uuid: &str, stash: &str, label: &str) -> Account {
        Account {
            enabled: false,
            ..account(uuid, stash, label)
        }
    }

    fn tunables(trigger: u8, floor: u8, cooldown: u64) -> Tunables {
        // Weekly trigger fixed high (98) so the existing tests' weekly readings
        // (all well below it) never trip the new weekly path (issue #41): these
        // tests pin the SESSION trigger. A fixed strategy draws no RNG, so the
        // per-cycle draw sequence — and every seeded-jitter test — is unchanged.
        const WEEKLY_TRIGGER: u8 = 98;
        Tunables {
            poll_secs: 60,
            cooldown_secs: cooldown,
            // Most daemon tests opt the floor IN (the pre-#10 behavior they were
            // written against); `tunables_floor_off` covers the new default.
            session_floor: Some(floor),
            session_trigger: trigger,
            weekly_trigger: WEEKLY_TRIGGER,
            monitor_401_n: 3,
            monitor_recovery_m: 2,
            // Existing daemon tests exercise the fixed (no-jitter) path: each
            // strategy draws its base verbatim, identical to the pre-#38 scalars.
            poll_strategy: Strategy::fixed(60.0),
            trigger_strategy: Strategy::fixed(f64::from(trigger)),
            weekly_trigger_strategy: Strategy::fixed(f64::from(WEEKLY_TRIGGER)),
            cooldown_strategy: Strategy::fixed(cooldown as f64),
        }
    }

    /// Tunables with the session-floor guard OFF — the #10 default. The floor is
    /// the only field that differs from [`tunables`], so the rest is reused.
    fn tunables_floor_off(trigger: u8, cooldown: u64) -> Tunables {
        Tunables {
            session_floor: None,
            ..tunables(trigger, 0, cooldown)
        }
    }

    fn cred(blob: &[u8]) -> Credential {
        Credential::new(blob.to_vec())
    }

    fn oauth(uuid: &str) -> OauthAccount {
        OauthAccount::from_object_bytes(
            format!(r#"{{"accountUuid":"{uuid}","emailAddress":"{uuid}@example.com"}}"#).as_bytes(),
        )
        .unwrap()
    }

    fn stashed(token: &[u8], uuid: &str) -> StashedAccount {
        StashedAccount {
            credential: cred(token),
            oauth_account: oauth(uuid),
        }
    }

    /// A temp `~/.claude.json` displaying `uuid`. Returns the tempdir guard + path.
    fn claude_json(uuid: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"numStartups":1,"oauthAccount":{{"accountUuid":"{uuid}","emailAddress":"{uuid}@x.com"}}}}"#
            ),
        )
        .unwrap();
        (dir, path)
    }

    fn displayed_uuid(path: &Path) -> Option<String> {
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        value["oauthAccount"]["accountUuid"]
            .as_str()
            .map(str::to_owned)
    }

    async fn store_holding(blob: &[u8]) -> FakeCredentialStore {
        let store = FakeCredentialStore::empty();
        store.write(&cred(blob)).await.unwrap();
        store
    }

    async fn stash_with(entries: &[(&str, &[u8], &str)]) -> FakeAccountStash {
        let stash = FakeAccountStash::empty();
        for (service, token, uuid) in entries {
            stash.write(service, &stashed(token, uuid)).await.unwrap();
        }
        stash
    }

    type FakeDaemon = Daemon<FakeRosterPoller, FakeCredentialStore, FakeAccountStash, FakeClock>;

    // --- pick_target (pure) ------------------------------------------------

    // A weekly trigger well above every reading in the pick_target tests below, so
    // the weekly-exhaustion exclusion (#11) is a no-op for the ones that pin the
    // floor / selection behavior; the #11 tests use readings at/above it.
    const WK: f64 = 0.98;

    /// An all-enabled flag slice sized to `readings` (issue #36): the pre-#36
    /// pick_target tests pin the floor / selection / weekly-exhaustion behavior with
    /// every account enabled, so the new disabled exclusion is a no-op for them.
    fn all_on(readings: &[Option<Usage>]) -> Vec<bool> {
        vec![true; readings.len()]
    }

    #[test]
    fn pick_target_chooses_the_soonest_reset_among_viable_accounts() {
        // #37: among viable accounts the one whose weekly window resets SOONEST wins,
        // even when it does NOT have the most weekly headroom (the superseded rule).
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: Some(100), // soonest overall — but it is active
            }), // index 0 = active (excluded)
            Some(Usage {
                session: 0.50,
                weekly: 0.60,                // less headroom than index 2…
                weekly_resets_at: Some(200), // …but resets soonest among viable -> winner
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.20,                // most headroom — would win the OLD rule…
                weekly_resets_at: Some(500), // …but resets latest
            }),
            Some(Usage {
                session: 0.85,
                weekly: 0.01,
                weekly_resets_at: Some(50), // earliest of all — but session over floor
            }), // session over floor -> not viable
        ];
        // Index 1 (reset 200) beats the most-headroom index 2 (reset 500); index 0 is
        // active and index 3 fails the floor, so neither earlier reset is eligible.
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), Some(0.80), WK),
            Some(1)
        );
    }

    #[test]
    fn pick_target_excludes_the_active_account_and_unavailable_readings() {
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: None,
            }),
            None, // unavailable
            Some(Usage {
                session: 0.10,
                weekly: 0.30,
                weekly_resets_at: None,
            }),
        ];
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), Some(0.80), WK),
            Some(2)
        );
    }

    #[test]
    fn pick_target_is_none_when_every_candidate_is_over_the_floor() {
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: None,
            }),
            Some(Usage {
                session: 0.90,
                weekly: 0.10,
                weekly_resets_at: None,
            }),
            Some(Usage {
                session: 0.81,
                weekly: 0.10,
                weekly_resets_at: None,
            }),
        ];
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), Some(0.80), WK),
            None
        );
    }

    #[test]
    fn pick_target_breaks_a_reset_tie_by_roster_order() {
        // #37: when two viable accounts share the same weekly reset, the earlier
        // roster index wins — matching soonest_weekly_reset's tie-break (#11). The
        // superseded rule would have picked index 2 here on its lower session.
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: Some(100),
            }), // active (excluded)
            Some(Usage {
                session: 0.40,
                weekly: 0.20,
                weekly_resets_at: Some(300), // tie -> first of the tie wins
            }),
            Some(Usage {
                session: 0.20,
                weekly: 0.20,
                weekly_resets_at: Some(300), // tie, lower session (the OLD winner)
            }),
        ];
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), Some(0.80), WK),
            Some(1)
        );
    }

    #[test]
    fn pick_target_prefers_a_known_reset_over_an_unknown_one() {
        // #37: an account with a known reset is preferred over one whose reset is
        // unknown (None sorts last) — even when the unknown-reset account has an
        // earlier roster index and more weekly headroom.
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: None,
            }), // active (excluded)
            Some(Usage {
                session: 0.10,
                weekly: 0.05,           // more headroom + earlier index…
                weekly_resets_at: None, // …but no known reset -> sorts last
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.40,
                weekly_resets_at: Some(900), // a known reset -> preferred
            }),
        ];
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), None, WK),
            Some(2)
        );
    }

    #[test]
    fn pick_target_falls_back_to_roster_order_when_no_reset_is_known() {
        // #37: with no viable account reporting a weekly reset, selection falls back
        // to the earliest roster index (the all-unknown tie) — NOT to weekly headroom
        // (the superseded rule, which would have picked the lower-weekly index 2).
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: None,
            }), // active (excluded)
            Some(Usage {
                session: 0.10,
                weekly: 0.30, // more weekly used, earlier index -> winner
                weekly_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.05, // most headroom, but no reset and a later index
                weekly_resets_at: None,
            }),
        ];
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), None, WK),
            Some(1)
        );
    }

    #[test]
    fn pick_target_with_no_floor_admits_any_available_other() {
        // #10: with the session floor OFF (None), an account is a viable target on
        // its reset alone — even one whose session usage is high (which an enabled
        // floor would exclude).
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: Some(100),
            }), // index 0 = active (excluded)
            Some(Usage {
                session: 0.95, // high session — an enabled floor would exclude this…
                weekly: 0.10,
                weekly_resets_at: Some(200), // …but with no floor it is the soonest-reset viable target
            }),
            Some(Usage {
                session: 0.05,
                weekly: 0.60,
                weekly_resets_at: Some(300),
            }), // low session but resets later
        ];
        // No floor → index 1 wins as the soonest-reset viable target despite its high session…
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), None, WK),
            Some(1)
        );
        // …whereas an enabled 80% floor excludes index 1 and falls to index 2.
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), Some(0.80), WK),
            Some(2)
        );
    }

    #[test]
    fn pick_target_excludes_weekly_exhausted_accounts() {
        // #11: an account at/above the weekly trigger is not a viable target, even
        // with the session floor OFF and ample session headroom — swapping there
        // would only re-trigger and thrash.
        let readings = vec![
            Some(Usage {
                session: 0.50,
                weekly: 0.99,
                weekly_resets_at: None,
            }), // active (excluded)
            Some(Usage {
                session: 0.10,
                weekly: 0.99, // weekly-exhausted -> not viable despite low session
                weekly_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.20, // the only non-exhausted other account
                weekly_resets_at: None,
            }),
        ];
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), None, WK),
            Some(2)
        );
    }

    #[test]
    fn pick_target_is_none_when_every_other_account_is_weekly_exhausted() {
        // #11 core: with the floor off, the ONLY thing that makes all others
        // non-viable is weekly exhaustion — at/above the trigger (inclusive).
        let readings = vec![
            Some(Usage {
                session: 0.50,
                weekly: 0.99,
                weekly_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.98, // exactly at the trigger -> exhausted (>=)
                weekly_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 1.00,
                weekly_resets_at: None,
            }),
        ];
        assert_eq!(
            pick_target(0, &readings, &all_on(&readings), None, WK),
            None
        );
    }

    #[test]
    fn pick_target_excludes_a_disabled_account_even_when_it_resets_soonest() {
        // #36 × #37: index 1 resets soonest (it would win the new rule) but is
        // disabled, so it is never a target; selection falls to the enabled index 2.
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
                weekly_resets_at: Some(500),
            }), // active (excluded)
            Some(Usage {
                session: 0.10,
                weekly: 0.05,
                weekly_resets_at: Some(100), // soonest reset — the would-be winner…
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.30,
                weekly_resets_at: Some(200),
            }),
        ];
        let enabled = [true, false, true]; // …but index 1 is parked
        assert_eq!(pick_target(0, &readings, &enabled, None, WK), Some(2));
    }

    #[test]
    fn pick_target_a_disabled_account_does_not_rescue_an_all_exhausted_roster() {
        // #11 × #36: the only account with weekly headroom is disabled, so the
        // verdict is still no-viable-target — a parked account must not hold the
        // daemon out of the all-exhausted terminal state, however soon it resets.
        let readings = vec![
            Some(Usage {
                session: 0.50,
                weekly: 0.99,
                weekly_resets_at: None,
            }), // active (excluded)
            Some(Usage {
                session: 0.10,
                weekly: 0.98, // enabled but weekly-exhausted
                weekly_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.01, // ample headroom + soonest reset — but disabled, so not viable
                weekly_resets_at: Some(100),
            }),
        ];
        let enabled = [true, true, false];
        assert_eq!(pick_target(0, &readings, &enabled, None, WK), None);
    }

    // --- soonest_weekly_reset (pure, #11) ---------------------------------

    #[test]
    fn soonest_weekly_reset_picks_the_earliest_known_timestamp() {
        let readings = vec![
            Some(Usage {
                session: 0.0,
                weekly: 0.0,
                weekly_resets_at: Some(300),
            }),
            Some(Usage {
                session: 0.0,
                weekly: 0.0,
                weekly_resets_at: Some(100), // soonest
            }),
            Some(Usage {
                session: 0.0,
                weekly: 0.0,
                weekly_resets_at: Some(200),
            }),
            None,
        ];
        assert_eq!(soonest_weekly_reset(&readings), Some((1, 100)));
    }

    #[test]
    fn soonest_weekly_reset_ignores_unknowns_and_breaks_ties_to_first() {
        // Accounts without a known reset are skipped; an exact tie keeps the
        // earliest roster index.
        let tie = vec![
            Some(Usage {
                session: 0.0,
                weekly: 0.0,
                weekly_resets_at: None,
            }),
            Some(Usage {
                session: 0.0,
                weekly: 0.0,
                weekly_resets_at: Some(500), // first of the tie -> winner
            }),
            Some(Usage {
                session: 0.0,
                weekly: 0.0,
                weekly_resets_at: Some(500),
            }),
        ];
        assert_eq!(soonest_weekly_reset(&tie), Some((1, 500)));
        // All-unknown → None (the caller falls back to the active account).
        let none = vec![
            Some(Usage {
                session: 0.0,
                weekly: 0.0,
                weekly_resets_at: None,
            }),
            None,
        ];
        assert_eq!(soonest_weekly_reset(&none), None);
    }

    // --- tick: decision + swap --------------------------------------------

    #[tokio::test]
    async fn tick_swaps_active_over_trigger_to_the_soonest_reset_target() {
        // #37 end-to-end: the active account is over its trigger; among the two viable
        // targets the daemon picks the one that resets SOONEST — even though the other
        // has more weekly headroom (the superseded rule would have picked it).
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
            account("u-C", "Sessiometer/u-C", "third"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
            ("Sessiometer/u-C", b"C-token", "u-C"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        const B_RESET: i64 = 1_782_864_000; // 2026-07-01 — later
        const C_RESET: i64 = 1_782_496_800; // 2026-06-26 — soonest
        let poller = FakeRosterPoller::new()
            .ok_resets("u-A", 0.97, 0.40, 1_782_777_600) // active: over trigger
            .ok_resets("u-B", 0.10, 0.20, B_RESET) // viable, most headroom but resets later
            .ok_resets("u-C", 0.30, 0.50, C_RESET); // viable, resets soonest -> winner
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );
        let outcome = daemon.tick().await;

        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 2 });
        // The canonical item now holds C's token, and the display shows C…
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"C-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-C"));
        // …and the in-memory active advanced to C, so the next read polls C.
        assert_eq!(daemon.state.active, Some(2));
    }

    #[tokio::test]
    async fn tick_excludes_a_disabled_account_from_polling_and_targeting() {
        // #36 end-to-end: the active account is over its trigger; the parked account
        // (index 1) would be an obvious target but is disabled, so the swap goes to
        // the enabled `spare` (index 2) instead — and the parked account is never
        // polled, so its snapshot reading stays absent despite a scripted `ok`.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            disabled_account("u-B", "Sessiometer/u-B", "parked"),
            account("u-C", "Sessiometer/u-C", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
            ("Sessiometer/u-C", b"C-token", "u-C"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40) // active: over trigger
            .ok("u-B", 0.01, 0.01) // parked: would be an obvious target IF polled
            .ok("u-C", 0.30, 0.50); // enabled, viable
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );
        let outcome = daemon.tick().await;

        // Swapped to the ENABLED spare, not the parked account.
        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 2 });
        assert_eq!(daemon.state.active, Some(2));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-C"));
        // The parked account was skipped by the poll loop: its reading is absent
        // even though the poller was scripted to return one for it.
        let parked = &outcome.snapshot.accounts[1];
        assert_eq!(parked.label, "parked");
        assert!(!parked.enabled, "the snapshot marks it disabled");
        assert!(
            parked.usage.is_none(),
            "a disabled account is not polled, so its reading stays absent"
        );
    }

    #[tokio::test]
    async fn tick_holds_when_active_is_below_the_trigger() {
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.30)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        let outcome = daemon.tick().await;

        assert_eq!(outcome.action, TickAction::Held);
        // No swap has happened, so `status` would show `last swap: none`.
        assert!(outcome.snapshot.last_swap.is_none());
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn tick_swaps_when_weekly_reaches_its_trigger_while_session_is_below() {
        // AC #2 (the new dimension, issue #41): the active account's SESSION usage
        // is comfortably below its trigger, but its WEEKLY usage has reached the
        // separate weekly trigger → swap to the (only) viable target.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // A: session 0.50 (below the 95 session trigger) but weekly 0.98 (at the
        // helper's 98 weekly trigger) → must swap. B is open and session-viable.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.98)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );
        let outcome = daemon.tick().await;

        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert_eq!(daemon.state.active, Some(1));
    }

    #[tokio::test]
    async fn tick_holds_when_weekly_is_below_its_own_trigger_even_above_the_session_trigger() {
        // Issue #41: weekly is gated by its OWN (higher) trigger, not the session
        // one. Weekly 0.96 sits ABOVE the 0.95 session trigger yet BELOW the 0.98
        // weekly trigger, and session itself (0.50) is below its trigger — so the
        // cycle HOLDS. (Under a single-threshold rule keyed on session_trigger this
        // same reading would have swapped; the separate weekly trigger is exactly
        // what changes that.)
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.96)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0); // session trigger 95, weekly trigger 98

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        let outcome = daemon.tick().await;

        assert_eq!(outcome.action, TickAction::Held);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn tick_skips_without_swapping_when_the_active_poll_fails() {
        // Active A's poll fails (transient); B is wide open. Must NOT swap on
        // missing active data.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new().failing("u-A").ok("u-B", 0.05, 0.05);
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        let outcome = daemon.tick().await;

        assert_eq!(outcome.action, TickAction::SkippedActiveUnavailable);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn tick_skips_when_the_active_account_cannot_be_identified() {
        // Canonical token matches no stash, and ~/.claude.json shows an account
        // not in the roster → active unknown → poll-only, no swap.
        let roster = vec![account("u-A", "Sessiometer/u-A", "work")];
        let store = store_holding(b"unknown-token").await;
        let stash = stash_with(&[("Sessiometer/u-A", b"A-token", "u-A")]).await;
        let (_dir, json) = claude_json("u-STRANGER");
        let poller = FakeRosterPoller::new().ok("u-A", 0.99, 0.99);
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        let outcome = daemon.tick().await;

        assert_eq!(outcome.action, TickAction::SkippedActiveUnknown);
        assert_eq!(daemon.state.active, None);
    }

    #[tokio::test]
    async fn tick_resolves_active_via_claude_json_when_the_canonical_token_has_drifted() {
        // Steady state: the active account's token has refreshed in place, so the
        // canonical matches NO stash. The `~/.claude.json` display (u-A, in the
        // roster) is the fallback that still identifies the active account.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-drifted-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-stale-token", "u-A"), // no longer matches canonical
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // A below the trigger, so the cycle simply holds — the point is that the
        // active account was resolved at all (via the display, not a stash match).
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.30)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        let outcome = daemon.tick().await;

        assert_eq!(outcome.action, TickAction::Held);
        // Resolved to A purely through the `~/.claude.json` fallback branch.
        assert_eq!(daemon.state.active, Some(0));
    }

    // --- locked keychain & re-auth re-stash (issue #13) --------------------

    #[tokio::test]
    async fn a_locked_keychain_defers_the_whole_tick_and_signals_once() {
        // #13: a locked keychain defers the ENTIRE cycle — no resolve, no poll, no
        // swap — emits ONE edge-triggered keychain_locked_wait, and returns a
        // back-off as the next wait. The daemon never auto-unlocks or prompts; the
        // back-off is the whole response. A is set over the session trigger so that,
        // absent the lock, this cycle WOULD swap — proving the lock truly defers it.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        daemon.store.set_locked(true);

        let first = daemon.tick().await;
        assert_eq!(first.action, TickAction::KeychainLocked);
        // One lock-wait event on the FIRST locked cycle (edge-triggered)…
        assert_eq!(first.events, vec![Event::KeychainLockedWait]);
        // …with the back-off starting at the base.
        assert_eq!(first.next_wait, Some(LOCK_BACKOFF_BASE));
        // The cycle deferred before resolving: no active account, no swap.
        assert_eq!(daemon.state.active, None);
        // status still answers — the roster is listed, every reading absent.
        assert_eq!(first.snapshot.accounts.len(), 2);
        assert!(first.snapshot.accounts.iter().all(|a| a.usage.is_none()));

        // A second locked cycle is SILENT (edge-triggered) and the back-off grows.
        let second = daemon.tick().await;
        assert_eq!(second.action, TickAction::KeychainLocked);
        assert!(
            second.events.is_empty(),
            "the lock signal is edge-triggered"
        );
        assert_eq!(second.next_wait, Some(LOCK_BACKOFF_BASE * 2));

        // The canonical was never written (no auto-unlock, no swap): once the lock
        // clears, it still holds A's original token.
        daemon.store.set_locked(false);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn the_locked_keychain_back_off_doubles_then_caps() {
        // #13: the deferred-cycle back-off grows exponentially from the base and
        // saturates at the cap, so a long lock settles at one retry per cap-interval
        // rather than spinning or growing without bound.
        let roster = vec![account("u-A", "Sessiometer/u-A", "work")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-A", b"A-token", "u-A")]).await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new().ok("u-A", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        daemon.store.set_locked(true);
        let mut waits = Vec::new();
        for _ in 0..8 {
            waits.push(daemon.tick().await.next_wait.unwrap());
        }
        // Doubling from the 1 s base, capped at the 60 s ceiling:
        // 1, 2, 4, 8, 16, 32, then 64→capped 60, then 60.
        assert_eq!(
            waits,
            vec![
                LOCK_BACKOFF_BASE,
                LOCK_BACKOFF_BASE * 2,
                LOCK_BACKOFF_BASE * 4,
                LOCK_BACKOFF_BASE * 8,
                LOCK_BACKOFF_BASE * 16,
                LOCK_BACKOFF_BASE * 32,
                LOCK_BACKOFF_CAP, // 64 s would exceed the cap → clamped
                LOCK_BACKOFF_CAP,
            ]
        );
    }

    #[tokio::test]
    async fn unlocking_the_keychain_resumes_normal_ticks_and_rearms_the_signal() {
        // #13: after a lock episode, the first readable cycle clears the back-off
        // (next_wait None → normal interval) and re-arms the edge-trigger, so a
        // LATER lock episode signals afresh and restarts the back-off at the base.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        daemon.store.set_locked(true);
        let locked = daemon.tick().await;
        assert_eq!(locked.action, TickAction::KeychainLocked);
        assert_eq!(locked.events, vec![Event::KeychainLockedWait]);

        // Unlock: the next cycle reads normally, resolves the active account, holds,
        // and restores the normal interval.
        daemon.store.set_locked(false);
        let resumed = daemon.tick().await;
        assert_eq!(resumed.action, TickAction::Held);
        assert_eq!(resumed.next_wait, None);
        assert_eq!(daemon.state.active, Some(0));

        // A second lock episode signals again (the readable cycle re-armed the edge)
        // and the back-off restarts at the base, not where the first episode left off.
        daemon.store.set_locked(true);
        let relocked = daemon.tick().await;
        assert_eq!(relocked.events, vec![Event::KeychainLockedWait]);
        assert_eq!(relocked.next_wait, Some(LOCK_BACKOFF_BASE));
    }

    #[tokio::test]
    async fn a_reauth_rewrites_the_canonical_and_the_daemon_restashes_the_account() {
        // #13 core: tick 1 primes the watch on A's token. The operator then re-auths
        // A via `claude /login`, rewriting the canonical to a FRESH token (display
        // stays A — same account, refreshed credential). Tick 2 detects the
        // out-of-band change and re-stashes A with the new token, so A's stash tracks
        // the live credential; tick 3 sees no further change and does not re-fire.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // A stays below the trigger throughout: the point is the re-stash, not a swap.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        // Tick 1 primes the watch on the current canonical — no re-stash.
        let first = daemon.tick().await;
        assert_eq!(first.action, TickAction::Held);
        assert!(
            !first
                .events
                .iter()
                .any(|e| matches!(e, Event::ReStash { .. })),
            "the first observation primes, it does not re-stash"
        );
        assert_eq!(daemon.state.active, Some(0));

        // The operator re-auths A: `claude /login` rewrites the canonical token.
        daemon
            .store
            .write(&cred(b"A-reauthed-token"))
            .await
            .unwrap();

        // Tick 2 detects the change and re-stashes A with the new token.
        let second = daemon.tick().await;
        assert_eq!(
            second.events,
            vec![Event::ReStash {
                account: "work".to_owned(),
            }]
        );
        let a = daemon.stash.read("Sessiometer/u-A").await.unwrap();
        assert_eq!(a.credential.expose(), b"A-reauthed-token");
        // The identity half is preserved, and A is still the resolved active account.
        assert_eq!(a.oauth_account.account_uuid(), "u-A");
        assert_eq!(daemon.state.active, Some(0));

        // Tick 3: no further change → the committed baseline means no repeat re-stash.
        let third = daemon.tick().await;
        assert!(
            !third
                .events
                .iter()
                .any(|e| matches!(e, Event::ReStash { .. })),
            "a committed change must not re-fire"
        );
    }

    #[tokio::test]
    async fn a_reauth_to_a_different_account_restashes_it_and_reresolves_active() {
        // #13: the operator `claude /login`s into account B while A was active, so
        // the canonical becomes B's fresh token AND the display switches to B. The
        // daemon re-stashes B with the new token (resolved via the display, since no
        // stash matches the fresh token yet) and re-resolves the active account to B.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-old-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );

        // Tick 1 primes on A and resolves A as active.
        daemon.tick().await;
        assert_eq!(daemon.state.active, Some(0));

        // `/login` to B: the canonical becomes B's fresh token and the display
        // switches to B (what Claude Code writes to `~/.claude.json`).
        daemon.store.write(&cred(b"B-reauthed")).await.unwrap();
        crate::claude_state::write_oauth_account(&json, &oauth("u-B")).unwrap();

        // Tick 2 detects the change, re-stashes B (resolved via the display), and
        // re-resolves the active account to B.
        let second = daemon.tick().await;
        assert_eq!(
            second.events,
            vec![Event::ReStash {
                account: "spare".to_owned(),
            }]
        );
        let b = daemon.stash.read("Sessiometer/u-B").await.unwrap();
        assert_eq!(b.credential.expose(), b"B-reauthed");
        assert_eq!(b.oauth_account.account_uuid(), "u-B");
        assert_eq!(daemon.state.active, Some(1));
    }

    #[tokio::test]
    async fn tick_reports_no_viable_target_when_every_other_account_is_over_the_floor() {
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // A over trigger; B's session (0.85) is above the floor (0.80) → not viable.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.50)
            .ok("u-B", 0.85, 0.10);
        let tun = tunables(95, 80, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        let outcome = daemon.tick().await;

        assert_eq!(outcome.action, TickAction::NoViableTarget);
        // The floor-driven no-viable-target path emits one all_exhausted event.
        // No reading carried a weekly reset here, so #11 falls back to the active
        // handle with `resets_at` omitted (the soonest-reset path is covered by the
        // all-weekly-exhausted test below).
        assert_eq!(
            outcome.events,
            vec![Event::AllExhausted {
                hold: "work".to_owned(),
                resets_at: None,
            }],
        );
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn tick_holds_on_soonest_reset_when_all_accounts_are_weekly_exhausted() {
        // #11 acceptance: every account is weekly-exhausted, so there is no viable
        // swap target. The daemon must HOLD on the least-bad account — the one
        // whose weekly window resets soonest — emit exactly ONE signal, and perform
        // ZERO swaps no matter how many ticks run.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
            account("u-C", "Sessiometer/u-C", "third"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
            ("Sessiometer/u-C", b"C-token", "u-C"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // All three weekly-exhausted (weekly 0.99 ≥ weekly_trigger 0.98). B resets
        // soonest, so it is the least-bad hold target even though A is active.
        const A_RESET: i64 = 1_782_777_600; // 2026-06-30T00:00:00Z
        const B_RESET: i64 = 1_782_496_800; // 2026-06-26T18:00:00Z (soonest)
        const C_RESET: i64 = 1_782_864_000; // 2026-07-01T00:00:00Z
        let poller = FakeRosterPoller::new()
            .ok_resets("u-A", 0.50, 0.99, A_RESET)
            .ok_resets("u-B", 0.50, 0.99, B_RESET)
            .ok_resets("u-C", 0.50, 0.99, C_RESET);
        // Floor OFF (the #10 default); weekly_trigger 98 via the tunables helper, so
        // the swap-away fires on the weekly dimension and every target is excluded.
        let tun = tunables_floor_off(95, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        // First tick: detect all-exhausted, hold on B (soonest reset), emit once.
        let first = daemon.tick().await;
        assert_eq!(first.action, TickAction::NoViableTarget);
        assert_eq!(
            first.events,
            vec![Event::AllExhausted {
                hold: "spare".to_owned(),
                resets_at: Some(B_RESET),
            }],
        );

        // Two more ticks in the same episode: still no viable target, but the
        // signal is edge-triggered, so NOTHING further is emitted.
        for _ in 0..2 {
            let again = daemon.tick().await;
            assert_eq!(again.action, TickAction::NoViableTarget);
            assert!(
                again.events.is_empty(),
                "all_exhausted must be edge-triggered, got {:?}",
                again.events
            );
        }

        // ZERO swaps across the whole episode: canonical still A, active unchanged.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
        assert_eq!(daemon.state.active, Some(0));
    }

    #[tokio::test]
    async fn leaving_the_all_exhausted_state_clears_the_edge_guard() {
        // #11 edge re-fire: once the daemon leaves the all-exhausted state the
        // guard clears, so a later re-entry signals afresh. Here a Hold (active
        // below both triggers) is the non-exhausted cycle that resets it.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables_floor_off(95, 0);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        // Pretend a prior all-exhausted episode already signaled.
        daemon.state.signaled_all_exhausted = true;

        let outcome = daemon.tick().await;
        assert_eq!(outcome.action, TickAction::Held);
        assert!(
            !daemon.state.signaled_all_exhausted,
            "leaving the all-exhausted state must clear the edge guard",
        );
    }

    #[tokio::test]
    async fn an_over_trigger_active_within_the_cooldown_is_skipped() {
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.05, 0.05);
        let tun = tunables(95, 80, 100); // cooldown 100s

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        );
        // Simulate a swap that just happened: active A, last swap at "now".
        daemon.state.active = Some(0);
        daemon.state.last_swap = Some(LastSwap {
            to: "spare".to_owned(),
            at: daemon.clock.now(),
        });
        daemon.clock.advance(Duration::from_secs(10)); // still within the 100s cooldown

        let outcome = daemon.tick().await;

        assert_eq!(outcome.action, TickAction::SkippedCooldown);
        // No swap despite A being over the trigger and B wide open.
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
    }

    #[tokio::test]
    async fn an_over_trigger_active_past_the_cooldown_swaps() {
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.05, 0.05);
        let tun = tunables(95, 80, 100);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        );
        daemon.state.active = Some(0);
        daemon.state.last_swap = Some(LastSwap {
            to: "spare".to_owned(),
            at: daemon.clock.now(),
        });
        daemon.clock.advance(Duration::from_secs(150)); // past the 100s cooldown

        let outcome = daemon.tick().await;

        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });
    }

    #[tokio::test]
    async fn two_high_accounts_swap_at_most_once_per_cooldown_window() {
        // Issue #10 acceptance (non-oscillation): with the session floor OFF (the
        // default) and two accounts both hovering 94–96%, the cooldown ALONE bounds
        // oscillation — ≤ 1 swap per cooldown window, and never A→B→A within it.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // Both hover high (over the 95 trigger), low weekly so each is a viable
        // target for the other — the setup that WOULD ping-pong without a cooldown.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.95, 0.20)
            .ok("u-B", 0.96, 0.20);
        // Floor OFF (the #10 default); cooldown 100 s, trigger 95, no jitter.
        let tun = tunables_floor_off(95, 100);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json,
            &tun,
        );

        // Tick 1 (window opens): A is over the trigger, no prior swap → swap A→B.
        assert_eq!(
            daemon.tick().await.action,
            TickAction::Swapped { from: 0, to: 1 }
        );

        // Every later tick WITHIN the 100 s window: B is now active and also over
        // the trigger, with A wide open as a target — yet each re-swap is refused by
        // the cooldown. No second swap in the window → in particular no A→B→A.
        for offset in [20u64, 40, 60, 80] {
            daemon.clock.advance(Duration::from_secs(20));
            assert_eq!(
                daemon.tick().await.action,
                TickAction::SkippedCooldown,
                "a re-swap at +{offset}s (within the 100 s cooldown) must be refused"
            );
        }

        // Past the cooldown the swap-back is allowed — oscillation is BOUNDED by the
        // cooldown, not frozen.
        daemon.clock.advance(Duration::from_secs(40)); // now at +120 s
        assert_eq!(
            daemon.tick().await.action,
            TickAction::Swapped { from: 1, to: 0 }
        );
    }

    // --- timing jitter strategies (issue #38) ------------------------------

    /// A minimal daemon over empty seams — enough to exercise the pure
    /// `next_poll_interval` draw without any roster/poll/keychain setup.
    fn poll_daemon(tun: &Tunables, seed: u64) -> FakeDaemon {
        Daemon::new(
            vec![],
            FakeRosterPoller::new(),
            FakeCredentialStore::empty(),
            FakeAccountStash::empty(),
            FakeClock::frozen(),
            PathBuf::from("/nonexistent/.claude.json"),
            tun,
        )
        .with_seed(seed)
    }

    #[test]
    fn next_poll_interval_is_deterministic_and_stays_in_range() {
        // AC: each cycle draws a jittered poll interval within the valid range,
        // deterministic under an injected seed.
        let mut tun = tunables(95, 80, 0);
        tun.poll_strategy = Strategy {
            base: 300.0,
            jitter: Jitter::Normal { stddev: 80.0 },
        };
        let mut a = poll_daemon(&tun, 2024);
        let mut b = poll_daemon(&tun, 2024);
        let seq_a: Vec<f64> = (0..256)
            .map(|_| a.next_poll_interval().as_secs_f64())
            .collect();
        let seq_b: Vec<f64> = (0..256)
            .map(|_| b.next_poll_interval().as_secs_f64())
            .collect();
        assert_eq!(
            seq_a, seq_b,
            "same seed must replay the same poll intervals"
        );
        for s in &seq_a {
            assert!(
                (POLL_SECS_LO..=POLL_SECS_HI).contains(s),
                "poll interval {s}s out of 5..=3600"
            );
        }
        // The normal jitter actually moves the interval off the 300 s base.
        assert!(seq_a.iter().any(|&s| (s - 300.0).abs() > 1.0));
    }

    #[test]
    fn a_fixed_poll_strategy_draws_the_base_verbatim() {
        // The no-jitter path is unchanged behavior: every draw is the base.
        let tun = tunables(95, 80, 0); // poll_strategy = fixed(60.0)
        let mut daemon = poll_daemon(&tun, 1);
        for _ in 0..8 {
            assert_eq!(daemon.next_poll_interval(), Duration::from_secs(60));
        }
    }

    #[tokio::test]
    async fn a_jittered_trigger_is_deterministic_and_varies_the_swap_decision() {
        // Active A sits at a fixed 60% session; a wide uniform trigger jitter
        // spans the whole 50..=99 range, so some cycles draw a trigger ≤ 60
        // (→ swap) and others > 60 (→ hold). Deterministic per seed, but VARYING
        // across seeds — proof the trigger is drawn anew each cycle.
        async fn action_for(seed: u64) -> TickAction {
            let roster = vec![
                account("u-A", "Sessiometer/u-A", "work"),
                account("u-B", "Sessiometer/u-B", "spare"),
            ];
            let store = store_holding(b"A-token").await;
            let stash = stash_with(&[
                ("Sessiometer/u-A", b"A-token", "u-A"),
                ("Sessiometer/u-B", b"B-token", "u-B"),
            ])
            .await;
            let (_dir, json) = claude_json("u-A");
            let poller = FakeRosterPoller::new()
                .ok("u-A", 0.60, 0.10)
                .ok("u-B", 0.05, 0.05);
            let mut tun = tunables(95, 80, 0);
            tun.trigger_strategy = Strategy {
                base: 95.0,
                jitter: Jitter::Uniform { spread: 100.0 },
            };
            let mut daemon: FakeDaemon = Daemon::new(
                roster,
                poller,
                store,
                stash,
                FakeClock::frozen(),
                json,
                &tun,
            )
            .with_seed(seed);
            daemon.tick().await.action
        }
        // Determinism: the same seed replays the same decision.
        assert_eq!(action_for(11).await, action_for(11).await);
        // Variation: across seeds the jittered trigger yields BOTH outcomes at
        // the same fixed 60% usage.
        let mut holds = 0;
        let mut swaps = 0;
        for seed in 0..48 {
            match action_for(seed).await {
                TickAction::Held => holds += 1,
                TickAction::Swapped { from: 0, to: 1 } => swaps += 1,
                other => panic!("unexpected action under seed {seed}: {other:?}"),
            }
        }
        assert!(
            holds > 0 && swaps > 0,
            "jittered trigger should produce both holds ({holds}) and swaps ({swaps})"
        );
    }

    #[tokio::test]
    async fn a_jittered_weekly_trigger_is_deterministic_and_varies_the_swap_decision() {
        // The WEEKLY-axis mirror of the jittered-trigger test (issue #41): session
        // is held LOW (never trips its trigger), weekly sits at a fixed 60%, and a
        // wide uniform weekly-trigger jitter spans the whole 50..=99 range — so
        // some cycles draw a weekly trigger ≤ 60 (→ swap on the weekly dimension)
        // and others > 60 (→ hold). Deterministic per seed, varying across seeds:
        // proof the weekly trigger is drawn anew each cycle from its own strategy.
        async fn action_for(seed: u64) -> TickAction {
            let roster = vec![
                account("u-A", "Sessiometer/u-A", "work"),
                account("u-B", "Sessiometer/u-B", "spare"),
            ];
            let store = store_holding(b"A-token").await;
            let stash = stash_with(&[
                ("Sessiometer/u-A", b"A-token", "u-A"),
                ("Sessiometer/u-B", b"B-token", "u-B"),
            ])
            .await;
            let (_dir, json) = claude_json("u-A");
            // Session fixed low (never trips the 95 session trigger); weekly fixed
            // at 60%, the axis the jittered weekly trigger straddles.
            let poller = FakeRosterPoller::new()
                .ok("u-A", 0.10, 0.60)
                .ok("u-B", 0.05, 0.05);
            let mut tun = tunables(95, 80, 0);
            tun.weekly_trigger_strategy = Strategy {
                base: 95.0,
                jitter: Jitter::Uniform { spread: 100.0 },
            };
            let mut daemon: FakeDaemon = Daemon::new(
                roster,
                poller,
                store,
                stash,
                FakeClock::frozen(),
                json,
                &tun,
            )
            .with_seed(seed);
            daemon.tick().await.action
        }
        // Determinism: the same seed replays the same decision.
        assert_eq!(action_for(11).await, action_for(11).await);
        // Variation: across seeds the jittered weekly trigger yields BOTH outcomes
        // at the same fixed 60% weekly usage.
        let mut holds = 0;
        let mut swaps = 0;
        for seed in 0..48 {
            match action_for(seed).await {
                TickAction::Held => holds += 1,
                TickAction::Swapped { from: 0, to: 1 } => swaps += 1,
                other => panic!("unexpected action under seed {seed}: {other:?}"),
            }
        }
        assert!(
            holds > 0 && swaps > 0,
            "jittered weekly trigger should produce both holds ({holds}) and swaps ({swaps})"
        );
    }

    #[tokio::test]
    async fn a_jittered_cooldown_is_deterministic_and_varies_the_skip() {
        // Active A is over the (fixed) trigger with a swap 100 s ago; a wide
        // uniform cooldown jitter around 100 s makes some cycles draw a cooldown
        // below the 100 s elapsed (→ swap) and others above it (→ skip).
        // Deterministic per seed, varying across seeds.
        async fn action_for(seed: u64) -> TickAction {
            let roster = vec![
                account("u-A", "Sessiometer/u-A", "work"),
                account("u-B", "Sessiometer/u-B", "spare"),
            ];
            let store = store_holding(b"A-token").await;
            let stash = stash_with(&[
                ("Sessiometer/u-A", b"A-token", "u-A"),
                ("Sessiometer/u-B", b"B-token", "u-B"),
            ])
            .await;
            let (_dir, json) = claude_json("u-A");
            let poller = FakeRosterPoller::new()
                .ok("u-A", 0.97, 0.40)
                .ok("u-B", 0.05, 0.05);
            let mut tun = tunables(95, 80, 100);
            tun.cooldown_strategy = Strategy {
                base: 100.0,
                jitter: Jitter::Uniform { spread: 200.0 },
            };
            let mut daemon: FakeDaemon = Daemon::new(
                roster,
                poller,
                store,
                stash,
                FakeClock::new(Duration::ZERO),
                json,
                &tun,
            )
            .with_seed(seed);
            daemon.state.active = Some(0);
            daemon.state.last_swap = Some(LastSwap {
                to: "spare".to_owned(),
                at: daemon.clock.now(),
            });
            daemon.clock.advance(Duration::from_secs(100));
            daemon.tick().await.action
        }
        assert_eq!(action_for(5).await, action_for(5).await);
        let mut skipped = 0;
        let mut swapped = 0;
        for seed in 0..48 {
            match action_for(seed).await {
                TickAction::SkippedCooldown => skipped += 1,
                TickAction::Swapped { from: 0, to: 1 } => swapped += 1,
                other => panic!("unexpected action under seed {seed}: {other:?}"),
            }
        }
        assert!(
            skipped > 0 && swapped > 0,
            "jittered cooldown should produce both skips ({skipped}) and swaps ({swapped})"
        );
    }

    // --- reconcile-on-start ------------------------------------------------

    #[tokio::test]
    async fn reconcile_co_writes_the_matched_account_when_the_display_is_stale() {
        // Post-swap crash: canonical holds B's token, but the display still shows
        // A (the co-write never landed). Reconcile heals the display to B.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"B-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A"); // stale display
        let tun = tunables(95, 80, 0);
        let daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );

        daemon.reconcile_on_start().await.unwrap();
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-B"));
    }

    #[tokio::test]
    async fn reconcile_leaves_the_display_untouched_when_no_stash_matches() {
        // Normal restart: the active account's token has drifted (refreshed in
        // place), matching no stash. The display is already correct → untouched.
        let roster = vec![account("u-A", "Sessiometer/u-A", "work")];
        let store = store_holding(b"A-drifted-token").await;
        let stash = stash_with(&[("Sessiometer/u-A", b"A-old-token", "u-A")]).await;
        let (_dir, json) = claude_json("u-A");
        let tun = tunables(95, 80, 0);
        let daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );

        daemon.reconcile_on_start().await.unwrap();
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-A"));
    }

    #[tokio::test]
    async fn reconcile_is_a_noop_when_the_display_already_matches() {
        let roster = vec![account("u-A", "Sessiometer/u-A", "work")];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[("Sessiometer/u-A", b"A-token", "u-A")]).await;
        let (_dir, json) = claude_json("u-A");
        let tun = tunables(95, 80, 0);
        let daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );

        daemon.reconcile_on_start().await.unwrap();
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-A"));
    }

    // --- status snapshot + control protocol --------------------------------

    #[test]
    fn status_response_carries_handles_and_percentages_and_never_a_secret() {
        let snapshot = StatusSnapshot {
            accounts: vec![
                AccountReading {
                    label: "work".to_owned(),
                    active: true,
                    enabled: true,
                    quarantined: false,
                    usage: Some(Usage {
                        session: 0.97,
                        weekly: 0.40,
                        weekly_resets_at: None,
                    }),
                },
                AccountReading {
                    label: "spare".to_owned(),
                    active: false,
                    enabled: true,
                    quarantined: false,
                    usage: None,
                },
            ],
            last_swap: None,
        };
        let json = serde_json::to_string(&status_response(&snapshot)).unwrap();
        assert!(json.contains("\"label\":\"work\""));
        assert!(json.contains("\"active\":true"));
        // Issue #36: the rotation flag is carried so `status` can mark a parked account.
        assert!(json.contains("\"enabled\":true"));
        assert!(json.contains("\"session_pct\":97"));
        assert!(json.contains("\"weekly_pct\":40"));
        // The unavailable account reports null, not a fabricated 0.
        assert!(json.contains("\"session_pct\":null"));
        // No swap yet → the wire carries an explicit null, never a fabricated entry.
        assert!(json.contains("\"last_swap\":null"));
        // Issue #15: the projection sources only labels + percentages, so neither
        // an email nor a token can ever reach the wire.
        assert!(!json.contains('@'));
        assert!(!json.to_lowercase().contains("token"));
    }

    #[tokio::test]
    async fn serve_control_answers_status_with_exactly_one_line() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let snapshot = StatusSnapshot {
            accounts: vec![AccountReading {
                label: "work".to_owned(),
                active: true,
                enabled: true,
                quarantined: false,
                usage: Some(Usage {
                    session: 0.50,
                    weekly: 0.25,
                    weekly_resets_at: None,
                }),
            }],
            last_swap: None,
        };
        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"status\"}\n").await.unwrap();
        serve_control(server, &snapshot).await.unwrap();

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert_eq!(
            reply.lines().count(),
            1,
            "exactly one status line: {reply:?}"
        );
        assert!(reply.contains("\"label\":\"work\""));
        assert!(reply.contains("\"session_pct\":50"));
        assert!(!reply.contains('@'));
    }

    #[tokio::test]
    async fn serve_control_rejects_an_unknown_command() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"{\"cmd\":\"nope\"}\n").await.unwrap();
        serve_control(server, &StatusSnapshot::default())
            .await
            .unwrap();

        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(reply.contains("unknown command"), "got {reply:?}");
    }

    #[test]
    fn control_reply_rejects_malformed_json() {
        assert!(control_reply("not json", &StatusSnapshot::default()).contains("malformed"));
    }

    // --- last_swap + swap report (issue #8) --------------------------------

    #[test]
    fn status_response_projects_a_present_last_swap_without_a_secret() {
        let snapshot = StatusSnapshot {
            accounts: vec![],
            last_swap: Some(LastSwapLine {
                to: "spare".to_owned(),
                secs_ago: 125,
            }),
        };
        let json = serde_json::to_string(&status_response(&snapshot)).unwrap();
        assert!(json.contains("\"to\":\"spare\""), "got {json}");
        assert!(json.contains("\"secs_ago\":125"), "got {json}");
        // #15: a label + an integer only — never an email or token sigil.
        assert!(!json.contains('@'));
        assert!(!json.to_lowercase().contains("token"));
    }

    #[test]
    fn swap_report_renders_only_for_a_swap_outcome() {
        let snapshot = StatusSnapshot {
            accounts: vec![
                AccountReading {
                    label: "work".to_owned(),
                    active: false,
                    enabled: true,
                    quarantined: false,
                    usage: None,
                },
                AccountReading {
                    label: "spare".to_owned(),
                    active: true,
                    enabled: true,
                    quarantined: false,
                    usage: None,
                },
            ],
            last_swap: None,
        };
        let outcome = |action| TickOutcome {
            action,
            events: Vec::new(),
            snapshot: snapshot.clone(),
            next_wait: None,
        };
        assert_eq!(
            swap_report(&outcome(TickAction::Swapped { from: 0, to: 1 })).as_deref(),
            Some("swapped: work → spare"),
        );
        // #42: an emergency swap echoes too, named distinctly so the operator sees a
        // dead credential forced the rotation.
        assert_eq!(
            swap_report(&outcome(TickAction::EmergencySwapped { from: 0, to: 1 })).as_deref(),
            Some("emergency swap (dead credential): work → spare"),
        );
        assert_eq!(swap_report(&outcome(TickAction::Held)), None);
        assert_eq!(swap_report(&outcome(TickAction::SkippedCooldown)), None);
        assert_eq!(swap_report(&outcome(TickAction::NoViableTarget)), None);
        // A dead active account with no viable target holds — no console echo.
        assert_eq!(swap_report(&outcome(TickAction::ActiveDeadNoTarget)), None);
    }

    #[tokio::test]
    async fn snapshot_carries_last_swap_with_a_relative_age_after_a_swap() {
        // Tick 1: A is over the trigger → swap to B; the snapshot reports the swap
        // at age 0. Advance the clock; tick 2 holds (B is fresh) but the snapshot
        // still reports the swap, now aged by the elapsed time.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // After the swap, B is active; keep B below the trigger so tick 2 holds.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let first = daemon.tick().await;
        assert_eq!(first.action, TickAction::Swapped { from: 0, to: 1 });
        let swap = first.snapshot.last_swap.expect("a swap was recorded");
        assert_eq!(swap.to, "spare");
        assert_eq!(swap.secs_ago, 0);

        daemon.clock.advance(Duration::from_secs(125));
        let second = daemon.tick().await;
        assert_eq!(second.action, TickAction::Held);
        let swap = second
            .snapshot
            .last_swap
            .expect("the swap is still reported");
        assert_eq!(swap.to, "spare");
        assert_eq!(swap.secs_ago, 125);
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

    // --- run loop ----------------------------------------------------------

    #[tokio::test]
    async fn run_loop_ticks_deterministically_and_stops_on_shutdown() {
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10); // all Hold
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::from_secs(60)),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        let mut shutdown = FakeShutdown::after(3);
        let control = NoControl;

        run_loop(&mut daemon, &mut log, &mut shutdown, &control)
            .await
            .unwrap();

        // The fake clock makes the cadence deterministic: exactly 3 ticks ran.
        assert_eq!(daemon.state.ticks, 3);
    }

    #[tokio::test]
    async fn run_loop_completes_a_swap_before_a_concurrent_shutdown() {
        // Tick 1 triggers a swap; shutdown is then requested. Because a swap runs
        // to completion inside `tick` (shutdown is only observed between ticks),
        // the post-loop state is coherent — no half-swap.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        let mut shutdown = FakeShutdown::after(1); // stop right after the first tick
        let control = NoControl;

        run_loop(&mut daemon, &mut log, &mut shutdown, &control)
            .await
            .unwrap();

        // The single tick's swap landed fully: canonical = B, display = B, active = B.
        assert_eq!(daemon.state.ticks, 1);
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-B"));
        assert_eq!(daemon.state.active, Some(1));

        // End-to-end (issue #9): the swap wrote exactly one structured event line —
        // handles only (work → spare), never a token or email — to the event log.
        // The session reading (0.97) is at/over the 95 % trigger, so the line is
        // tagged `reason=session` with the outgoing account's `session_pct`.
        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(logged.lines().count(), 1, "one event line: {logged:?}");
        assert!(
            logged.contains("event=swap from=work to=spare reason=session session_pct=97"),
            "got: {logged:?}"
        );
        assert!(logged.starts_with("ts="), "stamped: {logged:?}");
        assert!(!logged.contains('@'), "no email: {logged:?}");
    }

    #[tokio::test]
    async fn note_poll_outcome_walks_the_401_streak_and_emits_one_event_per_named_condition() {
        // The daemon-side poll-outcome → event mapping and the per-account 401
        // streak (issue #9) are exercised directly: `note_poll_outcome` turns each
        // poll `Result` into at most one event and maintains the streak. Driving it
        // by hand (rather than through the loop) lets us assert the reset, which a
        // static poller cannot script on a single account across ticks.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            FakeRosterPoller::new(),
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let mut events = Vec::new();
        // Issue #42: the per-account 401 streak now lives in `health[i].consec_401`.
        let streak_of = |d: &FakeDaemon| {
            d.state
                .health
                .iter()
                .map(|h| h.consec_401)
                .collect::<Vec<_>>()
        };

        // A 401 on account 0 starts its streak at 1; a second consecutive 401
        // climbs to 2 — one `monitor_401` per occurrence, account 1 untouched.
        daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        assert_eq!(streak_of(&daemon), vec![2, 0]);
        assert_eq!(
            events,
            vec![
                Event::Monitor401 {
                    account: "work".to_owned(),
                    consecutive: 1,
                },
                Event::Monitor401 {
                    account: "work".to_owned(),
                    consecutive: 2,
                },
            ]
        );

        // A success resets account 0's streak and emits nothing.
        events.clear();
        daemon.note_poll_outcome(
            0,
            &Ok(Usage {
                session: 0.10,
                weekly: 0.10,
                weekly_resets_at: None,
            }),
            &mut events,
        );
        assert_eq!(streak_of(&daemon), vec![0, 0]);
        assert!(events.is_empty());

        // After the reset the next 401 restarts the streak at 1 (not 3).
        daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        assert_eq!(streak_of(&daemon), vec![1, 0]);
        assert_eq!(
            events,
            vec![Event::Monitor401 {
                account: "work".to_owned(),
                consecutive: 1,
            }]
        );

        // A locked keychain is detected at top-of-tick now, not per-account (issue
        // #13), so this arm emits NOTHING — it only resets the streak, like any
        // other non-401 outcome. Account 0's streak (1) is left untouched.
        events.clear();
        daemon.note_poll_outcome(1, &Err(Error::KeychainLocked { op: "read" }), &mut events);
        assert_eq!(streak_of(&daemon), vec![1, 0]);
        assert!(events.is_empty());

        // A 403 (missing usage scope) on account 0 emits `usage_scope_fail` and
        // resets its streak — every non-401 outcome clears the streak.
        events.clear();
        daemon.note_poll_outcome(0, &Err(Error::UsageScopeMissing), &mut events);
        assert_eq!(streak_of(&daemon), vec![0, 0]);
        assert_eq!(
            events,
            vec![Event::UsageScopeFail {
                account: "work".to_owned(),
            }]
        );

        // A transient error is silent and also resets (no event, streak cleared).
        events.clear();
        daemon.note_poll_outcome(0, &Err(Error::UsageTransient { status: 0 }), &mut events);
        assert_eq!(streak_of(&daemon), vec![0, 0]);
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn run_loop_logs_one_line_per_poll_rejection_each_tick() {
        // Issue #9 acceptance (as amended by #13): each PER-ACCOUNT poll rejection
        // — a 401 and a 403 (missing usage scope) — emits EXACTLY one structured
        // line per occurrence. A per-account keychain-lock is now SILENT here: the
        // lock is process-global and signaled once at top-of-tick (#13), not per
        // poll. A roster where one account 401s, one hits a (now-silent) lock, and
        // one 403s, run for two ticks, writes two lines per EMITTING account — and
        // the 401 streak must climb 1 → 2, proving `note_poll_outcome` is wired into
        // the loop and serialized.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
            account("u-C", "Sessiometer/u-C", "backup"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
            ("Sessiometer/u-C", b"C-token", "u-C"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .unauthorized("u-A")
            .keychain_locked("u-B")
            .scope_missing("u-C");
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::from_secs(60)),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        let mut shutdown = FakeShutdown::after(2);
        let control = NoControl;

        run_loop(&mut daemon, &mut log, &mut shutdown, &control)
            .await
            .unwrap();

        assert_eq!(daemon.state.ticks, 2);

        // Two ticks × two EMITTING accounts (401 + 403) = four event lines, each
        // stamped, none carrying secret material (handles only — never a token or
        // email). The locked account contributes nothing per-account (#13).
        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(logged.lines().count(), 4, "four lines: {logged:?}");
        assert!(
            logged.lines().all(|l| l.starts_with("ts=")),
            "stamped: {logged:?}"
        );
        assert!(!logged.contains('@'), "no email: {logged:?}");

        // The 401 streak is per-occurrence and climbs across ticks.
        assert!(
            logged.contains("event=monitor_401 account=work consecutive=1"),
            "{logged:?}"
        );
        assert!(
            logged.contains("event=monitor_401 account=work consecutive=2"),
            "{logged:?}"
        );
        // The per-account keychain-lock is silent now (#13): NO lock line appears,
        // even though account `spare`'s poll returned a locked error every tick.
        assert!(
            !logged.contains("event=keychain_locked_wait"),
            "a per-account lock must not emit a line: {logged:?}"
        );
        // The 403 line renders once per tick and carries `status=403`.
        assert_eq!(
            logged
                .lines()
                .filter(|l| l.contains("event=usage_scope_fail account=backup status=403"))
                .count(),
            2,
            "{logged:?}"
        );
        // The active account was unavailable every tick, so no swap line appears;
        // the streak is pure observability. Final state: account 0 saw two 401s.
        assert!(!logged.contains("event=swap"), "{logged:?}");
        let streak_of = |d: &FakeDaemon| {
            d.state
                .health
                .iter()
                .map(|h| h.consec_401)
                .collect::<Vec<_>>()
        };
        assert_eq!(streak_of(&daemon), vec![2, 0, 0]);
    }

    #[tokio::test]
    async fn run_loop_logs_a_weekly_reason_when_only_the_weekly_dimension_trips() {
        // Issue #9: a swap driven by the WEEKLY dimension (session below its
        // trigger) is logged `reason=weekly`, while `session_pct` still reports the
        // outgoing account's session reading (the schema carries no weekly percent).
        // This guards the reason re-derivation against mislabeling a weekly-only
        // swap as `session`.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        // Session 0.50 is below the 95 % session trigger; weekly 0.99 is over the
        // fixed 98 % weekly trigger → a weekly-only swap. Target B is under the floor.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.50, 0.99)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );

        let logdir = tempfile::tempdir().unwrap();
        let log_path = logdir.path().join("sessiometer.log");
        let mut log = EventLog::at(&log_path).unwrap();
        let mut shutdown = FakeShutdown::after(1);
        let control = NoControl;

        run_loop(&mut daemon, &mut log, &mut shutdown, &control)
            .await
            .unwrap();

        let logged = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(logged.lines().count(), 1, "one event line: {logged:?}");
        assert!(
            logged.contains("event=swap from=work to=spare reason=weekly session_pct=50"),
            "got: {logged:?}"
        );
    }

    // --- #42 dead-credential lifecycle -------------------------------------
    //
    // The persistent-401 lifecycle: detect (N consecutive 401s → DEAD), quarantine
    // (skip the dead account, never halt the rotation), emergency-swap (escape a dead
    // ACTIVE account immediately, bypassing trigger + cooldown), auto-recover (M
    // consecutive live polls un-quarantine a re-logged-in account), and signal (one
    // edge-triggered event per transition + a durable "needs re-login" status). The
    // pure `classify_poll` mapping and the per-account health that carries the streak
    // ACROSS ticks (the issue's CODE PREREQUISITE) are exercised directly.

    /// A two-account daemon (`work` active, `spare` spare) with both tokens stashed
    /// and the canonical holding `work`'s — the common fixture for the lifecycle
    /// tests below. `monitor_401_n` = 3, `monitor_recovery_m` = 2 (the test defaults).
    async fn lifecycle_daemon() -> FakeDaemon {
        lifecycle_daemon_with(FakeRosterPoller::new(), tunables(95, 80, 0)).await
    }

    /// Like [`lifecycle_daemon`] but with a caller-chosen poller + tunables, for the
    /// tick-driven tests that script per-account poll outcomes.
    async fn lifecycle_daemon_with(poller: FakeRosterPoller, tun: Tunables) -> FakeDaemon {
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (dir, json) = claude_json("u-A");
        // Keep the temp `~/.claude.json` alive for the daemon's lifetime by leaking
        // the guard — these are short-lived unit-test daemons.
        std::mem::forget(dir);
        Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        )
    }

    fn live(session: f64, weekly: f64) -> Result<Usage> {
        Ok(Usage {
            session,
            weekly,
            weekly_resets_at: None,
        })
    }

    #[tokio::test]
    async fn classify_poll_maps_each_result_to_its_liveness_class() {
        // The typed poll outcome (issue #42 CODE PREREQUISITE): the HTTP taxonomy is
        // folded onto the liveness/death axis in exactly one place. Success is Live,
        // 401 is Unauthorized (the death signal), 403 is its own ScopeMissing class,
        // and EVERY other failure collapses into the single Transient class.
        assert_eq!(classify_poll(&live(0.5, 0.5)), PollOutcome::Live);
        assert_eq!(
            classify_poll(&Err(Error::UsageUnauthorized)),
            PollOutcome::Unauthorized
        );
        assert_eq!(
            classify_poll(&Err(Error::UsageScopeMissing)),
            PollOutcome::ScopeMissing
        );
        for err in [
            Error::UsageTransient { status: 0 },
            Error::UsageRateLimited { status: 429 },
            Error::UsageRejected { status: 400 },
            Error::KeychainLocked { op: "read" },
            Error::UsageTokenUnreadable,
            Error::UsageParse("no dimension".to_owned()),
        ] {
            assert_eq!(
                classify_poll(&Err(err)),
                PollOutcome::Transient,
                "every non-401/403 failure folds into Transient",
            );
        }
    }

    #[tokio::test]
    async fn nth_consecutive_401_quarantines_the_account_and_signals_once() {
        // Detection + edge-trigger + anti-spam, driven directly (a static poller
        // cannot script a streak that crosses the threshold). Driving `spare`
        // (non-active) isolates detection from the emergency-swap path.
        let mut daemon = lifecycle_daemon().await;
        let mut events = Vec::new();

        // Two 401s climb the streak; below the threshold (3) the account stays alive.
        daemon.note_poll_outcome(1, &Err(Error::UsageUnauthorized), &mut events);
        daemon.note_poll_outcome(1, &Err(Error::UsageUnauthorized), &mut events);
        assert!(!daemon.state.health[1].quarantined);
        assert_eq!(daemon.state.health[1].consec_401, 2);

        // The 3rd consecutive 401 declares the credential DEAD: the climbing
        // `monitor_401` AND exactly one `credential_dead`, on the false→true edge.
        events.clear();
        daemon.note_poll_outcome(1, &Err(Error::UsageUnauthorized), &mut events);
        assert!(daemon.state.health[1].quarantined);
        assert_eq!(
            events,
            vec![
                Event::Monitor401 {
                    account: "spare".to_owned(),
                    consecutive: 3,
                },
                Event::CredentialDead {
                    account: "spare".to_owned(),
                },
            ]
        );

        // A 4th 401 on the already-dead account is SILENT — the dead state is a
        // durable status, not a repeated log line (no spam).
        events.clear();
        daemon.note_poll_outcome(1, &Err(Error::UsageUnauthorized), &mut events);
        assert!(daemon.state.health[1].quarantined);
        assert!(
            events.is_empty(),
            "an already-dead 401 re-emits nothing: {events:?}"
        );
    }

    #[tokio::test]
    async fn a_dead_non_active_account_is_skipped_while_the_rotation_continues() {
        // Quarantine-one (never halt): a dead SPARE is skipped in polling — not a
        // wasted curl, not a swap candidate — while the active account still rotates
        // to a healthy target. The daemon never halts the whole rotation on one dead
        // account.
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
            account("u-C", "Sessiometer/u-C", "backup"),
        ];
        let store = store_holding(b"A-token").await;
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"),
            ("Sessiometer/u-B", b"B-token", "u-B"),
            ("Sessiometer/u-C", b"C-token", "u-C"),
        ])
        .await;
        let (_dir, json) = claude_json("u-A");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.10) // active, over the session trigger → wants a swap
            .unauthorized("u-B") // scripted to 401 — but it is dead, so never polled
            .ok("u-C", 0.10, 0.10); // the only healthy target
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json,
            &tun,
        );
        // `spare` is already dead from a prior episode.
        daemon.state.health[1].quarantined = true;

        let outcome = daemon.tick().await;

        // The rotation continues: the active account swaps to the healthy `backup`,
        // NOT to the dead `spare` (a quarantined account is never a target).
        assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 2 });
        // `spare` was skipped, not polled: its 401 script never ran, so its streak
        // stayed 0 and it emitted no `monitor_401`.
        assert_eq!(
            daemon.state.health[1].consec_401, 0,
            "the dead spare was not polled"
        );
        assert!(
            !outcome.events.iter().any(|e| matches!(
                e,
                Event::Monitor401 { account, .. } if account == "spare"
            )),
            "a skipped account emits no poll-outcome event: {:?}",
            outcome.events
        );
    }

    #[tokio::test]
    async fn an_emergency_swap_escapes_a_dead_active_account_bypassing_trigger_and_cooldown() {
        // Emergency-swap: a confirmed-dead ACTIVE account is escaped IMMEDIATELY to
        // the soonest-reset viable target, bypassing BOTH the swap-away trigger (the
        // dead account has no reading to be "over") and the cooldown. A long cooldown
        // plus a just-completed swap would make a NORMAL over-trigger swap
        // `SkippedCooldown`; the emergency path overrides it.
        let mut daemon =
            lifecycle_daemon_with(FakeRosterPoller::new(), tunables(95, 80, 9_999)).await;
        let at = daemon.clock.now();
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true;
        daemon.state.last_swap = Some(LastSwap {
            to: "work".to_owned(),
            at, // zero elapsed against a 9_999s cooldown → a normal swap would defer
        });

        // The dead active has no reading (still 401ing); the spare polled live.
        let readings = vec![
            None,
            Some(Usage {
                session: 0.10,
                weekly: 0.10,
                weekly_resets_at: None,
            }),
        ];
        let mut events = Vec::new();
        let action = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;

        assert_eq!(action, TickAction::EmergencySwapped { from: 0, to: 1 });
        assert_eq!(
            events,
            vec![Event::EmergencySwap {
                from: "work".to_owned(),
                to: "spare".to_owned(),
            }]
        );
        // The swap took effect: the spare is now active.
        assert_eq!(daemon.state.active, Some(1));
    }

    #[tokio::test]
    async fn a_recovering_active_account_is_held_never_swapped_away() {
        // Thrash-safety / protect-recovery: a quarantined ACTIVE account that is
        // polling live again is the operator's re-login recovering it. Hold — never
        // emergency-swap a credential that now works, never swap away mid-recovery.
        let mut daemon = lifecycle_daemon().await;
        let at = daemon.clock.now();
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true;

        // The active account polled live (recovering); the spare is also available.
        let readings = vec![
            Some(Usage {
                session: 0.10,
                weekly: 0.10,
                weekly_resets_at: None,
            }),
            Some(Usage {
                session: 0.10,
                weekly: 0.10,
                weekly_resets_at: None,
            }),
        ];
        let mut events = Vec::new();
        let action = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;

        assert_eq!(action, TickAction::Held);
        assert!(
            events.is_empty(),
            "a held recovery emits nothing: {events:?}"
        );
        // `decide_action` never recovers — only `note_poll_outcome` does — so the
        // account is still quarantined here.
        assert!(daemon.state.health[0].quarantined);
        assert_eq!(daemon.state.active, Some(0), "no swap away mid-recovery");
    }

    #[tokio::test]
    async fn a_dead_active_account_with_no_viable_target_holds_silently() {
        // Emergency-swap with nowhere to go: a dead active account whose only other
        // account is also unavailable holds (`ActiveDeadNoTarget`) without thrashing
        // — and silently, because `credential_dead` already fired on the transition.
        let mut daemon = lifecycle_daemon().await;
        let at = daemon.clock.now();
        daemon.state.active = Some(0);
        daemon.state.health[0].quarantined = true;

        // No other account has a reading → no viable target.
        let readings = vec![None, None];
        let mut events = Vec::new();
        let action = daemon
            .decide_action(at, Some(0), &readings, &mut events)
            .await;

        assert_eq!(action, TickAction::ActiveDeadNoTarget);
        assert!(
            events.is_empty(),
            "the stuck dead-active state re-signals nothing: {events:?}"
        );
        assert_eq!(daemon.state.active, Some(0), "no swap with no target");
    }

    #[tokio::test]
    async fn m_consecutive_live_polls_recover_a_quarantined_account_and_signal_once() {
        // Auto-recovery: a re-logged-in account (active again via the #13 re-stash)
        // un-quarantines after M consecutive live polls, emitting exactly one
        // `credential_restored` on the dead→alive edge.
        let mut daemon = lifecycle_daemon().await;
        daemon.state.health[0].quarantined = true;
        let mut events = Vec::new();

        // The first live poll while quarantined is a recovery PROBE — still dead,
        // and silent (below `monitor_recovery_m` = 2).
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(daemon.state.health[0].quarantined);
        assert_eq!(daemon.state.health[0].recovery_successes, 1);
        assert!(events.is_empty());

        // The 2nd consecutive live reaches the threshold → RESTORED (one event).
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(!daemon.state.health[0].quarantined);
        assert_eq!(
            daemon.state.health[0].recovery_successes, 0,
            "the probe resets on restore"
        );
        assert_eq!(
            events,
            vec![Event::CredentialRestored {
                account: "work".to_owned(),
            }]
        );

        // A later live on the now-healthy account emits nothing (edge-triggered).
        events.clear();
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn a_401_mid_recovery_resets_the_probe_so_recovery_must_restart() {
        // The recovery streak is consecutive: a 401 partway through breaks it, so a
        // single later live is NOT enough — a full M=2 fresh live polls are required.
        let mut daemon = lifecycle_daemon().await;
        daemon.state.health[0].quarantined = true;
        let mut events = Vec::new();

        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events); // probe at 1
        assert_eq!(daemon.state.health[0].recovery_successes, 1);
        // A 401 mid-recovery breaks the streak (and is silent — already dead).
        daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        assert_eq!(
            daemon.state.health[0].recovery_successes, 0,
            "the 401 reset the probe"
        );
        assert!(daemon.state.health[0].quarantined);
        assert!(events.is_empty());

        // One live after the reset is not enough; the second crosses the threshold.
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(
            daemon.state.health[0].quarantined,
            "one live after a reset is not enough"
        );
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        assert!(!daemon.state.health[0].quarantined);
        assert_eq!(
            events,
            vec![Event::CredentialRestored {
                account: "work".to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn a_quarantined_account_surfaces_a_durable_needs_relogin_status() {
        // Signal — the durable status: a dead account is reported `quarantined` in
        // the `status` snapshot and on the wire, carrying a stable handle but no
        // token and no email (#15).
        let poller = FakeRosterPoller::new().ok("u-A", 0.10, 0.10); // active holds
        let mut daemon = lifecycle_daemon_with(poller, tunables(95, 80, 0)).await;
        daemon.state.health[1].quarantined = true; // `spare` is dead

        let outcome = daemon.tick().await;

        let spare = &outcome.snapshot.accounts[1];
        assert_eq!(spare.label, "spare");
        assert!(
            spare.quarantined,
            "the dead account carries a durable status"
        );
        // The wire projection carries the flag but never a secret.
        let json = serde_json::to_string(&status_response(&outcome.snapshot)).unwrap();
        assert!(json.contains(r#""quarantined":true"#), "got {json}");
        assert!(!json.contains('@'), "no email on the wire: {json}");
        assert!(!json.to_lowercase().contains("token"));
    }

    #[tokio::test]
    async fn a_dead_spare_is_never_polled_so_it_cannot_spuriously_recover() {
        // The recovery precondition, enforced structurally: a quarantined NON-active
        // account is skipped in polling, so it accrues no recovery successes and can
        // never un-quarantine on its own. It can only recover by first becoming active
        // — which happens only via the operator's re-login (the #13 re-stash, covered
        // by the next test). Without that, even an account whose token WOULD poll live
        // stays dead across ticks.
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10) // active, holds
            .ok("u-B", 0.10, 0.10); // WOULD be live — but the dead spare is never polled
        let mut daemon = lifecycle_daemon_with(poller, tunables(95, 80, 0)).await;
        daemon.state.health[1].quarantined = true; // `spare` died in a prior episode

        for _ in 0..3 {
            let outcome = daemon.tick().await;
            assert!(
                !outcome
                    .events
                    .iter()
                    .any(|e| matches!(e, Event::CredentialRestored { .. })),
                "a never-polled spare must not recover: {:?}",
                outcome.events
            );
        }

        assert!(
            daemon.state.health[1].quarantined,
            "still dead — never polled"
        );
        assert_eq!(daemon.state.health[1].recovery_successes, 0);
        assert_eq!(daemon.state.health[1].consec_401, 0);
    }

    #[tokio::test]
    async fn a_relogin_makes_a_dead_account_active_and_m_live_polls_restore_it() {
        // The full auto-recovery path end-to-end (AC #4), exercising the #13↔#42 seam
        // the unit tests stub: a dead account (quarantined, already emergency-swapped
        // away so the spare is active) is re-logged-in by the operator. The #13
        // canonical-change re-stash makes it active again; THEN — and only then — its
        // live polls count toward recovery, un-quarantining it after M (2).
        let roster = vec![
            account("u-A", "Sessiometer/u-A", "work"),
            account("u-B", "Sessiometer/u-B", "spare"),
        ];
        let store = store_holding(b"B-token").await; // `spare` is active post-emergency-swap
        let stash = stash_with(&[
            ("Sessiometer/u-A", b"A-token", "u-A"), // the OLD dead token
            ("Sessiometer/u-B", b"B-token", "u-B"),
        ])
        .await;
        let (_dir, json) = claude_json("u-B");
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.10, 0.10)
            .ok("u-B", 0.10, 0.10);
        let tun = tunables(95, 80, 0);
        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::frozen(),
            json.clone(),
            &tun,
        );
        // The post-emergency-swap state: `work` is dead and parked off the active slot.
        daemon.state.active = Some(1);
        daemon.state.health[0].quarantined = true;

        // Tick 1 primes the canonical watch on `spare`; the dead `work` is skipped.
        let first = daemon.tick().await;
        assert!(!first
            .events
            .iter()
            .any(|e| matches!(e, Event::ReStash { .. } | Event::CredentialRestored { .. })));
        assert_eq!(
            daemon.state.health[0].recovery_successes, 0,
            "not polled while parked"
        );
        assert!(daemon.state.health[0].quarantined);

        // The operator `claude /login`s back into `work`: the canonical becomes its
        // fresh token and the display switches to it.
        daemon.store.write(&cred(b"A-reauthed")).await.unwrap();
        crate::claude_state::write_oauth_account(&json, &oauth("u-A")).unwrap();

        // Tick 2 detects the change, re-stashes `work`, re-resolves it active, and its
        // first live poll is the first recovery success — still dead (M = 2).
        let second = daemon.tick().await;
        assert!(
            second
                .events
                .iter()
                .any(|e| matches!(e, Event::ReStash { account } if account == "work")),
            "the re-login re-stashes work: {:?}",
            second.events
        );
        assert_eq!(
            daemon.state.active,
            Some(0),
            "the re-logged-in account is active again"
        );
        assert!(
            daemon.state.health[0].quarantined,
            "one live poll is not yet a recovery"
        );
        assert_eq!(daemon.state.health[0].recovery_successes, 1);

        // Tick 3: the second consecutive live poll reaches M → RESTORED, once.
        let third = daemon.tick().await;
        assert!(
            !daemon.state.health[0].quarantined,
            "M live polls un-quarantine it"
        );
        assert_eq!(
            third
                .events
                .iter()
                .filter(|e| matches!(e, Event::CredentialRestored { account } if account == "work"))
                .count(),
            1,
            "exactly one credential_restored on the edge: {:?}",
            third.events
        );
    }

    #[tokio::test]
    async fn the_dead_and_restored_edges_re_arm_across_episodes() {
        // Edge-trigger re-arm (AC #5): a full dead→restored→dead cycle emits
        // credential_dead on EACH death edge and credential_restored on the recovery
        // edge — never stuck, never doubled. Proves the signals are per-transition,
        // not one-shot-per-process.
        let mut daemon = lifecycle_daemon().await;
        let mut events = Vec::new();

        // Episode 1 — death: 3 consecutive 401s.
        for _ in 0..3 {
            daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        }
        // Recovery: 2 consecutive live polls.
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        daemon.note_poll_outcome(0, &live(0.10, 0.10), &mut events);
        // Episode 2 — death again: the streak re-armed, so 3 fresh 401s re-quarantine.
        for _ in 0..3 {
            daemon.note_poll_outcome(0, &Err(Error::UsageUnauthorized), &mut events);
        }

        let deaths = events
            .iter()
            .filter(|e| matches!(e, Event::CredentialDead { .. }))
            .count();
        let restores = events
            .iter()
            .filter(|e| matches!(e, Event::CredentialRestored { .. }))
            .count();
        assert_eq!(deaths, 2, "one credential_dead per death edge: {events:?}");
        assert_eq!(
            restores, 1,
            "one credential_restored per recovery edge: {events:?}"
        );
        assert!(daemon.state.health[0].quarantined, "ends dead in episode 2");
    }

    // --- redaction METER (issue #15) ---------------------------------------
    //
    // The whole-corpus output-redaction gate. It drives the poll→decide→swap loop
    // body ([`Daemon::tick`]) across fault-injected scenarios with KNOWN secrets
    // seeded into every daemon input (the canonical store, the stashes, and
    // `~/.claude.json`), harvests EVERY operator-facing channel into one corpus,
    // and asserts — via [`crate::redaction::meter`] — that no token, no
    // credential-blob fingerprint, and no email surfaces anywhere. The meter
    // engine and its own non-vacuity proofs (each leak class planted and caught)
    // live in `crate::redaction`; this is the driver that feeds it real output.

    /// An `oauthAccount` carrying a chosen `uuid` and the secret `email`.
    fn meter_oauth(uuid: &str, email: &str) -> OauthAccount {
        OauthAccount::from_object_bytes(
            format!(r#"{{"accountUuid":"{uuid}","emailAddress":"{email}"}}"#).as_bytes(),
        )
        .unwrap()
    }

    /// A stash holding the secret `blob` + an identity carrying the secret `email`.
    fn meter_stashed(blob: &[u8], uuid: &str, email: &str) -> StashedAccount {
        StashedAccount {
            credential: cred(blob),
            oauth_account: meter_oauth(uuid, email),
        }
    }

    /// A daemon whose every credential input carries the fixture's secrets: the
    /// canonical store and each per-account stash hold the secret blob, and each
    /// stashed identity (plus `~/.claude.json`) carries the secret email. Returns
    /// the daemon and the tempdir guard that keeps `~/.claude.json` alive.
    ///
    /// `~/.claude.json` is Claude Code's OWN state file — it legitimately holds the
    /// email — and is deliberately NOT one of the harvested output channels.
    async fn meter_daemon(
        secrets: &crate::redaction::meter::Secrets,
        accounts: &[(&str, &str)],
        poller: FakeRosterPoller,
        tun: &Tunables,
    ) -> (FakeDaemon, tempfile::TempDir) {
        let blob = secrets.blob();
        let email = secrets.email();

        let roster: Vec<Account> = accounts
            .iter()
            .map(|(uuid, label)| account(uuid, &format!("Sessiometer/{uuid}"), label))
            .collect();

        let store = FakeCredentialStore::empty();
        store.write(&cred(blob)).await.unwrap();
        let stash = FakeAccountStash::empty();
        for (uuid, _) in accounts {
            stash
                .write(
                    &format!("Sessiometer/{uuid}"),
                    &meter_stashed(blob, uuid, email),
                )
                .await
                .unwrap();
        }

        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join(".claude.json");
        std::fs::write(
            &json,
            format!(
                r#"{{"numStartups":1,"oauthAccount":{{"accountUuid":"{}","emailAddress":"{email}"}}}}"#,
                accounts[0].0
            ),
        )
        .unwrap();

        let daemon = Daemon::new(roster, poller, store, stash, FakeClock::frozen(), json, tun);
        (daemon, dir)
    }

    /// Append every operator-facing channel of one tick's outcome to `corpus`,
    /// sourced from the EXACT canonical surfaces production uses: the single log
    /// surface ([`Event::to_log_line`]), the UDS wire ([`status_response`] +
    /// [`control_reply`]), the `status` text ([`crate::cli::render_status`]), and
    /// the foreground swap echo ([`swap_report`]).
    fn harvest_channels(outcome: &TickOutcome, corpus: &mut String) {
        // A fixed wall-clock stamp keeps the log lines deterministic; the value is
        // a non-secret timestamp regardless.
        let ts = std::time::UNIX_EPOCH + Duration::from_secs(1_782_777_600);
        for event in &outcome.events {
            corpus.push_str(&event.to_log_line(ts));
            corpus.push('\n');
        }
        let response = status_response(&outcome.snapshot);
        corpus.push_str(&serde_json::to_string(&response).unwrap());
        corpus.push('\n');
        corpus.push_str(&control_reply(r#"{"cmd":"status"}"#, &outcome.snapshot));
        corpus.push('\n');
        corpus.push_str(&crate::cli::render_status(&response));
        if let Some(report) = swap_report(outcome) {
            corpus.push_str(&report);
            corpus.push('\n');
        }
    }

    /// One representative value of EVERY [`Error`] variant — the error-message
    /// channel. Each carries only structural fields (paths, counts, codes, static
    /// field/op names); none can carry a token or email by construction, and the
    /// METER confirms the Display format strings hold to that.
    fn every_error_variant() -> Vec<Error> {
        vec![
            Error::Unimplemented("usage polling (#5)"),
            Error::UnknownCommand("bogus".to_owned()),
            Error::HomeUnresolved,
            Error::ForeignOwnership(PathBuf::from("/home/op/.config/sessiometer")),
            Error::CredentialNotFound,
            Error::CredentialAmbiguous { count: 2 },
            Error::KeychainLocked { op: "read" },
            Error::Keychain {
                op: "write",
                code: 1,
            },
            Error::ConfigNotFound {
                path: PathBuf::from("/home/op/.config/sessiometer/config.toml"),
            },
            Error::RosterEmpty,
            Error::ConfigParse("expected `=` at line 3".to_owned()),
            Error::ConfigInvalid("session_trigger must be in 50..=99, got 120".to_owned()),
            Error::ConfigFloorAboveTrigger {
                floor: 95,
                trigger: 90,
            },
            Error::ClaudeStateNotFound {
                path: PathBuf::from("/home/op/.claude.json"),
            },
            Error::ClaudeStateParse {
                line: 5,
                column: 12,
            },
            Error::OauthAccountMissing,
            Error::OauthAccountFieldMissing {
                field: "accountUuid",
            },
            Error::LabelRequired,
            Error::RotationLabelRequired { verb: "disable" },
            Error::AccountLabelNotFound {
                label: "work".to_owned(),
            },
            Error::StashIncomplete {
                service: "Sessiometer/11111111-1111-1111-1111-111111111111".to_owned(),
            },
            Error::UsageTokenUnreadable,
            Error::UsageTransient { status: 0 },
            Error::UsageRateLimited { status: 429 },
            Error::UsageRejected { status: 400 },
            Error::UsageUnauthorized,
            Error::UsageScopeMissing,
            Error::UsageParse("no session (five_hour) dimension".to_owned()),
            Error::AlreadyRunning,
            Error::DaemonNotRunning,
            Error::Io(std::io::Error::other("boom")),
        ]
    }

    #[tokio::test]
    async fn redaction_meter_emits_no_secret_on_any_channel_across_the_full_loop() {
        use crate::redaction::meter::{assert_clean, Secrets};

        let secrets = Secrets::meter_fixture();
        let mut corpus = String::new();

        // Recognizable, LOW-entropy uuids/labels: only the label reaches the
        // log/status/UDS channels; the uuid reaches only the `list` view. Keeping
        // them low-entropy means the entropy backstop fires only on a genuine
        // secret leak, never on the test scaffolding itself.
        const A: (&str, &str) = ("11111111-1111-1111-1111-111111111111", "work");
        const B: (&str, &str) = ("22222222-2222-2222-2222-222222222222", "spare");
        const C: (&str, &str) = ("33333333-3333-3333-3333-333333333333", "backup");

        // Scenario 1 — a swap: Event::Swap, the snapshot, and the foreground echo.
        {
            let poller = FakeRosterPoller::new()
                .ok(A.0, 0.97, 0.40) // active, over the session trigger
                .ok(B.0, 0.10, 0.20); // the (only) viable target
            let tun = tunables(95, 80, 0);
            let (mut daemon, _dir) = meter_daemon(&secrets, &[A, B], poller, &tun).await;
            let outcome = daemon.tick().await;
            assert_eq!(outcome.action, TickAction::Swapped { from: 0, to: 1 });
            harvest_channels(&outcome, &mut corpus);
        }

        // Scenario 2 — the all-exhausted terminal state: Event::AllExhausted with a
        // rendered `resets_at` (every account weekly-exhausted, no viable target).
        {
            const A_RESET: i64 = 1_782_777_600;
            const B_RESET: i64 = 1_782_496_800; // soonest -> the held account
            const C_RESET: i64 = 1_782_864_000;
            let poller = FakeRosterPoller::new()
                .ok_resets(A.0, 0.50, 0.99, A_RESET)
                .ok_resets(B.0, 0.50, 0.99, B_RESET)
                .ok_resets(C.0, 0.50, 0.99, C_RESET);
            let tun = tunables_floor_off(95, 0);
            let (mut daemon, _dir) = meter_daemon(&secrets, &[A, B, C], poller, &tun).await;
            let outcome = daemon.tick().await;
            assert_eq!(outcome.action, TickAction::NoViableTarget);
            harvest_channels(&outcome, &mut corpus);
        }

        // Scenario 3a — poll-rejection fault injection: a 401 (active) and a 403
        // each emit their poll-outcome event in one tick. Account B's poll hits a
        // per-account lock, which is now silent (#13) and contributes no event.
        {
            let poller = FakeRosterPoller::new()
                .unauthorized(A.0) // monitor_401
                .keychain_locked(B.0) // silent per-account (#13)
                .scope_missing(C.0); // usage_scope_fail (403)
            let tun = tunables(95, 80, 0);
            let (mut daemon, _dir) = meter_daemon(&secrets, &[A, B, C], poller, &tun).await;
            let outcome = daemon.tick().await;
            assert_eq!(outcome.action, TickAction::SkippedActiveUnavailable);
            harvest_channels(&outcome, &mut corpus);
        }

        // Scenario 3b — a globally LOCKED keychain (#13): the top-of-tick canonical
        // read fails, the whole cycle defers, and the accountless
        // keychain_locked_wait event plus the all-absent status snapshot are
        // harvested — proving the locked-path channels leak nothing either.
        {
            let poller = FakeRosterPoller::new();
            let tun = tunables(95, 80, 0);
            let (mut daemon, _dir) = meter_daemon(&secrets, &[A, B, C], poller, &tun).await;
            daemon.store.set_locked(true);
            let outcome = daemon.tick().await;
            assert_eq!(outcome.action, TickAction::KeychainLocked);
            harvest_channels(&outcome, &mut corpus);
        }

        // Scenario 4 — the dead-credential lifecycle (#42): a single 401 on the
        // active account (threshold 1) declares it DEAD and triggers an emergency
        // swap in one tick, so `credential_dead`, `emergency_swap`, AND the durable
        // `quarantined` status (snapshot + wire + text) are all harvested at once.
        {
            let poller = FakeRosterPoller::new()
                .unauthorized(A.0) // active → 401 → dead at threshold 1
                .ok(B.0, 0.10, 0.20); // the viable escape target
            let tun = Tunables {
                monitor_401_n: 1,
                ..tunables(95, 80, 0)
            };
            let (mut daemon, _dir) = meter_daemon(&secrets, &[A, B], poller, &tun).await;
            let outcome = daemon.tick().await;
            assert_eq!(
                outcome.action,
                TickAction::EmergencySwapped { from: 0, to: 1 }
            );
            harvest_channels(&outcome, &mut corpus);
        }

        // Scenario 5 — auto-recovery (#42): a re-logged-in account polls live and,
        // at `monitor_recovery_m` = 1, un-quarantines — harvesting the
        // `credential_restored` line through the real daemon path.
        {
            let poller = FakeRosterPoller::new()
                .ok(A.0, 0.10, 0.20)
                .ok(B.0, 0.10, 0.20);
            let tun = Tunables {
                monitor_recovery_m: 1,
                ..tunables(95, 80, 0)
            };
            let (mut daemon, _dir) = meter_daemon(&secrets, &[A, B], poller, &tun).await;
            daemon.state.active = Some(0);
            daemon.state.health[0].quarantined = true; // dead, now being re-probed
            let outcome = daemon.tick().await;
            assert_eq!(outcome.action, TickAction::Held);
            harvest_channels(&outcome, &mut corpus);
        }

        // Channel — the offline `list` roster view (label · uuid · stash).
        let roster: Vec<Account> = [A, B, C]
            .iter()
            .map(|(uuid, label)| account(uuid, &format!("Sessiometer/{uuid}"), label))
            .collect();
        corpus.push_str(&crate::cli::render_roster(&roster));

        // Channel — the UDS error replies (malformed request / unknown command).
        corpus.push_str(&control_reply("not json", &StatusSnapshot::default()));
        corpus.push('\n');
        corpus.push_str(&control_reply(
            r#"{"cmd":"nope"}"#,
            &StatusSnapshot::default(),
        ));
        corpus.push('\n');

        // Channel — every error message Display.
        for err in every_error_variant() {
            corpus.push_str(&err.to_string());
            corpus.push('\n');
        }

        // Cardinality: a gate that passes on an empty/degraded corpus is no
        // evidence (issue #15). Prove every channel actually contributed its
        // expected non-secret content before trusting the clean verdict.
        assert!(
            corpus.contains("event=swap from=work to=spare"),
            "log channel: swap event missing"
        );
        assert!(
            corpus.contains("event=all_exhausted hold=spare"),
            "log channel: all_exhausted event missing"
        );
        assert!(
            corpus.contains("event=monitor_401 account=work"),
            "log channel: 401 event missing"
        );
        assert!(
            corpus.contains("event=keychain_locked_wait"),
            "log channel: keychain-lock event missing"
        );
        assert!(
            corpus.contains("event=usage_scope_fail account=backup"),
            "log channel: 403 event missing"
        );
        // #42 lifecycle channels: the three edge-triggered events plus the durable
        // quarantine status, on both the wire and the rendered text.
        assert!(
            corpus.contains("event=credential_dead account=work"),
            "log channel: credential_dead event missing"
        );
        assert!(
            corpus.contains("event=emergency_swap from=work to=spare"),
            "log channel: emergency_swap event missing"
        );
        assert!(
            corpus.contains("event=credential_restored account=work"),
            "log channel: credential_restored event missing"
        );
        assert!(
            corpus.contains(r#""quarantined":true"#),
            "UDS channel: quarantine status missing"
        );
        assert!(
            corpus.contains("· needs re-login"),
            "status-text channel: quarantine tag missing"
        );
        assert!(
            corpus.contains(r#""session_pct":97"#),
            "UDS channel: status wire missing"
        );
        assert!(
            corpus.contains("· session 97%"),
            "status-text channel missing"
        );
        assert!(
            corpus.contains("swapped: work → spare"),
            "foreground channel: swap report missing"
        );
        assert!(
            corpus.contains("Sessiometer/11111111"),
            "list channel: roster view missing"
        );
        assert!(
            corpus.contains("daemon not running"),
            "error channel missing"
        );
        assert!(
            corpus.len() > 800,
            "corpus implausibly small ({} bytes) — channels not captured",
            corpus.len()
        );

        // The METER: no token prefix, no known token, no blob fingerprint (leading
        // bytes or sha256), no email shape, and no high-entropy run — on ANY of the
        // channels above.
        assert_clean(&corpus, &secrets);
    }

    /// The 0.1.0 "done-when" acceptance, driven end-to-end through the four seams
    /// (the injected `UsageSource` via [`FakeRosterPoller`], [`FakeCredentialStore`],
    /// [`FakeAccountStash`], [`FakeClock`]) so it burns no real quota, touches no
    /// keychain, and runs in zero real time (issue #14). One hermetic run proves the
    /// whole loop that the smaller unit tests cover only in pieces:
    ///
    ///   - **reconcile-on-start (#13):** a deliberate canonical≠oauth mismatch — the
    ///     canonical holds B's token while `~/.claude.json` still DISPLAYS A (a torn
    ///     post-swap crash) — is healed before the first poll.
    ///   - **threshold → pick-viable → swap → propagate:** the active account, over
    ///     its session trigger, swaps to a VIABLE target (never the weekly-exhausted
    ///     distractor C), and the promoted credential propagates to BOTH the canonical
    ///     keychain item AND the `~/.claude.json` display.
    ///   - **B→A→B without oscillation (#10):** with A and B both hovering over the
    ///     trigger, the post-swap cooldown bounds the ping-pong — a re-swap inside the
    ///     window is refused (never A→B→A), and only past the window does the loop swap
    ///     back, completing a B→A→B cycle. No manual step at any point.
    ///   - **every event surfaced (#9) + nothing leaked (#15):** each cycle's output on
    ///     every operator channel (log / status / UDS / error / list) is harvested and
    ///     run through the redaction METER as a CI gate over the whole acceptance flow.
    ///
    /// The documented MANUAL counterpart — the same acceptance against real accounts,
    /// gated on the #16 H0–H3 checks — lives in `build/smoke-test.md`; it is documented,
    /// not run here, so this path stays hermetic and burns no real quota.
    #[tokio::test]
    async fn e2e_acceptance_full_loop_swaps_propagates_and_reconciles_without_oscillation_or_leak()
    {
        use crate::redaction::meter::{assert_clean, Secrets};

        // Low-entropy uuids/labels: only labels reach the harvested channels and only
        // uuids reach the `list` view, so the METER's entropy backstop fires solely on
        // a genuine secret leak, never on this scaffolding (as the meter test above).
        const A: (&str, &str) = ("11111111-1111-1111-1111-111111111111", "work");
        const B: (&str, &str) = ("22222222-2222-2222-2222-222222222222", "spare");
        const C: (&str, &str) = ("33333333-3333-3333-3333-333333333333", "backup");

        // Three DISTINCT secret blobs — distinct so a swap visibly MOVES the canonical
        // token (propagation is observable) and so token↔account resolution stays
        // unambiguous. Each carries `sk-ant-…` bearers the METER would catch on any
        // channel. A reuses the fixture blob (exercising the blob/known-token detectors
        // too); B and C are their own secrets, with C's never reaching the canonical.
        let secrets = Secrets::meter_fixture();
        let email = secrets.email();
        let a_blob = secrets.blob().to_vec();
        let b_blob = br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-SPARE00SECRET00ACCESS00qR7sT3uV5wX9yZ","refreshToken":"sk-ant-ort-SPARE00SECRET00REFRESH00eF6gH8iJ0kL2mN","expiresAt":1782777600}}"#.to_vec();
        let c_blob = br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-BACKUP0SECRET0ACCESS0sV1wY3zA5bC7dE","refreshToken":"sk-ant-ort-BACKUP0SECRET0REFRESH0iK2lM4nO6pQ8rS","expiresAt":1782777600}}"#.to_vec();

        // Roster: A (index 0), B (index 1), C (index 2 — the non-viable distractor).
        let roster: Vec<Account> = [A, B, C]
            .iter()
            .map(|(uuid, label)| account(uuid, &format!("Sessiometer/{uuid}"), label))
            .collect();

        // Each account's stash holds its OWN secret blob + a secret-bearing identity.
        let stash = FakeAccountStash::empty();
        for (id, blob) in [(A, &a_blob), (B, &b_blob), (C, &c_blob)] {
            stash
                .write(
                    &format!("Sessiometer/{}", id.0),
                    &meter_stashed(blob, id.0, email),
                )
                .await
                .unwrap();
        }

        // The canonical item holds B's token — so the active account resolves to B …
        let store = FakeCredentialStore::empty();
        store.write(&cred(&b_blob)).await.unwrap();
        // … while `~/.claude.json` still DISPLAYS A: the deliberate canonical≠oauth
        // mismatch (a torn post-swap crash, #13) that reconcile-on-start must heal.
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join(".claude.json");
        std::fs::write(
            &json,
            format!(
                r#"{{"numStartups":1,"oauthAccount":{{"accountUuid":"{}","emailAddress":"{email}"}}}}"#,
                A.0
            ),
        )
        .unwrap();

        // A and B both hover over the 95 session trigger with low weekly usage, so each
        // is a viable target for the other — the setup that WOULD ping-pong without a
        // cooldown. C is WEEKLY-EXHAUSTED (0.99 ≥ the 0.98 weekly trigger) → never a
        // viable target, so a correct loop must SELECT A or B and EXCLUDE C.
        const C_RESET: i64 = 1_900_000_000; // far future; C is excluded regardless
        let poller = FakeRosterPoller::new()
            .ok(A.0, 0.96, 0.20)
            .ok(B.0, 0.96, 0.20)
            .ok_resets(C.0, 0.50, 0.99, C_RESET);
        // Floor OFF (the #10 default — the cooldown ALONE bounds oscillation); cooldown
        // 100 s; session trigger 95; no jitter, so every draw is deterministic.
        let tun = tunables_floor_off(95, 100);

        let mut daemon: FakeDaemon = Daemon::new(
            roster,
            poller,
            store,
            stash,
            FakeClock::new(Duration::ZERO),
            json.clone(),
            &tun,
        );

        // --- reconcile-on-start: heal the canonical≠oauth mismatch -------------
        assert_eq!(
            displayed_uuid(&json).as_deref(),
            Some(A.0),
            "precondition: the display starts STALE (shows A while the canonical holds B)"
        );
        daemon.reconcile_on_start().await.unwrap();
        assert_eq!(
            displayed_uuid(&json).as_deref(),
            Some(B.0),
            "reconcile must heal the display to the account the canonical actually holds (B)"
        );

        let mut corpus = String::new();

        // --- B → A: the active account (B), over its trigger, swaps to a viable
        // target. C (weekly-exhausted) is excluded; A is selected. The promoted
        // credential propagates to BOTH the canonical item and the display. --------
        let outcome = daemon.tick().await;
        assert_eq!(
            outcome.action,
            TickAction::Swapped { from: 1, to: 0 },
            "B (active, over trigger) must swap to the viable A, never the exhausted C"
        );
        assert!(
            daemon.store.read().await.unwrap().matches(&cred(&a_blob)),
            "propagate: the canonical item now holds A's token"
        );
        assert_eq!(
            displayed_uuid(&json).as_deref(),
            Some(A.0),
            "propagate: the display now shows A"
        );
        assert_eq!(daemon.state.active, Some(0), "the cached active is now A");
        assert!(
            outcome
                .events
                .iter()
                .any(|e| matches!(e, Event::Swap { .. })),
            "the swap must surface a structured event (#9)"
        );
        harvest_channels(&outcome, &mut corpus);

        // --- no oscillation: every tick WITHIN the 100 s cooldown is refused, even
        // though A is now active and ALSO over the trigger — so never an A→B→A. ----
        for offset in [20u64, 40, 60, 80] {
            daemon.clock.advance(Duration::from_secs(20));
            let outcome = daemon.tick().await;
            assert_eq!(
                outcome.action,
                TickAction::SkippedCooldown,
                "a re-swap at +{offset}s (inside the 100 s cooldown) must be refused"
            );
            assert!(
                daemon.store.read().await.unwrap().matches(&cred(&a_blob)),
                "no oscillation: the canonical still holds A's token inside the window"
            );
            harvest_channels(&outcome, &mut corpus);
        }

        // --- A → B: past the cooldown the swap-back is allowed, completing the
        // B→A→B cycle — oscillation is BOUNDED by the cooldown, not frozen. --------
        daemon.clock.advance(Duration::from_secs(40)); // now at +120 s, past the window
        let outcome = daemon.tick().await;
        assert_eq!(
            outcome.action,
            TickAction::Swapped { from: 0, to: 1 },
            "past the cooldown A (active, over trigger) swaps back to the viable B"
        );
        assert!(
            daemon.store.read().await.unwrap().matches(&cred(&b_blob)),
            "propagate: the canonical item holds B's token again"
        );
        assert_eq!(
            displayed_uuid(&json).as_deref(),
            Some(B.0),
            "propagate: the display shows B again — a full B→A→B cycle"
        );
        assert_eq!(daemon.state.active, Some(1), "the cached active is B again");
        harvest_channels(&outcome, &mut corpus);

        // --- the remaining operator channels: the offline `list` view, the UDS error
        // replies, and every Error Display — all secret-free by construction. -------
        corpus.push_str(&crate::cli::render_roster(&[
            account(A.0, &format!("Sessiometer/{}", A.0), A.1),
            account(B.0, &format!("Sessiometer/{}", B.0), B.1),
            account(C.0, &format!("Sessiometer/{}", C.0), C.1),
        ]));
        corpus.push('\n');
        corpus.push_str(&control_reply("not json", &StatusSnapshot::default()));
        corpus.push('\n');
        corpus.push_str(&control_reply(
            r#"{"cmd":"nope"}"#,
            &StatusSnapshot::default(),
        ));
        corpus.push('\n');
        for err in every_error_variant() {
            corpus.push_str(&err.to_string());
            corpus.push('\n');
        }

        // Cardinality (issue #15): a gate that passes on an empty corpus is no
        // evidence. Prove the loop actually surfaced each swap on a real channel before
        // trusting the clean verdict.
        assert!(
            corpus.contains("event=swap from=spare to=work"),
            "log channel: the B→A swap event is missing"
        );
        assert!(
            corpus.contains("event=swap from=work to=spare"),
            "log channel: the A→B swap-back event is missing"
        );
        assert!(
            corpus.contains(r#""session_pct":96"#),
            "UDS channel: the status wire is missing"
        );
        assert!(
            corpus.contains("· session 96%"),
            "status-text channel is missing"
        );
        assert!(
            corpus.contains("swapped: spare → work"),
            "foreground channel: the B→A swap report is missing"
        );
        assert!(
            corpus.len() > 800,
            "corpus implausibly small ({} bytes) — channels not captured",
            corpus.len()
        );

        // The METER gate (#15): no token prefix, known token, blob fingerprint (leading
        // bytes or sha256), email shape, or high-entropy run leaked onto ANY channel
        // across the whole acceptance loop.
        assert_clean(&corpus, &secrets);
    }
}
