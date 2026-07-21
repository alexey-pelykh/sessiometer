// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Records: the poll loop's value types, and the pure bounded-blindness view projection over them.
//!
//! The plain data the decision core carries between ticks, in file order: what a tick DECIDED
//! ([`TickAction`] / [`TickOutcome`]), the retained anchors and records it keys off ([`LastSwap`],
//! [`LastGood`], [`BlindAnchor`], [`BlindPreemptSwapRecord`], [`ParkedLanding`], [`VelocityEma`]),
//! the tick-local poll back-off ([`TickBackoff`]), and the socket-swap re-validation verdict
//! ([`SwapVerdict`]) — plus [`blind_active_view`], the pure `&self`-free projection of the retained
//! pre-blind anchor onto the `status` wire (issue #479). None holds a `Clock`, a seam, or any I/O:
//! they are plain data (a monotonic `Instant` at most) plus one function reading only its
//! arguments, so records and projection alike are unit-tested without a `Daemon`.
//!
//! Extracted verbatim from `daemon` per the God-module decomposition (issue #637 step 3, issue
//! #658) — a behavior-preserving move, re-exported under `crate::daemon::*` so every existing call
//! site resolves unchanged. The state machine that MUTATES these records (the `Daemon` tick, the
//! swap paths, `DecisionState`) stays in `daemon` — as do the two records this step did not name,
//! `AnchorArmInputs` (the [`blind_active_view`] argument bundle) and `LandingOvershootRecord`, and
//! BOTH `recent_*_view` projections: `recent_landing_overshoot_view` beside the record it projects,
//! and `recent_blind_preempt_swap_view`, left behind projecting the [`BlindPreemptSwapRecord`] that
//! moved here.
//!
//! Visibility RE-STATES what each item had before the move, never widens it. Everything that was
//! daemon-private — eight of the types, every one of their fields, [`TickAction::decision_class`]
//! and [`blind_active_view`] — is `pub(super)`, i.e. `pub(in crate::daemon)`: EXACTLY the daemon
//! subtree that could reach it under the ancestor rule before the split, and no wider. Only
//! [`TickAction`] and [`TickOutcome`] are `pub(crate)`, because they already were (`observability`
//! intra-doc-links `crate::daemon::TickAction`, and `run_loop` consumes both). `daemon` re-exports
//! the `pub(super)` names through a PRIVATE `use`, which the sibling submodules still pick up via
//! their own `use super::*` — so nothing leaks crate-wide to buy the relocation.

use super::*;

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
    /// PREEMPTIVELY-swapped away from a BLIND (not dead) active account `from` to `to`
    /// before it could self-exhaust unobserved (issue #452, ADR-0017): the bounded-blindness
    /// gate fired — the active was blind past `session_blind_swap_secs`, its retained
    /// pre-blind anchor (`last_good`, #450) — plausibility-corrected to its window high-water
    /// mark (#619) — sat at/over `session_blind_risk_band`, and a viable target existed. Distinct
    /// from [`Swapped`](Self::Swapped) (reactive, on a fresh reading) and
    /// [`EmergencySwapped`](Self::EmergencySwapped) (dead active, gates bypassed): this path HONORS
    /// cooldown and the target reserve, and keys off the STALE anchor — never the missing reading —
    /// so a genuinely-unknown active makes no swap.
    PreemptivelySwapped { from: usize, to: usize },
    /// PREEMPTIVELY-swapped away from an OBSERVED active account `from` to `to` whose PROJECTED
    /// session usage crossed the trigger before the observed reading did (issue #539, ADR-0017):
    /// the velocity-projection gate fired — the observed reading was at/over
    /// `session_velocity_min_project_above` but below the trigger, its retained EMA velocity
    /// (≥ [`MIN_VELOCITY_SAMPLES`] samples) projected `last + rate × session_velocity_horizon_secs`
    /// at/over the trigger, and a viable target existed. Distinct from
    /// [`PreemptivelySwapped`](Self::PreemptivelySwapped) (BLIND, stale anchor): this fires on a
    /// FRESH reading + its velocity. Like it, HONORS cooldown and the target reserve, and never
    /// fires on a missing reading or an unwarmed velocity.
    VelocityPreemptivelySwapped { from: usize, to: usize },
    /// The active account's credential is DEAD (quarantined, #42) but no other
    /// account is a viable swap target — the daemon holds on the dead active, unable
    /// to escape. The `credential_dead` signal already fired on the death transition,
    /// so this state is silent (no repeat-spam). The dead-credential cousin of
    /// [`NoViableTarget`](Self::NoViableTarget).
    ActiveDeadNoTarget,
    /// The shared canonical `Claude Code-credentials` item was scrubbed/empty (Claude Code's
    /// first-`invalid_grant` scrub, ADR-0018) and the daemon AUTONOMOUSLY adopted a viable roster
    /// account (roster index `to`) into it — healing every local `claude` session on its next
    /// request with no operator action (issue #467). The narrow ADR-0007 decision-4 carve-out: a
    /// scrubbed canonical WITH a live target, distinct from the genuinely-all-dead
    /// [`ActiveDeadNoTarget`](Self::ActiveDeadNoTarget) that still needs a manual `claude /login`.
    CanonicalAdopted { to: usize },
    /// Active is over the trigger but no other account is a viable target: every
    /// other account is weekly-exhausted (or, with the opt-in target-max-session-usage
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

