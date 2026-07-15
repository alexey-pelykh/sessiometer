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
//!
//! Like `stats` (issue #158) this is an OFFLINE reader: it reads the log file directly and
//! makes no live control-socket / keychain / usage-API call, so it renders when the daemon
//! is down. The daemon is the sole WRITER of the log, this verb one READER. The readout is
//! roster-wide (no per-account breakdown), so it emits no account identifier at all — every
//! output line is bare numbers and fixed labels, secret-free by construction (issue #15);
//! the durable-line redaction test in this module asserts it.
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

use crate::error::{Error, Result};
use crate::usage::epoch_from_rfc3339;
use std::collections::BTreeMap;

/// SLO target: swap-out `session_pct` **P100 must be `< 99`** — no `reason=session` swap fires
/// at or above 99%. INTERIM per issue #455 (the extended #363 acceptance); the source of
/// truth until the #451/#484 confirmation gate finalizes it against production — the
/// interim-const-with-provenance stance of [`crate::daemon`]'s `BLIND_GATE_*`.
const SLO_SWAP_P100_MAX: u8 = 99;

/// SLO target: swap-out `session_pct` **P50 must be `<= 97`** (median swap-out lands in the
/// [95, 97] band, not later). INTERIM per issue #455; see [`SLO_SWAP_P100_MAX`] for the
/// finalization gate. Note the comparator differs from P100 — inclusive here, strict there.
const SLO_SWAP_P50_MAX: u8 = 97;

/// Proxy margin (percentage points) for the #452-pending false-preempt SLI: a hypothetical
/// anchor-keyed preemptive swap is classed "would-be wasted" when the fresh recovery reading
/// had dropped more than this far below the stale pre-blind anchor. INTERIM (issue #455); the
/// real necessary/wasted threshold is #451/#484's to derive — this only supplies the
/// ingredient, exactly as the `blind_window` SLI records the raw readings rather than a baked
/// verdict.
const PREEMPT_WASTED_MARGIN_PCT: u8 = 20;

/// The stable `--json` schema version. Owned by this readout, independent of `stats`'
/// schema. Named to match [`crate::stats`]'s own `JSON_SCHEMA_VERSION`. Bumped `1 → 2` when
/// the `--since` window (issue #494) added the top-level `window` object — an ADDITIVE change
/// (an always-present field, `null` in the whole-log default), so a `--json` consumer of the
/// #363 acceptance gate that ignores unknown fields still parses every prior field unchanged.
const JSON_SCHEMA_VERSION: u32 = 2;

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
    let report = aggregate(&parse_events(&text, cutoff), window);
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
}

/// Parse the SLI ingredients out of the structured event-log `text`.
///
/// Tolerant, forward-only, self-contained: it reads the flat `key=val` grammar
/// ([`crate::observability`]) line by line and folds the four relevant event families into
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
                // #452 preemptive swaps (reason=blind_preempt, ADR-0017): count each observed one
                // for the false-preempt SLI's REAL numerator, then skip the session-overshoot
                // accounting below — a preemptive swap fires on a STALE anchor, not a fresh reading,
                // so its session_pct is not a swap-out overshoot sample.
                if fields.get("reason").copied() == Some("blind_preempt") {
                    inputs.preemptive_swaps = inputs.preemptive_swaps.saturating_add(1);
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
    time_blind_near_limit_secs: u64,
    false_preempt: FalsePreempt,
    rate_limit: RateLimit,
}

