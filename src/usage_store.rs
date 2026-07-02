// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The local usage-sample datastore (issue #155).
//!
//! A daemon-written, tool-read store of per-account usage readings, backed by two
//! files in the native support dir (alongside `config`/`log`/`socket` — see
//! [`crate::paths::usage_samples`] / [`crate::paths::usage_rollup`]):
//!
//! ```text
//! usage-samples.jsonl : the raw rolling window — one JSON [`Sample`] per line,
//!                       append-only (the daemon writes one line per poll).
//! usage-rollup.json   : hourly + daily aggregates + a `rolled_through_ts`
//!                       watermark, a single atomically-rewritten [`Rollup`].
//! ```
//!
//! **No database, no new dependency**: the container is `serde_json` and the
//! durability comes from the crate's existing atomic-write path
//! ([`crate::paths::write_private_file`] / [`crate::paths::write_preserving_mode`]
//! — same-dir tmp + `fsync` + `rename(2)`, never `/tmp`).
//!
//! # Three bounded retention tiers
//!
//! A monitoring store must not grow without bound, yet "lifetime" trend must
//! survive. So samples age through three tiers of decreasing resolution and
//! increasing horizon, each bounded:
//!
//! | Tier   | Resolution          | Horizon        | Aggregates                          |
//! |--------|---------------------|----------------|-------------------------------------|
//! | raw    | every poll          | ~14 d (config) | the [`Sample`]s verbatim            |
//! | hourly | one bucket per hour | ~90 d          | max / mean / count                  |
//! | daily  | one bucket per day  | lifetime       | max / mean / p95 / cap-hits / coverage |
//!
//! [`compact_and_roll`] moves samples down the tiers: it bounds the raw window,
//! folds aged samples into the hourly + daily buckets, and prunes the hourly tier.
//! A **"lifetime" reader reads the daily tier only** — it is the sole tier kept
//! without a horizon.
//!
//! # Both quota dimensions, never one worst-case scalar
//!
//! Every tier keeps `session` and `weekly` separately, mirroring the swap
//! decision's own discipline (issue #41 / [`crate::usage::Usage`]): the store
//! projects neither window to a single blended number, because the two limits are
//! independent and a reader may care about either.
//!
//! # Roll-once-per-whole-day, so the aggregates are exact
//!
//! A sample is rolled only once its **entire day** has aged out of the raw window,
//! so a day bucket is always built from that day's complete sample set in a single
//! batch. That keeps max / mean / **p95** / cap-hits / coverage exact — there is no
//! lossy re-merge of already-summarised aggregates across compaction runs (p95, in
//! particular, cannot be recovered from a summary). The `rolled_through_ts`
//! watermark guarantees every sample is folded at most once.
//!
//! # Redaction discipline (issue #15)
//!
//! The store carries **no secret**: a [`Sample`] holds percentages, epoch
//! timestamps, a provider tag, an optional severity label and an optional spend
//! estimate, plus `acct` — the account's existing **redacted handle** (the
//! operator's non-secret label), never an email or token. Every field is therefore
//! safe to persist and safe to `Debug`, unlike the credential-bearing types
//! ([`crate::keychain`] / [`crate::claude_state`]) that deliberately omit `Debug`.
//!
//! # Not-yet-wired seam
//!
//! This module owns the store's data model and file operations only. The daemon's
//! per-poll collector (issue #156) will call [`append_sample`] + [`compact_and_roll`],
//! and the read-only reporting tools (issue #157) will call [`read_samples`] /
//! [`read_rollup`]. Until then every item here is unused by the binary itself
//! (main.rs only declares the module), exactly as main.rs frames every subsystem —
//! hence the module-level `dead_code` allowance, mirroring [`crate::migration`].

// See the "Not-yet-wired seam" note above: #156/#157 wire the read/write callers.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths;

/// Seconds in an hour — the hourly-bucket alignment unit.
const HOUR_SECS: i64 = 3_600;
/// Seconds in a day — the daily-bucket alignment unit (UTC-midnight aligned, since
/// epoch 0 is itself UTC midnight).
const DAY_SECS: i64 = 86_400;
/// Default raw-window horizon (~14 d). The daemon (issue #156) overrides this from
/// `config.toml`; the store carries a sane default so it is self-contained.
const DEFAULT_RAW_WINDOW_SECS: i64 = 14 * DAY_SECS;
/// Default hourly-tier horizon (~90 d).
const DEFAULT_HOURLY_WINDOW_SECS: i64 = 90 * DAY_SECS;
/// Default poll cadence (seconds) — the denominator for a bucket's coverage
/// (observed samples ÷ expected samples). Matches `config`'s `DEFAULT_POLL_SECS`.
const DEFAULT_POLL_INTERVAL_SECS: i64 = 300;
/// A reading at or above this utilisation fraction has hit the quota cap — the
/// `cap_hits` tally counts these per day, per dimension. Usage fractions are
/// `consumed / limit`, so `1.0` is exactly the cap and readings can exceed it.
const CAP_FRACTION: f64 = 1.0;
/// The percentile the daily tier records (95th).
const P95: f64 = 0.95;

/// The bounded wait for the single-writer store lock (issue #188) before failing
/// closed. Generous versus the sub-millisecond critical sections it guards; a
/// contended acquire that ever exhausts it signals a genuinely stuck store, so the
/// caller (both producers are fail-open) skips this one write rather than stall the
/// poll loop. Kept short because the store is best-effort telemetry — a dropped
/// sample beats a stalled rotation loop.
const STORE_LOCK_MAX_WAIT: Duration = Duration::from_secs(2);
/// Poll interval while waiting on a contended store lock (issue #188) — short enough
/// that the wait ends within ~one interval of the holder releasing, negligible over
/// the few polls a typical sub-millisecond critical section needs.
const STORE_LOCK_RETRY: Duration = Duration::from_millis(20);

/// One point-in-time usage reading for one account, as persisted to the raw tier.
///
/// Provider-tagged and redaction-clean: `acct` is the account's existing redacted
/// handle (never an email/token), and every other field is a non-secret percentage,
/// timestamp or label — so `Debug` is safe here (contrast the credential-bearing
/// types that omit it). The four optional fields are omitted from the JSON entirely
/// when absent, keeping each line minimal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Sample {
    /// When the reading was taken, as whole UTC epoch seconds.
    pub(crate) ts: i64,
    /// The quota provider this reading came from (e.g. `"claude"`) — a tag, so a
    /// future multi-provider store keeps readings distinguishable.
    pub(crate) provider: String,
    /// The account's **redacted handle** (the operator's non-secret label). NEVER
    /// an email or token — the store's redaction invariant (issue #15).
    pub(crate) acct: String,
    /// Fraction in `[0.0, …]` of the rolling 5-hour session window consumed
    /// (`1.0` = exhausted; readings can exceed it).
    pub(crate) session: f64,
    /// Fraction in `[0.0, …]` of the weekly window consumed.
    pub(crate) weekly: f64,
    /// Epoch seconds at which the SESSION window resets, when the poll knew it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session_resets_at: Option<i64>,
    /// Epoch seconds at which the WEEKLY window resets, when the poll knew it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) weekly_resets_at: Option<i64>,
    /// The provider-reported severity label for the reading (e.g. `"critical"`),
    /// when present. A tolerant free string, not an enum, so an unrecognised future
    /// value is stored, not a parse failure (the [`crate::migration`] precedent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) severity: Option<String>,
    /// An optional coarse spend estimate for the reading — a forward slot (no
    /// producer yet). Approximate by design; never accounting-grade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) spend: Option<f64>,
}

