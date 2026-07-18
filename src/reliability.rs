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
/// the #539 velocity-projection trigger added the `projected_swap_overshoot` + `false_projection`
/// objects; bumped `3 → 4` when the #595 landing-point SLI added the `landing` object — every
/// bump ADDITIVE (always-present new fields), so a `--json` consumer of the #363 acceptance gate
/// that ignores unknown fields still parses every prior field unchanged; bumped `4 → 5` when the
/// #608 observed session-velocity SLI added the `observed_peak` object (the live peak vs the assumed
/// `v_peak` the coupling bound is calibrated on) — additive, same as every prior bump.
const JSON_SCHEMA_VERSION: u32 = 5;

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
    /// `session_pct` of every `reason=velocity_preempt` swap (issue #539, ADR-0017) — the PROJECTED
    /// swap-out overshoot distribution on COVERED swaps, kept SEPARATE from `swap_out_pcts` (the
    /// reactive `reason=session` residual). Its P50/P100 are the #539 acceptance (`<= 94` / `<= 98`).
    projected_swap_out_pcts: Vec<f64>,
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
                // the false-projection SLI, and fold its FRESH `session_pct` into the PROJECTED
                // swap-out overshoot distribution (the #539 covered-swap acceptance) — SEPARATE from
                // the reactive `reason=session` distribution below (which is now the poll-gap residual
                // #540 owns). A projective swap fires on a live reading, so — unlike blind_preempt —
                // its session_pct IS a real swap-out sample.
                if fields.get("reason").copied() == Some("velocity_preempt") {
                    inputs.velocity_preempt_swaps = inputs.velocity_preempt_swaps.saturating_add(1);
                    if let Some(pct) = fields.get("session_pct").and_then(|v| v.parse::<u8>().ok())
                    {
                        inputs.projected_swap_out_pcts.push(f64::from(pct));
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

/// The PROJECTED swap-out overshoot distribution (issue #539, ADR-0017): the `session_pct`
/// percentiles over `reason=velocity_preempt` swaps — the COVERED-swap acceptance for the velocity-
/// projection trigger, distinct from the reactive [`SwapOvershoot`] (the poll-gap residual #540
/// owns). `None` percentiles when no projective swap was observed, so the readout never asserts a
/// target PASS on an empty subject (the same cardinality-zero discipline as [`SwapOvershoot`]).
#[derive(Debug, PartialEq)]
struct ProjectedSwapOvershoot {
    n: usize,
    p50: Option<u8>,
    p95: Option<u8>,
    p100: Option<u8>,
}

impl ProjectedSwapOvershoot {
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
/// bounds it: 0 truly-wasted swaps at H ≤ 150 s. The companion projected swap-out overshoot
/// distribution ([`ProjectedSwapOvershoot`]) shows these swaps land at P50 = 94 (barely ahead of the
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
    /// The #539 velocity-projection covered-swap overshoot (`reason=velocity_preempt` percentiles).
    projected_swap_overshoot: ProjectedSwapOvershoot,
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

    // The #539 PROJECTED swap-out overshoot — the same percentile discipline over the
    // `reason=velocity_preempt` distribution (its own cardinality gate, so a target is never PASSED
    // on zero projective swaps).
    let projected_n = inputs.projected_swap_out_pcts.len();
    let projected_pct = |p: f64| -> Option<u8> {
        (projected_n > 0)
            .then(|| crate::percentile::percentile(&inputs.projected_swap_out_pcts, p) as u8)
    };
    let projected_swap_overshoot = ProjectedSwapOvershoot {
        n: projected_n,
        p50: projected_pct(0.50),
        p95: projected_pct(0.95),
        p100: projected_pct(1.0),
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

    Report {
        window,
        swap_overshoot,
        projected_swap_overshoot,
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

    // SLI 1b — PROJECTED swap-out session_pct percentiles (issue #539): the covered-swap acceptance
    // for the velocity-projection trigger, vs its own targets. Separate from the reactive block above
    // (now the poll-gap residual #540 owns); the full-trace P100 < 99 is #539 + #540 together.
    match (
        r.projected_swap_overshoot.p50,
        r.projected_swap_overshoot.p95,
        r.projected_swap_overshoot.p100,
    ) {
        (Some(p50), Some(p95), Some(p100)) => {
            out.push_str(&format!(
                "projected swap-out session_pct (reason=velocity_preempt), n={}\n",
                r.projected_swap_overshoot.n
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
            "projected swap-out session_pct (reason=velocity_preempt): no projective swaps observed\n",
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
    // reconciliation, still pending (see the projected swap-out P50 above for the low-waste evidence).
    out.push_str(
        "false-projection (velocity-projection swap fired ahead of the observed overshoot)\n",
    );
    out.push_str(&format!(
        "  velocity-projection swaps observed: {}\n\n",
        r.false_projection.velocity_preempt_swaps_observed
    ));

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
    /// The #539 velocity-projection covered-swap overshoot (schema:3, additive).
    projected_swap_overshoot: ProjectedSwapOvershootWire,
    /// The #595 landing-point overshoot — where reason=session swaps actually landed (schema:4, additive).
    landing: LandingWire,
    /// The #608 observed session-velocity distribution vs the assumed `v_peak` (schema:5, additive).
    observed_peak: ObservedPeakWire,
    time_blind_near_limit_secs: u64,
    false_preempt: FalsePreemptWire,
    /// The #539 false-projection SLI (schema:3, additive).
    false_projection: FalseProjectionWire,
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

/// PROJECTED swap-out overshoot block (issue #539): the covered-swap acceptance for the velocity-
/// projection trigger, `null` percentiles / flags when no projective swap was observed.
#[derive(serde::Serialize)]
struct ProjectedSwapOvershootWire {
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
        projected_swap_overshoot: ProjectedSwapOvershootWire {
            n: r.projected_swap_overshoot.n,
            p50: r.projected_swap_overshoot.p50,
            p95: r.projected_swap_overshoot.p95,
            p100: r.projected_swap_overshoot.p100,
            targets: ProjectedSwapTargetsWire {
                p50_max: SLO_PROJECTED_SWAP_P50_MAX,
                p100_max: SLO_PROJECTED_SWAP_P100_MAX,
            },
            met: SwapMetWire {
                p50: r.projected_swap_overshoot.p50_met(),
                p100: r.projected_swap_overshoot.p100_met(),
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
                "projected swap-out session_pct (reason=velocity_preempt): no projective swaps observed\n",
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
    fn json_render_is_stable_schema_5() {
        // The whole-log default: `window` is null and every PRIOR field is byte-identical to
        // schema:1/2/3/4 — the additive contract (#494/#539/#595/#608). The #608 `observed_peak`
        // object is new-but-always-present (n=1 here — the FIXTURE_LOG's single usage_velocity line
        // at 0.20 %/min, well under the 6.95 v_peak, so v_peak_honest=true). A `--since` document is
        // asserted separately in `json_documents_the_active_window`.
        let out = render_json(&fixture_report()).expect("integer wire serializes");
        assert_eq!(
            out,
            concat!(
                "{\n",
                "  \"schema\": 5,\n",
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
                "  \"projected_swap_overshoot\": {\n",
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
        assert!(out.contains("\"schema\": 5,"), "schema bumped to 5: {out}");
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
}
