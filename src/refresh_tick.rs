// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The periodic isolated-refresh tick (issue #105) — the **second** thin caller of the
//! #102 refresh engine, after the one-shot `poke` (#104). Where `poke` is an operator
//! command, this runs INSIDE the daemon's `run` loop, in its idle path (off the
//! poll→usage→swap seam), keeping PARKED accounts' stored tokens fresh on a cadence so a
//! spare is ready to swap to without a stale-token round-trip.
//!
//! ## What it does each cycle
//!
//! Between poll ticks, once the daemon has been idle for `idle_after_secs` AND a full
//! `cadence_secs` has elapsed since the last refresh, it sweeps the roster for *due*
//! accounts and runs one isolated-refresh cycle per account through the engine
//! ([`crate::refresh::refresh_account`]). An account is **due** when:
//!   - it is not excluded (the daemon passes the active account + the imminent swap target
//!     — the engine Caller contract's "parked only"; the swap lock the engine holds covers
//!     the mid-swap case), and
//!   - it is in the configured `accounts` allowlist (empty = all parked accounts), and
//!   - its stored token would expire within one `cadence_secs` of now — i.e. it would not
//!     survive until the next tick. **The cadence IS the near-expiry horizon** (#104 left
//!     the all-accounts horizon provisional for #105 to own); deriving it from the cadence
//!     keeps the threshold configurable and TTL-aware without a second knob.
//!
//! ## Honoring the engine Caller contract
//!
//!   - **Parked only.** The daemon-supplied exclusion set removes the active account and
//!     the imminent swap target before selection; the swap lock the engine acquires around
//!     its stash read + re-stash enforces the mid-swap case.
//!   - **A refresh `Err` is non-fatal.** A per-account error (a contended lock, a wedged
//!     keychain, a spawn failure) — or a whole-cycle timeout — is logged (redacted to the
//!     label + classification, issue #15) and the sweep moves on; the dead-credential
//!     recovery path (#13/#42) heals a forfeited token. One account's failure never aborts
//!     the rest, and a refresh never touches the live session's canonical credential.
//!
//! ## Zero effect on the live session
//!
//! Every refresh happens in an isolated `CLAUDE_CONFIG_DIR` with its own keychain item
//! (the engine's whole design, #102); the `Claude Code-credentials` item a live session
//! reads is never written here. The tick runs in the idle path only — never concurrently
//! with the poll→usage→swap tick — and a wedged cycle is bounded by `timeout_secs` so it
//! can never stall the daemon's return to polling.
//!
//! The selection → refresh flow is generic over a [`RefreshEngine`] seam (mirroring `poke`'s
//! `PokeEngine`) and a [`Clock`] seam, so the whole tick runs hermetically against in-memory
//! fakes in tests; production wires [`RealRefreshEngine`] + [`crate::contract::RealClock`].

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::{Account, RefreshConfig};
use crate::contract::{Clock, RefreshDelta, RefreshObservation, RefreshTicker, SweepOutcome};
use crate::error::Result;
use crate::observability::{Event, RefreshEventOutcome};
use crate::refresh::{self, RefreshOutcome, RefreshReport};
use crate::stash::RealAccountStash;

/// The per-account isolated-refresh operations [`RefreshTick`] drives, injected as a seam so
/// the whole selection → refresh flow runs hermetically against an in-memory fake in tests —
/// exactly as `poke`'s `PokeEngine` and `use`'s swap seams. The production implementation is
/// [`RealRefreshEngine`].
pub(crate) trait RefreshEngine {
    /// The stored credential's `expiresAt` (epoch ms) for `account`, or `None` if it is
    /// unreadable — drives the near-expiry filter. Non-secret (only the timestamp).
    async fn stored_expires_at(&self, account: &Account) -> Option<i64>;
    /// Run one isolated-refresh cycle for `account` (the #102 engine).
    async fn refresh(&self, account: &Account) -> Result<RefreshReport>;
}

/// The production [`RefreshEngine`]: the real keychain-backed stash plus the resolved
/// `claude` binary, wired straight into the #102 engine entry points
/// ([`refresh::stored_expires_at`], [`refresh::refresh_account`]) — the same wiring `poke`'s
/// `RealPokeEngine` uses.
pub(crate) struct RealRefreshEngine {
    stash: RealAccountStash,
    claude_binary: PathBuf,
}

impl RealRefreshEngine {
    pub(crate) fn new(stash: RealAccountStash, claude_binary: PathBuf) -> Self {
        Self {
            stash,
            claude_binary,
        }
    }
}

impl RefreshEngine for RealRefreshEngine {
    async fn stored_expires_at(&self, account: &Account) -> Option<i64> {
        refresh::stored_expires_at(&self.stash, &account.stash()).await
    }

    async fn refresh(&self, account: &Account) -> Result<RefreshReport> {
        refresh::refresh_account(
            &self.stash,
            &account.stash(),
            &account.account_uuid,
            self.claude_binary.clone(),
        )
        .await
    }
}

