//! Shared boundary contract between [`crate::daemon`] and [`crate::refresh_tick`].
//!
//! These types are the seam the two modules speak across: the time [`Clock`] and the
//! periodic-refresh [`RefreshTicker`] the daemon's run loop drives, plus the per-sweep data
//! ([`SweepOutcome`], [`RefreshObservation`], [`RefreshDelta`]) the ticker hands back. Housing
//! them here — rather than inside `daemon` — lets `refresh_tick` depend on the contract WITHOUT
//! depending on the whole daemon, untangling the `daemon ↔ refresh_tick` dependency cycle
//! (issue #202; the enabling step for the #195 per-concern decomposition). The module depends
//! only on [`crate::observability`] and `std` / `tokio`, never on `daemon` or `refresh_tick`, so
//! it is a leaf both build on. `daemon` re-exports these under `crate::daemon::*` for its own
//! callers, so relocating them is source-compatible for every existing consumer.

use std::time::{Duration, Instant};

use crate::observability::{Event, RefreshEventOutcome};

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

/// Periodic-refresh seam (issue #105): the run loop drives the in-daemon isolated-refresh
/// tick from its idle path, off the poll→usage→swap seam. The production impl
/// ([`crate::refresh_tick::RefreshTick`]) keeps PARKED accounts' stored tokens fresh through
/// the #102 engine — and is wholly inert when the feature is off: its `until_due` never
/// resolves, so a feature-off daemon (or a hermetic test wired with a no-op ticker) behaves
/// exactly as it did before #105.
///
/// Two methods so the run loop can serve the control socket WHILE waiting for the tick to
/// fall due, yet protect an in-flight sweep from being cancelled by a control read (only
/// shutdown interrupts a sweep): [`until_due`](RefreshTicker::until_due) is the wait;
/// [`sweep`](RefreshTicker::sweep) is the bounded work.
pub(crate) trait RefreshTicker {
    /// Whether the tick currently has #106 RESTORE work (issue #280): ≥1 account THIS sweep would
    /// actually refresh for the restore path — quarantined (in the daemon's `quarantined` set), NOT
    /// in `excluded` (the active account + imminent swap target), AND within the refresh allowlist.
    /// It is the EXACT per-account predicate [`sweep`](RefreshTicker::sweep) gates on, evaluated by
    /// the ticker (which owns the allowlist) and kept in one place so the two cannot drift — so a
    /// quarantined account the sweep would SKIP (an excluded active/target, or one outside a
    /// configured allowlist) never raises a prompt for a restore that would not happen. The run
    /// loop threads the result into [`until_due`](RefreshTicker::until_due). `false` when disabled.
    fn recovery_pending(&self, excluded: &[String], quarantined: &[String]) -> bool;
    /// Resolve when a refresh sweep is due (the ticker's own cadence/idle gating, on its own
    /// [`Clock`] seam). MUST never resolve when the feature is disabled, so it never wins the
    /// idle select and adds no clock activity. Re-armable: the run loop awaits it afresh each
    /// idle iteration, and a control read between waits simply restarts it.
    ///
    /// `has_recovery_work` is the ticker's own [`recovery_pending`](RefreshTicker::recovery_pending)
    /// verdict, gated by the run loop to fire at most once per idle period (issue #280). When set,
    /// the ticker becomes due within a short bounded interval (the idle floor) instead of deferring
    /// the restore up to a full refresh cadence after an unrelated recent sweep. The run loop
    /// passes it TRUE only until the current idle period's sweep has run, so the prompt fires at
    /// most once per idle period (poll cadence) — never the sub-poll retry storm ADR-0007 decided
    /// against.
    async fn until_due(&mut self, has_recovery_work: bool);
    /// Run ONE refresh sweep over the due parked accounts, EXCLUDING the `excluded` uuids
    /// (the active account + the imminent swap target the daemon supplies). `quarantined` is
    /// the daemon's currently-dead ("needs re-login") set: those accounts are refreshed even
    /// when not near expiry, and a successful one is reported for RESTORE (issue #106).
    /// Records the sweep for cadence gating. Per-account failures are non-fatal (the engine
    /// Caller contract). Returns the per-cycle [`SweepOutcome`] for the daemon to emit + apply.
    async fn sweep(&mut self, excluded: &[String], quarantined: &[String]) -> SweepOutcome;
}

/// What one [`RefreshTick::sweep`](RefreshTicker::sweep) produced (issue #106): the
/// per-cycle [`Event::Refresh`] log lines, plus the `account_uuid`s of QUARANTINED
/// accounts whose refresh succeeded and so should be RESTORED to eligible.
///
/// Both are handed back to the daemon (which owns the event log and the health machine)
/// rather than acted on here: the tick is a hermetic seam with no `EventLog` handle and
/// no view of the quarantine state. The daemon emits the events and applies the restores
/// ([`crate::daemon`]'s run loop) — keeping each `restored` flip paired with its
/// [`Event::CredentialRestored`] in the one place that owns the health machine.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct SweepOutcome {
    /// One [`Event::Refresh`] per refreshed account, in sweep order.
    pub(crate) events: Vec<Event>,
    /// `account_uuid`s of quarantined accounts the cycle proved still refreshable.
    pub(crate) restored: Vec<String>,
    /// One [`RefreshObservation`] per account the sweep READ this cycle (issue #119) —
    /// the credential clocks the daemon folds into its per-account health state for the
    /// `status` rollup. Recorded for EVERY non-excluded, allowlisted account whose stash
    /// the sweep touched (so a healthy far-from-expiry account still surfaces its expiry
    /// clock), with the refresh-health delta present only on the ones actually refreshed.
    pub(crate) observations: Vec<RefreshObservation>,
}

/// One account's credential-clock observation from a sweep (issue #119): the stored
/// access-token expiry the sweep read, plus — only when the account was actually
/// refreshed this cycle — the refresh-health delta. The daemon folds these into its
/// per-account health state ([`crate::daemon`]) for the `status` 4-state rollup; every
/// field is non-secret (a timestamp, a classification, a boolean — never a token / email).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RefreshObservation {
    /// The account, keyed by `account_uuid` (the daemon resolves it to a roster slot) —
    /// the same resolution key `restored` uses; never the email or a token.
    pub(crate) account_uuid: String,
    /// The stored access-token `expiresAt` (epoch MS, CC's native unit) the sweep read
    /// this cycle, or `None` when the stash was unreadable. The daemon converts to epoch
    /// seconds at the fold boundary.
    pub(crate) expires_at_ms: Option<i64>,
    /// The refresh-health delta — `Some` ONLY when this cycle actually ran a refresh (a
    /// near-expiry or quarantined account); `None` when the sweep merely READ the
    /// account's expiry without refreshing it (a healthy, far-from-expiry account).
    pub(crate) refresh: Option<RefreshDelta>,
}

/// The non-secret refresh-health signal from one completed refresh cycle (issue #119):
/// the classification plus whether the refresh token rotated. The expiry slide lives in
/// [`RefreshObservation::expires_at_ms`]; this is the "did it work / did the token value
/// change" half the rollup's at-risk / dead inputs key off.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RefreshDelta {
    /// The cycle's non-secret classification (the same one the [`Event::Refresh`] carries).
    pub(crate) outcome: RefreshEventOutcome,
    /// Whether CC rotated the refresh token value this cycle (the AC-3 durability signal).
    pub(crate) token_rotated: bool,
}
