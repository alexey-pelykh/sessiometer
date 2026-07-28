// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Daemon-level detector for a SYSTEMIC refresh-mechanism failure (issue #378).
//!
//! A failure of the refresh *mechanism* — a stale pinned `claude` path (#375), a wedged spawn,
//! an unresolvable binary — fails **every** eligible account's refresh cycle at once, and keeps
//! failing until an operator intervenes. The per-account credential-health rollup (#119) reflects
//! that only as per-account `at_risk` (🟠), which trips per account, only after a streak, and
//! never distinguishes "one account's creds" from "the whole mechanism is down." In the #375
//! incident that gap kept a total refresh outage invisible for ~4.5h, until an account's token
//! finally expired and it was quarantined (🔴).
//!
//! This detector adds the missing signal: it watches consecutive refresh SWEEPS in which EVERY
//! eligible-account cycle failed with `outcome=error` (the mechanism-failure class — spawn /
//! read-back / malformed / timeout, #377; NOT `dead`, which is a per-account credential fact the
//! mechanism successfully *determined*), and once that streak crosses a config-backed threshold N
//! ([`crate::config::RefreshConfig::systemic_failure_n`], default 3) it emits a distinct
//! edge-triggered [`Event::RefreshSystemicFailure`] — ONCE per episode, not per tick while down.
//! The systemic state CLEARS — also edge-triggered, one [`Event::RefreshSystemicRecovered`] — on
//! the first sweep in which the mechanism demonstrably works again (any non-`error` cycle).
//!
//! It is deliberately DISTINCT from the per-account `at_risk` rollup: it fires only when ALL
//! eligible cycles error (a mechanism verdict, not one account's creds), and is visible WITHOUT
//! waiting for any account to die. Pairs with #377 (which gives the per-cycle `reason=`; this
//! gives "it's everyone, not one account") and stays #15-clean by construction — it carries only
//! COUNTS + the `error`/non-`error` classification, never a token, path, or email.
//!
//! The type is a pure state machine ([`SystemicRefreshHealth`]) fed one classified sweep at a time
//! ([`SweepHealth`]), so its edge-trigger behavior is unit-tested here directly, independent of the
//! daemon's async run loop. The daemon owns the live instance in its `DecisionState`, classifies
//! each sweep from the [`crate::contract::SweepOutcome`] it already produces, and folds the result
//! in through [`crate::daemon::Daemon::note_systemic_refresh`] (deferred post-idle, like the
//! sweep's #106 restores and #119 observations); [`SystemicRefreshHealth::status`] projects the
//! live state onto the `status` snapshot.
//!
//! # The false-green window (issue #787)
//!
//! The property this module exists to BOUND is the **false-green window**: the interval in which
//! the refresh mechanism is broken while `status` still renders a clean board. The threshold above
//! bounds it deliberately — N sweeps is the price paid for immunity to one flaky sweep — and the
//! `active` latch then holds the signal up for as long as the fault lasts.
//!
//! A RESTART re-opened that window every single time. This detector is pure IN-MEMORY state in the
//! daemon's `DecisionState`, with no `Serialize`/`Deserialize` and no persistence, so a restart
//! resets the streak to zero and clears the latch: the mechanism stayed broken, but the board went
//! green again for another N sweeps. Not theoretical — the production event log carries exactly ONE
//! `refresh_systemic_failure` (`consecutive=3`) and ZERO `refresh_systemic_recovered`. That episode
//! never closed; it was ERASED, and afterwards `status` showed six healthy accounts over an
//! entirely unfixed fault. The bundled launchd job runs `KeepAlive { SuccessfulExit: false }`, so
//! an abnormal exit is restarted automatically — the supervisor keeping the daemon alive was
//! periodically blinding the detector built to make a #375-class outage visible.
//!
//! The STARTUP PREFLIGHT ([`preflight`] → [`SystemicRefreshHealth::note_preflight`]) closes that
//! window for the class of fault it can observe: at startup the daemon resolves the `claude` binary
//! ONCE, through the same policy the per-cycle spawn site uses, and a failure re-opens the episode
//! immediately — so the operator sees the fault at once rather than N sweeps later, however many
//! times launchd restarts the process.
//!
//! What the preflight does NOT bound, stated plainly so a later reader does not over-trust it:
//!
//! * **Only the RESOLUTION class of fault.** The preflight RESOLVES; it does not SPAWN. A `claude`
//!   that resolves but then wedges, times out, or reads back malformed (#377) is invisible to it,
//!   so for those faults the post-restart window is still the full N sweeps.
//! * **Nothing is frozen or cached.** The preflight is an OBSERVATION, never a resolution the
//!   cycles then reuse: resolution stays per-cycle at the spawn site (#375), and a preflight
//!   failure never gates startup. Both properties are structural — see [`preflight`].
//! * **The signal is not sticky — but "clears" means "on the next sweep that runs".** A
//!   preflight-opened episode clears on the first sweep that demonstrably works, exactly like a
//!   sweep-opened one. A stale fault would mislead an operator who has just fixed the problem and
//!   restarted to apply the fix — which is also why the episode is RE-DERIVED at each startup
//!   rather than persisted across restarts. What that does NOT promise is promptness: a sweep in
//!   which every account was skipped (all far from expiry, all inside a #408 back-off, or all with
//!   an unreadable stored expiry) classifies as `NoSignal` and clears nothing, so the DOWN signal
//!   can outlast the fix by a while. That exposure is shared with the sweep-opened case and is
//!   self-limiting; the variant that is NOT — a config where no sweep can EVER run, which only the
//!   preflight could latch against — is refused up front by
//!   [`crate::refresh_tick::mechanism_is_observable`].