impl Sample {
    /// A reading with the required core fields and no optionals. Chain
    /// [`with_resets`](Sample::with_resets) / [`with_severity`](Sample::with_severity)
    /// / [`with_spend`](Sample::with_spend) to add the optional ones.
    pub(crate) fn new(
        ts: i64,
        provider: impl Into<String>,
        acct: impl Into<String>,
        session: f64,
        weekly: f64,
    ) -> Self {
        Self {
            ts,
            provider: provider.into(),
            acct: acct.into(),
            session,
            weekly,
            session_resets_at: None,
            weekly_resets_at: None,
            severity: None,
            spend: None,
        }
    }

    /// Attach the two window reset timestamps (each optional).
    pub(crate) fn with_resets(
        mut self,
        session_resets_at: Option<i64>,
        weekly_resets_at: Option<i64>,
    ) -> Self {
        self.session_resets_at = session_resets_at;
        self.weekly_resets_at = weekly_resets_at;
        self
    }

    /// Attach the provider-reported severity label.
    pub(crate) fn with_severity(mut self, severity: Option<String>) -> Self {
        self.severity = severity;
        self
    }

    /// Attach the coarse spend estimate.
    pub(crate) fn with_spend(mut self, spend: Option<f64>) -> Self {
        self.spend = spend;
        self
    }
}

/// The rolled-aggregate object — one atomically-rewritten JSON document holding the
/// hourly and daily tiers plus the roll watermark.
///
/// `rolled_through_ts` is the newest sample epoch already folded into the tiers: a
/// sample with `ts <= rolled_through_ts` has been rolled and must never be folded
/// again (the exactly-once guarantee). Non-secret throughout, so `Debug` is safe.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Rollup {
    /// The newest sample epoch already folded into the tiers (0 before any roll).
    pub(crate) rolled_through_ts: i64,
    /// The hourly tier — one bucket per hour, bounded to ~90 d, sorted by
    /// `hour_start`.
    pub(crate) hourly: Vec<HourBucket>,
    /// The daily tier — one bucket per day, kept for the store's lifetime, sorted
    /// by `day_start`.
    pub(crate) daily: Vec<DayBucket>,
}

/// One hour's aggregate: max / mean / count per dimension. The mid-resolution tier —
/// enough to chart a day's shape, cheap enough to keep ~90 d of them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct HourBucket {
    /// Epoch seconds at the top of the hour (UTC), the bucket key.
    pub(crate) hour_start: i64,
    /// How many samples fell in this hour.
    pub(crate) count: u32,
    /// Session-dimension max + mean over the hour.
    pub(crate) session: HourStat,
    /// Weekly-dimension max + mean over the hour.
    pub(crate) weekly: HourStat,
}

/// The hourly tier's per-dimension summary: peak and mean utilisation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct HourStat {
    /// Peak utilisation fraction over the bucket.
    pub(crate) max: f64,
    /// Mean utilisation fraction over the bucket.
    pub(crate) mean: f64,
}

/// One day's aggregate: max / mean / p95 / cap-hits per dimension, plus a coverage
/// ratio. The lowest-resolution tier — kept for the store's lifetime, so it is what
/// a "lifetime" reader consults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DayBucket {
    /// Epoch seconds at UTC midnight of the day, the bucket key.
    pub(crate) day_start: i64,
    /// How many samples fell in this day.
    pub(crate) count: u32,
    /// Observed samples ÷ expected samples for the day (clamped to `1.0`) — how
    /// complete the day's data is, so a sparsely-polled day is not misread as calm.
    pub(crate) coverage: f64,
    /// Session-dimension summary over the day.
    pub(crate) session: DayStat,
    /// Weekly-dimension summary over the day.
    pub(crate) weekly: DayStat,
}

/// The daily tier's per-dimension summary: peak, mean, 95th percentile and the
/// count of cap-hit samples.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DayStat {
    /// Peak utilisation fraction over the day.
    pub(crate) max: f64,
    /// Mean utilisation fraction over the day.
    pub(crate) mean: f64,
    /// 95th-percentile utilisation fraction over the day (nearest-rank).
    pub(crate) p95: f64,
    /// Samples at or above the quota cap ([`CAP_FRACTION`]) over the day.
    pub(crate) cap_hits: u32,
}

/// The bounds that govern [`compact_and_roll`]. Defaults are self-contained; the
/// daemon (issue #156) constructs one from `config.toml`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RetentionPolicy {
    /// Raw samples whose whole day is older than `now - raw_window_secs` are rolled
    /// out of the raw tier.
    pub(crate) raw_window_secs: i64,
    /// Hourly buckets older than `now - hourly_window_secs` are pruned.
    pub(crate) hourly_window_secs: i64,
    /// Daily buckets older than `now - daily_window_secs` are pruned, or — when this is
    /// **`0` (the default, "lifetime")** — the daily tier is never pruned, kept for the
    /// store's whole lifetime (issue #161). A summarised day cannot be rebuilt once its raw
    /// samples age out, so lifetime is the default; a non-zero value opts INTO bounding it.
    pub(crate) daily_window_secs: i64,
    /// The expected poll cadence, the denominator for daily coverage.
    pub(crate) poll_interval_secs: i64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            raw_window_secs: DEFAULT_RAW_WINDOW_SECS,
            hourly_window_secs: DEFAULT_HOURLY_WINDOW_SECS,
            // 0 = lifetime: the daily tier is kept for the store's lifetime by default.
            daily_window_secs: 0,
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
        }
    }
}

/// What a [`compact_and_roll`] pass moved (issue #161) — the non-secret summary a caller
/// (the daemon collector) turns into a redacted `usage_rollup` event. `raw_lines` is how
/// many raw samples were folded down into the aggregates this pass (`0` = nothing aged out,
/// so no event is emitted); `rolled_through_ts` is the store's roll watermark AFTER the pass
/// (the newest sample epoch now folded). Both are plain integers — never a handle or token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RollSummary {
    /// The roll watermark after this pass — the newest sample epoch folded into the tiers.
    pub(crate) rolled_through_ts: i64,
    /// How many raw samples this pass folded down into the hourly/daily aggregates.
    pub(crate) raw_lines: u32,
}

/// The store's advisory-lock path — the `usage.lock` sibling of the raw-sample file.
///
/// [`append_sample`] and [`compact_and_roll`] both derive it from the SAME
/// `samples_path`, so they contend on one stable lock file and serialize (issue
/// #188). A dedicated lock file — never the sample file itself — is required because
/// [`compact_and_roll`] REPLACES the sample file via `rename`: a lock taken on that
/// inode would not carry across the swap, so a fresh appender opening the new file
/// by path would not contend with it. In production `samples_path` is
/// `<support_dir>/usage-samples.jsonl`, so the lock lands at
/// `<support_dir>/usage.lock` — native-local alongside `swap.lock` / `daemon.lock`.
fn lock_path(samples_path: &Path) -> PathBuf {
    samples_path.with_file_name("usage.lock")
}

/// A held single-writer store lock: a kernel advisory `flock(LOCK_EX)` on the
/// store's [`lock_path`], held only for the DURATION of one store write (issue
/// #188). The file is held open for the critical section; the kernel releases the
/// lock on drop (or process death), so there is no stale-lock reaping.
///
/// It ENFORCES the single-writer invariant the store's read-tolerance and
/// atomic-rewrite behaviour rest on: [`append_sample`] and [`compact_and_roll`] both
/// acquire it, so an append can never interleave `compact_and_roll`'s
/// read-modify-rewrite of the raw file (which would silently drop the appended
/// line). Mirrors the crate's per-swap [`crate::swap`] lock, but SYNCHRONOUS — the
/// store's writes are blocking (they `fsync`), so a blocking acquire fits the same
/// call context.
#[derive(Debug)]
struct StoreLock {
    // Held open purely to keep the lock; dropping it (or the process dying)
    // releases it.
    _file: std::fs::File,
}