/// The periodic refresh tick — the run loop's [`RefreshTicker`] seam (issue #105).
///
/// Owns a copy of the roster (fixed for the daemon's life), the validated [`RefreshConfig`],
/// the engine + clock seams, and the cadence anchor (`last_refresh`). `enabled` is the
/// EFFECTIVE switch: `config.enabled` AND a successfully-resolved `claude` binary (a
/// resolution failure disables the tick with a warning rather than failing the daemon — see
/// [`crate::cli`]). When disabled the ticker is wholly inert: [`until_due`](Self::until_due)
/// never resolves, so the tick adds no clock activity and the idle select behaves exactly as
/// before #105.
pub(crate) struct RefreshTick<E, K> {
    roster: Vec<Account>,
    config: RefreshConfig,
    enabled: bool,
    engine: E,
    clock: K,
    /// When the last sweep ran (this clock's `Instant`), or `None` until the first — the
    /// cadence anchor. `None` makes the first sweep due as soon as the idle floor is met.
    last_refresh: Option<Instant>,
}

impl<E, K> RefreshTick<E, K> {
    /// Build a tick. `enabled` is the effective switch (caller folds in binary resolution).
    pub(crate) fn new(
        roster: Vec<Account>,
        config: RefreshConfig,
        enabled: bool,
        engine: E,
        clock: K,
    ) -> Self {
        Self {
            roster,
            config,
            enabled,
            engine,
            clock,
            last_refresh: None,
        }
    }

    /// How long from `now` until a refresh is permitted: the idle floor (`idle_after`), but
    /// never sooner than a full cadence since the last refresh. With no prior refresh the
    /// cadence term is zero, so the first sweep waits only the idle floor.
    ///
    /// The cadence term is anchored ABSOLUTELY (from `last_refresh`) so control-socket
    /// activity that re-arms this wait cannot let refreshes outrun the cadence; the idle
    /// floor is relative to `now`, so each re-arm restarts the "quiet since last activity"
    /// clock — the intended semantics of `idle_after_secs`.
    fn delay_until_due(&self, now: Instant) -> Duration {
        let cadence_remaining = match self.last_refresh {
            Some(last) => self
                .config
                .cadence()
                .saturating_sub(now.saturating_duration_since(last)),
            None => Duration::ZERO,
        };
        self.config.idle_after().max(cadence_remaining)
    }

    /// Whether `account` is named in the `accounts` allowlist — by `list` label OR
    /// `account_uuid` (the resolution `poke`/`use` key on). Only consulted when the
    /// allowlist is non-empty.
    fn account_listed(&self, account: &Account) -> bool {
        self.config
            .accounts
            .iter()
            .any(|entry| entry == &account.label || entry == &account.account_uuid)
    }
}

