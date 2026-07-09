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
//! Between poll ticks, once the idle floor `idle_after_secs` (anchored absolutely since #260)
//! AND a full `cadence_secs` since the last refresh have both elapsed, it sweeps the roster
//! for *due* accounts and runs one isolated-refresh cycle per account through the engine
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
use crate::observability::{Event, RefreshEventOutcome, RefreshEventReason};
use crate::paths;
use crate::refresh::{self, RefreshErrorReason, RefreshOutcome, RefreshReport};
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

/// The production [`RefreshEngine`]: the real keychain-backed stash plus the
/// `[refresh].claude_bin` override, wired straight into the #102 engine entry points
/// ([`refresh::stored_expires_at`], [`refresh::refresh_account`]) — the same wiring `poke`'s
/// `RealPokeEngine` uses.
///
/// Holds the OVERRIDE, not a resolved path (issue #375): the `claude` binary is resolved PER
/// CYCLE at the spawn site via [`resolve_binary`](Self::resolve_binary), so a symlink / `$PATH`
/// / version change AFTER the daemon started is picked up on the next cycle with no restart.
/// Before #375 `cli` resolved once at startup and froze the `PathBuf` here, so any later change
/// was invisible until a manual restart — and a frozen path that stopped working failed EVERY
/// cycle (the periodic sweep #105 AND the #162 poll-refresh, which share this one engine).
pub(crate) struct RealRefreshEngine {
    stash: RealAccountStash,
    claude_bin: Option<PathBuf>,
}

impl RealRefreshEngine {
    pub(crate) fn new(stash: RealAccountStash, claude_bin: Option<PathBuf>) -> Self {
        Self { stash, claude_bin }
    }

    /// Resolve the `claude` binary to spawn THIS cycle (issue #375). Reuses the UNCHANGED
    /// resolution policy ([`paths::claude_binary_with_override`]: `[refresh].claude_bin` →
    /// `$CLAUDE_BIN` → `$PATH`); only the TIMING moved from once-at-startup to per-cycle, and
    /// WHICH binary is chosen is identical to before — no symlink canonicalization, no
    /// prefer-the-"real"-binary, no validation that the target is the genuine CLI (a wrapper
    /// symlink is spawned as-is). A resolution failure is returned as `Err` for the caller to
    /// treat non-fatally: [`run_sweep`](RefreshTick::run_sweep) records an `error` refresh event
    /// and the #162 poll path lets the 401 stand — both retry next cycle, and the tick is never
    /// permanently disabled.
    fn resolve_binary(&self) -> Result<PathBuf> {
        paths::claude_binary_with_override(self.claude_bin.as_deref())
    }
}

impl RefreshEngine for RealRefreshEngine {
    async fn stored_expires_at(&self, account: &Account) -> Option<i64> {
        refresh::stored_expires_at(&self.stash, &account.stash()).await
    }

    async fn refresh(&self, account: &Account) -> Result<RefreshReport> {
        // Resolve per cycle at the spawn site (issue #375), not from a frozen field — the `?` is
        // the non-fatal path `run_sweep` (and the #162 poll) already handle fail-safe.
        let claude_binary = self.resolve_binary()?;
        refresh::refresh_account(
            &self.stash,
            &account.stash(),
            &account.account_uuid,
            claude_binary,
        )
        .await
    }
}

/// The periodic refresh tick — the run loop's [`RefreshTicker`] seam (issue #105).
///
/// Owns a copy of the roster (fixed for the daemon's life), the validated [`RefreshConfig`],
/// the engine + clock seams, and the cadence anchor (`last_refresh`). `enabled` mirrors the
/// CONFIG `[refresh].enabled` (issue #375): the `claude` binary is resolved PER CYCLE at the
/// engine's spawn site, so the tick is no longer gated on a successful startup resolution — a
/// cycle-time resolution failure is non-fatal (recorded as an `error` refresh event, retried
/// next cycle) and never disables the tick (see [`crate::cli`]). When disabled the ticker is
/// wholly inert: [`until_due`](Self::until_due) never resolves, so the tick adds no clock
/// activity and the idle select behaves exactly as before #105.
pub(crate) struct RefreshTick<E, K> {
    roster: Vec<Account>,
    config: RefreshConfig,
    enabled: bool,
    engine: E,
    clock: K,
    /// When the last sweep ran (this clock's `Instant`), or `None` until the first — the
    /// cadence anchor. `None` makes the first sweep due as soon as the idle floor is met.
    last_refresh: Option<Instant>,
    /// The absolute anchor for the idle floor (issue #260): the `Instant` the current idle
    /// window started. Seeded lazily on the first [`until_due`](RefreshTicker::until_due) of a
    /// window and cleared after each [`sweep`](RefreshTicker::sweep). The idle-floor term of
    /// [`delay_until_due`](Self::delay_until_due) counts down toward `idle_anchor + idle_after`,
    /// so a `until_due` future the run loop RE-CREATES every idle iteration sees a SHRINKING
    /// delay rather than a fresh full floor — the fix for the 15s-login-watch starvation.
    idle_anchor: Option<Instant>,
}