impl StoreLock {
    /// Acquire the store lock at `path` (creating the file `0600` if needed),
    /// bounded-blocking up to `max_wait`.
    ///
    /// FAIL-CLOSED: if the lock cannot be taken within `max_wait` — another store
    /// write held it the whole time — returns [`Error::UsageStoreBusy`] so the caller
    /// aborts with ZERO writes rather than racing a torn rewrite. Polls
    /// `flock(LOCK_EX|LOCK_NB)`: the uncontended case (the only one on the
    /// single-daemon poll loop, which calls the two writers sequentially) succeeds on
    /// the first try with no sleep; a genuine contender waits [`STORE_LOCK_RETRY`]
    /// between tries. `EINTR` (a signal during the wait) retries immediately.
    fn acquire(path: &Path, max_wait: Duration) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;

        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        let deadline = Instant::now() + max_wait;
        loop {
            // SAFETY: `flock` takes a valid open fd (owned by `file`, which outlives
            // the call) and the two flag constants; it has no other preconditions.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(Self { _file: file });
            }
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                // EWOULDBLOCK (== EAGAIN): another store write holds it — wait, retry.
                Some(libc::EWOULDBLOCK) => {}
                // Interrupted by a signal — not contention; retry immediately.
                Some(libc::EINTR) => continue,
                // A genuine I/O failure (broken fd / filesystem), surfaced as itself
                // rather than masqueraded as contention.
                _ => return Err(Error::Io(err)),
            }
            // Out of patience: fail closed (the caller aborts with ZERO writes).
            if Instant::now() >= deadline {
                return Err(Error::UsageStoreBusy);
            }
            std::thread::sleep(STORE_LOCK_RETRY);
        }
    }
}

/// Append one `sample` to the raw JSONL file as a single line.
///
/// One `write_all` of `<json>\n` to the `0600` append-open file
/// ([`crate::paths::create_private_file`]) — deliberately un-`fsync`ed: the raw tier
/// is best-effort (the durable checkpoint is the rollup), and a crash mid-append
/// leaves a torn trailing line that [`read_samples`] tolerates.
///
/// Serialized against [`compact_and_roll`]'s atomic rewrite by the store lock
/// ([`StoreLock`], issue #188): an append can never interleave that
/// read-modify-rewrite, so the single-writer invariant is ENFORCED, not merely
/// assumed. A contended lock fails closed ([`Error::UsageStoreBusy`]); the daemon's
/// collector is fail-open, so it logs and skips the sample rather than risk a torn
/// write.
pub(crate) fn append_sample(samples_path: &Path, sample: &Sample) -> Result<()> {
    let _guard = StoreLock::acquire(&lock_path(samples_path), STORE_LOCK_MAX_WAIT)?;
    let mut line = serde_json::to_vec(sample).map_err(|_| serialize_err())?;
    line.push(b'\n');
    let mut file = paths::create_private_file(samples_path)?;
    file.write_all(&line)?;
    Ok(())
}

/// Read every well-formed [`Sample`] from the raw JSONL file, tolerating a torn
/// trailing line.
///
/// An absent file reads as no samples. Each newline-delimited record is parsed on
/// its raw bytes (so a torn multi-byte UTF-8 tail cannot poison the whole read);
/// any record that fails to parse is skipped. Under the single-writer append model
/// (now ENFORCED by the store lock — [`StoreLock`], issue #188 — so only one writer
/// runs at a time, and [`compact_and_roll`] rewrites atomically) the sole reachable
/// parse failure is a crash-torn trailing line, exactly the AC's case.
pub(crate) fn read_samples(samples_path: &Path) -> Result<Vec<Sample>> {
    let bytes = match std::fs::read(samples_path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(Error::Io(err)),
    };
    let mut samples = Vec::new();
    for record in bytes.split(|&b| b == b'\n') {
        if record.is_empty() {
            continue;
        }
        // A torn/partial trailing record fails to parse and is skipped; complete
        // records (guaranteed by the atomic rewrite path) always parse.
        if let Ok(sample) = serde_json::from_slice::<Sample>(record) {
            samples.push(sample);
        }
    }
    Ok(samples)
}

/// Read the rolled-aggregate object; an absent file reads as an empty [`Rollup`].
///
/// A present-but-unparseable rollup is a hard error ([`Error::UsageRollupMalformed`],
/// redacted to a line/column) rather than a silent reset — the daily tier is
/// lifetime state that cannot be rebuilt once the raw samples behind it have aged
/// out, so losing it must never be quiet.
pub(crate) fn read_rollup(rollup_path: &Path) -> Result<Rollup> {
    let bytes = match std::fs::read(rollup_path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Rollup::default()),
        Err(err) => return Err(Error::Io(err)),
    };
    serde_json::from_slice(&bytes).map_err(|err| Error::UsageRollupMalformed {
        line: err.line(),
        column: err.column(),
    })
}

/// Bound the raw window and fold aged samples into the hourly + daily tiers.
///
/// A single-writer operation (the daemon, issue #156) run periodically, serialized
/// against [`append_sample`] by the store lock ([`StoreLock`], issue #188) so the two
/// writers can never interleave a torn rewrite. Under that lock it:
///
/// 1. reads the raw samples and the current rollup;
/// 2. rolls every sample whose **whole day** has aged past `now - raw_window` and
///    is newer than the watermark — grouped by day (and, within a day, by hour) so
///    each bucket is built from that day's complete set in one exact batch;
/// 3. advances `rolled_through_ts` past the rolled batch and prunes the hourly tier
///    to `hourly_window` (the daily tier is lifetime);
/// 4. persists the rollup **atomically** — the durable checkpoint, written FIRST;
/// 5. rewrites the raw file **atomically** to just the still-in-window samples,
///    AFTER the rollup.
///
/// **Crash-safe ordering (issue #188)**: the durable rollup is persisted BEFORE the
/// raw source is truncated, so a crash between the two writes leaves every aged
/// sample still in the raw file for a re-roll — the `rolled_through_ts` watermark
/// then de-dupes it on the next pass — rather than absent from BOTH tiers (the old
/// raw-first order lost it, permanently for the lifetime daily tier). This is the
/// same invariant [`crate::capture`] applies as "stash BEFORE persisting the roster",
/// inverted here. Each file's write is itself atomic (tmp + `fsync` + `rename`), so a
/// torn write of either file degrades to old-or-new, never partial. "Crash-safe" here
/// covers a process crash / interruption / failed write (the AC's fault boundaries),
/// where a returned write is visible to the next process; there is no WAL (a non-goal),
/// so ordering under hard power loss is bounded by the atomic-write primitive rather
/// than a journal.
///
/// Idempotent: a second call with the same `now` re-rolls nothing (the watermark
/// excludes already-folded samples, and the aged ones are gone from the raw file).
///
/// Returns a [`RollSummary`] naming what this pass folded (issue #161): `raw_lines` = how
/// many raw samples were rolled down (`0` when nothing aged out), and the post-pass roll
/// watermark. The daemon collector turns a non-empty summary into a redacted `usage_rollup`
/// event; the value is non-secret (two integers), so returning it never widens the redaction
/// surface.
pub(crate) fn compact_and_roll(
    samples_path: &Path,
    rollup_path: &Path,
    now: i64,
    policy: &RetentionPolicy,
) -> Result<RollSummary> {
    // Hold the single-writer lock across the WHOLE read-modify-write (issue #188):
    // acquired BEFORE the reads so a concurrent `append_sample` cannot slip a line in
    // between our read and our atomic rewrite (which would silently drop that line).
    let _guard = StoreLock::acquire(&lock_path(samples_path), STORE_LOCK_MAX_WAIT)?;

    let samples = read_samples(samples_path)?;
    let rollup = read_rollup(rollup_path)?;
    let (retained, rollup, summary) = plan_roll(samples, rollup, now, policy);

    // Persist the durable rollup checkpoint BEFORE truncating the raw source (issue
    // #188). A crash between these two atomic writes then leaves every aged sample
    // still in raw for a re-roll — the watermark de-dupes it — never absent from both
    // tiers. The reverse order (the pre-#188 bug) lost the aged batch from both.
    write_rollup(rollup_path, &rollup)?;
    write_samples(samples_path, &retained)?;
    Ok(summary)
}

