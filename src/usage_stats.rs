// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Usage aggregation over a period (issue #157).
//!
//! A **pure, synchronous** function, [`aggregate`], that folds a slice of raw
//! [`Sample`]s (issue #155) plus the swap events parsed out of the structured event
//! log ([`crate::observability`]) into per-account and roster-level statistics for a
//! single period. It reads no files and holds no state: the caller (the #158 `stats`
//! verb) supplies the already-read samples ([`crate::usage_store::read_samples`]) and
//! the event-log text; this module only computes.
//!
//! # What it produces
//!
//! Per account, over the period:
//!
//! * mean / peak / p95 utilisation for BOTH quota dimensions (session + weekly),
//!   mirroring the store's own refusal to blend the two windows into one scalar;
//! * `cap_hits` — samples whose session utilisation is at or above the trigger
//!   (`session >= session_cap`, inclusive per the AC);
//! * `time_at_cap_secs` — the sampled time spent at/above that cap;
//! * `contribution_share` — the fraction of the period's observations made while this
//!   account was the swapped-in (active) credential, reconstructed from the swap-active
//!   spans (see [`contribution`](AccountStats::contribution_share));
//! * `coverage` — `seen ÷ expected`, how complete the account's data is.
//!
//! Roster-wide, over the period:
//!
//! * `swap_count` + a per-reason [`SwapBreakdown`] — the swap frequency, INCLUDING the
//!   manual `sessiometer use` verb (`reason=manual|forced`) and emergency swaps;
//! * `all_high_episodes` + `all_high_secs` — the count and total duration of intervals
//!   during which EVERY rostered account was simultaneously at/above the session
//!   high-water threshold: a UTILISATION CENSUS ("was the roster running hot?"), NOT a
//!   capacity claim (issue #804 — "could the daemon still swap?" is its own fact);
//! * `all_high_covered_secs` + `high_threshold` — the census's own denominator and water,
//!   so a reader can tell "measured, and it never happened" from "never measurable"
//!   (issue #804) and every surface can state the threshold it actually used.
//!
//! # Gap honesty — a missing sample is UNKNOWN, never zero
//!
//! The store is sampled, not continuous, and a poll can be missed. This module NEVER
//! invents a reading: a gap contributes nothing (it is not counted as `0.0`, as
//! "healthy", or as "exhausted"). Concretely,
//!
//! * every per-account metric is computed over that account's OBSERVED samples only;
//! * each reading "covers" only a bounded window forward — `[ts, min(next_ts,
//!   ts + stale_after))` — so a gap wider than `stale_after` leaves genuinely UNKNOWN
//!   time that no metric fills (this drives `time_at_cap_secs` and the all-high
//!   episodes);
//! * `coverage = seen ÷ expected` is reported per account/period so a consumer can
//!   annotate a sparsely-sampled period rather than misread it as calm;
//! * an all-high episode requires every rostered account to be KNOWN-and-high at the
//!   instant — if any account has no covering sample there, the instant is UNKNOWN and
//!   is NOT part of an episode.
//!
//! # Time discipline
//!
//! Everything is whole UTC epoch seconds, end to end — the same currency the store and
//! event log already speak — so month lengths and daylight-saving transitions are
//! non-events: there is no civil-calendar arithmetic to get wrong. A [`Period`] is a
//! half-open `[start, end)` window (**inclusive start, exclusive end**), so abutting
//! periods partition the timeline with no sample lost or double-counted. Cap-hit
//! membership is inclusive (`>=`).
//!
//! # Units — fractions, not percents
//!
//! A [`Sample`]'s `session`/`weekly` are fractions in `[0.0, …]` (`1.0` = exhausted),
//! whereas the config triggers ([`crate::config`]) are integer PERCENTS. The thresholds
//! in [`AggregateParams`] are therefore FRACTIONS, in the sample's own units; the #158
//! caller converts config percents once (e.g. via `Config::swap_threshold`) before
//! calling in, so this module never has to reason about the mismatch.
//!
//! # Not-yet-wired seam
//!
//! Like [`crate::usage_store`], this module is pure data-plumbing that the binary does
//! not call yet — the #158 `stats` verb wires [`aggregate`] to the read path and the
//! CLI. Until then every item here is unused by the binary itself (main.rs only
//! declares the module), hence the module-level `dead_code` allowance, mirroring the
//! store and [`crate::migration`].

// See the "Not-yet-wired seam" note above: #158 wires the CLI caller.
#![allow(dead_code)]

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::swap::ViabilityBoundary;
use crate::usage::epoch_from_rfc3339;
use crate::usage_store::Sample;

/// A half-open aggregation window in whole UTC epoch seconds: **inclusive start,
/// exclusive end**. Abutting periods (`[a, b)` then `[b, c)`) therefore partition the
/// timeline exactly — every sample lands in exactly one, none is lost or double-counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Period {
    /// Inclusive lower bound (UTC epoch seconds).
    pub(crate) start: i64,
    /// Exclusive upper bound (UTC epoch seconds).
    pub(crate) end: i64,
}

impl Period {
    /// A window `[start, end)`. A caller may pass `start > end`; such a window simply
    /// contains nothing (every metric is empty), never a panic.
    pub(crate) fn new(start: i64, end: i64) -> Self {
        Self { start, end }
    }

    /// Whether `ts` falls in `[start, end)` — inclusive start, exclusive end.
    fn contains(&self, ts: i64) -> bool {
        ts >= self.start && ts < self.end
    }

    /// The window's length in seconds (`0` for an empty/inverted window). Crate-visible since
    /// issue #804: the roster line needs it as the denominator for the census's coverage
    /// annotation, and re-deriving `end - start` at the render would be a second, driftable
    /// definition of the same quantity.
    pub(crate) fn duration(&self) -> i64 {
        (self.end - self.start).max(0)
    }
}

/// Why a swap happened — the parsed `reason=` of a swap event, plus [`Emergency`] for
/// an `event=emergency_swap` (which carries no reason). Mirrors
/// [`crate::observability::SwapReason`] but is re-declared here to keep this module
/// self-contained (it depends on the log GRAMMAR, not the writer's types).
///
/// [`Emergency`]: SwapKind::Emergency
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SwapKind {
    /// `event=swap reason=session` — the session-window trigger fired.
    Session,
    /// `event=swap reason=weekly` — the weekly-window trigger fired.
    Weekly,
    /// `event=swap reason=manual` — an operator `sessiometer use` whose gate passed.
    Manual,
    /// `event=swap reason=forced` — an operator `sessiometer use --force`.
    Forced,
    /// `event=emergency_swap` — a bypass swap away from a dead/quarantined credential.
    Emergency,
    /// `event=swap reason=blind_preempt` (#452) OR `reason=velocity_preempt` (#539) — a
    /// preemptive swap-away (ADR-0017), the two folded onto ONE kind: both are reliability
    /// tail-risk guards (excluded from `swap_count`/`swap_breakdown`, surfaced instead by
    /// `sessiometer reliability`) that still bound the contribution timeline. #452 swaps a
    /// BLIND active before it self-exhausts unobserved; #539 swaps a still-observed active
    /// whose PROJECTED usage would cross the trigger within the horizon.
    Preempt,
}

/// One swap parsed out of the event log: WHO the active credential moved from/to, WHEN
/// (UTC epoch seconds), and WHY. The full ordered list of these reconstructs the
/// active-account timeline that [`aggregate`] overlays onto the samples for
/// contribution share, and — filtered to the period — is the swap frequency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SwapEvent {
    /// When the swap happened (UTC epoch seconds).
    pub(crate) ts: i64,
    /// The account handle that WAS active before the swap.
    pub(crate) from: String,
    /// The account handle that became active after the swap.
    pub(crate) to: String,
    /// Why the swap happened.
    pub(crate) kind: SwapKind,
}

/// The knobs [`aggregate`] needs, all in the SAMPLE's own units (fractions, not
/// percents — see the module note on units). The #158 caller derives these from
/// [`crate::config`] once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AggregateParams {
    /// The expected poll cadence in seconds — the coverage denominator (`expected =
    /// period ÷ cadence`) and the default forward-coverage window per reading.
    pub(crate) poll_interval_secs: i64,
    /// The session utilisation fraction at/above which a reading is a cap-hit
    /// (`session >= session_cap`, inclusive). The session trigger as a fraction.
    pub(crate) session_cap: f64,
    /// The utilisation fraction at/above which an account counts toward an
    /// all-accounts-high episode (applied to the session dimension).
    pub(crate) high_threshold: f64,
    /// How long a single reading "covers" forward before its value is treated as
    /// UNKNOWN (gap honesty). A reading at `ts` is valid over `[ts, min(next_ts,
    /// ts + stale_after_secs))`; a gap wider than this leaves genuinely unknown time.
    /// Defaults to `poll_interval_secs` in [`AggregateParams::new`].
    pub(crate) stale_after_secs: i64,
    /// The daemon's viability boundary (issue #803), when the caller knows it — the pair of
    /// lines at/above which an account cannot be swapped TO. Supplied by the caller from
    /// [`crate::config`], exactly as the other knobs here are.
    ///
    /// `None` means the capacity-holds census is NOT TAKEN: without the daemon's own boundary
    /// there is no honest predicate for it, and inventing one would report a fact about a daemon
    /// that does not exist. The aggregate then yields zero jointly-covered seconds, which every
    /// surface must read as UNKNOWN rather than as a calm `0 holds` (REQ-STA-B-010, refining
    /// REQ-STA-B-008). It is deliberately NOT defaulted to a plausible-looking pair.
    pub(crate) viability: Option<ViabilityBoundary>,
}

impl AggregateParams {
    /// Params with `stale_after_secs` defaulted to the poll cadence — a reading covers
    /// exactly one nominal poll interval forward unless a newer reading supersedes it — and
    /// NO viability boundary (the capacity-holds census is not taken; chain
    /// [`with_viability`](AggregateParams::with_viability) to take it).
    pub(crate) fn new(poll_interval_secs: i64, session_cap: f64, high_threshold: f64) -> Self {
        Self {
            poll_interval_secs,
            session_cap,
            high_threshold,
            stale_after_secs: poll_interval_secs,
            viability: None,
        }
    }

    /// Supply the daemon's viability boundary, enabling the capacity-holds census. Mirrors
    /// [`Sample::with_resets`]'s builder idiom.
    pub(crate) fn with_viability(mut self, viability: ViabilityBoundary) -> Self {
        self.viability = Some(viability);
        self
    }
}

/// One quota dimension's central statistics over an account's period samples.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub(crate) struct DimStats {
    /// Arithmetic mean utilisation over the observed samples.
    pub(crate) mean: f64,
    /// Peak (max) utilisation over the observed samples.
    pub(crate) peak: f64,
    /// 95th-percentile utilisation (nearest-rank), matching the store's daily tier.
    pub(crate) p95: f64,
}

/// Everything computed for one account over one period.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub(crate) struct AccountStats {
    /// Observed samples for this account in the period.
    pub(crate) seen: u32,
    /// Samples that a fully-covered period would hold at the poll cadence
    /// (`period ÷ cadence`, at least 1) — the coverage denominator.
    pub(crate) expected: f64,
    /// `seen ÷ expected`, clamped to `1.0`. Below 1 means the period is under-sampled
    /// for this account and its metrics should be read with that caveat.
    pub(crate) coverage: f64,
    /// Session-dimension mean/peak/p95.
    pub(crate) session: DimStats,
    /// Weekly-dimension mean/peak/p95.
    pub(crate) weekly: DimStats,
    /// Samples with `session >= session_cap` (inclusive at the boundary).
    pub(crate) cap_hits: u32,
    /// Sampled seconds spent at/above the session cap — the summed forward-coverage
    /// windows of the cap-hit samples (gap-honest: a gap adds nothing).
    pub(crate) time_at_cap_secs: i64,
    /// The fraction of the period's observations made while THIS account was the active
    /// (swapped-in) credential, from the swap-active spans. Across all accounts these
    /// shares sum to 1 (a single active account → 1.0); `0.0` for an account that held
    /// samples but was never the active credential in-period.
    pub(crate) contribution_share: f64,
}

/// The swap frequency broken out by reason. `swap_count` on [`RosterStats`] is the sum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub(crate) struct SwapBreakdown {
    /// Autonomous session-trigger swaps.
    pub(crate) session: u32,
    /// Autonomous weekly-trigger swaps.
    pub(crate) weekly: u32,
    /// Manual `sessiometer use` swaps whose gate passed.
    pub(crate) manual: u32,
    /// Manual `sessiometer use --force` swaps.
    pub(crate) forced: u32,
    /// Emergency (bypass) swaps.
    pub(crate) emergency: u32,
}

/// Roster-wide statistics over the period.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub(crate) struct RosterStats {
    /// Total swaps in the period (all reasons, including manual and emergency).
    pub(crate) swap_count: u32,
    /// The `swap_count` split by reason.
    pub(crate) swaps: SwapBreakdown,
    /// Number of maximal intervals during which every rostered account was
    /// simultaneously KNOWN-and-at/above [`AggregateParams::high_threshold`].
    ///
    /// Meaningful ONLY alongside `all_high_covered_secs`: with no jointly-covered instant
    /// there is nothing to count, so this reads `0` for a window that was never
    /// measurable. Surfaces MUST consult the denominator and render UNKNOWN rather than a
    /// bare `0` — an unmeasurable period is not a calm one (issue #804, REQ-STA-B-008).
    pub(crate) all_high_episodes: u32,
    /// Total duration (seconds) of those all-accounts-high intervals. Same caveat as the
    /// count above.
    pub(crate) all_high_secs: i64,
    /// Seconds of the period during which EVERY account in the census set was
    /// SIMULTANEOUSLY covered (each holding a non-gap reading) — the denominator the two
    /// figures above were measured over, and the coverage figure REQ-STA-B-008 requires
    /// this aggregate to carry.
    ///
    /// `0` means the census was never measurable at all: no instant existed at which the
    /// whole roster was observable, so "0 episodes" is UNKNOWN, not calm. That zero is a
    /// measured quantity, not a sentinel — jointly-covered time really was nil.
    pub(crate) all_high_covered_secs: i64,
    /// The session-utilisation water an account had to be at/above to count toward an
    /// episode — [`AggregateParams::high_threshold`], carried through so every surface can
    /// state the value it ACTUALLY used instead of hardcoding a literal (issue #804).
    pub(crate) high_threshold: f64,
    /// Number of maximal CAPACITY HOLDS in the period (issue #803, REQ-STA-B-010): intervals
    /// during which EVERY rostered account was simultaneously non-viable at the daemon's own
    /// viability boundary, so swapping could not have restored capacity.
    ///
    /// A distinct fact from `all_high_episodes`, not a variant of it. That one is the
    /// UTILISATION census ("was the roster running hot?"); this is the CAPACITY fact ("could the
    /// daemon still swap?"). A roster can run hot for a week without ever cornering the daemon,
    /// and — the case this metric exists for — it can corner the daemon while the census reads
    /// calm. Do not merge or substitute the two.
    ///
    /// Read ONLY against `capacity_hold_covered_secs`, which is `0` when the census could not be
    /// taken at all; the accompanying `0` is then UNKNOWN, never calm.
    pub(crate) capacity_holds: u32,
    /// The `capacity_holds` whose relief was gated by a SESSION window.
    pub(crate) capacity_holds_session: u32,
    /// The `capacity_holds` whose relief was gated by a WEEKLY window. Splits `capacity_holds`
    /// exactly (`session + weekly == capacity_holds`) — this census names every hold one or the
    /// other, in the daemon's own `cause=` vocabulary so the split reconciles against its
    /// `all_exhausted` events (which are the ORACLE for these figures, never their source).
    pub(crate) capacity_holds_weekly: u32,
    /// Total held seconds — a LOWER BOUND, never an exact figure (REQ-STA-B-011), which is why
    /// the name says so and why surfaces render it with a `≥`.
    ///
    /// Two independent reasons it can only bound the truth, both structural rather than
    /// provisional: a coverage gap inside a hold truncates it (unknown time is never assumed
    /// held), and a hold still running at the period's end is clipped to that end. The
    /// closing instant is likewise ANCHORED to the blocking window's own carried reset rather
    /// than observed — the account is known to stay blocked at least that long, not to un-block
    /// exactly then.
    pub(crate) capacity_hold_secs_lower_bound: i64,
    /// Seconds during which EVERY account in the census set was SIMULTANEOUSLY covered under the
    /// reset-anchored windows — the denominator the four figures above were measured over.
    ///
    /// `0` means the census was never taken or never measurable, so `capacity_holds: 0` is
    /// UNKNOWN rather than calm. Surfaces MUST consult it and render their own gap sentinel,
    /// exactly as REQ-STA-B-008 already requires of the utilisation census.
    pub(crate) capacity_hold_covered_secs: i64,
    /// The SESSION line an account had to be at/above to count as non-viable
    /// ([`ViabilityBoundary::session`]), carried out so every surface states the value actually
    /// used — the same lesson `high_threshold` encodes for the census (issue #804). Meaningful
    /// only when `capacity_hold_covered_secs > 0`.
    pub(crate) capacity_session_line: f64,
    /// The WEEKLY line, likewise ([`ViabilityBoundary::weekly`]) — `0.97` at defaults, NOT the
    /// raw `0.98` ceiling. Meaningful only when `capacity_hold_covered_secs > 0`.
    pub(crate) capacity_weekly_line: f64,
}

