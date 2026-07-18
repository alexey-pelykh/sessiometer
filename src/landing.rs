// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Runtime detector primitives for a LOCAL landing-point swap-out overshoot (issue #613).
//!
//! The OFFLINE landing SLI ([`crate::reliability`], issue #595) reconstructs where each
//! `reason=session` swap-out ACTUALLY landed — the peak `session_pct` its parked (outgoing)
//! account reached within a bounded window after the swap, the post-swap committed tail that SLI 1
//! (the swap-DECISION reading) is blind to. But that reader is OFFLINE and PER-MACHINE: a breach
//! surfaces only on a manual `sessiometer reliability` run, and this machine's log cannot see
//! another machine's push past the ceiling at all — cross-machine coordination is out of scope (no
//! shared backend; see [`crate::swap`]'s single-machine-sync boundary note).
//!
//! This module is the RUNTIME mirror of that SLI's `P100 < 99` ceiling check, scoped to what THIS
//! daemon observes live. The daemon already polls every roster account on the #366 staggered
//! interleave, so it sees a parked account keep billing after a swap redirects only NEW requests.
//! When a recently-parked account's live reading reaches the SLO ceiling within the landing window,
//! that is a LOCAL landing overshoot — surfaced through `status` (like the #378
//! `systemic_refresh_failure` banner) so a silent breach becomes operator-visible in the moment,
//! not only in a later offline readout. It adds NO durable event: the offline SLI already
//! reconstructs the landing from the durable usage samples + swap events, so the runtime signal is
//! purely a visibility layer over state the daemon already holds.
//!
//! Best-available, NOT complete: it is still per-machine (a co-consuming second machine's tail is
//! invisible to it), and it fires only on a parked reading the staggered poll actually catches
//! inside the window — the same coverage bound the offline SLI's "unmeasured" swaps carry. Only the
//! PURE pieces (the ceiling comparison and the two windows) live here so the daemon wiring stays a
//! thin observer; the arming, per-account watch, and windowed `status` projection are in
//! [`crate::daemon`], the same split the #479 `recent_blind_preempt_swap` notice uses.

use std::time::Duration;

/// The SLO ceiling a post-swap landing must stay STRICTLY BELOW — the swap-out `session_pct`
/// `P100 < 99` target (issue #455 / #595). Referenced from the offline reader's own
/// [`crate::reliability::SLO_SWAP_P100_MAX`] so the runtime signal and the offline SLI check the
/// SAME line and cannot drift. Distinct from the CONFIG `session_ceiling` ceiling (default 95,
/// ADR-0023), which sits deliberately BELOW this SLO — a landing overshoot means the post-swap
/// committed tail carried the parked account past the SLO despite that sub-SLO margin.
pub(crate) const LANDING_SLO_CEILING_PCT: u8 = crate::reliability::SLO_SWAP_P100_MAX;

/// The bounded window after a `reason=session` swap within which the parked (outgoing) account's
/// live session peak counts as its LANDING (issue #613). The monotonic-clock mirror of the offline
/// SLI's [`crate::reliability::LANDING_WINDOW_SECS`] (~15 min), tied to it so the runtime and
/// offline windows cannot drift. After it elapses the watch disarms with no overshoot — a later
/// climb is a fresh session cycle, not this swap's committed tail.
pub(crate) const LANDING_WINDOW: Duration =
    Duration::from_secs(crate::reliability::LANDING_WINDOW_SECS as u64);

/// How long a fired local landing-overshoot notice is projected onto `status` (issue #613): the
/// bounded window the retained record is surfaced within, after which it clears. Mirrors the #479
/// `BLIND_PREEMPT_NOTICE_SECS` (300 s) — a transient, high-signal notice an operator should see for
/// a few minutes after the breach, not a latch that lingers indefinitely (a point-in-time overshoot
/// has no natural "recovery" edge to clear it, unlike the #378 systemic-failure latch).
pub(crate) const LANDING_OVERSHOOT_NOTICE_SECS: u64 = 300;

/// Whether a parked account's post-swap `landing_pct` (a rounded `0..=100` session percent, the
/// same `to_pct` unit the swap event and `status` speak) is a landing OVERSHOOT: at or above the
/// [`LANDING_SLO_CEILING_PCT`] SLO ceiling. The runtime counterpart of the offline SLI's
/// `Landing::p100_met` check (`p100 < ceiling` = met), inverted to "did THIS landing breach".
pub(crate) fn is_overshoot(landing_pct: u8) -> bool {
    landing_pct >= LANDING_SLO_CEILING_PCT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overshoot_fires_at_and_above_the_slo_ceiling() {
        // Below the ceiling is not an overshoot; AT it already is — the SLO is a strict `< 99`, so a
        // landing that reaches exactly the ceiling has breached it.
        assert!(!is_overshoot(LANDING_SLO_CEILING_PCT - 1));
        assert!(is_overshoot(LANDING_SLO_CEILING_PCT));
        assert!(is_overshoot(100));
    }

    #[test]
    fn the_runtime_ceiling_tracks_the_offline_slo() {
        // The runtime check must be the SAME line the offline landing SLI enforces — tied by
        // construction, asserted here so a future edit to one side cannot silently drift the other.
        assert_eq!(
            LANDING_SLO_CEILING_PCT,
            crate::reliability::SLO_SWAP_P100_MAX
        );
    }

    #[test]
    fn the_runtime_window_tracks_the_offline_landing_window() {
        // Same tie for the observation window: the live watch and the offline reconstruction bound
        // the post-swap tail over the identical ~15 min span.
        assert_eq!(
            LANDING_WINDOW.as_secs() as i64,
            crate::reliability::LANDING_WINDOW_SECS
        );
    }
}