use std::future::Future;
use std::path::PathBuf;

use crate::error::Result;
use crate::observability::{Event, RefreshEventOutcome};

/// How one refresh SWEEP bore on the refresh *mechanism*'s health (issue #378) — the classified
/// input [`SystemicRefreshHealth::note`] folds. A sweep is judged only on the cycles that actually
/// RAN (a healthy, far-from-expiry account the sweep merely read is not a refresh attempt, so it
/// is no evidence either way), and only the `error` class counts as a mechanism failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SweepHealth {
    /// No refresh cycle ran this sweep (nothing was due, or no sweep fired) — no evidence about
    /// the mechanism, so it neither advances nor resets the streak.
    NoSignal,
    /// ≥1 refresh cycle ran and EVERY one failed with `outcome=error` — a mechanism-failure signal
    /// (all eligible accounts erroring is the whole mechanism down, not one account's creds).
    AllError,
    /// ≥1 refresh cycle ran and ≥1 did NOT error (`refreshed` / `no_change` / even `dead` — the
    /// mechanism successfully produced a verdict) — the mechanism demonstrably works, so the
    /// streak resets and, if a systemic episode was active, it recovers.
    Working,
}

impl SweepHealth {
    /// Classify a sweep from its per-cycle refresh outcomes — the `outcome=` of each account the
    /// sweep actually refreshed this cycle (the daemon supplies these from the
    /// [`crate::contract::SweepOutcome`]'s refresh observations). An empty iterator (no cycle ran)
    /// is [`NoSignal`](SweepHealth::NoSignal); otherwise the sweep is [`AllError`](SweepHealth::AllError)
    /// iff EVERY cycle was [`RefreshEventOutcome::Error`], else [`Working`](SweepHealth::Working).
    pub(crate) fn classify(outcomes: impl IntoIterator<Item = RefreshEventOutcome>) -> Self {
        let mut saw_cycle = false;
        let mut all_error = true;
        for outcome in outcomes {
            saw_cycle = true;
            if outcome != RefreshEventOutcome::Error {
                all_error = false;
            }
        }
        match (saw_cycle, all_error) {
            (false, _) => SweepHealth::NoSignal,
            (true, true) => SweepHealth::AllError,
            (true, false) => SweepHealth::Working,
        }
    }
}

/// How the daemon's STARTUP PREFLIGHT (issue #787) bore on the refresh *mechanism*'s health — the
/// classified input [`SystemicRefreshHealth::note_preflight`] folds, and the restart-time sibling of
/// [`SweepHealth`].
///
/// Deliberately TWO variants where [`SweepHealth`] has three: there is no "nothing was due" case at
/// startup (the preflight always runs, so it always produces evidence), but that evidence is only
/// about the mechanism's PRECONDITION — which is why [`Resolved`](Self::Resolved) is emphatically
/// not a [`Working`](SweepHealth::Working)-equivalent. See
/// [`note_preflight`](SystemicRefreshHealth::note_preflight).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreflightHealth {
    /// The `claude` binary RESOLVED — the mechanism's precondition holds. NOT proof the mechanism
    /// works: a binary that resolves can still wedge, time out, or read back malformed on spawn.
    Resolved,
    /// The `claude` binary could NOT be resolved — every eligible account's refresh cycle is
    /// guaranteed to fail (`outcome=error reason=unresolved`, issue #786) until an operator
    /// intervenes. The strongest mechanism-failure evidence available, and available before a
    /// single spawn.
    Unresolved,
}

