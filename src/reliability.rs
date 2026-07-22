// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The `reliability` verb — an OFFLINE reliability-SLO readout over the event log (issue #455).
//!
//! `sessiometer reliability [--since <duration>] [--json]` aggregates the durable event log
//! (`~/Library/Logs/sessiometer/sessiometer.log`, written by [`crate::observability`]) into
//! four service-level indicators for the reaction-latency / bounded-blindness work (umbrella
//! #363), each with its documented target, so the swap-out behavior is provably meeting its
//! SLOs and a regression is caught:
//!
//! 1. **swap-out `session_pct` P50/P95/P100** — how late the active account is when it swaps
//!    out. Targets: **P100 < 99** and **P50 <= 97** (the extended #363 acceptance). Measured
//!    over `reason=session` swaps ONLY: a weekly swap fires while session usage is BELOW its
//!    trigger, so its `session_pct` is a low, incidental value — not a session overshoot at
//!    all — and this increment is session-limit-latency-scoped (weekly cadence is out of
//!    scope per `hq/strategy/prd-swap-latency.md` §6). `manual`/`forced` (`session_pct=0`)
//!    and `emergency_swap` (no `session_pct`) are likewise excluded.
//! 2. **time blind & near-limit** — the summed `blind_window` duration while the account's
//!    retained anchor was in the risk band (`near_limit=true`).
//! 3. **false-preempt** — preemptive swaps whose target turned out unnecessary. The real
//!    rate needs the #452 preemptive-swap path (still pending), so today it is reported as
//!    `0 observed` alongside a clearly-labeled forward-looking PROXY derived from the
//!    `blind_window` recovery reconciliation (a hypothetical anchor-keyed swap is "would-be
//!    wasted" when the fresh recovery reading had dropped well below the stale anchor).
//! 4. **429-rate neutrality** — the roster-wide `usage_backoff` rate-limit vs transient
//!    counts, so a regression that raises the usage-poll 429 rate is caught. (Per-active-
//!    account attribution needs the swap timeline the readout forgoes; a roster-wide count
//!    is the v1 indicator — precise active attribution is a follow-up.)
//! 5. **landing-point swap-out overshoot** (issue #595) — the peak `session_pct` the OUTGOING
//!    account actually reaches within a bounded (~15 min) window AFTER a `reason=session` swap
//!    parks it. SLI 1 measures the swap DECISION point (where the daemon fired); this measures
//!    where the parked account LANDED — in-flight work keeps billing the outgoing credential
//!    after the swap redirects new requests, so an on-target 95 swap can still land ≥99 unseen.
//!    Attribution found 46% of ≥99 breaches are this post-swap tail, invisible to SLI 1. Excludes
//!    any window minutes after the account is re-activated (`active_at != acct`), and splits the
//!    measured episodes into the three breach classes (post-swap tail / gap-crossing / blind-burn).
//!    Reconstructed offline by joining the event log's swaps with the daemon's per-account usage
//!    samples — the two-source recipe spike #596 used; it neither adds an event nor changes the
//!    swap mechanism it measures (the #449 → #452 precedent: expose the SLI before the fix).
//! 6. **blind-arm projection error** (issue #636) — `projected − session_at_recovery` for the
//!    REPORT-ONLY blind velocity-projection arm ([`crate::daemon`]'s
//!    `blind_velocity_projected_armed`, issues #584/#600). That arm fires no swap and emits no
//!    event of its own, so its forecast was unfalsifiable until #634 stamped its ingredients onto
//!    `blind_window`; `projected` is RECOMPUTED here from those tokens and reconciled against the
//!    durable actual already on the same line. BLIND-ARM ONLY: the velocity arm's actual is a
//!    counterfactual (the swap parks the account, so its crossing never happens), which is why
//!    `false_projection.rate` stays `None` rather than gaining a symmetric forecast-error rate.
//!    The error percentiles are published PAIRED with their cardinality + censoring counts (the
//!    survivorship guard, issue #484) and never bare.
//!
//! Like `stats` (issue #158) this is an OFFLINE reader: it reads the daemon's durable files
//! directly — the event log for SLIs 1-4, plus (for SLI 5) the `usage-samples.jsonl` store
//! (issue #155) — and makes no live control-socket / keychain / usage-API call, so it renders
//! when the daemon is down. The daemon is the sole WRITER of both files, this verb one READER.
//! The readout is roster-wide (no per-account breakdown), so it emits no account identifier at
//! all — the sample store's `acct` roster label is a join key used INTERNALLY only; every output
//! line is bare numbers and fixed labels, secret-free by construction (issue #15); the
//! durable-line redaction test in this module asserts it over both the event-log and sample paths.
//!
//! The targets are INTERIM constants with in-code provenance, matching the SLI interim
//! constants in [`crate::daemon`] (`BLIND_GATE_SECS` / `BLIND_GATE_RISK_BAND`): a config
//! surface for them is premature until they are ratified against production (issues
//! #451/#484). This verb is a pure READER — it changes no state, adds no event, and does not
//! build the #452 fix it measures.
//!
//! By default the four indicators fold the WHOLE log. `--since <duration>` (issue #494) bounds
//! them to a recent window — every event whose `ts=` is at/after `now - duration` — so a recent
//! regression (or recovery) is not diluted by ancient data as the durable log grows. The window
//! is duration-only (`<int><unit>`, units `s`/`m`/`h`/`d`/`w`), hand-rolled per the
//! minimal-dependency line (no date crate); the `ts=` parse and the cutoff render reuse the
//! crate's existing civil-date primitives ([`crate::usage::epoch_from_rfc3339`] and
//! [`crate::observability::rfc3339`]) rather than a second calendar routine. The default (no
//! flag) is unchanged and backward-compatible.
//!
//! **Single-machine-sync boundary (issue #613).** Every SLI here is PER-MACHINE: this
//! reader folds only THIS daemon's own durable files, so it can never see a second
//! machine co-consuming the same roster (Sessiometer has no shared backend; the full
//! treatment is in [`crate::swap`]'s boundary note). SLI 5 above is the sharpest case — a
//! parked account another machine pushed past the ceiling never lands in this machine's
//! samples, so that overshoot goes unmeasured here. [`crate::landing`] is the RUNTIME
//! mirror of SLI 5 and carries the identical per-machine bound; velocity-spike detection
//! (which reads the account-global `/oauth/usage` signal that DOES reflect both machines'
//! combined burn) is the partial mitigation, not a fix.

use crate::error::{Error, Result};
use crate::usage::epoch_from_rfc3339;
use crate::usage_store::Sample;
use std::collections::BTreeMap;

/// SLO target: swap-out `session_pct` **P100 must be `< 99`** — no `reason=session` swap fires
/// at or above 99%. INTERIM per issue #455 (the extended #363 acceptance); the source of
/// truth until the #451/#484 confirmation gate finalizes it against production — the
/// interim-const-with-provenance stance of [`crate::daemon`]'s `BLIND_GATE_*`.
///
/// `pub(crate)` so the RUNTIME landing-overshoot detector ([`crate::landing`], issue #613) checks
/// the SAME ceiling this OFFLINE reader does — one SLO line, referenced from both, so the runtime
/// and offline signals cannot drift.
pub(crate) const SLO_SWAP_P100_MAX: u8 = 99;

/// SLO target: swap-out `session_pct` **P50 must be `<= 97`** (median swap-out lands in the
/// [95, 97] band, not later). INTERIM per issue #455; see [`SLO_SWAP_P100_MAX`] for the
/// finalization gate. Note the comparator differs from P100 — inclusive here, strict there.
const SLO_SWAP_P50_MAX: u8 = 97;

/// SLO target: PROJECTED swap-out `session_pct` **P100 must be `<= 98`** — the #539 velocity-
/// projection preemptive trigger's acceptance on COVERED swaps (an active account with a usable
/// near-limit reading to project from). Measured over `reason=velocity_preempt` swaps ONLY —
/// separate from the reactive `reason=session` distribution above, which post-#539 is the poll-gap
/// RESIDUAL the sibling #540 (near-limit poll coverage) owns; the full-trace `P100 < 99` is met by
/// #539 + #540 together. INTERIM: the #538 spike's measured result (P100=98 on 67/76 covered swaps),
/// the source of truth until the #451/#484 production gate finalizes it — the same interim-const
/// stance as [`SLO_SWAP_P100_MAX`]. Note the comparator is INCLUSIVE (`<=`), unlike the strict
/// full-trace P100.
const SLO_PROJECTED_SWAP_P100_MAX: u8 = 98;

/// SLO target: PROJECTED swap-out `session_pct` **P50 must be `<= 94`** (the #538-measured median
/// projected swap-out on covered swaps — ~5 pp more runway than the θ=88 stopgap, adaptively).
/// INTERIM per issue #539/#538; see [`SLO_PROJECTED_SWAP_P100_MAX`] for the finalization gate.
const SLO_PROJECTED_SWAP_P50_MAX: u8 = 94;

/// Proxy margin (percentage points) for the #452-pending false-preempt SLI: a hypothetical
/// anchor-keyed preemptive swap is classed "would-be wasted" when the fresh recovery reading
/// had dropped more than this far below the stale pre-blind anchor. INTERIM (issue #455); the
/// real necessary/wasted threshold is #451/#484's to derive — this only supplies the
/// ingredient, exactly as the `blind_window` SLI records the raw readings rather than a baked
/// verdict.
const PREEMPT_WASTED_MARGIN_PCT: u8 = 20;

/// The bounded post-swap observation window (issue #595): the peak `session_pct` the OUTGOING
/// (parked) account reaches within this many seconds of a `reason=session` swap is its LANDING
/// point. `~15 min` per the issue — generously above the spike #596-measured tail settling (the
/// post-swap climb settled `≤ 455 s` after the swap), so the peak is captured with margin without
/// bleeding into the next session cycle. INTERIM per issue #595 — a config surface is premature
/// until the #451/#484 production gate finalizes it, the same interim-const-with-provenance stance
/// as [`SLO_SWAP_P100_MAX`]. The landing point is measured against that same `< 99` ceiling: SLI 1
/// checks it on the swap DECISION reading, this on where the parked account actually LANDED.
///
/// `pub(crate)` so the RUNTIME landing-overshoot detector ([`crate::landing`], issue #613) bounds
/// its live watch to the SAME window this OFFLINE reconstruction does — tied so the two cannot drift.
pub(crate) const LANDING_WINDOW_SECS: i64 = 15 * 60;

/// The stable `--json` schema version. Owned by this readout, independent of `stats`'
/// schema. Named to match [`crate::stats`]'s own `JSON_SCHEMA_VERSION`. Bumped `1 → 2` when
/// the `--since` window (issue #494) added the top-level `window` object; bumped `2 → 3` when
/// the #539 velocity-projection trigger added the `projective_swap_out_pct` + `false_projection`
/// objects; bumped `3 → 4` when the #595 landing-point SLI added the `landing` object; bumped
/// `4 → 5` when the #608 observed session-velocity SLI added the `observed_peak` object (the live
/// peak vs the assumed `v_peak` the coupling bound is calibrated on) — every bump through 5
/// ADDITIVE (always-present new fields), so a `--json` consumer of the #363 acceptance gate that
/// ignores unknown fields still parses every prior field unchanged. Bumped `5 → 6`
/// (issue #635) by RENAMING the velocity-projection block's key to `projective_swap_out_pct` — the one
/// non-additive bump: the block measures the OBSERVED session_pct at projection-triggered swaps, not
/// projection error, so the prior name (implying tracked projection accuracy) was corrected. Bumped
/// `6 → 7` when the #636 blind-arm projection-error SLI added the `blind_projection_error` object —
/// ADDITIVE again (a new always-present field; every schema:6 key is byte-identical), so the rename
/// at 6 stays the lone non-additive bump.
const JSON_SCHEMA_VERSION: u32 = 7;

/// Parsed `reliability` options (issues #455/#494). A plain comparable value so the CLI parser
/// is unit-testable by value, like `StatsArgs`.
#[derive(Debug, PartialEq)]
pub(crate) struct ReliabilityArgs {
    /// `--json` — print the machine-readable readout (for scripts / the #363 acceptance gate)
    /// instead of the human text.
    pub(crate) json: bool,
    /// `--since <duration>` — bound all four SLIs to events at/after `now - duration`. The RAW
    /// value as given (e.g. `"7d"`); parsed and validated in [`run`], where the wall clock is
    /// read (mirrors `StatsArgs::since`). `None` = the whole-log aggregate (backward-compatible
    /// default).
    pub(crate) since: Option<String>,
}

/// Entry point for the `reliability` verb: read the event log once, aggregate, and render.
/// The two impure steps are reading the log file and (for `--since`) reading the wall clock;
/// everything else is a pure function of the text and the resolved cutoff. Not `async` — it
/// makes no live call (mirrors the read-only `config` verbs).
pub(crate) fn run(args: ReliabilityArgs) -> Result<()> {
    let text = read_event_log()?;
    // Resolve the optional window against the wall clock BEFORE parsing, so the cutoff is a
    // plain integer the pure aggregation path can filter by. A malformed `--since` fails here,
    // before any output, as `Error::ReliabilitySinceInvalid`.
    let window = match args.since.as_deref() {
        Some(raw) => Some(Window::resolve(raw, now_epoch())?),
        None => None,
    };
    let cutoff = window.as_ref().map(|w| w.cutoff_epoch);
    // The SECOND daemon-written source (issue #595): the raw usage samples, joined with the event
    // log's swap anchors to reconstruct the landing point. Read whole (the join windows per anchor);
    // an absent store reads as empty, so the landing SLI degrades to "no episodes", never an error.
    let samples = read_usage_samples()?;
    let report = aggregate(&parse_events(&text, cutoff), &samples, window);
    let out = if args.json {
        render_json(&report)?
    } else {
        render_human(&report)
    };
    print!("{out}");
    Ok(())
}

/// Current wall clock as epoch seconds (`0` on the pre-1970 impossible case) — the crate's
/// display-path clock read (mirrors [`crate::stats`]'s `wall_clock_now`). Only reached when
/// `--since` is given; the default whole-log path reads no clock.
fn now_epoch() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The active `--since` window (issue #494). Present only when `--since` was given; its
/// absence is the whole-log default. Carries the raw span (echoed in output exactly as the
/// operator typed it) plus the absolute cutoff, so both renderers can document the window and
/// [`parse_events`] can drop pre-cutoff lines.
#[derive(Debug, PartialEq)]
struct Window {
    /// The raw `--since` value, echoed verbatim in the human + JSON output (e.g. `"7d"`).
    since_arg: String,
    /// Events whose `ts=` is `<` this epoch-second cutoff are excluded; at/after are kept.
    /// Clamped to `>= 0`, so a span wider than the log's age simply means "the whole log".
    cutoff_epoch: i64,
}

impl Window {
    /// Resolve a raw `--since` value against `now` (epoch seconds) into a [`Window`]. Malformed
    /// input is [`Error::ReliabilitySinceInvalid`]. Saturating throughout: an absurd span can
    /// never overflow into a future cutoff, and a span reaching past the epoch clamps to `0`.
    fn resolve(raw: &str, now: i64) -> Result<Window> {
        let secs = parse_duration_secs(raw)?;
        // i64 `now` − u64 `secs` → `saturating_sub_unsigned`; `.max(0)` then floors a
        // past-the-epoch result at 0 (the saturating rationale is on the doc comment above).
        let cutoff_epoch = now.saturating_sub_unsigned(secs).max(0);
        Ok(Window {
            since_arg: raw.trim().to_owned(),
            cutoff_epoch,
        })
    }

    /// The cutoff rendered back to the event log's own RFC 3339 UTC shape, for display —
    /// through the SAME [`crate::observability::rfc3339`] the log writes `ts=` with, so a
    /// documented window reads in the identical format as the lines it bounds.
    fn cutoff_rfc3339(&self) -> String {
        use std::time::{Duration, UNIX_EPOCH};
        // cutoff_epoch is clamped `>= 0`, so the `as u64` cast is lossless (no wraparound).
        crate::observability::rfc3339(UNIX_EPOCH + Duration::from_secs(self.cutoff_epoch as u64))
    }
}

/// Parse a relative-duration `<non-negative int><unit>` into whole seconds (issue #494). Units:
/// `s`/`m`/`h`/`d`/`w` (seconds/minutes/hours/days/weeks) — the same vocabulary as the relative
/// branch of `stats --since`, minus its absolute-date forms (an absolute date is out of this
/// window's scope; the issue asks a duration). Rejected as [`Error::ReliabilitySinceInvalid`]:
/// an empty string, a missing or unknown unit, and a non-integer, negative, or empty count.
/// Saturating multiply, so an absurd count yields `u64::MAX` (→ a clamped cutoff) rather than
/// overflow. Hand-rolled per the minimal-dependency line — no date crate.
fn parse_duration_secs(raw: &str) -> Result<u64> {
    let s = raw.trim();
    let invalid = || Error::ReliabilitySinceInvalid(s.to_owned());
    let unit = s.chars().last().ok_or_else(invalid)?;
    let per_unit: u64 = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3_600,
        'd' => 86_400,
        'w' => 7 * 86_400,
        _ => return Err(invalid()),
    };
    // The count is everything before the unit char. `parse::<u64>` inherently rejects a
    // negative sign, an empty string, and any non-digit — no separate sign/empty guard needed.
    let digits = &s[..s.len() - unit.len_utf8()];
    let n: u64 = digits.parse().map_err(|_| invalid())?;
    Ok(n.saturating_mul(per_unit))
}

/// The event-log text, tolerating an absent file (no daemon has ever run) as empty — the
/// same NotFound→empty read the `stats` verb uses, so the readout works pre-`run`.
fn read_event_log() -> Result<String> {
    match std::fs::read_to_string(crate::observability::log_path()?) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(Error::Io(err)),
    }
}

/// The daemon's raw usage samples (`usage-samples.jsonl`, issue #155) — the second durable file
/// this offline reader folds, for the #595 landing-point SLI. An absent store reads as empty (the
/// same NotFound→empty tolerance [`read_event_log`] and [`crate::usage_store::read_samples`] use),
/// so the landing SLI renders "no episodes" before the store exists rather than failing the verb.
fn read_usage_samples() -> Result<Vec<Sample>> {
    crate::usage_store::read_samples(&crate::paths::usage_samples()?)
}

/// A `reason=session` swap-out anchor for the landing-point reconstruction (issue #595): the
/// instant a parked account's post-swap window opens, plus the join key and the decision reading.
#[derive(Debug, Clone, PartialEq)]
struct SwapOut {
    /// The swap instant (epoch seconds) — the window origin; landing samples are `> ts`.
    ts: i64,
    /// The OUTGOING account's roster label (`from=`) — the join key into `usage-samples.jsonl`'s
    /// `acct`. Used INTERNALLY only; never rendered (the #15 roster-wide-numbers invariant).
    acct: String,
    /// The decision-point `session_pct` logged at the swap — separates a gap-crossing (already
    /// ≥ ceiling here) from a post-swap tail (fired below, landed at/over).
    decision_pct: u8,
}

/// A re-activation edge for the landing filter (issue #595): the instant `acct` becomes the ACTIVE
/// account again, closing that account's parked landing window (`active_at != acct`). Four durable
/// events revive an account: any `event=swap` (whose `to=` names it) and an `event=emergency_swap`
/// (the dead-active escape, also `to=`), plus an `event=restash` (out-of-band `claude /login`
/// reconciled onto a roster account) and an `event=canonical_recovered` (the scrub-adopt recovery),
/// both naming it in `account=`. Collected across all four — re-activation is not swap-kind-specific.
#[derive(Debug, Clone, PartialEq)]
struct Reactivation {
    /// The instant `acct` is re-activated (epoch seconds).
    ts: i64,
    /// The re-activated account's roster label (`to=` on a swap/emergency_swap, `account=` on a
    /// restash) — matched against a [`SwapOut::acct`].
    acct: String,
}

/// One reconciled blind-arm projection (issue #636): what the REPORT-ONLY blind velocity-projection
/// arm would have forecast for this `blind_window`, beside what the account actually arrived at.
///
/// Recomputed OFFLINE from the line's own tokens — the house log-the-ingredients / derive-the-views
/// idiom the #634 `BlindVelocity` doc names as this readout's contract — rather than read from a
/// stored projection: `projected = anchor + rate × inflation × duration_secs`, with `anchor` the
/// #632-corrected base `session_pct.max(session_high_water_pct)` (the frozen high-water mark issue
/// #670 stamps beside the raw anchor precisely when it was stale-low; absent the token the raw
/// `session_pct` stands), `rate` the full-precision (6-dp) pre-blind EMA in %/s and `inflation` the
/// factor STAMPED on the line, never today's [`crate::daemon`] constant (an old window read through
/// a new factor would silently mis-report). The anchor term carries those fields' `u8` rounding, so
/// a recomputed projection inherits up to ±0.5 pp of anchor error; over any window long enough to
/// arm the report the rate term dominates it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BlindProjection {
    /// The recomputed projection, as a PERCENT. Deliberately UNCLAMPED (a steep retained rate over a
    /// multi-minute blind window routinely projects past 100), so the "how far over did this
    /// project?" signal the error distribution exists to measure survives.
    projected_pct: f64,
    /// The durable actual: `session_at_recovery`, the FRESH reading at the recovery poll that closed
    /// the window. `0` is the session-window-RESET sentinel — the 5 h window rolled mid-blindness, so
    /// the account never "arrived" anywhere — and is excluded from the error distribution downstream.
    arrived_pct: u8,
}