/// The pure compute half of [`compact_and_roll`] — no I/O, so the persist ORDERING
/// can be exercised under a fault injected at each write boundary (issue #188).
///
/// Partitions `samples` into the still-in-window `retained` set and the aged,
/// not-yet-folded batch; folds that batch into `rollup`'s hourly + daily tiers and
/// advances `rolled_through_ts`; prunes the hourly tier (and the daily tier when the
/// policy opts into a bound). Returns the sorted retained raw window, the updated
/// rollup, and the [`RollSummary`]. The caller persists the rollup FIRST, then the
/// retained window (the crash-safe order).
fn plan_roll(
    samples: Vec<Sample>,
    mut rollup: Rollup,
    now: i64,
    policy: &RetentionPolicy,
) -> (Vec<Sample>, Rollup, RollSummary) {
    // A sample rolls only once its ENTIRE day sits older than the raw window, so the
    // day's bucket is always folded from a complete batch (exact p95, no re-merge).
    let roll_before = now - policy.raw_window_secs;
    let day_fully_aged = |ts: i64| day_start(ts) + DAY_SECS <= roll_before;

    let mut retained: Vec<Sample> = Vec::new();
    let mut to_roll: Vec<Sample> = Vec::new();
    for sample in samples {
        if !day_fully_aged(sample.ts) {
            retained.push(sample); // still inside the raw window
        } else if sample.ts > rollup.rolled_through_ts {
            to_roll.push(sample); // aged out and not yet folded
        }
        // else: aged out AND already folded (ts <= watermark) → compacted away.
    }

    // How many raw lines this pass folds down — the `usage_rollup` event's `raw_lines`
    // (issue #161). Captured before `to_roll` is consumed by the fold below.
    let raw_lines = to_roll.len() as u32;
    if let Some(newest) = to_roll.iter().map(|s| s.ts).max() {
        fold_into_tiers(&mut rollup, &to_roll, policy);
        rollup.rolled_through_ts = rollup.rolled_through_ts.max(newest);
    }

    // Bound the hourly tier. The daily tier is lifetime UNLESS the policy opts into a
    // bound (`daily_window_secs > 0`, issue #161) — `0` keeps every daily bucket, the
    // by-design lifetime default.
    let hourly_cutoff = now - policy.hourly_window_secs;
    rollup
        .hourly
        .retain(|bucket| bucket.hour_start >= hourly_cutoff);
    if policy.daily_window_secs > 0 {
        let daily_cutoff = now - policy.daily_window_secs;
        rollup
            .daily
            .retain(|bucket| bucket.day_start >= daily_cutoff);
    }
    rollup.hourly.sort_by_key(|bucket| bucket.hour_start);
    rollup.daily.sort_by_key(|bucket| bucket.day_start);

    // The bounded raw window, sorted — the caller rewrites it atomically (never a
    // torn read).
    retained.sort_by_key(|sample| sample.ts);

    let rolled_through_ts = rollup.rolled_through_ts;
    (
        retained,
        rollup,
        RollSummary {
            rolled_through_ts,
            raw_lines,
        },
    )
}

/// Atomically rewrite the raw file to exactly `samples` (used by
/// [`compact_and_roll`] — never the append path). Empty `samples` writes an empty
/// file. Uses [`crate::paths::write_private_file`] (tmp + `fsync` + `rename`, `0600`)
/// so a concurrent reader never observes a half-written file.
fn write_samples(samples_path: &Path, samples: &[Sample]) -> Result<()> {
    let mut buf = Vec::new();
    for sample in samples {
        serde_json::to_writer(&mut buf, sample).map_err(|_| serialize_err())?;
        buf.push(b'\n');
    }
    paths::write_private_file(samples_path, &buf)
}

/// Atomically (over)write the rollup file.
///
/// The rollup is our own `0600` file; the rewrite goes through the crate's atomic
/// path so a concurrent reader sees the old-or-new object, never a half-written one.
/// A first write creates it `0600` ([`crate::paths::write_private_file`]); a rewrite
/// preserves the existing mode via `fchmod` ([`crate::paths::write_preserving_mode`]),
/// honouring an operator who tightened/loosened it rather than silently forcing it
/// back. Single-writer (now enforced by the store lock — [`StoreLock`], issue #188),
/// so the `exists()` probe is race-free.
fn write_rollup(rollup_path: &Path, rollup: &Rollup) -> Result<()> {
    let bytes = serde_json::to_vec(rollup).map_err(|_| serialize_err())?;
    if rollup_path.exists() {
        paths::write_preserving_mode(rollup_path, &bytes)
    } else {
        paths::write_private_file(rollup_path, &bytes)
    }
}

/// Fold a `batch` of aged samples into the hourly + daily tiers, grouped by day and
/// (within a day) by hour. Each group is summarised exactly from its complete set;
/// a same-key bucket is merged defensively (unreachable under the roll-once-per-day
/// guarantee, but keeps the tiers duplicate-free if a late sample ever appears).
fn fold_into_tiers(rollup: &mut Rollup, batch: &[Sample], policy: &RetentionPolicy) {
    let expected = expected_per_day(policy);

    let mut by_day: BTreeMap<i64, Vec<&Sample>> = BTreeMap::new();
    for sample in batch {
        by_day.entry(day_start(sample.ts)).or_default().push(sample);
    }

    for (day, day_samples) in by_day {
        let mut by_hour: BTreeMap<i64, Vec<&Sample>> = BTreeMap::new();
        for sample in &day_samples {
            by_hour
                .entry(hour_start(sample.ts))
                .or_default()
                .push(sample);
        }
        for (hour, hour_samples) in by_hour {
            let session: Vec<f64> = hour_samples.iter().map(|s| s.session).collect();
            let weekly: Vec<f64> = hour_samples.iter().map(|s| s.weekly).collect();
            upsert_hour(
                rollup,
                HourBucket {
                    hour_start: hour,
                    count: hour_samples.len() as u32,
                    session: HourStat {
                        max: max_of(&session),
                        mean: mean_of(&session),
                    },
                    weekly: HourStat {
                        max: max_of(&weekly),
                        mean: mean_of(&weekly),
                    },
                },
            );
        }

        let session: Vec<f64> = day_samples.iter().map(|s| s.session).collect();
        let weekly: Vec<f64> = day_samples.iter().map(|s| s.weekly).collect();
        let count = day_samples.len() as u32;
        upsert_day(
            rollup,
            DayBucket {
                day_start: day,
                count,
                coverage: (f64::from(count) / expected).min(1.0),
                session: day_stat(&session),
                weekly: day_stat(&weekly),
            },
            expected,
        );
    }
}