/// Run the daemon's STARTUP PREFLIGHT (issue #787): attempt the `claude`-binary resolution exactly
/// ONCE through `resolve`, and classify the outcome.
///
/// Two properties are enforced STRUCTURALLY rather than by convention, because both are ways this
/// fix could regress the very issue it is built on top of (#375, which moved resolution to the
/// spawn site precisely so a transient absence self-heals):
///
/// * **A SIGNAL, never a GATE.** The return type is [`PreflightHealth`], NOT
///   `Result<PreflightHealth>`. A resolution failure IS the signal, so there is no `Err` for a
///   caller to `?`-propagate and no way for this to abort daemon startup. Refusing to start on an
///   unresolvable binary would re-freeze exactly what #375 unfroze — an operator who fixes their
///   `PATH` must not also have to notice the daemon died.
/// * **Nothing is cached or frozen.** The resolved `PathBuf` is DROPPED in this body and the return
///   type carries no path, so there is nothing for a later cycle to reuse. Resolution stays
///   per-cycle at the spawn site ([`crate::refresh_tick`]), and the preflight cannot become the
///   startup-resolution-frozen-for-the-process-lifetime design #375 removed.
///
/// `resolve` is INJECTED (the ambient-read seam idiom [`crate::paths`] adopted for its login-shell
/// harvest in #785) so these properties are driven by tests rather than asserted in prose. Production
/// passes [`crate::paths::claude_binary_with_override`] with the operator's `[refresh].claude_bin` —
/// the SAME three-tier policy the spawn site uses, so the preflight can never disagree with what a
/// cycle would have resolved.
///
/// One shared effect is deliberate and harmless: calling the shared resolver also (re)fills the
/// process-wide harvested-`PATH` memo (#783/#784, [`crate::paths::HARVESTED_PATH_TTL`]). That memo
/// holds the harvested PATH *string*, not a resolution, and the directory scan still runs on every
/// single call — so the per-cycle self-healing #375 exists for is untouched.
pub(crate) async fn preflight<F, Fut>(resolve: F) -> PreflightHealth
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<PathBuf>>,
{
    // Both bindings are discarded HERE, at the one place a path or an error could have escaped:
    // the path (so no cycle can reuse it) and the error (so no caller can propagate it).
    match resolve().await {
        Ok(_resolved_path) => PreflightHealth::Resolved,
        Err(_unresolvable) => PreflightHealth::Unresolved,
    }
}

/// The daemon-level systemic-refresh-failure detector (issue #378): a pure edge-triggered state
/// machine over classified sweeps. Tracks the consecutive all-error sweep streak and a latch for
/// whether a systemic-failure episode is currently active, so BOTH the failure (streak crosses N)
/// and the recovery (first working sweep) fire exactly once per episode. `Default` is the healthy
/// start (no streak, not active), so it drops straight into the daemon's `DecisionState`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SystemicRefreshHealth {
    /// Consecutive fleet-wide mechanism-failure OBSERVATIONS so far; reset to 0 by any working
    /// sweep. Usually all-error sweeps — but a failed startup preflight (issue #787) is the same
    /// class of evidence and floors it at one, so a restart-established episode still projects a
    /// non-degenerate count. Kept climbing while a systemic episode is active so
    /// [`status`](Self::status) can surface how long the mechanism has been down.
    consecutive_error_sweeps: u32,
    /// Whether a systemic-failure episode is currently active — the streak has crossed the
    /// threshold (or a startup preflight found the mechanism already broken, issue #787) and no
    /// working sweep has cleared it yet. The edge latch that makes the failure signal fire once on
    /// the crossing (not per subsequent all-error sweep) and the recovery signal fire once when it
    /// clears. Both entry points — [`note`](Self::note) and
    /// [`note_preflight`](Self::note_preflight) — gate on this same latch, so the once-per-episode
    /// contract is a property of the latch rather than of either caller. Mirrors the daemon's
    /// `signaled_all_exhausted` / `signaled_keychain_locked` once-per-episode idiom, at the
    /// refresh-MECHANISM scope.
    active: bool,
}

impl SystemicRefreshHealth {
    /// Fold one classified sweep into the detector, returning the edge-triggered event to emit at
    /// an episode boundary — [`Event::RefreshSystemicFailure`] on the sweep that first crosses the
    /// threshold, [`Event::RefreshSystemicRecovered`] on the first working sweep of an active
    /// episode — or `None` on a neutral / mid-episode sweep. `threshold` is the config-backed N
    /// ([`crate::config::RefreshConfig::systemic_failure_n`], `1..=100`); a `0` is treated as `1`
    /// so a misconfigured floor can never make the detector fire before a single failed sweep.
    ///
    /// - [`SweepHealth::NoSignal`]: neutral — the streak and latch are untouched (no cycle ran, so
    ///   the mechanism was not tested), so an idle gap between near-expiry windows does not clear
    ///   an active episode nor advance a healthy one.
    /// - [`SweepHealth::AllError`]: advances the streak; on the sweep that first reaches the
    ///   threshold (and only then, gated by the `active` latch) it activates the episode and emits
    ///   the failure. Subsequent all-error sweeps keep climbing the count but do NOT re-emit.
    /// - [`SweepHealth::Working`]: resets the streak to 0; if an episode was active it clears it
    ///   and emits the recovery (a single successful mechanism cycle is the recovery edge).
    pub(crate) fn note(&mut self, health: SweepHealth, threshold: u32) -> Option<Event> {
        let threshold = threshold.max(1);
        match health {
            SweepHealth::NoSignal => None,
            SweepHealth::AllError => {
                self.consecutive_error_sweeps = self.consecutive_error_sweeps.saturating_add(1);
                if !self.active && self.consecutive_error_sweeps >= threshold {
                    self.active = true;
                    Some(Event::RefreshSystemicFailure {
                        consecutive: self.consecutive_error_sweeps,
                    })
                } else {
                    None
                }
            }
            SweepHealth::Working => {
                self.consecutive_error_sweeps = 0;
                if self.active {
                    self.active = false;
                    Some(Event::RefreshSystemicRecovered)
                } else {
                    None
                }
            }
        }
    }