/// The raw SLI ingredients pulled out of the event log, before aggregation.
#[derive(Debug, Default, PartialEq)]
struct Inputs {
    /// `session_pct` of every `reason=session` swap (the swap-out overshoot distribution).
    /// weekly (low incidental session_pct, out of scope), `manual`/`forced` (`session_pct=0`),
    /// and `emergency_swap` (no field) are excluded so they cannot poison the low tail.
    swap_out_pcts: Vec<f64>,
    /// Σ `blind_window.duration_secs` over windows with `near_limit=true`.
    time_blind_near_limit_secs: u64,
    /// `(anchor session_pct, session_at_recovery)` for each `near_limit=true` blind window —
    /// the false-preempt proxy input.
    near_limit_reconciliations: Vec<(u8, u8)>,
    /// `usage_backoff class=rate_limited` count (HTTP 429 on a usage poll).
    rate_limited: u32,
    /// `usage_backoff class=transient` count (5xx / network).
    transient: u32,
    /// `usage_backoff_cleared` count (back-off episodes that ended).
    cleared: u32,
    /// `event=swap reason=blind_preempt` count — the #452 bounded-blindness preemptive swaps
    /// (ADR-0017) actually observed; the REAL false-preempt numerator, superseding the proxy.
    preemptive_swaps: u32,
    /// `session_pct` of every `reason=velocity_preempt` swap (issue #539, ADR-0017) — the projective
    /// swap-out session_pct distribution on COVERED swaps (the OBSERVED pct each account had climbed to
    /// when its projective swap fired, NOT projection error; issue #635), kept SEPARATE from
    /// `swap_out_pcts` (the reactive `reason=session` residual). Its P50/P100 are the #539 acceptance
    /// (`<= 94` / `<= 98`).
    projective_swap_out_pcts: Vec<f64>,
    /// `event=swap reason=velocity_preempt` count — the #539 velocity-projection preemptive swaps
    /// actually observed; the false-projection SLI's real numerator (counted by the `reason` field,
    /// so a malformed-`session_pct` line still counts even if it is dropped from the distribution).
    velocity_preempt_swaps: u32,
    /// `reason=session` swap-out anchors for the landing-point SLI (issue #595): the (ts, outgoing
    /// account, decision reading) triples the reconstruction windows forward from. A superset of the
    /// WHO/WHEN that `swap_out_pcts` discards — a swap line missing a parseable `ts=`/`from=` still
    /// feeds `swap_out_pcts` but cannot be an anchor (there is no window to open), so it is dropped
    /// here (the tolerant-drop precedent).
    session_swaps: Vec<SwapOut>,
    /// Every re-activation edge (issue #595): the (ts, re-activated account) of any `event=swap`,
    /// `event=emergency_swap`, `event=restash`, or `event=canonical_recovered`, so the landing window
    /// of a previously-parked account can be closed at the instant it becomes active again
    /// (`active_at != acct`). All four revival paths count — re-activation is not swap-reason- or
    /// swap-kind-specific.
    reactivations: Vec<Reactivation>,
    /// Every observed POSITIVE `session_pct_per_min` from an `event=usage_velocity` line (issue
    /// #608) — the live session-climb distribution the assumed peak constant
    /// (`swap::V_PEAK_SESSION_PCT_PER_MIN`) is measured against. Roster-wide (no per-account split,
    /// like every other SLI here); negatives (window resets) and malformed values are dropped at
    /// parse. Keeps the constant honest: when the real peak outruns it, the `v_peak` coupling bound
    /// is silently too loose, exactly as `TAIL_MARGIN` is kept honest by the #595 landing SLI.
    session_velocities: Vec<f64>,
    /// Every `blind_window` that carried #634's velocity ingredients, recomputed into a
    /// (projection, actual) pair — the blind-arm projection-error input (issue #636).
    ///
    /// Deliberately NOT restricted to `near_limit=true`, unlike the two SLIs above: the climbing
    /// population this SLI exists to score is predominantly `near_limit=FALSE` (a stale anchor
    /// sitting well UNDER the risk band, climbing unseen, is exactly the 2026-07-17 episode issue
    /// #584 filed the arm for), so the near-limit filter would keep only the degenerate
    /// already-at-the-ceiling windows and measure nothing.
    blind_projections: Vec<BlindProjection>,
    /// Every `blind_window` line in view, classified (issue #636) — the DENOMINATOR context that
    /// keeps the projection-error percentiles from reading as the whole blind story.
    blind_window_census: BlindWindowCensus,
}

/// The complete classification of the `blind_window` lines in view (issue #636).
///
/// Every line lands in EXACTLY ONE bucket — `total == projectable + below_arm_gate +
/// without_velocity + malformed` — so no line can vanish between parse and render. That total
/// invariant is the point: this readout's whole thesis is that a percentile without its denominator
/// is a survivorship lie, and a silently-dropped line is exactly a missing denominator.
///
/// The three non-projectable buckets are ordered to MIRROR
/// [`crate::daemon`]'s `blind_velocity_projected_armed` gate order — duration first, then the
/// sustained-EMA check — so "outside the arm's domain" here means the same thing it means there.
#[derive(Debug, Default, PartialEq)]
struct BlindWindowCensus {
    /// Every `blind_window` line in view, whatever its disposition.
    total: usize,
    /// Lines shorter than the arm's first gate ([`crate::daemon::BLIND_GATE_SECS`]).
    ///
    /// The arm returns `false` on these BEFORE computing any projection, so it never forecast here
    /// and there is nothing to grade — yet the daemon still stamps the #634 ingredients on them
    /// (`blind_velocity_ingredients` gates only on the SUSTAINED-EMA condition, not on duration).
    /// Scoring them would swamp the signal: on the production log four fifths of `blind_window`
    /// lines are under the gate, and their `rate × inflation × duration_secs` term is small enough
    /// that their error mostly measures ANCHOR STALENESS, not the inflation factor this SLI exists
    /// to tune. Excluded and counted, never mixed in.
    ///
    /// **Disclosed bound.** `T` is applied at TODAY's value: unlike `inflation` / `ceiling` (which
    /// issue #634 STAMPS per line precisely so an old record is never re-read through a new
    /// constant), the gate is not on the line, so a `T` that moves would silently re-partition old
    /// windows. Stamping it is the #634-style follow-up; until then this is a documented,
    /// disclosed limitation rather than a hidden one — and the shared `pub(crate)` constant at
    /// least keeps the offline reader and the runtime arm on ONE value.
    below_arm_gate: usize,
    /// Lines at/over the gate but carrying NO `rate=` ingredient. Absent tokens mean "no SUSTAINED
    /// retained EMA — this arm could not have armed here", never "unknown", so there is no
    /// projection to score. The arm's SECOND gate.
    without_velocity: usize,
    /// Lines this reader could not classify: a missing or unparseable `session_pct` /
    /// `session_at_recovery` / `duration_secs`, an unparseable or non-finite `rate=` / `inflation=`,
    /// a PRESENT-but-unparseable `session_high_water_pct=` (issue #670), or a projection that
    /// overflowed to non-finite. A CORRUPT record, distinct from a well-formed window the arm simply
    /// could not arm on — folding the two together would report corruption as coverage. The tolerant-drop precedent every sibling arm here uses, made VISIBLE.
    malformed: usize,
}

/// Record a re-activation edge (issue #595): the account named by `{acct_field}=` on this log line
/// becomes the ACTIVE account at `ts=`, closing that account's parked landing window
/// (`active_at != acct`). Four durable events revive a roster account — the complete set found by
/// tracing every daemon active-account setter (each caller of `record_swap`, plus the reconcile
/// re-resolve): `event=swap` (any reason) and `event=emergency_swap` name the revived account in
/// `to=`; `event=restash` (an out-of-band `claude /login` the daemon reconciles onto a roster
/// account) and `event=canonical_recovered` (the scrub-adopt recovery re-adopting one, its session
/// gate bypassed so a just-parked account is eligible) name it in `account=`. Passing the field name
/// keeps one edge-recorder over all four. (A re-activation with NO durable event — a cross-restart
/// out-of-band re-login silently adopted at first-tick startup — is invisible to this reader-side
/// reconstruction; an accepted bound of the approach, like the raw-sample retention window.) A line
/// missing a parseable `ts=` or `{acct_field}=` is skipped (unplaceable — the tolerant-drop
/// precedent).
fn record_reactivation_edge(inputs: &mut Inputs, fields: &BTreeMap<&str, &str>, acct_field: &str) {
    if let (Some(ts), Some(acct)) = (
        fields.get("ts").copied().and_then(epoch_from_rfc3339),
        fields.get(acct_field).copied(),
    ) {
        inputs.reactivations.push(Reactivation {
            ts,
            acct: acct.to_owned(),
        });
    }
}

/// Fold one `blind_window` line into the blind-arm projection-error input (issue #636).
///
/// Recomputes the REPORT-ONLY arm's forecast from the line's OWN tokens — `anchor + rate ×
/// inflation × duration_secs`, the [`crate::daemon`] `blind_velocity_projected_armed` formula — and
/// pairs it with the durable `session_at_recovery` beside it. Every term is stamped on the line
/// (issue #634), so no daemon constant is imported and an old window is never read through a
/// today-value. The `anchor` term is the #632-corrected base: since issue #632 the live arm projects
/// off the #619 plausibility-CORRECTED base (`gate_session`), and issue #670 carries the frozen
/// high-water mark (`session_high_water_pct`, stamped only when the anchor was stale-low) so this
/// recompute applies the SAME [`crate::swap::plausible_anchor_session`] correction —
/// `session_pct.max(session_high_water_pct)` — and reproduces the live forecast exactly rather than
/// under-computing off the stale-low base. Absent the mark token no correction applies and the raw
/// `session_pct` stands; the anchor term still carries those fields' `u8` rounding (≤ ±0.5 pp),
/// dominated by the rate term over any armed window.
///
/// Classifies EVERY line into exactly one [`BlindWindowCensus`] bucket, applying the arm's OWN
/// gates in the arm's OWN order — duration first, sustained EMA second — so "outside the arm's
/// domain" means here exactly what it means there. Collapsing any two of these buckets would
/// fabricate the survivorship story the SLI exists to guard:
///
/// 1. **Core fields unreadable** (`session_pct` / `session_at_recovery` / `duration_secs` missing or
///    unparseable) ⇒ `malformed`. A corrupt record is not evidence of anything.
/// 2. **`duration_secs <= BLIND_GATE_SECS`** ⇒ `below_arm_gate`. The arm returns `false` here BEFORE
///    computing a projection, so there is no forecast to grade — even though the daemon does stamp
///    the ingredients on such a line.
/// 3. **No `rate=` token** ⇒ `without_velocity`. Absent tokens mean "no SUSTAINED retained EMA", the
///    arm's second gate — never "unknown".
/// 4. **`rate=` / `inflation=` unreadable, a PRESENT `session_high_water_pct=` unreadable (issue
///    #670), or the projection overflows to non-finite** ⇒ `malformed`. Publishing a non-finite
///    percentile would make the human text (`+inf`) and the `--json` wire (`null`, which this
///    schema defines as "empty population") disagree about the same episode; a garbage mark
///    silently reverted to the stale base would misreport the arm the same way.
/// 5. **Complete** ⇒ a [`BlindProjection`]. The `session_at_recovery = 0` window-reset sentinel is
///    carried through and excluded later, at aggregation, so the exclusion is COUNTED rather than
///    silently swallowed at parse.
///
/// Deliberately called BEFORE the `near_limit=true` gate — see [`Inputs::blind_projections`].
fn record_blind_projection(inputs: &mut Inputs, fields: &BTreeMap<&str, &str>) {
    let census = &mut inputs.blind_window_census;
    census.total = census.total.saturating_add(1);
    let (Some(anchor), Some(arrived), Some(blind_secs)) = (
        fields.get("session_pct").and_then(|v| v.parse::<u8>().ok()),
        fields
            .get("session_at_recovery")
            .and_then(|v| v.parse::<u8>().ok()),
        fields
            .get("duration_secs")
            .and_then(|v| v.parse::<u64>().ok()),
    ) else {
        census.malformed = census.malformed.saturating_add(1);
        return;
    };
    if blind_secs <= crate::daemon::BLIND_GATE_SECS {
        census.below_arm_gate = census.below_arm_gate.saturating_add(1);
        return;
    }
    if !fields.contains_key("rate") {
        census.without_velocity = census.without_velocity.saturating_add(1);
        return;
    }
    let (Some(rate), Some(inflation)) = (
        fields
            .get("rate")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite()),
        fields
            .get("inflation")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite()),
    ) else {
        census.malformed = census.malformed.saturating_add(1);
        return;
    };
    // Issue #670: the live #584 arm projects off the #619/#632 plausibility-CORRECTED base
    // (`gate_session`), not the raw anchor. When the window carries the frozen high-water mark
    // (`session_high_water_pct`, stamped ONLY when the anchor was stale-low), apply the SAME
    // `swap::plausible_anchor_session` correction — the greater of the raw anchor and the mark — so
    // this recompute reproduces the corrected forecast rather than under-computing off the stale base
    // (the residual issue #670 closes). Absent the token no correction applies and the raw anchor
    // stands. A PRESENT-but-unparseable mark is corruption, dropped to `malformed` exactly like an
    // unreadable `rate` / `inflation` — the mark is now part of the projection-reconstruction
    // contract, so a garbage mark cannot silently revert to the stale base and misreport the arm.
    let corrected_anchor = match fields
        .get("session_high_water_pct")
        .map(|raw| raw.parse::<u8>())
    {
        None => anchor,
        Some(Ok(mark)) => anchor.max(mark),
        Some(Err(_)) => {
            census.malformed = census.malformed.saturating_add(1);
            return;
        }
    };
    // Finite INPUTS do not imply a finite product: `rate × inflation × blind_secs` can overflow to
    // `inf` (or, with a zero duration, `inf × 0 = NaN`) on a corrupted line. Checked on the RESULT,
    // so no non-finite value can reach the percentile and split the two renderers apart.
    let projected_pct = f64::from(corrected_anchor) + rate * inflation * blind_secs as f64;
    if !projected_pct.is_finite() {
        census.malformed = census.malformed.saturating_add(1);
        return;
    }
    inputs.blind_projections.push(BlindProjection {
        projected_pct,
        arrived_pct: arrived,
    });
}

/// Parse the SLI ingredients out of the structured event-log `text`.
///
/// Tolerant, forward-only, self-contained: it reads the flat `key=val` grammar
/// ([`crate::observability`]) line by line and folds the relevant event families into
/// [`Inputs`], skipping blank lines, other event kinds, and any line missing a field it needs
/// or carrying an unparseable value (the same tolerant-drop the `stats` swap parser uses).
///
/// `cutoff` bounds the window (issue #494): `None` reads every line (the whole-log default,
/// timestamps ignored exactly as before). `Some(epoch)` keeps only lines whose `ts=` parses to
/// an instant `>=` the cutoff (at/after — the boundary itself is IN the window); a line whose
/// `ts=` is missing or unparseable is dropped from a windowed view, since it cannot be placed
/// in time (the tolerant-drop precedent, mirroring `crate::usage_stats`' swap parser). The
/// `ts=` parse reuses [`epoch_from_rfc3339`] — the crate's one canonical RFC-3339 reader — so
/// no second calendar routine is introduced.
fn parse_events(text: &str, cutoff: Option<i64>) -> Inputs {
    let mut inputs = Inputs::default();
    for line in text.lines() {
        // Field map from the whitespace-separated `key=val` tokens. Handles/values are
        // whitespace-free by the log's grammar, so tokenizing on spaces is exact.
        let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
        for token in line.split_whitespace() {
            if let Some((key, val)) = token.split_once('=') {
                fields.insert(key, val);
            }
        }

        // Window gate (only when `--since` is active): drop lines before the cutoff, and drop
        // any line we cannot timestamp (unplaceable ⇒ not provably in-window). Runs before the
        // event match so a dropped line feeds no SLI.
        if let Some(cutoff) = cutoff {
            let in_window = fields
                .get("ts")
                .copied()
                .and_then(epoch_from_rfc3339)
                .is_some_and(|ts| ts >= cutoff);
            if !in_window {
                continue;
            }
        }

        match fields.get("event").copied() {
            Some("swap") => {
                // Re-activation edge (issue #595): ANY swap re-activates its INCOMING account, so
                // record its `to=` BEFORE the reason-specific `continue`s below — a previously-parked
                // account's landing window closes the instant it becomes active again
                // (`active_at != acct`), whatever the reason of the swap that revives it.
                record_reactivation_edge(&mut inputs, &fields, "to");
                // #452 preemptive swaps (reason=blind_preempt, ADR-0017): count each observed one
                // for the false-preempt SLI's REAL numerator, then skip the session-overshoot
                // accounting below — a preemptive swap fires on a STALE anchor, not a fresh reading,
                // so its session_pct is not a swap-out overshoot sample.
                if fields.get("reason").copied() == Some("blind_preempt") {
                    inputs.preemptive_swaps = inputs.preemptive_swaps.saturating_add(1);
                    continue;
                }
                // #539 velocity-projection swaps (reason=velocity_preempt, ADR-0017): count each for
                // the false-projection SLI, and fold its FRESH `session_pct` into the projective
                // swap-out session_pct distribution (the #539 covered-swap acceptance) — SEPARATE from
                // the reactive `reason=session` distribution below (which is now the poll-gap residual
                // #540 owns). A projective swap fires on a live reading, so — unlike blind_preempt —
                // its session_pct IS a real swap-out sample.
                if fields.get("reason").copied() == Some("velocity_preempt") {
                    inputs.velocity_preempt_swaps = inputs.velocity_preempt_swaps.saturating_add(1);
                    if let Some(pct) = fields.get("session_pct").and_then(|v| v.parse::<u8>().ok())
                    {
                        inputs.projective_swap_out_pcts.push(f64::from(pct));
                    }
                    continue;
                }
                // SESSION-triggered swaps only. A weekly swap fires while session is BELOW its
                // trigger, so its session_pct is a low, incidental value — not a session
                // overshoot — and weekly cadence is out of scope for this session-limit-latency
                // increment (prd-swap-latency.md §6). manual/forced (session_pct=0) and
                // emergency_swap (no session_pct field) are likewise not session overshoots.
                if fields.get("reason").copied() != Some("session") {
                    continue;
                }
                if let Some(pct) = fields.get("session_pct").and_then(|v| v.parse::<u8>().ok()) {
                    inputs.swap_out_pcts.push(f64::from(pct));
                    // Landing anchor (issue #595): the reconstruction needs the WHO (`from=`) and
                    // WHEN (`ts=`) to window this parked account's post-swap samples. A line missing
                    // either still fed the pct above, but cannot open a window — not an anchor.
                    if let (Some(ts), Some(from)) = (
                        fields.get("ts").copied().and_then(epoch_from_rfc3339),
                        fields.get("from").copied(),
                    ) {
                        inputs.session_swaps.push(SwapOut {
                            ts,
                            acct: from.to_owned(),
                            decision_pct: pct,
                        });
                    }
                }
            }
            Some("blind_window") => {
                // The blind-arm projection error (issue #636) is folded FIRST, deliberately ahead of
                // the near-limit gate below: the climbing population it scores is predominantly
                // `near_limit=false` (an anchor under the risk band, burning unseen — the #584
                // episode), so gating it would keep only the degenerate already-at-the-ceiling
                // windows. The two SLIs below keep their near-limit scope unchanged.
                record_blind_projection(&mut inputs, &fields);
                // Only near-limit windows feed either the time-blind sum or the proxy.
                if fields.get("near_limit").copied() != Some("true") {
                    continue;
                }
                if let Some(secs) = fields
                    .get("duration_secs")
                    .and_then(|v| v.parse::<u64>().ok())
                {
                    inputs.time_blind_near_limit_secs =
                        inputs.time_blind_near_limit_secs.saturating_add(secs);
                }
                if let (Some(anchor), Some(recovery)) = (
                    fields.get("session_pct").and_then(|v| v.parse::<u8>().ok()),
                    fields
                        .get("session_at_recovery")
                        .and_then(|v| v.parse::<u8>().ok()),
                ) {
                    inputs.near_limit_reconciliations.push((anchor, recovery));
                }
            }
            Some("usage_backoff") => match fields.get("class").copied() {
                Some("rate_limited") => inputs.rate_limited = inputs.rate_limited.saturating_add(1),
                Some("transient") => inputs.transient = inputs.transient.saturating_add(1),
                _ => {}
            },
            Some("usage_backoff_cleared") => inputs.cleared = inputs.cleared.saturating_add(1),
            // An emergency swap (issue #405 dead-active escape) is a DISTINCT event token, but it too
            // moves the active account onto `to=` — so it is a re-activation edge for the #595 landing
            // filter, exactly like a normal swap-in. It is NOT a session overshoot, so — unlike the
            // `swap` arm — it contributes no landing ANCHOR, only the re-activation edge.
            Some("emergency_swap") => record_reactivation_edge(&mut inputs, &fields, "to"),
            // A restash (issue #595) is the out-of-band `claude /login` path: the daemon's canonical
            // watch detects a foreign credential, reconciles it onto its roster account, and
            // re-resolves THAT account active (`reconcile_canonical_change`). So it too revives a
            // possibly-parked account — a re-activation edge for the landing filter — but names it in
            // `account=` (not `to=`), and like emergency_swap it is no session overshoot, so it
            // contributes only the edge, no anchor.
            Some("restash") => record_reactivation_edge(&mut inputs, &fields, "account"),
            // A canonical recovery (issue #595) is the fourth revival door: when the shared canonical
            // credential is scrubbed and the daemon re-adopts a roster account to keep the fleet live
            // (the scrub-adopt path, its session gate bypassed so a just-parked near-limit account is
            // eligible), it calls the same `record_swap` — re-activating that account — and emits
            // event=canonical_recovered account={label}. So it too is a re-activation edge, keyed off
            // `account=` like restash, and contributes only the edge (it is no session overshoot).
            Some("canonical_recovered") => {
                record_reactivation_edge(&mut inputs, &fields, "account")
            }
            // The observed session-velocity distribution (issue #608): every `usage_velocity` line
            // carries the account's climb rate between its last two readings, already normalized to
            // %/min by the emitter (issue #449). NEGATIVE rates are dropped: a negative delta is a
            // session-window RESET (usage fell because the 5 h window rolled), not a climb — folding
            // it in would drag the distribution down and understate the peak this SLI exists to
            // catch. Zero is likewise not a climb, and the emitter is already silent on a flat
            // account, so the filter is `> 0`. A malformed value is dropped (the tolerant-drop
            // precedent the sibling arms use), never defaulted to 0.
            Some("usage_velocity") => {
                if let Some(rate) = fields
                    .get("session_pct_per_min")
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|v| v.is_finite() && *v > 0.0)
                {
                    inputs.session_velocities.push(rate);
                }
            }
            _ => {}
        }
    }
    inputs
}