impl TickAction {
    /// The operator-facing [`DecisionClass`] this action renders as on the diagnostic
    /// channel (issue #77). Total and 1:1 over the variants; the swap participants of
    /// [`Swapped`](Self::Swapped) / [`EmergencySwapped`](Self::EmergencySwapped) are
    /// intentionally dropped (the decision line is a pure label — the handles ride the
    /// event log's `swap` line and the foreground echo).
    pub(super) fn decision_class(self) -> DecisionClass {
        match self {
            TickAction::Held => DecisionClass::Hold,
            TickAction::Swapped { .. } => DecisionClass::Swap,
            TickAction::EmergencySwapped { .. } => DecisionClass::EmergencySwap,
            TickAction::PreemptivelySwapped { .. } => DecisionClass::PreemptiveSwap,
            TickAction::VelocityPreemptivelySwapped { .. } => DecisionClass::VelocityPreemptiveSwap,
            TickAction::ActiveDeadNoTarget => DecisionClass::ActiveDeadNoTarget,
            TickAction::CanonicalAdopted { .. } => DecisionClass::CanonicalAdopted,
            TickAction::NoViableTarget => DecisionClass::AllExhausted,
            TickAction::SkippedActiveUnknown => DecisionClass::SkipActiveUnknown,
            TickAction::SkippedActiveUnavailable => DecisionClass::SkipActiveUnavailable,
            TickAction::SkippedCooldown => DecisionClass::SkipCooldown,
            TickAction::SwapFailed => DecisionClass::SwapFailed,
            TickAction::KeychainLocked => DecisionClass::KeychainLocked,
        }
    }
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
    /// The operator-facing diagnostics this cycle generated (issue #77), in the
    /// order they are emitted: one [`Diagnostic::Poll`] per polled account (in
    /// roster order), then — on the edge — a [`Diagnostic::AllExhaustedCleared`]
    /// when this cycle LEFT the all-exhausted state, and finally the per-tick
    /// [`Diagnostic::Tick`] decision (with any back-off). Unlike `events`, EVERY
    /// tick produces some (a Hold still logs its poll outcomes + decision), so
    /// `run_loop`'s [`DiagnosticLog`] — not this vec — applies the verbosity gate.
    /// Produced unconditionally so the #15 redaction meter scans them on every
    /// cycle, in quiet mode too.
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// The per-account readings this cycle, for the control socket (`status`).
    pub(crate) snapshot: StatusSnapshot,
    /// How long the run loop should wait before the next tick. `None` = the normal
    /// jittered poll interval (issue #38); `Some(d)` = the locked-keychain back-off
    /// (issue #13), imposed while the keychain stays locked and NOTHING can be polled.
    /// The rate-limit / transient back-off is NO LONGER a whole-loop wait (issue #293):
    /// it is scoped per-account and applied by skipping the throttled account's own poll
    /// (see [`Daemon::note_account_backoff`]), so it never widens this loop-level wait.
    pub(crate) next_wait: Option<Duration>,
}

/// When the loop last performed a swap. Drives the post-swap cooldown floor (its
/// `at`); the forward-looking `status` candidate is computed fresh from readings
/// (#88's `next_swap`), so this record no longer feeds the display.
#[derive(Debug, Clone)]
pub(super) struct LastSwap {
    /// When the swap completed — monotonic, so it is the cooldown floor.
    /// Process-local: never serialized directly (an [`Instant`] is meaningless across
    /// the socket).
    pub(super) at: Instant,
}

