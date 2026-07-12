// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The `reliability` verb — an OFFLINE reliability-SLO readout over the event log (issue #455).
//!
//! `sessiometer reliability [--json]` aggregates the durable event log
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

use crate::error::{Error, Result};
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
/// schema; additive changes extend it without a bump (the `stats` `schema:1` precedent).
/// Named to match [`crate::stats`]'s own `JSON_SCHEMA_VERSION`.
const JSON_SCHEMA_VERSION: u32 = 1;

/// Parsed `reliability` options (issue #455). A plain comparable value so the CLI parser is
/// unit-testable by value, like `StatsArgs`.
#[derive(Debug, PartialEq)]
pub(crate) struct ReliabilityArgs {
    /// `--json` — print the machine-readable readout (for scripts / the #363 acceptance gate)
    /// instead of the human text.
    pub(crate) json: bool,
}

/// Entry point for the `reliability` verb: read the event log once, aggregate, and render.
/// The only impure step is reading the log file; everything else is a pure function of its
/// text. Not `async` — it makes no live call (mirrors the read-only `config` verbs).
pub(crate) fn run(args: ReliabilityArgs) -> Result<()> {
    let text = read_event_log()?;
    let report = aggregate(&parse_events(&text));
    let out = if args.json {
        render_json(&report)?
    } else {
        render_human(&report)
    };
    print!("{out}");
    Ok(())
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
}

/// Parse the SLI ingredients out of the structured event-log `text`.
///
/// Tolerant, forward-only, self-contained: it reads the flat `key=val` grammar
/// ([`crate::observability`]) line by line and folds the four relevant event families into
/// [`Inputs`], skipping blank lines, other event kinds, and any line missing a field it needs
/// or carrying an unparseable value (the same tolerant-drop the `stats` swap parser uses). No
/// timestamps are read — the readout is a whole-log aggregate, not a windowed view.
fn parse_events(text: &str) -> Inputs {
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

        match fields.get("event").copied() {
            Some("swap") => {
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
    /// Real preemptive swaps observed. Always `0` today — the #452 preemptive-swap path is
    /// not built, so there is no such event to count; this populates once #452 lands and its
    /// swap-outcome event is folded into [`parse_events`].
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

/// The aggregated readout — one whole-log pass folded into the four SLIs.
#[derive(Debug, PartialEq)]
struct Report {
    swap_overshoot: SwapOvershoot,
    time_blind_near_limit_secs: u64,
    false_preempt: FalsePreempt,
    rate_limit: RateLimit,
}

/// Fold the parsed [`Inputs`] into a [`Report`]. Pure and total.
fn aggregate(inputs: &Inputs) -> Report {
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
        swap_overshoot,
        time_blind_near_limit_secs: inputs.time_blind_near_limit_secs,
        false_preempt: FalsePreempt {
            preemptive_swaps_observed: 0,
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

    // SLI 3 — false-preempt (real rate pending #452; blind-window proxy today).
    out.push_str("false-preempt (preemptive swap whose target turned out unnecessary)\n");
    out.push_str(&format!(
        "  preemptive swaps observed: {} (#452 pending — real rate not yet measurable)\n",
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

// --- rendering: JSON wire (schema:1) ----------------------------------------

/// The stable `--json` document. Field names are OWNED by this wire contract (decoupled from
/// the internal aggregate types), so an internal refactor cannot silently break the schema.
#[derive(serde::Serialize)]
struct ReliabilityWire {
    schema: u32,
    swap_overshoot: SwapOvershootWire,
    time_blind_near_limit_secs: u64,
    false_preempt: FalsePreemptWire,
    rate_limit_neutrality: RateLimitWire,
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
        aggregate(&parse_events(FIXTURE_LOG))
    }

    #[test]
    fn parse_folds_only_the_four_relevant_families() {
        let inputs = parse_events(FIXTURE_LOG);
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
        let r = aggregate(&parse_events(""));
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
        let r = aggregate(&parse_events(log));
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
                "  preemptive swaps observed: 0 (#452 pending — real rate not yet measurable)\n",
                "  proxy (blind-window reconciliation, interim margin 20pp): 1 of 2 near-limit windows would-be-wasted\n",
                "\n",
                "usage-poll 429 neutrality (roster-wide): rate_limited=2 transient=1 cleared=1\n",
            )
        );
    }

    #[test]
    fn human_render_handles_no_swaps() {
        let out = render_human(&aggregate(&parse_events("")));
        assert!(
            out.contains("swap-out session_pct (reason=session): no swaps observed"),
            "cardinality-zero must not print a fabricated P100: {out}"
        );
    }

    #[test]
    fn json_render_is_stable_schema_1() {
        let out = render_json(&fixture_report()).expect("integer wire serializes");
        assert_eq!(
            out,
            concat!(
                "{\n",
                "  \"schema\": 1,\n",
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
        let out = render_json(&aggregate(&parse_events(""))).expect("serializes");
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
        let r = fixture_report();
        for out in [render_human(&r), render_json(&r).expect("serializes")] {
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