/// The swap-out overshoot distribution. Percentiles are `None` when no swap was observed —
/// cardinality-zero is distinguished from a real `0` so the readout never asserts a target
/// PASS on an empty subject.
#[derive(Debug, PartialEq)]
struct SwapOvershoot {
    n: usize,
    p50: Option<u8>,
    p95: Option<u8>,
    p100: Option<u8>,
}

impl SwapOvershoot {
    /// Whether P50 meets its `<= SLO_SWAP_P50_MAX` target (`None` with no data).
    fn p50_met(&self) -> Option<bool> {
        self.p50.map(|v| v <= SLO_SWAP_P50_MAX)
    }

    /// Whether P100 meets its strict `< SLO_SWAP_P100_MAX` target (`None` with no data).
    fn p100_met(&self) -> Option<bool> {
        self.p100.map(|v| v < SLO_SWAP_P100_MAX)
    }
}

/// The projective swap-out session_pct distribution (issue #539, ADR-0017): the `session_pct`
/// percentiles over `reason=velocity_preempt` swaps — how high each account had actually climbed when
/// its projective swap fired (the OBSERVED pct, NOT projection error; issue #635). The COVERED-swap
/// acceptance for the velocity-projection trigger, distinct from the reactive [`SwapOvershoot`] (the
/// poll-gap residual #540 owns). `None` percentiles when no projective swap was observed, so the
/// readout never asserts a target PASS on an empty subject (the same cardinality-zero discipline as
/// [`SwapOvershoot`]).
#[derive(Debug, PartialEq)]
struct ProjectiveSwapOutPct {
    n: usize,
    p50: Option<u8>,
    p95: Option<u8>,
    p100: Option<u8>,
}

impl ProjectiveSwapOutPct {
    /// Whether P50 meets its `<= SLO_PROJECTED_SWAP_P50_MAX` target (`None` with no data).
    fn p50_met(&self) -> Option<bool> {
        self.p50.map(|v| v <= SLO_PROJECTED_SWAP_P50_MAX)
    }

    /// Whether P100 meets its `<= SLO_PROJECTED_SWAP_P100_MAX` target (`None` with no data). Note the
    /// INCLUSIVE comparator (`<=`) — the #538 acceptance is P100 = 98, unlike the strict full-trace
    /// P100 `< 99` of [`SwapOvershoot::p100_met`].
    fn p100_met(&self) -> Option<bool> {
        self.p100.map(|v| v <= SLO_PROJECTED_SWAP_P100_MAX)
    }
}

/// The landing-point swap-out overshoot SLI (issue #595): where each `reason=session` swap-out
/// ACTUALLY landed — the peak `session_pct` its OUTGOING account reached within
/// [`LANDING_WINDOW_SECS`] after being parked, EXCLUDING any window minutes past a re-activation
/// (`active_at != acct`). Reconstructed by joining the event log's swap anchors with the daemon's
/// `usage-samples.jsonl` per-account readings (spike #596's two-source recipe), so it surfaces the
/// post-swap committed tail SLI 1 (the swap DECISION point) is blind to. Percentiles are `None` when
/// no swap had a post-swap sample — cardinality-zero is not a passing 0 (the [`SwapOvershoot`]
/// discipline). `p90` is the tail-calibration input the trigger-redesign sibling (issue #597) reads.
#[derive(Debug, PartialEq)]
struct Landing {
    /// `reason=session` swap anchors in view (already `--since`-windowed) — the coverage denominator.
    swaps_total: usize,
    /// Anchors with ≥1 post-swap sample of the parked account — the subject the percentiles summarize.
    n_measured: usize,
    /// Anchors with NO post-swap sample in the window: a sample-coverage gap, reported honestly
    /// rather than fabricated as a `0` landing (a swap the store cannot reconstruct is UNMEASURED,
    /// not on-target).
    n_unmeasured: usize,
    p50: Option<u8>,
    /// The 90th-percentile landing — the tail the #597 trigger redesign calibrates against.
    p90: Option<u8>,
    p100: Option<u8>,
    /// Measured episodes whose DECISION reading was already `>= SLO_SWAP_P100_MAX`: the swap fired
    /// late, so the overshoot is already visible in SLI 1 (gap-crossing — issue #595 breach class 2).
    gap_crossing: usize,
    /// Measured episodes that fired BELOW the ceiling but LANDED at/over it: the post-swap committed
    /// tail — the invisible ~46% this SLI exists to expose (issue #595 breach class 1).
    post_swap_tail: usize,
}

impl Landing {
    /// Whether the worst landing meets the strict `< SLO_SWAP_P100_MAX` ceiling (`None` with no
    /// measured episode). The SAME ceiling [`SwapOvershoot::p100_met`] checks on the decision
    /// reading — the issue's thesis is that the SLO belongs on THIS event, so the readout flags it
    /// here too.
    fn p100_met(&self) -> Option<bool> {
        self.p100.map(|v| v < SLO_SWAP_P100_MAX)
    }
}

/// The observed session-velocity distribution SLI (issue #608): the live `session_pct_per_min`
/// percentiles, measured against the assumed peak constant [`crate::swap::V_PEAK_SESSION_PCT_PER_MIN`]
/// that the `v_peak` coupling bound ([`crate::swap::peak_runway_reserve_bound`]) is calibrated on.
/// Its job is to keep that constant HONEST: the bound assumes no account climbs faster than `v_peak`,
/// so if the real peak (`p100`) outruns it, the config-load coupling is silently too loose and the
/// constant needs re-calibrating — the same "measure, don't trust the constant" discipline the #595
/// landing SLI provides for `TAIL_MARGIN`.
///
/// Percentiles are `None` when no positive velocity sample was observed — cardinality-zero is not a
/// passing distribution (the [`SwapOvershoot`] discipline), so the readout never asserts the constant
/// is honest on an empty subject. Rates are `f64` %/min (rounded for display only), not the `u8`
/// percents the swap-out SLIs carry.
#[derive(Debug, PartialEq)]
struct ObservedPeak {
    /// Count of positive `session_pct_per_min` samples in view.
    n: usize,
    p50: Option<f64>,
    p90: Option<f64>,
    /// The observed MAXIMUM climb rate — the value compared against `v_peak`.
    p100: Option<f64>,
}

impl ObservedPeak {
    /// Whether the assumed peak still bounds the observed one: `Some(true)` when the measured max
    /// (`p100`) is at or below [`crate::swap::V_PEAK_SESSION_PCT_PER_MIN`], `Some(false)` when the
    /// real peak has OUTRUN the constant (the coupling bound is too loose — re-calibrate), `None`
    /// with no data. A tiny epsilon absorbs the display-rounding + `%/min → frac/s → %/min` round
    /// trips so a sample recorded at exactly `v_peak` is not flagged by float dust.
    fn v_peak_honest(&self) -> Option<bool> {
        self.p100
            .map(|v| v <= crate::swap::V_PEAK_SESSION_PCT_PER_MIN + 1e-9)
    }
}

/// Reconstruct the landing-point SLI (issue #595) by joining the parsed swap anchors with the raw
/// usage samples. Pure and total: no clock, no I/O — the samples are read once in [`run`] and passed
/// in, so the whole aggregation stays a function of the two file contents (and the `--since` cutoff,
/// already applied to `inputs`).
fn compute_landing(inputs: &Inputs, samples: &[Sample]) -> Landing {
    // Index the samples by roster label once, so each anchor scans only ITS account's readings
    // rather than re-sweeping the whole store. The label is the join key (swap `from=` ↔
    // `Sample.acct`) and stays INTERNAL — no label reaches the rendered output. Group order is
    // irrelevant: the peak below is an order-independent filter + `reduce(f64::max)` over the window.
    let mut by_acct: BTreeMap<&str, Vec<&Sample>> = BTreeMap::new();
    for s in samples {
        by_acct.entry(s.acct.as_str()).or_default().push(s);
    }

    let mut landing_pcts: Vec<f64> = Vec::new();
    let mut n_unmeasured = 0usize;
    let mut gap_crossing = 0usize;
    let mut post_swap_tail = 0usize;

    for swap in &inputs.session_swaps {
        let window_end = swap.ts.saturating_add(LANDING_WINDOW_SECS);
        // The parked window closes at the earliest re-activation of THIS account after the swap, when
        // one falls inside the window — samples at/after it read the now-ACTIVE account, not the
        // parked tail (`active_at != acct`). No re-activation ⇒ the full bounded window.
        let effective_end = inputs
            .reactivations
            .iter()
            .filter(|si| si.acct == swap.acct && si.ts > swap.ts)
            .map(|si| si.ts.saturating_sub(1)) // strictly before the re-activation instant
            .min()
            .map_or(window_end, |before_reactivation| {
                window_end.min(before_reactivation)
            });
        // Peak absolute session over the parked window (readings strictly after the swap, through the
        // effective end). The `is_finite` guard drops a NaN/inf reading, which would otherwise clamp to
        // the u8 cap (255) below and fabricate a max-value breach (and poison the issue #597 tail
        // calibration). `None` ⇒ no reading of the parked account in view — an unmeasured anchor.
        let peak = by_acct
            .get(swap.acct.as_str())
            .into_iter()
            .flatten()
            .filter(|s| s.ts > swap.ts && s.ts <= effective_end && s.session.is_finite())
            .map(|s| s.session)
            .reduce(f64::max);
        match peak {
            Some(peak) => {
                // Fraction → percent, matching the swap event's u8 `session_pct` so the decision and
                // landing readings are directly comparable. Readings can exceed 1.0 (the store doc), so
                // clamp into u8 without wrapping — an over-100 landing keeps its true rounded value up to
                // the u8 cap (255), clamping only an implausibly huge reading rather than overflowing.
                let landing_pct = (peak * 100.0).round().clamp(0.0, u8::MAX as f64) as u8;
                landing_pcts.push(f64::from(landing_pct));
                if swap.decision_pct >= SLO_SWAP_P100_MAX {
                    gap_crossing += 1;
                } else if landing_pct >= SLO_SWAP_P100_MAX {
                    post_swap_tail += 1;
                }
            }
            None => n_unmeasured += 1,
        }
    }

    let n = landing_pcts.len();
    let pct = |p: f64| -> Option<u8> {
        (n > 0).then(|| crate::percentile::percentile(&landing_pcts, p) as u8)
    };
    Landing {
        swaps_total: inputs.session_swaps.len(),
        n_measured: n,
        n_unmeasured,
        p50: pct(0.50),
        p90: pct(0.90),
        p100: pct(1.0),
        gap_crossing,
        post_swap_tail,
    }
}

/// The false-preempt SLI: the real (still-pending) rate plus the interim blind-window proxy.
#[derive(Debug, PartialEq)]
struct FalsePreempt {
    /// Real preemptive swaps observed (issue #452, ADR-0017): the `event=swap reason=blind_preempt`
    /// count — the false-preempt SLI's real numerator, superseding the blind-window proxy as the
    /// data accrues. Folded in from [`parse_events`].
    preemptive_swaps_observed: u32,
    /// Proxy denominator: near-limit blind windows (a hypothetical preemptive swap's chance).
    near_limit_windows: u32,
    /// Proxy numerator: near-limit windows whose fresh recovery reading had fallen more than
    /// [`PREEMPT_WASTED_MARGIN_PCT`] below the stale anchor — a would-be-wasted swap.
    would_be_wasted: u32,
}

/// The false-projection SLI (issue #539, ADR-0017): velocity-projection preemptive swaps that fired
/// on a projection the observed reading had not yet reached. Every `reason=velocity_preempt` swap is
/// one by construction (the projective path only fires when the reactive path HELD — observed below
/// the trigger), so the observed COUNT directly measures "swaps the projection fired [ahead of the
/// observed overshoot]". The true WASTED fraction (would the account actually have overshot?) needs a
/// post-swap reconciliation of the swapped-away account — not available from the swap event alone —
/// so `rate` stays `None`, exactly as [`FalsePreempt`]'s real rate is still pending. The #538 spike
/// bounds it: 0 truly-wasted swaps at H ≤ 150 s. The companion projective swap-out session_pct
/// distribution ([`ProjectiveSwapOutPct`]) shows these swaps land at P50 = 94 (barely ahead of the
/// trigger, so low-waste by construction), the primary evidence the projection is not over-firing.
#[derive(Debug, PartialEq)]
struct FalseProjection {
    /// Real velocity-projection preemptive swaps observed (`event=swap reason=velocity_preempt`) —
    /// the false-projection SLI's numerator. Folded in from [`parse_events`].
    velocity_preempt_swaps_observed: u32,
    /// The real false-projection rate (wasted ÷ observed). Always `None` today — the wasted count
    /// needs a post-swap reconciliation of the swapped-away account (out of scope for #539; the
    /// poll-coverage sibling #540 and the umbrella #363 own the full-trace picture). Mirrors
    /// [`FalsePreempt`]'s still-`None` real rate.
    rate: Option<f64>,
}

/// The blind-arm projection-error SLI (issue #636): `projected − session_at_recovery` percentiles
/// for the REPORT-ONLY blind velocity-projection arm ([`crate::daemon`]'s
/// `blind_velocity_projected_armed`, issues #584/#600), in percentage points.
///
/// POSITIVE = the arm over-projected (it would have cried DEGRADED further ahead of the real burn
/// than the account actually got); NEGATIVE = it under-projected (the account burned past where the
/// inflated forecast put it — the failure mode the arm exists to prevent). The distribution's centre
/// and spread are the tuning input for [`crate::daemon`]'s interim, ratification-pending
/// `BLIND_VELOCITY_RATE_INFLATION = 1.75`, which is the primary value here — the observed burns are
/// small (single-digit pp), so this is a calibration instrument, not a catastrophe detector.
///
/// **Survivorship guard (mandatory, issue #484).** The percentiles are NEVER published bare: every
/// renderer emits them paired with the counts below, and with the censoring disclosure. `blind_window`
/// fires only on the `None -> live` RECOVERY edge of the ACTIVE account, so the population here is
/// RECOVERED-ONLY by construction — measuring the EASY episodes. The two censored tails
/// ([`Self::n_swapped_away`] / [`Self::n_never_recovered`]) are structurally invisible to this event
/// and are reported as `None`, never as a fabricated `0`; the uncensored `blind_enter` / `blind_exit`
/// denominator that populates them is issue #591's, and this SLI consumes rather than duplicates it.
///
/// **Domain guard.** The scored population is the arm's OWN domain: windows past
/// [`crate::daemon::BLIND_GATE_SECS`], the gate the arm checks FIRST. The daemon stamps #634's
/// ingredients on shorter windows too, but the arm returns `false` on them before projecting
/// anything — and on the production log four fifths of `blind_window` lines are under the gate, so
/// scoring them would drag P50 (this readout's own stated tuning output) toward zero and read as
/// "1.75 is well calibrated" on episodes the arm never evaluated. They are excluded and COUNTED.
///
/// Percentiles are `None` on an empty reconcilable population — cardinality-zero is not a passing
/// `0` (the [`SwapOvershoot`] discipline), so a `1.75` tuning verdict is never asserted on no data.
#[derive(Debug, PartialEq)]
struct BlindProjectionError {
    /// Every `blind_window` line in view — the denominator the buckets below partition exactly:
    /// `n_blind_windows == n_projectable + n_below_arm_gate + n_without_velocity + n_malformed`.
    n_blind_windows: usize,
    /// Windows inside the arm's domain that carried #634's velocity ingredients — the projectable
    /// population. Partitioned exactly by `n_reconcilable + n_sentinel_excluded`.
    n_projectable: usize,
    /// Projectable windows with a real actual (`session_at_recovery > 0`) — the percentile subject.
    n_reconcilable: usize,
    /// Projectable windows dropped for the `session_at_recovery = 0` session-window-RESET sentinel:
    /// the 5 h window rolled mid-blindness, so the account never "arrived" anywhere and the
    /// difference would measure the reset, not the forecast. Same reset-drop discipline the #608
    /// `usage_velocity` arm applies to a negative rate — reported as a count, not swallowed.
    n_sentinel_excluded: usize,
    /// Windows shorter than the arm's first gate — it never projected here, so there is nothing to
    /// grade. See [`BlindWindowCensus::below_arm_gate`] for the disclosed today's-`T` bound.
    n_below_arm_gate: usize,
    /// In-domain windows with NO retained velocity — the arm's second gate could not pass, so there
    /// was no projection to score. Coverage context, not a zero error.
    n_without_velocity: usize,
    /// Windows this reader could not classify (corrupt fields, or a projection that overflowed).
    /// Reported rather than silently dropped: an undisclosed drop is a missing denominator, which is
    /// precisely the survivorship failure the rest of this block guards against.
    n_malformed: usize,
    /// Episodes the daemon SWAPPED AWAY from before they recovered. Always `None`: `blind_window` is
    /// active-scoped, so an episode the daemon swaps off is structurally unrecordable by it. Filled
    /// in when issue #591's censoring-aware denominator lands — `None` is the honest "unobservable",
    /// distinct from an observed `0` (the same still-pending shape as [`FalseProjection::rate`]).
    n_swapped_away: Option<usize>,
    /// Episodes that NEVER recovered. Always `None` for the mirrored reason: `blind_window` fires on
    /// the RECOVERY edge, so an account that goes dark and stays dark emits nothing at all — the
    /// episode is invisible precisely when it is worst. Issue #591 owns it.
    n_never_recovered: Option<usize>,
    /// Error percentiles in percentage points, rounded to 2 dp (see [`round_pp`]). Signed.
    p50: Option<f64>,
    p95: Option<f64>,
    p100: Option<f64>,
}

/// Round a percentage-point error to 2 dp for display and the wire.
///
/// Applied at AGGREGATION, not per-renderer, so the human text and the `--json` document cannot
/// report different numbers for the same episode. The trailing `+ 0.0` normalizes IEEE `-0.0` (an
/// error that rounds to zero from below) to `0.0`, so a spot-on projection renders `+0.00` rather
/// than the confusing `-0.00`.
fn round_pp(v: f64) -> f64 {
    (v * 100.0).round() / 100.0 + 0.0
}

/// 429-rate neutrality counts.
#[derive(Debug, PartialEq)]
struct RateLimit {
    rate_limited: u32,
    transient: u32,
    cleared: u32,
}

/// The aggregated readout — one pass folded into the four SLIs, plus the active window (if
/// any). With `window: None` this is the whole-log aggregate; with `Some` the four SLIs above
/// were computed over the windowed subset only, and `window` documents the bound.
#[derive(Debug, PartialEq)]
struct Report {
    /// The active `--since` window, or `None` for the whole-log aggregate. Carried through so
    /// the renderers document the bound; the SLIs are already windowed by [`parse_events`].
    window: Option<Window>,
    swap_overshoot: SwapOvershoot,
    /// The #539 velocity-projection covered-swap session_pct (`reason=velocity_preempt` percentiles).
    projective_swap_out_pct: ProjectiveSwapOutPct,
    /// The #595 landing-point overshoot — where `reason=session` swaps actually landed (post-swap
    /// peak of the parked account), reconstructed from the usage-sample store.
    landing: Landing,
    /// The #608 observed session-velocity distribution — the live peak climb rate vs the assumed
    /// `v_peak` constant the coupling bound is calibrated on.
    observed_peak: ObservedPeak,
    time_blind_near_limit_secs: u64,
    false_preempt: FalsePreempt,
    /// The #539 false-projection SLI (velocity-projection swaps observed; real rate pending).
    false_projection: FalseProjection,
    /// The #636 blind-arm projection error — `projected − session_at_recovery` percentiles, paired
    /// with the cardinality + censoring counts that keep them from reading as the whole blind story.
    blind_projection_error: BlindProjectionError,
    rate_limit: RateLimit,
}