/// The ACTIVE account's last SUCCESSFUL usage reading, retained as a pre-blind
/// anchor (issue #450) SEPARATELY from [`DecisionState::last_readings`] — which a
/// failed / throttled poll clears to `None`, leaving the reactive swap path
/// (`swap::decide`) byte-for-byte unchanged but losing any answer to "how near the
/// band was the active account when it went blind?". Refreshed on every successful
/// active-account poll and carried untouched across a `429` / `5xx`, so the
/// bounded-blindness preemptive swap (issue #452, ADR-0017) can key off `session`
/// and `blind_elapsed = now - at`. Reset to `None` on every swap-away / active-loss
/// (the reset sites at [`record_swap`](Daemon::record_swap),
/// [`adopt_manual_swap`](Daemon::adopt_manual_swap), and the reconcile paths), so it
/// always describes the CURRENT active account — which is why the anchor carries no
/// account handle of its own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct LastGood {
    /// Session-window fraction (`[0.0, 1.0]`) of the retained pre-blind reading.
    pub(super) session: f64,
    /// Weekly-window fraction (`[0.0, 1.0]`) of the retained pre-blind reading.
    pub(super) weekly: f64,
    /// When the reading was observed — monotonic ([`Instant`]), so #452 measures
    /// `blind_elapsed` against the SAME clock as the swap cooldown ([`LastSwap::at`]).
    /// Process-local: never serialized (an [`Instant`] is meaningless across the socket).
    pub(super) at: Instant,
}

/// A per-account PRE-BLIND anchor (issue #583): the account's last successful reading, captured on
/// the live→blind ENTRY edge and held for the whole blind episode, so the uncensored blind-episode
/// pair ([`Event::BlindEnter`] / [`Event::BlindExit`]) can be measured for ANY account regardless of
/// whether it recovers and regardless of whether the daemon swaps away from it.
///
/// DELIBERATELY SEPARATE from [`LastGood`], which cannot serve this purpose despite carrying similar
/// fields: `last_good` is a single ACTIVE-only slot that every swap-away / active-loss site RESETS to
/// `None` (by design — it must always describe the CURRENT active account, which is why it carries no
/// account handle). Those resets are exactly the second censoring tail #583 fixes, so this anchor is
/// held one-per-roster-slot and is touched by NOTHING but its own episode edges — no swap path, no
/// active resolution. Keeping the two separate leaves #450/#452's anchor semantics byte-for-byte
/// unchanged, exactly as `last_good` itself is kept separate from `last_readings`.
///
/// Carries BOTH usage windows: the session window resets on its own 5 h cadence, so a session-only
/// anchor cannot distinguish a mid-blindness reset from a quiet account — the failure that hid a real
/// weekly burn in production (see [`Event::BlindExit`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct BlindAnchor {
    /// Session-window fraction (`[0.0, 1.0]`) of the last reading before the account went blind.
    pub(super) session: f64,
    /// Weekly-window fraction (`[0.0, 1.0]`) of the last reading before the account went blind.
    pub(super) weekly: f64,
    /// When that reading was observed — monotonic ([`Instant`]), carried over from the account's
    /// `last_reading_at` slot, so the episode duration is measured against the SAME clock as the
    /// #450 anchor and the swap cooldown. Process-local: never serialized.
    pub(super) at: Instant,
    /// Whether this account was the ACTIVE one at the moment it went blind — context the episode
    /// carries to both edges (and the input to [`Event::BlindExit`]'s `swapped_away`), never a
    /// filter on whether the episode is recorded.
    pub(super) was_active: bool,
    /// Whether the anchor sat at/over the session trigger (the risk band) when the account went
    /// blind — the same tag [`Event::BlindWindow`] carries, evaluated ONCE at entry (the anchor is
    /// fixed for a whole episode) and carried through to the exit.
    pub(super) near_limit: bool,
}

/// The most recent #452 bounded-blindness PREEMPTIVE swap, retained so `status` can NARRATE it
/// (issue #479). Set in [`Daemon::blind_swap`] on a successful swap-away from a BLIND active account;
/// projected onto the wire ([`BlindPreemptSwap`]) by [`recent_blind_preempt_swap_view`] only while
/// still-current (the swap's `to` is still active) AND recent (within [`BLIND_PREEMPT_NOTICE_SECS`]).
/// Cleared in [`Daemon::record_swap`] (so a later same-active swap supersedes it before
/// [`blind_swap`](Daemon::blind_swap) re-sets its own); a differently-targeted swap self-invalidates
/// it at projection time instead. DEDICATED — kept SEPARATE from the cooldown-bearing [`LastSwap`]
/// (read on every swap path) exactly as [`LastGood`] is kept separate from `last_readings`, so the
/// narration state never burdens the cooldown primitive. Non-secret — two operator handles + a `u8`
/// + a process-local [`Instant`] (never serialized), never a token or email (issue #15).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct BlindPreemptSwapRecord {
    /// The label swapped AWAY FROM (the blind account) — the undo the surface names is `use <from>`.
    pub(super) from: String,
    /// The label swapped TO (now active) — the target-still-active projection gate compares against it.
    pub(super) to: String,
    /// The stale pre-blind session % the gate fired on (`to_pct(anchor.session)`), captured at
    /// swap-time (by projection time the anchor `last_good` is `None`).
    pub(super) last_known_session_pct: u8,
    /// When the swap fired — monotonic ([`Instant`]), so the [`BLIND_PREEMPT_NOTICE_SECS`] window is
    /// measured against the SAME clock as the anchor / cooldown. Process-local: never serialized.
    pub(super) at: Instant,
}

