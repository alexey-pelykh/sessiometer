// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The `stats` verb — an OFFLINE reader of the usage-sample store (issue #158).
//!
//! `sessiometer stats [ACCOUNT]... [--period day|week|month|lifetime] [--since <when>]
//! [--json]` reports usage over a rolling window. It reads the store's own files
//! DIRECTLY (raw samples + rolled aggregates via `crate::usage_store`, and the swap
//! timeline out of the structured event log via `crate::observability`), so it renders
//! when the daemon is down and makes **no** live control-socket / keychain / usage-API
//! call — the daemon is the sole WRITER, this verb the sole READER. That one-way data
//! flow is enforced structurally by the `HistoryStore` seam: the whole pipeline is a
//! pure function of what that seam returns, so a live call is not reachable from here.
//!
//! # What it produces
//!
//! The heavy lifting is the pure aggregator from issue #157 (`crate::usage_stats`): this
//! module only resolves the window, buckets it, calls `aggregate`, and renders. Two
//! views come out:
//!
//! * a **summary** — one whole-window `aggregate` (per-account mean/peak/p95 for both
//!   quota dimensions, cap-hits, time-at-cap, contribution share; plus roster-wide swap
//!   frequency and all-accounts-high episodes);
//! * a **series** — the same `aggregate` over each sub-bucket of the window (hourly for
//!   `day`, daily otherwise), the time-ordered points a chart plots.
//!
//! The human render is terminal CHARTS on an interactive TTY (issue #159) and the NUMERIC
//! text table (the summary table + a neutral summary band + a roster line + the
//! resolved-window echo in local time) when stdout is not one — a pipe / redirect keeps the
//! plain, greppable numbers. Both views foot with the same summary band (issue #160). `--json`
//! emits the versioned, stable `schema:1` wire contract (full series + summary + neutral
//! descriptor enums; redacted handles only), never charted, never coloured.
//!
//! # Scope seam (issues #159 / #160)
//!
//! The terminal CHARTS (issue #159) live in the `rendering: terminal charts` section below:
//! they render the same `series` / `summary` the base verb computed — nothing is
//! re-aggregated, the store is not re-read — presentation-only, so the `--json` wire is
//! byte-for-byte the #158 contract (no chart glyph reaches it). The neutral SIGNAL summary
//! (issue #160) is the `rendering: neutral summary band` section: it foots BOTH human views
//! with a symmetric, facts-only band derived from the neutral per-account descriptor enums
//! (`band`, `coverage_class`) the wire already carries — no projection, no recommendation,
//! and (like the charts) no new wire field, so `--json` stays byte-for-byte stable.
//! `HistoryStore::read_rollup` also exposes the lifetime daily
//! tier as a seam for deep-history charts (that tier is roster-wide, so it cannot back a
//! per-account series; here it only anchors the `lifetime` window start).
//!
//! # Gap honesty
//!
//! The aggregator never invents a reading, and neither does this verb: a bucket that
//! predates the store's raw retention simply reports low `coverage` rather than a
//! fabricated calm. Everything is whole UTC epoch seconds end to end; only the human
//! window echo is rendered in the operator's local time zone.

use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Serialize;

// The `status` view's terminal-cell width primitive (issue #73), reused so the charts
// (issue #159) size their columns on the SAME wcwidth — one definition for the crate.
use crate::cli::{display_width, pad_end};
use crate::config::{Config, Tunables};
use crate::error::{Error, Result};
use crate::observability;
use crate::paths;
use crate::usage::epoch_from_rfc3339;
use crate::usage_stats::{
    aggregate, parse_swap_events, AccountStats, AggregateParams, Period, RosterStats, UsageReport,
};
use crate::usage_store::{self, Rollup, Sample};

/// The `schema:` version of the `--json` wire contract. Bumped only on a breaking change;
/// #159 / #160 add fields without bumping it.
const JSON_SCHEMA_VERSION: u32 = 1;

/// Seconds in an hour / day — bucket-alignment units, matching the store's own tiers.
const HOUR_SECS: i64 = 3_600;
const DAY_SECS: i64 = 86_400;

/// A hard cap on how many series buckets a window is split into. A window longer than
/// `MAX_BUCKETS × bucket` widens the bucket (coarser resolution) rather than truncating —
/// no data is dropped, a bucket just spans more time. Keeps a multi-year `lifetime` JSON
/// bounded.
const MAX_BUCKETS: i64 = 366;

/// The parsed `stats` argument vector, as collected by the CLI dispatcher. Validation
/// (period enum, `--since` grammar, mutual exclusion) happens downstream in [`run`] so it
/// is unit-testable.
///
/// `Debug`/`PartialEq` let the CLI parser's own tests (issue #175) assert the parsed
/// `stats` invocation by value alongside the rest of the `Command` enum.
#[derive(Debug, PartialEq)]
pub(crate) struct StatsArgs {
    /// Positional account filter — the redacted handles to show (empty = all).
    pub(crate) accounts: Vec<String>,
    /// The raw `--period` value, if given.
    pub(crate) period: Option<String>,
    /// The raw `--since` value, if given.
    pub(crate) since: Option<String>,
    /// Whether `--json` was set.
    pub(crate) json: bool,
    /// Whether `--no-color` was set — forces the chart colour overlay off (issue #159).
    pub(crate) no_color: bool,
    /// Whether `--ascii` was set — forces the ASCII glyph ramp (issue #159).
    pub(crate) ascii: bool,
}

/// The `--period` selector: a rolling look-back window with a natural bucket resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeriodSpec {
    Day,
    Week,
    Month,
    Lifetime,
}

impl PeriodSpec {
    /// Parse a `--period` value, or [`Error::StatsPeriodInvalid`].
    fn parse(s: &str) -> Result<Self> {
        match s {
            "day" => Ok(Self::Day),
            "week" => Ok(Self::Week),
            "month" => Ok(Self::Month),
            "lifetime" => Ok(Self::Lifetime),
            other => Err(Error::StatsPeriodInvalid(other.to_owned())),
        }
    }

    /// The rolling look-back in seconds, or `None` for `lifetime` (whose start is the
    /// earliest datum in the store).
    fn span_secs(self) -> Option<i64> {
        match self {
            Self::Day => Some(DAY_SECS),
            Self::Week => Some(7 * DAY_SECS),
            Self::Month => Some(30 * DAY_SECS),
            Self::Lifetime => None,
        }
    }

    /// The human head of the window echo, e.g. `last 7d`.
    fn label(self) -> &'static str {
        match self {
            Self::Day => "last 24h",
            Self::Week => "last 7d",
            Self::Month => "last 30d",
            Self::Lifetime => "lifetime",
        }
    }

    /// The `period` tag on the JSON wire.
    fn wire_tag(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Lifetime => "lifetime",
        }
    }
}

/// How the window was selected — a preset `--period` or an explicit `--since`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum WindowKind {
    Period(PeriodSpec),
    /// The raw `--since` value, echoed back verbatim for transparency.
    Since(String),
}

/// A resolved reporting window: `[start, end)` in UTC epoch seconds plus how it was chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Window {
    start: i64,
    end: i64,
    kind: WindowKind,
}

impl Window {
    /// The natural bucket width for this window: hourly for `day` (and short `--since`
    /// spans), daily otherwise.
    fn base_bucket(&self) -> i64 {
        match &self.kind {
            WindowKind::Period(PeriodSpec::Day) => HOUR_SECS,
            WindowKind::Period(_) => DAY_SECS,
            WindowKind::Since(_) => {
                if self.end - self.start <= 2 * DAY_SECS {
                    HOUR_SECS
                } else {
                    DAY_SECS
                }
            }
        }
    }
}

/// Everything read out of the store in ONE pass — the sole input to the (otherwise pure)
/// pipeline. Reading here, then computing over this, keeps window resolution and report
/// building hermetically testable without touching disk.
struct StoreData {
    samples: Vec<Sample>,
    rollup: Rollup,
    events: String,
}

impl StoreData {
    /// Read raw samples, the rolled aggregates, and the event-log text from a store.
    fn read(store: &dyn HistoryStore) -> Result<Self> {
        Ok(Self {
            samples: store.read_samples()?,
            rollup: store.read_rollup()?,
            events: store.read_events()?,
        })
    }
}

/// The read seam over the on-disk store. The whole `stats` pipeline consumes only this —
/// which is exactly why it cannot reach a live socket / keychain / usage-API call. The
/// native implementation reads files; tests use an in-memory fake.
pub(crate) trait HistoryStore {
    /// The raw per-poll samples (issue #155). Absent file → empty.
    fn read_samples(&self) -> Result<Vec<Sample>>;
    /// The rolled hourly/daily aggregates (issue #155). Absent file → default.
    fn read_rollup(&self) -> Result<Rollup>;
    /// The structured event-log text (issue #15), for the swap timeline. Absent → empty.
    fn read_events(&self) -> Result<String>;
}

/// The production store: the three native-local files, read directly. Holds the paths so a
/// test can point one at a temp dir and prove the offline read without a daemon.
pub(crate) struct NativeHistoryStore {
    samples_path: PathBuf,
    rollup_path: PathBuf,
    events_path: PathBuf,
}

impl NativeHistoryStore {
    /// The store rooted at the native-local paths (`crate::paths` + the event log).
    fn from_paths() -> Result<Self> {
        Ok(Self {
            samples_path: paths::usage_samples()?,
            rollup_path: paths::usage_rollup()?,
            events_path: observability::log_path()?,
        })
    }
}

impl HistoryStore for NativeHistoryStore {
    fn read_samples(&self) -> Result<Vec<Sample>> {
        usage_store::read_samples(&self.samples_path)
    }
    fn read_rollup(&self) -> Result<Rollup> {
        usage_store::read_rollup(&self.rollup_path)
    }
    fn read_events(&self) -> Result<String> {
        read_log_text(&self.events_path)
    }
}

/// The event-log text, tolerating an absent file (no daemon has ever run) as empty.
fn read_log_text(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(Error::Io(err)),
    }
}

/// The computed report: the resolved window, the applied filter, and the (filtered)
/// summary + series aggregates, plus the local UTC offset used for the echo.
struct Report {
    window: Window,
    accounts: Vec<String>,
    summary: UsageReport,
    series: Vec<UsageReport>,
    offset: i64,
    /// Handles present in the store's window but NOT in the live roster (issue #314):
    /// removed/renamed accounts, or stray samples. Split OUT of `summary.per_account` (and
    /// every `series` bucket) so they never render as peers of live accounts — they surface
    /// only in the dedicated "not in roster" section of each view. Empty when the roster is
    /// unknown (no config loaded) — see [`split_orphans`] — so a pre-`capture` `stats` reads
    /// exactly as before. Summary-window stats only (the series need not re-carry them).
    orphans: BTreeMap<String, AccountStats>,
    /// The per-account velocity + runway readout (issue #543), keyed by the SAME handle as
    /// `summary.per_account` — one entry per summary account, overlaid by [`with_velocity`]
    /// AFTER [`build_report`] (so the base report stays a pure aggregate). Empty until that
    /// overlay runs (a bare `build_report` result — every hermetic aggregate test — carries no
    /// velocity), so the readout is presentation-additive: a report built without it renders
    /// and serializes exactly as it did pre-#543. Summary-window only, like `orphans`.
    velocity: BTreeMap<String, AccountVelocity>,
}

/// One account's velocity + runway readout (issue #543) — the recent per-account usage RATE
/// and the approximate head-room to its swap trigger, computed stats-side by replaying #539's
/// session-velocity EMA over the stored sample series (same α, same reset guard, same
/// [`MIN_VELOCITY_SAMPLES`] gate) so the shown rate matches the daemon's own projection rather
/// than a second, divergent one. Every field is `Option` — an unknown / zero / stale velocity
/// yields `None` (honest degradation), NEVER a fabricated or infinite number.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct AccountVelocity {
    /// Smoothed session-usage rate in usage-FRACTION per second (the EMA's native unit; the
    /// human/wire views scale it to `%/min`). `None` when the account has < 2 usable sample
    /// intervals, a window reset cleared the EMA, or the last reading is stale. Non-negative.
    session_rate: Option<f64>,
    /// Smoothed weekly-usage rate (fraction/second) — the SAME smoothing recipe applied to the
    /// weekly dimension (#539 retains only the session EMA; the weekly runway `#541` wants
    /// reuses the identical definition, not a divergent one). `None` on the same cases.
    weekly_rate: Option<f64>,
    /// Approximate whole seconds until the session reading reaches `session_trigger` at the
    /// smoothed rate: `(trigger − current) / rate`. `None` when the rate is unknown or `0`, or
    /// the reading is already at/over the trigger (no positive head-room to state as a fact).
    session_runway_secs: Option<i64>,
    /// Approximate whole seconds until the weekly reading reaches `weekly_trigger`. `None` on
    /// the same cases (commonly `None` — the weekly window moves slowly, so a flat weekly
    /// dimension has no measurable rate).
    weekly_runway_secs: Option<i64>,
    /// The account's remaining WEEKLY head-room as a usage fraction — `max(0, weekly_trigger −
    /// latest weekly reading)` — the pool contribution the fleet aggregate (issue #544) sums. `Some`
    /// EXACTLY when [`Self::weekly_rate`] is `Some` (a KNOWN weekly velocity), so head-room is
    /// recorded only for an account whose burn is also known: a KNOWN-zero (flat, measured) account
    /// keeps a `Some` head-room (real spare capacity, `0` burn), while an unknown / stale account is
    /// `None` and excluded from the aggregate — [`fleet_runway`] owns why. Distinct from
    /// [`Self::weekly_runway_secs`], which is `None` for a flat account even though its head-room is
    /// positive — this field is the raw head-room the pool needs, not the per-account time-to-trigger.
    weekly_headroom: Option<f64>,
}

/// Entry point for the `stats` verb: read the store once, resolve the window, aggregate,
/// and render. The only impure step is reading the store + wall clock; everything else is
/// a pure function of `StoreData` + `now`.
pub(crate) async fn run(args: StatsArgs) -> Result<()> {
    let store = NativeHistoryStore::from_paths()?;
    let data = StoreData::read(&store)?;
    let now = wall_clock_now();
    let offset = local_offset_secs(now);

    let window = plan_window(args.period.as_deref(), args.since.as_deref(), now, &data)?;
    // One config load feeds BOTH the aggregator thresholds AND the roster reconciliation
    // (issue #314). A missing/malformed config is not fatal for a read-only view — the
    // thresholds fall back to built-in defaults, and an unknown roster simply disables the
    // orphan partition (every handle renders as before), so `stats` still works pre-`capture`.
    let config = Config::load().ok();
    let params = params_from(config.as_ref());
    let vparams = velocity_params_from(config.as_ref());
    let roster = config.as_ref().map(roster_handles);
    let report = with_velocity(
        build_report(
            &data,
            window,
            args.accounts,
            roster.as_ref(),
            &params,
            offset,
        ),
        &data.samples,
        &params,
        &vparams,
    );

    let out = if args.json {
        render_json(&report)?
    } else {
        render_human(&report, TermEnv::detect(args.no_color, args.ascii))
    };
    print!("{out}");
    Ok(())
}

/// The daemon `stats` socket verb (issue #356): read the store, compute the bounded per-account
/// daily series, and return the reply line the spawned socket task writes verbatim — the compact
/// `StatsWire` JSON for `period`, or a non-secret `{"error":"…"}` envelope on an invalid period or
/// an unreadable store.
///
/// Reads the SAME on-disk store `sessiometer stats` reads and runs it through the SAME
/// [`build_report`] + [`stats_wire`] pipeline, so a socket read equals `sessiometer stats --period
/// <period> --json` for the same instant (R-2 parity) — only the serialization differs (compact
/// here, for the newline-delimited socket frame; the CLI pretty-prints for a file). `period` is the
/// CLI `--period` grammar (`day|week|month|lifetime`); the panel Stats tab's 7-day daily series is
/// `week` (the CLI has NO `--period 7d` — `7d` is `--since` grammar — so the issue's `"7d"` example
/// maps to `week`, the 7-day daily-bucket window). A missing `period` defaults to `week`, mirroring
/// the CLI default.
///
/// Pure of daemon state — only the store files + wall clock + on-disk config — so the daemon answers
/// it in a blocking task OFF the run loop (the store read is blocking `std::fs`; ADR-0001 forbids
/// stalling the single runtime thread). Non-secret by construction: usage fractions + already-
/// redacted roster labels (issue #15), never a credential — so, like `status` / `watch`, the verb is
/// un-auth-gated.
pub(crate) fn socket_stats_reply(period: Option<&str>) -> String {
    let now = wall_clock_now();
    let offset = local_offset_secs(now);
    match NativeHistoryStore::from_paths().and_then(|store| StoreData::read(&store)) {
        Ok(data) => stats_socket_json(&data, now, offset, period),
        // An unreadable / missing store is a non-secret failure, not a panic — the panel shows
        // "stats unavailable" rather than a broken view (the same tolerance the CLI reader has).
        Err(_) => r#"{"error":"stats unavailable"}"#.to_owned(),
    }
}