/// The full aggregation result for one period.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct UsageReport {
    /// The window these statistics cover.
    pub(crate) period: Period,
    /// Per-account statistics, keyed by the account's redacted handle (sorted).
    pub(crate) per_account: BTreeMap<String, AccountStats>,
    /// Roster-wide statistics.
    pub(crate) roster: RosterStats,
}

/// Parse the swap and emergency-swap events out of the structured event-log `text`.
///
/// Tolerant, forward-only, and self-contained: it reads the flat `key=val` grammar
/// ([`crate::observability`]) line by line, keeps the `event=swap` /
/// `event=emergency_swap` lines, and skips everything else — other event kinds, blank
/// lines, and any line missing a required field or with an unrecognised `reason=`
/// (mirroring the store's tolerant read). Timestamps go through the crate's one
/// canonical [`epoch_from_rfc3339`] parser, so a swap with an unparseable `ts=` is
/// dropped rather than mis-timed. Returns events in the log's own (chronological) order;
/// [`aggregate`] sorts defensively regardless.
pub(crate) fn parse_swap_events(text: &str) -> Vec<SwapEvent> {
    let mut events = Vec::new();
    for line in text.lines() {
        // Build a field map from the whitespace-separated `key=val` tokens. Handles are
        // whitespace-free by the log's own grammar, so tokenising on spaces is exact.
        let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
        for token in line.split_whitespace() {
            if let Some((key, val)) = token.split_once('=') {
                fields.insert(key, val);
            }
        }

        let kind = match fields.get("event").copied() {
            Some("swap") => match fields.get("reason").copied() {
                Some("session") => SwapKind::Session,
                Some("weekly") => SwapKind::Weekly,
                Some("manual") => SwapKind::Manual,
                Some("forced") => SwapKind::Forced,
                Some("blind_preempt") => SwapKind::Preempt,
                // The #539 velocity-projection preemptive swap is folded onto the SAME `Preempt`
                // kind as the #452 blind-preempt: both are reliability-concern tail-risk guards
                // (surfaced by `sessiometer reliability`, not a usage-frequency reason), both
                // excluded from `swap_count`/`swap_breakdown`, and — the load-bearing reason NOT to
                // drop it — both must stay in the `swaps` list so they bound the contribution
                // timeline (`active_at`). Dropping it would misattribute every post-swap sample to
                // the departed account. Default-reachable (horizon default 120s ⇒ path ON).
                Some("velocity_preempt") => SwapKind::Preempt,
                // A swap with a missing/unknown reason is malformed for our purposes —
                // skip it rather than guess a reason (tolerant-drop).
                _ => continue,
            },
            Some("emergency_swap") => SwapKind::Emergency,
            _ => continue,
        };

        let (Some(ts_raw), Some(from), Some(to)) = (
            fields.get("ts").copied(),
            fields.get("from").copied(),
            fields.get("to").copied(),
        ) else {
            continue;
        };
        let Some(ts) = epoch_from_rfc3339(ts_raw) else {
            continue;
        };

        events.push(SwapEvent {
            ts,
            from: from.to_owned(),
            to: to.to_owned(),
            kind,
        });
    }
    events
}

/// Aggregate `samples` and `swaps` over `period` into a [`UsageReport`].
///
/// Pure and total — it never reads a file, holds no state, and cannot fail: malformed
/// or out-of-period inputs are simply excluded, never a panic. `samples` and `swaps`
/// may be in any order (both are used order-independently; swaps are sorted internally
/// for the active-account timeline). Output is deterministic: per-account results are a
/// [`BTreeMap`] keyed by handle, and every metric is a pure function of the inputs.
///
/// The all-accounts-high census is taken over the accounts that HOLD SAMPLES in the period,
/// which fires more readily than over a known roster (an unsampled account cannot withhold
/// it). A caller that knows the configured roster should pass it to
/// [`aggregate_with_roster`] instead — this is exactly that call with `None` (issue #804).
pub(crate) fn aggregate(
    samples: &[Sample],
    swaps: &[SwapEvent],
    period: Period,
    params: &AggregateParams,
) -> UsageReport {
    aggregate_with_roster(samples, swaps, period, params, None)
}

/// [`aggregate`], with the CONFIGURED roster supplied so the all-accounts-high census is
/// taken over it rather than over whoever happens to hold samples (issue #804).
///
/// `roster` is the set of handles the census must cover. A rostered account with ZERO
/// samples in the period stays IN the intersection and contributes no covering interval —
/// so it cannot silently leave and make the metric fire more easily, and its absence is
/// reported as UNKNOWN (`all_high_covered_secs == 0`) rather than as a calm `0 episodes`.
/// It also keeps ORPHAN handles (samples from a removed/renamed account, issue #314) out
/// of the census, which the sampled-accounts form wrongly admitted.
///
/// `None` means the caller does not know the roster (no readable config): the census then
/// degrades to the accounts present in the period's samples — the pre-#804 behaviour, and
/// the honest fallback when there is no configured set to intersect over. Everything else
/// is identical to [`aggregate`], which is exactly this call with `None`.
pub(crate) fn aggregate_with_roster(
    samples: &[Sample],
    swaps: &[SwapEvent],
    period: Period,
    params: &AggregateParams,
    roster: Option<&BTreeSet<String>>,
) -> UsageReport {
    // Samples that fall in [start, end). References only — no copies of the readings.
    let in_period: Vec<&Sample> = samples.iter().filter(|s| period.contains(s.ts)).collect();

    // Group by account handle, each group sorted by ts (validity windows need order).
    let mut by_acct: BTreeMap<&str, Vec<&Sample>> = BTreeMap::new();
    for &s in &in_period {
        by_acct.entry(s.acct.as_str()).or_default().push(s);
    }
    for group in by_acct.values_mut() {
        group.sort_by_key(|s| s.ts);
    }

    // Contribution: attribute each in-period observation to whichever account was the
    // active (swapped-in) credential at its instant, per the swap-active spans.
    let contribution = contribution_counts(&in_period, swaps);
    let total_obs = in_period.len() as f64;

    // Coverage denominator: how many samples a fully-covered period would hold.
    let expected = (period.duration() as f64 / params.poll_interval_secs.max(1) as f64).max(1.0);

    let mut per_account: BTreeMap<String, AccountStats> = BTreeMap::new();
    for (&acct, group) in &by_acct {
        let session: Vec<f64> = group.iter().map(|s| s.session).collect();
        let weekly: Vec<f64> = group.iter().map(|s| s.weekly).collect();
        let seen = group.len() as u32;

        let windows = validity_windows(group, period, params.stale_after_secs);
        let cap_hits = session.iter().filter(|&&v| v >= params.session_cap).count() as u32;
        let time_at_cap_secs = windows
            .iter()
            .zip(group.iter())
            .filter(|(_, s)| s.session >= params.session_cap)
            .map(|((lo, hi), _)| hi - lo)
            .sum();

        let share = share_of(&contribution, acct, total_obs);
        per_account.insert(
            acct.to_owned(),
            AccountStats {
                seen,
                expected,
                coverage: (f64::from(seen) / expected).min(1.0),
                session: dim_stats(&session),
                weekly: dim_stats(&weekly),
                cap_hits,
                time_at_cap_secs,
                contribution_share: share,
            },
        );
    }

    // An account that was active for some observations but never itself sampled in the
    // period (the daemon polled a different account) still holds a contribution share —
    // record it with zeroed readings so the shares always sum to 1 (gap honesty: active
    // time is known, its utilisation is not).
    for acct in contribution.keys() {
        per_account
            .entry(acct.clone())
            .or_insert_with(|| AccountStats {
                seen: 0,
                expected,
                coverage: 0.0,
                session: DimStats::ZERO,
                weekly: DimStats::ZERO,
                cap_hits: 0,
                time_at_cap_secs: 0,
                contribution_share: share_of(&contribution, acct, total_obs),
            });
    }

    let (all_high_episodes, all_high_secs, all_high_covered_secs) =
        all_high(&by_acct, roster, period, params);
    let holds = capacity_holds(&by_acct, roster, period, params);
    let roster = RosterStats {
        // Excludes #452 preemptive swaps (`SwapKind::Preempt`) so the count stays the SUM of the
        // itemized `swap_breakdown` reasons (which likewise omits them — see there). Preemptive
        // swaps are a reliability-SLI concern (`sessiometer reliability`), not a usage-frequency
        // reason; they still bound the contribution timeline below via the full `swaps` list.
        swap_count: swaps
            .iter()
            .filter(|e| period.contains(e.ts) && e.kind != SwapKind::Preempt)
            .count() as u32,
        swaps: swap_breakdown(swaps, period),
        all_high_episodes,
        all_high_secs,
        all_high_covered_secs,
        high_threshold: params.high_threshold,
        capacity_holds: holds.count,
        capacity_holds_session: holds.session,
        capacity_holds_weekly: holds.weekly,
        capacity_hold_secs_lower_bound: holds.secs,
        capacity_hold_covered_secs: holds.covered_secs,
        capacity_session_line: params.viability.map_or(0.0, |b| b.session),
        capacity_weekly_line: params.viability.map_or(0.0, |b| b.weekly),
    };

    UsageReport {
        period,
        per_account,
        roster,
    }
}

/// Attribute every in-period observation to the active account at its instant, per the
/// swap-active spans, returning the per-account observation counts.
///
/// The active account between swaps is the `to` of the last swap at or before the
/// instant (and the `from` of the very first swap for instants before it) — so a swap
/// that happened BEFORE the period still correctly establishes who was active at the
/// period's start. With no swaps at all there are no spans, and each observation falls
/// back to its own `acct` (so a single account trivially gets 100%).
fn contribution_counts(in_period: &[&Sample], swaps: &[SwapEvent]) -> BTreeMap<String, u32> {
    let mut sorted: Vec<&SwapEvent> = swaps.iter().collect();
    sorted.sort_by_key(|e| e.ts);

    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for &s in in_period {
        let active = active_at(&sorted, s.ts)
            .map(str::to_owned)
            .unwrap_or_else(|| s.acct.clone());
        *counts.entry(active).or_insert(0) += 1;
    }
    counts
}

/// The account active at `ts` given the swaps sorted ascending by `ts`, or `None` when
/// there are no swaps (the caller then falls back to the observation's own account).
fn active_at<'e>(sorted: &[&'e SwapEvent], ts: i64) -> Option<&'e str> {
    let first = sorted.first()?;
    if ts < first.ts {
        // Before the first recorded swap, the active account is who it swapped away from.
        return Some(&first.from);
    }
    // Otherwise it is the destination of the most recent swap at or before `ts`.
    let mut active = first.to.as_str();
    for e in sorted {
        if e.ts <= ts {
            active = &e.to;
        } else {
            break;
        }
    }
    Some(active)
}

/// One account's `contribution` count as a share of the total observations (`0.0` when
/// there were none, or when the account holds none).
fn share_of(contribution: &BTreeMap<String, u32>, acct: &str, total_obs: f64) -> f64 {
    if total_obs == 0.0 {
        return 0.0;
    }
    f64::from(contribution.get(acct).copied().unwrap_or(0)) / total_obs
}

/// Count the in-period swaps by reason.
fn swap_breakdown(swaps: &[SwapEvent], period: Period) -> SwapBreakdown {
    let mut bd = SwapBreakdown::default();
    for e in swaps.iter().filter(|e| period.contains(e.ts)) {
        match e.kind {
            SwapKind::Session => bd.session += 1,
            SwapKind::Weekly => bd.weekly += 1,
            SwapKind::Manual => bd.manual += 1,
            SwapKind::Forced => bd.forced += 1,
            SwapKind::Emergency => bd.emergency += 1,
            // #452 preemptive swaps (reason=blind_preempt) are a RELIABILITY concern — surfaced by
            // `sessiometer reliability`'s false-preempt SLI (ADR-0017), NOT a usage-frequency reason
            // (they are a rare tail-risk guard, not a rotation pattern). They still bound the
            // contribution timeline (`parse_swap_events` keeps them); `swap_count` excludes them in
            // lockstep so it stays the SUM of the reasons itemized here. Surfacing them in the stats
            // wire is a deliberate future schema step (with the cross-language fixture lockstep).
            SwapKind::Preempt => {}
        }
    }
    bd
}

/// Count + total duration of the intervals during which EVERY account in the census set is
/// simultaneously known-and-at/above `high_threshold` — the utilisation census ("was the
/// roster running hot?"), plus the jointly-covered time it was measured over.
///
/// Each account contributes the disjoint intervals over which its readings are BOTH
/// covering (not a gap) AND high; the all-high intervals are the intersection of every
/// account's high-interval set. Any account with no high interval yields no episodes.
/// Because a gap produces no covering interval, an instant where any account is unsampled
/// cannot be part of an episode — gaps are UNKNOWN, never high.
///
/// The census set is `roster` when the caller knows it, else the sampled accounts (see
/// [`aggregate_with_roster`]). A rostered account with NO samples therefore keeps its
/// (empty) place in the intersection instead of vanishing from it. An EMPTY census set —
/// an empty configured roster, or no samples at all under the fallback — has nothing to
/// intersect over and yields `(0, 0, 0)`: unmeasurable, which the zero third return says.
///
/// The third return is the intersection of every census account's COVERING intervals — the
/// time over which the census could be taken at all. It is `0` exactly when no instant had
/// the whole set observable, which is what separates "measured, and it never happened" from
/// "never measurable" for a caller that would otherwise print a fabricated `0 episodes`.
/// Since high ⊆ covering per account, this bounds the episode total: zero jointly-covered
/// time implies zero episodes, never the reverse.
///
/// Returns `(episode_count, total_secs, jointly_covered_secs)`.
fn all_high(
    by_acct: &BTreeMap<&str, Vec<&Sample>>,
    roster: Option<&BTreeSet<String>>,
    period: Period,
    params: &AggregateParams,
) -> (u32, i64, i64) {
    let census: Vec<&str> = match roster {
        Some(r) => r.iter().map(String::as_str).collect(),
        None => by_acct.keys().copied().collect(),
    };
    // Nothing to intersect over: no account is observable, so the census is UNMEASURABLE
    // (covered `0`), not a calm zero.
    if census.is_empty() {
        return (0, 0, 0);
    }

    let mut high_acc: Option<Vec<(i64, i64)>> = None;
    let mut cov_acc: Option<Vec<(i64, i64)>> = None;
    for handle in census {
        // A rostered account absent from the period's samples covers NOTHING — it stays in
        // the intersection and empties it, rather than silently leaving it.
        let group: &[&Sample] = by_acct.get(handle).map_or(&[], Vec::as_slice);
        let windows = validity_windows(group, period, params.stale_after_secs);
        let highs = merge_intervals(
            windows
                .iter()
                .zip(group.iter())
                .filter(|(_, s)| s.session >= params.high_threshold)
                .map(|(&w, _)| w)
                .collect(),
        );
        let covering = merge_intervals(windows);

        high_acc = Some(match high_acc {
            None => highs,
            Some(prev) => intersect(&prev, &highs),
        });
        cov_acc = Some(match cov_acc {
            None => covering,
            Some(prev) => intersect(&prev, &covering),
        });
        // Empty ∩ anything stays empty, and `highs ⊆ covering` per account (the high list is
        // a FILTERED subset of the very windows the covering one merges), so an emptied
        // COVERING intersection has ALREADY emptied the high one — nothing more can change.
        // `high_acc` is deliberately NOT forced empty here: doing so would pre-satisfy the
        // very ⊆ invariant `prop_all_high_time_never_exceeds_the_jointly_covered_time` exists
        // to gate, silently REPAIRING a broken subset relation instead of failing on it.
        if cov_acc.as_ref().is_some_and(Vec::is_empty) {
            break;
        }
    }

    let episodes = merge_intervals(high_acc.unwrap_or_default());
    let covered = merge_intervals(cov_acc.unwrap_or_default());
    (
        episodes.len() as u32,
        episodes.iter().map(|(lo, hi)| hi - lo).sum(),
        covered.iter().map(|(lo, hi)| hi - lo).sum(),
    )
}

