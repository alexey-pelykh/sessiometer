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

/// The daemon-level systemic-refresh-failure detector (issue #378): a pure edge-triggered state
/// machine over classified sweeps. Tracks the consecutive all-error sweep streak and a latch for
/// whether a systemic-failure episode is currently active, so BOTH the failure (streak crosses N)
/// and the recovery (first working sweep) fire exactly once per episode. `Default` is the healthy
/// start (no streak, not active), so it drops straight into the daemon's `DecisionState`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SystemicRefreshHealth {
    /// Consecutive all-error sweeps observed so far; reset to 0 by any working sweep. Kept
    /// climbing while a systemic episode is active so [`status`](Self::status) can surface how
    /// long the mechanism has been down.
    consecutive_error_sweeps: u32,
    /// Whether a systemic-failure episode is currently active — the streak has crossed the
    /// threshold and no working sweep has cleared it yet. The edge latch that makes the failure
    /// signal fire once on the crossing (not per subsequent all-error sweep) and the recovery
    /// signal fire once when it clears. Mirrors the daemon's `signaled_all_exhausted` /
    /// `signaled_keychain_locked` once-per-episode idiom, at the refresh-MECHANISM scope.
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

    /// The daemon-level refresh-health indicator for the `status` snapshot (issue #378):
    /// `Some(consecutive_error_sweeps)` while a systemic-failure episode is active (so `status`
    /// can show the mechanism is down and for how many sweeps), `None` when the mechanism is
    /// healthy. A COUNT only — never a token, path, or email (#15).
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
}