/// Build the compact `stats` socket reply from an already-read store — the testable core of
/// [`socket_stats_reply`], split out so a controlled `StoreData` can assert R-2 parity with the CLI
/// `--json` render without touching the real on-disk store. Same [`build_report`] + [`stats_wire`]
/// pipeline [`render_json`] uses, serialized COMPACT (no trailing newline — the socket framing
/// appends it). Returns a redacted `{"error":…}` envelope on an invalid period or a non-finite usage
/// value; the caller maps an unreadable store to the same shape.
fn stats_socket_json(data: &StoreData, now: i64, offset: i64, period: Option<&str>) -> String {
    // `since` is always `None` over the socket — only the CLI `--since` grammar drives that path — so
    // the sole `plan_window` failure here is an unknown `--period` value (`StatsPeriodInvalid`).
    let window = match plan_window(period, None, now, data) {
        Ok(window) => window,
        Err(_) => return r#"{"error":"invalid period"}"#.to_owned(),
    };
    // One config load feeds both the aggregator thresholds and roster reconciliation, exactly as
    // `run` does; a missing / malformed config falls back to defaults (the read-only view still
    // works pre-`capture`). Re-read from disk — the same config the CLI reader sees — rather than
    // the daemon's in-memory copy (which the spawned, `Send`-only task cannot borrow), so the socket
    // series stays byte-parity with `stats --json`.
    let config = Config::load().ok();
    let params = params_from(config.as_ref());
    let vparams = velocity_params_from(config.as_ref());
    let roster = config.as_ref().map(roster_handles);
    // No account filter over the socket — the whole roster (matches `stats --period <p> --json` with
    // no `--account`). Overlay the velocity + runway readout from the SAME params the CLI uses, so a
    // socket read stays byte-parity with `stats --period <p> --json` (issue #543 keeps R-2).
    let report = with_velocity(
        build_report(data, window, Vec::new(), roster.as_ref(), &params, offset),
        &data.samples,
        &params,
        &vparams,
    );
    serde_json::to_string(&stats_wire(&report))
        .unwrap_or_else(|_| r#"{"error":"stats unavailable"}"#.to_owned())
}

/// The resolved terminal environment for the human render — the ONE impure probe of
/// stdout (width + colour gate + glyph ramp), computed in [`run`] and then passed as
/// plain data so the whole chart pipeline is a pure function of it (issue #159). Mirrors
/// the `status` view's render discipline: width drives column degradation, `color` the
/// ANSI overlay, `ascii` the glyph ramp. Reuses `crate::cli`'s single width probe and
/// single colour gate rather than re-deriving either.
#[derive(Clone, Copy, Debug)]
struct TermEnv {
    /// Terminal columns, or `None` when stdout is NOT a TTY (piped / redirected) — the
    /// signal that drops the charts for the plain numeric table.
    cols: Option<usize>,
    /// Whether the ANSI colour overlay may be emitted (the shared `status` colour gate).
    color: bool,
    /// Whether to render the ASCII glyph ramp instead of the Unicode blocks (`--ascii`,
    /// or `TERM=dumb`).
    ascii: bool,
}

impl TermEnv {
    /// Probe stdout ONCE: width via [`crate::cli::terminal_cols`], colour via the shared
    /// [`crate::cli::should_colorize`] gate, and the ASCII ramp when forced (`--ascii`) or
    /// the terminal cannot render the block glyphs (`TERM=dumb`).
    fn detect(no_color: bool, ascii: bool) -> Self {
        Self {
            cols: crate::cli::terminal_cols(),
            color: crate::cli::should_colorize(no_color),
            ascii: ascii || term_is_dumb(),
        }
    }
}

/// Whether `TERM=dumb` — a terminal that cannot render SGR OR the Unicode block ramp, so
/// the charts fall back to the ASCII ramp (issue #159). The colour half is already folded
/// into [`crate::cli::should_colorize`]; this is only the ramp half.
fn term_is_dumb() -> bool {
    std::env::var("TERM").as_deref() == Ok("dumb")
}

/// Render the HUMAN-facing view: the terminal CHARTS (issue #159) on an interactive TTY,
/// or the #158 numeric table when stdout is NOT one (piped / redirected → `cols` is
/// `None`), so `stats | grep` and `stats > file` stay the plain, greppable numeric
/// surface with zero ANSI. Pure over `env`, so the whole view is golden-testable at a
/// fixed width / colour / ramp.
fn render_human(report: &Report, env: TermEnv) -> String {
    match env.cols {
        None => render_text(report),
        Some(w) => render_charts(report, w, env.color, env.ascii),
    }
}

/// Resolve the reporting window from the raw `--period` / `--since` values.
///
/// `--period` and `--since` are mutually exclusive; neither given defaults to `week`.
/// Pure over `now` + `data` (the latter only for the `lifetime` start), so the whole
/// selection path is unit-testable.
fn plan_window(
    period: Option<&str>,
    since: Option<&str>,
    now: i64,
    data: &StoreData,
) -> Result<Window> {
    match (period, since) {
        (Some(_), Some(_)) => Err(Error::StatsPeriodSinceConflict),
        (None, Some(s)) => {
            let start = parse_since(s, now)?;
            Ok(Window {
                start,
                end: now,
                kind: WindowKind::Since(s.to_owned()),
            })
        }
        (Some(p), None) => Ok(period_window(PeriodSpec::parse(p)?, now, data)),
        (None, None) => Ok(period_window(PeriodSpec::Week, now, data)),
    }
}

/// The `[start, now)` window for a preset period; `lifetime` anchors at the earliest datum.
fn period_window(spec: PeriodSpec, now: i64, data: &StoreData) -> Window {
    let start = match spec.span_secs() {
        Some(span) => now - span,
        None => lifetime_start(data, now),
    };
    Window {
        start,
        end: now,
        kind: WindowKind::Period(spec),
    }
}

/// The earliest datum in the store — the oldest raw sample or rolled bucket — or `now`
/// when the store is empty. Consults the rolled tiers too, since raw samples are bounded
/// (~14 d) while the daily tier is kept for the store's lifetime.
fn lifetime_start(data: &StoreData, now: i64) -> i64 {
    data.samples
        .iter()
        .map(|s| s.ts)
        .chain(data.rollup.daily.iter().map(|d| d.day_start))
        .chain(data.rollup.hourly.iter().map(|h| h.hour_start))
        .min()
        .unwrap_or(now)
}

/// Parse a `--since` value into an absolute start epoch.
///
/// Accepts a relative offset — an integer followed by `s`/`m`/`h`/`d`/`w` (seconds,
/// minutes, hours, days, weeks), e.g. `7d`, `24h`, `30m` — or an absolute `YYYY-MM-DD`
/// (UTC midnight) or full RFC 3339 instant. Anything else is [`Error::StatsSinceInvalid`].
fn parse_since(raw: &str, now: i64) -> Result<i64> {
    let s = raw.trim();

    // Relative offset: <non-negative int><unit>.
    if let Some(unit) = s.chars().last() {
        if matches!(unit, 's' | 'm' | 'h' | 'd' | 'w') {
            if let Ok(n) = s[..s.len() - unit.len_utf8()].parse::<i64>() {
                if n >= 0 {
                    let secs = match unit {
                        's' => n,
                        'm' => n * 60,
                        'h' => n * HOUR_SECS,
                        'd' => n * DAY_SECS,
                        'w' => n * 7 * DAY_SECS,
                        _ => unreachable!("guarded by the matches! above"),
                    };
                    return Ok(now - secs);
                }
            }
        }
    }

    // Absolute date-only → UTC midnight (the crate's parser wants a full instant).
    if is_ymd(s) {
        if let Some(epoch) = epoch_from_rfc3339(&format!("{s}T00:00:00Z")) {
            return Ok(epoch);
        }
    }
    // Absolute full RFC 3339 instant.
    if let Some(epoch) = epoch_from_rfc3339(s) {
        return Ok(epoch);
    }

    Err(Error::StatsSinceInvalid(s.to_owned()))
}

/// Whether `s` looks like a bare `YYYY-MM-DD` (shape only; the parser validates ranges).
fn is_ymd(s: &str) -> bool {
    let b = s.as_bytes();
    s.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, &c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// The aggregator thresholds from config (or its defaults when config is absent).
///
/// Config triggers are integer PERCENTS; the aggregator wants FRACTIONS — the `stats`
/// caller converts them here, once, so `crate::usage_stats` never reasons about the
/// mismatch. Session cap and the all-accounts-high water are both the session swap
/// threshold (a neutral, config-derived "hot" line).
fn params_from(config: Option<&Config>) -> AggregateParams {
    let (poll_secs, cap) = match config {
        Some(c) => (c.tunables.poll_secs as i64, c.swap_threshold()),
        None => {
            let t = Tunables::default();
            (t.poll_secs as i64, f64::from(t.session_trigger) / 100.0)
        }
    };
    AggregateParams::new(poll_secs.max(1), cap, cap)
}

/// The live-roster handle set for the orphan partition (issue #314): every account's
/// `label`, which is EXACTLY what the daemon freezes into each `Sample.acct`
/// ([`crate::daemon`] writes the label verbatim), so set membership is a plain string
/// compare against the [`aggregate`] output's `per_account` keys. DISABLED accounts are
/// KEPT — a disabled account is still in the roster (its samples are legitimate); only
/// removed / renamed / stray handles fall outside this set and become orphans.
fn roster_handles(config: &Config) -> BTreeSet<String> {
    config.roster.iter().map(|a| a.label.clone()).collect()
}

/// The #539 SUSTAINED-motion gate, mirrored stats-side (issue #543): a velocity is usable only
/// once its EMA has blended at least this many intervals, so a single-interval spike is never
/// reported as a trend. Kept in lockstep with `crate::daemon`'s own `MIN_VELOCITY_SAMPLES` (both
/// `2`) — deliberately duplicated rather than shared, to keep this readout a stats-local change
/// (no daemon edit); the provenance is cited here so the two cannot silently drift.
const MIN_VELOCITY_SAMPLES: u32 = 2;

/// The velocity + runway knobs (issue #543), derived from config ONCE — mirroring
/// [`params_from`] so the two read the same [`crate::config`]. All in the sample's own units
/// (usage fractions), so [`account_velocity`] never reasons about the percent/fraction mismatch.
/// A missing / malformed config falls back to built-in [`Tunables`] defaults, so the read-only
/// view still works pre-`capture` (the same tolerance the aggregator params have).
#[derive(Clone, Copy, Debug, PartialEq)]
struct VelocityParams {
    /// The #539 session-velocity EMA smoothing weight α (`session_velocity_ema_alpha_pct / 100`)
    /// — REUSED verbatim so the stats-shown rate matches the daemon's own projection, never a
    /// second, divergent rate. `1.0` = no smoothing (the raw last-interval rate).
    session_ema_alpha: f64,
    /// The session swap trigger as a fraction — the neutral head-room reference for the session
    /// runway (`(trigger − current) / rate`): the point the daemon acts, stated as a fact.
    session_trigger: f64,
    /// The weekly swap trigger as a fraction — the weekly-runway reference.
    weekly_trigger: f64,
}

fn velocity_params_from(config: Option<&Config>) -> VelocityParams {
    let (alpha_pct, session, weekly) = match config {
        Some(c) => (
            c.tunables.session_velocity_ema_alpha_pct,
            c.tunables.session_trigger,
            c.tunables.weekly_trigger,
        ),
        None => {
            let t = Tunables::default();
            (
                t.session_velocity_ema_alpha_pct,
                t.session_trigger,
                t.weekly_trigger,
            )
        }
    };
    VelocityParams {
        session_ema_alpha: f64::from(alpha_pct) / 100.0,
        session_trigger: f64::from(session) / 100.0,
        weekly_trigger: f64::from(weekly) / 100.0,
    }
}

/// Replay #539's velocity EMA over one dimension's `(ts, fraction)` samples in ASCENDING ts
/// order — the SAME smoothing the daemon applies live (`Daemon::note_session_velocity`): the
/// per-interval instant rate `(next − prev) / elapsed` in fraction-per-second, blended `α·instant
/// + (1 − α)·prev`, SEEDED with the raw rate on the first interval (not zero — a zero seed biases
/// the EMA below the true rate), and CLEARED whenever an interval has non-positive elapsed OR the
/// reading DROPS (a window reset), so a post-reset climb re-seeds from the drop. Returns the
/// smoothed rate ONLY once ≥ [`MIN_VELOCITY_SAMPLES`] intervals have blended since the last reset
/// (#539's sustained gate); fewer → `None`. Non-negative by construction (a drop resets).
fn replay_velocity_ema(series: &[(i64, f64)], alpha: f64) -> Option<f64> {
    let mut ema: Option<(f64, u32)> = None; // (rate, intervals blended since the last reset)
    for pair in series.windows(2) {
        let (prev_ts, prev_v) = pair[0];
        let (next_ts, next_v) = pair[1];
        let elapsed = next_ts - prev_ts;
        if elapsed <= 0 || next_v < prev_v {
            ema = None; // reset — mirrors `note_session_velocity` clearing the slot
            continue;
        }
        let instant = (next_v - prev_v) / elapsed as f64;
        ema = Some(match ema {
            Some((prev_rate, n)) => (
                alpha * instant + (1.0 - alpha) * prev_rate,
                n.saturating_add(1),
            ),
            None => (instant, 1),
        });
    }
    match ema {
        Some((rate, blended)) if blended >= MIN_VELOCITY_SAMPLES => Some(rate),
        _ => None,
    }
}

/// Approximate whole seconds until `current` reaches `trigger` at `rate` (fraction/second):
/// `(trigger − current) / rate`. `None` — NEVER a sentinel — when the rate is unknown or
/// non-positive (an idle / flat dimension has no finite runway) or the reading is already at/over
/// the trigger (no positive head-room left to state as a neutral fact).
fn runway_secs(rate: Option<f64>, current: f64, trigger: f64) -> Option<i64> {
    let rate = rate?;
    if rate <= 0.0 || current >= trigger {
        return None;
    }
    Some(((trigger - current) / rate).round() as i64)
}

/// The per-account velocity + runway readout (issue #543) for `handle`, computed over its
/// in-window samples. Both dimensions' rates come from [`replay_velocity_ema`] (the #539 recipe);
/// each runway is [`runway_secs`] from the account's LATEST in-window reading to its trigger.
/// Returns the all-`None` default (honest degradation) when the account has no in-window reading,
/// or its latest reading is STALE — older than the aggregator's forward-coverage horizon before
/// the window end (the daemon stopped polling / an idle or blind account) — so a no-longer-current
/// reading never backs a velocity or a fabricated runway.
///
/// This is a faithful re-application of #539's rate DEFINITION over the STORED series, NOT a
/// reconstruction of the daemon's transient in-memory EMA (an offline reader cannot see that). The
/// two agree on a steadily-polled account; they can differ slightly across a polling GAP (a
/// throttle / failure writes no sample), where the live daemon FREEZES its EMA and skips the gap
/// interval while this replay blends one long spanning interval — a bounded, CONSERVATIVE
/// approximation (a large elapsed damps the instant rate, so it under- rather than over-states
/// velocity) that still resets on a drop and never yields a wrong-sign / infinite / sentinel value.
fn account_velocity(
    samples: &[Sample],
    handle: &str,
    window: &Window,
    params: &AggregateParams,
    vparams: &VelocityParams,
) -> AccountVelocity {
    // This account's in-window readings, ascending by ts. The store appends chronologically, but
    // sort defensively — the aggregator does too, and the EMA replay depends on the order.
    let mut rows: Vec<&Sample> = samples
        .iter()
        .filter(|s| s.acct == handle && s.ts >= window.start && s.ts < window.end)
        .collect();
    rows.sort_by_key(|s| s.ts);
    let Some(last) = rows.last() else {
        return AccountVelocity::default(); // no reading — everything unknown
    };
    // STALE: the latest reading no longer covers the window end (gap honesty — a reading is valid
    // only over `[ts, ts + stale_after)`), so there is no CURRENT velocity to state.
    if window.end - last.ts > params.stale_after_secs {
        return AccountVelocity::default();
    }
    let session_series: Vec<(i64, f64)> = rows.iter().map(|s| (s.ts, s.session)).collect();
    let weekly_series: Vec<(i64, f64)> = rows.iter().map(|s| (s.ts, s.weekly)).collect();
    let session_rate = replay_velocity_ema(&session_series, vparams.session_ema_alpha);
    let weekly_rate = replay_velocity_ema(&weekly_series, vparams.session_ema_alpha);
    AccountVelocity {
        session_rate,
        weekly_rate,
        session_runway_secs: runway_secs(session_rate, last.session, vparams.session_trigger),
        weekly_runway_secs: runway_secs(weekly_rate, last.weekly, vparams.weekly_trigger),
        // The pool contribution for the fleet aggregate (issue #544): raw weekly head-room from the
        // SAME latest reading and trigger the weekly runway uses, recorded ONLY when the weekly
        // velocity is known (so an unknown / stale account contributes neither head-room nor burn).
        // Clamped at `0` — an over-trigger account is exhausted (no spare capacity), never negative.
        weekly_headroom: weekly_rate.map(|_| (vparams.weekly_trigger - last.weekly).max(0.0)),
    }
}

/// Overlay the per-account velocity + runway readout (issue #543) onto a built [`Report`],
/// computing one [`AccountVelocity`] per SUMMARY account from `samples`. Applied AFTER
/// [`build_report`] (which leaves `report.velocity` empty) by BOTH the CLI reader ([`run`]) and
/// the daemon `stats` socket verb ([`stats_socket_json`]), from the SAME `params` / `vparams`, so
/// the two stay byte-parity (R-2). Series buckets and orphans carry no velocity — this is a
/// CURRENT-rate readout, not a per-bucket or non-roster metric.
fn with_velocity(
    mut report: Report,
    samples: &[Sample],
    params: &AggregateParams,
    vparams: &VelocityParams,
) -> Report {
    report.velocity = report
        .summary
        .per_account
        .keys()
        .map(|handle| {
            (
                handle.clone(),
                account_velocity(samples, handle, &report.window, params, vparams),
            )
        })
        .collect();
    report
}

/// The fleet/roster weekly runway aggregate (issue #544) — the single approximate figure that
/// answers the operator's fleet-level question, "across all my accounts, how long do I last?"
#[derive(Clone, Copy, Debug, PartialEq)]
struct FleetRunway {
    /// Approximate whole seconds until the roster's COMBINED weekly head-room is exhausted at its
    /// combined weekly burn — `Σ head-room ÷ Σ rate` over the counted accounts. `None` when no
    /// counted account has a measurable burn (`Σ rate == 0`): honest degradation, never an infinite
    /// or sentinel figure. Days-scale in practice — the weekly window is the days horizon (the
    /// session dimension resets every few hours, so it is not the "how long do I last" figure).
    runway_secs: Option<i64>,
    /// Accounts that CONTRIBUTED to the aggregate — those with a KNOWN weekly velocity (the
    /// honest-degradation gate). The `n` of the surfaced `n of m`.
    counted: usize,
    /// Accounts OBSERVED in the window (`seen > 0`) — the `m` of `n of m`. `observed − counted` were
    /// EXCLUDED for an unknown / stale weekly velocity: surfaced as a fact, never silently dropped.
    observed: usize,
}

/// Aggregate the per-account weekly velocity + head-room (the issue #543 overlay) into ONE fleet
/// runway (issue #544), a pure function of the already-built [`Report`] — it reuses the #157 window
/// machinery and the #543 per-account readout VERBATIM, adding NO parallel aggregation path and no
/// second rate/sample walk (`report.velocity` is the single source).
///
/// AGGREGATION METHOD (the design choice the issue delegates, settled here): the roster is a shared
/// POOL of weekly head-room drained at the combined observed rate —
///
/// ```text
///   fleet runway ≈  Σ_counted max(0, weekly_trigger − weekly_now)  ÷  Σ_counted weekly_rate
/// ```
///
/// — summed over the accounts with a known weekly velocity. This is what honestly answers "how long
/// until I am FORCED TO STOP":
///
/// - NOT the soonest single-account exhaustion (`min` runway): the daemon SWAPS to the next account
///   at a trigger, it does not stop — so the first exhaustion is a swap, not the end. `min` answers
///   the wrong question and drastically understates the fleet.
/// - NOT an average of per-account runways: that gives equal weight to an idle spare account's long
///   runway and an active account's short one, and for identical accounts collapses to a single
///   account's runway — it is not a pool.
/// - The pool form is honest because only ONE account burns at a time (a single active credential):
///   an idle peer reads a flat, ~0 weekly rate, so `Σ rate` is dominated by whichever account is
///   actually climbing, and `Σ head-room / Σ rate` is the pool's true remaining time. When several
///   accounts genuinely burned across the window it stays a faithful, CONSERVATIVE reading of the
///   observed combined rate — it never OVER-states runway.
///
/// HONEST DEGRADATION (the load-bearing AC): an account with an unknown / stale weekly velocity is
/// EXCLUDED ENTIRELY — neither its head-room (numerator) nor its burn (denominator) enters. Treating
/// such an account as zero-burn would add head-room WITHOUT burn and inflate the runway; excluding it
/// avoids that and is surfaced as `observed − counted` in the `n of m` cardinality. A KNOWN-zero
/// (flat, measured) account DOES count — it is real spare capacity contributing head-room at `0`
/// burn, which correctly EXTENDS the runway rather than fabricating it.
///
/// Returns `None` (no fleet figure at all) when the per-account overlay never ran (a bare
/// [`build_report`], so the wire stays byte-identical to pre-#544), when no account was observed, or
/// when no account could be counted. A `Some` with `runway_secs == None` is the counted-but-not-
/// burning case (every counted account is flat): the cardinality is still surfaced, the runway is an
/// explicit unknown.
fn fleet_runway(report: &Report) -> Option<FleetRunway> {
    if report.velocity.is_empty() {
        return None; // the per-account overlay never ran — a bare aggregate carries no fleet
    }
    let mut observed = 0usize;
    let mut counted = 0usize;
    let mut total_headroom = 0.0_f64;
    let mut total_rate = 0.0_f64;
    for (handle, a) in &report.summary.per_account {
        if a.seen == 0 {
            continue; // gap honesty: an unmeasured account is not a fleet member (matches the band)
        }
        observed += 1;
        let Some(v) = report.velocity.get(handle) else {
            continue;
        };
        // Count an account ONLY with a KNOWN weekly velocity (both fields are `Some` together): the
        // honest-degradation gate. An unknown / stale account is skipped ENTIRELY — never zero-burned.
        let (Some(headroom), Some(rate)) = (v.weekly_headroom, v.weekly_rate) else {
            continue;
        };
        total_headroom += headroom;
        total_rate += rate;
        counted += 1;
    }
    if counted == 0 {
        return None; // nothing to aggregate — no fleet figure to state
    }
    // `None` when the combined burn is zero (every counted account is flat) — no measurable drain, so
    // no finite runway to state (honest degradation, never an infinite / sentinel figure).
    let runway_secs = (total_rate > 0.0).then(|| (total_headroom / total_rate).round() as i64);
    Some(FleetRunway {
        runway_secs,
        counted,
        observed,
    })
}

/// Aggregate the window's samples into a filtered summary + series.
///
/// The summary is one whole-window `aggregate`; the series is one `aggregate` per bucket.
/// Roster-wide statistics (swap frequency, all-high) are computed over the FULL roster;
/// the account filter then restricts only which per-account rows are displayed, so a
/// filtered view never distorts the roster picture.
fn build_report(
    data: &StoreData,
    window: Window,
    accounts: Vec<String>,
    roster: Option<&BTreeSet<String>>,
    params: &AggregateParams,
    offset: i64,
) -> Report {
    let swaps = parse_swap_events(&data.events);

    let mut summary = aggregate(
        &data.samples,
        &swaps,
        Period::new(window.start, window.end),
        params,
    );
    apply_filter(&mut summary.per_account, &accounts);
    // Split non-roster handles out of the SUMMARY view — they render in their own section
    // (issue #314), never as peers of live accounts. The summary partition is the one every
    // view surfaces; the series buckets are cleaned below only so they never PLOT an orphan.
    let orphans = split_orphans(&mut summary.per_account, roster);

    let series = bucket_bounds(window.start, window.end, window.base_bucket())
        .into_iter()
        .map(|(lo, hi)| {
            let mut bucket = aggregate(&data.samples, &swaps, Period::new(lo, hi), params);
            apply_filter(&mut bucket.per_account, &accounts);
            // Drop orphans from each series bucket too, so the charts' per-account series
            // (and the JSON `series`) only ever plot live-roster accounts.
            split_orphans(&mut bucket.per_account, roster);
            bucket
        })
        .collect();

    Report {
        window,
        accounts,
        summary,
        series,
        offset,
        orphans,
        // Empty here — the velocity + runway readout (issue #543) is an overlay applied AFTER
        // this pure aggregate, by [`with_velocity`], so a bare `build_report` (every hermetic
        // aggregate test) renders/serializes exactly as it did pre-#543.
        velocity: BTreeMap::new(),
    }
}

/// Restrict a per-account map to the requested handles (no-op when the filter is empty).
fn apply_filter(per_account: &mut BTreeMap<String, AccountStats>, accounts: &[String]) {
    if accounts.is_empty() {
        return;
    }
    per_account.retain(|handle, _| accounts.iter().any(|a| a == handle));
}

/// Split the non-roster ("orphan") handles OUT of `per_account`, returning them (issue #314).
///
/// A handle is an orphan when it is absent from the live `roster` — a removed/renamed
/// account, or a stray sample. Retaining roster handles in place and extracting the rest
/// mirrors [`apply_filter`]'s removal shape, so the three render surfaces keep iterating a
/// live-accounts-only `per_account` UNCHANGED; orphans surface only through the returned map
/// (and thence each view's dedicated "not in roster" section). Roster-wide statistics
/// (`swap_count`, all-high) are computed by [`aggregate`] over the full sample set and are
/// independent of this display subset, exactly as they already are under [`apply_filter`].
///
/// When `roster` is `None` (no config / roster known) NOTHING is split — every handle stays
/// and the caller gets an empty orphan map, so a pre-`capture` `stats` (or one whose config
/// failed to load) reads exactly as it did before roster-awareness. An EMPTY roster (config
/// present, zero accounts) is distinct from `None`: every present handle is then a genuine
/// orphan.
fn split_orphans(
    per_account: &mut BTreeMap<String, AccountStats>,
    roster: Option<&BTreeSet<String>>,
) -> BTreeMap<String, AccountStats> {
    let Some(roster) = roster else {
        return BTreeMap::new();
    };
    let mut orphans = BTreeMap::new();
    per_account.retain(|handle, stats| {
        if roster.contains(handle) {
            true
        } else {
            orphans.insert(handle.clone(), *stats);
            false
        }
    });
    orphans
}

/// Split `[start, end)` into uniform sub-buckets of at most `MAX_BUCKETS` at `base` width,
/// widening the bucket if the window is very long (no data dropped — a bucket just spans
/// more time). An empty/inverted window yields no buckets.
fn bucket_bounds(start: i64, end: i64, base: i64) -> Vec<(i64, i64)> {
    if end <= start {
        return Vec::new();
    }
    // Widen the bucket so the window never splits into more than `MAX_BUCKETS` (a longer
    // window gets coarser buckets; no data is dropped). All operands are positive, so the
    // ceil-division is done on `u64` (signed `div_ceil` is still unstable).
    let span = (end - start) as u64;
    let base = base.max(1) as u64;
    let bucket = if span.div_ceil(base) > MAX_BUCKETS as u64 {
        span.div_ceil(MAX_BUCKETS as u64) as i64
    } else {
        base as i64
    };
    let mut out = Vec::new();
    let mut lo = start;
    while lo < end {
        let hi = (lo + bucket).min(end);
        out.push((lo, hi));
        lo = hi;
    }
    out
}

// --- rendering: numeric text ------------------------------------------------

/// The per-account table header, sized to the handle column. Shared by the live-account
/// table and the "not in roster" section (issue #314) so both foot identical columns.
fn text_table_header(handle_w: usize) -> String {
    format!(
        "{}  cov   session m/p/p95   weekly m/p/p95    caps  t@cap   share\n",
        pad_end("account", handle_w),
    )
}

/// One per-account table row, sized to the handle column. Shared by the live-account table
/// and the orphan section, so an orphan row is column-identical to a live one — the ONLY
/// difference is which section it sits under.
fn text_account_row(handle: &str, a: &AccountStats, handle_w: usize) -> String {
    format!(
        "{}  {:>3}%  {:<15}  {:<15}  {:>4}  {:>5}  {:>4}%\n",
        pad_end(handle, handle_w),
        pct(a.coverage),
        triple(&a.session),
        triple(&a.weekly),
        a.cap_hits,
        fmt_dur(a.time_at_cap_secs),
        pct(a.contribution_share),
    )
}

/// Render the numeric text view: the window echo, the per-account summary table, an optional
/// "not in roster" section (issue #314), the neutral summary band (issue #160), and the
/// roster line. This is the NON-TTY surface (issue #159): a piped / redirected `stats`
/// renders exactly this — plain, greppable, zero ANSI, no chart glyph — while an interactive
/// TTY gets [`render_charts`]. Reports only magnitudes and neutral descriptors — no
/// recommendation, no forecast (issue #160).
fn render_text(report: &Report) -> String {
    let mut out = String::new();
    let label = format_window_label(&report.window, report.offset);
    out.push_str(&format!("usage — {label}\n\n"));

    let summary = &report.summary;
    let has_live = !summary.per_account.is_empty();
    let has_orphans = !report.orphans.is_empty();
    if !has_live && !has_orphans {
        out.push_str("  no per-account usage in this window\n");
    } else {
        // Size the label column on DISPLAY width, not `String::len()` bytes (issue #249):
        // a wide glyph spans fewer bytes than its terminal footprint, so byte sizing AND
        // char-count `{:<hw$}` padding both mis-aligned the numeric columns. Those numeric
        // fields stay literal `{:>N}` / `{:<15}` fills — they are ASCII-only. Sized across
        // BOTH the live table and the orphan section so the two align under one column width.
        let handle_w = summary
            .per_account
            .keys()
            .chain(report.orphans.keys())
            .map(|handle| display_width(handle))
            .max()
            .unwrap_or(0)
            .max(display_width("account"));
        if has_live {
            out.push_str(&text_table_header(handle_w));
            for (handle, a) in &summary.per_account {
                out.push_str(&text_account_row(handle, a, handle_w));
            }
        }
        // Non-roster handles (issue #314): a clearly-labelled, self-contained section so an
        // orphan is never read as a live account. Shown, not hidden — this is reconciliation,
        // not deletion (a store `gc` that DROPS them is issue #314 option (c), out of scope).
        if has_orphans {
            if has_live {
                out.push('\n');
            }
            out.push_str(&format!("not in roster ({}):\n", report.orphans.len()));
            out.push_str(&text_table_header(handle_w));
            for (handle, a) in &report.orphans {
                out.push_str(&text_account_row(handle, a, handle_w));
            }
        }
    }

    out.push('\n');
    // The neutral summary band (issue #160), then the roster line. The numeric text is the
    // NON-TTY surface, so the band renders WITHOUT colour (zero ANSI on a pipe).
    let band = render_summary(report, false);
    if !band.is_empty() {
        out.push_str(&band);
        out.push('\n');
    }
    out.push_str(&roster_line(&summary.roster));
    out
}

/// The roster summary line (issue #158): swap frequency broken out by reason and the
/// all-accounts-high episodes. Extracted so the numeric [`render_text`] and the charts
/// [`render_charts`] (issue #159) foot the view with the IDENTICAL roster sentence.
fn roster_line(r: &RosterStats) -> String {
    format!(
        "roster: {} swap{} ({} session, {} weekly, {} manual, {} forced, {} emergency) · \
         all-accounts-high: {} episode{} ({})\n",
        r.swap_count,
        plural(r.swap_count),
        r.swaps.session,
        r.swaps.weekly,
        r.swaps.manual,
        r.swaps.forced,
        r.swaps.emergency,
        r.all_high_episodes,
        plural(r.all_high_episodes),
        fmt_dur(r.all_high_secs),
    )
}

/// A dimension as `mean/peak/p95` in whole percents, e.g. `42/88/85`.
fn triple(d: &crate::usage_stats::DimStats) -> String {
    format!("{}/{}/{}", pct(d.mean), pct(d.peak), pct(d.p95))
}

/// A `[0.0, …]` fraction as a rounded whole percent (may exceed 100 — readings can exceed
/// the cap, and that is reported honestly, not clamped).
fn pct(fraction: f64) -> i64 {
    (fraction * 100.0).round() as i64
}

/// `""`/`"s"` pluraliser for the roster line.
fn plural(n: u32) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// A coarse human duration: `0s`, `45s`, `12m`, `2h`, `2h30m`.
fn fmt_dur(secs: i64) -> String {
    if secs <= 0 {
        return "0s".to_owned();
    }
    let (h, m, s) = (secs / HOUR_SECS, (secs % HOUR_SECS) / 60, secs % 60);
    if h > 0 {
        if m > 0 {
            format!("{h}h{m}m")
        } else {
            format!("{h}h")
        }
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

// --- rendering: neutral summary band (issue #160) ---------------------------
//
// A concise, NEUTRAL summary of the period, footing the human views (the numeric text
// table and the charts) just above the roster line. It reports MAGNITUDES and neutral
// descriptor words only — no projection, no recommendation, no value judgement (the
// `summary_render_carries_no_banned_token` guard test enforces that vocabulary against a
// central banned list). Underuse and saturation are SYMMETRIC: both are equal-weight
// deviations from the balanced middle, drawn with the SAME emphasis — underuse is not
// "green for good", saturation is not "red for alarm". Colour merely augments; the
// descriptor WORD carries the full signal, so a no-colour reader loses nothing. The final
// wording is PROVISIONAL pending a brand/framing review (issue #160) — centralised in
// [`SignalBand::label`] for a one-line swap — and it never reaches the `--json` wire,
// which keeps the finer #159 `band` / `coverage_class` enums byte-for-byte unchanged.

/// A neutral, SYMMETRIC utilisation signal collapsed from the wire's [`Band`]: the two
/// deviations from the balanced middle carry identical weight — neither is "good" nor
/// "bad", neither is an alarm. Human-render only; the `--json` wire keeps the finer
/// [`Band`], so this is the summary band's presentation of the SAME underlying magnitude
/// (the two can never disagree on a reading — see [`SignalBand::of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SignalBand {
    /// Below the balanced middle — collapses [`Band::Idle`] / [`Band::Low`].
    Underused,
    /// The balanced middle — [`Band::Moderate`].
    Balanced,
    /// Above the balanced middle — collapses [`Band::High`] / [`Band::AtCap`].
    Saturated,
}

impl SignalBand {
    /// Collapse a session-peak fraction into the symmetric signal THROUGH the wire
    /// [`Band`], so the summary band and the JSON `band` field are always consistent on the
    /// same reading (one threshold definition, two presentations).
    fn of(session_peak: f64) -> Self {
        match Band::of(session_peak) {
            Band::Idle | Band::Low => Self::Underused,
            Band::Moderate => Self::Balanced,
            Band::High | Band::AtCap => Self::Saturated,
        }
    }

    /// The PROVISIONAL descriptor word (final copy pending a brand/framing review, issue
    /// #160). Neutral magnitude vocabulary only — no imperative, forecast, or value
    /// judgement. Centralised here so a copy change is a one-line swap.
    fn label(self) -> &'static str {
        match self {
            Self::Underused => "underused",
            Self::Balanced => "balanced",
            Self::Saturated => "saturated",
        }
    }

    /// The SYMMETRIC emphasis SGR: BOTH deviations share ONE "notable" colour (identical
    /// visual weight — underuse and saturation are equal-weight departures from balanced),
    /// while the balanced middle is un-emphasised. An empty string means no colour wrap.
    /// Emitted only when the shared colour gate is open (issue #15: carries no secret).
    fn sgr(self) -> &'static str {
        match self {
            Self::Underused | Self::Saturated => "33",
            Self::Balanced => "",
        }
    }
}

/// The neutral summary band for the human views (issue #160): a per-account symmetric
/// signal line, then the lowest-utilisation callout. Returns an EMPTY string when there is
/// nothing to summarise (an empty roster), so a caller can append it unconditionally. Pure
/// over `color` (the shared gate), so the band is golden-testable with and without ANSI.
/// Facts only — magnitudes and neutral descriptors, never a recommendation or forecast.
fn render_summary(report: &Report, color: bool) -> String {
    // OBSERVED accounts only — gap honesty. An account can be in the summary with `seen ==
    // 0` (it held the active credential but the daemon polled a different one), its readings
    // zeroed rather than measured; banding that as "underused" or ranking its fabricated 0%
    // as the lowest would invent a low reading the aggregator deliberately never does. The
    // band summarises what was MEASURED, so an unmeasured account is simply not in it.
    let observed: Vec<(&String, &AccountStats)> = report
        .summary
        .per_account
        .iter()
        .filter(|(_, a)| a.seen > 0)
        .collect();
    if observed.is_empty() {
        return String::new();
    }

    // Per-account signal, symmetric emphasis. The band is keyed on the session PEAK — the
    // same basis as the wire's #159 `band` — so the two views classify a reading alike.
    let signals: Vec<String> = observed
        .iter()
        .map(|(handle, a)| {
            let band = SignalBand::of(a.session.peak);
            let word = band.label();
            match (color, band.sgr()) {
                (true, sgr) if !sgr.is_empty() => format!("{handle} \x1b[{sgr}m{word}\x1b[0m"),
                _ => format!("{handle} {word}"),
            }
        })
        .collect();

    // Lowest-utilisation account: the smallest session MEAN among the observed — a
    // magnitude, not a verdict. The handle breaks ties, so the pick is deterministic.
    let lowest = observed
        .iter()
        .min_by(|a, b| {
            a.1.session
                .mean
                .total_cmp(&b.1.session.mean)
                .then_with(|| a.0.cmp(b.0))
        })
        .map(|(handle, a)| {
            format!(
                "lowest utilisation: {handle} (session mean {}%)",
                pct(a.session.mean)
            )
        });

    let mut out = format!("signal  {}\n", signals.join(" · "));
    if let Some(lowest) = lowest {
        out.push_str(&format!("        {lowest}\n"));
    }

    // Velocity + runway readout (issue #543), footing the band beneath the signal: a NEUTRAL
    // per-account rate and the APPROXIMATE head-room to the swap trigger — facts only (a `%/min`
    // rate, `~Xh to trigger`), never advice, so the render passes the amended framing guard
    // (#542). Each line renders ONLY when at least one observed account HAS the datum; within it,
    // an unknown / zero / stale value is `—` (honest degradation), never a fabricated number. A
    // report without the velocity overlay (a bare aggregate) carries an empty map, so BOTH lines
    // are absent and the band foots exactly as it did pre-#543.
    let vel = |handle: &str| report.velocity.get(handle);
    let has_velocity = observed
        .iter()
        .any(|(h, _)| vel(h).and_then(|v| v.session_rate).is_some());
    if has_velocity {
        // The velocity line (session `%/min` per account) and — PAIRED beneath it — the runway
        // line (session `~Xh to trigger`, plus weekly `~Yd` where meaningful). An account whose
        // rate / head-room is unknown renders `—` on both, so the degradation is EXPLICIT rather
        // than a silently missing figure.
        let rates: Vec<String> = observed
            .iter()
            .map(|(h, _)| match vel(h).and_then(|v| v.session_rate) {
                Some(rate) => format!("{h} {}", fmt_pct_per_min(rate)),
                None => format!("{h} —"),
            })
            .collect();
        out.push_str(&format!("velocity  {}\n", rates.join(" · ")));
        let runways: Vec<String> = observed
            .iter()
            .map(|(h, _)| format!("{h} {}", fmt_runway(vel(h).copied().unwrap_or_default())))
            .collect();
        out.push_str(&format!("runway  {}\n", runways.join(" · ")));
    }

    // The FLEET/roster runway aggregate (issue #544), footing the band: ONE approximate figure for
    // "across all my accounts, how long do I last?" — the roster's combined weekly head-room drained
    // at its combined weekly burn, days-scale (`fmt_runway_days`). NEUTRAL and APPROXIMATE (a `~`
    // figure, framed "at the current combined rate"), so it clears the amended #542 guard. Rendered
    // ONLY when the pool has a finite runway; the counted-account cardinality `(n of m counted)` is
    // ALWAYS shown alongside it, so an excluded (unknown / stale) account is surfaced as a fact, not
    // silently folded in as zero-burn.
    if let Some(FleetRunway {
        runway_secs: Some(secs),
        counted,
        observed,
    }) = fleet_runway(report)
    {
        out.push_str(&format!(
            "fleet  accounts last {} at the current combined rate ({counted} of {observed} counted)\n",
            fmt_runway_days(secs),
        ));
    }
    out
}

/// A usage rate in the sample's native fraction-per-SECOND, scaled to percent-per-minute
/// (`× 60 × 100`) — the neutral unit BOTH the human (`fmt_pct_per_min`) and wire
/// (`round_pct_per_min`) views present, so the two scale the EMA's native rate identically
/// (the `pct` sibling for the plain fraction → percent conversion).
fn pct_per_min(rate_frac_per_sec: f64) -> f64 {
    rate_frac_per_sec * 60.0 * 100.0
}

/// A smoothed usage rate (usage-fraction per SECOND — the EMA's native unit) as a neutral
/// `%/min` string, to one decimal. `0.0%/min` for an idle (flat) account is an honest
/// reading, not a gap.
fn fmt_pct_per_min(rate_frac_per_sec: f64) -> String {
    format!("{:.1}%/min", pct_per_min(rate_frac_per_sec))
}

/// This account's runway entry for the human band: the session head-room `~Xh to trigger`, and
/// the weekly head-room `weekly ~Yd` where meaningful (issue #543). `—` when neither is known
/// (unknown / zero / stale velocity, or already at/over the trigger) — never a fabricated or
/// infinite number. The `~` marks every figure APPROXIMATE; the trigger is the neutral reference
/// (the point the daemon acts), stated as a fact, not advice — so the entry clears the #542 guard.
fn fmt_runway(v: AccountVelocity) -> String {
    match (v.session_runway_secs, v.weekly_runway_secs) {
        (Some(s), Some(w)) => format!(
            "{} to trigger, weekly {}",
            fmt_runway_hours(s),
            fmt_runway_days(w)
        ),
        (Some(s), None) => format!("{} to trigger", fmt_runway_hours(s)),
        (None, Some(w)) => format!("weekly {}", fmt_runway_days(w)),
        (None, None) => "—".to_owned(),
    }
}

/// An APPROXIMATE hours-scale runway, e.g. `~4h`, `~45m`, `~30s` — rounded to the coarsest
/// non-zero unit so a glance reads the scale, not false precision. The session window is hours-
/// scale, so this is the session runway's natural unit.
fn fmt_runway_hours(secs: i64) -> String {
    if secs >= HOUR_SECS {
        format!("~{}h", (secs as f64 / HOUR_SECS as f64).round() as i64)
    } else if secs >= 60 {
        // Round to minutes — but a rounded-up 60 m IS an hour, so promote it to the coarser
        // `~1h` rather than emit a boundary `~60m` (the coarsest non-zero unit, not false
        // precision at the top of the minutes range).
        match (secs as f64 / 60.0).round() as i64 {
            60 => "~1h".to_owned(),
            mins => format!("~{mins}m"),
        }
    } else {
        format!("~{}s", secs.max(0))
    }
}

/// An APPROXIMATE days-scale runway, e.g. `~5 days`, `~1 day`, falling back to `~Xh` under a day
/// — days is the weekly window's natural scale, so this is the weekly runway's unit.
fn fmt_runway_days(secs: i64) -> String {
    if secs >= DAY_SECS {
        let d = (secs as f64 / DAY_SECS as f64).round() as i64;
        format!("~{} {}", d, if d == 1 { "day" } else { "days" })
    } else {
        fmt_runway_hours(secs)
    }
}

// --- rendering: local-time window echo --------------------------------------

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The resolved-window echo, e.g. `last 7d (Jun 24–Jul 1)` — head from the selector,
/// date range rendered in the operator's local time zone (`offset` seconds east of UTC).
fn format_window_label(window: &Window, offset: i64) -> String {
    let head = match &window.kind {
        WindowKind::Period(p) => p.label().to_owned(),
        WindowKind::Since(raw) => format!("since {raw}"),
    };
    format!(
        "{head} ({}–{})",
        civil_date(window.start, offset),
        civil_date(window.end, offset)
    )
}

/// `Mon Day` for an epoch in a zone `offset` seconds east of UTC, e.g. `Jun 24`.
fn civil_date(epoch: i64, offset: i64) -> String {
    let (_, m, d) = civil_from_epoch(epoch + offset);
    format!("{} {}", MONTHS[(m - 1) as usize], d)
}

/// `(year, month, day)` for a UTC epoch-second — Howard Hinnant's `civil_from_days`, the
/// dependency-free date math the crate already uses (mirrors `crate::observability`).
fn civil_from_epoch(secs: i64) -> (i64, u32, u32) {
    let days = secs.div_euclid(DAY_SECS);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The local UTC offset (seconds east) for `now`, via `localtime_r`. Falls back to UTC
/// (`0`) on the impossible null return. This is the sole system-time-zone dependency; the
/// rest of the module is pure UTC epoch math, so passing an explicit `offset` keeps the
/// formatter deterministically testable.
fn local_offset_secs(now: i64) -> i64 {
    // SAFETY: `localtime_r` writes the broken-down time into our caller-owned, zeroed
    // `tm`; we pass a valid `time_t` pointer. A null return (cannot happen for a valid
    // `time_t`) is handled as UTC.
    unsafe {
        let t = now as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            0
        } else {
            tm.tm_gmtoff as i64
        }
    }
}

/// Current wall clock as epoch seconds (`0` on the pre-1970 impossible case) — mirrors the
/// crate's other display-path clock reads.
fn wall_clock_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// --- rendering: JSON wire (schema:1) ----------------------------------------

/// The stable `--json` document. Field names are OWNED by this wire contract (decoupled
/// from the aggregator's internal types), so an internal refactor cannot silently break
/// the schema. #159 / #160 extend it additively; they do not bump `schema`.
#[derive(Serialize)]
struct StatsWire<'a> {
    schema: u32,
    window: WindowWire<'a>,
    /// The applied account filter (redacted handles); empty means "all".
    accounts: &'a [String],
    series: Vec<BucketWire>,
    summary: PeriodWire,
    /// Non-roster handles present in the window (issue #314): removed / renamed accounts or
    /// stray samples, keyed exactly like `summary.accounts` but held apart so a consumer
    /// never reads an orphan as a live account. OMITTED entirely when there are none (or when
    /// no roster is known — a pre-`capture` read), so the key appears only when orphans exist.
    /// Additive to `schema:1` (matches the `#159`/`#160` extend-without-bumping precedent);
    /// summary-window only — the `series` buckets never carry orphans.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    orphans: BTreeMap<String, AccountWire>,
}

