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
// (issue #159) size their columns on the SAME wcwidth — one definition for the crate. The
// REFRESH-token expiry cell (issue #883) is reused from the same place and for the same reason:
// ONE spelling of that fact, so the `status` and `stats` renders cannot drift apart.
use crate::cli::{display_width, expiry_table_cell, pad_end, EXPIRY_GAP};
use crate::config::{Config, Tunables};
use crate::daemon::AccountExpiry;
use crate::error::{Error, Result};
use crate::observability;
use crate::paths;
use crate::usage::epoch_from_rfc3339;
use crate::usage_stats::{
    aggregate_with_roster, parse_swap_events, AccountStats, AggregateParams, Period, RosterStats,
    UsageReport,
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
    /// The per-account REFRESH-token expiry modifier (issue #883), keyed by the SAME handle as
    /// `summary.per_account` — the overlay backing the `expiry` COLUMN of the one per-account
    /// table (design-stats.md §D-STA-5: a per-account metric is a COLUMN, never a band or a
    /// footer list keyed per account, the shape issues #543/#544 were retired for).
    ///
    /// **EMPTY on every path this crate builds TODAY, and that is a stated condition with a named
    /// remaining step, not an oversight.** `stats` is a structurally OFFLINE reader (see the
    /// module header): its whole pipeline is a pure function of the [`HistoryStore`] seam — raw
    /// samples, rolled aggregates, event-log text. Two of the three still cannot carry a
    /// `refreshTokenExpiresAt` deadline: [`crate::usage_store::Sample`] has no such field, and the
    /// only other local source, [`crate::cli::AuthSubset`], is both the wrong axis (the ACCESS
    /// token) and a keychain read this verb is forbidden to make. Reaching for the live socket
    /// would forfeit the "renders when the daemon is down" property that seam exists to guarantee.
    ///
    /// The THIRD now can. Issue #880 has landed `event=credential_expiry_horizon` and
    /// `event=credential_expiry_observed` on the durable log this store already reads via
    /// [`HistoryStore::read_events`] — so the material is present in the seam. What does not exist
    /// yet is the FOLD: [`build_report`] parses that text only for swaps
    /// ([`parse_swap_events`]), and reducing the expiry lines to one deadline per account is
    /// aggregation-layer work with its own questions (which line wins when both appear, how a
    /// deadline ages out of the window, whether an account carrying only a `first_observation`
    /// anchor and no horizon edge should render at all). Issue #883 owns the RENDER path, so the
    /// fold is tracked separately rather than smuggled in beside a column definition.
    ///
    /// So this is the CONSUMER half, wired here so that landing the fold populates one map rather
    /// than re-shaping `Report`, [`AccountRow`], the catalog, and both subsets. Until then the
    /// column is uniformly [`EXPIRY_GAP`] and [`render_account_table`]'s empty-column elision
    /// drops it before the width fit — exactly what `velocity` / `runway` do on a report with no
    /// velocity overlay, so every existing render stays byte-identical. Gap-honest by
    /// construction: an unpopulated overlay reads as "not observed", never as "not expiring" (the
    /// issue #137 invariant).
    expiry: BTreeMap<String, AccountExpiry>,
    /// WHICH SET the all-accounts-high census intersected over (issue #836): `true` when
    /// [`build_report`] had the CONFIGURED roster, `false` when it did not and the census
    /// degraded to whoever held samples in the period.
    ///
    /// Issue #804 introduced the two regimes and they are NOT interchangeable — under the
    /// fallback an unsampled account silently leaves the intersection, so the metric fires on
    /// strictly less evidence than the configured form, which is the one direction
    /// REQ-STA-B-005's amendment forbids. Without this the human render states the census's
    /// water but not its set, so the two regimes print the same bytes and a reader cannot tell
    /// which number they hold.
    ///
    /// CARRIED from the very `roster` argument [`aggregate_with_roster`] consumed, for the same
    /// reason [`crate::usage_stats::RosterStats::high_threshold`] is carried rather than
    /// re-derived: a render that recomputes the regime from a second source (the caller's own
    /// `Config`, one hop earlier) would keep printing a claim about the census after the
    /// census's input stopped agreeing with it.
    ///
    /// Issue #836 deliberately added no wire field, which left the panel's aggregate callout with
    /// the same blindness the human render had just lost — issue #866. It now also rides the wire
    /// as [`RosterWire::census_over_roster`], read from this one field, so the two surfaces cannot
    /// disagree about the REGIME ITSELF.
    ///
    /// Their RENDERS still can, deliberately, though no longer on THIS axis: [`roster_line`] below
    /// suppresses the qualifier when the census was never measurable
    /// (`all_high_covered_secs == 0`), and since issue #1029 the panel decodes that key and
    /// suppresses it too (`StatusPanelFormat.statsAllHighLabel`), so the two surfaces now agree on
    /// WHEN the set is named. What they still render differently is the UNKNOWN reading itself —
    /// `—` here, a sentence on the panel — which is R-2 STATE-parity, not glyph-parity. Do not read
    /// the parity above as extending to the rendered strings.
    census_over_roster: bool,
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
    /// Approximate whole seconds until the session reading reaches `session_ceiling` at the
    /// smoothed rate: `(ceiling − current) / rate`. `None` when the rate is unknown or `0`, or
    /// the reading is already at/over the ceiling (no positive head-room to state as a fact), or
    /// the quotient is not PLAUSIBLE — past one rolling session window
    /// ([`SESSION_RUNWAY_PLAUSIBLE_MAX_SECS`], issue #1075).
    session_runway_secs: Option<i64>,
    /// Approximate whole seconds until the weekly reading reaches `weekly_ceiling`. `None` on
    /// the same cases (commonly `None` — the weekly window moves slowly, so a flat weekly
    /// dimension has no measurable rate), bounded at one weekly window
    /// ([`WEEKLY_RUNWAY_PLAUSIBLE_MAX_SECS`]) rather than the session one.
    weekly_runway_secs: Option<i64>,
    /// The account's remaining WEEKLY head-room as a usage fraction — `max(0, weekly_ceiling −
    /// latest weekly reading)` — the pool contribution the fleet aggregate (issue #544) sums. `Some`
    /// EXACTLY when [`Self::weekly_rate`] is `Some` (a KNOWN weekly velocity), so head-room is
    /// recorded only for an account whose burn is also known: a KNOWN-zero (flat, measured) account
    /// keeps a `Some` head-room (real spare capacity, `0` burn), while an unknown / stale account is
    /// `None` and excluded from the aggregate — [`fleet_runway`] owns why. Distinct from
    /// [`Self::weekly_runway_secs`], which is `None` for a flat account even though its head-room is
    /// positive — this field is the raw head-room the pool needs, not the per-account time-to-trigger.
    weekly_headroom: Option<f64>,
}

/// Why a `stats` view fell back to DEFAULT tunables — the two-sink split issues #627 and #642
/// together demand. Built ONLY for the malformed case; an ABSENT config produces no fault at all.
struct ConfigFault {
    /// The full, secret-free detail for the OPERATOR-SCOPED sink — the CLI's stderr ([`run_output`])
    /// or the daemon log ([`stats_socket_json_with`]). This is [`Error`]'s own `Display`: the parser's
    /// span echo, caret art and all, exactly as `config validate` prints it (issue #627). Unchanged.
    log_detail: String,
    /// The reason placed on the WIRE (issue #642) — and deliberately a `&'static str`.
    ///
    /// That type IS the redaction guarantee: a static string cannot contain a byte of the operator's
    /// `config.toml`, so no scrubber has to anticipate every shape a parser error can take. That
    /// matters because the shapes are genuinely unbounded — the TOML span echo re-prints the whole
    /// offending line, serde's `invalid type: string "…"` quotes the VALUE, and validation errors
    /// interpolate the offending value (`duplicate account_uuid: …`). `label` is exactly where an
    /// operator's e-mail address lives (`src/config.rs`) — `account_uuid` no longer can, since
    /// issue #1052 constrained it to `[A-Za-z0-9_-]{1,128}`, but `label` stays free-form — and
    /// #642 moves this string from the log onto three WIDER surfaces: `stats --json` on stdout
    /// (piped into files and dashboards), the control socket, and a screenshot-able panel. Sanitising
    /// an adversarial string for those is a losing game; not deriving it from the config wins outright.
    ///
    /// Actionability is preserved by NAMING THE COMMAND rather than echoing the content — the
    /// operator runs `sessiometer config validate` on their own terminal and gets the full detail
    /// from the tool built to print it. Being static also makes the string trivially short, which is
    /// what the fixed-width panel needs.
    wire_reason: &'static str,
}

/// Resolve the config for a read-only `stats` view (issue #627), separating the two
/// silent-fallback cases that must be treated differently:
///
/// - an **absent** config ([`Error::ConfigNotFound`]) is the normal pre-`capture` state — `stats`
///   works before the first capture — so it stays silent and falls back to built-in defaults;
/// - a config that **exists but is malformed** (a parse / validate / I/O failure) is the #627 case:
///   every *other* command (`config validate` / `show`, `list`, `run`) hard-fails and names the
///   offending key, so `stats` — the sole silent consumer — surfaces the SAME secret-free detail
///   ([`Error`]'s `Display`, issue #15) and *then* falls back to defaults.
///
/// Returns the effective config (or `None` → defaults) plus, on the malformed branch, the
/// [`ConfigFault`] whose two fields the caller routes to its two different sinks. Pure over its
/// argument — the [`Config::load`] I/O happens in the caller — so the absent-vs-malformed branch is
/// unit-tested through the [`Config::load_path`] seam without touching the ambient config path or
/// process env (issue #627 regression test).
fn resolve_stats_config(loaded: Result<Config>) -> (Option<Config>, Option<ConfigFault>) {
    match loaded {
        Ok(config) => (Some(config), None),
        // Absent: pre-`capture` normal state — silent defaults, no warning (would be noise on
        // every fresh install).
        Err(Error::ConfigNotFound { .. }) => (None, None),
        // Malformed: the file exists but failed to parse/validate — surface the same detail the
        // sibling commands name, then fall back to defaults.
        Err(err) => (
            None,
            Some(ConfigFault {
                wire_reason: wire_config_reason(&err),
                log_detail: err.to_string(),
            }),
        ),
    }
}

/// Classify a config-load failure into the static wire reason (issue #642).
///
/// Matches on the [`Error`] VARIANT, never on its rendered message, so the classification cannot
/// drift when a dependency reformats its error text — and, by returning `&'static str`, cannot carry
/// config content by construction (see [`ConfigFault::wire_reason`]). The arms are deliberately
/// coarse: the wire says WHICH KIND of failure and where to get the detail; the operator-scoped log
/// says exactly what and where.
fn wire_config_reason(err: &Error) -> &'static str {
    match err {
        Error::ConfigParse(_) => {
            "config.toml is not valid TOML — run `sessiometer config validate` for the detail"
        }
        Error::Io(_) => {
            "config.toml could not be read — run `sessiometer config validate` for the detail"
        }
        // `ConfigInvalid`, the cross-field `ConfigTargetMaxSessionAboveTrigger`, and any future
        // validation variant: the file parsed, a value did not hold.
        _ => "config.toml failed validation — run `sessiometer config validate` for the detail",
    }
}

/// Entry point for the `stats` verb: read the store once, resolve the window, aggregate,
/// and render. The only impure step is reading the store + wall clock; everything else is
/// a pure function of `StoreData` + `now`.
pub(crate) async fn run(args: StatsArgs) -> Result<()> {
    let store = NativeHistoryStore::from_paths()?;
    let data = StoreData::read(&store)?;
    let now = wall_clock_now();
    let offset = local_offset_secs(now);
    print!("{}", run_output(args, &data, now, offset, Config::load)?);
    Ok(())
}

/// Everything [`run`] does AFTER the store read, over an INJECTED config load — the CLI peer of
/// [`stats_socket_json_with`], and for the same reason: `run` reads the AMBIENT config, so without
/// this seam the `config_unreadable` argument could be severed at its call site with the whole suite
/// still green. Returns the rendered stdout text; the stderr warning is emitted here (it is part of
/// the behaviour under test, issue #627).
fn run_output(
    args: StatsArgs,
    data: &StoreData,
    now: i64,
    offset: i64,
    load_config: impl FnOnce() -> Result<Config>,
) -> Result<String> {
    let window = plan_window(args.period.as_deref(), args.since.as_deref(), now, data)?;
    // One config load feeds BOTH the aggregator thresholds AND the roster reconciliation
    // (issue #314). An ABSENT config is the normal pre-`capture` state — thresholds fall back to
    // built-in defaults and the missing roster just disables the orphan partition (every handle
    // renders as before), so `stats` still works before the first `capture`, silently. But a
    // config that EXISTS yet fails to parse/validate is surfaced (issue #627): every sibling
    // command names the offending key, so `stats` must not be the one that silently renders its
    // ceiling-dependent figures (`caps`, `t@cap`, `runway`) against DEFAULT tunables as if valid.
    // Warn on stderr, then continue against defaults — the silence is the bug, not the exit 0.
    let (config, config_fault) = resolve_stats_config(load_config());
    // Two sinks, two fidelities (issues #627 + #642): stderr is operator-scoped and keeps the FULL
    // parser detail; the `--json` document below gets only the static wire reason, because stdout is
    // one of the surfaces #642 widens (it is piped into files and dashboards).
    if let Some(fault) = &config_fault {
        eprintln!(
            "sessiometer: warning: rendering stats against default tunables — {}",
            fault.log_detail
        );
    }
    let report = overlaid_report(data, window, args.accounts, config.as_ref(), offset);

    if args.json {
        // The `--json` document states the same malformed-config provenance the stderr warning
        // above does (issue #642) — a consumer piping stdout never sees that warning, so without
        // this key it is exactly as blind as the panel was.
        render_json(&report, config_fault.map(|f| f.wire_reason))
    } else {
        // …and so does the HUMAN render (issue #836), which is the surface this metric is
        // actually read on. It was the last one left without the provenance: `--json` got it in
        // #642 and stderr in #627, while `render_text` / `render_charts` printed a census whose
        // roster regime — configured or degraded — was unstated and therefore unknowable.
        Ok(render_human(
            &report,
            TermEnv::detect(args.no_color, args.ascii),
            config_fault.map(|f| f.wire_reason),
        ))
    }
}

/// The daemon `stats` socket verb (issue #356): read the store, compute the bounded per-account
/// daily series, and return the reply line the spawned socket task writes verbatim — the compact
/// `StatsWire` JSON for `period`, or a non-secret `{"error":"…"}` envelope on an invalid period or
/// an unreadable store. When `config.toml` exists but is malformed, the document is still served
/// and additionally carries `config_unreadable` (issue #642) — the client's signal that every
/// ceiling-dependent figure rests on DEFAULT tunables.
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
///
/// A malformed `config.toml` is NOT one of those envelope cases: the document is still served (the
/// panel keeps its series) but carries `config_unreadable` so the client can say the numbers rest on
/// defaults (issue #642).
///
/// Reads the ambient `config.toml`; [`stats_socket_json_with`] is the same body over an INJECTED
/// config load, so the malformed-config path is testable without touching the real config path.
fn stats_socket_json(data: &StoreData, now: i64, offset: i64, period: Option<&str>) -> String {
    stats_socket_json_with(data, now, offset, period, Config::load)
}

/// [`stats_socket_json`] over an injected config loader — the seam that lets a test drive the
/// WHOLE socket path (window → config resolve → report → wire) against a controlled malformed
/// `config.toml`, rather than only the `stats_wire` builder in isolation. Without it, severing the
/// `config_unreadable` argument at the call site below leaves every test green: the plumbing #642
/// exists to create would be uncovered by construction.
///
/// Takes a CLOSURE, not a `Result<Config>`, so the load still happens lazily AFTER the period
/// check — an invalid period must short-circuit before any config I/O, exactly as before.
fn stats_socket_json_with(
    data: &StoreData,
    now: i64,
    offset: i64,
    period: Option<&str>,
    load_config: impl FnOnce() -> Result<Config>,
) -> String {
    // `since` is always `None` over the socket — only the CLI `--since` grammar drives that path — so
    // the sole `plan_window` failure here is an unknown `--period` value (`StatsPeriodInvalid`).
    let window = match plan_window(period, None, now, data) {
        Ok(window) => window,
        Err(_) => return r#"{"error":"invalid period"}"#.to_owned(),
    };
    // One config load feeds both the aggregator thresholds and roster reconciliation, exactly as
    // `run` does. Re-read from disk — the same config the CLI reader sees — rather than the daemon's
    // in-memory copy (which the spawned, `Send`-only task cannot borrow), so the socket series stays
    // byte-parity with `stats --json`. An ABSENT config stays silent (defaults, the pre-`capture`
    // state); a MALFORMED one is BOTH logged daemon-side (issue #627) AND surfaced on the wire as
    // `config_unreadable` (issue #642), so the client can annotate the numbers instead of trusting
    // them. The reply stays a FULL document rather than an `{"error":…}` envelope: dropping the
    // whole payload would cost the panel its best-effort series, whereas the flag lets it render
    // "computed against defaults" over real data. Preferring the daemon's valid in-memory config
    // over this disk re-read (which needs threading it through the `Send`-only task) remains the
    // orthogonal, still-open follow-up — it would change WHICH tunables apply; #642 changes only
    // whether the client is TOLD which applied.
    let (config, config_fault) = resolve_stats_config(load_config());
    // The LOG keeps the full parser detail (span echo and all) — it is operator-scoped and #627
    // already accepted it. The WIRE gets only the static reason, because the socket is one of the
    // wider surfaces #642 opens.
    if let Some(fault) = &config_fault {
        eprintln!(
            "sessiometer: warning: serving stats against default tunables — {}",
            fault.log_detail
        );
    }
    // No account filter over the socket — the whole roster (matches `stats --period <p> --json` with
    // no `--account`), overlaid from the SAME config the CLI uses so a socket read stays byte-parity
    // with `stats --period <p> --json` (issue #543 keeps R-2).
    let report = overlaid_report(data, window, Vec::new(), config.as_ref(), offset);
    serde_json::to_string(&stats_wire(&report, config_fault.map(|f| f.wire_reason)))
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
///
/// `config_unreadable` is the malformed-config provenance [`render_json`] already carries for
/// issue #642, taken here as an ARGUMENT for the same reason it is one there: the fact belongs
/// to the CALLER's config load, not to the aggregate, and one carrier per fact keeps the human
/// and `--json` surfaces from describing the same failure two different ways. `None` for a
/// readable — or absent — config. See [`config_regime_line`], which is the only thing that
/// reads it; the census's own SET rides [`Report::census_over_roster`] instead, on a separate
/// key, because it is true under an absent config too (issue #836).
fn render_human(report: &Report, env: TermEnv, config_unreadable: Option<&str>) -> String {
    match env.cols {
        None => render_text(report, config_unreadable),
        Some(w) => render_charts(report, w, env.color, env.ascii, config_unreadable),
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
/// mismatch. Session cap and the all-accounts-high water are both `session_ceiling`
/// (`Config::swap_threshold` is exactly that percent as a fraction) — a neutral,
/// config-derived "hot" line, PINNED as the census threshold by issue #804 and carried out
/// on [`RosterStats::high_threshold`] so no surface has to hardcode a literal.
fn params_from(config: Option<&Config>) -> AggregateParams {
    // ONE resolved tunables source for every knob below — the config's when there is a config, the
    // shipping defaults otherwise — so no knob is read through two different paths.
    let defaults = Tunables::default();
    let (tunables, cap) = match config {
        Some(c) => (&c.tunables, c.swap_threshold()),
        None => (&defaults, f64::from(defaults.session_ceiling) / 100.0),
    };
    // The daemon's OWN viability boundary (issue #803), so the capacity-holds census measures the
    // lines the daemon actually enforced rather than a water chosen here. Sourced through
    // `swap::viability_boundary` — the same helper the daemon's decision path calls — so the two
    // surfaces cannot drift; in particular the weekly arm is `weekly_ceiling − WEEKLY_TAIL_MARGIN`
    // (0.97 at defaults), NOT the raw ceiling, and a census taken against the raw value would miss
    // exactly the accounts resting on the line.
    let viability = crate::swap::viability_boundary(
        f64::from(tunables.session_ceiling) / 100.0,
        f64::from(tunables.target_max_session_usage) / 100.0,
        f64::from(tunables.weekly_ceiling) / 100.0,
    );
    AggregateParams::new((tunables.poll_secs as i64).max(1), cap, cap).with_viability(viability)
}

/// The live-roster handle set for the orphan partition (issue #314): every account's
/// `label`, which is EXACTLY what the daemon freezes into each `Sample.acct`
/// ([`crate::daemon`] writes the label verbatim), so set membership is a plain string
/// compare against the [`aggregate_with_roster`] output's `per_account` keys. DISABLED
/// accounts are KEPT — a disabled account is still in the roster (its samples are
/// legitimate); only removed / renamed / stray handles fall outside this set and become
/// orphans.
///
/// Since issue #804 this same set is ALSO the all-accounts-high census set, which gives the
/// disabled-account rule a second consequence worth stating: [`crate::daemon`]'s poll
/// schedule skips a disabled (#36) or quarantined (#42) NON-ACTIVE account, so one of those
/// contributes no samples and the census over a roster holding it reports UNKNOWN for the
/// whole window. (Only non-active — the ACTIVE account is polled even while disabled or
/// quarantined, since its swap-AWAY trigger must still fire, so a disabled ACTIVE account
/// keeps contributing until the daemon rotates off it.) That UNKNOWN is the gap-honest
/// reading — the roster genuinely was not wholly observable — and it is the reading
/// REQ-STA-B-005's amendment pins ("the CONFIGURED roster … an account with zero samples
/// SHALL NOT silently leave the intersection"). Narrowing the census to enabled accounts
/// would be a fourth unstated parameter, and it is the very move the amendment forbids, so
/// it is deliberately NOT made here; if the UNKNOWN proves too blunt in practice, the
/// amendment is what should change.
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
    /// The session ceiling as a fraction — the neutral head-room reference for the session
    /// runway (`(ceiling − current) / rate`): the not-cross line, stated as a fact. (The daemon
    /// actually fires BACKWARD from it by the tail margin + lookahead, so it acts strictly below;
    /// the runway readout deliberately references the raw ceiling, not that derived fire point.)
    session_ceiling: f64,
    /// The weekly ceiling as a fraction — the weekly-runway reference.
    weekly_ceiling: f64,
}

fn velocity_params_from(config: Option<&Config>) -> VelocityParams {
    let (alpha_pct, session, weekly) = match config {
        Some(c) => (
            c.tunables.session_velocity_ema_alpha_pct,
            c.tunables.session_ceiling,
            c.tunables.weekly_ceiling,
        ),
        None => {
            let t = Tunables::default();
            (
                t.session_velocity_ema_alpha_pct,
                t.session_ceiling,
                t.weekly_ceiling,
            )
        }
    };
    VelocityParams {
        session_ema_alpha: f64::from(alpha_pct) / 100.0,
        session_ceiling: f64::from(session) / 100.0,
        weekly_ceiling: f64::from(weekly) / 100.0,
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

/// The plausibility bound on a per-account SESSION runway: ONE rolling session window.
///
/// The DERIVATION, not the number, is what carries over from [`FLEET_RUNWAY_PLAUSIBLE_MAX_SECS`].
/// `(trigger − current) / rate` computes a drain time that ignores replenishment entirely — it asks
/// "how long to reach the trigger at this burn" as if the window never reset. What bounds that
/// question is **the dimension's OWN reset cadence**, and seven days is merely what that cadence
/// happens to be for the weekly dimension. The session quota rides the rolling 5-hour session
/// window ([`crate::usage::Usage::session`], [`crate::usage_store::Sample::session`]), so **any
/// session runway exceeding one session window asserts the account drains with no session reset
/// intervening, which cannot happen.**
///
/// Sharing the weekly bound here would wave absurd session figures straight through: 7 days is
/// 33.6 session windows, so a 3-day "session runway" — asserting ~14 intervening resets — would
/// pass it. The converse fails just as badly: bounding the weekly arm at 5 hours would refuse
/// nearly every legitimate weekly runway. One bound for both dimensions is a wrong answer even
/// where the arithmetic is right.
///
/// The window ROLLS; usage does not drain continuously inside it. Those are separate claims, and
/// only the first is the crate's. [`crate::swap::SessionHighWater`] (issue #614, calibrated over
/// 24,244 measured samples) holds the model this bound rests on: session usage is *"MONOTONIC
/// within a window"* — *"the 5 h window only accumulates"* — so *"a DROP inside one window is
/// implausible"*, and the only legitimate fall is the discrete roll, observed there as a cluster at
/// 17999–18001 s. Accumulate, then reset; never a continuous ageing-out that returns each consumed
/// unit one window later.
///
/// Under that model the bound is an IDENTITY, not an approximation — which is the strongest form
/// the claim takes. For a finite, strictly positive rate:
///
/// ```text
/// runway > W   ⟺   (ceiling − current) / rate > W   ⟺   current + rate·W < ceiling
/// ```
///
/// and the right-hand side says exactly *the ceiling is unreachable before this window resets*.
/// Accumulation is all that happens until the reset, so a runway past one window does not describe
/// a distant crossing — it describes a crossing this window cannot contain **at all**. Refusing it
/// is not caution about a large number; it is declining to state an event that does not occur.
/// Concretely, at `current = 0.10` and `rate = 1e-5` frac/s the reading tops out at
/// `0.10 + 1e-5 × 18000 = 0.28`: the quotient's confident `~19h` to a `0.80` ceiling is a crossing
/// that never happens, in this window or any later one.
///
/// One WINDOW, deliberately, rather than the time to this account's next reset.
/// [`crate::usage_store::Sample::session_resets_at`] does carry that stamp, but it is `Option` —
/// absent whenever the poll did not know it — and it moves with the window's phase. With `τ ≤ W`
/// left before the reset the phase-EXACT bound would be `τ`, so using `W` is the conservative
/// choice in the one direction that matters: it can only admit figures a phase-aware rule would
/// refuse, never refuse one that rule would admit. Refusal stays sound at every phase, since
/// `current + rate·τ ≤ current + rate·W < ceiling`.
const SESSION_RUNWAY_PLAUSIBLE_MAX_SECS: i64 = 5 * HOUR_SECS;

/// The plausibility bound on a per-account WEEKLY runway: ONE weekly window.
///
/// This arm SHARES [`FLEET_RUNWAY_PLAUSIBLE_MAX_SECS`]'s derivation — the weekly quota resets on
/// its own ~7-day cycle, so a runway past one weekly window asserts a drain with no reset
/// intervening — and equals it by construction. Kept a separate name because the SUBJECT differs:
/// that constant bounds the pooled roster and argues its independence from roster size, while this
/// bounds ONE account, the primitive that argument generalises. Reading either as the other's alias
/// would make a fleet-scoped rationale look like authority over a per-account figure.
const WEEKLY_RUNWAY_PLAUSIBLE_MAX_SECS: i64 = 7 * DAY_SECS;

/// Approximate whole seconds until `current` reaches `trigger` at `rate` (fraction/second):
/// `(trigger − current) / rate`, refused unless the result is PLAUSIBLE. `None` — NEVER a sentinel
/// — for every refusal alike, which is all the two consumers need: the `runway` cell renders the
/// gap sentinel `—` and the wire emits explicit `null`, neither of which distinguishes *why*.
///
/// `max_plausible_secs` is the caller's DIMENSION-SPECIFIC bound
/// ([`SESSION_RUNWAY_PLAUSIBLE_MAX_SECS`] / [`WEEKLY_RUNWAY_PLAUSIBLE_MAX_SECS`]) — a parameter
/// rather than one shared constant precisely because the two dimensions reset on different
/// cadences; each constant's doc derives its own.
///
/// Four gates, mirroring [`fleet_runway_state`]'s order (issue #1075 — this is the PER-ACCOUNT
/// sibling, and it carried both faults #1028 removed from that one until now):
///
/// 1. **The rate must be known**, and otherwise finite and strictly positive. A flat or idle
///    dimension has no drain to divide by, and a non-finite one was never a measurement.
/// 2. **The head-room must be finite and strictly positive.** At or over the trigger there is no
///    positive head-room left to state as a neutral fact.
/// 3. **The quotient must fall within `max_plausible_secs`**, compared in `f64` BEFORE converting —
///    an `as i64` cast would saturate the very overflow being rejected. This is the gate that
///    refuses a vanishing decayed EMA: gate 1 cannot, because `1e-11` is finite and positive and
///    so a perfectly well-formed rate. Expressing the bound on the RESULT, in seconds, is also
///    what keeps it readable — an input-side epsilon on the rate would be arbitrary, unit-coupled
///    to the EMA's native fraction-per-second, and would drift silently if that unit ever changed.
/// 4. **The conversion must succeed** ([`checked_runway_secs`]), or there is no whole-second figure
///    to state.
///
/// Deliberately NOT a clamp. Clamping an absurd figure to a large plausible one converts an
/// obviously-wrong number into a CREDIBLE one, which is strictly harder to notice — refusal is the
/// fix. That is also why gate 1 refuses a NON-FINITE rate rather than letting it divide: a `NaN`
/// rate slipped past the old `rate <= 0.0` guard (every `NaN` comparison is false) and
/// `NaN.round() as i64` is `0`, so it surfaced as `~0s` — "exhausted right now" — the one failure
/// mode of the three that produces a small, entirely credible figure.
fn runway_secs(
    rate: Option<f64>,
    current: f64,
    trigger: f64,
    max_plausible_secs: i64,
) -> Option<i64> {
    let rate = rate?;
    if !rate.is_finite() || rate <= 0.0 {
        return None;
    }
    if !current.is_finite() || !trigger.is_finite() || current >= trigger {
        return None;
    }
    let secs = ((trigger - current) / rate).round();
    // A non-finite quotient here can ONLY be `+inf`, and only from overflow: the gates above leave
    // the numerator finite and strictly positive and the denominator finite and strictly positive,
    // so `0/0` and `inf/inf` — the only routes to `NaN` — are both unreachable. Overflow happens
    // for a real, MEASURED burn once the EMA decays subnormal, and it is refused for the same
    // reason a merely enormous quotient is: neither is a runway an operator can read.
    if !secs.is_finite() || secs > max_plausible_secs as f64 {
        return None;
    }
    checked_runway_secs(secs)
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
        // Each arm carries its OWN dimension's plausibility bound (issue #1075) — the session
        // quota resets every rolling 5 h and the weekly one every ~7 days, so one shared bound
        // would either wave through absurd session figures or refuse legitimate weekly ones.
        session_runway_secs: runway_secs(
            session_rate,
            last.session,
            vparams.session_ceiling,
            SESSION_RUNWAY_PLAUSIBLE_MAX_SECS,
        ),
        weekly_runway_secs: runway_secs(
            weekly_rate,
            last.weekly,
            vparams.weekly_ceiling,
            WEEKLY_RUNWAY_PLAUSIBLE_MAX_SECS,
        ),
        // The pool contribution for the fleet aggregate (issue #544): raw weekly head-room from the
        // SAME latest reading and trigger the weekly runway uses, recorded ONLY when the weekly
        // velocity is known (so an unknown / stale account contributes neither head-room nor burn).
        // Clamped at `0` — an over-trigger account is exhausted (no spare capacity), never negative.
        weekly_headroom: weekly_rate.map(|_| (vparams.weekly_ceiling - last.weekly).max(0.0)),
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
///
/// `pub(crate)` since issue #650: the daemon's proactive fleet-runway warning probes the SAME
/// aggregate ([`current_fleet_runway`]) — the type crosses the module boundary so the warning
/// cannot drift into a parallel metric. The WIRE shape stays [`FleetWire`]; this is internal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FleetRunway {
    /// What the pooled computation yielded — the figure, or WHY there is none.
    ///
    /// The single source of truth for both: [`Self::runway_secs`] derives the wire figure from it, so
    /// a state and a figure cannot disagree. Carrying the reason (rather than re-deriving it at each
    /// surface from the pooled rate) is what lets a surface STATE its unknown (R-3 / R-20) and lets
    /// the daemon's warn edge tell "demonstrably not near exhaustion" apart from "genuinely
    /// unknown" — two questions a bare `Option` cannot answer.
    ///
    /// NOT on the wire — [`fleet_wire`] carries the derived `Option<i64>` and the cardinality only,
    /// so `stats`' `JSON_SCHEMA_VERSION` is untouched.
    pub(crate) state: FleetRunwayState,
    /// Accounts that CONTRIBUTED to the aggregate — those with a KNOWN weekly velocity (the
    /// honest-degradation gate). The `n` of the surfaced `n of m`.
    pub(crate) counted: usize,
    /// Accounts OBSERVED in the window (`seen > 0`) — the `m` of `n of m`. `observed − counted` were
    /// EXCLUDED for an unknown / stale weekly velocity: surfaced as a fact, never silently dropped.
    pub(crate) observed: usize,
}

/// What pooling the roster's weekly head-room over its weekly burn yielded — a figure, or WHY there
/// is none (issue #1028).
///
/// The three unknowns are kept DISTINCT rather than collapsed into one `None` because two consumers
/// must tell them apart. A surface states the reader-meaningful condition it represents (R-3 / R-20 —
/// an unknown fact is stated, never omitted); the daemon's warn edge treats only ONE of them as
/// evidence about the fleet's actual position. Collapsing them forces each consumer to re-derive the
/// reason from the pooled rate, which is both a second implementation of the decision and unable to
/// distinguish [`Self::BeyondWeeklyWindow`] from [`Self::Unmeasurable`] at all.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FleetRunwayState {
    /// A PLAUSIBLE finite runway, in whole seconds — the only state that yields a figure.
    Known(i64),
    /// Every counted account is FLAT: measured, and none of it burning, so there is no drain to
    /// divide the pooled head-room by. Honest degradation, not a fault.
    Flat,
    /// The pooled head-room outlasts one weekly window at the pooled burn
    /// ([`FLEET_RUNWAY_PLAUSIBLE_MAX_SECS`]).
    ///
    /// Not a figure to state — the computation ignores replenishment, so any horizon past the first
    /// reset asserts a drain no reset interrupts, which cannot happen. But it IS positive evidence
    /// about the fleet: the drain is slower than one reset cycle, which answers "is the fleet near
    /// exhaustion?" with a definite NO for any threshold at or below that window. That is why the
    /// daemon's warn edge can act on this state while it must hold on [`Self::Unmeasurable`].
    BeyondWeeklyWindow,
    /// The INPUTS yielded nothing usable — a non-finite or negative rate or head-room. No figure,
    /// and no evidence about the fleet's position either.
    ///
    /// Inputs only. A non-finite QUOTIENT is [`Self::BeyondWeeklyWindow`], not this: it comes from
    /// a burn that was measured and merely tiny, so it still says the pool outlasts a week. The
    /// checked conversion ([`checked_runway_secs`]) also falls back here when it refuses, but the
    /// gates above make that arm unreachable — it is a fail-closed backstop, not a fifth
    /// classification. The conversion is nonetheless tested, by calling it directly rather than
    /// through [`fleet_runway_state`], which is what leaves this arm's status unchanged (#1081).
    Unmeasurable,
}

impl FleetRunway {
    /// The figure for the WIRE: `Some` ONLY for a plausible [`Known`] runway.
    ///
    /// Derived from [`Self::state`] rather than stored beside it, so the two can never disagree —
    /// a fixture cannot pair a `Some` figure with a state that says there is none.
    ///
    /// A DECISION should read [`Self::state`] instead, as `check_fleet_runway_warn` does. This
    /// projection collapses all three unknowns into one `None`, which is right for a wire that has
    /// no field to carry the reason and wrong for anything that must tell "demonstrably not near
    /// exhaustion" from "genuinely unknown".
    ///
    /// [`Known`]: FleetRunwayState::Known
    pub(crate) fn runway_secs(self) -> Option<i64> {
        match self.state {
            FleetRunwayState::Known(secs) => Some(secs),
            FleetRunwayState::Flat
            | FleetRunwayState::BeyondWeeklyWindow
            | FleetRunwayState::Unmeasurable => None,
        }
    }
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
///   fleet runway ≈  Σ_counted max(0, weekly_ceiling − weekly_now)  ÷  Σ_counted weekly_rate
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
/// when no account could be counted — there is then no counted fleet to state anything ABOUT, and no
/// cardinality to state it with.
///
/// A `Some` whose [`FleetRunwayState`] is not `Known` is a COUNTED fleet whose runway is an explicit
/// unknown ([`fleet_runway_state`] classifies which). Every such state still surfaces the cardinality
/// and is STATED rather than omitted at the render (R-3 / R-20).
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
    Some(FleetRunway {
        state: fleet_runway_state(total_headroom, total_rate),
        counted,
        observed,
    })
}

/// The plausibility bound on a fleet runway: ONE weekly window.
///
/// `weekly_headroom` is a usage FRACTION and `weekly_rate` is fraction-per-SECOND, so their quotient
/// is SECONDS. That computation ignores replenishment entirely — it asks "how long to drain the
/// pooled head-room at the pooled burn" as if no window ever reset. But every account's weekly quota
/// resets on its own ~7-day cycle, so **any runway exceeding one weekly window asserts the fleet
/// drains with no reset intervening, which cannot happen.**
///
/// The bound is INDEPENDENT of roster size: pooling head-room across N accounts does not push the
/// horizon past the first reset, because the resets are what refill the pool.
pub(crate) const FLEET_RUNWAY_PLAUSIBLE_MAX_SECS: i64 = 7 * DAY_SECS;

/// Pool the head-room over the burn and classify the result (issue #1028).
///
/// Four gates, in this order, each load-bearing:
///
/// 1. **A zero burn is FLAT**, not a fault — every counted account was measured and none of it was
///    burning, so there is simply no drain to divide by.
/// 2. **The inputs must otherwise be finite and sane.** Anything else is
///    [`Unmeasurable`](FleetRunwayState::Unmeasurable): no figure, and no evidence either way.
/// 3. **The quotient must fall within one weekly window** ([`FLEET_RUNWAY_PLAUSIBLE_MAX_SECS`]),
///    compared in `f64` BEFORE converting — a `f64 as i64` cast would saturate the very overflow
///    being rejected. Past the window, AND on an overflow to `inf`, the answer is
///    [`BeyondWeeklyWindow`](FleetRunwayState::BeyondWeeklyWindow): both are the same statement, that
///    the pool outlasts a week, and an infinite quotient is merely the emphatic form of it. That is
///    distinct from `Unmeasurable`, because it still tells a decision-maker the drain is slower than
///    one reset cycle.
/// 4. **The conversion must succeed** ([`checked_runway_secs`]), or there is no whole-second figure
///    to state.
///
/// Note what gate 1 does NOT do: it does not reject a merely SMALL rate. A decayed EMA of `1e-11` is
/// finite and positive, so it passes — and at the reproduction's pooled head-room (≈0.68) yields
/// ~787,000 days, the order of the `~648427 days` this issue reported. The head-room is named
/// because the figure is a QUOTIENT: at another head-room the same rate yields another horizon, so
/// a reader recomputing from the rate alone cannot land on this number. The prior guard was written
/// for the flat case (`total_rate > 0.0`) and so read a vanishing burn as a real one. What refuses
/// it is the RESULT bound at gate 3, which is the point: the bound is expressed in the unit an
/// operator can check, not in the EMA's native fraction-per-second (where a "small" constant is
/// unreadable and drifts if the unit ever changes — the input-side epsilon this issue explicitly
/// rejected).
///
/// The corollary is that `Unmeasurable` is a statement about the INPUTS only. Nothing about a
/// well-formed pooled reading lands there, however extreme — a slower burn must never be read as
/// carrying LESS information than a faster one.
///
/// The conversion is CHECKED, never `as i64`. That gate is [`checked_runway_secs`], extracted so it
/// is REACHABLE on its own (issue #1081): gate 3 leaves it nothing to refuse, so its independence
/// from gate 3 — the R-4 defence-in-depth claim — was prose no test could exercise, and measurably
/// ungated (a mutation swapping the checked conversion for a saturating `as i64` passed the entire
/// suite). The claim, and the reasoning behind it, now live on that function with the test that
/// pins them.
///
/// Deliberately NOT a clamp. Clamping an absurd figure to a large plausible one converts an
/// obviously-wrong number into a CREDIBLE one, which is strictly harder to notice — refusal is the fix.
fn fleet_runway_state(headroom: f64, rate: f64) -> FleetRunwayState {
    if rate == 0.0 {
        return FleetRunwayState::Flat;
    }
    // Only the INPUTS can be unmeasurable. Once they are known good, every outcome below is a
    // statement about the fleet, including the overflowing one.
    if !rate.is_finite() || rate < 0.0 || !headroom.is_finite() || headroom < 0.0 {
        return FleetRunwayState::Unmeasurable;
    }
    let secs = (headroom / rate).round();
    // A non-finite quotient here can ONLY be `+inf`, and only from overflow: the guards above leave
    // `headroom` finite and non-negative and `rate` finite and strictly positive, so `0/0` and
    // `inf/inf` — the only routes to `NaN` — are both unreachable, and the sign is necessarily
    // positive. Overflow happens for a real, MEASURED burn once the EMA decays subnormal (with the
    // fleet's pooled head-room, below ~6.6e-309), and an infinite quotient is the STRONGEST evidence
    // the pool outlasts a week — so it belongs with the other out-of-window readings, not with the
    // unmeasurable ones. Filing it as unmeasurable would tell an operator their burn "could not be
    // measured" when it was measured and merely tiny, and would deny the daemon's warn edge the
    // recovery evidence it is entitled to (see `check_fleet_runway_warn`).
    if !secs.is_finite() || secs > FLEET_RUNWAY_PLAUSIBLE_MAX_SECS as f64 {
        return FleetRunwayState::BeyondWeeklyWindow;
    }
    // `secs` is now finite, inside the window, and non-negative (non-negative numerator, strictly
    // positive denominator), so gate 4 already holds for every value that can arrive here: the
    // `Unmeasurable` arm is a fail-closed backstop that outlives any later relaxation of gate 3,
    // not a reachable outcome. That is why gate 4 is TESTED through `checked_runway_secs` directly
    // rather than through this function — no argument pair reaches it with a value it would refuse.
    match checked_runway_secs(secs) {
        Some(secs) => FleetRunwayState::Known(secs),
        None => FleetRunwayState::Unmeasurable,
    }
}

/// Gate 4 of [`fleet_runway_state`] as its own function: the CHECKED conversion of a rounded second
/// count into the `i64` a [`Known`](FleetRunwayState::Known) runway carries. `None` is "no
/// whole-second figure to state".
///
/// SHARED with the per-account [`runway_secs`], which is gate-for-gate the same decision one
/// account down (issue #1075) and reaches this as its own gate 4. One implementation rather than
/// two, because a second copy of a conversion whose whole purpose is to fail closed is a second
/// place for it to fail open — and the reasoning below then has to hold in only one place. Every
/// claim here is about THIS function's arguments, so it reads identically for both callers.
///
/// That second caller does NOT make the gate reachable through a caller: its own gate 3 bounds
/// `secs` to `0..=max_plausible_secs` first, exactly as the fleet's does, so the independence
/// argument below — and the direct test that carries it — still speaks for both. Neither caller's
/// bound is this function's business; it refuses the same three shapes whoever asks.
///
/// TOTAL on its own, independent of gate 3 — the R-4 claim, and the reason this is a separate
/// function at all (issue #1081). Gate 3 has already bounded `secs` to
/// `0..=`[`FLEET_RUNWAY_PLAUSIBLE_MAX_SECS`] before any call from `fleet_runway_state`, so no such
/// call can make this answer `None`; reaching the gate from a test therefore requires reaching it
/// HERE, with a value gate 3 would have refused. Doing so does not change the reachability of that
/// function's `Unmeasurable` arm, which stays the fail-closed backstop its own doc describes — this
/// returns `None`, and only `fleet_runway_state` turns a `None` into a classification.
///
/// Three ways an unchecked `secs as i64` fails open, all refused here:
///
/// - **Above the range.** A float→int `as` cast SATURATES rather than trapping (Rust 1.45+), which
///   is exactly how an overflowing quotient reached the surface as `i64::MAX`
///   (106,751,991,167,300 days) instead of failing (issue #1028). Widening to `i128` first cannot
///   launder it — a saturated `i128::MAX` is still out of `i64` range, so [`i64::try_from`] rejects
///   it.
/// - **Below zero.** A negative count saturates toward `i64::MIN` rather than refusing, and a
///   NEGATIVE runway would state a figure as confidently as a positive one.
/// - **Not a number.** `NaN as i128` is `0`, not a saturated sentinel — the one failure mode that
///   produces a small, entirely CREDIBLE figure. A `Known(0)` runway reads as "exhausted now" and
///   would FIRE the daemon's warn edge, so the finite check is the fail-closed half of this gate,
///   not a duplicate of gate 3's: gate 3 classifies a non-finite QUOTIENT as out-of-window, while
///   this refuses to convert one at all.
///
/// The last is inert for every call `fleet_runway_state` can make (gate 3 rejects non-finite first)
/// and is what makes "relaxing either gate later cannot silently reintroduce a fabricated figure"
/// true of this function rather than merely of the pair.
fn checked_runway_secs(secs: f64) -> Option<i64> {
    if !secs.is_finite() {
        return None;
    }
    match i64::try_from(secs as i128) {
        Ok(secs) if secs >= 0 => Some(secs),
        _ => None,
    }
}

/// The CURRENT #544 fleet-runway aggregate, read fresh from the on-disk store — the daemon's
/// probe for the proactive `fleet_runway_low` warning (issue #650). Runs the SAME pipeline the
/// `stats` socket verb serializes — store read → window plan → config-derived params →
/// [`build_report`] + [`with_velocity`] → [`fleet_runway`] — so the probed figure IS the figure
/// `sessiometer stats` shows, never a parallel aggregation path (the issue's reuse mandate).
///
/// The window is PINNED to the socket default (a `None` period → `week`, [`plan_window`]), NOT
/// `[stats].default_period`: the warning is a stable signal, not a display, so an operator's
/// reporting-period preference must not move the fire point. The config resolve is the same
/// tolerant [`resolve_stats_config`] the socket verb uses (#642) — a malformed / absent
/// `config.toml` falls back to default tunables so the probe still answers (the fault is already
/// logged + surfaced by the other consumers of that resolve; the probe stays quiet).
///
/// `None` on ANY degradation — unreadable store, or the aggregate's own honest-degradation gates
/// (no velocity overlay, no observed / counted account) — the caller then HOLDS its prior edge
/// state: a hiccup neither fires nor re-arms the warning. Blocking `std::fs` read; the daemon
/// cadence-gates the call far below the poll rate (the #161 maintenance-layer precedent for
/// inline store IO in the tick).
pub(crate) fn current_fleet_runway() -> Option<FleetRunway> {
    let now = wall_clock_now();
    let offset = local_offset_secs(now);
    let data = NativeHistoryStore::from_paths()
        .and_then(|store| StoreData::read(&store))
        .ok()?;
    // `(None, None)` cannot fail (`plan_window` maps it to the rolling `week` window), but stay
    // tolerant rather than unwrap — a probe must never panic the tick.
    let window = plan_window(None, None, now, &data).ok()?;
    let (config, _config_fault) = resolve_stats_config(Config::load());
    let report = overlaid_report(&data, window, Vec::new(), config.as_ref(), offset);
    fleet_runway(&report)
}

/// Build the velocity-overlaid [`Report`] — the shared `params → vparams → roster →
/// with_velocity(build_report)` spine every stats consumer runs (issue #693). The CLI render
/// ([`run_output`]), the socket verb ([`stats_socket_json_with`]), and the daemon runway probe
/// ([`current_fleet_runway`]) overlay a snapshot the SAME way; written out per-site, a change to
/// overlay semantics had to land in three places and a missed one diverged silently — the CLI and
/// the socket would disagree about the same snapshot (the R-2 parity #543 keeps). One helper makes
/// that divergence unrepresentable.
///
/// Derives the aggregator thresholds ([`params_from`]), the velocity tunables
/// ([`velocity_params_from`]), and the roster partition ([`roster_handles`]) from the ONE resolved
/// `config` each caller already holds, then overlays the velocity + runway readout
/// ([`with_velocity`]) onto the base aggregate ([`build_report`]). `accounts` is the sole per-site
/// difference — the CLI passes its `--account` filter; the socket and the runway probe pass an empty
/// filter (the whole roster).
fn overlaid_report(
    data: &StoreData,
    window: Window,
    accounts: Vec<String>,
    config: Option<&Config>,
    offset: i64,
) -> Report {
    let params = params_from(config);
    let vparams = velocity_params_from(config);
    let roster = config.map(roster_handles);
    with_velocity(
        build_report(data, window, accounts, roster.as_ref(), &params, offset),
        &data.samples,
        &params,
        &vparams,
    )
}

/// Aggregate the window's samples into a filtered summary + series.
///
/// The summary is one whole-window `aggregate`; the series is one `aggregate` per bucket.
/// Roster-wide statistics (swap frequency, all-high) are computed over the FULL roster;
/// the account filter then restricts only which per-account rows are displayed, so a
/// filtered view never distorts the roster picture. `roster` reaches the aggregate itself
/// (issue #804) — the all-accounts-high census intersects over the CONFIGURED handles, so a
/// rostered account with no samples cannot silently leave the intersection and an orphan
/// handle cannot silently join it.
fn build_report(
    data: &StoreData,
    window: Window,
    accounts: Vec<String>,
    roster: Option<&BTreeSet<String>>,
    params: &AggregateParams,
    offset: i64,
) -> Report {
    let swaps = parse_swap_events(&data.events);

    let mut summary = aggregate_with_roster(
        &data.samples,
        &swaps,
        Period::new(window.start, window.end),
        params,
        roster,
    );
    apply_filter(&mut summary.per_account, &accounts);
    // Split non-roster handles out of the SUMMARY view — they render in their own section
    // (issue #314), never as peers of live accounts. The summary partition is the one every
    // view surfaces; the series buckets are cleaned below only so they never PLOT an orphan.
    let orphans = split_orphans(&mut summary.per_account, roster);

    let series = bucket_bounds(window.start, window.end, window.base_bucket())
        .into_iter()
        .map(|(lo, hi)| {
            let mut bucket =
                aggregate_with_roster(&data.samples, &swaps, Period::new(lo, hi), params, roster);
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
        // Empty here AND on every other path this crate can build (issue #883) — see the field's
        // own doc: the offline `HistoryStore` seam carries no refresh-token deadline until issue
        // #880's durable horizon Event exists to be read out of the event log.
        expiry: BTreeMap::new(),
        // Read off the SAME `roster` the census above consumed, not off the caller's config
        // (issue #836) — the render's claim about which set was intersected then cannot drift
        // from the set that actually was.
        census_over_roster: roster.is_some(),
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
/// (`swap_count`, all-high) are computed by [`aggregate_with_roster`] BEFORE this split and
/// are independent of this display subset, exactly as they already are under
/// [`apply_filter`]. They are not independent of `roster` itself, though: since issue #804
/// the same set drives the all-accounts-high census, so an orphan is excluded from that
/// census by the aggregate rather than merely hidden from the table by this split.
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

// The per-account NUMERIC-text table is the WIDER surface (design-stats.md §D-STA-5): it carries
// `signal`, `cov`, the full `session` / `weekly` `m/p/p95` triples, `caps` / `t@cap` / `share`, and
// the shared `velocity` / `runway`. Since issue #557 it is NOT laid out by its own renderer —
// [`render_text`] builds one [`AccountRow`] per account and renders the [`piped_columns`] subset
// through the ONE [`render_account_table`] at `w = usize::MAX`, `color = false`, so the piped and
// TTY tables share the elision / width / drop machinery and can no longer silently diverge in
// column shape. A per-account column now lives in exactly one [`Column`] catalog entry, shown on
// each surface by subset choice; the former hand-built `text_table_header` / `text_account_row`
// (fixed-width, right-aligned) are gone.

/// Render the numeric text view: the window echo, the per-account summary table, an optional
/// "not in roster" section (issue #314), the neutral summary band (issue #160), and the
/// roster line. This is the NON-TTY surface (issue #159): a piped / redirected `stats`
/// renders exactly this — plain, greppable, zero ANSI, no chart glyph — while an interactive
/// TTY gets [`render_charts`]. Reports only magnitudes and neutral descriptors — no
/// recommendation, no forecast (issue #160).
///
/// `config_unreadable` is [`render_human`]'s issue #642 provenance, rendered by
/// [`config_regime_line`] directly above the roster line it qualifies (issue #836).
fn render_text(report: &Report, config_unreadable: Option<&str>) -> String {
    let mut out = String::new();
    let label = format_window_label(&report.window, report.offset);
    out.push_str(&format!("usage — {label}\n\n"));

    let summary = &report.summary;
    let has_live = !summary.per_account.is_empty();
    let has_orphans = !report.orphans.is_empty();
    if !has_live && !has_orphans {
        out.push_str("  no per-account usage in this window\n");
    } else {
        // The numeric view is the WIDER surface: one [`AccountRow`] per account, rendered through
        // the ONE [`render_account_table`] over the [`piped_columns`] subset at `w = usize::MAX`
        // (never priority-drops — the pipe is the full-width table) and `color = false` (zero ANSI,
        // issue #159). The live table and the #314 "not in roster" section are sibling SECTIONS of
        // one call, so they foot IDENTICAL column widths and the empty-column `velocity` / `runway`
        // elision is decided across BOTH — the cross-section discipline the hand-built renderer
        // carried, now shared with the TTY chart table (issue #557). The `account` column is sized
        // on DISPLAY width by the renderer (issue #249). ASCII is irrelevant here (the piped subset
        // omits `trend`); `true` is a harmless placeholder.
        let mut sections: Vec<AccountSection> = Vec::new();
        if has_live {
            sections.push(AccountSection {
                heading: None,
                rows: account_rows(report, summary.per_account.iter(), true),
            });
        }
        if has_orphans {
            sections.push(AccountSection {
                heading: Some(format!("not in roster ({}):", report.orphans.len())),
                rows: account_rows(report, report.orphans.iter(), true),
            });
        }
        out.push_str(&render_account_table(
            &sections,
            &piped_columns(),
            usize::MAX,
            false,
        ));
    }

    out.push('\n');
    // The bottom roster block (§D-STA-5): the aggregate-only summary (lowest-utilisation +
    // fleet-runway), then the config-regime caveat, then the roster line — ONE contiguous
    // block, no blank line between. The summary carries no trailing newline, so the
    // terminating `\n` here abuts whatever follows. The caveat sits directly ABOVE the line it
    // qualifies (issue #836) so it is read before the census number, not after it.
    let band = render_summary(report);
    if !band.is_empty() {
        out.push_str(&band);
        out.push('\n');
    }
    if let Some(line) = config_regime_line(config_unreadable) {
        out.push_str(&line);
    }
    out.push_str(&roster_line(summary, report.census_over_roster));
    out
}

/// The roster summary line (issue #158): swap frequency broken out by reason and the
/// all-accounts-high census. Extracted so the numeric [`render_text`] and the charts
/// [`render_charts`] (issue #159) foot the view with the IDENTICAL roster sentence.
///
/// The census states the water it used (issue #804) rather than leaving the reader to guess
/// which threshold produced the number — the defect that had three surfaces quoting three
/// different values. Under insufficient joint coverage it renders the view's own gap
/// sentinel `—`, NEVER `0 episodes (0s)`: an unmeasurable period is not a calm one, and a
/// bare `0` is indistinguishable from a genuinely quiet week (REQ-STA-B-008).
///
/// Takes the whole [`UsageReport`] rather than its `roster` alone so the coverage
/// annotation's denominator — the window the census was measured over — is read from the
/// SAME report as the numerator and cannot be mismatched by a caller. The wire needs no
/// equivalent field: `all_high_covered_secs` rides on it alongside the window's own `start`
/// / `end`, so every surface can derive the same share.
///
/// It also states its SET, for exactly the reason it states its water (issue #836).
/// `census_over_roster` is [`Report::census_over_roster`] — `false` means no roster was known
/// and the census degraded to the sampled accounts, where an unsampled account cannot withhold
/// the metric and it therefore fires more readily. The qualifier rides the census label's own
/// parenthetical, beside the water, because both are parameters of the same reading; a reader
/// who greps out this line cannot separate the number from the set that produced it.
///
/// STATED ONLY WHEN THE CENSUS WAS TAKEN. `—` is the UNKNOWN sentinel: it carries no confident
/// number to misread and reads the same under both regimes ("I could not see"), so naming the
/// set there would describe a measurement that never happened — and, since a pre-`capture`
/// install has no roster AND nothing to report, it would put a permanent qualifier on the most
/// common render of all for no informational gain. Note this is NOT the same rule
/// [`capacity_holds_cell`] follows: that cell drops its boundary on `—` because the carried
/// lines are then `0.0`, whereas the water here is carried and IS still stated on `—`. So one
/// parameter of this cell is stated on a non-reading and the other is not, deliberately.
///
/// The residual, stated rather than implied: an ABSENT config with an untaken census gets
/// neither this qualifier (suppressed) nor [`config_regime_line`]'s caveat (no fault to report),
/// so those two regimes do still print identical bytes. That corner is the one where the
/// distinction carries no operator consequence — both readings are `—` — which is why it is
/// accepted here rather than closed.
///
/// The capacity-holds cell beside this one degrades over the SAME `roster` and is deliberately
/// NOT annotated by this change (issue #836 is scoped to the census); that asymmetry is tracked
/// as issue #864. Until it lands, a reader who notices one cell carrying a regime qualifier may
/// infer the neighbour is regime-independent — it is not.
fn roster_line(report: &UsageReport, census_over_roster: bool) -> String {
    let r = &report.roster;
    let period_secs = report.period.duration();
    // "Insufficient" is taken at its most conservative: NO jointly-covered instant at all.
    // A stricter CUTOFF (below X% of the window ⇒ UNKNOWN) would be a fourth unstated
    // parameter, and pinning one is not this fix's job.
    let census = if r.all_high_covered_secs > 0 {
        let mut detail = fmt_dur(r.all_high_secs);
        // A conservative bar alone would leave the reported defect reachable: one covered
        // second in a week reads as a confident calm, which is the very thing "a bare 0 is
        // indistinguishable from a genuinely quiet week" forbids. So a partly-covered period
        // ANNOTATES itself, as REQ-STA-B-008 requires ("low-coverage periods SHALL be
        // annotated") — the measured share, in its own percent. That is not a fourth
        // parameter: the bar for annotating is wholly-covered-or-not, the module's own
        // `Complete` / `Partial` line, and the number shown is measured, never chosen.
        //
        // No `period_secs > 0` guard is needed to keep the division safe: this branch already
        // has `all_high_covered_secs >= 1`, so reaching the body forces `period_secs >= 2`.
        if r.all_high_covered_secs < period_secs {
            let share = r.all_high_covered_secs as f64 / period_secs as f64;
            // Rounding must not manufacture a whole the share is NOT. Sub-1% would read `0%`
            // and over-99% would read `100%` — and BOTH are false in here, where coverage is
            // strictly between nothing and everything, which is the one thing this annotation
            // exists to say. A render that states a falsehood is what this issue exists to
            // end, at either end of the scale.
            let shown = match pct(share) {
                0 => "<1".to_owned(),
                100 => ">99".to_owned(),
                whole => whole.to_string(),
            };
            // SAYS WHAT THE SHARE MEASURES, in the reader's words. This annotation used to read
            // `, {n}% covered`, which is the field's NAME (`all_high_covered_secs`) leaking onto
            // an operator-facing surface: it never answers *covered by what?*, and a reader who
            // cannot answer that cannot use the number. What it measures is how much of the
            // window the census could see the whole set at ONE moment — so that is what it now
            // says (issue #1029). "all" is the set the label beside it already named, so the
            // sampled-accounts fallback needs no second wording: `(≥95%, sampled accounts):
            // 0 episodes (0s, all in view 8% of the window)` reads correctly for whichever set
            // was censused, which naming a set here a second time would not.
            detail.push_str(&format!(", all in view {shown}% of the window"));
        }
        format!(
            "{} episode{} ({detail})",
            r.all_high_episodes,
            plural(r.all_high_episodes),
        )
    } else {
        // The view's own gap sentinel, the same glyph the `signal` / `velocity` / `runway`
        // cells degrade to — one UNKNOWN vocabulary across the surface.
        "—".to_owned()
    };
    // The census's SET, named beside its water and only when a reading was actually taken
    // (issue #836) — see this function's doc comment for why the `—` branch omits it.
    let set = if census_over_roster || r.all_high_covered_secs == 0 {
        String::new()
    } else {
        ", sampled accounts".to_owned()
    };
    format!(
        "roster: {} swap{} ({} session, {} weekly, {} manual, {} forced, {} emergency) · \
         all-accounts-high (≥{}%{}): {} · {}\n",
        r.swap_count,
        plural(r.swap_count),
        r.swaps.session,
        r.swaps.weekly,
        r.swaps.manual,
        r.swaps.forced,
        r.swaps.emergency,
        pct(r.high_threshold),
        set,
        census,
        capacity_holds_cell(r),
    )
}

/// The config-provenance caveat the human render places directly ABOVE the roster line when
/// `config.toml` exists but could not be read (issue #836), e.g.
/// `all-accounts-high fires more readily without a roster — config.toml is not valid TOML —
/// run `sessiometer config validate` for the detail`. `None` — nothing rendered — for a
/// readable config AND for an ABSENT one, which is the normal pre-`capture` state issue #627
/// deliberately keeps silent (its regime is already stated by [`roster_line`]'s own qualifier).
///
/// `reason` is the SAME static string [`wire_config_reason`] puts on the wire for issue #642,
/// so the human surface and `--json` cannot describe one config failure two ways. That type is
/// what makes printing it safe here: a `&'static str` cannot carry a byte of the operator's
/// `config.toml`, which is the whole reason #642 chose it for the wider surfaces — and stdout,
/// piped into a file or a screenshot, is one of them.
///
/// This is a SEPARATE annotation from the roster line's set qualifier, on a separate key,
/// because the two answer different questions: the qualifier says WHICH SET the census used
/// (true under an absent config too), while this says WHY there was no roster to use. It is
/// NOT gated on the census having fired — "fires more readily" is a property of the metric
/// under this regime, not a claim that it fired — so a broken config is stated whether or not
/// the window happened to yield a reading.
///
/// The stderr warning [`run_output`] already emits is not a substitute: it carries the FULL
/// operator-scoped parser detail to a stream that a `stats > file`, a dashboard, or a
/// screenshot does not capture, and it says nothing about the census's regime.
fn config_regime_line(reason: Option<&str>) -> Option<String> {
    reason
        .map(|reason| format!("all-accounts-high fires more readily without a roster — {reason}\n"))
}

/// The capacity-holds cell of the roster line (§D-STA-5, issue #803): `capacity holds (session
/// ≥80%, weekly ≥97%): 7 (2 session / 5 weekly) · ≥29h28m`.
///
/// Answers a DIFFERENT operator question from the census beside it — "could the daemon still
/// swap?" rather than "was the roster running hot?" — which is why it is a second cell and not a
/// re-rendering of the first. The two disagreed on the week that motivated it: the daemon was
/// cornered 95 times while the census read calm.
///
/// It STATES THE BOUNDARY it measured against, for the same reason the census beside it states its
/// water (issue #804): both lines are operator-configurable, so without them the figure's meaning
/// moves with the config while the render looks identical. The two are named per dimension rather
/// than positionally — a bare `≥80%/≥97%` pair would re-introduce, in the render, exactly the
/// silent transposition [`crate::swap::ViabilityBoundary`] is a named struct to prevent.
///
/// The duration carries a `≥` because it is a LOWER BOUND and never an exact figure
/// (REQ-STA-B-011): a coverage gap inside a hold truncates it, a hold still running at the
/// window's end is clipped to it, and the closing instant is anchored to the blocking window's
/// carried reset rather than observed. Under no joint coverage the whole cell degrades to the
/// view's own gap sentinel `—`, never `0 holds` — an unmeasurable period is not a calm one, the
/// same contract the census beside it keeps. The boundary is omitted from THAT branch on purpose:
/// with the census untaken the carried lines are `0.0`, and printing `≥0%` would state a line no
/// reading was ever measured against.
///
/// It does NOT yet state its SET, and the omission is a gap rather than a decision. The
/// `capacity_holds` aggregate (`src/usage_stats.rs`) intersects over the same `roster` the census
/// does and degrades the same way when none is known, so this cell has the two regimes issue #836
/// made the census beside it declare — it is simply out of that issue's scope, and is tracked as
/// issue #864. Until then the parenthetical asymmetry on the rendered line (a qualified census
/// beside an unqualified holds cell) reads as though holds were regime-independent. It is not.
fn capacity_holds_cell(r: &crate::usage_stats::RosterStats) -> String {
    if r.capacity_hold_covered_secs == 0 {
        return "capacity holds: —".to_owned();
    }
    format!(
        "capacity holds (session ≥{}%, weekly ≥{}%): {} ({} session / {} weekly) · ≥{}",
        pct(r.capacity_session_line),
        pct(r.capacity_weekly_line),
        r.capacity_holds,
        r.capacity_holds_session,
        r.capacity_holds_weekly,
        fmt_dur(r.capacity_hold_secs_lower_bound),
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

/// The neutral roster block for the human views: the aggregate-only foot beneath the one
/// per-account table (design-stats.md §D-STA-5). Per-account facts — `signal` / `velocity` /
/// `runway` — are COLUMNS of that table now, NOT band lists (the #543/#544 stacked
/// `handle value · …` walls are retired); what remains here is strictly roster/fleet-level:
/// the lowest-utilisation callout and the fleet-runway aggregate. Returns an EMPTY string when
/// there is nothing to summarise (no observed account), so a caller appends it unconditionally.
/// Lines are 2-space indented (the §D-STA-5 render) and carry NO trailing newline, so the caller
/// terminates the block and the roster line foots it contiguously. Facts only — magnitudes and
/// neutral descriptors, never a recommendation or forecast (issue #160).
fn render_summary(report: &Report) -> String {
    // OBSERVED accounts only — gap honesty. An account can be in the summary with `seen ==
    // 0` (it held the active credential but the daemon polled a different one), its readings
    // zeroed rather than measured; ranking its fabricated 0% as the lowest would invent a low
    // reading the aggregator deliberately never does. The block summarises what was MEASURED,
    // so an unmeasured account is simply not in it.
    let observed: Vec<(&String, &AccountStats)> = report
        .summary
        .per_account
        .iter()
        .filter(|(_, a)| a.seen > 0)
        .collect();
    if observed.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::new();

    // Lowest-utilisation account: the smallest session MEAN among the observed — a
    // magnitude, not a verdict. The handle breaks ties, so the pick is deterministic.
    if let Some((handle, a)) = observed.iter().min_by(|a, b| {
        a.1.session
            .mean
            .total_cmp(&b.1.session.mean)
            .then_with(|| a.0.cmp(b.0))
    }) {
        lines.push(format!(
            "  lowest utilisation: {handle} (session mean {}%)",
            pct(a.session.mean)
        ));
    }

    // The FLEET/roster runway aggregate (issue #544): ONE approximate figure for "across all my
    // accounts, how long do I last?" — the roster's combined weekly head-room drained at its
    // combined weekly burn, days-scale (`fmt_runway_days`). NEUTRAL and APPROXIMATE (a `~` figure,
    // framed "at the current combined rate"), so it clears the amended #542 guard. The
    // counted-account cardinality `(n of m counted)` is ALWAYS shown alongside it, so an excluded
    // (unknown / stale) account is surfaced as a fact, not silently folded in as zero-burn.
    //
    // Printed in EVERY state of a counted fleet, including both unknowns (R-3 / R-20, issue #1028).
    // It previously rendered only under `runway_secs: Some(_)`, which made tightening the
    // plausibility guard *quieter* rather than more honest: the absurd figure would vanish and
    // leave nothing in its place, so an operator could not tell "the fleet is fine" from "we could
    // not measure it". An unknown fact is STATED, never omitted by dropping its line.
    //
    // The two unknowns are worded as the reader-meaningful condition each represents, not as the
    // internal cause (R-21 — no implementation vocabulary on a user-facing stat string), and both
    // stay descriptive of what WAS measured, with no forecast verb (REQ-STA-B-006 / REQ-STA-SUR-001).
    if let Some(fleet) = fleet_runway(report) {
        lines.push(format!("  {}", fleet_runway_line(fleet)));
    }

    lines.join("\n")
}

/// The fleet runway's whole line for the roster band: the `accounts last:` label, the clause
/// [`fleet_runway_phrase`] states for the runway's state, and the `n of m` cardinality.
///
/// The label takes a COLON (issue #1082) because the clause after it is a figure in only one of
/// four states. Without one, `last` is a verb reaching for a duration, and the three refusing states
/// render `accounts last unknown …` — a sentence whose object never arrives, and which parses only
/// for a reader who already knows the figure-bearing shape it stands in for. The colon demotes the
/// label to what the facts around it already are — `lowest utilisation: …` directly above in this
/// band, and the roster line's `capacity holds …: …` cell, which keeps the label form in its
/// degraded state (`capacity holds: —`) as well as in its measured one — so a stated unknown reads
/// as the value of a named fact rather than as a truncation. The label is state-INDEPENDENT for the
/// same reason: one line rendering as a sentence in one state and as a label in another asks the
/// reader to decide whether the two are even the same fact.
///
/// Split out from the render for the reason [`fleet_runway_phrase`] was: the frame is what was
/// mis-worded, and a frame reachable only through a whole [`Report`] cannot be checked against the
/// state ([`FleetRunwayState::Unmeasurable`]) no WELL-FORMED report produces.
fn fleet_runway_line(fleet: FleetRunway) -> String {
    let FleetRunway {
        state,
        counted,
        observed,
    } = fleet;
    format!(
        "accounts last: {} ({counted} of {observed} counted)",
        fleet_runway_phrase(state)
    )
}

/// The fleet runway's clause for the roster band — the figure, or the STATED unknown that stands in
/// for it (issue #1028). [`fleet_runway_line`] wraps it with the `accounts last:` label and the
/// `n of m` cardinality.
///
/// Split out from the render so every arm is reachable from a test. The unknown arms are otherwise
/// gated behind constructing a whole [`Report`] whose pooled arithmetic lands in each state, and one
/// of them ([`FleetRunwayState::Unmeasurable`]) cannot be reached that way at all — it is a
/// statement about malformed INPUTS, which a well-formed report by definition does not carry. A
/// string that only a degenerate input can produce is exactly the one that ships mis-worded.
///
/// Each unknown names the condition a READER can act on, not the internal cause (R-21), and each
/// stays a description of what WAS measured — carrying the same "at the current combined rate"
/// descriptor framing the known figure uses to clear the §D-STA-6 forecast firewall.
fn fleet_runway_phrase(state: FleetRunwayState) -> String {
    match state {
        FleetRunwayState::Known(secs) => {
            format!("{} at the current combined rate", fmt_runway_days(secs))
        }
        FleetRunwayState::Flat => "unknown — no combined usage measured".to_owned(),
        // A BOUND, not a figure: what was measured is that the pooled head-room outlasts a week's
        // drain. Naming the rate "too small" here would misattribute it — the gate is on the RATIO,
        // so an entirely ordinary burn trips this whenever head-room is ample, and an operator would
        // read it as a broken measurement rather than an unremarkable fleet. The bound is also the
        // SAFER claim: the computation is invalid because it ignores replenishment, and
        // replenishment only ADDS head-room, so a lower bound survives the very effect that
        // invalidates the point figure.
        FleetRunwayState::BeyondWeeklyWindow => {
            "unknown — more than a week at the current combined rate".to_owned()
        }
        FleetRunwayState::Unmeasurable => "unknown — no combined rate could be read".to_owned(),
    }
}

/// A usage rate in the sample's native fraction-per-SECOND, scaled to percent-per-minute
/// (`× 60 × 100`) — the neutral unit BOTH the human (`fmt_pct_per_min`) and wire
/// (`round_pct_per_min`) views present, so the two scale the EMA's native rate identically
/// (the `pct` sibling for the plain fraction → percent conversion).
fn pct_per_min(rate_frac_per_sec: f64) -> f64 {
    rate_frac_per_sec * 60.0 * 100.0
}

/// The `%/min` cell's MEASURED-ZERO form and — since issue #1136 — nothing else. Also the probe
/// [`fmt_pct_per_min`] compares its OWN output against, so the sub-threshold band is defined by the
/// precision actually displayed rather than by a second, drifting copy of it.
const ZERO_PCT_PER_MIN: &str = "0.0%/min";

/// The `%/min` cell's SUB-THRESHOLD form: a measured, strictly positive rate too small to state at
/// [`fmt_pct_per_min`]'s one decimal. The `0.1` here must be the SMALLEST figure that precision can
/// carry, so that the bound is exactly the figure form's floor — a bound naming a value the figure
/// form could itself have stated would exclude readings that are not in the band at all. Nothing in
/// the type system ties the two; `sub_threshold_bound_is_the_figure_forms_floor` does, by asserting
/// the figure just above the cliff. Change the precision without changing this string and that test
/// fails, rather than a lying bound shipping.
const SUB_THRESHOLD_PCT_PER_MIN: &str = "<0.1%/min";

/// A smoothed usage rate (usage-fraction per SECOND — the EMA's native unit) as a neutral `%/min`
/// string. THREE forms, and the distinction between the first two is the whole point (issue #1136):
///
/// - [`ZERO_PCT_PER_MIN`] (`0.0%/min`) — a MEASURED ZERO, and nothing else. An idle-but-known
///   account is a reading; `—` is the gap ([`velocity_cell`]).
/// - [`SUB_THRESHOLD_PCT_PER_MIN`] (`<0.1%/min`) — a BOUND: measured, strictly positive, and below
///   what one decimal can state.
/// - `X.Y%/min` — the figure, to one decimal.
///
/// Before #1136 the first two shared one string. `{:.1}` alone renders every rate under `8.33e-6`
/// frac/s as `0.0%/min`, and that band reaches ~15% of a session quota per 5 h window — so an
/// account draining that much of its quota read exactly like an idle one. Issue #1075 made the
/// pairing worse rather than better: such an account's runway is now refused as implausible, so
/// *measured flat, no drain to divide by* and *measured burning, quotient unreachable before the
/// reset* rendered identical bytes on BOTH cells, leaving nothing on the human surface to
/// separate them.
///
/// The band is read back from THIS call's own output rather than compared against a hand-written
/// epsilon: whatever `{:.1}` rounds to zero IS the band, by construction, so the two cannot drift —
/// including at the half-way case, which follows the binary representation rather than a decimal
/// rule. The guard is `> 0.0` and not `!= 0.0` because the bound asserts a BURN, which a negative
/// rate is not; [`replay_velocity_ema`] resets on a drop and divides by a positive elapsed, so
/// every rate reaching here is non-negative anyway.
///
/// Two shapes the issue offered and this rejects:
///
/// - **More decimals for small magnitudes** (`0.03%/min`). It keeps one string per fact, but it
///   does not remove the cliff — it moves it: a fixed-width cell cannot carry unbounded precision,
///   and below wherever the digits stop, a real burn renders `0.000…%/min`, the same defect three
///   decades down. It also states precision the EMA does not have, and the issue's own `1e-9` row
///   needs `0.000006%/min` — 13 columns against this column's 8.
/// - **The bound for the whole band, exact zero included.** It satisfies "`0.0%/min` means a
///   measured zero" vacuously, by deleting `0.0%/min` from the vocabulary — and with it the
///   flat-versus-gap distinction the cell turns on. A flat account would read `<0.1%/min`: true,
///   but weaker than the reading the aggregator actually made.
///
/// A BOUND is this surface's established form for "a figure here would be false precision" —
/// [`fleet_runway_phrase`] states *more than a week* rather than a number for the same reason, at
/// the other end of the same scale. It is not a third ambiguous state: `—` still means no
/// measurement was made, and `<0.1%/min` is emphatically one that was.
fn fmt_pct_per_min(rate_frac_per_sec: f64) -> String {
    let per_min = pct_per_min(rate_frac_per_sec);
    let figure = format!("{per_min:.1}%/min");
    if per_min > 0.0 && figure == ZERO_PCT_PER_MIN {
        return SUB_THRESHOLD_PCT_PER_MIN.to_owned();
    }
    figure
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

// --- rendering: per-account COLUMN cells (issue #556) ------------------------
//
// The per-account signal / velocity / runway are COLUMNS of the one per-account table now
// (design-stats.md §D-STA-5), not lists in the neutral band — a new per-account metric is a
// column addition, never a footer list keyed per account. These formatters render one account's
// cell for each; they foot BOTH the numeric text table and the TTY chart table with the identical
// value, and the gap sentinel `—` is honest degradation, never a fabricated figure.

/// This account's SIGNAL cell — the neutral symmetric class word for an OBSERVED account
/// (`seen > 0`), or the gap sentinel `—` for an unobserved one (its readings were zeroed, not
/// measured; classifying a fabricated 0% as "underused" would invent a reading the aggregator
/// never made — the per-cell form of the band's observed-only filter). Keyed on the session PEAK,
/// the same basis as the wire [`Band`], so the word and the session tint classify one reading alike.
fn signal_cell(a: &AccountStats) -> &'static str {
    if a.seen > 0 {
        SignalBand::of(a.session.peak).label()
    } else {
        "—"
    }
}

/// The SIGNAL cell's SYMMETRIC emphasis SGR (issue #160 / §D-STA-6): `Some("33")` for BOTH
/// deviations from the balanced middle (equal weight — underuse is not "good", saturation not
/// "alarm"), `None` for the balanced middle and for an unobserved `—` (no colour wrap). The
/// descriptor WORD carries the full signal, so a NO_COLOR reader loses nothing.
fn signal_sgr(a: &AccountStats) -> Option<&'static str> {
    if a.seen == 0 {
        return None;
    }
    match SignalBand::of(a.session.peak).sgr() {
        "" => None,
        sgr => Some(sgr),
    }
}

/// This account's compact SESSION cell — `mean/peak` in whole percents (e.g. `42/100`), so a
/// lowest-by-MEAN account reads self-consistently even while `saturated` by PEAK (the signal is
/// banded on peak). The wider numeric table keeps the full `mean/peak/p95` [`triple`]; this is the
/// compact TTY form.
fn session_cell(a: &AccountStats) -> String {
    format!("{}/{}", pct(a.session.mean), pct(a.session.peak))
}

/// This account's compact VELOCITY cell — the neutral session `%/min` rate, or `—` when the rate
/// is unknown (too few samples, stale, or no velocity overlay on the report). The COLUMN form of
/// the retired band's velocity list; `0.0%/min` for an idle-but-known account is a reading,
/// [`SUB_THRESHOLD_PCT_PER_MIN`] a measured burn below the displayed precision (issue #1136), `—` a
/// gap. The bound is a reading too, so a column holding one does not elide — the empty-column
/// pre-pass keys on [`EXPIRY_GAP`], which only the gap spells.
fn velocity_cell(v: Option<&AccountVelocity>) -> String {
    match v.and_then(|v| v.session_rate) {
        Some(rate) => fmt_pct_per_min(rate),
        None => "—".to_owned(),
    }
}

/// This account's compact RUNWAY cell — the APPROXIMATE session head-room to the swap trigger
/// (`~Xh`), or `—` when unknown / already at the trigger. The `~` marks it approximate and the
/// trigger is the neutral reference (a fact, not advice), so the cell clears the #542 guard. The
/// weekly per-account head-room is not shown per row — it feeds the aggregate `fleet` line instead.
fn runway_cell(v: Option<&AccountVelocity>) -> String {
    match v.and_then(|v| v.session_runway_secs) {
        Some(secs) => fmt_runway_hours(secs),
        None => "—".to_owned(),
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
    /// The honesty signal for issue #642: PRESENT exactly when `config.toml` EXISTS but failed to
    /// parse/validate, carrying [`ConfigFault::wire_reason`] — a STATIC string naming the failure
    /// class and the command that prints the detail, never the raw [`Error`] `Display` the log
    /// gets (see that field for why the wire copy is not derived from the config at all). OMITTED
    /// on the healthy path AND on an ABSENT config (the normal pre-`capture` state, which
    /// legitimately uses defaults).
    ///
    /// Its presence means: **every ceiling-dependent figure in this document was computed against
    /// DEFAULT tunables, not the operator's.** That is not cosmetic — between `session_ceiling` 95
    /// and 50 the same store yields `cap_hits` 112 vs 356 and `time_at_cap_secs` 8h52m vs 17h37m.
    /// Before #642 the daemon merely LOGGED this and served the numbers bare, so a socket client
    /// (the menubar Stats tab) read a confident payload it had no way to distrust — the same
    /// honesty failure as issues #479 / #582 / #632: a surface must not read more confident than
    /// reality. The reply stays a FULL document rather than degrading to an `{"error":…}` envelope
    /// so the panel keeps its best-effort series and annotates it, rather than losing the tab.
    ///
    /// Additive to `schema:1` — `skip_serializing_if` keeps the healthy payload byte-identical, so
    /// a pre-#642 client that ignores the key decodes every prior field unchanged. Per this wire's
    /// own rule ("bumped only on a breaking change", see [`JSON_SCHEMA_VERSION`]) and the
    /// `#159`/`#160`/`#314`/`#543`/`#544` precedent, it does NOT bump `schema`.
    #[serde(skip_serializing_if = "Option::is_none")]
    config_unreadable: Option<&'a str>,
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
/// the human band's fleet-runway line. `runway_secs` is explicit `null` (NEVER a sentinel like `0` /
/// `999`) whenever there is no plausible figure to state; `counted` / `observed` carry the `n of m`
/// cardinality so a reader sees exactly how many accounts the figure rests on — and that
/// `observed − counted` accounts were EXCLUDED for an unknown / stale velocity (honest degradation,
/// never silently zero-burned).
///
/// **`null` does NOT imply an idle fleet** (issue #1028). It covers three distinct outcomes — a flat
/// roster, a pooled head-room that outlasts one weekly window, and an unmeasurable computation — and
/// this wire carries NO discriminator between them, so a reader cannot tell which occurred and must
/// not infer "idle". The human surface states the distinction in prose; a machine reader wanting it
/// on the wire is a schema change, deliberately not made here. Before #1028 the first outcome was the
/// only one that produced `null` (an over-long quotient instead SATURATED to `i64::MAX`), which is
/// what made the old equivalence look safe to assert.
#[derive(Serialize)]
struct FleetWire {
    /// Approximate whole seconds until the roster's COMBINED weekly head-room is exhausted at its
    /// combined weekly burn, or `null` when no plausible figure could be stated — see the type doc:
    /// `null` is NOT specifically "no measurable burn".
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
/// of the human table's `velocity` / `runway` columns. A KNOWN object carries the session rate as a
/// real number; an individually-unknown figure (a zero-rate or at/over-trigger runway, a flat
/// weekly) is explicit `null`, NEVER a sentinel like `0` or `999`. Rates are `%/min`; runways are
/// whole seconds (the reader scales to whatever unit it renders).
#[derive(Serialize)]
struct VelocityWire {
    /// Smoothed session-usage rate in percent-per-minute (#539's EMA, replayed stats-side over
    /// the stored series). Always a real number — the object is present only when it is known.
    /// `0.0` here is a MEASURED FLAT account and nothing else: [`round_pct_per_min`] re-quantizes a
    /// rate too small for its three decimals rather than collapsing it onto this reading (#1146).
    session_pct_per_min: f64,
    /// Smoothed weekly-usage rate in percent-per-minute, or `null` when the weekly dimension has
    /// no measurable rate (flat / reset / fewer than two sample intervals). `0.0` carries the same
    /// measured-flat meaning it does on the session field, on the same quantizer.
    weekly_pct_per_min: Option<f64>,
    /// Approximate whole seconds until the session reading reaches `session_ceiling`, or `null`
    /// when the rate is `0` or the reading is already at/over the ceiling (no positive head-room),
    /// or the figure would be IMPLAUSIBLE — past one rolling session window (issue #1075). The
    /// last is a value-side refusal only: this field's type, presence and JSON value-kinds are
    /// unchanged, so `JSON_SCHEMA_VERSION` does not move and a reader that already handled the
    /// `null` this field has always been able to carry decodes exactly as before.
    session_runway_secs: Option<i64>,
    /// Approximate whole seconds until the weekly reading reaches `weekly_ceiling`, or `null` —
    /// on the same cases, bounded at one WEEKLY window rather than the session one.
    weekly_runway_secs: Option<i64>,
}

#[derive(Serialize)]
struct DimWire {
    mean: f64,
    peak: f64,
    p95: f64,
}

/// The roster-wide block of the `--json` / socket wire.
///
/// `all_high_threshold` and `all_high_covered_secs` are ADDITIVE (issue #804), as are the seven
/// `capacity_*` fields (issue #803) and `census_over_roster` (issue #866) — no existing field
/// changed type or meaning, so `JSON_SCHEMA_VERSION` does not move and a reader that ignores them
/// decodes exactly as before (the Swift `StatsRoster` names its keys explicitly and ignores the
/// rest). All are ALWAYS present rather than `skip_serializing_if`-elided, following #804's
/// rationale: they are always known, and a surface that found a threshold or a denominator ABSENT
/// would fall back to a hardcoded literal or to reading a bare count as calm — precisely what
/// carrying them is meant to end. Eliding them would make "not measurable" and "field not sent"
/// indistinguishable, which is the one distinction the capacity readout must preserve.
///
/// Read `all_high_episodes` ONLY against `all_high_covered_secs`. Zero jointly-covered
/// seconds means the census was never measurable, and the accompanying `0` is UNKNOWN, not
/// calm — a consumer MUST render its own gap sentinel there, never a bare `0`
/// (REQ-STA-B-008; the CLI does so in [`roster_line`]).
#[derive(Serialize)]
struct RosterWire {
    swap_count: u32,
    swaps: SwapsWire,
    all_high_episodes: u32,
    all_high_secs: i64,
    /// The session-utilisation water each account had to be at/above, as a FRACTION —
    /// the same units as every other utilisation on this wire (`session_ceiling` / 100).
    all_high_threshold: f64,
    /// Seconds of the window during which EVERY rostered account was simultaneously
    /// covered — the denominator the two figures above were measured over.
    all_high_covered_secs: i64,
    /// The census's SET, beside its water and its denominator (issue #866): `true` when the census
    /// intersected the CONFIGURED roster, `false` when no roster was known and it degraded to
    /// whoever held samples — where an unsampled account cannot withhold the metric, so it fires on
    /// strictly less evidence. Carried for the same reason `all_high_threshold` is — issue #804
    /// put the water on the wire because three surfaces were each giving a different answer for
    /// it, and a surface that has to re-derive a census parameter answers from a second source
    /// that can drift. The set is the same shape of fact: the panel's aggregate callout
    /// otherwise renders both regimes as identical bytes (the defect issue #866 reports), and
    /// re-deriving the regime panel-side would keep asserting it after the census's own input
    /// stopped agreeing.
    ///
    /// ALWAYS present, and here that is load-bearing rather than merely consistent with the block's
    /// rule above. Eliding the `false` — the one value a reader acts on — would make the DEGRADED
    /// regime indistinguishable from a pre-#866 daemon that never sent the key, and those two
    /// demand opposite renders: name the set, versus drop the qualifier (a claim about a set the
    /// daemon never reported is the fabrication this field exists to end). That is exactly the
    /// "not measurable" / "field not sent" collapse the block rule forbids, on the one field where
    /// the elided value is the informative one.
    ///
    /// This is the census's set ONLY. The capacity-holds figures below degrade over the SAME
    /// `roster` and carry no equivalent flag — issue #864 tracks that asymmetry CLI-side; a
    /// consumer must not read this field as annotating them.
    census_over_roster: bool,
    /// Maximal intervals in which EVERY rostered account was simultaneously non-viable at the
    /// daemon's own boundary, so swapping could not have restored capacity (issue #803).
    ///
    /// A DIFFERENT fact from `all_high_episodes`, not a stricter version of it: that one is the
    /// utilisation census ("was the roster running hot?"), this is the capacity fact ("could the
    /// daemon still swap?"). Read it ONLY against `capacity_hold_covered_secs`.
    capacity_holds: u32,
    /// The `capacity_holds` gated by a SESSION window reopening.
    capacity_holds_session: u32,
    /// The `capacity_holds` gated by a WEEKLY window reopening. Together with the field above
    /// these split `capacity_holds` exactly.
    capacity_holds_weekly: u32,
    /// Total held seconds — a LOWER BOUND (REQ-STA-B-011), which is why the name says so and why
    /// a consumer must render it as `≥`, never as an exact figure. A coverage gap inside a hold
    /// truncates it and a hold still running at the window's end is clipped to that end.
    capacity_hold_secs_lower_bound: i64,
    /// Seconds during which EVERY rostered account was simultaneously covered under the
    /// reset-anchored windows — the denominator the four figures above were measured over. `0`
    /// means the census was never taken or never measurable, so the accompanying `0` holds is
    /// UNKNOWN, not calm; a consumer MUST render its own gap sentinel there.
    capacity_hold_covered_secs: i64,
    /// The SESSION line an account had to be at/above to count as non-viable, as a FRACTION
    /// (`min(session_ceiling, target_max_session_usage)` — 0.80 at defaults).
    capacity_session_line: f64,
    /// The WEEKLY line, as a FRACTION (`weekly_ceiling − WEEKLY_TAIL_MARGIN` — 0.97 at defaults,
    /// NOT the raw 0.98 ceiling).
    capacity_weekly_line: f64,
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
///
/// `config_unreadable` is the caller's config-load provenance (issue #642): `Some(detail)` when the
/// report's tunables fell back to DEFAULTS because an EXISTING `config.toml` failed to parse, else
/// `None`. Passed explicitly rather than carried on [`Report`] so the aggregate stays a pure value
/// (this is an I/O outcome, not an aggregation result) and every caller must state its provenance —
/// there is no default that silently claims a healthy config.
fn stats_wire<'a>(report: &'a Report, config_unreadable: Option<&'a str>) -> StatsWire<'a> {
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
                roster: roster_wire(&r.roster, report.census_over_roster),
                // Series buckets carry no velocity — it is a CURRENT-rate readout, not per-bucket.
                accounts: accounts_wire(&r.per_account, None),
            })
            .collect(),
        summary: PeriodWire {
            roster: roster_wire(&report.summary.roster, report.census_over_roster),
            accounts: accounts_wire(&report.summary.per_account, Some(&report.velocity)),
            // The fleet/roster runway aggregate (issue #544) — summary-only, from the SAME built
            // report the CLI and daemon socket both serialize, so the fleet figure keeps R-2 parity
            // structurally (one `stats_wire` builder). Absent on a bare aggregate (no overlay).
            fleet: fleet_runway(report).map(fleet_wire),
        },
        // Orphans carry no velocity either (a non-roster readout is out of scope, issue #543).
        orphans: accounts_wire(&report.orphans, None),
        // The caller's config-load provenance (issue #642), verbatim: the wire says "these numbers
        // rest on defaults" exactly when the caller resolved a malformed config.
        config_unreadable,
    }
}

/// Render the stable `--json` document — the human / file view: PRETTY-printed with a trailing
/// newline. (The daemon `stats` socket verb serializes the same [`stats_wire`] COMPACT, no trailing
/// newline — issue #356; the newline is the socket framing, added on write.)
///
/// `config_unreadable` carries the same issue #642 provenance [`stats_wire`] documents. The CLI
/// passes it too — not just the socket — because R-2 parity is structural (one builder, so a
/// socket-only field would break byte-equality with `stats --json`) AND because a `--json` consumer
/// PIPES stdout and never sees the stderr warning [`run`] prints: without the key it is exactly as
/// blind as the panel was.
fn render_json(report: &Report, config_unreadable: Option<&str>) -> Result<String> {
    let mut json = serde_json::to_string_pretty(&stats_wire(report, config_unreadable))
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
/// are quantized to `%/min` by [`round_pct_per_min`] so the wire is stable — no float-tail noise —
/// while keeping the weekly dimension's small figures and never quantizing a burn onto the
/// measured-flat `0.0` (issue #1146); runways stay whole seconds.
fn velocity_wire(v: &AccountVelocity) -> Option<VelocityWire> {
    let session = v.session_rate?; // absent field when the session velocity is unknown
    Some(VelocityWire {
        session_pct_per_min: round_pct_per_min(session),
        weekly_pct_per_min: v.weekly_rate.map(round_pct_per_min),
        session_runway_secs: v.session_runway_secs,
        weekly_runway_secs: v.weekly_runway_secs,
    })
}

/// The wire form of the fleet/roster runway aggregate (issue #544). The honest degradation (unknown
/// runway → `null`, the `n of m` cardinality) is already resolved in [`fleet_runway`]; this only
/// reshapes it for `serde`, PROJECTING [`FleetRunway::state`] down to the wire's `Option<i64>` via
/// [`FleetRunway::runway_secs`]. That projection is lossy on purpose — carrying the reason would be a
/// schema change (issue #1028), so the three unknown states all serialise as `null`.
fn fleet_wire(f: FleetRunway) -> FleetWire {
    FleetWire {
        runway_secs: f.runway_secs(),
        counted: f.counted,
        observed: f.observed,
    }
}

/// A usage rate (fraction/second) as percent-per-minute, QUANTIZED so the `--json` wire is stable
/// across runs — and never quantized ACROSS the zero boundary (issue #1146).
///
/// Three decimals is the primary form: it holds the wire diffable while keeping the weekly
/// dimension's small values, and every rate it can carry reaches the wire unchanged by the arm
/// below.
///
/// That arm exists because `0.0` is TAKEN on this wire. [`VelocityWire`] spends a three-way
/// vocabulary deliberately — `0.0` is a MEASURED FLAT reading, `null` is UNKNOWN, a positive number
/// is a measured rate — so a burn quantized down to `0.0` does not merely lose precision, it changes
/// CATEGORY, asserting a reading the aggregator never made. Three decimals of `%/min` collapse every
/// rate under `8.333e-8` frac/s, and the worst case still inside that band drains ~0.15% of a
/// session quota per 5 h window. `1e-9` frac/s sat there too — the rate the human cell has stated as
/// [`SUB_THRESHOLD_PCT_PER_MIN`] since issue #1136 — so the two surfaces disagreed about whether the
/// account was measured burning at all.
///
/// The collapse is detected by comparing against the primary form's OWN output rather than a
/// hand-written epsilon, exactly as [`fmt_pct_per_min`] does: whatever three decimals round to zero
/// IS the band, by construction, so the two cannot drift. The guard is `> 0.0` and not `!= 0.0`
/// because the bound asserts a BURN, which a negative rate is not — the same reason
/// [`fmt_pct_per_min`] gives. A measured flat rate reaches `0.0` under either spelling (`0.0 != 0.0`
/// is false); the spelling bites only on a NEGATIVE sub-threshold rate, which `replay_velocity_ema`
/// cannot produce — non-negative by construction, since a drop resets. And no non-finite rate
/// satisfies BOTH conditions: `NaN` and `-inf` fail `> 0.0`, while `+inf` passes that and fails
/// `rounded == 0.0`. All of them take the primary path exactly as before, so [`render_json`]'s
/// serialize refusal is unaffected.
///
/// The fallback re-quantizes to three SIGNIFICANT figures — the same quantity of information, placed
/// where the value actually is. Being a quantizer, it keeps the stability the rounding exists for:
/// every one of 2000 adjacent `f64`s around `1e-9` frac/s serialises the identical `6e-6`, where the
/// unrounded rate gives over a thousand distinct strings
/// (`sub_threshold_wire_values_are_stable_across_runs`). It goes through the shortest decimal
/// round-trip rather than `powi` scaling, which loses its own last digits at extreme exponents —
/// `1.335e-304 %/min` re-quantizes to `1.339999999999999e-304` that way, reintroducing the very tail
/// this avoids.
///
/// Two shapes issue #1146 offered and this rejects:
///
/// - **Floor a positive rate to the smallest representable non-zero** (`0.001`). It repairs the
///   category by breaking the magnitude: the issue's own `1e-9` row is a true `6e-6 %/min`, which
///   the floor overstates 167-fold, unboundedly so further down the band. That trades a false `0.0`
///   for a false `0.001` — moving the lie rather than removing it — and a machine reader cannot
///   eyeball the discrepancy the way an operator can, which is the one respect in which this wire is
///   harder to fix than the cell.
/// - **Carry the unrounded value on this field alone.** Honest and unstable: it puts float-tail
///   noise back on a wire that gets diffed across runs, which is the defect the rounding exists to
///   prevent, and splits this field's contract from its sibling's for no reason a reader could infer.
///
/// `JSON_SCHEMA_VERSION` does not move. Type, presence and JSON value-kind are all unchanged and
/// only the value differs — the same value-side reasoning issue #1075 recorded one field down on
/// `session_runway_secs`. What changes is that the wire now MEANS what it already documented.
fn round_pct_per_min(rate_frac_per_sec: f64) -> f64 {
    let per_min = pct_per_min(rate_frac_per_sec);
    let rounded = (per_min * 1000.0).round() / 1000.0;
    if per_min > 0.0 && rounded == 0.0 {
        // `{:.2e}` IS three significant figures. Parsing it back is total for the finite, strictly
        // positive `per_min` the guard admits; the `unwrap_or` is unreachable rather than load-
        // bearing, and falls back to the true rate so the strictly-positive invariant holds either
        // way.
        return format!("{per_min:.2e}").parse().unwrap_or(per_min);
    }
    rounded
}

fn dim_wire(d: &crate::usage_stats::DimStats) -> DimWire {
    DimWire {
        mean: d.mean,
        peak: d.peak,
        p95: d.p95,
    }
}

/// `census_over_roster` is [`Report::census_over_roster`], passed in rather than read off
/// `RosterStats`: the regime is a property of the aggregation the whole report came from, not of
/// the per-window roster block, so both the summary and every series bucket carry the SAME value —
/// the one [`Report`] recorded from the very `roster` [`aggregate_with_roster`] consumed.
fn roster_wire(r: &RosterStats, census_over_roster: bool) -> RosterWire {
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
        all_high_threshold: r.high_threshold,
        all_high_covered_secs: r.all_high_covered_secs,
        census_over_roster,
        capacity_holds: r.capacity_holds,
        capacity_holds_session: r.capacity_holds_session,
        capacity_holds_weekly: r.capacity_holds_weekly,
        capacity_hold_secs_lower_bound: r.capacity_hold_secs_lower_bound,
        capacity_hold_covered_secs: r.capacity_hold_covered_secs,
        capacity_session_line: r.capacity_session_line,
        capacity_weekly_line: r.capacity_weekly_line,
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

/// Where a cell sits WITHIN its content-sized column: TEXT cells left-align, NUMERIC cells
/// right-align, so magnitudes read down a column and their `%` / unit suffixes line up
/// (design-stats.md §D-STA-5, as amended by issue #793). This is alignment inside a column —
/// widths stay content-sized, NOT the fixed-width fields the #557 reflow retired. The gap
/// sentinel `—` simply follows its own column's alignment.
#[derive(Clone, Copy)]
enum Align {
    Left,
    Right,
}

/// One droppable table column: a header, per-row cells, an optional per-row colour, the
/// spaces rendered BEFORE it, its cell [`Align`]ment, and a drop priority (`None` = always
/// keep; `Some(n)` = droppable, the LOWEST present `n` dropping first under a narrow
/// terminal). Mirrors the `status` view's [`Column`](crate::cli) discipline but over
/// already-rendered string cells.
struct ChartCol {
    header: &'static str,
    cells: Vec<String>,
    colors: Vec<Option<&'static str>>,
    lead_gap: usize,
    align: Align,
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

/// Render one table line: each cell preceded by its lead gap and padded to its column width on
/// DISPLAY width, the pad going AFTER the cell for an [`Align::Left`] column and BEFORE it for
/// an [`Align::Right`] one (text left, numeric right — issue #795), colour wrapping the raw cell
/// BEFORE the pad either way (so the escape bytes never enter the width math, the SGR pair hugs
/// the cell text and never the padding, and stripping the escapes recovers the exact plain
/// table), trailing whitespace trimmed. A right-aligned column pads on the LEADING side, so the
/// trim only ever reaches a left-aligned trailing column. The `status` view's `render_cells`
/// discipline (issue #159 reuse).
fn render_line(
    cells: &[&str],
    widths: &[usize],
    colors: &[Option<&str>],
    gaps: &[usize],
    aligns: &[Align],
) -> String {
    let mut line = String::new();
    for ((((cell, &width), color), &gap), &align) in
        cells.iter().zip(widths).zip(colors).zip(gaps).zip(aligns)
    {
        let pad = " ".repeat(width.saturating_sub(display_width(cell)));
        let (lead, trail) = match align {
            Align::Left => ("", pad.as_str()),
            Align::Right => (pad.as_str(), ""),
        };
        line.push_str(&" ".repeat(gap));
        line.push_str(lead);
        match color {
            Some(sgr) => line.push_str(&format!("\x1b[{sgr}m{cell}\x1b[0m")),
            None => line.push_str(cell),
        }
        line.push_str(trail);
    }
    format!("{}\n", line.trim_end())
}

/// One per-account row's source for the table catalog (issue #557): the handle, its stats, its
/// optional velocity overlay, and the pre-rendered `trend` sparkline (built with the surface's
/// ASCII ramp, since a [`Column`] extractor cannot itself reach `report.series`). BOTH surfaces
/// build the same `AccountRow`; each renders its declared [`Column`] subset over it, so a
/// per-account metric can no longer diverge in shape between the two renderers.
struct AccountRow<'a> {
    handle: &'a str,
    stats: &'a AccountStats,
    velocity: Option<&'a AccountVelocity>,
    trend: String,
    /// The pre-rendered REFRESH-token expiry cell (issue #883), bracketed while the deadline is
    /// inside the configured horizon (issue #934) — [`EXPIRY_GAP`] when this account has no entry
    /// in the report's `expiry` overlay. Pre-rendered for the same reason `trend` is: a [`Column`]
    /// extractor is a bare `fn` pointer, so it cannot reach the reference instant the cell's
    /// time-until needs.
    expiry: String,
}

/// One column of the per-account table catalog (issue #557), generalising the [`ChartCol`] idea to
/// a reusable SPEC: a header, the spaces BEFORE it, the cell [`Align`]ment (issue #795), the drop
/// `priority` (`None` = the `account · signal · session` floor, `Some(n)` sheds lowest-first under
/// a narrow terminal), and pure extractors for the cell string and its optional per-row colour SGR
/// from an [`AccountRow`].
/// There is ONE catalog; each surface renders its declared ordered subset ([`piped_columns`] /
/// [`tty_columns`]). A new per-account metric is a single `Column` that appears on both surfaces by
/// subset choice — the convergence that kills the latent shape-drift #556 left between the two
/// renderers.
struct Column {
    header: &'static str,
    lead_gap: usize,
    align: Align,
    priority: Option<u8>,
    cell: fn(&AccountRow) -> String,
    color: fn(&AccountRow) -> Option<&'static str>,
}

// --- the per-account column catalog (issue #557) ----------------------------
//
// Each column is ONE constructor, composed into the two declared subsets below. The SHARED
// columns (`account` / `signal` / `velocity` / `runway`) are the SAME constructor on both
// surfaces, so they cannot diverge; the surface-specific ones differ only in which subset lists
// them. `account` leads with no gap; every other column is preceded by two spaces.
//
// ALIGNMENT (issue #795, §D-STA-5 as amended by issue #793) is declared per column here: the two
// TEXT columns (`account` / `signal`) left-align, every NUMERIC one right-aligns. `trend` is
// neither — a per-bucket sparkline read left-to-right in time, one glyph per series bucket and so
// the same width on every row — and stays LEFT.

/// The `account` handle — the floor's first column, sized on DISPLAY width by the renderer.
fn col_account() -> Column {
    Column {
        header: "account",
        lead_gap: 0,
        align: Align::Left,
        priority: None,
        cell: |r| r.handle.to_owned(),
        color: |_| None,
    }
}
/// The neutral `signal` class word (`—` for an unobserved account — gap honesty), tinted by its
/// symmetric emphasis SGR when colour is on (the tint is dropped on the piped surface).
fn col_signal() -> Column {
    Column {
        header: "signal",
        lead_gap: 2,
        align: Align::Left,
        priority: None,
        cell: |r| signal_cell(r.stats).to_owned(),
        color: |r| signal_sgr(r.stats),
    }
}
/// `cov` — observed-window coverage as a whole percent (piped-only wide column).
fn col_cov() -> Column {
    Column {
        header: "cov",
        lead_gap: 2,
        align: Align::Right,
        priority: None,
        cell: |r| format!("{}%", pct(r.stats.coverage)),
        color: |_| None,
    }
}
/// The full `session` `mean/peak/p95` triple (the WIDE piped surface; the TTY keeps the compact
/// [`col_session_compact`]). Part of the `account · signal · session` floor — never dropped.
fn col_session_triple() -> Column {
    Column {
        header: "session m/p/p95",
        lead_gap: 2,
        align: Align::Right,
        priority: None,
        cell: |r| triple(&r.stats.session),
        color: |_| None,
    }
}
/// The compact `session` `mean/peak` (the TTY surface), tinted by the session-peak band.
fn col_session_compact() -> Column {
    Column {
        header: "session",
        lead_gap: 2,
        align: Align::Right,
        priority: None,
        cell: |r| session_cell(r.stats),
        color: |r| Some(Band::of(r.stats.session.peak).sgr()),
    }
}
/// The full `weekly` `mean/peak/p95` triple (the WIDE piped surface).
fn col_weekly_triple() -> Column {
    Column {
        header: "weekly m/p/p95",
        lead_gap: 2,
        align: Align::Right,
        priority: None,
        cell: |r| triple(&r.stats.weekly),
        color: |_| None,
    }
}
/// The compact `weekly` peak percent (the TTY surface), tinted by the weekly-peak band; drops
/// after `trend` / `velocity` / `runway` under a narrow terminal.
fn col_weekly_peak() -> Column {
    Column {
        header: "weekly",
        lead_gap: 2,
        align: Align::Right,
        priority: Some(5),
        cell: |r| format!("{}%", pct(r.stats.weekly.peak)),
        color: |r| Some(Band::of(r.stats.weekly.peak).sgr()),
    }
}
/// `caps` — cap-hit count (piped-only wide column).
fn col_caps() -> Column {
    Column {
        header: "caps",
        lead_gap: 2,
        align: Align::Right,
        priority: None,
        cell: |r| r.stats.cap_hits.to_string(),
        color: |_| None,
    }
}
/// `t@cap` — coarse time-at-cap duration (piped-only wide column).
fn col_time_at_cap() -> Column {
    Column {
        header: "t@cap",
        lead_gap: 2,
        align: Align::Right,
        priority: None,
        cell: |r| fmt_dur(r.stats.time_at_cap_secs),
        color: |_| None,
    }
}
/// `share` — whole-window contribution share as a whole percent (piped-only wide column).
fn col_share() -> Column {
    Column {
        header: "share",
        lead_gap: 2,
        align: Align::Right,
        priority: None,
        cell: |r| format!("{}%", pct(r.stats.contribution_share)),
        color: |_| None,
    }
}
/// `velocity` — the neutral session `%/min` rate, `—` when unknown. SHARED by both surfaces;
/// droppable (elides when uniformly `—`; sheds before `runway` under a narrow terminal).
fn col_velocity() -> Column {
    Column {
        header: "velocity",
        lead_gap: 2,
        align: Align::Right,
        priority: Some(3),
        cell: |r| velocity_cell(r.velocity),
        color: |_| None,
    }
}
/// `runway` — approximate session head-room `~Xh`, `—` when unknown. SHARED by both surfaces;
/// droppable (elides when uniformly `—`).
fn col_runway() -> Column {
    Column {
        header: "runway",
        lead_gap: 2,
        align: Align::Right,
        priority: Some(4),
        cell: |r| runway_cell(r.velocity),
        color: |_| None,
    }
}
/// `trend` — the per-bucket session-peak sparkline (the TTY surface); sheds second under a narrow
/// terminal (a populated rate out-informs the sparkline).
///
/// The one droppable column whose gap is BLANK rather than the `—` every other producer spells:
/// one break (a space, [`spark_glyph`]) per all-gap bucket, or the empty string when there are no
/// buckets. [`render_account_table`]'s elision pre-pass reads a blank cell as a gap for exactly
/// that reason (issue #815) — keep this cell blank; do NOT teach it the sentinel, which would
/// restate a gap the sparkline already spells and would do so per ROW.
fn col_trend() -> Column {
    Column {
        header: "trend",
        lead_gap: 2,
        align: Align::Left,
        priority: Some(2),
        cell: |r| r.trend.clone(),
        color: |_| None,
    }
}
/// `expiry` — this account's REFRESH-token deadline as a compact time-until (`6d21h`), the state
/// word `lapsed`, or [`EXPIRY_GAP`] when none was observed (issue #883), BRACKETED (`[6d21h]`)
/// while that deadline is inside the configured horizon (issue #934). SHARED by both surfaces.
///
/// A COLUMN of the one per-account table, per design-stats.md §D-STA-5's structural rule — never a
/// band or a footer list keyed per account (the shape issues #543/#544 were retired for). The
/// FLEET-level synchronized-cohort fact is a different thing entirely and belongs in the roster
/// block; it is issue #879's, not this column's.
///
/// RIGHT-aligned: a time-until is a NUMERIC cell, and §D-STA-5 as amended by issue #793 (→ #795)
/// right-aligns numeric cells while only the two text cells (`account` / `signal`) stay left.
///
/// Sheds FIRST under a narrow terminal (`priority: Some(1)`), ahead of `trend`. It is the
/// SLOWEST-MOVING fact on the row — a server-issued deadline measured in days that no tick can
/// move — and this is the HISTORICAL utilisation surface, so a forward-looking credential deadline
/// is the least-historical thing on it; the live `status` verb reports it first-class and
/// continuously. The same rule places it first in `status`'s own shed order, so one sentence
/// explains both surfaces rather than two.
///
/// Droppable, so it also participates in the empty-column elision pre-pass: a fleet with no
/// observed deadline (today, EVERY fleet — see `Report::expiry`) elides it entirely rather than
/// rendering a column of em dashes.
/// UNCOLOURED, deliberately, even though `status` tints the same string by its horizon band (its
/// own private `expiry_severity`). The shared-renderer guarantee covers the FACT, not its
/// presentation: this surface's colour vocabulary is the neutral utilisation band (§D-STA-6), so an
/// urgency tint on a credential deadline would editorialise inside it — the very framing the
/// neutral-band rules exist to keep out.
///
/// The horizon MARK (issue #934, `crate::cli::expiry_table_cell`) is a different channel and DOES
/// apply here. That reasoning above bans spending this surface's COLOUR on a credential deadline;
/// it says nothing about typography, and a bracket overloads no band. The mark states a fact
/// ("this deadline is inside the configured window") in the same register as the `lapsed` this
/// column already renders, so it clears §D-STA-6 on the same ground — and it is the ONLY channel
/// this column has for that fact, since `status` at least tints while this one never does.
///
/// Latent rather than live today: the overlay elides on every production path (above), so the mark
/// is currently exercised by the tests and goldens only. Wired now so that issue #917 — which gives
/// `Report::expiry` its first producer — lands a column already colour-independent, rather than one
/// that has to be revisited.
fn col_expiry() -> Column {
    Column {
        header: "expiry",
        lead_gap: 2,
        align: Align::Right,
        priority: Some(1),
        cell: |r| r.expiry.clone(),
        color: |_| None,
    }
}

/// The NUMERIC-text (piped, issue #159) column subset — the WIDER surface (design-stats.md
/// §D-STA-5): `account · signal · cov · session(m/p/p95) · weekly(m/p/p95) · caps · t@cap · share`
/// plus the shared `velocity` / `runway` / `expiry`. Rendered at `w = usize::MAX`, so it never
/// priority-drops — but it DOES elide a uniformly-gap droppable column, so `expiry` (issue #883)
/// is absent here too until its overlay is populated.
fn piped_columns() -> Vec<Column> {
    vec![
        col_account(),
        col_signal(),
        col_cov(),
        col_session_triple(),
        col_weekly_triple(),
        col_caps(),
        col_time_at_cap(),
        col_share(),
        col_velocity(),
        col_runway(),
        col_expiry(),
    ]
}

/// The TTY chart-table column subset (design-stats.md §D-STA-5): `account · signal ·
/// session(mean/peak) · weekly(peak) · runway · velocity · expiry · trend`. Priority column-drop
/// order (lowest `Some(n)` first) is `expiry → trend → velocity → runway → weekly`; the
/// `account · signal · session` floor never drops.
fn tty_columns() -> Vec<Column> {
    vec![
        col_account(),
        col_signal(),
        col_session_compact(),
        col_weekly_peak(),
        col_runway(),
        col_velocity(),
        col_expiry(),
        col_trend(),
    ]
}

/// Build one [`AccountRow`] per `(handle, stats)` (in the given order), pre-rendering each `trend`
/// sparkline with `ascii`. Shared by the piped and TTY renderers, so both foot ONE row model.
fn account_rows<'a>(
    report: &'a Report,
    accounts: impl IntoIterator<Item = (&'a String, &'a AccountStats)>,
    ascii: bool,
) -> Vec<AccountRow<'a>> {
    // The reference instant for the expiry cell's time-until. `plan_window` sets `window.end` to
    // the caller's `now` on EVERY branch (rolling period, `--since`, `lifetime`), so it is the
    // wall clock the render was built against — not a second, drifting one read here.
    let now = report.window.end;
    accounts
        .into_iter()
        .map(|(handle, stats)| AccountRow {
            handle: handle.as_str(),
            stats,
            velocity: report.velocity.get(handle),
            trend: render_sparkline(&account_series(&report.series, handle, session_peak), ascii),
            expiry: expiry_table_cell(report.expiry.get(handle).copied(), now),
        })
        .collect()
}

/// One SECTION of the per-account table: an optional heading (the #314 "not in roster (N):" label)
/// and its rows. The TTY view is a single un-headed section; the piped view is the live section
/// plus an optional orphan section. Sibling sections are footed by one [`render_account_table`]
/// call so they share column widths and one elision decision.
struct AccountSection<'a> {
    heading: Option<String>,
    rows: Vec<AccountRow<'a>>,
}

/// THE per-account table renderer (issue #557) — the ONE layout both the piped numeric view and
/// the TTY chart table route through. Renders every `section` over the given `columns` catalog
/// subset. EMPTY-COLUMN ELISION (a droppable column uniformly the gap sentinel `—` across every
/// section's rows is dropped — self-drops `velocity` / `runway` on a fleet with no computable rate,
/// design-stats.md §D-STA-5) and PRIORITY COLUMN-DROP (lowest `Some(n)` sheds first until the table
/// fits `w`, the floor never dropping and the row OVERFLOWING rather than wrapping, issue #159) are
/// both computed across the COMBINED cohort, and column widths are SHARED across sections — so the
/// live and orphan tables foot identical columns (issue #314), sized on DISPLAY width (issue #249).
/// Colour is applied per cell only when `color`. The piped surface passes `w = usize::MAX` (never
/// drops) and `color = false` (zero ANSI); the TTY passes the real terminal width and its colour.
fn render_account_table(
    sections: &[AccountSection],
    columns: &[Column],
    w: usize,
    color: bool,
) -> String {
    // Each section's row span within the concatenated cohort — so elision + widths are computed
    // across ALL sections (sibling tables stay column-identical), while each section still renders
    // its own rows under its own heading.
    let mut spans: Vec<std::ops::Range<usize>> = Vec::with_capacity(sections.len());
    let mut start = 0;
    for s in sections {
        spans.push(start..start + s.rows.len());
        start += s.rows.len();
    }
    let all_rows: Vec<&AccountRow> = sections.iter().flat_map(|s| s.rows.iter()).collect();

    // One ChartCol per catalog column over the WHOLE cohort — the row model is now the [`Column`]
    // extractors, not two hand-built layouts.
    let mut cols: Vec<ChartCol> = columns
        .iter()
        .map(|c| ChartCol {
            header: c.header,
            cells: all_rows.iter().map(|&r| (c.cell)(r)).collect(),
            colors: all_rows.iter().map(|&r| (c.color)(r)).collect(),
            lead_gap: c.lead_gap,
            align: c.align,
            priority: c.priority,
        })
        .collect();

    // Empty-column elision, then priority column-drop to fit `w` — the discipline the former
    // `render_table` carried, now over the combined cohort. A keep-column (`priority == None`, the
    // floor) is never elided even if every cell is a gap. The piped view (`w = usize::MAX`) never
    // enters the drop loop.
    // The elision keys on [`EXPIRY_GAP`] — the SAME em dash `crate::cli::expiry_table_cell`
    // returns, so an all-gap `expiry` column (issue #883) elides on the identical string its
    // producer emits rather than on a second literal that could drift from it. The horizon mark
    // (issue #934) cannot break that: the gap is never bracketed. `signal` / `velocity` / `runway`
    // still spell the sentinel inline; `the_gap_sentinel_is_one_string_across_every_producer` pins
    // all of them equal, so this retain speaks for every droppable column, not just `expiry`.
    //
    // A cell carrying NO VISIBLE MARK is a gap too (issue #815). The sentinel is the EXPLICIT
    // spelling of "nothing measured here"; a blank cell says the same thing, and used to survive
    // this retain — `render_line` trims the trailing pad, so the reader got a bare header with
    // nothing under it at any row and reasonably inferred the data exists. [`col_trend`] is the
    // reachable producer and it spells the blank TWO ways: the EMPTY string when the report has no
    // series buckets at all, and one break per bucket — a SPACE, see `spark_glyph` — when every
    // bucket is a gap. Only the second is reachable from `build_report`, whose `bucket_bounds`
    // yields no buckets only for an empty window; the first is what the `stats-all-na` fixture
    // holds. Trimming catches both, so the contract stays ONE rule rather than a list of spellings
    // the next droppable column would have to be added to.
    //
    // Rejected: emitting the sentinel from [`col_trend`] when its series is empty, so this
    // predicate could fire unchanged. It repairs the PRODUCER, not the CONTRACT — every other
    // droppable column keeps the asymmetry — and it is the LARGER semantic claim, not the smaller
    // one: a break IS this column's honest rendering of a gap (issue #159), so `—` would restate
    // "unmeasured" in a vocabulary that already has a word for it, and would do so per ROW,
    // rewriting the cell of a single all-gap account inside an otherwise populated fleet. That row
    // is not this defect: its column has marks in it and must stay.
    cols.retain(|c| {
        c.priority.is_none()
            || c.cells
                .iter()
                .any(|s| !s.trim().is_empty() && s != EXPIRY_GAP)
    });
    while table_width(&cols) > w {
        match cols.iter().filter_map(|c| c.priority).min() {
            Some(p) => cols.retain(|c| c.priority != Some(p)),
            None => break, // only keep-columns left → accept overflow, never wrap
        }
    }

    // Every per-column layout slice is derived from the POST-DROP `cols` — widths, gaps, headers
    // AND aligns alike — so they stay index-aligned with each other through both retains above.
    // Deriving any one of them from the pre-drop `columns` catalog instead would desynchronise it
    // the moment a column elides or sheds, silently padding cells on the wrong side under a narrow
    // terminal — a wide-terminal golden cannot see it, but
    // `alignment_survives_column_elision_and_the_priority_drop` can.
    let widths: Vec<usize> = cols.iter().map(ChartCol::width).collect();
    let gaps: Vec<usize> = cols.iter().map(|c| c.lead_gap).collect();
    let aligns: Vec<Align> = cols.iter().map(|c| c.align).collect();
    let headers: Vec<&str> = cols.iter().map(|c| c.header).collect();
    let no_color: Vec<Option<&str>> = vec![None; cols.len()];

    let mut out = String::new();
    for (s, span) in sections.iter().zip(&spans) {
        if !out.is_empty() {
            out.push('\n'); // a blank line separates sibling sections (live | "not in roster")
        }
        if let Some(heading) = &s.heading {
            out.push_str(heading);
            out.push('\n');
        }
        // A header follows its OWN column's alignment, so it sits over its data as one unit.
        out.push_str(&render_line(&headers, &widths, &no_color, &gaps, &aligns));
        for r in span.clone() {
            let cells: Vec<&str> = cols.iter().map(|c| c.cells[r].as_str()).collect();
            let colors: Vec<Option<&str>> = cols
                .iter()
                .map(|c| if color { c.colors[r] } else { None })
                .collect();
            out.push_str(&render_line(&cells, &widths, &colors, &gaps, &aligns));
        }
    }
    out
}

/// The per-account chart table (design-stats.md §D-STA-5): `account`, the neutral `signal` word,
/// the compact `session` mean/peak %, the `weekly` peak %, the session `runway` and `velocity`,
/// and a `trend` sparkline of the per-bucket session peak. Priority column-drop under a narrow
/// terminal — `expiry` sheds FIRST (the slowest-moving fact, [`col_expiry`]), then `trend`,
/// `velocity`, `runway`, `weekly` (a populated rate out-informs the sparkline; an empty one is
/// already gone via elision) — while the `account · session · signal` FLOOR is always kept, never
/// wrapping (issue #159). A `velocity` / `runway` / `expiry` column that is uniformly `—` is
/// elided before the width fit. Colour tints each magnitude by its neutral utilisation band and
/// the signal word symmetrically; the sparkline glyphs carry their own magnitude. Since issue #557
/// this is just the [`tty_columns`] subset over the shared [`render_account_table`] — one
/// un-headed section of live accounts.
fn render_chart_table(
    report: &Report,
    accounts: &[&String],
    w: usize,
    color: bool,
    ascii: bool,
) -> String {
    let summary = &report.summary;
    let rows = account_rows(
        report,
        accounts.iter().map(|&h| (h, &summary.per_account[h])),
        ascii,
    );
    let sections = [AccountSection {
        heading: None,
        rows,
    }];
    render_account_table(&sections, &tty_columns(), w, color)
}

/// The cross-account horizontal-bar chart: each account's whole-window contribution share
/// (the fraction of in-period observations made while it was the active credential) as a
/// bar filled on the FIXED 0–100% scale, followed by the share percent. `None` when the
/// terminal is too narrow for a readable bar (the block degrades away cleanly, issue #159).
fn render_bars(report: &Report, accounts: &[&String], w: usize, ascii: bool) -> Option<String> {
    let summary = &report.summary;
    let (fill, track) = if ascii { BAR_ASCII } else { BAR_UNICODE };
    let label_w = accounts.iter().map(|h| display_width(h)).max().unwrap_or(0);
    // line = label + "  " + bar + "  " + "NNN%"; reserve 4 for the percent field. `w` is a
    // BUDGET, not a target: bound the bar at the same 40 cells `render_percentiles` bounds its
    // gauge to (issue #794) — past that the extra cells add nothing, since the bar is a
    // comparative shape on a fixed scale and the exact figure already sits beside it.
    let bar_w = w.checked_sub(label_w + 2 + 2 + 4)?.min(40);
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
///
/// `config_unreadable` is [`render_human`]'s issue #642 provenance, rendered by
/// [`config_regime_line`] directly above the roster line — the SAME placement [`render_text`]
/// uses, so neither human surface is the one that stays blind (issue #836).
fn render_charts(
    report: &Report,
    w: usize,
    color: bool,
    ascii: bool,
    config_unreadable: Option<&str>,
) -> String {
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
        if let Some(line) = config_regime_line(config_unreadable) {
            out.push_str(&line);
        }
        out.push_str(&roster_line(&report.summary, report.census_over_roster));
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
    // The bottom roster block (§D-STA-5): the aggregate-only summary (lowest-utilisation +
    // fleet-runway — per-account signal/velocity/runway are TABLE COLUMNS now, not band lists),
    // the "not in roster" line (issue #314), the config-regime caveat (issue #836), then the
    // roster line — ONE contiguous block. The summary is colour-free: it reports roster-level
    // magnitudes, and the per-account `signal` colour lives in the table cell (issue #160
    // symmetric emphasis, unchanged). The caveat is colour-free for the same reason and sits
    // directly above the line it qualifies, so it is read before the census number.
    let band = render_summary(report);
    if !band.is_empty() {
        out.push_str(&band);
        out.push('\n');
    }
    if let Some(line) = orphan_names_line(&report.orphans) {
        out.push_str(&line);
    }
    if let Some(line) = config_regime_line(config_unreadable) {
        out.push_str(&line);
    }
    out.push_str(&roster_line(&report.summary, report.census_over_roster));
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
            roster: RosterStats {
                // Spelled out rather than `Default`ed: this report FEEDS THE RENDER, and a
                // defaulted roster would quietly foot every chart fixture with a 0% census
                // water over zero measurable time (issue #804). The hour is fully jointly
                // covered at the default 95% water — a measured, never-fired census.
                all_high_covered_secs: HOUR_SECS,
                high_threshold: 0.95,
                ..Default::default()
            },
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
            // No expiry overlay (issue #883) — so the `expiry` column elides and every chart
            // golden below keeps pinning the pre-#883 render byte for byte. The fixtures that
            // POPULATE it build on this one via `with_expiry`.
            expiry: BTreeMap::new(),
            // The CONFIGURED regime — the normal one, so the chart fixtures below keep pinning
            // the un-annotated render. The degraded regime has its own fixtures (issue #836).
            census_over_roster: true,
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
        // §D-STA-5 columns: `account · signal · session(mean/peak) · weekly · trend`. The fixture
        // carries no velocity overlay, so `velocity` / `runway` are uniformly `—` and elide before
        // the width fit — the sparse-fleet default. `session` is mean/peak (`50/99`) not peak-only.
        // The text columns (`account` / `signal`) and the sparkline left-align; the NUMERIC
        // `session` / `weekly` right-align inside their content-sized widths, so `40%` and `5%`
        // land their `%` in one terminal cell (issue #795).
        let r = two_account_charts();
        assert_eq!(
            render_chart_table(&r, &keys(&r), 60, false, false),
            "account  signal     session  weekly  trend\n\
             alpha    saturated    50/99     40%  ▃▅█\n\
             beta     underused    10/20      5%  ▂ ▂\n",
        );
    }

    #[test]
    fn bars_heatmap_percentiles_golden_wide() {
        let r = two_account_charts();
        assert_eq!(
            render_bars(&r, &keys(&r), 60, false).unwrap(),
            "contribution share\n\
             alpha  ██████████████████████████████░░░░░░░░░░   75%\n\
             beta   ██████████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░   25%\n",
            "the bar is bounded at 40 cells, not the 47 this width allows (issue #794)",
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

    // --- issue #794 AC: the bar is bounded, not full-bleed ----------------------------

    /// A bar row's fill + track glyphs.
    const BAR_GLYPHS: [char; 2] = ['█', '░'];
    /// A gauge row's track plus its three markers.
    const GAUGE_GLYPHS: [char; 4] = ['─', 'm', 'P', 'x'];

    /// How many of `glyphs` the row for `label` carries. None of them occurs in a label or in
    /// the figures trailing the row, so the count over the whole line IS that row's rendered
    /// bar or gauge width.
    fn row_cells(out: &str, label: &str, glyphs: &[char]) -> usize {
        out.lines()
            .find(|l| l.starts_with(label))
            .unwrap()
            .chars()
            .filter(|c| glyphs.contains(c))
            .count()
    }

    #[test]
    fn render_bars_bounds_the_bar_rather_than_filling_a_wide_terminal() {
        let r = two_account_charts();

        // Far past the bound the bar stops at 40 rather than tracking `w`. Unbounded it would
        // be 187 cells, drawing beta's 25% share as 140 cells of empty track and dwarfing the
        // account table it sits under.
        let wide = render_bars(&r, &keys(&r), 200, false).unwrap();
        assert_eq!(row_cells(&wide, "alpha", &BAR_GLYPHS), 40);
        assert_eq!(row_cells(&wide, "beta", &BAR_GLYPHS), 40);

        // The bound is a CEILING, not a fixed width: under it the bar still sizes to the
        // terminal, so the block keeps degrading with the narrow-width contract (issue #159).
        // The budget less the label column (`alpha`, 5), the two gaps and the percent field.
        let snug = render_bars(&r, &keys(&r), 40, false).unwrap();
        assert_eq!(row_cells(&snug, "alpha", &BAR_GLYPHS), 40 - (5 + 2 + 2 + 4));

        // The bound is SHARED with the sibling gauge of this same view — that precedent is
        // where the value comes from. Pin the pair so drifting one alone silently re-opens the
        // asymmetry this issue closed.
        let gauges = render_percentiles(&r, &keys(&r), 200, false).unwrap();
        assert_eq!(
            row_cells(&wide, "alpha", &BAR_GLYPHS),
            row_cells(&gauges, "alpha", &GAUGE_GLYPHS),
            "the contribution bar and the distribution gauge share one bound"
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
        // (bytes) AND padded on char count. The coverage `%` terminates `cov`, the first NUMERIC
        // column after the label, so it lands at one display column per row only when the label
        // column is sized AND padded on display width.
        let out = render_text(&wide_glyph_charts(), None);
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

    // --- issue #795 AC: numeric columns right-align within their content-sized width ----

    /// A three-account chart report whose `weekly` peaks render at THREE different widths —
    /// `7%` / `97%` / `100%`, the exact ragged trio issue #795 reports — under the given
    /// `handles`. Every account is present and non-zero in the single series bucket, so
    /// nothing elides; no velocity overlay, so `velocity` / `runway` elide as usual.
    fn mixed_width_weekly_charts(handles: [&'static str; 3]) -> Report {
        let row = |weekly: f64| stat(3, ds(0.30, 0.50, 0.40), weekly, 0.33);
        let accts = [
            (handles[0], row(0.07)),
            (handles[1], row(0.97)),
            (handles[2], row(1.00)),
        ];
        charts_report(&accts, &[&accts[..]])
    }

    /// The three `(handle, weekly cell)` pairs `mixed_width_weekly_charts` renders, for the
    /// ASCII handles. Each cell string occurs exactly once in its own row.
    const MIXED_WEEKLY: [(&str, &str); 3] = [("aa", "7%"), ("bbbb", "97%"), ("c", "100%")];

    /// The display column at which `cell` ENDS in the row of `out` starting with `row` — where
    /// that cell's right edge lands on screen. Equal across rows IFF the column right-aligns;
    /// equal to the header's own right edge IFF the header aligns with its data (issue #795).
    fn cell_right_edge(out: &str, row: &str, cell: &str) -> usize {
        let line = out
            .lines()
            .find(|l| l.starts_with(row))
            .unwrap_or_else(|| panic!("a row for `{row}`:\n{out}"));
        let end = line
            .find(cell)
            .unwrap_or_else(|| panic!("`{cell}` in row `{row}`:\n{out}"))
            + cell.len();
        display_width(&line[..end])
    }

    #[test]
    fn a_numeric_column_right_aligns_so_its_values_line_up_down_the_column() {
        // THE reported defect: `weekly` rendered `7%` / `97%` / `100%` LEFT-aligned, so the `%`
        // signs staggered and magnitudes could not be compared down the column. Right-aligned,
        // every value's right edge lands in ONE terminal cell whatever its width — and so does
        // the column's own header, so the header reads as part of the column it heads.
        let r = mixed_width_weekly_charts(["aa", "bbbb", "c"]);
        let out = render_chart_table(&r, &keys(&r), 80, false, false);
        let edges: Vec<usize> = MIXED_WEEKLY
            .iter()
            .map(|&(row, cell)| cell_right_edge(&out, row, cell))
            .collect();
        assert!(
            edges.iter().all(|&e| e == edges[0]),
            "weekly's `%` signs land in one terminal cell: {edges:?}\n{out}"
        );
        // A numeric column's HEADER right-aligns with its own cells, so the header reads as part
        // of the column it heads. `weekly` cannot witness that — its header is exactly as wide as
        // its column, so it fills the column under EITHER alignment. The piped surface's `cov`
        // can: a 3-cell header over 4-cell `100%` values, so a left-aligned header would sit one
        // column left of the data it heads.
        let piped = render_text(&r, None);
        let cov_header = cell_right_edge(&piped, "account", "cov");
        for (handle, _) in MIXED_WEEKLY {
            assert_eq!(
                cell_right_edge(&piped, handle, "100%"),
                cov_header,
                "`cov`'s header right-aligns with its own cells\n{piped}"
            );
        }
        // The TEXT columns are untouched: every handle still starts flush at column 0, however
        // short — right-aligning `account` would indent `c` under `bbbb`.
        for (handle, _) in MIXED_WEEKLY {
            assert!(
                out.lines().any(|l| l.starts_with(handle)),
                "`account` still left-aligns: `{handle}` starts its own row\n{out}"
            );
        }
    }

    /// The display column at which the row's LAST cell BEGINS. `trend` renders last on the TTY
    /// surface and its cells are narrower than its 5-wide header, so this is where a
    /// left-aligned `trend` sits — four columns left of where a right-aligned one would.
    fn last_cell_left_edge(line: &str) -> usize {
        let last = line.split_whitespace().next_back().unwrap();
        display_width(&line[..line.rfind(last).unwrap()])
    }

    #[test]
    fn alignment_survives_column_elision_and_the_priority_drop() {
        // THE desync trap: the per-column alignment slice must go through the SAME elision and
        // priority-drop retains as the widths and gaps. Derived from the pre-drop catalog
        // instead, it would shift the moment a column vanishes and pad cells on the WRONG side —
        // and the shed states are ones a wide-terminal golden never renders. Sweep the width
        // down over a fixture that BOTH elides (no velocity overlay → `velocity` / `runway` go,
        // from the MIDDLE of the catalog) and priority-drops, asserting at every width that
        // `weekly` still lands its right edge on its header's and that `trend` still sits flush
        // under the LEFT edge of its own.
        let r = mixed_width_weekly_charts(["aa", "bbbb", "c"]);
        let header_cols = |out: &str| out.lines().next().unwrap().split_whitespace().count();
        let full = header_cols(&render_chart_table(&r, &keys(&r), 200, false, false));
        let mut shed_states = 0;
        for w in (10..90).rev() {
            let out = render_chart_table(&r, &keys(&r), w, false, false);
            let header = out.lines().next().unwrap();
            shed_states += usize::from(header_cols(&out) < full);
            if header.contains("weekly") {
                let edge = cell_right_edge(&out, "account", "weekly");
                for (row, cell) in MIXED_WEEKLY {
                    assert_eq!(
                        cell_right_edge(&out, row, cell),
                        edge,
                        "at w={w} `weekly` still right-aligns under its header\n{out}"
                    );
                }
            }
            if header.contains("trend") {
                let edge = last_cell_left_edge(header);
                for line in out.lines().skip(1) {
                    assert_eq!(
                        last_cell_left_edge(line),
                        edge,
                        "at w={w} `trend` still left-aligns under its header\n{out}"
                    );
                }
            }
        }
        assert!(
            shed_states > 0,
            "the sweep must actually reach a shed state — otherwise this asserts nothing"
        );
    }

    /// Handles that stress the table's own width math, unlike [`WIDE_LABELS`] (whose widest is
    /// still narrower than the 7-cell `account` header, so the header sizes that column and a
    /// cell-sizing bug hides): an ASCII label (5 cells / 5 chars), a CJK run WIDER than the
    /// header (12 cells / 6 chars — so the CELLS size the column), and a ZWJ-family emoji (one
    /// coalesced 2-cell glyph / 5 code points). Char count and display width disagree on all
    /// three, and disagree DIFFERENTLY, so either a char-count SIZE or a char-count PAD
    /// staggers the rows.
    const WIDE_HANDLES: [&str; 3] = ["ascii", "日本語日本語", "👨\u{200D}👩\u{200D}👧"];

    #[test]
    fn a_right_aligned_numeric_column_lands_on_display_width() {
        // The issue #249 guarantee holds across the new pad side too: the `account` column is
        // sized and padded on DISPLAY width, so a CJK / ZWJ-emoji handle cannot stagger the
        // right-aligned numeric columns downstream of it — the `%` column would tear apart.
        let r = mixed_width_weekly_charts(WIDE_HANDLES);
        let out = render_chart_table(&r, &keys(&r), 80, false, false);
        let cells = ["7%", "97%", "100%"];
        let edges: Vec<usize> = WIDE_HANDLES
            .iter()
            .zip(cells)
            .map(|(&row, cell)| cell_right_edge(&out, row, cell))
            .collect();
        assert!(
            edges.iter().all(|&e| e == edges[0]),
            "weekly right-aligns at one display column behind wide-glyph handles: \
             {edges:?}\n{out}"
        );
    }

    #[test]
    fn full_charts_view_wide_tty() {
        let r = two_account_charts();
        let out = render_charts(&r, 60, false, false, None);
        assert!(out.starts_with("usage — last 24h (Jun 30–Jul 1)\n\n"));
        assert!(out.contains("account  signal     session  weekly  trend\n"));
        assert!(out.contains("contribution share\n"));
        assert!(out.contains("session pattern — hourly mean\n"));
        assert!(out.contains("session distribution — mean · p95 · peak\n"));
        assert!(out
            .trim_end()
            .contains("all-accounts-high (≥95%): 0 episodes (0s) ·"));
    }

    #[test]
    fn ascii_ramp_replaces_the_unicode_blocks() {
        // AC `TERM=dumb` / `--ascii` → ASCII ramp: the sparkline uses the ASCII intensity
        // run and carries no Unicode block glyph.
        let r = two_account_charts();
        let table = render_chart_table(&r, &keys(&r), 60, false, true);
        assert!(table.contains("alpha    saturated    50/99     40%  -+@\n"));
        assert!(table.contains("beta     underused    10/20      5%  : :\n"));
        for glyph in ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█', '░', '▒', '▓'] {
            assert!(!table.contains(glyph), "no Unicode block survives --ascii");
        }
    }

    // --- issue #159 AC: narrow terminal → priority column-drop, no row wrap -----------

    #[test]
    fn narrow_terminal_drops_trend_then_weekly_keeping_session_never_wrapping() {
        let r = two_account_charts();
        // The floor is `account · signal · session` (§D-STA-5); `velocity` / `runway` are elided
        // (this fixture has no overlay), so the priority drop reduces to `trend → weekly`. Just too
        // narrow for the trend column → it drops FIRST; signal + session + weekly stay.
        let w40 = render_chart_table(&r, &keys(&r), 40, false, false);
        assert_eq!(
            w40,
            "account  signal     session  weekly\n\
             alpha    saturated    50/99     40%\n\
             beta     underused    10/20      5%\n",
        );
        // Narrower still → weekly drops NEXT; the `account · signal · session` floor is always kept.
        let w30 = render_chart_table(&r, &keys(&r), 30, false, false);
        assert_eq!(
            w30,
            "account  signal     session\n\
             alpha    saturated    50/99\n\
             beta     underused    10/20\n",
        );
        // Below the floor width the floor OVERFLOWS rather than wrapping — nothing more drops, so
        // the render is byte-identical to the floor at any narrower width, one line per account.
        let w15 = render_chart_table(&r, &keys(&r), 15, false, false);
        assert_eq!(w15, w30, "below the floor width nothing more drops");
        assert_eq!(
            w15.lines().count(),
            3,
            "one header + one line per account: no wrap"
        );
        assert!(
            w15.contains("50/99") && w15.contains("10/20"),
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
        let out = render_charts(&r, 12, false, false, None);
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
            None,
        );
        assert_eq!(
            piped,
            render_text(&r, None),
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

    #[test]
    fn piped_numeric_table_golden_with_the_folded_columns() {
        // A frozen byte-golden of the piped numeric view — the unversioned de-facto contract
        // (#159): the window echo, the per-account table, and the contiguous aggregate roster
        // block. `aa` carries a velocity overlay (populated cells); `bb` has none (`—`). Since
        // issue #557 the piped table is the shared render_account_table over piped_columns at
        // w = usize::MAX / color = false, so this layout is CONTENT-SIZED — the deliberate,
        // reviewed reflow from the former hand-built FIXED-WIDTH fields — with the text columns
        // (`account` / `signal`) left-aligned and the numeric ones right-aligned WITHIN those
        // content-sized widths (§D-STA-5 as amended by issue #793; landed by issue #795). The two
        // halves are independent: sizing to content is not a licence to left-align, and aligning
        // right is not a return to fixed-width fields.
        // The piped-contract risk was resolved before reflowing:
        // no workflow parses piped column positions; `--json schema:1` is the machine contract. It
        // still pins the exact bytes so a future SILENT reflow (reordered / re-spaced columns) is
        // caught — the circular `piped == render_text` check cannot see a layout regression.
        let mut r = charts_report(
            &[
                ("aa", stat(3, ds(0.30, 0.90, 0.85), 0.40, 0.60)),
                ("bb", stat(3, ds(0.10, 0.15, 0.12), 0.20, 0.40)),
            ],
            &[],
        );
        r.velocity.insert(
            "aa".to_string(),
            AccountVelocity {
                session_rate: Some(0.00015),
                session_runway_secs: Some(7200),
                ..Default::default()
            },
        );
        assert_eq!(
            render_text(&r, None),
            "usage — last 24h (Jun 30–Jul 1)\n\n\
             account  signal      cov  session m/p/p95  weekly m/p/p95  caps  t@cap  share  velocity  runway\n\
             aa       saturated  100%         30/90/85          0/40/0     0     0s    60%  0.9%/min     ~2h\n\
             bb       underused  100%         10/15/12          0/20/0     0     0s    40%         —       —\n\
             \n  lowest utilisation: bb (session mean 10%)\n\
             roster: 0 swaps (0 session, 0 weekly, 0 manual, 0 forced, 0 emergency) · all-accounts-high (≥95%): 0 episodes (0s) · capacity holds: —\n",
        );
    }

    // --- issue #557 AC: the piped + TTY tables carry their declared column subsets ------

    /// Whether each header in `headers` occurs in `line`, IN ORDER (a later header found only after
    /// the previous one ends). Robust against a header being a substring of another (`session`
    /// inside `session m/p/p95`), which a naive `contains` set-check would miss.
    fn headers_appear_in_order(line: &str, headers: &[&str]) -> bool {
        let mut pos = 0;
        for h in headers {
            match line[pos..].find(h) {
                Some(i) => pos += i + h.len(),
                None => return false,
            }
        }
        true
    }

    #[test]
    fn piped_and_tty_tables_carry_their_declared_column_subsets() {
        // #557 column-parity golden: the piped (w = MAX) and TTY tables render from ONE catalog,
        // each its own declared subset. This pins each surface's columns AND order, so a future edit
        // that diverges one renderer's shape from the catalog — the latent drift #556 left between
        // the two former hand-built layouts — is caught. `aa` carries a velocity overlay AND an
        // expiry one (issue #883) so nothing elides and the full declared subsets render; the TTY
        // renders wide so nothing drops.
        //
        // The SERIES bucket is load-bearing for that "nothing elides" premise (issue #815). This
        // fixture used to pass `&[]`, so `trend` reached the header row with no cell under it at
        // any row — the assertion below read as "the full subset renders" while the column it was
        // counting was the reported defect. One populated bucket makes the premise true.
        let aa = stat(3, ds(0.30, 0.90, 0.85), 0.40, 0.60);
        let bb = stat(3, ds(0.10, 0.15, 0.12), 0.20, 0.40);
        let mut r = charts_report(&[("aa", aa), ("bb", bb)], &[&[("aa", aa), ("bb", bb)]]);
        r.velocity.insert(
            "aa".to_string(),
            AccountVelocity {
                session_rate: Some(0.00015),
                session_runway_secs: Some(7200),
                ..Default::default()
            },
        );
        let now = r.window.end;
        r.expiry.insert(
            "aa".to_string(),
            expiry_in(
                now,
                3 * DAY_SECS,
                crate::observability::ExpiryHorizon::Within,
            ),
        );

        // The declared subsets — the catalog constructors' headers, in render order.
        let piped_headers: Vec<&str> = piped_columns().iter().map(|c| c.header).collect();
        let tty_headers: Vec<&str> = tty_columns().iter().map(|c| c.header).collect();
        assert_eq!(
            piped_headers,
            [
                "account",
                "signal",
                "cov",
                "session m/p/p95",
                "weekly m/p/p95",
                "caps",
                "t@cap",
                "share",
                "velocity",
                "runway",
                "expiry",
            ],
            "the piped subset is the wide numeric catalog"
        );
        assert_eq!(
            tty_headers,
            ["account", "signal", "session", "weekly", "runway", "velocity", "expiry", "trend"],
            "the TTY subset is the compact chart catalog"
        );

        // Each surface's RENDERED header row carries its declared subset, in order.
        let piped = render_text(&r, None);
        let tty = render_chart_table(&r, &keys(&r), 200, false, false);
        let piped_header_line = piped
            .lines()
            .find(|l| l.starts_with("account"))
            .expect("a piped header row");
        let tty_header_line = tty.lines().next().expect("a tty header row");
        assert!(
            headers_appear_in_order(piped_header_line, &piped_headers),
            "piped header carries its declared subset in order: {piped_header_line:?}"
        );
        assert!(
            headers_appear_in_order(tty_header_line, &tty_headers),
            "tty header carries its declared subset in order: {tty_header_line:?}"
        );

        // The SHARED columns (`account · signal · velocity · runway`) are ONE catalog entry, so they
        // head both surfaces AND produce the same cell CONTENT for a given account (the same
        // extractor runs on both) — the anti-drift guarantee. `aa`'s shared cell strings therefore
        // appear verbatim on both surfaces (a content check; each surface pads them to its own
        // column width, so position differs).
        for shared in ["account", "signal", "velocity", "runway"] {
            assert!(
                piped_header_line.contains(shared) && tty_header_line.contains(shared),
                "`{shared}` heads both surfaces"
            );
        }
        for cell in ["saturated", "0.9%/min", "~2h"] {
            assert!(
                piped.contains(cell) && tty.contains(cell),
                "aa's shared `{cell}` renders identically on both surfaces"
            );
        }

        // Neither surface leaks the OTHER's exclusive columns.
        assert!(
            !piped_header_line.contains("trend"),
            "piped omits the tty-only `trend`: {piped_header_line:?}"
        );
        for piped_only in ["cov", "caps", "t@cap", "share"] {
            assert!(
                !tty_header_line.contains(piped_only),
                "tty omits the piped-only `{piped_only}`: {tty_header_line:?}"
            );
        }
    }

    // --- issue #159 AC: NO_COLOR / --no-color → zero ANSI, full signal in text --------

    #[test]
    fn color_gate_governs_every_ansi_byte() {
        let r = two_account_charts();
        // Gate open → the utilisation bands tint the cells (alpha's hot 50/99 session reads red).
        // Both tinted columns are RIGHT-aligned (issue #795), so these two literals also pin the
        // SGR pair hugging the cell TEXT with the leading pad outside it — never weaken them to a
        // bare `contains("\x1b[31m")`, or a pad drifting inside the escape stops being caught.
        let colored = render_chart_table(&r, &keys(&r), 60, true, false);
        assert!(
            colored.contains("\x1b[31m50/99\x1b[0m"),
            "hot session reads red"
        );
        assert!(
            colored.contains("\x1b[32m40%\x1b[0m"),
            "a low weekly reads green"
        );
        // Gate closed → not one escape byte anywhere in the whole view, yet the full signal
        // survives in text (the percentages and the glyphs).
        let plain = render_charts(&r, 60, false, false, None);
        assert!(!plain.contains('\x1b'), "no ANSI when the gate is closed");
        assert!(
            plain.contains("50/99") && plain.contains("▃▅█"),
            "full signal without colour"
        );
    }

    // --- issue #556 AC: per-account columns, empty-column elision, floor, roster block -----

    #[test]
    fn priority_drop_sheds_trend_then_velocity_then_runway_then_weekly_keeping_the_floor() {
        // With a velocity overlay PRESENT (so `velocity` + `runway` do NOT elide), the narrow-width
        // priority drop sheds `trend → velocity → runway → weekly` in that exact order (§D-STA-5),
        // while the `account · signal · session` floor is always kept and OVERFLOWS, never wrapping.
        let mut r = two_account_charts();
        for h in ["alpha", "beta"] {
            r.velocity.insert(
                h.to_string(),
                AccountVelocity {
                    session_rate: Some(0.001),
                    session_runway_secs: Some(7200),
                    ..Default::default()
                },
            );
        }
        let header_cols = |w: usize| -> Vec<String> {
            render_chart_table(&r, &keys(&r), w, false, false)
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .map(str::to_string)
                .collect()
        };
        // Wide → the full §D-STA-5 catalog order.
        assert_eq!(
            header_cols(200),
            ["account", "signal", "session", "weekly", "runway", "velocity", "trend"]
        );
        // Walk the width down and record the order in which columns vanish.
        let mut prev = header_cols(200);
        let mut shed: Vec<String> = Vec::new();
        for w in (5..200).rev() {
            let now = header_cols(w);
            for c in &prev {
                if !now.contains(c) {
                    shed.push(c.clone());
                }
            }
            prev = now;
        }
        assert_eq!(shed, ["trend", "velocity", "runway", "weekly"]);
        // The floor is `account · signal · session`, never wrapped — one header + one line per
        // account even when it overflows a very narrow width.
        assert_eq!(header_cols(5), ["account", "signal", "session"]);
        assert_eq!(
            render_chart_table(&r, &keys(&r), 5, false, false)
                .lines()
                .count(),
            3,
            "one header + one line per account: no wrap"
        );
    }

    #[test]
    fn the_signal_cell_is_a_dash_for_an_unobserved_account_never_a_fabricated_class() {
        // Gap honesty at the CELL level (§D-STA-5): an OBSERVED account carries its neutral class
        // word; an unobserved one (`seen == 0`, zeroed readings) shows `—`, never a fabricated
        // "underused". `signal_sgr` leaves both the gap and the balanced middle un-emphasised (an
        // empty SGR would emit a bare reset), colouring only the two symmetric deviations alike.
        assert_eq!(
            signal_cell(&stat(3, ds(0.10, 0.15, 0.12), 0.0, 0.5)),
            "underused"
        );
        assert_eq!(
            signal_cell(&stat(3, ds(0.40, 0.60, 0.50), 0.0, 0.5)),
            "balanced"
        );
        assert_eq!(signal_cell(&stat(0, ds(0.00, 0.00, 0.00), 0.0, 0.5)), "—");
        assert_eq!(signal_sgr(&stat(0, ds(0.0, 0.0, 0.0), 0.0, 0.5)), None);
        assert_eq!(signal_sgr(&stat(3, ds(0.4, 0.6, 0.5), 0.0, 0.5)), None);
        assert_eq!(
            signal_sgr(&stat(3, ds(0.1, 0.15, 0.12), 0.0, 0.5)),
            Some("33")
        );
        assert_eq!(
            signal_sgr(&stat(3, ds(0.7, 0.95, 0.9), 0.0, 0.5)),
            Some("33")
        );
    }

    #[test]
    fn the_roster_block_is_one_contiguous_indented_aggregate_beneath_the_table() {
        // §D-STA-5: the aggregate summary (lowest-utilisation [+ fleet]) and the roster line form
        // ONE block beneath the table — the aggregate lines 2-space indented, and NO blank line
        // separating them from the roster line (they read as a single foot).
        let text = render_text(&report_fixture(), None);
        let lines: Vec<&str> = text.lines().collect();
        let lu = lines
            .iter()
            .position(|l| l.contains("lowest utilisation:"))
            .expect("a lowest-utilisation line");
        let roster = lines
            .iter()
            .position(|l| l.starts_with("roster:"))
            .expect("a roster line");
        assert!(
            lines[lu].starts_with("  lowest utilisation:"),
            "the aggregate line is 2-space indented: {:?}",
            lines[lu]
        );
        assert!(lu < roster, "the aggregate precedes the roster line");
        assert!(
            lines[lu..=roster].iter().all(|l| !l.is_empty()),
            "the bottom block is contiguous — no blank line within it: {:?}",
            &lines[lu..=roster]
        );
    }

    #[test]
    fn velocity_and_runway_columns_elide_when_empty_and_appear_when_any_account_has_them() {
        // Empty-column elision (§D-STA-5): a fleet with NO computable rate shows neither column; a
        // fleet where AT LEAST ONE account has a rate shows the column, with `—` for the accounts
        // lacking it (an explicit gap, never a fabricated 0).
        let sparse = two_account_charts(); // no velocity overlay
        let text = render_text(&sparse, None);
        assert!(
            !text.contains("velocity") && !text.contains("runway"),
            "both columns elide on a sparse fleet: {text}"
        );

        let mut mixed = two_account_charts();
        mixed.velocity.insert(
            "alpha".to_string(),
            AccountVelocity {
                session_rate: Some(0.001),
                session_runway_secs: Some(7200),
                ..Default::default()
            },
        );
        let text = render_text(&mixed, None);
        assert!(
            text.contains("velocity") && text.contains("runway"),
            "the columns appear when one account has the datum: {text}"
        );
        assert!(
            text.contains("6.0%/min") && text.contains("~2h"),
            "alpha's populated velocity + runway cells: {text}"
        );
        assert!(
            text.contains('—'),
            "beta's missing datum is an explicit gap, not a fabricated 0: {text}"
        );
    }

    // --- AC (issue #815): a BLANK cell is a gap for elision, in BOTH its spellings -----

    /// The chart table's header words at a width nothing sheds at, so a missing column is the
    /// EMPTY-COLUMN ELISION and never the priority drop. Read off the rendered header line —
    /// the layout the reader actually gets — rather than a hand-built column list, so the
    /// assertion exercises the real pre-pass in [`render_account_table`].
    fn elided_wide_header(r: &Report) -> Vec<String> {
        render_chart_table(r, &keys(r), 200, false, false)
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    /// The `stats-all-na` shape — every account present but never observed (`seen == 0`) — over
    /// `buckets` series buckets. It is the shape that spells an all-blank `trend`, and the bucket
    /// count picks WHICH spelling: `0` renders the EMPTY string (no bucket to draw), any other
    /// count renders that many BREAKS, one space each ([`spark_glyph`]), because an unobserved
    /// account is a gap in every bucket. Both render identically — `render_line` trims the pad —
    /// and before issue #815 both kept the column. The bucket-bearing shape is the reachable one:
    /// [`bucket_bounds`] yields no buckets only for an empty window.
    fn all_unobserved_charts(buckets: usize) -> Report {
        let unseen = stat(0, ds(0.0, 0.0, 0.0), 0.0, 0.0);
        let bucket: &[(&str, AccountStats)] = &[("alpha", unseen), ("beta", unseen)];
        charts_report(bucket, &vec![bucket; buckets])
    }

    #[test]
    fn an_all_blank_optional_column_elides_in_both_spellings_of_blank() {
        for buckets in [0usize, 1, 4] {
            let r = all_unobserved_charts(buckets);
            let header = elided_wide_header(&r);
            assert!(
                !header.contains(&"trend".to_string()),
                "at {buckets} bucket(s) the all-blank `trend` column elides rather than heading \
                 an empty space: {header:?}"
            );
            // The header row is the whole claim: before #815 `trend` sat here with no cell under
            // it at ANY row, so the render asserted trend data the report does not hold.
            let out = render_chart_table(&r, &keys(&r), 200, false, false);
            assert!(
                !out.contains("trend"),
                "and it is gone from the render, not merely from the split: {out}"
            );
        }
    }

    #[test]
    fn elision_keeps_a_column_with_any_mark_and_never_touches_the_floor() {
        // The negative half. Without it a blanket `retain(|_| false)` would pass the test above.
        let r = all_unobserved_charts(2);
        let header = elided_wide_header(&r);
        // FLOOR (`priority: None`): kept even though every `signal` cell is the gap sentinel and
        // every `session` / `weekly` cell a zero. An unmeasured roster is unmeasured, not absent.
        assert_eq!(
            signal_cell(&stat(0, ds(0.0, 0.0, 0.0), 0.0, 0.0)),
            EXPIRY_GAP,
            "the premise: this fixture's `signal` column IS uniformly the sentinel"
        );
        assert_eq!(
            header,
            ["account", "signal", "session", "weekly"],
            "the `account · signal · session` floor survives an all-gap roster, and `weekly` \
             (droppable, `Some(5)`) survives on its own `0%` marks — only the blank and the \
             all-sentinel droppables go"
        );

        // A single mark anywhere in the column keeps it — `beta` stays blank in every bucket, and
        // that per-ROW blank is NOT this defect. This is `stats-sparse-fleet`'s `delta` shape.
        let alpha = stat(4, ds(0.50, 0.99, 0.80), 0.40, 0.75);
        let unseen = stat(0, ds(0.0, 0.0, 0.0), 0.0, 0.0);
        let mixed = charts_report(
            &[("alpha", alpha), ("beta", unseen)],
            &[&[("alpha", alpha), ("beta", unseen)]],
        );
        assert!(
            elided_wide_header(&mixed).contains(&"trend".to_string()),
            "one populated sparkline keeps the column for the whole cohort"
        );
        let rows = render_chart_table(&mixed, &keys(&mixed), 200, false, false);
        assert!(
            rows.lines()
                .any(|l| l.starts_with("beta") && !l.contains('█')),
            "and `beta`'s own cell stays blank — no sentinel is invented for it: {rows}"
        );
    }

    #[test]
    fn a_uniformly_sentinel_column_still_elides() {
        // The pre-existing contract issue #815 must not regress: `velocity` / `runway` spell the
        // gap `—`, not blank, and an all-`—` droppable column elides on that string alone.
        let sparse = two_account_charts(); // populated sparklines, no velocity overlay
        let header = elided_wide_header(&sparse);
        assert!(
            header.contains(&"trend".to_string()),
            "the fixture's premise: `trend` is populated here, so only the sentinel is on trial"
        );
        assert!(
            !header.contains(&"velocity".to_string()) && !header.contains(&"runway".to_string()),
            "an all-sentinel droppable column elides exactly as before: {header:?}"
        );
    }

    // --- AC (issue #883): per-account expiry is a COLUMN of the one table -------------

    /// An OBSERVED expiry `offset_secs` from `now`, classified `state`. Builds the axis
    /// `Report::expiry` cannot yet reach from real data — the offline `HistoryStore` seam carries
    /// no refresh-token deadline until issue #880's durable horizon Event exists to be read out of
    /// the event log — so the column's populated rendering is proven here rather than left
    /// untested behind an overlay that is empty on every production path.
    ///
    /// `now` is the caller's `report.window.end`, which is what [`account_rows`] renders each cell
    /// against; passing it explicitly keeps the helper correct for fixtures with different windows.
    fn expiry_in(
        now: i64,
        offset_secs: i64,
        state: crate::observability::ExpiryHorizon,
    ) -> AccountExpiry {
        AccountExpiry {
            expires_at: Some(now + offset_secs),
            horizon_state: state,
            // Issue #879 shipped cohort detection on the DAEMON, and deliberately not here: the
            // offline `stats` verb never talks to the daemon, so this column has no cohort to
            // carry.
            cohort_id: None,
        }
    }

    #[test]
    fn the_gap_sentinel_is_one_string_across_every_producer() {
        // The elision pre-pass in `render_account_table` retains a droppable column only when some
        // cell differs from `EXPIRY_GAP`. That is correct ONLY while every producer spells the
        // sentinel identically — `signal` / `velocity` / `runway` write the em dash inline, and
        // `crate::cli::expiry_table_cell` returns the constant. Pin them equal, so a change to one
        // is a test failure rather than a column that silently stops eliding. The producer here is
        // the TABLE cell, not the bare fact underneath it (issue #934): a mark that ever wrapped
        // the gap would take the elision down with it, and only this spelling would catch that.
        assert_eq!(velocity_cell(None), EXPIRY_GAP);
        assert_eq!(runway_cell(None), EXPIRY_GAP);
        assert_eq!(expiry_table_cell(None, 0), EXPIRY_GAP);
        // `signal` is the floor (`priority: None`) so it never elides, but it is the same gap fact.
        assert_eq!(
            signal_cell(&stat(0, ds(0.0, 0.0, 0.0), 0.0, 0.0)),
            EXPIRY_GAP
        );
    }

    #[test]
    fn expiry_is_a_column_of_the_one_table_eliding_when_no_account_has_a_deadline() {
        // §D-STA-5's structural rule: a per-account metric is a COLUMN of the one per-account
        // table — never a band or a footer list keyed per account (the shape #543/#544 were
        // retired for). Same empty-column elision as `velocity` / `runway`.
        let sparse = two_account_charts(); // no expiry overlay — every production path today
        let text = render_text(&sparse, None);
        assert!(
            !text.contains("expiry"),
            "the column elides on a fleet with no observed deadline: {text}"
        );

        let mut mixed = two_account_charts();
        let now = mixed.window.end;
        mixed.expiry.insert(
            "alpha".to_string(),
            expiry_in(
                now,
                3 * DAY_SECS,
                crate::observability::ExpiryHorizon::Within,
            ),
        );
        let text = render_text(&mixed, None);
        assert!(
            text.contains("expiry"),
            "the column appears when one account has the datum: {text}"
        );

        // It is a COLUMN — its header sits on the same header row as `account`, and its cell on
        // alpha's own row. Not a band, not a footer list: nothing keyed per account below the table.
        let header = text
            .lines()
            .find(|l| l.trim_start().starts_with("account"))
            .expect("a header row");
        assert!(
            header.contains("expiry"),
            "`expiry` is a header of the per-account table: {header:?}"
        );
        let alpha = text
            .lines()
            .find(|l| l.starts_with("alpha"))
            .expect("alpha's row");
        assert!(alpha.contains("3d"), "alpha's populated cell: {alpha:?}");
        let beta = text
            .lines()
            .find(|l| l.starts_with("beta"))
            .expect("beta's row");
        assert!(
            beta.contains(EXPIRY_GAP),
            "beta's unobserved deadline is an explicit gap, never a silent 'fine': {beta:?}"
        );
    }

    #[test]
    fn the_expiry_column_right_aligns_and_sheds_first_under_a_narrow_terminal() {
        // ALIGNMENT (§D-STA-5 as amended by #793 → #795): a time-until is a NUMERIC cell, so it
        // RIGHT-aligns — the trap here is that every older render was all-left.
        let mut r = two_account_charts();
        let now = r.window.end;
        r.expiry.insert(
            "alpha".to_string(),
            expiry_in(
                now,
                3 * DAY_SECS,
                crate::observability::ExpiryHorizon::Within,
            ),
        );
        r.expiry.insert(
            "beta".to_string(),
            expiry_in(
                now,
                29 * DAY_SECS,
                crate::observability::ExpiryHorizon::Beyond,
            ),
        );
        let wide = render_chart_table(&r, &keys(&r), 200, false, false);
        // The two cells are of DIFFERENT widths, so they discriminate the alignment: right-aligned
        // they END at the same column and the SHORTER one starts LATER; left-aligned they would
        // START at the same column and end at different ones. Both cells sit left of the multibyte
        // sparkline, so byte offsets are display columns here.
        //
        // Which of the two is shorter INVERTED with issue #934: `alpha` is inside the configured
        // horizon and so renders `[3d]` (4 columns), against `beta`'s unmarked `29d` (3). Compare
        // the whole cells rather than the durations inside them — the mark is part of the cell the
        // column pads, which is exactly what §D-STA-5's "pad on display-width before colorize"
        // requires it to be.
        let row = |h: &str| {
            wide.lines()
                .find(|l| l.starts_with(h))
                .unwrap_or_else(|| panic!("{h}'s row in {wide}"))
                .to_owned()
        };
        let (alpha, beta) = (row("alpha"), row("beta"));
        let a_start = alpha.find("[3d]").expect("alpha's marked expiry cell");
        let b_start = beta.find("29d").expect("beta's expiry cell");
        assert_eq!(
            a_start + "[3d]".len(),
            b_start + "29d".len(),
            "right-aligned: both expiry cells end at the same column: {wide}"
        );
        assert!(
            b_start > a_start,
            "right-aligned: the shorter cell (`29d`, now that `[3d]` carries the mark) starts \
             LATER, i.e. it is padded on the LEFT — a left-aligned column would start both at the \
             same offset: {wide}"
        );

        // SHED ORDER, as BEHAVIOUR rather than as a priority integer: rendered ONE column narrower
        // than the full table, `expiry` is the column that goes and `trend` — the next-lowest
        // priority — survives. Both are populated here, so neither can elide; only the priority
        // drop can remove them.
        let full_width = wide
            .lines()
            .map(display_width)
            .max()
            .expect("a rendered table");
        let narrowed = render_chart_table(&r, &keys(&r), full_width - 1, false, false);
        assert!(
            !narrowed.contains("expiry"),
            "expiry sheds first: {narrowed}"
        );
        assert!(
            narrowed.contains("trend"),
            "and it sheds ALONE — `trend`, the next-lowest priority, survives: {narrowed}"
        );

        // The recorded order behind that drop, with the pre-existing columns' RELATIVE order
        // unchanged by the renumbering issue #883 forced on them.
        let order: Vec<Option<u8>> = [
            col_expiry(),
            col_trend(),
            col_velocity(),
            col_runway(),
            col_weekly_peak(),
        ]
        .iter()
        .map(|c| c.priority)
        .collect();
        assert_eq!(
            order,
            [Some(1), Some(2), Some(3), Some(4), Some(5)],
            "shed order is expiry → trend → velocity → runway → weekly"
        );
    }

    #[test]
    fn the_expiry_column_renders_present_tense_state_and_never_an_imperative() {
        // The D-STA-6 neutral-framing conditions, applied to the one surface the guard scans.
        // A refresh-token deadline CLEARS the projection ban — it is a server-issued timestamp,
        // not a rate extrapolation — but only under two conditions: present-tense STATE, and no
        // banned token. Both are asserted over a render that actually CARRIES the column, which
        // the guard's own fixtures cannot do (their overlay is empty, so the column elides).
        let mut r = three_band_report();
        let now = r.window.end;
        let handles: Vec<String> = r.summary.per_account.keys().cloned().collect();
        for (i, handle) in handles.iter().enumerate() {
            let (offset, state) = match i % 3 {
                0 => (3 * DAY_SECS, crate::observability::ExpiryHorizon::Within),
                1 => (29 * DAY_SECS, crate::observability::ExpiryHorizon::Beyond),
                _ => (-DAY_SECS, crate::observability::ExpiryHorizon::Lapsed),
            };
            r.expiry
                .insert(handle.clone(), expiry_in(now, offset, state));
        }
        for surface in [
            render_text(&r, None),
            render_chart_table(&r, &keys(&r), 200, true, false),
        ] {
            assert!(
                surface.contains("expiry"),
                "the fixture actually carries the column — otherwise this proves nothing: {surface}"
            );
            assert_eq!(
                scan_banned(&surface),
                None,
                "the expiry column emits no banned token: {surface:?}"
            );
            // Present-tense STATE, never an imperative or a remedy: the cell reports the fact and
            // leaves the action to the operator.
            for imperative in ["re-login", "renew", "log in", "sessiometer login", "you"] {
                assert!(
                    !surface.contains(imperative),
                    "the expiry column names no imperative (`{imperative}`): {surface:?}"
                );
            }
        }
    }

    #[test]
    fn full_email_labels_degrade_within_80_and_70_columns_never_wrapping() {
        // AC (#556 width-fit falsifier): the reporter's 6-account fleet with FULL operator email
        // labels (~29 cols — the wide account column, never shortened). SPARSE (no velocity overlay)
        // → `velocity` / `runway` elide and the table fits 80 and 70. DENSE (every account has a
        // rate) → at 80 the lowest-priority `trend` sheds, by 70 `velocity` too — strictly by
        // priority — while the `account · signal · session` floor is one line per row, never wrapped.
        let emails = [
            "oleksii@pelykh-consulting.com",
            "oleksii@pelykh.com",
            "oleksii@pelykh.consulting",
            "oleksii@pelykhconsulting.com",
            "oleksii@pelykhconsulting.eu",
            "oleksii@pelykhconsulting.fr",
        ];
        let accts: Vec<(&str, AccountStats)> = emails
            .iter()
            .enumerate()
            .map(|(i, &e)| {
                let base = 0.30 + i as f64 * 0.10;
                (e, stat(3, ds(base, base + 0.05, base), 0.40, 1.0 / 6.0))
            })
            .collect();

        // SPARSE: velocity + runway elide (all `—`); the table fits 80 and 70 with no wrap.
        let sparse = charts_report(&accts, &[]);
        for w in [80, 70] {
            let t = render_chart_table(&sparse, &keys(&sparse), w, false, false);
            assert_eq!(t.lines().count(), 7, "header + 6 rows, no wrap at {w}");
            assert!(
                !t.contains("velocity") && !t.contains("runway"),
                "a sparse fleet elides both columns at {w}: {t}"
            );
            assert!(
                t.lines().all(|l| l.chars().count() <= w),
                "the sparse table fits within {w}: {t}"
            );
        }

        // DENSE: give every account a rate + runway so neither column elides.
        let mut dense = charts_report(&accts, &[]);
        for &e in &emails {
            dense.velocity.insert(
                e.to_string(),
                AccountVelocity {
                    session_rate: Some(0.001),
                    session_runway_secs: Some(7200),
                    ..Default::default()
                },
            );
        }
        let header = |w: usize| -> Vec<String> {
            render_chart_table(&dense, &keys(&dense), w, false, false)
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .map(str::to_string)
                .collect()
        };
        // At 80 the lowest-priority `trend` sheds; `velocity` is still shown. By 70, `velocity` too.
        assert!(
            !header(80).contains(&"trend".to_string()),
            "trend sheds first at 80: {:?}",
            header(80)
        );
        assert!(
            header(80).contains(&"velocity".to_string()),
            "velocity survives at 80: {:?}",
            header(80)
        );
        assert!(
            !header(70).contains(&"velocity".to_string()),
            "velocity sheds by 70: {:?}",
            header(70)
        );
        // The `account · signal · session` floor is always kept, one line per account, never wrapped.
        for w in [80, 70, 40] {
            assert_eq!(
                render_chart_table(&dense, &keys(&dense), w, false, false)
                    .lines()
                    .count(),
                7,
                "one header + 6 rows at {w}: no wrap"
            );
            let hdr = header(w);
            assert_eq!(hdr[0], "account");
            assert_eq!(hdr[1], "signal");
            assert_eq!(hdr[2], "session");
        }
    }

    // --- issue #159: --json wire stays byte-stable vs #158 (no chart glyphs) ----------

    #[test]
    fn charts_never_leak_into_the_json_wire() {
        // The charts are presentation-only: the schema:1 wire carries no glyph, no ANSI, no
        // chart field — the #158 contract is unchanged by #159.
        let json = render_json(&two_account_charts(), None).unwrap();
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
        let out = render_charts(&empty, 80, true, false, None);
        assert!(out.contains("no per-account usage in this window"));
        assert!(out.contains("roster:"));

        // A single account with a single sample and no series buckets.
        let single = charts_report(&[("solo", stat(1, ds(0.5, 0.5, 0.5), 0.5, 1.0))], &[]);
        let _ = render_charts(&single, 80, true, false, None);
        let _ = render_charts(&single, 1, true, true, None);

        // An account present in the summary but a GAP in every series bucket.
        let all_gap = charts_report(
            &[("ghost", stat(1, ds(0.0, 0.0, 0.0), 0.0, 0.0))],
            &[&[], &[]],
        );
        let out = render_charts(&all_gap, 80, false, false, None);
        assert!(
            out.contains("ghost"),
            "an all-gap account still lists — its own row survives even though every column that \
             could have charted it has nothing to say"
        );
        // Its trend is all breaks, and it is the ONLY account, so the whole column is blank and
        // elides (issue #815) rather than heading an empty space. Pinned here because this is the
        // one PRE-EXISTING fixture in the file whose blank `trend` is spelled in BREAKS rather
        // than the empty string — the reachable spelling. The helper `all_unobserved_charts` this
        // change adds spells it that way too, at 1 and 4 buckets.
        assert!(
            !out.contains("trend"),
            "the all-break `trend` column elides on a one-account all-gap fleet: {out}"
        );
        // A pathological width of 0 must not panic either.
        let _ = render_charts(&two_account_charts(), 0, true, true, None);
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
            session_ceiling: 0.80,
            weekly_ceiling: 0.95,
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
        let json = render_json(&report_fixture(), None).unwrap();
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
        let json = render_json(&report_fixture(), None).unwrap();
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
        let json = render_json(&report_fixture(), None).unwrap();
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
        let json = render_json(&report, None).unwrap();

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
        let json = render_json(&report_fixture(), None).unwrap();
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
        let out = render_text(&report_fixture(), None);
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

    // --- issue #804: the all-accounts-high census reads honestly -----------------------

    /// A whole DAY, the window every census line below is measured against.
    const CENSUS_WINDOW: i64 = 86_400;

    /// A roster line from a census with the given `(episodes, secs, jointly-covered secs)` at
    /// the given water, over a [`CENSUS_WINDOW`] period. Swap fields are irrelevant here and
    /// stay zero; so is `per_account`, which the roster line never reads.
    fn census_line(episodes: u32, secs: i64, covered: i64, water: f64) -> String {
        census_line_over(episodes, secs, covered, water, CENSUS_WINDOW)
    }

    /// [`census_line`] over an explicit `period_secs` — the degenerate zero-length window.
    fn census_line_over(
        episodes: u32,
        secs: i64,
        covered: i64,
        water: f64,
        period_secs: i64,
    ) -> String {
        roster_line(
            &UsageReport {
                period: Period::new(0, period_secs),
                per_account: BTreeMap::new(),
                roster: RosterStats {
                    all_high_episodes: episodes,
                    all_high_secs: secs,
                    all_high_covered_secs: covered,
                    high_threshold: water,
                    ..Default::default()
                },
            },
            true,
        )
    }

    // --- issue #803: the capacity-holds cell ------------------------------------------

    /// A roster line from a capacity-holds census with the given figures, over a
    /// [`CENSUS_WINDOW`] period at the shipping-default boundary (0.80 session / 0.97 weekly).
    fn capacity_line(holds: u32, session: u32, weekly: u32, secs: i64, covered: i64) -> String {
        roster_line(
            &UsageReport {
                period: Period::new(0, CENSUS_WINDOW),
                per_account: BTreeMap::new(),
                roster: RosterStats {
                    capacity_holds: holds,
                    capacity_holds_session: session,
                    capacity_holds_weekly: weekly,
                    capacity_hold_secs_lower_bound: secs,
                    capacity_hold_covered_secs: covered,
                    capacity_session_line: 0.80,
                    capacity_weekly_line: 0.97,
                    ..Default::default()
                },
            },
            true,
        )
    }

    #[test]
    fn a_measured_capacity_census_renders_its_counts_boundary_and_bound_marker() {
        // The MEASURED branch, pinned as an exact string. Every other render assertion and all
        // eleven goldens exercise only the `—` branch, so without this the whole cell was
        // ungated: a mutation transposing the session/weekly counts AND dropping the `≥` marker
        // passed the entire suite. Both of those are exactly what this asserts.
        let line = capacity_line(7, 2, 5, 106_080, CENSUS_WINDOW);
        assert!(
            line.ends_with(
                "capacity holds (session ≥80%, weekly ≥97%): 7 (2 session / 5 weekly) · ≥29h28m\n"
            ),
            "measured capacity cell renders counts, boundary and the bound marker: {line}"
        );
    }

    #[test]
    fn a_measured_capacity_census_with_no_holds_is_calm_and_not_the_gap_sentinel() {
        // The branch a healthy fleet sees every day, and the one nothing else pins: the goldens
        // and every other assertion here exercise either `—` or a NONZERO hold count, so a
        // regression that collapsed measured-calm into the `—` branch would erase the distinction
        // between "the daemon was never cornered" and "we could not tell" — the very distinction
        // this readout exists to make — and still pass the suite.
        let calm = capacity_line(0, 0, 0, 0, CENSUS_WINDOW);
        assert!(
            calm.ends_with(
                "capacity holds (session ≥80%, weekly ≥97%): 0 (0 session / 0 weekly) · ≥0s\n"
            ),
            "a MEASURED zero states its boundary and stays marked as a bound: {calm}"
        );
        assert!(
            !calm.contains("capacity holds: —"),
            "measured calm is NOT the gap sentinel: {calm}"
        );
    }

    #[test]
    fn the_capacity_cell_marks_its_duration_as_a_bound_and_never_states_an_exact_figure() {
        // REQ-STA-B-011: until a hold's END is reconstructable offline, the duration is a BOUND
        // and must be marked as one. The `≥` is that marking — a render that dropped it would
        // state an exact figure the aggregate cannot support, which is the one thing the
        // requirement forbids. Asserted separately from the shape above so a future re-layout
        // of the cell cannot quietly take the marker with it.
        let line = capacity_line(1, 1, 0, 3_600, CENSUS_WINDOW);
        assert!(line.contains("· ≥1h\n"), "the duration is marked: {line}");
        assert!(
            !line.contains(": 1h\n") && !line.contains(" 1h\n"),
            "an UNMARKED duration would read as exact: {line}"
        );
    }

    #[test]
    fn the_capacity_cell_names_each_boundary_by_its_own_dimension() {
        // The two lines are independent (issue #41) and both operator-configurable, so a
        // positional `≥80%/≥97%` pair would re-introduce in the render the silent transposition
        // `ViabilityBoundary` is a named struct to prevent. A non-default, ASYMMETRIC boundary
        // proves each number is bound to its own dimension rather than to its position.
        let line = roster_line(
            &UsageReport {
                period: Period::new(0, CENSUS_WINDOW),
                per_account: BTreeMap::new(),
                roster: RosterStats {
                    capacity_holds: 1,
                    capacity_holds_session: 1,
                    capacity_hold_secs_lower_bound: 60,
                    capacity_hold_covered_secs: CENSUS_WINDOW,
                    capacity_session_line: 0.55,
                    capacity_weekly_line: 0.91,
                    ..Default::default()
                },
            },
            true,
        );
        assert!(
            line.contains("capacity holds (session ≥55%, weekly ≥91%):"),
            "each line is named by its own dimension: {line}"
        );
    }

    #[test]
    fn an_untaken_capacity_census_renders_the_gap_sentinel_and_states_no_boundary() {
        // Zero jointly-covered seconds means the census was never taken or never measurable, so
        // the count is UNKNOWN — the same contract the census beside it keeps, and the reason a
        // bare `0 holds` is forbidden here too. The boundary is withheld in this branch because
        // the carried lines are `0.0` when the census was not taken: printing `≥0%` would state
        // a line no reading was ever measured against.
        let unmeasurable = capacity_line(0, 0, 0, 0, 0);
        assert!(
            unmeasurable.ends_with("capacity holds: —\n"),
            "an unmeasurable capacity census renders `—`: {unmeasurable}"
        );
        // Scoped to the capacity cell's own label: the census cell to its left legitimately
        // states ITS water, so a bare `≥0%` search over the whole line would match that instead
        // — which is precisely what this assertion caught on its first run.
        assert!(
            !unmeasurable.contains("0 holds") && !unmeasurable.contains("capacity holds ("),
            "never a fabricated calm, and never a boundary it did not measure against: \
             {unmeasurable}"
        );
    }

    #[test]
    fn the_two_roster_cells_answer_their_two_questions_independently() {
        // The motivating contrast, pinned: the utilisation census can read UNKNOWN while the
        // capacity fact beside it reads a measured hold. They are separate facts over separate
        // denominators (issue #803 vs #804), and merging or substituting one for the other is
        // exactly the defect that left a 95-hold week reading as calm.
        let line = roster_line(
            &UsageReport {
                period: Period::new(0, CENSUS_WINDOW),
                per_account: BTreeMap::new(),
                roster: RosterStats {
                    all_high_covered_secs: 0, // census unmeasurable
                    high_threshold: 0.95,
                    capacity_holds: 5,
                    capacity_holds_session: 4,
                    capacity_holds_weekly: 1,
                    capacity_hold_secs_lower_bound: 99_435,
                    capacity_hold_covered_secs: CENSUS_WINDOW, // capacity measured
                    capacity_session_line: 0.80,
                    capacity_weekly_line: 0.97,
                    ..Default::default()
                },
            },
            true,
        );
        assert!(
            line.contains("all-accounts-high (≥95%): — ·"),
            "census UNKNOWN: {line}"
        );
        assert!(
            line.contains("capacity holds (session ≥80%, weekly ≥97%): 5 "),
            "capacity measured: {line}"
        );
    }

    #[test]
    fn an_unmeasurable_census_renders_the_gap_sentinel_never_a_bare_zero() {
        // THE reported defect: `stats` printed `all-accounts-high: 0 episodes (0s)` for a week
        // in which the metric could see nothing — indistinguishable from a genuinely quiet one.
        // With no jointly-covered second, the count is UNKNOWN and must say so.
        let unmeasurable = census_line(0, 0, 0, 0.95);
        assert!(
            unmeasurable.contains("all-accounts-high (≥95%): — ·"),
            "unmeasurable census renders `—`: {unmeasurable}"
        );
        assert!(
            !unmeasurable.contains("0 episodes"),
            "a bare `0` is forbidden — it reads as a calm week: {unmeasurable}"
        );

        // And the other branch is NOT swallowed: a measured zero still reports itself as one,
        // so the sentinel marks unmeasurability rather than merely "nothing happened".
        assert!(
            census_line(0, 0, CENSUS_WINDOW, 0.95)
                .contains("all-accounts-high (≥95%): 0 episodes (0s) ·"),
            "a MEASURED zero still renders as zero"
        );
    }

    #[test]
    fn a_barely_covered_census_annotates_its_share_instead_of_reading_as_calm() {
        // The conservative "no covered second at all" bar is necessary but NOT sufficient: the
        // reported defect's own field shape — an hourly-polled peer against a 300 s staleness
        // horizon — leaves a sliver of joint coverage, not none, so `covered > 0` alone would
        // still print a confident calm for a week the metric barely saw. A partly-covered
        // period says so (REQ-STA-B-008's annotation clause).
        let sliver = census_line(0, 0, 7_200, 0.95); // 2 h of a day
        assert!(
            sliver.contains(
                "all-accounts-high (≥95%): 0 episodes (0s, all in view 8% of the window) ·"
            ),
            "a partly-covered census annotates its measured share: {sliver}"
        );
        // And it does so in the READER's words, never the field's. `covered` names
        // `all_high_covered_secs` and leaves *covered by what?* unanswered, which is the second
        // half of issue #1029; the annotation must say what the share measures instead.
        assert!(
            !sliver.contains("% covered"),
            "the annotation must not leak the field name `covered`: {sliver}"
        );

        // Rounding must not manufacture a whole the share is NOT, at EITHER end: both of
        // these windows are strictly partly covered, and a `0%` or a `100%` would deny it.
        let trace = census_line(0, 0, 60, 0.95); // 1 min of a day ≈ 0.07%
        assert!(
            trace.contains(
                "all-accounts-high (≥95%): 0 episodes (0s, all in view <1% of the window) ·"
            ),
            "a trace of coverage renders `<1%`, never a false `0%`: {trace}"
        );
        // A day covered but for its last 5 min rounds to 100 — reachable whenever one account
        // joins the roster (or its daemon restarts) a sliver into the window.
        let nearly = census_line(0, 0, CENSUS_WINDOW - 300, 0.95); // ≈ 99.65%
        assert!(
            nearly.contains(
                "all-accounts-high (≥95%): 0 episodes (0s, all in view >99% of the window) ·"
            ),
            "near-total coverage renders `>99%`, never a false `100%`: {nearly}"
        );

        // A WHOLLY covered period carries no annotation — the common case stays terse, and
        // the annotation's presence is itself the low-coverage signal. That is the ONLY way
        // to read `100%`, so the `>99%` above can never be mistaken for it.
        assert!(
            !census_line(2, 600, CENSUS_WINDOW, 0.95).contains("in view"),
            "a fully-covered census is not annotated"
        );

        // Episodes and a partial window COMPOSE — the everyday shape, and the one that says
        // both what was seen and how much of the window it was seen over.
        let partial = census_line(2, 600, CENSUS_WINDOW / 2, 0.95);
        assert!(
            partial.contains(
                "all-accounts-high (≥95%): 2 episodes (10m, all in view 50% of the window) ·"
            ),
            "a measured episode over half a window reports both: {partial}"
        );

        // A degenerate (zero-length) window: nothing can be jointly covered inside it, so it
        // takes the UNKNOWN branch and the share is never computed — which is why the division
        // above needs no zero guard, not because one is applied.
        let degenerate = census_line_over(0, 0, 0, 0.95, 0);
        assert!(degenerate.contains("all-accounts-high (≥95%): — ·"));
    }

    #[test]
    fn the_roster_line_states_the_water_it_used_rather_than_a_literal() {
        // Three surfaces once quoted three different thresholds because none stated its own.
        // The rendered percent must track the census's carried water, not a constant.
        for (water, shown) in [(0.95, "≥95%"), (0.80, "≥80%"), (0.90, "≥90%")] {
            let line = census_line(3, 6_000, 86_400, water);
            assert!(
                line.contains(&format!("all-accounts-high ({shown}): 3 episodes (1h40m)")),
                "water {water} renders as {shown}: {line}"
            );
        }
    }

    #[test]
    fn build_report_takes_the_census_over_the_configured_roster() {
        // The plumbing half, end to end through `build_report`: `beta` is configured but was
        // never sampled in the window, so the census cannot be taken and the line renders `—`
        // — where the sampled-set form would have degenerated to `alpha` alone and reported a
        // confident episode.
        let now = 1_000_000;
        let samples = vec![
            sample(now - 900, "alpha", 0.90, 0.1),
            sample(now - 600, "alpha", 0.95, 0.1),
            sample(now - 300, "alpha", 0.97, 0.1),
        ];
        let store = data(samples, "");
        let window = plan_window(None, None, now, &store).unwrap();
        let configured: BTreeSet<String> =
            ["alpha", "beta"].iter().map(|h| (*h).to_owned()).collect();

        let over_roster = build_report(
            &store,
            window.clone(),
            vec![],
            Some(&configured),
            &params(),
            0,
        );
        assert_eq!(over_roster.summary.roster.all_high_covered_secs, 0);
        assert!(
            render_text(&over_roster, None).contains("all-accounts-high (≥80%): —"),
            "an unobserved rostered account leaves the census UNKNOWN"
        );
        // The SERIES buckets take the roster too, not just the summary. Asserted on
        // `all_high_covered_secs`, which is roster-derived — a threshold assertion here would
        // pass either way (it comes from `params`) and so would gate nothing.
        assert!(
            over_roster
                .series
                .iter()
                .all(|b| b.roster.all_high_covered_secs == 0),
            "every bucket's census is taken over the configured roster as well"
        );

        let no_roster = build_report(&store, window, vec![], None, &params(), 0);
        assert!(
            no_roster.summary.roster.all_high_episodes > 0,
            "without a configured roster the census degrades to the sampled set — the \
             pre-#804 reading, kept as the honest fallback when config is unreadable"
        );
        assert!(
            no_roster
                .series
                .iter()
                .any(|b| b.roster.all_high_covered_secs > 0),
            "the fallback reaches the buckets too — so the assertion above is a real gate on \
             the pass-through, not a summary-only one that a `None` bucket would still pass"
        );
    }

    // --- issue #836: the human render says WHICH regime produced the census ------------

    /// A census that FIRED — an episode over a fully-covered day — rendered under the given
    /// regime. The fired branch is the one whose number can be misread, so it is the branch
    /// every regime assertion below is measured on.
    fn fired_census_line(census_over_roster: bool) -> String {
        roster_line(
            &UsageReport {
                period: Period::new(0, CENSUS_WINDOW),
                per_account: BTreeMap::new(),
                roster: RosterStats {
                    all_high_episodes: 3,
                    all_high_secs: 6_000,
                    all_high_covered_secs: CENSUS_WINDOW,
                    high_threshold: 0.95,
                    ..Default::default()
                },
            },
            census_over_roster,
        )
    }

    #[test]
    fn a_fired_census_names_its_set_only_when_the_roster_was_unknown() {
        // THE reported defect: issue #804 gave the census two regimes that print the same
        // bytes. The degraded one drops an unsampled account from the intersection, so it fires
        // on strictly less evidence — the direction REQ-STA-B-005's amendment forbids — and a
        // reader holding the number could not tell which one produced it.
        let configured = fired_census_line(true);
        assert!(
            configured.contains("all-accounts-high (≥95%): 3 episodes (1h40m)"),
            "the configured regime is the norm and stays unqualified: {configured}"
        );
        assert!(
            !configured.contains("sampled accounts"),
            "no qualifier when the census DID intersect the configured roster: {configured}"
        );

        // Asserted as ONE contiguous string, which pins PLACEMENT as well as presence:
        // placement is load-bearing, not cosmetic. The roster line is what a reader greps out
        // (`stats | grep all-accounts-high`), so a qualifier rendered anywhere else — its own
        // line, a footer, the neighbouring `·` cell — is separable from the number it qualifies
        // by exactly the command operators actually run, and would fail here.
        let degraded = fired_census_line(false);
        assert!(
            degraded.contains("all-accounts-high (≥95%, sampled accounts): 3 episodes (1h40m)"),
            "the degraded regime names its set beside its water: {degraded}"
        );
        // The whole degraded line is neutral vocabulary (issue #160 / SUR-001). The qualifier
        // is authored English on the human surface, so it answers to the same framing guard
        // every other rendered string here does.
        assert_eq!(
            scan_banned(&degraded),
            None,
            "the set qualifier must not editorialise: {degraded}"
        );
    }

    #[test]
    fn an_untaken_census_states_no_set_under_either_regime() {
        // `—` is the UNKNOWN sentinel: it carries no confident number to misread and reads the
        // same under both regimes ("I could not see"), so naming the set there would describe a
        // measurement that never happened — the same reason `capacity_holds_cell` withholds its
        // boundary from this branch. It would also put a permanent qualifier on every
        // pre-`capture` render, which has no roster and nothing to report.
        for regime in [true, false] {
            let line = roster_line(
                &UsageReport {
                    period: Period::new(0, CENSUS_WINDOW),
                    per_account: BTreeMap::new(),
                    roster: RosterStats {
                        all_high_covered_secs: 0,
                        high_threshold: 0.95,
                        ..Default::default()
                    },
                },
                regime,
            );
            assert!(
                line.contains("all-accounts-high (≥95%): — ·"),
                "an untaken census renders `—` with no set, regime={regime}: {line}"
            );
        }
    }

    #[test]
    fn the_config_caveat_carries_the_wire_reason_verbatim_and_only_when_there_is_one() {
        // One fact, one string: the human surface and `--json` must not describe the same
        // config failure two different ways, so the caveat prints exactly what
        // `wire_config_reason` puts on the wire for issue #642 — which is also what makes it
        // safe to print here, since a `&'static str` cannot carry a byte of the operator's
        // `config.toml`.
        let reason = wire_config_reason(&Error::ConfigParse("x".into()));
        let line = config_regime_line(Some(reason)).expect("a malformed config yields a caveat");
        assert!(line.contains(reason), "the wire reason, verbatim: {line}");
        assert!(
            line.starts_with("all-accounts-high "),
            "leads with the metric it qualifies, so one grep catches the caveat AND the \
             number: {line}"
        );
        assert!(
            line.contains("fires more readily"),
            "states the DIRECTION of the bias, which is the operator-actionable half: {line}"
        );
        assert!(
            line.ends_with('\n'),
            "a whole line, so the caller never has to append one: {line}"
        );
        // EVERY reason arm, against the #160 / SUR-001 framing guard. Until this change those
        // strings were wire-and-stderr only, and the `--json` scan covers KEYS, not values — so
        // this is the first gate any of them has ever answered to as authored ENGLISH on a human
        // surface, alongside the caveat sentence they are composed into.
        for err in [
            Error::ConfigParse("x".into()),
            Error::ConfigInvalid("x".into()),
            Error::Io(std::io::Error::other("x")),
        ] {
            let arm = config_regime_line(Some(wire_config_reason(&err))).expect("a caveat");
            assert_eq!(
                scan_banned(&arm),
                None,
                "the caveat must not editorialise, whichever reason it carries: {arm}"
            );
        }
        // A readable — or ABSENT — config leaves no trace. The absent case is the normal
        // pre-`capture` state issue #627 deliberately keeps silent; its regime is already
        // stated by the roster line's own qualifier, which needs no config to be true.
        assert!(config_regime_line(None).is_none());
    }

    /// A report whose census actually FIRED, built through [`build_report`] under the given
    /// regime over the SAME samples — so the only difference between the two renders is the
    /// annotation itself, not a number that moved underneath it. A single rostered account
    /// makes the two censuses numerically identical by construction (the sampled set IS the
    /// roster), which is what isolates the annotation as the sole variable.
    fn fired_report(census_over_roster: bool) -> Report {
        let now = epoch("2026-07-01T12:00:00Z");
        let store = data(
            vec![
                sample(now - 600, "alpha", 0.97, 0.4),
                sample(now - 400, "alpha", 0.98, 0.45),
            ],
            "",
        );
        let window = plan_window(Some("day"), None, now, &store).unwrap();
        let roster: BTreeSet<String> = ["alpha"].iter().map(|h| (*h).to_owned()).collect();
        build_report(
            &store,
            window,
            vec![],
            census_over_roster.then_some(&roster),
            &params(),
            0,
        )
    }

    #[test]
    fn both_human_surfaces_state_the_regime_not_just_the_piped_one() {
        // `render_charts` had exactly the same blind spot as `render_text`, and the issue's
        // example named only the latter. A fix that covered one surface would leave the metric
        // unannotated on the interactive TTY — the surface an operator actually watches.
        let degraded = fired_report(false);
        assert!(
            degraded.summary.roster.all_high_covered_secs > 0,
            "the fixture's census really fired — else every assertion below rides the `—` \
             branch and gates nothing"
        );
        let reason = wire_config_reason(&Error::ConfigInvalid("x".into()));
        let piped = render_text(&degraded, Some(reason));
        let charts = render_charts(&degraded, 100, false, false, Some(reason));
        for (surface, out) in [("piped", &piped), ("charts", &charts)] {
            assert!(
                out.contains("sampled accounts"),
                "{surface} names the census's set: {out}"
            );
            assert!(out.contains(reason), "{surface} carries the reason: {out}");
            // Directly ABOVE the line it qualifies, so it is read BEFORE the number rather
            // than as a footnote correcting a reading already taken.
            let caveat = out
                .find("all-accounts-high fires more readily")
                .expect("the caveat is present");
            let roster = out.find("\nroster: ").expect("the roster line is present");
            assert!(
                caveat < roster,
                "{surface} places the caveat above the roster line: {out}"
            );
            assert!(
                !out[caveat..roster].contains('\n'),
                "{surface} keeps the bottom block contiguous — nothing, not even a blank \
                 line, between the caveat and what it qualifies: {out}"
            );
        }
        // And the CONFIGURED regime with a readable config renders exactly as it always did:
        // the annotation is purely additive, which is why no committed golden moved.
        let normal = fired_report(true);
        for out in [
            render_text(&normal, None),
            render_charts(&normal, 100, false, false, None),
        ] {
            assert!(!out.contains("sampled accounts"), "unqualified: {out}");
            assert!(!out.contains("fires more readily"), "no caveat: {out}");
        }
    }

    #[test]
    fn the_accountless_charts_branch_is_annotated_too_not_just_the_populated_one() {
        // `render_charts` returns EARLY when no account survives the filter, and that branch
        // renders its own footer — so the annotation added to the main path does not reach it.
        // Severing either the caveat or the qualifier there left the whole suite green, which is
        // exactly the hole `run_output`'s injected-config seam exists to refuse elsewhere.
        //
        // Production-reachable, and it is the case an operator most needs: `--account` matching
        // nothing empties `per_account` while `summary.roster` — computed by
        // `aggregate_with_roster` BEFORE `apply_filter` — keeps its measured census. A fresh
        // install with a malformed `config.toml` lands here too, where this caveat is the only
        // thing on stdout saying the config is broken.
        let measured = UsageReport {
            period: Period::new(0, CENSUS_WINDOW),
            per_account: BTreeMap::new(), // filtered away — the early-return branch
            roster: RosterStats {
                all_high_episodes: 2,
                all_high_secs: 900,
                all_high_covered_secs: CENSUS_WINDOW, // …but the census WAS taken
                high_threshold: 0.95,
                ..Default::default()
            },
        };
        let report = Report {
            summary: measured.clone(),
            series: vec![measured],
            census_over_roster: false,
            ..fired_report(false)
        };
        let reason = wire_config_reason(&Error::Io(std::io::Error::other("x")));
        let out = render_charts(&report, 100, false, false, Some(reason));
        assert!(
            out.contains("no per-account usage in this window"),
            "the fixture really takes the early-return branch — else this gates the main path \
             a second time and the hole stays open: {out}"
        );
        assert!(
            out.contains("all-accounts-high (≥95%, sampled accounts): 2 episodes"),
            "the accountless branch names the census's set: {out}"
        );
        assert!(out.contains(reason), "…and carries the reason: {out}");
    }

    #[test]
    fn build_report_records_the_set_the_census_actually_intersected() {
        // The carrier is read off the very `roster` argument `aggregate_with_roster` consumed,
        // not re-derived from the caller's `Config` one hop earlier — so a render that states
        // the regime cannot drift from the census that produced the number.
        let now = 1_000_000;
        let store = data(vec![sample(now - 600, "alpha", 0.96, 0.1)], "");
        let window = plan_window(None, None, now, &store).unwrap();
        let configured: BTreeSet<String> = ["alpha"].iter().map(|h| (*h).to_owned()).collect();

        assert!(
            build_report(
                &store,
                window.clone(),
                vec![],
                Some(&configured),
                &params(),
                0
            )
            .census_over_roster,
            "a known roster IS the configured regime"
        );
        assert!(
            !build_report(&store, window, vec![], None, &params(), 0).census_over_roster,
            "no roster IS the degraded regime — the case issue #804 documented as the fallback"
        );
    }

    #[test]
    fn the_cli_run_path_wires_the_regime_all_the_way_to_the_human_render() {
        // The CLI's OWN call site, not just the seam it calls — the human-render sibling of
        // `the_cli_run_path_wires_the_signal_all_the_way_to_stdout`, and for the same reason:
        // `run` reads the AMBIENT config, so without driving `run_output` the whole annotation
        // could be severed at the `render_human` call site with every test above still green.
        // Both halves ride this one path: a malformed config yields no roster (hence the set
        // qualifier) AND a reason (hence the caveat).
        let dir = tempfile::tempdir().unwrap();
        let bad = malformed_config(
            dir.path(),
            "cli-human.toml",
            "[tunables]\nsession_trigger = 50\n",
        );
        let now = epoch("2026-07-08T00:00:00Z");
        // Two readings above the default water, close enough to be jointly covered, so the
        // census actually FIRES — the branch that carries the qualifier. A window with no
        // episode would render `—` and pass this test while gating nothing.
        let store = data(
            vec![
                sample(now - 600, "work", 0.97, 0.3),
                sample(now - 300, "work", 0.98, 0.3),
            ],
            "",
        );
        let human_args = || StatsArgs {
            accounts: Vec::new(),
            period: Some("week".to_owned()),
            since: None,
            json: false,
            no_color: true,
            ascii: true,
        };

        let out = run_output(human_args(), &store, now, 0, || Config::load_path(&bad)).unwrap();
        assert!(
            out.contains("sampled accounts"),
            "the human render names the degraded census's set: {out}"
        );
        assert!(
            out.contains(wire_config_reason(&Config::load_path(&bad).unwrap_err())),
            "…and the reason, which until now reached only `--json` and stderr: {out}"
        );

        // The healthy counterpart over the SAME path leaves no trace of either annotation — a
        // roster the census could intersect, and nothing to explain.
        let good = dir.path().join("good.toml");
        std::fs::write(
            &good,
            "[[account]]\naccount_uuid = \"u-1\"\nlabel = \"work\"\n",
        )
        .unwrap();
        let clean = run_output(human_args(), &store, now, 0, || Config::load_path(&good)).unwrap();
        assert!(
            !clean.contains("sampled accounts") && !clean.contains("fires more readily"),
            "a readable config renders the configured regime, unannotated: {clean}"
        );
        assert!(
            clean.contains("all-accounts-high (≥"),
            "…and still renders the census itself, so the assertion above is not passing on \
             an empty render: {clean}"
        );
    }

    #[test]
    fn the_wire_carries_the_census_water_and_its_coverage_denominator() {
        // #805 (the menubar's hardcoded `≥90%` label) and any other surface read these two
        // keys rather than re-deriving or hardcoding. Both are ALWAYS present: a surface that
        // found the water absent would fall back to a literal, which is the defect.
        let report = wire_golden_report();
        let v: serde_json::Value = serde_json::from_str(&render_json(&report, None).unwrap())
            .expect("the wire is valid JSON");
        let roster = &v["summary"]["roster"];
        assert_eq!(roster["all_high_threshold"], 0.95);
        assert_eq!(roster["all_high_covered_secs"], 21_600);
        assert_eq!(
            v["schema"], 1,
            "both keys are ADDITIVE — no existing field changed, so `schema` does not move"
        );
        // The series buckets carry them too, so a per-bucket reader is not left guessing.
        assert_eq!(v["series"][0]["roster"]["all_high_threshold"], 0.95);
    }

    /// The census's SET rides the wire beside its water (issue #866), so the panel can make the
    /// same distinction the human render makes without re-deriving it from a second source.
    ///
    /// Asserts BOTH values. The committed golden pins only `true`, and a field that is hardwired
    /// to the constant the golden happens to hold passes every single-value assertion — this is
    /// the assertion that actually reaches the wiring, in the same shape
    /// `roster_line`'s own regime pair does.
    #[test]
    fn the_wire_carries_the_census_set_under_both_regimes() {
        fn roster_of(census_over_roster: bool) -> serde_json::Value {
            let report = Report {
                census_over_roster,
                ..wire_golden_report()
            };
            serde_json::from_str::<serde_json::Value>(&render_json(&report, None).unwrap())
                .expect("the wire is valid JSON")
        }

        let configured = roster_of(true);
        assert_eq!(configured["summary"]["roster"]["census_over_roster"], true);

        // The `false` doubles as the ALWAYS-present gate, which is why no separate presence
        // assertion follows: an elided key reads back as `Null`, and `Null != false`. `false` is
        // the value a consumer acts on, so eliding it would make the degraded regime
        // indistinguishable from a pre-#866 daemon, and those two demand OPPOSITE renders (name
        // the set / drop the qualifier).
        let degraded = roster_of(false);
        assert_eq!(
            degraded["summary"]["roster"]["census_over_roster"], false,
            "the degraded regime must be SENT, not elided: {}",
            degraded["summary"]["roster"]
        );

        // Additive: `schema` does not move, and a per-bucket reader gets the same regime as the
        // summary — one report, one census, one set.
        assert_eq!(degraded["schema"], 1);
        assert_eq!(degraded["series"][0]["roster"]["census_over_roster"], false);
        assert_eq!(
            configured["series"][0]["roster"]["census_over_roster"],
            true
        );
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
        let out = render_text(&report, None);
        assert!(out.contains("no per-account usage in this window"));
        assert!(out.contains("0 swaps"));
        // JSON of an empty window is still a valid schema:1 document.
        let v: serde_json::Value =
            serde_json::from_str(&render_json(&report, None).unwrap()).unwrap();
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
            // The whole 6 h bucket was jointly covered, so `0 episodes` here is a MEASURED
            // reading — the wire's `all_high_covered_secs` is what says so (issue #804).
            all_high_covered_secs: 21_600,
            high_threshold: 0.95,
            // The capacity-holds census is left at its unmeasurable default (issue #803), so
            // these buckets pin the census cell's UNKNOWN branch. That is the honest reading for
            // a hand-authored roster block: nothing here was measured against a viability
            // boundary, so a hand-set hold count would assert a fact no input supports.
            ..RosterStats::default()
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
            expiry: BTreeMap::new(),
            census_over_roster: true,
        }
    }

    // --- the framing guard: a CENTRAL banned vocabulary + its scanner ----------------

    // The vocabulary and the scanner now live in `crate::framing_vocabulary`, hoisted there by
    // issue #918 so `src/cli.rs` can guard `--help` against the same list instead of the
    // coverage that issue #885's AC4 assumed and nobody had written. Nothing was subtracted on
    // the way out: `scan_banned` below is the identical scan over the identical `BANNED_TOKENS`
    // and `BANNED_PHRASES`, and `every_central_token_and_phrase_bites` asserts token by token
    // that this side still sees all of it. What `--help` gets is a DERIVED subset, never a
    // second copy of the list.
    use crate::framing_vocabulary::scan_banned;

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
    fn summary_band_is_neutral_and_deterministic() {
        // The bottom roster block, frozen: AGGREGATE-ONLY (§D-STA-5). Per-account signal /
        // velocity / runway are TABLE COLUMNS now, never band lists — what remains is the
        // lowest-utilisation callout, a MAGNITUDE and neutral descriptor (no imperative, no
        // forecast, no verdict). `three_band_report` carries no velocity overlay and no weekly
        // head-room, so there is no fleet line; the block is the single lowest-util line, 2-space
        // indented and WITHOUT a trailing newline (the caller foots it with the roster line).
        assert_eq!(
            render_summary(&three_band_report()),
            "  lowest utilisation: aa (session mean 10%)",
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

        // And in the rendered SIGNAL column: both deviation words are wrapped in the identical
        // SGR, the balanced middle is plain — proof the colour half is symmetric too. The signal
        // is a TABLE COLUMN now (§D-STA-5), so the emphasis is asserted on the chart-table cell,
        // not on the (now aggregate-only) band.
        let three = three_band_report();
        let table = render_chart_table(&three, &keys(&three), 80, true, false);
        assert!(table.contains("\x1b[33munderused\x1b[0m"));
        assert!(table.contains("\x1b[33msaturated\x1b[0m"));
        assert!(
            table.contains("balanced") && !table.contains("\x1b[33mbalanced"),
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
        // Human surfaces (numeric text + charts) both foot with the aggregate roster block.
        let text = render_text(&report_fixture(), None);
        assert!(
            text.contains("lowest utilisation:"),
            "the numeric text carries the roster block"
        );
        let charts = render_charts(&two_account_charts(), 60, false, false, None);
        assert!(
            charts.contains("lowest utilisation:"),
            "the charts view carries the roster block"
        );

        // The band is HUMAN-only — none of its vocabulary reaches the schema:1 wire (which
        // keeps the finer per-account `band`/`coverage_class` enums, byte-stable vs #159).
        let json = render_json(&report_fixture(), None).unwrap();
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
                render_summary(report),
                render_text(report, None),
                render_charts(report, 80, true, false, None),
            ] {
                assert_eq!(
                    scan_banned(&surface),
                    None,
                    "a real render must contain no banned token: {surface:?}"
                );
            }
        }

        // The `--json` KEYS are neutral too (the wire carries descriptor enums, no verb).
        let json = render_json(&report_fixture(), None).unwrap();
        let mut keys = Vec::new();
        json_keys(&serde_json::from_str(&json).unwrap(), &mut keys);
        assert_eq!(scan_banned(&keys.join(" ")), None, "json keys are neutral");

        // The guard BITES: inject a banned word into a real render and it is caught — proof the
        // test would FAIL if editorialising copy ever slipped in. Injected into the numeric text
        // (which carries the `signal` column word `balanced`), the aggregate band having no
        // per-account signal list to poison.
        let poisoned = render_text(&three, None).replace("balanced", "upgrade");
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

        // Human: velocity + runway are TABLE COLUMNS now (§D-STA-5) — the header carries both and
        // the `work` row its neutral %/min rate + approximate SESSION head-room (`~2h`), facts not
        // advice. (The weekly head-room feeds the aggregate fleet line, not the per-row cell.)
        let text = render_text(&report, None);
        assert!(text.contains("velocity"), "velocity column header: {text}");
        assert!(text.contains("runway"), "runway column header: {text}");
        assert!(text.contains("0.2%/min"), "the neutral rate cell: {text}");
        assert!(
            text.contains("~2h"),
            "the approximate session head-room cell: {text}"
        );
        assert_eq!(scan_banned(&text), None, "the real render is neutral");

        // Wire: the velocity object carries %/min + whole-second runway; the flat weekly is a
        // KNOWN 0.0 rate with an EXPLICIT null runway (honest degradation, never a sentinel).
        let v: serde_json::Value =
            serde_json::from_str(&render_json(&report, None).unwrap()).unwrap();
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
        let text = render_text(&report, None);
        // The per-row runway cell is the SESSION head-room (`~2h`); the WEEKLY head-room feeds the
        // aggregate fleet line (§D-STA-5 — weekly per-account is not a per-row cell), on day scale.
        assert!(text.contains("~2h"), "session head-room cell: {text}");
        assert!(
            text.contains("accounts last: ~2 days at the current combined rate"),
            "weekly head-room on the fleet line: {text}"
        );
        assert_eq!(scan_banned(&text), None, "the days render is neutral");

        let v: serde_json::Value =
            serde_json::from_str(&render_json(&report, None).unwrap()).unwrap();
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
        let text = render_text(&report, None);
        // The velocity cell reads the KNOWN zero; the runway column — uniformly `—` across the
        // fleet (no finite head-room) — elides entirely (§D-STA-5 empty-column elision).
        assert!(text.contains("0.0%/min"), "known zero rate cell: {text}");
        assert!(
            !text.contains("runway"),
            "the all-unknown runway column elides: {text}"
        );

        let v: serde_json::Value =
            serde_json::from_str(&render_json(&report, None).unwrap()).unwrap();
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
        let text = render_text(&report, None);
        assert!(
            text.contains("work"),
            "the account still appears in the table"
        );
        assert!(
            !text.contains("velocity"),
            "no velocity column without a rate (elided): {text}"
        );

        let v: serde_json::Value =
            serde_json::from_str(&render_json(&report, None).unwrap()).unwrap();
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
        let text = render_text(&report, None);
        assert!(
            !text.contains("velocity"),
            "a stale reading shows no velocity column (elided): {text}"
        );

        let v: serde_json::Value =
            serde_json::from_str(&render_json(&report, None).unwrap()).unwrap();
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
        // The readout lives in the TABLE now (§D-STA-5). The piped numeric view always carries the
        // velocity column (it is not width-degraded); every surface — the aggregate band included —
        // stays neutral.
        assert!(
            render_text(&report, None).contains("velocity"),
            "the piped table carries the velocity column"
        );
        for surface in [
            render_summary(&report),
            render_text(&report, None),
            render_charts(&report, 80, true, false, None),
        ] {
            assert_eq!(
                scan_banned(&surface),
                None,
                "the velocity + runway readout must contain no banned token: {surface:?}"
            );
        }

        // The `--json` keys the readout adds are neutral (the wire carries figures, no verb).
        let json = render_json(&report, None).unwrap();
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
    }

    // --- AC (issue #1075): the PER-ACCOUNT runway refuses an implausible figure ---------

    #[test]
    fn runway_secs_refuses_the_quotient_of_a_vanishing_rate() {
        // DEFECT 1 of the two. `1e-11` is finite and strictly positive — a perfectly well-formed
        // rate — so no guard on the RATE can refuse it, and the old `rate <= 0.0` duly waved it
        // through. Re-derived against the pre-fix expression, the issue's own session reproduction:
        //   (0.80 − 0.0) / 1e-11 = 80_000_000_000 s = 925,925 days.
        // What refuses it is the RESULT bound, which is the point — the bound is expressed in the
        // unit an operator can check, not in the EMA's native fraction-per-second (the input-side
        // epsilon the issue explicitly rejected).
        assert_eq!(
            runway_secs(Some(1e-11), 0.0, 0.80, SESSION_RUNWAY_PLAUSIBLE_MAX_SECS),
            None,
            "925,925 days is not a session runway"
        );
        // NOT a clamp: refusal, never a large plausible-looking stand-in at the boundary — that
        // would convert an obviously-wrong number into a credible one.
        assert_ne!(
            runway_secs(Some(1e-11), 0.0, 0.80, SESSION_RUNWAY_PLAUSIBLE_MAX_SECS),
            Some(SESSION_RUNWAY_PLAUSIBLE_MAX_SECS),
            "refused, never clamped back to the bound"
        );
        // The weekly arm carries the same fault and the same fix, at its own bound.
        assert_eq!(
            runway_secs(Some(1e-11), 0.0, 0.95, WEEKLY_RUNWAY_PLAUSIBLE_MAX_SECS),
            None
        );

        // The gate does NOT over-refuse: an ordinary drain still states its figure. 0.20 head-room
        // at 1e-4 frac/s is 2,000 s (~33 m), well inside one session window.
        assert_eq!(
            runway_secs(Some(1e-4), 0.60, 0.80, SESSION_RUNWAY_PLAUSIBLE_MAX_SECS),
            Some(2_000)
        );
    }

    #[test]
    fn runway_secs_refuses_a_figure_a_saturating_cast_would_state() {
        // DEFECT 2 of the two, and the arm that survives when only the bound is removed: a float→int
        // `as` cast SATURATES rather than trapping (Rust 1.45+). Re-derived against the pre-fix
        // expression, the issue's own weekly reproduction:
        //   (0.95 − 0.0) / 1e-300 = 9.5e299 → `as i64` → 9223372036854775807, EXACTLY `i64::MAX`
        //   (106,751,991,167,300 days).
        assert_eq!(
            runway_secs(Some(1e-300), 0.0, 0.95, WEEKLY_RUNWAY_PLAUSIBLE_MAX_SECS),
            None
        );
        // Naming the saturated sentinel explicitly is what makes this test the one that detects a
        // restored bare cast — an `is_none` assertion alone cannot tell the two defects apart.
        assert_ne!(
            runway_secs(Some(1e-300), 0.0, 0.95, WEEKLY_RUNWAY_PLAUSIBLE_MAX_SECS),
            Some(i64::MAX),
            "a saturating conversion must be unreachable"
        );
        for rate in [1e-300, f64::MIN_POSITIVE, 5e-324] {
            for (trigger, bound) in [
                (0.80, SESSION_RUNWAY_PLAUSIBLE_MAX_SECS),
                (0.95, WEEKLY_RUNWAY_PLAUSIBLE_MAX_SECS),
            ] {
                assert_eq!(
                    runway_secs(Some(rate), 0.0, trigger, bound),
                    None,
                    "rate {rate:e} against trigger {trigger} states no figure"
                );
            }
        }
    }

    #[test]
    fn runway_secs_refuses_a_non_finite_rate_rather_than_stating_exhausted_now() {
        // A THIRD arm, found while re-deriving the two the issue reports and fixed by the same
        // gate. Every `NaN` comparison is false, so `NaN <= 0.0` did not refuse it; `NaN.round() as
        // i64` is `0`, not a saturated sentinel — so a `NaN` rate surfaced as `~0s`, "exhausted
        // right now". That is the one failure mode of the three producing a small, entirely
        // CREDIBLE figure, and on the session dimension it reads as an account about to be swapped
        // off. An infinite rate lands on the same `Some(0)` by a different route.
        for rate in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let got = runway_secs(Some(rate), 0.0, 0.80, SESSION_RUNWAY_PLAUSIBLE_MAX_SECS);
            assert_eq!(got, None, "rate {rate} is not a measurement");
            assert_ne!(got, Some(0), "and must never read as exhausted-now");
        }
        // The same for a non-finite READING or trigger, which reach the quotient the same way.
        assert_eq!(
            runway_secs(
                Some(1e-4),
                f64::NAN,
                0.80,
                SESSION_RUNWAY_PLAUSIBLE_MAX_SECS
            ),
            None
        );
        assert_eq!(
            runway_secs(
                Some(1e-4),
                0.60,
                f64::NAN,
                SESSION_RUNWAY_PLAUSIBLE_MAX_SECS
            ),
            None
        );
    }

    #[test]
    fn the_runway_bound_is_each_dimension_s_own_reset_cadence_not_one_shared_number() {
        // The design claim the issue makes load-bearing: the bound is DERIVED per dimension, not
        // copied. Both constants are one of their own dimension's reset windows.
        assert_eq!(
            SESSION_RUNWAY_PLAUSIBLE_MAX_SECS, 18_000,
            "one rolling 5-hour session window in seconds"
        );
        assert_eq!(
            WEEKLY_RUNWAY_PLAUSIBLE_MAX_SECS, 604_800,
            "one weekly window in seconds"
        );

        // THE test that goes red if the two are ever collapsed into one number. A three-day
        // quotient is an ordinary weekly runway and an absurd session one — it asserts the account
        // drains across ~14 intervening session resets. One shared bound cannot hold both readings.
        let headroom = 0.5;
        let three_days = 3 * DAY_SECS;
        let rate = headroom / three_days as f64;
        assert_eq!(
            runway_secs(Some(rate), 0.0, headroom, WEEKLY_RUNWAY_PLAUSIBLE_MAX_SECS),
            Some(three_days),
            "three days is inside one weekly window — a legitimate weekly runway"
        );
        assert_eq!(
            runway_secs(Some(rate), 0.0, headroom, SESSION_RUNWAY_PLAUSIBLE_MAX_SECS),
            None,
            "the SAME quotient is refused on the session dimension, whose window is 5 h"
        );

        // Each bound is inclusive at its own edge and refuses — never clamps — one second past it.
        for bound in [
            SESSION_RUNWAY_PLAUSIBLE_MAX_SECS,
            WEEKLY_RUNWAY_PLAUSIBLE_MAX_SECS,
        ] {
            assert_eq!(
                runway_secs(Some(headroom / bound as f64), 0.0, headroom, bound),
                Some(bound),
                "exactly one window is still plausible — the bound is inclusive"
            );
            assert_eq!(
                runway_secs(Some(headroom / (bound + 1) as f64), 0.0, headroom, bound),
                None,
                "one second past the window is refused, not clamped back to the bound"
            );
        }
    }

    #[test]
    fn a_refused_runway_is_stated_as_a_gap_beside_a_real_one() {
        // R-3 / R-20 for the NEWLY-refused values (the issue's "also worth deciding"): a refusal
        // must be STATED, never hidden. Two accounts through the real `build_report` →
        // `with_velocity` pipeline — `alpha` drains at an ordinary rate, `beta` at a vanishing
        // 1e-9 frac/s (0.0000003 per 300 s) whose quotient, ~2,314 days, this issue now refuses.
        let now = epoch("2026-07-01T12:00:00Z");
        let report = velocity_report(
            vec![
                sample(now - 900, "alpha", 0.60, 0.30),
                sample(now - 600, "alpha", 0.65, 0.30),
                sample(now - 300, "alpha", 0.70, 0.30),
                sample(now - 900, "beta", 0.6000000, 0.30),
                sample(now - 600, "beta", 0.6000003, 0.30),
                sample(now - 300, "beta", 0.6000006, 0.30),
            ],
            now,
        );

        // `beta`'s runway is refused while its VELOCITY is still known — the refusal is scoped to
        // the implausible quotient and takes no measured fact down with it.
        let beta = report.velocity.get("beta").expect("beta has a readout");
        assert_eq!(beta.session_runway_secs, None, "the quotient is refused");
        assert!(beta.session_rate.is_some(), "the measured rate survives it");

        let text = render_text(&report, None);
        // The column is STATED, not dropped: `alpha` holds the datum, so §D-STA-5's empty-column
        // elision cannot fire, and `beta`'s refusal appears as the gap sentinel on its own row.
        assert!(
            text.contains("runway"),
            "the runway column is stated: {text}"
        );
        assert!(
            text.contains("~10m"),
            "alpha's real runway — 0.10 head-room at 1.667e-4 frac/s is 600 s: {text}"
        );
        let beta_row = text
            .lines()
            .find(|l| l.contains("beta"))
            .expect("a beta row");
        assert!(
            beta_row.contains('—'),
            "beta's refused runway is an explicit gap, never a fabricated figure: {beta_row}"
        );
        assert!(
            !beta_row.contains("day"),
            "and specifically not the ~2,314 days the pre-fix quotient stated: {beta_row}"
        );

        // The wire says the same thing in its own vocabulary: explicit `null`, never a sentinel.
        let v: serde_json::Value =
            serde_json::from_str(&render_json(&report, None).unwrap()).unwrap();
        let vel = &v["summary"]["accounts"]["beta"]["velocity"];
        assert!(
            vel["session_runway_secs"].is_null(),
            "refused → null, not `0` and not a saturated integer: {vel}"
        );
        assert!(
            vel["session_pct_per_min"].is_number(),
            "beside the rate, which is still a measured fact: {vel}"
        );
    }

    #[test]
    fn a_wholly_refused_runway_column_elides_exactly_as_a_wholly_flat_one_already_does() {
        // The interaction the issue asks to CONFIRM rather than assume: §D-STA-5 drops a droppable
        // column that is uniformly the gap sentinel, so refusing enough values could in principle
        // stop the runway being stated at all. Measured here with EVERY account vanishing-rated.
        //
        // The verdict is that this issue adds no new omission mode. The elision is reached today,
        // before any of this, by a wholly FLAT fleet — `zero_velocity_reports_a_known_rate_but_an_
        // unknown_runway` pins exactly this output for `rate == 0` — and a refusal is the same
        // fact-shaped gap as that one: no runway to state. What changes is only WHICH inputs land
        // there, and every one of them previously stated a figure that was FALSE — not merely
        // large. Most were not even conspicuous: at `current = 0.6` and `rate = 5e-6` frac/s the
        // pre-fix code rendered `~11h`, which an operator would act on without hesitation, while
        // the reading tops out at 0.69 against a 0.80 ceiling and never crosses it. A credible
        // wrong figure is worse than an absurd one, for the reason the non-finite arm gives: an
        // absurd number invites the suspicion that saves the reader, and a plausible one does not.
        //
        // R-3 / R-20 are satisfied where they are actually addressed: the account keeps stating its
        // measured VELOCITY, and the fleet line below states its own unknown in words rather than
        // disappearing. Nothing here silently drops a fact that has a value.
        let now = epoch("2026-07-01T12:00:00Z");
        let report = velocity_report(
            vec![
                sample(now - 900, "alpha", 0.6000000, 0.30),
                sample(now - 600, "alpha", 0.6000003, 0.30),
                sample(now - 300, "alpha", 0.6000006, 0.30),
                sample(now - 900, "beta", 0.5000000, 0.30),
                sample(now - 600, "beta", 0.5000003, 0.30),
                sample(now - 300, "beta", 0.5000006, 0.30),
            ],
            now,
        );
        for handle in ["alpha", "beta"] {
            let v = report.velocity.get(handle).expect("a readout");
            assert_eq!(
                v.session_runway_secs, None,
                "{handle}'s quotient is refused"
            );
        }
        let text = render_text(&report, None);
        assert!(
            !text.contains("runway"),
            "the uniformly-refused column elides, as the uniformly-flat one does: {text}"
        );
        // The rate is still stated beside it — and since issue #1136 it is stated as the BOUND,
        // not as `0.0%/min`. Until then this assertion pinned the very string the sentence above
        // denies: `1e-9` frac/s rendered byte-identical to a flat account, so "keeps stating its
        // measured velocity" was satisfied by a cell claiming there was nothing to measure.
        assert!(
            text.contains("velocity") && text.contains(SUB_THRESHOLD_PCT_PER_MIN),
            "the measured rate is still stated beside it: {text}"
        );
        assert!(
            text.contains("accounts last: unknown"),
            "and the fleet line — the surface R-3 / R-20 govern — states its unknown rather than \
             vanishing: {text}"
        );
    }

    // --- AC (issue #1136): `0.0%/min` means a measured zero, and nothing else ----------

    #[test]
    fn fmt_pct_per_min_bounds_the_sub_threshold_band_instead_of_claiming_zero() {
        // The BOUNDARY the issue measured, straddled. These two rates differ by 1e-7 frac/s and
        // sit either side of the one-decimal cliff at 8.33e-6; they must differ in KIND, because
        // one is inside the band the figure form cannot state and the other is the smallest figure
        // it can. Before this fix the first rendered `0.0%/min`.
        assert_eq!(fmt_pct_per_min(8.3e-6), "<0.1%/min"); // 0.0498 %/min — under the cliff
        assert_eq!(fmt_pct_per_min(8.4e-6), "0.1%/min"); // 0.0504 %/min — over it

        // The issue's other measured rows. `5e-6` is a REAL burn moving ~9% of the session quota
        // per 5 h window — it is also #1075's own `~11h` review case — and `1e-9` is the vanishing
        // rate #1132's merge judge flagged. Both used to read as idle.
        assert_eq!(fmt_pct_per_min(5e-6), "<0.1%/min");
        assert_eq!(fmt_pct_per_min(1e-9), "<0.1%/min");

        // The half that makes the change a distinction rather than a blanket reformat: a genuinely
        // flat account still renders the measured-zero string, and is now the ONLY thing that does.
        // Without this line a formatter that bounded everything below 0.1 would pass the rows above.
        assert_eq!(fmt_pct_per_min(0.0), ZERO_PCT_PER_MIN);
    }

    #[test]
    fn sub_threshold_bound_is_the_figure_forms_floor() {
        // The coupling nothing in the type system holds: `<0.1%/min` is honest only while `0.1` is
        // the SMALLEST figure the cell's precision can carry. Re-derived from the formatter itself
        // rather than restated — the rate just over the cliff must render exactly the value the
        // bound names. Widen the precision to `{:.2}` and this fails (`0.05%/min`), instead of a
        // bound shipping that excludes readings the figure form could have stated outright.
        assert_eq!(SUB_THRESHOLD_PCT_PER_MIN, "<0.1%/min");
        assert_eq!(fmt_pct_per_min(8.4e-6), "0.1%/min");
    }

    #[test]
    fn velocity_cell_separates_a_refused_runway_from_a_flat_one() {
        // The concrete harm: since #1075 a sub-threshold BURN and a flat account rendered the same
        // bytes on BOTH cells. Driven through the render, not the formatter — the cells have to
        // reach the operator's table distinguishable, which is a fact about the row and not about
        // `fmt_pct_per_min` in isolation.
        //
        // `burn` climbs +0.03 per 300 s → 1e-4 frac/s → 0.6 %/min, head-room 0.24 to the 0.80
        //   trigger → 2400 s, inside the one-window bound, so its runway is a figure and the
        //   droppable runway column survives the elision pre-pass.
        // `creep` climbs +0.0015 per 300 s → 5e-6 frac/s → 0.03 %/min, head-room 0.297 → 59 400 s,
        //   past the 18 000 s bound, so #1075 refuses the quotient.
        // `flat` never moves → a known 0.0 rate, and gate 1 has no drain to divide by.
        let now = epoch("2026-07-01T12:00:00Z");
        let report = velocity_report(
            vec![
                sample(now - 900, "burn", 0.5000, 0.30),
                sample(now - 600, "burn", 0.5300, 0.30),
                sample(now - 300, "burn", 0.5600, 0.30),
                sample(now - 900, "creep", 0.5000, 0.30),
                sample(now - 600, "creep", 0.5015, 0.30),
                sample(now - 300, "creep", 0.5030, 0.30),
                sample(now - 900, "flat", 0.6000, 0.30),
                sample(now - 600, "flat", 0.6000, 0.30),
                sample(now - 300, "flat", 0.6000, 0.30),
            ],
            now,
        );
        for (handle, refused) in [("burn", false), ("creep", true), ("flat", true)] {
            let v = report.velocity.get(handle).expect("a readout");
            assert_eq!(
                v.session_runway_secs.is_none(),
                refused,
                "{handle}'s runway: {v:?}"
            );
        }

        let text = render_text(&report, None);
        let row = |handle: &str| {
            text.lines()
                .find(|l| l.starts_with(handle))
                .unwrap_or_else(|| panic!("no `{handle}` row in: {text}"))
                .to_owned()
        };

        // The RUNWAY cell cannot tell `creep` from `flat` — refused and absent are one gap, by
        // #1075's design ("`None` — NEVER a sentinel — for every refusal alike"). Only `burn`,
        // whose quotient was admitted, carries an approximate figure. Asserted against a LIVE
        // column: `burn` keeps it off the elision pre-pass, so "no figure on the other two" is a
        // measured absence rather than the vacuous truth an elided column would also satisfy.
        assert!(text.contains("runway"), "the runway column is live: {text}");
        assert!(row("burn").contains("~40m"), "admitted runway: {text}");
        for handle in ["creep", "flat"] {
            assert!(
                !row(handle).contains('~'),
                "{handle}'s runway states no figure: {text}"
            );
        }

        // So the VELOCITY cell is the whole discriminator, and it now discriminates.
        assert!(row("burn").contains("0.6%/min"), "the figure: {text}");
        assert!(
            row("creep").contains(SUB_THRESHOLD_PCT_PER_MIN),
            "a measured burn under the precision reads as a bound: {text}"
        );
        assert!(
            row("flat").contains(ZERO_PCT_PER_MIN),
            "a measured zero keeps the zero string: {text}"
        );
        assert!(
            !row("flat").contains(SUB_THRESHOLD_PCT_PER_MIN),
            "and does NOT read as a burn: {text}"
        );
        // Stated once more on the CELLS themselves rather than on the rows around them: two rows
        // differ in their session column whatever the velocity cell does, so a row-level inequality
        // would go green without touching the thing this issue is about.
        assert_ne!(
            velocity_cell(report.velocity.get("creep")),
            velocity_cell(report.velocity.get("flat")),
            "the two cells that #1136 found byte-identical"
        );

        // The new string is a neutral observation, not advice — it must clear the #542 guard the
        // same way every other cell on this surface does.
        assert_eq!(scan_banned(&text), None, "the bound render is neutral");
    }

    // --- AC (issue #1146): `0.0` on the WIRE means a measured zero, and nothing else ----

    #[test]
    fn round_pct_per_min_never_wires_a_burn_as_the_measured_flat_reading() {
        // The boundary re-derived by bit-exact binary search on the `f64` ladder, straddled. These
        // two rates are ADJACENT `f64`s — there is nothing between them — and they sat either side
        // of the three-decimal cliff, so before this fix the first wired the same `0.0` a measured
        // flat account carries while the second wired `0.001`.
        assert!(round_pct_per_min(8.333_333_333_333_331e-8) > 0.0);
        assert_eq!(round_pct_per_min(8.333_333_333_333_333e-8), 0.001);

        // The issue's measured rows, worst-case first. `8e-8` frac/s drains ~0.144% of a session
        // quota per 5 h window and used to wire as idle; `1e-9` is the rate #1136 cites as its
        // motivating case. Bounding BOTH sides is what makes this a distinction rather than a
        // blanket rescale: the value must be strictly positive AND still under the smallest figure
        // three decimals can carry, which is what a floor-to-`0.001` shape would fail.
        for rate in [1e-9f64, 8.0e-8, 8.333_333_333_333_331e-8] {
            let wired = round_pct_per_min(rate);
            assert!(
                wired > 0.0,
                "{rate:e} frac/s is a measured BURN, not a measured flat: wired {wired:?}"
            );
            assert!(
                wired < 0.001,
                "{rate:e} frac/s must keep its magnitude, not be floored to the smallest \
                 three-decimal value: wired {wired:?}"
            );
            // And the magnitude is the rate's OWN, not a synthetic constant — within the three
            // significant figures the fallback quantizes to.
            let truth = pct_per_min(rate);
            assert!(
                (wired - truth).abs() <= truth * 1e-2,
                "{rate:e} frac/s: wired {wired:?} is not the measured {truth:e}"
            );
        }

        // Every rate the primary form can already carry is untouched — this arm adds a case, it does
        // not rescale the field.
        assert_eq!(round_pct_per_min(5e-6), 0.03);
        assert_eq!(round_pct_per_min(1e-2), 60.0);

        // The half that makes it a distinction: a genuinely flat account still wires the measured
        // zero, and is now the ONLY thing that does. Without this line a quantizer that lifted
        // everything off zero would pass every row above.
        assert_eq!(round_pct_per_min(0.0), 0.0);
    }

    #[test]
    fn sub_threshold_wire_values_are_stable_across_runs() {
        // The constraint the rounding exists for, and the one the "carry the unrounded value"
        // shape would have broken: a `--json` wire that gets diffed across runs must not churn
        // when the EMA lands a few bits apart on two runs of the same data. Walked over 2000
        // ADJACENT `f64`s — the finest perturbation that exists — on both sides of the cliff.
        //
        // Deliberately INERT against the pre-#1146 body, which was stable too (`0.0` never churns).
        // This guards the OTHER direction — a future edit that reaches for raw precision in the
        // sub-threshold band — so it is a constraint on the fix, not a regression test for it.
        for base in [1e-9f64, 8.0e-8, 1.234_567_89e-2] {
            let mut seen = BTreeSet::new();
            let mut rate = base;
            for _ in 0..2000 {
                rate = f64::from_bits(rate.to_bits() + 1);
                seen.insert(format!("{:?}", round_pct_per_min(rate)));
            }
            assert_eq!(
                seen.len(),
                1,
                "{base:e} frac/s must quantize to ONE wire value, got {seen:?}"
            );
        }

        // The control that keeps the assertion above from being vacuous: the same walk on the
        // UNROUNDED rate churns, so the stability measured there is the quantizer's doing and not a
        // property of `f64`s that close together.
        let mut raw = BTreeSet::new();
        let mut rate = 1e-9f64;
        for _ in 0..2000 {
            rate = f64::from_bits(rate.to_bits() + 1);
            raw.insert(format!("{:?}", pct_per_min(rate)));
        }
        assert!(
            raw.len() > 1000,
            "the unrounded rate should churn across adjacent f64s, got {} distinct",
            raw.len()
        );
    }

    #[test]
    fn the_wire_and_the_cell_agree_that_a_sub_threshold_rate_is_a_burn() {
        // The issue's stated acceptance signal. Since #1136 the human cell states `1e-9` frac/s as a
        // BOUND — an explicit "measured, strictly positive" — while the wire still called the same
        // reading flat. The two surfaces scale the identical EMA through the identical
        // `pct_per_min`, so a reader consulting both got contradictory answers about whether the
        // account was burning at all. Asserted as the AGREEMENT it is, on both surfaces at once.
        for rate in [1e-9f64, 8.0e-8, 5e-6] {
            assert_eq!(fmt_pct_per_min(rate), SUB_THRESHOLD_PCT_PER_MIN);
            assert!(
                round_pct_per_min(rate) > 0.0,
                "{rate:e} frac/s: the cell says measured-burning, the wire must not say flat"
            );
        }
        // And they agree about flatness too. This pins the agreement, NOT the guard's spelling —
        // a flat rate reaches `0.0` under `!= 0.0` as well, since `0.0 != 0.0` is false. The
        // spelling only separates them on a negative rate, which `replay_velocity_ema` cannot
        // produce.
        assert_eq!(fmt_pct_per_min(0.0), ZERO_PCT_PER_MIN);
        assert_eq!(round_pct_per_min(0.0), 0.0);
    }

    #[test]
    fn a_sub_threshold_burn_reaches_the_json_wire_as_a_burn_on_both_rate_fields() {
        // Driven through `render_json`, not the quantizer — the values have to reach a machine
        // reader distinguishable, which is a fact about the serialized document and not about
        // `round_pct_per_min` in isolation.
        //
        // `creep` climbs +6e-6 session per 300 s → 2e-8 frac/s → 1.2e-4 %/min, and +3e-6 weekly →
        //   1e-8 frac/s → 6e-5 %/min. Both are inside the band three decimals collapse.
        // `flat` never moves → a measured 0.0 on BOTH rates. A flat weekly is a KNOWN zero here,
        //   not an unknown one — `replay_velocity_ema` only withholds a rate on a reset or too few
        //   intervals, which is why the third account is needed to reach `null` at all.
        // `reset` climbs its session but DROPS its weekly on the last interval → the EMA resets, so
        //   its weekly rate is genuinely unmeasurable → the explicit `null`.
        let now = epoch("2026-07-01T12:00:00Z");
        let report = velocity_report(
            vec![
                sample(now - 900, "creep", 0.600_000, 0.300_000),
                sample(now - 600, "creep", 0.600_006, 0.300_003),
                sample(now - 300, "creep", 0.600_012, 0.300_006),
                sample(now - 900, "flat", 0.600_000, 0.300_000),
                sample(now - 600, "flat", 0.600_000, 0.300_000),
                sample(now - 300, "flat", 0.600_000, 0.300_000),
                sample(now - 900, "reset", 0.500_000, 0.300_000),
                sample(now - 600, "reset", 0.530_000, 0.310_000),
                sample(now - 300, "reset", 0.560_000, 0.290_000),
            ],
            now,
        );
        let v: serde_json::Value =
            serde_json::from_str(&render_json(&report, None).unwrap()).unwrap();
        let vel = |handle: &str| v["summary"]["accounts"][handle]["velocity"].clone();

        // The three-way vocabulary, all three states present in ONE document so the assertions are
        // about the distinction rather than about any single value.
        let creep = vel("creep");
        for field in ["session_pct_per_min", "weekly_pct_per_min"] {
            let wired = creep[field]
                .as_f64()
                .unwrap_or_else(|| panic!("{field} is a real number: {creep}"));
            assert!(
                wired > 0.0,
                "a measured burn must not wire the measured-flat 0.0 on {field}: {creep}"
            );
            assert!(
                wired < 0.001,
                "and must keep its own magnitude rather than be floored on {field}: {creep}"
            );
        }

        // MEASURED FLAT — the reading the `0.0` this issue reclaimed actually belongs to. Both
        // rates, because the fix touches both fields through the one quantizer.
        let flat = vel("flat");
        for field in ["session_pct_per_min", "weekly_pct_per_min"] {
            assert_eq!(
                flat[field].as_f64().unwrap(),
                0.0,
                "a measured flat account still wires 0.0 on {field}: {flat}"
            );
        }

        // UNKNOWN — still the explicit `null`, never a sentinel, which is the state the rescued
        // burns must not be pushed into either.
        let reset = vel("reset");
        assert!(
            reset["weekly_pct_per_min"].is_null(),
            "an unmeasurable weekly rate is an explicit null, never a number: {reset}"
        );

        // All three states in one document, mutually distinct — a wire that collapsed any pair
        // would satisfy the individual assertions above but not this.
        assert_ne!(creep["session_pct_per_min"], flat["session_pct_per_min"]);
        assert_ne!(creep["weekly_pct_per_min"], flat["weekly_pct_per_min"]);
        assert_ne!(flat["weekly_pct_per_min"], reset["weekly_pct_per_min"]);
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
        let secs = fleet.runway_secs().expect("a finite pooled runway");
        assert!(
            (secs - 164_400).abs() <= 2,
            "pooled runway ≈ 164 400 s: {secs}"
        );

        // Human: the roster block foots with ONE approximate, neutral fleet figure + the n-of-m
        // cardinality (§D-STA-5 — an aggregate line, 2-space indented, no per-account `fleet` prefix).
        let text = render_text(&report, None);
        assert!(
            text.contains("  accounts last: ~2 days at the current combined rate (2 of 3 counted)"),
            "fleet line: {text}"
        );
        assert_eq!(
            scan_banned(&text),
            None,
            "the fleet render is neutral (issue #542 guard)"
        );

        // Wire: a `fleet` object on the SUMMARY, carrying the whole-second runway + the cardinality.
        let v: serde_json::Value =
            serde_json::from_str(&render_json(&report, None).unwrap()).unwrap();
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
            clean.runway_secs(),
            mixed.runway_secs(),
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
        // explicit unknown, but the cardinality is still surfaced (counted > 0). The human STATES the
        // unknown on its own line (R-3 / R-20 — never omits it, issue #1028); the wire carries the
        // object with a `null` runway (never a sentinel), so a machine reader still learns
        // "2 accounts, no measurable burn".
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
            fleet.runway_secs().is_none(),
            "no combined burn → unknown runway"
        );
        // The line is PRESENT and states the unknown + the cardinality. (The prior assertion here
        // scanned for `"fleet  "`, a string this render never emits — it passed on every input,
        // including the absurd figure the issue reported. Assert the line the surface really prints.)
        let flat_text = render_text(&flat, None);
        assert!(
            flat_text
                .contains("accounts last: unknown — no combined usage measured (2 of 2 counted)"),
            "a flat fleet STATES its unknown rather than omitting the line: {flat_text}"
        );
        assert_eq!(
            scan_banned(&flat_text),
            None,
            "the stated unknown is neutral — no forecast verb, no acquisitive call"
        );
        let v: serde_json::Value =
            serde_json::from_str(&render_json(&flat, None).unwrap()).unwrap();
        assert!(
            v["summary"]["fleet"]["runway_secs"].is_null(),
            "an unknown fleet runway is an explicit null, not a sentinel: {}",
            v["summary"]["fleet"]
        );
        assert_eq!(v["summary"]["fleet"]["counted"].as_i64().unwrap(), 2);
        assert_eq!(
            scan_banned(&render_json(&flat, None).unwrap()),
            None,
            "the null-runway fleet object is neutral too"
        );

        // (b) Every account is UNDER-SAMPLED (a single reading → no interval → no velocity) → NOTHING
        // is countable → no fleet at all, on either surface (the wire OMITS the object). Distinct
        // from (a): R-3 / R-20 make an unknown RUNWAY state itself, but here there is no counted
        // fleet to state one ABOUT — no cardinality, no pooled rate, and the wire has never carried
        // the object. That degradation predates #1028 and is deliberately left as it was.
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
        assert!(
            !render_text(&thin, None).contains("accounts last"),
            "no counted fleet → no runway line to state at all"
        );
        let v2: serde_json::Value =
            serde_json::from_str(&render_json(&thin, None).unwrap()).unwrap();
        assert!(
            v2["summary"].get("fleet").is_none(),
            "the wire omits an empty fleet: {}",
            v2["summary"]
        );
    }

    // --- AC: the fleet runway refuses an implausible figure (issue #1028) ------------

    #[test]
    fn fleet_runway_state_refuses_the_quotient_of_a_vanishing_burn() {
        // The REPORTED defect. A decayed EMA is not zero, so the old `total_rate > 0.0` guard let it
        // through and printed the quotient. The fix still DIVIDES — an input-side epsilon on the
        // rate was the rejected alternative — and refuses the RESULT, which is why both of the
        // issue's own reproductions land in `BeyondWeeklyWindow` and NOT in `Flat`: the burn was
        // real, it was the RATIO that was absurd.
        assert_eq!(
            fleet_runway_state(0.5, 1e-11),
            FleetRunwayState::BeyondWeeklyWindow,
            "0.5 head-room at 1e-11 fraction/sec is ~578,700 days — the same order as the reported \
             ~648427 (whose ~787,000 came from the reproduction's larger head-room, not this rate)"
        );
        assert_eq!(
            fleet_runway_state(0.5, 1e-300),
            FleetRunwayState::BeyondWeeklyWindow,
            "1e-300 overflowed i64 and SATURATED to i64::MAX (9223372036854775807)"
        );

        // R-4: whatever this refuses, it must never be reachable as a saturated sentinel — on the
        // state, and on the `Option` every consumer actually reads.
        for rate in [1e-11, 1e-300, f64::MIN_POSITIVE, 5e-324] {
            let state = fleet_runway_state(0.5, rate);
            assert_ne!(
                state,
                FleetRunwayState::Known(i64::MAX),
                "a saturating conversion must be unreachable (rate {rate:e})"
            );
            let fleet = FleetRunway {
                state,
                counted: 1,
                observed: 1,
            };
            assert_eq!(fleet.runway_secs(), None, "and it states no figure");
        }
    }

    #[test]
    fn fleet_runway_state_bounds_the_result_at_one_weekly_window() {
        // The bound is on the RESULT, in seconds, and is exactly one weekly window: a longer runway
        // asserts the fleet drains with no weekly reset intervening, which cannot happen. Probe both
        // sides of the boundary by choosing a rate that lands the quotient on it exactly.
        let bound = FLEET_RUNWAY_PLAUSIBLE_MAX_SECS;
        assert_eq!(bound, 604_800, "one weekly window in seconds");

        let headroom = 0.5;
        assert_eq!(
            fleet_runway_state(headroom, headroom / bound as f64),
            FleetRunwayState::Known(bound),
            "exactly one weekly window is still plausible — the bound is inclusive"
        );
        assert_eq!(
            fleet_runway_state(headroom, headroom / (bound + 1) as f64),
            FleetRunwayState::BeyondWeeklyWindow,
            "one second past the window is refused, not clamped back to the bound"
        );

        // NOT a clamp: nothing past the bound yields a figure — least of all a large
        // plausible-looking stand-in at the boundary itself.
        for rate in [1e-11, 1e-300] {
            let fleet = FleetRunway {
                state: fleet_runway_state(headroom, rate),
                counted: 1,
                observed: 1,
            };
            assert_eq!(fleet.runway_secs(), None);
            assert_ne!(fleet.runway_secs(), Some(bound), "refused, never clamped");
        }

        // An ordinary drain is untouched — 0.5 head-room at 1e-5 /sec is 50,000 s (~14 h).
        assert_eq!(
            fleet_runway_state(0.5, 1e-5),
            FleetRunwayState::Known(50_000)
        );
    }

    #[test]
    fn checked_runway_secs_refuses_every_figure_a_saturating_cast_would_state() {
        // Gate 4 IN ISOLATION (issue #1081). Gate 3 bounds the quotient to one weekly window before
        // `fleet_runway_state` can reach the conversion, so no argument pair to that function
        // exercises this gate at all — which is why the R-4 independence claim shipped ungated:
        // replacing the checked conversion with `secs as i64`, the plausibility bound left fully
        // intact, passed the entire suite. Reaching the gate means calling it directly, with the
        // values gate 3 would have refused.
        //
        // This does NOT make `fleet_runway_state`'s `Unmeasurable` arm reachable: the refusals here
        // are `None`, and only that function turns a `None` into a classification.

        // Out of range ABOVE. What the unchecked cast does is asserted FIRST on every input —
        // a value refused for some unrelated reason would look like it proves the gate while
        // discriminating nothing. `i64::MAX as f64` is the boundary: `i64::MAX` is not
        // representable in `f64`, so it rounds to `i64::MAX + 1` — one past the range, and the
        // exact figure the cast collapses to.
        for secs in [1e300, f64::MAX, i64::MAX as f64, f64::INFINITY] {
            assert_eq!(
                secs as i64,
                i64::MAX,
                "{secs:e} must saturate to issue #1028's sentinel, or this case tests nothing"
            );
            assert_eq!(
                checked_runway_secs(secs),
                None,
                "{secs:e} has no whole-second figure — refuse it, never state i64::MAX"
            );
        }

        // Out of range BELOW: the cast states a NEGATIVE runway as confidently as a positive one.
        assert_eq!(-1.0_f64 as i64, -1, "the cast states a negative runway");
        assert_eq!(checked_runway_secs(-1.0), None);

        // NOT A NUMBER — the only failure mode yielding a small, entirely credible figure rather
        // than a conspicuous sentinel: `NaN as i64` is 0, so `Known(0)` would read as "exhausted
        // now" and FIRE the daemon's warn edge rather than merely mis-state a horizon. That one is
        // stated rather than asserted because `clippy::cast_nan_to_int` — warn-by-default, and
        // denied by this repo's clippy gate's `-D warnings` — rejects the demonstrating cast under
        // `cargo clippy`. `ci-ok` requires that gate, so the cast cannot land; `cargo test` alone
        // would compile it, which is why the guarantee is the gate's and not the compiler's.
        assert_eq!(checked_runway_secs(f64::NAN), None);

        // The positive half, so the gate is a filter and not a blanket refusal: everything gate 3
        // can actually hand it converts, endpoints included.
        for secs in [0.0, 1.0, 50_000.0, FLEET_RUNWAY_PLAUSIBLE_MAX_SECS as f64] {
            assert_eq!(
                checked_runway_secs(secs),
                Some(secs as i64),
                "{secs} is inside the window and must convert"
            );
        }
    }

    #[test]
    fn fleet_runway_state_separates_flat_from_unmeasurable_from_out_of_window() {
        // The three unknowns are DISTINCT, and the distinction is load-bearing: the daemon's warn
        // edge acts on `BeyondWeeklyWindow` and holds on the other two, so collapsing them would
        // either strand the guard or fabricate a recovery.

        // Gate 1 — a zero burn is FLAT: measured, and none of it burning. Not a fault.
        for rate in [0.0, -0.0] {
            assert_eq!(fleet_runway_state(0.5, rate), FleetRunwayState::Flat);
        }
        // Gate 2 — anything else non-finite or negative is unmeasurable: no figure, and no evidence
        // about the fleet's position either. `Unmeasurable` is a statement about the INPUTS ONLY.
        for rate in [-1e-5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                fleet_runway_state(0.5, rate),
                FleetRunwayState::Unmeasurable,
                "rate {rate:?} is not a measurable burn"
            );
        }

        // ...and correspondingly, NO well-formed reading lands there however extreme. A burn small
        // enough to overflow the quotient to `inf` is still a MEASURED burn, and an infinite
        // quotient is the strongest possible evidence the pool outlasts a week — so it must be
        // classified with the other out-of-window readings.
        //
        // This is reachable, not theoretical: `replay_velocity_ema` halves the weekly EMA on each
        // flat sample, so a fleet that stops burning walks down through this band. Classifying it as
        // unmeasurable would tell the operator their rate "could not be read" when it was read and
        // merely tiny, AND would deny the daemon's warn edge its recovery evidence.
        let headroom = 1.18_f64; // the goldens' pooled head-room
        assert!(
            (headroom / 1e-310).is_infinite(),
            "precondition: this rate really does overflow the quotient"
        );
        assert_eq!(
            fleet_runway_state(headroom, 1e-310),
            FleetRunwayState::BeyondWeeklyWindow,
            "an overflowing quotient is the emphatic form of out-of-window, not unmeasurable"
        );
        // Monotonicity, stated as the property rather than the two points: a SLOWER burn can never
        // be read as carrying LESS information about the fleet than a faster one.
        for rate in [1e-11, 1e-300, 1e-310, f64::MIN_POSITIVE, 5e-324] {
            assert_eq!(
                fleet_runway_state(headroom, rate),
                FleetRunwayState::BeyondWeeklyWindow,
                "rate {rate:e} is measured and slight — out-of-window, never unmeasurable"
            );
        }
        for headroom in [f64::NAN, f64::INFINITY, -0.1] {
            assert_eq!(
                fleet_runway_state(headroom, 1e-5),
                FleetRunwayState::Unmeasurable
            );
        }
        // `0.0 / 0.0` is NaN, and `NaN as i64` is 0 — a silent, plausible-looking zero if the
        // finite check did not precede the conversion. Classified FLAT by gate 1 (the zero rate is
        // the more specific truth), so the NaN quotient is never reached.
        assert_eq!(fleet_runway_state(0.0, 0.0), FleetRunwayState::Flat);
        // Zero head-room at a real burn is a genuine, plausible zero — refusal is for the
        // UNMEASURABLE, not for a measured "already exhausted".
        assert_eq!(fleet_runway_state(0.0, 1e-5), FleetRunwayState::Known(0));
    }

    #[test]
    fn every_fleet_runway_state_has_a_neutral_stated_phrase() {
        // R-3 / R-20 across ALL FOUR states, including the one no `Report` can produce
        // (`Unmeasurable` is a statement about malformed inputs) — which is precisely the arm that
        // would otherwise ship unread. Properties, not a second copy of the strings, so a reworded
        // phrase does not have to be edited in two places to stay honest.
        let phrases = [
            FleetRunwayState::Known(50_000),
            FleetRunwayState::Flat,
            FleetRunwayState::BeyondWeeklyWindow,
            FleetRunwayState::Unmeasurable,
        ]
        .map(|s| (s, fleet_runway_phrase(s)));

        for (state, phrase) in &phrases {
            assert_eq!(
                scan_banned(phrase),
                None,
                "{state:?} must stay neutral: {phrase}"
            );
            assert!(
                !phrase.is_empty() && !phrase.ends_with(' '),
                "{state:?} renders a clause the caller can wrap: {phrase:?}"
            );
            // R-21: no implementation vocabulary reaches a user-facing stat string.
            for jargon in [
                "None", "null", "i64", "f64", "NaN", "inf", "saturat", "overflow", "EMA",
                "quotient", "fault",
            ] {
                assert!(
                    !phrase.contains(jargon),
                    "{state:?} leaks implementation vocabulary {jargon:?}: {phrase}"
                );
            }
        }

        // Only the KNOWN state states a figure; every unknown says so in the operator's word.
        assert!(!phrases[0].1.contains("unknown"));
        for (state, phrase) in &phrases[1..] {
            assert!(
                phrase.starts_with("unknown — "),
                "{state:?} leads with the unknown: {phrase}"
            );
        }

        // Each state says something DIFFERENT — otherwise the distinction the model exists to carry
        // would be invisible on the surface that reports it.
        let distinct: std::collections::HashSet<_> = phrases.iter().map(|(_, p)| p).collect();
        assert_eq!(distinct.len(), 4, "each state states its own condition");
    }

    #[test]
    fn the_fleet_runway_line_labels_its_value_so_a_stated_unknown_still_parses() {
        // Issue #1082. The frame was `accounts last {phrase}` — a sentence whose verb reaches for a
        // duration, which only ONE of the four states supplies. The other three rendered
        // `accounts last unknown — …`: an object that never arrives, parseable only by a reader who
        // already knows the figure-bearing shape it stands in for. Asserted on the LINE rather than
        // the phrase, and over all four states, because the FRAME is what was mis-worded and
        // `Unmeasurable` reaches no render through a WELL-FORMED `Report`.
        let lines = [
            FleetRunwayState::Known(300_000),
            FleetRunwayState::Flat,
            FleetRunwayState::BeyondWeeklyWindow,
            FleetRunwayState::Unmeasurable,
        ]
        .map(|state| {
            (
                state,
                fleet_runway_line(FleetRunway {
                    state,
                    counted: 2,
                    observed: 3,
                }),
            )
        });

        for (state, line) in &lines {
            // A LABEL, not a verb: what precedes the first `: ` names the fact and what follows is
            // its value, whatever that value turns out to be. A bare-verb frame affords no such
            // split, so this fails on the shape rather than on the copy.
            let (label, value) = line
                .split_once(": ")
                .unwrap_or_else(|| panic!("{state:?} states a labelled value: {line}"));
            assert_eq!(
                label, "accounts last",
                "{state:?} labels the same fact — the frame does not move with the state: {line}"
            );
            assert!(
                value.starts_with(&fleet_runway_phrase(*state)),
                "{state:?} states its own clause as that value: {line}"
            );
            assert!(
                !line.contains("accounts last unknown"),
                "{state:?} leaves the verb reaching for an object that never comes: {line}"
            );
            assert!(
                line.ends_with("(2 of 3 counted)"),
                "{state:?} keeps its cardinality (R-5): {line}"
            );
            assert_eq!(scan_banned(line), None, "{state:?} stays neutral: {line}");
        }

        // The distinctions the line exists to carry survive the label — a figure only where one was
        // measured, and three unknowns still telling themselves apart from it and from each other.
        assert!(
            lines[0].1.contains('~') && !lines[0].1.contains("unknown"),
            "a plausible runway still states its figure: {}",
            lines[0].1
        );
        for (state, line) in &lines[1..] {
            assert!(
                line.starts_with("accounts last: unknown — "),
                "{state:?} states the unknown AS the labelled value: {line}"
            );
        }
        let distinct: std::collections::HashSet<_> = lines.iter().map(|(_, l)| l).collect();
        assert_eq!(distinct.len(), 4, "each state renders its own line");
    }

    #[test]
    fn fleet_runway_figure_is_derived_from_the_state_so_the_two_cannot_disagree() {
        // `runway_secs()` is a projection, not a stored twin — the property that makes a fixture
        // pairing "a figure" with "there is no figure" unrepresentable.
        let known = FleetRunway {
            state: FleetRunwayState::Known(1234),
            counted: 1,
            observed: 1,
        };
        assert_eq!(known.runway_secs(), Some(1234));
        for state in [
            FleetRunwayState::Flat,
            FleetRunwayState::BeyondWeeklyWindow,
            FleetRunwayState::Unmeasurable,
        ] {
            let fleet = FleetRunway {
                state,
                counted: 1,
                observed: 1,
            };
            assert_eq!(
                fleet.runway_secs(),
                None,
                "{state:?} states no figure on the wire"
            );
        }
    }

    #[test]
    fn fleet_runway_states_its_unknown_on_the_cli_when_the_burn_is_too_slight() {
        // End-to-end through the real aggregation: a fleet that moves by the smallest representable
        // step over the window has a positive-but-vanishing pooled rate — the intermittent case the
        // issue describes, where landing on exactly 0.0 hid the defect and anything else exposed it.
        let now = epoch("2026-07-01T12:00:00Z");
        let creeping = velocity_report(
            vec![
                sample(now - 900, "work", 0.60, 0.50),
                sample(now - 600, "work", 0.60, 0.50),
                sample(now - 300, "work", 0.60, 0.50 + f64::EPSILON),
                sample(now - 900, "home", 0.40, 0.30),
                sample(now - 600, "home", 0.40, 0.30),
                sample(now - 300, "home", 0.40, 0.30 + f64::EPSILON),
            ],
            now,
        );
        let fleet = fleet_runway(&creeping).expect("a counted fleet");
        assert_eq!(
            fleet.state,
            FleetRunwayState::BeyondWeeklyWindow,
            "a real but slight burn is OUT-OF-WINDOW, not flat and not unmeasurable"
        );
        assert_eq!(fleet.runway_secs(), None, "so it states no figure");

        // R-20: the fact is STATED, not omitted — as a BOUND on what was measured, worded as the
        // reader-meaningful condition rather than the internal cause (R-21). Deliberately NOT "the
        // rate is too small": the gate is on the RATIO, so that wording would misattribute an
        // ordinary burn with ample head-room to a broken measurement.
        let text = render_text(&creeping, None);
        assert!(
            text.contains(
                "accounts last: unknown — more than a week at the current combined rate \
                 (2 of 2 counted)"
            ),
            "the out-of-window unknown is stated with its cardinality: {text}"
        );
        assert_eq!(scan_banned(&text), None, "the stated unknown stays neutral");

        // REQ-STA-B-006 / REQ-STA-SUR-001: no absurd magnitude can be read as a forecast, and no
        // saturated sentinel reaches the wire. `fmt_runway_days` is the ONLY source of `day`/`days`
        // in this render (the per-account `runway` cell is hours-scale), so its absence is exactly
        // "no days-scale figure was stated" — the class the reported `~648427 days` belonged to.
        assert!(
            !text.contains("day"),
            "an out-of-window burn states no days-scale figure: {text}"
        );
        let v: serde_json::Value =
            serde_json::from_str(&render_json(&creeping, None).unwrap()).unwrap();
        assert!(
            v["summary"]["fleet"]["runway_secs"].is_null(),
            "the wire states the unknown as null, never i64::MAX: {}",
            v["summary"]["fleet"]
        );
        assert_eq!(v["summary"]["fleet"]["counted"].as_i64().unwrap(), 2);
    }

    #[test]
    fn fleet_runway_line_is_printed_in_every_state_of_a_counted_fleet() {
        // R-20 as a single sweep: each state of a COUNTED fleet renders exactly one runway line
        // carrying the `n of m` cardinality. Regression guard for the shape of the defect — the fix
        // to the guard must not make the surface quieter (premortem P2 in
        // `docs/requirements/stats-honesty-cross-surface.md`).
        let now = epoch("2026-07-01T12:00:00Z");

        let known = velocity_report(
            vec![
                sample(now - 900, "work", 0.60, 0.50),
                sample(now - 600, "work", 0.65, 0.60),
                sample(now - 300, "work", 0.70, 0.70),
            ],
            now,
        );
        let flat = velocity_report(
            vec![
                sample(now - 900, "work", 0.60, 0.60),
                sample(now - 600, "work", 0.60, 0.60),
                sample(now - 300, "work", 0.60, 0.60),
            ],
            now,
        );
        let creeping = velocity_report(
            vec![
                sample(now - 900, "work", 0.60, 0.50),
                sample(now - 600, "work", 0.60, 0.50),
                sample(now - 300, "work", 0.60, 0.50 + f64::EPSILON),
            ],
            now,
        );

        for (label, report) in [
            ("known", &known),
            ("flat", &flat),
            ("out-of-window burn", &creeping),
        ] {
            let fleet = fleet_runway(report).expect("a counted fleet");
            assert!(fleet.counted > 0, "{label}: precondition — counted fleet");
            let text = render_text(report, None);
            let lines: Vec<&str> = text
                .lines()
                .filter(|l| l.contains("accounts last"))
                .collect();
            assert_eq!(
                lines.len(),
                1,
                "{label}: exactly one runway line in every state, got {lines:?}"
            );
            assert!(
                lines[0].contains(&format!(
                    "({} of {} counted)",
                    fleet.counted, fleet.observed
                )),
                "{label}: the cardinality survives every state: {}",
                lines[0]
            );
            assert_eq!(scan_banned(&text), None, "{label}: neutral in every state");
        }

        // The three states are genuinely distinct — the sweep is not passing on one shape thrice.
        assert!(matches!(
            fleet_runway(&known).unwrap().state,
            FleetRunwayState::Known(_)
        ));
        assert_eq!(fleet_runway(&flat).unwrap().state, FleetRunwayState::Flat);
        assert_eq!(
            fleet_runway(&creeping).unwrap().state,
            FleetRunwayState::BeyondWeeklyWindow
        );

        // ...and each renders a DIFFERENT line, so "one line per state" is not one line thrice.
        let rendered: Vec<String> = [&known, &flat, &creeping]
            .iter()
            .map(|r| {
                render_text(r, None)
                    .lines()
                    .find(|l| l.contains("accounts last"))
                    .expect("a runway line")
                    .to_owned()
            })
            .collect();
        assert_eq!(
            rendered
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3,
            "each state states its own condition: {rendered:?}"
        );
    }

    // --- AC: --json schema:1 stays byte-stable vs #158/#159 --------------------------

    /// The frozen schema:1 wire. #160 is HUMAN-render only — it adds no field, no
    /// recommendation, no glyph — so this is the #158/#159 contract plus, since issue #804,
    /// the roster block's two ADDITIVE census fields (`all_high_threshold` +
    /// `all_high_covered_secs`), and since issue #866 its third (`census_over_roster`, the
    /// census's SET). Additive is why `schema` still reads `1`: no existing field
    /// changed type, name, order or meaning, so every pre-#804 reader decodes these bytes
    /// exactly as before.
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
        "all_high_secs": 0,
        "all_high_threshold": 0.95,
        "all_high_covered_secs": 21600,
        "census_over_roster": true,
        "capacity_holds": 0,
        "capacity_holds_session": 0,
        "capacity_holds_weekly": 0,
        "capacity_hold_secs_lower_bound": 0,
        "capacity_hold_covered_secs": 0,
        "capacity_session_line": 0.0,
        "capacity_weekly_line": 0.0
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
      "all_high_secs": 0,
      "all_high_threshold": 0.95,
      "all_high_covered_secs": 21600,
      "census_over_roster": true,
      "capacity_holds": 0,
      "capacity_holds_session": 0,
      "capacity_holds_weekly": 0,
      "capacity_hold_secs_lower_bound": 0,
      "capacity_hold_covered_secs": 0,
      "capacity_session_line": 0.0,
      "capacity_weekly_line": 0.0
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
            render_json(&wire_golden_report(), None).unwrap(),
            WIRE_GOLDEN,
            "the schema:1 wire drifted — only an ADDITIVE change (a new key, as issue #804 \
             made) may move these bytes without a `schema` bump"
        );
    }

    // --- AC: degenerate periods render a neutral summary without panic ---------------

    #[test]
    fn summary_band_renders_empty_single_and_all_gap_without_panic() {
        // Empty roster → the band is empty (nothing to summarise); the views print their
        // own "no per-account usage" line, never a panic.
        let empty = charts_report(&[], &[]);
        assert_eq!(render_summary(&empty), "");
        let _ = render_text(&empty, None);
        let _ = render_charts(&empty, 80, true, false, None);

        // A single account is its own lowest-utilisation pick. The band is AGGREGATE-ONLY now
        // (§D-STA-5) — the per-account `signal` word is a table column, not a band entry.
        let single = charts_report(&[("solo", stat(1, ds(0.5, 0.5, 0.5), 0.0, 1.0))], &[]);
        assert_eq!(
            render_summary(&single),
            "  lowest utilisation: solo (session mean 50%)"
        );

        // An all-gap account (present in the summary, absent from every bucket) still ranks as the
        // lowest — no panic, no fabricated data, still neutral.
        let all_gap = charts_report(
            &[("ghost", stat(1, ds(0.0, 0.0, 0.0), 0.0, 0.0))],
            &[&[], &[]],
        );
        let band = render_summary(&all_gap);
        assert!(band.contains("lowest utilisation: ghost"));
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
        let band = render_summary(&report);
        assert!(
            !band.contains("dark"),
            "an unsampled account is not in the aggregate block: {band:?}"
        );
        assert!(
            band.contains("lowest utilisation: live"),
            "lowest ranges over observed accounts, not the 0% unsampled one: {band:?}"
        );

        // A roster of ONLY unsampled accounts has nothing measured to summarise → empty band.
        let all_dark = charts_report(&[("dark", stat(0, ds(0.0, 0.0, 0.0), 0.0, 1.0))], &[]);
        assert_eq!(render_summary(&all_dark), "");
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
        let out = render_text(&orphan_report(Some(&live)), None);
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
        let out = render_charts(&orphan_report(Some(&live)), 120, false, false, None);
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
        let json = render_json(&orphan_report(Some(&live)), None).unwrap();
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
        let json = render_json(&orphan_report(Some(&live)), None).unwrap();
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
        let out = render_text(&orphan_report(None), None);
        assert!(
            !out.contains("not in roster"),
            "no orphan section without a roster:\n{out}"
        );
        for h in ["work", "spare", "backup", "third"] {
            assert!(out.contains(h), "{h} still rendered in the main table");
        }
        let json = render_json(&orphan_report(None), None).unwrap();
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
        let out = render_text(&report, None);
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
        let out = render_charts(&orphan_report(Some(&empty)), 120, false, false, None);
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
        let out = render_text(&report, None);
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
        let socket = serde_json::to_string(&stats_wire(&report, None)).unwrap();
        let cli = render_json(&report, None).unwrap();
        let socket_v: serde_json::Value = serde_json::from_str(&socket).unwrap();
        let cli_v: serde_json::Value = serde_json::from_str(cli.trim_end()).unwrap();
        assert_eq!(
            socket_v, cli_v,
            "the stats socket wire must equal `stats --json` for the same window (R-2 parity)"
        );

        // R-2 holds on the DEGRADED path too (issue #642): the honesty flag is a `stats_wire`
        // parameter, not a socket-only key, so a socket-only field can never silently break
        // byte-equality with `stats --json` for the same config-load outcome.
        let detail = "malformed config: unknown field `session_trigger`";
        let socket_bad = serde_json::to_string(&stats_wire(&report, Some(detail))).unwrap();
        let cli_bad = render_json(&report, Some(detail)).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&socket_bad).unwrap(),
            serde_json::from_str::<serde_json::Value>(cli_bad.trim_end()).unwrap(),
            "R-2 parity must survive the #642 degraded path, not just the healthy one"
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

    // --- config-load fallback for the read-only stats view (issue #627) ----------------

    #[test]
    fn stats_config_surfaces_a_malformed_file_but_stays_silent_when_absent() {
        // #627: `stats` was the ONLY config consumer that silently rendered against DEFAULT
        // tunables when `config.toml` failed to parse — exit 0, zero stderr — while every sibling
        // command hard-failed and named the offending key. The fix keeps the two fallbacks apart.
        let dir = tempfile::tempdir().unwrap();

        // ABSENT config — the normal pre-`capture` state: fall back to defaults, NO warning (a
        // warning here would be noise on every fresh install, before anything has been captured).
        let missing = dir.path().join("config.toml");
        let (config, fault) = resolve_stats_config(Config::load_path(&missing));
        assert!(config.is_none(), "an absent config falls back to defaults");
        assert!(
            fault.is_none(),
            "an absent config is the silent pre-capture state, not a #627 surface"
        );

        // MALFORMED config — the exact #627 upgrade case: an old `session_trigger` key (renamed to
        // `session_ceiling` in #606) is now unknown (`deny_unknown_fields` → parse error). The
        // report still renders against defaults (config is `None`), and the warning carries the
        // SAME secret-free detail `config validate` surfaces — naming the offending key.
        let malformed = dir.path().join("stale.toml");
        std::fs::write(&malformed, b"[tunables]\nsession_trigger = 50\n").unwrap();
        let (config, fault) = resolve_stats_config(Config::load_path(&malformed));
        assert!(
            config.is_none(),
            "a malformed config falls back to defaults — the report still renders"
        );
        let fault = fault.expect("a malformed config must END the silence with a warning");
        assert_eq!(
            fault.log_detail,
            Config::load_path(&malformed).unwrap_err().to_string(),
            "the LOG detail is the SAME one every sibling command surfaces (issue #627)"
        );
        assert!(
            fault.log_detail.contains("session_trigger"),
            "and it names the offending key, like `config validate`: {}",
            fault.log_detail
        );
    }

    // --- the malformed config is SIGNALLED on the wire, not just logged (issue #642) ----

    #[test]
    fn a_healthy_config_omits_the_degraded_key_entirely() {
        // #642's additive contract: on the healthy path (and on the ABSENT-config path, which
        // legitimately uses defaults) the key is not merely `null` — it is ABSENT, so the payload
        // stays byte-identical to pre-#642 and a client that never heard of `config_unreadable`
        // decodes every prior field unchanged. This is what makes the change safe WITHOUT a
        // `schema` bump, per this wire's own "bumped only on a breaking change" rule.
        let report = report_fixture();
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&stats_wire(&report, None)).unwrap())
                .unwrap();
        assert!(
            v.as_object().unwrap().get("config_unreadable").is_none(),
            "a healthy config must leave NO trace on the wire (absent, not null): {v}"
        );
        assert_eq!(v["schema"], 1, "an additive field does not bump `schema`");
    }

    #[test]
    fn a_malformed_config_is_signalled_on_the_wire_not_only_in_the_daemon_log() {
        // THE #642 REGRESSION. Before this fix a socket client (the menubar Stats tab) receiving a
        // malformed-config reply saw a full, confident document computed against DEFAULT tunables
        // with zero indication anything was wrong — the daemon logged it, the client could not tell.
        // The wire now says so, so the panel can annotate the numbers instead of trusting them
        // (honesty family: issues #479 / #582 / #632).
        let dir = tempfile::tempdir().unwrap();
        // A config that EXISTS and fails to PARSE — deliberately not a missing file, which is the
        // ABSENT case `resolve_stats_config` routes to silent defaults with no fault at all.
        let bad = malformed_config(
            dir.path(),
            "stale.toml",
            "[tunables]\nsession_trigger = 50\n",
        );
        let (_, fault) = resolve_stats_config(Config::load_path(&bad));
        let fault = fault.expect("a malformed config must produce a fault");

        let report = report_fixture();
        let v: serde_json::Value = serde_json::from_str(
            &serde_json::to_string(&stats_wire(&report, Some(fault.wire_reason))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            v["config_unreadable"],
            serde_json::Value::from(fault.wire_reason),
            "the wire states the classified reason"
        );
        // Still a FULL document, never degraded to an `{"error":…}` envelope: the panel keeps its
        // best-effort series and annotates it, rather than losing the tab entirely.
        assert!(v.get("error").is_none(), "not an error envelope: {v}");
        assert!(v["summary"]["accounts"].is_object(), "series still served");
        assert_eq!(
            v["schema"], 1,
            "and still `schema:1` — the field is additive"
        );
    }

    /// A config file that EXISTS but fails to parse — the #627/#642 case, written into a temp dir.
    fn malformed_config(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn the_socket_reply_itself_carries_the_signal_end_to_end() {
        // THE PLUMBING TEST. Drives the WHOLE socket path — window → config resolve → report →
        // wire — against a controlled malformed config, not just the `stats_wire` builder in
        // isolation. Without it, severing the `config_unreadable` argument inside
        // `stats_socket_json_with` leaves every other test green: the very wire #642 exists to
        // create would be uncovered by construction.
        let dir = tempfile::tempdir().unwrap();
        let bad = malformed_config(
            dir.path(),
            "stale.toml",
            "[tunables]\nsession_trigger = 50\n",
        );
        let now = epoch("2026-07-08T00:00:00Z");
        let store = data(vec![sample(now - 2 * DAY_SECS, "work", 0.6, 0.3)], "");

        let reply =
            stats_socket_json_with(&store, now, 0, Some("week"), || Config::load_path(&bad));
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        let signalled = v["config_unreadable"]
            .as_str()
            .expect("the socket reply itself must carry the #642 signal");
        assert_eq!(
            signalled,
            wire_config_reason(&Config::load_path(&bad).unwrap_err()),
            "the socket carries the classified wire reason for this failure"
        );
        assert!(
            signalled.contains("config validate"),
            "which points at the command that prints the detail: {signalled}"
        );
        // The FULL detail is not lost — it goes to the operator-scoped log instead (issue #627).
        assert!(
            resolve_stats_config(Config::load_path(&bad))
                .1
                .unwrap()
                .log_detail
                .contains("session_trigger"),
            "the offending key still reaches the daemon log, just not the wire"
        );
        // Still a FULL document — the panel keeps its series and annotates it.
        assert_eq!(v["schema"], 1);
        assert_eq!(v["series"].as_array().unwrap().len(), 7);

        // And the ABSENT-config counterpart over the SAME path leaves no trace.
        let missing = dir.path().join("nope.toml");
        let clean =
            stats_socket_json_with(&store, now, 0, Some("week"), || Config::load_path(&missing));
        let cv: serde_json::Value = serde_json::from_str(&clean).unwrap();
        assert!(
            cv.as_object().unwrap().get("config_unreadable").is_none(),
            "an ABSENT config is the silent pre-capture state, not a #642 surface: {cv}"
        );
    }

    #[test]
    fn an_invalid_period_short_circuits_before_any_config_load() {
        // The lazy-loader contract: the period check must reject BEFORE config I/O, exactly as it
        // did when the load was inline. A closure that panics proves the load never ran.
        let store = data(vec![], "");
        assert_eq!(
            stats_socket_json_with(&store, 1_000_000, 0, Some("7d"), || {
                panic!("config must not be loaded for an invalid period")
            }),
            r#"{"error":"invalid period"}"#
        );
    }

    #[test]
    fn no_config_derived_byte_can_reach_the_wire_across_every_failure_class() {
        // THE #642 REDACTION INVARIANT, swept across the failure classes rather than asserted on
        // one input. Each case plants the SAME e-mail in a different place a config error is known
        // to echo it: the TOML span echo re-prints the whole offending line; serde's `invalid type`
        // quotes the VALUE; validation errors interpolate the offending value; and an
        // unknown KEY is echoed verbatim. That variety is exactly why the wire reason is a
        // `&'static str` rather than a scrubbed copy of the message — there is no sanitiser that
        // could be trusted to anticipate all of these, and more arrive with every dependency bump.
        let dir = tempfile::tempdir().unwrap();
        let planted = "alice@example.com";
        let cases = [
            // (name, config body) — every one MUST fail to load.
            ("span echo", format!("[[account]]\nlabel = \"{planted}\n")),
            (
                "quoted value / wrong type",
                format!("[tunables]\npoll_secs = \"{planted}\"\n"),
            ),
            // Reaches the `account_uuid` SHAPE rejection (issue #1052), which names the
            // offending value. It used to reach the duplicate-uuid rejection instead — hence
            // the two accounts this case originally carried — but shape is now checked first,
            // and an e-mail cannot pass its charset. That reordering also closed the
            // un-delimited variant of this class at the source: the duplicate error still
            // interpolates with no delimiter, but can now only ever echo a value already
            // constrained to `[A-Za-z0-9_-]{1,128}`, which no e-mail or newline can satisfy.
            (
                "validation interpolation",
                format!("[[account]]\nlabel = \"a\"\naccount_uuid = \"{planted}\"\n"),
            ),
            ("unknown key", format!("[tunables]\n\"{planted}\" = 1\n")),
            ("unknown table", format!("[\"{planted}\"]\nx = 1\n")),
        ];

        for (name, body) in &cases {
            let file = format!(
                "{}.toml",
                name.chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect::<String>()
            );
            let path = malformed_config(dir.path(), &file, body);
            let err = Config::load_path(&path)
                .err()
                .unwrap_or_else(|| panic!("{name}: precondition — this config must fail to load"));
            let (_, fault) = resolve_stats_config(Err(err));
            let fault = fault.unwrap_or_else(|| panic!("{name}: a malformed config must fault"));

            // The wire reason is what the panel and `stats --json` actually receive.
            assert_eq!(
                crate::redaction::meter::unauthored_emails(fault.wire_reason, &[]),
                Vec::<String>::new(),
                "{name}: an e-mail reached the wire: {}",
                fault.wire_reason
            );
            assert!(
                !fault.wire_reason.contains(planted),
                "{name}: config content reached the wire: {}",
                fault.wire_reason
            );
            // ...and it says enough to act on, rather than buying honesty with uselessness.
            assert!(
                fault.wire_reason.contains("config.toml")
                    && fault.wire_reason.contains("config validate"),
                "{name}: the wire reason must name the file and the command: {}",
                fault.wire_reason
            );
            assert!(
                !fault.wire_reason.contains('\n'),
                "{name}: one line — the panel has no scroll view"
            );
        }

        // Control: at least one case really DOES leak through the raw Display, so this test would
        // have caught the pre-fix behaviour rather than passing vacuously on a parser that never
        // echoed anything.
        let leaky = malformed_config(
            dir.path(),
            "control.toml",
            &format!("[[account]]\nlabel = \"{planted}\n"),
        );
        assert!(
            Config::load_path(&leaky)
                .unwrap_err()
                .to_string()
                .contains(planted),
            "control: the RAW error must echo the address, else the sweep proves nothing"
        );
    }

    #[test]
    fn the_wire_reason_is_classified_by_error_variant_not_by_message_text() {
        // Matching the VARIANT (not the rendered string) is what keeps the classification stable
        // when a dependency reformats its error text — and keeps the arms exhaustive by the type
        // system rather than by a chain of `contains` guesses.
        assert!(wire_config_reason(&Error::ConfigParse("x".into())).contains("not valid TOML"));
        assert!(wire_config_reason(&Error::ConfigInvalid("x".into())).contains("failed validation"));
        assert!(wire_config_reason(&Error::Io(std::io::Error::other("x")))
            .contains("could not be read"));
        // The cross-field variant is a validation failure, not a parse failure.
        assert!(
            wire_config_reason(&Error::ConfigTargetMaxSessionAboveTrigger {
                target_max_session_usage: 99,
                trigger: 50,
            })
            .contains("failed validation")
        );
    }

    #[test]
    fn the_cli_run_path_wires_the_signal_all_the_way_to_stdout() {
        // The CLI's OWN call site, not just the seam it calls. `run` reads the ambient config, so
        // `run_output` is the injected-load peer of `stats_socket_json_with` — without driving it,
        // severing `config_fault` inside `run` leaves the whole suite green and `stats --json` could
        // silently stop carrying the honesty signal.
        let dir = tempfile::tempdir().unwrap();
        let bad = malformed_config(
            dir.path(),
            "cli-run.toml",
            "[tunables]\nsession_trigger = 50\n",
        );
        let now = epoch("2026-07-08T00:00:00Z");
        let store = data(vec![sample(now - 2 * DAY_SECS, "work", 0.6, 0.3)], "");
        let json_args = || StatsArgs {
            accounts: Vec::new(),
            period: Some("week".to_owned()),
            since: None,
            json: true,
            no_color: true,
            ascii: true,
        };

        let out = run_output(json_args(), &store, now, 0, || Config::load_path(&bad)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["config_unreadable"],
            serde_json::Value::from(wire_config_reason(&Config::load_path(&bad).unwrap_err())),
            "`stats --json` must carry the reason the socket does — stdout is piped, stderr is not"
        );

        // The healthy/absent counterpart over the SAME path leaves no trace.
        let missing = dir.path().join("nope.toml");
        let clean =
            run_output(json_args(), &store, now, 0, || Config::load_path(&missing)).unwrap();
        let cv: serde_json::Value = serde_json::from_str(&clean).unwrap();
        assert!(
            cv.as_object().unwrap().get("config_unreadable").is_none(),
            "an absent config is the silent pre-capture state: {cv}"
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
        serde_json::to_string(&stats_wire(&wire_golden_report(), None))
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

    // --- issue #767: FULL-OUTPUT goldens for the `stats` human render ----------------
    //
    // The chart tests above assert PROPERTIES (a gap is a break, a label column aligns, a
    // narrow terminal sheds `trend` before `weekly`). Valuable, and structurally unable to see
    // the render as a whole: none would notice a duplicated block, a dropped section, or the
    // two chart blocks swapping places. These pin the ENTIRE output, byte for byte, across
    // every axis `stats` degrades on — terminal WIDTH (piped / wide / narrow / very narrow),
    // the ASCII glyph ramp vs the Unicode blocks, the COLOUR gate, and a degenerate roster
    // (empty / single / sparse).
    //
    // Determinism: `render_human` is pure over `(Report, TermEnv)` and the fixture's `window`
    // is a pair of FIXED epochs at `offset: 0`, so no wall clock reaches the render — the
    // `stats` surface has no `humanize_until`-style relative cell to pin a clock for (that is
    // `status`, whose goldens pin `GOLDEN_NOW` in `src/cli.rs`).
    //
    // PROVISIONAL BYTES — read before re-baselining. The `signal` column's class words
    // (`underused` / `balanced` / `saturated`) come from `SignalBand::label`, which is marked
    // PROVISIONAL pending a brand/framing review (issue #160), and the roster line's
    // `all-accounts-high: 0 episodes` is the reading issue #804 is open to change to the gap
    // sentinel `—`. Both are goldened deliberately: this issue pins CURRENT behaviour, and a
    // re-baseline is precisely the designed response when either lands. Neither is a settled
    // decision that these files should be read as ratifying.
    mod goldens {
        use super::*;
        use crate::render_golden::{self, Case};

        /// Comfortably wider than the full chart table — nothing priority-drops.
        const WIDE_COLS: usize = 120;
        /// Narrow enough to shed the lowest-priority columns, wide enough to keep some.
        const NARROW_COLS: usize = 52;
        /// Narrower than the `account · signal · session` floor itself.
        const VERY_NARROW_COLS: usize = 16;

        /// One account row, spelled out field by field — a golden fixture should read as its
        /// own complete, auditable input rather than lean on the chart helpers' defaults.
        fn acct(
            seen: u32,
            coverage: f64,
            session: crate::usage_stats::DimStats,
            weekly: crate::usage_stats::DimStats,
            cap_hits: u32,
            time_at_cap_secs: i64,
            share: f64,
        ) -> AccountStats {
            AccountStats {
                seen,
                expected: 12.0,
                coverage,
                session,
                weekly,
                cap_hits,
                time_at_cap_secs,
                contribution_share: share,
            }
        }

        /// A velocity overlay entry. Values are supplied ALREADY DERIVED (a rate and a
        /// runway in seconds), so no `session_ceiling` is pinned by these goldens — the
        /// config-derived ceiling never enters the rendered bytes. That is deliberate: the
        /// ceiling is a tunable each surface states for itself, and a golden that froze the
        /// default 95 would quietly turn a tunable into a constant.
        fn vel(
            session_rate: Option<f64>,
            weekly_rate: Option<f64>,
            session_runway_secs: Option<i64>,
            weekly_runway_secs: Option<i64>,
            weekly_headroom: Option<f64>,
        ) -> AccountVelocity {
            AccountVelocity {
                session_rate,
                weekly_rate,
                session_runway_secs,
                weekly_runway_secs,
                weekly_headroom,
            }
        }

        /// The canonical multi-account fixture. Deliberately heterogeneous — a golden over a
        /// uniform roster proves very little:
        ///
        /// - `alpha` is SATURATED (session peak 0.99), fully covered, with cap hits and a
        ///   known velocity + runway;
        /// - `beta` is BALANCED and only partially covered, with a known-but-flat weekly rate
        ///   (so it contributes head-room to the fleet aggregate with zero burn);
        /// - `ガンマ` carries a WIDE-GLYPH label and is UNDERUSED, so the goldens pin
        ///   display-width padding (UAX #11, issue #176) across every block;
        /// - `delta` has `seen == 0` — never observed — so its `signal`, `velocity` and
        ///   `runway` cells must render the gap sentinel `—` and NOT a fabricated `0`.
        ///
        /// The series carries interior GAPS (a bucket where an account has no reading), so the
        /// `trend` sparkline shows breaks rather than fabricated zeroes, and one ORPHAN handle
        /// exercises the #314 "not in roster" section.
        fn golden_report() -> Report {
            let d = |mean, peak, p95| crate::usage_stats::DimStats { mean, peak, p95 };
            let summary = [
                (
                    "alpha",
                    acct(
                        12,
                        1.0,
                        d(0.62, 0.99, 0.94),
                        d(0.40, 0.48, 0.46),
                        3,
                        900,
                        0.55,
                    ),
                ),
                (
                    "beta",
                    acct(
                        7,
                        0.58,
                        d(0.44, 0.66, 0.61),
                        d(0.20, 0.24, 0.23),
                        0,
                        0,
                        0.30,
                    ),
                ),
                (
                    "ガンマ",
                    acct(
                        9,
                        0.75,
                        d(0.08, 0.17, 0.15),
                        d(0.03, 0.05, 0.04),
                        0,
                        0,
                        0.15,
                    ),
                ),
                (
                    "delta",
                    acct(0, 0.0, d(0.0, 0.0, 0.0), d(0.0, 0.0, 0.0), 0, 0, 0.0),
                ),
            ];
            // Four hourly buckets. `beta` is absent from bucket 1 and `ガンマ` from bucket 2,
            // so two sparklines carry an INTERIOR break — the gap-is-not-zero invariant.
            let bucket = |accts: &[(&str, AccountStats)], start: i64| UsageReport {
                period: Period::new(start, start + HOUR_SECS),
                per_account: accts.iter().map(|(h, a)| ((*h).to_owned(), *a)).collect(),
                roster: RosterStats::default(),
            };
            let point = |mean: f64, peak: f64| {
                acct(1, 1.0, d(mean, peak, peak), d(0.0, 0.0, 0.0), 0, 0, 0.0)
            };
            Report {
                window: Window {
                    start: epoch("2026-06-30T12:00:00Z"),
                    end: epoch("2026-07-01T12:00:00Z"),
                    kind: WindowKind::Period(PeriodSpec::Day),
                },
                accounts: vec![],
                summary: UsageReport {
                    period: Period::new(
                        epoch("2026-06-30T12:00:00Z"),
                        epoch("2026-07-01T12:00:00Z"),
                    ),
                    per_account: summary.iter().map(|(h, a)| ((*h).to_owned(), *a)).collect(),
                    roster: RosterStats {
                        swap_count: 4,
                        swaps: crate::usage_stats::SwapBreakdown {
                            session: 2,
                            weekly: 1,
                            manual: 1,
                            forced: 0,
                            emergency: 0,
                        },
                        all_high_episodes: 0,
                        all_high_secs: 0,
                        // Set to the whole 24 h window so these goldens pin the census's
                        // MEASURED branch, exactly as the hand-set `all_high_*` above have
                        // always pinned a swap/episode count — this roster block is authored
                        // FOR THE RENDER and was never derived from the `per_account` beside
                        // it. (It is not self-consistent with it either: `delta` sits at
                        // `seen: 0`, so a real aggregation over this fixture's accounts would
                        // yield `covered == 0`. That is fine here and worth stating plainly —
                        // the aggregator's own behaviour is pinned by the `usage_stats` tests;
                        // what these goldens pin is that the RENDERER, handed a measured
                        // census, prints one.) The degenerate goldens below override this to
                        // `0` so the UNKNOWN branch is goldened too (issue #804).
                        all_high_covered_secs: 86_400,
                        high_threshold: 0.95,
                        // Capacity holds (issue #803) stay at the unmeasurable default for the
                        // same reason the note above gives: this block is authored FOR THE
                        // RENDER, so goldening a hand-set hold count would pin a fact no input
                        // supports. What the goldens below pin is that the renderer prints the
                        // census cell's UNKNOWN branch here.
                        ..RosterStats::default()
                    },
                },
                series: vec![
                    bucket(
                        &[
                            ("alpha", point(0.20, 0.28)),
                            ("beta", point(0.30, 0.35)),
                            ("ガンマ", point(0.05, 0.09)),
                        ],
                        0,
                    ),
                    bucket(
                        &[("alpha", point(0.55, 0.70)), ("ガンマ", point(0.10, 0.14))],
                        HOUR_SECS,
                    ), // beta: GAP
                    bucket(
                        &[("alpha", point(0.80, 0.92)), ("beta", point(0.50, 0.58))],
                        2 * HOUR_SECS,
                    ), // ガンマ: GAP
                    bucket(
                        &[
                            ("alpha", point(0.90, 0.99)),
                            ("beta", point(0.60, 0.66)),
                            ("ガンマ", point(0.12, 0.17)),
                        ],
                        3 * HOUR_SECS,
                    ),
                ],
                offset: 0,
                orphans: [(
                    "retired".to_owned(),
                    acct(4, 0.33, d(0.11, 0.19, 0.18), d(0.02, 0.03, 0.03), 0, 0, 0.0),
                )]
                .into_iter()
                .collect(),
                velocity: [
                    // A climbing account: both rates known, both runways finite.
                    (
                        "alpha".to_owned(),
                        vel(
                            Some(0.000_15),
                            Some(0.000_004),
                            Some(2 * 3_600),
                            Some(4 * 86_400),
                            Some(0.47),
                        ),
                    ),
                    // A KNOWN-flat weekly dimension: `0` burn is a reading, so it contributes
                    // head-room to the fleet aggregate but has no per-account weekly runway.
                    (
                        "beta".to_owned(),
                        vel(Some(0.000_05), Some(0.0), Some(9 * 3_600), None, Some(0.71)),
                    ),
                    // Unknown session rate → the `velocity` / `runway` cells degrade to `—`
                    // for this row while remaining populated for the others, so the goldens
                    // pin per-cell degradation as well as the whole-column elision below.
                    ("ガンマ".to_owned(), vel(None, None, None, None, None)),
                ]
                .into_iter()
                .collect(),
                // No expiry overlay (issue #883): the `expiry` column elides, so the twelve
                // goldens derived from this fixture stay byte-identical to their pre-#883 selves,
                // and are the only proof that elision holds. The one derived golden that DOES grow
                // the column is `stats-expiry-wide`, via [`report_with_expiry`] — a separate
                // fixture precisely so both directions are pinned (issue #886).
                expiry: BTreeMap::new(),
                // The CONFIGURED regime — every golden derived from this fixture pins the
                // un-annotated render, so the degraded one gets its own case (issue #836).
                census_over_roster: true,
            }
        }

        /// The same roster under the DEGRADED census regime (issue #836): no configured roster
        /// was known, so the census intersected whoever held samples and fires more readily.
        ///
        /// Goldened because the whole point of issue #836 is that the two regimes used to print
        /// IDENTICAL bytes over identical data — which is exactly the corruption a substring
        /// assertion cannot see and a whole-output golden can. Every other `stats` case pins
        /// the CONFIGURED regime, so this one is the other half of the pair: diff it against
        /// `stats-piped` / `stats-wide-unicode-plain` and the only delta is the annotation.
        fn fallback_census_report() -> Report {
            Report {
                census_over_roster: false,
                ..golden_report()
            }
        }

        /// The malformed-config reason the degraded goldens carry — the SAME static string
        /// [`wire_config_reason`] puts on the wire for issue #642, taken through that function
        /// rather than hand-written so a reworded reason moves the golden and is reviewed,
        /// instead of the golden silently pinning a string the wire no longer emits.
        fn fallback_census_reason() -> &'static str {
            wire_config_reason(&Error::ConfigParse("golden".into()))
        }

        /// The same roster whose pooled weekly burn is REAL but slight enough that the pooled
        /// head-room outlasts one weekly window — [`FleetRunwayState::BeyondWeeklyWindow`], the
        /// state issue #1028's plausibility bound introduced.
        ///
        /// Goldened because this is where the reported defect SURFACED: the old guard divided by
        /// the vanishing rate and printed `accounts last ~648427 days`. The unit tests assert the
        /// replacement string; only a whole-output golden pins how it SITS in the band — that the
        /// substantially longer clause neither wraps nor perturbs the blocks around it.
        ///
        /// `alpha` is the only burning account (`beta` is KNOWN-flat), so dropping its weekly rate
        /// to `1e-6` against the pooled head-room of `0.47 + 0.71` takes the quotient to ~13.7 d —
        /// past the window, well short of the absurd. Only `weekly_rate` moves (the per-account
        /// `runway` cell is the SESSION runway, so no row changes), which is why the diff against
        /// `stats-wide-unicode-plain` is exactly the fleet line.
        fn report_beyond_weekly_window() -> Report {
            let base = golden_report();
            let mut velocity = base.velocity.clone();
            let alpha = velocity.get_mut("alpha").expect("`alpha` is in the roster");
            alpha.weekly_rate = Some(0.000_001);
            Report { velocity, ..base }
        }

        /// The same roster with EVERY counted account KNOWN-flat — [`FleetRunwayState::Flat`].
        ///
        /// The other unknown the band can state, and the one that already existed before issue
        /// #1028 yet was never goldened: `render_summary` used to omit the line entirely, so the
        /// corpus pinned an ABSENCE that no case distinguished from "the fleet is fine". Zeroing
        /// `alpha`'s weekly rate leaves both counted accounts measured and not burning.
        ///
        /// (The requirements and design docs call that render site `fleet_line`; it has never been
        /// a Rust item, so grep `render_summary` when arriving from them.)
        fn report_flat_fleet() -> Report {
            let base = golden_report();
            let mut velocity = base.velocity.clone();
            let alpha = velocity.get_mut("alpha").expect("`alpha` is in the roster");
            alpha.weekly_rate = Some(0.0);
            Report { velocity, ..base }
        }

        /// The same roster with the velocity overlay ABSENT — a "sparse fleet". Every
        /// `velocity` and `runway` cell is then uniformly `—`, which fires the EMPTY-COLUMN
        /// ELISION pre-pass: both columns are dropped entirely rather than rendered as a wall
        /// of sentinels (design-stats.md §D-STA-5). This is the axis the populated fixture
        /// above cannot exercise.
        fn report_without_velocity() -> Report {
            Report {
                velocity: BTreeMap::new(),
                ..golden_report()
            }
        }

        /// The same roster WITH the REFRESH-token expiry overlay populated (issue #886) — the
        /// `expiry` column's other direction.
        ///
        /// [`golden_report`] leaves `Report::expiry` empty, which is the shape every PRODUCTION
        /// path still produces (`stats` reads a persisted series and never talks to the daemon, so
        /// the overlay has no producer until issue #917 folds the durable expiry events into it).
        /// That empty map is why the twelve cases above pin the column's ELISION. This one pins
        /// the POPULATED render, so the column that ships is goldened rather than only unit-tested
        /// — the layout facts a whole-output comparison sees (its RIGHT alignment among left-aligned
        /// text cells, its position in the row, the widths around it) are exactly the ones a
        /// `contains()` assertion is blind to.
        ///
        /// The four horizon states are spread across the roster deliberately, `delta` taking the
        /// UNKNOWN one so the gap sits beside three real deadlines rather than alone: an unmeasured
        /// credential must read as a pointed absence, never as the calm `Beyond` two rows up
        /// (issues #137/#876). `delta` is also the never-observed account, so its `—` lands in a
        /// row whose other cells are already gaps — the honest composite, and the row a reader is
        /// likeliest to skim past.
        ///
        /// Deadlines are offsets from the report window's END, which is the instant
        /// [`account_rows`] renders each cell against — so the humanized cells are fixed bytes and
        /// the golden does not move with the wall clock.
        fn report_with_expiry() -> Report {
            let base = golden_report();
            let now = base.window.end;
            let at = |offset: i64, state: crate::observability::ExpiryHorizon| AccountExpiry {
                expires_at: Some(now + offset),
                horizon_state: state,
                // The offline `stats` verb never talks to the daemon, so this column has no cohort
                // to carry — the fleet condition lives on `status` (issue #879).
                cohort_id: None,
            };
            let expiry = [
                // Inside the horizon: 2d10h out.
                (
                    "alpha".to_owned(),
                    at(
                        2 * 86_400 + 10 * 3_600,
                        crate::observability::ExpiryHorizon::Within,
                    ),
                ),
                // Beyond it: 29 days out, and the ONLY state that means "not expiring soon".
                (
                    "beta".to_owned(),
                    at(29 * 86_400, crate::observability::ExpiryHorizon::Beyond),
                ),
                // Already past — the bare state word, never a humanized negative remainder.
                (
                    "ガンマ".to_owned(),
                    at(-3 * 86_400, crate::observability::ExpiryHorizon::Lapsed),
                ),
                // POLLED, and the credential carried no deadline: UNKNOWN, which renders the gap.
                (
                    "delta".to_owned(),
                    AccountExpiry {
                        expires_at: None,
                        horizon_state: crate::observability::ExpiryHorizon::Unknown,
                        cohort_id: None,
                    },
                ),
            ]
            .into_iter()
            .collect();
            Report { expiry, ..base }
        }

        /// A report with no per-account usage at all — the degenerate roster.
        ///
        /// Nothing was observed, so nothing was jointly covered: this golden pins the
        /// all-accounts-high census rendering the gap sentinel `—` rather than the `0
        /// episodes (0s)` that used to read as a genuinely calm window (issue #804).
        fn empty_report() -> Report {
            let base = golden_report();
            Report {
                summary: UsageReport {
                    period: base.summary.period,
                    per_account: BTreeMap::new(),
                    roster: RosterStats {
                        all_high_covered_secs: 0,
                        ..base.summary.roster
                    },
                },
                series: Vec::new(),
                orphans: BTreeMap::new(),
                velocity: BTreeMap::new(),
                ..base
            }
        }

        /// A single-account report — the other degenerate shape, where every chart has exactly
        /// one row and the fleet aggregate has a cardinality of one.
        fn single_report() -> Report {
            let base = golden_report();
            let keep = |m: &BTreeMap<String, AccountStats>| {
                m.iter()
                    .filter(|(h, _)| h.as_str() == "alpha")
                    .map(|(h, a)| (h.clone(), *a))
                    .collect::<BTreeMap<_, _>>()
            };
            Report {
                summary: UsageReport {
                    period: base.summary.period,
                    per_account: keep(&base.summary.per_account),
                    roster: base.summary.roster,
                },
                series: base
                    .series
                    .iter()
                    .map(|b| UsageReport {
                        period: b.period,
                        per_account: keep(&b.per_account),
                        roster: b.roster,
                    })
                    .collect(),
                orphans: BTreeMap::new(),
                velocity: base
                    .velocity
                    .iter()
                    .filter(|(h, _)| h.as_str() == "alpha")
                    .map(|(h, v)| (h.clone(), *v))
                    .collect(),
                ..base
            }
        }

        /// A roster where EVERY account was never observed (`seen == 0`) — the third degenerate
        /// shape, and a distinct path from both `empty_report` (no accounts at all) and
        /// `sparse_report` (accounts observed, velocity overlay absent).
        ///
        /// It is the case that separates the two elision rules: the empty-column pre-pass drops
        /// only DROPPABLE columns, so `velocity` / `runway` vanish while the FLOOR column
        /// `signal` stays and renders a full column of the gap sentinel `—`. A keep-column is
        /// never elided even when every one of its cells is a gap — the roster is unmeasured,
        /// not absent, and the render must say so rather than quietly narrowing to look tidy.
        ///
        /// `trend` vanishes with them since issue #815. It is the golden that REPORTED that
        /// defect: `series` is empty here, so every trend cell is the empty string, and the
        /// pre-pass used to keep the column on the technicality that empty is not the sentinel —
        /// leaving a `trend` header with no cell under it at any row. The predicate now reads any
        /// cell with no visible mark as a gap, so this fixture pins the header WITHOUT it.
        ///
        /// The footer says so too: with no account observed, no instant had the whole roster
        /// covered, so the all-accounts-high census renders `—` rather than a fabricated `0
        /// episodes` (issue #804). It is the exact shape of the reported defect — a window in
        /// which the metric could see nothing reading as a calm one.
        fn all_unobserved_report() -> Report {
            let base = golden_report();
            let unobserved = |m: &BTreeMap<String, AccountStats>| {
                m.keys()
                    .map(|handle| {
                        (
                            handle.clone(),
                            AccountStats {
                                seen: 0,
                                expected: 12.0,
                                coverage: 0.0,
                                session: crate::usage_stats::DimStats {
                                    mean: 0.0,
                                    peak: 0.0,
                                    p95: 0.0,
                                },
                                weekly: crate::usage_stats::DimStats {
                                    mean: 0.0,
                                    peak: 0.0,
                                    p95: 0.0,
                                },
                                cap_hits: 0,
                                time_at_cap_secs: 0,
                                contribution_share: 0.0,
                            },
                        )
                    })
                    .collect::<BTreeMap<_, _>>()
            };
            Report {
                summary: UsageReport {
                    period: base.summary.period,
                    per_account: unobserved(&base.summary.per_account),
                    roster: RosterStats {
                        all_high_covered_secs: 0,
                        ..base.summary.roster
                    },
                },
                series: Vec::new(),
                orphans: BTreeMap::new(),
                velocity: BTreeMap::new(),
                ..base
            }
        }

        /// The terminal environments the matrix uses, named after the cases they produce so a
        /// call site says WHICH axis it is on.
        ///
        /// Named consts rather than a positional `env(cols, color, ascii)` helper: the COLOUR and
        /// ASCII cells are the two most important in the matrix and a positional form
        /// distinguishes them only by an argument order the reader cannot see. The repo's habit
        /// for `TermEnv` in tests is the named-field literal, as in
        /// `non_tty_falls_back_to_the_numeric_table_with_zero_ansi` above.
        const PIPED: TermEnv = TermEnv {
            cols: None,
            color: false,
            ascii: false,
        };
        const WIDE_UNICODE_PLAIN: TermEnv = TermEnv {
            cols: Some(WIDE_COLS),
            color: false,
            ascii: false,
        };
        const WIDE_UNICODE_COLOR: TermEnv = TermEnv {
            cols: Some(WIDE_COLS),
            color: true,
            ascii: false,
        };
        const WIDE_ASCII: TermEnv = TermEnv {
            cols: Some(WIDE_COLS),
            color: false,
            ascii: true,
        };
        const NARROW: TermEnv = TermEnv {
            cols: Some(NARROW_COLS),
            color: false,
            ascii: false,
        };
        const VERY_NARROW: TermEnv = TermEnv {
            cols: Some(VERY_NARROW_COLS),
            color: false,
            ascii: false,
        };

        /// Every goldened `stats` case, freshly rendered. The single source of truth for the
        /// case list: the comparison, the canary, and the emitter all consume THIS.
        fn cases() -> Vec<Case> {
            let full = golden_report();
            vec![
                // Piped / not a TTY: `cols` is `None`, so `render_human` takes the
                // `render_text` branch — the WIDER numeric column set, zero ANSI, no charts.
                Case::new("stats-piped", render_human(&full, PIPED, None)),
                // A wide TTY: the full chart surface, in each of the three glyph/colour modes.
                Case::new(
                    "stats-wide-unicode-plain",
                    render_human(&full, WIDE_UNICODE_PLAIN, None),
                ),
                Case::new(
                    "stats-wide-unicode-color",
                    render_human(&full, WIDE_UNICODE_COLOR, None),
                ),
                Case::new("stats-wide-ascii", render_human(&full, WIDE_ASCII, None)),
                // Narrow: the lowest-priority columns shed and the wide blocks shrink.
                Case::new("stats-narrow", render_human(&full, NARROW, None)),
                // Very narrow: only the floor remains, OVERFLOWING rather than wrapping.
                Case::new("stats-very-narrow", render_human(&full, VERY_NARROW, None)),
                // Sparse fleet: the empty-column elision self-drops `velocity` + `runway`.
                Case::new(
                    "stats-sparse-fleet",
                    render_human(&report_without_velocity(), WIDE_UNICODE_PLAIN, None),
                ),
                // The fleet runway's two STATED unknowns (issue #1028). Every case above pins the
                // KNOWN figure, so without these the corpus would never see either replacement
                // string — and the defect that motivated them lived on exactly this line.
                Case::new(
                    "stats-fleet-beyond-window",
                    render_human(&report_beyond_weekly_window(), WIDE_UNICODE_PLAIN, None),
                ),
                Case::new(
                    "stats-fleet-flat",
                    render_human(&report_flat_fleet(), WIDE_UNICODE_PLAIN, None),
                ),
                // Degenerate rosters.
                Case::new(
                    "stats-empty-roster",
                    render_human(&empty_report(), WIDE_UNICODE_PLAIN, None),
                ),
                Case::new(
                    "stats-single-account",
                    render_human(&single_report(), WIDE_UNICODE_PLAIN, None),
                ),
                Case::new(
                    "stats-all-na",
                    render_human(&all_unobserved_report(), WIDE_UNICODE_PLAIN, None),
                ),
                // The DEGRADED census regime (issue #836), on BOTH human surfaces — the charts
                // view had the same blind spot as the numeric one, so goldening only the piped
                // case would leave the TTY free to regress. Same report as `stats-piped` /
                // `stats-wide-unicode-plain` above, so the diff between the pairs is exactly
                // the annotation and nothing else.
                Case::new(
                    "stats-fallback-census-piped",
                    render_human(
                        &fallback_census_report(),
                        PIPED,
                        Some(fallback_census_reason()),
                    ),
                ),
                Case::new(
                    "stats-fallback-census-wide",
                    render_human(
                        &fallback_census_report(),
                        WIDE_UNICODE_PLAIN,
                        Some(fallback_census_reason()),
                    ),
                ),
                // The POPULATED `expiry` column (issue #886) — every case above elides it, so
                // this is the only golden in which the column ships. Rendered on the wide TTY,
                // where the whole column set fits and the cell's RIGHT alignment among the
                // left-aligned text cells is visible.
                Case::new(
                    "stats-expiry-wide",
                    render_human(&report_with_expiry(), WIDE_UNICODE_PLAIN, None),
                ),
            ]
        }

        /// The committed goldens, named by case. The macro derives each path from the name, so an
        /// entry cannot pair a case with someone else's bytes, and `include_str!` keeps every
        /// file a COMPILE-TIME input — a missing golden is a build error, not a silent skip.
        const GOLDENS: &[(&str, &str)] = render_golden::cli_render_goldens![
            "stats-piped",
            "stats-wide-unicode-plain",
            "stats-wide-unicode-color",
            "stats-wide-ascii",
            "stats-narrow",
            "stats-very-narrow",
            "stats-sparse-fleet",
            "stats-fleet-beyond-window",
            "stats-fleet-flat",
            "stats-empty-roster",
            "stats-single-account",
            "stats-all-na",
            "stats-fallback-census-piped",
            "stats-fallback-census-wide",
            "stats-expiry-wide",
        ];

        /// One-time emitter for the committed `stats` render goldens (issue #767).
        /// `#[ignore]` — NOT part of the suite. Run it ONLY alongside a DELIBERATE change to
        /// the `stats` render:
        ///   `cargo test -- --ignored emit_cli_render_goldens`
        /// then look at the regenerated files and record why in a `CLI-Goldens-Rebaselined:`
        /// commit trailer (CI requires it — `scripts/check-cli-golden-rebaseline.sh`).
        #[test]
        #[ignore = "one-time cli-render-golden emitter — run ONLY alongside a deliberate render change"]
        fn emit_cli_render_goldens_stats() {
            render_golden::emit(&cases());
        }

        #[test]
        fn the_committed_stats_goldens_still_match_the_render() {
            render_golden::assert_matches_goldens("stats", &cases(), GOLDENS);
        }

        /// CONSTRAINT-A: the gate can FAIL, demonstrated by MUTATION through the SAME
        /// predicate the assertion above uses — not by inspection.
        #[test]
        fn the_stats_golden_gate_rejects_a_corrupted_render() {
            render_golden::assert_canary("stats", &cases(), &[]);
        }

        /// The input-side half of the canary: a report whose readings actually changed must
        /// not match the unperturbed golden.
        #[test]
        fn a_perturbed_report_does_not_match_the_stats_golden() {
            let mut perturbed = golden_report();
            perturbed
                .summary
                .per_account
                .get_mut("alpha")
                .expect("`alpha` is in the golden roster")
                .session
                .peak = 0.98; // was 0.99
            render_golden::assert_perturbed_input_is_rejected(
                "stats",
                "stats-piped",
                &render_human(&golden_report(), PIPED, None),
                &render_human(&perturbed, PIPED, None),
            );
        }

        /// The expiry case must actually EXERCISE the column it was added for (issue #886) — and
        /// the twelve cases beside it must go on eliding it.
        ///
        /// Both halves matter and neither is visible to [`assert_matches_goldens`]. If
        /// `report_with_expiry` silently lost its overlay, `stats-expiry-wide` would re-emit as a
        /// copy of `stats-wide-unicode-plain`, match its own golden forever, and assert nothing
        /// about the column. If the elision rule broke the other way, twelve goldens would grow a
        /// wall of `—`. Stated as properties of the render, so both survive a re-baseline.
        #[test]
        fn the_expiry_case_renders_the_column_and_the_others_still_elide_it() {
            let with = render_human(&report_with_expiry(), WIDE_UNICODE_PLAIN, None);
            let without = render_human(&golden_report(), WIDE_UNICODE_PLAIN, None);
            assert!(
                with.contains("expiry"),
                "the populated overlay must materialize the column:\n{with}"
            );
            assert!(
                !without.contains("expiry"),
                "…and an empty overlay must still elide it, which is every OTHER stats \
                 golden's claim:\n{without}"
            );

            // All four states reach the render, so the case covers what it says it covers.
            let row = |handle: &str| {
                with.lines()
                    .find(|line| line.starts_with(handle))
                    .unwrap_or_else(|| panic!("`{handle}` is not in the render:\n{with}"))
            };
            assert!(row("alpha").contains("2d10h"), "{}", row("alpha")); // Within
            assert!(row("beta").contains("29d"), "{}", row("beta")); // Beyond
            assert!(row("ガンマ").contains("lapsed"), "{}", row("ガンマ")); // Lapsed

            // UNKNOWN — the gap, and not the calm `Beyond` cell two rows above it.
            //
            // COUNTED rather than `contains`, and that is the whole point of this pair: `delta`'s
            // row already carries gaps in `signal`, `runway` and `velocity`, so a bare
            // `delta.contains(EXPIRY_GAP)` is satisfied by three cells that have nothing to do with
            // expiry — it would stay green if the unmeasured cell started rendering `n/a`, or the
            // calm `29d`. Differencing against the SAME row in the elided render names the expiry
            // cell specifically without hard-coding a column position.
            let gaps = |line: &str| line.matches(EXPIRY_GAP).count();
            let delta = row("delta");
            let delta_elided = without
                .lines()
                .find(|line| line.starts_with("delta"))
                .expect("the elided render carries the same roster");
            assert_eq!(
                gaps(delta),
                gaps(delta_elided) + 1,
                "materializing EXPIRY must add exactly ONE gap to `delta`'s row — the one an \
                 unmeasured credential earns:\n{delta}\n{delta_elided}"
            );
            assert!(
                !delta.contains("29d"),
                "an unmeasured credential must never borrow the `Beyond` cell: {delta}"
            );

            // The `stats` surface leaves the cell UNCOLOURED — the tint is `status`' alone
            // (`expiry_severity`), and this render is the plain one, so no SGR may appear.
            assert!(
                !with.contains('\u{1b}'),
                "the plain stats render carries no escape sequences:\n{with}"
            );
        }

        /// The matrix cells must actually DIFFER along the axis they claim to exercise.
        ///
        /// Stated as properties of the render rather than pinned bytes, so it survives a
        /// re-baseline — and it is what stops a badly-chosen width or an inert axis from
        /// leaving a green golden that asserts nothing.
        #[test]
        fn each_stats_case_exercises_the_axis_it_claims() {
            let full = golden_report();
            let render = |term| render_human(&full, term, None);

            // WIDTH: each step must shed at least one more column than the last.
            let header_cols = |term| {
                header_line(&render(term))
                    .expect("the chart table has a header row")
                    .split_whitespace()
                    .count()
            };
            let (wide, narrow, very_narrow) = (
                header_cols(WIDE_UNICODE_PLAIN),
                header_cols(NARROW),
                header_cols(VERY_NARROW),
            );
            assert!(
                narrow < wide,
                "NARROW_COLS={NARROW_COLS} dropped no column (headers {narrow} vs {wide}) — the \
                 `stats-narrow` golden duplicates the wide one and proves nothing"
            );
            assert!(
                very_narrow < narrow,
                "VERY_NARROW_COLS={VERY_NARROW_COLS} shed nothing beyond NARROW_COLS (headers \
                 {very_narrow} vs {narrow})"
            );

            // ASCII: the Unicode blocks must actually be replaced, not merely re-laid-out.
            let unicode = render(WIDE_UNICODE_PLAIN);
            let ascii = render(WIDE_ASCII);
            assert_ne!(
                unicode, ascii,
                "the `--ascii` case renders identically to the Unicode one — the ramp axis is inert"
            );
            // The account labels stay Unicode (an operator's label is their own), so the
            // check is on the RAMP: no block glyph may survive `--ascii`.
            const BLOCKS: [char; 9] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█', '░'];
            assert!(
                unicode.contains(BLOCKS),
                "the Unicode case renders no block glyph, so the ASCII case has nothing to replace"
            );
            assert!(
                !ascii.contains(BLOCKS),
                "the `--ascii` render still contains a Unicode block glyph — the ramp did not \
                 fall back"
            );

            // COLOUR: augments only — stripping every SGR escape must yield the plain render.
            // Only true if padding is computed on display width BEFORE the colour wrap.
            let coloured = render(WIDE_UNICODE_COLOR);
            assert!(
                coloured.contains('\x1b'),
                "the coloured case carries no SGR escape, so it is not exercising the colour gate"
            );
            assert_eq!(
                render_golden::strip_ansi(&coloured)
                    .expect("the coloured render has escapes to strip"),
                unicode,
                "the coloured render does not reduce to the plain one — colour is changing the \
                 layout, not augmenting it (pad-before-colour is broken)"
            );

            // ELISION: the sparse fleet must actually LOSE the two columns the populated
            // fixture carries — otherwise `stats-sparse-fleet` is a second copy of the wide case.
            let sparse = render_human(&report_without_velocity(), WIDE_UNICODE_PLAIN, None);
            for column in ["velocity", "runway"] {
                assert!(
                    unicode.contains(column),
                    "the populated fixture does not render a `{column}` column, so the elision \
                     case has nothing to elide"
                );
                assert!(
                    !sparse.contains(column),
                    "`{column}` survived the empty-column elision on a fleet with no velocity \
                     overlay — the sparse case is not exercising §D-STA-5's elision pre-pass"
                );
            }

            // ALL-`n/a`: the third degenerate roster, and the case that separates the elision's
            // two halves — a DROPPABLE all-gap column goes, a FLOOR one stays and prints the
            // sentinel. Without this assertion the golden could not distinguish "the roster is
            // unmeasured" from "the roster is absent", which are different facts.
            let all_na = render_human(&all_unobserved_report(), WIDE_UNICODE_PLAIN, None);
            assert_ne!(
                all_na,
                render_human(&empty_report(), WIDE_UNICODE_PLAIN, None),
                "an all-unobserved roster renders identically to an EMPTY one — the render is \
                 conflating `we measured nothing about these accounts` with `there are no \
                 accounts`"
            );
            let signal_cells: Vec<&str> = table_rows(&all_na)
                .iter()
                .filter_map(|row| split_columns(row).get(1).copied())
                .collect();
            assert_eq!(
                signal_cells.len(),
                all_unobserved_report().summary.per_account.len(),
                "expected one `signal` cell per unobserved account"
            );
            assert!(
                signal_cells.iter().all(|cell| *cell == "—"),
                "the floor `signal` column does not read as all-gap on an all-unobserved \
                 roster: {signal_cells:?}"
            );
            assert!(
                all_na.contains("signal"),
                "the FLOOR column `signal` was elided when every one of its cells was a gap — a \
                 keep-column must never be elided, however empty"
            );

            // REGIME (issue #836): the fallback pair claims to differ from its configured twin
            // by EXACTLY the two annotations, over the same report — and each annotation has to
            // be asserted on its own. The pair is wired five lines from the correct call, so
            // half of it can be mis-wired to `golden_report()` / `None`; the render then still
            // differs from the twin by whichever annotation survived, and a whole-render
            // `assert_ne!` passes while the golden pins one annotation under the name of two.
            //
            // Read off the CASES rather than re-derived like the axes above: this cell's inputs
            // are two independent arguments (which report, which reason), so re-rendering them
            // here would assert the render and never the wiring — which is the half that can
            // actually be wrong.
            let all = cases();
            let caveat =
                config_regime_line(Some(fallback_census_reason())).expect("a reason, a caveat");
            for (name, twin_name) in [
                ("stats-fallback-census-piped", "stats-piped"),
                ("stats-fallback-census-wide", "stats-wide-unicode-plain"),
            ] {
                let fallback = render_golden::rendered(&all, name);
                let twin = render_golden::rendered(&all, twin_name);
                for annotation in [caveat.as_str(), ", sampled accounts"] {
                    assert!(
                        fallback.contains(annotation),
                        "`{name}` is missing `{annotation}` — its golden pins the un-annotated \
                         render under the regime's name"
                    );
                    assert!(
                        !twin.contains(annotation),
                        "`{twin_name}` already carries `{annotation}`, so the pair no longer \
                         isolates the regime"
                    );
                }
                assert_eq!(
                    fallback
                        .replace(&caveat, "")
                        .replace(", sampled accounts", ""),
                    twin,
                    "`{name}` differs from `{twin_name}` by more than the regime annotation — \
                     the golden is pinning an unrelated render change"
                );
            }
        }

        /// The goldens must AGREE with the property tests that already guard this surface —
        /// they are two views of one behaviour, and a golden that contradicted
        /// `narrow_terminal_drops_trend_then_weekly_keeping_session_never_wrapping` would be
        /// pinning a regression rather than the contract.
        #[test]
        fn the_width_goldens_agree_with_the_existing_degradation_contract() {
            let all = cases();
            let case = |name: &str| render_golden::rendered(&all, name);

            // The floor is never dropped, at any width.
            for name in ["stats-narrow", "stats-very-narrow"] {
                let text = case(name);
                for floor in ["account", "signal"] {
                    assert!(
                        text.contains(floor),
                        "`{name}` dropped the `{floor}` floor column, which is never droppable"
                    );
                }
            }
            // `trend` sheds before `weekly` (the catalog's priority order).
            let narrow = case("stats-narrow");
            assert!(
                !narrow.contains("trend"),
                "`trend` is the first-shed column but survived at {NARROW_COLS} columns"
            );
            // Never wrap: at ANY width the per-account table holds exactly one line per
            // account. Scoped to the TABLE block, because an account handle also leads each
            // chart-block row — counting handles across the whole render would conflate the
            // two and read a legitimate heatmap row as a wrap.
            for name in ["stats-narrow", "stats-very-narrow"] {
                let rows = table_rows(case(name));
                assert_eq!(
                    rows.len(),
                    golden_report().summary.per_account.len(),
                    "`{name}`'s table holds {} lines for {} accounts — it wrapped instead of \
                     overflowing:\n{}",
                    rows.len(),
                    golden_report().summary.per_account.len(),
                    rows.join("\n")
                );
                for handle in ["alpha", "beta", "delta", "ガンマ"] {
                    assert_eq!(
                        rows.iter()
                            .filter(|l| l.split_whitespace().next() == Some(handle))
                            .count(),
                        1,
                        "`{handle}` does not occupy exactly one table line in `{name}`"
                    );
                }
            }

            // …and the very-narrow floor OVERFLOWS its width rather than being truncated.
            let floor = table_rows(case("stats-very-narrow"));
            assert!(
                floor
                    .iter()
                    .any(|l| crate::cli::display_width(l) > VERY_NARROW_COLS),
                "nothing overflowed {VERY_NARROW_COLS} columns, so the very-narrow case does not \
                 pin the overflow-rather-than-wrap invariant"
            );
        }

        /// One table line's cells. Columns are delimited by a run of TWO OR MORE spaces (the
        /// inter-column gap plus padding); a single interior space belongs to a cell or a
        /// header label (`session m/p/p95`), so it must NOT split. Applied to the header and to
        /// a data row alike, this yields matching arity and lets a column be looked up by name.
        fn split_columns(line: &str) -> Vec<&str> {
            line.split("  ")
                .map(str::trim)
                .filter(|cell| !cell.is_empty())
                .collect()
        }

        /// The per-account table's header line — the single predicate the row scanner, the column
        /// counter and the sentinel check all use, so they can never disagree about where the
        /// table starts. Anchored on `account ` WITH its trailing space: a bare `contains`
        /// would also match a data row for an account whose handle contains "account".
        fn header_line(rendered: &str) -> Option<&str> {
            rendered
                .lines()
                .find(|l| l.trim_start().starts_with("account "))
        }

        /// The per-account table's DATA rows (header and blank line excluded) from a rendered
        /// view — the block between the `account …` header and the following blank line.
        fn table_rows(rendered: &str) -> Vec<&str> {
            rendered
                .lines()
                .skip_while(|l| Some(*l) != header_line(rendered))
                .skip(1)
                .take_while(|l| !l.trim().is_empty())
                .collect()
        }

        /// The gap sentinel is semantic, not cosmetic: for an account that was never observed
        /// (`seen == 0`) the OBSERVED-ONLY columns — `signal`, `velocity`, `runway` — render
        /// `—`, never a fabricated reading. An unmeasurable period is not a calm one.
        ///
        /// Asserted per named column (not "the row contains a `—` somewhere"), so a
        /// re-baseline cannot quietly turn one of them into a zero while another still carries
        /// the sentinel and keeps a loose check green.
        #[test]
        fn an_unobserved_account_renders_the_gap_sentinel_in_every_observed_only_column() {
            let all = cases();
            let piped = render_golden::rendered(&all, "stats-piped");
            let header = header_line(piped).expect("the numeric table has a header row");
            let row = table_rows(piped)
                .into_iter()
                .find(|l| l.split_whitespace().next() == Some("delta"))
                .expect("the never-observed account has a table row");

            // Header and data row must yield the same arity for the per-column lookup below to
            // mean anything — see `split_columns` for why the split is on 2+-space runs.
            let headers = split_columns(header);
            let cells = split_columns(row);
            assert_eq!(
                headers.len(),
                cells.len(),
                "header/cell arity disagree, so the per-column lookup below would be wrong:\n\
                 {header}\n{row}"
            );
            for column in ["signal", "velocity", "runway"] {
                let at = headers
                    .iter()
                    .position(|h| *h == column)
                    .unwrap_or_else(|| panic!("the piped table renders a `{column}` column"));
                assert_eq!(
                    cells[at], "—",
                    "the never-observed account's `{column}` cell is `{}`, not the gap sentinel \
                     — a fabricated reading where a gap belongs:\n{row}",
                    cells[at]
                );
            }
        }
    }
}