/// Fold the parsed [`Inputs`] into a [`Report`], attaching the active `window` for display.
/// Pure and total: the windowing already happened in [`parse_events`] (the `inputs` are the
/// filtered subset); `window` is carried through untouched, only so the renderers can document
/// the bound. `samples` are the daemon's raw usage readings (`usage-samples.jsonl`), joined with
/// the swap anchors in [`compute_landing`] to reconstruct the #595 landing-point SLI.
fn aggregate(inputs: &Inputs, samples: &[Sample], window: Option<Window>) -> Report {
    let n = inputs.swap_out_pcts.len();
    // percentile() returns one of the input samples, each an integer-valued `f64::from(u8)`,
    // so `as u8` is exact (values are 0..=100). `None` when there is nothing to summarize.
    let pct = |p: f64| -> Option<u8> {
        (n > 0).then(|| crate::percentile::percentile(&inputs.swap_out_pcts, p) as u8)
    };
    let swap_overshoot = SwapOvershoot {
        n,
        p50: pct(0.50),
        p95: pct(0.95),
        p100: pct(1.0),
    };

    // The #539 projective swap-out session_pct — the same percentile discipline over the
    // `reason=velocity_preempt` distribution (its own cardinality gate, so a target is never PASSED
    // on zero projective swaps).
    let projective_n = inputs.projective_swap_out_pcts.len();
    let projective_pct = |p: f64| -> Option<u8> {
        (projective_n > 0)
            .then(|| crate::percentile::percentile(&inputs.projective_swap_out_pcts, p) as u8)
    };
    let projective_swap_out_pct = ProjectiveSwapOutPct {
        n: projective_n,
        p50: projective_pct(0.50),
        p95: projective_pct(0.95),
        p100: projective_pct(1.0),
    };

    let near_limit_windows = inputs.near_limit_reconciliations.len() as u32;
    let would_be_wasted = inputs
        .near_limit_reconciliations
        .iter()
        // Saturating: recovery >= anchor → 0, never "> margin", correctly "would-be necessary".
        .filter(|(anchor, recovery)| anchor.saturating_sub(*recovery) > PREEMPT_WASTED_MARGIN_PCT)
        .count() as u32;

    // The #608 observed session-velocity distribution — its own cardinality gate (percentiles
    // `None` on an empty subject), so `v_peak_honest` is never asserted on zero samples.
    let velocity_n = inputs.session_velocities.len();
    let velocity_pct = |p: f64| -> Option<f64> {
        (velocity_n > 0).then(|| crate::percentile::percentile(&inputs.session_velocities, p))
    };
    let observed_peak = ObservedPeak {
        n: velocity_n,
        p50: velocity_pct(0.50),
        p90: velocity_pct(0.90),
        p100: velocity_pct(1.0),
    };

    // The #636 blind-arm projection error. The sentinel split happens HERE rather than at parse, so
    // the excluded count is reported instead of silently vanishing: `session_at_recovery = 0` is the
    // session-window RESET, which would otherwise enter the distribution as a huge phantom
    // over-projection and swamp the single-digit-pp signal this SLI is tuning `1.75` against. The
    // `arrived_pct = 0` sentinel is the strict complement of the reconcilable set, so its count is
    // exact by construction — nothing escapes the split.
    let errors: Vec<f64> = inputs
        .blind_projections
        .iter()
        .filter(|p| p.arrived_pct > 0)
        .map(|p| p.projected_pct - f64::from(p.arrived_pct))
        .collect();
    let error_n = errors.len();
    let n_sentinel_excluded = inputs.blind_projections.len() - error_n;
    let error_pct = |p: f64| -> Option<f64> {
        (error_n > 0).then(|| round_pp(crate::percentile::percentile(&errors, p)))
    };
    let blind_projection_error = BlindProjectionError {
        n_blind_windows: inputs.blind_window_census.total,
        n_projectable: inputs.blind_projections.len(),
        n_reconcilable: error_n,
        n_sentinel_excluded,
        n_below_arm_gate: inputs.blind_window_census.below_arm_gate,
        n_without_velocity: inputs.blind_window_census.without_velocity,
        n_malformed: inputs.blind_window_census.malformed,
        // Both censored tails are unobservable from `blind_window` — `None`, never a fabricated 0.
        // Issue #591's uncensored `blind_enter`/`blind_exit` denominator populates them.
        n_swapped_away: None,
        n_never_recovered: None,
        p50: error_pct(0.50),
        p95: error_pct(0.95),
        p100: error_pct(1.0),
    };

    Report {
        window,
        swap_overshoot,
        projective_swap_out_pct,
        landing: compute_landing(inputs, samples),
        observed_peak,
        time_blind_near_limit_secs: inputs.time_blind_near_limit_secs,
        false_preempt: FalsePreempt {
            preemptive_swaps_observed: inputs.preemptive_swaps,
            near_limit_windows,
            would_be_wasted,
        },
        false_projection: FalseProjection {
            velocity_preempt_swaps_observed: inputs.velocity_preempt_swaps,
            rate: None,
        },
        blind_projection_error,
        rate_limit: RateLimit {
            rate_limited: inputs.rate_limited,
            transient: inputs.transient,
            cleared: inputs.cleared,
        },
    }
}

/// `[ok]` / `[OVER]` marker for a target check (ASCII so `--json`-free output needs no color).
fn ok_flag(met: bool) -> &'static str {
    if met {
        "[ok]"
    } else {
        "[OVER]"
    }
}

/// Render the human text readout — plain, greppable, targets inline. Roster-wide numbers and
/// fixed labels only; no account identifier appears (issue #15).
fn render_human(r: &Report) -> String {
    let mut out = String::new();
    out.push_str(
        "sessiometer reliability — swap-out overshoot SLO readout (offline; reads the event log + usage samples)\n\n",
    );

    // Active window (issue #494) — documents the bound so the numbers below are read in
    // context. Absent for the whole-log default, so that output is byte-for-byte unchanged.
    if let Some(w) = &r.window {
        out.push_str(&format!(
            "window: since {} ({}) — all SLIs bounded to events at/after the cutoff\n\n",
            w.cutoff_rfc3339(),
            w.since_arg,
        ));
    }

    // SLI 1 — swap-out session_pct percentiles vs targets.
    match (
        r.swap_overshoot.p50,
        r.swap_overshoot.p95,
        r.swap_overshoot.p100,
    ) {
        (Some(p50), Some(p95), Some(p100)) => {
            out.push_str(&format!(
                "swap-out session_pct (reason=session), n={}\n",
                r.swap_overshoot.n
            ));
            out.push_str(&format!(
                "  P50  = {p50}  target <= {SLO_SWAP_P50_MAX}  {}\n",
                ok_flag(p50 <= SLO_SWAP_P50_MAX)
            ));
            out.push_str(&format!("  P95  = {p95}\n"));
            out.push_str(&format!(
                "  P100 = {p100}  target < {SLO_SWAP_P100_MAX}   {}\n",
                ok_flag(p100 < SLO_SWAP_P100_MAX)
            ));
        }
        _ => out.push_str("swap-out session_pct (reason=session): no swaps observed\n"),
    }
    out.push('\n');

    // SLI 1b — projective swap-out session_pct percentiles (issue #539): the covered-swap acceptance
    // for the velocity-projection trigger, vs its own targets. Separate from the reactive block above
    // (now the poll-gap residual #540 owns); the full-trace P100 < 99 is #539 + #540 together.
    match (
        r.projective_swap_out_pct.p50,
        r.projective_swap_out_pct.p95,
        r.projective_swap_out_pct.p100,
    ) {
        (Some(p50), Some(p95), Some(p100)) => {
            out.push_str(&format!(
                "projective swap-out session_pct (reason=velocity_preempt), n={}\n",
                r.projective_swap_out_pct.n
            ));
            out.push_str(&format!(
                "  P50  = {p50}  target <= {SLO_PROJECTED_SWAP_P50_MAX}  {}\n",
                ok_flag(p50 <= SLO_PROJECTED_SWAP_P50_MAX)
            ));
            out.push_str(&format!("  P95  = {p95}\n"));
            out.push_str(&format!(
                "  P100 = {p100}  target <= {SLO_PROJECTED_SWAP_P100_MAX}  {}\n",
                ok_flag(p100 <= SLO_PROJECTED_SWAP_P100_MAX)
            ));
        }
        _ => out.push_str(
            "projective swap-out session_pct (reason=velocity_preempt): no projective swaps observed\n",
        ),
    }
    out.push('\n');

    // SLI 1c — LANDING-point session_pct (issue #595): the peak the OUTGOING account actually reached
    // after a reason=session swap parked it, reconstructed from usage-samples.jsonl. This is where the
    // #455 ceiling belongs — SLI 1's decision-point reading is blind to the post-swap committed tail.
    match (r.landing.p50, r.landing.p90, r.landing.p100) {
        (Some(p50), Some(p90), Some(p100)) => {
            out.push_str(&format!(
                "landing-point session_pct (post-swap peak of the outgoing account, window <= {LANDING_WINDOW_SECS}s)\n"
            ));
            out.push_str(&format!(
                "  measured n={} of {} reason=session swaps ({} with no post-swap sample)\n",
                r.landing.n_measured, r.landing.swaps_total, r.landing.n_unmeasured
            ));
            out.push_str(&format!("  P50  = {p50}\n"));
            out.push_str(&format!("  P90  = {p90}  (issue #597 tail-calibration input)\n"));
            out.push_str(&format!(
                "  P100 = {p100}  vs ceiling < {SLO_SWAP_P100_MAX}  {}\n",
                ok_flag(p100 < SLO_SWAP_P100_MAX)
            ));
            out.push_str(&format!(
                "  breach classes: {} post-swap tail (fired < {SLO_SWAP_P100_MAX}, landed >= {SLO_SWAP_P100_MAX}); {} gap-crossing (decision >= {SLO_SWAP_P100_MAX}); blind-burn: see time-blind SLI (issue #583)\n",
                r.landing.post_swap_tail, r.landing.gap_crossing
            ));
        }
        _ if r.landing.swaps_total == 0 => out.push_str(
            "landing-point session_pct (post-swap peak of the outgoing account): no reason=session swaps observed\n",
        ),
        _ => out.push_str(&format!(
            "landing-point session_pct (post-swap peak of the outgoing account): no post-swap samples in window ({} of {} reason=session swaps unmeasured)\n",
            r.landing.n_unmeasured, r.landing.swaps_total
        )),
    }
    out.push('\n');

    // SLI 1d — OBSERVED session velocity (issue #608): the live session_pct_per_min distribution vs
    // the assumed v_peak the swap-target reserve coupling bound is calibrated on. Keeps that constant
    // honest — if the real peak outruns v_peak, the config-load bound is silently too loose.
    match (
        r.observed_peak.p50,
        r.observed_peak.p90,
        r.observed_peak.p100,
    ) {
        (Some(p50), Some(p90), Some(p100)) => {
            out.push_str(
                "observed session velocity (session_pct_per_min, positive climbs only; the v_peak reserve-coupling calibration input)\n",
            );
            out.push_str(&format!(
                "  measured n={} usage_velocity samples\n",
                r.observed_peak.n
            ));
            out.push_str(&format!("  P50  = {p50:.2} %/min\n"));
            out.push_str(&format!("  P90  = {p90:.2} %/min\n"));
            out.push_str(&format!(
                "  P100 = {p100:.2} %/min  vs assumed v_peak {:.2} %/min  {}\n",
                crate::swap::V_PEAK_SESSION_PCT_PER_MIN,
                // Distinct label from the swap-out SLIs' [ok]/[OVER]: this is not an SLO breach but
                // a calibration signal — the constant is too loose, not the daemon too slow.
                // `v_peak_honest()` is `Some` here (this arm has `p100 = Some`, its sole input), so
                // the `== Some(false)` test — with any other value reading `[ok]` — carries no dead
                // arm; it stays the single source of truth the JSON path (`v_peak_honest`) also uses.
                if r.observed_peak.v_peak_honest() == Some(false) {
                    "[RECALIBRATE]"
                } else {
                    "[ok]"
                }
            ));
        }
        _ => out.push_str(
            "observed session velocity (session_pct_per_min): no usage_velocity samples observed\n",
        ),
    }
    out.push('\n');

    // SLI 2 — time blind & near-limit.
    out.push_str(&format!(
        "time blind & near-limit: {}s (sum of blind_window duration_secs where near_limit=true)\n\n",
        r.time_blind_near_limit_secs
    ));

    // SLI 3 — false-preempt: the real preemptive-swap count (issue #452, ADR-0017) plus the
    // interim blind-window proxy.
    out.push_str("false-preempt (preemptive swap whose target turned out unnecessary)\n");
    out.push_str(&format!(
        "  preemptive swaps observed: {}\n",
        r.false_preempt.preemptive_swaps_observed
    ));
    out.push_str(&format!(
        "  proxy (blind-window reconciliation, interim margin {PREEMPT_WASTED_MARGIN_PCT}pp): {} of {} near-limit windows would-be-wasted\n\n",
        r.false_preempt.would_be_wasted, r.false_preempt.near_limit_windows
    ));

    // SLI 3b — false-projection (issue #539): velocity-projection swaps that fired on a projection
    // the observed reading had not yet reached. Real count; the wasted FRACTION needs a post-swap
    // reconciliation, still pending (see the projective swap-out P50 above for the low-waste evidence).
    out.push_str(
        "false-projection (velocity-projection swap fired ahead of the observed overshoot)\n",
    );
    out.push_str(&format!(
        "  velocity-projection swaps observed: {}\n\n",
        r.false_projection.velocity_preempt_swaps_observed
    ));

    // SLI 6 — blind-arm projection error (issue #636): the REPORT-ONLY blind velocity-projection
    // arm's forecast, recomputed from #634's stamped ingredients, against the durable actual on the
    // same line. The percentiles are ALWAYS preceded by their cardinality line and ALWAYS followed
    // by the censoring disclosure — the #484 survivorship guard: this population is recovered-only,
    // so a bare percentile would read as the whole blind story when it is the easy half of it.
    let e = &r.blind_projection_error;
    out.push_str(
        "blind-arm projection error (projected − session_at_recovery, pp; the BLIND_VELOCITY_RATE_INFLATION tuning input)\n",
    );
    out.push_str(&format!(
        "  reconcilable n={} of {} projectable ({} excluded: session_at_recovery=0 window-reset sentinel), from {} blind windows\n",
        e.n_reconcilable, e.n_projectable, e.n_sentinel_excluded, e.n_blind_windows
    ));
    out.push_str(&format!(
        "  outside the arm's domain: {} below the T={}s gate; {} with no retained velocity; {} malformed\n",
        e.n_below_arm_gate,
        crate::daemon::BLIND_GATE_SECS,
        e.n_without_velocity,
        e.n_malformed
    ));
    out.push_str(
        "  censoring: RECOVERED-ONLY — swapped-away and never-recovered episodes are unobservable from blind_window (issue #591 owns the uncensored denominator)\n",
    );
    match (e.p50, e.p95, e.p100) {
        (Some(p50), Some(p95), Some(p100)) => {
            // Signed and explicitly so: positive = over-projected (cried DEGRADED early), negative =
            // under-projected (burned past the inflated forecast — the failure the arm exists to
            // prevent). Dropping the sign would erase the whole direction of the tuning signal.
            out.push_str(&format!("  P50  = {p50:+.2} pp\n"));
            out.push_str(&format!("  P95  = {p95:+.2} pp\n"));
            out.push_str(&format!("  P100 = {p100:+.2} pp\n"));
        }
        _ => out.push_str("  no reconcilable blind windows — percentiles withheld (an empty subject is not a 0 pp error)\n"),
    }
    out.push('\n');

    // SLI 4 — 429-rate neutrality (roster-wide counts; active attribution is a follow-up).
    out.push_str(&format!(
        "usage-poll 429 neutrality (roster-wide): rate_limited={} transient={} cleared={}\n",
        r.rate_limit.rate_limited, r.rate_limit.transient, r.rate_limit.cleared
    ));
    out
}

// --- rendering: JSON wire (schema:2) ----------------------------------------

/// The stable `--json` document. Field names are OWNED by this wire contract (decoupled from
/// the internal aggregate types), so an internal refactor cannot silently break the schema.
#[derive(serde::Serialize)]
struct ReliabilityWire {
    schema: u32,
    /// The active `--since` window (issue #494), or `null` for the whole-log aggregate. Added
    /// in `schema:2` — ADDITIVE (an always-present field), so a consumer that ignores unknown
    /// keys still parses every prior field. When present, the four SLIs below are bounded to it.
    window: Option<WindowWire>,
    swap_overshoot: SwapOvershootWire,
    /// The #539 velocity-projection covered-swap session_pct (schema:3; key renamed at schema:6, issue #635).
    projective_swap_out_pct: ProjectiveSwapOutPctWire,
    /// The #595 landing-point overshoot — where reason=session swaps actually landed (schema:4, additive).
    landing: LandingWire,
    /// The #608 observed session-velocity distribution vs the assumed `v_peak` (schema:5, additive).
    observed_peak: ObservedPeakWire,
    time_blind_near_limit_secs: u64,
    false_preempt: FalsePreemptWire,
    /// The #539 false-projection SLI (schema:3, additive).
    false_projection: FalseProjectionWire,
    /// The #636 blind-arm projection error (schema:7, additive).
    blind_projection_error: BlindProjectionErrorWire,
    rate_limit_neutrality: RateLimitWire,
}

/// The `--since` window bound (issue #494). Carries the operator's raw span plus the resolved
/// cutoff in BOTH forms — RFC 3339 (the log's own `ts=` shape) and epoch seconds (a machine
/// consumer can compare it without re-parsing a timestamp).
#[derive(serde::Serialize)]
struct WindowWire {
    /// The `--since` value as given (e.g. `"7d"`).
    since: String,
    /// The absolute cutoff instant, RFC 3339 UTC; events at/after it are included.
    cutoff_ts: String,
    /// The same cutoff as epoch seconds — the numeric bound `cutoff_ts` mirrors.
    cutoff_epoch: i64,
}

/// Swap-out overshoot block. `p50`/`p95`/`p100`/`met.*` are `null` with no data (an empty
/// subject is not a passing `0`), so a gate reads a target as met only on real evidence.
#[derive(serde::Serialize)]
struct SwapOvershootWire {
    n: usize,
    p50: Option<u8>,
    p95: Option<u8>,
    p100: Option<u8>,
    targets: SwapTargetsWire,
    met: SwapMetWire,
}

/// The documented swap-out targets (the extended #363 acceptance).
#[derive(serde::Serialize)]
struct SwapTargetsWire {
    p50_max: u8,
    p100_max: u8,
}

/// Per-target PASS flags — `null` when the corresponding percentile has no data.
#[derive(serde::Serialize)]
struct SwapMetWire {
    p50: Option<bool>,
    p100: Option<bool>,
}

/// Projective swap-out session_pct block (issue #539): the covered-swap acceptance for the velocity-
/// projection trigger, `null` percentiles / flags when no projective swap was observed.
#[derive(serde::Serialize)]
struct ProjectiveSwapOutPctWire {
    n: usize,
    p50: Option<u8>,
    p95: Option<u8>,
    p100: Option<u8>,
    targets: ProjectedSwapTargetsWire,
    met: SwapMetWire,
}

/// The documented projected swap-out targets (the #539/#538 covered-swap acceptance).
#[derive(serde::Serialize)]
struct ProjectedSwapTargetsWire {
    p50_max: u8,
    p100_max: u8,
}

/// Landing-point overshoot block (issue #595): where reason=session swaps actually landed — the
/// post-swap peak of the parked account, reconstructed from the usage-sample store. `p50`/`p90`/
/// `p100`/`p100_met` are `null` when no swap had a post-swap sample (an empty subject is not a
/// passing `0`).
#[derive(serde::Serialize)]
struct LandingWire {
    /// reason=session swap anchors in view (the coverage denominator).
    swaps_total: usize,
    /// Anchors with >= 1 post-swap sample — the measured subject the percentiles summarize.
    n_measured: usize,
    /// Anchors with no post-swap sample in the window — a coverage gap, not a `0` landing.
    n_unmeasured: usize,
    p50: Option<u8>,
    /// The 90th-percentile landing — the #597 tail-calibration input.
    p90: Option<u8>,
    p100: Option<u8>,
    /// The bounded post-swap window these landings were measured over (seconds).
    window_secs: i64,
    /// The strict-`<` ceiling this landing is checked against — the SAME #455 ceiling SLI 1 uses.
    ceiling: u8,
    /// Whether the worst landing meets `< ceiling` (`null` with no measured episode).
    p100_met: Option<bool>,
    /// The issue #595 breach-class split over the measured episodes.
    classes: LandingClassesWire,
}

/// The landing-point breach classes (issue #595): the two the readout computes directly; blind-burn
/// is the separate blind-episode record (issue #583 / the time-blind SLI), referenced not recomputed.
#[derive(serde::Serialize)]
struct LandingClassesWire {
    /// Fired below the ceiling but landed at/over it — the post-swap committed tail (class 1).
    post_swap_tail: usize,
    /// Decision reading already at/over the ceiling — visible in SLI 1 (gap-crossing, class 2).
    gap_crossing: usize,
}

/// Observed session-velocity block (issue #608): the live `session_pct_per_min` percentiles vs the
/// assumed peak constant the `v_peak` coupling bound is calibrated on. `p50`/`p90`/`p100`/`met.*` are
/// `null` with no positive velocity sample (an empty subject is not a passing distribution). Rates
/// are rounded to two decimals for the wire, matching the `usage_velocity` log line's own precision.
#[derive(serde::Serialize)]
struct ObservedPeakWire {
    n: usize,
    p50: Option<f64>,
    p90: Option<f64>,
    p100: Option<f64>,
    /// The assumed peak the bound is calibrated on (`swap::V_PEAK_SESSION_PCT_PER_MIN`), %/min.
    v_peak_pct_per_min: f64,
    /// Whether the observed max (`p100`) is still at/below `v_peak` — `false` means the constant is
    /// too loose and the coupling bound needs re-calibrating; `null` with no data.
    v_peak_honest: Option<bool>,
}

/// False-preempt block: the real (pending) rate plus the labeled interim proxy.
#[derive(serde::Serialize)]
struct FalsePreemptWire {
    preemptive_swaps_observed: u32,
    /// The real false-preempt rate. Always `null` today (#452 pending); populates when the
    /// preemptive-swap path lands.
    rate: Option<f64>,
    proxy: FalsePreemptProxyWire,
}

/// The blind-window-reconciliation proxy for false-preempt (clearly NOT the real rate).
#[derive(serde::Serialize)]
struct FalsePreemptProxyWire {
    near_limit_windows: u32,
    would_be_wasted: u32,
    interim_margin_pct: u8,
}

/// False-projection block (issue #539): the real velocity-projection swap count; `rate` is the
/// wasted fraction, always `null` today (needs a post-swap reconciliation, pending — like
/// [`FalsePreemptWire::rate`]).
#[derive(serde::Serialize)]
struct FalseProjectionWire {
    velocity_preempt_swaps_observed: u32,
    rate: Option<f64>,
}

/// Blind-arm projection-error block (issue #636): `projected − session_at_recovery` percentiles in
/// percentage points, SIGNED (positive = over-projected, negative = under-projected — the burn ran
/// past the inflated forecast).
///
/// The percentiles are never published alone: the four counts above them carry the cardinality and
/// the sentinel exclusion, and the two `null` census fields carry the CENSORING — this population is
/// recovered-only, because `blind_window` fires on the recovery edge of the active account. A
/// consumer that reads `p100` without reading `n_swapped_away` / `n_never_recovered` is reading the
/// easy half of the distribution; those two are `null` (unobservable), NEVER `0`, until issue #591's
/// uncensored `blind_enter`/`blind_exit` denominator lands. `p50`/`p95`/`p100` are `null` on an empty
/// reconcilable population (an empty subject is not a passing `0 pp` error).
#[derive(serde::Serialize)]
struct BlindProjectionErrorWire {
    /// Every `blind_window` line in view. Partitioned EXACTLY by `n_projectable +
    /// n_below_arm_gate + n_without_velocity + n_malformed`, so no line goes undisclosed.
    n_blind_windows: usize,
    /// In-domain windows carrying #634's velocity ingredients (`n_reconcilable +
    /// n_sentinel_excluded`).
    n_projectable: usize,
    /// The percentile subject: projectable windows with a real actual (`session_at_recovery > 0`).
    n_reconcilable: usize,
    /// Excluded for the `session_at_recovery = 0` session-window-RESET sentinel.
    n_sentinel_excluded: usize,
    /// Windows shorter than the arm's first gate (`T` seconds, `arm_gate_secs` below) — the arm
    /// never projected here, so there is no forecast to grade.
    n_below_arm_gate: usize,
    /// In-domain windows with no retained velocity — the arm's second gate (coverage, not error).
    n_without_velocity: usize,
    /// Windows dropped as corrupt (unreadable fields, or a projection that overflowed).
    n_malformed: usize,
    /// The arm's first gate in seconds, at TODAY's value — stamped here because it is NOT on the
    /// log line, so a consumer can tell which `T` this partition was computed with.
    arm_gate_secs: u64,
    /// Swapped-away episodes. Always `null`: unobservable from the active-scoped `blind_window`.
    n_swapped_away: Option<usize>,
    /// Never-recovered episodes. Always `null`: unobservable from the recovery-edge `blind_window`.
    n_never_recovered: Option<usize>,
    p50: Option<f64>,
    p95: Option<f64>,
    p100: Option<f64>,
}