/// Which window's reset gates a hold's relief — the daemon's own `cause=` vocabulary, so a
/// reported split is directly reconcilable against its `all_exhausted` event stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldCause {
    /// Relief waits on a SESSION window reopening.
    Session,
    /// Relief waits on a WEEKLY window reopening.
    Weekly,
}

/// A cause-tagged half-open interval `[lo, hi)`: a span during which the relevant account (or,
/// after intersection, the whole roster) was non-viable, plus the dimension gating its end.
type HoldSpan = (i64, i64, HoldCause);

/// The capacity-holds census over one period — see [`RosterStats::capacity_holds`] for what each
/// figure means and the UNKNOWN contract `covered_secs == 0` carries.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct CapacityHoldCensus {
    count: u32,
    session: u32,
    weekly: u32,
    secs: i64,
    covered_secs: i64,
}

/// Count + cause split + bounded duration of the intervals during which EVERY account in the
/// census set was simultaneously NON-VIABLE at the daemon's own viability boundary — the capacity
/// fact "could the daemon still swap?" (issue #803, REQ-STA-B-010).
///
/// Structurally the sibling of [`all_high`], and deliberately a SEPARATE function measuring a
/// SEPARATE fact rather than a parameterisation of it: that one asks whether the roster was
/// running hot, this one whether the daemon still had anywhere to land. They answer differently on
/// the same week, which is the whole reason both exist.
///
/// # Why the validity window is RESET-anchored, not cadence-anchored
///
/// This is the part that makes the metric measurable at all. A blocked account is polled at
/// `exhausted_poll_secs` (3600 s at defaults) against a `poll_secs` (300 s) staleness horizon, so a
/// window that expired after one nominal cadence would go blind EXACTLY while the condition it is
/// meant to detect holds — detector sensitivity inversely coupled to the thing detected. Measured
/// on the live corpus, a cadence-anchored predicate swings 0 → 84 episodes as `stale_after` moves
/// between those two values, which is a reading of the poll schedule, not of the fleet.
///
/// A blocked reading sidesteps this because it already STATES its own expiry: the
/// `session_resets_at` / `weekly_resets_at` the poll recorded. So a blocked reading covers
/// `[ts, min(next_ts, max(ts + stale_after, blocking_dimension_reset)))` — it is counted as blocked
/// on the account's own last assertion of when its block ends, never by assumption. Over the live
/// 7 d corpus, 2036 / 2036 blocked→next transitions honoured that carried reset and none un-blocked
/// early. A blocked reading carrying NO usable expiry falls back to the cadence horizon and the
/// time beyond it stays UNKNOWN — this REFINES REQ-STA-B-008's UNKNOWN (from "no sample" to "no
/// sample AND no carried expiry"), it does not repeal it.
///
/// # Why the event log is the ORACLE and not the source
///
/// The daemon emits `all_exhausted` on ENTERING this state, but re-arms that guard on any
/// non-`NoViableTarget` tick, so its ENTERs over-count episodes (95 ENTERs over one measured week
/// against ~7 true holds). The events reconcile this figure; they cannot produce it.
///
/// # What this census does NOT see — stated, not papered over
///
/// Three known gaps, all in the direction of UNDER-reporting the daemon's own experience:
///
/// - `enabled` / quarantine state is invisible here (it is not in the sample stream), so a PARKED
///   account — which the daemon excludes from viability outright — still has to be independently
///   blocked before it can contribute to a hold. Closing this fully needs the store to carry
///   roster state, a separate and larger change.
/// - The census intersects over the whole CONFIGURED roster, which includes the ACTIVE account;
///   the daemon's own target scan excludes it (it is looking for somewhere to swap TO). A daemon
///   cornered while its active account still reads viable is therefore not counted here.
/// - The COUNT, not only the duration, is affected by coverage: a gap INSIDE a real hold splits it
///   into two, so an under-covered window can report more, shorter holds. The duration stays a
///   lower bound throughout (the split loses the gap's seconds), but the count is not a bound in
///   either direction — read it against `covered_secs`, which is what says how much was seen.
fn capacity_holds(
    by_acct: &BTreeMap<&str, Vec<&Sample>>,
    roster: Option<&BTreeSet<String>>,
    period: Period,
    params: &AggregateParams,
) -> CapacityHoldCensus {
    // No boundary supplied ⇒ the census is not taken at all. Zero covered seconds says so, and
    // every surface must render UNKNOWN rather than the calm `0 holds` that zero would otherwise
    // read as.
    let Some(boundary) = params.viability else {
        return CapacityHoldCensus::default();
    };
    let census: Vec<&str> = match roster {
        Some(r) => r.iter().map(String::as_str).collect(),
        None => by_acct.keys().copied().collect(),
    };
    if census.is_empty() {
        return CapacityHoldCensus::default();
    }

    let mut hold_acc: Option<Vec<HoldSpan>> = None;
    let mut cov_acc: Option<Vec<(i64, i64)>> = None;
    for handle in census {
        // A rostered account absent from the period's samples covers NOTHING — it stays in the
        // intersection and empties it, rather than silently leaving and letting the remaining
        // accounts corner a daemon that in fact had this one to land on.
        let group: &[&Sample] = by_acct.get(handle).map_or(&[], Vec::as_slice);
        let (blocked, covering) = blocked_windows(group, period, params.stale_after_secs, boundary);

        hold_acc = Some(match hold_acc {
            None => blocked,
            Some(prev) => intersect_tagged(&prev, &blocked),
        });
        cov_acc = Some(match cov_acc {
            None => covering,
            Some(prev) => intersect(&prev, &covering),
        });
        // Same short-circuit (and same deliberate restraint) as [`all_high`]: `blocked ⊆ covering`
        // per account by construction, so an emptied COVERING intersection has already emptied the
        // hold one. `hold_acc` is NOT force-emptied here — doing so would pre-satisfy the very ⊆
        // invariant `prop_capacity_hold_time_never_exceeds_the_jointly_covered_time` exists to
        // gate, repairing a broken relation instead of failing on it.
        if cov_acc.as_ref().is_some_and(Vec::is_empty) {
            break;
        }
    }

    let episodes = hold_acc.unwrap_or_default();
    let covered = cov_acc.unwrap_or_default();
    CapacityHoldCensus {
        count: episodes.len() as u32,
        session: cause_count(&episodes, HoldCause::Session),
        weekly: cause_count(&episodes, HoldCause::Weekly),
        secs: episodes.iter().map(|(lo, hi, _)| hi - lo).sum(),
        covered_secs: covered.iter().map(|(lo, hi)| hi - lo).sum(),
    }
}

/// How many episodes carry `cause`.
fn cause_count(episodes: &[HoldSpan], cause: HoldCause) -> u32 {
    episodes.iter().filter(|&&(.., c)| c == cause).count() as u32
}

/// One account's RESET-ANCHORED blocked windows (each tagged with the dimension gating its end)
/// and its covering windows, both merged into sorted disjoint lists.
///
/// A reading covers `[ts, min(next_ts, max(ts + stale_after, relief)))`, clamped into the period
/// exactly as [`validity_windows`] clamps its own, where `relief` is the reading's own carried
/// expiry for whichever dimensions block it (see [`relief_of`]); an unblocked reading, or a
/// blocked one whose gating reset is unknown, has no `relief` and keeps the plain cadence horizon
/// [`validity_windows`] uses. The period clamp is why a hold still running at the period's end is
/// reported clipped to that end rather than out to its reset.
///
/// The covering list extends over the SAME anchored windows, because "blocked until its stated
/// reset" is knowledge about the account's state over that span, not a gap in it. That also makes
/// `blocked ⊆ covering` true by construction, which the census's short-circuit relies on.
fn blocked_windows(
    group: &[&Sample],
    period: Period,
    stale_after: i64,
    boundary: ViabilityBoundary,
) -> (Vec<HoldSpan>, Vec<(i64, i64)>) {
    let mut blocked: Vec<HoldSpan> = Vec::new();
    let mut covering: Vec<(i64, i64)> = Vec::with_capacity(group.len());
    for (i, s) in group.iter().enumerate() {
        let session_blocked = boundary.session_blocked(s.session);
        let weekly_blocked = boundary.weekly_blocked(s.weekly);
        let relief = relief_of(s, session_blocked, weekly_blocked);

        let next = group.get(i + 1).map_or(period.end, |n| n.ts);
        let cadence_hi = s.ts + stale_after.max(0);
        let anchored_hi = relief.map_or(cadence_hi, |(at, _)| cadence_hi.max(at));
        let hi = next.min(anchored_hi).min(period.end).max(s.ts);

        covering.push((s.ts, hi));
        if session_blocked || weekly_blocked {
            // With no usable expiry the window is cadence-bounded, but the hold still has a
            // gating dimension to name — the same weekly-preferring default the daemon's own
            // classification falls back to when a blocked spare reports no reset.
            let cause = relief.map_or_else(
                || {
                    if weekly_blocked {
                        HoldCause::Weekly
                    } else {
                        HoldCause::Session
                    }
                },
                |(_, c)| c,
            );
            blocked.push((s.ts, hi, cause));
        }
    }
    (merge_tagged(blocked), merge_intervals(covering))
}

/// When a blocked reading's own carried expiry says capacity returns, and which dimension gates
/// it — the per-account inner half of the daemon's relief rule, over a stored [`Sample`] rather
/// than a live reading.
///
/// Deliberately mirrors `crate::daemon`'s `spare_relief` exactly, including both of its judgements:
/// an account blocked on BOTH dimensions returns only once the LATER window clears (so the reset
/// is a `max`, not a `min`), and an exact tie names WEEKLY — the scarcer window. A reading missing
/// the reset for ANY dimension that blocks it yields `None`: its return time is genuinely unknown,
/// and guessing it is what reset-anchoring exists to avoid.
fn relief_of(s: &Sample, session_blocked: bool, weekly_blocked: bool) -> Option<(i64, HoldCause)> {
    let session = if session_blocked {
        Some((s.session_resets_at?, HoldCause::Session))
    } else {
        None
    };
    let weekly = if weekly_blocked {
        Some((s.weekly_resets_at?, HoldCause::Weekly))
    } else {
        None
    };
    match (session, weekly) {
        (Some(s), Some(w)) => Some(if w.0 >= s.0 { w } else { s }),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

/// The cause to carry when two tagged intervals end at the SAME instant, so neither one's cause
/// wins on its own — WEEKLY when either says so, the same tie rule [`relief_of`] and the daemon's
/// classification apply. Not a rare branch: every hold still running at the period's end has each
/// account's span clipped to that same instant, so an ongoing hold ties by construction.
fn tie_cause(a: HoldCause, b: HoldCause) -> HoldCause {
    if a == HoldCause::Weekly || b == HoldCause::Weekly {
        HoldCause::Weekly
    } else {
        HoldCause::Session
    }
}

/// [`merge_intervals`] for cause-tagged intervals: the merged span keeps the cause of whichever
/// window ends LAST, since that is the one whose reset the merged hold is waiting on.
fn merge_tagged(mut ivs: Vec<HoldSpan>) -> Vec<HoldSpan> {
    ivs.retain(|(lo, hi, _)| hi > lo);
    ivs.sort_by_key(|(lo, _, _)| *lo);
    let mut out: Vec<HoldSpan> = Vec::with_capacity(ivs.len());
    for (lo, hi, cause) in ivs {
        match out.last_mut() {
            Some(last) if lo <= last.1 => match hi.cmp(&last.1) {
                Ordering::Greater => {
                    last.1 = hi;
                    last.2 = cause;
                }
                Ordering::Equal => last.2 = tie_cause(last.2, cause),
                Ordering::Less => {}
            },
            _ => out.push((lo, hi, cause)),
        }
    }
    out
}

/// [`intersect`] for cause-tagged intervals: the overlap keeps the cause of whichever side ends
/// FIRST, because that is the account whose relief ends the joint hold — the outer `min` of the
/// daemon's relief rule, expressed over intervals.
///
/// The output needs no re-merge: both inputs are merged (so each has a real gap between
/// consecutive intervals), and an overlap can only end where one input's interval ends, so two
/// outputs can never abut.
fn intersect_tagged(a: &[HoldSpan], b: &[HoldSpan]) -> Vec<HoldSpan> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let lo = a[i].0.max(b[j].0);
        let hi = a[i].1.min(b[j].1);
        if lo < hi {
            let cause = match a[i].1.cmp(&b[j].1) {
                Ordering::Less => a[i].2,
                Ordering::Greater => b[j].2,
                Ordering::Equal => tie_cause(a[i].2, b[j].2),
            };
            out.push((lo, hi, cause));
        }
        // Advance the interval that ends first; the other may still overlap the next.
        if a[i].1 <= b[j].1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// The forward-coverage window of each sample in the sorted `group`, clamped into the
/// period: sample `i` covers `[ts_i, min(next_ts, ts_i + stale_after))`, so a reading
/// holds until the next reading or one staleness horizon later, whichever is sooner. A
/// wider gap therefore leaves uncovered (UNKNOWN) time. Same length/order as `group`;
/// an empty window (`hi == lo`, e.g. duplicate timestamps) is length zero.
fn validity_windows(group: &[&Sample], period: Period, stale_after: i64) -> Vec<(i64, i64)> {
    group
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let next = group.get(i + 1).map_or(period.end, |n| n.ts);
            let hi = next.min(s.ts + stale_after.max(0)).min(period.end);
            (s.ts, hi.max(s.ts))
        })
        .collect()
}

/// Merge `[lo, hi)` intervals into sorted, disjoint, non-empty intervals, coalescing
/// any that overlap OR abut (so two back-to-back covering windows read as one span).
fn merge_intervals(mut ivs: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    ivs.retain(|(lo, hi)| hi > lo);
    ivs.sort_by_key(|(lo, _)| *lo);
    let mut out: Vec<(i64, i64)> = Vec::with_capacity(ivs.len());
    for (lo, hi) in ivs {
        match out.last_mut() {
            Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
            _ => out.push((lo, hi)),
        }
    }
    out
}

