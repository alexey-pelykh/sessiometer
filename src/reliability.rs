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
//! 7. **refresh-token loss** (issue #881) — the accounts a lapsed/revoked REFRESH token removed
//!    from the fleet. Attributed DISTINCTLY from every swap SLI above, following the issue #719
//!    precedent that segregated `all_exhausted` capacity-holds out of the swap-out SLI: an account
//!    lost this way is a credential-LIFECYCLE event with a known operator cure (`sessiometer
//!    login`), not a swap-out failure, so folding it into a reliability signal would contaminate
//!    that signal with an operator-action-pending condition. Unlike #719 the segregation here is
//!    STRUCTURAL rather than a filter — the loss evidence rides refresh-family events, which are
//!    disjoint from the `event=swap` population every swap SLI folds — so no existing partition
//!    changes meaning. Depth lives with the code: [`RefreshTokenLoss`] for the segregation, the
//!    refresh-family arm of [`parse_events`] for the predicate and its lapsed-vs-revoked scope bound.
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
//!
//! ## Which event the readout reads (issue #591)
//!
//! BOTH, routed per-SLI by semantics — deliberately NOT a swap from the censored `blind_window`
//! (issue #449) to the uncensored `blind_enter` / `blind_exit` pair (issue #583):
//!
//! - **SLI 2 and SLI 3's proxy keep reading `blind_window`, unchanged.** Re-pointing a published
//!   figure at a different population would make one number mean two different things either side
//!   of a schema bump, so a stored reading could no longer be compared with a fresh one. That
//!   event is also retained deliberately by [`crate::daemon`] as the recovery-edge duration
//!   histogram: it was assigned the wrong PURPOSE (detection), not built wrong for this one.
//! - **SLI 6 cannot move.** Its inputs are the #634 velocity ingredients and the #670
//!   `session_high_water_pct`, and both are stamped on `blind_window` alone — the pair carries
//!   neither. It instead CONSUMES the pair's census for its two censored-tail counts, which is
//!   exactly what those two fields were reserved for.
//! - **The pair gets its own census** ([`BlindEpisodes`]), published BESIDE the censored figures
//!   rather than replacing them. The gap between the two IS the censoring, and showing both is what
//!   makes it legible. Blending them would also double-count every episode that emits both families
//!   (an active account that recovers emits `blind_window` AND `blind_exit` for the one episode).
//!
//! Two treatments in that census are deliberate rather than mechanical:
//!
//! - **Never-recovered episodes are right-censored, not summed.** Such an episode has no exit line
//!   and therefore NO measured duration, so it contributes a LOWER BOUND (`horizon − entry`) kept in
//!   its own field — never folded into a figure that would read as a total. The horizon is the last
//!   `ts=` in view, not the wall clock, so the same text always folds to the same number.
//! - **Restart orphans are excluded from that tail.** An unmatched entry counts as never-recovered
//!   only when no LATER entry for the same account supersedes it. Because [`crate::daemon`]'s
//!   per-account edge machine strictly alternates, a second entry with no exit between PROVES the
//!   in-memory anchor was lost — a daemon restart — so those are counted separately
//!   ([`BlindEpisodes::n_anchor_lost`]) and cannot inflate the worst tail with ordinary restarts.
//!   Issue #591 proposes disambiguating restarts with a `diag=start` marker instead; that marker
//!   rides the verbosity-gated OPERATOR diagnostic channel to stderr (default
//!   [`crate::observability::Verbosity::Quiet`] emits nothing) and never reaches the durable log
//!   this reader folds, so the re-entry signal is used in its place.
//!
//! The interim [`PREEMPT_WASTED_MARGIN_PCT`] proxy margin is deliberately NOT reworked here. The
//! pair's weekly dimension does now make a session-window RESET distinguishable from a genuinely
//! quiet window (the weekly window does not roll on the 5 h session cadence), so reworking it has
//! become possible — but that threshold is issues #451/#484's to derive against production, and
//! this module's standing discipline is to supply the INGREDIENT and leave the verdict a query-time
//! view. The ingredient is exposed; the margin stays where its owner can set it.