/// 429-rate neutrality counts.
#[derive(serde::Serialize)]
struct RateLimitWire {
    rate_limited: u32,
    transient: u32,
    cleared: u32,
}

/// Build the wire view from the internal [`Report`].
fn reliability_wire(r: &Report) -> ReliabilityWire {
    ReliabilityWire {
        schema: JSON_SCHEMA_VERSION,
        window: r.window.as_ref().map(|w| WindowWire {
            since: w.since_arg.clone(),
            cutoff_ts: w.cutoff_rfc3339(),
            cutoff_epoch: w.cutoff_epoch,
        }),
        swap_overshoot: SwapOvershootWire {
            n: r.swap_overshoot.n,
            p50: r.swap_overshoot.p50,
            p95: r.swap_overshoot.p95,
            p100: r.swap_overshoot.p100,
            targets: SwapTargetsWire {
                p50_max: SLO_SWAP_P50_MAX,
                p100_max: SLO_SWAP_P100_MAX,
            },
            met: SwapMetWire {
                p50: r.swap_overshoot.p50_met(),
                p100: r.swap_overshoot.p100_met(),
            },
        },
        projective_swap_out_pct: ProjectiveSwapOutPctWire {
            n: r.projective_swap_out_pct.n,
            p50: r.projective_swap_out_pct.p50,
            p95: r.projective_swap_out_pct.p95,
            p100: r.projective_swap_out_pct.p100,
            targets: ProjectedSwapTargetsWire {
                p50_max: SLO_PROJECTED_SWAP_P50_MAX,
                p100_max: SLO_PROJECTED_SWAP_P100_MAX,
            },
            met: SwapMetWire {
                p50: r.projective_swap_out_pct.p50_met(),
                p100: r.projective_swap_out_pct.p100_met(),
            },
        },
        landing: LandingWire {
            swaps_total: r.landing.swaps_total,
            n_measured: r.landing.n_measured,
            n_unmeasured: r.landing.n_unmeasured,
            p50: r.landing.p50,
            p90: r.landing.p90,
            p100: r.landing.p100,
            window_secs: LANDING_WINDOW_SECS,
            ceiling: SLO_SWAP_P100_MAX,
            p100_met: r.landing.p100_met(),
            classes: LandingClassesWire {
                post_swap_tail: r.landing.post_swap_tail,
                gap_crossing: r.landing.gap_crossing,
            },
        },
        observed_peak: ObservedPeakWire {
            n: r.observed_peak.n,
            p50: r.observed_peak.p50,
            p90: r.observed_peak.p90,
            p100: r.observed_peak.p100,
            v_peak_pct_per_min: crate::swap::V_PEAK_SESSION_PCT_PER_MIN,
            v_peak_honest: r.observed_peak.v_peak_honest(),
        },
        time_blind_near_limit_secs: r.time_blind_near_limit_secs,
        false_preempt: FalsePreemptWire {
            preemptive_swaps_observed: r.false_preempt.preemptive_swaps_observed,
            rate: None,
            proxy: FalsePreemptProxyWire {
                near_limit_windows: r.false_preempt.near_limit_windows,
                would_be_wasted: r.false_preempt.would_be_wasted,
                interim_margin_pct: PREEMPT_WASTED_MARGIN_PCT,
            },
        },
        false_projection: FalseProjectionWire {
            velocity_preempt_swaps_observed: r.false_projection.velocity_preempt_swaps_observed,
            rate: r.false_projection.rate,
        },
        blind_projection_error: BlindProjectionErrorWire {
            n_blind_windows: r.blind_projection_error.n_blind_windows,
            n_projectable: r.blind_projection_error.n_projectable,
            n_reconcilable: r.blind_projection_error.n_reconcilable,
            n_sentinel_excluded: r.blind_projection_error.n_sentinel_excluded,
            n_below_arm_gate: r.blind_projection_error.n_below_arm_gate,
            n_without_velocity: r.blind_projection_error.n_without_velocity,
            n_malformed: r.blind_projection_error.n_malformed,
            arm_gate_secs: crate::daemon::BLIND_GATE_SECS,
            n_swapped_away: r.blind_projection_error.n_swapped_away,
            n_never_recovered: r.blind_projection_error.n_never_recovered,
            p50: r.blind_projection_error.p50,
            p95: r.blind_projection_error.p95,
            p100: r.blind_projection_error.p100,
        },
        rate_limit_neutrality: RateLimitWire {
            rate_limited: r.rate_limit.rate_limited,
            transient: r.rate_limit.transient,
            cleared: r.rate_limit.cleared,
        },
    }
}

