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
//! Default render is NUMERIC text (the summary table + a roster line + the resolved-window
//! echo in local time). `--json` emits the versioned, stable `schema:1` wire contract
//! (full series + summary + neutral descriptor enums; redacted handles only).
//!
//! # Scope seam (issues #159 / #160)
//!
//! This is the BASE verb: numbers + JSON. The terminal CHARTS (#159) render the JSON
//! `series`; the neutral SIGNAL summary (#160) annotates the JSON `summary`. Neither is
//! built here — the wire schema carries the series + summary + neutral per-account
//! descriptor enums (`band`, `coverage_class`) they consume, and deliberately carries no
//! chart glyph and no recommendation field. `HistoryStore::read_rollup` also exposes the
//! lifetime daily tier as a seam for #159's deep-history charts (that tier is roster-wide,
//! so it cannot back a per-account series; here it only anchors the `lifetime` window
//! start).
//!
//! # Gap honesty
//!
//! The aggregator never invents a reading, and neither does this verb: a bucket that
//! predates the store's raw retention simply reports low `coverage` rather than a
//! fabricated calm. Everything is whole UTC epoch seconds end to end; only the human
//! window echo is rendered in the operator's local time zone.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Serialize;

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
pub(crate) struct StatsArgs {
    /// Positional account filter — the redacted handles to show (empty = all).
    pub(crate) accounts: Vec<String>,
    /// The raw `--period` value, if given.
    pub(crate) period: Option<String>,
    /// The raw `--since` value, if given.
    pub(crate) since: Option<String>,
    /// Whether `--json` was set.
    pub(crate) json: bool,
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
    // Tunables drive the aggregator's thresholds; a missing config is not fatal for a
    // read-only view — fall back to the built-in defaults so `stats` works pre-`capture`.
    let params = params_from(Config::load().ok().as_ref());
    let report = build_report(&data, window, args.accounts, &params, offset);

    let out = if args.json {
        render_json(&report)?
    } else {
        render_text(&report)
    };
    print!("{out}");
    Ok(())
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

    let series = bucket_bounds(window.start, window.end, window.base_bucket())
        .into_iter()
        .map(|(lo, hi)| {
            let mut bucket = aggregate(&data.samples, &swaps, Period::new(lo, hi), params);
            apply_filter(&mut bucket.per_account, &accounts);
            bucket
        })
        .collect();

    Report {
        window,
        accounts,
        summary,
        series,
        offset,
    }
}

/// Restrict a per-account map to the requested handles (no-op when the filter is empty).
fn apply_filter(per_account: &mut BTreeMap<String, AccountStats>, accounts: &[String]) {
    if accounts.is_empty() {
        return;
    }
    per_account.retain(|handle, _| accounts.iter().any(|a| a == handle));
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

/// Render the numeric text view: the window echo, the per-account summary table, and the
/// roster line. Deliberately carries no chart glyph (issue #159) and no recommendation
/// (issue #160) — just numbers.
fn render_text(report: &Report) -> String {
    let mut out = String::new();
    let label = format_window_label(&report.window, report.offset);
    out.push_str(&format!("usage — {label}\n\n"));

    let summary = &report.summary;
    if summary.per_account.is_empty() {
        out.push_str("  no per-account usage in this window\n");
    } else {
        let handle_w = summary
            .per_account
            .keys()
            .map(String::len)
            .max()
            .unwrap_or(0)
            .max("account".len());
        out.push_str(&format!(
            "{:<hw$}  cov   session m/p/p95   weekly m/p/p95    caps  t@cap   share\n",
            "account",
            hw = handle_w,
        ));
        for (handle, a) in &summary.per_account {
            out.push_str(&format!(
                "{:<hw$}  {:>3}%  {:<15}  {:<15}  {:>4}  {:>5}  {:>4}%\n",
                handle,
                pct(a.coverage),
                triple(&a.session),
                triple(&a.weekly),
                a.cap_hits,
                fmt_dur(a.time_at_cap_secs),
                pct(a.contribution_share),
                hw = handle_w,
            ));
        }
    }

    let r = &summary.roster;
    out.push('\n');
    out.push_str(&format!(
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
    ));
    out
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

/// The per-account + roster body shared by the summary and each series bucket.
#[derive(Serialize)]
struct PeriodWire {
    roster: RosterWire,
    accounts: BTreeMap<String, AccountWire>,
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

/// Render the stable `--json` document.
fn render_json(report: &Report) -> Result<String> {
    let (period, since) = match &report.window.kind {
        WindowKind::Period(p) => (Some(p.wire_tag()), None),
        WindowKind::Since(s) => (None, Some(s.as_str())),
    };
    let wire = StatsWire {
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
                accounts: accounts_wire(&r.per_account),
            })
            .collect(),
        summary: PeriodWire {
            roster: roster_wire(&report.summary.roster),
            accounts: accounts_wire(&report.summary.per_account),
        },
    };
    let mut json = serde_json::to_string_pretty(&wire)
        .map_err(|_| Error::StatsSerialize("a usage value was not a finite number"))?;
    json.push('\n');
    Ok(json)
}

fn accounts_wire(per_account: &BTreeMap<String, AccountStats>) -> BTreeMap<String, AccountWire> {
    per_account
        .iter()
        .map(|(handle, a)| (handle.clone(), account_wire(a)))
        .collect()
}

fn account_wire(a: &AccountStats) -> AccountWire {
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
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let report = build_report(&read, window, vec![], &params(), 0);
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
        let report = build_report(&read, window, vec![], &params(), 0);

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
        build_report(&store, window, vec![], &params(), 0)
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
        assert!(!json.contains('@'), "no email may reach the wire: {json}");
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
        let report = build_report(&store, window, vec!["work".to_owned()], &params(), 0);
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
}