/// Insert `bucket`, or merge it into an existing same-hour bucket (max / mean /
/// count all exactly mergeable).
fn upsert_hour(rollup: &mut Rollup, bucket: HourBucket) {
    if let Some(existing) = rollup
        .hourly
        .iter_mut()
        .find(|b| b.hour_start == bucket.hour_start)
    {
        existing.session = merge_hour_stat(
            &existing.session,
            existing.count,
            &bucket.session,
            bucket.count,
        );
        existing.weekly = merge_hour_stat(
            &existing.weekly,
            existing.count,
            &bucket.weekly,
            bucket.count,
        );
        existing.count += bucket.count;
    } else {
        rollup.hourly.push(bucket);
    }
}

/// Insert `bucket`, or merge it into an existing same-day bucket. Merging is
/// defensive-only (the roll-once-per-day guarantee means a day is never folded
/// twice); p95 is merged conservatively as the max of the two, since it cannot be
/// recovered exactly from summaries.
fn upsert_day(rollup: &mut Rollup, bucket: DayBucket, expected: f64) {
    if let Some(existing) = rollup
        .daily
        .iter_mut()
        .find(|b| b.day_start == bucket.day_start)
    {
        existing.session = merge_day_stat(
            &existing.session,
            existing.count,
            &bucket.session,
            bucket.count,
        );
        existing.weekly = merge_day_stat(
            &existing.weekly,
            existing.count,
            &bucket.weekly,
            bucket.count,
        );
        existing.count += bucket.count;
        existing.coverage = (f64::from(existing.count) / expected).min(1.0);
    } else {
        rollup.daily.push(bucket);
    }
}

/// Count-weighted merge of two hourly summaries.
fn merge_hour_stat(a: &HourStat, a_count: u32, b: &HourStat, b_count: u32) -> HourStat {
    HourStat {
        max: a.max.max(b.max),
        mean: weighted_mean(a.mean, a_count, b.mean, b_count),
    }
}

/// Count-weighted merge of two daily summaries (p95 conservative, cap-hits additive).
fn merge_day_stat(a: &DayStat, a_count: u32, b: &DayStat, b_count: u32) -> DayStat {
    DayStat {
        max: a.max.max(b.max),
        mean: weighted_mean(a.mean, a_count, b.mean, b_count),
        p95: a.p95.max(b.p95),
        cap_hits: a.cap_hits + b.cap_hits,
    }
}

/// The count-weighted mean of two means. `a_count + b_count` is always `> 0` here
/// (both operands come from non-empty buckets).
fn weighted_mean(a_mean: f64, a_count: u32, b_mean: f64, b_count: u32) -> f64 {
    let total = f64::from(a_count) + f64::from(b_count);
    if total == 0.0 {
        return 0.0;
    }
    (a_mean * f64::from(a_count) + b_mean * f64::from(b_count)) / total
}

/// Expected samples per day at the policy's cadence (at least 1), the coverage
/// denominator.
fn expected_per_day(policy: &RetentionPolicy) -> f64 {
    let per_day = (DAY_SECS / policy.poll_interval_secs.max(1)).max(1);
    per_day as f64
}

/// The UTC-midnight epoch of `ts`'s day. `rem_euclid` floors correctly for any sign.
fn day_start(ts: i64) -> i64 {
    ts - ts.rem_euclid(DAY_SECS)
}

/// The top-of-hour epoch of `ts`'s hour.
fn hour_start(ts: i64) -> i64 {
    ts - ts.rem_euclid(HOUR_SECS)
}