impl<E: RefreshEngine, K: Clock> RefreshTick<E, K> {
    /// Sweep the roster and run one isolated-refresh cycle per DUE account (issue #105),
    /// returning the [`SweepOutcome`] the daemon emits + applies (issue #106).
    ///
    /// `excluded` is the daemon-supplied parked-only set (active + imminent swap target
    /// uuids); `quarantined` is the daemon's currently-dead ("needs re-login") set. A
    /// quarantined account BYPASSES the near-expiry filter — the point is to test whether
    /// its refresh token still works, regardless of where its (possibly server-revoked)
    /// stored token sits relative to its timestamp expiry — and a successful refresh of one
    /// is reported in [`SweepOutcome::restored`] for the daemon to un-quarantine. `now_ms`
    /// is the wall clock for the near-expiry horizon. Per-account errors and timeouts are
    /// non-fatal — recorded as an `error` refresh event and stepped past.
    async fn run_sweep(
        &self,
        excluded: &[String],
        quarantined: &[String],
        now_ms: i64,
    ) -> SweepOutcome {
        // The near-expiry horizon = one cadence: refresh anything that would not survive to
        // the next tick. `* 1000` → ms (the unit CC's `expiresAt` uses).
        let horizon_ms = (self.config.cadence_secs as i64).saturating_mul(1000);
        let allowlist = !self.config.accounts.is_empty();
        let mut outcome = SweepOutcome::default();
        for account in &self.roster {
            // Parked only: the daemon excludes the active account + imminent swap target.
            if excluded.iter().any(|uuid| uuid == &account.account_uuid) {
                continue;
            }
            // Allowlist (empty = all parked accounts).
            if allowlist && !self.account_listed(account) {
                continue;
            }
            let is_quarantined = quarantined.iter().any(|uuid| uuid == &account.account_uuid);
            // The stored expiry BEFORE the cycle: the event's `expires_before` AND the
            // near-expiry input — read once here and reused for the event.
            let before_ms = self.engine.stored_expires_at(account).await;
            // Near-expiry within one cadence gates HEALTHY accounts (an unreadable expiry is
            // skipped — a stash the sweep cannot even read is not a routine candidate). A
            // quarantined account is exempt: it is refreshed for the RESTORE check (#106).
            // A read-only (not near-expiry, not quarantined) account still records a #119
            // credential-clock observation — just its expiry, with no refresh-health delta —
            // so a healthy, far-from-expiry account surfaces its expiry clock to the rollup.
            if !is_quarantined && !is_near_expiry(before_ms, now_ms, horizon_ms) {
                outcome.observations.push(RefreshObservation {
                    account_uuid: account.account_uuid.clone(),
                    expires_at_ms: before_ms,
                    refresh: None,
                });
                continue;
            }
            // One whole-cycle, timeout-bounded refresh. Every terminal state is non-fatal
            // (engine Caller contract); the event is redacted to the handle + classification
            // + the non-secret before/after expiry (issue #106, via the single #15 surface).
            // The same report also yields the #119 observation: the post-cycle expiry plus
            // the refresh-health delta (classification + token-rotation) the rollup keys off.
            let (event, observation) =
                match tokio::time::timeout(self.config.timeout(), self.engine.refresh(account))
                    .await
                {
                    Ok(Ok(report)) => {
                        // RESTORE a quarantined account ONLY when THIS cycle persisted the fresh
                        // token (`Refreshed` AND `re_stashed`): then the canonical demonstrably
                        // holds a token we know is good. On a CAS-discarded refresh (`Refreshed`
                        // but not `re_stashed`) a concurrent swap/login changed the stash and is
                        // authoritative — it OWNS the un-quarantine (the #42 poll once it polls
                        // active, or #107's re-login), so we do not second-guess its credential.
                        if is_quarantined
                            && report.outcome == RefreshOutcome::Refreshed
                            && report.re_stashed
                        {
                            outcome.restored.push(account.account_uuid.clone());
                        }
                        let observation = RefreshObservation {
                            account_uuid: account.account_uuid.clone(),
                            // The post-cycle stored expiry (the event's `expires_after`): a
                            // re-stashed refresh slid it forward; every other terminal state
                            // left the stash — and so the expiry — unchanged.
                            expires_at_ms: expires_after(before_ms, &report),
                            refresh: Some(RefreshDelta {
                                outcome: refresh_event_outcome(&report),
                                token_rotated: report.refresh_token_rotated,
                            }),
                        };
                        (
                            refresh_event(&account.label, before_ms, &report),
                            observation,
                        )
                    }
                    // Secret-free: a hard `Err` / a timeout is an `error` outcome. The engine's
                    // error Display is NOT folded into the structured event — only the class is.
                    // The stash is untouched, so the rollup sees a refresh failure (→ at-risk)
                    // with the expiry held at the before, never a misleading slide.
                    Ok(Err(_)) | Err(_) => (
                        error_refresh_event(&account.label, before_ms),
                        RefreshObservation {
                            account_uuid: account.account_uuid.clone(),
                            expires_at_ms: before_ms,
                            refresh: Some(RefreshDelta {
                                outcome: RefreshEventOutcome::Error,
                                token_rotated: false,
                            }),
                        },
                    ),
                };
            outcome.events.push(event);
            outcome.observations.push(observation);
        }
        outcome
    }
}

impl<E: RefreshEngine, K: Clock> RefreshTicker for RefreshTick<E, K> {
    async fn until_due(&mut self) {
        if !self.enabled {
            // Disabled: never become due. This arm therefore never wins the idle select and
            // the ticker touches no clock — the idle loop behaves exactly as pre-#105.
            std::future::pending::<()>().await;
            return;
        }
        let delay = self.delay_until_due(self.clock.now());
        self.clock.tick(delay).await;
    }

    async fn sweep(&mut self, excluded: &[String], quarantined: &[String]) -> SweepOutcome {
        if !self.enabled {
            return SweepOutcome::default();
        }
        let outcome = self.run_sweep(excluded, quarantined, now_ms()).await;
        // Anchor the cadence from the END of the sweep, so a long sweep does not let the
        // next one start early.
        self.last_refresh = Some(self.clock.now());
        outcome
    }
}

/// Whether a stored token is *near expiry*: its `expiresAt` is within `horizon_ms` of
/// `now_ms` (already-expired included). `None` — the expiry could not be read — is NOT
/// near-expiry (the sweep skips a stash it cannot read). Mirrors `poke`'s predicate; kept
/// local so the tick is an independent caller of the engine (no shared mutable surface).
fn is_near_expiry(expires_at_ms: Option<i64>, now_ms: i64, horizon_ms: i64) -> bool {
    match expires_at_ms {
        Some(expires_at) => expires_at <= now_ms.saturating_add(horizon_ms),
        None => false,
    }
}