#[derive(Serialize)]
struct WindowWire<'a> {
    start: i64,
    end: i64,
    /// The human echo, in the operator's local time zone.
    label: String,
    /// The preset period tag, when a `--period` (or the default) selected the window.
    #[serde(skip_serializing_if = "Option::is_none")]
    period: Option<&'a str>,
    /// The raw `--since` value, when that selected the window.
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<&'a str>,
}

/// One series bucket: its `[start, end)` plus the same shape as the summary.
#[derive(Serialize)]
struct BucketWire {
    start: i64,
    end: i64,
    roster: RosterWire,
    accounts: BTreeMap<String, AccountWire>,
}

/// The per-account + roster body for the summary. Mirrors the shape of a series [`BucketWire`] (a
/// distinct type) but additionally carries the summary-only [`FleetWire`] roster aggregate.
#[derive(Serialize)]
struct PeriodWire {
    roster: RosterWire,
    accounts: BTreeMap<String, AccountWire>,
    /// The fleet/roster weekly runway aggregate (issue #544): PRESENT when ≥ 1 summary account has a
    /// KNOWN weekly velocity (so the pool can be aggregated), OMITTED otherwise (no countable account,
    /// or a bare aggregate that never ran the velocity overlay — the wire then stays byte-identical to
    /// pre-#544). Summary-only — a series [`BucketWire`] never carries it, exactly as it carries no
    /// per-account velocity. Additive to `schema:1` (the `#159`/`#160`/`#543` extend-without-bumping
    /// precedent); does NOT bump `schema`.
    #[serde(skip_serializing_if = "Option::is_none")]
    fleet: Option<FleetWire>,
}

/// The fleet/roster weekly runway aggregate on the `--json` wire (issue #544) — the machine peer of
/// the human band's `fleet` line. `runway_secs` is explicit `null` (NEVER a sentinel like `0` /
/// `999`) when the counted accounts have no combined burn; `counted` / `observed` carry the `n of m`
/// cardinality so a reader sees exactly how many accounts the figure rests on — and that
/// `observed − counted` accounts were EXCLUDED for an unknown / stale velocity (honest degradation,
/// never silently zero-burned).
#[derive(Serialize)]
struct FleetWire {
    /// Approximate whole seconds until the roster's COMBINED weekly head-room is exhausted at its
    /// combined weekly burn, or `null` when no counted account has a measurable burn.
    runway_secs: Option<i64>,
    /// Accounts that CONTRIBUTED to the aggregate (a known weekly velocity) — the `n` of `n of m`.
    counted: usize,
    /// Accounts OBSERVED in the window (`seen > 0`) — the `m`; `observed − counted` were excluded.
    observed: usize,
}

#[derive(Serialize)]
struct AccountWire {
    seen: u32,
    coverage: f64,
    /// Neutral data-completeness descriptor (issue #160 consumes it; no recommendation).
    coverage_class: CoverageClass,
    session: DimWire,
    weekly: DimWire,
    cap_hits: u32,
    time_at_cap_secs: i64,
    contribution_share: f64,
    /// Neutral utilisation-level descriptor from the session peak (issue #160 consumes it).
    band: Band,
    /// The velocity + runway readout (issue #543): PRESENT on a summary account with a KNOWN
    /// session velocity, OMITTED otherwise (insufficient / stale data — the reader reads that as
    /// an absent field) and on every series bucket + orphan (a current-rate readout is neither
    /// per-bucket nor a non-roster metric). Additive to `schema:1` (the `#159`/`#160`
    /// extend-without-bumping precedent); does NOT bump `schema`.
    #[serde(skip_serializing_if = "Option::is_none")]
    velocity: Option<VelocityWire>,
}

/// The per-account velocity + runway readout on the `--json` wire (issue #543) — the machine peer
/// of the human band's `velocity` / `runway` lines. A KNOWN object carries the session rate as a
/// real number; an individually-unknown figure (a zero-rate or at/over-trigger runway, a flat
/// weekly) is explicit `null`, NEVER a sentinel like `0` or `999`. Rates are `%/min`; runways are
/// whole seconds (the reader scales to whatever unit it renders).
#[derive(Serialize)]
struct VelocityWire {
    /// Smoothed session-usage rate in percent-per-minute (#539's EMA, replayed stats-side over
    /// the stored series). Always a real number — the object is present only when it is known.
    session_pct_per_min: f64,
    /// Smoothed weekly-usage rate in percent-per-minute, or `null` when the weekly dimension has
    /// no measurable rate (flat / reset / fewer than two sample intervals).
    weekly_pct_per_min: Option<f64>,
    /// Approximate whole seconds until the session reading reaches `session_trigger`, or `null`
    /// when the rate is `0` or the reading is already at/over the trigger (no positive head-room).
    session_runway_secs: Option<i64>,
    /// Approximate whole seconds until the weekly reading reaches `weekly_trigger`, or `null`.
    weekly_runway_secs: Option<i64>,
}

#[derive(Serialize)]
struct DimWire {
    mean: f64,
    peak: f64,
    p95: f64,
}

#[derive(Serialize)]
struct RosterWire {
    swap_count: u32,
    swaps: SwapsWire,
    all_high_episodes: u32,
    all_high_secs: i64,
}

#[derive(Serialize)]
struct SwapsWire {
    session: u32,
    weekly: u32,
    manual: u32,
    forced: u32,
    emergency: u32,
}

/// A neutral utilisation band from a session peak fraction — a DESCRIPTOR, not a signal:
/// it classifies the level, it does not recommend an action (that is issue #160). Bands
/// are fixed (not the config trigger) so the wire vocabulary is stable across configs.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Band {
    /// peak < 20%
    Idle,
    /// 20% ≤ peak < 50%
    Low,
    /// 50% ≤ peak < 80%
    Moderate,
    /// 80% ≤ peak < 100%
    High,
    /// peak ≥ 100% (at or over the quota cap)
    AtCap,
}

impl Band {
    fn of(session_peak: f64) -> Self {
        if session_peak >= 1.0 {
            Self::AtCap
        } else if session_peak >= 0.8 {
            Self::High
        } else if session_peak >= 0.5 {
            Self::Moderate
        } else if session_peak >= 0.2 {
            Self::Low
        } else {
            Self::Idle
        }
    }
}

/// A neutral data-completeness descriptor.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum CoverageClass {
    /// The period was fully sampled for this account.
    Complete,
    /// The period was under-sampled — read the metrics with that caveat.
    Partial,
    /// No reading of this account's own in the period (it may still hold a contribution
    /// share as the active, but unsampled, credential).
    Absent,
}