/// Render the stable `--json` document — PRETTY-printed with a trailing newline (the `stats
/// --json` shape). The wire is all bare integers / bools / nulls, so serialization is
/// infallible in practice; the error is mapped, never panicked.
fn render_json(r: &Report) -> Result<String> {
    let mut json = serde_json::to_string_pretty(&reliability_wire(r))
        .map_err(|_| Error::ReliabilitySerialize("a readout value was not serializable"))?;
    json.push('\n');
    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative log slice exercising all four event families, plus lines that MUST be
    /// dropped: a weekly swap (out of scope — #455 Finding 1), a manual swap (`session_pct=0`), an
    /// emergency swap (no `session_pct`), a non-near-limit blind window, and unrelated events.
    /// Swap lines carry real-shaped account **emails** in `from=`/`to=` — exactly as the production
    /// log does — so `readout_carries_no_pii` genuinely exercises the email-leak guard instead of
    /// passing vacuously on non-email handles.
    ///
    /// The `blind_enter` / `blind_exit` pair (issue #583) is here as a BLAST-RADIUS guard, not as an
    /// input: this readout aggregates `blind_window` and must stay on it (that event's recovery-edge
    /// semantics are retained for exactly this SLO purpose), so the uncensored pair MUST fall through
    /// the `_ => {}` arm and perturb NOTHING. Their fields are deliberately adversarial — a
    /// `near_limit=true` u-D episode with a 999 s `duration_secs` and its own `session_pct`, and u-D
    /// has no `blind_window` line of its own — so any arm that ever picks them up fails the
    /// assertions below LOUDLY rather than silently corrupting the SLI the #484 promotion bar reads:
    /// `time_blind_near_limit_secs` would read 1899 against the asserted 900, and
    /// `near_limit_reconciliations` would gain a spurious third pair against the two it pins.
    /// Whether this readout should ever aggregate the uncensored pair instead is a separate,
    /// unfiled decision — the guard pins today's answer either way, so making that change has to
    /// be deliberate.
    const FIXTURE_LOG: &str = "\
ts=2026-07-11T00:00:00Z event=swap from=oleksii@pelykh.com to=oleksii@pelykhconsulting.fr reason=session session_pct=96
ts=2026-07-11T00:05:00Z event=swap from=oleksii@pelykhconsulting.fr to=oleksii@pelykh.com reason=weekly session_pct=42
ts=2026-07-11T00:06:00Z event=swap from=oleksii@pelykh.com to=oleksii@pelykhconsulting.fr reason=session session_pct=100 late=true
ts=2026-07-11T00:07:00Z event=swap from=oleksii@pelykh.com to=oleksii@pelykhconsulting.fr reason=manual session_pct=0
ts=2026-07-11T00:08:00Z event=emergency_swap from=oleksii@pelykh.com to=oleksii@pelykhconsulting.fr
ts=2026-07-11T00:09:00Z event=restash account=oleksii@pelykh.com
ts=2026-07-11T00:09:30Z event=canonical_recovered account=oleksii@pelykhconsulting.fr
ts=2026-07-11T00:10:00Z event=blind_window acct=u-A duration_secs=300 session_pct=97 session_at_recovery=99 near_limit=true
ts=2026-07-11T00:20:00Z event=blind_window acct=u-B duration_secs=600 session_pct=96 session_at_recovery=40 near_limit=true
ts=2026-07-11T00:30:00Z event=blind_window acct=u-C duration_secs=120 session_pct=50 session_at_recovery=51 near_limit=false
ts=2026-07-11T00:31:00Z event=blind_enter acct=u-D session_pct=97 weekly_pct=40 was_active=true near_limit=true
ts=2026-07-11T00:32:00Z event=blind_exit acct=u-D duration_secs=999 session_burn_pct=-97 weekly_burn_pct=12 session_pct=97 session_at_recovery=0 weekly_pct=40 weekly_at_recovery=52 was_active=true swapped_away=true near_limit=true
ts=2026-07-11T00:40:00Z event=usage_backoff acct=u-A class=rate_limited consecutive=1 backoff_secs=60
ts=2026-07-11T00:41:00Z event=usage_backoff acct=u-A class=rate_limited consecutive=2 backoff_secs=120 retry_after_secs=120
ts=2026-07-11T00:42:00Z event=usage_backoff acct=u-B class=transient consecutive=1 backoff_secs=30
ts=2026-07-11T00:45:00Z event=usage_backoff_cleared acct=u-A
ts=2026-07-11T00:50:00Z event=usage_velocity acct=u-A session_pct_per_min=0.20 weekly_pct_per_min=0.01 elapsed_secs=120 session_delta_pct=1 weekly_delta_pct=0
";

    fn fixture_report() -> Report {
        // The event-log fixture carries reason=session swaps but NO usage samples, so the landing
        // SLI reconstructs zero measured episodes (both swaps unmeasured) — the landing-specific
        // fixtures below supply samples. Passing `&[]` keeps every prior assertion unchanged.
        aggregate(&parse_events(FIXTURE_LOG, None), &[], None)
    }

    /// A usage [`Sample`] at `ts` for roster label `acct` with absolute `session` as a fraction
    /// (`weekly` is fixed at 0.10 — the landing join reads only `session`) — the landing-SLI join
    /// input, a trimmed form of `stats`' own `sample` test helper.
    fn sample(ts: i64, acct: &str, session: f64) -> Sample {
        Sample::new(ts, "claude", acct, session, 0.10)
    }

    #[test]
    fn parse_folds_only_the_four_relevant_families() {
        let inputs = parse_events(FIXTURE_LOG, None);
        // reason=session swaps ONLY — weekly (42), manual (0), and emergency all dropped (#455 Finding 1).
        assert_eq!(inputs.swap_out_pcts, vec![96.0, 100.0]);
        // Only near_limit=true windows: 300 + 600 (the near_limit=false 120 is excluded).
        assert_eq!(inputs.time_blind_near_limit_secs, 900);
        assert_eq!(inputs.near_limit_reconciliations, vec![(97, 99), (96, 40)]);
        assert_eq!(inputs.rate_limited, 2);
        assert_eq!(inputs.transient, 1);
        assert_eq!(inputs.cleared, 1);
    }

    #[test]
    fn aggregate_computes_percentiles_targets_and_proxy() {
        let r = fixture_report();
        // n=2 sorted [96,100]: P50=ceil(.5·2)=1→96, P95=ceil(.95·2)=2→100, P100→100.
        assert_eq!(r.swap_overshoot.n, 2);
        assert_eq!(r.swap_overshoot.p50, Some(96));
        assert_eq!(r.swap_overshoot.p95, Some(100));
        assert_eq!(r.swap_overshoot.p100, Some(100));
        // P50=96 <= 97 → met; P100=100 not < 99 → NOT met.
        assert_eq!(r.swap_overshoot.p50_met(), Some(true));
        assert_eq!(r.swap_overshoot.p100_met(), Some(false));
        assert_eq!(r.time_blind_near_limit_secs, 900);
        // Proxy: 2 near-limit windows; (97,99) recovery rose → necessary; (96,40) dropped 56>20
        // → would-be-wasted. So 1 of 2.
        assert_eq!(r.false_preempt.near_limit_windows, 2);
        assert_eq!(r.false_preempt.would_be_wasted, 1);
        assert_eq!(r.false_preempt.preemptive_swaps_observed, 0);
        assert_eq!(r.rate_limit.rate_limited, 2);
    }

    #[test]
    fn empty_log_yields_no_swaps_and_zeroed_slis() {
        let r = aggregate(&parse_events("", None), &[], None);
        assert_eq!(r.swap_overshoot.n, 0);
        // Cardinality-zero: percentiles are None (not a passing 0), so no target is asserted met.
        assert_eq!(r.swap_overshoot.p50, None);
        assert_eq!(r.swap_overshoot.p100, None);
        assert_eq!(r.swap_overshoot.p50_met(), None);
        assert_eq!(r.swap_overshoot.p100_met(), None);
        assert_eq!(r.time_blind_near_limit_secs, 0);
        assert_eq!(r.false_preempt.near_limit_windows, 0);
        // #608 observed-peak SLI: no usage_velocity samples → None percentiles, v_peak_honest None
        // (never asserted honest on an empty subject).
        assert_eq!(r.observed_peak.n, 0);
        assert_eq!(r.observed_peak.p100, None);
        assert_eq!(r.observed_peak.v_peak_honest(), None);
    }

    // --- issue #608: the observed session-velocity SLI ------------------------

    /// A usage_velocity fixture spanning the p50/p90/max shape of the real distribution PLUS the
    /// two lines that must be DROPPED: a window-reset NEGATIVE rate, and a flat 0.00 climb. The
    /// positive samples are 0.63 / 1.86 / 6.95 (the ADR's p50/p90/max) so the percentiles land on
    /// recognizable values.
    const VELOCITY_LOG: &str = "\
ts=2026-07-11T00:00:00Z event=usage_velocity acct=u-A session_pct_per_min=0.63 weekly_pct_per_min=0.01 elapsed_secs=60 session_delta_pct=1 weekly_delta_pct=0
ts=2026-07-11T00:01:00Z event=usage_velocity acct=u-A session_pct_per_min=1.86 weekly_pct_per_min=0.02 elapsed_secs=60 session_delta_pct=2 weekly_delta_pct=0
ts=2026-07-11T00:02:00Z event=usage_velocity acct=u-A session_pct_per_min=6.95 weekly_pct_per_min=0.03 elapsed_secs=60 session_delta_pct=7 weekly_delta_pct=0
ts=2026-07-11T00:03:00Z event=usage_velocity acct=u-A session_pct_per_min=-92.00 weekly_pct_per_min=0.00 elapsed_secs=60 session_delta_pct=-92 weekly_delta_pct=0
ts=2026-07-11T00:04:00Z event=usage_velocity acct=u-A session_pct_per_min=0.00 weekly_pct_per_min=0.00 elapsed_secs=60 session_delta_pct=0 weekly_delta_pct=0
";

    #[test]
    fn observed_peak_folds_positive_climbs_and_drops_resets_and_flats() {
        let inputs = parse_events(VELOCITY_LOG, None);
        // Only the three POSITIVE climbs — the −92 window-reset and the 0.00 flat are dropped so
        // they cannot drag the distribution down and hide a real peak.
        assert_eq!(inputs.session_velocities, vec![0.63, 1.86, 6.95]);
        let r = aggregate(&inputs, &[], None);
        assert_eq!(r.observed_peak.n, 3);
        // n=3 sorted [0.63,1.86,6.95]: P50=ceil(.5·3)=2→1.86, P90=ceil(.9·3)=3→6.95, P100→6.95.
        assert_eq!(r.observed_peak.p50, Some(1.86));
        assert_eq!(r.observed_peak.p90, Some(6.95));
        assert_eq!(r.observed_peak.p100, Some(6.95));
        // The observed max EQUALS the assumed v_peak (6.95), so the constant is still honest — the
        // epsilon absorbs the display-rounding round trip so equality is not flagged by float dust.
        assert_eq!(r.observed_peak.v_peak_honest(), Some(true));
    }

    #[test]
    fn observed_peak_flags_a_real_peak_that_outruns_the_assumed_v_peak() {
        // A single sample above v_peak (6.95) trips the recalibrate signal — the SLI's entire
        // purpose: when the live peak outruns the constant, the config-load coupling bound is
        // silently too loose and the constant needs re-calibrating (the "measure, don't trust the
        // constant" discipline TAIL_MARGIN has via the #595 landing SLI).
        let log = "ts=2026-07-11T00:00:00Z event=usage_velocity acct=u-A session_pct_per_min=8.40 weekly_pct_per_min=0.01 elapsed_secs=60 session_delta_pct=8 weekly_delta_pct=0\n";
        let r = aggregate(&parse_events(log, None), &[], None);
        assert_eq!(r.observed_peak.p100, Some(8.40));
        assert_eq!(
            r.observed_peak.v_peak_honest(),
            Some(false),
            "a peak above the assumed v_peak must flag the constant as too loose"
        );
        // The human render surfaces the distinct calibration marker, NOT the swap-out [OVER].
        let human = render_human(&r);
        assert!(
            human.contains("[RECALIBRATE]"),
            "the too-loose constant must surface a calibration signal: {human}"
        );
        // And the JSON exposes the machine-readable flag for a gate.
        let json = render_json(&r).expect("serializes");
        assert!(
            json.contains("\"v_peak_honest\": false"),
            "json must carry the recalibrate flag: {json}"
        );
    }

    #[test]
    fn observed_peak_is_bounded_by_the_active_window() {
        // The #494 `--since` window bounds this SLI like every other: a cutoff after the early
        // samples drops them. Two samples days apart, cut between them.
        let log = "\
ts=2026-07-01T00:00:00Z event=usage_velocity acct=u-A session_pct_per_min=5.00 weekly_pct_per_min=0.01 elapsed_secs=60 session_delta_pct=5 weekly_delta_pct=0
ts=2026-07-10T00:00:00Z event=usage_velocity acct=u-A session_pct_per_min=1.00 weekly_pct_per_min=0.01 elapsed_secs=60 session_delta_pct=1 weekly_delta_pct=0
";
        let cutoff = epoch("2026-07-05T00:00:00Z");
        let inputs = parse_events(log, Some(cutoff));
        assert_eq!(
            inputs.session_velocities,
            vec![1.00],
            "the pre-cutoff 5.00 sample is dropped; only the Jul-10 1.00 remains"
        );
    }

    #[test]
    fn passing_targets_are_flagged_met() {
        // A clean roster: swaps at 95/96/97 → P50=96<=97, P100=97<99.
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=a to=b reason=session session_pct=95
ts=2026-07-11T00:01:00Z event=swap from=a to=b reason=session session_pct=96
ts=2026-07-11T00:02:00Z event=swap from=a to=b reason=session session_pct=97
";
        let r = aggregate(&parse_events(log, None), &[], None);
        assert_eq!(r.swap_overshoot.p50, Some(96));
        assert_eq!(r.swap_overshoot.p100, Some(97));
        assert_eq!(r.swap_overshoot.p50_met(), Some(true));
        assert_eq!(r.swap_overshoot.p100_met(), Some(true));
    }

    #[test]
    fn human_render_is_stable_and_targets_documented() {
        let out = render_human(&fixture_report());
        assert_eq!(
            out,
            concat!(
                "sessiometer reliability — swap-out overshoot SLO readout (offline; reads the event log + usage samples)\n",
                "\n",
                "swap-out session_pct (reason=session), n=2\n",
                "  P50  = 96  target <= 97  [ok]\n",
                "  P95  = 100\n",
                "  P100 = 100  target < 99   [OVER]\n",
                "\n",
                "projective swap-out session_pct (reason=velocity_preempt): no projective swaps observed\n",
                "\n",
                "landing-point session_pct (post-swap peak of the outgoing account): no post-swap samples in window (2 of 2 reason=session swaps unmeasured)\n",
                "\n",
                "observed session velocity (session_pct_per_min, positive climbs only; the v_peak reserve-coupling calibration input)\n",
                "  measured n=1 usage_velocity samples\n",
                "  P50  = 0.20 %/min\n",
                "  P90  = 0.20 %/min\n",
                "  P100 = 0.20 %/min  vs assumed v_peak 6.95 %/min  [ok]\n",
                "\n",
                "time blind & near-limit: 900s (sum of blind_window duration_secs where near_limit=true)\n",
                "\n",
                "false-preempt (preemptive swap whose target turned out unnecessary)\n",
                "  preemptive swaps observed: 0\n",
                "  proxy (blind-window reconciliation, interim margin 20pp): 1 of 2 near-limit windows would-be-wasted\n",
                "\n",
                "false-projection (velocity-projection swap fired ahead of the observed overshoot)\n",
                "  velocity-projection swaps observed: 0\n",
                "\n",
                "blind-arm projection error (projected − session_at_recovery, pp; the BLIND_VELOCITY_RATE_INFLATION tuning input)\n",
                "  reconcilable n=0 of 0 projectable (0 excluded: session_at_recovery=0 window-reset sentinel), from 3 blind windows\n",
                "  outside the arm's domain: 2 below the T=300s gate; 1 with no retained velocity; 0 malformed\n",
                "  censoring: RECOVERED-ONLY — swapped-away and never-recovered episodes are unobservable from blind_window (issue #591 owns the uncensored denominator)\n",
                "  no reconcilable blind windows — percentiles withheld (an empty subject is not a 0 pp error)\n",
                "\n",
                "usage-poll 429 neutrality (roster-wide): rate_limited=2 transient=1 cleared=1\n",
            )
        );
    }

    #[test]
    fn human_render_handles_no_swaps() {
        let out = render_human(&aggregate(&parse_events("", None), &[], None));
        assert!(
            out.contains("swap-out session_pct (reason=session): no swaps observed"),
            "cardinality-zero must not print a fabricated P100: {out}"
        );
    }

    #[test]
    fn json_render_is_stable_schema_7() {
        // The whole-log default: `window` is null and every field except the #635-renamed
        // velocity-projection key (`projective_swap_out_pct`, schema:6) is byte-identical to
        // schema:1–5 — the additive contract (#494/#539/#595/#608/#636) plus the one #635 rename. The
        // #608 `observed_peak` object is always-present (n=1 here — the FIXTURE_LOG's single
        // usage_velocity line at 0.20 %/min, well under the 6.95 v_peak, so v_peak_honest=true), as is
        // the #636 `blind_projection_error` object (schema:7 — the FIXTURE_LOG's three blind_window
        // lines predate #634's ingredients, so all three land in `n_without_velocity` and no
        // percentile is asserted). A `--since` document is asserted separately in
        // `json_documents_the_active_window`.
        let out = render_json(&fixture_report()).expect("integer wire serializes");
        assert_eq!(
            out,
            concat!(
                "{\n",
                "  \"schema\": 7,\n",
                "  \"window\": null,\n",
                "  \"swap_overshoot\": {\n",
                "    \"n\": 2,\n",
                "    \"p50\": 96,\n",
                "    \"p95\": 100,\n",
                "    \"p100\": 100,\n",
                "    \"targets\": {\n",
                "      \"p50_max\": 97,\n",
                "      \"p100_max\": 99\n",
                "    },\n",
                "    \"met\": {\n",
                "      \"p50\": true,\n",
                "      \"p100\": false\n",
                "    }\n",
                "  },\n",
                "  \"projective_swap_out_pct\": {\n",
                "    \"n\": 0,\n",
                "    \"p50\": null,\n",
                "    \"p95\": null,\n",
                "    \"p100\": null,\n",
                "    \"targets\": {\n",
                "      \"p50_max\": 94,\n",
                "      \"p100_max\": 98\n",
                "    },\n",
                "    \"met\": {\n",
                "      \"p50\": null,\n",
                "      \"p100\": null\n",
                "    }\n",
                "  },\n",
                "  \"landing\": {\n",
                "    \"swaps_total\": 2,\n",
                "    \"n_measured\": 0,\n",
                "    \"n_unmeasured\": 2,\n",
                "    \"p50\": null,\n",
                "    \"p90\": null,\n",
                "    \"p100\": null,\n",
                "    \"window_secs\": 900,\n",
                "    \"ceiling\": 99,\n",
                "    \"p100_met\": null,\n",
                "    \"classes\": {\n",
                "      \"post_swap_tail\": 0,\n",
                "      \"gap_crossing\": 0\n",
                "    }\n",
                "  },\n",
                "  \"observed_peak\": {\n",
                "    \"n\": 1,\n",
                "    \"p50\": 0.2,\n",
                "    \"p90\": 0.2,\n",
                "    \"p100\": 0.2,\n",
                "    \"v_peak_pct_per_min\": 6.95,\n",
                "    \"v_peak_honest\": true\n",
                "  },\n",
                "  \"time_blind_near_limit_secs\": 900,\n",
                "  \"false_preempt\": {\n",
                "    \"preemptive_swaps_observed\": 0,\n",
                "    \"rate\": null,\n",
                "    \"proxy\": {\n",
                "      \"near_limit_windows\": 2,\n",
                "      \"would_be_wasted\": 1,\n",
                "      \"interim_margin_pct\": 20\n",
                "    }\n",
                "  },\n",
                "  \"false_projection\": {\n",
                "    \"velocity_preempt_swaps_observed\": 0,\n",
                "    \"rate\": null\n",
                "  },\n",
                "  \"blind_projection_error\": {\n",
                "    \"n_blind_windows\": 3,\n",
                "    \"n_projectable\": 0,\n",
                "    \"n_reconcilable\": 0,\n",
                "    \"n_sentinel_excluded\": 0,\n",
                "    \"n_below_arm_gate\": 2,\n",
                "    \"n_without_velocity\": 1,\n",
                "    \"n_malformed\": 0,\n",
                "    \"arm_gate_secs\": 300,\n",
                "    \"n_swapped_away\": null,\n",
                "    \"n_never_recovered\": null,\n",
                "    \"p50\": null,\n",
                "    \"p95\": null,\n",
                "    \"p100\": null\n",
                "  },\n",
                "  \"rate_limit_neutrality\": {\n",
                "    \"rate_limited\": 2,\n",
                "    \"transient\": 1,\n",
                "    \"cleared\": 1\n",
                "  }\n",
                "}\n",
            )
        );
    }

    #[test]
    fn json_no_data_serializes_nulls_not_a_passing_zero() {
        let out = render_json(&aggregate(&parse_events("", None), &[], None)).expect("serializes");
        assert!(
            out.contains("\"p100\": null"),
            "no-data P100 must be null: {out}"
        );
        assert!(
            out.contains("\"p50\": null"),
            "no-data P50 must be null: {out}"
        );
        assert!(out.contains("\"met\": {\n      \"p50\": null,\n      \"p100\": null\n    }"));
    }

    // --- issue #636: the blind-arm projection error ---------------------------

    /// A `blind_window` fixture spanning every census bucket: the three lines the SLI must SCORE,
    /// and the four it must not — the `session_at_recovery=0` window-reset sentinel, two windows
    /// under the arm's own duration gate, and a pre-#634 line with no ingredients at all.
    ///
    /// The arithmetic is `projected = session_pct + rate × inflation × duration_secs`
    /// ([`crate::daemon`]'s `blind_velocity_projected_armed`; no line here carries a #670 mark, so
    /// the anchor term is the raw `session_pct`), recomputed from each line's OWN stamped tokens:
    ///
    /// - u-A: `30 + 0.01 × 1.75 × 600  = 40.50` vs 40 ⇒ **+0.50** (mild over-projection)
    /// - u-B: `55 + 0.01 × 1.75 × 900  = 70.75` vs 75 ⇒ **−4.25** (UNDER-projected — the account
    ///   burned past the inflated forecast, the failure direction the arm exists to prevent)
    /// - u-C: `40 + 0.008 × 1.75 × 1800 = 65.20` vs 52 ⇒ **+13.20**
    /// - u-D: `62 + 0.015 × 1.75 × 1200 = 93.50` vs **0** ⇒ SENTINEL-EXCLUDED. Left in deliberately
    ///   as a guard with teeth: admitted, its `+93.50` phantom would become P95/P100 and swamp the
    ///   single-digit-pp signal the readout tunes `1.75` against, so a regression fails loudly.
    /// - u-E: past the gate but NO ingredients ⇒ `without_velocity` coverage, never a `0 pp` error.
    /// - u-F (`200 s`) / u-G (exactly `300 s`) ⇒ BELOW the arm's `T` gate. Both carry full
    ///   ingredients — because the daemon stamps them regardless of duration — and both would score
    ///   a tiny `+0.05` error that drags P50 toward "1.75 is perfectly calibrated" on windows the
    ///   arm never evaluated. u-G pins the boundary as EXCLUSIVE (`blind_secs <= T` ⇒ no arm), the
    ///   same comparator the arm itself uses.
    ///
    /// Every scored line is `near_limit=false` — the climbing population is exactly the one the
    /// near-limit gate would discard, so this fixture also pins that the fold runs BEFORE that gate.
    const BLIND_PROJECTION_LOG: &str = "\
ts=2026-07-11T00:10:00Z event=blind_window acct=u-A duration_secs=600 session_pct=30 session_at_recovery=40 near_limit=false rate=0.010000 inflation=1.75 ceiling=95.00
ts=2026-07-11T00:20:00Z event=blind_window acct=u-B duration_secs=900 session_pct=55 session_at_recovery=75 near_limit=false rate=0.010000 inflation=1.75 ceiling=95.00
ts=2026-07-11T00:30:00Z event=blind_window acct=u-C duration_secs=1800 session_pct=40 session_at_recovery=52 near_limit=false rate=0.008000 inflation=1.75 ceiling=95.00
ts=2026-07-11T00:40:00Z event=blind_window acct=u-D duration_secs=1200 session_pct=62 session_at_recovery=0 near_limit=false rate=0.015000 inflation=1.75 ceiling=95.00
ts=2026-07-11T00:50:00Z event=blind_window acct=u-E duration_secs=600 session_pct=80 session_at_recovery=82 near_limit=false
ts=2026-07-11T01:00:00Z event=blind_window acct=u-F duration_secs=200 session_pct=30 session_at_recovery=31 near_limit=false rate=0.010000 inflation=1.75 ceiling=95.00
ts=2026-07-11T01:10:00Z event=blind_window acct=u-G duration_secs=300 session_pct=30 session_at_recovery=31 near_limit=false rate=0.010000 inflation=1.75 ceiling=95.00
";

    #[test]
    fn blind_projection_error_recomputes_the_forecast_from_the_logged_ingredients() {
        let r = aggregate(&parse_events(BLIND_PROJECTION_LOG, None), &[], None);
        let e = &r.blind_projection_error;
        // Sorted errors [−4.25, +0.50, +13.20], n=3 — nearest rank: P50=ceil(.5·3)=2 → +0.50,
        // P95=ceil(.95·3)=3 → +13.20, P100 → +13.20.
        assert_eq!(e.p50, Some(0.5));
        assert_eq!(e.p95, Some(13.2));
        assert_eq!(e.p100, Some(13.2));
    }

    #[test]
    fn blind_projection_error_applies_the_stale_low_mark_to_the_anchor() {
        // Issue #670: since #632 the live arm projects off the #619 plausibility-corrected base, and
        // the daemon stamps the frozen high-water mark (`session_high_water_pct`) beside the RAW
        // anchor precisely when that anchor was stale-low. The recompute must apply the SAME
        // correction: `max(30, 62) + 0.010 × 1.75 × 600 = 72.50` vs 70 ⇒ **+2.50** — the corrected
        // arm's own forecast. Off the raw base it would read `30 + 10.50 = 40.50` vs 70 ⇒ −29.50,
        // grading a projection the live arm no longer makes and reporting a phantom under-projection
        // — the "offline reads sicker than the arm decided" faithfulness gap #670 closes.
        let r = aggregate(
            &parse_events(
                "ts=2026-07-11T00:10:00Z event=blind_window acct=u-A duration_secs=600 session_pct=30 session_at_recovery=70 near_limit=false rate=0.010000 inflation=1.75 ceiling=95.00 session_high_water_pct=62\n",
                None,
            ),
            &[],
            None,
        );
        let e = &r.blind_projection_error;
        assert_eq!(e.n_reconcilable, 1);
        assert_eq!(
            e.p100,
            Some(2.5),
            "the forecast must be graded off the mark-corrected base, not the raw stale-low anchor"
        );
    }

    #[test]
    fn blind_projection_error_never_lowers_the_anchor_from_the_mark() {
        // The daemon stamps the mark ONLY when it exceeds the raw anchor fraction, but the reader
        // applies `max()` — the same shape as `swap::plausible_anchor_session` — so a line whose
        // mark sits AT the anchor (reachable today: `u8` rounding of a sub-percent raise renders a
        // tie) or BELOW it (hand-crafted) cannot DRAG the base down: the raw anchor stands and
        // `30 + 10.50 = 40.50` vs 40 ⇒ +0.50, exactly as if the token were absent. The mark is a
        // one-sided floor, never a substitute reading.
        for mark in ["30", "10"] {
            let line = format!(
                "ts=2026-07-11T00:10:00Z event=blind_window acct=u-A duration_secs=600 session_pct=30 session_at_recovery=40 near_limit=false rate=0.010000 inflation=1.75 ceiling=95.00 session_high_water_pct={mark}\n"
            );
            let r = aggregate(&parse_events(&line, None), &[], None);
            assert_eq!(
                r.blind_projection_error.p100,
                Some(0.5),
                "an at/below-anchor mark must be a no-op (mark={mark})"
            );
        }
    }

    #[test]
    fn blind_projection_error_publishes_cardinality_and_censoring_beside_the_percentiles() {
        let r = aggregate(&parse_events(BLIND_PROJECTION_LOG, None), &[], None);
        let e = &r.blind_projection_error;
        // The mandatory survivorship pairing (issue #484). Every count is published, and BOTH
        // partitions close exactly — no `blind_window` line can go undisclosed between parse and
        // render, because an undisclosed drop IS a missing denominator.
        assert_eq!(e.n_blind_windows, 7);
        assert_eq!(e.n_projectable, 4);
        assert_eq!(e.n_reconcilable, 3);
        assert_eq!(e.n_sentinel_excluded, 1);
        assert_eq!(e.n_below_arm_gate, 2);
        assert_eq!(e.n_without_velocity, 1);
        assert_eq!(e.n_malformed, 0);
        assert_eq!(e.n_projectable, e.n_reconcilable + e.n_sentinel_excluded);
        assert_eq!(
            e.n_blind_windows,
            e.n_projectable + e.n_below_arm_gate + e.n_without_velocity + e.n_malformed,
            "every blind_window line must land in exactly one disclosed bucket"
        );
        // The two censored tails are UNOBSERVABLE from `blind_window` (recovery-edge + active-scoped),
        // so they read `None` — never a fabricated `0`, which would assert this recovered-only
        // population is the whole blind story. Issue #591's uncensored denominator fills them in.
        assert_eq!(e.n_swapped_away, None);
        assert_eq!(e.n_never_recovered, None);
    }

    #[test]
    fn blind_projection_error_scores_only_the_arms_own_domain() {
        // The arm checks `blind_secs > BLIND_GATE_SECS` FIRST and returns before projecting anything,
        // but the daemon stamps the #634 ingredients regardless of duration — so a reader that scores
        // every ingredient-bearing line grades the arm on windows it never evaluated. On the live log
        // that is ~80 % of them, and their errors are dominated by anchor staleness rather than the
        // inflation factor, so admitting them drags P50 toward zero and reads as "1.75 is well
        // calibrated". u-F/u-G would each contribute `30 + 0.01×1.75×200 = 30.35` vs 31 ⇒ −0.65 and
        // `30 + 0.01×1.75×300 = 30.525` vs 31 ⇒ −0.475; admitted, the five-sample P50 becomes −0.475
        // instead of the in-domain +0.50 — the tuning verdict inverts on a population the arm never
        // touched. These assertions fail the moment the domain gate is dropped.
        let r = aggregate(&parse_events(BLIND_PROJECTION_LOG, None), &[], None);
        let e = &r.blind_projection_error;
        assert_eq!(e.n_below_arm_gate, 2, "u-F (200s) and u-G (exactly 300s)");
        assert_eq!(
            e.n_reconcilable, 3,
            "the below-gate pair must not be scored"
        );
        assert_eq!(e.p50, Some(0.5), "P50 must be the in-domain median");
    }

    #[test]
    fn blind_projection_error_gate_boundary_is_exclusive_like_the_arms_own() {
        // `blind_velocity_projected_armed` bails on `blind_secs <= BLIND_GATE_SECS`, so a window of
        // EXACTLY `T` is outside the domain and one of `T + 1` is inside. Pinned against the shared
        // constant rather than a literal, so a future `T` move cannot silently desynchronize the
        // offline grader from the runtime arm it grades.
        let line = |secs: u64| {
            format!(
                "ts=2026-07-11T00:10:00Z event=blind_window acct=u-A duration_secs={secs} session_pct=30 session_at_recovery=31 near_limit=false rate=0.010000 inflation=1.75 ceiling=95.00\n"
            )
        };
        let at_gate = parse_events(&line(crate::daemon::BLIND_GATE_SECS), None);
        assert_eq!(at_gate.blind_window_census.below_arm_gate, 1);
        assert!(at_gate.blind_projections.is_empty());

        let past_gate = parse_events(&line(crate::daemon::BLIND_GATE_SECS + 1), None);
        assert_eq!(past_gate.blind_window_census.below_arm_gate, 0);
        assert_eq!(past_gate.blind_projections.len(), 1);
    }

    #[test]
    fn blind_projection_error_excludes_the_session_reset_sentinel() {
        let r = aggregate(&parse_events(BLIND_PROJECTION_LOG, None), &[], None);
        let e = &r.blind_projection_error;
        // u-D's `session_at_recovery=0` is the session-window RESET, not an arrival at 0: admitting it
        // would score a `93.50 − 0 = +93.50` phantom, which as the max would become BOTH P95 and P100
        // and bury the real single-digit-pp spread. These two assertions are the ones that fail if the
        // exclusion is ever dropped.
        assert_eq!(e.n_sentinel_excluded, 1);
        assert_eq!(e.p100, Some(13.2), "the sentinel must not become the max");
    }

    #[test]
    fn blind_projection_error_is_scored_outside_the_near_limit_gate() {
        // Every scored line above is `near_limit=false`, so the two near-limit SLIs see nothing at all
        // while the projection error sees four windows. This pins that the fold runs BEFORE the
        // near-limit gate: were it inside, the climbing population (#584's whole point) would vanish.
        let inputs = parse_events(BLIND_PROJECTION_LOG, None);
        assert_eq!(inputs.time_blind_near_limit_secs, 0);
        assert!(inputs.near_limit_reconciliations.is_empty());
        assert_eq!(inputs.blind_projections.len(), 4);
        assert_eq!(inputs.blind_window_census.total, 7);
    }

    #[test]
    fn blind_projection_error_is_none_on_an_empty_population() {
        // Cardinality-zero discipline: no reconcilable window ⇒ `None`, never a passing `0 pp` (which
        // would read as a perfectly-calibrated `1.75`). Checked on BOTH empty shapes — a log with no
        // blind windows at all, and one whose only projectable window is the excluded sentinel.
        let empty = aggregate(&parse_events("", None), &[], None);
        assert_eq!(empty.blind_projection_error.n_blind_windows, 0);
        assert_eq!(empty.blind_projection_error.n_projectable, 0);
        assert_eq!(empty.blind_projection_error.n_reconcilable, 0);
        assert_eq!(empty.blind_projection_error.p50, None);
        assert_eq!(empty.blind_projection_error.p95, None);
        assert_eq!(empty.blind_projection_error.p100, None);

        let sentinel_only = aggregate(
            &parse_events(
                "ts=2026-07-11T00:40:00Z event=blind_window acct=u-D duration_secs=1200 session_pct=62 session_at_recovery=0 near_limit=false rate=0.015000 inflation=1.75 ceiling=95.00\n",
                None,
            ),
            &[],
            None,
        );
        assert_eq!(sentinel_only.blind_projection_error.n_projectable, 1);
        assert_eq!(sentinel_only.blind_projection_error.n_sentinel_excluded, 1);
        assert_eq!(sentinel_only.blind_projection_error.n_reconcilable, 0);
        assert_eq!(sentinel_only.blind_projection_error.p100, None);
    }

    #[test]
    fn blind_projection_error_classifies_corruption_apart_from_coverage() {
        // A CORRUPT record is not evidence that "the arm could not have armed" — folding the two
        // together would report corruption as coverage, and dropping it from both counters would
        // leave a silent hole in the denominator (the survivorship failure this block exists to
        // prevent: 40 truncated lines would render as a clean full-coverage readout over 60 % of the
        // data). Six corruption shapes, each landing in `malformed` and none in `without_velocity` —
        // the sixth (issue #670) a PRESENT-but-unreadable `session_high_water_pct`, which is part of
        // the projection-reconstruction contract exactly like `rate` / `inflation`: silently
        // reverting it to the stale-low base would misreport the arm, so it drops as corruption.
        // The seventh line pins the boundary of that contract: with NO `rate=` the arm's second gate
        // classifies the line `without_velocity` BEFORE the mark is ever parsed (the census applies
        // the arm's own gates in the arm's own order), so a garbage mark on a rate-less line is
        // unconsumed coverage context — never corruption of a projection that was never recomputed.
        let inputs = parse_events(
            "\
ts=2026-07-11T00:10:00Z event=blind_window acct=u-A duration_secs=600 session_pct=30 session_at_recovery=40 near_limit=false rate=oops inflation=1.75 ceiling=95.00
ts=2026-07-11T00:20:00Z event=blind_window acct=u-B duration_secs=600 session_pct=30 session_at_recovery=40 near_limit=false rate=NaN inflation=1.75 ceiling=95.00
ts=2026-07-11T00:30:00Z event=blind_window acct=u-C duration_secs=600 session_pct=30 near_limit=false rate=0.010000 inflation=1.75 ceiling=95.00
ts=2026-07-11T00:40:00Z event=blind_window acct=u-D near_limit=false
ts=2026-07-11T00:50:00Z event=blind_window acct=u-E duration_secs=600 session_pct=30 session_at_recovery=40 near_limit=false rate=1e300 inflation=1e300 ceiling=95.00
ts=2026-07-11T01:00:00Z event=blind_window acct=u-F duration_secs=600 session_pct=30 session_at_recovery=40 near_limit=false rate=0.010000 inflation=1.75 ceiling=95.00 session_high_water_pct=oops
ts=2026-07-11T01:10:00Z event=blind_window acct=u-G duration_secs=600 session_pct=30 session_at_recovery=40 near_limit=false session_high_water_pct=oops
",
            None,
        );
        assert!(inputs.blind_projections.is_empty());
        assert_eq!(inputs.blind_window_census.total, 7);
        assert_eq!(inputs.blind_window_census.malformed, 6);
        assert_eq!(
            inputs.blind_window_census.without_velocity, 1,
            "a rate-less line is coverage context whatever its mark says — gate order"
        );
        assert_eq!(inputs.blind_window_census.below_arm_gate, 0);
    }

    #[test]
    fn blind_projection_error_never_publishes_a_non_finite_percentile() {
        // `rate=1e300 inflation=1e300` each pass an individual `is_finite()` check, but their PRODUCT
        // overflows: the projection becomes `inf` (and with a zero duration, `inf × 0 = NaN`). Left
        // unguarded, the human text would print `P100 = +inf pp` while `--json` printed `"p100":
        // null` — and this schema defines `null` as "empty population", so a machine consumer would
        // read cardinality-1-with-no-data instead of a corrupt record. The two renderers must never
        // disagree about the same episode; the guard is on the RESULT, not the inputs.
        for line in [
            "ts=2026-07-11T00:10:00Z event=blind_window acct=u-A duration_secs=600 session_pct=30 session_at_recovery=40 near_limit=false rate=1e300 inflation=1e300 ceiling=95.00\n",
            "ts=2026-07-11T00:10:00Z event=blind_window acct=u-A duration_secs=600 session_pct=30 session_at_recovery=40 near_limit=false rate=1e300 inflation=-1e300 ceiling=95.00\n",
        ] {
            let r = aggregate(&parse_events(line, None), &[], None);
            let e = &r.blind_projection_error;
            assert_eq!(e.n_malformed, 1, "overflow is corruption: {line}");
            assert_eq!(e.n_reconcilable, 0);
            assert_eq!(e.p100, None);
            let human = render_human(&r);
            assert!(!human.contains("inf"), "no inf in human text: {human}");
            assert!(!human.contains("NaN"), "no NaN in human text: {human}");
            // The wire's `null` must mean what the schema says it means — empty population — so a
            // `null` percentile can never sit beside a non-zero reconcilable count.
            let json = render_json(&r).expect("wire serializes");
            assert!(json.contains("\"n_reconcilable\": 0,"), "{json}");
        }
    }

    #[test]
    fn blind_projection_error_render_pairs_percentiles_with_their_censoring() {
        let r = aggregate(&parse_events(BLIND_PROJECTION_LOG, None), &[], None);
        let human = render_human(&r);
        assert!(
            human.contains(
                "  reconcilable n=3 of 4 projectable (1 excluded: session_at_recovery=0 window-reset sentinel), from 7 blind windows\n"
            ),
            "percentiles must never be published bare: {human}"
        );
        assert!(
            human.contains(
                "  outside the arm's domain: 2 below the T=300s gate; 1 with no retained velocity; 0 malformed\n"
            ),
            "the out-of-domain split must be published beside the percentiles: {human}"
        );
        assert!(
            human.contains("  censoring: RECOVERED-ONLY"),
            "the survivorship disclosure is mandatory: {human}"
        );
        // Signed rendering: the direction of the error IS the tuning signal, so a negative P50 must
        // stay visibly negative and a positive one visibly positive.
        assert!(human.contains("  P50  = +0.50 pp\n"), "{human}");
        assert!(human.contains("  P100 = +13.20 pp\n"), "{human}");

        let json = render_json(&r).expect("wire serializes");
        assert!(json.contains("\"n_blind_windows\": 7,"), "{json}");
        assert!(json.contains("\"n_reconcilable\": 3,"), "{json}");
        assert!(json.contains("\"n_sentinel_excluded\": 1,"), "{json}");
        assert!(json.contains("\"n_below_arm_gate\": 2,"), "{json}");
        assert!(json.contains("\"n_without_velocity\": 1,"), "{json}");
        assert!(json.contains("\"n_malformed\": 0,"), "{json}");
        assert!(json.contains("\"arm_gate_secs\": 300,"), "{json}");
        assert!(json.contains("\"n_swapped_away\": null,"), "{json}");
        assert!(json.contains("\"n_never_recovered\": null,"), "{json}");
        assert!(json.contains("\"p50\": 0.5,"), "{json}");
        assert!(json.contains("\"p100\": 13.2"), "{json}");
    }

    #[test]
    fn blind_projection_error_renders_an_under_projection_with_its_sign() {
        // A distribution whose worst case is an UNDER-projection: the account burned 6 pp past the
        // inflated forecast. `60 + 0.005 × 1.75 × 600 = 65.25` vs 71 ⇒ −5.75. Rendering this as
        // `5.75` would invert the tuning verdict — 1.75 reads too HIGH when it is too LOW.
        let r = aggregate(
            &parse_events(
                "ts=2026-07-11T00:10:00Z event=blind_window acct=u-A duration_secs=600 session_pct=60 session_at_recovery=71 near_limit=false rate=0.005000 inflation=1.75 ceiling=95.00\n",
                None,
            ),
            &[],
            None,
        );
        assert_eq!(r.blind_projection_error.p100, Some(-5.75));
        assert!(
            render_human(&r).contains("  P100 = -5.75 pp\n"),
            "an under-projection must keep its sign"
        );
    }

    #[test]
    fn blind_projection_error_normalizes_negative_zero_to_a_positive_display() {
        // A spot-on projection that rounds to zero FROM BELOW: `40 + 0.000950 × 1.75 × 600 = 40.9975`
        // vs 41 ⇒ −0.0025, which `f64::round` sends to IEEE −0.0. Without `round_pp`'s `+ 0.0`
        // normalization that renders as the confusing `-0.00 pp`. `Some(0.0) == Some(-0.0)` is true in
        // IEEE, so a percentile equality check CANNOT catch a regression here (removing `+ 0.0` leaves
        // every other test green — mutation-verified) — this locks it on the sign bit and the rendered
        // bytes instead.
        assert!(
            !round_pp(-0.0025).is_sign_negative(),
            "round_pp must normalize a rounds-to-zero-from-below error to +0.0, not -0.0"
        );
        let r = aggregate(
            &parse_events(
                "ts=2026-07-11T00:10:00Z event=blind_window acct=u-A duration_secs=600 session_pct=40 session_at_recovery=41 near_limit=false rate=0.000950 inflation=1.75 ceiling=95.00\n",
                None,
            ),
            &[],
            None,
        );
        assert!(
            !r.blind_projection_error.p100.unwrap().is_sign_negative(),
            "a near-zero error must not carry a negative sign into the report"
        );
        let human = render_human(&r);
        assert!(human.contains("  P100 = +0.00 pp\n"), "{human}");
        assert!(
            !human.contains("-0.00"),
            "no negative zero in the display: {human}"
        );
    }

    #[test]
    fn blind_projection_error_reads_the_stamped_inflation_not_a_todays_constant() {
        // #634 stamps `inflation=` per line precisely so an OLD window is never re-read through a NEW
        // factor. Two identical windows differing only in the stamped factor must therefore score
        // differently: `30 + 0.01 × 1.00 × 600 = 36` vs 40 ⇒ −4.00, against u-A's 1.75 ⇒ +0.50.
        let r = aggregate(
            &parse_events(
                "ts=2026-07-11T00:10:00Z event=blind_window acct=u-A duration_secs=600 session_pct=30 session_at_recovery=40 near_limit=false rate=0.010000 inflation=1.00 ceiling=95.00\n",
                None,
            ),
            &[],
            None,
        );
        assert_eq!(r.blind_projection_error.p100, Some(-4.0));
    }

    #[test]
    fn blind_projection_error_is_bounded_by_the_active_window() {
        // `--since` bounds this SLI like every other: the cutoff drops u-A/u-B, leaving u-C (+13.20),
        // the sentinel, and the no-velocity line. A single remaining sample is its own P50/P95/P100.
        let cutoff = epoch("2026-07-11T00:25:00Z");
        let r = aggregate(&parse_events(BLIND_PROJECTION_LOG, Some(cutoff)), &[], None);
        let e = &r.blind_projection_error;
        assert_eq!(e.n_blind_windows, 5, "u-A and u-B fall before the cutoff");
        assert_eq!(e.n_projectable, 2);
        assert_eq!(e.n_reconcilable, 1);
        assert_eq!(e.n_sentinel_excluded, 1);
        assert_eq!(e.n_below_arm_gate, 2);
        assert_eq!(e.n_without_velocity, 1);
        assert_eq!(e.p50, Some(13.2));
        assert_eq!(e.p100, Some(13.2));
    }

    // --- issue #494: the `--since` window --------------------------------------

    /// A window fixture spanning two clusters days apart, exercising ALL FOUR event families on
    /// BOTH sides of a mid-fixture cutoff — so a window is provably bounding EVERY SLI, not just
    /// the swap percentiles. The Jul-5 swap sits exactly on the boundary the tests key off.
    const WINDOW_LOG: &str = "\
ts=2026-07-01T00:00:00Z event=swap from=a to=b reason=session session_pct=91
ts=2026-07-01T01:00:00Z event=blind_window acct=u-A duration_secs=100 session_pct=98 session_at_recovery=50 near_limit=true
ts=2026-07-01T02:00:00Z event=usage_backoff acct=u-A class=rate_limited
ts=2026-07-05T00:00:00Z event=swap from=a to=b reason=session session_pct=96
ts=2026-07-10T00:00:00Z event=swap from=a to=b reason=session session_pct=98
ts=2026-07-10T01:00:00Z event=blind_window acct=u-B duration_secs=200 session_pct=97 session_at_recovery=60 near_limit=true
ts=2026-07-10T02:00:00Z event=usage_backoff acct=u-B class=transient
";

    /// Parse a fixture `ts=` through the SAME canonical reader the production window path uses,
    /// so a test cutoff is derived exactly as `parse_events` derives each line's instant.
    fn epoch(ts: &str) -> i64 {
        epoch_from_rfc3339(ts).expect("valid RFC 3339 fixture")
    }

    #[test]
    fn parse_duration_secs_accepts_each_unit() {
        assert_eq!(parse_duration_secs("45s").unwrap(), 45);
        assert_eq!(parse_duration_secs("30m").unwrap(), 1_800);
        assert_eq!(parse_duration_secs("24h").unwrap(), 86_400);
        assert_eq!(parse_duration_secs("7d").unwrap(), 604_800);
        assert_eq!(parse_duration_secs("2w").unwrap(), 1_209_600);
        assert_eq!(parse_duration_secs("0d").unwrap(), 0);
        // Surrounding whitespace is trimmed (lexopt hands the value through verbatim).
        assert_eq!(parse_duration_secs("  7d  ").unwrap(), 604_800);
        // Saturating multiply: a u64-representable count whose ×unit overflows yields u64::MAX
        // (→ a clamped cutoff), never a wrapped value.
        assert_eq!(
            parse_duration_secs(&format!("{}w", u64::MAX)).unwrap(),
            u64::MAX
        );
    }

    #[test]
    fn parse_duration_secs_rejects_malformed() {
        for bad in [
            "",                      // empty
            "   ",                   // whitespace only (trims to empty)
            "7",                     // no unit
            "d",                     // no count
            "7x",                    // unknown unit
            "-3d",                   // negative sign
            "3.5h",                  // non-integer
            "abc",                   // gibberish
            "7dd",                   // trailing junk after the unit
            "d7",                    // unit before count
            "7 d",                   // internal whitespace
            "99999999999999999999s", // count overflows u64 → rejected, not silently saturated
        ] {
            let err = parse_duration_secs(bad).unwrap_err();
            assert!(
                matches!(err, Error::ReliabilitySinceInvalid(_)),
                "{bad:?} must be rejected as ReliabilitySinceInvalid, got {err:?}"
            );
        }
    }

    #[test]
    fn window_resolve_computes_and_clamps_the_cutoff() {
        let now = epoch("2026-07-12T00:00:00Z");
        // A normal span: cutoff = now − duration.
        let w = Window::resolve("7d", now).expect("valid duration");
        assert_eq!(w.since_arg, "7d");
        assert_eq!(w.cutoff_epoch, now - 604_800);
        assert_eq!(w.cutoff_epoch, epoch("2026-07-05T00:00:00Z"));
        // A span reaching before the epoch clamps to 0 ("whole log"), never negative.
        let w = Window::resolve("999999w", 1_000).expect("valid duration");
        assert_eq!(w.cutoff_epoch, 0);
        // A malformed span surfaces the error rather than a window.
        assert!(matches!(
            Window::resolve("nope", now),
            Err(Error::ReliabilitySinceInvalid(_))
        ));
    }

    #[test]
    fn window_bounds_all_four_slis_to_events_at_or_after_the_cutoff() {
        let cutoff = epoch("2026-07-05T00:00:00Z");
        let inputs = parse_events(WINDOW_LOG, Some(cutoff));
        // Swaps: 91 (Jul 1) dropped; 96 (Jul 5, == cutoff) and 98 (Jul 10) kept.
        assert_eq!(inputs.swap_out_pcts, vec![96.0, 98.0]);
        // Blind: the Jul 1 window (100s) dropped; only the Jul 10 window (200s) remains.
        assert_eq!(inputs.time_blind_near_limit_secs, 200);
        assert_eq!(inputs.near_limit_reconciliations, vec![(97, 60)]);
        // 429 neutrality: the Jul 1 rate_limited dropped; the Jul 10 transient kept.
        assert_eq!(inputs.rate_limited, 0);
        assert_eq!(inputs.transient, 1);
    }

    #[test]
    fn window_boundary_is_inclusive_at_exactly_the_cutoff() {
        // Exactly AT the cutoff → in the window (the bound is at/after: half-open [cutoff, ∞)).
        let at = epoch("2026-07-05T00:00:00Z");
        assert_eq!(
            parse_events(WINDOW_LOG, Some(at)).swap_out_pcts,
            vec![96.0, 98.0],
            "an event whose ts == cutoff is at/after the cutoff, so it is included"
        );
        // One second later, the Jul-5 swap now falls just before the cutoff and drops out.
        assert_eq!(
            parse_events(WINDOW_LOG, Some(at + 1)).swap_out_pcts,
            vec![98.0],
            "one second past the Jul-5 instant excludes it — the boundary is exclusive-below"
        );
    }

    #[test]
    fn window_drops_a_line_it_cannot_timestamp() {
        // A line with no `ts=` and one with an unparseable `ts=` cannot be placed in time, so a
        // windowed pass drops both — while the whole-log default still folds them.
        let log = "\
event=swap from=a to=b reason=session session_pct=95
ts=not-a-timestamp event=swap from=a to=b reason=session session_pct=96
ts=2026-07-10T00:00:00Z event=swap from=a to=b reason=session session_pct=97
";
        let cutoff = epoch("2026-07-01T00:00:00Z");
        assert_eq!(
            parse_events(log, Some(cutoff)).swap_out_pcts,
            vec![97.0],
            "un-timestamped / unparseable-ts lines are not provably in-window ⇒ dropped"
        );
        // Whole-log default is unaffected: every reason=session swap folds in regardless of ts.
        assert_eq!(
            parse_events(log, None).swap_out_pcts,
            vec![95.0, 96.0, 97.0]
        );
    }

    #[test]
    fn default_none_matches_the_whole_log_and_a_wide_window() {
        // No window folds every line…
        let whole = parse_events(WINDOW_LOG, None);
        assert_eq!(whole.swap_out_pcts, vec![91.0, 96.0, 98.0]);
        assert_eq!(whole.time_blind_near_limit_secs, 300);
        assert_eq!(whole.rate_limited, 1);
        assert_eq!(whole.transient, 1);
        // …and a cutoff at epoch 0 admits every real (post-1970) line — identical to None.
        assert_eq!(parse_events(WINDOW_LOG, Some(0)), whole);
    }

    #[test]
    fn cardinality_zero_within_the_window_is_honest_not_a_fabricated_pass() {
        // A window AFTER every swap: the windowed subset has no swaps at all. Percentiles must
        // stay None (no target asserted met), the human line reads "no swaps observed", the JSON
        // serializes nulls — the #455 degenerate-subject discipline, now on the windowed subset.
        let window = Window::resolve("1s", epoch("2026-07-11T00:00:00Z") + 1).unwrap();
        assert_eq!(window.cutoff_epoch, epoch("2026-07-11T00:00:00Z")); // after the Jul-10 swaps
        let inputs = parse_events(WINDOW_LOG, Some(window.cutoff_epoch));
        let r = aggregate(&inputs, &[], Some(window));
        assert_eq!(r.swap_overshoot.n, 0);
        assert_eq!(r.swap_overshoot.p50, None);
        assert_eq!(r.swap_overshoot.p100, None);
        assert_eq!(r.swap_overshoot.p50_met(), None);
        assert_eq!(r.swap_overshoot.p100_met(), None);

        let human = render_human(&r);
        assert!(
            human.contains("swap-out session_pct (reason=session): no swaps observed"),
            "windowed cardinality-zero must not fabricate a percentile: {human}"
        );
        let json = render_json(&r).expect("serializes");
        assert!(
            json.contains("\"p100\": null"),
            "windowed no-data P100 must be null: {json}"
        );
        assert!(json.contains("\"met\": {\n      \"p50\": null,\n      \"p100\": null\n    }"));
    }

    #[test]
    fn human_documents_the_active_window() {
        let window = Window::resolve("7d", epoch("2026-07-12T00:00:00Z")).unwrap();
        let inputs = parse_events(WINDOW_LOG, Some(window.cutoff_epoch));
        let out = render_human(&aggregate(&inputs, &[], Some(window)));
        assert!(
            out.contains(
                "window: since 2026-07-05T00:00:00Z (7d) — all SLIs bounded to events at/after the cutoff"
            ),
            "human output must document the window bound: {out}"
        );
        // The whole-log default emits NO such line (default output is unchanged).
        assert!(!render_human(&fixture_report()).contains("window: since"));
    }

    #[test]
    fn json_documents_the_active_window() {
        let window = Window::resolve("7d", epoch("2026-07-12T00:00:00Z")).unwrap();
        let cutoff = window.cutoff_epoch;
        let out = render_json(&aggregate(
            &parse_events(WINDOW_LOG, Some(cutoff)),
            &[],
            Some(window),
        ))
        .expect("serializes");
        assert!(out.contains("\"schema\": 7,"), "schema bumped to 7: {out}");
        assert!(
            out.contains(concat!(
                "  \"window\": {\n",
                "    \"since\": \"7d\",\n",
                "    \"cutoff_ts\": \"2026-07-05T00:00:00Z\",\n",
            )),
            "json window block documents since + cutoff_ts: {out}"
        );
        assert!(
            out.contains(&format!("\"cutoff_epoch\": {cutoff}")),
            "json window carries the epoch cutoff: {out}"
        );
    }

    /// The #15 durable-line guarantee, extended to the readout: neither the human nor the JSON
    /// output may carry an email, token sigil, or the free-form operator `label` — the readout
    /// is roster-wide numbers only, secret-free BY CONSTRUCTION, but assert it.
    #[test]
    fn readout_carries_no_pii() {
        // Non-degeneracy guard: the fixture MUST carry an email in its swap `from=`/`to=` (as the
        // production log does), else the email assertion below would pass vacuously and prove nothing.
        assert!(
            !crate::redaction::meter::unauthored_emails(FIXTURE_LOG, &[]).is_empty(),
            "fixture must contain an email so the leak guard is a real regression catch"
        );
        // Cover BOTH output paths: the whole-log default AND a windowed readout (#494), the
        // latter built over the same email-bearing fixture so the window line + JSON `window`
        // block are exercised on real swap data — the window metadata (a duration + a bare
        // cutoff instant) must itself stay secret-free. Samples carry the SAME email roster
        // labels as the swap `from=` (as production does), so the #595 landing join runs over
        // email-bearing data — the leak guard then genuinely covers the landing path, not just SLI 1.
        let samples = [
            // Two readings of the account parked by the 00:00 reason=session swap (from=…pelykh.com),
            // inside its window and before its 00:05 re-activation, so a landing episode is measured.
            sample(epoch("2026-07-11T00:02:00Z"), "oleksii@pelykh.com", 0.99),
            sample(epoch("2026-07-11T00:03:00Z"), "oleksii@pelykh.com", 1.00),
        ];
        let whole = aggregate(&parse_events(FIXTURE_LOG, None), &samples, None);
        let windowed = aggregate(
            &parse_events(FIXTURE_LOG, Some(epoch("2026-07-11T00:00:00Z"))),
            &samples,
            Some(Window::resolve("30m", epoch("2026-07-11T00:30:00Z")).unwrap()),
        );
        // Non-degeneracy: the window must retain the fixture's swaps, else the windowed render
        // is empty and its leak guard proves nothing.
        assert!(
            windowed.swap_overshoot.n > 0,
            "windowed report must fold the fixture swaps"
        );
        // Non-degeneracy for the #595 landing path: the email-acct join must produce a measured
        // episode, else the landing render is empty and its leak guard proves nothing.
        assert!(
            whole.landing.n_measured > 0,
            "landing join must fold the email-acct samples so the leak guard is a real catch"
        );
        for r in [&whole, &windowed] {
            for out in [render_human(r), render_json(r).expect("serializes")] {
                assert!(
                    crate::redaction::meter::unauthored_emails(out.as_str(), &[]).is_empty(),
                    "no non-authored email may appear (#15): {out}"
                );
                assert!(!out.contains("token"), "no token may appear: {out}");
                assert!(!out.contains("Bearer"), "no bearer may appear: {out}");
                assert!(!out.contains("sk-ant"), "no api key may appear: {out}");
                assert!(!out.contains("label="), "no operator label: {out}");
                assert!(!out.contains("acct="), "no account uuid: {out}");
            }
        }
    }

    // --- issue #595: the landing-point SLI ------------------------------------

    #[test]
    fn parse_collects_session_swap_anchors_and_reactivation_edges() {
        // The landing reconstruction needs two new extractions from the swap stream: reason=session
        // ANCHORS (ts + outgoing acct + decision pct) and re-activation EDGES (every swap's `to=`).
        let inputs = parse_events(FIXTURE_LOG, None);
        // Anchors: only the two reason=session swaps (96 @ 00:00, 100 @ 00:06); weekly/manual excluded.
        assert_eq!(
            inputs.session_swaps,
            vec![
                SwapOut {
                    ts: epoch("2026-07-11T00:00:00Z"),
                    acct: "oleksii@pelykh.com".to_owned(),
                    decision_pct: 96,
                },
                SwapOut {
                    ts: epoch("2026-07-11T00:06:00Z"),
                    acct: "oleksii@pelykh.com".to_owned(),
                    decision_pct: 100,
                },
            ]
        );
        // Edges: every event=swap `to=` (ANY reason), the event=emergency_swap `to=`, the
        // event=restash `account=`, AND the event=canonical_recovered `account=` — all four move the
        // active account onto that label, so all four re-activate their target (issue #595 AC2). The
        // restash and canonical_recovered carry `account=`, not `to=`.
        assert_eq!(
            inputs.reactivations,
            vec![
                Reactivation {
                    ts: epoch("2026-07-11T00:00:00Z"),
                    acct: "oleksii@pelykhconsulting.fr".to_owned(),
                },
                Reactivation {
                    ts: epoch("2026-07-11T00:05:00Z"),
                    acct: "oleksii@pelykh.com".to_owned(),
                },
                Reactivation {
                    ts: epoch("2026-07-11T00:06:00Z"),
                    acct: "oleksii@pelykhconsulting.fr".to_owned(),
                },
                Reactivation {
                    ts: epoch("2026-07-11T00:07:00Z"),
                    acct: "oleksii@pelykhconsulting.fr".to_owned(),
                },
                // The 00:08 emergency_swap `to=` — a re-activation edge too (regression: issue #595).
                Reactivation {
                    ts: epoch("2026-07-11T00:08:00Z"),
                    acct: "oleksii@pelykhconsulting.fr".to_owned(),
                },
                // The 00:09 restash `account=` — the out-of-band `claude /login` re-activation, keyed
                // off `account=` not `to=` (regression: issue #595).
                Reactivation {
                    ts: epoch("2026-07-11T00:09:00Z"),
                    acct: "oleksii@pelykh.com".to_owned(),
                },
                // The 00:09:30 canonical_recovered `account=` — the scrub-adopt recovery re-activation,
                // the fourth revival door, also keyed off `account=` (regression: issue #595).
                Reactivation {
                    ts: epoch("2026-07-11T00:09:30Z"),
                    acct: "oleksii@pelykhconsulting.fr".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn landing_reconstructs_the_post_swap_peak() {
        // A reason=session swap fires ON TARGET at 96; the parked account then climbs to 100 within
        // the window. SLI 1 sees only the 96 decision; the landing SLI catches the 100 it reached.
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=session session_pct=96
";
        let samples = [
            sample(epoch("2026-07-11T00:01:00Z"), "work", 0.97),
            sample(epoch("2026-07-11T00:05:00Z"), "work", 1.00),
            sample(epoch("2026-07-11T00:10:00Z"), "work", 0.98), // past the peak; peak stays 100
            sample(epoch("2026-07-11T00:03:00Z"), "spare", 0.50), // the INCOMING account — never joined
        ];
        let r = aggregate(&parse_events(log, None), &samples, None);
        assert_eq!(r.landing.swaps_total, 1);
        assert_eq!(r.landing.n_measured, 1);
        assert_eq!(r.landing.n_unmeasured, 0);
        assert_eq!(r.landing.p100, Some(100));
        assert_eq!(r.landing.p50, Some(100)); // n=1 → every percentile is the one peak
                                              // Fired at 96 (< 99) but landed at 100 (>= 99): the invisible post-swap committed tail.
        assert_eq!(r.landing.post_swap_tail, 1);
        assert_eq!(r.landing.gap_crossing, 0);
        assert_eq!(r.landing.p100_met(), Some(false));
    }

    #[test]
    fn landing_excludes_samples_after_reactivation() {
        // work is parked at 00:00, then RE-ACTIVATED at 00:04 (a later swap names it `to=`). The 100
        // reading at 00:06 is AFTER re-activation — the account is active again, so it is NOT part of
        // the parked tail (`active_at != acct`). The peak before re-activation is 97.
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=session session_pct=95
ts=2026-07-11T00:04:00Z event=swap from=spare to=work reason=weekly session_pct=40
";
        let samples = [
            sample(epoch("2026-07-11T00:02:00Z"), "work", 0.97), // parked → counts
            sample(epoch("2026-07-11T00:06:00Z"), "work", 1.00), // post-reactivation → excluded
        ];
        let r = aggregate(&parse_events(log, None), &samples, None);
        assert_eq!(r.landing.n_measured, 1);
        assert_eq!(
            r.landing.p100,
            Some(97),
            "peak must exclude the post-reactivation 100 reading"
        );
        assert_eq!(
            r.landing.post_swap_tail, 0,
            "97 landed below the ceiling once the re-activation reading is excluded"
        );
    }

    #[test]
    fn landing_excludes_samples_after_emergency_reactivation() {
        // The re-activation that closes a parked window need not be a normal swap: on a 2-account
        // roster the freshly-active account can DIE, and an event=emergency_swap revives the parked
        // account. A reading AFTER that emergency swap is an ACTIVE reading, NOT the parked tail — so
        // it must be excluded exactly as a normal re-activation would (issue #595 AC2). Regression: an
        // earlier cut collected only `event=swap` edges, so an emergency revival fabricated a breach.
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=session session_pct=95
ts=2026-07-11T00:03:00Z event=emergency_swap from=spare to=work
";
        let samples = [
            sample(epoch("2026-07-11T00:02:00Z"), "work", 0.97), // parked → counts
            sample(epoch("2026-07-11T00:05:00Z"), "work", 1.00), // post-emergency-reactivation → excluded
        ];
        let r = aggregate(&parse_events(log, None), &samples, None);
        assert_eq!(r.landing.n_measured, 1);
        assert_eq!(
            r.landing.p100,
            Some(97),
            "an emergency swap re-activates work; its post-reactivation 100 reading must not fold in"
        );
        assert_eq!(
            r.landing.post_swap_tail, 0,
            "excluding the post-emergency reading, work landed at 97 (< 99) — no fabricated breach"
        );
    }

    #[test]
    fn landing_excludes_samples_after_restash_reactivation() {
        // The third revival door: an operator runs `claude /login` as the JUST-PARKED account. The
        // daemon's canonical watch reconciles that credential onto its roster account and re-resolves
        // it active, emitting event=restash account=work (issue #595 AC2). A reading AFTER the restash
        // is an ACTIVE reading, not the parked tail — it must be excluded exactly as a swap re-activation
        // is. Regression: restash carries `account=`, not `to=`, so an edge-recorder keyed only on `to=`
        // would miss it and fold work's post-relogin climb into a fabricated post-swap-tail breach.
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=session session_pct=95
ts=2026-07-11T00:03:00Z event=restash account=work
";
        let samples = [
            sample(epoch("2026-07-11T00:02:00Z"), "work", 0.97), // parked → counts
            sample(epoch("2026-07-11T00:05:00Z"), "work", 1.00), // post-restash-reactivation → excluded
        ];
        let r = aggregate(&parse_events(log, None), &samples, None);
        assert_eq!(r.landing.n_measured, 1);
        assert_eq!(
            r.landing.p100,
            Some(97),
            "a restash re-activates work; its post-reactivation 100 reading must not fold in"
        );
        assert_eq!(
            r.landing.post_swap_tail, 0,
            "excluding the post-restash reading, work landed at 97 (< 99) — no fabricated breach"
        );
    }

    #[test]
    fn landing_excludes_samples_after_canonical_recovery_reactivation() {
        // The fourth revival door: the shared canonical credential is scrubbed, and the daemon's
        // scrub-adopt recovery RE-ADOPTS the just-parked account to keep the fleet live — its session
        // gate bypassed, so a near-limit parked account is a fully eligible re-adopt target. That calls
        // record_swap (active := work) and emits event=canonical_recovered account=work. A reading
        // AFTER it is an ACTIVE reading, not the parked tail, and must be excluded (issue #595 AC2).
        // Regression: canonical_recovered carries `account=` (like restash), and an edge-recorder that
        // stopped at swap/emergency_swap/restash would fold work's post-recovery climb into a fabricated
        // post-swap-tail breach — and INFLATE p90/p100, the #597 tail-calibration input.
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=session session_pct=95
ts=2026-07-11T00:03:00Z event=canonical_recovered account=work
";
        let samples = [
            sample(epoch("2026-07-11T00:02:00Z"), "work", 0.97), // parked → counts
            sample(epoch("2026-07-11T00:05:00Z"), "work", 1.00), // post-recovery-reactivation → excluded
        ];
        let r = aggregate(&parse_events(log, None), &samples, None);
        assert_eq!(r.landing.n_measured, 1);
        assert_eq!(
            r.landing.p100,
            Some(97),
            "a canonical recovery re-adopts work; its post-reactivation 100 reading must not fold in"
        );
        assert_eq!(
            r.landing.post_swap_tail, 0,
            "excluding the post-recovery reading, work landed at 97 (< 99) — no fabricated breach"
        );
    }

    #[test]
    fn landing_bounded_window_excludes_late_samples() {
        // A 100 reading arrives AFTER the window closes — too late to attribute to this swap's
        // landing (a fresh session cycle by then). The in-window peak is 98.
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=session session_pct=95
";
        let samples = [
            sample(epoch("2026-07-11T00:10:00Z"), "work", 0.98), // 600s in → counts
            sample(epoch("2026-07-11T00:20:00Z"), "work", 1.00), // 1200s in > 900 → excluded
        ];
        let r = aggregate(&parse_events(log, None), &samples, None);
        assert_eq!(r.landing.n_measured, 1);
        assert_eq!(
            r.landing.p100,
            Some(98),
            "a reading past the {LANDING_WINDOW_SECS}s window is not this swap's landing"
        );
    }

    #[test]
    fn landing_boundary_fraction_rounds_consistently_with_the_slo() {
        // Issue #615. The daemon DECIDES in fraction space but this SLO is stated — and compared — in
        // rounded whole percent (`landing_pct >= SLO_SWAP_P100_MAX`, a `u8`). Rounding is therefore
        // part of the SLO's definition, not a display detail, and it is half-away-from-zero
        // (`f64::round`): a landing fraction of 0.985 rounds UP to 99 and is already a breach.
        //
        // So the fraction-space boundary is **0.985, not 0.99** — the rounding widens the breach band
        // by half a percentage point below the nominal ceiling. That is the boundary this test pins,
        // so a future change of rounding mode cannot pass unnoticed: truncation would release the
        // sub-ceiling `[0.985, 0.99)` band back to a compliant 98 and silently stop reporting those
        // breaches. (From 0.99 up the two modes agree, so only the sub-ceiling band discriminates.)
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=session session_pct=95
";
        // One swap anchor and one in-window post-swap reading of the parked account, so the ONLY
        // thing varying across the fractions below is the rounding under test.
        let events = parse_events(log, None);
        let landed_at = epoch("2026-07-11T00:05:00Z");
        let landing_at =
            |session: f64| aggregate(&events, &[sample(landed_at, "work", session)], None).landing;

        // Just BELOW the boundary: 98.49 rounds to 98 — under the strict `< 99` ceiling, so the SLO
        // is met and the episode is not classed a post-swap tail.
        let under = landing_at(0.9849);
        assert_eq!(under.p100, Some(98), "0.9849 → 98.49 → rounds DOWN to 98");
        assert_eq!(under.p100_met(), Some(true), "98 < 99 → the SLO is met");
        assert_eq!(under.post_swap_tail, 0, "not a breach below the boundary");

        // AT the boundary: 98.50 rounds UP to 99, which the strict `< 99` ceiling already counts as a
        // breach — and, since the swap itself fired below the ceiling (95), as a post-swap tail.
        let at = landing_at(0.985);
        assert_eq!(at.p100, Some(99), "0.985 → 98.5 → rounds UP to 99");
        assert_eq!(at.p100_met(), Some(false), "99 is NOT < 99 → breached");
        assert_eq!(
            at.gap_crossing, 0,
            "the swap fired at 95, below the ceiling"
        );
        assert_eq!(at.post_swap_tail, 1, "so the breach is the post-swap tail");

        // The REST of the sub-ceiling band rounding alone pulls onto the ceiling — every fraction here
        // is a breach that a truncating implementation would report as a compliant 98. Stopping
        // strictly below 0.99 keeps every entry discriminating (at and above it the modes agree).
        for session in [0.9875, 0.9899] {
            assert_eq!(
                landing_at(session).p100,
                Some(99),
                "{session} is below the nominal ceiling but rounds onto it",
            );
        }
        // Above the band the value keeps climbing rather than pinning at the ceiling.
        assert_eq!(landing_at(0.995).p100, Some(100), "0.995 → 99.5 → 100");
    }

    #[test]
    fn landing_classifies_gap_crossing_when_decision_already_over_ceiling() {
        // The daemon's OWN reading was already 100 at the swap — the overshoot is a gap-crossing,
        // visible in SLI 1 already, NOT a post-swap tail (even though the parked account stays >= 99).
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=session session_pct=100
";
        let samples = [sample(epoch("2026-07-11T00:02:00Z"), "work", 1.00)];
        let r = aggregate(&parse_events(log, None), &samples, None);
        assert_eq!(r.landing.n_measured, 1);
        assert_eq!(r.landing.gap_crossing, 1);
        assert_eq!(r.landing.post_swap_tail, 0);
    }

    #[test]
    fn landing_swap_without_a_post_swap_sample_is_unmeasured_not_zero() {
        // A session swap with NO usage sample of the parked account in the window: the store cannot
        // reconstruct where it landed. That is UNMEASURED (a coverage gap), never a fabricated 0.
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=session session_pct=96
";
        let samples = [
            sample(epoch("2026-07-10T23:59:00Z"), "work", 0.96), // BEFORE the swap → not a landing
            sample(epoch("2026-07-11T00:05:00Z"), "other", 1.00), // wrong account → never joined
        ];
        let r = aggregate(&parse_events(log, None), &samples, None);
        assert_eq!(r.landing.swaps_total, 1);
        assert_eq!(r.landing.n_measured, 0);
        assert_eq!(r.landing.n_unmeasured, 1);
        assert_eq!(
            r.landing.p100, None,
            "no measured episode → percentile is None, not a passing 0"
        );
        assert_eq!(r.landing.p100_met(), None);
    }

    #[test]
    fn landing_render_surfaces_measured_episodes_and_classes() {
        // Two session swaps: one fires at 96 and lands 100 (post-swap tail), one fires at 99 and
        // stays 100 (gap-crossing). The human + JSON both surface the distribution and the split.
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=session session_pct=96
ts=2026-07-11T01:00:00Z event=swap from=spare to=work reason=session session_pct=99
";
        let samples = [
            sample(epoch("2026-07-11T00:02:00Z"), "work", 1.00), // landing for the 00:00 swap
            sample(epoch("2026-07-11T01:02:00Z"), "spare", 1.00), // landing for the 01:00 swap
        ];
        let r = aggregate(&parse_events(log, None), &samples, None);
        assert_eq!(r.landing.n_measured, 2);
        assert_eq!(r.landing.post_swap_tail, 1);
        assert_eq!(r.landing.gap_crossing, 1);
        let human = render_human(&r);
        assert!(
            human.contains(
                "landing-point session_pct (post-swap peak of the outgoing account, window <= 900s)"
            ),
            "{human}"
        );
        assert!(
            human.contains("measured n=2 of 2 reason=session swaps (0 with no post-swap sample)"),
            "{human}"
        );
        assert!(
            human.contains("P100 = 100  vs ceiling < 99  [OVER]"),
            "{human}"
        );
        // Pin the full operator-facing breach-classes line (pluralization, the parenthetical
        // thresholds, and the blind-burn → time-blind pointer) — the measured human block is otherwise
        // only substring-checked, whereas the no-data block is byte-golden'd elsewhere.
        assert!(
            human.contains(
                "  breach classes: 1 post-swap tail (fired < 99, landed >= 99); 1 gap-crossing \
                 (decision >= 99); blind-burn: see time-blind SLI (issue #583)"
            ),
            "{human}"
        );
        let json = render_json(&r).expect("serializes");
        assert!(json.contains("\"n_measured\": 2,"), "{json}");
        assert!(json.contains("\"post_swap_tail\": 1,"), "{json}");
        assert!(json.contains("\"gap_crossing\": 1"), "{json}");
        assert!(json.contains("\"p100_met\": false,"), "{json}");
    }

    #[test]
    fn landing_anchors_are_bounded_by_the_since_window() {
        // The landing SLI shares SLI 1's `--since` bound: a swap BEFORE the cutoff contributes no
        // landing anchor even if its samples exist. Only the in-window swap is reconstructed.
        let log = "\
ts=2026-07-01T00:00:00Z event=swap from=work to=spare reason=session session_pct=95
ts=2026-07-10T00:00:00Z event=swap from=work to=spare reason=session session_pct=96
";
        let samples = [
            sample(epoch("2026-07-01T00:02:00Z"), "work", 1.00), // for the pre-cutoff swap
            sample(epoch("2026-07-10T00:02:00Z"), "work", 1.00), // for the in-window swap
        ];
        let cutoff = epoch("2026-07-05T00:00:00Z");
        let r = aggregate(&parse_events(log, Some(cutoff)), &samples, None);
        assert_eq!(
            r.landing.swaps_total, 1,
            "only the Jul-10 swap is in the --since window"
        );
        assert_eq!(r.landing.n_measured, 1);
        assert_eq!(r.landing.post_swap_tail, 1);
    }

    #[test]
    fn landing_stays_under_the_slo_for_a_burst_across_the_reobservation_gap() {
        // Issue #610 (AC2): the landing-overshoot magnitude for a burst-across-gap swap. Post-#609
        // (ADR-0024) the reactive arm looks ahead over the measured p90 re-observation gap
        // (`swap::REACTIVE_REOBSERVATION_GAP_SECS` = 313 s), so a burst that climbs across the gap is
        // caught by the effective ceiling (`ceiling − TAIL_MARGIN`) and the post-swap committed tail
        // (issue #595, measured max +5 pp) then lands the parked account BELOW the ceiling — under the
        // `P100 < SLO_SWAP_P100_MAX` (99) landing SLO. Pre-#609 the 120 s lookahead under-modeled the
        // real gap, so a burst climbed past the effective ceiling before re-observation and the tail
        // carried the landing to/over 99 (the residual #609 closed); the issue expected this test to
        // fail at ceiling 99 before that fix, and to hold after it.
        const MAX_COMMITTED_TAIL: f64 = 0.05; // issue #595: measured max post-swap committed tail (+5 pp)
        let slo = f64::from(SLO_SWAP_P100_MAX) / 100.0; // 0.99

        // Part 1 — the bound holds BY CONSTRUCTION across the operator ceiling range: the effective
        // ceiling plus the measured max committed tail stays under the SLO. TAIL_MARGIN (0.06) is set
        // strictly above the measured tail, so the landing lands below the ceiling; the ceiling being
        // < 1.0 keeps it under the SLO. Fails if TAIL_MARGIN regresses below the measured tail.
        for ceiling_pct in 95..=99u8 {
            let ceiling = f64::from(ceiling_pct) / 100.0;
            let worst_landing = crate::swap::effective_ceiling(ceiling) + MAX_COMMITTED_TAIL;
            assert!(
                worst_landing < slo,
                "ceiling {ceiling_pct}: worst landing {worst_landing} must stay under the P100<{SLO_SWAP_P100_MAX} SLO",
            );
        }

        // Part 2 — the landing SLI agrees for a concrete burst-across-gap swap at the DEFAULT ceiling
        // (95, ADR-0024 §5). The account rode the burst up to the effective ceiling (89) before the
        // bare-ceiling fire caught it (the cold-EMA / gap-beyond-lookahead worst case), then the
        // committed tail peaked at 94 — under the SLO, with the sub-SLO ceiling headroom to spare.
        let eff95 = crate::swap::effective_ceiling(0.95); // 0.89
        let decision_pct = (eff95 * 100.0).round() as u8; // 89
        let landing = eff95 + MAX_COMMITTED_TAIL; // 0.94
        let log = format!(
            "ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=session session_pct={decision_pct}\n"
        );
        let samples = [
            sample(epoch("2026-07-11T00:02:00Z"), "work", eff95 + 0.01), // climbing across the gap (90)
            sample(epoch("2026-07-11T00:05:00Z"), "work", landing),      // committed-tail peak (94)
            sample(epoch("2026-07-11T00:12:00Z"), "work", landing - 0.02), // settling back below (92)
        ];
        let r = aggregate(&parse_events(&log, None), &samples, None);
        assert_eq!(r.landing.n_measured, 1);
        assert_eq!(
            r.landing.p100,
            Some(94),
            "the burst-across-gap landing peaks at 94"
        );
        assert_eq!(r.landing.gap_crossing, 0, "fired at 89, below the SLO");
        assert_eq!(r.landing.post_swap_tail, 0, "94 landed below the SLO");
        assert_eq!(
            r.landing.p100_met(),
            Some(true),
            "P100 < 99 holds post-#609 for a burst across the re-observation gap",
        );
    }

    #[test]
    fn the_weekly_margin_covers_the_scaled_session_tail_down_to_its_breakeven() {
        // Issue #607. `WEEKLY_TAIL_MARGIN` is SCALED from the #595 session measurement, not
        // measured: the committed tail is one fixed quantity of in-flight work billing BOTH
        // windows, so `weekly_tail = session_tail / k` where `k = weekly_quota / session_quota`.
        // This test states the breakeven that scaling implies, so the assumption is executable
        // rather than prose-only — it is NOT independent evidence for the tail's magnitude (only a
        // weekly landing SLI can supply that; see `swap::WEEKLY_TAIL_MARGIN`).
        const SESSION_MAX_TAIL: f64 = 0.05; // issue #595, measured max post-swap committed tail
        const BREAKEVEN_K: f64 = 5.0; // the documented assumption: weekly budget >= 5 session windows

        // At the breakeven the margin exactly covers the scaled tail; above it, strictly covers.
        assert!((SESSION_MAX_TAIL / BREAKEVEN_K - crate::swap::WEEKLY_TAIL_MARGIN).abs() < 1e-9);
        for k in [5.0, 8.0, 12.0, 20.0, 33.6_f64] {
            assert!(
                SESSION_MAX_TAIL / k <= crate::swap::WEEKLY_TAIL_MARGIN,
                "at k={k} the scaled weekly tail must fit inside the margin",
            );
        }
        // Below the breakeven the margin is NOT sufficient — recorded so the failure mode is
        // explicit rather than discovered in production. If a weekly landing measurement ever puts
        // the real k under 5, this constant must be re-calibrated upward.
        const { assert!(SESSION_MAX_TAIL / 4.0 > crate::swap::WEEKLY_TAIL_MARGIN) };

        // The structural half, which does NOT depend on k: the landing sits below the ceiling for
        // every operator-settable weekly ceiling, and so below the real 100% weekly wall.
        for ceiling_pct in 50..=99u8 {
            let ceiling = f64::from(ceiling_pct) / 100.0;
            let fire = crate::swap::weekly_effective_ceiling(ceiling);
            assert!(
                fire < ceiling,
                "weekly ceiling {ceiling_pct}: fire below ceiling"
            );
            assert!(fire + crate::swap::WEEKLY_TAIL_MARGIN <= ceiling + 1e-9);
            assert!(ceiling < 1.0);
        }
    }
}