/// The peak of a non-empty slice of finite fractions.
fn max_of(xs: &[f64]) -> f64 {
    xs.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

/// The arithmetic mean of a slice (`0.0` for an empty slice, which the tiers never
/// produce).
fn mean_of(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

/// The 95th-percentile value of `xs` by the nearest-rank method (`0.0` for empty).
fn p95_of(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(f64::total_cmp);
    // Nearest-rank: the ceil(p·n)-th value (1-indexed), clamped into range.
    let rank = ((P95 * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// The daily per-dimension summary over a day's `xs`.
fn day_stat(xs: &[f64]) -> DayStat {
    DayStat {
        max: max_of(xs),
        mean: mean_of(xs),
        p95: p95_of(xs),
        cap_hits: xs.iter().filter(|&&x| x >= CAP_FRACTION).count() as u32,
    }
}

/// The secret-free serialize error — reachable only for a non-finite float in a
/// usage fraction/spend, which JSON cannot represent. The store holds no secret, so
/// the static hint leaks nothing.
fn serialize_err() -> Error {
    Error::UsageStoreSerialize("a usage value was not a finite number")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A minimal reading: `provider="claude"`, `acct="work"`, no optionals.
    fn sample(ts: i64, session: f64, weekly: f64) -> Sample {
        Sample::new(ts, "claude", "work", session, weekly)
    }

    /// The two store paths under a fresh temp dir.
    fn store_paths(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        (
            dir.join("usage-samples.jsonl"),
            dir.join("usage-rollup.json"),
        )
    }

    // --- AC 1: sample serialize/deserialize round-trip ------------------------

    #[test]
    fn sample_round_trips_with_and_without_optionals() {
        let full = sample(1_700_000_000, 0.42, 0.88)
            .with_resets(Some(1_700_003_600), Some(1_700_600_000))
            .with_severity(Some("critical".to_owned()))
            .with_spend(Some(1.25));
        let restored: Sample =
            serde_json::from_str(&serde_json::to_string(&full).unwrap()).unwrap();
        assert_eq!(restored, full);

        // A bare sample omits every optional key entirely (skip_serializing_if).
        let bare = sample(1_700_000_000, 0.10, 0.20);
        let json = serde_json::to_string(&bare).unwrap();
        assert_eq!(serde_json::from_str::<Sample>(&json).unwrap(), bare);
        for absent in ["session_resets_at", "weekly_resets_at", "severity", "spend"] {
            assert!(!json.contains(absent), "absent optional leaked: {json}");
        }
    }

    #[test]
    fn append_writes_one_parseable_line_per_sample() {
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, _) = store_paths(dir.path());
        append_sample(&samples_path, &sample(1, 0.1, 0.2)).unwrap();
        append_sample(&samples_path, &sample(2, 0.3, 0.4)).unwrap();

        let text = fs::read_to_string(&samples_path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "one line per sample");
        assert!(text.ends_with('\n'), "each record is newline-terminated");
        assert_eq!(read_samples(&samples_path).unwrap().len(), 2);
        // The raw file is 0600 (created through the private-file path).
        assert_eq!(
            fs::metadata(&samples_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn reading_absent_files_yields_empty_and_default() {
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, rollup_path) = store_paths(dir.path());
        assert!(read_samples(&samples_path).unwrap().is_empty());
        assert_eq!(read_rollup(&rollup_path).unwrap(), Rollup::default());
    }

    // --- AC 2: rollup written atomically (reader sees old-or-new, never half) --

    #[test]
    fn rollup_rewrite_leaves_no_temp_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let (_, rollup_path) = store_paths(dir.path());
        let mut rollup = Rollup {
            rolled_through_ts: 42,
            ..Rollup::default()
        };
        rollup.daily.push(DayBucket {
            day_start: 0,
            count: 3,
            coverage: 0.5,
            session: DayStat {
                max: 0.9,
                mean: 0.5,
                p95: 0.85,
                cap_hits: 0,
            },
            weekly: DayStat {
                max: 0.4,
                mean: 0.3,
                p95: 0.38,
                cap_hits: 0,
            },
        });
        write_rollup(&rollup_path, &rollup).unwrap();
        assert_eq!(
            read_rollup(&rollup_path).unwrap(),
            rollup,
            "exact round-trip"
        );
        // No sibling temp file survives the atomic rename.
        assert!(!rollup_path.with_extension("json.tmp").exists());
        let mut tmp = rollup_path.clone().into_os_string();
        tmp.push(".tmp");
        assert!(!Path::new(&tmp).exists(), "no <path>.tmp left behind");
    }

    #[test]
    fn concurrent_reader_never_sees_a_half_written_rollup() {
        // The atomicity AC: while one writer rewrites the rollup repeatedly, a
        // reader must only ever observe a COMPLETE object (old or new), never a
        // parse error from a half-written file. rename(2) on one filesystem gives
        // exactly that; a non-atomic write would let the reader catch a torn file.
        let dir = tempfile::tempdir().unwrap();
        let (_, rollup_path) = store_paths(dir.path());

        let big = |marker: i64| {
            let mut r = Rollup {
                rolled_through_ts: marker,
                ..Rollup::default()
            };
            // Many buckets → a large document a non-atomic write would tear.
            for d in 0..200 {
                r.daily.push(DayBucket {
                    day_start: i64::from(d) * DAY_SECS,
                    count: 288,
                    coverage: 1.0,
                    session: DayStat {
                        max: 0.99,
                        mean: 0.55,
                        p95: 0.92,
                        cap_hits: 2,
                    },
                    weekly: DayStat {
                        max: 0.61,
                        mean: 0.40,
                        p95: 0.58,
                        cap_hits: 0,
                    },
                });
            }
            r
        };
        write_rollup(&rollup_path, &big(0)).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let reads = Arc::new(AtomicUsize::new(0));
        let reader = {
            let path = rollup_path.clone();
            let stop = Arc::clone(&stop);
            let reads = Arc::clone(&reads);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    // A half-written file would fail to parse → Err → panic here.
                    let r = read_rollup(&path).expect("reader saw a torn rollup");
                    assert_eq!(r.daily.len(), 200, "reader saw a partial rollup");
                    reads.fetch_add(1, Ordering::Relaxed);
                }
            })
        };

        for marker in 0..300 {
            write_rollup(&rollup_path, &big(marker)).unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        reader.join().unwrap();
        assert!(reads.load(Ordering::Relaxed) > 0, "reader never ran");
    }

    #[test]
    fn rollup_rewrite_preserves_an_operator_set_mode() {
        // First write creates it 0600; a rewrite fchmod-preserves the current mode
        // rather than forcing 0600 back (the durability AC's fchmod-preserve).
        let dir = tempfile::tempdir().unwrap();
        let (_, rollup_path) = store_paths(dir.path());
        write_rollup(&rollup_path, &Rollup::default()).unwrap();
        assert_eq!(
            fs::metadata(&rollup_path).unwrap().permissions().mode() & 0o777,
            0o600,
            "created 0600"
        );
        fs::set_permissions(&rollup_path, fs::Permissions::from_mode(0o640)).unwrap();

        write_rollup(
            &rollup_path,
            &Rollup {
                rolled_through_ts: 7,
                ..Rollup::default()
            },
        )
        .unwrap();
        assert_eq!(
            fs::metadata(&rollup_path).unwrap().permissions().mode() & 0o777,
            0o640,
            "rewrite preserved the operator-set mode"
        );
        assert_eq!(read_rollup(&rollup_path).unwrap().rolled_through_ts, 7);
    }

    // --- AC 3: torn last raw line tolerated on read ---------------------------

    #[test]
    fn torn_trailing_line_is_skipped_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, _) = store_paths(dir.path());
        let good = [
            sample(1, 0.1, 0.2),
            sample(2, 0.3, 0.4),
            sample(3, 0.5, 0.6),
        ];
        for s in &good {
            append_sample(&samples_path, s).unwrap();
        }
        // Simulate a crash mid-append: a partial JSON record with no newline.
        {
            let mut f = paths::create_private_file(&samples_path).unwrap();
            f.write_all(br#"{"ts":4,"provider":"claude","acct":"wo"#)
                .unwrap();
        }
        assert_eq!(read_samples(&samples_path).unwrap(), good.to_vec());
    }

    #[test]
    fn torn_trailing_line_with_invalid_utf8_is_skipped() {
        // A crash can cut mid-UTF-8, leaving bytes that are not valid UTF-8. Parsing
        // per-record on raw bytes tolerates it — the whole read must not fail.
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, _) = store_paths(dir.path());
        append_sample(&samples_path, &sample(1, 0.1, 0.2)).unwrap();
        {
            let mut f = paths::create_private_file(&samples_path).unwrap();
            // A partial record ending in a lone UTF-8 lead byte (0xF0 = 4-byte start).
            f.write_all(b"{\"ts\":2,\"acct\":\"w\xf0\x9f").unwrap();
        }
        assert_eq!(
            read_samples(&samples_path).unwrap(),
            vec![sample(1, 0.1, 0.2)]
        );
    }

    // --- AC 4: retention bounds raw + rolls into hourly/daily (lifetime=daily) -

    #[test]
    fn retention_bounds_raw_window_and_rolls_into_tiers_lifetime_reads_daily() {
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, rollup_path) = store_paths(dir.path());
        let policy = RetentionPolicy::default(); // raw ~14d, hourly ~90d, poll 300s
        let now: i64 = 200 * DAY_SECS; // a clean day boundary well past every horizon

        // Three cohorts by age: ~120d (hourly-pruned, daily-kept), ~30d (both kept),
        // and recent (stays raw).
        let very_old = 80 * DAY_SECS + 5 * HOUR_SECS; // 120 days before `now`
        let mid_old = 170 * DAY_SECS + 2 * HOUR_SECS; // 30 days before `now`
        let recent = now - 2 * DAY_SECS; // inside the 14d raw window
        for k in 0..4 {
            append_sample(
                &samples_path,
                &sample(very_old + k * 600, 0.30 + 0.1 * k as f64, 0.20),
            )
            .unwrap();
            append_sample(&samples_path, &sample(mid_old + k * 600, 0.50, 0.60)).unwrap();
            append_sample(&samples_path, &sample(recent + k * 600, 0.15, 0.25)).unwrap();
        }

        compact_and_roll(&samples_path, &rollup_path, now, &policy).unwrap();

        // Raw window is bounded: only the recent cohort remains.
        let remaining = read_samples(&samples_path).unwrap();
        assert_eq!(remaining.len(), 4, "only the in-window samples remain raw");
        assert!(
            remaining.iter().all(|s| s.ts >= recent),
            "aged samples were removed"
        );

        let rollup = read_rollup(&rollup_path).unwrap();
        let very_old_day = day_start(very_old);
        let mid_old_day = day_start(mid_old);

        // Daily tier is LIFETIME — it holds both aged days, even the >90d one.
        assert!(
            rollup.daily.iter().any(|d| d.day_start == very_old_day),
            "120d-old day kept in daily"
        );
        assert!(
            rollup.daily.iter().any(|d| d.day_start == mid_old_day),
            "30d-old day kept in daily"
        );

        // Hourly tier is bounded to ~90d — the 120d-old hours are pruned, the 30d
        // ones kept. So a LIFETIME reader must consult daily (the only tier with the
        // old data), which is exactly the AC.
        assert!(
            !rollup
                .hourly
                .iter()
                .any(|h| day_start(h.hour_start) == very_old_day),
            "120d-old hours pruned"
        );
        assert!(
            rollup
                .hourly
                .iter()
                .any(|h| day_start(h.hour_start) == mid_old_day),
            "30d-old hours kept"
        );

        assert!(
            rollup.rolled_through_ts >= mid_old + 3 * 600,
            "watermark advanced past rolled batch"
        );

        // Idempotent: a second identical roll changes nothing.
        let before = rollup.clone();
        compact_and_roll(&samples_path, &rollup_path, now, &policy).unwrap();
        let after = read_rollup(&rollup_path).unwrap();
        assert_eq!(after, before, "re-rolling the same state is a no-op");
        assert_eq!(read_samples(&samples_path).unwrap().len(), 4);
    }

    #[test]
    fn daily_aggregates_are_exact_for_a_single_day_batch() {
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, rollup_path) = store_paths(dir.path());
        let policy = RetentionPolicy::default();
        let now: i64 = 200 * DAY_SECS;
        let day = 10 * DAY_SECS; // long aged out

        // Six session readings across one hour, two of them at/over the cap.
        let sessions = [0.2, 0.4, 0.6, 0.8, 1.0, 1.2];
        for (k, &s) in sessions.iter().enumerate() {
            append_sample(&samples_path, &sample(day + k as i64 * 600, s, 0.10)).unwrap();
        }
        compact_and_roll(&samples_path, &rollup_path, now, &policy).unwrap();

        let rollup = read_rollup(&rollup_path).unwrap();
        let bucket = rollup.daily.iter().find(|d| d.day_start == day).unwrap();
        assert_eq!(bucket.count, 6);
        assert!((bucket.session.max - 1.2).abs() < 1e-9, "max");
        assert!((bucket.session.mean - 0.7).abs() < 1e-9, "mean");
        // Nearest-rank p95 of 6 values: ceil(0.95*6)=6 → the 6th (largest) = 1.2.
        assert!((bucket.session.p95 - 1.2).abs() < 1e-9, "p95");
        assert_eq!(bucket.session.cap_hits, 2, "1.0 and 1.2 are cap hits");
        // Coverage = 6 observed ÷ (86400/300 = 288 expected).
        assert!((bucket.coverage - 6.0 / 288.0).abs() < 1e-9, "coverage");
    }

    // --- AC 5: store carries redacted handles only (no email/token) -----------

    #[test]
    fn persisted_store_carries_no_email_or_token() {
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, rollup_path) = store_paths(dir.path());
        // A realistic sample: a redacted handle + a severity label, nothing secret.
        append_sample(
            &samples_path,
            &sample(1_700_000_000, 0.9, 0.7).with_severity(Some("critical".to_owned())),
        )
        .unwrap();
        compact_and_roll(
            &samples_path,
            &rollup_path,
            1_700_000_000 + 400 * DAY_SECS,
            &RetentionPolicy::default(),
        )
        .unwrap();

        for path in [&samples_path, &rollup_path] {
            if let Ok(text) = fs::read_to_string(path) {
                assert!(!text.contains('@'), "no email may reach the store: {text}");
                assert!(
                    !text.contains("sk-ant"),
                    "no token may reach the store: {text}"
                );
            }
        }
    }

    #[test]
    fn sample_serializes_exactly_the_intended_keys() {
        // Structural redaction proof: a Sample exposes only the intended, non-secret
        // keys — there is no field that could carry an email or token.
        let json = serde_json::to_string(
            &sample(1, 0.5, 0.6)
                .with_resets(Some(2), Some(3))
                .with_severity(Some("warning".to_owned()))
                .with_spend(Some(0.0)),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "acct",
                "provider",
                "session",
                "session_resets_at",
                "severity",
                "spend",
                "ts",
                "weekly",
                "weekly_resets_at",
            ]
        );
    }

    // --- unit-level checks on the aggregate math ------------------------------

    #[test]
    fn bucket_alignment_floors_to_utc_hour_and_day() {
        assert_eq!(hour_start(3_600 + 59), 3_600);
        assert_eq!(hour_start(7_199), 3_600);
        assert_eq!(day_start(DAY_SECS + 5 * HOUR_SECS), DAY_SECS);
        assert_eq!(day_start(2 * DAY_SECS - 1), DAY_SECS);
    }

    #[test]
    fn p95_uses_nearest_rank() {
        // 20 values 1..=20: ceil(0.95*20)=19 → the 19th smallest = 19.0.
        let xs: Vec<f64> = (1..=20).map(f64::from).collect();
        assert!((p95_of(&xs) - 19.0).abs() < 1e-9);
        // Single value → itself; empty → 0.0.
        assert!((p95_of(&[0.42]) - 0.42).abs() < 1e-9);
        assert!(p95_of(&[]).abs() < 1e-9);
    }

    #[test]
    fn weighted_mean_pools_by_count() {
        // (0.2·1 + 0.8·3) / 4 = 0.65.
        assert!((weighted_mean(0.2, 1, 0.8, 3) - 0.65).abs() < 1e-9);
    }

    // --- issue #161: RollSummary + configurable daily retention ---------------

    #[test]
    fn compact_and_roll_reports_what_it_folded() {
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, rollup_path) = store_paths(dir.path());
        let policy = RetentionPolicy::default();
        let now: i64 = 200 * DAY_SECS;
        let aged = 10 * DAY_SECS; // one aged-out day (past the 14d raw window)
        let recent = now - 2 * DAY_SECS; // inside the raw window
        for k in 0..3 {
            append_sample(&samples_path, &sample(aged + k * 600, 0.5, 0.6)).unwrap();
        }
        for k in 0..2 {
            append_sample(&samples_path, &sample(recent + k * 600, 0.1, 0.2)).unwrap();
        }

        let summary = compact_and_roll(&samples_path, &rollup_path, now, &policy).unwrap();
        assert_eq!(summary.raw_lines, 3, "folded exactly the 3 aged samples");
        assert_eq!(
            summary.rolled_through_ts,
            aged + 2 * 600,
            "watermark = newest folded ts"
        );

        // A second pass folds nothing new: raw_lines 0 (so the daemon emits no event),
        // watermark unchanged.
        let again = compact_and_roll(&samples_path, &rollup_path, now, &policy).unwrap();
        assert_eq!(again.raw_lines, 0, "nothing new to fold");
        assert_eq!(again.rolled_through_ts, summary.rolled_through_ts);
    }

    #[test]
    fn zero_daily_window_keeps_the_daily_tier_for_the_stores_lifetime() {
        // The default (`daily_window_secs = 0`) preserves the by-design lifetime daily tier.
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, rollup_path) = store_paths(dir.path());
        let policy = RetentionPolicy::default();
        assert_eq!(policy.daily_window_secs, 0, "default daily = lifetime");
        let now: i64 = 400 * DAY_SECS;
        let ancient = 5 * DAY_SECS; // ~395 days old
        for k in 0..4 {
            append_sample(&samples_path, &sample(ancient + k * 600, 0.5, 0.6)).unwrap();
        }

        compact_and_roll(&samples_path, &rollup_path, now, &policy).unwrap();
        let rollup = read_rollup(&rollup_path).unwrap();
        assert!(
            rollup
                .daily
                .iter()
                .any(|d| d.day_start == day_start(ancient)),
            "daily=0 keeps even a 395d-old day (lifetime)"
        );
    }

    #[test]
    fn a_nonzero_daily_window_prunes_aged_daily_buckets() {
        // Opting into a bounded daily tier prunes days older than the window while keeping
        // days inside it (issue #161) — retention honouring the `[stats]` config.
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, rollup_path) = store_paths(dir.path());
        let policy = RetentionPolicy {
            daily_window_secs: 60 * DAY_SECS, // bound the daily tier to ~60d
            ..RetentionPolicy::default()
        };
        let now: i64 = 400 * DAY_SECS;
        let ancient = 5 * DAY_SECS; // ~395d old → beyond the 60d daily window
        let within = 370 * DAY_SECS; // 30d old → inside the 60d window (and past the raw window)
        for k in 0..4 {
            append_sample(&samples_path, &sample(ancient + k * 600, 0.5, 0.6)).unwrap();
            append_sample(&samples_path, &sample(within + k * 600, 0.4, 0.5)).unwrap();
        }

        compact_and_roll(&samples_path, &rollup_path, now, &policy).unwrap();
        let rollup = read_rollup(&rollup_path).unwrap();
        assert!(
            !rollup
                .daily
                .iter()
                .any(|d| d.day_start == day_start(ancient)),
            "395d-old daily bucket pruned by the 60d window"
        );
        assert!(
            rollup
                .daily
                .iter()
                .any(|d| d.day_start == day_start(within)),
            "30d-old daily bucket kept (inside the 60d window)"
        );
    }

    // --- issue #188: crash-safe ordering + enforced single-writer lock ----------

    #[test]
    fn a_fault_at_every_write_boundary_loses_no_sample() {
        // AC (issue #188): a fault injected at every write boundary in the compaction
        // persist step must leave every sample recoverable — folded-and-durable in
        // the rollup OR still present in raw for a re-roll, never lost from BOTH
        // tiers. Because the rollup is persisted BEFORE the raw is truncated (and the
        // `rolled_through_ts` watermark de-dupes a re-roll), a crash anywhere in the
        // persist window loses nothing and double-counts nothing: recovery converges
        // to the SAME end state at every boundary.
        //
        //   boundary 0: crash BEFORE write_rollup                (nothing persisted)
        //   boundary 1: crash AFTER write_rollup, BEFORE write_samples (the window)
        //   boundary 2: crash AFTER write_samples                (fully persisted)
        for crash_after in 0..=2 {
            let dir = tempfile::tempdir().unwrap();
            let (samples_path, rollup_path) = store_paths(dir.path());
            let policy = RetentionPolicy::default();
            let now: i64 = 200 * DAY_SECS;
            let aged_day = 10 * DAY_SECS; // long past the 14d raw window
            let recent = now - 2 * DAY_SECS; // inside the raw window
            for k in 0..3 {
                append_sample(&samples_path, &sample(aged_day + k * 600, 0.5, 0.6)).unwrap();
            }
            for k in 0..2 {
                append_sample(&samples_path, &sample(recent + k * 600, 0.1, 0.2)).unwrap();
            }

            // Drive the plan + a PARTIAL persist by hand — the real write functions,
            // the real crash-safe order — stopping at boundary `crash_after`.
            let samples = read_samples(&samples_path).unwrap();
            let rollup = read_rollup(&rollup_path).unwrap();
            let (retained, rolled, _summary) = plan_roll(samples, rollup, now, &policy);
            if crash_after >= 1 {
                write_rollup(&rollup_path, &rolled).unwrap();
            }
            if crash_after >= 2 {
                write_samples(&samples_path, &retained).unwrap();
            }

            // Recovery: a full compaction to completion.
            compact_and_roll(&samples_path, &rollup_path, now, &policy).unwrap();

            // Identical invariant at EVERY boundary: the 3 aged samples are folded
            // EXACTLY once into `aged_day`, and only the 2 recent samples stay raw —
            // never lost (count < 3) and never double-counted (count > 3).
            let remaining = read_samples(&samples_path).unwrap();
            assert_eq!(
                remaining.len(),
                2,
                "crash_after={crash_after}: only the in-window samples stay raw"
            );
            assert!(
                remaining.iter().all(|s| s.ts >= recent),
                "crash_after={crash_after}: aged samples left raw"
            );
            let rollup = read_rollup(&rollup_path).unwrap();
            let day = rollup
                .daily
                .iter()
                .find(|d| d.day_start == day_start(aged_day))
                .unwrap_or_else(|| panic!("crash_after={crash_after}: aged day never folded"));
            assert_eq!(
                day.count, 3,
                "crash_after={crash_after}: aged day folded exactly once (no loss, no double-count)"
            );
        }
    }

    #[test]
    fn a_held_store_lock_fails_a_second_bounded_acquire_closed() {
        // The enforcement AC (issue #188): while one `StoreLock` is held, a second
        // bounded acquire on the same path fails CLOSED with `UsageStoreBusy` rather
        // than proceeding without the lock. A second, SEPARATE open of the same file
        // contends even within one process (flock locks the open file description),
        // so the bounded wait elapses — mirroring the swap lock's own proof.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.lock");

        let _held = StoreLock::acquire(&path, STORE_LOCK_MAX_WAIT).unwrap();
        let start = Instant::now();
        let busy = StoreLock::acquire(&path, Duration::from_millis(120)).unwrap_err();
        assert!(
            matches!(busy, Error::UsageStoreBusy),
            "a held lock must fail closed, got {busy:?}"
        );
        assert!(
            start.elapsed() >= Duration::from_millis(100),
            "the bounded wait must actually elapse (not an instant fail)"
        );
        assert_eq!(
            busy.exit_code(),
            4,
            "fail-closed shares the retry-shortly code"
        );
    }

    #[test]
    fn append_never_loses_a_sample_to_a_concurrent_compaction() {
        // The enforcement AC end-to-end (issue #188): an `append_sample` running
        // concurrently with a `compact_and_roll` rewrite is serialized by the store
        // lock, so the rewrite's read-modify-write can never drop it. Every appended
        // sample sits INSIDE the raw window (never aged), so a correct run keeps all
        // of them in the raw file; a lost append (the pre-lock race) would surface as
        // a short count.
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, rollup_path) = store_paths(dir.path());
        let policy = RetentionPolicy::default();
        let now: i64 = 200 * DAY_SECS;
        let base = now - DAY_SECS; // inside the 14d raw window → always retained

        const N: i64 = 50;
        let appender = {
            let samples_path = samples_path.clone();
            std::thread::spawn(move || {
                for k in 0..N {
                    append_sample(&samples_path, &sample(base + k, 0.1, 0.2)).unwrap();
                }
            })
        };
        // Compact concurrently while the appender runs. A brief yield between passes
        // models reality (the daemon compacts at most hourly, never in a tight loop)
        // AND keeps the advisory lock — which `flock` grants unfairly — free often
        // enough that neither writer starves the other; a tight no-yield loop would
        // let the compactor hold the lock ~continuously and starve the backing-off
        // appender, a test artifact rather than a serialization failure.
        while !appender.is_finished() {
            compact_and_roll(&samples_path, &rollup_path, now, &policy).unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        appender.join().unwrap();
        // Settle any samples appended after the last concurrent compaction.
        compact_and_roll(&samples_path, &rollup_path, now, &policy).unwrap();

        let remaining = read_samples(&samples_path).unwrap();
        assert_eq!(
            remaining.len(),
            N as usize,
            "every concurrently-appended sample survived the compaction rewrites"
        );
    }
}