/// The intersection of two already-merged (sorted, disjoint) interval lists.
fn intersect(a: &[(i64, i64)], b: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let lo = a[i].0.max(b[j].0);
        let hi = a[i].1.min(b[j].1);
        if lo < hi {
            out.push((lo, hi));
        }
        // Advance the interval that ends first; the other may still overlap the next.
        if a[i].1 <= b[j].1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

impl DimStats {
    /// The all-zero summary — used only for an account with a contribution share but no
    /// readings of its own (see [`aggregate`]); never produced from actual samples.
    const ZERO: DimStats = DimStats {
        mean: 0.0,
        peak: 0.0,
        p95: 0.0,
    };
}

/// Mean / peak / p95 over a dimension's values. `xs` is non-empty for every real
/// account (each is in the roster because it has ≥1 sample); an empty slice yields the
/// finite all-zero summary rather than a NaN/∞ that JSON could not represent.
fn dim_stats(xs: &[f64]) -> DimStats {
    DimStats {
        mean: mean_of(xs),
        peak: max_of(xs),
        p95: p95_of(xs),
    }
}

/// Arithmetic mean (`0.0` for empty).
fn mean_of(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

/// Peak/max (`0.0` for empty — never `-∞`, so the result is always JSON-representable).
fn max_of(xs: &[f64]) -> f64 {
    xs.iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max)
        .max(0.0)
}

/// 95th-percentile by the nearest-rank method (`0.0` for empty), matching the store's
/// daily tier so the two agree on the same samples. Delegates to the shared
/// [`crate::percentile::percentile`] (issue #455) — the single copy of the nearest-rank math.
fn p95_of(xs: &[f64]) -> f64 {
    crate::percentile::percentile(xs, 0.95)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal reading: `provider="claude"`, given `acct`, no optionals.
    fn sample(ts: i64, acct: &str, session: f64, weekly: f64) -> Sample {
        Sample::new(ts, "claude", acct, session, weekly)
    }

    /// Default-ish params: 300 s cadence, cap and high-water both at 0.80.
    fn params() -> AggregateParams {
        AggregateParams::new(300, 0.80, 0.80)
    }

    /// Resolve an RFC 3339 instant to epoch seconds through the crate's canonical
    /// parser — used to build civil-calendar boundaries without magic numbers.
    fn epoch(s: &str) -> i64 {
        epoch_from_rfc3339(s).expect("valid RFC 3339 fixture")
    }

    // --- swap-event parsing ---------------------------------------------------

    #[test]
    fn parses_every_swap_reason_and_emergency_skips_the_rest() {
        // A realistic log slice: the five swap shapes interleaved with unrelated events.
        let log = "\
ts=2026-01-01T00:00:00Z event=credential_health account=work state=healthy
ts=2026-01-01T00:05:00Z event=swap from=work to=play reason=session session_pct=82
ts=2026-01-01T00:10:00Z event=restash account=play
ts=2026-01-01T00:15:00Z event=swap from=play to=work reason=weekly session_pct=40
ts=2026-01-01T00:20:00Z event=swap from=work to=play reason=manual session_pct=0
ts=2026-01-01T00:25:00Z event=swap from=play to=work reason=forced session_pct=0
ts=2026-01-01T00:30:00Z event=emergency_swap from=work to=play
ts=2026-01-01T00:35:00Z event=swap from=play to=work reason=bogus session_pct=0
garbage line with no fields
";
        let events = parse_swap_events(log);
        let kinds: Vec<SwapKind> = events.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                SwapKind::Session,
                SwapKind::Weekly,
                SwapKind::Manual,
                SwapKind::Forced,
                SwapKind::Emergency,
            ],
            "the five valid swaps parse in order; the unknown-reason swap is dropped"
        );
        assert_eq!(events[0].from, "work");
        assert_eq!(events[0].to, "play");
        assert_eq!(events[0].ts, epoch("2026-01-01T00:05:00Z"));
        // The emergency swap carries from/to but no reason.
        assert_eq!(events[4].from, "work");
        assert_eq!(events[4].to, "play");
    }

    #[test]
    fn parsing_skips_the_all_exhausted_cleared_leave_edge() {
        // Issue #800 promoted the all-exhausted LEAVE edge from a stderr `Diagnostic` to a durable
        // `Event`, so `event=all_exhausted_cleared` lines now appear in the log this parser reads.
        // It carries NEITHER `from=`/`to=` NOR a `reason=`, so it must land on the `_ => continue`
        // drop like any other non-swap event — never a malformed `SwapEvent` that would corrupt the
        // contribution timeline. Pinned against the real production shape: the daemon emits the
        // clear on the relief swap's OWN tick, immediately after the swap line.
        let log = "\
ts=2026-01-01T00:00:00Z event=all_exhausted hold=play cause=session resets_at=2026-01-01T05:00:00Z
ts=2026-01-01T00:05:00Z event=swap from=work to=play reason=session session_pct=97
ts=2026-01-01T00:05:00Z event=all_exhausted_cleared
";
        assert_eq!(
            parse_swap_events(log).len(),
            1,
            "only the swap parses; the ENTER and LEAVE edges are both skipped"
        );
        // Equivalence with the same log minus the LEAVE line — the new event kind is inert here.
        assert_eq!(
            parse_swap_events(log),
            parse_swap_events(
                "ts=2026-01-01T00:05:00Z event=swap from=work to=play reason=session session_pct=97\n"
            )
        );
    }

    #[test]
    fn velocity_preempt_swap_parses_as_preempt_bounds_the_timeline_and_is_excluded_from_count() {
        // Issue #539 regression, two halves. PARSER: a `reason=velocity_preempt` line must fold onto
        // SwapKind::Preempt (like #452's blind_preempt), NEVER the `_ => continue` drop — dropping it
        // would misattribute every post-swap sample to the departed account. Default-reachable (the
        // horizon defaults to 120s, so the projective swap path is ON in a stock daemon).
        let parsed = parse_swap_events(
            "ts=2026-01-01T00:05:00Z event=swap from=work to=play reason=velocity_preempt session_pct=92\n",
        );
        assert_eq!(
            parsed.len(),
            1,
            "the velocity_preempt swap is parsed, not dropped"
        );
        assert_eq!(parsed[0].kind, SwapKind::Preempt);
        assert_eq!(parsed[0].to, "play");

        // AGGREGATE: a Preempt swap BOUNDS the contribution timeline (a sample after it credits the
        // swap TARGET) yet is EXCLUDED from swap_count/breakdown (a reliability tail-risk guard, not a
        // rotation reason). `work` active at the start (prior swap), a session swap work→play at
        // t=1500 (counts), a velocity-preempt swap play→work at t=2500 (excluded, but bounds).
        let period = Period::new(1_000, 4_000);
        let swaps = vec![
            mk_swap_ab(500, "play", "work", SwapKind::Session),
            mk_swap_ab(1_500, "work", "play", SwapKind::Session),
            mk_swap_ab(2_500, "play", "work", SwapKind::Preempt),
        ];
        let samples = vec![
            sample(1_200, "work", 0.4, 0.3), // work active (before the t=1500 swap)
            sample(2_000, "play", 0.5, 0.3), // play active (t=1500 swap → play, before the preempt)
            sample(3_000, "work", 0.6, 0.3), // work active AGAIN — only if the preempt swap bounded
        ];
        let report = aggregate(&samples, &swaps, period, &params());
        assert!(
            (report.per_account["work"].contribution_share - 2.0 / 3.0).abs() < 1e-9,
            "work credited for the pre-swap AND the post-preempt obs (timeline bounded)",
        );
        assert!(
            (report.per_account["play"].contribution_share - 1.0 / 3.0).abs() < 1e-9,
            "play credited only for the middle obs",
        );
        assert_eq!(
            report.roster.swap_count, 1,
            "only the in-period Session swap counts; the Preempt swap is excluded",
        );
        assert_eq!(report.roster.swaps.session, 1);
    }

    #[test]
    fn parsing_tolerates_missing_fields_and_bad_timestamps() {
        let log = "\
ts=2026-01-01T00:05:00Z event=swap from=work reason=session
ts=not-a-timestamp event=swap from=work to=play reason=session
event=swap from=work to=play reason=session
ts=2026-01-01T00:05:00Z event=swap from=work to=play reason=session session_pct=82
";
        let events = parse_swap_events(log);
        assert_eq!(events.len(), 1, "only the fully-formed line survives");
        assert_eq!(events[0].to, "play");
    }

    // --- period boundary: inclusive start / exclusive end ---------------------

    #[test]
    fn period_is_inclusive_start_exclusive_end() {
        let period = Period::new(1_000, 2_000);
        let samples = vec![
            sample(999, "a", 0.1, 0.1),   // just before → excluded
            sample(1_000, "a", 0.1, 0.1), // exactly start → included
            sample(1_999, "a", 0.1, 0.1), // just before end → included
            sample(2_000, "a", 0.1, 0.1), // exactly end → excluded
        ];
        let report = aggregate(&samples, &[], period, &params());
        assert_eq!(report.per_account["a"].seen, 2, "only [1000, 2000) counts");
    }

    // --- per-account central statistics ---------------------------------------

    #[test]
    fn per_account_mean_peak_p95_for_both_dimensions() {
        let period = Period::new(0, 10_000);
        // Sessions 0.2..1.2 by 0.2; weekly a flat 0.30.
        let sessions = [0.2, 0.4, 0.6, 0.8, 1.0, 1.2];
        let samples: Vec<Sample> = sessions
            .iter()
            .enumerate()
            .map(|(k, &s)| sample(k as i64 * 300, "work", s, 0.30))
            .collect();
        let report = aggregate(&samples, &[], period, &params());
        let a = &report.per_account["work"];
        assert!((a.session.mean - 0.7).abs() < 1e-9, "session mean");
        assert!((a.session.peak - 1.2).abs() < 1e-9, "session peak");
        // Nearest-rank p95 of 6 values: ceil(0.95·6)=6 → the 6th (largest) = 1.2.
        assert!((a.session.p95 - 1.2).abs() < 1e-9, "session p95");
        assert!((a.weekly.mean - 0.30).abs() < 1e-9, "weekly mean");
        assert!((a.weekly.peak - 0.30).abs() < 1e-9, "weekly peak");
    }

    // --- cap-hit boundary (>=) ------------------------------------------------

    #[test]
    fn cap_hit_count_is_inclusive_at_the_threshold() {
        let period = Period::new(0, 10_000);
        // Exactly the cap, just below, and above — only at/above the 0.80 cap count.
        let samples = vec![
            sample(0, "work", 0.7999, 0.1),
            sample(300, "work", 0.80, 0.1), // exactly the cap → counts
            sample(600, "work", 0.81, 0.1),
            sample(900, "work", 0.99, 0.1),
        ];
        let report = aggregate(&samples, &[], period, &params());
        assert_eq!(
            report.per_account["work"].cap_hits, 3,
            "0.80, 0.81, 0.99 are cap hits; 0.7999 is not"
        );
    }

    // --- gap honesty: coverage + no synthetic zeros ---------------------------

    #[test]
    fn coverage_is_seen_over_expected_and_gaps_are_not_zero_filled() {
        // A one-hour period at 300 s cadence expects 12 samples; supply only 3, all high.
        let period = Period::new(0, 3_600);
        let samples = vec![
            sample(0, "work", 0.90, 0.90),
            sample(300, "work", 0.95, 0.92),
            sample(600, "work", 0.99, 0.94),
        ];
        let report = aggregate(&samples, &[], period, &params());
        let a = &report.per_account["work"];
        assert_eq!(a.seen, 3);
        assert!((a.expected - 12.0).abs() < 1e-9, "3600/300 = 12 expected");
        assert!((a.coverage - 0.25).abs() < 1e-9, "3/12 coverage");
        // Gap honesty: the 9 missing polls are NOT invented as 0.0 — the mean stays high
        // (~0.947), it is not dragged toward zero by the absent samples.
        assert!(
            a.session.mean > 0.9,
            "absent samples are not counted as zero"
        );
    }

    // --- contribution share ---------------------------------------------------

    #[test]
    fn contribution_share_single_account_is_full() {
        let period = Period::new(0, 10_000);
        let samples = vec![sample(0, "solo", 0.5, 0.5), sample(300, "solo", 0.6, 0.5)];
        let report = aggregate(&samples, &[], period, &params());
        assert!(
            (report.per_account["solo"].contribution_share - 1.0).abs() < 1e-9,
            "one account with no swaps → 100%"
        );
    }

    #[test]
    fn contribution_share_follows_swap_active_spans_and_sums_to_one() {
        let period = Period::new(1_000, 4_000);
        // A swap BEFORE the period makes `work` active at the start; a swap at t=2500
        // hands the active credential to `play` mid-period.
        let swaps = vec![
            mk_swap_ab(500, "play", "work", SwapKind::Session),
            mk_swap_ab(2_500, "work", "play", SwapKind::Manual),
        ];
        // Two observations while `work` is active, three while `play` is — regardless of
        // which account each reading is ABOUT (attribution is by active span).
        let samples = vec![
            sample(1_200, "work", 0.3, 0.3),
            sample(2_000, "work", 0.4, 0.3),
            sample(2_600, "play", 0.5, 0.3),
            sample(3_000, "play", 0.6, 0.3),
            sample(3_500, "play", 0.7, 0.3),
        ];
        let report = aggregate(&samples, &swaps, period, &params());
        let work = report.per_account["work"].contribution_share;
        let play = report.per_account["play"].contribution_share;
        assert!(
            (work - 2.0 / 5.0).abs() < 1e-9,
            "work active for 2 of 5 obs"
        );
        assert!(
            (play - 3.0 / 5.0).abs() < 1e-9,
            "play active for 3 of 5 obs"
        );
        assert!((work + play - 1.0).abs() < 1e-9, "shares sum to 1");
    }

    #[test]
    fn contribution_share_credits_an_active_account_with_no_readings_of_its_own() {
        // `dark` is the active credential the whole period (a prior swap TO it), but the
        // only readings are ABOUT `work` (the daemon happened to poll a non-active
        // account). The active account still earns the contribution — with a zeroed
        // readings row — so the shares sum to 1 and no observation is dropped.
        let period = Period::new(1_000, 2_000);
        let swaps = vec![mk_swap_ab(500, "work", "dark", SwapKind::Emergency)];
        let samples = vec![
            sample(1_200, "work", 0.3, 0.3),
            sample(1_500, "work", 0.4, 0.3),
        ];
        let report = aggregate(&samples, &swaps, period, &params());
        let dark = &report.per_account["dark"];
        assert!(
            (dark.contribution_share - 1.0).abs() < 1e-9,
            "dark active throughout"
        );
        assert_eq!(dark.seen, 0, "dark has no readings of its own");
        assert_eq!(
            dark.session,
            DimStats::ZERO,
            "no readings → zeroed, not invented"
        );
        // `work` was sampled but never active → zero contribution, real readings.
        let work = &report.per_account["work"];
        assert_eq!(work.seen, 2);
        assert!(
            work.contribution_share.abs() < 1e-9,
            "work never active → 0 share"
        );
        let total: f64 = report
            .per_account
            .values()
            .map(|a| a.contribution_share)
            .sum();
        assert!((total - 1.0).abs() < 1e-9, "shares still sum to 1");
    }

    // --- swap frequency (incl. manual + emergency) ----------------------------

    #[test]
    fn swap_frequency_counts_all_reasons_only_within_period() {
        let period = Period::new(1_000, 3_000);
        let swaps = vec![
            mk_swap(500, SwapKind::Session),   // before period → excluded
            mk_swap(1_000, SwapKind::Session), // inclusive start → counts
            mk_swap(1_500, SwapKind::Manual),
            mk_swap(1_800, SwapKind::Forced),
            mk_swap(2_200, SwapKind::Weekly),
            mk_swap(2_600, SwapKind::Emergency),
            mk_swap(3_000, SwapKind::Session), // exclusive end → excluded
        ];
        let report = aggregate(&[], &swaps, period, &params());
        assert_eq!(report.roster.swap_count, 5, "five swaps in [1000, 3000)");
        assert_eq!(report.roster.swaps.session, 1);
        assert_eq!(report.roster.swaps.manual, 1);
        assert_eq!(report.roster.swaps.forced, 1);
        assert_eq!(report.roster.swaps.weekly, 1);
        assert_eq!(report.roster.swaps.emergency, 1);
    }

    // --- time at cap ----------------------------------------------------------

    #[test]
    fn time_at_cap_sums_covering_windows_and_a_gap_does_not_extend_it() {
        // Cadence 300, stale_after 300. Three consecutive cap-hits (t=0,300,600) then a
        // long gap before a final cap-hit at t=5000 near the period end.
        let period = Period::new(0, 5_200);
        let samples = vec![
            sample(0, "work", 0.90, 0.1),
            sample(300, "work", 0.90, 0.1),
            sample(600, "work", 0.90, 0.1),
            sample(5_000, "work", 0.90, 0.1),
        ];
        let report = aggregate(&samples, &[], period, &params());
        // First three cover [0,300)+[300,600)+[600,900) = 900 s; the last covers
        // [5000, min(5000+300, 5200)) = 200 s. The gap 900..5000 adds NOTHING.
        assert_eq!(report.per_account["work"].time_at_cap_secs, 900 + 200);
    }

    // --- all-accounts-high episodes -------------------------------------------

    #[test]
    fn all_accounts_high_episode_spans_only_the_overlap() {
        // Two accounts. `a` is high over its two covered windows [0,600); `b` is high
        // only at t=300 → covered [300,600). All-high overlap = [300,600) = one 300 s
        // episode. Cadence/stale 300.
        let period = Period::new(0, 900);
        let samples = vec![
            sample(0, "a", 0.90, 0.1),
            sample(300, "a", 0.90, 0.1),
            sample(0, "b", 0.10, 0.1), // b low here
            sample(300, "b", 0.90, 0.1),
        ];
        let report = aggregate(&samples, &[], period, &params());
        assert_eq!(report.roster.all_high_episodes, 1);
        assert_eq!(report.roster.all_high_secs, 300, "overlap [300,600)");
    }

    #[test]
    fn all_accounts_high_treats_a_missing_account_as_unknown_not_high() {
        // `a` is high across [0,600). `b` has NO sample at all in the window where `a` is
        // high → b is UNKNOWN there → NOT all-high. Gap honesty.
        let period = Period::new(0, 600);
        let samples = vec![
            sample(0, "a", 0.90, 0.1),
            sample(300, "a", 0.95, 0.1),
            sample(0, "b", 0.90, 0.1), // b only covers [0,300) then goes stale
        ];
        let report = aggregate(&samples, &[], period, &params());
        // b covers [0,300); a covers [0,600). Overlap high = [0,300) → one 300 s episode.
        assert_eq!(report.roster.all_high_episodes, 1);
        assert_eq!(report.roster.all_high_secs, 300);
        // Now drop b's only sample entirely: b never known → no all-high at all.
        let only_a = vec![sample(0, "a", 0.90, 0.1), sample(300, "a", 0.95, 0.1)];
        let single = aggregate(&only_a, &[], period, &params());
        // With just `a` in the roster, "all high" degenerates to a's high span.
        assert_eq!(single.roster.all_high_episodes, 1);
        assert_eq!(single.per_account.len(), 1);
    }

    /// A handle set, for the census-over-the-configured-roster tests below.
    fn roster(handles: &[&str]) -> BTreeSet<String> {
        handles.iter().map(|h| (*h).to_owned()).collect()
    }

    #[test]
    fn a_rostered_account_with_no_samples_stays_in_the_census_instead_of_vanishing() {
        // Issue #804's opposite-direction defect. `a` is high across the whole window; `b` is
        // rostered but was never sampled in it. Intersecting over the SAMPLED accounts drops
        // `b` and the census degenerates to `a` alone — so it fires MORE easily, on strictly
        // less evidence. Over the CONFIGURED roster, `b` keeps its (empty) place.
        let period = Period::new(0, 600);
        let samples = vec![sample(0, "a", 0.90, 0.1), sample(300, "a", 0.95, 0.1)];

        let sampled_only = aggregate(&samples, &[], period, &params());
        assert_eq!(
            sampled_only.roster.all_high_episodes, 1,
            "the roster-less form still degenerates to the sampled set — the documented fallback"
        );

        let configured =
            aggregate_with_roster(&samples, &[], period, &params(), Some(&roster(&["a", "b"])));
        assert_eq!(
            configured.roster.all_high_episodes, 0,
            "`b` was never observed, so the roster was never known to be all-high"
        );
        assert_eq!(
            configured.roster.all_high_covered_secs, 0,
            "no instant had BOTH accounts covered — the census was unmeasurable, and this zero \
             is what stops a surface printing that `0` as a calm window"
        );
    }

    #[test]
    fn the_census_excludes_an_orphan_handle_that_is_not_in_the_configured_roster() {
        // The other half of intersecting over the CONFIGURED set: samples from a removed /
        // renamed handle (issue #314's orphan partition) must not join the census. `retired`
        // idles across the window, so admitting it would suppress a real episode.
        let period = Period::new(0, 600);
        let samples = vec![
            sample(0, "a", 0.90, 0.1),
            sample(300, "a", 0.95, 0.1),
            sample(0, "retired", 0.05, 0.1),
            sample(300, "retired", 0.05, 0.1),
        ];

        assert_eq!(
            aggregate(&samples, &[], period, &params())
                .roster
                .all_high_episodes,
            0,
            "sampled-set census: the orphan's idle readings suppress the episode"
        );

        let configured =
            aggregate_with_roster(&samples, &[], period, &params(), Some(&roster(&["a"])));
        assert_eq!(
            configured.roster.all_high_episodes, 1,
            "the orphan is not in the roster, so it is not part of the census"
        );
        assert_eq!(configured.roster.all_high_secs, 600);
    }

    #[test]
    fn jointly_covered_seconds_separate_a_measured_zero_from_an_unmeasurable_one() {
        // The distinction the render rule turns on. Both windows report `0 episodes`; only one
        // of them MEASURED that zero.
        let period = Period::new(0, 600);
        let set = roster(&["a", "b"]);

        // Measured: both accounts covered the whole window, neither ever high.
        let calm = vec![
            sample(0, "a", 0.10, 0.1),
            sample(300, "a", 0.10, 0.1),
            sample(0, "b", 0.10, 0.1),
            sample(300, "b", 0.10, 0.1),
        ];
        let calm = aggregate_with_roster(&calm, &[], period, &params(), Some(&set));
        assert_eq!(calm.roster.all_high_episodes, 0);
        assert_eq!(
            calm.roster.all_high_covered_secs, 600,
            "both were observable for the whole window, so the zero is a real reading"
        );

        // Unmeasurable: the two accounts' coverage never overlaps, so no instant had the
        // roster observable — even though each was high while it WAS observed.
        let disjoint = vec![sample(0, "a", 0.90, 0.1), sample(300, "b", 0.90, 0.1)];
        let disjoint = aggregate_with_roster(&disjoint, &[], period, &params(), Some(&set));
        assert_eq!(disjoint.roster.all_high_episodes, 0);
        assert_eq!(
            disjoint.roster.all_high_covered_secs, 0,
            "a covers [0,300), b covers [300,600) — the intersection is empty"
        );
    }

    #[test]
    fn an_empty_configured_roster_is_unmeasurable_not_calm() {
        // Degenerate set: nothing to intersect over. `0 episodes` here would claim a calm
        // roster that does not exist.
        let period = Period::new(0, 600);
        let samples = vec![sample(0, "a", 0.90, 0.1)];
        let empty = aggregate_with_roster(&samples, &[], period, &params(), Some(&BTreeSet::new()));
        assert_eq!(empty.roster.all_high_episodes, 0);
        assert_eq!(empty.roster.all_high_covered_secs, 0);
    }

    #[test]
    fn the_census_carries_the_water_it_actually_used() {
        // The threshold rides OUT of the aggregate (issue #804) so no downstream surface has to
        // hardcode a literal. It is the param's value, not a constant: two runs at different
        // waters report their own.
        let period = Period::new(0, 600);
        let samples = vec![sample(0, "a", 0.90, 0.1)];
        for water in [0.80, 0.95] {
            let params = AggregateParams::new(300, 0.80, water);
            let report = aggregate(&samples, &[], period, &params);
            assert_eq!(report.roster.high_threshold, water);
        }
    }

    // --- month-length / DST boundaries (UTC epoch discipline) -----------------

    #[test]
    fn utc_boundaries_lose_no_sample_and_double_count_none() {
        // A month boundary and a US spring-forward DST instant, both taken as real civil
        // instants via the canonical parser. Because everything is UTC epoch seconds,
        // neither is special internally — this proves the half-open split partitions
        // samples straddling them with none lost or double-counted.
        for boundary_str in ["2026-02-01T00:00:00Z", "2026-03-08T07:00:00Z"] {
            let b = epoch(boundary_str);
            let samples = vec![
                sample(b - 1, "work", 0.5, 0.5), // last second of the earlier period
                sample(b, "work", 0.6, 0.5),     // first second of the later period
                sample(b + 1, "work", 0.7, 0.5),
            ];
            let whole = Period::new(b - 10, b + 10);
            let left = Period::new(b - 10, b); // exclusive end at the boundary
            let right = Period::new(b, b + 10); // inclusive start at the boundary

            let seen = |p: Period| aggregate(&samples, &[], p, &params()).per_account["work"].seen;
            assert_eq!(
                seen(whole),
                3,
                "{boundary_str}: all three in the whole window"
            );
            assert_eq!(
                seen(left),
                1,
                "{boundary_str}: only b-1 is left of the boundary"
            );
            assert_eq!(seen(right), 2, "{boundary_str}: b and b+1 are at/after it");
            assert_eq!(
                seen(left) + seen(right),
                seen(whole),
                "{boundary_str}: the split partitions exactly — none lost or doubled"
            );
        }
    }

    // --- interval helpers -----------------------------------------------------

    #[test]
    fn merge_and_intersect_intervals() {
        assert_eq!(
            merge_intervals(vec![(0, 10), (10, 20), (25, 30), (5, 8)]),
            vec![(0, 20), (25, 30)],
            "overlaps and abutments coalesce; a disjoint one stays separate"
        );
        assert_eq!(
            intersect(&[(0, 10), (20, 30)], &[(5, 25)]),
            vec![(5, 10), (20, 25)]
        );
        assert!(
            intersect(&[(0, 10)], &[(10, 20)]).is_empty(),
            "abut → empty"
        );
    }

    #[test]
    fn p95_matches_nearest_rank() {
        let xs: Vec<f64> = (1..=20).map(f64::from).collect();
        assert!(
            (p95_of(&xs) - 19.0).abs() < 1e-9,
            "ceil(0.95·20)=19 → 19th value"
        );
        assert!((p95_of(&[0.42]) - 0.42).abs() < 1e-9);
        assert!(p95_of(&[]).abs() < 1e-9);
        assert!(max_of(&[]).abs() < 1e-9, "empty max is 0.0, never -inf");
    }

    // --- output is JSON-clean (no NaN/inf) + deterministic --------------------

    #[test]
    fn report_serializes_to_finite_json() {
        let period = Period::new(0, 3_600);
        let samples = vec![
            sample(0, "work", 0.90, 0.90),
            sample(300, "play", 0.20, 0.30),
        ];
        let swaps = vec![mk_swap(150, SwapKind::Manual)];
        let report = aggregate(&samples, &swaps, period, &params());
        let json = serde_json::to_string(&report).expect("no NaN/inf reaches the wire");
        assert!(json.contains("\"contribution_share\""));
        assert!(!json.contains("null"), "every field is a concrete number");
    }

    // --- property tests (deterministic; a tiny LCG, no new dependency) --------

    /// A small deterministic PRNG so the property tests are reproducible with no extra
    /// crate — the same seed always drives the same inputs.
    struct Lcg(u64);
    impl Lcg {
        fn new(seed: u64) -> Self {
            Self(seed)
        }
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }
        /// A value in `[0, n)`, drawn from the HIGH bits (issue #801).
        ///
        /// `next_u64() % n` would be wrong here: in an LCG mod 2^64 the low `k` bits
        /// have period only 2^k, so bit 0 simply alternates `1,0,1,0,…`. For any EVEN
        /// `n` the remainder inherits that bit, so a caller drawing at an even stride
        /// gets a pinned parity forever. Both shapes bite the tests below:
        /// `below(2)` at the stride-4 per-sample loops returned a CONSTANT, collapsing
        /// a two-account corpus to one account and so never exercising the `all_high`
        /// intersection with N > 1; `1 + below(50)` was odd in 200/200 iterations,
        /// leaving half its sample counts unreachable. Shifting first discards the
        /// short-period bits — the lowest one kept is bit 33, of period 2^34.
        fn below(&mut self, n: u64) -> u64 {
            (self.next_u64() >> 33) % n
        }
        fn frac(&mut self) -> f64 {
            (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    #[test]
    fn prop_contribution_shares_sum_to_one_and_stay_in_unit() {
        let mut rng = Lcg::new(0x5157_1234);
        let accounts = ["work", "play", "spare"];
        for _ in 0..400 {
            let period = Period::new(0, 100_000);
            // Random swaps (some possibly outside the period) forming a plausible timeline.
            let n_swaps = rng.below(6);
            let swaps: Vec<SwapEvent> = (0..n_swaps)
                .map(|_| {
                    let from = accounts[rng.below(3) as usize];
                    let to = accounts[rng.below(3) as usize];
                    mk_swap_ab(
                        rng.below(120_000) as i64 - 10_000,
                        from,
                        to,
                        SwapKind::Session,
                    )
                })
                .collect();
            // Random samples across the accounts.
            let n = 1 + rng.below(40);
            let samples: Vec<Sample> = (0..n)
                .map(|_| {
                    let acct = accounts[rng.below(3) as usize];
                    sample(rng.below(100_000) as i64, acct, rng.frac(), rng.frac())
                })
                .collect();

            let report = aggregate(&samples, &swaps, period, &params());
            let sum: f64 = report
                .per_account
                .values()
                .map(|a| a.contribution_share)
                .sum();
            assert!(
                (sum - 1.0).abs() < 1e-9,
                "contribution shares must sum to 1 (got {sum})"
            );
            for a in report.per_account.values() {
                assert!(
                    (0.0..=1.0).contains(&a.contribution_share),
                    "each share ∈ [0,1]"
                );
            }
        }
    }

    #[test]
    fn prop_splitting_a_period_loses_no_sample_and_doubles_none() {
        let mut rng = Lcg::new(0xD57_9001);
        let accounts = ["work", "play"];
        // Splitting is only interesting when the samples span more than one account —
        // otherwise the per-account half of the assertion repeats the aggregate half.
        let mut multi_account_iters = 0_u32;
        for _ in 0..400 {
            let start = 0i64;
            let end = 100_000i64;
            let period = Period::new(start, end);
            let n = 1 + rng.below(60);
            let samples: Vec<Sample> = (0..n)
                .map(|_| {
                    let acct = accounts[rng.below(2) as usize];
                    sample(rng.below(100_000) as i64, acct, rng.frac(), rng.frac())
                })
                .collect();
            // An arbitrary split point strictly inside the window.
            let mid = 1 + rng.below((end - start - 1) as u64) as i64;
            let left = Period::new(start, mid);
            let right = Period::new(mid, end);

            let whole = aggregate(&samples, &[], period, &params());
            let l = aggregate(&samples, &[], left, &params());
            let r = aggregate(&samples, &[], right, &params());

            let total_seen =
                |rep: &UsageReport| -> u32 { rep.per_account.values().map(|a| a.seen).sum() };
            assert_eq!(
                total_seen(&l) + total_seen(&r),
                total_seen(&whole),
                "half-open split partitions samples exactly — none lost, none doubled"
            );
            // Per account too, not just in aggregate.
            for acct in accounts {
                let seen = |rep: &UsageReport| rep.per_account.get(acct).map_or(0, |a| a.seen);
                assert_eq!(seen(&l) + seen(&r), seen(&whole), "{acct}: partitioned");
            }
            if whole.per_account.len() > 1 {
                multi_account_iters += 1;
            }
        }
        // Non-degeneracy. This also carries the positive witness: reaching two accounts
        // requires at least two samples, so `0 + 0 == 0` cannot satisfy the conservation
        // assertions above vacuously.
        assert!(
            multi_account_iters > 0,
            "corpus is single-account in all 400 iterations — the per-account split is \
             never distinct from the aggregate one (expect ~388); is `Lcg::below` \
             drawing from the low bits?"
        );
    }

    #[test]
    fn prop_cap_hits_are_monotone_in_the_threshold() {
        let mut rng = Lcg::new(0xCAF_E157);
        let period = Period::new(0, 100_000);
        // `lo >= hi` holds for ANY constant function, `cap_hits = 0` included. Count the
        // iterations where the two thresholds actually disagree, so the ordering is
        // witnessed rather than merely not-contradicted.
        let mut strict_drop_iters = 0_u32;
        for _ in 0..200 {
            let n = 1 + rng.below(50);
            let samples: Vec<Sample> = (0..n)
                .map(|_| sample(rng.below(100_000) as i64, "work", rng.frac(), rng.frac()))
                .collect();
            let lenient = AggregateParams::new(300, 0.50, 0.80);
            let strict = AggregateParams::new(300, 0.90, 0.80);
            let lo = aggregate(&samples, &[], period, &lenient).per_account["work"].cap_hits;
            let hi = aggregate(&samples, &[], period, &strict).per_account["work"].cap_hits;
            assert!(
                lo >= hi,
                "a lower cap can only admit MORE hits ({lo} >= {hi})"
            );
            if lo > hi {
                strict_drop_iters += 1;
            }
        }
        // Positive witness. `lo > hi` implies `lo > 0`, so this subsumes the plain
        // "some iteration produced a hit" check and additionally proves the threshold
        // is what separates the two counts.
        assert!(
            strict_drop_iters > 0,
            "raising the cap never dropped a single hit in any of 200 iterations \
             (expect ~192) — `lo >= hi` above is holding as `lo == hi`, i.e. vacuously"
        );
    }

    #[test]
    fn prop_all_high_never_exceeds_period_duration() {
        let mut rng = Lcg::new(0x8118_2026);
        let accounts = ["work", "play"];
        let mut multi_account_iters = 0_u32;
        let mut nonzero_all_high_iters = 0_u32;
        for _ in 0..200 {
            let period = Period::new(0, 50_000);
            let n = 1 + rng.below(40);
            let samples: Vec<Sample> = (0..n)
                .map(|_| {
                    let acct = accounts[rng.below(2) as usize];
                    sample(rng.below(50_000) as i64, acct, rng.frac(), rng.frac())
                })
                .collect();
            let report = aggregate(&samples, &[], period, &params());
            assert!(
                report.roster.all_high_secs >= 0
                    && report.roster.all_high_secs <= period.duration(),
                "all-high time is within [0, period]"
            );
            if report.per_account.len() > 1 {
                multi_account_iters += 1;
            }
            if report.roster.all_high_secs > 0 {
                nonzero_all_high_iters += 1;
            }
        }
        // Non-degeneracy: the corpus must actually reach the N-fold intersection with
        // N > 1. A single-account corpus (what a low-bit `Lcg::below` produced) passes
        // every assertion above while never executing the intersection at all.
        assert!(
            multi_account_iters > 0,
            "corpus is single-account in all 200 iterations — the N>1 intersection is \
             never exercised (expect ~191); is `Lcg::below` drawing from the low bits?"
        );
        // Positive witness: without this a `return (0, 0)` stub passes 200/200.
        assert!(
            nonzero_all_high_iters > 0,
            "no iteration produced a non-zero all-high span (expect ~20) — the range \
             bound alone cannot tell a working implementation from a zero stub"
        );
    }

    #[test]
    fn prop_all_high_time_never_exceeds_the_jointly_covered_time() {
        // The invariant the UNKNOWN render rests on (issue #804): episodes are a SUBSET of the
        // jointly-covered time, so `covered == 0` implies `episodes == 0`. If it could ever be
        // violated, `—` would be printed over a real episode — an UNKNOWN hiding a fact, which
        // is worse than the fabricated calm it replaced. Held only by construction (high ⊆
        // covering per account, and intersection preserves ⊆) until this pinned it.
        let mut rng = Lcg::new(0x804_2026);
        let accounts = ["work", "play", "spare"];
        let mut covered_zero_iters = 0_u32;
        let mut covered_positive_iters = 0_u32;
        for _ in 0..300 {
            let period = Period::new(0, 50_000);
            let n = 1 + rng.below(40);
            let samples: Vec<Sample> = (0..n)
                .map(|_| {
                    let acct = accounts[rng.below(3) as usize];
                    sample(rng.below(50_000) as i64, acct, rng.frac(), rng.frac())
                })
                .collect();
            // Over the FULL three-account roster, so a never-sampled account really can empty
            // the intersection — the shape the render rule exists for.
            let roster: BTreeSet<String> = accounts.iter().map(|a| (*a).to_owned()).collect();
            let report = aggregate_with_roster(&samples, &[], period, &params(), Some(&roster));
            let r = &report.roster;
            assert!(
                r.all_high_secs <= r.all_high_covered_secs,
                "all-high time {} exceeded jointly-covered time {}",
                r.all_high_secs,
                r.all_high_covered_secs
            );
            assert!(
                r.all_high_covered_secs >= 0 && r.all_high_covered_secs <= period.duration(),
                "jointly-covered time is within [0, period]"
            );
            if r.all_high_covered_secs == 0 {
                covered_zero_iters += 1;
                assert_eq!(
                    r.all_high_episodes, 0,
                    "no jointly-covered second can hold an episode"
                );
            } else {
                covered_positive_iters += 1;
            }
        }
        // Both witnesses, so neither branch of the implication is vacuous: an all-zero corpus
        // would satisfy `all_high_secs <= all_high_covered_secs` trivially, and an
        // always-covered one would never exercise the `covered == 0` arm the render turns on.
        assert!(
            covered_zero_iters > 0,
            "no iteration was unmeasurable — the `covered == 0 ⇒ episodes == 0` arm is never \
             exercised"
        );
        assert!(
            covered_positive_iters > 0,
            "every iteration was unmeasurable — a `return (0, 0, 0)` stub would pass 300/300"
        );
    }

    // --- #803 capacity holds ---------------------------------------------------

    /// The shipping-default viability boundary: session 0.80 (the #398 reserve binding below the
    /// #597 ceiling 0.95), weekly 0.97 (the #607 effective ceiling below the raw 0.98).
    fn boundary() -> ViabilityBoundary {
        crate::swap::viability_boundary(0.95, 0.80, 0.98)
    }

    /// [`params`] with the capacity-holds census enabled at the default boundary.
    fn hold_params() -> AggregateParams {
        params().with_viability(boundary())
    }

    /// A reading carrying both window resets — what every real poll records.
    fn dated(
        ts: i64,
        acct: &str,
        session: f64,
        weekly: f64,
        session_reset: i64,
        weekly_reset: i64,
    ) -> Sample {
        Sample::new(ts, "claude", acct, session, weekly)
            .with_resets(Some(session_reset), Some(weekly_reset))
    }

    #[test]
    fn capacity_holds_are_not_taken_without_a_viability_boundary() {
        // Without the daemon's own boundary there is no honest predicate, so the census is not
        // taken — and it says so with zero covered seconds rather than reporting a calm `0 holds`
        // that a reader cannot tell from a genuinely uncornered week.
        let samples = vec![dated(0, "a", 0.99, 0.99, 9_000, 9_000)];
        let report = aggregate_with_roster(
            &samples,
            &[],
            Period::new(0, 10_000),
            &params(), // no `.with_viability(..)`
            Some(&roster(&["a"])),
        );
        assert_eq!(report.roster.capacity_hold_covered_secs, 0, "UNKNOWN");
        assert_eq!(report.roster.capacity_holds, 0);
    }

    #[test]
    fn a_blocked_reading_is_held_to_its_carried_reset_not_to_the_poll_cadence() {
        // The load-bearing behaviour (REQ-STA-B-010). One account, blocked, polled ONCE and then
        // not again for hours — exactly what `exhausted_poll_secs` (3600) does to a cornered
        // account against a 300 s staleness horizon. A cadence-anchored window would expire after
        // 300 s and report a 5-minute hold; the reading's OWN carried expiry says it stays blocked
        // until 7200.
        let period = Period::new(0, 10_000);
        let samples = vec![dated(0, "a", 0.85, 0.10, 7_200, 999_999)];
        let report =
            aggregate_with_roster(&samples, &[], period, &hold_params(), Some(&roster(&["a"])));
        let r = &report.roster;
        assert_eq!(r.capacity_holds, 1);
        assert_eq!(
            r.capacity_hold_secs_lower_bound, 7_200,
            "the hold runs to the carried session reset, not to ts + stale_after (300)"
        );
        assert_eq!(r.capacity_holds_session, 1, "session gated the relief");
        assert_eq!(r.capacity_holds_weekly, 0);
    }

    #[test]
    fn capacity_holds_are_insensitive_to_the_staleness_horizon() {
        // The signature property that separates a reset-anchored implementation from a
        // cadence-anchored one, and the falsifier for the whole approach: moving `stale_after`
        // between the two real cadences (poll_secs 300 and exhausted_poll_secs 3600) must not move
        // the answer, because a blocked reading's validity comes from its own carried expiry. On
        // the live corpus the reset-anchored figure held at 7 episodes across both while a
        // cadence-anchored one swung 0 → 84 — a reading of the poll schedule, not of the fleet.
        let period = Period::new(0, 40_000);
        let samples = vec![
            dated(0, "a", 0.85, 0.10, 20_000, 999_999),
            dated(0, "b", 0.10, 0.98, 999_999, 20_000),
            // Both return at 20_000 and the fleet is viable again from there.
            dated(20_000, "a", 0.10, 0.10, 999_999, 999_999),
            dated(20_000, "b", 0.10, 0.10, 999_999, 999_999),
        ];
        let at = |stale: i64| {
            let mut p = hold_params();
            p.stale_after_secs = stale;
            let report =
                aggregate_with_roster(&samples, &[], period, &p, Some(&roster(&["a", "b"])));
            (
                report.roster.capacity_holds,
                report.roster.capacity_hold_secs_lower_bound,
            )
        };
        assert_eq!(at(300), (1, 20_000));
        assert_eq!(
            at(300),
            at(3_600),
            "a stale_after-SENSITIVE answer is cadence-anchored, and so is wrong"
        );
    }

    #[test]
    fn the_weekly_line_is_the_effective_ceiling_not_the_raw_one() {
        // The undercount this metric was specified to avoid: an account resting at exactly 0.97 is
        // blocked FOR THE DAEMON (whose weekly line is `0.98 − WEEKLY_TAIL_MARGIN`) while a
        // predicate written against the raw 0.98 sees it as viable. One live roster account sat at
        // exactly 0.97 across both peak days, so this is the difference between measuring the drain
        // and missing it.
        let period = Period::new(0, 10_000);
        let samples = vec![dated(0, "a", 0.10, 0.97, 999_999, 8_000)];
        let report =
            aggregate_with_roster(&samples, &[], period, &hold_params(), Some(&roster(&["a"])));
        assert_eq!(
            report.roster.capacity_holds, 1,
            "0.97 blocks at the 0.97 line"
        );
        assert_eq!(report.roster.capacity_holds_weekly, 1);
        // Sanity in the other direction: the boundary itself says the raw ceiling is not the line.
        assert!(boundary().weekly < 0.98);
    }

    #[test]
    fn a_hold_needs_every_rostered_account_blocked_at_once() {
        // The capacity question is about the FLEET: one viable spare means the daemon could still
        // swap, so there is no hold however hot the others are. This is what makes the metric a
        // capacity fact rather than a utilisation one.
        let period = Period::new(0, 10_000);
        let samples = vec![
            dated(0, "a", 0.99, 0.99, 8_000, 8_000),
            dated(0, "b", 0.99, 0.99, 8_000, 8_000),
            dated(0, "c", 0.10, 0.10, 999_999, 999_999), // somewhere to land
        ];
        let report = aggregate_with_roster(
            &samples,
            &[],
            period,
            &hold_params(),
            Some(&roster(&["a", "b", "c"])),
        );
        assert_eq!(report.roster.capacity_holds, 0, "c was viable throughout");
        assert!(
            report.roster.capacity_hold_covered_secs > 0,
            "measured, and it genuinely did not happen — NOT unmeasurable"
        );
    }

    #[test]
    fn a_rostered_account_with_no_samples_makes_the_census_unmeasurable() {
        // A rostered account that contributed nothing cannot silently leave the intersection and
        // let the remaining accounts corner a daemon that in fact had this one to land on. The
        // honest answer is UNKNOWN, which zero covered seconds says.
        let period = Period::new(0, 10_000);
        let samples = vec![dated(0, "a", 0.99, 0.99, 8_000, 8_000)];
        let report = aggregate_with_roster(
            &samples,
            &[],
            period,
            &hold_params(),
            Some(&roster(&["a", "absent"])),
        );
        assert_eq!(report.roster.capacity_hold_covered_secs, 0, "UNKNOWN");
    }

    #[test]
    fn a_blocked_reading_without_a_carried_expiry_falls_back_to_the_cadence_horizon() {
        // REQ-STA-B-010 refines REQ-STA-B-008's UNKNOWN rather than repealing it: an instant is
        // counted as held on the account's own last assertion of when its block ends, never by
        // assumption. With no assertion to lean on, the window is the plain cadence horizon and
        // everything past it stays unknown.
        let period = Period::new(0, 10_000);
        let samples = vec![Sample::new(0, "claude", "a", 0.85, 0.10)]; // no resets carried
        let report =
            aggregate_with_roster(&samples, &[], period, &hold_params(), Some(&roster(&["a"])));
        assert_eq!(
            report.roster.capacity_hold_secs_lower_bound, 300,
            "one staleness horizon, not an invented extension"
        );
    }

    #[test]
    fn a_hold_blocked_on_both_dimensions_waits_for_the_later_window() {
        // Mirrors the daemon's own relief rule (`spare_relief`): an account blocked on BOTH
        // dimensions returns only once the LATER window clears, and the cause names THAT window —
        // because a window's LENGTH is not its time REMAINING (issue #665).
        let period = Period::new(0, 40_000);
        let samples = vec![dated(0, "a", 0.85, 0.98, 5_000, 30_000)];
        let report =
            aggregate_with_roster(&samples, &[], period, &hold_params(), Some(&roster(&["a"])));
        let r = &report.roster;
        assert_eq!(r.capacity_hold_secs_lower_bound, 30_000, "the LATER reset");
        assert_eq!(r.capacity_holds_weekly, 1, "named by the gating window");
        assert_eq!(r.capacity_holds_session, 0);
    }

    #[test]
    fn the_cause_split_partitions_the_holds_exactly() {
        // Two separated holds gated by different windows: the split is a partition, never an
        // overlapping tally, so a surface can print `N (a session / b weekly)` without the parts
        // contradicting the whole.
        let period = Period::new(0, 100_000);
        let samples = vec![
            dated(0, "a", 0.85, 0.10, 10_000, 999_999), // session-gated hold
            dated(10_000, "a", 0.10, 0.10, 999_999, 999_999), // relief
            dated(50_000, "a", 0.10, 0.98, 999_999, 60_000), // weekly-gated hold
            dated(60_000, "a", 0.10, 0.10, 999_999, 999_999), // relief
        ];
        let report =
            aggregate_with_roster(&samples, &[], period, &hold_params(), Some(&roster(&["a"])));
        let r = &report.roster;
        assert_eq!(r.capacity_holds, 2);
        assert_eq!(r.capacity_holds_session, 1);
        assert_eq!(r.capacity_holds_weekly, 1);
        assert_eq!(
            r.capacity_holds_session + r.capacity_holds_weekly,
            r.capacity_holds,
            "the cause split must partition the count"
        );
    }

    #[test]
    fn a_joint_hold_whose_accounts_end_together_is_named_by_the_weekly_window() {
        // The tie rule, whose VERDICT nothing else asserts — only that a tie is reached. And it
        // is not an edge case: every hold still RUNNING at the period's end has each account's
        // span clipped to that same instant, so an ongoing hold ties by construction and
        // something must break it. It breaks WEEKLY — the scarcer window, the same tie
        // `spare_relief` takes in the daemon.
        //
        // Both accounts here reset well past the period, so neither one's relief ends the joint
        // hold first: `a` is session-blocked, `b` weekly-blocked, and the answer must not depend
        // on which of the two the roster happens to intersect first.
        let period = Period::new(0, 10_000);
        let samples = vec![
            dated(0, "a", 0.85, 0.10, 999_999, 999_999),
            dated(0, "b", 0.10, 0.98, 999_999, 999_999),
        ];
        let report = aggregate_with_roster(
            &samples,
            &[],
            period,
            &hold_params(),
            Some(&roster(&["a", "b"])),
        );
        let r = &report.roster;
        assert_eq!(r.capacity_holds, 1);
        assert_eq!(
            r.capacity_hold_secs_lower_bound, 10_000,
            "clipped to the end"
        );
        assert_eq!(r.capacity_holds_weekly, 1, "a tie names the scarcer window");
        assert_eq!(r.capacity_holds_session, 0);
    }

    #[test]
    fn the_carried_boundary_lines_are_the_ones_actually_measured() {
        // Carried out for the same reason #804 carries `high_threshold`: every surface states the
        // value it ACTUALLY used instead of hardcoding a literal that can drift from the config.
        let report = aggregate_with_roster(
            &[dated(0, "a", 0.10, 0.10, 9_000, 9_000)],
            &[],
            Period::new(0, 10_000),
            &hold_params(),
            Some(&roster(&["a"])),
        );
        assert!((report.roster.capacity_session_line - 0.80).abs() < 1e-9);
        assert!((report.roster.capacity_weekly_line - 0.97).abs() < 1e-9);
    }

    #[test]
    fn prop_reset_anchoring_never_unblocks_a_reading_before_its_carried_expiry() {
        // The anchor-fidelity invariant, encoded so a provider-side change in reset semantics
        // fails LOUDLY rather than silently shortening every hold. Measured over the live 7 d
        // corpus at 2036/2036 blocked→next transitions honouring their carried reset, with none
        // un-blocking early; this pins the property the measurement observed.
        //
        // Stated over a single account, where the roster intersection is the identity, so the
        // assertion is about anchoring alone and not about interval algebra.
        let mut rng = Lcg::new(0x803_2026);
        let mut blocked_iters = 0_u32;
        let mut extended_beyond_cadence_iters = 0_u32;
        for _ in 0..300 {
            let period = Period::new(0, 200_000);
            let ts = rng.below(50_000) as i64;
            // A reset strictly after the cadence horizon is where anchoring can be observed at all.
            let reset = ts + 301 + rng.below(100_000) as i64;
            let session = rng.frac();
            let weekly = rng.frac();
            let samples = vec![dated(ts, "a", session, weekly, reset, reset)];
            let report =
                aggregate_with_roster(&samples, &[], period, &hold_params(), Some(&roster(&["a"])));
            let held = report.roster.capacity_hold_secs_lower_bound;
            let b = boundary();
            if b.session_blocked(session) || b.weekly_blocked(weekly) {
                blocked_iters += 1;
                let expected = (reset.min(period.end) - ts).max(0);
                assert_eq!(
                    held, expected,
                    "a blocked reading must stay held to its carried expiry (ts={ts}, \
                     reset={reset}, session={session}, weekly={weekly})"
                );
                if expected > 300 {
                    extended_beyond_cadence_iters += 1;
                }
            } else {
                assert_eq!(held, 0, "an unblocked reading holds nothing");
            }
        }
        // Positive witnesses: without them a corpus that never blocks — or one whose every reset
        // lands inside the cadence horizon — would pass while proving nothing about anchoring.
        assert!(
            blocked_iters > 0,
            "no iteration produced a blocked reading (expect ~60) — the anchoring arm is never \
             exercised"
        );
        assert!(
            extended_beyond_cadence_iters > 0,
            "no hold ever ran past the 300 s cadence horizon — a cadence-anchored implementation \
             would pass this test unchanged"
        );
    }

    #[test]
    fn prop_capacity_hold_time_never_exceeds_the_jointly_covered_time() {
        // The invariant the UNKNOWN render rests on, the capacity-side twin of
        // `prop_all_high_time_never_exceeds_the_jointly_covered_time`: holds are a SUBSET of the
        // jointly-covered time, so `covered == 0` implies `holds == 0`. Were it violable, `—`
        // would be printed over a real hold — an UNKNOWN hiding the very fact the readout exists
        // to surface.
        let mut rng = Lcg::new(0x803_0804);
        let accounts = ["work", "play", "spare"];
        let mut covered_zero_iters = 0_u32;
        let mut nonzero_hold_iters = 0_u32;
        for _ in 0..300 {
            let period = Period::new(0, 50_000);
            let n = 1 + rng.below(40);
            let samples: Vec<Sample> = (0..n)
                .map(|_| {
                    let acct = accounts[rng.below(3) as usize];
                    let ts = rng.below(50_000) as i64;
                    // Blocked readings are over-represented on purpose: a uniformly random corpus
                    // almost never corners a three-account fleet, so the hold arm would be vacuous.
                    dated(
                        ts,
                        acct,
                        0.70 + rng.frac() * 0.35,
                        0.90 + rng.frac() * 0.12,
                        ts + rng.below(20_000) as i64,
                        ts + rng.below(20_000) as i64,
                    )
                })
                .collect();
            let set: BTreeSet<String> = accounts.iter().map(|a| (*a).to_owned()).collect();
            let report = aggregate_with_roster(&samples, &[], period, &hold_params(), Some(&set));
            let r = &report.roster;
            assert!(
                r.capacity_hold_secs_lower_bound <= r.capacity_hold_covered_secs,
                "held time {} exceeded jointly-covered time {}",
                r.capacity_hold_secs_lower_bound,
                r.capacity_hold_covered_secs
            );
            assert!(
                r.capacity_hold_covered_secs >= 0
                    && r.capacity_hold_covered_secs <= period.duration(),
                "jointly-covered time is within [0, period]"
            );
            assert_eq!(
                r.capacity_holds_session + r.capacity_holds_weekly,
                r.capacity_holds,
                "the cause split must partition the count on every corpus"
            );
            if r.capacity_hold_covered_secs == 0 {
                covered_zero_iters += 1;
                assert_eq!(r.capacity_holds, 0, "no covered second can hold a hold");
            }
            if r.capacity_hold_secs_lower_bound > 0 {
                nonzero_hold_iters += 1;
            }
        }
        assert!(
            covered_zero_iters > 0,
            "no iteration was unmeasurable — the `covered == 0 ⇒ holds == 0` arm is never exercised"
        );
        assert!(
            nonzero_hold_iters > 0,
            "no iteration produced a hold — a zero stub would pass 300/300"
        );
    }

    // --- #806 frozen replay corpus (cross-surface regression gate) --------------

    /// The frozen corpus window, `[2026-07-24T00:00:00Z, 2026-07-26T00:00:00Z)` — the two worst
    /// days of the Jul-2026 fleet drain. PINNED as constants rather than derived from `now`: a
    /// replay that reads the clock is not a regression gate, it is a different test every day.
    const CORPUS_START: i64 = 1_784_851_200;
    const CORPUS_END: i64 = 1_785_024_000;

    /// The frozen sample corpus — committed, read-only, never regenerated. `include_str!` makes it
    /// a compile-time input, exactly as [`crate::migration`] pins its v1 artifacts, so a deleted
    /// or renamed fixture fails the BUILD rather than silently skipping the gate.
    const REPLAY_CORPUS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/build/fixtures/capacity-replay-corpus.tsv"
    ));

    /// The frozen ORACLE: the `event=all_exhausted` ENTERs the running daemon logged over the very
    /// same 48 h. Its independence is the whole design — it was produced by the daemon's live tick
    /// decisions, not by the aggregator, so it can refute the aggregator in a way no fixture
    /// derived from the aggregator's own premise ever can.
    const REPLAY_ORACLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/build/fixtures/capacity-replay-oracle.tsv"
    ));

    /// The corpus's redacted roster, in the order the handles first appear.
    const REPLAY_ROSTER: [&str; 6] = ["a1", "a2", "a3", "a4", "a5", "a6"];

    /// The corpus's stated shape. Asserted before every gate below, because a gate that passes on
    /// a truncated corpus is not evidence — a degraded subject makes the assertions vacuous rather
    /// than false, which is the failure mode that reads as green.
    const CORPUS_SAMPLES: usize = 1_734;
    /// ENTERs the daemon logged in the window. NOT a bound on the hold count in either direction
    /// (the daemon re-arms its guard, so it over-counts ~33x here) — used only to witness that the
    /// condition physically occurred, and reported as a diagnostic ratio.
    const ORACLE_ENTERS: usize = 67;

    /// Skip the fixture's `#` provenance header, yielding only data rows.
    fn data_rows(fixture: &str) -> impl Iterator<Item = &str> {
        fixture
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
    }

    /// Rehydrate the frozen corpus into [`Sample`]s. Offsets are re-anchored to [`CORPUS_START`];
    /// an empty reset column is the store's own `None`, which [`relief_of`] must keep treating as
    /// "return time unknown" rather than inventing one.
    fn replay_samples() -> Vec<Sample> {
        data_rows(REPLAY_CORPUS)
            .map(|line| {
                let mut f = line.split('\t');
                let mut next = || f.next().expect("corpus row has 6 tab-separated columns");
                let ts: i64 = next().parse::<i64>().expect("ts offset") + CORPUS_START;
                let acct = next();
                let session: f64 = next().parse().expect("session fraction");
                let weekly: f64 = next().parse().expect("weekly fraction");
                let reset = |s: &str| -> Option<i64> {
                    (!s.is_empty()).then(|| s.parse::<i64>().expect("reset offset") + CORPUS_START)
                };
                let (sr, wr) = (reset(next()), reset(next()));
                Sample::new(ts, "claude", acct, session, weekly).with_resets(sr, wr)
            })
            .collect()
    }

    /// Rehydrate the oracle into `(instant, held-on handle, cause)` triples. The handle is carried
    /// rather than dropped: it is what a maintainer greps the daemon's own log with when a figure
    /// below moves, and the shape guard asserts it names the SAME redacted roster as the corpus —
    /// two fixtures cut from different alias mappings would silently compare two different fleets.
    fn replay_oracle() -> Vec<(i64, &'static str, &'static str)> {
        data_rows(REPLAY_ORACLE)
            .map(|line| {
                let mut f = line.split('\t');
                let mut next = || f.next().expect("oracle row has 3 tab-separated columns");
                let ts: i64 = next().parse::<i64>().expect("ts offset") + CORPUS_START;
                (ts, next(), next())
            })
            .collect()
    }

    /// The frozen corpus aggregated at `stale_after`, over the full corpus roster.
    fn replay_at(stale_after: i64) -> RosterStats {
        let mut params = hold_params();
        params.stale_after_secs = stale_after;
        aggregate_with_roster(
            &replay_samples(),
            &[],
            Period::new(CORPUS_START, CORPUS_END),
            &params,
            Some(&roster(&REPLAY_ROSTER)),
        )
        .roster
    }

    #[test]
    fn the_frozen_replay_corpus_parses_to_its_stated_shape() {
        // The degenerate-subject guard for every gate below. A corpus silently truncated to one
        // account, or an oracle emptied by a header-parsing slip, would make the differential pass
        // on nothing at all — cardinality-zero reads exactly like cardinality-correct once the
        // assertions are `>= 0`-shaped. So the shape is asserted first, and by content.
        let samples = replay_samples();
        assert_eq!(
            samples.len(),
            CORPUS_SAMPLES,
            "corpus row count — the corpus is frozen INPUT, so this can only move if the fixture \
             itself was edited; nothing the aggregator does can reach it"
        );
        let accts: BTreeSet<&str> = samples.iter().map(|s| s.acct.as_str()).collect();
        assert_eq!(
            accts,
            REPLAY_ROSTER.iter().copied().collect::<BTreeSet<&str>>(),
            "all six redacted handles are present — a collapsed roster cannot corner a fleet"
        );
        assert!(
            samples
                .iter()
                .all(|s| (CORPUS_START..CORPUS_END).contains(&s.ts)),
            "every reading lands in the pinned window"
        );
        assert!(
            samples.iter().any(|s| s.session_resets_at.is_some())
                && samples.iter().any(|s| s.weekly_resets_at.is_some()),
            "both carried resets survive the redaction — they are what makes the census \
             reset-anchored rather than cadence-anchored"
        );

        let oracle = replay_oracle();
        assert_eq!(
            oracle.len(),
            ORACLE_ENTERS,
            "oracle ENTER count — frozen INPUT, same as the corpus"
        );
        assert!(
            oracle
                .iter()
                .all(|(ts, _, cause)| (CORPUS_START..CORPUS_END).contains(ts)
                    && (*cause == "session" || *cause == "weekly")),
            "every ENTER is in-window and names one of the daemon's two causes"
        );
        assert!(
            oracle
                .iter()
                .all(|(_, hold, _)| REPLAY_ROSTER.contains(hold)),
            "an ENTER names a handle outside the corpus roster — the two fixtures were cut from \
             different redaction mappings, so the differential is comparing two different fleets \
             (and an un-aliased handle here is a leaked real address)"
        );
    }

    #[test]
    fn the_replay_corpus_detects_the_capacity_holds_the_daemon_logged() {
        // THE GATE. Two surfaces, two independent premises, one physical condition: the daemon's
        // live tick decisions said it was cornered 67 times over these 48 h, so an offline census
        // folded from the same window's samples MUST also see it. This is the shape of assertion
        // that the pre-#803 suite could not make — every all-high fixture was derived from the
        // aggregator's own premise, so none of them could refute it.
        //
        // The direction is DETECTION, never a count comparison: the daemon re-arms its ENTER guard
        // on any non-`NoViableTarget` tick, so `count(ENTER)` over-counts episodes by design and
        // `episodes <= ENTERs` is unsound in both directions (merging collapses many ENTERs into
        // one hold; a coverage gap fragments one hold into several). The ratio is reported below
        // as a diagnostic and gated on nowhere.
        let oracle = replay_oracle();
        assert_eq!(
            oracle.len(),
            ORACLE_ENTERS,
            "antecedent: the daemon did report itself cornered in this window"
        );

        let r = replay_at(300);
        assert!(
            r.capacity_hold_covered_secs > 0,
            "the census reports UNKNOWN over a window the daemon spent cornered — a `—` printed \
             over a real drain is the exact blindness this corpus exists to catch"
        );
        assert!(
            r.capacity_holds > 0,
            "the daemon logged {} all-exhausted ENTERs here and the census found no hold at all",
            oracle.len()
        );
        assert!(
            r.capacity_hold_secs_lower_bound > 0,
            "positive witness: a held count with zero held seconds is a stub, not a measurement"
        );

        // Cause agreement. The oracle exercises BOTH of the daemon's causes (12 session, 55
        // weekly), so a census that can only ever name one — a tie-rule or relief-preference
        // regression — disagrees with the surface it is supposed to reconcile against.
        let (o_session, o_weekly) = oracle.iter().fold((0, 0), |(s, w), (_, _, cause)| {
            if *cause == "session" {
                (s + 1, w)
            } else {
                (s, w + 1)
            }
        });
        assert!(
            o_session > 0 && o_weekly > 0,
            "antecedent: the oracle itself names both causes ({o_session} session, {o_weekly} weekly)"
        );
        assert!(
            r.capacity_holds_session > 0 && r.capacity_holds_weekly > 0,
            "the daemon named both causes over this window but the census named only one \
             (session {}, weekly {})",
            r.capacity_holds_session,
            r.capacity_holds_weekly
        );
        assert_eq!(
            r.capacity_holds_session + r.capacity_holds_weekly,
            r.capacity_holds,
            "the cause split partitions the count exactly — every hold is named one or the other"
        );

        // Frozen regression pins. The corpus is immutable INPUT, so these are stable by
        // construction: any movement is a change in the aggregator, which is the point.
        assert_eq!(
            (
                r.capacity_holds,
                r.capacity_holds_session,
                r.capacity_holds_weekly,
                r.capacity_hold_secs_lower_bound,
                r.capacity_hold_covered_secs,
            ),
            (2, 1, 1, 74_961, 157_187),
            "frozen replay figures moved — the corpus did not, so the aggregator did. Order is \
             (holds, session, weekly, held_secs_lower_bound, covered_secs); the held figure is the \
             same one `..._under_both_real_poll_cadences` pins at its near horizon, so a genuine \
             rebaseline moves BOTH tests and both are supposed to be re-derived, not edited to fit"
        );

        // Reported, never gated (see above): no tuning-accident inequality is enforced anywhere on
        // this ratio. It surfaces when the test fails — which is when a maintainer wants to know
        // how far the two surfaces sit apart — and under `cargo test -- --nocapture` otherwise.
        eprintln!(
            "[#806 diagnostic] daemon ENTERs {} vs census holds {} — ratio {:.1}x; held >= {} s \
             over {} s jointly covered",
            oracle.len(),
            r.capacity_holds,
            oracle.len() as f64 / f64::from(r.capacity_holds),
            r.capacity_hold_secs_lower_bound,
            r.capacity_hold_covered_secs,
        );
    }

    #[test]
    fn the_replay_corpus_detects_the_same_holds_under_both_real_poll_cadences() {
        // `capacity_holds_are_insensitive_to_the_staleness_horizon` asserts this on four synthetic
        // readings; this asserts it on 1,734 real ones, with the real gaps, the real
        // `exhausted_poll_secs` sparsity, and the real resets. That is the falsifier with teeth:
        // cadence-anchoring this corpus swings the count 0 -> 69 across exactly these two horizons
        // (0 -> 84 over the full 7 d store, the figure `capacity_holds` cites), so detection
        // agreement here is not something a broken window can fake.
        let (near, far) = (replay_at(300), replay_at(3_600));
        assert_eq!(
            (
                near.capacity_holds,
                near.capacity_holds_session,
                near.capacity_holds_weekly
            ),
            (
                far.capacity_holds,
                far.capacity_holds_session,
                far.capacity_holds_weekly
            ),
            "moving stale_after between poll_secs (300) and exhausted_poll_secs (3600) moved the \
             answer — that is a reading of the poll schedule, not of the fleet. Order is (holds, \
             session, weekly), left at 300 and right at 3600"
        );
        assert!(
            near.capacity_holds > 0,
            "positive witness: two equal ZEROES would satisfy the equality above vacuously"
        );
        // Covered time may legitimately grow with the horizon (an unblocked reading's plain
        // cadence window is longer), so only DETECTION is invariant — which is precisely the
        // refinement gap honesty permits. Held time is pinned at BOTH horizons rather than bounded
        // by a tolerance: the far horizon is exactly as deterministic as the near one over frozen
        // input, so a tuned percentage would only be a place for a real drift to hide.
        assert_eq!(
            (
                near.capacity_hold_secs_lower_bound,
                far.capacity_hold_secs_lower_bound
            ),
            (74_961, 75_213),
            "held time moved across the two horizons — reset-anchored spans barely notice the \
             staleness knob, so this is a window that has stopped being anchored to its reset. \
             Order is (at 300, at 3600); the near figure is the same one the gate above pins"
        );
    }

    #[test]
    fn on_the_replay_corpus_the_utilisation_census_is_unknown_where_the_capacity_census_measures() {
        // The shipped blindness, frozen — and stated as the fact it is rather than as a bare
        // inequality. On this window the two readouts do not merely differ: the cadence-anchored
        // utilisation census cannot be TAKEN AT ALL (zero jointly-covered seconds, the documented
        // UNKNOWN sentinel), while the reset-anchored capacity census measures a real drain over
        // the very same samples. One cannot see; the other can. That asymmetry is the entire
        // reason issue #803 made capacity its own readout instead of a parameterisation of the
        // utilisation water.
        //
        // Pinning the UNKNOWN rather than asserting `!=` is the stronger gate in both directions:
        // it fails if #804's gap honesty is ever repealed into a fabricated calm zero, AND it
        // fails if the two censuses are conflated onto one predicate. It asserts nothing about
        // whether either figure is individually right, and touches no existing all-high test.
        let r = replay_at(300);
        assert_eq!(
            (r.all_high_covered_secs, r.all_high_episodes),
            (0, 0),
            "the utilisation census reports jointly-covered time on this corpus — either gap \
             honesty moved, or the two censuses now share one anchoring"
        );
        assert!(
            r.capacity_hold_covered_secs > 0 && r.capacity_holds > 0,
            "the capacity census went UNKNOWN alongside the utilisation one, so this window now \
             has no readout at all — which is the state issue #803 exists to prevent"
        );
    }

    // --- #806 roster-size scaling (T2) ------------------------------------------

    /// A fully-blocked roster of `n` accounts sampled on a shared cadence, each account's readings
    /// offset by `jitter` seconds from the previous one's — the shape a real poll produces, where
    /// accounts are visited in sequence rather than simultaneously.
    ///
    /// Per-account coverage is held FIXED as `n` grows: every account gets the same number of
    /// readings at the same spacing. Only the roster SIZE varies, which is the whole point.
    fn scaling_corpus(n: usize, jitter: i64, cadence: i64, readings: i64) -> Vec<Sample> {
        let mut out = Vec::new();
        for (i, handle) in REPLAY_ROSTER.iter().take(n).enumerate() {
            for k in 0..readings {
                let ts = k * cadence + (i as i64) * jitter;
                let relief = ts + cadence * readings;
                out.push(dated(ts, handle, 0.95, 0.99, relief, relief));
            }
        }
        out
    }

    /// A [`scaling_corpus`] aggregated over the fixed sweep window, at the default boundary — the
    /// synthetic twin of [`replay_at`]. The window is 100 000 s, comfortably longer than either
    /// regime's own span, so every arm is bounded by the coverage being probed rather than by the
    /// period clipping it.
    fn scaling_at(n: usize, jitter: i64, cadence: i64, readings: i64) -> RosterStats {
        aggregate_with_roster(
            &scaling_corpus(n, jitter, cadence, readings),
            &[],
            Period::new(0, 100_000),
            &hold_params(),
            Some(&roster(&REPLAY_ROSTER[..n])),
        )
        .roster
    }

    /// The two sampling regimes the sweep runs in, as `(cadence, readings, label)`.
    ///
    /// The second one is the point. A corpus whose cadence EQUALS `stale_after` (300/300) has every
    /// window abutting the next, so per-account coverage is ~100 %, ∏ᵢcoverageᵢ is 1, and the very
    /// decay the sweep exists to detect is structurally unreachable — which is the mechanical
    /// blindness issue #806 indicts in the pre-existing fixtures, and it would be self-defeating to
    /// reproduce it here. Production runs `exhausted_poll_secs` (3600) against a `poll_secs` (300)
    /// staleness horizon: a 12× UNDER-coverage, in exactly the regime being measured.
    const SCALING_REGIMES: [(i64, i64, &str); 2] =
        [(300, 100, "abutting"), (3_600, 12, "under-covered")];

    #[test]
    fn the_capacity_census_does_not_decay_as_the_roster_grows_at_fixed_per_account_coverage() {
        // T2 — the property that would have caught the shipped defect before it shipped, and the
        // cheapest of the lot. Both censuses are N-fold INTERSECTIONS, so the naive expectation is
        // that their value decays as the product of the per-account coverages: at the roster's real
        // coverages (24-44%) that product is 0.076% of a 168 h window, and at N=1 the penalty is
        // absent entirely while at N=2 — every unit fixture in this file — it is merely squared.
        // A defect that only bites at N >= 3 is therefore invisible to the whole unit suite.
        //
        // Held at FIXED per-account coverage, with every account equally blocked, growing the
        // roster must not erode the CAPACITY figure: the accounts agree, so their intersection is
        // their common span. Anything else is the intersection losing spans per fold. The
        // cadence-anchored utilisation census is held to the weaker bar it actually deserves — it
        // may go honestly UNKNOWN as the roster grows apart, but it may never fabricate a calm
        // zero, which is the branch at the foot of the loop.
        for &(cadence, readings, regime) in &SCALING_REGIMES {
            // 0 = simultaneous polls; 60 = staggered, the shape a real poll produces by visiting
            // accounts in sequence. The stagger is what makes the sweep discriminating: at jitter 0
            // a correct N-fold intersection and a DELETED one both return a constant, so only the
            // staggered arm can tell them apart.
            for &jitter in &[0_i64, 60] {
                let first = scaling_at(1, jitter, cadence, readings);
                for n in 1..=REPLAY_ROSTER.len() {
                    let r = scaling_at(n, jitter, cadence, readings);
                    let case = format!("regime {regime}, jitter {jitter}, roster size {n}");

                    // Positive witness at EVERY n, not merely at the end: on the jitter-0 arm the
                    // exact equality below degenerates to `0 == 0 - 0`, so an all-zero stub would
                    // satisfy it at every roster size. This is what makes the sweep non-vacuous.
                    assert!(
                        r.capacity_holds > 0 && r.capacity_hold_secs_lower_bound > 0,
                        "capacity census collapsed at {case} — {} holds / {} s. The roster grew; \
                         per-account coverage did not change.",
                        r.capacity_holds,
                        r.capacity_hold_secs_lower_bound
                    );

                    // The EXACT erosion, not a tolerance. Every account is equally covered and
                    // equally blocked, so the intersection is their common span and the only thing
                    // an added account can take is the `jitter` seconds by which its first reading
                    // starts later. A defect that drops or duplicates a fold breaks this equality
                    // immediately — including one that deletes the fold entirely, which holds the
                    // figure CONSTANT where it must decay.
                    assert_eq!(
                        r.capacity_hold_secs_lower_bound,
                        first.capacity_hold_secs_lower_bound - jitter * (n as i64 - 1),
                        "held time at {case} is not first-minus-stagger — the roster dimension \
                         is being folded wrongly, or not at all"
                    );
                    assert_eq!(
                        r.capacity_holds, first.capacity_holds,
                        "the roster grew and FRAGMENTED the hold at {case}"
                    );

                    // The utilisation census is cadence-anchored, so in the UNDER-COVERED regime it
                    // legitimately goes blind as the roster grows — that is REQ-STA-B-008 gap
                    // honesty, not a defect, and it is exactly why issue #803 gave capacity its own
                    // readout. What it must NEVER do is fabricate a calm zero: an unmeasurable
                    // census has to say so through a zero denominator (issue #804).
                    if r.all_high_covered_secs == 0 {
                        assert_eq!(
                            r.all_high_episodes, 0,
                            "utilisation census reported episodes it could not have seen at \
                             {case}"
                        );
                        assert!(
                            regime == "under-covered" && jitter > 0,
                            "utilisation census went UNKNOWN at {case}, where every account is \
                             fully covered — it should have been measurable"
                        );
                    } else {
                        assert!(
                            r.all_high_episodes > 0 && r.all_high_secs > 0,
                            "utilisation census measured {} s jointly yet found no episode at \
                             {case}, where every account is high throughout",
                            r.all_high_covered_secs
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn under_real_poll_sparsity_a_growing_roster_blinds_the_census_but_not_the_capacity_readout() {
        // The shipped defect, reproduced synthetically at the roster size and in the sampling
        // regime where it actually bit — the companion to the frozen live corpus above, and the
        // single clearest statement of why these are two readouts and not one.
        //
        // Six accounts, every one of them blocked for the whole window, polled at
        // `exhausted_poll_secs` (3600) against a `poll_secs` (300) staleness horizon and staggered
        // 60 s apart as a real sequential poll staggers them. Each account is observable for 300 s
        // out of every 3600, and the six 300 s windows walk apart until, at the sixth account, they
        // no longer share a single instant.
        //
        // This is the sweep's under-covered / jitter-60 / n=6 arm, and deliberately not only that:
        // the sweep pins held time RELATIVELY (`first` minus the stagger), which a uniform scaling
        // of every figure would survive intact. The absolute pin below is what closes that door,
        // so this is not the sweep restated.
        let six = scaling_at(6, 60, 3_600, 12);

        assert_eq!(
            (six.all_high_episodes, six.all_high_covered_secs),
            (0, 0),
            "the cadence-anchored utilisation census still has joint visibility here — if this \
             ever becomes measurable the regime has changed and the comparison below is no longer \
             the one this test was written to make"
        );
        assert!(
            six.capacity_hold_covered_secs > 0 && six.capacity_hold_secs_lower_bound > 0,
            "the RESET-anchored capacity census went blind in the same breath as the utilisation \
             one — reset-anchoring is the only reason it can see here, and this is the regression \
             that would resurrect `all-accounts-high: 0 episodes` as the fleet's only answer while \
             the daemon sits cornered"
        );
        assert_eq!(
            six.capacity_hold_secs_lower_bound, 82_500,
            "held time under real poll sparsity moved. This corpus is synthetic and fully \
             deterministic (6 accounts x 12 readings at 3600 s, staggered 60 s), so the input did \
             not move and the aggregator did — and the sweep's relative pin is blind to a uniform \
             rescale, which is exactly why this figure is spelled out here"
        );
    }

    #[test]
    fn a_single_account_roster_hides_the_intersection_the_sweep_exercises() {
        // Why the sweep above needs its full range, stated as a test rather than a comment. At
        // N=1 an "intersection" is just the account's own span, so every intersection defect —
        // dropped fold, mis-ordered merge, emptied accumulator — is unreachable. This pins that
        // N=1 and N=6 are genuinely different computations over the same per-account data.
        //
        // The sweep's exact identity already IMPLIES this inequality, so as an assertion it is
        // redundant today. It is kept as a standalone tripwire for the one edit that would make it
        // stop being redundant: trimming the sweep to "one representative roster size". After such
        // a trim this test still states the fact the sweep would have stopped covering, which a
        // comment inside the deleted loop could not.
        let one = scaling_at(1, 60, 300, 100);
        let six = scaling_at(6, 60, 300, 100);
        assert!(
            one.capacity_hold_secs_lower_bound > 0 && six.capacity_hold_secs_lower_bound > 0,
            "positive witness, kept for the DIAGNOSIS it gives rather than for extra strength: a \
             zero stub also trips the inequality below, but as `0 != 0`, which reads as \"the \
             roster dimension collapsed\" when the truth is \"nothing was measured at all\" — two \
             different defects in two different places (N=1 {} s, N=6 {} s)",
            one.capacity_hold_secs_lower_bound,
            six.capacity_hold_secs_lower_bound
        );
        assert_ne!(
            one.capacity_hold_secs_lower_bound, six.capacity_hold_secs_lower_bound,
            "N=1 and N=6 produced identical held time — the roster dimension is not being \
             intersected over at all"
        );
    }

    // --- test constructors ----------------------------------------------------

    /// A swap with placeholder handles at `ts` with `kind`.
    fn mk_swap(ts: i64, kind: SwapKind) -> SwapEvent {
        mk_swap_ab(ts, "work", "play", kind)
    }

    /// A swap from `from` to `to` at `ts` with `kind`.
    fn mk_swap_ab(ts: i64, from: &str, to: &str, kind: SwapKind) -> SwapEvent {
        SwapEvent {
            ts,
            from: from.to_owned(),
            to: to.to_owned(),
            kind,
        }
    }
}