impl<E, K> RefreshTick<E, K> {
    /// Build a tick. `enabled` mirrors the CONFIG `[refresh].enabled` switch (issue #375: the
    /// engine resolves `claude` per cycle, so the caller no longer folds startup resolution in).
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
            idle_anchor: None,
        }
    }

    /// How long from `now` until a refresh is permitted: the idle floor (`idle_after`), but
    /// never sooner than a full cadence since the last refresh. With no prior refresh the
    /// cadence term is zero, so the first sweep waits only the idle floor.
    ///
    /// BOTH terms are anchored ABSOLUTELY (issue #260). The cadence term counts from
    /// `last_refresh`, so control-socket activity that re-arms this wait cannot let refreshes
    /// outrun the cadence. The idle-floor term counts from `idle_anchor` (the start of the
    /// current idle window), NOT from `now`, so the run loop RE-CREATING this wait every idle
    /// iteration cannot reset it: a shorter-cadence sibling wake (the 15s external-login watch,
    /// the poll `wait`) merely re-enters with a larger `now`, SHRINKING the remaining floor
    /// toward zero rather than restarting it at a full `idle_after`. Before #260 the idle floor
    /// was relative to `now`, and the 15s watch re-armed it below 60 s forever — starving the
    /// sweep so it effectively never fired.
    ///
    /// Consequence of the absolute idle anchor: `idle_after` bounds primarily the FIRST sweep
    /// after a (re)start — and, after a sweep, until the anchor ages past it — while steady-state
    /// sweeps then fire on the cadence alone (effectively "sweep once `max(idle_after, cadence)`
    /// has elapsed since the last sweep"). Dropping the per-sweep idle debounce is safe: the
    /// sweep already excludes the active account and the imminent swap target and runs only in
    /// the idle path off the poll→usage→swap seam.
    ///
    /// `has_recovery_work` (issue #280) is the "≥1 quarantined-parked account is waiting for the
    /// #106 restore probe" signal. When set, the CADENCE term is DROPPED and the wait gates ONLY
    /// on the idle floor — so a freshly-quarantined account gets a restore attempt within a short
    /// bounded interval (~the idle floor) rather than sitting dead up to a full `cadence_secs`
    /// after an unrelated recent sweep. The idle floor stays anchored absolutely (as above), so a
    /// steady stream of control-socket reads shrinks it toward zero rather than re-arming it — the
    /// recovery path inherits #260's starvation immunity. This bypass cannot degenerate into the
    /// sub-poll retry storm ADR-0007 decided against: the run loop only signals recovery-due until
    /// THIS idle period's sweep has run, after which the (now-recent) cadence term re-throttles —
    /// so the prompt fires at most once per idle period (poll cadence), even at `idle_after_secs`
    /// = 0 (the post-sweep cadence term blocks a re-fire that would otherwise busy-spin).
    fn delay_until_due(
        &self,
        now: Instant,
        idle_anchor: Instant,
        has_recovery_work: bool,
    ) -> Duration {
        let idle_remaining = self
            .config
            .idle_after()
            .saturating_sub(now.saturating_duration_since(idle_anchor));
        if has_recovery_work {
            return idle_remaining;
        }
        let cadence_remaining = match self.last_refresh {
            Some(last) => self
                .config
                .cadence()
                .saturating_sub(now.saturating_duration_since(last)),
            None => Duration::ZERO,
        };
        idle_remaining.max(cadence_remaining)
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
            let cycle =
                tokio::time::timeout(self.config.timeout(), self.engine.refresh(account)).await;
            // The OUTER `Err` is the whole-cycle timeout bound firing → `reason=timeout` (#377);
            // a hard engine `Err` (`Ok(Err)`) has no secret-free sub-class → no `reason=`.
            // Computed before the match consumes `cycle`.
            let timeout_reason = cycle.is_err().then_some(RefreshEventReason::Timeout);
            let (event, observation) = match cycle {
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
                // Secret-free: a hard `Err` (`Ok(Err)`) or a timeout (`Err`) is an `error`
                // outcome. The engine's error Display is NOT folded into the structured event —
                // only the class, plus the `timeout` reason when the bound fired (#377); a hard
                // `Err` carries no secret-free reason. The stash is untouched, so the rollup sees
                // a refresh failure (→ at-risk) with the expiry held at the before, never a slide.
                Ok(Err(_)) | Err(_) => (
                    error_refresh_event(&account.label, before_ms, timeout_reason),
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
    fn recovery_pending(&self, excluded: &[String], quarantined: &[String]) -> bool {
        if !self.enabled {
            return false;
        }
        // The SAME per-account gate `run_sweep` applies before it reaches the quarantine bypass:
        // parked (not excluded) AND allowlisted. An account that clears both AND is quarantined is
        // one the sweep WOULD refresh for the #106 restore — the only kind worth prompting for
        // (issue #280). Kept in lockstep with `run_sweep` so a quarantined account outside the
        // allowlist, or an excluded dead active/target, never raises a prompt the sweep no-ops.
        let allowlist = !self.config.accounts.is_empty();
        self.roster.iter().any(|account| {
            quarantined.iter().any(|uuid| uuid == &account.account_uuid)
                && !excluded.iter().any(|uuid| uuid == &account.account_uuid)
                && (!allowlist || self.account_listed(account))
        })
    }

    async fn until_due(&mut self, has_recovery_work: bool) {
        if !self.enabled {
            // Disabled: never become due. This arm therefore never wins the idle select and
            // the ticker touches no clock — the idle loop behaves exactly as pre-#105.
            std::future::pending::<()>().await;
            return;
        }
        let now = self.clock.now();
        // Seed the idle-floor anchor on the first re-arm of this idle window; later re-arms reuse
        // it, so a sub-`idle_after` sibling wake (the 15s login watch) shrinks the floor toward
        // zero rather than resetting it to a full `idle_after` — the #260 fix. `has_recovery_work`
        // (issue #280) drops the cadence term so a quarantined-parked account's restore is prompt.
        let anchor = *self.idle_anchor.get_or_insert(now);
        let delay = self.delay_until_due(now, anchor, has_recovery_work);
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
        // Clear the idle-floor anchor so the next idle window re-seeds it (issue #260); until the
        // cadence is nearly elapsed the cadence term dominates the idle floor anyway.
        self.idle_anchor = None;
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
        // The already-computed AC-3 rotation flag, threaded straight through (issue #279).
        refresh_token_rotated: report.refresh_token_rotated,
        // The non-secret error sub-class (issue #377): `Some` iff the completed cycle
        // classified `Error`, mapped from the engine's `RefreshErrorReason`; `None` otherwise.
        reason: refresh_event_reason(report),
    }
}

/// The [`Event::Refresh`] for a cycle that did not complete — a hard engine `Err` or a
/// whole-cycle timeout: an `error` outcome with the stored expiry unchanged. The engine's
/// error Display is deliberately NOT folded in — the structured event carries only the
/// non-secret class, and that field discipline is what keeps the channel #15-clean.
///
/// `reason` is the non-secret `reason=` sub-class (issue #377): `Some(Timeout)` when the
/// whole-cycle timeout bound fired, `None` for a hard engine `Err` (a locked keychain, a
/// contended lock, an FS error, an unresolved binary) — that carries no secret-free class, so
/// it renders a bare `outcome=error`.
fn error_refresh_event(
    label: &str,
    before_ms: Option<i64>,
    reason: Option<RefreshEventReason>,
) -> Event {
    Event::Refresh {
        account: label.to_owned(),
        outcome: RefreshEventOutcome::Error,
        expires_before: before_ms,
        expires_after: before_ms,
        // No completed cycle, so no report to source a rotation from (issue #279): a hard
        // engine `Err` / whole-cycle timeout renders `rotated=false`.
        refresh_token_rotated: false,
        reason,
    }
}

/// Map a completed cycle's [`RefreshReport`] to the non-secret [`RefreshEventOutcome`]
/// (issue #106) — the classification #105's removed `eprintln` summarized, now folded into
/// the structured event. `Refreshed` splits on whether the CAS re-stash stored the token.
///
/// `pub(crate)` so the #162 poll-path refresh ([`crate::daemon`], issue #255) maps its own
/// cycle's report to the SAME event vocabulary through this one function — the periodic sweep
/// and the poll path never drift on how a report becomes an `outcome=` token.
pub(crate) fn refresh_event_outcome(report: &RefreshReport) -> RefreshEventOutcome {
    match report.outcome {
        RefreshOutcome::Refreshed if report.re_stashed => RefreshEventOutcome::Refreshed,
        RefreshOutcome::Refreshed => RefreshEventOutcome::RefreshedNotReStashed,
        RefreshOutcome::NoChange => RefreshEventOutcome::NoChange,
        RefreshOutcome::Dead => RefreshEventOutcome::Dead,
        // The `outcome=` token folds every error sub-cause to `error`; the sub-reason rides the
        // separate `reason=` field (issue #377, via `refresh_event_reason`).
        RefreshOutcome::Error(_) => RefreshEventOutcome::Error,
    }
}

/// Map a completed cycle's [`RefreshReport`] to its non-secret `reason=` sub-class (issue #377),
/// or `None` for any non-error outcome — the event-level [`RefreshEventReason`] mirror of the
/// engine's [`RefreshErrorReason`]. Every arm is explicit (no `_`), exactly like
/// [`refresh_event_outcome`]: a future engine [`RefreshOutcome`] or [`RefreshErrorReason`] variant
/// is then a COMPILE error here, never a silently dropped `reason=`. [`RefreshEventReason::Timeout`]
/// has no arm — it is NOT reachable from a completed report (it is the tick's `timeout` bound,
/// supplied directly at the error arm of the sweep).
fn refresh_event_reason(report: &RefreshReport) -> Option<RefreshEventReason> {
    let reason = match report.outcome {
        RefreshOutcome::Error(reason) => reason,
        RefreshOutcome::Refreshed | RefreshOutcome::NoChange | RefreshOutcome::Dead => {
            return None;
        }
    };
    Some(match reason {
        RefreshErrorReason::SpawnFailed => RefreshEventReason::SpawnFailed,
        RefreshErrorReason::ReadbackUnreadable => RefreshEventReason::ReadbackUnreadable,
        RefreshErrorReason::Malformed => RefreshEventReason::Malformed,
    })
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
            systemic_failure_n: 3,
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

    /// A [`Clock`] on tokio's VIRTUAL timeline (issue #260 regression harness): `now` reads a
    /// `tokio::time::Instant` — which `#[tokio::test(start_paused = true)]` advances in lockstep
    /// with virtual sleeps — bridged to `std::time::Instant` via `into_std`. This is the load-
    /// bearing difference from [`crate::contract::RealClock`], whose `now` reads
    /// `std::time::Instant::now()` that `pause()` does NOT freeze-advance: under a paused runtime
    /// a `RealClock` would report a frozen `now`, so the anchored idle floor would never shrink
    /// and the race below would falsely starve. Behaviourally identical to `RealClock` in
    /// production (both track wall-clock); they diverge only under a paused test runtime.
    struct TokioClock;
    impl Clock for TokioClock {
        fn now(&self) -> Instant {
            tokio::time::Instant::now().into_std()
        }
        async fn tick(&self, interval: Duration) {
            tokio::time::sleep(interval).await;
        }
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

    // --- RealRefreshEngine per-cycle binary resolution (issue #375) ---------

    #[test]
    fn real_refresh_engine_resolves_the_binary_per_cycle_not_frozen_at_construction() {
        // Issue #375 regression, at the engine that backs BOTH the #105 periodic sweep and the
        // #162 poll-refresh (it impls `RefreshEngine` and, through it, `PollRefresh`). The engine
        // holds the `[refresh].claude_bin` OVERRIDE and resolves the spawn binary PER CYCLE, so a
        // mid-run symlink re-point — a Claude Code auto-update, a version-dir swap — is picked up
        // on the next cycle with no daemon restart. Built ONCE, resolved THREE times across two
        // re-points: the frozen-at-startup design this fixes could only ever return its first
        // result, so the Ok → Err → Ok sequence below is impossible under it.
        let tmp = tempfile::tempdir().unwrap();
        let version_a = tmp.path().join("claude-A");
        let version_b = tmp.path().join("claude-B");
        std::fs::write(&version_a, b"#!/bin/sh\n").unwrap();
        std::fs::write(&version_b, b"#!/bin/sh\n").unwrap();
        // The `claude` symlink an updater re-points, configured as the override.
        let link = tmp.path().join("claude");
        std::os::unix::fs::symlink(&version_a, &link).unwrap();

        // Built ONCE — exactly as the daemon builds it once at startup.
        let engine = RealRefreshEngine::new(RealAccountStash::new(), Some(link.clone()));

        // Cycle 1: link → A (exists) → Ok. The resolver returns the SYMLINK path UNCANONICALIZED
        // (AC4 / issue constraint [C1]: a wrapper symlink is spawned as-is, never resolved to its
        // target), so the fix changes only the timing of resolution, never which binary is chosen.
        assert_eq!(engine.resolve_binary().unwrap(), link);

        // A Claude Code auto-update removes the target the symlink pointed at (the "resolved
        // version directory deleted by an updater" failure): the SAME engine now resolves to a
        // NON-FATAL error on its next cycle (AC2), rather than reusing a stale frozen path.
        std::fs::remove_file(&version_a).unwrap();
        assert!(matches!(
            engine.resolve_binary(),
            Err(crate::error::Error::ClaudeBinaryNotFound)
        ));

        // …then the update re-points `claude` at the freshly installed binary — and the next cycle
        // SELF-HEALS with no restart and no reconstruction of the engine (AC1, the whole point of
        // #375). Under the frozen design this stays broken until a manual restart.
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&version_b, &link).unwrap();
        assert_eq!(engine.resolve_binary().unwrap(), link);
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
        // No prior refresh → the cadence term is zero, so a freshly-anchored window (anchor ==
        // now) waits the full idle floor.
        let t = tick(vec![], cfg(3600, 60, &[]), FakeEngine::new());
        let now = t.clock.now();
        assert_eq!(t.delay_until_due(now, now, false), Duration::from_secs(60));
    }

    #[test]
    fn cadence_dominates_right_after_a_refresh() {
        let base = Instant::now();
        let mut t = tick(vec![], cfg(3600, 60, &[]), FakeEngine::new());
        t.last_refresh = Some(base);
        // 100 s after a refresh (anchor ≈ the sweep instant): ~3500 s of cadence remain, well
        // above the idle floor, so the cadence term dominates.
        let delay = t.delay_until_due(base + Duration::from_secs(100), base, false);
        assert_eq!(delay, Duration::from_secs(3500));
    }

    #[test]
    fn an_aged_idle_anchor_saturates_so_cadence_alone_gates_a_later_sweep() {
        // Issue #260 behaviour change. Once the idle anchor has aged past `idle_after`, the idle
        // floor saturates to zero and no longer RE-dominates — the cadence term alone gates a
        // steady-state sweep. (Pre-#260 the floor was relative to `now`, re-imposing a fresh 60 s
        // before EVERY sweep; that same relativity is what a 15 s watch exploited to starve the
        // sweep forever.)
        let base = Instant::now();
        let mut t = tick(vec![], cfg(3600, 60, &[]), FakeEngine::new());
        t.last_refresh = Some(base);
        // Two hours after the sweep (anchor seeded at the sweep instant): cadence long satisfied
        // AND the idle floor saturated → due now.
        assert_eq!(
            t.delay_until_due(base + Duration::from_secs(7200), base, false),
            Duration::ZERO,
        );
        // 30 s before the cadence elapses, the idle floor is still saturated (anchor long aged),
        // so the cadence term alone sets the wait — the floor does not add a second 60 s.
        assert_eq!(
            t.delay_until_due(base + Duration::from_secs(3570), base, false),
            Duration::from_secs(30),
        );
    }

    #[test]
    fn a_sub_idle_floor_rearm_sees_a_shrinking_absolute_floor() {
        // The pure-math core of the #260 fix: with the idle floor anchored ABSOLUTELY, a wait
        // re-created at a later `now` against the SAME anchor returns a SHORTER delay — 60 → 45 →
        // 30 → 15 → 0 as a 15 s watch re-arms — instead of resetting to a full 60 s. So a
        // faster-cadence sibling wake cannot pin the floor above its own interval forever.
        let anchor = Instant::now();
        // No prior refresh → the cadence term is zero, so the idle floor alone sets the delay.
        let t = tick(vec![], cfg(3600, 60, &[]), FakeEngine::new());
        for (elapsed, expected) in [(0, 60), (15, 45), (30, 30), (45, 15), (60, 0), (75, 0)] {
            assert_eq!(
                t.delay_until_due(anchor + Duration::from_secs(elapsed), anchor, false),
                Duration::from_secs(expected),
                "a re-arm at +{elapsed}s must see the absolute floor shrink, not reset to 60",
            );
        }
    }

    #[test]
    fn recovery_work_drops_the_cadence_term_for_a_prompt_restore() {
        // Issue #280 AC1: a quarantined-parked account present (`has_recovery_work`) drops the
        // cadence term, so even right after an unrelated sweep — when the cadence would otherwise
        // defer the next sweep ~a full hour — the tick is due within the idle floor. Without the
        // signal the cadence dominates (the pre-#280 behaviour, unchanged).
        let base = Instant::now();
        let mut t = tick(vec![], cfg(3600, 60, &[]), FakeEngine::new());
        t.last_refresh = Some(base);
        let now = base + Duration::from_secs(100); // 100 s after a refresh: ~3500 s cadence remains
                                                   // Normal (no recovery work): the cadence term dominates, deferring the sweep ~3500 s.
        assert_eq!(
            t.delay_until_due(now, now, false),
            Duration::from_secs(3500)
        );
        // Recovery work present: the cadence term is dropped, so only the (freshly-anchored) idle
        // floor gates — a prompt restore attempt, not a full-cadence wait.
        assert_eq!(t.delay_until_due(now, now, true), Duration::from_secs(60));
    }

    #[test]
    fn recovery_work_gates_on_the_idle_floor_even_with_no_prior_refresh() {
        // With no prior refresh the cadence term is already zero, so recovery work does not change
        // the delay — the idle floor gates either way. The recovery bypass is about DROPPING a
        // non-zero cadence, never about tightening below the idle floor (which would risk a storm).
        let anchor = Instant::now();
        let t = tick(vec![], cfg(3600, 60, &[]), FakeEngine::new());
        let now = anchor + Duration::from_secs(15); // 15 s into the idle window
        assert_eq!(
            t.delay_until_due(now, anchor, false),
            Duration::from_secs(45)
        );
        assert_eq!(
            t.delay_until_due(now, anchor, true),
            Duration::from_secs(45)
        );
    }

    #[test]
    fn recovery_pending_matches_what_the_sweep_would_restore() {
        // Issue #280: `recovery_pending` gates on the SAME predicate `run_sweep` acts on —
        // quarantined AND not excluded AND within the allowlist — so a prompt is never raised for a
        // quarantined account the sweep would SKIP (an excluded active/target, or one outside a
        // configured allowlist). Without this parity the tick would fire prompt (poll-cadence)
        // sweeps that no-op on the dead account while over-refreshing the allowlisted ones.
        let roster = vec![
            acct("work", "u-A"),
            acct("spare", "u-B"),
            acct("backup", "u-C"),
        ];
        // No allowlist (empty = all parked): a quarantined, non-excluded account is recovery work.
        let t = tick(roster.clone(), cfg(3600, 60, &[]), FakeEngine::new());
        assert!(t.recovery_pending(&[], &["u-B".to_owned()]));
        assert!(
            !t.recovery_pending(&[], &[]),
            "no quarantine → no recovery work"
        );
        assert!(
            !t.recovery_pending(&["u-B".to_owned()], &["u-B".to_owned()]),
            "an EXCLUDED quarantined account (dead active / target) is not recovery work — the sweep skips it",
        );
        // Allowlist of just `spare` (u-B): a quarantined account OUTSIDE it is NOT recovery work,
        // because the sweep would skip it — so no prompt for a restore that cannot happen.
        let t = tick(roster, cfg(3600, 60, &["spare"]), FakeEngine::new());
        assert!(
            t.recovery_pending(&[], &["u-B".to_owned()]),
            "a quarantined ALLOWLISTED account is recovery work",
        );
        assert!(
            !t.recovery_pending(&[], &["u-C".to_owned()]),
            "a quarantined NON-allowlisted account is not recovery work (#280 allowlist parity)",
        );
    }

    #[test]
    fn a_disabled_tick_reports_no_recovery_work() {
        // A disabled ticker is wholly inert (its `until_due` never resolves), so it has no restore
        // work to prompt for regardless of the daemon's quarantine set.
        let roster = vec![acct("spare", "u-B")];
        let t = RefreshTick::new(
            roster,
            cfg(3600, 60, &[]),
            false, // disabled
            FakeEngine::new(),
            FixedClock {
                now: Instant::now(),
            },
        );
        assert!(!t.recovery_pending(&[], &["u-B".to_owned()]));
    }

    #[test]
    fn recovery_bypass_does_not_busy_spin_at_a_zero_idle_floor() {
        // Issue #280 + ADR-0007 (no retry storm), the `idle_after_secs = 0` edge (a valid config).
        // The recovery path's idle floor is then 0, so the FIRST prompt sweep is immediate — but the
        // run loop disarms recovery after that sweep (`until_due` then sees `false`) and the CADENCE
        // term (from the just-set `last_refresh`) gates the next wait, so there is no busy-spin. The
        // two halves proven here: recovery=true → 0 (prompt), and the post-sweep normal path →
        // ~cadence (never a 0-delay re-fire). The run loop's once-per-period disarm routes the
        // second wait through `false`; see `run_loop_prompts_the_tick_...` for that coupling.
        let base = Instant::now();
        let mut t = tick(vec![], cfg(3600, 0, &[]), FakeEngine::new());
        // Recovery-prompted, no prior refresh: idle floor 0 → immediate (the prompt).
        assert_eq!(t.delay_until_due(base, base, true), Duration::ZERO);
        // After a sweep sets `last_refresh`, the disarmed (`false`) wait gates on the cadence, not
        // the 0 idle floor — a full cadence out, never a 0-delay spin.
        t.last_refresh = Some(base);
        assert_eq!(
            t.delay_until_due(base, base, false),
            Duration::from_secs(3600)
        );
    }

    // --- #260: a faster re-arming watch must not starve the idle floor ------

    #[tokio::test(start_paused = true)]
    async fn until_due_is_not_starved_by_a_faster_rearming_watch() {
        // Issue #260 regression, at the exact seam that starved. The run loop
        // (`src/daemon/run_loop.rs`) RE-CREATES the `refresh.until_due()` select arm every idle
        // iteration, and the 15s external-login watch (`EXTERNAL_LOGIN_WATCH_SECS`) forces an
        // iteration every 15s — shorter than the 60s idle floor. Pre-fix the idle floor was
        // relative to `now`, so each re-created `until_due` reset it to a FULL 60s and the 15s
        // watch won forever: the sweep never became due. With the floor anchored absolutely, each
        // re-created `until_due` computes a SHRINKING delay (60 → 45 → 30 → 15 → 0), so the
        // refresh arm wins within one idle_after window. This mirrors the loop's two load-bearing
        // details: the `biased` arm ORDER (refresh before the watch) and RE-CREATING `until_due`
        // each iteration. A regression that reset the anchor would re-starve and exhaust the loop.
        let mut t = RefreshTick::new(
            vec![],
            cfg(3600, 60, &[]),
            true,
            FakeEngine::new(),
            TokioClock,
        );
        let mut became_due = false;
        for _ in 0..5 {
            tokio::select! {
                biased;
                // Re-created every iteration, exactly as the run loop does.
                () = t.until_due(false) => {
                    became_due = true;
                    break;
                }
                // The 15s login-watch cadence, re-armed every iteration.
                () = tokio::time::sleep(Duration::from_secs(15)) => {}
            }
        }
        assert!(
            became_due,
            "a 15s-cadence watch must not starve the 60s idle floor forever (#260)"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn recovery_until_due_is_not_starved_by_control_read_churn() {
        // Issue #280 AC2/AC4(b): with a quarantined-parked account present AND a refresh that just
        // ran — whose cadence term would otherwise defer the next sweep a full hour — a steady
        // stream of control-socket reads (re-creating `until_due` faster than the idle floor) must
        // NOT starve the restore. The recovery path drops the cadence and gates on the idle floor,
        // which #260 anchored absolutely, so each re-created `until_due` SHRINKS toward due (60 →
        // 45 → … → 0) rather than re-arming. Mirrors the #260 harness with recovery work + a LIVE
        // cadence: without the bypass the cadence (3600 s) would win every churn iteration forever.
        let mut t = RefreshTick::new(
            vec![],
            cfg(3600, 60, &[]),
            true,
            FakeEngine::new(),
            TokioClock,
        );
        // A refresh just ran: the cadence term alone would defer the next sweep ~1 h.
        t.last_refresh = Some(tokio::time::Instant::now().into_std());
        let mut became_due = false;
        for _ in 0..5 {
            tokio::select! {
                biased;
                // Re-created every iteration with recovery work signalled, exactly as the run loop
                // does while a quarantined-parked account is present (before this period's sweep).
                () = t.until_due(true) => {
                    became_due = true;
                    break;
                }
                // A control-read churn cadence shorter than the idle floor, re-armed every iteration.
                () = tokio::time::sleep(Duration::from_secs(15)) => {}
            }
        }
        assert!(
            became_due,
            "recovery work must become due despite control-read churn and a live cadence (#280)"
        );
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
        // outcome, the before/after expiry — after = before + the engine's slide delta — and
        // the cycle's `refresh_token_rotated` flag threaded straight through (issue #279).
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
                    refresh_token_rotated: true,       // rotated — must reach the event
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
                refresh_token_rotated: true,           // sourced from the report above (#279)
                reason: None, // a successful refresh carries no reason (#377)
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
                refresh_token_rotated: false, // this cycle's report did not rotate
                reason: None,              // not an error outcome — no reason (#377)
            }]
        );
    }

    #[tokio::test]
    async fn sweep_records_an_error_event_for_a_hard_failure() {
        // A hard engine `Err` is an `error` event with the stored expiry unchanged — the
        // error Display never reaches the structured event (only the class does). A hard
        // `Err` has NO secret-free sub-class among the fixed #377 set, so it renders a bare
        // `outcome=error` with `reason: None` — distinct from a timeout (which is `Timeout`).
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
                // A hard `Err` has no report to source a rotation from → `false` (#279).
                refresh_token_rotated: false,
                reason: None, // hard `Err`: no secret-free sub-class → no `reason=` (#377)
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn sweep_records_a_timeout_reason_on_a_hung_cycle() {
        // Issue #377: a whole-cycle TIMEOUT is the one error sub-cause detected OUTSIDE a
        // completed engine cycle — the tick's `tokio::time::timeout` bound firing — so it is
        // event-level only and renders `reason=timeout`, distinct from a hard `Err`'s bare
        // `outcome=error`. `start_paused` auto-advances the virtual clock past the 5 s bound.
        let now_ms = now_ms();
        let soon = now_ms + 60_000;
        let roster = vec![acct("work", "u-A")];
        let engine = FakeEngine::new()
            .with_expiry("u-A", Some(soon))
            .with_result("u-A", FakeRefresh::Hang); // sleeps far past the timeout
        let mut config = cfg(3600, 60, &[]);
        config.timeout_secs = 5;
        let mut t = tick(roster, config, engine);
        let outcome = t.sweep(&[], &[]).await;
        assert_eq!(
            outcome.events,
            vec![Event::Refresh {
                account: "work".to_owned(),
                outcome: RefreshEventOutcome::Error,
                expires_before: Some(soon),
                expires_after: Some(soon),
                refresh_token_rotated: false,
                reason: Some(RefreshEventReason::Timeout), // the whole-cycle bound fired (#377)
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

    #[tokio::test(start_paused = true)]
    async fn a_quarantined_account_triggers_a_prompt_sweep_and_restore_without_a_full_cadence() {
        // Issue #280 AC4(a), end-to-end at the tick seam: a quarantined-parked account makes the
        // tick due PROMPTLY (the idle floor, measured on the virtual clock) even though a refresh
        // just ran and the cadence alone would defer it ~1 h — then the ensuing sweep RESTORES it
        // with AC3 semantics unchanged (Refreshed && re_stashed). Ties the recovery-prompted DUE
        // to the restore the whole change exists to make timely.
        let now_ms = now_ms();
        let far = now_ms + 30 * 24 * 3_600_000; // far from expiry — refreshed only via the quarantine bypass
        let roster = vec![acct("dead", "u-Q")];
        let engine = FakeEngine::new().with_expiry("u-Q", Some(far)).with_result(
            "u-Q",
            FakeRefresh::Report(report(RefreshOutcome::Refreshed, true)),
        );
        let mut t = RefreshTick::new(roster, cfg(3600, 60, &[]), true, engine, TokioClock);
        // A refresh just ran: without the recovery bypass the cadence would defer the sweep ~1 h.
        t.last_refresh = Some(tokio::time::Instant::now().into_std());
        // The tick becomes due within the idle floor (60 s virtual), NOT a full cadence (3600 s).
        let start = tokio::time::Instant::now();
        t.until_due(true).await;
        assert_eq!(
            start.elapsed(),
            Duration::from_secs(60),
            "recovery work makes the tick due within the idle floor, not a full cadence (#280)",
        );
        // The prompt sweep restores the quarantined account — AC3 restore semantics unchanged.
        let outcome = t.sweep(&[], &["u-Q".to_owned()]).await;
        assert_eq!(t.engine.refreshed(), vec!["u-Q"]);
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