use crate::error::{Error, Result};
use crate::usage::epoch_from_rfc3339;
use crate::usage_store::Sample;
use crate::use_account::ADOPT_UNKNOWN_FROM;
use std::collections::{BTreeMap, BTreeSet};

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
/// at 6 stays the lone non-additive bump. Bumped `7 → 8` (issue #591) when the uncensored
/// blind-episode census added the `blind_episodes` object — ADDITIVE (a new always-present field;
/// every schema:7 key is byte-identical and keeps its meaning). The same bump finally POPULATES
/// `blind_projection_error.n_swapped_away` / `.n_never_recovered`, which schema:7 always emitted as
/// `null`: a value-domain fill of two already-present, already-documented nullable keys, not a key
/// change — a consumer that already handled the documented `null` handles the number. Bumped
/// `8 → 9` (issue #719) when the `all_exhausted` capacity-hold segregation added the top-level
/// `capacity_held` object and `landing.capacity_held` — both ADDITIVE (new always-present fields).
/// The bump ALSO carries a value-domain SEMANTIC correction with no key change: `swap_overshoot`
/// (the #363 gate) and `landing` now EXCLUDE capacity-held swaps — #363 was always scoped to
/// exclude the all-exhausted condition, so folding them in was the latent defect; their `n`/
/// percentiles now reflect reaction-latency only, and the excluded population reads from
/// `capacity_held` / `landing.capacity_held`. A consumer reading `swap_overshoot.met.p100` now gets
/// the honest gate verdict; the pre-9 total is reconstructable as `swap_overshoot.n + capacity_held.n`.
/// Bumped `9 → 10` (issue #881) when the refresh-token-loss attribution added the top-level
/// `refresh_token_loss` object — ADDITIVE (a new always-present field; every schema:9 key is
/// byte-identical and keeps its meaning). Unlike `8 → 9` it carries NO value-domain correction:
/// the new block folds refresh-family events, which are DISJOINT from the `event=swap` population
/// every swap SLI reads, so not one prior figure moves. That is the issue #881 acceptance ("the
/// existing swap-out SLI partition is unchanged in meaning") holding BY CONSTRUCTION rather than by
/// a filter — diff a schema:9 and a schema:10 readout of one log and only the added key differs.
/// Bumped `10 → 11` (issue #1367) when `refresh_token_loss.by_mechanism` gained `parked_recovery`
/// — ADDITIVE in KEYS (a new always-present field; every schema:10 key is byte-identical), and,
/// like `8 → 9`, carrying a value-domain correction with no key change: `poll_retry` no longer
/// counts the issue #643 re-probe the `restored` control signal drives, which shares the
/// `event=poll_refresh` name but involves no poll and no 401. `by_mechanism.total()` is unchanged —
/// the observations moved buckets, none left the set — so `accounts` and `confirmed_unrecoverable`
/// do not move either. The correction is NOT retroactive, and this is the one place a consumer can
/// read that: the discriminating `trigger=` VALUE did not exist on the line before #1367, so over
/// any window predating it `parked_recovery` reads `0` and `poll_retry` still carries both
/// populations. A schema:11 readout of an OLD log is not a corrected schema:10 readout of it; it is
/// the same wrong split under a new key. Bumped `11 → 12` (issue #1453) when the operator-initiated
/// landing partition added the top-level `operator_landing` object — ADDITIVE (a new always-present
/// field; every schema:11 key is byte-identical). Like `9 → 10`'s key addition and unlike `8 → 9`
/// it carries NO value-domain correction: the new block folds `reason=manual` / `reason=forced`
/// anchors, a population every prior block EXCLUDED by its `reason=session` filter, so not one prior
/// figure moves — diff a schema:11 and a schema:12 readout of one log and only the added key
/// differs. The exclusion itself is left in place deliberately rather than widened: `session_pct=0`
/// on a manual swap is a correct record of "not session-triggered", so folding those swaps into
/// `swap_overshoot` would corrupt the #363 gate with readings no daemon decision produced.
///
/// `pub(crate)` so the number [`crate::cli`]'s `RELIABILITY_USAGE` advertises to script authors is
/// held against this one by a test instead of by hand (issue #913) — tied so the two cannot drift,
/// the way [`LANDING_WINDOW_SECS`] ties the offline window to the runtime detector's. Nothing else
/// outside this module reads it.
pub(crate) const JSON_SCHEMA_VERSION: u32 = 12;

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
    /// Resolve a raw `--since` value against `now` (epoch seconds) into a [`Window`]. The span
    /// grammar is [`crate::duration::parse_duration_secs`] — shared with `log` (issue #773) so
    /// the two offline readers cannot drift — and its rejection is mapped HERE, so an operator
    /// mistyping this verb's flag still reads [`Error::ReliabilitySinceInvalid`]. Saturating
    /// throughout: an absurd span can never overflow into a future cutoff, and a span reaching
    /// past the epoch clamps to `0`.
    fn resolve(raw: &str, now: i64) -> Result<Window> {
        let secs = crate::duration::parse_duration_secs(raw)
            .ok_or_else(|| Error::ReliabilitySinceInvalid(raw.trim().to_owned()))?;
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

/// A swap-out anchor for the landing-point reconstruction (issue #595): the instant a parked
/// account's post-swap window opens, plus the join key and — where the swap had one — the decision
/// reading. Carries `reason=session` anchors and, since issue #1453, operator-initiated ones.
#[derive(Debug, Clone, PartialEq)]
struct SwapOut {
    /// The swap instant (epoch seconds) — the window origin; landing samples are `> ts`.
    ts: i64,
    /// The OUTGOING account's roster label (`from=`) — the join key into `usage-samples.jsonl`'s
    /// `acct`. Used INTERNALLY only; never rendered (the #15 roster-wide-numbers invariant).
    acct: String,
    /// The decision-point `session_pct` logged at the swap — separates a gap-crossing (already
    /// ≥ ceiling here) from a post-swap tail (fired below, landed at/over).
    ///
    /// `None` for an OPERATOR-initiated swap (issue #1453), which has no decision reading at all: it
    /// records `session_pct=0` because it was not session-triggered, so reading that `0` as a
    /// decision would classify every operator rescue as a post-swap tail — a breach class invented
    /// out of a field that means "not applicable". An anchor without a reading is still a perfectly
    /// good WINDOW, which is what the landing reconstruction needs.
    decision_pct: Option<u8>,
    /// Whether this swap resolved an `all_exhausted` capacity hold (issue #719): the outgoing
    /// account was pinned at the ceiling because NO viable target existed, not because a reaction
    /// was late. `true` when this swap's `to=` matches the `hold=` of the latest STILL-OPEN
    /// `all_exhausted` — an episode already closed by its `all_exhausted_cleared` LEAVE edge holds
    /// nothing back, so a later swap onto that same spare is a plain reaction-latency sample (issue
    /// #828). That `hold=` names the soonest-returning spare the relief lands on, so the OUTGOING
    /// (`from=`) account is the capacity casualty. #363 explicitly EXCLUDES the all-exhausted
    /// condition, so a held anchor is kept OUT of the landing SLO and counted in
    /// [`Landing::capacity_held`] instead of a breach class.
    held: bool,
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
    /// `session_pct` of every CONTROLLABLE `reason=session` swap — the reaction-latency swap-out
    /// overshoot distribution, the #363 acceptance gate's subject. weekly (low incidental
    /// session_pct, out of scope), `manual`/`forced` (`session_pct=0`), and `emergency_swap` (no
    /// field) are excluded so they cannot poison the low tail. Capacity-holds (issue #719) are ALSO
    /// excluded — routed to [`Inputs::swap_out_held_pcts`] — because #363 excludes the all-exhausted
    /// condition; folding them here (they land at 100 by construction) dragged P100 to 100 and made
    /// the gate un-judgeable.
    swap_out_pcts: Vec<f64>,
    /// `session_pct` of every CAPACITY-HELD `reason=session` swap (issue #719): a swap that resolved
    /// an `all_exhausted` hold (no viable target — the active account pinned at the ceiling until a
    /// peer reset), so it is a fleet-CAPACITY limit, not a reaction-latency miss. Reported as its own
    /// partition beside `swap_out_pcts`; NEVER feeds the #363 gate. `hold=` matched to the swap's
    /// `from=` at parse time.
    swap_out_held_pcts: Vec<f64>,
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
    /// OPERATOR-INITIATED swap-out anchors for the landing-point SLI (issue #1453): the same
    /// (ts, outgoing account) windows as `session_swaps`, for `reason=manual` and `reason=forced`.
    ///
    /// Kept in their OWN vec rather than folded into `session_swaps`, because the two populations
    /// cannot share a percentile without one of them lying. An operator swap records
    /// `session_pct=0` DELIBERATELY — it was not session-triggered, and that `0` is the token which
    /// says so (`crate::observability::SwapReason`) — so it carries no decision reading to enter a
    /// decision-point distribution or to classify a breach against. What it DOES carry is a real
    /// parked account and a real instant, which is all the landing reconstruction needs, and the
    /// operator's own rescue of an at-limit active is exactly the event this readout was blind to.
    /// The segregation is the issue #719 precedent applied to a second population: publish it beside
    /// the gate rather than inside it, so no existing figure changes meaning.
    operator_swaps: Vec<SwapOut>,
    /// Every re-activation edge (issue #595): the (ts, re-activated account) of any `event=swap`,
    /// `event=emergency_swap`, `event=restash`, or `event=canonical_recovered`, so the landing window
    /// of a previously-parked account can be closed at the instant it becomes active again
    /// (`active_at != acct`). All four revival paths count — re-activation is not swap-reason- or
    /// swap-kind-specific.
    reactivations: Vec<Reactivation>,
    /// Every observed POSITIVE session climb rate in %/min from an `event=usage_velocity` line
    /// (issue #608) — the live session-climb distribution the assumed peak constant
    /// (`swap::V_PEAK_SESSION_PCT_PER_MIN`) is measured against. RECOMPUTED from the line's
    /// `session_delta_pct` + `elapsed_secs`, not read from its rendered `session_pct_per_min`, whose
    /// `{:.2}` floors a slow climb to `0.00` (issue #1158). Roster-wide (no per-account split, like
    /// every other SLI here); negatives (window resets) and malformed or absent ingredients are
    /// dropped at parse. Keeps the constant honest: when the real peak outruns it, the `v_peak`
    /// coupling bound is silently too loose, exactly as `TAIL_MARGIN` is kept honest by the #595
    /// landing SLI.
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
    /// Every `blind_enter` line in view (issues #583/#591) — the OPENING halves of the UNCENSORED
    /// episode record, kept RAW here and paired in [`fold_blind_episodes`] at aggregation (the
    /// ingredients-here / verdict-there split every sibling SLI in this module uses).
    blind_entries: Vec<BlindEntry>,
    /// Every `blind_exit` line in view (issues #583/#591) — the CLOSING halves.
    blind_exits: Vec<BlindExit>,
    /// Lines of the uncensored pair this reader could not place: a missing or unparseable `ts=` /
    /// `acct=` (either half) or `duration_secs=` (an exit). DISCLOSED rather than silently dropped —
    /// an undisclosed drop is a missing denominator, the survivorship failure this module guards
    /// against everywhere else.
    blind_pair_malformed: usize,
    /// The DISTINCT accounts a refresh cycle reported `outcome=dead` for (issue #881) — the
    /// refresh-token-loss population. A [`BTreeSet`] because the same loss is observed repeatedly:
    /// on the live log one account's single lapse produced four `outcome=dead` lines across two
    /// mechanisms, so counting lines would report four losses where one account was lost.
    ///
    /// The handles are a join key used INTERNALLY only — never rendered, exactly like
    /// [`SwapOut::acct`] — so the roster-wide, secret-free output contract (issue #15) holds: the
    /// readout publishes this set's CARDINALITY, never its members.
    refresh_token_loss_accounts: BTreeSet<String>,
    /// Every `outcome=dead` line in view, split by the refresh mechanism that observed it (issue
    /// #881) — the raw evidence count behind [`Inputs::refresh_token_loss_accounts`], and the reason this
    /// readout can say WHERE a loss was seen rather than only that one happened.
    refresh_token_loss_by_mechanism: RefreshTokenLossByMechanism,
    /// `event=credential_unrecoverable` count (issue #261): the terminal latch fired once a
    /// QUARANTINED account's sweep-refresh came back dead, i.e. automated recovery is exhausted and
    /// only an operator re-login can revive it — the same `sessiometer login` this readout renders
    /// as the cure (the daemon's own `status` cue names the `claude /login` it wraps).
    ///
    /// Counted BESIDE the `outcome=dead` observations above, never as the predicate itself: it
    /// requires an account to be quarantined FIRST, so it is strictly rarer than the loss it
    /// confirms — zero across the whole 13,983-line live log while six `outcome=dead` lines sat in
    /// the same file. A predicate keyed on this event ALONE would have matched nothing and passed
    /// every test that only asserts "no false positives" (the issue #719 inert-predicate lesson,
    /// where a drafted `hold == from` rule matched 0 of 11 relief swaps).
    refresh_token_loss_confirmed: u32,
    /// The latest `ts=` seen ANYWHERE in the folded text — the OBSERVATION HORIZON a
    /// never-recovered episode's right-censored floor is measured to (issue #591).
    ///
    /// Data-derived, deliberately NOT the wall clock: this verb is otherwise a pure function of the
    /// log text (`run` reads the clock only to resolve `--since`), and a horizon read from the clock
    /// would make the same text fold to a different number on every invocation — and would silently
    /// count the gap since the daemon last wrote as blindness. Taken over ALL lines, not just the
    /// blind family, so a quiet account's open episode is still measured to the log's real end.
    horizon_ts: Option<i64>,
}

/// WHICH refresh mechanism observed each refresh-token loss (issue #881) — the durable families
/// that carry a [`crate::observability::RefreshEventOutcome`], counted separately.
///
/// Published rather than summed away because the buckets cover DIFFERENT parts of the fleet and
/// different causes, so the split is the operator's first diagnostic: a loss seen only on
/// `keep_warm` is the ACTIVE account lapsing under a live session, one seen only on `sweep` is a
/// parked spare rotting unnoticed, `poll_retry` is a parked account caught at its first usage-401,
/// and `parked_recovery` is a recovery attempt that did NOT fix the credential. Same total,
/// different problem — and a different next action.
///
/// It is also this readout's own evidence that the classification predicate is not inert: the
/// counts show the union genuinely spans several producers instead of resting on one that may never
/// fire (issue #719's lesson — see [`Inputs::refresh_token_loss_confirmed`]).
///
/// The buckets are NOT one-per-event-name: `event=poll_refresh` splits in two on its `trigger=`
/// (issue #1367). See [`RefreshTokenLossByMechanism::parked_recovery`] for why, and
/// [`parse_events`] for the sub-split that fills them.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
struct RefreshTokenLossByMechanism {
    /// `event=refresh outcome=dead` — the periodic isolated-refresh sweep (issue #105), which walks
    /// PARKED accounts on a cadence. The only mechanism that reaches an account nothing else touched.
    sweep: u32,
    /// `event=poll_refresh trigger=poll_401 outcome=dead` — the poll-path refresh-then-retry (issue
    /// #162/#255), fired on the FIRST usage-401 of a streak in the hope a merely-expired ACCESS
    /// token is revived. A `dead` here means the retry found the REFRESH token itself gone. Never
    /// the active account.
    poll_retry: u32,
    /// `event=poll_refresh trigger=recovery outcome=dead` — the issue #643 re-probe of a `Dead`
    /// PARKED credential, driven by the `restored` control signal. NOT a re-login count: that
    /// signal has two senders — a non-activating revive on the `login` verb ([`crate::capture`]),
    /// and a [`crate::poke`] cycle that proved a fresh token against a quarantined account, which
    /// involves no login anywhere in its path. Split out from `poll_retry` by issue #1367: it rides
    /// the same event name but nothing polled and no 401 occurred, so folding it in reported live
    /// traffic that never happened. A `dead` here says the recovery attempt did NOT take — the most
    /// actionable line in the set, and the one an operator is most likely to be looking for at the
    /// moment they read this.
    ///
    /// Deliberately the PARKED half only. The ACTIVE half of the same #643 fix renders
    /// `event=keep_warm trigger=recovery` and counts in `keep_warm`, which stays correct because
    /// that bucket is keyed on the mechanism's fleet coverage (the active account) rather than on a
    /// trigger — unlike `poll_retry`, whose own definition names a condition.
    parked_recovery: u32,
    /// `event=keep_warm outcome=dead` — the in-place ACTIVE-account keep-warm (issue #282), on any
    /// of its three triggers. A `dead` here is the sharpest signal in the set: the account serving
    /// live traffic cannot re-mint.
    keep_warm: u32,
}

impl RefreshTokenLossByMechanism {
    /// Every `outcome=dead` observation in view, across all mechanisms — the raw evidence count.
    /// Saturating, matching the per-mechanism counters' own overflow discipline.
    fn total(self) -> u32 {
        self.sweep
            .saturating_add(self.poll_retry)
            .saturating_add(self.parked_recovery)
            .saturating_add(self.keep_warm)
    }
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

/// One `blind_enter` line in view (issue #591) — the OPENING half of an uncensored episode.
#[derive(Debug, Clone, PartialEq)]
struct BlindEntry {
    /// The entry instant: the origin a never-recovered episode's right-censored floor measures from.
    ts: i64,
    /// The line's ordinal in the folded text — the tie-break that keeps same-`ts` edges in TRUE log
    /// order. `ts=` has whole-second resolution, so one account's exit and its next entry can share a
    /// timestamp; ordering those two wrong turns a real never-recovered episode into a phantom
    /// restart and zeroes its censored floor. See [`fold_blind_episodes`].
    seq: usize,
    /// The account UUID (`acct=`) — the pairing key. Used INTERNALLY only, never rendered (the #15
    /// roster-wide-bare-numbers invariant this whole readout keeps).
    acct: String,
    /// Whether the pre-blind anchor sat at/over the session trigger, carried from the entry line so
    /// this family filters exactly as the `blind_window` SLIs do.
    near_limit: bool,
}

/// One `blind_exit` line in view (issue #591) — the CLOSING half.
///
/// SELF-CONTAINED: the daemon measured `duration_secs` against its own per-account anchor and tagged
/// `swapped_away` at the exit edge, so an exit is valid evidence of a COMPLETED episode whether or
/// not its entry is also in view — a `--since` cutoff or a rotated log routinely severs the entry.
/// This is why the fold reads the exit-derived facts off the LINES and uses pairing only for what a
/// single line cannot answer (which ENTRIES never closed).
#[derive(Debug, Clone, PartialEq)]
struct BlindExit {
    ts: i64,
    /// Log-order tie-break, as [`BlindEntry::seq`].
    seq: usize,
    acct: String,
    near_limit: bool,
    /// How long the account was blind, anchor → this recovery. A MEASURED value (contrast the
    /// censored floor a never-recovered episode contributes).
    duration_secs: u64,
    /// `swapped_away=true` — active at entry, not active now. The censoring tail `blind_window` is
    /// structurally blind to (it is `active == Some(i)`-guarded off the swap-dropped `last_good`).
    swapped_away: bool,
}

/// The UNCENSORED blind-episode census (issue #591), folded from the `blind_enter` / `blind_exit`
/// pair (issue #583) instead of the recovery-edge, active-only `blind_window`.
///
/// This is the ADDITIVE counterpart to [`BlindWindowCensus`], not its replacement — the two count
/// different populations on purpose, and the gap between them IS the censoring this readout exists
/// to disclose. See the module-level § "Which event the readout reads" for the routing decision.
///
/// **Right-censoring is carried, never averaged away.** A never-recovered episode has no exit line
/// and therefore NO measured duration. Its contribution is a LOWER BOUND (`horizon − entry`), kept
/// in its own field so the observed and the censored parts are never silently summed into a figure
/// that reads as a total. A consumer that wants the honest answer reads
/// `observed + censored_floor` and knows it is a floor; a consumer that wants only measured time
/// reads `observed`. Neither is fabricated from the other.
#[derive(Debug, Default, PartialEq)]
struct BlindEpisodes {
    /// Every `blind_enter` line in view.
    n_entered: usize,
    /// Every `blind_exit` line in view.
    n_exited: usize,
    /// Exits tagged `swapped_away=true` — `blind_window`'s SECOND censoring tail, now counted.
    n_swapped_away: usize,
    /// Entries still OPEN at the horizon, after restart orphans are removed — `blind_window`'s FIRST
    /// censoring tail (the episode that is invisible precisely when it is worst).
    n_never_recovered: usize,
    /// Entries superseded by a LATER entry for the same account with no exit between: the anchor was
    /// lost out-of-band (daemon restart — `blind_anchor` is in-memory — or a roster-reconcile drop),
    /// so the episode's end is simply unknown. Counted SEPARATELY and never as "never recovered",
    /// which would inflate the worst tail with ordinary restarts.
    n_anchor_lost: usize,
    /// Exits whose entry is not in view (a `--since` cutoff or a rotated log severed it). Their
    /// duration and `swapped_away` tag are still valid (see [`BlindExit`]); disclosed so the
    /// entry/exit counts visibly need not balance.
    n_exit_without_enter: usize,
    /// Pair lines this reader could not place. See [`Inputs::blind_pair_malformed`].
    n_malformed: usize,
    /// `near_limit=true` episodes witnessed: completed exits plus open (censored) entries.
    near_limit_episodes: usize,
    /// Σ `duration_secs` over `near_limit=true` exits — MEASURED blind time.
    near_limit_observed_secs: u64,
    /// Σ `horizon − entry` over `near_limit=true` OPEN episodes — a right-censored FLOOR, kept
    /// apart from the measured sum above.
    near_limit_censored_floor_secs: u64,
}

impl BlindEpisodes {
    /// `observed + censored floor` — a LOWER BOUND on true near-limit blind time, never a total (the
    /// censored part can only grow). Derived, never stored, so the two parts and their sum cannot
    /// disagree — and derived in ONE place, so the human and JSON surfaces cannot disagree either.
    fn near_limit_total_secs_lower_bound(&self) -> u64 {
        self.near_limit_observed_secs
            .saturating_add(self.near_limit_censored_floor_secs)
    }
}

/// Pair the raw `blind_enter` / `blind_exit` halves into the uncensored census (issue #591).
///
/// Exit-derived facts (`duration_secs`, `swapped_away`, `near_limit`) are read off the LINES; the
/// pairing walk answers only what one line cannot — which entries never closed, and whether an
/// unclosed one is a genuine never-recovered episode or a restart orphan.
///
/// **The restart disambiguator.** [`crate::daemon`]'s `note_blind_episode` is a strictly-alternating
/// per-account state machine — `(None, Err)` opens, `(Some, Ok)` closes, and a held-blind tick or an
/// ordinary live poll matches no edge — so a SECOND entry for one account with no exit between is
/// IMPOSSIBLE while the anchor is intact. Observing one therefore PROVES the anchor was lost
/// out-of-band, which is exactly the daemon-restart case (`blind_anchor` is in-memory). That signal
/// is used deliberately here instead of the `diag=start` marker issue #591 proposes: `diag=start`
/// rides the OPERATOR-facing diagnostic channel, which is written to stderr and gated behind
/// `--verbose` (default [`crate::observability::Verbosity::Quiet`] emits nothing), so it is absent
/// from the durable event log this reader folds and cannot disambiguate anything here.
fn fold_blind_episodes(
    entries: &[BlindEntry],
    exits: &[BlindExit],
    horizon_ts: Option<i64>,
    malformed: usize,
) -> BlindEpisodes {
    let mut out = BlindEpisodes {
        n_entered: entries.len(),
        n_exited: exits.len(),
        n_malformed: malformed,
        ..BlindEpisodes::default()
    };

    for x in exits {
        if x.swapped_away {
            out.n_swapped_away += 1;
        }
        if x.near_limit {
            out.near_limit_episodes += 1;
            out.near_limit_observed_secs =
                out.near_limit_observed_secs.saturating_add(x.duration_secs);
        }
    }

    enum Edge<'a> {
        Enter(&'a BlindEntry),
        Exit(&'a BlindExit),
    }

    // One timeline over BOTH halves, so the per-account walk sees the edges in the order the daemon
    // emitted them. Sorting makes the fold independent of the log's own append order (a rotated or
    // concatenated log can interleave out of order).
    //
    // The key is `(ts, seq)`, NOT `ts` alone. `ts=` has whole-second resolution, so an exit and the
    // next entry for one account genuinely can share a timestamp — and this Vec is built as ALL
    // entries then ALL exits, which destroys log order BEFORE the sort. Stability preserves that
    // construction order for ties, not the log's, so a bare `ts` key would reorder a same-second
    // `enter → exit → enter` into `enter → enter → exit`: a phantom anchor-loss, a DROPPED
    // never-recovered episode, and its censored floor silently zeroed — understating exactly the
    // worst tail this census exists to surface. `seq` is the line ordinal, so ties resolve to true
    // log order.
    let mut timeline: Vec<Edge> = entries
        .iter()
        .map(Edge::Enter)
        .chain(exits.iter().map(Edge::Exit))
        .collect();
    timeline.sort_by_key(|edge| match edge {
        Edge::Enter(e) => (e.ts, e.seq),
        Edge::Exit(x) => (x.ts, x.seq),
    });

    let mut pending: BTreeMap<&str, &BlindEntry> = BTreeMap::new();
    for edge in &timeline {
        match edge {
            // Replacing a pending entry is the anchor-loss proof documented above.
            Edge::Enter(e) => {
                if pending.insert(e.acct.as_str(), e).is_some() {
                    out.n_anchor_lost += 1;
                }
            }
            Edge::Exit(x) => {
                if pending.remove(x.acct.as_str()).is_none() {
                    out.n_exit_without_enter += 1;
                }
            }
        }
    }

    // Whatever is still open at the horizon never recovered IN VIEW. Right-censored: a LOWER BOUND,
    // never a measured duration, and never folded into the observed sum.
    for e in pending.values() {
        out.n_never_recovered += 1;
        if e.near_limit {
            out.near_limit_episodes += 1;
            if let Some(h) = horizon_ts {
                out.near_limit_censored_floor_secs = out
                    .near_limit_censored_floor_secs
                    .saturating_add(h.saturating_sub(e.ts).max(0) as u64);
            }
        }
    }

    out
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
    // The pending `all_exhausted` hold (issue #719): the `hold=` account of the latest
    // `all_exhausted` event whose episode has not yet ended. `all_exhausted` is edge-triggered (one
    // event on entering the exhausted state). `hold=` names the soonest-returning SPARE the daemon
    // holds out for (not the pinned-active account), so the relief swap lands on it — a
    // `reason=session` swap whose `to=` equals this pending hold resolved a capacity hold (no viable
    // target); its OUTGOING account was pinned at the ceiling, NOT a reaction-latency miss. Reset to
    // `None` on BOTH edges that end an episode, so a hold never leaks past the one it belongs to:
    // ANY swap (the relief) and `all_exhausted_cleared` (the LEAVE edge — issue #828). The arms
    // below carry why each edge ends it.
    let mut exhausted_hold: Option<String> = None;
    for (seq, line) in text.lines().enumerate() {
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

        // The observation horizon (issue #591): the latest `ts=` in the folded view. Tracked over
        // EVERY line, not just the blind family, so an open episode on an otherwise quiet account is
        // still measured to the log's real end.
        //
        // Placed AFTER the window gate so the horizon is always the horizon of what was actually
        // FOLDED. Today that is indistinguishable from the whole log's maximum: `--since` is a pure
        // LOWER bound (`ts >= cutoff`, no upper bound), so the largest in-window `ts` IS the largest
        // `ts` overall. The placement is therefore DEFENSIVE, not currently observable — it is what
        // keeps the censored floor honest if an upper-bounded window is ever added. Said precisely
        // so a later reader does not mistake it for a live invariant and "pin" it with a test that
        // cannot fail.
        if let Some(ts) = fields.get("ts").copied().and_then(epoch_from_rfc3339) {
            inputs.horizon_ts = Some(inputs.horizon_ts.map_or(ts, |h: i64| h.max(ts)));
        }

        match fields.get("event").copied() {
            Some("swap") => {
                // Re-activation edge (issue #595): ANY swap re-activates its INCOMING account, so
                // record its `to=` BEFORE the reason-specific `continue`s below — a previously-parked
                // account's landing window closes the instant it becomes active again
                // (`active_at != acct`), whatever the reason of the swap that revives it.
                record_reactivation_edge(&mut inputs, &fields, "to");
                // Capacity-hold classification (issue #719): a swap is the relief edge for a pending
                // `all_exhausted` hold. The daemon's `all_exhausted hold=` names the SOONEST-RETURNING
                // SPARE it is holding out for — `all_exhausted_relief` skips the active account and
                // returns a blocked peer — so the relief swap lands ON that spare: `hold=` matches the
                // swap's `to=`, NOT its `from=`. (Verified against the live event log: hold==to in 11/11
                // relief swaps, hold==from in 0.) The OUTGOING account (`from=`) is the one that was
                // pinned at the ceiling while no target was viable, so its high swap-out `session_pct`
                // is a fleet-CAPACITY limit, not a reaction-latency miss — that is what the `held` flag
                // segregates out of the #363 gate. Capture the match BEFORE the reason-specific
                // `continue`s, then clear the pending hold unconditionally — a swap of ANY reason ends
                // the hold context (the daemon re-emits `all_exhausted` if the hold persists), so the
                // flag never leaks to a later swap.
                let held_by_exhaustion = matches!(
                    (exhausted_hold.as_deref(), fields.get("to").copied()),
                    (Some(hold), Some(to)) if hold == to
                );
                exhausted_hold = None;
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
                // OPERATOR-INITIATED swaps (issue #1453): `reason=manual` / `reason=forced`, an
                // operator's own `sessiometer use`. Anchored for the LANDING reconstruction only,
                // then skipped past the decision-point accounting below.
                //
                // Until this arm existed they fell through the `reason != session` filter and were
                // graded by nothing at all — which is a hole precisely where it hurts most: the
                // defining event of a post-swap observation incident is the operator noticing an
                // at-limit active and rescuing it by hand. The rescue was invisible to every SLI in
                // this file, so a fix to the observation gap would move no readout and could not be
                // shown to have worked.
                //
                // They enter as anchors WITHOUT a decision reading (see `SwapOut::decision_pct`),
                // which is what lets them be graded without touching what the daemon records: the
                // `session_pct=0` on the line stays deliberate and unread, and the landing is
                // reconstructed from the usage samples exactly as a session swap's is. Kept in
                // their own partition, so no figure published before this bump changes meaning.
                if matches!(fields.get("reason").copied(), Some("manual" | "forced")) {
                    if let (Some(ts), Some(from)) = (
                        fields.get("ts").copied().and_then(epoch_from_rfc3339),
                        // The adopt-target recovery emits `from=(unknown)` — a redaction-safe
                        // SENTINEL, not a roster label, for the case where the departing account
                        // could not be named at all (`crate::use_account`). It shares no namespace
                        // with `Sample.acct`, so an anchor keyed on it can never join a usage
                        // sample: it would sit in `n_unmeasured` on every run forever, reported as a
                        // sample-coverage gap the operator could go looking for and never find. Drop
                        // it here, on the same tolerant terms a swap line missing `from=` is
                        // dropped — an anchor that cannot open a window is not an anchor.
                        fields
                            .get("from")
                            .copied()
                            .filter(|from| *from != ADOPT_UNKNOWN_FROM),
                    ) {
                        inputs.operator_swaps.push(SwapOut {
                            ts,
                            acct: from.to_owned(),
                            decision_pct: None,
                            held: held_by_exhaustion,
                        });
                    }
                    continue;
                }
                // SESSION-triggered swaps only. A weekly swap fires while session is BELOW its
                // trigger, so its session_pct is a low, incidental value — not a session
                // overshoot — and weekly cadence is out of scope for this session-limit-latency
                // increment (prd-swap-latency.md §6). manual/forced (handled above, and
                // `session_pct=0`) and emergency_swap (no session_pct field) are likewise not
                // session overshoots.
                if fields.get("reason").copied() != Some("session") {
                    continue;
                }
                if let Some(pct) = fields.get("session_pct").and_then(|v| v.parse::<u8>().ok()) {
                    // Partition (issue #719): a capacity-held swap-out is a fleet-capacity limit, not
                    // a reaction-latency miss — it feeds the held distribution, never the #363 gate.
                    if held_by_exhaustion {
                        inputs.swap_out_held_pcts.push(f64::from(pct));
                    } else {
                        inputs.swap_out_pcts.push(f64::from(pct));
                    }
                    // Landing anchor (issue #595): the reconstruction needs the WHO (`from=`) and
                    // WHEN (`ts=`) to window this parked account's post-swap samples. A line missing
                    // either still fed the pct above, but cannot open a window — not an anchor. The
                    // `held` flag (issue #719) carries the same capacity-hold classification forward so
                    // the landing SLI segregates it too.
                    if let (Some(ts), Some(from)) = (
                        fields.get("ts").copied().and_then(epoch_from_rfc3339),
                        fields.get("from").copied(),
                    ) {
                        inputs.session_swaps.push(SwapOut {
                            ts,
                            acct: from.to_owned(),
                            decision_pct: Some(pct),
                            held: held_by_exhaustion,
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
            // The UNCENSORED blind-episode pair (issues #583/#591). Kept RAW here and paired in
            // [`fold_blind_episodes`] at aggregation: which entry never closed is a WHOLE-VIEW
            // property, not a per-line one, so it cannot be folded in this single forward pass.
            //
            // These feed ONLY the `blind_episodes` census — never the `blind_window`-derived
            // `time_blind_near_limit_secs` / `near_limit_reconciliations` above. The two families
            // OVERLAP by construction (an active account that recovers emits `blind_window` AND
            // `blind_exit` for the one episode), so summing them into a single figure would
            // double-count exactly the episodes both can see. Keeping the populations apart is the
            // whole point of the issue #591 routing decision; the FIXTURE_LOG assertions pin it.
            Some("blind_enter") => {
                if let (Some(ts), Some(acct)) = (
                    fields.get("ts").copied().and_then(epoch_from_rfc3339),
                    fields.get("acct").copied(),
                ) {
                    inputs.blind_entries.push(BlindEntry {
                        ts,
                        seq,
                        acct: acct.to_owned(),
                        near_limit: fields.get("near_limit").copied() == Some("true"),
                    });
                } else {
                    inputs.blind_pair_malformed += 1;
                }
            }
            Some("blind_exit") => {
                if let (Some(ts), Some(acct), Some(duration_secs)) = (
                    fields.get("ts").copied().and_then(epoch_from_rfc3339),
                    fields.get("acct").copied(),
                    fields
                        .get("duration_secs")
                        .and_then(|v| v.parse::<u64>().ok()),
                ) {
                    inputs.blind_exits.push(BlindExit {
                        ts,
                        seq,
                        acct: acct.to_owned(),
                        near_limit: fields.get("near_limit").copied() == Some("true"),
                        duration_secs,
                        swapped_away: fields.get("swapped_away").copied() == Some("true"),
                    });
                } else {
                    inputs.blind_pair_malformed += 1;
                }
            }
            Some("usage_backoff") => match fields.get("class").copied() {
                Some("rate_limited") => inputs.rate_limited = inputs.rate_limited.saturating_add(1),
                Some("transient") => inputs.transient = inputs.transient.saturating_add(1),
                _ => {}
            },
            Some("usage_backoff_cleared") => inputs.cleared = inputs.cleared.saturating_add(1),
            // The all-accounts-exhausted hold (issue #719): the daemon wants to swap the active
            // account away but no viable target exists, so it HOLDS the active account (pinned at the
            // ceiling) until the soonest peer resets. Edge-triggered — one event on entering the state,
            // naming the pinned account in `hold=`. Record it as the pending hold; the next swap of
            // `from=`==`hold=` resolved it and is a CAPACITY limit, not a reaction-latency miss (#363
            // explicitly excludes the all-exhausted condition). `cause=`/`resets_at=` are informational.
            Some("all_exhausted") => exhausted_hold = fields.get("hold").map(|h| h.to_string()),
            // The matching LEAVE edge (issue #828, durable since issue #800). A hold does not only
            // end in a swap: the daemon leaves the no-viable-target state on ANY non-`NoViableTarget`
            // tick, so a plain `Hold` tick (the active account's own window resetting, say) ends the
            // episode with no swap at all and emits only this marker. Closing the pending hold here
            // stops a stale `hold=` from outliving its episode and mislabeling a later, unrelated
            // swap onto that same spare as capacity-held — which would silently drop a genuine
            // reaction-latency sample out of the #363 gate.
            //
            // Inert on a RELIEF swap's own tick, by emission order: the daemon pushes the clear
            // AFTER `decide_action` returns, so the swap arm above has already captured
            // `held_by_exhaustion` and reset the hold by the time this line is read.
            Some("all_exhausted_cleared") => exhausted_hold = None,
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
            // carries the account's climb rate between its last two readings, normalized to %/min
            // (issue #449). The rate is RECOMPUTED here from the raw ingredients the line also
            // carries — `session_delta_pct` and `elapsed_secs` — never parsed back out of the
            // rendered `session_pct_per_min` field (issue #1158).
            //
            // THE DISTINCTION THAT MATTERS, because getting it wrong is what hid a real drop: a
            // `0.00` in the rendered field is TWO different facts wearing one spelling.
            //   - The EMITTER's zero — the session dimension genuinely did not move
            //     (`session_delta_pct=0`). The emitter is not silent on it, because its gate is
            //     `(session_delta_pct != 0 || weekly_delta_pct != 0)`: a flat session alongside a
            //     moving WEEKLY dimension still emits. Correctly not a climb.
            //   - The RENDERER's zero — a measured climb that `{:.2}` floored, which happens
            //     whenever `session_delta_pct / minutes < 0.005` (for the smallest non-zero delta,
            //     any interval past 12 000 s). The climb is right there on the same line.
            // Reading the rendered field cannot tell these apart; reading the ingredients does not
            // have to. `elapsed_secs` is `saturating_duration_since` with only a `> 0` guard at the
            // call site, so nothing bounds the interval from above.
            //
            // The `> 0` filter itself is unchanged and still load-bearing: NEGATIVE rates are
            // dropped because a negative delta is a session-window RESET (usage fell because the
            // 5 h window rolled), not a climb — folding it in would drag the distribution down and
            // understate the peak this SLI exists to catch. A malformed or absent value is dropped
            // (the tolerant-drop precedent the sibling arms use), never defaulted to 0 — which also
            // keeps the pre-#449 lines that predate `elapsed_secs` out, exactly as before: they
            // carry no interval, so they have no rate to contribute.
            Some("usage_velocity") => {
                if let Some(rate) = fields
                    .get("session_delta_pct")
                    .and_then(|v| v.parse::<f64>().ok())
                    .zip(
                        fields
                            .get("elapsed_secs")
                            .and_then(|v| v.parse::<f64>().ok())
                            .filter(|secs| *secs > 0.0),
                    )
                    .map(|(delta_pct, elapsed_secs)| delta_pct / (elapsed_secs / 60.0))
                    .filter(|v| v.is_finite() && *v > 0.0)
                {
                    inputs.session_velocities.push(rate);
                }
            }
            // Refresh-token loss (issue #881). THE CLASSIFICATION PREDICATE: an account is lost to
            // refresh-token expiry/revocation exactly when a refresh cycle reports `outcome=dead` —
            // `RefreshEventOutcome::Dead`, "CC cleared the refresh token in place — the credential is
            // dead and needs an operator re-login". That outcome vocabulary is SHARED by the three
            // refresh mechanisms and by no other event, so the union of the three IS the predicate.
            //
            // VALIDATED AGAINST THE LIVE EVENT LOG, not assumed — the issue #719 discipline, where a
            // drafted `hold == from` rule was inert (0 of 11 relief swaps) and only a replay caught
            // it. Over 13,983 live lines an exhaustive `(event, outcome)` enumeration found
            // `outcome=dead` on EXACTLY these three families — sweep 1, poll_retry 3, keep_warm 2 —
            // and on no fourth producer; `event=login`'s own `outcome=` vocabulary
            // (`onboarded`/`revived`/`failed`/`cancelled`) cannot collide. The union therefore
            // matches 6 of 6 real dead-outcome lines. The tempting narrower predicate,
            // `event=credential_unrecoverable`, matched ZERO in the same file (see
            // `Inputs::refresh_token_loss_confirmed`) — it is counted below as corroboration,
            // never as the key.
            //
            // Those three families are still the whole predicate; what issue #1367 changed is that
            // `poll_refresh` no longer maps to one BUCKET. It carried a hard-coded
            // `trigger=poll_401` while issue #643 was already emitting it for a second, unrelated
            // condition, so its lines were a mixed population reported as one. The split below
            // separates them GOING FORWARD ONLY: every line written before #1367 renders the same
            // `poll_401` regardless of which origin produced it, so a recovery-driven death in that
            // stretch is indistinguishable from a poll-driven one, reads as `poll_retry`, and stays
            // that way. The historical split is wrong and cannot be repaired by re-reading the log —
            // the discriminating information was never written down. Read a `--since` window that
            // predates the fix accordingly, including the enumeration above.
            //
            // SCOPE BOUND, stated rather than glossed: `outcome=dead` is Claude Code's `invalid_grant`
            // scrub, which covers a token that LAPSED on its deadline and one that was REVOKED. The
            // signal that would split those is the `refreshTokenExpiresAt` horizon issue #878 reads,
            // and it is daemon-INTERNAL — `ExpiryHorizon` rides a non-`Serialize` reading and issue
            // #880 owns putting it on a durable Event. So this block attributes the LOSS CLASS, whose
            // members share one cure (`sessiometer login`) and one exclusion (not a swap-out
            // failure), and #880 is what later lets a sub-split name the deadline case. Narrowing to
            // "deadline lapse" today would key on a token no log carries: an inert predicate.
            Some(family @ ("refresh" | "poll_refresh" | "keep_warm")) => {
                if fields.get("outcome").copied() == Some("dead") {
                    let m = &mut inputs.refresh_token_loss_by_mechanism;
                    match family {
                        "refresh" => m.sweep = m.sweep.saturating_add(1),
                        // `poll_refresh` is the ONE family whose event name does not identify the
                        // mechanism on its own (issue #1367): the reactive #162 poll path and the
                        // #643 `restored` re-probe both emit it, so the sub-split reads `trigger=`.
                        // Keyed POSITIVELY on `recovery`; every other token — including a future
                        // `PollRefreshTrigger` variant this reader predates, and a line from a
                        // daemon old enough to have rendered the hard-coded literal — falls to
                        // `poll_retry`, which is what those lines actually say. That default is
                        // silent by nature: a variant whose lines are NOT poll-driven, added at the
                        // emitter and nowhere else, re-creates exactly the #1367 defect with every
                        // assertion green. What prices it is
                        // `poll_refresh_trigger_tokens_all_reach_a_named_bucket`, and it takes both
                        // of that test's layers to reach THIS line (issue #1386). Its
                        // `expected_bucket` fails to COMPILE until the variant is named — but names
                        // it there, in the test module, and answering it there is not an answer
                        // about this line. What reaches this line is the replay: since #1386 the
                        // list of variants replayed is checked against the enum's own source, so a
                        // new variant is rendered through the emitter and parsed back HERE, and the
                        // assertion fails whenever the bucket named for it is not the one this
                        // `else` hands it. A variant that really does belong in `poll_retry` needs
                        // no edit here, and is asked for none.
                        "poll_refresh" => {
                            if fields.get("trigger").copied() == Some("recovery") {
                                m.parked_recovery = m.parked_recovery.saturating_add(1);
                            } else {
                                m.poll_retry = m.poll_retry.saturating_add(1);
                            }
                        }
                        // `_` is `keep_warm` and nothing else — the arm's own pattern admits
                        // exactly these three tokens, so widening it means revisiting this line.
                        // Its own `trigger=` is deliberately NOT read: all three keep-warm triggers
                        // act on the ACTIVE account, which is what this bucket means.
                        _ => m.keep_warm = m.keep_warm.saturating_add(1),
                    }
                    // A line with no parseable `account=` still counts as an observation above but
                    // cannot join the distinct-account set — the tolerant-drop precedent the sibling
                    // arms use (a landing anchor missing `from=` feeds the pct and no window). The
                    // two figures are published together, so such a line is visible as the gap
                    // between them rather than silently inflating either.
                    if let Some(account) = fields.get("account").copied() {
                        inputs
                            .refresh_token_loss_accounts
                            .insert(account.to_owned());
                    }
                }
            }
            // The terminal confirmation (issue #261): automated recovery is EXHAUSTED for an already-
            // quarantined account. Corroborates the loss above; deliberately not the predicate, since
            // it requires quarantine first and so under-counts (0 live occurrences).
            Some("credential_unrecoverable") => {
                inputs.refresh_token_loss_confirmed =
                    inputs.refresh_token_loss_confirmed.saturating_add(1);
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

/// The capacity-held swap-out distribution (issue #719): the `session_pct` percentiles over the
/// `reason=session` swaps that resolved an `all_exhausted` hold — a fleet-CAPACITY limit (no viable
/// target), NOT a reaction-latency miss, so segregated from [`SwapOvershoot`] and carrying NO
/// target/`met` gate (#363 explicitly excludes the all-exhausted condition). Reported beside the
/// gate so the excluded population stays visible (and feeds the separate fleet-capacity problem)
/// rather than silently vanishing. Same cardinality-zero discipline: `None` with no held swaps.
#[derive(Debug, PartialEq)]
struct CapacityHeld {
    n: usize,
    p50: Option<u8>,
    p95: Option<u8>,
    p100: Option<u8>,
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
    /// Anchors excluded as `all_exhausted` capacity holds (issue #719): the outgoing account was
    /// pinned at the ceiling with no viable target, so it is a fleet-capacity limit, not a landing
    /// overshoot. Kept OUT of `swaps_total`/the percentiles/the breach classes above and counted
    /// here so the excluded population stays visible — the landing counterpart of
    /// [`CapacityHeld`].
    capacity_held: usize,
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

/// The OPERATOR-INITIATED landing partition (issue #1453): where each `reason=manual` /
/// `reason=forced` swap-out left its parked account — the same post-swap peak [`Landing`] measures,
/// over the population that readout excluded.
///
/// It excluded them for a good reason and a bad consequence. The `reason != session` filter is
/// correct for SLI 1: a manual swap records `session_pct=0` because it was not session-triggered,
/// so folding that `0` into the decision-point distribution would drag the percentiles toward a
/// number no daemon decision ever made. But the same filter runs ahead of the landing anchors, and
/// the landing reconstruction never reads `session_pct` at all — it joins the usage samples — so the
/// exclusion cost nothing it was protecting and hid the one event class an operator most needs
/// graded: their own manual rescue of an at-limit active, which is the defining action of a
/// post-swap observation incident.
///
/// Published BESIDE [`Landing`] rather than merged into it, the issue #719 precedent: a merge would
/// silently change what an already-published `landing.p95` counts, and the two populations answer
/// different questions — one grades the daemon's reaction, the other grades where the fleet ends up
/// when a human has to intervene.
///
/// Deliberately carries NO breach classes and NO `p100_met`. Both would need a decision reading to
/// be meaningful, and an operator swap has none: an SLO verdict here would be a gate on a number the
/// daemon never chose. `n_at_or_over_ceiling` is the honest remainder — how many rescues still ended
/// up at/over the ceiling — stated as a count rather than dressed as a class.
#[derive(Debug, PartialEq)]
struct OperatorLanding {
    /// Operator-initiated swap anchors in view (already `--since`-windowed), capacity-holds
    /// excluded — the coverage denominator.
    swaps_total: usize,
    /// Anchors with ≥1 post-swap sample of the parked account — the subject the percentiles
    /// summarize.
    n_measured: usize,
    /// Anchors with NO post-swap sample in the window: a sample-coverage gap, reported rather than
    /// fabricated as a `0` landing.
    n_unmeasured: usize,
    p50: Option<u8>,
    p90: Option<u8>,
    p100: Option<u8>,
    /// Measured episodes that landed at/over [`SLO_SWAP_P100_MAX`]. NOT a breach class — see the
    /// type doc: with no decision reading there is nothing to say the swap fired below it.
    n_at_or_over_ceiling: usize,
    /// Anchors excluded as `all_exhausted` capacity holds (issue #719), on the same terms
    /// [`Landing::capacity_held`] excludes them: the outgoing account was pinned with no viable
    /// target, so its at-ceiling landing is a fleet-capacity limit whoever pressed the button.
    capacity_held: usize,
}

/// The observed session-velocity distribution SLI (issue #608): percentiles of the session climb
/// rate RECOMPUTED from each `usage_velocity` line's own ingredients — `session_delta_pct` over
/// `elapsed_secs` — rather than parsed back out of the line's `{:.2}`-rendered
/// `session_pct_per_min` field, which floored a real climb to `0.00` and dropped it (issue #1158).
/// Measured against the assumed peak constant [`crate::swap::V_PEAK_SESSION_PCT_PER_MIN`]
/// that the `v_peak` coupling bound ([`crate::swap::peak_runway_reserve_bound`]) is calibrated on.
/// Its job is to keep that constant HONEST: the bound assumes no account climbs faster than `v_peak`,
/// so if the real peak (`p100`) outruns it, the config-load coupling is silently too loose and the
/// constant needs re-calibrating — the same "measure, don't trust the constant" discipline the #595
/// landing SLI provides for `TAIL_MARGIN`.
///
/// Percentiles are `None` when no positive velocity sample was observed — cardinality-zero is not a
/// passing distribution (the [`SwapOvershoot`] discipline), so the readout never asserts the constant
/// is honest on an empty subject. Rates are `f64` %/min, not the `u8` percents the swap-out SLIs
/// carry.
///
/// **The stored rates are never rounded** (issue #1172). Each surface renders them at its own width
/// — the wire at full `f64` precision, the human render at 4 dp — but the value these fields hold is
/// the recomputed rate as measured, because [`Self::v_peak_honest`] compares `p100` against a 2-dp
/// constant and rounding first would let a real peak that exceeds it read as honest. Why that is a
/// reachable false negative and not a rounding nicety: [`round_pp`] § "Where this is the wrong tool".
#[derive(Debug, PartialEq)]
struct ObservedPeak {
    /// Count of positive session-velocity samples in view.
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

/// One anchor set folded against the usage samples (issues #595 / #1453) — the shared half of the
/// landing reconstruction, so the `reason=session` gate and the operator-initiated partition are
/// measured the IDENTICAL way and any difference between them is a property of the population
/// rather than of two hand-copied loops. The CLASSIFICATION is deliberately not shared: what a
/// landing means depends on whether the swap had a decision reading, and only one of the two
/// populations does.
struct LandingFold {
    /// `(landing_pct, decision_pct)` for every anchor that had at least one post-swap sample, in
    /// anchor order. The decision reading rides along because the breach classes are a question
    /// about the PAIR — where it fired against where it landed — not about the landing alone.
    measured: Vec<(u8, Option<u8>)>,
    /// Anchors with NO post-swap sample in the window: a sample-coverage gap, reported honestly
    /// rather than fabricated as a `0` landing.
    n_unmeasured: usize,
    /// Anchors excluded as `all_exhausted` capacity holds (issue #719).
    capacity_held: usize,
}

/// Fold one set of swap anchors against the indexed usage samples.
///
/// Pure and total: no clock, no I/O. `by_acct` is the sample index (built once by the caller);
/// `reactivations` closes a parked window early when the account goes active again.
fn fold_landings(
    by_acct: &BTreeMap<&str, Vec<&Sample>>,
    reactivations: &[Reactivation],
    anchors: &[SwapOut],
) -> LandingFold {
    let mut fold = LandingFold {
        measured: Vec::new(),
        n_unmeasured: 0,
        capacity_held: 0,
    };
    for swap in anchors {
        // Capacity holds (issue #719) are excluded from the landing SLO entirely — the outgoing
        // account was pinned at the ceiling with no viable target (a fleet-capacity limit), so its
        // at-ceiling "landing" is not a reaction-latency overshoot. Counted separately, so it neither
        // inflates the percentiles nor masquerades as a breach.
        if swap.held {
            fold.capacity_held += 1;
            continue;
        }
        let window_end = swap.ts.saturating_add(LANDING_WINDOW_SECS);
        // The parked window closes at the earliest re-activation of THIS account after the swap, when
        // one falls inside the window — samples at/after it read the now-ACTIVE account, not the
        // parked tail (`active_at != acct`). No re-activation ⇒ the full bounded window.
        let effective_end = reactivations
            .iter()
            .filter(|si| si.acct == swap.acct && si.ts > swap.ts)
            .map(|si| si.ts.saturating_sub(1)) // strictly before the re-activation instant
            .min()
            .map_or(window_end, |before_reactivation| {
                window_end.min(before_reactivation)
            });
        // Peak absolute session over the parked window (readings strictly after the swap, through the
        // effective end). The `is_finite` guard drops a NaN/inf reading, which would otherwise reach
        // the clamp below and FABRICATE a landing — 255 for `+∞`, but 0 for `NaN` and `−∞`, since the
        // saturating float→int cast floors both — poisoning the issue #597 tail calibration either
        // way: once as a max-value breach, once as a spotless one.
        // `None` ⇒ no reading of the parked account in view — an unmeasured anchor.
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
                fold.measured.push((landing_pct, swap.decision_pct));
            }
            None => fold.n_unmeasured += 1,
        }
    }
    fold
}

/// Percentiles over one fold's measured landings. `None` when nothing was measured —
/// cardinality-zero is not a passing `0` (the [`SwapOvershoot`] discipline).
fn landing_percentiles(fold: &LandingFold) -> (Option<u8>, Option<u8>, Option<u8>) {
    let pcts: Vec<f64> = fold
        .measured
        .iter()
        .map(|(landing, _)| f64::from(*landing))
        .collect();
    let pct = |p: f64| -> Option<u8> {
        (!pcts.is_empty()).then(|| crate::percentile::percentile(&pcts, p) as u8)
    };
    (pct(0.50), pct(0.90), pct(1.0))
}

/// Reconstruct BOTH landing-point partitions (issues #595 / #1453) by joining the parsed swap
/// anchors with the raw usage samples. Pure and total: no clock, no I/O — the samples are read once
/// in [`run`] and passed in, so the whole aggregation stays a function of the two file contents (and
/// the `--since` cutoff, already applied to `inputs`).
///
/// Returns the two partitions TOGETHER because they share one sample index: building it twice would
/// be the only alternative, and splitting them into two entry points would invite the index to drift
/// apart from the reactivation edges it is joined against.
fn compute_landing(inputs: &Inputs, samples: &[Sample]) -> (Landing, OperatorLanding) {
    // Index the samples by roster label once, so each anchor scans only ITS account's readings
    // rather than re-sweeping the whole store. The label is the join key (swap `from=` ↔
    // `Sample.acct`) and stays INTERNAL — no label reaches the rendered output. Group order is
    // irrelevant: the peak below is an order-independent filter + `reduce(f64::max)` over the window.
    let mut by_acct: BTreeMap<&str, Vec<&Sample>> = BTreeMap::new();
    for s in samples {
        by_acct.entry(s.acct.as_str()).or_default().push(s);
    }

    let session = fold_landings(&by_acct, &inputs.reactivations, &inputs.session_swaps);
    let (p50, p90, p100) = landing_percentiles(&session);
    // The breach split is a property of the (decision, landing) PAIR: a swap already at/over the
    // ceiling when it fired is a gap-crossing (SLI 1 already shows it); one that fired below and
    // landed at/over is the post-swap committed tail this SLI exists to expose.
    let gap_crossing = session
        .measured
        .iter()
        .filter(|(_, decision)| decision.is_some_and(|d| d >= SLO_SWAP_P100_MAX))
        .count();
    let post_swap_tail = session
        .measured
        .iter()
        .filter(|(landing, decision)| {
            // `Some(d) if d < ceiling`, NOT `!(d >= ceiling)`: an anchor with NO decision reading
            // (`SwapOut::decision_pct`) is not a swap that "fired below" — it is one whose firing
            // point is unanswerable, and the negated form would silently classify it as a post-swap
            // tail. Unreachable from the session partition today, which only ever carries
            // `Some`; stated positively so it stays right if a future caller folds a mixed set.
            decision.is_some_and(|d| d < SLO_SWAP_P100_MAX) && *landing >= SLO_SWAP_P100_MAX
        })
        .count();
    let landing = Landing {
        // Clean reaction-latency anchors only (issue #719): the capacity-held anchors are excluded
        // from the coverage denominator so `n_measured + n_unmeasured == swaps_total` still holds.
        swaps_total: inputs.session_swaps.len() - session.capacity_held,
        n_measured: session.measured.len(),
        n_unmeasured: session.n_unmeasured,
        p50,
        p90,
        p100,
        gap_crossing,
        post_swap_tail,
        capacity_held: session.capacity_held,
    };

    let operator = fold_landings(&by_acct, &inputs.reactivations, &inputs.operator_swaps);
    let (op_p50, op_p90, op_p100) = landing_percentiles(&operator);
    // NOT a breach split. An operator swap carries no decision reading (`SwapOut::decision_pct`), so
    // "fired below and landed at/over" is unanswerable for it — a plain count of the landings that
    // reached the ceiling is the whole of what the evidence supports, and naming it that way keeps
    // it from being read as the `post_swap_tail` class it is not.
    let n_at_or_over_ceiling = operator
        .measured
        .iter()
        .filter(|(landing, _)| *landing >= SLO_SWAP_P100_MAX)
        .count();
    let operator_landing = OperatorLanding {
        swaps_total: inputs.operator_swaps.len() - operator.capacity_held,
        n_measured: operator.measured.len(),
        n_unmeasured: operator.n_unmeasured,
        p50: op_p50,
        p90: op_p90,
        p100: op_p100,
        n_at_or_over_ceiling,
        capacity_held: operator.capacity_held,
    };

    (landing, operator_landing)
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
/// ([`Self::n_swapped_away`] / [`Self::n_never_recovered`]) are structurally invisible to this event,
/// so they are never fabricated as `0`; since issue #591 they are POPULATED from the uncensored
/// `blind_enter` / `blind_exit` census ([`BlindEpisodes`]), which this SLI CONSUMES rather than
/// duplicates — the percentiles above still score the `blind_window` population, unchanged.
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
    /// Episodes the daemon SWAPPED AWAY from before they recovered — structurally unrecordable by
    /// the active-scoped `blind_window`. POPULATED since issue #591 from [`BlindEpisodes`], the
    /// uncensored pair's census, which this SLI consumes rather than duplicates. `None` only when
    /// that pair is absent from view (a log predating issue #583): the honest "unobservable",
    /// distinct from an observed `0`.
    n_swapped_away: Option<usize>,
    /// Episodes that NEVER recovered — invisible to `blind_window` for the mirrored reason: it fires
    /// on the RECOVERY edge, so an account that goes dark and stays dark emits nothing at all, and
    /// the episode is unrecorded precisely when it is worst. POPULATED since issue #591 from
    /// [`BlindEpisodes`], with restart orphans excluded ([`BlindEpisodes::n_anchor_lost`]) so a
    /// daemon restart cannot masquerade as a never-recovered episode. `None` on the same
    /// pair-absent condition as the sibling above.
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
///
/// # Where this is the wrong tool, and why that is not an inconsistency (issue #1172)
///
/// The invariant above is about a single stored value feeding both renderers — it is NOT a licence
/// to quantize any figure at aggregation. It is safe HERE because the blind-arm projection error is
/// only ever reported: nothing downstream compares it against a threshold, so rounding it changes
/// how it reads and nothing else.
///
/// [`ObservedPeak`] deliberately does NOT use this, and the reason is a correctness one rather than
/// a taste one. Its `p100` is not merely reported — [`ObservedPeak::v_peak_honest`] compares it
/// against [`crate::swap::V_PEAK_SESSION_PCT_PER_MIN`], a 2-dp constant (`6.95`). Rounding it here
/// would quantize the compared value onto the constant's own grid, so any real peak exceeding the
/// constant by less than half a quantum would land exactly ON it and read as honest. That is
/// reachable from an ordinary emittable line, not a contrived one: `session_delta_pct=19` (an `i16`)
/// over `elapsed_secs=164` is `6.9512…` %/min, which `round_pp` sends to exactly `6.95`, flipping
/// the verdict from "re-calibrate" to "ok" — a silent false negative on the one question that SLI
/// exists to answer, at precisely the boundary where it matters. Pinned by the test
/// `v_peak_honest_compares_the_unrounded_peak`, which also asserts the counterfactual so the trap
/// stays visible rather than merely avoided.
///
/// The divergence that made this question worth asking is settled on the DISPLAY side instead: see
/// the § "WHY 4 dp AND NOT 2" note in [`render_human`]'s SLI 1d arm.
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

/// The refresh-token-loss attribution (issue #881): accounts removed from the fleet because their
/// REFRESH token lapsed or was revoked, reported as their OWN class rather than folded into any
/// swap SLI.
///
/// **Why it is separate, and why that is not merely tidy.** An account lost this way is a
/// credential-LIFECYCLE event with a known operator cure — `sessiometer login` mints a new token —
/// so it is *pending an operator action*, not evidence that the swap machinery reacted late. A
/// reliability signal that mixed the two would move for a reason no amount of daemon work can fix,
/// and an operator reading a degraded swap-out SLO would go looking in the wrong place entirely.
/// Same argument, same shape, as issue #719's segregation of `all_exhausted` capacity-holds.
///
/// **The segregation is STRUCTURAL, which is stronger than #719's.** #719 had to *filter* a shared
/// population: capacity-holds and reaction-latency misses are both `reason=session` swaps, so a
/// classification rule decides which bucket each lands in — and a wrong rule silently re-baselines
/// the #363 gate. Here the two SWAP populations never meet: loss evidence rides `event=refresh` /
/// `event=poll_refresh` / `event=keep_warm`, while every swap SLI folds `event=swap`. A
/// refresh-family line therefore CANNOT reach [`Inputs::swap_out_pcts`] — not because a filter
/// excludes it, but because no code path connects them. That is the issue #881 acceptance
/// discharged by construction rather than by assertion.
///
/// **One shared input, named rather than glossed**: [`Inputs::horizon_ts`] is tracked OUTSIDE the
/// event match, so every `ts=`-bearing line advances it — refresh-family lines included, and they
/// did so before this attribution existed too, since the horizon has never been event-keyed. It is
/// the only figure a refresh-family line can influence, it does so identically on either side of
/// this change, and the influence is confined to a never-recovered episode's right-censored floor
/// (issue #591). So "no swap SLI moves" is exact, where the broader "no code anywhere reads both"
/// would not be.
///
/// Roster-wide and secret-free (issue #15): cardinalities and fixed labels, never a handle.
#[derive(Debug, PartialEq)]
struct RefreshTokenLoss {
    /// DISTINCT accounts observed lost — the loss population, and the figure to read as "how many
    /// accounts need a re-login". Deduplicated because one lapse is observed many times (see
    /// [`Inputs::refresh_token_loss_accounts`]).
    accounts: usize,
    /// Which mechanism saw each `outcome=dead` observation. Its
    /// [`RefreshTokenLossByMechanism::total`] is the raw evidence count; it is `>= accounts`
    /// whenever a loss was seen more than once, and the gap between the two is exactly that
    /// repetition (plus any line whose `account=` was unparseable).
    by_mechanism: RefreshTokenLossByMechanism,
    /// `credential_unrecoverable` LATCH EVENTS in view (issue #261) — the edge fired once an already-
    /// quarantined account's sweep-refresh came back dead, i.e. automated recovery is exhausted.
    ///
    /// An EVENT COUNT over the window, deliberately NOT a subset of [`RefreshTokenLoss::accounts`],
    /// and the two are independent in BOTH directions — stated plainly, because "confirmed" invites
    /// the subset reading. It commonly reads `0` while `accounts` is non-zero (the latch needs a
    /// prior quarantine, so it is strictly rarer than the loss it confirms — zero across the whole
    /// live log while six `outcome=dead` lines sat in the same file). It can also EXCEED `accounts`,
    /// or be non-zero while `accounts` is `0`: the count is per event, not per account, and under
    /// `--since` a window can catch a latch whose originating dead-refresh line fell before the
    /// cutoff. Neither reading is a defect — they answer different questions over the same window,
    /// which is exactly why they are published side by side rather than folded together.
    confirmed_unrecoverable: u32,
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
    /// The #719 capacity-held swap-out partition — `reason=session` swaps that resolved an
    /// `all_exhausted` hold, segregated from `swap_overshoot` (the #363 gate) as a fleet-capacity
    /// limit rather than a reaction-latency miss.
    capacity_held: CapacityHeld,
    /// The #539 velocity-projection covered-swap session_pct (`reason=velocity_preempt` percentiles).
    projective_swap_out_pct: ProjectiveSwapOutPct,
    /// The #595 landing-point overshoot — where `reason=session` swaps actually landed (post-swap
    /// peak of the parked account), reconstructed from the usage-sample store.
    landing: Landing,
    /// The #1453 operator-initiated landing partition — where an operator's own `use` swap left the
    /// account it parked. Same reconstruction, the population `landing` filters out.
    operator_landing: OperatorLanding,
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
    /// The issue #591 uncensored blind-episode census — the `blind_enter` / `blind_exit` population,
    /// published BESIDE the `blind_window`-derived figures above so the censoring gap is visible
    /// rather than silently folded away.
    blind_episodes: BlindEpisodes,
    rate_limit: RateLimit,
    /// The issue #881 refresh-token-loss attribution — the credential-lifecycle class, reported
    /// distinctly from every swap SLI above (which it cannot reach: disjoint event families).
    refresh_token_loss: RefreshTokenLoss,
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

    // The #719 capacity-held partition — the `reason=session` swaps that resolved an `all_exhausted`
    // hold, segregated from the reaction-latency gate above. Its own cardinality gate, so an empty
    // partition renders `None` rather than a fabricated `0`.
    let held_n = inputs.swap_out_held_pcts.len();
    let held_pct = |p: f64| -> Option<u8> {
        (held_n > 0).then(|| crate::percentile::percentile(&inputs.swap_out_held_pcts, p) as u8)
    };
    let capacity_held = CapacityHeld {
        n: held_n,
        p50: held_pct(0.50),
        p95: held_pct(0.95),
        p100: held_pct(1.0),
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

    // The #881 refresh-token-loss attribution — a straight carry-through of the parse-side counters,
    // with NO cardinality gate unlike the three blocks above: a zero here is a genuine reading
    // ("nothing was lost in view"), not the empty subject they withhold a figure for. Why this class
    // is segregated from every swap SLI, and why that segregation is structural: [`RefreshTokenLoss`].
    let refresh_token_loss = RefreshTokenLoss {
        accounts: inputs.refresh_token_loss_accounts.len(),
        by_mechanism: inputs.refresh_token_loss_by_mechanism,
        confirmed_unrecoverable: inputs.refresh_token_loss_confirmed,
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
    // The UNCENSORED blind-episode census (issue #591), folded from the `blind_enter` / `blind_exit`
    // pair — the ADDITIVE counterpart to the `blind_window`-derived SLIs above, never a replacement
    // for them (see the module-level § "Which event the readout reads").
    let blind_episodes = fold_blind_episodes(
        &inputs.blind_entries,
        &inputs.blind_exits,
        inputs.horizon_ts,
        inputs.blind_pair_malformed,
    );
    // Is the pair OBSERVABLE at all in this view? A log written before issue #583 — or a `--since`
    // window that predates it — carries no pair lines, and reporting `Some(0)` there would assert
    // "zero swapped-away episodes" when the truth is "unobservable". That is exactly the fabricated
    // zero the censored tails' `None` has always refused, so absence of the family keeps them `None`.
    //
    // MALFORMED lines deliberately do NOT count as observation: "present but unreadable" is
    // unobservable too. A view whose every pair line is corrupt yields nothing to count, so claiming
    // an observed `0` there would be the same fabrication in a subtler dress — the corrupt lines are
    // still DISCLOSED via `n_malformed`, which is the honest report. A view with even one readable
    // line IS observed, and its `n_malformed` discloses the rest.
    let pair_observed = blind_episodes.n_entered > 0 || blind_episodes.n_exited > 0;

    let blind_projection_error = BlindProjectionError {
        n_blind_windows: inputs.blind_window_census.total,
        n_projectable: inputs.blind_projections.len(),
        n_reconcilable: error_n,
        n_sentinel_excluded,
        n_below_arm_gate: inputs.blind_window_census.below_arm_gate,
        n_without_velocity: inputs.blind_window_census.without_velocity,
        n_malformed: inputs.blind_window_census.malformed,
        // The two censored tails, POPULATED (issue #591) from the uncensored pair this SLI consumes
        // rather than duplicates — the promise these fields' `None` has carried since issue #636.
        // Still `None` when the pair is absent from view: unobservable, not zero.
        n_swapped_away: pair_observed.then_some(blind_episodes.n_swapped_away),
        n_never_recovered: pair_observed.then_some(blind_episodes.n_never_recovered),
        p50: error_pct(0.50),
        p95: error_pct(0.95),
        p100: error_pct(1.0),
    };

    // Both landing partitions come out of one pass over one sample index (issue #1453).
    let (landing, operator_landing) = compute_landing(inputs, samples);

    Report {
        window,
        swap_overshoot,
        capacity_held,
        projective_swap_out_pct,
        landing,
        operator_landing,
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
        blind_episodes,
        rate_limit: RateLimit {
            rate_limited: inputs.rate_limited,
            transient: inputs.transient,
            cleared: inputs.cleared,
        },
        refresh_token_loss,
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

    // SLI 1 — swap-out session_pct percentiles vs targets. The #363 acceptance gate, measured over
    // CONTROLLABLE reaction-latency swaps only: `all_exhausted` capacity-holds (issue #719) are
    // segregated into the block below, since #363 excludes the all-exhausted condition (folding them
    // in dragged P100 to an unreachable 100).
    match (
        r.swap_overshoot.p50,
        r.swap_overshoot.p95,
        r.swap_overshoot.p100,
    ) {
        (Some(p50), Some(p95), Some(p100)) => {
            out.push_str(&format!(
                "swap-out session_pct (reason=session, reaction-latency), n={}\n",
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
        _ => out.push_str(
            "swap-out session_pct (reason=session, reaction-latency): no swaps observed\n",
        ),
    }
    // The #719 capacity-held partition — reason=session swaps that resolved an `all_exhausted` hold
    // (no viable target). A fleet-CAPACITY limit, NOT reaction-latency, so OUT of the #363 gate; shown
    // here (only when non-empty) so the excluded population stays visible. No target flags — it gates
    // nothing. All at/near the ceiling by construction (the active was pinned there).
    if let (n @ 1.., Some(p50), Some(p100)) =
        (r.capacity_held.n, r.capacity_held.p50, r.capacity_held.p100)
    {
        out.push_str(&format!(
            "capacity-held (reason=session, all_exhausted — no viable target; out of #363 scope), n={n}\n"
        ));
        out.push_str(&format!("  P50  = {p50}\n"));
        out.push_str(&format!("  P100 = {p100}\n"));
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
            // Capacity-holds (issue #719) are EXCLUDED from the percentiles + breach classes above,
            // so a `gap-crossing` count is now honest reaction-latency only. Surface the excluded
            // tally when non-zero so the segregation is visible on this SLI too.
            if r.landing.capacity_held > 0 {
                out.push_str(&format!(
                    "  capacity-held (all_exhausted, excluded — out of #363 scope): {}\n",
                    r.landing.capacity_held
                ));
            }
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

    // SLI 1c-op — the OPERATOR-INITIATED landing partition (issue #1453). Rendered immediately
    // beneath SLI 1c and never merged into it: the two are the same measurement over two
    // populations, and the whole point is that a reader can see the manual rescues WITHOUT the
    // daemon's own reaction figures moving. No `[ok]`/`[OVER]` flag on the P100 line — an operator
    // swap carries no decision reading, so there is no SLO here to pass or fail; the at-ceiling
    // count below is the finding, stated as a count.
    match (
        r.operator_landing.p50,
        r.operator_landing.p90,
        r.operator_landing.p100,
    ) {
        (Some(p50), Some(p90), Some(p100)) => {
            out.push_str(&format!(
                "landing-point session_pct, OPERATOR-initiated swaps (reason=manual/forced, window <= {LANDING_WINDOW_SECS}s)\n"
            ));
            out.push_str(&format!(
                "  measured n={} of {} operator swaps ({} with no post-swap sample)\n",
                r.operator_landing.n_measured,
                r.operator_landing.swaps_total,
                r.operator_landing.n_unmeasured
            ));
            out.push_str(&format!("  P50  = {p50}\n"));
            out.push_str(&format!("  P90  = {p90}\n"));
            out.push_str(&format!("  P100 = {p100}  (no SLO: a manual swap records no decision reading)\n"));
            out.push_str(&format!(
                "  landed >= {SLO_SWAP_P100_MAX}: {}  (not a breach class — where the fleet ended up when a human intervened)\n",
                r.operator_landing.n_at_or_over_ceiling
            ));
            if r.operator_landing.capacity_held > 0 {
                // Issue #719's terms, NOT "#363 scope" as the session block above says it: #363 is
                // the session-trigger reaction-latency umbrella, and this partition is the
                // `reason=manual`/`forced` population that umbrella never covered — so explaining
                // the exclusion by a scope this block is not in would be a non-sequitur.
                out.push_str(&format!(
                    "  capacity-held (all_exhausted, excluded — issue #719): {}\n",
                    r.operator_landing.capacity_held
                ));
            }
        }
        _ if r.operator_landing.swaps_total == 0 => {
            out.push_str(
                "landing-point session_pct, OPERATOR-initiated swaps: no gradeable reason=manual/forced swaps observed\n",
            );
            // A partition that is ENTIRELY capacity-held reaches here with `swaps_total == 0`, and
            // saying only "none observed" over a log that carried several would be false. The
            // excluded count is the whole of what there is to report, so report it.
            if r.operator_landing.capacity_held > 0 {
                out.push_str(&format!(
                    "  capacity-held (all_exhausted, excluded — issue #719): {}\n",
                    r.operator_landing.capacity_held
                ));
            }
        }
        _ => out.push_str(&format!(
            "landing-point session_pct, OPERATOR-initiated swaps: no post-swap samples in window ({} of {} operator swaps unmeasured)\n",
            r.operator_landing.n_unmeasured, r.operator_landing.swaps_total
        )),
    }
    out.push('\n');

    // SLI 1d — OBSERVED session velocity (issue #608): the live session_pct_per_min distribution vs
    // the assumed v_peak the swap-target reserve coupling bound is calibrated on. Keeps that constant
    // honest — if the real peak outruns v_peak, the config-load bound is silently too loose.
    //
    // WHY 4 dp AND NOT 2 (issue #1172). Both figures on the P100 line are widened together, because
    // the line is a COMPARISON and comparing two numbers printed at different widths is the confusion
    // being fixed. Two dp was too coarse on both of this SLI's jobs:
    //   - Reading a rate at all. A climb under 0.005 %/min printed `0.00` — the same floor that hid
    //     the sample from the SLI entirely until issue #1158 read the ingredients instead of the
    //     rendered field. Fixing the input while the readout still printed `0.00` left the fix
    //     invisible on the surface an operator actually reads.
    //   - Reading the verdict. `v_peak_honest` compares the UNROUNDED p100 against a 2-dp constant,
    //     so a peak exceeding it by less than half of the 2-dp quantum rendered as
    //     `6.95 … vs assumed v_peak 6.95 … [RECALIBRATE]` — the flag correct, the numbers beside it
    //     apparently contradicting it, and nothing to tell the reader which to believe.
    // 4 dp CLOSES that second band for every line this emitter can produce, which is the reason it
    // is 4 and not 3. As a format it does not: an excess below 0.00005 still prints as a tie. But
    // `session_delta_pct` is a difference of two `to_pct` values (`src/daemon/snapshot.rs:1072`
    // clamps each to 0..=100), so the numerator is an integer in -100..=100, and a tie additionally
    // needs the rate to sit above 6.95, which bounds `elapsed_secs`. Sweeping every integer
    // `(delta in 1..=100, elapsed in 1..=1_000_000)` pair that carries a real excess yet renders
    // peak and constant alike: 30 such pairs at 2 dp (the first is `(19, 164)`, which
    // `NEAR_BOUNDARY_LOG` is), 3 at 3 dp, and ZERO at 4. The first pair reaching the 4-dp band at
    // all needs `session_delta_pct = 248` — two and a half times what the emitter can produce.
    // So two equal-looking figures under `[RECALIBRATE]` are never a rendering artifact here, and
    // must not be discounted as one. The FLAG stays authoritative regardless — it is computed from
    // the stored value, never from these digits.
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
            out.push_str(&format!("  P50  = {p50:.4} %/min\n"));
            out.push_str(&format!("  P90  = {p90:.4} %/min\n"));
            out.push_str(&format!(
                "  P100 = {p100:.4} %/min  vs assumed v_peak {:.4} %/min  {}\n",
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

    // SLI 2 — time blind & near-limit, in BOTH populations (issue #591). The censored `blind_window`
    // figure keeps its exact wording and position — it is the one comparable across this log's whole
    // history — and the uncensored episode census is rendered immediately beneath it, so the
    // censoring gap is READ at a glance instead of inferred. Deliberately adjacent, not merged: the
    // two count different populations and a single blended number would hide exactly what this
    // readout exists to disclose.
    out.push_str(&format!(
        "time blind & near-limit: {}s (sum of blind_window duration_secs where near_limit=true)\n",
        r.time_blind_near_limit_secs
    ));
    let ep = &r.blind_episodes;
    if ep.n_entered == 0 && ep.n_exited == 0 && ep.n_malformed == 0 {
        // Deliberately does NOT name a cause: absence has three (no blind episodes occurred at all,
        // a `--since` window excluding them, or a log predating the record) and the reader cannot
        // tell them apart from here. Asserting the third would tell a healthy fleet its log is stale.
        out.push_str("  uncensored episodes: none in view\n");
    } else {
        out.push_str(&format!(
            "  uncensored episodes: entered={} exited={} swapped_away={} never_recovered={} anchor_lost={}\n",
            ep.n_entered, ep.n_exited, ep.n_swapped_away, ep.n_never_recovered, ep.n_anchor_lost
        ));
        out.push_str(&format!(
            "  near-limit blind time: >= {}s ({}s measured + {}s right-censored floor, n={})\n",
            ep.near_limit_total_secs_lower_bound(),
            ep.near_limit_observed_secs,
            ep.near_limit_censored_floor_secs,
            ep.near_limit_episodes
        ));
        // Never hide a drop: an undisclosed one is a missing denominator.
        if ep.n_exit_without_enter > 0 || ep.n_malformed > 0 {
            out.push_str(&format!(
                "  (exits with no entry in view: {}; unplaceable pair lines: {})\n",
                ep.n_exit_without_enter, ep.n_malformed
            ));
        }
    }
    out.push('\n');

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
    // The censoring disclosure, now QUANTIFIED where the uncensored pair supplies it (issue #591).
    // The percentiles above still score the recovered-only `blind_window` population — that has not
    // changed — but the reader can finally see HOW MUCH of the blind story sits outside it, instead
    // of only being told that some does.
    match (e.n_swapped_away, e.n_never_recovered) {
        (Some(swapped), Some(never)) => out.push_str(&format!(
            "  censoring: RECOVERED-ONLY — excludes {swapped} swapped-away and {never} never-recovered episodes (counted from the uncensored blind_enter/blind_exit pair above)\n"
        )),
        _ => out.push_str(
            "  censoring: RECOVERED-ONLY — swapped-away and never-recovered episodes are unobservable from blind_window (no uncensored blind_enter/blind_exit pair in view)\n",
        ),
    }
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
    out.push('\n');

    // SLI 7 — refresh-token loss (issue #881). Rendered as its OWN block rather than under the
    // swap-out SLI on purpose: #719's `capacity-held` sits inside that block because it is a
    // partition OF the same `reason=session` swaps, whereas this is a different population entirely
    // (refresh-family events), and nesting it there would read as a third swap bucket. The heading
    // therefore carries the exclusion in words, since the layout alone cannot say it.
    //
    // Always rendered, including at zero — unlike `capacity-held`, which is hidden when empty. Zero
    // accounts lost is a real and reassuring reading, not an absent measurement, and suppressing it
    // would leave an operator unable to tell "no losses" from "this readout does not track losses".
    let loss = &r.refresh_token_loss;
    let m = loss.by_mechanism;
    out.push_str(
        "refresh-token loss (credential lifecycle — cure is `sessiometer login`; NOT a swap-out failure, and folded into no SLI above)\n",
    );
    out.push_str(&format!(
        "  accounts observed lost: {} (from {} dead-refresh observations: sweep={} poll-retry={} parked-recovery={} keep-warm={})\n",
        loss.accounts,
        m.total(),
        m.sweep,
        m.poll_retry,
        m.parked_recovery,
        m.keep_warm,
    ));
    out.push_str(&format!(
        "  confirmed unrecoverable (automated recovery exhausted): {}\n",
        loss.confirmed_unrecoverable
    ));
    out
}

// --- rendering: JSON wire (schema:12) ---------------------------------------

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
    /// The #719 capacity-held partition (schema:9, additive) — `reason=session` swaps that resolved an
    /// `all_exhausted` hold, segregated from `swap_overshoot` (the #363 gate) as a fleet-capacity limit.
    capacity_held: CapacityHeldWire,
    /// The #539 velocity-projection covered-swap session_pct (schema:3; key renamed at schema:6, issue #635).
    projective_swap_out_pct: ProjectiveSwapOutPctWire,
    /// The #595 landing-point overshoot — where reason=session swaps actually landed (schema:4, additive).
    landing: LandingWire,
    /// The #1453 operator-initiated landing partition (schema:12, additive) — where a
    /// `reason=manual` / `reason=forced` swap left the account it parked. Its own block, so no
    /// schema:11 figure changes meaning.
    operator_landing: OperatorLandingWire,
    /// The #608 observed session-velocity distribution vs the assumed `v_peak` (schema:5, additive).
    observed_peak: ObservedPeakWire,
    time_blind_near_limit_secs: u64,
    false_preempt: FalsePreemptWire,
    /// The #539 false-projection SLI (schema:3, additive).
    false_projection: FalseProjectionWire,
    /// The #636 blind-arm projection error (schema:7, additive).
    blind_projection_error: BlindProjectionErrorWire,
    /// The issue #591 uncensored blind-episode census (schema:8, additive) — the `blind_enter` /
    /// `blind_exit` population beside the `blind_window`-derived figures above.
    blind_episodes: BlindEpisodesWire,
    rate_limit_neutrality: RateLimitWire,
    /// The issue #881 refresh-token-loss attribution (schema:10, additive) — the credential-
    /// lifecycle class, kept out of every swap SLI above. Placed LAST so every schema:9 key keeps
    /// its position as well as its meaning.
    refresh_token_loss: RefreshTokenLossWire,
}

/// The issue #881 refresh-token-loss block: accounts a lapsed/revoked REFRESH token removed from
/// the fleet — an operator-cured credential-lifecycle condition (`sessiometer login`), NOT a
/// swap-out failure. No `targets`/`met`: it gates nothing, and deliberately so — this is the
/// population a reliability gate must EXCLUDE, not one it should score.
///
/// Plain counts rather than nullable percentiles (the shape its swap-SLI siblings use): a zero here
/// is a real, meaningful reading — "no account was lost in view" — not the empty subject those
/// blocks guard against by withholding a figure.
#[derive(serde::Serialize)]
struct RefreshTokenLossWire {
    /// DISTINCT accounts observed lost — how many need a `sessiometer login`.
    accounts: usize,
    /// Total `outcome=dead` observations behind `accounts`. `>= accounts`; one lapse is commonly
    /// observed several times across mechanisms.
    observations: u32,
    /// The observation split by refresh mechanism — WHERE each loss was seen.
    by_mechanism: RefreshTokenLossByMechanismWire,
    /// `credential_unrecoverable` latch EVENTS in view (issue #261) — automated recovery exhausted
    /// for an already-quarantined account. An event count, NOT a subset of `accounts`: it commonly
    /// reads `0` while `accounts` is non-zero, and can equally exceed it or be non-zero at
    /// `accounts: 0`. See [`RefreshTokenLoss::confirmed_unrecoverable`] for why both directions are
    /// expected rather than contradictory.
    confirmed_unrecoverable: u32,
}

/// Which refresh mechanism observed each loss (issue #881) — different fleet coverage, different
/// operator conclusion. See [`RefreshTokenLossByMechanism`].
#[derive(serde::Serialize)]
struct RefreshTokenLossByMechanismWire {
    /// `event=refresh outcome=dead` — the periodic isolated-refresh sweep over PARKED accounts.
    sweep: u32,
    /// `event=poll_refresh trigger=poll_401 outcome=dead` — the first-usage-401 refresh-then-retry
    /// on a parked account.
    poll_retry: u32,
    /// `event=poll_refresh trigger=recovery outcome=dead` — the issue #643 re-probe the `restored`
    /// control signal drives (a `login` revive, or a `poke` that proved a fresh token against a
    /// quarantined account — no login on that path), split out of `poll_retry` by issue #1367. A
    /// `dead` here means the credential is still dead after that recovery attempt, NOT that a
    /// re-login failed. Lines written before that split carry the poll trigger whatever their
    /// origin, so this reads `0` over any window predating it while `poll_retry` still holds both
    /// populations.
    parked_recovery: u32,
    /// `event=keep_warm outcome=dead` — the in-place ACTIVE-account keep-warm. The sharpest signal.
    keep_warm: u32,
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

/// The #719 capacity-held swap-out block: the `reason=session` swaps excluded from the #363 gate as
/// `all_exhausted` capacity-holds (no viable target — a fleet-capacity limit). No `targets`/`met` — it
/// gates nothing; `null` percentiles with no held swaps (an empty subject is not a passing `0`).
#[derive(serde::Serialize)]
struct CapacityHeldWire {
    n: usize,
    p50: Option<u8>,
    p95: Option<u8>,
    p100: Option<u8>,
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
    /// Anchors excluded as `all_exhausted` capacity holds (schema:9, issue #719) — kept out of
    /// `swaps_total` / the percentiles / the breach classes, counted here so the excluded population
    /// stays visible. The landing counterpart of the top-level `capacity_held` block.
    capacity_held: usize,
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

/// Operator-initiated landing block (issue #1453): where `reason=manual` / `reason=forced` swaps
/// left the accounts they parked — the population every other block here excludes. `p50`/`p90`/
/// `p100` are `null` when no operator swap had a post-swap sample (an empty subject is not a passing
/// `0`).
///
/// No `classes` and no `p100_met`, unlike [`LandingWire`]: an operator swap records no decision
/// reading, so neither a breach split nor an SLO verdict has an input. `n_at_or_over_ceiling` is
/// what the evidence does support, and `ceiling` is stamped beside it so a consumer reads the count
/// against the value it was computed with rather than against today's constant.
#[derive(serde::Serialize)]
struct OperatorLandingWire {
    /// Operator-initiated swap anchors in view, capacity-holds excluded (the coverage denominator).
    swaps_total: usize,
    /// Anchors with >= 1 post-swap sample — the measured subject the percentiles summarize.
    n_measured: usize,
    /// Anchors with no post-swap sample in the window — a coverage gap, not a `0` landing.
    n_unmeasured: usize,
    p50: Option<u8>,
    p90: Option<u8>,
    p100: Option<u8>,
    /// The bounded post-swap window these landings were measured over (seconds).
    window_secs: i64,
    /// The ceiling `n_at_or_over_ceiling` was counted against — the SAME #455 ceiling SLI 1 uses.
    ceiling: u8,
    /// Measured episodes that landed at/over `ceiling`. A COUNT, not a breach class.
    n_at_or_over_ceiling: usize,
    /// Anchors excluded as `all_exhausted` capacity holds (issue #719), on the same terms
    /// `landing.capacity_held` excludes them.
    capacity_held: usize,
}

/// Observed session-velocity block (issue #608): the live session-velocity percentiles vs the
/// assumed peak constant the `v_peak` coupling bound is calibrated on. `p50`/`p90`/`p100`/`met.*` are
/// `null` with no positive velocity sample (an empty subject is not a passing distribution).
///
/// Rates are FULL-PRECISION %/min. Until issue #1158 they were 2 dp — not because anything rounded
/// them, but because the samples were parsed out of the log's `{:.2}`-rendered field and a
/// percentile SELECTS a sample rather than interpolating one, so 2-dp in meant 2-dp out. The samples
/// are now recomputed from `session_delta_pct` + `elapsed_secs`, so a rate like `0.004975…`
/// reaches the wire as written. Deliberately NOT re-rounded here: re-quantizing the calibration
/// readout would undo, at the surface a human reads to re-calibrate the constant, exactly the
/// precision loss #1158 removed.
///
/// The precision the human render shows beside it was reconciled by issue #1172, and the layer it
/// was reconciled AT is the point: the human render widened to 4 dp, while the aggregation — and
/// therefore this wire — kept the value unrounded. Both surfaces now render one shared value at
/// their own width rather than reporting two different ones, which is what the divergence was.
/// Rounding at aggregation instead would have quantized the value `v_peak_honest` compares
/// ([`round_pp`] § "Where this is the wrong tool"), and rounding in both renderers would have
/// re-quantized exactly the machine-readable figure a calibration script consumes.
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

/// The uncensored blind-episode census (schema:8, issue #591) — the `blind_enter` / `blind_exit`
/// population (issue #583), published BESIDE the `blind_window`-derived figures rather than
/// replacing them.
///
/// Read the two together: `time_blind_near_limit_secs` above is the RECOVERY-EDGE, ACTIVE-ONLY
/// measurement (censored at both tails, and comparable across this log's whole history);
/// `near_limit.total_secs_lower_bound` here is the uncensored floor over the same risk band. The
/// GAP between them is the censoring, and it is meant to be visible.
#[derive(serde::Serialize)]
struct BlindEpisodesWire {
    /// Every `blind_enter` line in view.
    n_entered: usize,
    /// Every `blind_exit` line in view.
    n_exited: usize,
    /// Exits tagged `swapped_away=true` — the tail `blind_window` structurally cannot record.
    n_swapped_away: usize,
    /// Entries still open at the horizon, restart orphans EXCLUDED (see `n_anchor_lost`).
    n_never_recovered: usize,
    /// Entries superseded by a later entry for the same account: the in-memory anchor was lost
    /// out-of-band (a daemon restart, or a roster-reconcile drop), so the episode's end is unknown.
    /// Counted apart from `n_never_recovered` so restarts cannot inflate the worst tail.
    n_anchor_lost: usize,
    /// Exits whose entry is not in view (a `--since` cutoff or a rotated log severed it), so the
    /// entry and exit counts visibly need not balance.
    n_exit_without_enter: usize,
    /// Pair lines that could not be placed (unreadable `ts=` / `acct=` / `duration_secs=`).
    n_malformed: usize,
    near_limit: BlindEpisodesNearLimitWire,
}

/// The near-limit slice of the episode census (schema:8, issue #591) — the censoring-aware answer to
/// "how long was the fleet blind while at risk?", with the MEASURED and CENSORED parts kept apart so
/// neither is fabricated from the other.
#[derive(serde::Serialize)]
struct BlindEpisodesNearLimitWire {
    /// Near-limit episodes witnessed: completed exits plus open (censored) entries.
    n_episodes: usize,
    /// MEASURED blind time: Σ `duration_secs` over near-limit exits.
    observed_secs: u64,
    /// The right-censored FLOOR contributed by episodes still open at the horizon — `horizon −
    /// entry` each. A never-recovered episode has no exit and therefore no measured duration, so
    /// this is a lower bound on time already elapsed, not an estimate of the episode's true length.
    censored_floor_secs: u64,
    /// `observed_secs + censored_floor_secs` — a LOWER BOUND on true near-limit blind time. Named so
    /// it cannot be mistaken for a total: the censored part can only grow.
    total_secs_lower_bound: u64,
}

/// Blind-arm projection-error block (issue #636): `projected − session_at_recovery` percentiles in
/// percentage points, SIGNED (positive = over-projected, negative = under-projected — the burn ran
/// past the inflated forecast).
///
/// The percentiles are never published alone: the four counts above them carry the cardinality and
/// the sentinel exclusion, and the two census fields carry the CENSORING — this population is
/// recovered-only, because `blind_window` fires on the recovery edge of the active account. A
/// consumer that reads `p100` without reading `n_swapped_away` / `n_never_recovered` is reading the
/// easy half of the distribution. Since schema:8 (issue #591) those two carry REAL counts, sourced
/// from the uncensored `blind_enter` / `blind_exit` pair this SLI CONSUMES rather than duplicates
/// (the full census is the sibling `blind_episodes` block). They fall back to `null` — unobservable,
/// NEVER `0` — when that pair is absent from view, as in a log predating issue #583.
/// `p50`/`p95`/`p100` are `null` on an empty reconcilable population (an empty subject is not a
/// passing `0 pp` error).
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
    /// Episodes the daemon swapped away from before they recovered — the tail the active-scoped
    /// `blind_window` cannot see. Counted from the issue #591 pair; `null` when it is not in view.
    n_swapped_away: Option<usize>,
    /// Episodes that never recovered in view — the tail the recovery-edge `blind_window` cannot see.
    /// Counted from the issue #591 pair, restart orphans excluded; `null` when it is not in view.
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
        capacity_held: CapacityHeldWire {
            n: r.capacity_held.n,
            p50: r.capacity_held.p50,
            p95: r.capacity_held.p95,
            p100: r.capacity_held.p100,
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
            capacity_held: r.landing.capacity_held,
        },
        operator_landing: OperatorLandingWire {
            swaps_total: r.operator_landing.swaps_total,
            n_measured: r.operator_landing.n_measured,
            n_unmeasured: r.operator_landing.n_unmeasured,
            p50: r.operator_landing.p50,
            p90: r.operator_landing.p90,
            p100: r.operator_landing.p100,
            window_secs: LANDING_WINDOW_SECS,
            ceiling: SLO_SWAP_P100_MAX,
            n_at_or_over_ceiling: r.operator_landing.n_at_or_over_ceiling,
            capacity_held: r.operator_landing.capacity_held,
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
        refresh_token_loss: RefreshTokenLossWire {
            accounts: r.refresh_token_loss.accounts,
            observations: r.refresh_token_loss.by_mechanism.total(),
            by_mechanism: RefreshTokenLossByMechanismWire {
                sweep: r.refresh_token_loss.by_mechanism.sweep,
                poll_retry: r.refresh_token_loss.by_mechanism.poll_retry,
                parked_recovery: r.refresh_token_loss.by_mechanism.parked_recovery,
                keep_warm: r.refresh_token_loss.by_mechanism.keep_warm,
            },
            confirmed_unrecoverable: r.refresh_token_loss.confirmed_unrecoverable,
        },
        blind_episodes: BlindEpisodesWire {
            n_entered: r.blind_episodes.n_entered,
            n_exited: r.blind_episodes.n_exited,
            n_swapped_away: r.blind_episodes.n_swapped_away,
            n_never_recovered: r.blind_episodes.n_never_recovered,
            n_anchor_lost: r.blind_episodes.n_anchor_lost,
            n_exit_without_enter: r.blind_episodes.n_exit_without_enter,
            n_malformed: r.blind_episodes.n_malformed,
            near_limit: BlindEpisodesNearLimitWire {
                n_episodes: r.blind_episodes.near_limit_episodes,
                observed_secs: r.blind_episodes.near_limit_observed_secs,
                censored_floor_secs: r.blind_episodes.near_limit_censored_floor_secs,
                // The SAME derivation the human surface renders, so the two cannot disagree.
                total_secs_lower_bound: r.blind_episodes.near_limit_total_secs_lower_bound(),
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
    ///
    /// The `blind_enter` / `blind_exit` pair (issue #583) is now an INPUT — to the uncensored
    /// [`BlindEpisodes`] census ONLY (issue #591) — while remaining the BLAST-RADIUS guard for the
    /// two `blind_window`-derived SLIs, which that decision keeps on that event UNCHANGED. The guard
    /// therefore now pins BOTH directions:
    ///
    /// - The pair must perturb the censored SLIs by NOTHING. Its fields stay deliberately
    ///   adversarial — a `near_limit=true` u-D episode with a 999 s `duration_secs` and its own
    ///   `session_pct`, and u-D has NO `blind_window` line of its own — so a regression that ever
    ///   folds the pair back into them fails LOUDLY rather than silently corrupting what the #484
    ///   promotion bar reads: `time_blind_near_limit_secs` would read 1899 against the asserted 900,
    ///   and `near_limit_reconciliations` would gain a spurious third pair against the two it pins.
    /// - The pair must REACH the census (`entered=1 exited=1 swapped_away=1`), so a regression that
    ///   drops the two parse arms — silently restoring the censored-at-both-tails reading issue #583
    ///   exists to end — is caught just as loudly as the double-count above.
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
ts=2026-07-11T00:50:00Z event=usage_velocity acct=u-A session_pct_per_min=0.20 weekly_pct_per_min=0.01 elapsed_secs=300 session_delta_pct=1 weekly_delta_pct=0
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
        // #719: no all_exhausted in FIXTURE_LOG → no swap is capacity-held; both reason=session swaps
        // stay in the reaction-latency bucket above, the held partition is empty.
        assert!(inputs.swap_out_held_pcts.is_empty());
        // Only near_limit=true windows: 300 + 600 (the near_limit=false 120 is excluded).
        assert_eq!(inputs.time_blind_near_limit_secs, 900);
        assert_eq!(inputs.near_limit_reconciliations, vec![(97, 99), (96, 40)]);
        // The uncensored pair (issue #591) reaches its OWN raw ingredients and nothing else — the
        // second direction of the FIXTURE_LOG blast-radius guard. Paired with the two assertions
        // just above (900s, two reconciliations), this pins BOTH failure modes: the pair must not
        // leak into the censored SLIs, and it must not be dropped from the census.
        assert_eq!(inputs.blind_entries.len(), 1);
        assert_eq!(inputs.blind_exits.len(), 1);
        assert_eq!(inputs.blind_pair_malformed, 0);
        assert_eq!(inputs.rate_limited, 2);
        assert_eq!(inputs.transient, 1);
        assert_eq!(inputs.cleared, 1);
        // #881: FIXTURE_LOG carries no refresh-family line, so the loss attribution is empty — the
        // blast-radius guard's third direction (the new arms must not fire on unrelated families).
        assert!(inputs.refresh_token_loss_accounts.is_empty());
        assert_eq!(
            inputs.refresh_token_loss_by_mechanism,
            RefreshTokenLossByMechanism::default()
        );
        assert_eq!(inputs.refresh_token_loss_confirmed, 0);
    }

    // --- issue #881: refresh-token-loss attribution ---------------------------------------

    /// A VERBATIM REPLAY of the refresh-token losses in the author's live event log
    /// (`~/Library/Logs/sessiometer/sessiometer.log`, 13,983 lines as of 2026-07-29): every
    /// `outcome=dead` line it contains, unaltered in shape, field order, or handle.
    ///
    /// This fixture exists because issue #719 shipped a classification rule that was drafted as
    /// `hold == from`, matched ZERO of 11 real relief swaps, and passed every test that only
    /// asserted the absence of false positives. A replay of real lines is the one check that would
    /// have caught it, so #881's predicate is pinned against real lines before anything else.
    ///
    /// The population is deliberately uneven — one account observed FOUR times across two
    /// mechanisms, two others once each — because that is what the real log looks like, and it is
    /// exactly the shape that makes line-counting and account-counting diverge.
    const LIVE_REPLAY_REFRESH_TOKEN_LOSS_LOG: &str = "\
ts=2026-07-14T13:25:30Z event=keep_warm account=oleksii@pelykh.com trigger=reactive outcome=dead rotated=true
ts=2026-07-15T15:12:53Z event=keep_warm account=oleksii@pelykhconsulting.com trigger=reactive outcome=dead rotated=true
ts=2026-07-19T03:25:39Z event=poll_refresh account=oleksii@pelykhconsulting.eu trigger=poll_401 outcome=dead rotated=true
ts=2026-07-19T04:39:31Z event=poll_refresh account=oleksii@pelykhconsulting.eu trigger=poll_401 outcome=dead rotated=true
ts=2026-07-19T05:56:47Z event=poll_refresh account=oleksii@pelykhconsulting.eu trigger=poll_401 outcome=dead rotated=true
ts=2026-07-19T06:07:06Z event=refresh account=oleksii@pelykhconsulting.eu outcome=dead expires_before=1970-01-01T00:00:00Z expires_after=1970-01-01T00:00:00Z rotated=false window_secs=0
";

    /// The healthy refresh traffic that surrounds those losses in the same live log — the
    /// overwhelming majority of refresh-family lines (338 `refreshed`, 83 `error`, 52
    /// `refreshed_not_restashed`, …). Kept as its own fixture so "the predicate fires" and "the
    /// predicate is selective" are pinned by separate assertions rather than one combined count.
    const LIVE_REPLAY_HEALTHY_REFRESH_LOG: &str = "\
ts=2026-07-19T07:00:00Z event=refresh account=oleksii@pelykh.com outcome=refreshed expires_before=2026-07-19T06:00:00Z expires_after=2026-07-19T14:00:00Z rotated=false window_secs=28800
ts=2026-07-19T07:05:00Z event=refresh account=oleksii@pelykh.com outcome=error rotated=false reason=timeout backoff_secs=60
ts=2026-07-19T07:10:00Z event=poll_refresh account=oleksii@pelykh.com trigger=poll_401 outcome=refreshed rotated=false
ts=2026-07-19T07:15:00Z event=keep_warm account=oleksii@pelykh.com trigger=proactive outcome=refreshed_not_restashed rotated=false
ts=2026-07-19T07:20:00Z event=refresh account=oleksii@pelykh.com outcome=no_change rotated=false
";

    /// THE non-inertness check. Replays the real losses and asserts the predicate fires on all
    /// three mechanisms with the real cardinalities — the assertion #719's drafted rule would have
    /// failed, and the reason this predicate is not merely plausible.
    #[test]
    fn the_refresh_token_loss_predicate_fires_on_every_real_dead_refresh_line() {
        let inputs = parse_events(LIVE_REPLAY_REFRESH_TOKEN_LOSS_LOG, None);
        // 6 real `outcome=dead` lines, split exactly as the live log splits them. All three
        // `poll_refresh` lines read `trigger=poll_401` because that is the only token any daemon
        // predating issue #1367 could write, whatever produced them — so `parked_recovery` is 0
        // here BY CONSTRUCTION, not by observation, and this fixture is also the demonstration that
        // the corrected split does not reach backwards. See `parse_events`' `poll_refresh` arm.
        assert_eq!(
            inputs.refresh_token_loss_by_mechanism,
            RefreshTokenLossByMechanism {
                sweep: 1,
                poll_retry: 3,
                parked_recovery: 0,
                keep_warm: 2,
            },
            "the predicate must match every real dead-refresh line, on all three mechanisms"
        );
        assert_eq!(inputs.refresh_token_loss_by_mechanism.total(), 6);
        // 3 DISTINCT accounts behind those 6 observations — the dedup that makes the two figures
        // differ, and the reason `accounts` is not a line count.
        assert_eq!(inputs.refresh_token_loss_accounts.len(), 3);
    }

    /// The DISCRIMINATING half: the narrower, semantically-tempting predicate
    /// (`event=credential_unrecoverable`, issue #261's "confirmed dead and unrecoverable" latch)
    /// matches NOTHING on the same real lines, because it requires an account to be quarantined
    /// first. Keying on it alone would have shipped an inert rule that passes every no-false-
    /// positive test — the #719 failure mode, reproduced here as an executable falsifier rather
    /// than a comment.
    #[test]
    fn keying_refresh_token_loss_on_credential_unrecoverable_alone_would_be_inert() {
        let inputs = parse_events(LIVE_REPLAY_REFRESH_TOKEN_LOSS_LOG, None);
        assert_eq!(
            inputs.refresh_token_loss_confirmed, 0,
            "the live log carries no credential_unrecoverable line; a predicate keyed on it \
             would report zero losses while six dead-refresh lines sat in the same file"
        );
        assert!(
            inputs.refresh_token_loss_by_mechanism.total() > 0,
            "the shipped predicate must find the losses the narrow one misses"
        );
    }

    /// The latch IS counted when it fires — corroboration beside the observations, never instead
    /// of them. Pins that `confirmed_unrecoverable` is wired, so the assertion above measures the
    /// live log's silence rather than a dead code path.
    #[test]
    fn a_credential_unrecoverable_line_is_counted_as_confirmation() {
        let log = format!(
            "{LIVE_REPLAY_REFRESH_TOKEN_LOSS_LOG}\
ts=2026-07-19T08:00:00Z event=credential_unrecoverable account=oleksii@pelykhconsulting.eu\n"
        );
        let inputs = parse_events(&log, None);
        assert_eq!(inputs.refresh_token_loss_confirmed, 1);
        // The latch adds no observation and no account — it confirms a loss already counted.
        assert_eq!(inputs.refresh_token_loss_by_mechanism.total(), 6);
        assert_eq!(inputs.refresh_token_loss_accounts.len(), 3);
    }

    /// `confirmed_unrecoverable` is an EVENT count over the window, independent of `accounts` in
    /// BOTH directions — pinned because the word "confirmed" invites the subset reading, and a doc
    /// that merely asserted the independence would be the kind of prose the code can quietly
    /// contradict. The live log shows one direction (0 latches beside 3 lost accounts); this pins
    /// the other, which `--since` makes routine: a window can catch the latch while the dead-refresh
    /// line that produced it falls before the cutoff.
    #[test]
    fn confirmed_unrecoverable_is_an_event_count_not_a_subset_of_accounts() {
        let log = "\
ts=2026-07-19T08:00:00Z event=credential_unrecoverable account=oleksii@pelykhconsulting.eu
ts=2026-07-19T08:05:00Z event=credential_unrecoverable account=oleksii@pelykhconsulting.eu
";
        let r = aggregate(&parse_events(log, None), &[], None);
        assert_eq!(
            r.refresh_token_loss.accounts, 0,
            "no dead-refresh line in view"
        );
        assert_eq!(
            r.refresh_token_loss.confirmed_unrecoverable, 2,
            "latches are counted per EVENT, so they can exceed accounts — and at accounts=0"
        );
        // The render must show both without pretending one bounds the other.
        let text = render_human(&r);
        assert!(text.contains("accounts observed lost: 0"));
        assert!(text.contains("confirmed unrecoverable (automated recovery exhausted): 2"));
    }

    /// Selectivity: the healthy refresh traffic that dwarfs the losses in the real log contributes
    /// nothing. Without this, a predicate that matched the FAMILY rather than the OUTCOME would
    /// pass the non-inertness test above while reporting the whole fleet lost.
    #[test]
    fn healthy_refresh_outcomes_are_not_refresh_token_losses() {
        let inputs = parse_events(LIVE_REPLAY_HEALTHY_REFRESH_LOG, None);
        assert_eq!(
            inputs.refresh_token_loss_by_mechanism,
            RefreshTokenLossByMechanism::default()
        );
        assert!(inputs.refresh_token_loss_accounts.is_empty());
    }

    /// One account's single lapse is observed repeatedly, so the account count MUST dedup while
    /// the observation count MUST not. Both directions are asserted: reporting 4 losses for one
    /// account overstates the operator's work, and collapsing the observations would hide which
    /// mechanisms saw it.
    #[test]
    fn repeated_observations_of_one_loss_count_as_one_account() {
        let inputs = parse_events(LIVE_REPLAY_REFRESH_TOKEN_LOSS_LOG, None);
        let report = aggregate(&inputs, &[], None);
        assert_eq!(report.refresh_token_loss.accounts, 3);
        assert_eq!(report.refresh_token_loss.by_mechanism.total(), 6);
        assert!(
            report.refresh_token_loss.by_mechanism.total()
                > u32::try_from(report.refresh_token_loss.accounts).unwrap(),
            "the live replay's whole point is that observations outnumber accounts"
        );
    }

    /// Issue #1367: `event=poll_refresh` is the one family whose name does not identify the
    /// mechanism, so the split reads `trigger=`. Both origins are replayed here through the SAME
    /// event name, at the SAME outcome, differing only in that token — which is the whole content
    /// of the fix.
    #[test]
    fn a_recovery_triggered_poll_refresh_death_is_not_counted_as_a_poll_driven_retry() {
        let log = "\
ts=2026-07-19T09:00:00Z event=poll_refresh account=spare trigger=poll_401 outcome=dead rotated=true
ts=2026-07-19T09:01:00Z event=poll_refresh account=spare trigger=recovery outcome=dead rotated=true
";
        let inputs = parse_events(log, None);
        assert_eq!(
            inputs.refresh_token_loss_by_mechanism,
            RefreshTokenLossByMechanism {
                sweep: 0,
                poll_retry: 1,
                parked_recovery: 1,
                keep_warm: 0,
            },
            "the recovery re-probe must not report a usage-401 that never happened"
        );
        // Neither line LEFT the evidence set — the correction moves observations between buckets,
        // it does not drop any. This is what keeps `accounts` and the `total` unchanged across the
        // schema:10 → schema:11 bump.
        assert_eq!(inputs.refresh_token_loss_by_mechanism.total(), 2);
        assert_eq!(inputs.refresh_token_loss_accounts.len(), 1);
    }

    /// The variant name inside a derived-`Debug` rendering: its LEADING IDENTIFIER RUN.
    ///
    /// Named rather than left inline at its one call site so the property can be asserted on a
    /// rendering that CARRIES FIELDS. No `PollRefreshTrigger` variant does, which is precisely
    /// how the premise this replaced — that they are all fieldless, so the whole `{:?}` IS the
    /// name — expired without anything noticing (issue #1397). A test standing on today's two
    /// variants cannot tell the two readings apart, because on `Poll401` and `Recovery` the run
    /// is the whole rendering.
    ///
    /// The predicate is byte-identical to the issue #1085 precedent in `crate::daemon`'s
    /// redaction meter. It is Unicode `is_alphanumeric`, while the declared side of the
    /// comparison takes `is_ascii_alphanumeric` — and that asymmetry does NOT announce itself.
    /// Measured: on a `Café` variant the declared side's run stops at `Caf`, leaving `rest` as
    /// `é`, which `declares_variant` refuses — so the name is dropped from `declared` entirely
    /// rather than truncated into it. Replayed, it then fires `undeclared` alone; declared and
    /// NOT replayed, it fires NEITHER list and passes silently, like any other spelling outside
    /// the parser's subset.
    ///
    /// Inlining this back into the call site is caught by nothing, and the test below states
    /// that bound rather than leaving it to be discovered.
    fn replayed_variant_name(replayed: impl std::fmt::Debug) -> String {
        format!("{replayed:?}")
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect()
    }

    /// What the run-taking buys, on the shapes the comparison below cannot itself produce.
    ///
    /// On a fielded variant the whole rendering is NOT the name, and comparing it against the
    /// declared set is what made issue #1397's replay blame a drifted scan for a variant the
    /// scan had read correctly.
    ///
    /// The bound of the pin, stated because it is not obvious: it holds the READING, not the
    /// WIRING. Dropping the run out of the function above fails here, naming the rendering it
    /// kept. Re-inlining `format!("{trigger:?}")` at the call site below does not — measured,
    /// the whole suite stays at `0 failed` — and nothing can make it fail while every
    /// `PollRefreshTrigger` variant is fieldless and the two readings agree on every input the
    /// comparison has.
    #[test]
    fn the_replayed_name_is_the_leading_run_whether_or_not_the_variant_carries_fields() {
        // The fields are never READ — their whole job is to make `Debug` render past the name,
        // which is the shape under test. `Debug` does not count as a read for `dead_code`.
        #[derive(Debug)]
        #[allow(non_camel_case_types, dead_code)]
        enum Probe {
            Bare,
            Fielded { every_secs: u32 },
            Wrapped(u32),
            Poll_401,
        }

        // The two shapes that broke: a struct body and a tuple body both render past the name.
        assert_eq!(
            replayed_variant_name(Probe::Fielded { every_secs: 60 }),
            "Fielded"
        );
        assert_eq!(replayed_variant_name(Probe::Wrapped(60)), "Wrapped");
        // A fieldless variant renders as the bare name, so the run is the identity on it — which
        // is why the comparison below is unchanged by any of this today.
        assert_eq!(replayed_variant_name(Probe::Bare), "Bare");
        assert_eq!(
            replayed_variant_name(crate::observability::PollRefreshTrigger::Poll401),
            "Poll401"
        );
        // `_` and digits are part of the run, so a non-camel-case variant keeps its whole name
        // rather than being truncated into a false mismatch against the declared side.
        assert_eq!(replayed_variant_name(Probe::Poll_401), "Poll_401");
    }

    /// The gate the issue asked for. Adding a [`crate::observability::PollRefreshTrigger`] variant
    /// and stopping at the emitter re-creates the #1367 defect silently: the classifier's `else`
    /// arm absorbs the new token into `poll_retry`, and every existing assertion stays green while
    /// a second population is quietly reported as poll-driven again.
    ///
    /// Two layers close that — the same pair `crate::observability`'s issue #891 sweeps use over
    /// `Event` and `Diagnostic`. Only the first was here until issue #1386.
    ///
    /// LAYER 1, at COMPILE TIME. `expected_bucket` below is an EXHAUSTIVE match with no wildcard,
    /// so a new variant does not compile until someone names the bucket it belongs in. That fires
    /// before the code can run at all, which is what a silent fall-through needs.
    ///
    /// LAYER 2, at TEST TIME. Layer 1 forces an ARM, not a REPLAY. `EVERY_TRIGGER` is a separate
    /// `const` the compiler has nothing to say about, so a variant could be named above and never
    /// exercised — the hole reopening one step later. Measured on issue #1386: a third variant
    /// added to the enum, given an `as_str` arm and a bucket, and left out of that list ran the
    /// whole suite to `0 failed`, while the classifier routed its token to `poll_retry` — the
    /// #1367 defect, one variant over. Adding it to the list alone turned that assertion red. The
    /// comparison beside the list is what makes the list say so.
    ///
    /// Neither layer can decide whether the bucket NAMED for a variant is the RIGHT one; nothing
    /// mechanical can. Together they make the question unskippable and the answer tested.
    ///
    /// Each replayed variant is rendered THROUGH the real emitter (never a hand-typed token, which
    /// could only ever agree with itself) and fed to the real parser, so the two ends of the
    /// contract are checked against each other rather than against a shared assumption.
    #[test]
    fn poll_refresh_trigger_tokens_all_reach_a_named_bucket() {
        use crate::observability::{Event, PollRefreshTrigger, RefreshEventOutcome};
        use std::collections::BTreeSet;
        use std::time::{Duration, UNIX_EPOCH};

        /// Which bucket each trigger must land in — layer 1. Exhaustive on purpose: the enum
        /// grows and this stops compiling until the new variant is named here. Extending
        /// `EVERY_TRIGGER` to match is layer 2's job, and is no longer left to this sentence.
        fn expected_bucket(trigger: PollRefreshTrigger) -> RefreshTokenLossByMechanism {
            match trigger {
                PollRefreshTrigger::Poll401 => RefreshTokenLossByMechanism {
                    poll_retry: 1,
                    ..RefreshTokenLossByMechanism::default()
                },
                PollRefreshTrigger::Recovery => RefreshTokenLossByMechanism {
                    parked_recovery: 1,
                    ..RefreshTokenLossByMechanism::default()
                },
            }
        }
        const EVERY_TRIGGER: &[PollRefreshTrigger] =
            &[PollRefreshTrigger::Poll401, PollRefreshTrigger::Recovery];

        // Layer 2. Writing a list down does not make it complete, and nothing above this line
        // reaches it, so it is held against the variants the enum's own SOURCE declares — the
        // issue #891 scan, published by `crate::observability::tests` for this consumer.
        //
        // Derived `Debug` supplies the replayed side, read the way `crate::daemon`'s redaction
        // meter reads it against the sibling scan in `crate::error` — the issue #1085 idiom: the
        // LEADING IDENTIFIER RUN of the rendering is the variant name. Derived, so unlike a second
        // hand-written list it cannot fall out of step with the declaration.
        //
        // Taking the run rather than the whole rendering is what keeps this pointed at the right
        // failure. Until issue #1397 the whole `{:?}` was compared, on the stated premise that
        // these variants are fieldless — true today, and untrue the moment one is not. Measured on
        // that issue: a `Scheduled { every_secs: u32 }` variant, wired AND replayed, rendered
        // `Scheduled { every_secs: 60 }`, which matched nothing declared; BOTH directions then
        // fired and the message below blamed a drifted scan. The scan was right. `Debug` had
        // rendered the fields, and the premise the comparison rested on had simply expired
        // without anything noticing. The run is what the declaration and the rendering genuinely
        // share, so it holds whether or not a variant carries fields.
        //
        // Compared BOTH ways. Declared-but-unreplayed is the hole issue #1386 closes;
        // replayed-but-undeclared means the scan has drifted onto the wrong enum or stopped early,
        // which would quietly weaken this check rather than fail it. An emptied `EVERY_TRIGGER`
        // fails the first direction, and a scan that parsed nothing fails inside the scan itself,
        // so neither degenerate reading survives as a pass.
        let declared = crate::observability::tests::declared_variant_names("PollRefreshTrigger");
        let replayed: BTreeSet<String> = EVERY_TRIGGER.iter().map(replayed_variant_name).collect();
        let unreplayed: Vec<&String> = declared.difference(&replayed).collect();
        let undeclared: Vec<&String> = replayed.difference(&declared).collect();
        assert!(
            unreplayed.is_empty() && undeclared.is_empty(),
            "issue #1386: `EVERY_TRIGGER` must replay EVERY declared `PollRefreshTrigger` \
             variant — naming one in `expected_bucket` does not exercise it.\n  declared but \
             never replayed (nothing checks which bucket the classifier hands its token): \
             {unreplayed:?}\n  replayed but absent from the enum source (the scan has drifted): \
             {undeclared:?}"
        );

        for &trigger in EVERY_TRIGGER {
            let line = Event::PollRefresh {
                account: "spare".to_owned(),
                trigger,
                outcome: RefreshEventOutcome::Dead,
            }
            .to_log_line(UNIX_EPOCH + Duration::from_secs(1_784_000_000));
            let inputs = parse_events(&format!("{line}\n"), None);
            assert_eq!(
                inputs.refresh_token_loss_by_mechanism,
                expected_bucket(trigger),
                "{trigger:?} rendered `{line}`, which the classifier did not route to the bucket \
                 this test names for it"
            );
            // Whichever bucket claimed it, the observation stayed in the evidence set — the
            // correction moves lines between buckets and drops none.
            assert_eq!(inputs.refresh_token_loss_by_mechanism.total(), 1);
        }
    }

    /// A dead line whose `account=` is missing still counts as EVIDENCE but cannot join the
    /// account set — the tolerant-drop precedent (a landing anchor missing `from=` feeds the pct
    /// and opens no window). The gap between the two figures is where such a line is visible.
    #[test]
    fn a_dead_line_without_an_account_counts_as_an_observation_only() {
        let log = "ts=2026-07-19T09:00:00Z event=refresh outcome=dead rotated=false\n";
        let inputs = parse_events(log, None);
        assert_eq!(inputs.refresh_token_loss_by_mechanism.total(), 1);
        assert!(inputs.refresh_token_loss_accounts.is_empty());
    }

    /// `--since` bounds the loss attribution exactly as it bounds every other SLI: the window gate
    /// runs before the event match, so no per-SLI windowing code is needed — this pins that the new
    /// arms sit downstream of it rather than beside it.
    #[test]
    fn the_since_window_bounds_the_refresh_token_loss_attribution() {
        // Cutoff between the 07-15 keep_warm and the 07-19 poll_refresh burst.
        let cutoff = epoch("2026-07-17T00:00:00Z");
        let inputs = parse_events(LIVE_REPLAY_REFRESH_TOKEN_LOSS_LOG, Some(cutoff));
        assert_eq!(
            inputs.refresh_token_loss_by_mechanism,
            RefreshTokenLossByMechanism {
                sweep: 1,
                poll_retry: 3,
                parked_recovery: 0,
                keep_warm: 0,
            },
            "the two pre-cutoff keep_warm losses must fall outside the window"
        );
        assert_eq!(inputs.refresh_token_loss_accounts.len(), 1);
    }

    /// THE issue #881 ACCEPTANCE — "the existing swap-out SLI partition is unchanged in meaning".
    ///
    /// Asserted EXHAUSTIVELY rather than field-by-field: both readouts are rendered to JSON, the
    /// `refresh_token_loss` key is removed from each, and the remainders must be byte-identical.
    /// A field-by-field comparison would silently stop covering a future SLI; this cannot, because
    /// any new key appears in both documents automatically.
    ///
    /// The segregation being verified is STRUCTURAL — refresh-family events and `event=swap` are
    /// disjoint populations — so this test is pinning that no code path was accidentally wired
    /// between them, not that a filter is tuned correctly.
    #[test]
    fn adding_refresh_token_losses_changes_no_other_figure() {
        // The added lines are dated INSIDE the fixture's own span (00:00–00:50) rather than reusing
        // the live replay's 07-14→07-19 dates. That is deliberate and load-bearing: `horizon_ts` is
        // tracked outside the event match, so ANY later-dated line — of any family — advances the
        // observation horizon, and a never-recovered episode would then legitimately report a larger
        // censored floor. FIXTURE_LOG happens to have no such episode, so out-of-span dates would
        // pass here anyway — on a branch that never runs. Keeping the lines in-span makes the
        // equality below hold for the stated reason (disjoint populations) instead of by luck, so
        // the test cannot silently start proving something weaker than it claims.
        const IN_SPAN_LOSSES: &str = "\
ts=2026-07-11T00:12:00Z event=keep_warm account=work trigger=reactive outcome=dead rotated=true
ts=2026-07-11T00:13:00Z event=poll_refresh account=spare trigger=poll_401 outcome=dead rotated=true
ts=2026-07-11T00:14:00Z event=refresh account=spare outcome=dead rotated=false
";
        let without = aggregate(&parse_events(FIXTURE_LOG, None), &[], None);
        let with = aggregate(
            &parse_events(&format!("{FIXTURE_LOG}{IN_SPAN_LOSSES}"), None),
            &[],
            None,
        );

        let strip = |r: &Report| -> serde_json::Value {
            let mut v: serde_json::Value = serde_json::from_str(&render_json(r).expect("render"))
                .expect("the readout renders valid JSON");
            let removed = v
                .as_object_mut()
                .expect("the readout is a JSON object")
                .remove("refresh_token_loss");
            assert!(removed.is_some(), "the block under test must be present");
            v
        };
        assert_eq!(
            strip(&without),
            strip(&with),
            "adding refresh-token-loss lines must leave every other figure identical"
        );

        // The canary: the perturbation must actually have DONE something, or the equality above is
        // satisfied by a fixture that changed nothing (the degenerate-subject failure).
        assert_ne!(
            without.refresh_token_loss, with.refresh_token_loss,
            "the added lines must move the loss block, else this test proves nothing"
        );
        assert_eq!(with.refresh_token_loss.accounts, 2, "work + spare");
        assert_eq!(with.refresh_token_loss.by_mechanism.total(), 3);
    }

    /// The swap SLIs are computed from `event=swap` alone, so a log of PURE losses yields no swap
    /// evidence at all. The complement of the test above: there, losses do not perturb swaps; here,
    /// losses cannot manufacture them.
    #[test]
    fn refresh_token_losses_alone_produce_no_swap_evidence() {
        let inputs = parse_events(LIVE_REPLAY_REFRESH_TOKEN_LOSS_LOG, None);
        assert!(inputs.swap_out_pcts.is_empty());
        assert!(inputs.swap_out_held_pcts.is_empty());
        assert!(inputs.projective_swap_out_pcts.is_empty());
        assert!(inputs.session_swaps.is_empty());
        assert!(inputs.reactivations.is_empty());
    }

    /// The block reaches the human readout with its exclusion stated in words — the layout alone
    /// cannot say "this is not a swap-out failure", and an operator reading a degraded swap SLO
    /// needs to see where credential losses went.
    #[test]
    fn the_human_readout_renders_the_loss_block_with_its_exclusion() {
        let text = render_human(&aggregate(
            &parse_events(LIVE_REPLAY_REFRESH_TOKEN_LOSS_LOG, None),
            &[],
            None,
        ));
        assert!(text.contains("refresh-token loss"));
        assert!(text.contains("NOT a swap-out failure"));
        assert!(text.contains("sessiometer login"));
        assert!(text.contains("accounts observed lost: 3"));
        assert!(text.contains("sweep=1 poll-retry=3 parked-recovery=0 keep-warm=2"));
    }

    /// The `--json` contract: the block is on the wire, under the bumped schema, with the shape a
    /// consumer is promised. The schema assertion is deliberately literal — the issue #881 AC ties
    /// the bump to a payload-shape change, so the two must be pinned together or a later edit can
    /// add a key and leave the version behind.
    #[test]
    fn the_json_wire_carries_the_loss_block_under_schema_12() {
        let r = aggregate(
            &parse_events(LIVE_REPLAY_REFRESH_TOKEN_LOSS_LOG, None),
            &[],
            None,
        );
        let json: serde_json::Value =
            serde_json::from_str(&render_json(&r).expect("serializes")).expect("valid JSON");
        assert_eq!(json["schema"], 12);
        let loss = &json["refresh_token_loss"];
        assert_eq!(loss["accounts"], 3);
        assert_eq!(loss["observations"], 6);
        assert_eq!(loss["by_mechanism"]["sweep"], 1);
        assert_eq!(loss["by_mechanism"]["poll_retry"], 3);
        // 0 because every `poll_refresh` line in this real-log replay predates issue #1367 and so
        // renders the poll trigger whatever produced it — the schema:11 key exists on the wire, and
        // reads honestly, over a window whose lines cannot populate it.
        assert_eq!(loss["by_mechanism"]["parked_recovery"], 0);
        assert_eq!(loss["by_mechanism"]["keep_warm"], 2);
        assert_eq!(loss["confirmed_unrecoverable"], 0);
        // No `targets` / `met`: this block gates nothing, deliberately — it is the population a
        // reliability gate must EXCLUDE, so publishing a pass/fail flag would invite the exact
        // conflation issue #881 exists to prevent.
        assert!(loss.get("targets").is_none());
        assert!(loss.get("met").is_none());
    }

    /// Zero is RENDERED, not suppressed. "No account was lost in view" is a real reading, and an
    /// absent block would leave an operator unable to distinguish it from a readout that does not
    /// track losses at all — the same absence-is-not-evidence discipline the percentile blocks
    /// apply by withholding a figure instead of printing a passing `0`.
    #[test]
    fn a_clean_log_still_renders_the_loss_block_at_zero() {
        let text = render_human(&aggregate(&parse_events("", None), &[], None));
        assert!(text.contains("refresh-token loss"));
        assert!(text.contains("accounts observed lost: 0"));
        assert!(text.contains("confirmed unrecoverable (automated recovery exhausted): 0"));
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
        // #719: no all_exhausted → the capacity-held partition is empty and the landing SLI excludes
        // nothing; the reaction-latency gate above sees both swaps.
        assert_eq!(r.capacity_held.n, 0);
        assert_eq!(r.landing.capacity_held, 0);
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
    /// positive samples are 0.63 / 1.86 / 6.90 so the percentiles land on recognizable values.
    ///
    /// Every line is INTERNALLY CONSISTENT: `session_delta_pct / (elapsed_secs / 60)` reproduces the
    /// rendered `session_pct_per_min` exactly, which is what
    /// [`crate::observability::Event::to_log_line`] would actually emit. Before issue #1158 only the rendered half was read, so the two halves were free to
    /// disagree and did — `0.63` sat on ingredients meaning 1.00. Now the ingredients ARE the input,
    /// so a fixture that could never be emitted would be testing nothing. `elapsed_secs` is kept a
    /// multiple of 60 so `elapsed_secs / 60.0` is exact and the quotient matches the 2-dp literal
    /// bit-for-bit.
    ///
    /// The max is 6.90 rather than the `V_PEAK_SESSION_PCT_PER_MIN` 6.95 for an arithmetic reason
    /// worth recording: 6.95 is 139/20 and 139 is prime, so `60·delta/elapsed_secs = 6.95` forces
    /// `delta` to be a multiple of 139 — impossible for a session-percent delta bounded by 100. No
    /// emittable line can carry exactly 6.95, so the `p100 == v_peak` equality boundary is pinned
    /// directly on the predicate instead, by
    /// [`v_peak_honest_admits_a_sample_exactly_at_the_assumed_peak`].
    const VELOCITY_LOG: &str = "\
ts=2026-07-11T00:00:00Z event=usage_velocity acct=u-A session_pct_per_min=0.63 weekly_pct_per_min=0.01 elapsed_secs=6000 session_delta_pct=63 weekly_delta_pct=0
ts=2026-07-11T00:01:00Z event=usage_velocity acct=u-A session_pct_per_min=1.86 weekly_pct_per_min=0.02 elapsed_secs=3000 session_delta_pct=93 weekly_delta_pct=0
ts=2026-07-11T00:02:00Z event=usage_velocity acct=u-A session_pct_per_min=6.90 weekly_pct_per_min=0.03 elapsed_secs=600 session_delta_pct=69 weekly_delta_pct=0
ts=2026-07-11T00:03:00Z event=usage_velocity acct=u-A session_pct_per_min=-92.00 weekly_pct_per_min=0.00 elapsed_secs=60 session_delta_pct=-92 weekly_delta_pct=0
ts=2026-07-11T00:04:00Z event=usage_velocity acct=u-A session_pct_per_min=0.00 weekly_pct_per_min=0.00 elapsed_secs=60 session_delta_pct=0 weekly_delta_pct=0
";

    #[test]
    fn observed_peak_folds_positive_climbs_and_drops_resets_and_flats() {
        let inputs = parse_events(VELOCITY_LOG, None);
        // Only the three POSITIVE climbs — the −92 window-reset and the 0.00 flat are dropped so
        // they cannot drag the distribution down and hide a real peak.
        assert_eq!(inputs.session_velocities, vec![0.63, 1.86, 6.90]);
        let r = aggregate(&inputs, &[], None);
        assert_eq!(r.observed_peak.n, 3);
        // n=3 sorted [0.63,1.86,6.90]: P50=ceil(.5·3)=2→1.86, P90=ceil(.9·3)=3→6.90, P100→6.90.
        assert_eq!(r.observed_peak.p50, Some(1.86));
        assert_eq!(r.observed_peak.p90, Some(6.90));
        assert_eq!(r.observed_peak.p100, Some(6.90));
        // The observed max sits just under the assumed v_peak (6.95), so the constant is still
        // honest. The `==` boundary itself is pinned by the sibling test below.
        assert_eq!(r.observed_peak.v_peak_honest(), Some(true));
    }

    /// Issue #1158: a MEASURED climb whose rendered rate the emitter's `{:.2}` floored to `0.00`.
    /// `session_delta_pct=1` over 12 060 s is 0.004975 %/min — under the 0.005 the second decimal
    /// can hold — so the rendered field reads `0.00` while the raw ingredients on the SAME line
    /// record a real 1 % climb. Reading the ingredients keeps the sample; reading the rendered
    /// field dropped it as "not a climb".
    ///
    /// The fixture is SYNTHETIC and deliberately so: a replay of the live event log
    /// (~/Library/Logs/sessiometer/sessiometer.log, 8 527 `usage_velocity` lines carrying an
    /// interval, 2026-07-13 → 2026-08-10) found ZERO lines matching this shape — the longest
    /// interval over a positive delta was 4 108 s, and the slowest positive climb was
    /// 0.0147 %/min, ~2.95x the 0.005 rounding floor. So this test pins the filter's LOGIC, and
    /// must not be read as evidence the shape occurs in production. `elapsed_secs` is
    /// `saturating_duration_since` with only a `> 0` guard at the call site, so the observed
    /// ceiling is a property of this host's poll cadence, not of the code.
    #[test]
    fn observed_peak_folds_a_slow_climb_whose_rendered_rate_floored_to_zero() {
        let log = "\
ts=2026-07-11T00:00:00Z event=usage_velocity acct=u-A session_pct_per_min=0.00 weekly_pct_per_min=0.00 elapsed_secs=12060 session_delta_pct=1 weekly_delta_pct=0
ts=2026-07-11T00:01:00Z event=usage_velocity acct=u-A session_pct_per_min=0.00 weekly_pct_per_min=0.00 elapsed_secs=12060 session_delta_pct=0 weekly_delta_pct=3
ts=2026-07-11T00:02:00Z event=usage_velocity acct=u-A session_pct_per_min=-0.15 weekly_pct_per_min=0.00 elapsed_secs=12060 session_delta_pct=-30 weekly_delta_pct=0
";
        let inputs = parse_events(log, None);
        // EXACTLY ONE sample: the 1 %-over-12060 s climb. The flat session dimension (whose
        // non-zero WEEKLY delta is what got the line emitted at all) and the window reset are
        // both still excluded — the drop this fixes must not be traded for a new one.
        assert_eq!(
            inputs.session_velocities,
            vec![1.0 / (12060.0 / 60.0)],
            "a measured climb must survive its own rendered 0.00, while the flat and the reset stay dropped"
        );
    }

    #[test]
    fn observed_peak_flags_a_real_peak_that_outruns_the_assumed_v_peak() {
        // A single sample above v_peak (6.95) trips the recalibrate signal — the SLI's entire
        // purpose: when the live peak outruns the constant, the config-load coupling bound is
        // silently too loose and the constant needs re-calibrating (the "measure, don't trust the
        // constant" discipline TAIL_MARGIN has via the #595 landing SLI).
        let log = "ts=2026-07-11T00:00:00Z event=usage_velocity acct=u-A session_pct_per_min=8.40 weekly_pct_per_min=0.01 elapsed_secs=300 session_delta_pct=42 weekly_delta_pct=0\n";
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

    // --- issue #1179: canarying the `is_finite()` guards ----------------------
    //
    // WHAT THE CANARY BELOW COVERS, AND WHAT IT DOES NOT. Issue #1179 enumerates five `is_finite()`
    // sites in this module and reports none of them canaried. The tests here reach exactly ONE —
    // the `usage_velocity` arm's `.filter(|v| v.is_finite() && *v > 0.0)` in `parse_events`. The
    // rest are accounted for individually below, because "write a canary" was not the same verdict
    // for each: the two `record_blind_projection` INPUT filters are behaviour-preserving to delete,
    // its RESULT guard was already covered, and `compute_landing`'s post-swap window stayed open
    // until issue #1210 canaried it, over in the landing cluster. Stated here rather than left
    // implied, so the next reader does not take one gate's coverage for the family's.
    //
    //   - `record_blind_projection`'s `rate` and `inflation` INPUT filters: not canary-able, and
    //     not an unguarded gate either. Every non-finite input reaches `projected_pct` still
    //     non-finite: `corrected_anchor` is a `u8`, and the arm's own gate leaves
    //     `blind_secs > BLIND_GATE_SECS`, hence `>= 1`, so the `x × 0` escape does not exist —
    //     a NaN propagates through both the product and the sum, an infinity times a non-zero
    //     finite stays infinite, and `inf × 0` is NaN. The RESULT guard therefore catches every
    //     line either filter would have, landing it in the same `malformed` bucket by the same
    //     `return`. Deleting either is behaviour-preserving, so no test can distinguish it: they
    //     are defence in depth, not coverage debt.
    //   - `record_blind_projection`'s `projected_pct` RESULT guard: ALREADY canaried, by
    //     `blind_projection_error_never_publishes_a_non_finite_percentile` and
    //     `blind_projection_error_classifies_corruption_apart_from_coverage` — deleting it reds
    //     both. #1179 read the comment on the first as awareness rather than coverage; it is
    //     coverage, and it is what makes the bullet above true.
    //   - `compute_landing`'s post-swap sample window (`s.session.is_finite()`): CANARIED since
    //     issue #1210, in the landing cluster rather than here — by
    //     `landing_refuses_a_non_finite_parked_reading_rather_than_fabricating_one` and
    //     `landing_keeps_the_finite_peak_when_a_non_finite_reading_shares_the_window`, which sit
    //     beside the other `compute_landing` tests they share fixtures and idiom with. #1179 left it
    //     deliberately UNcanaried, on the grounds that nothing reaches it through the real parse
    //     path — those samples are `serde_json`-decoded from `usage-samples.jsonl`, and JSON has no
    //     infinity — so pinning it would assert the filter's arithmetic while implying a reachable
    //     hazard the store's own decoder forecloses. #1210 overrode the VERDICT and kept the
    //     reasoning: an unpinned guard is one a later reader deletes as dead weight, however its
    //     hazard is reached. What answers the objection is that the reachability claim is no longer
    //     prose — `the_sample_store_cannot_carry_a_non_finite_session_reading` asserts the decoder
    //     really does foreclose it, so the canary states its own status (defence in depth against an
    //     in-memory `Sample`) instead of implying a stronger one.

    /// A `usage_velocity` line whose recomputed rate is not finite, in the shapes a garbled durable
    /// log can carry it — all the same gate, approached from different sides.
    ///
    /// The two `session_delta_pct` shapes put the infinity in directly: `str::parse::<f64>` accepts
    /// both spellings and yields `f64::INFINITY` (`"inf"` by name, `"1e400"` by overflow), which
    /// [`non_finite_rate_reaches_the_filter_rather_than_failing_to_parse`] asserts rather than
    /// assumes — were either a parse ERROR, the sample would drop one combinator earlier and this
    /// fixture would be testing nothing.
    ///
    /// The `quotient overflows` shape is the one worth reading twice: both ingredients parse finite
    /// and pass every companion predicate (`1e-320` is subnormal, so `*secs > 0.0` holds), and the
    /// DIVISION overflows. It is the twin of the `rate=1e300 inflation=1e300` product overflow that
    /// [`blind_projection_error_never_publishes_a_non_finite_percentile`] pins, and it is why this
    /// guard belongs on the RESULT of the recomputation rather than on its inputs.
    ///
    /// None of these is emittable: `session_delta_pct` is an `i16` and `elapsed_secs` a whole-second
    /// count on [`crate::observability::Event::UsageVelocity`], so the daemon cannot write any of
    /// them. That is the point — this reader is deliberately tolerant of whatever is on disk (a torn
    /// write, an interleaved append, a hand-concatenated log), the same corruption class the
    /// `rate=oops` / `rate=NaN` fixtures in
    /// [`blind_projection_error_classifies_corruption_apart_from_coverage`] already stand for.
    const NON_FINITE_VELOCITY_LINES: &[(&str, &str)] = &[
        (
            "session_delta_pct spelled as an infinity",
            "ts=2026-07-11T00:00:00Z event=usage_velocity acct=u-A session_pct_per_min=inf weekly_pct_per_min=0.01 elapsed_secs=300 session_delta_pct=inf weekly_delta_pct=0\n",
        ),
        (
            "session_delta_pct overflowing f64 on parse",
            "ts=2026-07-11T00:01:00Z event=usage_velocity acct=u-B session_pct_per_min=inf weekly_pct_per_min=0.01 elapsed_secs=300 session_delta_pct=1e400 weekly_delta_pct=0\n",
        ),
        (
            "finite ingredients whose quotient overflows",
            "ts=2026-07-11T00:02:00Z event=usage_velocity acct=u-C session_pct_per_min=inf weekly_pct_per_min=0.01 elapsed_secs=1e-320 session_delta_pct=42 weekly_delta_pct=0\n",
        ),
    ];

    /// The premise [`observed_peak_excludes_a_non_finite_recomputed_rate`] rests on, asserted so it
    /// cannot rot into a vacuous pass: the infinity must actually REACH the `is_finite()` filter. If
    /// `"inf"` or `"1e400"` failed to parse, `.and_then(|v| v.parse::<f64>().ok())` would drop the
    /// sample first, the exclusion would be attributable to the parse rather than to the guard, and
    /// deleting the guard would leave the canary green — a gate proving nothing while reading as
    /// coverage.
    ///
    /// The `> 0.0` assertion is why the guard is not redundant with its own companion predicate:
    /// `f64::INFINITY > 0.0` is TRUE, so "simplifying" `is_finite() && *v > 0.0` down to `*v > 0.0`
    /// on the reasonable-looking grounds that a positivity check subsumes a finiteness one admits
    /// every positive infinity in the file.
    #[test]
    fn non_finite_rate_reaches_the_filter_rather_than_failing_to_parse() {
        // Bound through `str::parse` rather than written as `f64::INFINITY` literals, so these are
        // values the real parse path produced — the claim under test — and not constants a reader
        // (or the optimizer) can fold away without ever touching the parser.
        let named = "inf".parse::<f64>().expect("the named spelling must PARSE");
        let overflowed = "1e400"
            .parse::<f64>()
            .expect("an overflowing literal must PARSE");
        assert_eq!(
            (named, overflowed),
            (f64::INFINITY, f64::INFINITY),
            "both spellings must reach the filter as infinities, or the canary excludes for the wrong reason"
        );
        assert!(
            named > 0.0,
            "the companion `> 0.0` predicate does NOT cover for a missing `is_finite()`"
        );
        // And the quotient-overflow shape's ingredients are individually finite and individually
        // admissible, so its exclusion is likewise the guard on the RESULT doing the work.
        let secs = "1e-320".parse::<f64>().expect("a subnormal parses");
        assert!(
            secs.is_finite() && secs > 0.0,
            "{secs} clears every input-side gate"
        );
        assert!(
            !(42.0f64 / (secs / 60.0)).is_finite(),
            "yet the quotient overflows — which is why the guard sits on the result"
        );
    }

    /// The canary (issue #1179): a non-finite recomputed rate is EXCLUDED from the session-velocity
    /// distribution, asserted per shape so each is separately falsifiable.
    ///
    /// Every shape is checked before asserting, rather than one `assert!` per iteration, so the
    /// mutation report names ALL the shapes that got through instead of stopping at the first.
    /// Deleting `v.is_finite() &&` from the arm's filter REDs this, and reds it for the stated
    /// reason: the message prints each offending shape beside the `[inf]` it admitted, which no
    /// parse failure could produce.
    #[test]
    fn observed_peak_excludes_a_non_finite_recomputed_rate() {
        let admitted: Vec<(&str, Vec<f64>)> = NON_FINITE_VELOCITY_LINES
            .iter()
            .map(|(shape, line)| (*shape, parse_events(line, None).session_velocities))
            .filter(|(_, rates)| !rates.is_empty())
            .collect();
        assert!(
            admitted.is_empty(),
            "no garbled shape may enter the distribution; these did: {admitted:?}"
        );
    }

    /// What that exclusion BUYS, pinned on the verdict rather than on the vector: an infinity that
    /// reached the distribution would become `p100`, and `inf <= V_PEAK_SESSION_PCT_PER_MIN + 1e-9`
    /// is false — so one garbled line would flip the honesty verdict this SLI exists to publish, and
    /// send an operator re-calibrating a constant that is still sound.
    ///
    /// The 4.20 %/min control line is load-bearing twice over. It proves the arm is LIVE on this
    /// fixture — were the whole block malformed, the equality below would fail rather than pass on
    /// an empty distribution, the cardinality-zero trap this module refuses elsewhere — and it holds
    /// `p100` under the assumed peak, so `v_peak_honest` reads `Some(true)` on the shipped tree and
    /// `Some(false)` under the mutation. Internally consistent like every fixture here: `42 / (600 /
    /// 60)` is exactly the rendered `4.20`, bit-for-bit.
    #[test]
    fn a_garbled_rate_never_flips_the_v_peak_honesty_verdict() {
        let mut log = String::from(
            "ts=2026-07-11T00:03:00Z event=usage_velocity acct=u-D session_pct_per_min=4.20 weekly_pct_per_min=0.01 elapsed_secs=600 session_delta_pct=42 weekly_delta_pct=0\n",
        );
        for (_, line) in NON_FINITE_VELOCITY_LINES {
            log.push_str(line);
        }
        let inputs = parse_events(&log, None);
        // Asserted on the CONTENTS, not on a count: under the mutation this prints
        // `[4.2, inf, inf, inf]`, naming the live control and every admitted infinity at once.
        assert_eq!(
            inputs.session_velocities,
            vec![4.2],
            "only the control may reach the distribution — a live arm with no infinity in it"
        );
        let r = aggregate(&inputs, &[], None);
        assert_eq!(r.observed_peak.p100, Some(4.2));
        assert_eq!(
            r.observed_peak.v_peak_honest(),
            Some(true),
            "the constant still bounds the real peak — only a garbage line said otherwise"
        );
        // The renderers never see it either — the same two-surface discipline the blind arm's
        // non-finite guard keeps.
        let human = render_human(&r);
        assert!(!human.contains("inf"), "no inf in human text: {human}");
        let json = render_json(&r).expect("wire serializes");
        assert!(
            json.contains("\"v_peak_honest\": true"),
            "the wire must carry the unflipped verdict: {json}"
        );
    }

    /// The `<=` boundary of [`ObservedPeak::v_peak_honest`]: a max recorded at EXACTLY the assumed
    /// peak is still honest — the constant bounds the observation, so nothing needs re-calibrating.
    ///
    /// Pinned directly on the predicate rather than through a log fixture because no emittable line
    /// can carry exactly 6.95 (see [`VELOCITY_LOG`] — it would need a `session_delta_pct` of at
    /// least 139). Asserting it here also makes the boundary independent of the parse path, so a
    /// future change to how the rate is derived cannot quietly retire this case.
    #[test]
    fn v_peak_honest_admits_a_sample_exactly_at_the_assumed_peak() {
        let at_peak = ObservedPeak {
            n: 1,
            p50: Some(crate::swap::V_PEAK_SESSION_PCT_PER_MIN),
            p90: Some(crate::swap::V_PEAK_SESSION_PCT_PER_MIN),
            p100: Some(crate::swap::V_PEAK_SESSION_PCT_PER_MIN),
        };
        assert_eq!(
            at_peak.v_peak_honest(),
            Some(true),
            "a max exactly AT the assumed peak is bounded by it — `<=`, not `<`"
        );
        // And a sample above it is not, so the assertion above is not vacuously true for every
        // input. `1e-6` is chosen to clear `v_peak_honest`'s ABSOLUTE `1e-9` tolerance, not because
        // it is the smallest representable step: one ulp at 6.95 is ~8.88e-16, which sits INSIDE
        // that tolerance, so substituting a true ulp here makes this assertion fail. Measured, not
        // argued — `f64::from_bits(V_PEAK_SESSION_PCT_PER_MIN.to_bits() + 1)` in this slot REDs
        // `v_peak_honest_admits_a_sample_exactly_at_the_assumed_peak`.
        let over = ObservedPeak {
            p100: Some(crate::swap::V_PEAK_SESSION_PCT_PER_MIN + 1e-6),
            ..at_peak
        };
        assert_eq!(
            over.v_peak_honest(),
            Some(false),
            "a max above the assumed peak must flag the constant as too loose"
        );
    }

    // --- issue #1172: which layer may quantize this SLI's rates -----------------

    /// An ordinary emittable line whose rate exceeds `V_PEAK_SESSION_PCT_PER_MIN` by less than half
    /// of the 2-dp quantum: `session_delta_pct=19` over `elapsed_secs=164` is `6.9512…` %/min.
    ///
    /// Both ingredients are shapes the emitter really produces — `session_delta_pct` is an `i16` and
    /// `elapsed_secs` a whole-second interval — so this is not a synthetic float chosen to sit in the
    /// gap. It was found by scanning integer `(delta, elapsed)` pairs for a rate inside
    /// `(6.95, 6.955)`; the first is `(19, 164)`, and a 19-point climb over 2 min 44 s is exactly the
    /// heavy-usage burst this SLI is watching for.
    const NEAR_BOUNDARY_LOG: &str = "ts=2026-07-11T00:00:00Z event=usage_velocity acct=u-A session_pct_per_min=6.95 weekly_pct_per_min=0.01 elapsed_secs=164 session_delta_pct=19 weekly_delta_pct=0\n";

    /// [`ObservedPeak::v_peak_honest`] must compare the rate AS MEASURED, never a display-rounded
    /// one — the constraint that decides which layer may quantize this SLI (issue #1172), and the
    /// reason [`round_pp`] is not reached for from [`aggregate`]'s `velocity_pct`.
    ///
    /// The second half asserts the COUNTERFACTUAL rather than merely avoiding it: rounding at
    /// aggregation flips this exact sample from "re-calibrate" to "ok". Without it the first
    /// assertion looks like an arbitrary precision preference, and the next reader re-derives the
    /// trap from scratch — or does not.
    #[test]
    fn v_peak_honest_compares_the_unrounded_peak() {
        let r = aggregate(&parse_events(NEAR_BOUNDARY_LOG, None), &[], None);
        let p100 = r.observed_peak.p100.expect("one positive sample");
        assert!(
            p100 > crate::swap::V_PEAK_SESSION_PCT_PER_MIN,
            "fixture must sit ABOVE the assumed peak or this test asserts nothing: {p100}"
        );
        assert_eq!(
            r.observed_peak.v_peak_honest(),
            Some(false),
            "a real peak above the constant must flag it too loose, however small the excess"
        );

        // The rejected repair, made executable. `round_pp` lands this sample exactly ON the
        // constant, and `<=` then reads it as bounded — the SLI reporting the constant honest at the
        // one moment it is not.
        assert_eq!(
            round_pp(p100),
            crate::swap::V_PEAK_SESSION_PCT_PER_MIN,
            "the trap's premise: 2-dp rounding puts this peak exactly on the constant"
        );
        let as_if_rounded = ObservedPeak {
            p100: Some(round_pp(p100)),
            ..r.observed_peak
        };
        assert_eq!(
            as_if_rounded.v_peak_honest(),
            Some(true),
            "rounding at aggregation would invert the verdict — why this SLI must not use round_pp"
        );
    }

    /// The issue #1172 divergence itself: one `Report`, two renderers, and they must not report
    /// different NUMBERS for the same sample.
    ///
    /// The fixture is the issue #1158 climb that `{:.2}` floored to `0.00` — `session_delta_pct=1`
    /// over 12 060 s. Reading the ingredients kept the sample; this asserts the readout an operator
    /// looks at finally shows it, while the wire keeps the value at full precision for a calibration
    /// script. Both surfaces render ONE stored value; neither re-quantizes what is stored.
    #[test]
    fn both_surfaces_report_the_same_sub_two_dp_climb() {
        let log = "ts=2026-07-11T00:00:00Z event=usage_velocity acct=u-A session_pct_per_min=0.00 weekly_pct_per_min=0.00 elapsed_secs=12060 session_delta_pct=1 weekly_delta_pct=0\n";
        let r = aggregate(&parse_events(log, None), &[], None);

        let human = render_human(&r);
        assert!(
            human.contains("  P100 = 0.0050 %/min"),
            "the human readout must show a sub-0.01 climb as a rate, not as 0.00: {human}"
        );
        assert!(
            !human.contains("= 0.00 %/min"),
            "no percentile may still render as a flat zero: {human}"
        );

        let json = render_json(&r).expect("serializes");
        assert!(
            json.contains("\"p100\": 0.004975124378109453"),
            "the wire must keep the rate unrounded for a calibration consumer: {json}"
        );
    }

    /// A `[RECALIBRATE]` verdict must be legible in the numbers printed beside it.
    ///
    /// At 2 dp this line read `P100 = 6.95 %/min  vs assumed v_peak 6.95 %/min  [RECALIBRATE]` —
    /// two identical figures and a flag apparently contradicting them. Both are widened together,
    /// because the line is a comparison. This does not hold for an excess below `0.00005`, where the
    /// two again print alike; the flag, computed from the stored value, stays authoritative.
    #[test]
    fn human_render_shows_the_excess_behind_a_recalibrate_flag() {
        let r = aggregate(&parse_events(NEAR_BOUNDARY_LOG, None), &[], None);
        let human = render_human(&r);
        assert!(
            human.contains(
                "  P100 = 6.9512 %/min  vs assumed v_peak 6.9500 %/min  [RECALIBRATE]\n"
            ),
            "the printed peak must visibly exceed the printed constant when the flag says so: {human}"
        );
    }

    /// 4 dp is the narrowest width that leaves NO emittable line printing a real excess as a tie —
    /// the measurement behind [`render_human`]'s "why 4 dp and not 3", and the reason an operator
    /// must never discount two equal-looking figures under `[RECALIBRATE]` as a rounding artifact.
    ///
    /// The claim is bounded, not universal: as a FORMAT `{:.4}` still ties on an excess below
    /// `0.00005`. What closes it is the emitter. `session_delta_pct` is a difference of two
    /// [`crate::daemon::snapshot::to_pct`] values, each clamped to `0..=100`, so the numerator is
    /// an integer in `-100..=100`; requiring a real excess then bounds `elapsed_secs`, because the
    /// rate falls monotonically as it grows. That makes the search finite and this sweep
    /// exhaustive over it rather than a sample of it.
    #[test]
    fn four_dp_leaves_no_emittable_near_boundary_tie() {
        let v_peak = crate::swap::V_PEAK_SESSION_PCT_PER_MIN;
        // Every integer (delta, elapsed) carrying a REAL excess that still renders alike at `width`.
        let ties = |width: usize| -> Vec<(i32, i32)> {
            let mut out = Vec::new();
            for delta in 1..=100 {
                for elapsed in 1..1_000_000 {
                    let rate = f64::from(delta) * 60.0 / f64::from(elapsed);
                    if rate <= v_peak {
                        break; // monotonically decreasing in `elapsed` — nothing further can exceed
                    }
                    if format!("{rate:.*}", width) == format!("{v_peak:.*}", width) {
                        out.push((delta, elapsed));
                    }
                }
            }
            out
        };

        // Canaried against a width whose answer is already known and pinned elsewhere: 2 dp is the
        // regime this change replaces, and NEAR_BOUNDARY_LOG's own pair must be the first hit. A
        // sweep that cannot reproduce a known-positive is not evidence about the negative.
        let two = ties(2);
        assert_eq!(
            two.first(),
            Some(&(19, 164)),
            "the 2-dp sweep must rediscover NEAR_BOUNDARY_LOG's own pair, or it is not measuring \
             what this test claims"
        );
        assert_eq!(two.len(), 30, "2 dp: {two:?}");
        assert_eq!(ties(3).len(), 3, "3 dp still leaves emittable ties");
        assert_eq!(
            ties(4),
            Vec::new(),
            "4 dp must leave NONE — if this reds, render_human's stated bound is false and the \
             comment must be narrowed to what the sweep actually shows"
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

    // --- issue #719: all_exhausted capacity-holds segregated from the #363 reaction-latency gate ---

    #[test]
    fn all_exhausted_holds_are_segregated_from_the_reaction_latency_gate() {
        // AC #5: a swap that resolved an `all_exhausted` capacity-hold (the OUTGOING account was
        // pinned at the ceiling because ALL peers were exhausted — no viable target) is NOT a
        // reaction-latency miss, so it must not count against the #363 P100 gate; a plain climb→swap
        // in the same log MUST still count. The discriminator is `hold == to`: `all_exhausted hold=`
        // names the soonest-returning SPARE the daemon holds out for, and the relief swap lands ON it
        // (verified against the live event log — hold==to, never hold==from). The hold is reset on ANY
        // swap (the relief edge), so a later swap is clean again.
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=a to=b reason=session session_pct=96
ts=2026-07-11T00:01:00Z event=all_exhausted hold=c cause=all_accounts_exhausted resets_at=2026-07-11T05:00:00Z
ts=2026-07-11T00:02:00Z event=swap from=b to=c reason=session session_pct=100
ts=2026-07-11T00:03:00Z event=swap from=c to=a reason=session session_pct=98
";
        let inputs = parse_events(log, None);
        // The two plain climbs stay in the reaction-latency bucket; the hold-resolving swap (100, its
        // `to=c` matching the awaited spare `hold=c`, its pinned `from=b` the capacity casualty) is
        // segregated out. Reset-on-swap: the c→a swap is clean despite the earlier exhaustion, because
        // the b→c swap already consumed (cleared) the hold.
        assert_eq!(inputs.swap_out_pcts, vec![96.0, 98.0]);
        assert_eq!(inputs.swap_out_held_pcts, vec![100.0]);

        let r = aggregate(&inputs, &[], None);
        // Reaction-latency gate (#363) sees only the clean climbs: P100=98 < 99 → MET. The 100 does
        // NOT drag it to an unreachable breach.
        assert_eq!(r.swap_overshoot.n, 2);
        assert_eq!(r.swap_overshoot.p100, Some(98));
        assert_eq!(r.swap_overshoot.p100_met(), Some(true));
        // The capacity-held partition carries the excluded swap, out of the gate.
        assert_eq!(r.capacity_held.n, 1);
        assert_eq!(r.capacity_held.p100, Some(100));
        // Landing SLI segregates too: the held anchor is excluded from swaps_total and surfaced as a
        // capacity_held count, never mislabeled gap-crossing.
        assert_eq!(r.landing.swaps_total, 2);
        assert_eq!(r.landing.capacity_held, 1);
    }

    #[test]
    fn the_leave_edge_is_inert_on_a_relief_swap_tick() {
        // On a RELIEF-swap tick the LEAVE edge changes nothing — not because the parser ignores it
        // (issue #828 gave the kind its own arm), but because the swap arm consumed the hold before
        // this line is ever read. That inertness is a consequence of emission ORDER, so this is the
        // parser-side guard on the daemon-side contract
        // `daemon::tests::a_relief_swap_emits_the_durable_leave_edge_after_its_own_swap_event` pins:
        // the clear is pushed post-`decide_action`, hence `event=swap` THEN
        // `event=all_exhausted_cleared` on the same tick. (This NARROWS the claim the test asserted
        // before issue #828 — that the SLI is indifferent to the marker because an unrecognised
        // `event=` kind falls through every arm, which the new arm made false.)
        //
        // The whole-`Inputs` equivalence is kept rather than spot-checking one field: it also covers
        // `horizon_ts` and every other ingredient, and it is what proves issue #828's new arm left
        // the issue #719 relief-swap classification undisturbed. The cleared line sits exactly where
        // the daemon emits it — on the relief swap's own tick, AFTER the swap event — and its `ts`
        // is not the log's maximum, so the observation horizon is untouched too.
        let without = "\
ts=2026-07-11T00:01:00Z event=all_exhausted hold=c cause=all_accounts_exhausted resets_at=2026-07-11T05:00:00Z
ts=2026-07-11T00:02:00Z event=swap from=b to=c reason=session session_pct=100
ts=2026-07-11T00:03:00Z event=swap from=c to=a reason=session session_pct=98
";
        let with = "\
ts=2026-07-11T00:01:00Z event=all_exhausted hold=c cause=all_accounts_exhausted resets_at=2026-07-11T05:00:00Z
ts=2026-07-11T00:02:00Z event=swap from=b to=c reason=session session_pct=100
ts=2026-07-11T00:02:00Z event=all_exhausted_cleared
ts=2026-07-11T00:03:00Z event=swap from=c to=a reason=session session_pct=98
";
        assert_eq!(parse_events(with, None), parse_events(without, None));
        // …and the #719 classification the bracket sits inside is still correct with the LEAVE
        // edge present: the relief swap (`to=c` == `hold=c`) is still segregated as capacity-held,
        // and the later swap is still a clean reaction-latency sample. A cleared line must neither
        // consume the hold early nor resurrect a consumed one.
        let inputs = parse_events(with, None);
        assert_eq!(inputs.swap_out_held_pcts, vec![100.0]);
        assert_eq!(inputs.swap_out_pcts, vec![98.0]);
    }

    #[test]
    fn the_promoted_leave_edges_are_folded_transparently() {
        // Issue #827 promoted the OTHER two LEAVE edges — the active-dead-no-target strand's
        // (issue #405) and the proactive fleet-runway warn's (issue #650) — from stderr
        // `Diagnostic`s to durable `Event`s, so `event=active_dead_no_target_cleared` and
        // `event=fleet_runway_recovered` lines now appear in the log this parser reads.
        //
        // Unlike `all_exhausted_cleared` (which issue #828 gave its OWN arm, because a stale
        // `hold=` corrupts the issue #719 capacity-held classification), these two must be fully
        // INERT here: neither state feeds any reliability SLI. The parser is a tolerant field-map
        // fold, so an unrecognised `event=` kind falls through every arm — pinned by parsing the
        // SAME log with and without the two new lines and asserting the folds are IDENTICAL, a
        // stronger claim than spot-checking one field since it also covers `horizon_ts` and every
        // other ingredient.
        //
        // The two lines sit where the daemon emits them: each clear rides its own tick AFTER any
        // decision event, and the active-dead strand's real exit is an EMERGENCY swap — which IS a
        // re-activation edge for the issue #595 landing filter, so the surrounding swap here is a
        // load-bearing part of the shape rather than decoration. Neither `ts` is the log's
        // maximum, so the observation horizon is untouched too.
        let without = "\
ts=2026-07-11T00:01:00Z event=active_dead_no_target hold=a cause=weekly resets_at=2026-07-11T05:00:00Z
ts=2026-07-11T00:02:00Z event=emergency_swap from=a to=b
ts=2026-07-11T00:04:00Z event=swap from=b to=c reason=session session_pct=97
";
        let with = "\
ts=2026-07-11T00:01:00Z event=active_dead_no_target hold=a cause=weekly resets_at=2026-07-11T05:00:00Z
ts=2026-07-11T00:02:00Z event=emergency_swap from=a to=b
ts=2026-07-11T00:02:00Z event=active_dead_no_target_cleared
ts=2026-07-11T00:03:00Z event=fleet_runway_low runway_secs=1800 threshold_secs=3600 counted=2 observed=2
ts=2026-07-11T00:03:30Z event=fleet_runway_recovered
ts=2026-07-11T00:04:00Z event=swap from=b to=c reason=session session_pct=97
";
        assert_eq!(parse_events(with, None), parse_events(without, None));
        // And the fold is non-degenerate — an equivalence between two EMPTY parses would pass
        // vacuously, so pin that the surrounding swap really did land as a reaction-latency
        // sample with the new lines present.
        assert_eq!(parse_events(with, None).swap_out_pcts, vec![97.0]);
    }

    #[test]
    fn the_refresh_token_expiry_lines_are_folded_transparently() {
        // Issue #880 added two durable event kinds to the log this parser reads — the horizon-entry
        // edge (`event=credential_expiry_horizon`) and the provenance-tagged deadline observation
        // (`event=credential_expiry_observed`). Neither may contribute to any reliability SLI:
        // refresh-token expiry is a FORESIGHT axis. Attributing it to one is issue #881's job, and
        // doing it accidentally — through a field this tolerant map-fold happens to share — is
        // exactly what this pins against.
        //
        // "Contributes to no SLI" is the precise claim, NOT that the lines are wholly inert: the
        // `horizon_ts` fold above the event `match` runs on EVERY line carrying a parseable `ts`, so
        // these two advance the observation horizon exactly as every other durable event does. That
        // is correct — the horizon measures how long the log was being written, and these lines are
        // evidence it was — and the second half of this test pins it rather than leaving it to be
        // rediscovered as a surprise.
        //
        // Sibling of `the_promoted_leave_edges_are_folded_transparently` above, and pinned the same
        // stronger way: parse the SAME log with and without the new lines and assert the folds are
        // IDENTICAL, which also covers `horizon_ts` and every other ingredient rather than
        // spot-checking one field.
        //
        // The shapes are the real ones. `acct=` is deliberately NOT the `from=`/`to=`/`hold=` key
        // any arm reads, and neither line carries a `reason=`; the observed line's trailing
        // `delta_secs=` is the kind of numeric field a careless arm could pick up. Neither `ts` is
        // the log's maximum, so the observation horizon is untouched too.
        let without = "\
ts=2026-07-11T00:01:00Z event=all_exhausted hold=a cause=session resets_at=2026-07-11T05:00:00Z
ts=2026-07-11T00:04:00Z event=swap from=b to=c reason=session session_pct=97
";
        let with = "\
ts=2026-07-11T00:01:00Z event=all_exhausted hold=a cause=session resets_at=2026-07-11T05:00:00Z
ts=2026-07-11T00:02:00Z event=credential_expiry_observed acct=u-A provenance=first_observation after=2026-08-30T12:00:00Z
ts=2026-07-11T00:02:30Z event=credential_expiry_horizon acct=u-A state=within expires_at=2026-07-16T12:00:00Z horizon_secs=604800
ts=2026-07-11T00:03:00Z event=credential_expiry_observed acct=u-A provenance=canonical_restash before=2026-07-16T12:00:00Z after=2026-07-16T12:00:00Z delta_secs=0
ts=2026-07-11T00:04:00Z event=swap from=b to=c reason=session session_pct=97
";
        assert_eq!(parse_events(with, None), parse_events(without, None));
        // Non-degenerate: an equivalence between two EMPTY parses would pass vacuously, so pin that
        // the surrounding swap still landed as a sample with the new lines interleaved.
        assert!(!parse_events(with, None).swap_out_pcts.is_empty());

        // The one fold these lines DO reach, stated as behaviour instead of assumed away: a #880 line
        // that is the log's LATEST advances `horizon_ts`, and changes nothing else. Asserted by
        // normalizing the horizon and re-comparing — so if a future arm ever started reading one of
        // these lines into an SLI, this would fail rather than be masked by the horizon difference.
        let trailing = format!(
            "{without}ts=2026-07-11T09:00:00Z event=credential_expiry_horizon acct=u-A state=lapsed expires_at=2026-07-11T08:00:00Z horizon_secs=604800\n"
        );
        let mut folded = parse_events(&trailing, None);
        let baseline = parse_events(without, None);
        assert_eq!(
            folded.horizon_ts,
            Some(epoch_from_rfc3339("2026-07-11T09:00:00Z").unwrap()),
            "a trailing durable line extends the observation horizon"
        );
        assert_ne!(
            folded.horizon_ts, baseline.horizon_ts,
            "…and the two logs genuinely differ there, so the normalization below is not vacuous"
        );
        folded.horizon_ts = baseline.horizon_ts;
        assert_eq!(
            folded, baseline,
            "the horizon is the ONLY thing a #880 line touches"
        );
    }

    #[test]
    fn a_relief_swap_to_a_different_account_than_the_hold_is_not_capacity_held() {
        // The discriminator is precise: `all_exhausted hold=` names ONE awaited spare; a swap whose
        // `to=` is a DIFFERENT account did not land on that spare, so it did not resolve THAT hold and
        // stays a reaction-latency swap. Guards against over-counting every swap after any exhaustion
        // as capacity-held.
        let log = "\
ts=2026-07-11T00:00:00Z event=all_exhausted hold=x cause=all_accounts_exhausted resets_at=2026-07-11T05:00:00Z
ts=2026-07-11T00:01:00Z event=swap from=y to=z reason=session session_pct=99
";
        let inputs = parse_events(log, None);
        assert_eq!(inputs.swap_out_pcts, vec![99.0]);
        assert!(inputs.swap_out_held_pcts.is_empty());
    }

    #[test]
    fn a_hold_left_without_a_swap_does_not_taint_a_later_unrelated_swap() {
        // Issue #828, and the LEAVE-with-no-swap shape the clear arm exists for: here the active
        // account's own session window resets and it drops below its trigger, so the tick decides
        // `Hold`, not `Swapped`, and the episode ends on a bare marker. That shape is pinned
        // daemon-side by `daemon::tests::leaving_the_all_exhausted_state_clears_the_edge_guard`,
        // which asserts the LEAVE tick emits `Event::AllExhaustedCleared` and NOTHING else — so this
        // log is the daemon's real output, not an invented shape.
        //
        // Before the fix the parser cleared `exhausted_hold` on a swap and on nothing else, so the
        // stale hold outlived the leave and the next unrelated swap that merely HAPPENED to land on
        // the former `hold=` account was misclassified as capacity-held — deleting a genuine
        // reaction-latency sample in the direction that FLATTERS the #363 gate. That is why the
        // assertions below check the gate's VERDICT, not just the partition.
        let log = "\
ts=2026-07-11T00:00:00Z event=all_exhausted hold=c cause=all_accounts_exhausted resets_at=2026-07-11T05:00:00Z
ts=2026-07-11T00:01:00Z event=all_exhausted_cleared
ts=2026-07-11T00:02:00Z event=swap from=a to=c reason=session session_pct=99
";
        let inputs = parse_events(log, None);
        // The hold ended at 00:01 with no relief swap. The 00:02 swap is a plain climb→swap that
        // only coincidentally lands on `c`; it resolved no capacity hold, so it is a
        // reaction-latency sample and must reach the #363 gate.
        assert_eq!(inputs.swap_out_pcts, vec![99.0]);
        assert!(inputs.swap_out_held_pcts.is_empty());

        let r = aggregate(&inputs, &[], None);
        // The stakes, made explicit: with the stale hold the gate saw a CARDINALITY-ZERO subject
        // (n=0, p100=None, verdict None — no verdict at all); with the hold correctly closed it
        // sees the sample and correctly reports the breach (99 is not `< 99`). A gate that reads
        // "no data" because a sample was misfiled is exactly the flattering failure.
        assert_eq!(r.swap_overshoot.n, 1);
        assert_eq!(r.swap_overshoot.p100, Some(99));
        assert_eq!(r.swap_overshoot.p100_met(), Some(false));
        assert_eq!(r.capacity_held.n, 0);
        // The classification feeds a SECOND consumer: landing anchors carry the same flag through
        // `SwapOut::held` (issue #595), so the stale hold was emptying the landing SLI's denominator
        // too — `(capacity_held, swaps_total)` read `(1, 0)` before this fix and `(0, 1)` after. That
        // is a second cardinality-zero subject in the same flattering direction, and it was covered
        // only transitively; pin it directly so a future change cannot split the two consumers apart.
        assert_eq!(r.landing.capacity_held, 0);
        assert_eq!(r.landing.swaps_total, 1);
    }

    #[test]
    fn a_leave_edge_closes_only_its_own_episode_and_a_re_entry_re_arms() {
        // Issue #828's clear arm resets the pending hold UNCONDITIONALLY, so the companion property
        // is that it disarms the episode it ends and nothing more — a later re-entry must arm a
        // FRESH hold and still be classifiable. Without this, a clear that permanently disabled hold
        // tracking would look identical to the fix in the single-episode test above, and every
        // capacity-hold after the fleet's first LEAVE would be silently re-admitted to the #363 gate
        // (the exact defect this issue fixes, merely displaced one episode later).
        //
        // Two episodes, and the second names a DIFFERENT spare — so this also pins that the re-entry
        // arms on the new `hold=`, not a remembered one.
        let log = "\
ts=2026-07-11T00:00:00Z event=all_exhausted hold=c cause=all_accounts_exhausted resets_at=2026-07-11T05:00:00Z
ts=2026-07-11T00:01:00Z event=all_exhausted_cleared
ts=2026-07-11T00:02:00Z event=swap from=a to=c reason=session session_pct=99
ts=2026-07-11T00:03:00Z event=all_exhausted hold=d cause=all_accounts_exhausted resets_at=2026-07-11T05:00:00Z
ts=2026-07-11T00:04:00Z event=swap from=c to=d reason=session session_pct=100
";
        let inputs = parse_events(log, None);
        // First episode ended without a swap → its trailing swap onto `c` is reaction-latency.
        // Second episode ended IN its relief swap onto `d` (`hold=d` == `to=d`) → capacity-held.
        assert_eq!(inputs.swap_out_pcts, vec![99.0]);
        assert_eq!(inputs.swap_out_held_pcts, vec![100.0]);
    }

    #[test]
    fn human_render_is_stable_and_targets_documented() {
        let out = render_human(&fixture_report());
        assert_eq!(
            out,
            concat!(
                "sessiometer reliability — swap-out overshoot SLO readout (offline; reads the event log + usage samples)\n",
                "\n",
                "swap-out session_pct (reason=session, reaction-latency), n=2\n",
                "  P50  = 96  target <= 97  [ok]\n",
                "  P95  = 100\n",
                "  P100 = 100  target < 99   [OVER]\n",
                "\n",
                "projective swap-out session_pct (reason=velocity_preempt): no projective swaps observed\n",
                "\n",
                "landing-point session_pct (post-swap peak of the outgoing account): no post-swap samples in window (2 of 2 reason=session swaps unmeasured)\n",
                "\n",
                "landing-point session_pct, OPERATOR-initiated swaps: no post-swap samples in window (1 of 1 operator swaps unmeasured)\n",
                "\n",
                "observed session velocity (session_pct_per_min, positive climbs only; the v_peak reserve-coupling calibration input)\n",
                "  measured n=1 usage_velocity samples\n",
                "  P50  = 0.2000 %/min\n",
                "  P90  = 0.2000 %/min\n",
                "  P100 = 0.2000 %/min  vs assumed v_peak 6.9500 %/min  [ok]\n",
                "\n",
                "time blind & near-limit: 900s (sum of blind_window duration_secs where near_limit=true)\n",
                // The issue #591 uncensored census, rendered BESIDE the censored 900s figure — never
                // summed into it. The u-D episode is 999s and swapped away, exactly the tail
                // blind_window cannot see; that the line above still reads 900 is the assertion.
                "  uncensored episodes: entered=1 exited=1 swapped_away=1 never_recovered=0 anchor_lost=0\n",
                "  near-limit blind time: >= 999s (999s measured + 0s right-censored floor, n=1)\n",
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
                "  censoring: RECOVERED-ONLY — excludes 1 swapped-away and 0 never-recovered episodes (counted from the uncensored blind_enter/blind_exit pair above)\n",
                "  no reconcilable blind windows — percentiles withheld (an empty subject is not a 0 pp error)\n",
                "\n",
                "usage-poll 429 neutrality (roster-wide): rate_limited=2 transient=1 cleared=1\n",
                "\n",
                // Issue #881: the refresh-token-loss block, always rendered — at ZERO here, since
                // FIXTURE_LOG carries no refresh-family line. Pinned in the byte-exact expectation
                // rather than only in a `contains` assertion so its position (last, after the 429
                // line) and its zero wording are both regression-locked.
                "refresh-token loss (credential lifecycle — cure is `sessiometer login`; NOT a swap-out failure, and folded into no SLI above)\n",
                "  accounts observed lost: 0 (from 0 dead-refresh observations: sweep=0 poll-retry=0 parked-recovery=0 keep-warm=0)\n",
                "  confirmed unrecoverable (automated recovery exhausted): 0\n",
            )
        );
    }

    #[test]
    fn human_render_handles_no_swaps() {
        let out = render_human(&aggregate(&parse_events("", None), &[], None));
        assert!(
            out.contains(
                "swap-out session_pct (reason=session, reaction-latency): no swaps observed"
            ),
            "cardinality-zero must not print a fabricated P100: {out}"
        );
    }

    #[test]
    fn json_render_is_stable_schema_12() {
        // The whole-log default: `window` is null and every field except the #635-renamed
        // velocity-projection key (`projective_swap_out_pct`, schema:6) is byte-identical to
        // schema:1–5 — the additive contract (#494/#539/#595/#608/#636/#591) plus the one #635
        // rename. The #608 `observed_peak` object is always-present (n=1 here — the FIXTURE_LOG's
        // single usage_velocity line at 0.20 %/min, well under the 6.95 v_peak, so
        // v_peak_honest=true), as is the #636 `blind_projection_error` object (schema:7 — the
        // FIXTURE_LOG's three blind_window lines predate #634's ingredients, so all three land in
        // `n_without_velocity` and no percentile is asserted).
        //
        // schema:8 (issue #591) adds `blind_episodes` and FILLS the two censored-tail counts that
        // schema:7 always emitted as `null`. The u-D pair supplies them: one entered, one exited,
        // `swapped_away=true`, `near_limit=true`, 999 s. Note what did NOT move —
        // `time_blind_near_limit_secs` stays 900 and the `false_preempt.proxy` counts stay 2/1, the
        // blind_window population untouched by the issue #591 routing decision. That pinning is the
        // point: the uncensored 999 s episode is published BESIDE the censored 900 s figure, never
        // summed into it. A `--since` document is asserted separately in
        // `json_documents_the_active_window`.
        //
        // schema:9 (issue #719) adds the top-level `capacity_held` object (between `swap_overshoot`
        // and `projective_swap_out_pct`) and `landing.capacity_held` — both ADDITIVE and always
        // present. FIXTURE_LOG carries no all_exhausted, so both read empty (n=0 / all-null / 0): the
        // segregation is inert here, so every prior figure is byte-unchanged but for the schema bump.
        //
        // schema:10 (issue #881) appends the top-level `refresh_token_loss` object — ADDITIVE, and
        // this time with no value-domain correction at all: it folds refresh-family events, which
        // are DISJOINT from the `event=swap` population every figure above is computed from. So
        // unlike the 8→9 bump, nothing above moved even in meaning. This document is the direct
        // evidence for that acceptance: read it against the schema:9 expectation in git history and
        // exactly two things differ — the version integer and the appended block.
        //
        // schema:11 (issue #1367) adds `refresh_token_loss.by_mechanism.parked_recovery` — ADDITIVE
        // in keys, and carrying a value-domain correction `poll_retry` cannot show here: FIXTURE_LOG
        // has no dead-refresh line at all, so both read 0 and only the key and the version differ.
        // The correction itself is pinned where it can actually fire, by
        // `a_recovery_triggered_poll_refresh_death_is_not_counted_as_a_poll_driven_retry`.
        //
        // schema:12 (issue #1453) appends the top-level `operator_landing` object after `landing` —
        // ADDITIVE, and, like 10→11's key addition, with no value-domain correction: it folds
        // `reason=manual` / `reason=forced` anchors, which every block above EXCLUDES by its
        // `reason=session` filter. This document is the direct evidence: read it against the
        // schema:11 expectation in git history and exactly two things differ — the version integer
        // and the inserted block. And the block is NOT inert here, which is the point of the bump:
        // FIXTURE_LOG has always carried a manual swap, `swaps_total` reads 1 for it, and until this
        // block existed that swap was graded by nothing at all.
        let out = render_json(&fixture_report()).expect("integer wire serializes");
        assert_eq!(
            out,
            concat!(
                "{\n",
                "  \"schema\": 12,\n",
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
                "  \"capacity_held\": {\n",
                "    \"n\": 0,\n",
                "    \"p50\": null,\n",
                "    \"p95\": null,\n",
                "    \"p100\": null\n",
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
                "    },\n",
                "    \"capacity_held\": 0\n",
                "  },\n",
                "  \"operator_landing\": {\n",
                "    \"swaps_total\": 1,\n",
                "    \"n_measured\": 0,\n",
                "    \"n_unmeasured\": 1,\n",
                "    \"p50\": null,\n",
                "    \"p90\": null,\n",
                "    \"p100\": null,\n",
                "    \"window_secs\": 900,\n",
                "    \"ceiling\": 99,\n",
                "    \"n_at_or_over_ceiling\": 0,\n",
                "    \"capacity_held\": 0\n",
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
                "    \"n_swapped_away\": 1,\n",
                "    \"n_never_recovered\": 0,\n",
                "    \"p50\": null,\n",
                "    \"p95\": null,\n",
                "    \"p100\": null\n",
                "  },\n",
                "  \"blind_episodes\": {\n",
                "    \"n_entered\": 1,\n",
                "    \"n_exited\": 1,\n",
                "    \"n_swapped_away\": 1,\n",
                "    \"n_never_recovered\": 0,\n",
                "    \"n_anchor_lost\": 0,\n",
                "    \"n_exit_without_enter\": 0,\n",
                "    \"n_malformed\": 0,\n",
                "    \"near_limit\": {\n",
                "      \"n_episodes\": 1,\n",
                "      \"observed_secs\": 999,\n",
                "      \"censored_floor_secs\": 0,\n",
                "      \"total_secs_lower_bound\": 999\n",
                "    }\n",
                "  },\n",
                "  \"rate_limit_neutrality\": {\n",
                "    \"rate_limited\": 2,\n",
                "    \"transient\": 1,\n",
                "    \"cleared\": 1\n",
                "  },\n",
                // schema:10 (issue #881): the refresh-token-loss block, appended LAST so every
                // schema:9 key keeps its position as well as its value. FIXTURE_LOG carries no
                // refresh-family line, so it reads all-zero — and every figure above is byte-for-
                // byte what schema:9 emitted, which is the "unchanged in meaning" acceptance
                // pinned as bytes rather than argued in prose.
                "  \"refresh_token_loss\": {\n",
                "    \"accounts\": 0,\n",
                "    \"observations\": 0,\n",
                "    \"by_mechanism\": {\n",
                "      \"sweep\": 0,\n",
                "      \"poll_retry\": 0,\n",
                "      \"parked_recovery\": 0,\n",
                "      \"keep_warm\": 0\n",
                "    },\n",
                "    \"confirmed_unrecoverable\": 0\n",
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
        // The two censored tails read `None` HERE for a precise reason: `BLIND_PROJECTION_LOG`
        // carries no `blind_enter`/`blind_exit` lines, so `pair_observed` is false. That is the
        // pair-ABSENT branch, not a structural impossibility — since issue #591 these fields DO carry
        // real counts wherever the pair is in view (see the schema:8 stable render, where they read
        // 1 and 0). What stays true either way is the discipline: never a fabricated `0` asserting
        // this recovered-only population is the whole blind story.
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
    fn window_resolve_accepts_each_unit_of_the_shared_grammar() {
        // The span grammar itself moved to `crate::duration` (issue #773) and is unit-tested
        // there. What THIS verb still owns — and what these assertions pin — is that its
        // `--since` resolves each unit to the same cutoff it always did.
        let now = epoch("2026-07-12T00:00:00Z");
        for (raw, secs) in [
            ("45s", 45),
            ("30m", 1_800),
            ("24h", 86_400),
            ("7d", 604_800),
            ("2w", 1_209_600),
            ("0d", 0),
            // Surrounding whitespace is trimmed (lexopt hands the value through verbatim).
            ("  7d  ", 604_800),
        ] {
            let w = Window::resolve(raw, now).expect("valid duration");
            assert_eq!(
                now - w.cutoff_epoch,
                secs,
                "{raw:?} must resolve to now − {secs}s"
            );
        }
        // Saturating multiply, then clamp: a count whose ×unit overflows u64 yields `u64::MAX`
        // seconds, which floors the cutoff at 0 ("the whole log") — never a wrapped, and so
        // future-dated, cutoff that would silently empty the window.
        let w = Window::resolve(&format!("{}w", u64::MAX), now).expect("valid duration");
        assert_eq!(w.cutoff_epoch, 0);
    }

    #[test]
    fn window_resolve_rejects_malformed_as_this_verbs_error() {
        // The shared grammar rejects each of these (see `crate::duration`); what THIS verb owns
        // is that the rejection surfaces as ReliabilitySinceInvalid — naming the flag the
        // operator actually mistyped, not `log`'s.
        let now = epoch("2026-07-12T00:00:00Z");
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
            let err = Window::resolve(bad, now).unwrap_err();
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
            human.contains(
                "swap-out session_pct (reason=session, reaction-latency): no swaps observed"
            ),
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
        assert!(
            out.contains("\"schema\": 12,"),
            "schema bumped to 12: {out}"
        );
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

    #[test]
    fn a_never_recovered_episode_contributes_a_censored_floor_not_a_measured_duration() {
        // u-A enters a near-limit blind window at 00:00 and NEVER exits. The horizon is the last line
        // in view (00:10:00), so the episode contributes a 600 s LOWER BOUND, kept apart from the
        // measured sum — which stays 0, because no exit was ever recorded. This is the treatment
        // issue #591 requires and a plain sum over exits would get wrong: that sum reports ZERO
        // blindness for an account that has been dark for ten minutes and counting.
        let log = "\
ts=2026-07-11T00:00:00Z event=blind_enter acct=u-A session_pct=97 weekly_pct=40 was_active=true near_limit=true
ts=2026-07-11T00:10:00Z event=usage_backoff acct=u-A class=transient consecutive=1 backoff_secs=30
";
        let r = aggregate(&parse_events(log, None), &[], None);
        let ep = &r.blind_episodes;
        assert_eq!(ep.n_entered, 1);
        assert_eq!(ep.n_exited, 0);
        assert_eq!(ep.n_never_recovered, 1);
        assert_eq!(ep.n_anchor_lost, 0);
        assert_eq!(ep.near_limit_observed_secs, 0, "no exit ⇒ nothing MEASURED");
        assert_eq!(
            ep.near_limit_censored_floor_secs, 600,
            "horizon − entry, a lower bound"
        );
        assert_eq!(ep.near_limit_episodes, 1);

        // Both RENDERED surfaces are pinned with a NON-ZERO floor. The internal-field assertions
        // above cannot catch a renderer that drops the censored term — with floor==0 everywhere else
        // in the suite, `observed` and `observed + floor` are indistinguishable, so the feature's own
        // headline number would be unguarded on both surfaces.
        assert!(
            render_human(&r).contains(
                "near-limit blind time: >= 600s (0s measured + 600s right-censored floor, n=1)"
            ),
            "human surface must carry the censored floor"
        );
        let json = render_json(&r).expect("integer wire serializes");
        assert!(json.contains("\"censored_floor_secs\": 600"), "{json}");
        assert!(
            json.contains("\"total_secs_lower_bound\": 600"),
            "the lower bound must INCLUDE the censored floor, not just measured time: {json}"
        );
    }

    #[test]
    fn a_same_second_exit_then_re_entry_is_not_mistaken_for_a_restart() {
        // `ts=` has whole-second resolution, so one account's exit and its next entry CAN land on the
        // same second. Ordering them wrong inverts the verdict: `enter → exit → enter` is one CLOSED
        // episode plus one never-recovered episode, but if the re-entry sorts BEFORE the exit it
        // reads as a phantom anchor-loss — dropping the never-recovered episode and zeroing its
        // censored floor, understating precisely the worst tail. The `(ts, seq)` sort key is what
        // prevents it; a bare `ts` key regresses here because the timeline is built entries-then-
        // exits, which destroys log order before a stable sort can preserve it.
        let log = "\
ts=2026-07-11T00:00:00Z event=blind_enter acct=u-A session_pct=97 weekly_pct=40 was_active=true near_limit=true
ts=2026-07-11T00:05:00Z event=blind_exit acct=u-A duration_secs=300 session_burn_pct=1 weekly_burn_pct=0 session_pct=97 session_at_recovery=98 weekly_pct=40 weekly_at_recovery=40 was_active=true swapped_away=false near_limit=true
ts=2026-07-11T00:05:00Z event=blind_enter acct=u-A session_pct=98 weekly_pct=40 was_active=true near_limit=true
ts=2026-07-11T00:20:00Z event=usage_backoff acct=u-A class=transient consecutive=1 backoff_secs=30
";
        let ep = aggregate(&parse_events(log, None), &[], None).blind_episodes;
        assert_eq!(ep.n_anchor_lost, 0, "the exit closed the first episode");
        assert_eq!(ep.n_never_recovered, 1, "the re-entry is still open");
        assert_eq!(ep.n_exit_without_enter, 0);
        assert_eq!(ep.near_limit_observed_secs, 300, "the closed episode");
        assert_eq!(
            ep.near_limit_censored_floor_secs, 900,
            "00:20 − 00:05 for the still-open episode"
        );
    }

    #[test]
    fn a_re_entry_marks_the_prior_open_episode_anchor_lost_not_never_recovered() {
        // Two entries for u-A with NO exit between. `daemon::note_blind_episode` strictly alternates
        // per account, so this is impossible while the anchor is intact — it PROVES the in-memory
        // anchor was dropped (a daemon restart). Counting the superseded entry as "never recovered"
        // would inflate the worst tail with ordinary restarts, the inflation issue #591 warns about.
        let log = "\
ts=2026-07-11T00:00:00Z event=blind_enter acct=u-A session_pct=97 weekly_pct=40 was_active=true near_limit=true
ts=2026-07-11T00:05:00Z event=blind_enter acct=u-A session_pct=98 weekly_pct=41 was_active=true near_limit=true
ts=2026-07-11T00:06:00Z event=usage_backoff acct=u-A class=transient consecutive=1 backoff_secs=30
";
        let ep = aggregate(&parse_events(log, None), &[], None).blind_episodes;
        assert_eq!(ep.n_entered, 2);
        assert_eq!(
            ep.n_anchor_lost, 1,
            "the superseded entry is a restart orphan"
        );
        assert_eq!(
            ep.n_never_recovered, 1,
            "only the LAST entry is genuinely open"
        );
        // Only the surviving open entry contributes a floor (00:06 − 00:05). The orphan contributes
        // NOTHING: its end is unknown, and inventing one would be the fabrication.
        assert_eq!(ep.near_limit_censored_floor_secs, 60);
        assert_eq!(ep.near_limit_episodes, 1);
    }

    #[test]
    fn absent_pair_leaves_the_censored_tails_null_rather_than_zero() {
        // A log predating issue #583 carries no pair lines at all. Reporting `Some(0)` would assert
        // "zero swapped-away episodes" when the truth is "unobservable" — the fabricated zero this
        // readout refuses everywhere else (the SwapOvershoot cardinality discipline).
        let log = "\
ts=2026-07-11T00:10:00Z event=blind_window acct=u-A duration_secs=300 session_pct=97 session_at_recovery=99 near_limit=true
";
        let r = aggregate(&parse_events(log, None), &[], None);
        assert_eq!(r.blind_projection_error.n_swapped_away, None);
        assert_eq!(r.blind_projection_error.n_never_recovered, None);
        // And the censored SLI is unaffected by the pair's absence — it never read the pair.
        assert_eq!(r.time_blind_near_limit_secs, 300);
        assert!(render_human(&r).contains("no uncensored blind_enter/blind_exit pair in view"));
    }

    #[test]
    fn an_exit_whose_entry_is_out_of_view_is_disclosed_and_still_measured() {
        // A `--since` cutoff or a rotated log routinely severs the entry. The exit is SELF-CONTAINED
        // — the daemon measured `duration_secs` against its own per-account anchor — so its time and
        // `swapped_away` tag still count; the orphaned-exit count is disclosed so the entered/exited
        // totals visibly need not balance.
        let log = "\
ts=2026-07-11T00:02:00Z event=blind_exit acct=u-A duration_secs=120 session_burn_pct=2 weekly_burn_pct=0 session_pct=97 session_at_recovery=99 weekly_pct=40 weekly_at_recovery=40 was_active=true swapped_away=true near_limit=true
";
        let ep = aggregate(&parse_events(log, None), &[], None).blind_episodes;
        assert_eq!(ep.n_exit_without_enter, 1);
        assert_eq!(ep.n_never_recovered, 0);
        assert_eq!(ep.n_swapped_away, 1);
        assert_eq!(ep.near_limit_observed_secs, 120);
        assert_eq!(ep.near_limit_censored_floor_secs, 0);
    }

    #[test]
    fn unplaceable_pair_lines_are_counted_not_silently_dropped() {
        // An undisclosed drop is a missing denominator — the survivorship failure this module guards
        // against. Three corrupt shapes: an entry with no `acct=`, an exit with an unparseable `ts=`,
        // and an exit with no `duration_secs=`.
        let log = "\
ts=2026-07-11T00:00:00Z event=blind_enter session_pct=97 weekly_pct=40 was_active=true near_limit=true
ts=not-a-timestamp event=blind_exit acct=u-A duration_secs=120 session_pct=97 session_at_recovery=99 weekly_pct=40 weekly_at_recovery=40 was_active=true swapped_away=false near_limit=true
ts=2026-07-11T00:05:00Z event=blind_exit acct=u-B session_pct=97 session_at_recovery=99 weekly_pct=40 weekly_at_recovery=40 was_active=true swapped_away=false near_limit=true
";
        let r = aggregate(&parse_events(log, None), &[], None);
        assert_eq!(r.blind_episodes.n_malformed, 3);
        assert_eq!(r.blind_episodes.n_entered, 0);
        assert_eq!(r.blind_episodes.n_exited, 0);
        // Every pair line here is corrupt, so NOTHING was actually observed — the tails stay `None`.
        // "Present but unreadable" is unobservable, not observed-zero: `Some(0)` would tell the
        // operator "excludes 0 swapped-away episodes" on a view that could not read a single one.
        // The corruption is still DISCLOSED, via the `n_malformed` assertion above.
        assert_eq!(r.blind_projection_error.n_swapped_away, None);
        assert_eq!(r.blind_projection_error.n_never_recovered, None);
    }

    #[test]
    fn the_window_bounds_the_episode_census() {
        // `--since` must bound this census like every sibling SLI: an entry outside the window is
        // neither an episode nor a restart orphan.
        //
        // Deliberately does NOT also pin the horizon's placement relative to the window gate: that
        // ordering is unobservable while `--since` is a pure lower bound, so an assertion "proving"
        // it could never fail. See the horizon note in `parse_events` for why.
        let log = "\
ts=2026-07-11T00:00:00Z event=blind_enter acct=u-A session_pct=97 weekly_pct=40 was_active=true near_limit=true
ts=2026-07-12T00:00:00Z event=blind_enter acct=u-B session_pct=96 weekly_pct=30 was_active=true near_limit=true
ts=2026-07-12T00:10:00Z event=usage_backoff acct=u-B class=transient consecutive=1 backoff_secs=30
";
        let ep = aggregate(
            &parse_events(log, Some(epoch("2026-07-12T00:00:00Z"))),
            &[],
            None,
        )
        .blind_episodes;
        // u-A's entry is out of window entirely — it is neither an episode nor a restart orphan.
        assert_eq!(ep.n_entered, 1);
        assert_eq!(ep.n_anchor_lost, 0);
        assert_eq!(ep.n_never_recovered, 1);
        assert_eq!(ep.near_limit_censored_floor_secs, 600);
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
        // The #15/#444 meter fixture (issue #881): a realistic credential blob plus its two
        // `sk-ant-…` tokens and owner email, so `assert_clean` below scans for KNOWN secret values
        // as well as the generic shapes. `authored_labels` is deliberately EMPTY: unlike the `log`
        // verb, this readout renders no operator label at all, so every email shape is a leak here
        // and none may be excused as permitted.
        let secrets = crate::redaction::meter::Secrets::meter_fixture();
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
        // Non-degeneracy for the issue #591 census path: its records hold `acct` UUIDs internally, so
        // the fixture's u-D pair must actually fold, else the identifier guard below passes on an
        // empty census and proves nothing about the newly-reachable path.
        for r in [&whole, &windowed] {
            assert!(
                r.blind_episodes.n_entered > 0 && r.blind_episodes.n_exited > 0,
                "census must fold the fixture pair so its leak guard is a real catch"
            );
        }
        for r in [&whole, &windowed] {
            for out in [render_human(r), render_json(r).expect("serializes")] {
                assert!(
                    crate::redaction::meter::unauthored_emails(out.as_str(), &[]).is_empty(),
                    "no non-authored email may appear (#15): {out}"
                );
                // The full redaction METER, replacing a bare `contains("token")` word check that
                // issue #881 made untenable: this readout's own heading ("refresh-token loss") and
                // wire key (`refresh_token_loss`) contain that word as legitimate vocabulary, so the
                // blanket check now fires on a FIXED in-code label that cannot carry data.
                //
                // The remedy STRENGTHENS rather than narrows. `meter::assert_clean` is the repo's
                // shared #15/#444 guard (`src/redaction.rs`, already used by `log`, `login`,
                // `refresh`, `use_account`): the `sk-ant-` prefix, the fixture's token values
                // verbatim, the credential blob's raw and sha256 fingerprints, email shapes, and —
                // the part no substring list can reach — an ENTROPY backstop for a leak in a format
                // nobody enumerated. It catches what the word check could not, e.g. a blob dump
                // whose token value is already redacted (`BlobLeadingBytes`): `contains("token")`
                // misses that outright, since `accessToken` carries a capital T.
                //
                // Not "strictly stronger" as a set relation, so state the trade plainly: three
                // WORD-PRESENCE proxies go — lowercase `token` (the necessary narrowing), `Bearer`,
                // and `sk-ant` widened to the real `sk-ant-` prefix. No secret hides behind any of
                // them: a real `Bearer <token>` still trips `TokenPrefix` or the entropy backstop,
                // and no Anthropic token has the `sk-antXYZ` shape (`redaction.rs`'s own
                // `TOKEN_PREFIXES = ["sk-ant-"]` is the repo's model of that). Three proxies for the
                // meter's whole detector set, and one less bespoke predicate maintained here.
                //
                // Non-vacuity is proved by MUTATION of THIS readout's own rendered bytes, in
                // `the_redaction_meter_catches_a_token_planted_in_this_readout` — measured, not
                // assumed: a canary asserting a COPY of the predicate against local literals keeps
                // passing after this very line is deleted, so it certifies nothing. Deleting the
                // call is itself a CI failure rather than a silent weakening — it orphans
                // `secrets`, which the repo's `clippy -D warnings` gate rejects as unused.
                crate::redaction::meter::assert_clean(out.as_str(), &secrets, &[]);
                assert!(!out.contains("label="), "no operator label: {out}");
                assert!(!out.contains("acct="), "no account uuid: {out}");
                // DISCRIMINATING, not just prefix-shaped: assert the identifiers THEMSELVES are
                // absent. A bare-value leak (a UUID rendered without its `acct=` key, which is
                // exactly how the issue #591 census could regress — it holds them on its internal
                // records) slips straight past the prefix-only check above.
                for acct in ["u-A", "u-B", "u-C", "u-D"] {
                    assert!(!out.contains(acct), "no account identifier {acct}: {out}");
                }
            }
        }
    }

    /// CONSTRAINT: the redaction meter guarding this readout can still FAIL — proved by MUTATION
    /// of the readout's OWN bytes, not by re-implementing the predicate beside it.
    ///
    /// That distinction is the whole point. Issue #881 replaced a blanket `contains("token")` word
    /// check (which its own "refresh-token loss" heading trips) with `meter::assert_clean`. A
    /// relaxation-or-swap of a leak guard is worth nothing unless the replacement is shown to bite,
    /// and a canary that asserts predicates against LOCAL LITERALS would keep passing even if every
    /// assertion in `readout_carries_no_pii` were deleted — a rubber stamp. So this plants a leak
    /// into the rendered readout and asserts the meter catches it there, the same
    /// mutate-the-real-subject discipline `a_poisoned_diagnostic_channel_never_reaches_the_default_view`
    /// (`src/log.rs`) and the golden canary both use.
    #[test]
    fn the_redaction_meter_catches_a_token_planted_in_this_readout() {
        use crate::redaction::meter;
        let secrets = meter::Secrets::meter_fixture();
        let clean = render_human(&fixture_report());
        // Baseline: the real readout is clean, so the mutation below is what makes the difference.
        meter::assert_clean(&clean, &secrets, &[]);

        // Each mutation is a way a secret has actually reached a string in this daemon: a panic
        // payload carrying a token (the #15 poisoned-channel case), the credential blob's own JSON,
        // and a bearer header. Planted INTO the readout so the meter runs over the same bytes the
        // production assertions run over.
        for leak in [
            "sk-ant-oat-LEAK0abc0def0ghi0jkl0mno0pqr0stu0vwx",
            r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-METER0SECRET0ACCESS0bC9dE2fG7hJ4kL6mN8"}}"#,
            "Authorization: Bearer victim@meter-redaction.example",
        ] {
            let poisoned = format!("{clean}{leak}\n");
            assert!(
                !meter::scan(&poisoned, &secrets, &[])
                    .into_iter()
                    .filter(|f| !matches!(f, meter::Finding::KnownEmail))
                    .collect::<Vec<_>>()
                    .is_empty(),
                "the meter must reject a readout carrying: {leak}"
            );
        }

        // The complement, and the reason the word check had to go: issue #881's fixed vocabulary is
        // NOT a leak. Asserted against the meter itself, so a future re-tightening back to a bare
        // `contains("token")` goes red HERE rather than silently breaking the readout's own heading.
        for vocabulary in ["refresh-token loss", "\"refresh_token_loss\": {"] {
            meter::assert_clean(&format!("{clean}{vocabulary}\n"), &secrets, &[]);
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
                    decision_pct: Some(96),
                    held: false,
                },
                SwapOut {
                    ts: epoch("2026-07-11T00:06:00Z"),
                    acct: "oleksii@pelykh.com".to_owned(),
                    decision_pct: Some(100),
                    held: false,
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

    /// The non-finite `session` readings a parked-window peak must refuse (issue #1210). Each is
    /// exercised in its OWN isolated window below, because their harms differ and one masks another:
    /// `reduce(f64::max)` IGNORES a NaN, so a NaN sitting beside a finite reading never reaches
    /// `peak` at all. Only a NaN that is its window's SOLE reading is observable, and isolating every
    /// shape is what keeps each of them separately falsifiable.
    ///
    /// The harms are not the same size, which is why the shapes are named rather than counted. A
    /// positive infinity clamps to the `u8` cap and fabricates a 255 % landing — the max-value breach
    /// the guard's own comment names, which lands in `post_swap_tail` and poisons the issue #597 tail
    /// calibration. NaN and a negative infinity instead cast to 0, fabricating a PERFECT landing:
    /// quieter, and still a lie, because it also moves the anchor out of `n_unmeasured` into the
    /// measured population the percentiles summarize. The guard refuses all three the same way, so
    /// the assertion below is one shape-named sweep rather than three bespoke expectations.
    const NON_FINITE_PARKED_READINGS: &[(&str, f64)] = &[
        ("a positive infinity", f64::INFINITY),
        ("a NaN", f64::NAN),
        ("a negative infinity", f64::NEG_INFINITY),
    ];

    /// The premise the canary rests on, asserted rather than assumed: a non-finite `session` reading
    /// cannot arrive from `usage-samples.jsonl`, so the guard at the peak filter is defence in depth
    /// against an in-memory `Sample` — NOT a live filter on the store's own output.
    ///
    /// Issue #1179 reasoned exactly this out in prose and concluded the site was not worth canarying,
    /// on the grounds that pinning it would assert the filter's arithmetic while implying a reachable
    /// hazard the decoder forecloses. Issue #1210 overrode the verdict, not the reasoning: an
    /// unpinned guard is one a later reader deletes as dead weight regardless of how its hazard is
    /// reached. This test is what makes the override honest — the reachability claim stops being
    /// prose a reader must take on trust, so the canary can state its own status instead of implying
    /// a stronger one.
    ///
    /// What it pins is exactly `serde_json`'s non-finite policy against `Sample`'s derived impls, and
    /// nothing wider. A change to either — the policy, `session`'s type, its serde attributes — REDs
    /// here. A store that adopted a DIFFERENT serialization format would leave this green while
    /// voiding its premise, because no assertion in it reaches the store's own writer; the canary
    /// below would silently become load-bearing with nothing here saying so.
    #[test]
    fn the_sample_store_cannot_carry_a_non_finite_session_reading() {
        for (shape, session) in NON_FINITE_PARKED_READINGS {
            // The near end of the round trip: `f64::INFINITY` has no JSON spelling, so `serde_json`
            // writes `null` — which is then not an `f64` on the way back in.
            let encoded = serde_json::to_string(&sample(0, "work", *session))
                .expect("a Sample always serializes");
            assert!(
                serde_json::from_str::<Sample>(&encoded).is_err(),
                "{shape} survived a store round trip: {encoded}"
            );
        }
        // The far end, for a file written by something other than that serializer (a torn write, a
        // hand-concatenated log — the same corruption class the #1179 fixtures stand for): JSON has
        // no infinity literal, and a decimal that OVERFLOWS `f64` is rejected rather than saturated
        // into one, so no byte sequence in the store decodes to a non-finite reading.
        // Positive control FIRST, and it is not decoration: every assertion in the loop below is an
        // `is_err()`, so one typo in a field name would make all six "fail to decode" for a reason
        // that has nothing to do with `session` and void the whole loop silently. Measured —
        // misspelling `provider` here leaves the entire suite green without this.
        let control = r#"{"ts":0,"provider":"claude","acct":"work","session":0.42,"weekly":0.1}"#;
        assert_eq!(
            serde_json::from_str::<Sample>(control)
                .expect("the control spelling must decode, or the loop below proves nothing")
                .session,
            0.42,
            "the control line must decode to its own `session`, or an `is_err()` below could be \
             any field's fault"
        );
        for spelling in ["Infinity", "-Infinity", "NaN", "1e400", "-1e400", "null"] {
            let line = format!(
                r#"{{"ts":0,"provider":"claude","acct":"work","session":{spelling},"weekly":0.1}}"#
            );
            assert!(
                serde_json::from_str::<Sample>(&line).is_err(),
                "`session:{spelling}` decoded — the parked-window guard is load-bearing against real \
                 store data, not defence in depth"
            );
        }
    }

    /// The canary (issue #1210): a non-finite reading of the parked account leaves its anchor
    /// UNMEASURED — the same coverage gap
    /// [`landing_swap_without_a_post_swap_sample_is_unmeasured_not_zero`] pins for an empty window —
    /// rather than being fabricated into a landing.
    ///
    /// Deleting `&& s.session.is_finite()` from the peak filter REDs this, and reds it naming the
    /// shape: the message prints every admitted shape beside the reading it fabricated, so the
    /// positive infinity's `Some(255)` with `post_swap_tail: 1` — the max-value breach — is
    /// distinguishable at a glance from the quieter `Some(0)` the other two produce.
    ///
    /// Non-vacuous against an ANCHOR-less fixture, and only that. The expectation includes
    /// `n_unmeasured: 1`, which holds only if the swap anchor actually PARSED — a fixture that
    /// quietly stopped producing one reads `0` there and REDs. It does NOT pin the JOIN, and saying
    /// otherwise would overstate it: under the live guard the reading is dropped either way, so
    /// "no sample joined" and "a sample joined and was filtered out" are indistinguishable at this
    /// assertion. Measured — relabelling the sample so nothing can match `from=work` leaves this
    /// green. The join is pinned by the shared-window canary below, and by that alone.
    #[test]
    fn landing_refuses_a_non_finite_parked_reading_rather_than_fabricating_one() {
        // On target at 96, so nothing here can classify as a gap-crossing (that needs >= 99): any
        // breach this test ever sees is therefore a fabricated post-swap tail.
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=session session_pct=96
";
        let fabricated: Vec<(&str, usize, usize, Option<u8>, usize)> = NON_FINITE_PARKED_READINGS
            .iter()
            .map(|(shape, session)| {
                let samples = [sample(epoch("2026-07-11T00:02:00Z"), "work", *session)];
                let r = aggregate(&parse_events(log, None), &samples, None);
                (
                    *shape,
                    r.landing.n_measured,
                    r.landing.n_unmeasured,
                    r.landing.p100,
                    r.landing.post_swap_tail,
                )
            })
            // Collected rather than asserted inside the loop, so a mutation report names EVERY shape
            // that got through instead of stopping at the first.
            .filter(|o| (o.1, o.2, o.3, o.4) != (0, 1, None, 0))
            .collect();
        assert!(
            fabricated.is_empty(),
            "every shape must leave its anchor unmeasured — (n_measured, n_unmeasured, p100, \
             post_swap_tail) should read (0, 1, None, 0); these fabricated a landing instead: \
             {fabricated:?}"
        );
    }

    /// What that refusal BUYS, pinned on a window that HAS an honest landing: a missing guard would
    /// not merely invent coverage where there was none, it would OVERWRITE a comfortably-on-target
    /// 42 % with the `u8` cap — converting a clean episode into a max-value `post_swap_tail` breach
    /// and handing the issue #597 tail calibration a 255 that never happened.
    ///
    /// The finite reading is load-bearing twice, exactly as the #1179 canary's control line is. It
    /// proves the join is LIVE on this fixture — an unwired one reads `n_measured: 0, p100: None`
    /// here and fails, rather than passing over an empty window — and it is the value the infinity
    /// destroys, so the mutation's failure names `Some(255)` against it directly.
    #[test]
    fn landing_keeps_the_finite_peak_when_a_non_finite_reading_shares_the_window() {
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=session session_pct=96
";
        let samples = [
            sample(epoch("2026-07-11T00:02:00Z"), "work", 0.42),
            sample(epoch("2026-07-11T00:03:00Z"), "work", f64::INFINITY),
        ];
        let r = aggregate(&parse_events(log, None), &samples, None);
        assert_eq!(
            r.landing.n_measured, 1,
            "the finite reading must still land — an empty join would pass the exclusion vacuously"
        );
        assert_eq!(
            r.landing.p100,
            Some(42),
            "the honest peak must survive the poisoned reading beside it"
        );
        assert_eq!(
            r.landing.post_swap_tail, 0,
            "42 is nowhere near the ceiling, so any breach here is fabricated"
        );
        assert_eq!(r.landing.p100_met(), Some(true));
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

    // --- issue #1453: operator-initiated swaps reach the post-swap readout ----

    /// AC-4. An operator's own `sessiometer use` swap is GRADED — it appears in the post-swap
    /// readout instead of being filtered away by `reason=session`.
    ///
    /// This is the whole of what #1453's readout half buys, and the reason it is not cosmetic: the
    /// defining action of a post-swap observation incident is an operator seeing an at-limit active
    /// and rescuing it by hand. Every SLI in this file dropped that swap at the `reason` filter, so
    /// nothing moved when it happened and nothing would move when the observation gap that caused it
    /// was fixed.
    #[test]
    fn an_operator_initiated_swap_is_graded_by_the_post_swap_readout() {
        let at = epoch("2026-07-11T00:00:00Z");
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=manual session_pct=0\n";
        // The parked account keeps burning after the operator moved off it: 0.94 → 0.99 inside the
        // window. Reconstructed from the SAMPLES, exactly as a reason=session landing is — the swap
        // line's `session_pct` is never read.
        let samples = vec![
            sample(at + 60, "work", 0.94),
            sample(at + 120, "work", 0.99),
        ];
        let r = aggregate(&parse_events(log, None), &samples, None);

        assert_eq!(r.operator_landing.swaps_total, 1, "the anchor is kept");
        assert_eq!(r.operator_landing.n_measured, 1);
        assert_eq!(r.operator_landing.n_unmeasured, 0);
        assert_eq!(r.operator_landing.p100, Some(99), "the peak of the window");
        assert_eq!(
            r.operator_landing.n_at_or_over_ceiling, 1,
            "the rescue still ended up at/over the ceiling — the finding this partition exists to \
             make visible"
        );
    }

    /// AC-4's `BUT NOT` half, and the reason the partition is separate rather than a widened filter:
    /// grading the operator swap must not change what a manual swap RECORDS, and must not let its
    /// deliberate `session_pct=0` reach a distribution that means something else.
    ///
    /// `0` on a manual swap is a correct record of "not session-triggered"
    /// (`crate::observability::SwapReason`). Folding these swaps into `swap_overshoot` would drag the
    /// #363 gate's percentiles toward a reading no daemon decision ever produced; folding them into
    /// `landing` would silently redefine an already-published figure. So both stay empty here while
    /// the operator partition is populated — the two directions of the same requirement.
    #[test]
    fn grading_an_operator_swap_moves_no_session_scoped_figure() {
        let at = epoch("2026-07-11T00:00:00Z");
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=manual session_pct=0\n\
ts=2026-07-11T00:10:00Z event=swap from=spare to=backup reason=forced session_pct=0\n";
        let samples = vec![
            sample(at + 60, "work", 0.99),
            sample(at + 660, "spare", 0.40),
        ];
        let r = aggregate(&parse_events(log, None), &samples, None);

        // BOTH operator reasons are anchored — `forced` is a `use --force`, equally operator-driven.
        assert_eq!(r.operator_landing.swaps_total, 2);
        assert_eq!(r.operator_landing.n_measured, 2);
        assert_eq!(r.operator_landing.p100, Some(99));

        // And nothing session-scoped moved: no decision-point sample, no landing anchor, no
        // capacity-held reclassification. A `0` reaching `swap_overshoot` would show as `n=1` here.
        assert_eq!(
            r.swap_overshoot.n, 0,
            "no session_pct=0 entered the #363 gate"
        );
        assert_eq!(r.swap_overshoot.p50, None);
        assert_eq!(r.landing.swaps_total, 0, "no operator anchor entered SLI 5");
        assert_eq!(r.landing.n_measured, 0);
        assert_eq!(r.capacity_held.n, 0);
    }

    /// The operator partition carries NO breach classes and NO SLO verdict, deliberately: with no
    /// decision reading there is nothing to say the swap fired below the ceiling, so a
    /// `post_swap_tail` count would be a class invented out of a field that means "not applicable".
    ///
    /// Pinned on the WIRE, because that is where a consumer would read a verdict that is not there.
    #[test]
    fn the_operator_landing_block_publishes_no_verdict_it_cannot_support() {
        let at = epoch("2026-07-11T00:00:00Z");
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=manual session_pct=0\n";
        let samples = vec![sample(at + 60, "work", 0.99)];
        let r = aggregate(&parse_events(log, None), &samples, None);
        let json: serde_json::Value =
            serde_json::from_str(&render_json(&r).expect("serializes")).expect("valid JSON");
        let block = &json["operator_landing"];

        assert_eq!(block["n_measured"], 1);
        assert_eq!(block["n_at_or_over_ceiling"], 1);
        // The ceiling the count was taken against travels WITH it, so a stored readout is read
        // against its own constant rather than today's.
        assert_eq!(block["ceiling"], u64::from(SLO_SWAP_P100_MAX));
        assert!(
            block.get("classes").is_none(),
            "no breach split: an operator swap has no decision reading to classify against"
        );
        assert!(
            block.get("p100_met").is_none(),
            "no SLO verdict: gating a manual swap would score a number the daemon never chose"
        );
    }

    /// A capacity-held operator swap is excluded on the SAME terms issue #719 excludes a session
    /// one: the outgoing account was pinned at the ceiling because no viable target existed, which
    /// is a fleet-capacity limit whoever pressed the button.
    ///
    /// Without this the partition would read an operator's rescue-of-last-resort as a landing
    /// overshoot — the exact conflation #719 removed from the session gate.
    #[test]
    fn a_capacity_held_operator_swap_is_segregated_not_counted_as_a_landing() {
        let at = epoch("2026-07-11T00:00:00Z");
        let log = "\
ts=2026-07-11T00:00:00Z event=all_exhausted hold=spare cause=session\n\
ts=2026-07-11T00:01:00Z event=swap from=work to=spare reason=manual session_pct=0\n";
        let samples = vec![sample(at + 120, "work", 1.00)];
        let r = aggregate(&parse_events(log, None), &samples, None);

        assert_eq!(r.operator_landing.capacity_held, 1);
        assert_eq!(
            r.operator_landing.swaps_total, 0,
            "held anchors leave the coverage denominator, so measured + unmeasured still totals it"
        );
        assert_eq!(r.operator_landing.n_measured, 0);
        assert_eq!(r.operator_landing.p100, None);
        assert_eq!(
            r.operator_landing.n_at_or_over_ceiling, 0,
            "an at-ceiling capacity hold is not a landing overshoot"
        );
    }

    /// An operator swap the sample store cannot reconstruct is UNMEASURED, never a passing `0`
    /// landing — the cardinality discipline every percentile block here follows.
    #[test]
    fn an_operator_swap_without_post_swap_samples_is_unmeasured_not_zero() {
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=manual session_pct=0\n";
        let r = aggregate(&parse_events(log, None), &[], None);

        assert_eq!(r.operator_landing.swaps_total, 1);
        assert_eq!(r.operator_landing.n_measured, 0);
        assert_eq!(r.operator_landing.n_unmeasured, 1);
        assert_eq!(r.operator_landing.p50, None);
        assert_eq!(r.operator_landing.p90, None);
        assert_eq!(r.operator_landing.p100, None);
        assert_eq!(r.operator_landing.n_at_or_over_ceiling, 0);
    }

    /// The RENDERED operator block, populated — the arm an operator actually reads once the
    /// partition has something in it.
    ///
    /// Every other test here asserts the computed `OperatorLanding`, and all three committed CLI
    /// render goldens exercise the EMPTY arm ("no reason=manual/forced swaps observed"), so the
    /// populated one shipped with no assertion over it at all: transposing its P50 and P90 left the
    /// whole suite and the golden gate green.
    ///
    /// Ten landings, because the percentile is NEAREST-RANK — `ceil(p · n)` — so P90 and P100 are the
    /// same element for every n below 10, and a fixture smaller than this cannot tell those two
    /// lines apart no matter how distinct its values are.
    #[test]
    fn the_populated_operator_landing_block_renders_each_figure_in_its_own_place() {
        let at = epoch("2026-07-11T00:00:00Z");
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=a0 to=b0 reason=manual session_pct=0\n\
ts=2026-07-11T01:00:00Z event=swap from=a1 to=b1 reason=forced session_pct=0\n\
ts=2026-07-11T02:00:00Z event=swap from=a2 to=b2 reason=manual session_pct=0\n\
ts=2026-07-11T03:00:00Z event=swap from=a3 to=b3 reason=forced session_pct=0\n\
ts=2026-07-11T04:00:00Z event=swap from=a4 to=b4 reason=manual session_pct=0\n\
ts=2026-07-11T05:00:00Z event=swap from=a5 to=b5 reason=forced session_pct=0\n\
ts=2026-07-11T06:00:00Z event=swap from=a6 to=b6 reason=manual session_pct=0\n\
ts=2026-07-11T07:00:00Z event=swap from=a7 to=b7 reason=forced session_pct=0\n\
ts=2026-07-11T08:00:00Z event=swap from=a8 to=b8 reason=manual session_pct=0\n\
ts=2026-07-11T09:00:00Z event=swap from=a9 to=b9 reason=forced session_pct=0\n\
ts=2026-07-11T10:00:00Z event=all_exhausted hold=held-to\n\
ts=2026-07-11T10:10:00Z event=swap from=held-from to=held-to reason=manual session_pct=0\n";
        // One post-swap sample per rescue, inside each swap's own window, so all ten are measured.
        let samples = vec![
            sample(at + 60, "a0", 0.80),
            sample(at + 3_660, "a1", 0.82),
            sample(at + 7_260, "a2", 0.84),
            sample(at + 10_860, "a3", 0.86),
            sample(at + 14_460, "a4", 0.88),
            sample(at + 18_060, "a5", 0.90),
            sample(at + 21_660, "a6", 0.92),
            sample(at + 25_260, "a7", 0.94),
            sample(at + 28_860, "a8", 0.96),
            sample(at + 32_460, "a9", 0.99),
        ];

        let out = render_human(&aggregate(&parse_events(log, None), &samples, None));
        let block: String = out
            .lines()
            .skip_while(|l| !l.starts_with("landing-point session_pct, OPERATOR-initiated"))
            .take_while(|l| !l.is_empty())
            .map(|l| format!("{l}\n"))
            .collect();

        assert_eq!(
            block,
            concat!(
                "landing-point session_pct, OPERATOR-initiated swaps (reason=manual/forced, window <= 900s)\n",
                "  measured n=10 of 10 operator swaps (0 with no post-swap sample)\n",
                "  P50  = 88\n",
                "  P90  = 96\n",
                "  P100 = 99  (no SLO: a manual swap records no decision reading)\n",
                "  landed >= 99: 1  (not a breach class — where the fleet ended up when a human intervened)\n",
                "  capacity-held (all_exhausted, excluded — issue #719): 1\n",
            ),
            "full render was:\n{out}"
        );
    }

    /// The same ten landings on the JSON wire, where a script reads them.
    ///
    /// `json_render_is_stable_schema_12` pins the whole document, but its fixture leaves every
    /// operator percentile `null`, `capacity_held` at `0`, and `swaps_total` equal to
    /// `n_unmeasured` — so a projection that transposed P50 with P90, or `swaps_total` with
    /// `n_unmeasured`, passes it. `the_populated_operator_landing_block_renders_each_figure_in_its_own_place`
    /// separates those values but only through `render_human`, which leaves everything between
    /// `Report` and the wire ungraded. Five figures, all distinct, on the surface a consumer parses.
    #[test]
    fn the_operator_landing_wire_carries_each_figure_in_its_own_field() {
        let at = epoch("2026-07-11T00:00:00Z");
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=a0 to=b0 reason=manual session_pct=0\n\
ts=2026-07-11T01:00:00Z event=swap from=a1 to=b1 reason=forced session_pct=0\n\
ts=2026-07-11T02:00:00Z event=swap from=a2 to=b2 reason=manual session_pct=0\n\
ts=2026-07-11T03:00:00Z event=swap from=a3 to=b3 reason=forced session_pct=0\n\
ts=2026-07-11T04:00:00Z event=swap from=a4 to=b4 reason=manual session_pct=0\n\
ts=2026-07-11T05:00:00Z event=swap from=a5 to=b5 reason=forced session_pct=0\n\
ts=2026-07-11T06:00:00Z event=swap from=a6 to=b6 reason=manual session_pct=0\n\
ts=2026-07-11T07:00:00Z event=swap from=a7 to=b7 reason=forced session_pct=0\n\
ts=2026-07-11T08:00:00Z event=swap from=a8 to=b8 reason=manual session_pct=0\n\
ts=2026-07-11T09:00:00Z event=swap from=a9 to=b9 reason=forced session_pct=0\n\
\
ts=2026-07-11T10:00:00Z event=all_exhausted hold=held-to\n\
ts=2026-07-11T10:10:00Z event=swap from=held-from to=held-to reason=manual session_pct=0\n";
        let samples = vec![
            sample(at + 60, "a0", 0.80),
            sample(at + 3_660, "a1", 0.82),
            sample(at + 7_260, "a2", 0.84),
            sample(at + 10_860, "a3", 0.86),
            sample(at + 14_460, "a4", 0.88),
            sample(at + 18_060, "a5", 0.90),
            sample(at + 21_660, "a6", 0.92),
            sample(at + 25_260, "a7", 0.94),
            sample(at + 28_860, "a8", 0.96),
            sample(at + 32_460, "a9", 0.99),
        ];

        let json = render_json(&aggregate(&parse_events(log, None), &samples, None)).unwrap();
        let block = json
            .split("\"operator_landing\": ")
            .nth(1)
            .expect("the wire carries the partition")
            .split("\n  }")
            .next()
            .expect("… as an object");

        for field in [
            "\"swaps_total\": 10",
            "\"n_measured\": 10",
            "\"n_unmeasured\": 0",
            "\"p50\": 88",
            "\"p90\": 96",
            "\"p100\": 99",
            "\"n_at_or_over_ceiling\": 1",
            "\"capacity_held\": 1",
        ] {
            assert!(block.contains(field), "missing {field} in {block}");
        }
    }

    /// A partition that is ENTIRELY capacity-held still says so on the human surface.
    ///
    /// Holds leave the denominator (issue #719), so `swaps_total` reads `0` and the render falls to
    /// the nothing-observed arm — over a log that carried an operator swap. Saying only "none
    /// observed" there would be false, and the excluded count is the whole of what there is to
    /// report. The `--json` surface was never affected: `capacity_held` rides the wire either way.
    #[test]
    fn an_all_held_operator_partition_reports_the_exclusion_rather_than_nothing() {
        let at = epoch("2026-07-11T00:00:00Z");
        let log = "\
ts=2026-07-11T00:00:00Z event=all_exhausted hold=spare cause=session\n\
ts=2026-07-11T00:01:00Z event=swap from=work to=spare reason=manual session_pct=0\n";
        let samples = vec![sample(at + 120, "work", 0.99)];

        let r = aggregate(&parse_events(log, None), &samples, None);
        assert_eq!(
            r.operator_landing.swaps_total, 0,
            "the hold left the denominator"
        );
        assert_eq!(r.operator_landing.capacity_held, 1);

        let out = render_human(&r);
        assert!(
            out.contains(
                "landing-point session_pct, OPERATOR-initiated swaps: no gradeable reason=manual/forced swaps observed\n  capacity-held (all_exhausted, excluded — issue #719): 1\n"
            ),
            "the exclusion is reported, not silently dropped:\n{out}"
        );
    }

    /// The adopt-recovery sentinel is not an anchor.
    ///
    /// `sessiometer use --force` onto a scrubbed canonical emits a real `reason=forced` swap whose
    /// `from=` is `(unknown)` — a redaction-safe placeholder for an outgoing account that could not
    /// be named, not a roster label. It shares no namespace with a usage sample's `acct`, so an
    /// anchor keyed on it can never join one: it would inflate `swaps_total` and sit in
    /// `n_unmeasured` on every run forever, reported as a sample-coverage gap an operator could go
    /// looking for and never find. Every OTHER operator swap in the same log must still be graded,
    /// which is what makes this a filter rather than a reason to drop the family.
    #[test]
    fn the_adopt_recovery_sentinel_is_not_counted_as_an_unmeasurable_operator_swap() {
        let at = epoch("2026-07-11T00:00:00Z");
        let log = "\
ts=2026-07-11T00:00:00Z event=swap from=(unknown) to=spare reason=forced session_pct=0\n\
ts=2026-07-11T01:00:00Z event=swap from=work to=backup reason=manual session_pct=0\n";
        let samples = vec![sample(at + 3_660, "work", 0.99)];

        let r = aggregate(&parse_events(log, None), &samples, None);

        assert_eq!(
            r.operator_landing.swaps_total, 1,
            "the sentinel is not an anchor; the real swap still is"
        );
        assert_eq!(r.operator_landing.n_measured, 1);
        assert_eq!(
            r.operator_landing.n_unmeasured, 0,
            "an unmeasurable-by-construction anchor must not read as a coverage gap"
        );
        assert_eq!(r.operator_landing.p100, Some(99));
    }

    // --- issue #767: FULL-OUTPUT goldens for the `reliability` human render -----------
    //
    // The SLI tests above assert individual numbers and individual lines. None sees the
    // readout as a whole, so a block that silently stopped printing, printed twice, or moved
    // above the block it is supposed to qualify would pass every one of them. This pins the
    // ENTIRE readout, byte for byte.
    //
    // AXES. `render_human` takes only a `Report` — this surface has no terminal-width
    // degradation, no glyph ramp and no colour gate (its `[ok]` / `[OVER]` markers are ASCII
    // precisely so it needs none; `src/reliability.rs` `ok_flag`). So the matrix here is over
    // the axes it DOES have: a populated report, its degenerate empty-log counterpart, and the
    // `--since` windowed variant that adds the cutoff banner. Claiming a width or colour cell
    // for this surface would be a golden of an axis that does not exist.
    //
    // Determinism: pure over the `Report`, which is derived from a FIXED log and a FIXED sample
    // list at fixed epochs. No wall clock reaches the render — `Window` carries an absolute
    // `cutoff_epoch`, so even the windowed banner is fixed bytes.
    mod goldens {
        use super::*;
        use crate::render_golden::{self, Case};

        /// The golden event log — self-contained rather than shared with `FIXTURE_LOG` above,
        /// so these goldens document their own input and an unrelated edit to that fixture
        /// cannot silently re-baseline them.
        ///
        /// Covers one event of each family the readout reports on: `reason=session` swaps (the
        /// #363 reaction-latency gate), a `reason=velocity_preempt` swap (#539's projective
        /// SLI), an `all_exhausted` hold and the swap that relieves it (#719's capacity-held
        /// partition — the discriminator is `hold == to`, so the OUTGOING account is the
        /// capacity casualty), `blind_window` reconciliations both near-limit and not, an
        /// uncensored `blind_enter`/`blind_exit` pair (#591), usage backoffs of both classes
        /// plus a clear, and a `usage_velocity` observation (#608).
        ///
        /// One event per family stopped being one event per BUCKET at issue #1367: `poll_refresh`
        /// now splits on `trigger=`, so both of its origins appear — on the SAME account, so the
        /// added line moves the observation count and deliberately not the account count, which is
        /// the distinction between those two figures made visible in the rendered bytes.
        const GOLDEN_LOG: &str = "\
ts=2026-07-11T00:00:00Z event=swap from=work to=spare reason=session session_pct=94
ts=2026-07-11T00:05:00Z event=swap from=spare to=work reason=weekly session_pct=42
ts=2026-07-11T00:06:00Z event=swap from=work to=spare reason=session session_pct=98 late=true
ts=2026-07-11T00:07:00Z event=swap from=spare to=work reason=velocity_preempt session_pct=88
ts=2026-07-11T00:08:00Z event=all_exhausted hold=third cause=all_accounts_exhausted resets_at=2026-07-11T05:00:00Z
ts=2026-07-11T00:09:00Z event=swap from=work to=third reason=session session_pct=100
ts=2026-07-11T00:10:00Z event=blind_window acct=u-A duration_secs=300 session_pct=97 session_at_recovery=99 near_limit=true
ts=2026-07-11T00:20:00Z event=blind_window acct=u-B duration_secs=600 session_pct=96 session_at_recovery=40 near_limit=true
ts=2026-07-11T00:30:00Z event=blind_window acct=u-C duration_secs=120 session_pct=50 session_at_recovery=51 near_limit=false
ts=2026-07-11T00:31:00Z event=blind_enter acct=u-D session_pct=95 weekly_pct=40 was_active=true near_limit=true
ts=2026-07-11T00:32:00Z event=blind_exit acct=u-D duration_secs=480 session_burn_pct=3 weekly_burn_pct=2 session_pct=95 session_at_recovery=98 weekly_pct=40 weekly_at_recovery=42 was_active=true swapped_away=false near_limit=true
ts=2026-07-11T00:40:00Z event=usage_backoff acct=u-A class=rate_limited consecutive=1 backoff_secs=60
ts=2026-07-11T00:41:00Z event=usage_backoff acct=u-B class=transient consecutive=1 backoff_secs=30
ts=2026-07-11T00:45:00Z event=usage_backoff_cleared acct=u-A
ts=2026-07-11T00:45:00Z event=keep_warm account=work trigger=reactive outcome=dead rotated=true
ts=2026-07-11T00:46:00Z event=poll_refresh account=spare trigger=poll_401 outcome=dead rotated=true
ts=2026-07-11T00:46:30Z event=poll_refresh account=spare trigger=recovery outcome=dead rotated=true
ts=2026-07-11T00:47:00Z event=refresh account=spare outcome=dead expires_before=1970-01-01T00:00:00Z expires_after=1970-01-01T00:00:00Z rotated=false window_secs=0
ts=2026-07-11T00:48:00Z event=refresh account=third outcome=refreshed expires_before=2026-07-11T00:00:00Z expires_after=2026-07-11T08:00:00Z rotated=false window_secs=28800
ts=2026-07-11T00:49:00Z event=credential_unrecoverable account=spare
ts=2026-07-11T00:50:00Z event=usage_velocity acct=u-A session_pct_per_min=0.20 weekly_pct_per_min=0.01 elapsed_secs=300 session_delta_pct=1 weekly_delta_pct=0
";

        /// The usage samples the #595 landing SLI joins against the swap anchors above: for
        /// each `reason=session` swap, the outgoing account's post-swap readings. `work` keeps
        /// climbing after being parked at 94 (a post-swap TAIL breach) and again after 98; the
        /// capacity-held swap at 100 is excluded from the SLO by #719 and counted separately.
        fn golden_samples() -> Vec<Sample> {
            vec![
                Sample::new(epoch("2026-07-11T00:01:00Z"), "claude", "work", 0.97, 0.10),
                Sample::new(epoch("2026-07-11T00:02:00Z"), "claude", "work", 0.99, 0.10),
                Sample::new(epoch("2026-07-11T00:06:30Z"), "claude", "work", 0.98, 0.10),
                Sample::new(epoch("2026-07-11T00:09:30Z"), "claude", "work", 1.00, 0.10),
            ]
        }

        /// The populated report — the whole-log aggregate, no `--since` bound.
        fn golden_report() -> Report {
            aggregate(&parse_events(GOLDEN_LOG, None), &golden_samples(), None)
        }

        /// The `--since` windowed report. The cutoff is an ABSOLUTE epoch (not `now − d`), so
        /// the banner it adds is fixed bytes; it sits between the two blind_window events, so
        /// the window demonstrably BOUNDS the aggregate rather than merely decorating it.
        fn windowed_report() -> Report {
            let window = Window {
                since_arg: "6h".to_owned(),
                cutoff_epoch: epoch("2026-07-11T00:25:00Z"),
            };
            aggregate(
                &parse_events(GOLDEN_LOG, Some(window.cutoff_epoch)),
                &golden_samples(),
                Some(window),
            )
        }

        /// The degenerate case: an empty log. Every percentile is `None` — cardinality-zero is
        /// reported as "no swaps observed", never as a passing `0`.
        fn empty_report() -> Report {
            aggregate(&parse_events("", None), &[], None)
        }

        /// Every goldened `reliability` case, freshly rendered. The single source of truth for
        /// the case list: the comparison, the canary, and the emitter all consume THIS.
        fn cases() -> Vec<Case> {
            vec![
                Case::new("reliability-full", render_human(&golden_report())),
                Case::new("reliability-windowed", render_human(&windowed_report())),
                Case::new("reliability-empty-log", render_human(&empty_report())),
            ]
        }

        /// The committed goldens, named by case. The macro derives each path from the name, so an
        /// entry cannot pair a case with someone else's bytes, and `include_str!` keeps every
        /// file a COMPILE-TIME input — a missing golden is a build error, not a silent skip.
        const GOLDENS: &[(&str, &str)] = render_golden::cli_render_goldens![
            "reliability-full",
            "reliability-windowed",
            "reliability-empty-log",
        ];

        /// One-time emitter for the committed `reliability` render goldens (issue #767).
        /// `#[ignore]` — NOT part of the suite. Run it ONLY alongside a DELIBERATE change to
        /// the `reliability` readout:
        ///   `cargo test -- --ignored emit_cli_render_goldens`
        /// then look at the regenerated files and record why in a `CLI-Goldens-Rebaselined:`
        /// commit trailer (CI requires it — `scripts/check-cli-golden-rebaseline.sh`).
        #[test]
        #[ignore = "one-time cli-render-golden emitter — run ONLY alongside a deliberate render change"]
        fn emit_cli_render_goldens_reliability() {
            render_golden::emit(&cases());
        }

        #[test]
        fn the_committed_reliability_goldens_still_match_the_render() {
            render_golden::assert_matches_goldens("reliability", &cases(), GOLDENS);
        }

        /// CONSTRAINT-A: the gate can FAIL, demonstrated by MUTATION through the SAME
        /// predicate the assertion above uses — not by inspection.
        ///
        /// `strip-ansi` is declared INAPPLICABLE: this readout has no colour gate at all — its
        /// `[ok]` / `[OVER]` markers are ASCII precisely so it needs none ([`ok_flag`]) — so
        /// there is no escape to strip. The declaration is checked both ways, so if a colour
        /// overlay is ever added here this exemption goes red rather than silently exempting
        /// the new bytes from the canary.
        #[test]
        fn the_reliability_golden_gate_rejects_a_corrupted_render() {
            render_golden::assert_canary("reliability", &cases(), &["strip-ansi"]);
        }

        /// The input-side half of the canary: a log whose readings actually changed must not
        /// match the unperturbed golden.
        #[test]
        fn a_perturbed_log_does_not_match_the_reliability_golden() {
            let perturbed = GOLDEN_LOG.replace("session_pct=94", "session_pct=93");
            assert_ne!(
                perturbed, GOLDEN_LOG,
                "the perturbation did not alter the log, so it cannot alter the render"
            );
            render_golden::assert_perturbed_input_is_rejected(
                "reliability",
                "reliability-full",
                &render_human(&golden_report()),
                &render_human(&aggregate(
                    &parse_events(&perturbed, None),
                    &golden_samples(),
                    None,
                )),
            );
        }

        /// Each case must exercise the axis it claims, or its golden is a duplicate that
        /// asserts nothing. Stated as properties, so it survives a re-baseline.
        #[test]
        fn each_reliability_case_exercises_the_axis_it_claims() {
            let all = cases();
            let case = |name: &str| render_golden::rendered(&all, name);
            let (full, windowed, empty) = (
                case("reliability-full"),
                case("reliability-windowed"),
                case("reliability-empty-log"),
            );

            // The WINDOW axis: the banner appears, names the cutoff, and the bound actually
            // reaches the SLIs (an unbounded readout must not equal a bounded one).
            assert!(
                windowed.contains("window: since"),
                "the windowed case carries no cutoff banner, so it is not exercising `--since`"
            );
            assert!(
                !full.contains("window: since"),
                "the unbounded case carries a cutoff banner it should omit — the whole-log \
                 readout is byte-for-byte window-free by design"
            );
            assert_ne!(
                windowed, full,
                "the `--since` window changed nothing, so it is decorating the readout rather \
                 than bounding it"
            );

            // The DEGENERATE axis: cardinality-zero reports "no swaps observed", never a
            // passing `0` — a gate that scores an empty subject is not evidence.
            assert!(
                empty.contains("no swaps observed"),
                "the empty-log case does not report cardinality-zero honestly"
            );
            assert!(
                !empty.contains("[ok]") && !empty.contains("[OVER]"),
                "the empty-log readout asserts a target verdict over ZERO observations — an \
                 unmeasurable period is not a passing one:\n{empty}"
            );
            assert!(
                full.contains("[ok]") || full.contains("[OVER]"),
                "the populated case asserts no target verdict at all, so the flag rendering is \
                 unexercised"
            );
        }

        /// The #719 capacity-held partition must be VISIBLE and SEGREGATED: the all-exhausted
        /// swap is excluded from the #363 reaction-latency gate and reported in its own block.
        /// Pinned as a property because it is the readout's most load-bearing distinction — a
        /// re-baseline that quietly folded the two populations back together would otherwise
        /// look like an ordinary byte change.
        #[test]
        fn the_capacity_held_partition_stays_segregated_from_the_reaction_latency_gate() {
            let report = golden_report();
            assert_eq!(
                report.capacity_held.n, 1,
                "the golden log's `all_exhausted`-relieving swap was not classified as \
                 capacity-held, so this case does not exercise the #719 partition"
            );
            // GOLDEN_LOG holds THREE `reason=session` swaps (94, 98, 100). Exactly one — the
            // 100 that relieved the hold — is capacity-held, so the reaction-latency gate must
            // see the other two and no more. Asserting the count rather than "n > 0" is what
            // catches a leak in EITHER direction.
            assert_eq!(
                report.swap_overshoot.n, 2,
                "the #363 reaction-latency gate counted {} swaps, not the 2 non-held ones — a \
                 capacity-held swap leaked in (or a reaction-latency swap leaked out)",
                report.swap_overshoot.n
            );
            assert_eq!(
                report.swap_overshoot.p100,
                Some(98),
                "the gate's P100 is not the worst NON-HELD swap — the 100 that resolved the \
                 all-exhausted hold is dragging a capacity limit into a latency SLO"
            );
            let rendered = render_human(&report);
            assert!(
                rendered.contains("capacity-held (reason=session, all_exhausted"),
                "the capacity-held block is absent from the readout, so the excluded population \
                 is invisible:\n{rendered}"
            );
        }
    }
}