    /// Fold the daemon's STARTUP PREFLIGHT into the detector (issue #787), returning the
    /// edge-triggered [`Event::RefreshPreflightUnresolved`] on the preflight that OPENS an episode,
    /// or `None` otherwise. Called ONCE per process, on the fresh (`Default`) state a restart just
    /// produced — the whole point being that this state no longer starts out lying.
    ///
    /// - [`PreflightHealth::Resolved`]: a NO-OP — deliberately not a
    ///   [`SweepHealth::Working`]-equivalent. Resolving the binary proves the mechanism's
    ///   PRECONDITION, not that the mechanism works, so treating it as a working sweep would let a
    ///   startup probe FABRICATE a recovery it never observed. Only a real cycle clears an episode.
    /// - [`PreflightHealth::Unresolved`]: OPENS the episode — the latch is set and the failure is
    ///   emitted at once, WITHOUT waiting for the streak to climb to the threshold. The threshold
    ///   exists to filter a single flaky sweep; an unresolvable binary is not flakiness, it is a
    ///   deterministic, fleet-wide precondition failure that makes every eligible cycle error by
    ///   construction. Gated on the `active` latch exactly as [`note`](Self::note) is, so the
    ///   once-per-episode contract holds across both entry points.
    ///
    /// The streak is raised to at least ONE rather than left at zero. It is the value
    /// [`status`](Self::status) projects onto the wire, and both renderers of that field — the
    /// CLI's `refresh mechanism: DOWN` line and the menu-bar panel's banner — phrase it as
    /// "N consecutive sweep(s) failed", so a zero would render a degenerate "0 consecutive sweeps
    /// failed" on both surfaces. Read the counter as *consecutive fleet-wide mechanism-failure
    /// OBSERVATIONS*: an all-error sweep is the usual one, a failed preflight is the restart-time
    /// one, and a later all-error sweep climbs from there. `max` rather than assignment so this can
    /// only ever raise a streak, never discard evidence a caller had already accumulated.
    ///
    /// The event is DISTINCT from [`Event::RefreshSystemicFailure`] on purpose: a preflight-opened
    /// episode and a genuine N-sweep crossing must stay tellable apart on the log, which is the
    /// same call issue #786 made when it split the resolved path onto its own line instead of
    /// folding it into `reason=`. An episode therefore has TWO possible opening brackets and one
    /// closing bracket — see [`Event::RefreshPreflightUnresolved`].
    pub(crate) fn note_preflight(&mut self, health: PreflightHealth) -> Option<Event> {
        match health {
            PreflightHealth::Resolved => None,
            PreflightHealth::Unresolved if self.active => None,
            PreflightHealth::Unresolved => {
                self.active = true;
                self.consecutive_error_sweeps = self.consecutive_error_sweeps.max(1);
                Some(Event::RefreshPreflightUnresolved)
            }
        }
    }