/// A per-account ARMED landing watch for the runtime landing-overshoot signal (issue #613). Set on
/// the account the daemon just swapped AWAY FROM on a `reason=session` swap (in
/// [`Daemon::decide_action`]), held in [`DecisionState::parked_landing`] (one slot per roster
/// account, like [`DecisionState::last_readings`]). While armed, each subsequent poll of THAT parked
/// account is checked against the SLO ceiling ([`landing::is_overshoot`]) within the
/// [`landing::LANDING_WINDOW`]: a crossing records a [`LandingOvershootRecord`] and disarms (fire
/// once per parked episode); the window elapsing, or the account going active again, disarms with no
/// overshoot. Process-local: never serialized. Non-secret — a `u8` + a monotonic [`Instant`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ParkedLanding {
    /// When the `reason=session` swap parked the account — monotonic ([`Instant`]), so the
    /// [`landing::LANDING_WINDOW`] is measured against the SAME clock as the anchor / cooldown.
    pub(super) armed_at: Instant,
    /// The session % the swap FIRED on (`to_pct(active_usage.session)`, the `session_pct=` on the
    /// swap line), carried so the fired notice can show the tail's size (fired at X, landed at Y).
    pub(super) decision_pct: u8,
}

/// The retained per-account SESSION-velocity signal (issue #399), EMA-smoothed, for the #539
/// velocity-projection preemptive trigger (ADR-0017). The transient [`usage_velocity`] the poll
/// fold logs is discarded, so the projective path — which runs at decision time, a step AFTER the
/// fold that would recompute it — has nothing to project from; this carries a smoothed rate ACROSS
/// polls instead. Held in [`DecisionState::session_velocity`], one slot per roster account (only the
/// ACTIVE slot is projected, but every account accrues its own so the signal is warm the moment it
/// becomes active), reset to `None` on a session-usage DROP (a 5 h window reset / recovery — the
/// prior climbing trend is then stale) so a post-reset projection never keys off a pre-reset rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct VelocityEma {
    /// EMA-smoothed session-usage rate as a FRACTION per second (`[0.0, 1.0]`-domain usage /
    /// second), NOT the integer `%/min` the durable [`Event::UsageVelocity`] renders — kept in the
    /// same fraction units as the `session` reading + the trigger so the projection
    /// `last + rate × horizon_secs` needs no unit conversion. Non-negative while climbing (a drop
    /// resets the slot to `None`, so a stored rate is never negative).
    pub(super) rate: f64,
    /// Velocity samples folded into `rate` since the last reset. The SUSTAINED-velocity gate: a
    /// single seeded sample (`samples == 1`) is one interval's spike, so the projective path fires
    /// only at [`MIN_VELOCITY_SAMPLES`] (≥ 2) — the EMA has then blended ≥ 2 intervals, so an
    /// isolated spike that did not persist has already been damped back down.
    pub(super) samples: u32,
}

/// The back-off one throttled poll imposed this tick (issue #293/#294), for the diagnostic
/// tick line. The output sibling of [`classify::BackoffSignal`] (the input): `wait` is the effective
/// window armed on the account — `max(self-capped exponential, server Retry-After)`, where
/// the exponential self-caps at [`POLL_BACKOFF_CAP`] for a peer or the tighter
/// [`ACTIVE_POLL_BACKOFF_CAP`] for the active account, and the `Retry-After` arm is clamped
/// to [`POLL_BACKOFF_CAP`] for a peer but is an un-clamped floor for the active account
/// (issue #453) — which the line renders as `backoff_secs`. `retry_after` is the RAW
/// server-advised `Retry-After` the response supplied (issue #295), BEFORE any clamp, or
/// `None` when the server sent none — the source label that tells a server-advised wait from
/// the daemon's self-capped exponential. Pre-cap on purpose: a pathological value the #294
/// PEER cap bit stays visible (`wait` = 3600 s beside `retry_after` = 86400 s), rather than
/// collapsing into an unplaceable `backoff_secs=3600`.
#[derive(Debug, Clone, Copy)]
pub(super) struct TickBackoff {
    pub(super) wait: Duration,
    pub(super) retry_after: Option<Duration>,
}

/// The daemon's own re-validation verdict for a socket `swap` command (issue #167) — the pure
/// core of [`Daemon::perform_socket_swap`], so the "the daemon re-validates the target itself,
/// never the client hint" rule is unit-testable apart from the swap I/O (mirroring the pure
/// [`pick_target`] / [`crate::use_account`] `cooldown_active`).
pub(super) enum SwapVerdict {
    /// Proceed: swap the active account OFF and the target ON.
    Swap,
    /// The target is ALREADY active — a no-op success (nothing to write), the non-`force`
    /// already-active case.
    AlreadyActive,
    /// Refused, with the redacted wire reason ([`SwapRejection`]).
    Reject(SwapRejection),
}

