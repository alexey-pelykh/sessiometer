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
//!   during which EVERY rostered account was simultaneously at/above a high-water
//!   threshold (the danger state: no healthy account to rotate to).
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

use std::collections::BTreeMap;

use serde::Serialize;

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

    /// The window's length in seconds (`0` for an empty/inverted window).
    fn duration(&self) -> i64 {
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
}

impl AggregateParams {
    /// Params with `stale_after_secs` defaulted to the poll cadence — a reading covers
    /// exactly one nominal poll interval forward unless a newer reading supersedes it.
    pub(crate) fn new(poll_interval_secs: i64, session_cap: f64, high_threshold: f64) -> Self {
        Self {
            poll_interval_secs,
            session_cap,
            high_threshold,
            stale_after_secs: poll_interval_secs,
        }
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
    pub(crate) all_high_episodes: u32,
    /// Total duration (seconds) of those all-accounts-high intervals.
    pub(crate) all_high_secs: i64,
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
pub(crate) fn aggregate(
    samples: &[Sample],
    swaps: &[SwapEvent],
    period: Period,
    params: &AggregateParams,
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

    let (all_high_episodes, all_high_secs) = all_high(&by_acct, period, params);
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

/// Count + total duration of the intervals during which EVERY rostered account is
/// simultaneously known-and-at/above `high_threshold` — the "no healthy account left"
/// danger state.
///
/// Each account contributes the disjoint intervals over which its readings are BOTH
/// covering (not a gap) AND high; the all-high intervals are the intersection of every
/// account's high-interval set. An empty roster, or any account with no high interval,
/// yields no episodes. Because a gap produces no covering interval, an instant where any
/// account is unsampled cannot be part of an episode — gaps are UNKNOWN, never high.
///
/// Returns `(episode_count, total_secs)`.
fn all_high(
    by_acct: &BTreeMap<&str, Vec<&Sample>>,
    period: Period,
    params: &AggregateParams,
) -> (u32, i64) {
    let mut acc: Option<Vec<(i64, i64)>> = None;
    for group in by_acct.values() {
        let windows = validity_windows(group, period, params.stale_after_secs);
        let highs: Vec<(i64, i64)> = windows
            .iter()
            .zip(group.iter())
            .filter(|(_, s)| s.session >= params.high_threshold)
            .map(|(&w, _)| w)
            .collect();
        let highs = merge_intervals(highs);
        acc = Some(match acc {
            None => highs,
            Some(prev) => intersect(&prev, &highs),
        });
        // Empty ∩ anything stays empty — nothing more can become all-high.
        if acc.as_ref().is_some_and(Vec::is_empty) {
            break;
        }
    }

    let episodes = merge_intervals(acc.unwrap_or_default());
    (
        episodes.len() as u32,
        episodes.iter().map(|(lo, hi)| hi - lo).sum(),
    )
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
        fn below(&mut self, n: u64) -> u64 {
            self.next_u64() % n
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
        }
    }

    #[test]
    fn prop_cap_hits_are_monotone_in_the_threshold() {
        let mut rng = Lcg::new(0xCAF_E157);
        let period = Period::new(0, 100_000);
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
        }
    }

    #[test]
    fn prop_all_high_never_exceeds_period_duration() {
        let mut rng = Lcg::new(0x8118_2026);
        let accounts = ["work", "play"];
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
        }
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