    /// The daemon-level refresh-health indicator for the `status` snapshot (issue #378):
    /// `Some(consecutive_error_sweeps)` while a systemic-failure episode is active (so `status`
    /// can show the mechanism is down and for how many sweeps), `None` when the mechanism is
    /// healthy. A COUNT only — never a token, path, or email (#15). Always `Some(n >= 1)` while
    /// active, however the episode was opened: the sweep path reaches the latch only by climbing to
    /// a `>= 1` threshold, and the preflight path floors its seed at one (issue #787). Only the
    /// FLOOR is guaranteed — the count keeps climbing for as long as the episode lasts, so it is
    /// not bounded above by `systemic_failure_n`'s validated `1..=100` range.
    pub(crate) fn status(&self) -> Option<u32> {
        self.active.then_some(self.consecutive_error_sweeps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One sweep in which every one of `n` eligible accounts errored — the mechanism-down input.
    fn all_error(n: usize) -> SweepHealth {
        SweepHealth::classify(std::iter::repeat_n(RefreshEventOutcome::Error, n))
    }

    #[test]
    fn classify_reads_no_signal_working_and_all_error() {
        // No cycle ran → NoSignal (a sweep that only read healthy accounts is no evidence).
        assert_eq!(
            SweepHealth::classify(std::iter::empty()),
            SweepHealth::NoSignal
        );
        // Every cycle errored → AllError (the mechanism-down class).
        assert_eq!(all_error(3), SweepHealth::AllError);
        // Any non-error cycle → Working, even when mixed with errors: a `dead` verdict means the
        // mechanism WORKED (it reached the server and got an answer), so it is not "all error".
        assert_eq!(
            SweepHealth::classify([RefreshEventOutcome::Error, RefreshEventOutcome::Dead]),
            SweepHealth::Working
        );
        assert_eq!(
            SweepHealth::classify([RefreshEventOutcome::Refreshed]),
            SweepHealth::Working
        );
        assert_eq!(
            SweepHealth::classify([RefreshEventOutcome::NoChange]),
            SweepHealth::Working
        );
    }

    #[test]
    fn crossing_the_threshold_emits_exactly_once() {
        let mut detector = SystemicRefreshHealth::default();
        // Below the threshold: no signal, and the status stays healthy.
        assert_eq!(detector.note(all_error(2), 3), None);
        assert_eq!(detector.note(all_error(2), 3), None);
        assert_eq!(detector.status(), None);
        // The third consecutive all-error sweep CROSSES N=3 → exactly one failure event, carrying
        // the live consecutive count, and the status now shows the mechanism down.
        assert_eq!(
            detector.note(all_error(2), 3),
            Some(Event::RefreshSystemicFailure { consecutive: 3 })
        );
        assert_eq!(detector.status(), Some(3));
    }

    #[test]
    fn a_mid_episode_all_error_sweep_does_not_re_emit() {
        let mut detector = SystemicRefreshHealth::default();
        for _ in 0..3 {
            detector.note(all_error(2), 3);
        }
        // Already active: further all-error sweeps keep climbing the count for `status` but must
        // NOT re-emit the edge-triggered failure — one signal per episode.
        assert_eq!(detector.note(all_error(2), 3), None);
        assert_eq!(detector.note(all_error(2), 3), None);
        assert_eq!(detector.status(), Some(5));
    }

    #[test]
    fn recovery_clears_the_episode_edge_triggered() {
        let mut detector = SystemicRefreshHealth::default();
        for _ in 0..3 {
            detector.note(all_error(2), 3);
        }
        assert_eq!(detector.status(), Some(3));
        // A single successful (working) sweep is the recovery edge → exactly one recovered event,
        // the streak resets, and the status goes healthy again.
        assert_eq!(
            detector.note(SweepHealth::Working, 3),
            Some(Event::RefreshSystemicRecovered)
        );
        assert_eq!(detector.status(), None);
        // Recovery is edge-triggered: a further working sweep on an already-healthy detector is a
        // no-op (no repeated `recovered`).
        assert_eq!(detector.note(SweepHealth::Working, 3), None);
    }

    #[test]
    fn a_working_sweep_resets_the_streak_before_the_threshold() {
        let mut detector = SystemicRefreshHealth::default();
        assert_eq!(detector.note(all_error(2), 3), None);
        assert_eq!(detector.note(all_error(2), 3), None);
        // A working sweep before the threshold resets the streak (and, not being active, emits
        // nothing) — so the NEXT run of failures must start the count over from 1.
        assert_eq!(detector.note(SweepHealth::Working, 3), None);
        assert_eq!(detector.note(all_error(2), 3), None);
        assert_eq!(detector.note(all_error(2), 3), None);
        assert_eq!(
            detector.note(all_error(2), 3),
            Some(Event::RefreshSystemicFailure { consecutive: 3 })
        );
    }

    #[test]
    fn no_signal_ticks_neither_advance_nor_clear() {
        let mut detector = SystemicRefreshHealth::default();
        // Idle sweeps around a climbing streak do not advance it...
        assert_eq!(detector.note(SweepHealth::NoSignal, 3), None);
        detector.note(all_error(2), 3);
        assert_eq!(detector.note(SweepHealth::NoSignal, 3), None);
        detector.note(all_error(2), 3);
        detector.note(all_error(2), 3); // crosses 3
        assert_eq!(detector.status(), Some(3));
        // ...and once active, an idle sweep does NOT clear it (only a working sweep does).
        assert_eq!(detector.note(SweepHealth::NoSignal, 3), None);
        assert_eq!(detector.status(), Some(3));
    }

    #[test]
    fn a_fully_backed_off_sweep_reads_as_no_signal_and_cannot_mask_a_systemic_failure() {
        // #408 × #378 reachable-state composition: once the refresh error back-off arms on every
        // eligible account, the next sweep SKIPS them all and yields ZERO refresh observations
        // (`refresh_tick` asserts that emptiness at its own seam). Feeding that empty set through
        // the SAME `classify` → `note` path the daemon's run loop uses must read as `NoSignal` and
        // leave an active systemic episode UNTOUCHED — the throttle may DELAY a re-probe but must
        // never fabricate a recovery that hides a genuine mechanism outage from #378.
        let mut detector = SystemicRefreshHealth::default();
        for _ in 0..3 {
            detector.note(all_error(2), 3); // mechanism down → episode active at N=3
        }
        assert_eq!(detector.status(), Some(3));
        // A fully-backed-off sweep classifies exactly like the empty observation set it produces.
        let backed_off = SweepHealth::classify(std::iter::empty());
        assert_eq!(backed_off, SweepHealth::NoSignal);
        assert_eq!(detector.note(backed_off, 3), None);
        // Still active, count intact — detection survives the back-off; only a real `Working` sweep
        // (the mechanism demonstrably recovered) clears it.
        assert_eq!(detector.status(), Some(3));
    }

    #[test]
    fn a_second_episode_re_fires_after_a_recovery() {
        let mut detector = SystemicRefreshHealth::default();
        for _ in 0..3 {
            detector.note(all_error(2), 3);
        }
        detector.note(SweepHealth::Working, 3); // recovered, streak reset
                                                // A fresh streak crossing the threshold again is a NEW episode → the failure fires afresh.
        assert_eq!(detector.note(all_error(2), 3), None);
        assert_eq!(detector.note(all_error(2), 3), None);
        assert_eq!(
            detector.note(all_error(2), 3),
            Some(Event::RefreshSystemicFailure { consecutive: 3 })
        );
    }

    #[test]
    fn systemic_is_distinct_from_per_account_at_risk() {
        // A single account failing (one error among successes) is per-account `at_risk`, NOT
        // systemic: the sweep is Working (the mechanism produced non-error results for others), so
        // the streak never advances and no systemic signal fires even far past the threshold.
        let mut detector = SystemicRefreshHealth::default();
        let one_at_risk = SweepHealth::classify([
            RefreshEventOutcome::Error,
            RefreshEventOutcome::Refreshed,
            RefreshEventOutcome::NoChange,
        ]);
        assert_eq!(one_at_risk, SweepHealth::Working);
        for _ in 0..10 {
            assert_eq!(detector.note(one_at_risk, 3), None);
        }
        assert_eq!(detector.status(), None);
    }

    #[test]
    fn a_single_eligible_account_keys_on_the_error_class_not_mere_failure() {
        // With one eligible account, "all eligible" is degenerate — so the `error`-vs-`dead`
        // distinction is what keeps it meaningful: a lone `dead` account (creds revoked) is
        // Working (the mechanism answered), never systemic; a lone `error` account (mechanism
        // broken) does advance the streak.
        let mut detector = SystemicRefreshHealth::default();
        assert_eq!(
            SweepHealth::classify([RefreshEventOutcome::Dead]),
            SweepHealth::Working
        );
        for _ in 0..5 {
            assert_eq!(
                detector.note(SweepHealth::classify([RefreshEventOutcome::Dead]), 3),
                None
            );
        }
        assert_eq!(detector.status(), None);
        assert_eq!(all_error(1), SweepHealth::AllError);
    }

    #[test]
    fn a_threshold_of_one_fires_on_the_first_all_error_sweep() {
        let mut detector = SystemicRefreshHealth::default();
        assert_eq!(
            detector.note(all_error(2), 1),
            Some(Event::RefreshSystemicFailure { consecutive: 1 })
        );
        // A zero threshold is floored to one (never fires before a failed sweep).
        let mut floored = SystemicRefreshHealth::default();
        assert_eq!(
            floored.note(all_error(2), 0),
            Some(Event::RefreshSystemicFailure { consecutive: 1 })
        );
    }

    // --- The startup preflight: closing the false-green window (issue #787) -------------------
    //
    // A daemon RESTART is modeled here as constructing a fresh `SystemicRefreshHealth::default()`,
    // because that is EXACTLY what a restart does to this type: it is pure in-memory state in the
    // daemon's `DecisionState`, with no `Serialize`/`Deserialize` and no on-disk episode, so the
    // process coming back up is indistinguishable from `Default::default()`. Every test below that
    // says "restart" therefore tests the real thing, not a stand-in.
    //
    // `t4`/`t5` are absent by design: issue #787's T4 and T5 name the two PRE-EXISTING regression
    // tests above — `recovery_clears_the_episode_edge_triggered` and
    // `no_signal_ticks_neither_advance_nor_clear` — which the fix must leave green, not new ones.

    use crate::error::Error;

    /// A preflight that could not resolve the `claude` binary — the fault that erased #378's signal.
    async fn preflight_unresolved() -> PreflightHealth {
        preflight(|| async { Err(Error::ClaudeBinaryNotFound) }).await
    }

    /// A preflight that resolved the binary — the mechanism's precondition holding.
    async fn preflight_resolved() -> PreflightHealth {
        preflight(|| async { Ok(PathBuf::from("/opt/homebrew/bin/claude")) }).await
    }

    #[tokio::test]
    async fn t1_a_restart_under_a_persistent_fault_re_establishes_the_signal_without_n_sweeps() {
        // The mechanism goes down and the detector correctly reports it (this half is #378, and it
        // always worked — the production log proves it fired once, `consecutive=3`).
        let mut before_restart = SystemicRefreshHealth::default();
        for _ in 0..3 {
            before_restart.note(all_error(6), 3);
        }
        assert_eq!(before_restart.status(), Some(3));

        // The daemon restarts with the fault ENTIRELY unfixed. This is the defect: the fresh state
        // reports a healthy mechanism, which is what put six 🟢 accounts on the board over a
        // broken daemon. Asserted explicitly so the test discriminates — it fails loudly if the
        // false-green premise ever stops holding, rather than passing vacuously.
        let mut after_restart = SystemicRefreshHealth::default();
        assert_eq!(
            after_restart.status(),
            None,
            "a restart erases the episode — the false-green this closes"
        );

        // The preflight re-establishes it AT ONCE: one signal, and the board reads DOWN again with
        // ZERO sweeps folded (the `note` counter is untouched below — nothing was swept).
        assert_eq!(
            after_restart.note_preflight(preflight_unresolved().await),
            Some(Event::RefreshPreflightUnresolved)
        );
        assert_eq!(after_restart.status(), Some(1));
    }

    #[tokio::test]
    async fn t2_a_restart_with_the_fault_resolved_raises_no_spurious_signal() {
        // The other half of AC4: a preflight that SUCCEEDS must be silent, and must leave the
        // detector genuinely pristine rather than partially pre-charged.
        let mut detector = SystemicRefreshHealth::default();
        assert_eq!(detector.note_preflight(preflight_resolved().await), None);
        assert_eq!(detector.status(), None);

        // Pristine means the streak still has to climb the FULL threshold afterwards — a preflight
        // success must not have quietly consumed (or added) a sweep's worth of evidence.
        assert_eq!(detector.note(all_error(2), 3), None);
        assert_eq!(detector.note(all_error(2), 3), None);
        assert_eq!(
            detector.note(all_error(2), 3),
            Some(Event::RefreshSystemicFailure { consecutive: 3 })
        );
    }

    #[tokio::test]
    async fn t3_a_preflight_opened_episode_stays_edge_triggered() {
        // AC5 across the two entry points. The existing suite above pins the edge-trigger for a
        // SWEEP-opened episode (`a_mid_episode_all_error_sweep_does_not_re_emit`); this pins the
        // composition #787 introduces — a PREFLIGHT-opened episode must be just as quiet.
        let mut detector = SystemicRefreshHealth::default();
        assert_eq!(
            detector.note_preflight(preflight_unresolved().await),
            Some(Event::RefreshPreflightUnresolved)
        );

        // Sweeps that keep failing climb the count for `status` but re-emit NOTHING — neither the
        // preflight event nor the sweep one. One signal per episode, whoever opened it.
        assert_eq!(detector.note(all_error(6), 3), None);
        assert_eq!(detector.note(all_error(6), 3), None);
        assert_eq!(detector.note(all_error(6), 3), None);
        assert_eq!(detector.status(), Some(4));

        // And a second preflight against an already-active episode is a no-op too (defensive: the
        // daemon runs exactly one per process, but the latch — not the caller — owns the contract).
        assert_eq!(detector.note_preflight(preflight_unresolved().await), None);
        assert_eq!(detector.status(), Some(4));
    }

    #[tokio::test]
    async fn t6_repeated_restarts_under_a_persistent_fault_never_re_hide_it() {
        // The direct regression test for the launchd interaction. The bundled job runs
        // `KeepAlive { SuccessfulExit: false }`, so an abnormal exit is restarted automatically —
        // and before #787 each restart bought a fresh `systemic_failure_n`-sweep false-green
        // window. Five restarts in a row, with the fault untouched and NOT ONE sweep in between.
        let mut signals = 0;
        for restart in 1..=5 {
            let mut detector = SystemicRefreshHealth::default();

            // The window that used to re-open on every single restart...
            assert_eq!(
                detector.status(),
                None,
                "restart {restart}: the erased state is the false-green window"
            );

            // ...is closed immediately, every time. Never once suppressed, never degraded after
            // the first — N restarts yield N signals and ZERO false-green windows, not N windows.
            assert_eq!(
                detector.note_preflight(preflight_unresolved().await),
                Some(Event::RefreshPreflightUnresolved),
                "restart {restart}: the fault must be re-established, not re-hidden"
            );
            assert_eq!(detector.status(), Some(1), "restart {restart}");
            signals += 1;
        }
        assert_eq!(signals, 5);
    }

    #[tokio::test]
    async fn t7_a_failed_preflight_yields_a_classification_never_an_error_so_startup_cannot_gate() {
        // AC2, the single most dangerous regression available here: a preflight that GATED startup
        // would satisfy #787 while violating #375, whose whole point is that resolution is
        // per-cycle so a transient absence self-heals. An operator who fixes their `PATH` must not
        // also have to notice the daemon refused to come up.
        //
        // The guard is STRUCTURAL, and this test is what makes it observable: `preflight` returns a
        // `PreflightHealth`, not a `Result<PreflightHealth>`. There is no `Err` variant for `cli`'s
        // `run` to `?`-propagate, so the unresolvable-binary path cannot reach the `?` that would
        // abort startup — the call below runs to completion and yields a VALUE.
        let health: PreflightHealth = preflight_unresolved().await;
        assert_eq!(health, PreflightHealth::Unresolved);

        // The fold is total in the same way — `Option<Event>`, never `Result` — so the whole
        // startup path from resolution failure to log line has no failure edge at all.
        let mut detector = SystemicRefreshHealth::default();
        let event: Option<Event> = detector.note_preflight(health);
        assert_eq!(event, Some(Event::RefreshPreflightUnresolved));
    }

    #[tokio::test]
    async fn t8_a_failed_preflight_emits_the_signal() {
        // AC1 on the log channel (T1 covers it on the `status` board): the failure is SURFACED, not
        // merely recorded in memory, so an operator reading the daemon log after a restart sees the
        // fault re-established rather than silence.
        let mut detector = SystemicRefreshHealth::default();
        assert_eq!(
            detector.note_preflight(preflight_unresolved().await),
            Some(Event::RefreshPreflightUnresolved)
        );
    }

    #[tokio::test]
    async fn t9_a_successful_preflight_emits_no_fault() {
        // A resolvable binary must be SILENT — and specifically must not be folded as a `Working`
        // sweep. Resolving proves the mechanism's PRECONDITION, not that it works: a `claude` that
        // resolves can still wedge or time out on spawn (#377). Were `Resolved` treated as
        // `Working`, a startup probe could fabricate a recovery it never observed.
        let mut detector = SystemicRefreshHealth::default();
        assert_eq!(detector.note_preflight(preflight_resolved().await), None);
        assert_eq!(detector.status(), None);

        // Concretely: a success cannot clear an episode the way a real working sweep does.
        let mut active = SystemicRefreshHealth::default();
        for _ in 0..3 {
            active.note(all_error(2), 3);
        }
        assert_eq!(active.status(), Some(3));
        assert_eq!(active.note_preflight(preflight_resolved().await), None);
        assert_eq!(
            active.status(),
            Some(3),
            "a resolvable binary is not a working mechanism — only a real cycle clears"
        );
    }

    #[tokio::test]
    async fn t10_a_failed_preflight_clears_on_the_first_working_cycle() {
        // AC4: a stale sticky fault is as misleading as a false-green — and this is exactly the
        // case that made mechanism (a) preferable to persisting the episode. A persisted episode
        // would be stale in the FALSE-POSITIVE direction, showing DOWN to an operator who has just
        // fixed the fault and restarted to apply the fix. Re-derived state cannot do that: the
        // first sweep that demonstrably works clears it.
        let mut detector = SystemicRefreshHealth::default();
        detector.note_preflight(preflight_unresolved().await);
        assert_eq!(detector.status(), Some(1));

        assert_eq!(
            detector.note(SweepHealth::Working, 3),
            Some(Event::RefreshSystemicRecovered)
        );
        assert_eq!(detector.status(), None);
        // Edge-triggered on the way out too: no repeated `recovered` on an already-healthy
        // detector, whichever bracket opened the episode.
        assert_eq!(detector.note(SweepHealth::Working, 3), None);
    }

    #[tokio::test]
    async fn t11_the_preflight_holds_no_resolution_for_a_later_cycle_to_reuse() {
        // AC3 / the #375 constraint: the preflight is an OBSERVATION, never a resolution the cycles
        // then reuse. Before #375 `cli` resolved once at startup and froze the `PathBuf` for the
        // daemon's whole life, so a mid-run change silently failed EVERY refresh until a manual
        // restart. Re-introducing a startup resolution is precisely the shape of that regression,
        // so the seam is counted here rather than trusted.
        let calls = std::cell::Cell::new(0usize);

        // First observation: the binary is there.
        let first = preflight(|| {
            calls.set(calls.get() + 1);
            async { Ok(PathBuf::from("/opt/homebrew/bin/claude")) }
        })
        .await;
        assert_eq!(first, PreflightHealth::Resolved);
        assert_eq!(calls.get(), 1, "exactly ONE resolution per preflight");

        // The binary then goes away (an updater removed the version dir the symlink pointed at —
        // the #375 failure). A caller that had CACHED the first resolution would still read
        // `Resolved`; this one resolves afresh and sees the truth. That divergence is the
        // discriminating assertion: it is impossible under any memoizing implementation.
        let second = preflight(|| {
            calls.set(calls.get() + 1);
            async { Err(Error::ClaudeBinaryNotFound) }
        })
        .await;
        assert_eq!(second, PreflightHealth::Unresolved);
        assert_eq!(calls.get(), 2, "the second caller resolved again");

        // And the reason nothing CAN be cached is structural, not incidental: `PreflightHealth` is
        // a bare two-variant classification with no `PathBuf` anywhere in it, so a resolved path
        // physically cannot escape `preflight` for a later cycle to pick up. The per-cycle
        // resolution at the spawn site stays the only source of a path to spawn — see
        // `refresh_tick::tests::real_refresh_engine_resolves_the_binary_per_cycle_not_frozen_at_construction`.
        assert_eq!(std::mem::size_of::<PreflightHealth>(), 1);
    }
}
