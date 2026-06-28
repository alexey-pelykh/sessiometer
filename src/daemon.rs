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
//! 3. **Decide and swap.** If the active account's worst dimension is at/above the
//!    swap-away trigger — drawn this cycle from its timing strategy and clamped to
//!    range (issue #38) — pick the freshest viable target (most account-dimension
//!    headroom, [`pick_target`]) and run the out-of-band [`swap::swap`]. A minimal
//!    post-swap cooldown floor (also a per-cycle jittered draw) guards against an
//!    immediate re-swap (the #10 seam).
//!
//! The trigger, the cooldown, and the inter-poll interval are each a
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
//! age) is surfaced here (#8). Sibling work this leaves as seams: the cooldown
//! *policy* (anti-oscillation, #10), the all-exhausted terminal state
//! ([`TickAction::NoViableTarget`], #11), and the structured swap-history
//! event-log (#9).

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::net::UnixListener;
use tokio::signal::unix::{signal, Signal, SignalKind};

use crate::claude_state;
use crate::config::{Account, Tunables};
use crate::error::{Error, Result};
use crate::keychain::{Credential, CredentialStore, RealCredentialStore};
use crate::observability::EventLog;
use crate::stash::{AccountStash, RealAccountStash};
use crate::swap::{self, SwapDecision};
use crate::timing::{SplitMix64, Strategy};
use crate::usage::{CurlTransport, NoopReStashTrigger, RealUsageSource, Usage, UsageSource};

/// Per-cycle clamp bounds for the swap-away trigger draw, in PERCENT — mirrors
/// config's `session_trigger` range so a jittered draw can never escape it.
const TRIGGER_PCT_LO: f64 = 50.0;
const TRIGGER_PCT_HI: f64 = 99.0;
/// Per-cycle clamp bounds for the cooldown draw, in seconds (config range).
const COOLDOWN_SECS_LO: f64 = 0.0;
const COOLDOWN_SECS_HI: f64 = 3600.0;
/// Per-cycle clamp bounds for the poll-interval draw, in seconds (config range).
const POLL_SECS_LO: f64 = 5.0;
const POLL_SECS_HI: f64 = 3600.0;

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
/// [`StashCredentialStore`] for any other.
pub(crate) struct RealRosterPoller {
    stash: RealAccountStash,
    monitor_401_n: u8,
}

impl RealRosterPoller {
    pub(crate) fn new(monitor_401_n: u8) -> Self {
        Self {
            stash: RealAccountStash::new(),
            monitor_401_n,
        }
    }
}