impl CoverageClass {
    fn of(a: &AccountStats) -> Self {
        if a.seen == 0 {
            Self::Absent
        } else if a.coverage >= 1.0 {
            Self::Complete
        } else {
            Self::Partial
        }
    }
}

/// Build the stable `--json` wire document from a resolved report. Extracted from [`render_json`]
/// so BOTH the CLI `--json` render (pretty, below) AND the daemon `stats` socket verb (issue #356,
/// [`socket_stats_reply`], compact) serialize the IDENTICAL `StatsWire` — the R-2 parity guarantee
/// is structural (one builder), not a re-derivation kept in lockstep by hand.
fn stats_wire(report: &Report) -> StatsWire<'_> {
    let (period, since) = match &report.window.kind {
        WindowKind::Period(p) => (Some(p.wire_tag()), None),
        WindowKind::Since(s) => (None, Some(s.as_str())),
    };
    StatsWire {
        schema: JSON_SCHEMA_VERSION,
        window: WindowWire {
            start: report.window.start,
            end: report.window.end,
            label: format_window_label(&report.window, report.offset),
            period,
            since,
        },
        accounts: &report.accounts,
        series: report
            .series
            .iter()
            .map(|r| BucketWire {
                start: r.period.start,
                end: r.period.end,
                roster: roster_wire(&r.roster),
                // Series buckets carry no velocity — it is a CURRENT-rate readout, not per-bucket.
                accounts: accounts_wire(&r.per_account, None),
            })
            .collect(),
        summary: PeriodWire {
            roster: roster_wire(&report.summary.roster),
            accounts: accounts_wire(&report.summary.per_account, Some(&report.velocity)),
            // The fleet/roster runway aggregate (issue #544) — summary-only, from the SAME built
            // report the CLI and daemon socket both serialize, so the fleet figure keeps R-2 parity
            // structurally (one `stats_wire` builder). Absent on a bare aggregate (no overlay).
            fleet: fleet_runway(report).map(fleet_wire),
        },
        // Orphans carry no velocity either (a non-roster readout is out of scope, issue #543).
        orphans: accounts_wire(&report.orphans, None),
    }
}

/// Render the stable `--json` document — the human / file view: PRETTY-printed with a trailing
/// newline. (The daemon `stats` socket verb serializes the same [`stats_wire`] COMPACT, no trailing
/// newline — issue #356; the newline is the socket framing, added on write.)
fn render_json(report: &Report) -> Result<String> {
    let mut json = serde_json::to_string_pretty(&stats_wire(report))
        .map_err(|_| Error::StatsSerialize("a usage value was not a finite number"))?;
    json.push('\n');
    Ok(json)
}

/// Map a per-account aggregate map to its wire form, attaching each account's velocity readout
/// from `velocity` when supplied (the summary; `None` for series buckets and orphans, which carry
/// no velocity — issue #543).
fn accounts_wire(
    per_account: &BTreeMap<String, AccountStats>,
    velocity: Option<&BTreeMap<String, AccountVelocity>>,
) -> BTreeMap<String, AccountWire> {
    per_account
        .iter()
        .map(|(handle, a)| {
            let v = velocity.and_then(|m| m.get(handle));
            (handle.clone(), account_wire(a, v))
        })
        .collect()
}

fn account_wire(a: &AccountStats, velocity: Option<&AccountVelocity>) -> AccountWire {
    AccountWire {
        seen: a.seen,
        coverage: a.coverage,
        coverage_class: CoverageClass::of(a),
        session: dim_wire(&a.session),
        weekly: dim_wire(&a.weekly),
        cap_hits: a.cap_hits,
        time_at_cap_secs: a.time_at_cap_secs,
        contribution_share: a.contribution_share,
        band: Band::of(a.session.peak),
        velocity: velocity.and_then(velocity_wire),
    }
}

/// The wire form of an [`AccountVelocity`] — `Some` only when the SESSION velocity is known (the
/// discriminator between "figures present" and "object absent"); the session rate is then a real
/// number and the weekly rate / both runways are explicit `null` when individually unknown. Rates
/// are rounded to `%/min` (3 decimals) so the wire is stable — no float-tail noise — while keeping
/// the weekly dimension's small figures; runways stay whole seconds.
fn velocity_wire(v: &AccountVelocity) -> Option<VelocityWire> {
    let session = v.session_rate?; // absent field when the session velocity is unknown
    Some(VelocityWire {
        session_pct_per_min: round_pct_per_min(session),
        weekly_pct_per_min: v.weekly_rate.map(round_pct_per_min),
        session_runway_secs: v.session_runway_secs,
        weekly_runway_secs: v.weekly_runway_secs,
    })
}

/// The wire form of the fleet/roster runway aggregate (issue #544). A plain field copy — the honest
/// degradation (unknown runway → `null`, the `n of m` cardinality) is already resolved in
/// [`fleet_runway`]; this only reshapes it for `serde`.
fn fleet_wire(f: FleetRunway) -> FleetWire {
    FleetWire {
        runway_secs: f.runway_secs,
        counted: f.counted,
        observed: f.observed,
    }
}

/// A usage rate (fraction/second) as percent-per-minute, rounded to 3 decimals so the `--json`
/// wire is stable across runs yet keeps the weekly dimension's small values.
fn round_pct_per_min(rate_frac_per_sec: f64) -> f64 {
    (pct_per_min(rate_frac_per_sec) * 1000.0).round() / 1000.0
}

fn dim_wire(d: &crate::usage_stats::DimStats) -> DimWire {
    DimWire {
        mean: d.mean,
        peak: d.peak,
        p95: d.p95,
    }
}

fn roster_wire(r: &RosterStats) -> RosterWire {
    RosterWire {
        swap_count: r.swap_count,
        swaps: SwapsWire {
            session: r.swaps.session,
            weekly: r.swaps.weekly,
            manual: r.swaps.manual,
            forced: r.swaps.forced,
            emergency: r.swaps.emergency,
        },
        all_high_episodes: r.all_high_episodes,
        all_high_secs: r.all_high_secs,
    }
}

// --- rendering: terminal charts (issue #159) --------------------------------
//
// Hand-rolled, dependency-free charts over the SAME series/summary the #158 base verb
// already produced — nothing is re-aggregated here, the store is not re-read. The charts
// render ONLY on an interactive TTY (a piped / redirected `stats` keeps the plain numeric
// table, [`render_human`]); they reuse the `status` view's render discipline — the shared
// [`display_width`], the shared colour gate, pad-before-colour, and priority column-drop
// that NEVER wraps a row. Every glyph encodes MAGNITUDE on the fixed 0–100% (cap) scale,
// so a no-colour reader keeps the full signal; colour merely augments. A GAP — a bucket in
// which an account had no reading — renders as a BREAK (a space), never a fabricated 0%.

/// The 8-level Unicode "vertical bar" ramp for the sparkline height: index `0` (a real,
/// lowest reading) → `▁`, `7` → `█`. A GAP is NOT in the ramp — it renders as a break, so
/// an absent bucket can never read as a fabricated 0%.
const SPARK_UNICODE: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// The 8-level ASCII intensity ramp (`--ascii` / `TERM=dumb`): the classic light→heavy
/// shading run; index `0` → `.` (a real lowest reading, still distinct from a ` ` gap).
const RAMP_ASCII: [char; 8] = ['.', ':', '-', '=', '+', '*', '#', '@'];
/// The 4-level Unicode shade ramp for the heatmap grid: `░` (lowest reading) → `█`.
const SHADE_UNICODE: [char; 4] = ['░', '▒', '▓', '█'];
/// The 4-level ASCII shade ramp for the heatmap grid.
const SHADE_ASCII: [char; 4] = ['.', ':', '+', '#'];
/// The bar glyphs for the horizontal-bar chart: `(fill, track)`, Unicode then ASCII.
const BAR_UNICODE: (char, char) = ('█', '░');
const BAR_ASCII: (char, char) = ('#', '-');

impl Band {
    /// The ANSI SGR colour for this band under the chart overlay, reusing the `status`
    /// view's green/yellow/red vocabulary: idle/low read green (headroom), moderate yellow
    /// (worth watching), high/at-cap red (near/over the cap). Emitted only when the shared
    /// colour gate is open ([`crate::cli::should_colorize`]); carries no secret (issue #15).
    fn sgr(self) -> &'static str {
        match self {
            Band::Idle | Band::Low => "32",
            Band::Moderate => "33",
            Band::High | Band::AtCap => "31",
        }
    }
}

/// A utilisation fraction → an `0..=(n-1)` ramp level on the FIXED `[0, 1]` (0–100%, the
/// cap) scale — an ABSOLUTE, cross-account-comparable magnitude, never normalised to the
/// series' own max (which would editorialise a flat-low account into a spiky one). A real
/// `0.0` maps to level `0` (the lowest glyph, a genuine reading); an over-cap reading
/// (`> 1.0`) clamps to the top. `n` is the ramp length (8 for the bar ramp, 4 for shade).
fn ramp_level(v: f64, n: usize) -> usize {
    let top = (n - 1) as f64;
    ((v.clamp(0.0, 1.0) * top).round() as usize).min(n - 1)
}

/// One sparkline glyph for a bucket value: a break (` `) for a GAP (`None`), else the ramp
/// glyph at the value's absolute level.
fn spark_glyph(v: Option<f64>, ascii: bool) -> char {
    match v {
        None => ' ',
        Some(v) => {
            let ramp = if ascii { &RAMP_ASCII } else { &SPARK_UNICODE };
            ramp[ramp_level(v, ramp.len())]
        }
    }
}

/// One heatmap-cell glyph: a break (` `) for a GAP, else the shade at the value's level.
fn shade_glyph(v: Option<f64>, ascii: bool) -> char {
    match v {
        None => ' ',
        Some(v) => {
            let ramp = if ascii { &SHADE_ASCII } else { &SHADE_UNICODE };
            ramp[ramp_level(v, ramp.len())]
        }
    }
}

/// One account's per-bucket `pick` values across the series, with GAPS (`None`) where the
/// account had NO reading in that bucket — it is absent from the bucket, or present with
/// `seen == 0`. Charts render those as breaks, never a fabricated 0% (issue #159 gap
/// honesty, mirroring the aggregator: an absent bucket is unknown, not calm).
fn account_series(
    series: &[UsageReport],
    handle: &str,
    pick: fn(&AccountStats) -> f64,
) -> Vec<Option<f64>> {
    series
        .iter()
        .map(|b| match b.per_account.get(handle) {
            Some(a) if a.seen > 0 => Some(pick(a)),
            _ => None,
        })
        .collect()
}

/// The per-bucket session peak — the sparkline / heatmap "how hot did it get" pick.
fn session_peak(a: &AccountStats) -> f64 {
    a.session.peak
}
/// The per-bucket session mean — the heatmap "average load" pick (complements the peak
/// trend so the two views are not the same number twice).
fn session_mean(a: &AccountStats) -> f64 {
    a.session.mean
}

/// A sparkline string for a per-bucket value series: one glyph per bucket, gaps as breaks.
fn render_sparkline(values: &[Option<f64>], ascii: bool) -> String {
    values.iter().map(|&v| spark_glyph(v, ascii)).collect()
}

/// One droppable table column: a header, per-row cells, an optional per-row colour, the
/// spaces rendered BEFORE it, and a drop priority (`None` = always keep; `Some(n)` =
/// droppable, the LOWEST present `n` dropping first under a narrow terminal). Mirrors the
/// `status` view's [`Column`](crate::cli) discipline but over already-rendered string cells.
struct ChartCol {
    header: &'static str,
    cells: Vec<String>,
    colors: Vec<Option<&'static str>>,
    lead_gap: usize,
    priority: Option<u8>,
}

impl ChartCol {
    /// This column's render width: the widest of its header and cells, on DISPLAY width.
    fn width(&self) -> usize {
        self.cells
            .iter()
            .map(|s| display_width(s))
            .max()
            .unwrap_or(0)
            .max(display_width(self.header))
    }
}

/// The rendered width of a column set: summed column widths plus each column's lead gap.
fn table_width(columns: &[ChartCol]) -> usize {
    columns.iter().map(|c| c.lead_gap + c.width()).sum()
}

/// Render one table line: each cell preceded by its lead gap, LEFT-padded to its column
/// width on DISPLAY width, colour wrapping the raw cell BEFORE the pad (so the escape bytes
/// never enter the width math and stripping them recovers the exact plain table), trailing
/// whitespace trimmed. The `status` view's `render_cells` discipline (issue #159 reuse).
fn render_line(
    cells: &[&str],
    widths: &[usize],
    colors: &[Option<&str>],
    gaps: &[usize],
) -> String {
    let mut line = String::new();
    for (((cell, &width), color), &gap) in cells.iter().zip(widths).zip(colors).zip(gaps) {
        line.push_str(&" ".repeat(gap));
        match color {
            Some(sgr) => line.push_str(&format!("\x1b[{sgr}m{cell}\x1b[0m")),
            None => line.push_str(cell),
        }
        line.push_str(&" ".repeat(width.saturating_sub(display_width(cell))));
    }
    format!("{}\n", line.trim_end())
}

/// Render a header row plus one line per data row, dropping the lowest-priority droppable
/// columns until the table fits `w` — or only always-keep columns remain, in which case the
/// table is allowed to OVERFLOW rather than WRAP a row (issue #159: never wrap). Colour is
/// applied per cell only when `color` is set.
fn render_table(mut columns: Vec<ChartCol>, w: usize, color: bool) -> String {
    while table_width(&columns) > w {
        match columns.iter().filter_map(|c| c.priority).min() {
            Some(p) => columns.retain(|c| c.priority != Some(p)),
            None => break, // only keep-columns left → accept overflow, never wrap
        }
    }
    let widths: Vec<usize> = columns.iter().map(ChartCol::width).collect();
    let gaps: Vec<usize> = columns.iter().map(|c| c.lead_gap).collect();
    let n_rows = columns.first().map_or(0, |c| c.cells.len());

    let headers: Vec<&str> = columns.iter().map(|c| c.header).collect();
    let no_color: Vec<Option<&str>> = vec![None; columns.len()];
    let mut out = render_line(&headers, &widths, &no_color, &gaps);
    for r in 0..n_rows {
        let cells: Vec<&str> = columns.iter().map(|c| c.cells[r].as_str()).collect();
        let colors: Vec<Option<&str>> = columns
            .iter()
            .map(|c| if color { c.colors[r] } else { None })
            .collect();
        out.push_str(&render_line(&cells, &widths, &colors, &gaps));
    }
    out
}

/// The per-account chart table: `account`, the whole-window `session` and `weekly` peak %,
/// and a `trend` sparkline of the per-bucket session peak. Priority column-drop under a
/// narrow terminal — `trend` drops FIRST, then `weekly`; `account` + `session` (the most
/// actionable signal) are always kept — never wrapping. Colour tints each `%` by its
/// neutral utilisation band; the sparkline glyphs carry their own magnitude.
fn render_chart_table(
    report: &Report,
    accounts: &[&String],
    w: usize,
    color: bool,
    ascii: bool,
) -> String {
    let summary = &report.summary;
    let n = accounts.len();
    let (mut acct, mut sess, mut sess_c) = (Vec::new(), Vec::new(), Vec::new());
    let (mut week, mut week_c, mut trend) = (Vec::new(), Vec::new(), Vec::new());
    for &h in accounts {
        let a = &summary.per_account[h];
        acct.push(h.clone());
        sess.push(format!("{}%", pct(a.session.peak)));
        sess_c.push(Some(Band::of(a.session.peak).sgr()));
        week.push(format!("{}%", pct(a.weekly.peak)));
        week_c.push(Some(Band::of(a.weekly.peak).sgr()));
        trend.push(render_sparkline(
            &account_series(&report.series, h, session_peak),
            ascii,
        ));
    }
    let columns = vec![
        ChartCol {
            header: "account",
            cells: acct,
            colors: vec![None; n],
            lead_gap: 0,
            priority: None,
        },
        ChartCol {
            header: "session",
            cells: sess,
            colors: sess_c,
            lead_gap: 2,
            priority: None,
        },
        ChartCol {
            header: "weekly",
            cells: week,
            colors: week_c,
            lead_gap: 2,
            priority: Some(2),
        },
        ChartCol {
            header: "trend",
            cells: trend,
            colors: vec![None; n],
            lead_gap: 2,
            priority: Some(1),
        },
    ];
    render_table(columns, w, color)
}

/// The cross-account horizontal-bar chart: each account's whole-window contribution share
/// (the fraction of in-period observations made while it was the active credential) as a
/// bar filled on the FIXED 0–100% scale, followed by the share percent. `None` when the
/// terminal is too narrow for a readable bar (the block degrades away cleanly, issue #159).
fn render_bars(report: &Report, accounts: &[&String], w: usize, ascii: bool) -> Option<String> {
    let summary = &report.summary;
    let (fill, track) = if ascii { BAR_ASCII } else { BAR_UNICODE };
    let label_w = accounts.iter().map(|h| display_width(h)).max().unwrap_or(0);
    // line = label + "  " + bar + "  " + "NNN%"; reserve 4 for the percent field.
    let bar_w = w.checked_sub(label_w + 2 + 2 + 4)?;
    if bar_w < 4 {
        return None;
    }
    let mut out = String::from("contribution share\n");
    for &h in accounts {
        let share = summary.per_account[h].contribution_share;
        let filled = (share.clamp(0.0, 1.0) * bar_w as f64).round() as usize;
        let bar: String = std::iter::repeat_n(fill, filled)
            .chain(std::iter::repeat_n(track, bar_w - filled))
            .collect();
        out.push_str(&format!(
            "{}  {bar}  {:>3}%\n",
            pad_end(h, label_w),
            pct(share),
        ));
    }
    Some(out)
}