/// Fold the parsed [`Inputs`] into a [`Report`], attaching the active `window` for display.
/// Pure and total: the windowing already happened in [`parse_events`] (the `inputs` are the
/// filtered subset); `window` is carried through untouched, only so the renderers can document
/// the bound.
fn aggregate(inputs: &Inputs, window: Option<Window>) -> Report {
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

    let near_limit_windows = inputs.near_limit_reconciliations.len() as u32;
    let would_be_wasted = inputs
        .near_limit_reconciliations
        .iter()
        // Saturating: recovery >= anchor → 0, never "> margin", correctly "would-be necessary".
        .filter(|(anchor, recovery)| anchor.saturating_sub(*recovery) > PREEMPT_WASTED_MARGIN_PCT)
        .count() as u32;

    Report {
        window,
        swap_overshoot,
        time_blind_near_limit_secs: inputs.time_blind_near_limit_secs,
        false_preempt: FalsePreempt {
            preemptive_swaps_observed: inputs.preemptive_swaps,
            near_limit_windows,
            would_be_wasted,
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
        "sessiometer reliability — swap-out overshoot SLO readout (offline; reads the event log)\n\n",
    );

    // Active window (issue #494) — documents the bound so the numbers below are read in
    // context. Absent for the whole-log default, so that output is byte-for-byte unchanged.
    if let Some(w) = &r.window {
        out.push_str(&format!(
            "window: since {} ({}) — all four SLIs bounded to events at/after the cutoff\n\n",
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
    time_blind_near_limit_secs: u64,
    false_preempt: FalsePreemptWire,
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
    const FIXTURE_LOG: &str = "\
ts=2026-07-11T00:00:00Z event=swap from=oleksii@pelykh.com to=oleksii@pelykhconsulting.fr reason=session session_pct=96
ts=2026-07-11T00:05:00Z event=swap from=oleksii@pelykhconsulting.fr to=oleksii@pelykh.com reason=weekly session_pct=42
ts=2026-07-11T00:06:00Z event=swap from=oleksii@pelykh.com to=oleksii@pelykhconsulting.fr reason=session session_pct=100 late=true
ts=2026-07-11T00:07:00Z event=swap from=oleksii@pelykh.com to=oleksii@pelykhconsulting.fr reason=manual session_pct=0
ts=2026-07-11T00:08:00Z event=emergency_swap from=oleksii@pelykh.com to=oleksii@pelykhconsulting.fr
ts=2026-07-11T00:10:00Z event=blind_window acct=u-A duration_secs=300 session_pct=97 session_at_recovery=99 near_limit=true
ts=2026-07-11T00:20:00Z event=blind_window acct=u-B duration_secs=600 session_pct=96 session_at_recovery=40 near_limit=true
ts=2026-07-11T00:30:00Z event=blind_window acct=u-C duration_secs=120 session_pct=50 session_at_recovery=51 near_limit=false
ts=2026-07-11T00:40:00Z event=usage_backoff acct=u-A class=rate_limited consecutive=1 backoff_secs=60
ts=2026-07-11T00:41:00Z event=usage_backoff acct=u-A class=rate_limited consecutive=2 backoff_secs=120 retry_after_secs=120
ts=2026-07-11T00:42:00Z event=usage_backoff acct=u-B class=transient consecutive=1 backoff_secs=30
ts=2026-07-11T00:45:00Z event=usage_backoff_cleared acct=u-A
ts=2026-07-11T00:50:00Z event=usage_velocity acct=u-A session_pct_per_min=0.20 weekly_pct_per_min=0.01 elapsed_secs=120 session_delta_pct=1 weekly_delta_pct=0
";

    fn fixture_report() -> Report {
        aggregate(&parse_events(FIXTURE_LOG, None), None)
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
        let r = aggregate(&parse_events("", None), None);
        assert_eq!(r.swap_overshoot.n, 0);
        // Cardinality-zero: percentiles are None (not a passing 0), so no target is asserted met.
        assert_eq!(r.swap_overshoot.p50, None);
        assert_eq!(r.swap_overshoot.p100, None);
        assert_eq!(r.swap_overshoot.p50_met(), None);
        assert_eq!(r.swap_overshoot.p100_met(), None);
        assert_eq!(r.time_blind_near_limit_secs, 0);
        assert_eq!(r.false_preempt.near_limit_windows, 0);
    }

    #[test]
    fn passing_targets_are_flagged_met() {
        // A clean roster: swaps at 95/96/97 → P50=96<=97, P100=97<99.
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=a to=b reason=session session_pct=95
ts=2026-07-11T00:01:00Z event=swap from=a to=b reason=session session_pct=96
ts=2026-07-11T00:02:00Z event=swap from=a to=b reason=session session_pct=97
";
        let r = aggregate(&parse_events(log, None), None);
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
                "sessiometer reliability — swap-out overshoot SLO readout (offline; reads the event log)\n",
                "\n",
                "swap-out session_pct (reason=session), n=2\n",
                "  P50  = 96  target <= 97  [ok]\n",
                "  P95  = 100\n",
                "  P100 = 100  target < 99   [OVER]\n",
                "\n",
                "time blind & near-limit: 900s (sum of blind_window duration_secs where near_limit=true)\n",
                "\n",
                "false-preempt (preemptive swap whose target turned out unnecessary)\n",
                "  preemptive swaps observed: 0\n",
                "  proxy (blind-window reconciliation, interim margin 20pp): 1 of 2 near-limit windows would-be-wasted\n",
                "\n",
                "usage-poll 429 neutrality (roster-wide): rate_limited=2 transient=1 cleared=1\n",
            )
        );
    }

    #[test]
    fn human_render_handles_no_swaps() {
        let out = render_human(&aggregate(&parse_events("", None), None));
        assert!(
            out.contains("swap-out session_pct (reason=session): no swaps observed"),
            "cardinality-zero must not print a fabricated P100: {out}"
        );
    }

    #[test]
    fn json_render_is_stable_schema_2() {
        // The whole-log default: `window` is null and every prior field is byte-identical to
        // schema:1 — the additive contract (#494). A `--since` document is asserted separately
        // in `json_documents_the_active_window`.
        let out = render_json(&fixture_report()).expect("integer wire serializes");
        assert_eq!(
            out,
            concat!(
                "{\n",
                "  \"schema\": 2,\n",
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
        let out = render_json(&aggregate(&parse_events("", None), None)).expect("serializes");
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
        let r = aggregate(&inputs, Some(window));
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
        let out = render_human(&aggregate(&inputs, Some(window)));
        assert!(
            out.contains(
                "window: since 2026-07-05T00:00:00Z (7d) — all four SLIs bounded to events at/after the cutoff"
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
            Some(window),
        ))
        .expect("serializes");
        assert!(out.contains("\"schema\": 2,"), "schema bumped to 2: {out}");
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
        // cutoff instant) must itself stay secret-free.
        let whole = fixture_report();
        let windowed = aggregate(
            &parse_events(FIXTURE_LOG, Some(epoch("2026-07-11T00:00:00Z"))),
            Some(Window::resolve("30m", epoch("2026-07-11T00:30:00Z")).unwrap()),
        );
        // Non-degeneracy: the window must retain the fixture's swaps, else the windowed render
        // is empty and its leak guard proves nothing.
        assert!(
            windowed.swap_overshoot.n > 0,
            "windowed report must fold the fixture swaps"
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
}