/// Project the active account's BOUNDED-BLINDNESS state (issue #479, umbrella #363 Path B) for the
/// `status` wire, or `None` when the account is not in bounded blindness. PURE — a function of the
/// retained pre-blind anchor (`last_good`, #450), the blind predicate, the quarantine flag, and the
/// monotonic clock — so it is unit-tested directly and `status` surfaces a SEMANTIC line
/// (blind duration + last-known session % + auto-protection OK/DEGRADED) instead of the content-free
/// `n/a … 🟡` a bare failed-poll row shows.
///
/// Returns `Some` only when ALL hold — the same episode shape [`Daemon::note_blind_gate_eligibility`]
/// gates on, MINUS its viable-target check (surfacing the state does not need a target):
/// - `active_is_blind` — the active account's live reading is cleared (`last_readings[active]` is
///   `None`, a `429`/`5xx` blind window), AND
/// - `!quarantined` — a DEAD (#42) blind active belongs to the `emergency_swap` path, not bounded
///   blindness; ADR-0017 keeps the two separate, so a quarantined active is excluded, AND
/// - a retained anchor exists (`anchor.last_good` is `Some`, #450) — never a spurious projection on a
///   genuinely-unknown account with no reading to key off.
///
/// `auto_protection_degraded` is `true` when ANY of THREE arms is active on this blind account:
/// - the ANCHOR arm ([`blind_gate_armed`] — the SAME arming test
///   [`Daemon::note_blind_gate_eligibility`] gates on, there as its negation), fronting the #452 swap,
///   on the #619 plausibility-corrected anchor session (`anchor.high_water`) so this projection
///   tracks the swap the gate actually fires rather than a stale-low pre-blind reading;
/// - the #582 SERVER-directed arm (`server_retry_after_hold` AND blind past the interim
///   [`BLIND_GATE_SECS`]), fronting the #582 swap-away; and
/// - the #584 VELOCITY-projection arm ([`blind_velocity_projected_armed`]) — the first arm mirroring NO
///   decision: it reports a BELOW-band anchor whose retained #539 velocity, projected over the blind
///   window, could PLAUSIBLY reach the trigger (a burn the anchor arm cannot see, because the frozen
///   anchor sits below the band). The daemon fires no swap on it (report-only, ADR-0017 / issue #584), so
///   this arm makes `status` HONEST about a blind account it cannot protect rather than protecting it.
///
/// The invariant the arms serve is ONE-SIDED — `status` must never claim "auto-protection OK" while the
/// daemon has actually stopped protecting the account. #582's episode was a below-band account blind
/// behind a `Retry-After: 3600`, burning, while `status` said OK; #584 is its velocity twin — a
/// below-band account burning fast with NO directive at all. Each arm is a disjunct, so adding one can
/// only move OK→DEGRADED, never the reverse: the first two mirror a swap DECISION, the velocity arm is
/// pure honesty with no swap behind it.
///
/// `server_retry_after_hold` is [`server_retry_after_holding`]`.is_some()` for THIS account — the
/// same shared predicate `blind_swap` decides on, so the report and the decision cannot drift. The
/// interim-const duration gate (not the config `session_blind_swap_secs`) matches the anchor arm's:
/// the status reflects the TRUE degraded state even when the kill-switch has disabled the swap,
/// exactly as `blind_gate_armed` does for the SLIs.
pub(super) fn blind_active_view(
    anchor: AnchorArmInputs,
    active_is_blind: bool,
    quarantined: bool,
    server_retry_after_hold: bool,
    velocity: Option<VelocityEma>,
    session_ceiling: f64,
    at: Instant,
) -> Option<BlindActive> {
    if quarantined || !active_is_blind {
        return None;
    }
    // Bind the mark BEFORE the anchor is shadowed to its inner `LastGood` below.
    let high_water = anchor.high_water;
    let anchor = anchor.last_good?;
    let blind_secs = at.saturating_duration_since(anchor.at).as_secs();
    // Issue #619: the anchor ARM decides on the anchor's PLAUSIBLE session (raised to the frozen
    // high-water mark when the pre-blind reading was stale-low), the SAME value `blind_swap` /
    // `note_blind_gate_eligibility` decide on — so `status` never reports "auto-protection OK" while
    // the corrected gate is actually armed and swapping. The DISPLAYED `last_known_session_pct`
    // below keeps the RAW anchor — a measurement of what was last observed. The #584 velocity base
    // ALSO stays raw, but as an out-of-scope DECISION read, not a measurement: #619 covers the #452
    // gate only, so an anchor corrected to below the risk band still projects that arm off the
    // stale-low value (see `swap::plausible_anchor_session`).
    let gate_session = swap::plausible_anchor_session(high_water, anchor.session);
    Some(BlindActive {
        blind_secs,
        last_known_session_pct: to_pct(anchor.session),
        auto_protection_degraded: blind_gate_armed(blind_secs, gate_session)
            || (blind_secs > BLIND_GATE_SECS && server_retry_after_hold)
            || blind_velocity_projected_armed(
                blind_secs,
                anchor.session,
                velocity,
                session_ceiling,
            ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An epoch-second session-window stamp for the #619 plausibility case below. A local copy of
    /// `daemon`'s same-named test helper (which carries it for the #614 tests): test helpers are
    /// private to their own `mod tests`, so the split leaves both modules self-contained rather than
    /// coupling one test module to the other.
    const WINDOW: i64 = 1_800_000_000;

    #[test]
    fn decision_class_maps_every_tick_action() {
        // 1:1 and total over the variants (#77); swap participants are dropped — the
        // decision line is a pure label.
        assert_eq!(TickAction::Held.decision_class(), DecisionClass::Hold);
        assert_eq!(
            TickAction::Swapped { from: 0, to: 1 }.decision_class(),
            DecisionClass::Swap
        );
        assert_eq!(
            TickAction::EmergencySwapped { from: 0, to: 1 }.decision_class(),
            DecisionClass::EmergencySwap
        );
        assert_eq!(
            TickAction::ActiveDeadNoTarget.decision_class(),
            DecisionClass::ActiveDeadNoTarget
        );
        assert_eq!(
            TickAction::NoViableTarget.decision_class(),
            DecisionClass::AllExhausted
        );
        assert_eq!(
            TickAction::SkippedActiveUnknown.decision_class(),
            DecisionClass::SkipActiveUnknown
        );
        assert_eq!(
            TickAction::SkippedActiveUnavailable.decision_class(),
            DecisionClass::SkipActiveUnavailable
        );
        assert_eq!(
            TickAction::SkippedCooldown.decision_class(),
            DecisionClass::SkipCooldown
        );
        assert_eq!(
            TickAction::SwapFailed.decision_class(),
            DecisionClass::SwapFailed
        );
        assert_eq!(
            TickAction::KeychainLocked.decision_class(),
            DecisionClass::KeychainLocked
        );
    }

    #[test]
    fn blind_active_view_projects_ok_below_the_gate_and_degraded_past_it() {
        // Issue #479: the bounded-blindness projection is a PURE function of the retained anchor,
        // the blind predicate, the quarantine flag, and the monotonic clock. A base instant + fixed
        // deltas make `blind_elapsed` deterministic regardless of wall time.
        let base = Instant::now();
        let near_band = LastGood {
            session: 0.70,
            weekly: 0.20,
            at: base,
        };

        // Blind, anchor in-band, but at EXACTLY T → OK: the gate arms on a strict `>` (matching
        // `note_blind_gate_eligibility`), so at T it is not yet armed. No server hold either.
        let ok = blind_active_view(
            // No high-water mark → no #619 correction: these cases exercise the raw anchor.
            AnchorArmInputs {
                last_good: Some(near_band),
                high_water: None,
            },
            true,
            false,
            false,
            // No retained velocity (and a representative base trigger): these cases exercise the anchor +
            // server arms only, so the #584 velocity arm stays inert — covered by its own test below.
            None,
            0.95,
            base + Duration::from_secs(BLIND_GATE_SECS),
        )
        .expect("a blind active account with an anchor projects a state");
        assert_eq!(ok.blind_secs, BLIND_GATE_SECS);
        assert_eq!(ok.last_known_session_pct, 70);
        assert!(
            !ok.auto_protection_degraded,
            "at exactly T the strict `>` gate is not yet armed",
        );

        // One second past T with the anchor in-band → DEGRADED (mirrors the gate exactly).
        let degraded = blind_active_view(
            AnchorArmInputs {
                last_good: Some(near_band),
                high_water: None,
            },
            true,
            false,
            false,
            None,
            0.95,
            base + Duration::from_secs(BLIND_GATE_SECS + 1),
        )
        .expect("still projects a state past T");
        assert!(
            degraded.auto_protection_degraded,
            "past T with the anchor at/over the risk band is DEGRADED",
        );
        assert_eq!(degraded.blind_secs, BLIND_GATE_SECS + 1);

        // Long past T, the anchor BELOW the band, and NO server hold → OK: the anchor arm would
        // never arm on it. (Issue #582 adds the server arm below; absent a directive the below-band
        // account is still nominally protected, so this half of the old invariant stands.)
        let below_band = LastGood {
            session: BLIND_GATE_RISK_BAND - 0.01,
            weekly: 0.10,
            at: base,
        };
        let ok_below = blind_active_view(
            AnchorArmInputs {
                last_good: Some(below_band),
                high_water: None,
            },
            true,
            false,
            false,
            None,
            0.95,
            base + Duration::from_secs(BLIND_GATE_SECS + 100),
        )
        .expect("blind with an anchor still projects");
        assert!(
            !ok_below.auto_protection_degraded,
            "a below-band anchor with no server hold and no velocity signal is not DEGRADED, \
             however long it is blind",
        );

        // Issue #582: the SAME below-band anchor, past T, but a server `Retry-After` IS holding it
        // off → DEGRADED. This is the regression the issue named — `status` must NOT report
        // "auto-protection OK" while the daemon sits on a below-band account blind behind a
        // directive. At exactly T the strict `>` gate is not yet armed, mirroring the anchor arm.
        let degraded_below = blind_active_view(
            AnchorArmInputs {
                last_good: Some(below_band),
                high_water: None,
            },
            true,
            false,
            true,
            None,
            0.95,
            base + Duration::from_secs(BLIND_GATE_SECS + 1),
        )
        .expect("blind with an anchor still projects");
        assert!(
            degraded_below.auto_protection_degraded,
            "a below-band anchor held by a server Retry-After past T IS DEGRADED (issue #582)",
        );
        let ok_below_at_t = blind_active_view(
            AnchorArmInputs {
                last_good: Some(below_band),
                high_water: None,
            },
            true,
            false,
            true,
            None,
            0.95,
            base + Duration::from_secs(BLIND_GATE_SECS),
        )
        .expect("blind with an anchor still projects");
        assert!(
            !ok_below_at_t.auto_protection_degraded,
            "the server arm shares the strict `>` T gate — not yet armed at exactly T",
        );
    }

    #[test]
    fn blind_active_view_is_none_when_not_bounded_blindness() {
        let base = Instant::now();
        let anchor = LastGood {
            session: 0.70,
            weekly: 0.20,
            at: base,
        };
        let past_t = base + Duration::from_secs(BLIND_GATE_SECS + 1);
        // A live reading (not blind) → no projection, even with an in-band anchor past T. (A server
        // hold cannot exist without blindness anyway; `false` here, and it makes no difference.)
        let anchor_only = AnchorArmInputs {
            last_good: Some(anchor),
            high_water: None,
        };
        assert!(blind_active_view(anchor_only, false, false, false, None, 0.95, past_t).is_none());
        // Blind but NO retained anchor (a genuinely-unknown active) → no spurious projection (#450),
        // even with a server hold — the #582 arm keys off the anchor too, exactly like `blind_swap`.
        let no_anchor = AnchorArmInputs {
            last_good: None,
            high_water: None,
        };
        assert!(blind_active_view(no_anchor, true, false, true, None, 0.95, past_t).is_none());
        // Blind WITH an anchor but QUARANTINED (dead, #42) → the emergency_swap path owns it, not
        // bounded blindness (ADR-0017 keeps the two separate).
        assert!(blind_active_view(anchor_only, true, true, false, None, 0.95, past_t).is_none());
    }

    #[test]
    fn blind_active_view_velocity_arm_degrades_a_fast_below_band_blind_account() {
        // Issue #584: the false-"OK" bug. A BELOW-band anchor (the anchor arm can never fire on it) that
        // was climbing fast when it went blind can burn to the trigger UNSEEN inside the blind window; the
        // velocity-projection arm reports it DEGRADED even though no swap acts on it (report-only).
        let base = Instant::now();
        let trigger = 0.95;
        // The 2026-07-17 incident shape: anchor 0.29, well below the 0.60 band, no server hold, climbing.
        let below_band = LastGood {
            session: 0.29,
            weekly: 0.10,
            at: base,
        };
        // A SUSTAINED climb (samples >= MIN_VELOCITY_SAMPLES), ~3 %/min = 0.0005 session-fraction/s.
        let climbing = Some(VelocityEma {
            rate: 0.0005,
            samples: MIN_VELOCITY_SAMPLES,
        });
        // Blind ~23 min past the anchor: 0.29 + 0.0005 × 1.75 × 1380 ≈ 1.50 ≫ 0.95 → DEGRADED, on the
        // velocity arm ALONE (anchor below band, no server hold — the two older arms are both silent).
        let degraded = blind_active_view(
            AnchorArmInputs {
                last_good: Some(below_band),
                high_water: None,
            },
            true,
            false,
            false,
            climbing,
            trigger,
            base + Duration::from_secs(1380),
        )
        .expect("a blind active account with an anchor projects a state");
        assert!(
            degraded.auto_protection_degraded,
            "a below-band anchor climbing fast enough to reach the trigger inside the blind window is \
             DEGRADED, not OK (issue #584)",
        );

        // The inflation factor is LOAD-BEARING, not decoration: a near-miss where the POINT estimate stays
        // below the trigger but the bias-HIGH bound crosses it must still degrade. anchor 0.50, blind
        // 1000 s, rate 0.0003 → point 0.50 + 0.30 = 0.80 < 0.95, inflated 0.50 + 0.525 = 1.025 ≥ 0.95.
        assert!(
            0.50 + 0.0003 * 1000.0 < trigger,
            "fixture sanity: the point estimate is below the trigger, so only the inflation can fire",
        );
        let near_miss_vel = Some(VelocityEma {
            rate: 0.0003,
            samples: MIN_VELOCITY_SAMPLES,
        });
        assert!(
            blind_velocity_projected_armed(1000, 0.50, near_miss_vel, trigger),
            "the bias-HIGH inflation bound catches a near-miss the point estimate would clear (issue #584)",
        );

        // Negatives — the arm never trips on:
        //   * a missing velocity signal (a first/failed poll, or a window-drop reset),
        assert!(!blind_velocity_projected_armed(1380, 0.29, None, trigger));
        //   * an UNSUSTAINED single-interval spike (samples < MIN_VELOCITY_SAMPLES), however steep,
        let spike = Some(VelocityEma {
            rate: 0.01,
            samples: MIN_VELOCITY_SAMPLES - 1,
        });
        assert!(!blind_velocity_projected_armed(1380, 0.29, spike, trigger));
        //   * exactly T — the strict `>` floor shared with the anchor + server arms — even for a rate
        //     steep enough to cross past it (armed one second later),
        let fast = Some(VelocityEma {
            rate: 0.01,
            samples: MIN_VELOCITY_SAMPLES,
        });
        assert!(!blind_velocity_projected_armed(
            BLIND_GATE_SECS,
            0.29,
            fast,
            trigger
        ));
        assert!(blind_velocity_projected_armed(
            BLIND_GATE_SECS + 1,
            0.29,
            fast,
            trigger
        ));
        //   * a rate too shallow to reach the trigger within the window.
        let crawl = Some(VelocityEma {
            rate: 0.00001,
            samples: MIN_VELOCITY_SAMPLES,
        });
        assert!(!blind_velocity_projected_armed(1380, 0.29, crawl, trigger));
    }

    #[test]
    fn blind_active_view_degrades_on_a_stale_low_anchor_corrected_into_band() {
        // Issue #619: the status projection tracks the CORRECTED gate, not the raw anchor. A pre-blind
        // anchor whose RAW session sits below the risk band but whose frozen high-water mark stands
        // in-band is a stale-low reading — the #452 gate arms and swaps on the corrected value, so
        // `status` must ALSO degrade, never report false-"OK" while the daemon is actually protecting
        // (the one-sided #479/#582/#584 honesty invariant, reached here through the #619 correction).
        let base = Instant::now();
        let past_t = base + Duration::from_secs(BLIND_GATE_SECS + 1);
        let stale_low = LastGood {
            session: BLIND_GATE_RISK_BAND - 0.20, // raw, below the band
            weekly: 0.20,
            at: base,
        };
        // The window's TRUE high-water (an earlier plausible reading), in-band.
        let mark = swap::SessionHighWater::fold(
            None,
            &Usage {
                session: BLIND_GATE_RISK_BAND + 0.10,
                weekly: 0.20,
                weekly_resets_at: None,
                session_resets_at: Some(WINDOW),
            },
        );

        // Control — on the RAW anchor (no mark) the arm reads below the band and reports OK: the bug.
        let raw = blind_active_view(
            AnchorArmInputs {
                last_good: Some(stale_low),
                high_water: None,
            },
            true,
            false,
            false,
            None,
            0.95,
            past_t,
        )
        .expect("a blind active account with an anchor projects a state");
        assert!(
            !raw.auto_protection_degraded,
            "control: keyed off the raw stale-low anchor the arm reports OK — the bug #619 fixes",
        );

        // Corrected against the mark → DEGRADED, mirroring the swap `blind_swap` actually fires.
        let corrected = blind_active_view(
            AnchorArmInputs {
                last_good: Some(stale_low),
                high_water: mark,
            },
            true,
            false,
            false,
            None,
            0.95,
            past_t,
        )
        .expect("still projects a state past T");
        assert!(
            corrected.auto_protection_degraded,
            "past T a stale-low anchor corrected into the band is DEGRADED, not false-OK (issue #619)",
        );
        // The DISPLAYED last-known pct stays the RAW measurement — only the arm decision is corrected.
        assert_eq!(
            corrected.last_known_session_pct,
            to_pct(stale_low.session),
            "the displayed last-known % is the raw anchor, never the synthesized correction",
        );
    }
}