/// Build the per-cycle [`Event::Refresh`] (issue #106) from a completed cycle's `report`
/// and the stored `before_ms` expiry read before the cycle. The event is the durable,
/// #15-metered replacement for #105's ad-hoc per-cycle `eprintln` — every field is a
/// handle / enum / timestamp, so a secret cannot reach the line.
///
/// `pub(crate)` so the engine's redaction-METER test ([`crate::refresh`]) can scan THIS
/// exact production builder's output over a real-secret cycle — a hand-rolled replica would
/// silently miss a future secret-bearing field added here (issue #106 deliverable 3).
pub(crate) fn refresh_event(label: &str, before_ms: Option<i64>, report: &RefreshReport) -> Event {
    Event::Refresh {
        account: label.to_owned(),
        outcome: refresh_event_outcome(report),
        expires_before: before_ms,
        expires_after: expires_after(before_ms, report),
    }
}

/// The [`Event::Refresh`] for a cycle that did not complete — a hard engine `Err` or a
/// whole-cycle timeout: an `error` outcome with the stored expiry unchanged. The engine's
/// error Display is deliberately NOT folded in — the structured event carries only the
/// non-secret class, and that field discipline is what keeps the channel #15-clean.
fn error_refresh_event(label: &str, before_ms: Option<i64>) -> Event {
    Event::Refresh {
        account: label.to_owned(),
        outcome: RefreshEventOutcome::Error,
        expires_before: before_ms,
        expires_after: before_ms,
    }
}

/// Map a completed cycle's [`RefreshReport`] to the non-secret [`RefreshEventOutcome`]
/// (issue #106) — the classification #105's removed `eprintln` summarized, now folded into
/// the structured event. `Refreshed` splits on whether the CAS re-stash stored the token.
fn refresh_event_outcome(report: &RefreshReport) -> RefreshEventOutcome {
    match report.outcome {
        RefreshOutcome::Refreshed if report.re_stashed => RefreshEventOutcome::Refreshed,
        RefreshOutcome::Refreshed => RefreshEventOutcome::RefreshedNotReStashed,
        RefreshOutcome::NoChange => RefreshEventOutcome::NoChange,
        RefreshOutcome::Dead => RefreshEventOutcome::Dead,
        RefreshOutcome::Error => RefreshEventOutcome::Error,
    }
}

/// The stored token's `expiresAt` AFTER the cycle (epoch ms). ONLY a re-stashed refresh
/// actually moved the stored expiry — by the engine-reported `expires_at_delta_secs` slide;
/// every other terminal state (a refresh the CAS discarded, NoChange, Dead, Error) left the
/// stash untouched, so the after equals the `before`. `None` only when the before was
/// unreadable on a slide (the absolute after cannot then be placed).
fn expires_after(before_ms: Option<i64>, report: &RefreshReport) -> Option<i64> {
    match report.expires_at_delta_secs {
        Some(delta_secs) if report.re_stashed => {
            before_ms.map(|before| before.saturating_add(delta_secs.saturating_mul(1000)))
        }
        _ => before_ms,
    }
}