impl RosterPoller for RealRosterPoller {
    async fn poll(&self, account: &Account, active: bool) -> Result<Usage> {
        if active {
            // The active account's token refreshes in place, so the canonical
            // item is the freshest bearer — poll through it.
            RealUsageSource::new(
                CurlTransport::new(RealCredentialStore::new()),
                NoopReStashTrigger,
                self.monitor_401_n,
            )
            .usage()
            .await
        } else {
            // A non-active account is polled with its stashed token — the seam #5
            // anticipated: `CurlTransport` is generic over `CredentialStore`.
            RealUsageSource::new(
                CurlTransport::new(StashCredentialStore {
                    stash: &self.stash,
                    service: account.stash.clone(),
                }),
                NoopReStashTrigger,
                self.monitor_401_n,
            )
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
    /// Active is over the trigger but no other account is a viable target (all
    /// over the floor, or unavailable). The terminal behavior is #11.
    NoViableTarget,
    /// The active account could not be identified — poll-only, no swap.
    SkippedActiveUnknown,
    /// The active account's reading was unavailable this cycle (transient / 401 /
    /// unreadable) — never swap on missing data.
    SkippedActiveUnavailable,
    /// Over the trigger but within the post-swap cooldown floor (the #10 seam).
    SkippedCooldown,
    /// A swap was attempted but the engine returned an error; #6 is no-half-swap,
    /// so the state is coherent and the loop retries next cycle.
    SwapFailed,
}

/// The result of one poll iteration.
#[derive(Debug)]
pub(crate) struct TickOutcome {
    /// 1-based sequence number of this poll.
    pub(crate) tick: u64,
    /// When the reading was taken.
    pub(crate) at: Instant,
    /// What the loop decided to do.
    pub(crate) action: TickAction,
    /// The per-account readings this cycle (for the event log and the socket).
    pub(crate) snapshot: StatusSnapshot,
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

/// Per-loop decision state carried across polls.
#[derive(Default)]
struct DecisionState {
    /// 1-based count of polls taken.
    ticks: u64,
    /// Roster index of the active account, resolved once and updated on each
    /// swap. `None` until first resolved (then the loop polls but never swaps).
    active: Option<usize>,
    /// The last swap performed, or `None` until the first. Drives both the
    /// post-swap cooldown floor (the #10 seam — the minimal #7 guard refuses an
    /// immediate re-swap within `cooldown`; the directional anti-oscillation
    /// policy using `cooldown_secs` lands in #10) and the minimal `last_swap`
    /// shown by `status` (#8).
    last_swap: Option<LastSwap>,
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
    /// Only swap TO an account whose session usage is below this fraction
    /// (`session_floor / 100`).
    session_floor: f64,
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
        Self {
            roster,
            poller,
            store,
            stash,
            clock,
            claude_json,
            trigger_strategy: tunables.trigger_strategy,
            session_floor: f64::from(tunables.session_floor) / 100.0,
            cooldown_strategy: tunables.cooldown_strategy,
            poll_strategy: tunables.poll_strategy,
            rng: SplitMix64::from_entropy(),
            state: DecisionState::default(),
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
    /// Two signals, in order: (1) the canonical token byte-matches a stash — exact
    /// right after a swap wrote a stashed token verbatim; (2) `~/.claude.json`'s
    /// displayed `accountUuid` maps to a roster account — the steady-state signal
    /// once the active account's token has refreshed in place (drifted from its
    /// stash). `None` if neither resolves; the caller then polls but never swaps.
    async fn resolve_active(&self) -> Option<usize> {
        if let Ok(canonical) = self.store.read().await {
            for (i, account) in self.roster.iter().enumerate() {
                if let Ok(stashed) = self.stash.read(&account.stash).await {
                    if stashed.credential.matches(&canonical) {
                        return Some(i);
                    }
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

    /// Run one poll iteration: resolve the active account, poll every roster
    /// account, then decide and (if warranted) swap.
    pub(crate) async fn tick(&mut self) -> TickOutcome {
        self.state.ticks += 1;
        let tick = self.state.ticks;
        let at = self.clock.now();

        // Resolve the active account once; cached and updated on each swap.
        if self.state.active.is_none() {
            self.state.active = self.resolve_active().await;
        }
        let active = self.state.active;

        // Poll every account: the active one via the canonical credential (its
        // token is the freshest), every other via its stash. A failed poll
        // (transient / 401 / unreadable) leaves that account's reading absent — it
        // is simply not a candidate this cycle, and the loop never swaps on
        // missing data.
        let mut readings: Vec<Option<Usage>> = Vec::with_capacity(self.roster.len());
        for i in 0..self.roster.len() {
            let reading = self
                .poller
                .poll(&self.roster[i], active == Some(i))
                .await
                .ok();
            readings.push(reading);
        }

        let action = self.decide_action(at, active, &readings).await;
        let snapshot = self.snapshot(at, active, &readings);
        TickOutcome {
            tick,
            at,
            action,
            snapshot,
        }
    }

    /// Decide what to do about the active account this cycle, performing the swap
    /// if one is warranted. Returns the per-cycle verdict.
    async fn decide_action(
        &mut self,
        at: Instant,
        active: Option<usize>,
        readings: &[Option<Usage>],
    ) -> TickAction {
        // No identifiable active account → poll-only (never swap on an unknown
        // active account: it is missing data about WHO to swap away from).
        let Some(active_idx) = active else {
            return TickAction::SkippedActiveUnknown;
        };
        // The active account's own reading is unavailable (transient / 401 /
        // unreadable) → skip; never swap on missing data.
        let Some(active_usage) = readings[active_idx] else {
            return TickAction::SkippedActiveUnavailable;
        };
        // Draw this cycle's swap-away trigger (issue #38): jittered + clamped to
        // 50..=99 percent, then to a fraction for the decision. Below it → hold.
        let trigger = self
            .trigger_strategy
            .draw(&mut self.rng, TRIGGER_PCT_LO, TRIGGER_PCT_HI)
            / 100.0;
        if swap::decide(&active_usage, trigger) == SwapDecision::Hold {
            return TickAction::Held;
        }
        // Over the trigger. Minimal cooldown floor (the #10 seam): refuse an
        // immediate re-swap within this cycle's (jittered) cooldown of the last.
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
        // Pick the freshest viable target (most account-dimension headroom).
        let Some(target_idx) = pick_target(active_idx, readings, self.session_floor) else {
            // Every other account is over the floor (or unavailable): nothing to
            // swap to. The all-exhausted terminal behavior is #11; here we hold.
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
                self.state.active = Some(target_idx);
                // Record the swap for the cooldown floor and the `status` display
                // (#8); `at` is the monotonic instant, the label is non-secret (#15).
                self.state.last_swap = Some(LastSwap {
                    to: self.roster[target_idx].label.clone(),
                    at,
                });
                TickAction::Swapped {
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
}

/// Pick the freshest viable swap target: among accounts other than `active` whose
/// reading is available and whose session usage is below `floor`, the one with the
/// most account-dimension (weekly) headroom — i.e. the lowest weekly usage —
/// breaking ties by lowest session usage, then roster order. `None` when no
/// account qualifies (the all-exhausted case, #11).
fn pick_target(active: usize, readings: &[Option<Usage>], floor: f64) -> Option<usize> {
    readings
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != active)
        .filter_map(|(i, reading)| reading.map(|usage| (i, usage)))
        .filter(|&(_, usage)| usage.session < floor)
        .min_by(|&(_, a), &(_, b)| {
            a.weekly
                .total_cmp(&b.weekly)
                .then(a.session.total_cmp(&b.session))
        })
        .map(|(i, _)| i)
}

/// The console line for a swap this cycle, or `None` for any non-swap outcome.
/// Surfaced to the operator watching the foreground `run` (issue #8) — the file
/// event log records every cycle separately. Sourced solely from labels, so it
/// can never carry a token or email (issue #15).
fn swap_report(outcome: &TickOutcome) -> Option<String> {
    match outcome.action {
        TickAction::Swapped { from, to } => Some(format!(
            "swapped: {} → {}",
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
        // Best-effort logging: a log write failure must not kill the daemon.
        if let Err(err) = log.record(&outcome) {
            eprintln!("sessiometer: event log write failed: {err}");
        }
        // Echo a swap to the operator watching the foreground process (issue #8).
        // The file event log (above) records every cycle; the console gets just
        // swaps, sourced solely from labels (issue #15).
        if let Some(report) = swap_report(&outcome) {
            eprintln!("sessiometer: {report}");
        }
        // The snapshot the control socket answers from until the next poll.
        let snapshot = outcome.snapshot;

        // Idle until the next poll is due, serving control requests and watching
        // for shutdown. A swap (if any) already completed inside `tick`, so a
        // shutdown observed here aborts cleanly before the next tick — no half-swap.
        let wait = daemon.wait_for_next_poll();
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

    /// Scripts per-account usage keyed by `account_uuid`. An unscripted account, or
    /// one explicitly marked failing, returns a transient error (unavailable).
    struct FakeRosterPoller {
        readings: HashMap<String, Option<Usage>>,
    }

    impl FakeRosterPoller {
        fn new() -> Self {
            Self {
                readings: HashMap::new(),
            }
        }
        fn ok(mut self, uuid: &str, session: f64, weekly: f64) -> Self {
            self.readings
                .insert(uuid.to_owned(), Some(Usage { session, weekly }));
            self
        }
        fn failing(mut self, uuid: &str) -> Self {
            self.readings.insert(uuid.to_owned(), None);
            self
        }
    }

    impl RosterPoller for FakeRosterPoller {
        async fn poll(&self, account: &Account, _active: bool) -> Result<Usage> {
            match self.readings.get(&account.account_uuid) {
                Some(Some(usage)) => Ok(*usage),
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
        }
    }

    fn tunables(trigger: u8, floor: u8, cooldown: u64) -> Tunables {
        Tunables {
            poll_secs: 60,
            cooldown_secs: cooldown,
            session_floor: floor,
            session_trigger: trigger,
            monitor_401_n: 3,
            // Existing daemon tests exercise the fixed (no-jitter) path: each
            // strategy draws its base verbatim, identical to the pre-#38 scalars.
            poll_strategy: Strategy::fixed(60.0),
            trigger_strategy: Strategy::fixed(f64::from(trigger)),
            cooldown_strategy: Strategy::fixed(cooldown as f64),
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

    #[test]
    fn pick_target_chooses_the_lowest_weekly_among_session_viable_accounts() {
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
            }), // index 0 = active (excluded)
            Some(Usage {
                session: 0.50,
                weekly: 0.60,
            }), // viable, weekly 0.60
            Some(Usage {
                session: 0.10,
                weekly: 0.20,
            }), // viable, weekly 0.20 -> winner
            Some(Usage {
                session: 0.85,
                weekly: 0.01,
            }), // session over floor -> not viable
        ];
        assert_eq!(pick_target(0, &readings, 0.80), Some(2));
    }

    #[test]
    fn pick_target_excludes_the_active_account_and_unavailable_readings() {
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
            }),
            None, // unavailable
            Some(Usage {
                session: 0.10,
                weekly: 0.30,
            }),
        ];
        assert_eq!(pick_target(0, &readings, 0.80), Some(2));
    }

    #[test]
    fn pick_target_is_none_when_every_candidate_is_over_the_floor() {
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
            }),
            Some(Usage {
                session: 0.90,
                weekly: 0.10,
            }),
            Some(Usage {
                session: 0.81,
                weekly: 0.10,
            }),
        ];
        assert_eq!(pick_target(0, &readings, 0.80), None);
    }

    #[test]
    fn pick_target_breaks_a_weekly_tie_by_lower_session() {
        let readings = vec![
            Some(Usage {
                session: 0.97,
                weekly: 0.10,
            }),
            Some(Usage {
                session: 0.40,
                weekly: 0.20,
            }), // tie weekly, session 0.40
            Some(Usage {
                session: 0.20,
                weekly: 0.20,
            }), // tie weekly, session 0.20 -> winner
        ];
        assert_eq!(pick_target(0, &readings, 0.80), Some(2));
    }

    // --- tick: decision + swap --------------------------------------------

    #[tokio::test]
    async fn tick_swaps_active_over_trigger_to_the_freshest_viable_target() {
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
        let poller = FakeRosterPoller::new()
            .ok("u-A", 0.97, 0.40) // active: over trigger
            .ok("u-B", 0.10, 0.20) // viable, lowest weekly -> freshest
            .ok("u-C", 0.30, 0.50); // viable, more weekly used
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
        // The canonical item now holds B's token, and the display shows B…
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"B-token")));
        assert_eq!(displayed_uuid(&json).as_deref(), Some("u-B"));
        // …and the in-memory active advanced to B, so the next read polls B.
        assert_eq!(daemon.state.active, Some(1));
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
        assert!(daemon
            .store
            .read()
            .await
            .unwrap()
            .matches(&cred(b"A-token")));
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
                    usage: Some(Usage {
                        session: 0.97,
                        weekly: 0.40,
                    }),
                },
                AccountReading {
                    label: "spare".to_owned(),
                    active: false,
                    usage: None,
                },
            ],
            last_swap: None,
        };
        let json = serde_json::to_string(&status_response(&snapshot)).unwrap();
        assert!(json.contains("\"label\":\"work\""));
        assert!(json.contains("\"active\":true"));
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
                usage: Some(Usage {
                    session: 0.50,
                    weekly: 0.25,
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
                    usage: None,
                },
                AccountReading {
                    label: "spare".to_owned(),
                    active: true,
                    usage: None,
                },
            ],
            last_swap: None,
        };
        let outcome = |action| TickOutcome {
            tick: 1,
            at: Instant::now(),
            action,
            snapshot: snapshot.clone(),
        };
        assert_eq!(
            swap_report(&outcome(TickAction::Swapped { from: 0, to: 1 })).as_deref(),
            Some("swapped: work → spare"),
        );
        assert_eq!(swap_report(&outcome(TickAction::Held)), None);
        assert_eq!(swap_report(&outcome(TickAction::SkippedCooldown)), None);
        assert_eq!(swap_report(&outcome(TickAction::NoViableTarget)), None);
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
        let mut log = EventLog::at(&logdir.path().join("events.log")).unwrap();
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
        let mut log = EventLog::at(&logdir.path().join("events.log")).unwrap();
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
    }
}