/// The account × bucket heatmap: one shaded row per account, one cell per series bucket,
/// shaded by that bucket's session MEAN — the "when was each account loaded" pattern that
/// complements the peak trend column. Gaps render as breaks. `None` when the grid is wider
/// than the terminal (it degrades away rather than wrapping, issue #159). Colour tints each
/// cell by its own value's band, so the grid reads as a true heat map when the gate is open.
fn render_heatmap(
    report: &Report,
    accounts: &[&String],
    w: usize,
    color: bool,
    ascii: bool,
) -> Option<String> {
    let buckets = report.series.len();
    let label_w = accounts.iter().map(|h| display_width(h)).max().unwrap_or(0);
    if buckets == 0 || label_w + 2 + buckets > w {
        return None;
    }
    let unit = if report.window.base_bucket() == HOUR_SECS {
        "hourly"
    } else {
        "daily"
    };
    let mut out = format!("session pattern — {unit} mean\n");
    for &h in accounts {
        let values = account_series(&report.series, h, session_mean);
        let mut line = format!("{}  ", pad_end(h, label_w));
        for &v in &values {
            let g = shade_glyph(v, ascii);
            match (color, v) {
                (true, Some(val)) => {
                    line.push_str(&format!("\x1b[{}m{}\x1b[0m", Band::of(val).sgr(), g))
                }
                _ => line.push(g),
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    Some(out)
}

/// The per-account session-distribution gauge: a 0–100% track marking `mean` (`m`), `p95`
/// (`P`) and `peak` (`x`), with the exact percents alongside so the distribution reads in
/// text without colour. `None` when the terminal is too narrow for a readable track. On a
/// marker collision the higher statistic wins the cell (peak over p95 over mean).
fn render_percentiles(
    report: &Report,
    accounts: &[&String],
    w: usize,
    ascii: bool,
) -> Option<String> {
    let summary = &report.summary;
    let (track, lb, rb) = if ascii {
        ('-', '[', ']')
    } else {
        ('─', '┤', '├')
    };
    let label_w = accounts.iter().map(|h| display_width(h)).max().unwrap_or(0);
    // The widest "NN% · NN% · NN%" trailer, so every gauge shares one width and aligns.
    let trailer = |a: &AccountStats| {
        format!(
            "{}% · {}% · {}%",
            pct(a.session.mean),
            pct(a.session.p95),
            pct(a.session.peak)
        )
    };
    let trailer_w = accounts
        .iter()
        .map(|&h| display_width(&trailer(&summary.per_account[h])))
        .max()
        .unwrap_or(0);
    // line = label + "  " + lb + gauge + rb + "  " + trailer; brackets are one cell each.
    let gauge_w = w.checked_sub(label_w + 2 + 1 + 1 + 2 + trailer_w)?.min(40);
    if gauge_w < 8 {
        return None;
    }
    let pos = |v: f64| (v.clamp(0.0, 1.0) * (gauge_w - 1) as f64).round() as usize;
    let mut out = String::from("session distribution — mean · p95 · peak\n");
    for &h in accounts {
        let a = &summary.per_account[h];
        let mut buf = vec![track; gauge_w];
        // Lower statistic first, so a higher one overwrites it on a shared cell.
        buf[pos(a.session.mean)] = 'm';
        buf[pos(a.session.p95)] = 'P';
        buf[pos(a.session.peak)] = 'x';
        let gauge: String = buf.into_iter().collect();
        out.push_str(&format!(
            "{}  {lb}{gauge}{rb}  {}\n",
            pad_end(h, label_w),
            trailer(a),
        ));
    }
    Some(out)
}

/// The compact "not in roster" footer line for the CHARTS view (issue #314): the orphan
/// handles named inline, e.g. `not in roster (2): backup, spare`. `None` when there are no
/// orphans (the caller appends nothing). The numeric view renders the fuller orphan TABLE
/// instead; the charts view keeps it to a single named line so orphans never take a peer
/// chart slot, yet remain impossible to mistake for live accounts.
fn orphan_names_line(orphans: &BTreeMap<String, AccountStats>) -> Option<String> {
    if orphans.is_empty() {
        return None;
    }
    let names: Vec<&str> = orphans.keys().map(String::as_str).collect();
    Some(format!(
        "not in roster ({}): {}\n",
        orphans.len(),
        names.join(", ")
    ))
}

/// Compose the HUMAN-facing charts view for an interactive TTY (issue #159): the window
/// echo, the per-account chart table (with inline sparkline), then the bars / heatmap /
/// percentile blocks (each degrading away cleanly when the terminal is too narrow), footed
/// by an optional "not in roster" line (issue #314) and the same roster line the numeric
/// view uses. Pure over `(w, color, ascii)` so the whole view is golden-testable at a fixed
/// width / colour / ramp.
fn render_charts(report: &Report, w: usize, color: bool, ascii: bool) -> String {
    let mut out = format!(
        "usage — {}\n\n",
        format_window_label(&report.window, report.offset)
    );
    // `per_account` is already live-roster-only (orphans were split out in `build_report`),
    // so every chart below plots live accounts; orphans surface only in the footer line.
    let accounts: Vec<&String> = report.summary.per_account.keys().collect();
    if accounts.is_empty() {
        out.push_str("  no per-account usage in this window\n\n");
        if let Some(line) = orphan_names_line(&report.orphans) {
            out.push_str(&line);
        }
        out.push_str(&roster_line(&report.summary.roster));
        return out;
    }
    out.push_str(&render_chart_table(report, &accounts, w, color, ascii));
    for block in [
        render_bars(report, &accounts, w, ascii),
        render_heatmap(report, &accounts, w, color, ascii),
        render_percentiles(report, &accounts, w, ascii),
    ]
    .into_iter()
    .flatten()
    {
        out.push('\n');
        out.push_str(&block);
    }
    out.push('\n');
    // The neutral summary band (issue #160), the "not in roster" line (issue #314), then the
    // roster line. Honours the shared colour gate (symmetric emphasis when open; the
    // descriptor word still carries the signal when closed).
    let band = render_summary(report, color);
    if !band.is_empty() {
        out.push_str(&band);
        out.push('\n');
    }
    if let Some(line) = orphan_names_line(&report.orphans) {
        out.push_str(&line);
    }
    out.push_str(&roster_line(&report.summary.roster));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- issue #159 chart fixtures: hand-built reports for deterministic goldens ------

    /// One dimension's stats.
    fn ds(mean: f64, peak: f64, p95: f64) -> crate::usage_stats::DimStats {
        crate::usage_stats::DimStats { mean, peak, p95 }
    }

    /// An account row: `seen`, its session dimension, its weekly PEAK (mean/p95 unused by
    /// the charts), and its contribution share. `seen == 0` is a GAP for chart purposes.
    fn stat(
        seen: u32,
        session: crate::usage_stats::DimStats,
        weekly_peak: f64,
        share: f64,
    ) -> AccountStats {
        AccountStats {
            seen,
            expected: 1.0,
            coverage: 1.0,
            session,
            weekly: ds(0.0, weekly_peak, 0.0),
            cap_hits: 0,
            time_at_cap_secs: 0,
            contribution_share: share,
        }
    }

    /// A `UsageReport` (series bucket or summary) from an account list.
    fn ureport(accts: &[(&str, AccountStats)]) -> UsageReport {
        UsageReport {
            period: Period::new(0, HOUR_SECS),
            per_account: accts.iter().map(|(h, a)| (h.to_string(), *a)).collect(),
            roster: RosterStats::default(),
        }
    }

    /// A charted `Report`: an hourly-bucketed `day` window (so the heatmap reads "hourly"),
    /// a summary account list, and a per-bucket series. Offset 0 (deterministic echo).
    fn charts_report(
        summary: &[(&str, AccountStats)],
        series: &[&[(&str, AccountStats)]],
    ) -> Report {
        Report {
            window: Window {
                start: epoch("2026-06-30T12:00:00Z"),
                end: epoch("2026-07-01T12:00:00Z"),
                kind: WindowKind::Period(PeriodSpec::Day),
            },
            accounts: vec![],
            summary: ureport(summary),
            series: series.iter().map(|b| ureport(b)).collect(),
            offset: 0,
            orphans: BTreeMap::new(),
            velocity: BTreeMap::new(),
        }
    }

    /// The canonical two-account fixture used across the chart goldens. `alpha` runs hot
    /// (session peak 0.99) and carries most of the roster; `beta` idles. `beta` is GAP in
    /// buckets 1 and 3, `alpha` in bucket 3 — so both a trend and a heatmap row carry an
    /// interior break, proving a gap renders as a break, never a 0%.
    fn two_account_charts() -> Report {
        let alpha_sum = stat(4, ds(0.50, 0.99, 0.80), 0.40, 0.75);
        let beta_sum = stat(2, ds(0.10, 0.20, 0.15), 0.05, 0.25);
        let a = |m, p| stat(1, ds(m, p, p), 0.0, 0.0);
        charts_report(
            &[("alpha", alpha_sum), ("beta", beta_sum)],
            &[
                &[("alpha", a(0.20, 0.30)), ("beta", a(0.10, 0.10))],
                &[("alpha", a(0.50, 0.60))], // beta GAP
                &[("alpha", a(0.90, 0.99)), ("beta", a(0.15, 0.20))],
                &[], // both GAP
            ],
        )
    }

    /// The sorted account handles of a report, as the chart renderers receive them.
    fn keys(r: &Report) -> Vec<&String> {
        r.summary.per_account.keys().collect()
    }

    // --- issue #159 AC: chart glyph primitives (fixed absolute scale, gaps ≠ 0%) ------

    #[test]
    fn ramp_level_is_a_fixed_absolute_scale_clamped_at_the_cap() {
        // 0% → level 0 (a real lowest reading), 100% → the top, over-cap clamps, mid rounds.
        assert_eq!(ramp_level(0.0, 8), 0);
        assert_eq!(ramp_level(1.0, 8), 7);
        assert_eq!(
            ramp_level(1.5, 8),
            7,
            "over-cap clamps, never overflows the ramp"
        );
        assert_eq!(ramp_level(0.5, 8), 4, "0.5·7 = 3.5 rounds to 4");
        assert_eq!(ramp_level(0.0, 4), 0);
        assert_eq!(ramp_level(1.0, 4), 3);
    }

    #[test]
    fn a_gap_renders_as_a_break_never_a_zero() {
        // The crux of AC "gaps render as breaks (not zero)": a GAP is a space; a real 0%
        // reading is the LOWEST glyph — visibly distinct, so an absent bucket never reads
        // as a fabricated calm. Holds for both the Unicode and the ASCII ramp.
        assert_eq!(spark_glyph(None, false), ' ');
        assert_eq!(spark_glyph(Some(0.0), false), '▁');
        assert_eq!(spark_glyph(None, true), ' ');
        assert_eq!(spark_glyph(Some(0.0), true), '.');
        assert_eq!(shade_glyph(None, false), ' ');
        assert_eq!(shade_glyph(Some(0.0), false), '░');
        assert_eq!(shade_glyph(Some(1.0), false), '█');
    }

    #[test]
    fn render_sparkline_is_deterministic_with_gaps_as_breaks() {
        // A real 0% (▁), a gap (space), a peak (█), and a mid value (▅) — the interior
        // space is the break, not a 0% glyph.
        assert_eq!(
            render_sparkline(&[Some(0.0), None, Some(1.0), Some(0.5)], false),
            "▁ █▅"
        );
        assert_eq!(
            render_sparkline(&[Some(0.0), None, Some(1.0), Some(0.5)], true),
            ". @+"
        );
    }

    #[test]
    fn account_series_marks_absent_or_unseen_buckets_as_gaps() {
        let series = vec![
            ureport(&[("a", stat(1, ds(0.3, 0.3, 0.3), 0.0, 0.0))]),
            ureport(&[]), // account absent from the bucket → gap
            ureport(&[("a", stat(0, ds(0.9, 0.9, 0.9), 0.0, 0.0))]), // present but seen==0 → gap
        ];
        assert_eq!(
            account_series(&series, "a", session_peak),
            vec![Some(0.3), None, None]
        );
    }

    // --- issue #159 AC: full chart set on a wide interactive TTY (golden strings) ------

    #[test]
    fn chart_table_golden_wide() {
        let r = two_account_charts();
        assert_eq!(
            render_chart_table(&r, &keys(&r), 60, false, false),
            "account  session  weekly  trend\n\
             alpha    99%      40%     ▃▅█\n\
             beta     20%      5%      ▂ ▂\n",
        );
    }

    #[test]
    fn bars_heatmap_percentiles_golden_wide() {
        let r = two_account_charts();
        assert_eq!(
            render_bars(&r, &keys(&r), 60, false).unwrap(),
            "contribution share\n\
             alpha  ███████████████████████████████████░░░░░░░░░░░░   75%\n\
             beta   ████████████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░   25%\n",
        );
        assert_eq!(
            render_heatmap(&r, &keys(&r), 60, false, false).unwrap(),
            "session pattern — hourly mean\n\
             alpha  ▒▓█\n\
             beta   ░ ░\n",
            "the heatmap carries interior gaps as breaks too",
        );
        assert_eq!(
            render_percentiles(&r, &keys(&r), 60, false).unwrap(),
            "session distribution — mean · p95 · peak\n\
             alpha  ┤─────────────────m────────P──────x├  50% · 80% · 99%\n\
             beta   ┤───m─P─x──────────────────────────├  10% · 15% · 20%\n",
            "distinct mean/p95/peak markers spread apart; clustered where they are close",
        );
    }

    // --- issue #249 AC: wide-glyph label columns align on DISPLAY width ----------------

    /// A three-row chart report whose account labels stress display-width padding: an ASCII
    /// label (5 cells), a CJK triple (`日本語`, 6 cells — the widest, so it sets `label_w`),
    /// and a ZWJ-family emoji (one coalesced 2-cell glyph, 5 code points). Rust's
    /// `{:<width$}` fill pads by `char` count, giving these three DIFFERENT display widths;
    /// only display-width padding lands the next column at one place. Every account is
    /// present and non-zero in the single series bucket, so the heatmap carries no leading
    /// gap (a space) that could mask a padding bug.
    fn wide_glyph_charts() -> Report {
        let row = |m: f64, p: f64, share: f64| stat(2, ds(m, p, p), 0.3, share);
        let accts = [
            ("ascii", row(0.40, 0.60, 0.50)),
            ("日本語", row(0.20, 0.40, 0.30)),
            ("👨\u{200D}👩\u{200D}👧", row(0.10, 0.20, 0.20)),
        ];
        charts_report(&accts, &[&accts[..]])
    }

    /// The three wide-glyph labels, in the sorted order the renderers receive them.
    const WIDE_LABELS: [&str; 3] = ["ascii", "日本語", "👨\u{200D}👩\u{200D}👧"];

    /// The display column at which the content after `label`'s padded field begins in the
    /// row containing `label`: skip the label, then the run of spaces (its right-padding
    /// plus the two-space inter-column gap), landing on the first cell of the next column.
    /// Equal across rows IFF the label column is padded on DISPLAY width (issue #249). The
    /// content that follows every label in these block renderers is left-aligned (a bar / a
    /// heat cell / a gauge bracket), so the first non-space IS that column's first cell.
    fn post_label_col(out: &str, label: &str) -> usize {
        let line = out.lines().find(|l| l.contains(label)).unwrap();
        let after = line.find(label).unwrap() + label.len();
        let gap = line[after..].find(|c: char| c != ' ').unwrap();
        display_width(&line[..after + gap])
    }

    #[test]
    fn render_bars_label_column_aligns_on_display_width() {
        let r = wide_glyph_charts();
        let out = render_bars(&r, &keys(&r), 60, false).unwrap();
        let cols: Vec<usize> = WIDE_LABELS
            .iter()
            .map(|&l| post_label_col(&out, l))
            .collect();
        assert!(
            cols.iter().all(|&c| c == cols[0]),
            "bars: every bar starts at one display column — char-count padding staggers the \
             CJK/emoji rows: {cols:?}\n{out}"
        );
    }

    #[test]
    fn render_heatmap_label_column_aligns_on_display_width() {
        // The heatmap is the worst case: it is read DOWN columns to compare a time bucket
        // across accounts, so a horizontally-shifted row is a cross-account misread.
        let r = wide_glyph_charts();
        let out = render_heatmap(&r, &keys(&r), 60, false, false).unwrap();
        let cols: Vec<usize> = WIDE_LABELS
            .iter()
            .map(|&l| post_label_col(&out, l))
            .collect();
        assert!(
            cols.iter().all(|&c| c == cols[0]),
            "heatmap: every row's cells start at one display column: {cols:?}\n{out}"
        );
    }

    #[test]
    fn render_percentiles_label_column_aligns_on_display_width() {
        let r = wide_glyph_charts();
        let out = render_percentiles(&r, &keys(&r), 60, false).unwrap();
        let cols: Vec<usize> = WIDE_LABELS
            .iter()
            .map(|&l| post_label_col(&out, l))
            .collect();
        assert!(
            cols.iter().all(|&c| c == cols[0]),
            "percentiles: every gauge's opening bracket starts at one display column: \
             {cols:?}\n{out}"
        );
    }

    #[test]
    fn render_text_label_column_aligns_on_display_width() {
        // render_text carried a DOUBLE bug: it sized the label column on `String::len()`
        // (bytes) AND padded on char count. The coverage `%` is a fixed-offset marker after
        // the label (a `{:>3}` field then a literal `%`), so it lands at one display column
        // per row only when the label column is sized AND padded on display width.
        let out = render_text(&wide_glyph_charts());
        let pct_col = |label: &str| {
            let line = out.lines().find(|l| l.contains(label)).unwrap();
            display_width(&line[..line.find('%').unwrap()])
        };
        let cols: Vec<usize> = WIDE_LABELS.iter().map(|&l| pct_col(l)).collect();
        assert!(
            cols.iter().all(|&c| c == cols[0]),
            "text: the coverage `%` aligns across rows: {cols:?}\n{out}"
        );
    }

    #[test]
    fn full_charts_view_wide_tty() {
        let r = two_account_charts();
        let out = render_charts(&r, 60, false, false);
        assert!(out.starts_with("usage — last 24h (Jun 30–Jul 1)\n\n"));
        assert!(out.contains("account  session  weekly  trend\n"));
        assert!(out.contains("contribution share\n"));
        assert!(out.contains("session pattern — hourly mean\n"));
        assert!(out.contains("session distribution — mean · p95 · peak\n"));
        assert!(out
            .trim_end()
            .ends_with("all-accounts-high: 0 episodes (0s)"));
    }

    #[test]
    fn ascii_ramp_replaces_the_unicode_blocks() {
        // AC `TERM=dumb` / `--ascii` → ASCII ramp: the sparkline uses the ASCII intensity
        // run and carries no Unicode block glyph.
        let r = two_account_charts();
        let table = render_chart_table(&r, &keys(&r), 60, false, true);
        assert!(table.contains("alpha    99%      40%     -+@\n"));
        assert!(table.contains("beta     20%      5%      : :\n"));
        for glyph in ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█', '░', '▒', '▓'] {
            assert!(!table.contains(glyph), "no Unicode block survives --ascii");
        }
    }

    // --- issue #159 AC: narrow terminal → priority column-drop, no row wrap -----------

    #[test]
    fn narrow_terminal_drops_trend_then_weekly_keeping_session_never_wrapping() {
        let r = two_account_charts();
        // Just too narrow for the trend column → it drops FIRST; weekly stays.
        let w25 = render_chart_table(&r, &keys(&r), 25, false, false);
        assert_eq!(
            w25,
            "account  session  weekly\n\
             alpha    99%      40%\n\
             beta     20%      5%\n",
        );
        // Narrower still → weekly drops NEXT; account + session are always kept, even when
        // that overflows the width — the row is never wrapped.
        let w15 = render_chart_table(&r, &keys(&r), 15, false, false);
        assert_eq!(
            w15,
            "account  session\n\
             alpha    99%\n\
             beta     20%\n",
        );
        assert_eq!(
            w15.lines().count(),
            3,
            "one header + one line per account: no wrap"
        );
        assert!(
            w15.contains("99%") && w15.contains("20%"),
            "the session signal is kept"
        );
    }

    #[test]
    fn a_very_narrow_terminal_degrades_the_wide_blocks_away() {
        let r = two_account_charts();
        // Below a readable width the bars / heatmap / percentile blocks drop out entirely
        // (rather than wrap or truncate), but the view still renders its table + roster.
        assert!(render_bars(&r, &keys(&r), 12, false).is_none());
        assert!(render_heatmap(&r, &keys(&r), 8, false, false).is_none());
        assert!(render_percentiles(&r, &keys(&r), 20, false).is_none());
        let out = render_charts(&r, 12, false, false);
        assert!(out.contains("account"), "the table still renders");
        assert!(out.contains("roster:"), "the roster line still renders");
        assert!(!out.contains('\x1b'));
    }

    // --- issue #159 AC: piped / non-TTY → numeric table, zero ANSI -------------------

    #[test]
    fn non_tty_falls_back_to_the_numeric_table_with_zero_ansi() {
        let r = two_account_charts();
        let piped = render_human(
            &r,
            TermEnv {
                cols: None,
                color: false,
                ascii: false,
            },
        );
        assert_eq!(
            piped,
            render_text(&r),
            "a piped stats is the #158 numeric table verbatim"
        );
        assert!(!piped.contains('\x1b'), "zero ANSI on a pipe");
        for glyph in ['▁', '█', '░', '▒', '▓', '┤'] {
            assert!(
                !piped.contains(glyph),
                "no chart glyph in the piped numeric table"
            );
        }
    }

    // --- issue #159 AC: NO_COLOR / --no-color → zero ANSI, full signal in text --------

    #[test]
    fn color_gate_governs_every_ansi_byte() {
        let r = two_account_charts();
        // Gate open → the utilisation bands tint the `%` cells (alpha's 99% is red).
        let colored = render_chart_table(&r, &keys(&r), 60, true, false);
        assert!(
            colored.contains("\x1b[31m99%\x1b[0m"),
            "hot session reads red"
        );
        assert!(
            colored.contains("\x1b[32m40%\x1b[0m"),
            "a low weekly reads green"
        );
        // Gate closed → not one escape byte anywhere in the whole view, yet the full signal
        // survives in text (the percentages and the glyphs).
        let plain = render_charts(&r, 60, false, false);
        assert!(!plain.contains('\x1b'), "no ANSI when the gate is closed");
        assert!(
            plain.contains("99%") && plain.contains("▃▅█"),
            "full signal without colour"
        );
    }

    // --- issue #159: --json wire stays byte-stable vs #158 (no chart glyphs) ----------

    #[test]
    fn charts_never_leak_into_the_json_wire() {
        // The charts are presentation-only: the schema:1 wire carries no glyph, no ANSI, no
        // chart field — the #158 contract is unchanged by #159.
        let json = render_json(&two_account_charts()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["schema"], 1);
        assert!(!json.contains('\x1b'));
        for glyph in ['▁', '▂', '█', '░', '▒', '▓', '┤', '├', '─'] {
            assert!(
                !json.contains(glyph),
                "no chart glyph on the wire (issue #159)"
            );
        }
    }

    // --- issue #159: empty / single-sample / all-gap series render without panic -------

    #[test]
    fn degenerate_series_render_without_panicking() {
        // Empty roster.
        let empty = charts_report(&[], &[]);
        let out = render_charts(&empty, 80, true, false);
        assert!(out.contains("no per-account usage in this window"));
        assert!(out.contains("roster:"));

        // A single account with a single sample and no series buckets.
        let single = charts_report(&[("solo", stat(1, ds(0.5, 0.5, 0.5), 0.5, 1.0))], &[]);
        let _ = render_charts(&single, 80, true, false);
        let _ = render_charts(&single, 1, true, true);

        // An account present in the summary but a GAP in every series bucket.
        let all_gap = charts_report(
            &[("ghost", stat(1, ds(0.0, 0.0, 0.0), 0.0, 0.0))],
            &[&[], &[]],
        );
        let out = render_charts(&all_gap, 80, false, false);
        assert!(
            out.contains("ghost"),
            "an all-gap account still lists, its trend all breaks"
        );
        // A pathological width of 0 must not panic either.
        let _ = render_charts(&two_account_charts(), 0, true, true);
    }

    /// A minimal reading: `provider="claude"`, given `acct`, no optionals.
    fn sample(ts: i64, acct: &str, session: f64, weekly: f64) -> Sample {
        Sample::new(ts, "claude", acct, session, weekly)
    }

    /// A `StoreData` from samples + event-log text (empty rollup).
    fn data(samples: Vec<Sample>, events: &str) -> StoreData {
        StoreData {
            samples,
            rollup: Rollup::default(),
            events: events.to_owned(),
        }
    }

    fn params() -> AggregateParams {
        AggregateParams::new(300, 0.80, 0.80)
    }

    /// Velocity knobs for the hermetic velocity + runway tests (issue #543): EMA α 0.5 (the #539
    /// default), session trigger 0.80 (matching `params`' session cap), weekly trigger 0.95.
    fn vparams() -> VelocityParams {
        VelocityParams {
            session_ema_alpha: 0.5,
            session_trigger: 0.80,
            weekly_trigger: 0.95,
        }
    }

    /// Build a `--period day` report from `samples` (window ending at `now`) and overlay the
    /// velocity + runway readout — the SAME `build_report` → `with_velocity` pairing `run` and the
    /// daemon socket verb apply in production.
    fn velocity_report(samples: Vec<Sample>, now: i64) -> Report {
        let store = data(samples, "");
        let window = plan_window(Some("day"), None, now, &store).unwrap();
        with_velocity(
            build_report(&store, window, vec![], None, &params(), 0),
            &store.samples,
            &params(),
            &vparams(),
        )
    }

    /// Resolve an RFC 3339 instant to epoch seconds via the crate's canonical parser.
    fn epoch(s: &str) -> i64 {
        epoch_from_rfc3339(s).expect("valid RFC 3339 fixture")
    }

    // --- AC 3: period + --since parsing and mutual exclusion ------------------

    #[test]
    fn period_spec_parses_the_four_presets_and_rejects_others() {
        assert_eq!(PeriodSpec::parse("day").unwrap(), PeriodSpec::Day);
        assert_eq!(PeriodSpec::parse("week").unwrap(), PeriodSpec::Week);
        assert_eq!(PeriodSpec::parse("month").unwrap(), PeriodSpec::Month);
        assert_eq!(PeriodSpec::parse("lifetime").unwrap(), PeriodSpec::Lifetime);
        assert!(matches!(
            PeriodSpec::parse("fortnight"),
            Err(Error::StatsPeriodInvalid(v)) if v == "fortnight"
        ));
    }

    #[test]
    fn plan_window_defaults_to_a_rolling_week() {
        let now = 1_000_000;
        let w = plan_window(None, None, now, &data(vec![], "")).unwrap();
        assert_eq!(w.end, now);
        assert_eq!(w.start, now - 7 * DAY_SECS, "default is a rolling 7 days");
        assert_eq!(w.kind, WindowKind::Period(PeriodSpec::Week));
    }

    #[test]
    fn plan_window_rejects_period_and_since_together() {
        let err = plan_window(Some("week"), Some("7d"), 0, &data(vec![], "")).unwrap_err();
        assert!(matches!(err, Error::StatsPeriodSinceConflict));
    }

    #[test]
    fn plan_window_surfaces_invalid_period_and_since() {
        assert!(matches!(
            plan_window(Some("bogus"), None, 0, &data(vec![], "")).unwrap_err(),
            Error::StatsPeriodInvalid(_)
        ));
        assert!(matches!(
            plan_window(None, Some("yesterday"), 0, &data(vec![], "")).unwrap_err(),
            Error::StatsSinceInvalid(_)
        ));
    }

    #[test]
    fn since_parses_relative_offsets() {
        let now = 10_000_000;
        assert_eq!(parse_since("45s", now).unwrap(), now - 45);
        assert_eq!(parse_since("30m", now).unwrap(), now - 30 * 60);
        assert_eq!(parse_since("24h", now).unwrap(), now - 24 * HOUR_SECS);
        assert_eq!(parse_since("7d", now).unwrap(), now - 7 * DAY_SECS);
        assert_eq!(parse_since("2w", now).unwrap(), now - 14 * DAY_SECS);
        // Whitespace tolerated; a negative offset is rejected (not a valid look-back).
        assert_eq!(parse_since("  7d ", now).unwrap(), now - 7 * DAY_SECS);
        assert!(matches!(
            parse_since("-3d", now),
            Err(Error::StatsSinceInvalid(_))
        ));
    }

    #[test]
    fn since_parses_absolute_dates_and_instants() {
        assert_eq!(
            parse_since("2026-06-24", 0).unwrap(),
            epoch("2026-06-24T00:00:00Z"),
            "a bare date is UTC midnight"
        );
        assert_eq!(
            parse_since("2026-06-24T06:30:00Z", 0).unwrap(),
            epoch("2026-06-24T06:30:00Z")
        );
        assert!(matches!(
            parse_since("2026-13-40", 0),
            Err(Error::StatsSinceInvalid(_)),
        ));
    }

    #[test]
    fn lifetime_window_anchors_at_the_earliest_datum() {
        let now = 100 * DAY_SECS;
        let mut store = data(vec![sample(now - 3 * DAY_SECS, "work", 0.5, 0.4)], "");
        // A rolled daily bucket far predates the raw sample → it sets the lifetime start.
        store.rollup.daily.push(crate::usage_store::DayBucket {
            day_start: 5 * DAY_SECS,
            count: 10,
            coverage: 1.0,
            session: crate::usage_store::DayStat {
                max: 0.9,
                mean: 0.5,
                p95: 0.8,
                cap_hits: 0,
            },
            weekly: crate::usage_store::DayStat {
                max: 0.4,
                mean: 0.3,
                p95: 0.38,
                cap_hits: 0,
            },
        });
        let w = plan_window(Some("lifetime"), None, now, &store).unwrap();
        assert_eq!(
            w.start,
            5 * DAY_SECS,
            "earliest is the rolled day, not the raw sample"
        );
        assert_eq!(w.end, now);
    }

    #[test]
    fn lifetime_of_an_empty_store_is_a_zero_width_window() {
        let now = 42;
        let w = plan_window(Some("lifetime"), None, now, &data(vec![], "")).unwrap();
        assert_eq!((w.start, w.end), (now, now));
    }

    // --- AC 4: resolved-window echo (local tz, deterministic via explicit offset) --

    #[test]
    fn window_echo_matches_the_ac_example() {
        // A 7-day window ending 2026-07-01; rendered in UTC (offset 0) reads exactly the
        // issue's example.
        let end = epoch("2026-07-01T12:00:00Z");
        let window = Window {
            start: end - 7 * DAY_SECS,
            end,
            kind: WindowKind::Period(PeriodSpec::Week),
        };
        assert_eq!(
            format_window_label(&window, 0),
            "last 7d (Jun 24–Jul 1)",
            "matches `last 7d (Jun 24–Jul 1)`"
        );
    }

    #[test]
    fn window_echo_reflects_the_local_offset() {
        // 2026-07-01T00:30:00Z is still Jun 30 in a −02:00 zone; the echo must follow the
        // supplied offset, not UTC.
        let end = epoch("2026-07-01T00:30:00Z");
        let window = Window {
            start: end - DAY_SECS,
            end,
            kind: WindowKind::Period(PeriodSpec::Day),
        };
        assert_eq!(format_window_label(&window, 0), "last 24h (Jun 30–Jul 1)");
        assert_eq!(
            format_window_label(&window, -2 * HOUR_SECS),
            "last 24h (Jun 29–Jun 30)",
            "the −02:00 offset shifts both ends back a day"
        );
    }

    #[test]
    fn since_echo_reflects_the_raw_input() {
        let end = epoch("2026-07-01T12:00:00Z");
        let window = Window {
            start: end - 3 * DAY_SECS,
            end,
            kind: WindowKind::Since("3d".to_owned()),
        };
        assert_eq!(format_window_label(&window, 0), "since 3d (Jun 28–Jul 1)");
    }

    #[test]
    fn civil_from_epoch_matches_known_dates() {
        assert_eq!(civil_from_epoch(0), (1970, 1, 1));
        assert_eq!(
            civil_from_epoch(epoch("2026-07-01T00:00:00Z")),
            (2026, 7, 1)
        );
        assert_eq!(
            civil_from_epoch(epoch("2024-02-29T23:59:59Z")),
            (2024, 2, 29)
        );
        // Pre-epoch instants floor correctly (div_euclid).
        assert_eq!(
            civil_from_epoch(epoch("1969-12-31T00:00:00Z")),
            (1969, 12, 31)
        );
    }

    // --- AC 1 + AC 2: offline read, store is the SOLE data source -------------

    /// A counting fake: the ONLY way the pipeline can obtain data. That the whole report
    /// builds from it — with no other seam in scope — is the structural proof that the
    /// stats path makes no live socket / keychain / usage-API call.
    #[derive(Default)]
    struct FakeStore {
        samples: Vec<Sample>,
        rollup: Rollup,
        events: String,
        reads: std::cell::Cell<u32>,
    }
    impl HistoryStore for FakeStore {
        fn read_samples(&self) -> Result<Vec<Sample>> {
            self.reads.set(self.reads.get() + 1);
            Ok(self.samples.clone())
        }
        fn read_rollup(&self) -> Result<Rollup> {
            self.reads.set(self.reads.get() + 1);
            Ok(self.rollup.clone())
        }
        fn read_events(&self) -> Result<String> {
            self.reads.set(self.reads.get() + 1);
            Ok(self.events.clone())
        }
    }

    #[test]
    fn the_store_seam_is_the_only_data_source() {
        let fake = FakeStore {
            samples: vec![sample(500, "work", 0.9, 0.4)],
            events: "ts=1970-01-01T00:02:30Z event=swap from=work to=play reason=manual\n"
                .to_owned(),
            ..FakeStore::default()
        };
        let read = StoreData::read(&fake).unwrap();
        assert_eq!(fake.reads.get(), 3, "exactly one read of each store file");
        let window = Window {
            start: 0,
            end: 1_000,
            kind: WindowKind::Period(PeriodSpec::Day),
        };
        let report = build_report(&read, window, vec![], None, &params(), 0);
        assert_eq!(report.summary.per_account["work"].seen, 1);
        assert_eq!(report.summary.roster.swaps.manual, 1);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn native_store_reads_offline_from_files_with_no_daemon() {
        // The AC's "renders offline (daemon down)": write the store's own files into a
        // temp dir and read them directly — no socket, no keychain, no `/usage`. Runs on
        // the daemon's current-thread runtime under a paused clock (the pipeline is a pure
        // function of an injected `now`, so no wall-clock wait is needed).
        let dir = tempfile::tempdir().unwrap();
        let samples_path = dir.path().join("usage-samples.jsonl");
        let rollup_path = dir.path().join("usage-rollup.json");
        let events_path = dir.path().join("sessiometer.log");

        let now = epoch("2026-07-01T00:00:00Z");
        for (k, s) in [0.20, 0.85, 0.99].iter().enumerate() {
            usage_store::append_sample(
                &samples_path,
                &sample(now - (3 - k as i64) * HOUR_SECS, "work", *s, 0.30),
            )
            .unwrap();
        }
        std::fs::write(
            &events_path,
            "ts=2026-06-30T23:00:00Z event=swap from=play to=work reason=session\n",
        )
        .unwrap();

        let store = NativeHistoryStore {
            samples_path,
            rollup_path,
            events_path,
        };
        let read = StoreData::read(&store).unwrap();
        let window = plan_window(Some("day"), None, now, &read).unwrap();
        let report = build_report(&read, window, vec![], None, &params(), 0);

        assert_eq!(
            report.summary.per_account["work"].seen, 3,
            "read the 3 samples"
        );
        assert_eq!(
            report.summary.per_account["work"].cap_hits, 2,
            "0.85 and 0.99 are both ≥ the 0.80 cap"
        );
        assert_eq!(
            report.summary.roster.swaps.session, 1,
            "read the swap event"
        );
        // An absent rollup file is not an error — it reads as empty.
        assert!(read.rollup.daily.is_empty());
    }

    // --- AC 5: --json schema:1 stable + redacted ------------------------------

    fn report_fixture() -> Report {
        let now = epoch("2026-07-01T12:00:00Z");
        let samples = vec![
            sample(now - 2 * HOUR_SECS, "work", 0.9, 0.4),
            sample(now - HOUR_SECS, "work", 0.99, 0.45),
            sample(now - 2 * HOUR_SECS, "play", 0.2, 0.1),
        ];
        let events = "ts=2026-07-01T09:00:00Z event=swap from=play to=work reason=session\n\
             ts=2026-07-01T11:00:00Z event=emergency_swap from=work to=play\n";
        let store = data(samples, events);
        let window = plan_window(Some("day"), None, now, &store).unwrap();
        build_report(&store, window, vec![], None, &params(), 0)
    }

    #[test]
    fn json_is_schema_1_with_series_summary_and_window() {
        let json = render_json(&report_fixture()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["schema"], 1);
        assert_eq!(v["window"]["period"], "day");
        assert!(v["window"]["label"]
            .as_str()
            .unwrap()
            .starts_with("last 24h ("));
        assert!(v["series"].is_array(), "the full series is present");
        assert!(!v["series"].as_array().unwrap().is_empty());
        assert!(v["summary"]["accounts"]["work"].is_object());
        assert!(v["summary"]["roster"]["swap_count"].as_i64().unwrap() >= 1);
    }

    #[test]
    fn json_carries_neutral_descriptor_enums_and_no_recommendation() {
        let json = render_json(&report_fixture()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let work = &v["summary"]["accounts"]["work"];
        // `work` peaks at 0.99 (≥ 0.8, < 1.0) → the neutral `high` band, NOT a signal.
        assert_eq!(work["band"], "high");
        assert!(matches!(
            work["coverage_class"].as_str().unwrap(),
            "complete" | "partial" | "absent"
        ));
        // Scope boundary: no signal/recommendation field, no chart glyph in the wire.
        assert!(
            !json.contains("recommend"),
            "no recommendation field (issue #160)"
        );
        assert!(!json.contains("signal"), "no signal field (issue #160)");
        for glyph in ['█', '▇', '▆', '▅', '▄', '▃', '▂', '▁'] {
            assert!(!json.contains(glyph), "no chart glyph (issue #159)");
        }
    }

    #[test]
    fn json_handles_are_redacted_and_no_secret_leaks() {
        let json = render_json(&report_fixture()).unwrap();
        // Handle fixture (`work`/`play`): no authored labels, so an empty allow-set is
        // the strict bar — any `@`-shape would be UNAUTHORED and fail. Provenance
        // vocabulary rather than a blanket no-`@` (issue #15, relaxed by #444/#447 —
        // an operator-authored email label reaches `stats` via `Sample.acct`).
        assert!(
            crate::redaction::meter::unauthored_emails(&json, &[]).is_empty(),
            "no unauthored email may reach the wire: {json}"
        );
        assert!(!json.contains("sk-ant"), "no token may reach the wire");
    }

    #[test]
    fn json_permits_an_operator_authored_email_label() {
        // #447: `stats` reads the persisted store and keys `per_account` by
        // `Sample.acct` — the roster label, which may now be an operator-authored
        // email. That label surfaces verbatim as a JSON account key; it is PERMITTED
        // under the same provenance-scoped waiver as the store's write side (#444),
        // while a stray UNAUTHORED email would still fail.
        let now = epoch("2026-07-01T12:00:00Z");
        let authored = "alice@example.com";
        let store = data(
            vec![
                sample(now - 2 * HOUR_SECS, authored, 0.9, 0.4),
                sample(now - HOUR_SECS, authored, 0.99, 0.45),
            ],
            "",
        );
        let window = plan_window(Some("day"), None, now, &store).unwrap();
        let report = build_report(&store, window, vec![], None, &params(), 0);
        let json = render_json(&report).unwrap();

        // The authored email label IS the account key on the wire (runtime honesty)…
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            v["summary"]["accounts"][authored].is_object(),
            "the authored email label keys the account: {json}"
        );
        // …permitted WHEN authored…
        assert!(
            crate::redaction::meter::unauthored_emails(&json, &[authored]).is_empty(),
            "an operator-authored email label is permitted: {json}"
        );
        // …but the same bytes read as a leak WITHOUT the provenance allow-set (the
        // assertion is not vacuous — the label really does carry an `@`; it recurs
        // across the summary + series, so assert containment, not an exact count).
        assert!(
            crate::redaction::meter::unauthored_emails(&json, &[]).contains(&authored.to_owned()),
            "without provenance the label reads as an unauthored email: {json}"
        );
        assert!(!json.contains("sk-ant"), "no token may reach the wire");
    }

    #[test]
    fn json_account_object_has_exactly_the_intended_keys() {
        let json = render_json(&report_fixture()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let mut keys: Vec<&str> = v["summary"]["accounts"]["work"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "band",
                "cap_hits",
                "contribution_share",
                "coverage",
                "coverage_class",
                "seen",
                "session",
                "time_at_cap_secs",
                "weekly",
            ]
        );
    }

    // --- numeric text render + account filter ---------------------------------

    #[test]
    fn text_render_has_the_echo_a_table_and_a_roster_line_but_no_glyphs() {
        let out = render_text(&report_fixture());
        assert!(
            out.starts_with("usage — last 24h ("),
            "leads with the window echo"
        );
        assert!(out.contains("work"), "the per-account table lists work");
        assert!(out.contains("roster:"), "a roster line summarises swaps");
        assert!(out.contains("emergency"), "the swap breakdown is present");
        assert!(!out.contains("recommend"), "no recommendation (issue #160)");
        for glyph in ['█', '▇', '▁'] {
            assert!(!out.contains(glyph), "no chart glyph (issue #159)");
        }
    }

    #[test]
    fn empty_window_still_renders_an_echo_and_roster_line() {
        let now = 1_000_000;
        let report = build_report(
            &data(vec![], ""),
            plan_window(None, None, now, &data(vec![], "")).unwrap(),
            vec![],
            None,
            &params(),
            0,
        );
        let out = render_text(&report);
        assert!(out.contains("no per-account usage in this window"));
        assert!(out.contains("0 swaps"));
        // JSON of an empty window is still a valid schema:1 document.
        let v: serde_json::Value = serde_json::from_str(&render_json(&report).unwrap()).unwrap();
        assert_eq!(v["schema"], 1);
        assert!(v["summary"]["accounts"].as_object().unwrap().is_empty());
    }

    #[test]
    fn account_filter_restricts_display_but_not_roster_stats() {
        let now = epoch("2026-07-01T12:00:00Z");
        let store = data(
            vec![
                sample(now - HOUR_SECS, "work", 0.9, 0.4),
                sample(now - HOUR_SECS, "play", 0.2, 0.1),
            ],
            "ts=2026-07-01T11:30:00Z event=swap from=play to=work reason=manual\n",
        );
        let window = plan_window(Some("day"), None, now, &store).unwrap();
        let report = build_report(&store, window, vec!["work".to_owned()], None, &params(), 0);
        assert!(report.summary.per_account.contains_key("work"));
        assert!(
            !report.summary.per_account.contains_key("play"),
            "the filter hides play from the per-account view"
        );
        assert_eq!(
            report.summary.roster.swap_count, 1,
            "roster stays roster-wide despite the filter"
        );
    }

    // --- bucketing ------------------------------------------------------------

    #[test]
    fn buckets_partition_the_window_and_stay_bounded() {
        // A day at hourly resolution → 24 buckets, contiguous, covering [0, day).
        let bounds = bucket_bounds(0, DAY_SECS, HOUR_SECS);
        assert_eq!(bounds.len(), 24);
        assert_eq!(bounds.first().copied(), Some((0, HOUR_SECS)));
        assert_eq!(bounds.last().copied(), Some((23 * HOUR_SECS, DAY_SECS)));
        for pair in bounds.windows(2) {
            assert_eq!(pair[0].1, pair[1].0, "abutting, no gap or overlap");
        }
        // A very long window is widened, never split past the cap.
        let long = bucket_bounds(0, 5 * MAX_BUCKETS * DAY_SECS, DAY_SECS);
        assert!(long.len() as i64 <= MAX_BUCKETS, "bounded to MAX_BUCKETS");
        // An empty/inverted window yields nothing.
        assert!(bucket_bounds(100, 100, HOUR_SECS).is_empty());
        assert!(bucket_bounds(100, 50, HOUR_SECS).is_empty());
    }

    #[test]
    fn fmt_dur_is_coarse_and_never_negative() {
        assert_eq!(fmt_dur(0), "0s");
        assert_eq!(fmt_dur(-5), "0s");
        assert_eq!(fmt_dur(45), "45s");
        assert_eq!(fmt_dur(15 * 60), "15m");
        assert_eq!(fmt_dur(2 * HOUR_SECS), "2h");
        assert_eq!(fmt_dur(2 * HOUR_SECS + 30 * 60), "2h30m");
    }

    #[test]
    fn band_thresholds_are_neutral_and_inclusive_at_the_low_edge() {
        let name = |peak: f64| serde_json::to_value(Band::of(peak)).unwrap();
        assert_eq!(name(0.0), "idle");
        assert_eq!(name(0.19), "idle");
        assert_eq!(name(0.20), "low");
        assert_eq!(name(0.50), "moderate");
        assert_eq!(name(0.80), "high");
        assert_eq!(name(1.00), "at_cap");
        assert_eq!(
            name(1.50),
            "at_cap",
            "over-cap readings are reported, not clamped"
        );
    }

    // --- issue #160: neutral summary band + framing guard =============================

    /// Three accounts spanning the whole symmetric signal: `aa` under the balanced middle
    /// (peak 0.15 → underused), `bb` in it (peak 0.60 → balanced), `cc` above it (peak 0.95
    /// → saturated). `aa` also has the lowest session mean, so it is the lowest-utilisation
    /// callout. Sorted handles make the render deterministic.
    fn three_band_report() -> Report {
        charts_report(
            &[
                ("aa", stat(3, ds(0.10, 0.15, 0.12), 0.0, 0.30)),
                ("bb", stat(3, ds(0.45, 0.60, 0.55), 0.0, 0.30)),
                ("cc", stat(3, ds(0.70, 0.95, 0.90), 0.0, 0.40)),
            ],
            &[],
        )
    }

    /// A minimal, fully-deterministic report exercising every `--json` wire field once —
    /// one account (band `high`, coverage `complete`), one series bucket, a session swap,
    /// and a UTC (offset 0) `day` window. Small enough to freeze byte-for-byte.
    fn wire_golden_report() -> Report {
        let acct = AccountStats {
            seen: 3,
            expected: 3.0,
            coverage: 1.0,
            session: ds(0.50, 0.90, 0.85),
            weekly: ds(0.30, 0.40, 0.38),
            cap_hits: 1,
            time_at_cap_secs: 300,
            contribution_share: 1.0,
        };
        let roster = RosterStats {
            swap_count: 1,
            swaps: crate::usage_stats::SwapBreakdown {
                session: 1,
                ..Default::default()
            },
            all_high_episodes: 0,
            all_high_secs: 0,
        };
        let bucket = |start, end| UsageReport {
            period: Period::new(start, end),
            per_account: [("work".to_string(), acct)].into_iter().collect(),
            roster,
        };
        Report {
            window: Window {
                start: epoch("2026-07-01T00:00:00Z"),
                end: epoch("2026-07-01T12:00:00Z"),
                kind: WindowKind::Period(PeriodSpec::Day),
            },
            accounts: vec![],
            summary: bucket(0, 6 * HOUR_SECS),
            series: vec![bucket(0, 6 * HOUR_SECS)],
            offset: 0,
            orphans: BTreeMap::new(),
            velocity: BTreeMap::new(),
        }
    }

    // --- the framing guard: a CENTRAL banned vocabulary + its scanner ----------------

    /// The editorialising vocabulary the neutral summary band (issue #160) — and every
    /// surface this guard scans — must NEVER contain: a value judgement (`healthy`, `danger`),
    /// an acquisitive imperative (`add`, `upgrade`, `buy`), a recommendation (`should`,
    /// `recommend`), or ALARMIST projection FRAMING (`forecast`, `imminent`, `soon`). CENTRAL +
    /// explicit so the guard stays maintainable: one list, one scanner, extended in a single
    /// place.
    ///
    /// Boundary (issue #542, ADR-0020) — these ban the FRAMING, not the FACT. A neutrally
    /// framed velocity + runway readout — a `%/min` rate, an approximate time-to-trigger or
    /// days-of-runway phrased as an observation (`~4h to trigger`, `~3 days at current rate`) —
    /// is PERMITTED: it uses none of this vocabulary. What stays banned is the acquisitive CALL
    /// (a purchase prompt) and the alarmist projection words, never a head-room number. Neutral
    /// MAGNITUDE words the wire legitimately uses (`idle`/`low`/`moderate`/`high`/`at_cap`) are
    /// likewise absent — they describe, they do not editorialise.
    const BANNED_TOKENS: &[&str] = &[
        // Imperatives / recommended actions (issue #160: "add / buy / upgrade / cancel /
        // bypass / need more").
        "add",
        "buy",
        "upgrade",
        "cancel",
        "bypass",
        "need",
        "purchase",
        "remove",
        "disable",
        "enable",
        "fix",
        "avoid",
        "reduce",
        "increase",
        "throttle",
        "rotate",
        // Value judgements (caller: "healthy / at risk / warning / danger / good / bad").
        "healthy",
        "unhealthy",
        "risk",
        "risky",
        "warning",
        "warn",
        "danger",
        "dangerous",
        "good",
        "bad",
        "critical",
        "severe",
        "poor",
        "safe",
        "unsafe",
        "optimal",
        // Recommendation framing (caller: "you should").
        "should",
        "must",
        "ought",
        "recommend",
        "recommended",
        "recommendation",
        "suggest",
        "suggestion",
        "consider",
        "advise",
        "advice",
        // Alarmist / editorialising projection FRAMING. A neutral numeric runway is a
        // permitted FACT (issue #542, ADR-0020); these ban the ALARM ("forecast", "imminent",
        // "soon"), not the head-room number ("~4h to trigger").
        "forecast",
        "predict",
        "prediction",
        "projected",
        "projection",
        "anticipate",
        "imminent",
        "soon",
    ];

    /// Acquisitive purchase-CALLS that span two adjacent words, so the single-token scan above
    /// misses them (issue #542): the imperative-free `top up` / `get more` a purchase prompt
    /// reaches for once `buy`/`add`/`upgrade` are gone. The discriminator the guard draws is the
    /// CALL to acquire, never the head-room fact — `runs out in ~4h` is permitted, `runs out —
    /// top up` is not. Kept SHORT and matched on WORD boundaries (adjacent tokens, not a raw
    /// substring) so a neutral render never false-trips (`laptop update` is not `top up`).
    const BANNED_PHRASES: &[&str] = &["top up", "get more"];

    /// The first banned token OR acquisitive phrase appearing in `text`, or `None` when it is
    /// clean. Strips ANSI SGR runs first (so a colour-wrapped word tokenises intact), then
    /// matches whole lowercase WORDS on non-alphanumeric boundaries — so `at-risk`, `At Risk`,
    /// and `risk!` all trip `risk`, while `saturated` or an account handle never false-trips —
    /// and finally adjacent-word purchase-calls (`top up`), so a neutral head-room fact passes
    /// while an acquisitive call does not (issue #542).
    fn scan_banned(text: &str) -> Option<&'static str> {
        let mut plain = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Drop the SGR sequence up to and including its `m` terminator.
                for c2 in chars.by_ref() {
                    if c2 == 'm' {
                        break;
                    }
                }
            } else {
                plain.push(c);
            }
        }
        // Lowercase words in READING ORDER (a Vec, not a set) — the order lets the phrase scan
        // below match an adjacent-word purchase-call without a fragile substring test.
        let words: Vec<String> = plain
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();
        // A single editorialising / acquisitive WORD (issue #160).
        if let Some(hit) = BANNED_TOKENS
            .iter()
            .copied()
            .find(|b| words.iter().any(|w| w == b))
        {
            return Some(hit);
        }
        // A purchase-CALL spanning adjacent words (issue #542): `top up` / `get more`.
        BANNED_PHRASES.iter().copied().find(|phrase| {
            let parts: Vec<&str> = phrase.split(' ').collect();
            words
                .windows(parts.len())
                .any(|win| win.iter().zip(&parts).all(|(w, p)| w.as_str() == *p))
        })
    }

    /// Every object key in `v`, recursively — the surface the `--json` banned-token scan
    /// covers (the wire's VALUES are numbers and neutral descriptor enums; the KEYS are the
    /// authored field names).
    fn json_keys(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, child) in map {
                    out.push(k.clone());
                    json_keys(child, out);
                }
            }
            serde_json::Value::Array(arr) => arr.iter().for_each(|e| json_keys(e, out)),
            _ => {}
        }
    }

    // --- AC: symmetric emphasis, facts only, deterministic render --------------------

    #[test]
    fn summary_band_is_neutral_symmetric_and_deterministic() {
        // The whole band, frozen: a per-account signal line (underused · balanced ·
        // saturated) then the lowest-utilisation callout — MAGNITUDES and neutral
        // descriptors only, no imperative, no forecast, no verdict.
        assert_eq!(
            render_summary(&three_band_report(), false),
            "signal  aa underused · bb balanced · cc saturated\n        lowest utilisation: aa (session mean 10%)\n",
        );
    }

    #[test]
    fn summary_band_gives_underuse_and_saturation_identical_emphasis() {
        // AC 1 — symmetric emphasis. At the vocabulary level the two DEVIATIONS share one
        // urgency-colour code (identical visual weight) while the balanced middle is
        // un-emphasised: underuse is not "green for good", saturation not "red for alarm".
        assert_eq!(
            SignalBand::Underused.sgr(),
            SignalBand::Saturated.sgr(),
            "underuse and saturation carry the SAME emphasis"
        );
        assert!(
            !SignalBand::Underused.sgr().is_empty(),
            "the deviations are emphasised"
        );
        assert!(SignalBand::Balanced.sgr().is_empty(), "the middle is not");

        // And in the rendered band: both deviation words are wrapped in the identical SGR,
        // the middle word is plain — proof the colour half is symmetric too.
        let colored = render_summary(&three_band_report(), true);
        assert!(colored.contains("aa \x1b[33munderused\x1b[0m"));
        assert!(colored.contains("cc \x1b[33msaturated\x1b[0m"));
        assert!(
            colored.contains("· bb balanced ·"),
            "balanced is not colour-wrapped"
        );
    }

    #[test]
    fn signal_band_collapses_the_wire_band_symmetrically() {
        // The summary signal is a symmetric collapse of the #159 wire `band`: both the
        // idle/low floor and the high/at_cap ceiling become single-word deviations flanking
        // the balanced middle, keyed on the SAME thresholds (so the two never disagree).
        for peak in [0.0, 0.19, 0.20, 0.49] {
            assert_eq!(SignalBand::of(peak), SignalBand::Underused, "peak {peak}");
        }
        for peak in [0.50, 0.79] {
            assert_eq!(SignalBand::of(peak), SignalBand::Balanced, "peak {peak}");
        }
        for peak in [0.80, 0.99, 1.00, 1.50] {
            assert_eq!(SignalBand::of(peak), SignalBand::Saturated, "peak {peak}");
        }
    }

    #[test]
    fn summary_band_shows_in_both_human_views_but_never_on_the_json_wire() {
        // Human surfaces (numeric text + charts) both foot with the band.
        let text = render_text(&report_fixture());
        assert!(
            text.contains("signal  "),
            "the numeric text carries the band"
        );
        assert!(text.contains("lowest utilisation:"));
        let charts = render_charts(&two_account_charts(), 60, false, false);
        assert!(
            charts.contains("signal  "),
            "the charts view carries the band"
        );

        // The band is HUMAN-only — none of its vocabulary reaches the schema:1 wire (which
        // keeps the finer per-account `band`/`coverage_class` enums, byte-stable vs #159).
        let json = render_json(&report_fixture()).unwrap();
        for token in [
            "signal",
            "underused",
            "balanced",
            "saturated",
            "lowest",
            "utilisation",
        ] {
            assert!(
                !json.contains(token),
                "the summary band stays off the json wire: `{token}`"
            );
        }
    }

    // --- AC: the framing guard passes on the real render, bites on injection ---------

    #[test]
    fn summary_render_carries_no_banned_token_but_the_guard_bites_on_injection() {
        // The guard PASSES on every real render — multi-account, single, all-gap — across
        // both human surfaces AND with the colour overlay on (issue #160: facts only).
        let three = three_band_report();
        let single = charts_report(&[("solo", stat(1, ds(0.5, 0.5, 0.5), 0.0, 1.0))], &[]);
        let all_gap = charts_report(&[("ghost", stat(1, ds(0.0, 0.0, 0.0), 0.0, 0.0))], &[]);
        for report in [&three, &single, &all_gap] {
            for surface in [
                render_summary(report, false),
                render_summary(report, true),
                render_text(report),
                render_charts(report, 80, true, false),
            ] {
                assert_eq!(
                    scan_banned(&surface),
                    None,
                    "a real render must contain no banned token: {surface:?}"
                );
            }
        }

        // The `--json` KEYS are neutral too (the wire carries descriptor enums, no verb).
        let json = render_json(&report_fixture()).unwrap();
        let mut keys = Vec::new();
        json_keys(&serde_json::from_str(&json).unwrap(), &mut keys);
        assert_eq!(scan_banned(&keys.join(" ")), None, "json keys are neutral");

        // The guard BITES: inject a banned word into a real render and it is caught — proof
        // the test would FAIL if editorialising copy ever slipped into the band.
        let poisoned = render_summary(&three, false).replace("balanced", "upgrade");
        assert_eq!(
            scan_banned(&poisoned),
            Some("upgrade"),
            "injection is caught"
        );
        // Case-insensitive + word-boundary: a capitalised, punctuation-hugged word trips.
        assert_eq!(scan_banned("period — you SHOULD."), Some("should"));
        // The scanner does not over-trip on the neutral descriptor vocabulary itself.
        assert_eq!(
            scan_banned("signal aa underused bb balanced cc saturated"),
            None
        );
    }

    // --- AC (issue #542): PERMIT a neutral runway, still BAN the acquisitive call ----

    #[test]
    fn framing_guard_permits_neutral_runway_but_bans_the_acquisitive_call() {
        // PERMIT — a neutrally framed velocity + runway readout is descriptive head-room, not
        // advice: a `%/min` rate, an approximate time-to-trigger, days-of-runway "at current
        // rate", and the bare "runs out in ~Xh" fact all read as an observation and pass clean.
        // (Unblocks issue #541's per-account + fleet runway surfaces, issues #543 / #544, which
        // can render these without tripping the guard.)
        for permitted in [
            "runway  work ~4h to trigger · 1.4%/min",
            "runway  fleet ~3 days at current rate",
            "velocity  work 0.8%/min · weekly 0.20%/min",
            "work runs out in ~4h at current rate",
            "~12h to trigger · ~5 days of runway",
        ] {
            assert_eq!(
                scan_banned(permitted),
                None,
                "a neutral velocity/runway readout is permitted: {permitted:?}"
            );
        }

        // BAN — the acquisitive / purchase-timeline framing stays caught: a call to acquire,
        // whether a single imperative ("buy" / "add" / "upgrade") OR an imperative-free purchase
        // phrase ("top up" / "get more"). The intent-leak concern is the PURCHASE PROMPT, never
        // the head-room number.
        for (acquisitive, caught) in [
            ("running low — top up / buy more", "buy"),
            ("you'll run out — top up", "top up"),
            ("add credits before you run out", "add"),
            ("get more before it resets", "get more"),
            ("almost out — upgrade to keep going", "upgrade"),
        ] {
            assert_eq!(
                scan_banned(acquisitive),
                Some(caught),
                "an acquisitive purchase-prompt still fails the guard: {acquisitive:?}"
            );
        }

        // The boundary is the CALL, not the fact: the SAME "runs out" head-room passes as a
        // neutral observation, and fails the instant a purchase call is appended.
        assert_eq!(scan_banned("work runs out in ~4h"), None);
        assert_eq!(scan_banned("work runs out in ~4h — top up"), Some("top up"));
    }

    // --- AC (issue #543): per-account velocity + runway readout (summary + --json) ----

    #[test]
    fn known_velocity_yields_the_expected_rate_and_runway_in_both_views() {
        // Three readings 300 s apart, session climbing a steady +0.01/interval → a constant instant
        // rate the EMA reproduces exactly: 0.01/300 frac/s = 0.2 %/min. From the last reading 0.52
        // toward the 0.80 session trigger, head-room 0.28 → 0.28 ÷ (0.01/300) = 8400 s ≈ ~2h. The
        // weekly dimension is FLAT (a known ZERO rate), so its runway is unknown — an explicit null.
        let now = epoch("2026-07-01T12:00:00Z");
        let report = velocity_report(
            vec![
                sample(now - 900, "work", 0.50, 0.30),
                sample(now - 600, "work", 0.51, 0.30),
                sample(now - 300, "work", 0.52, 0.30),
            ],
            now,
        );

        // Human: the numeric text foots with a neutral velocity + runway line — facts, no advice.
        let text = render_text(&report);
        assert!(
            text.contains("velocity  work 0.2%/min"),
            "velocity line: {text}"
        );
        assert!(
            text.contains("runway  work ~2h to trigger"),
            "runway line: {text}"
        );
        assert_eq!(scan_banned(&text), None, "the real render is neutral");

        // Wire: the velocity object carries %/min + whole-second runway; the flat weekly is a
        // KNOWN 0.0 rate with an EXPLICIT null runway (honest degradation, never a sentinel).
        let v: serde_json::Value = serde_json::from_str(&render_json(&report).unwrap()).unwrap();
        let vel = &v["summary"]["accounts"]["work"]["velocity"];
        assert!((vel["session_pct_per_min"].as_f64().unwrap() - 0.2).abs() < 1e-9);
        assert_eq!(vel["session_runway_secs"].as_i64().unwrap(), 8400);
        assert_eq!(vel["weekly_pct_per_min"].as_f64().unwrap(), 0.0);
        assert!(
            vel["weekly_runway_secs"].is_null(),
            "a flat weekly is a null runway, not a 0 / 999 sentinel: {vel}"
        );
    }

    #[test]
    fn weekly_runway_renders_in_days_when_it_is_meaningful() {
        // Session climbs (as above → ~2h), weekly climbs slowly +0.001/interval → 0.001/300 frac/s;
        // from 0.302 toward the 0.95 weekly trigger, head-room 0.648 → 0.648 ÷ (0.001/300) = 194 400
        // s ≈ 2.25 d → "~2 days". Proves the weekly head-room renders on its natural day scale.
        let now = epoch("2026-07-01T12:00:00Z");
        let report = velocity_report(
            vec![
                sample(now - 900, "work", 0.50, 0.300),
                sample(now - 600, "work", 0.51, 0.301),
                sample(now - 300, "work", 0.52, 0.302),
            ],
            now,
        );
        let text = render_text(&report);
        assert!(
            text.contains("runway  work ~2h to trigger, weekly ~2 days"),
            "session hours + weekly days on one entry: {text}"
        );
        assert_eq!(scan_banned(&text), None, "the days render is neutral");

        let v: serde_json::Value = serde_json::from_str(&render_json(&report).unwrap()).unwrap();
        let secs = v["summary"]["accounts"]["work"]["velocity"]["weekly_runway_secs"]
            .as_i64()
            .unwrap();
        assert!(
            (secs - 194_400).abs() <= 2,
            "weekly runway ≈ 194 400 s: {secs}"
        );
    }

    #[test]
    fn zero_velocity_reports_a_known_rate_but_an_unknown_runway() {
        // A flat account (three identical readings) has a KNOWN velocity of 0.0 %/min but NO finite
        // runway — the AC's "zero velocity → runway unknown". The wire carries 0.0 with an explicit
        // null runway; the human pairs "0.0%/min" with a "—".
        let now = epoch("2026-07-01T12:00:00Z");
        let report = velocity_report(
            vec![
                sample(now - 900, "work", 0.60, 0.30),
                sample(now - 600, "work", 0.60, 0.30),
                sample(now - 300, "work", 0.60, 0.30),
            ],
            now,
        );
        let text = render_text(&report);
        assert!(
            text.contains("velocity  work 0.0%/min"),
            "known zero rate: {text}"
        );
        assert!(
            text.contains("runway  work —"),
            "unknown runway shown as —: {text}"
        );

        let v: serde_json::Value = serde_json::from_str(&render_json(&report).unwrap()).unwrap();
        let vel = &v["summary"]["accounts"]["work"]["velocity"];
        assert_eq!(vel["session_pct_per_min"].as_f64().unwrap(), 0.0);
        assert!(
            vel["session_runway_secs"].is_null(),
            "zero velocity → null runway, not a sentinel: {vel}"
        );
    }

    #[test]
    fn too_few_samples_leave_the_velocity_unknown_and_the_wire_field_absent() {
        // A single reading cannot form even one interval → the velocity is unknown. The human band
        // carries no velocity line, and the wire OMITS the velocity object (an absent field, the
        // AC's permitted "null / absent" — never a fabricated rate).
        let now = epoch("2026-07-01T12:00:00Z");
        let report = velocity_report(vec![sample(now - 300, "work", 0.60, 0.30)], now);
        let text = render_text(&report);
        assert!(
            text.contains("work"),
            "the account still appears in the table"
        );
        assert!(
            !text.contains("velocity  "),
            "no velocity line without a rate: {text}"
        );

        let v: serde_json::Value = serde_json::from_str(&render_json(&report).unwrap()).unwrap();
        let work = &v["summary"]["accounts"]["work"];
        assert_eq!(
            work["seen"].as_i64().unwrap(),
            1,
            "the reading was still counted"
        );
        assert!(
            work.get("velocity").is_none(),
            "an unknown velocity omits the wire object: {work}"
        );
    }

    #[test]
    fn a_stale_last_reading_leaves_the_velocity_unknown() {
        // Three climbing readings that WOULD yield a velocity (cf. the known-velocity test) but whose
        // latest is far older than the aggregator's forward-coverage horizon (300 s) before now — the
        // daemon stopped polling / an idle window. No CURRENT velocity → unknown, though the readings
        // are still aggregated (seen == 3). Isolates STALENESS from insufficiency.
        let now = epoch("2026-07-01T12:00:00Z");
        let report = velocity_report(
            vec![
                sample(now - 7800, "work", 0.50, 0.30),
                sample(now - 7500, "work", 0.51, 0.30),
                sample(now - 7200, "work", 0.52, 0.30),
            ],
            now,
        );
        let text = render_text(&report);
        assert!(
            !text.contains("velocity  "),
            "a stale reading shows no velocity: {text}"
        );

        let v: serde_json::Value = serde_json::from_str(&render_json(&report).unwrap()).unwrap();
        let work = &v["summary"]["accounts"]["work"];
        assert_eq!(
            work["seen"].as_i64().unwrap(),
            3,
            "the readings were still counted"
        );
        assert!(
            work.get("velocity").is_none(),
            "a stale velocity omits the wire object: {work}"
        );
    }

    #[test]
    fn the_velocity_readout_is_neutral_across_every_surface_and_the_wire_keys() {
        // The #542 guard AC for the LIVE readout: a mixed roster — one account climbing (session +
        // weekly runway), one flat (zero rate), one under-sampled (unknown) — rendered on BOTH human
        // surfaces, with and without colour, contains no banned vocabulary; and the `--json` keys the
        // readout adds are neutral too. This is what unblocks #543 on top of #542.
        let now = epoch("2026-07-01T12:00:00Z");
        let report = velocity_report(
            vec![
                sample(now - 900, "work", 0.50, 0.300),
                sample(now - 600, "work", 0.51, 0.301),
                sample(now - 300, "work", 0.52, 0.302),
                sample(now - 900, "home", 0.40, 0.20),
                sample(now - 600, "home", 0.40, 0.20),
                sample(now - 300, "home", 0.40, 0.20),
                sample(now - 300, "spare", 0.10, 0.05),
            ],
            now,
        );
        for surface in [
            render_summary(&report, false),
            render_summary(&report, true),
            render_text(&report),
            render_charts(&report, 80, true, false),
        ] {
            assert!(
                surface.contains("velocity  "),
                "the readout is present on the surface under test: {surface:?}"
            );
            assert_eq!(
                scan_banned(&surface),
                None,
                "the velocity + runway readout must contain no banned token: {surface:?}"
            );
        }

        // The `--json` keys the readout adds are neutral (the wire carries figures, no verb).
        let json = render_json(&report).unwrap();
        let mut wire_keys = Vec::new();
        json_keys(&serde_json::from_str(&json).unwrap(), &mut wire_keys);
        assert!(
            wire_keys.iter().any(|k| k == "velocity"),
            "the velocity object reached the wire"
        );
        assert_eq!(
            scan_banned(&wire_keys.join(" ")),
            None,
            "the velocity wire keys are neutral (issue #542 guard)"
        );
    }

    #[test]
    fn velocity_and_runway_formatters_are_approximate_and_scale_aware() {
        // The rate scales fraction/second → %/min; runways round to the coarsest non-zero unit with
        // an explicit `~`, hours for the session scale and days for the weekly scale.
        assert_eq!(fmt_pct_per_min(0.01 / 300.0), "0.2%/min");
        assert_eq!(fmt_pct_per_min(0.0), "0.0%/min");
        assert_eq!(fmt_runway_hours(8400), "~2h");
        assert_eq!(fmt_runway_hours(1200), "~20m");
        assert_eq!(fmt_runway_hours(3585), "~1h"); // 59.75 m rounds up → promoted to ~1h, not a boundary ~60m
        assert_eq!(fmt_runway_days(432_000), "~5 days");
        assert_eq!(fmt_runway_days(86_400), "~1 day");
        assert_eq!(fmt_runway_days(18_000), "~5h"); // under a day falls back to hours
        assert_eq!(fmt_runway(AccountVelocity::default()), "—"); // all-unknown → em dash
    }

    // --- AC (issue #544): fleet/roster runway aggregate ("accounts last ~X days") ------

    #[test]
    fn fleet_runway_pools_weekly_headroom_and_surfaces_the_counted_cardinality() {
        // A three-account roster: `work` and `home` climb their weekly dimension at a steady, KNOWN
        // rate; `stale` climbs too but its latest reading is far older than the coverage horizon, so
        // it has no CURRENT velocity and is EXCLUDED. The fleet pools the counted accounts' weekly
        // head-room over their combined burn (the design choice, settled in `fleet_runway`):
        //   work: last weekly 0.302 → head-room 0.95 − 0.302 = 0.648, rate 0.001/300 frac/s
        //   home: last weekly 0.502 → head-room 0.95 − 0.502 = 0.448, rate 0.001/300 frac/s
        //   Σ head-room 1.096 ÷ Σ rate (0.002/300) = 164 400 s ≈ 1.9 d → "~2 days".
        let now = epoch("2026-07-01T12:00:00Z");
        let report = velocity_report(
            vec![
                sample(now - 900, "work", 0.50, 0.300),
                sample(now - 600, "work", 0.51, 0.301),
                sample(now - 300, "work", 0.52, 0.302),
                sample(now - 900, "home", 0.40, 0.500),
                sample(now - 600, "home", 0.41, 0.501),
                sample(now - 300, "home", 0.42, 0.502),
                // `stale`: three climbing readings whose latest is > the stale horizon before `now`.
                sample(now - 7800, "stale", 0.50, 0.30),
                sample(now - 7500, "stale", 0.51, 0.31),
                sample(now - 7200, "stale", 0.52, 0.32),
            ],
            now,
        );

        // The pure aggregate: 2 of 3 counted (stale excluded), pooled runway ≈ 164 400 s.
        let fleet = fleet_runway(&report).expect("a countable fleet");
        assert_eq!(
            (fleet.counted, fleet.observed),
            (2, 3),
            "stale is observed but not counted"
        );
        let secs = fleet.runway_secs.expect("a finite pooled runway");
        assert!(
            (secs - 164_400).abs() <= 2,
            "pooled runway ≈ 164 400 s: {secs}"
        );

        // Human: the band foots with ONE approximate, neutral fleet figure + the n-of-m cardinality.
        let text = render_text(&report);
        assert!(
            text.contains(
                "fleet  accounts last ~2 days at the current combined rate (2 of 3 counted)"
            ),
            "fleet line: {text}"
        );
        assert_eq!(
            scan_banned(&text),
            None,
            "the fleet render is neutral (issue #542 guard)"
        );

        // Wire: a `fleet` object on the SUMMARY, carrying the whole-second runway + the cardinality.
        let v: serde_json::Value = serde_json::from_str(&render_json(&report).unwrap()).unwrap();
        let fleet_obj = &v["summary"]["fleet"];
        assert!(
            (fleet_obj["runway_secs"].as_i64().unwrap() - 164_400).abs() <= 2,
            "{fleet_obj}"
        );
        assert_eq!(fleet_obj["counted"].as_i64().unwrap(), 2);
        assert_eq!(fleet_obj["observed"].as_i64().unwrap(), 3);

        // The `--json` keys the aggregate adds are neutral too (facts only, no verb — #542 guard).
        let mut keys = Vec::new();
        json_keys(&v, &mut keys);
        assert!(
            keys.iter().any(|k| k == "fleet"),
            "the fleet object reached the wire"
        );
        assert_eq!(
            scan_banned(&keys.join(" ")),
            None,
            "the fleet wire keys are neutral (issue #542 guard)"
        );
    }

    #[test]
    fn fleet_runway_excludes_a_stale_account_instead_of_zero_burning_it() {
        // Honest degradation (the load-bearing AC): an unknown / stale account is dropped ENTIRELY —
        // neither its head-room nor its burn enters. Proven by INVARIANCE: adding a stale account to a
        // healthy roster leaves the pooled runway UNCHANGED (only the `observed` denominator grows).
        // Zero-burning it instead — adding its head-room with no burn — would INFLATE the runway.
        let now = epoch("2026-07-01T12:00:00Z");
        let two = vec![
            sample(now - 900, "work", 0.50, 0.300),
            sample(now - 600, "work", 0.51, 0.301),
            sample(now - 300, "work", 0.52, 0.302),
            sample(now - 900, "home", 0.40, 0.500),
            sample(now - 600, "home", 0.41, 0.501),
            sample(now - 300, "home", 0.42, 0.502),
        ];
        let mut with_stale = two.clone();
        with_stale.extend([
            // A LARGE-head-room stale account (weekly ~0.10) — if it were zero-burned, its ~0.85
            // head-room would balloon the numerator and stretch the runway well past the true value.
            sample(now - 7800, "stale", 0.50, 0.08),
            sample(now - 7500, "stale", 0.51, 0.09),
            sample(now - 7200, "stale", 0.52, 0.10),
        ]);

        let clean = fleet_runway(&velocity_report(two, now)).expect("countable");
        let mixed = fleet_runway(&velocity_report(with_stale, now)).expect("countable");
        assert_eq!(
            clean.runway_secs, mixed.runway_secs,
            "a stale account must not change the pooled runway (excluded, not zero-burned)"
        );
        assert_eq!((clean.counted, clean.observed), (2, 2));
        assert_eq!(
            (mixed.counted, mixed.observed),
            (2, 3),
            "the stale account is surfaced in `observed` (m) but not `counted` (n)"
        );
    }

    #[test]
    fn fleet_runway_degrades_honestly_without_a_finite_pool_or_an_overlay() {
        let now = epoch("2026-07-01T12:00:00Z");

        // (a) Every counted account is FLAT (a known ZERO burn) → no combined drain → the runway is an
        // explicit unknown, but the cardinality is still surfaced (counted > 0). The human omits the
        // line (no figure to state); the wire carries the object with a `null` runway (never a
        // sentinel), so a machine reader still learns "2 accounts, no measurable burn".
        let flat = velocity_report(
            vec![
                sample(now - 900, "work", 0.60, 0.60),
                sample(now - 600, "work", 0.60, 0.60),
                sample(now - 300, "work", 0.60, 0.60),
                sample(now - 900, "home", 0.40, 0.40),
                sample(now - 600, "home", 0.40, 0.40),
                sample(now - 300, "home", 0.40, 0.40),
            ],
            now,
        );
        let fleet = fleet_runway(&flat).expect("counted-but-not-burning is still a fleet");
        assert_eq!((fleet.counted, fleet.observed), (2, 2));
        assert!(
            fleet.runway_secs.is_none(),
            "no combined burn → unknown runway"
        );
        assert!(
            !render_text(&flat).contains("fleet  "),
            "no fleet line without a figure"
        );
        let v: serde_json::Value = serde_json::from_str(&render_json(&flat).unwrap()).unwrap();
        assert!(
            v["summary"]["fleet"]["runway_secs"].is_null(),
            "an unknown fleet runway is an explicit null, not a sentinel: {}",
            v["summary"]["fleet"]
        );
        assert_eq!(v["summary"]["fleet"]["counted"].as_i64().unwrap(), 2);
        assert_eq!(
            scan_banned(&render_json(&flat).unwrap()),
            None,
            "the null-runway fleet object is neutral too"
        );

        // (b) Every account is UNDER-SAMPLED (a single reading → no interval → no velocity) → NOTHING
        // is countable → no fleet at all, on either surface (the wire OMITS the object).
        let thin = velocity_report(
            vec![
                sample(now - 300, "work", 0.60, 0.30),
                sample(now - 300, "home", 0.40, 0.20),
            ],
            now,
        );
        assert!(
            fleet_runway(&thin).is_none(),
            "nothing countable → no fleet"
        );
        assert!(!render_text(&thin).contains("fleet  "));
        let v2: serde_json::Value = serde_json::from_str(&render_json(&thin).unwrap()).unwrap();
        assert!(
            v2["summary"].get("fleet").is_none(),
            "the wire omits an empty fleet: {}",
            v2["summary"]
        );
    }

    // --- AC: --json schema:1 stays byte-stable vs #158/#159 --------------------------

    /// The frozen schema:1 wire. #160 is HUMAN-render only — it adds no field, no
    /// recommendation, no glyph — so this is the #158/#159 contract verbatim.
    const WIRE_GOLDEN: &str = r#"{
  "schema": 1,
  "window": {
    "start": 1782864000,
    "end": 1782907200,
    "label": "last 24h (Jul 1–Jul 1)",
    "period": "day"
  },
  "accounts": [],
  "series": [
    {
      "start": 0,
      "end": 21600,
      "roster": {
        "swap_count": 1,
        "swaps": {
          "session": 1,
          "weekly": 0,
          "manual": 0,
          "forced": 0,
          "emergency": 0
        },
        "all_high_episodes": 0,
        "all_high_secs": 0
      },
      "accounts": {
        "work": {
          "seen": 3,
          "coverage": 1.0,
          "coverage_class": "complete",
          "session": {
            "mean": 0.5,
            "peak": 0.9,
            "p95": 0.85
          },
          "weekly": {
            "mean": 0.3,
            "peak": 0.4,
            "p95": 0.38
          },
          "cap_hits": 1,
          "time_at_cap_secs": 300,
          "contribution_share": 1.0,
          "band": "high"
        }
      }
    }
  ],
  "summary": {
    "roster": {
      "swap_count": 1,
      "swaps": {
        "session": 1,
        "weekly": 0,
        "manual": 0,
        "forced": 0,
        "emergency": 0
      },
      "all_high_episodes": 0,
      "all_high_secs": 0
    },
    "accounts": {
      "work": {
        "seen": 3,
        "coverage": 1.0,
        "coverage_class": "complete",
        "session": {
          "mean": 0.5,
          "peak": 0.9,
          "p95": 0.85
        },
        "weekly": {
          "mean": 0.3,
          "peak": 0.4,
          "p95": 0.38
        },
        "cap_hits": 1,
        "time_at_cap_secs": 300,
        "contribution_share": 1.0,
        "band": "high"
      }
    }
  }
}
"#;

    #[test]
    fn json_wire_is_byte_stable_vs_158_159() {
        assert_eq!(
            render_json(&wire_golden_report()).unwrap(),
            WIRE_GOLDEN,
            "#160 must not perturb the schema:1 wire by a single byte"
        );
    }

    // --- AC: degenerate periods render a neutral summary without panic ---------------

    #[test]
    fn summary_band_renders_empty_single_and_all_gap_without_panic() {
        // Empty roster → the band is empty (nothing to summarise); the views print their
        // own "no per-account usage" line, never a panic.
        let empty = charts_report(&[], &[]);
        assert_eq!(render_summary(&empty, false), "");
        assert_eq!(render_summary(&empty, true), "");
        let _ = render_text(&empty);
        let _ = render_charts(&empty, 80, true, false);

        // A single account bands off its one reading and is its own lowest-utilisation pick.
        let single = charts_report(&[("solo", stat(1, ds(0.5, 0.5, 0.5), 0.0, 1.0))], &[]);
        let band = render_summary(&single, false);
        assert!(band.contains("solo balanced"));
        assert!(band.contains("lowest utilisation: solo"));

        // An all-gap account (present in the summary, absent from every bucket) still bands
        // its summary reading — no panic, no fabricated data, still neutral.
        let all_gap = charts_report(
            &[("ghost", stat(1, ds(0.0, 0.0, 0.0), 0.0, 0.0))],
            &[&[], &[]],
        );
        let band = render_summary(&all_gap, false);
        assert!(band.contains("ghost underused"));
        assert_eq!(scan_banned(&band), None);
    }

    #[test]
    fn summary_band_excludes_unsampled_accounts_and_never_fabricates_a_low_reading() {
        // Gap honesty: an account active but never polled (`seen == 0`, zeroed readings) has
        // UNKNOWN utilisation. The band must not fabricate it as "underused", and the
        // lowest-utilisation callout must not rank its fabricated 0% mean as the lowest — it
        // ranges over OBSERVED accounts only.
        let report = charts_report(
            &[
                ("live", stat(4, ds(0.50, 0.55, 0.52), 0.0, 0.5)),
                ("dark", stat(0, ds(0.0, 0.0, 0.0), 0.0, 0.5)), // active but unsampled
            ],
            &[],
        );
        let band = render_summary(&report, false);
        assert!(band.contains("live balanced"));
        assert!(
            !band.contains("dark"),
            "an unsampled account is not banded: {band:?}"
        );
        assert!(
            band.contains("lowest utilisation: live"),
            "lowest ranges over observed accounts, not the 0% unsampled one: {band:?}"
        );

        // A roster of ONLY unsampled accounts has nothing measured to summarise → empty band.
        let all_dark = charts_report(&[("dark", stat(0, ds(0.0, 0.0, 0.0), 0.0, 1.0))], &[]);
        assert_eq!(render_summary(&all_dark, false), "");
    }

    // --- issue #314: non-roster ("orphan") handle partition --------------------------

    /// A roster handle set from literals.
    fn roster(handles: &[&str]) -> BTreeSet<String> {
        handles.iter().map(|h| (*h).to_string()).collect()
    }

    /// A store with two in-roster handles (`work`, `spare`) and two non-roster ones
    /// (`backup`, `third`) sampled once each in a single `week` window, built against the
    /// given `live` roster. `live = None` models a store read with no config loaded.
    fn orphan_report(live: Option<&BTreeSet<String>>) -> Report {
        let now = epoch("2026-07-01T12:00:00Z");
        let samples = vec![
            sample(now - HOUR_SECS, "work", 0.9, 0.4),
            sample(now - HOUR_SECS, "spare", 0.5, 0.3),
            sample(now - HOUR_SECS, "backup", 0.2, 0.1),
            sample(now - HOUR_SECS, "third", 0.7, 0.2),
        ];
        let store = data(samples, "");
        let window = plan_window(Some("week"), None, now, &store).unwrap();
        build_report(&store, window, vec![], live, &params(), 0)
    }

    #[test]
    fn text_lists_non_roster_handles_in_a_separate_section() {
        let live = roster(&["work", "spare"]);
        let out = render_text(&orphan_report(Some(&live)));
        // Two orphans get their own counted, labelled section.
        assert!(
            out.contains("not in roster (2):"),
            "orphans surface in a counted section:\n{out}"
        );
        // Everything BEFORE that section is the live-account table: the two live handles, and
        // neither orphan (an orphan is never a peer of a live account).
        let head = out.split("not in roster").next().unwrap();
        assert!(
            head.contains("work") && head.contains("spare"),
            "live accounts head the view"
        );
        assert!(
            !head.contains("backup"),
            "orphan 'backup' never sits among live accounts"
        );
        assert!(
            !head.contains("third"),
            "orphan 'third' never sits among live accounts"
        );
        // The orphan handles do appear (in the section).
        assert!(
            out.contains("backup") && out.contains("third"),
            "orphans are listed, not hidden"
        );
    }

    #[test]
    fn charts_exclude_orphans_from_peer_charts_and_name_them_in_a_footer() {
        let live = roster(&["work", "spare"]);
        let out = render_charts(&orphan_report(Some(&live)), 120, false, false);
        // A compact, counted footer names the orphans.
        assert!(
            out.contains("not in roster (2): "),
            "charts foot with a named orphan line:\n{out}"
        );
        assert!(out.contains("backup") && out.contains("third"));
        // The charted region (everything before that footer) plots the live accounts and
        // NEITHER orphan — an orphan never takes a peer chart slot.
        let charted = out.split("not in roster").next().unwrap();
        assert!(
            charted.contains("work") && charted.contains("spare"),
            "live accounts are charted"
        );
        assert!(
            !charted.contains("backup"),
            "an orphan is never charted as a peer"
        );
        assert!(
            !charted.contains("third"),
            "an orphan is never charted as a peer"
        );
    }

    #[test]
    fn json_places_orphans_apart_from_live_accounts() {
        let live = roster(&["work", "spare"]);
        let json = render_json(&orphan_report(Some(&live))).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Live accounts under `summary.accounts`; an orphan is absent there.
        assert!(v["summary"]["accounts"]["work"].is_object());
        assert!(v["summary"]["accounts"]["spare"].is_object());
        assert!(
            v["summary"]["accounts"]["backup"].is_null(),
            "an orphan is not a live account: {json}"
        );
        // Orphans carried under the dedicated top-level `orphans` map; a live handle is absent.
        assert!(
            v["orphans"]["backup"].is_object(),
            "orphan under top-level `orphans`"
        );
        assert!(v["orphans"]["third"].is_object());
        assert!(
            v["orphans"]["work"].is_null(),
            "a live account is not an orphan"
        );
        // Series buckets never carry an orphan (they only ever plot live accounts).
        for bucket in v["series"].as_array().unwrap() {
            assert!(
                bucket["accounts"]["backup"].is_null(),
                "series never plots an orphan: {bucket}"
            );
        }
    }

    #[test]
    fn json_omits_orphans_key_when_no_orphans() {
        // Roster covers every present handle → no orphans → the key is omitted entirely
        // (additive to schema:1; a consumer sees `orphans` only when there are some).
        let live = roster(&["work", "spare", "backup", "third"]);
        let json = render_json(&orphan_report(Some(&live))).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            v.get("orphans").is_none(),
            "no `orphans` key when there are none: {json}"
        );
        assert!(
            v["summary"]["accounts"]["backup"].is_object(),
            "'backup' is now a live account"
        );
    }

    #[test]
    fn absent_roster_leaves_every_handle_in_the_main_table() {
        // No config / roster (None) → no partition: every handle stays a live row, no section
        // — a pre-`capture` `stats` reads exactly as before roster-awareness.
        let out = render_text(&orphan_report(None));
        assert!(
            !out.contains("not in roster"),
            "no orphan section without a roster:\n{out}"
        );
        for h in ["work", "spare", "backup", "third"] {
            assert!(out.contains(h), "{h} still rendered in the main table");
        }
        let json = render_json(&orphan_report(None)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.get("orphans").is_none(), "no roster ⇒ no orphans key");
        assert!(
            v["summary"]["accounts"]["backup"].is_object(),
            "every handle is a live account"
        );
    }

    #[test]
    fn empty_roster_makes_every_handle_an_orphan() {
        // Config present but EMPTY (Some, zero accounts) is distinct from None: every present
        // handle is a genuine orphan.
        let empty = roster(&[]);
        let report = orphan_report(Some(&empty));
        assert_eq!(report.summary.per_account.len(), 0, "no live accounts");
        assert_eq!(report.orphans.len(), 4, "every handle is an orphan");
        let out = render_text(&report);
        assert!(
            out.contains("not in roster (4):"),
            "all four surface in the section:\n{out}"
        );
        assert!(
            out.contains("backup") && out.contains("work"),
            "handles listed under the section"
        );
    }

    #[test]
    fn roster_handles_uses_labels_verbatim_and_keeps_disabled_accounts() {
        // The join key is `Account.label` verbatim (what the daemon freezes into `Sample.acct`),
        // and a DISABLED account is still in the roster — only removed/renamed handles orphan.
        let toml = "[[account]]\naccount_uuid = \"u1\"\nlabel = \"work\"\n\
                    [[account]]\naccount_uuid = \"u2\"\nlabel = \"spare\"\nenabled = false\n";
        let config = Config::from_toml_str(toml).expect("valid config");
        let set = roster_handles(&config);
        assert!(
            set.contains("work"),
            "enabled account label is in the roster"
        );
        assert!(
            set.contains("spare"),
            "DISABLED account label is STILL in the roster (issue #314)"
        );
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn orphan_split_preserves_roster_wide_swap_stats() {
        // Splitting orphans out of the DISPLAY must never drop roster-wide stats — those are
        // computed over the FULL sample/event set, independent of which rows are shown.
        let now = epoch("2026-07-01T12:00:00Z");
        let samples = vec![
            sample(now - HOUR_SECS, "work", 0.9, 0.4),
            sample(now - HOUR_SECS, "backup", 0.2, 0.1),
        ];
        let events = "ts=2026-07-01T09:00:00Z event=swap from=backup to=work reason=session\n";
        let store = data(samples, events);
        let window = plan_window(Some("week"), None, now, &store).unwrap();
        let live = roster(&["work"]);
        let report = build_report(&store, window, vec![], Some(&live), &params(), 0);
        assert_eq!(report.summary.per_account.len(), 1, "only 'work' is live");
        assert!(
            report.orphans.contains_key("backup"),
            "'backup' split into orphans"
        );
        assert_eq!(
            report.summary.roster.swap_count, 1,
            "the swap is still counted"
        );
    }

    #[test]
    fn charts_all_orphan_store_still_names_them_via_the_empty_path() {
        // Reachable state: EVERY handle is an orphan, so the live-account list is empty and
        // `render_charts` takes its `no per-account usage` early return. That path must still
        // surface the orphan footer (and never call the peer chart sub-renderers).
        let empty = roster(&[]);
        let out = render_charts(&orphan_report(Some(&empty)), 120, false, false);
        assert!(
            out.contains("no per-account usage in this window"),
            "no LIVE accounts:\n{out}"
        );
        assert!(
            out.contains("not in roster (4): "),
            "orphans still named on the empty path"
        );
        assert!(
            out.contains("backup") && out.contains("work"),
            "every handle named"
        );
    }

    #[test]
    fn positional_filter_selecting_an_orphan_shows_it_as_not_in_roster() {
        // Reachable state: the positional filter narrows to a single handle that is itself an
        // orphan. It must render UNDER the orphan section (honest), never as a live account —
        // the filter runs first, then the roster split classifies what remains.
        let now = epoch("2026-07-01T12:00:00Z");
        let samples = vec![
            sample(now - HOUR_SECS, "work", 0.9, 0.4),
            sample(now - HOUR_SECS, "backup", 0.2, 0.1),
        ];
        let store = data(samples, "");
        let window = plan_window(Some("week"), None, now, &store).unwrap();
        let live = roster(&["work"]);
        // `stats backup` — filter to the orphan handle.
        let report = build_report(
            &store,
            window,
            vec!["backup".to_owned()],
            Some(&live),
            &params(),
            0,
        );
        assert!(
            report.summary.per_account.is_empty(),
            "no LIVE account survives the filter"
        );
        assert!(
            report.orphans.contains_key("backup"),
            "the filtered-to handle is the orphan"
        );
        let out = render_text(&report);
        assert!(
            out.contains("not in roster (1):"),
            "shown, honestly, as an orphan:\n{out}"
        );
        let head = out.split("not in roster").next().unwrap();
        assert!(!head.contains("backup"), "never rendered as a live account");
    }

    // --- daemon `stats` socket verb (issue #356) --------------------------------------

    #[test]
    fn socket_stats_json_equals_the_cli_json_for_the_same_report() {
        // R-2 parity (issue #356), structural: the socket verb and `stats --json` serialize the SAME
        // `stats_wire` from the SAME report — the socket COMPACT, the CLI PRETTY — so they must
        // decode to the identical JSON value (the bytes differ only in whitespace). Parity is
        // guaranteed by the shared builder, not kept in lockstep by hand.
        let report = report_fixture();
        let socket = serde_json::to_string(&stats_wire(&report)).unwrap();
        let cli = render_json(&report).unwrap();
        let socket_v: serde_json::Value = serde_json::from_str(&socket).unwrap();
        let cli_v: serde_json::Value = serde_json::from_str(cli.trim_end()).unwrap();
        assert_eq!(
            socket_v, cli_v,
            "the stats socket wire must equal `stats --json` for the same window (R-2 parity)"
        );
    }

    #[test]
    fn socket_stats_defaults_a_missing_period_to_week() {
        // A `stats` request with no period resolves the SAME window as an explicit `week` — the
        // 7-day daily-bucket series the panel Stats tab reads (the CLI's own default, too).
        let now = epoch("2026-07-08T00:00:00Z");
        let store = data(
            vec![
                sample(now - 2 * DAY_SECS, "work", 0.6, 0.3),
                sample(now - 5 * DAY_SECS, "spare", 0.2, 0.1),
            ],
            "",
        );
        assert_eq!(
            stats_socket_json(&store, now, 0, None),
            stats_socket_json(&store, now, 0, Some("week")),
            "a periodless stats request is the 7-day `week` window"
        );
        // And it is genuinely the 7-day series: 7 bounded daily buckets, period tag `week`.
        let v: serde_json::Value =
            serde_json::from_str(&stats_socket_json(&store, now, 0, None)).unwrap();
        assert_eq!(v["window"]["period"], "week");
        assert_eq!(v["series"].as_array().unwrap().len(), 7);
    }

    #[test]
    fn socket_stats_rejects_an_invalid_period_with_a_redacted_envelope() {
        // The issue's literal `"7d"` example is NOT a valid `--period` (it is `--since` grammar) — the
        // 7-day series is `"week"`, not `"7d"`. The socket rejects an unknown period with a non-secret
        // machine envelope, exactly as the CLI errors on it (issue #356). Rejection precedes any store
        // read, so an empty store still yields the envelope, never a panic.
        let store = data(vec![], "");
        assert_eq!(
            stats_socket_json(&store, 1_000_000, 0, Some("7d")),
            r#"{"error":"invalid period"}"#
        );
        assert_eq!(
            stats_socket_json(&store, 1_000_000, 0, Some("garbage")),
            r#"{"error":"invalid period"}"#
        );
    }

    #[test]
    fn socket_stats_serves_an_empty_store_as_a_valid_empty_series() {
        // An empty store is not an error — the panel shows an empty 7-day series, not "unavailable"
        // (the same tolerance the CLI reader has). A bounded, well-formed reply.
        let now = epoch("2026-07-08T00:00:00Z");
        let reply = stats_socket_json(&data(vec![], ""), now, 0, Some("week"));
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["schema"], 1);
        assert_eq!(
            v["series"].as_array().unwrap().len(),
            7,
            "still 7 bounded daily buckets"
        );
        assert!(
            v["summary"]["accounts"].as_object().unwrap().is_empty(),
            "no accounts in an empty store"
        );
    }

    // --- Cross-language wire golden: stats socket reply (issues #356 / #340) -----------
    //
    // The `stats` socket verb (#356) puts `StatsWire` on the cross-language boundary for the first
    // time (the Swift menubar previously mirrored only the snapshot/heartbeat/status frames). This is
    // its byte-drift golden — the stats sibling of daemon.rs's snapshot/heartbeat goldens (#340),
    // living here because `StatsWire` + `stats_wire` are private to this module. Deterministic (a
    // fixed report), so the pin test re-emits in-process and asserts byte-equality — the same
    // discipline as the daemon goldens. Mirrored by Swift `Fixtures.statsBasic`
    // (`apps/menubar/Tests/Fixtures.swift`), which the CI swift job pins to the SAME committed bytes.

    /// The frozen `stats` socket reply the cross-language guard pins: the SAME [`wire_golden_report`]
    /// the CLI `--json` byte-stability golden ([`WIRE_GOLDEN`]) uses, serialized the way the socket
    /// verb emits it — COMPACT (`to_string`, no trailing newline; the newline is the socket framing).
    /// Freezing the identical report both PRETTY (CLI) and COMPACT (socket) makes R-2 parity
    /// self-evident: one `stats_wire`, two serializations.
    fn wire_golden_stats_socket_frame() -> String {
        serde_json::to_string(&stats_wire(&wire_golden_report()))
            .expect("the stats golden report serializes")
    }

    /// One-time emitter for the committed `stats` socket golden (issues #356 / #340). `#[ignore]` —
    /// NOT part of the suite; it WRITES the bytes the pin test and Swift `Fixtures.statsBasic`
    /// consume. Run it ONLY alongside a deliberate `StatsWire` change:
    ///   `cargo test -- --ignored emit_wire_stats_golden_fixture`
    /// then update the Swift mirror (`apps/menubar/Tests/Fixtures.swift`) so the byte-equality holds.
    #[test]
    #[ignore = "one-time wire-stats-golden emitter — run ONLY alongside a deliberate StatsWire change"]
    fn emit_wire_stats_golden_fixture() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("build/fixtures");
        std::fs::create_dir_all(&dir).expect("create build/fixtures");
        std::fs::write(
            dir.join("wire-stats-basic.json"),
            wire_golden_stats_socket_frame(),
        )
        .expect("write wire-stats golden");
    }

    /// The committed `stats` socket golden — the exact bytes Swift `Fixtures.statsBasic` is pinned to.
    /// `include_str!` makes the file a compile-time input, so it must exist before this module
    /// compiles (emit once via [`emit_wire_stats_golden_fixture`]).
    const WIRE_STATS_GOLDEN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/build/fixtures/wire-stats-basic.json"
    ));

    #[test]
    fn the_committed_stats_wire_golden_still_matches_the_socket_encoder() {
        // The cross-language pin (issues #356 / #340), the stats sibling of daemon.rs's
        // snapshot/heartbeat goldens. `StatsWire` serialization is deterministic, so re-emitting
        // in-process and comparing to the COMMITTED bytes catches any shape drift — a renamed /
        // added / reordered / re-typed field, a `schema` bump — that shifts the bytes: the golden
        // goes stale and this fails, forcing a regenerate (`emit_wire_stats_golden_fixture`) that in
        // turn breaks the Swift byte-equality until the hand-written Swift mirror is updated too.
        assert_eq!(
            wire_golden_stats_socket_frame(),
            WIRE_STATS_GOLDEN,
            "the committed wire-stats golden drifted from the stats socket encoder — re-run \
             `cargo test -- --ignored emit_wire_stats_golden_fixture`, then update the Swift mirror \
             (apps/menubar/Tests/Fixtures.swift) so its fixture stays byte-identical"
        );
    }
}