/// Current wall-clock as epoch milliseconds (the unit CC's `expiresAt` uses). `0` on the
/// pre-1970 impossible case.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    fn acct(label: &str, uuid: &str) -> Account {
        Account {
            account_uuid: uuid.to_owned(),
            label: label.to_owned(),
            enabled: true,
        }
    }

    fn report(outcome: RefreshOutcome, re_stashed: bool) -> RefreshReport {
        RefreshReport {
            outcome,
            expires_at_delta_secs: None,
            refresh_token_rotated: false,
            re_stashed,
        }
    }

    /// A refresh schedule with the given cadence/idle (seconds) and accounts allowlist,
    /// enabled, everything else default.
    fn cfg(cadence_secs: u64, idle_after_secs: u64, accounts: &[&str]) -> RefreshConfig {
        RefreshConfig {
            enabled: true,
            accounts: accounts.iter().map(|s| s.to_string()).collect(),
            cadence_secs,
            idle_after_secs,
            timeout_secs: 90,
            claude_bin: None,
        }
    }

    /// A clock whose `now` is fixed and whose `tick` is a no-op — sufficient for the sweep
    /// (which only reads `now()` to anchor the cadence) and the pure `delay_until_due` math.
    struct FixedClock {
        now: Instant,
    }
    impl Clock for FixedClock {
        fn now(&self) -> Instant {
            self.now
        }
        async fn tick(&self, _interval: Duration) {}
    }

    /// What a faked refresh cycle returns for an account.
    #[derive(Clone, Copy)]
    enum FakeRefresh {
        Report(RefreshReport),
        /// A hard cycle error (the engine's `Err` channel — e.g. a contended lock).
        HardError,
        /// Sleeps past any sane timeout, to exercise the whole-cycle timeout bound.
        Hang,
    }

    /// In-memory [`RefreshEngine`]: canned per-account expiries + refresh results, plus a
    /// record of which accounts (in order) actually had `refresh` called.
    struct FakeEngine {
        expiries: HashMap<String, Option<i64>>,
        results: HashMap<String, FakeRefresh>,
        refreshed: RefCell<Vec<String>>,
    }

    impl FakeEngine {
        fn new() -> Self {
            Self {
                expiries: HashMap::new(),
                results: HashMap::new(),
                refreshed: RefCell::new(Vec::new()),
            }
        }
        fn with_expiry(mut self, uuid: &str, expires_at: Option<i64>) -> Self {
            self.expiries.insert(uuid.to_owned(), expires_at);
            self
        }
        fn with_result(mut self, uuid: &str, result: FakeRefresh) -> Self {
            self.results.insert(uuid.to_owned(), result);
            self
        }
        fn refreshed(&self) -> Vec<String> {
            self.refreshed.borrow().clone()
        }
    }

    impl RefreshEngine for FakeEngine {
        async fn stored_expires_at(&self, account: &Account) -> Option<i64> {
            self.expiries.get(&account.account_uuid).copied().flatten()
        }
        async fn refresh(&self, account: &Account) -> Result<RefreshReport> {
            self.refreshed
                .borrow_mut()
                .push(account.account_uuid.clone());
            match self.results.get(&account.account_uuid) {
                Some(FakeRefresh::Report(r)) => Ok(*r),
                Some(FakeRefresh::HardError) => Err(crate::error::Error::SwapLockBusy),
                Some(FakeRefresh::Hang) => {
                    tokio::time::sleep(Duration::from_secs(10_000)).await;
                    Ok(report(RefreshOutcome::Refreshed, true))
                }
                None => Ok(report(RefreshOutcome::NoChange, false)),
            }
        }
    }

    fn tick(
        roster: Vec<Account>,
        config: RefreshConfig,
        engine: FakeEngine,
    ) -> RefreshTick<FakeEngine, FixedClock> {
        RefreshTick::new(
            roster,
            config,
            true,
            engine,
            FixedClock {
                now: Instant::now(),
            },
        )
    }

    // --- is_near_expiry / account_listed (pure) -----------------------------

    #[test]
    fn near_expiry_includes_within_horizon_and_already_expired() {
        let now = 1_000_000;
        let horizon = 3_600_000; // 1h in ms
        assert!(is_near_expiry(Some(now + 1_800_000), now, horizon)); // 30 min out
        assert!(is_near_expiry(Some(now - 1), now, horizon)); // already expired
        assert!(is_near_expiry(Some(now + horizon), now, horizon)); // boundary (<=)
        assert!(!is_near_expiry(Some(now + 7_200_000), now, horizon)); // 2h out
        assert!(!is_near_expiry(None, now, horizon)); // unreadable
    }

    // --- delay_until_due (pure cadence/idle gating) -------------------------

    #[test]
    fn first_refresh_waits_only_the_idle_floor() {
        // No prior refresh → the cadence term is zero, so the wait is the idle floor.
        let t = tick(vec![], cfg(3600, 60, &[]), FakeEngine::new());
        assert_eq!(t.delay_until_due(t.clock.now()), Duration::from_secs(60));
    }

    #[test]
    fn cadence_dominates_right_after_a_refresh() {
        let base = Instant::now();
        let mut t = tick(vec![], cfg(3600, 60, &[]), FakeEngine::new());
        t.last_refresh = Some(base);
        // 100 s after a refresh: ~3500 s of cadence remain, well above the 60 s idle floor.
        let delay = t.delay_until_due(base + Duration::from_secs(100));
        assert_eq!(delay, Duration::from_secs(3500));
    }

    #[test]
    fn idle_floor_dominates_once_the_cadence_has_elapsed() {
        let base = Instant::now();
        let mut t = tick(vec![], cfg(3600, 60, &[]), FakeEngine::new());
        t.last_refresh = Some(base);
        // Two hours later the cadence is long satisfied → only the idle floor remains.
        let delay = t.delay_until_due(base + Duration::from_secs(7200));
        assert_eq!(delay, Duration::from_secs(60));
    }

    // --- sweep selection ----------------------------------------------------

    #[tokio::test]
    async fn sweep_refreshes_only_parked_near_expiry_accounts() {
        let now_ms = now_ms();
        let soon = now_ms + 60_000; // within the 1h cadence horizon
        let later = now_ms + 24 * 3_600_000; // a day out — beyond it
        let roster = vec![
            acct("active", "u-A"),
            acct("near", "u-B"),
            acct("fresh", "u-C"),
        ];
        let engine = FakeEngine::new()
            .with_expiry("u-A", Some(soon)) // active, but excluded below
            .with_expiry("u-B", Some(soon))
            .with_expiry("u-C", Some(later));
        let mut t = tick(roster, cfg(3600, 60, &[]), engine);
        // The daemon excludes the active account u-A.
        t.sweep(&["u-A".to_owned()], &[]).await;
        assert_eq!(t.engine.refreshed(), vec!["u-B"]);
        // The cadence anchor advances after a sweep.
        assert!(t.last_refresh.is_some());
    }

    #[tokio::test]
    async fn sweep_honors_the_accounts_allowlist() {
        let now_ms = now_ms();
        let soon = now_ms + 60_000;
        let roster = vec![
            acct("work", "u-A"),
            acct("spare", "u-B"),
            acct("other", "u-C"),
        ];
        let engine = FakeEngine::new()
            .with_expiry("u-A", Some(soon))
            .with_expiry("u-B", Some(soon))
            .with_expiry("u-C", Some(soon));
        // Allowlist only "spare" (by label) and u-C (by uuid); all are near-expiry & parked.
        let mut t = tick(roster, cfg(3600, 60, &["spare", "u-C"]), engine);
        t.sweep(&[], &[]).await;
        assert_eq!(t.engine.refreshed(), vec!["u-B", "u-C"]);
    }

    #[tokio::test]
    async fn sweep_excludes_the_imminent_swap_target_too() {
        let now_ms = now_ms();
        let soon = now_ms + 60_000;
        let roster = vec![
            acct("active", "u-A"),
            acct("target", "u-B"),
            acct("parked", "u-C"),
        ];
        let engine = FakeEngine::new()
            .with_expiry("u-B", Some(soon))
            .with_expiry("u-C", Some(soon));
        let mut t = tick(roster, cfg(3600, 60, &[]), engine);
        // Daemon excludes BOTH the active account AND the imminent swap target (u-B).
        t.sweep(&["u-A".to_owned(), "u-B".to_owned()], &[]).await;
        assert_eq!(t.engine.refreshed(), vec!["u-C"]);
    }

    #[tokio::test]
    async fn sweep_continues_past_a_per_account_error() {
        let now_ms = now_ms();
        let soon = now_ms + 60_000;
        let roster = vec![acct("a", "u-A"), acct("b", "u-B")];
        let engine = FakeEngine::new()
            .with_expiry("u-A", Some(soon))
            .with_expiry("u-B", Some(soon))
            .with_result("u-A", FakeRefresh::HardError) // first errors hard…
            .with_result(
                "u-B",
                FakeRefresh::Report(report(RefreshOutcome::Refreshed, true)),
            );
        let mut t = tick(roster, cfg(3600, 60, &[]), engine);
        t.sweep(&[], &[]).await; // …the sweep must still reach the second.
        assert_eq!(t.engine.refreshed(), vec!["u-A", "u-B"]);
    }

    #[tokio::test(start_paused = true)]
    async fn sweep_bounds_a_hung_cycle_by_the_timeout_and_continues() {
        let now_ms = now_ms();
        let soon = now_ms + 60_000;
        let roster = vec![acct("hang", "u-A"), acct("ok", "u-B")];
        let engine = FakeEngine::new()
            .with_expiry("u-A", Some(soon))
            .with_expiry("u-B", Some(soon))
            .with_result("u-A", FakeRefresh::Hang) // sleeps far past the timeout…
            .with_result(
                "u-B",
                FakeRefresh::Report(report(RefreshOutcome::Refreshed, true)),
            );
        // timeout_secs = 5; the hang sleeps 10_000 s, so the bound fires (auto-advanced).
        let mut config = cfg(3600, 60, &[]);
        config.timeout_secs = 5;
        let mut t = tick(roster, config, engine);
        t.sweep(&[], &[]).await;
        // The hung account was attempted then timed out; the sweep still reached u-B.
        assert_eq!(t.engine.refreshed(), vec!["u-A", "u-B"]);
    }

    #[tokio::test]
    async fn disabled_sweep_is_a_no_op() {
        let now_ms = now_ms();
        let soon = now_ms + 60_000;
        let roster = vec![acct("near", "u-A")];
        let engine = FakeEngine::new().with_expiry("u-A", Some(soon));
        let mut t = RefreshTick::new(
            roster,
            cfg(3600, 60, &[]),
            false, // disabled
            engine,
            FixedClock {
                now: Instant::now(),
            },
        );
        t.sweep(&[], &[]).await;
        assert!(t.engine.refreshed().is_empty());
        assert!(t.last_refresh.is_none());
    }

    // --- refresh events (issue #106) ---------------------------------------

    #[tokio::test]
    async fn sweep_emits_a_refresh_event_per_cycle_with_before_and_after() {
        // A successful, re-stashed refresh: the event carries the handle, the `refreshed`
        // outcome, and the before/after expiry — after = before + the engine's slide delta.
        let now_ms = now_ms();
        let soon = now_ms + 60_000;
        let roster = vec![acct("work", "u-A")];
        let engine = FakeEngine::new()
            .with_expiry("u-A", Some(soon))
            .with_result(
                "u-A",
                FakeRefresh::Report(RefreshReport {
                    outcome: RefreshOutcome::Refreshed,
                    expires_at_delta_secs: Some(7200), // +2h slide
                    refresh_token_rotated: false,
                    re_stashed: true,
                }),
            );
        let mut t = tick(roster, cfg(3600, 60, &[]), engine);
        let outcome = t.sweep(&[], &[]).await;
        assert_eq!(
            outcome.events,
            vec![Event::Refresh {
                account: "work".to_owned(),
                outcome: RefreshEventOutcome::Refreshed,
                expires_before: Some(soon),
                expires_after: Some(soon + 7_200_000), // before + 7200 s in ms
            }]
        );
        assert!(
            outcome.restored.is_empty(),
            "a healthy account is not a restore"
        );
    }

    #[tokio::test]
    async fn sweep_event_records_a_cas_discarded_refresh_without_moving_the_expiry() {
        // Refreshed but NOT re-stashed (a concurrent change took precedence): the outcome
        // distinguishes it, and `expires_after` stays at `before` (this cycle stored nothing).
        let now_ms = now_ms();
        let soon = now_ms + 60_000;
        let roster = vec![acct("work", "u-A")];
        let engine = FakeEngine::new()
            .with_expiry("u-A", Some(soon))
            .with_result(
                "u-A",
                FakeRefresh::Report(RefreshReport {
                    outcome: RefreshOutcome::Refreshed,
                    expires_at_delta_secs: Some(7200),
                    refresh_token_rotated: false,
                    re_stashed: false,
                }),
            );
        let mut t = tick(roster, cfg(3600, 60, &[]), engine);
        let outcome = t.sweep(&[], &[]).await;
        assert_eq!(
            outcome.events,
            vec![Event::Refresh {
                account: "work".to_owned(),
                outcome: RefreshEventOutcome::RefreshedNotReStashed,
                expires_before: Some(soon),
                expires_after: Some(soon), // unchanged — the CAS discarded the fresh token
            }]
        );
    }

    #[tokio::test]
    async fn sweep_records_an_error_event_for_a_hard_failure() {
        // A hard engine `Err` is an `error` event with the stored expiry unchanged — the
        // error Display never reaches the structured event (only the class does).
        let now_ms = now_ms();
        let soon = now_ms + 60_000;
        let roster = vec![acct("work", "u-A")];
        let engine = FakeEngine::new()
            .with_expiry("u-A", Some(soon))
            .with_result("u-A", FakeRefresh::HardError);
        let mut t = tick(roster, cfg(3600, 60, &[]), engine);
        let outcome = t.sweep(&[], &[]).await;
        assert_eq!(
            outcome.events,
            vec![Event::Refresh {
                account: "work".to_owned(),
                outcome: RefreshEventOutcome::Error,
                expires_before: Some(soon),
                expires_after: Some(soon),
            }]
        );
    }

    // --- credential-clock observations (issue #119) ------------------------

    #[tokio::test]
    async fn sweep_records_a_credential_clock_observation_for_every_account_it_reads() {
        // A sweep surfaces each parked account's credential clocks to the daemon's rollup: a
        // near-expiry account through its refresh (the post-cycle expiry PLUS a refresh-health
        // delta), and a far-from-expiry one READ-ONLY (just its expiry, no delta). The
        // excluded active account is never read, so it records nothing.
        let now_ms = now_ms();
        let soon = now_ms + 60_000; // within the 1h horizon → refreshed
        let later = now_ms + 24 * 3_600_000; // far out → read-only
        let roster = vec![
            acct("active", "u-A"),
            acct("near", "u-B"),
            acct("fresh", "u-C"),
        ];
        let engine = FakeEngine::new()
            .with_expiry("u-A", Some(soon))
            .with_expiry("u-B", Some(soon))
            .with_expiry("u-C", Some(later))
            .with_result(
                "u-B",
                FakeRefresh::Report(RefreshReport {
                    outcome: RefreshOutcome::Refreshed,
                    expires_at_delta_secs: Some(7200), // +2h slide
                    refresh_token_rotated: true,       // the AC-3 durability signal
                    re_stashed: true,
                }),
            );
        let mut t = tick(roster, cfg(3600, 60, &[]), engine);
        let outcome = t.sweep(&["u-A".to_owned()], &[]).await;
        assert_eq!(
            outcome.observations,
            vec![
                // The refreshed account: post-cycle expiry slid forward, plus the delta the
                // rollup keys its alive/at-risk + token-rotation views off.
                RefreshObservation {
                    account_uuid: "u-B".to_owned(),
                    expires_at_ms: Some(soon + 7_200_000),
                    refresh: Some(RefreshDelta {
                        outcome: RefreshEventOutcome::Refreshed,
                        token_rotated: true,
                    }),
                },
                // The read-only account: just its (unchanged) expiry, no refresh-health delta.
                RefreshObservation {
                    account_uuid: "u-C".to_owned(),
                    expires_at_ms: Some(later),
                    refresh: None,
                },
            ]
        );
        // Only the near-expiry account was actually refreshed; the read-only one was not.
        assert_eq!(t.engine.refreshed(), vec!["u-B"]);
    }

    // --- restore-on-success (issue #106 deliverable #2) --------------------

    #[tokio::test]
    async fn sweep_restores_a_quarantined_account_whose_refresh_token_still_works() {
        // A quarantined ("needs re-login") account whose stored expiry is FAR from now — the
        // near-expiry filter would skip a healthy account here — is still refreshed because
        // it is quarantined, and a successful refresh reports it for restore.
        let now_ms = now_ms();
        let far = now_ms + 30 * 24 * 3_600_000; // a month out — NOT near expiry
        let roster = vec![acct("dead", "u-Q")];
        let engine = FakeEngine::new().with_expiry("u-Q", Some(far)).with_result(
            "u-Q",
            FakeRefresh::Report(report(RefreshOutcome::Refreshed, true)),
        );
        let mut t = tick(roster, cfg(3600, 60, &[]), engine);
        let outcome = t.sweep(&[], &["u-Q".to_owned()]).await;
        // Refreshed despite not being near expiry (the quarantine bypass)…
        assert_eq!(t.engine.refreshed(), vec!["u-Q"]);
        // …and reported for restore.
        assert_eq!(outcome.restored, vec!["u-Q".to_owned()]);
    }

    #[tokio::test]
    async fn sweep_does_not_restore_a_quarantined_account_that_stays_dead() {
        // A quarantined account whose refresh token is truly dead is refreshed (the restore
        // attempt) but NOT reported for restore — its event records `dead`.
        let now_ms = now_ms();
        let soon = now_ms + 60_000;
        let roster = vec![acct("dead", "u-Q")];
        let engine = FakeEngine::new()
            .with_expiry("u-Q", Some(soon))
            .with_result(
                "u-Q",
                FakeRefresh::Report(report(RefreshOutcome::Dead, false)),
            );
        let mut t = tick(roster, cfg(3600, 60, &[]), engine);
        let outcome = t.sweep(&[], &["u-Q".to_owned()]).await;
        assert_eq!(t.engine.refreshed(), vec!["u-Q"]);
        assert!(
            outcome.restored.is_empty(),
            "a still-dead account is not restored"
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [Event::Refresh {
                outcome: RefreshEventOutcome::Dead,
                ..
            }]
        ));
    }

    #[tokio::test]
    async fn sweep_does_not_restore_a_quarantined_account_whose_refresh_was_cas_discarded() {
        // Refreshed but NOT re-stashed: a concurrent swap/login changed the stash and is
        // authoritative, so it OWNS the un-quarantine — this cycle must NOT report a restore
        // off a token it did not persist (it could be a concurrently-written dead credential).
        // The event still records the distinct `refreshed_not_restashed` classification.
        let now_ms = now_ms();
        let far = now_ms + 30 * 24 * 3_600_000; // far from expiry — refreshed only via the quarantine bypass
        let roster = vec![acct("dead", "u-Q")];
        let engine = FakeEngine::new().with_expiry("u-Q", Some(far)).with_result(
            "u-Q",
            FakeRefresh::Report(report(RefreshOutcome::Refreshed, false)),
        );
        let mut t = tick(roster, cfg(3600, 60, &[]), engine);
        let outcome = t.sweep(&[], &["u-Q".to_owned()]).await;
        assert_eq!(t.engine.refreshed(), vec!["u-Q"]); // the restore was attempted…
        assert!(
            outcome.restored.is_empty(),
            "a CAS-discarded refresh does not restore — the concurrent writer owns it"
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [Event::Refresh {
                outcome: RefreshEventOutcome::RefreshedNotReStashed,
                ..
            }]
        ));
    }

    #[tokio::test]
    async fn sweep_never_refreshes_an_excluded_account_even_if_quarantined() {
        // The active account can be both excluded AND quarantined; exclusion wins (the engine
        // Caller contract forbids touching the active account) — no refresh, no restore.
        let now_ms = now_ms();
        let soon = now_ms + 60_000;
        let roster = vec![acct("active", "u-A")];
        let engine = FakeEngine::new()
            .with_expiry("u-A", Some(soon))
            .with_result(
                "u-A",
                FakeRefresh::Report(report(RefreshOutcome::Refreshed, true)),
            );
        let mut t = tick(roster, cfg(3600, 60, &[]), engine);
        let outcome = t.sweep(&["u-A".to_owned()], &["u-A".to_owned()]).await;
        assert!(
            t.engine.refreshed().is_empty(),
            "exclusion wins over quarantine"
        );
        assert!(outcome.restored.is_empty());
        assert!(outcome.events.is_empty());
    }
}
