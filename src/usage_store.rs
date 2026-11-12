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
//!                       append-only (the daemon writes one line per successful
//!                       poll and none for a failed one; a quota window the
//!                       account lacks is written as `null`).
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
//! # Every dimension the account has, never one worst-case scalar
//!
//! Every tier keeps `session` and `weekly` separately, mirroring the swap
//! decision's own discipline (issue #41 / [`crate::usage::Usage`]): the store
//! projects neither window to a single blended number, because the two limits are
//! independent and a reader may care about either. An account may lack a window (a
//! verified Ultra account has no session window since 2026-10-30): the raw line then
//! carries that axis as `null` — the key always written, never a fabricated `0` — and
//! a tier bucket omits that axis's statistic when none of its readings carried it. A
//! statistic that is present records how many of the bucket's samples it covers
//! (`samples`); one folded before that field existed reads as not recorded, never as
//! the bucket's `count` and never as `0`.
//!
//! # Store format and version skew
//!
//! From this build's first compaction, `usage-rollup.json` is the envelope
//! `{"format":2,"rollup":{…}}`. A file with no `format` key is the legacy format 1 and
//! reads as before; a `format` above this build's own is refused as written by a newer
//! sessiometer. This build prints that refusal, and any other compaction failure, to
//! its stderr, which reaches `daemon.err.log` only under a CLI-installed agent; the app
//! bundle's agent plist routes stderr to no file. There a stall on the read side, a
//! newer or malformed rollup, shows only as `stats unavailable` on the socket verb and
//! in the rollup's `format` key, and a compaction that fails on the write side shows
//! nowhere.
//!
//! Every older build refuses the envelope before it rewrites either store file. So on a
//! store this build has compacted, an older daemon keeps appending samples but stops
//! compacting without a word and erases nothing; an older `stats` fails with "malformed
//! usage rollup", which is expected after this change: read the file's `format` key
//! before treating it as damage; and an older `reliability`, which never reads the
//! rollup, leaves `null` lines out of its report and deletes nothing. Never delete or
//! move `usage-samples.jsonl` or `usage-rollup.json`: without the rollup an older build
//! reads the store as new and erases its `null` lines.
//!
//! The fence leaves open the lines appended before the first fenced write, a rollup
//! deleted or moved by hand, and an older `reliability`'s report, which still leaves
//! `null` lines out; and on every machine that has run this build, every older build's
//! `stats` and compaction stay stopped, which is the fence's purpose. No older build
//! runs as the daemon against a store that holds a `null` line (the deployment rule of
//! issue #1604, stated in `docs/runbooks/replacing-the-running-daemon.md` § Prerequisites,
//! "Both halves of the binary change together").
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
//! estimate, plus `acct` — the account's **roster label**. Since #444/#447 that
//! label may be an operator-authored email (the capture prompt pre-fills the
//! harvested address), carried under the same provenance-scoped rule as the
//! render/event channels: an authored email label is permitted, but an UNAUTHORED
//! email (a stranger's address, a credential-blob spill) or a token never is
//! (issue #15, relaxed by #444; see `redaction::meter::unauthored_emails`). Every
//! field is therefore safe to persist and safe to `Debug`, unlike the
//! credential-bearing types ([`crate::keychain`] / [`crate::claude_state`]) that
//! deliberately omit `Debug`.
//!
//! # Who writes and who reads
//!
//! This module owns the store's data model and file operations. The daemon's per-poll
//! collector (issue #156, in [`crate::daemon`]) calls [`append_sample`] and
//! [`compact_and_roll`]; the read-only reporting verbs (issue #157, [`crate::stats`] and
//! [`crate::reliability`]) call [`read_samples`] and [`read_rollup`].

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
/// `cap_hits` tally counts these per day, over the readings that carried the axis.
/// Usage fractions are `consumed / limit`, so `1.0` is exactly the cap and readings
/// can exceed it.
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
/// Provider-tagged and secret-free: `acct` is the account's roster label (an
/// operator-authored email permitted since #444/#447, an unauthored email or a token
/// never — see the module's redaction note), and every other field is a non-secret
/// percentage, timestamp or label — so `Debug` is safe here (contrast the
/// credential-bearing types that omit it). The four optional fields are omitted from
/// the JSON entirely when absent, keeping each line minimal; the two fraction keys
/// are always present, `null` for a window the account lacks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Sample {
    /// When the reading was taken, as whole UTC epoch seconds.
    pub(crate) ts: i64,
    /// The quota provider this reading came from (e.g. `"claude"`) — a tag, so a
    /// future multi-provider store keeps readings distinguishable.
    pub(crate) provider: String,
    /// The account's **roster label** (the operator's non-secret handle). An
    /// operator-authored email label is permitted (provenance-scoped, #444/#447); an
    /// UNAUTHORED email or a token NEVER — the store's redaction invariant (issue #15;
    /// see `redaction::meter::unauthored_emails`).
    pub(crate) acct: String,
    /// Fraction in `[0.0, …]` of the rolling 5-hour session window consumed (`1.0` =
    /// exhausted; readings can exceed it), or `None` when the account has no session
    /// window. Always written — as JSON `null` when `None` — so a line's key set does not
    /// depend on which windows the account has; a non-finite `Some` is refused at
    /// serialization rather than written as `null`, which would read back as "no window".
    #[serde(default, serialize_with = "serialize_finite_fraction")]
    pub(crate) session: Option<f64>,
    /// Fraction in `[0.0, …]` of the weekly window consumed, or `None` when the account
    /// has no weekly window. Written and refused exactly as `session`.
    #[serde(default, serialize_with = "serialize_finite_fraction")]
    pub(crate) weekly: Option<f64>,
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
        session: Option<f64>,
        weekly: Option<f64>,
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

/// Serialize a usage fraction: `None` as JSON `null`, a finite `Some` as the number, a
/// non-finite `Some` as an error (the hint `serialize_err` carries). JSON has no
/// spelling for `±∞` / `NaN`, and `serde_json` would otherwise write `null` — which now
/// means "no window", so the ambiguity is refused at the only writer.
fn serialize_finite_fraction<S: serde::Serializer>(
    value: &Option<f64>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    match value {
        None => serializer.serialize_none(),
        Some(fraction) if fraction.is_finite() => serializer.serialize_some(fraction),
        Some(_) => Err(serde::ser::Error::custom(
            "a usage value was not a finite number",
        )),
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

/// One hour's aggregate: the hour's sample count, and max / mean per dimension the
/// bucket's readings carried; omitted when none did. The mid-resolution tier —
/// enough to chart a day's shape, cheap enough to keep ~90 d of them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct HourBucket {
    /// Epoch seconds at the top of the hour (UTC), the bucket key.
    pub(crate) hour_start: i64,
    /// How many samples fell in this hour — every sample, whichever windows it carried.
    pub(crate) count: u32,
    /// Session-dimension max + mean over the hour's samples that carried a session
    /// reading; omitted when none did. An observation over the readings present, never a
    /// statistic fabricated for an account that has no such window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<HourStat>,
    /// Weekly-dimension max + mean over the hour's samples that carried a weekly
    /// reading; omitted when none did, on the same terms as `session`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) weekly: Option<HourStat>,
}

/// The hourly tier's summary of one dimension the bucket's readings carried: peak and
/// mean utilisation over those readings, and how many they were.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct HourStat {
    /// Peak utilisation fraction over the bucket's readings on this axis.
    pub(crate) max: f64,
    /// Mean utilisation fraction over the bucket's readings on this axis.
    pub(crate) mean: f64,
    /// How many of the bucket's samples carried this axis: the samples `max` and `mean` are
    /// over. Set at fold time, at least 1 and at most the bucket's `count`. `None` means not
    /// recorded (a bucket folded before this field existed): never the bucket's `count`,
    /// never `0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) samples: Option<u32>,
}

/// One day's aggregate: the day's sample count and coverage ratio, and max / mean / p95
/// / cap-hits per dimension the bucket's readings carried; omitted when none did. The
/// lowest-resolution tier — kept for the store's lifetime, so it is what a "lifetime"
/// reader consults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DayBucket {
    /// Epoch seconds at UTC midnight of the day, the bucket key.
    pub(crate) day_start: i64,
    /// How many samples fell in this day — every sample, whichever windows it carried.
    pub(crate) count: u32,
    /// Observed samples ÷ expected samples for the day (clamped to `1.0`) — how
    /// complete the day's data is, so a sparsely-polled day is not misread as calm.
    /// Over the day's whole sample count, like `count`.
    pub(crate) coverage: f64,
    /// Session-dimension summary over the day's samples that carried a session reading;
    /// omitted when none did. An observation over the readings present, never a statistic
    /// fabricated for an account that has no such window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<DayStat>,
    /// Weekly-dimension summary over the day's samples that carried a weekly reading;
    /// omitted when none did, on the same terms as `session`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) weekly: Option<DayStat>,
}

/// The daily tier's summary of one dimension the bucket's readings carried: peak, mean,
/// 95th percentile and the count of cap-hit samples over those readings, and how many
/// they were.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DayStat {
    /// Peak utilisation fraction over the day's readings on this axis.
    pub(crate) max: f64,
    /// Mean utilisation fraction over the day's readings on this axis.
    pub(crate) mean: f64,
    /// 95th-percentile utilisation fraction over the day's readings on this axis
    /// (nearest-rank).
    pub(crate) p95: f64,
    /// Readings on this axis at or above the quota cap ([`CAP_FRACTION`]) over the day.
    pub(crate) cap_hits: u32,
    /// How many of the day's samples carried this axis: the samples `max`, `mean`, `p95` and
    /// `cap_hits` are over. Set at fold time, at least 1 and at most the bucket's `count`.
    /// `None` means not recorded (a bucket folded before this field existed): never the
    /// bucket's `count`, never `0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) samples: Option<u32>,
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
        use std::os::unix::io::AsRawFd;

        let file = crate::file_policy::open_owner_only(
            OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false),
            path,
        )?;
        let deadline = Instant::now() + max_wait;
        loop {
            // Raw `flock` FFI, kept un-wrapped by ADR-0004: kept raw rather than
            // adding a `rustix` production dependency; the std wheel
            // (`File::try_lock`, stable 1.89) is the planned replacement once MSRV
            // reaches 1.89 (see #257).
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

/// The usage store's on-disk generation. Generation 1 is the bare `Rollup` every build
/// before issue #1598 wrote, with no `format` key; generation 2 wraps it as
/// `{"format":2,"rollup":{…}}`, which every build before #1598 refuses before it rewrites
/// either file. Bump it with any change to the written shape of either store file, and add
/// the new generation's pins to `the_written_store_shape_is_pinned_to_its_generation`.
/// The retired generation's pins stay: that test reads every generation's pinned text back
/// through [`read_rollup`] and the [`Sample`] decoder, so they keep guarding what this build
/// still reads once the writer assertions move on. That test detects a change to a value it
/// pins, so a new optional field is set in one of its canonical values, or its arrival passes
/// the test unseen.
const ROLLUP_FORMAT: u32 = 2;

/// The top-level `format` key alone, read before the body so the dispatch never guesses.
#[derive(Deserialize)]
struct FormatProbe {
    #[serde(default)]
    format: Option<u32>,
}

/// The rollup file as this build writes it: the generation, then the rollup itself.
#[derive(Serialize)]
struct RollupFileOut<'a> {
    format: u32,
    rollup: &'a Rollup,
}

/// The body of a rollup file whose `format` this build reads.
#[derive(Deserialize)]
struct RollupFileIn {
    rollup: Rollup,
}

/// Read the rolled-aggregate object; an absent file reads as an empty [`Rollup`].
///
/// The file's `format` key decides how it is read, explicitly: this build's own
/// generation ([`ROLLUP_FORMAT`]) is the envelope [`write_rollup`] writes; no `format` key
/// is the legacy generation 1, the bare [`Rollup`] every earlier build wrote, whose
/// statistics read back with no recorded `samples`; a `format` above this build's own was
/// written by a newer sessiometer and is refused ([`Error::UsageRollupNewerFormat`]) —
/// never a reason to delete or move the file.
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
    let malformed = |err: serde_json::Error| Error::UsageRollupMalformed {
        line: err.line(),
        column: err.column(),
    };
    match serde_json::from_slice::<FormatProbe>(&bytes)
        .map_err(malformed)?
        .format
    {
        Some(ROLLUP_FORMAT) => serde_json::from_slice::<RollupFileIn>(&bytes)
            .map(|file| file.rollup)
            .map_err(malformed),
        Some(found) if found > ROLLUP_FORMAT => Err(Error::UsageRollupNewerFormat {
            found,
            supported: ROLLUP_FORMAT,
        }),
        // No `format` key: generation 1, as every build before #1598 wrote it. No build
        // writes a `format` below 2; such a file takes the same decode, and an envelope that
        // carries one fails it at its own position.
        _ => serde_json::from_slice::<Rollup>(&bytes).map_err(malformed),
    }
}

/// Bound the raw window and fold aged samples into the hourly + daily tiers.
///
/// A single-writer operation (the daemon, issue #156) run periodically, serialized
/// against [`append_sample`] by the store lock ([`StoreLock`], issue #188) so the two
/// writers can never interleave a torn rewrite. Under that lock it:
///
/// 1. reads the raw samples and the current rollup, and with it the store's generation:
///    a rollup a newer sessiometer wrote ends the pass here, before either file is
///    rewritten ([`Error::UsageRollupNewerFormat`]);
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
///
/// Its one caller is [`compact_and_roll`], after [`read_rollup`] has checked the store's
/// generation. Any new caller checks the generation first, or it rewrites a store that
/// a newer generation wrote.
fn write_samples(samples_path: &Path, samples: &[Sample]) -> Result<()> {
    let mut buf = Vec::new();
    for sample in samples {
        serde_json::to_writer(&mut buf, sample).map_err(|_| serialize_err())?;
        buf.push(b'\n');
    }
    paths::write_private_file(samples_path, &buf)
}

/// Atomically (over)write the rollup file, as the envelope
/// `{"format":<ROLLUP_FORMAT>,"rollup":{…}}` — the only shape this build writes, and the
/// only rollup writer.
///
/// The rollup is our own `0600` file; the rewrite goes through the crate's atomic
/// path so a concurrent reader sees the old-or-new object, never a half-written one.
/// A first write creates it `0600` ([`crate::paths::write_private_file`]); a rewrite
/// preserves the existing mode via `fchmod` ([`crate::paths::write_preserving_mode`]),
/// honouring an operator who tightened/loosened it rather than silently forcing it
/// back. Single-writer (now enforced by the store lock — [`StoreLock`], issue #188),
/// so the `exists()` probe is race-free.
fn write_rollup(rollup_path: &Path, rollup: &Rollup) -> Result<()> {
    let file = RollupFileOut {
        format: ROLLUP_FORMAT,
        rollup,
    };
    let bytes = serde_json::to_vec(&file).map_err(|_| serialize_err())?;
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
///
/// Each dimension is summarised over the group's samples that carried it, and the
/// statistic records how many those were. A dimension none of them carried gets no
/// statistic, so an account with no such window is never summarised as zero.
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
            let session: Vec<f64> = hour_samples.iter().filter_map(|s| s.session).collect();
            let weekly: Vec<f64> = hour_samples.iter().filter_map(|s| s.weekly).collect();
            let hour_stat = |xs: &[f64]| {
                (!xs.is_empty()).then(|| HourStat {
                    max: max_of(xs),
                    mean: mean_of(xs),
                    samples: Some(xs.len() as u32),
                })
            };
            upsert_hour(
                rollup,
                HourBucket {
                    hour_start: hour,
                    count: hour_samples.len() as u32,
                    session: hour_stat(&session),
                    weekly: hour_stat(&weekly),
                },
            );
        }

        let session: Vec<f64> = day_samples.iter().filter_map(|s| s.session).collect();
        let weekly: Vec<f64> = day_samples.iter().filter_map(|s| s.weekly).collect();
        let count = day_samples.len() as u32;
        upsert_day(
            rollup,
            DayBucket {
                day_start: day,
                count,
                coverage: (f64::from(count) / expected).min(1.0),
                session: (!session.is_empty()).then(|| day_stat(&session)),
                weekly: (!weekly.is_empty()).then(|| day_stat(&weekly)),
            },
            expected,
        );
    }
}

/// Insert `bucket`, or merge it into an existing same-hour bucket, each dimension on
/// its own: see [`merge_hour_stat`]. `max` and `count` merge exactly; the pooled `mean`
/// is exact when both operands recorded `samples`, and otherwise weighted by the two
/// bucket counts, the pre-count rule, which is exact only when every sample of both
/// buckets carried the axis.
fn upsert_hour(rollup: &mut Rollup, bucket: HourBucket) {
    if let Some(existing) = rollup
        .hourly
        .iter_mut()
        .find(|b| b.hour_start == bucket.hour_start)
    {
        existing.session = merge_hour_stat(
            existing.session.as_ref(),
            existing.count,
            bucket.session.as_ref(),
            bucket.count,
        );
        existing.weekly = merge_hour_stat(
            existing.weekly.as_ref(),
            existing.count,
            bucket.weekly.as_ref(),
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
            existing.session.as_ref(),
            existing.count,
            bucket.session.as_ref(),
            bucket.count,
        );
        existing.weekly = merge_day_stat(
            existing.weekly.as_ref(),
            existing.count,
            bucket.weekly.as_ref(),
            bucket.count,
        );
        existing.count += bucket.count;
        existing.coverage = (f64::from(existing.count) / expected).min(1.0);
    } else {
        rollup.daily.push(bucket);
    }
}

/// Merge two hourly summaries of ONE dimension, either of which may be absent
/// (`a_count` / `b_count` are the two buckets' whole sample counts). Both absent: the
/// result is absent. One absent: the present side unchanged, its recorded count
/// included. Both present: `max` is the larger, and the mean and the merged count are
/// [`merged_mean`]'s.
fn merge_hour_stat(
    a: Option<&HourStat>,
    a_count: u32,
    b: Option<&HourStat>,
    b_count: u32,
) -> Option<HourStat> {
    match (a, b) {
        (None, None) => None,
        (Some(only), None) | (None, Some(only)) => Some(only.clone()),
        (Some(a), Some(b)) => {
            let (mean, samples) =
                merged_mean((a.mean, a.samples, a_count), (b.mean, b.samples, b_count));
            Some(HourStat {
                max: a.max.max(b.max),
                mean,
                samples,
            })
        }
    }
}

/// Merge two daily summaries of ONE dimension, on [`merge_hour_stat`]'s terms; p95 is
/// conservative (the larger of the two), cap-hits additive.
fn merge_day_stat(
    a: Option<&DayStat>,
    a_count: u32,
    b: Option<&DayStat>,
    b_count: u32,
) -> Option<DayStat> {
    match (a, b) {
        (None, None) => None,
        (Some(only), None) | (None, Some(only)) => Some(only.clone()),
        (Some(a), Some(b)) => {
            let (mean, samples) =
                merged_mean((a.mean, a.samples, a_count), (b.mean, b.samples, b_count));
            Some(DayStat {
                max: a.max.max(b.max),
                mean,
                p95: a.p95.max(b.p95),
                cap_hits: a.cap_hits + b.cap_hits,
                samples,
            })
        }
    }
}

/// The pooled mean of two present same-dimension statistics, and the count the merged
/// statistic records. Each operand is `(mean, recorded samples, the bucket's count)`.
///
/// When both operands recorded how many samples they are over, those two counts weight
/// the mean — `(m_a·n_a + m_b·n_b) / (n_a + n_b)` — and the merged statistic records
/// their sum. When either did not, the two bucket counts weight it, the rule every build
/// before the count existed applied, and the merged count is not recorded.
fn merged_mean(a: (f64, Option<u32>, u32), b: (f64, Option<u32>, u32)) -> (f64, Option<u32>) {
    let ((a_mean, a_samples, a_count), (b_mean, b_samples, b_count)) = (a, b);
    match (a_samples, b_samples) {
        (Some(n_a), Some(n_b)) => (weighted_mean(a_mean, n_a, b_mean, n_b), Some(n_a + n_b)),
        _ => (weighted_mean(a_mean, a_count, b_mean, b_count), None),
    }
}

/// The weighted mean of two means, by the weights it was given: the two recorded
/// `samples` when both operands carry one, the two bucket counts otherwise. Both weights
/// are at least 1 here, because a recorded `samples` is at least 1 and a bucket that
/// holds a statistic holds at least one sample; the zero arm is dead defence.
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

/// The peak of a non-empty slice of finite fractions — non-empty by construction: the
/// fold builds a statistic only for a dimension at least one reading carried.
fn max_of(xs: &[f64]) -> f64 {
    xs.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

/// The arithmetic mean of a slice. The `0.0` for an empty slice is dead defence: the
/// fold guarantees a non-empty one, building a statistic only for a dimension at least
/// one reading carried.
fn mean_of(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

/// The 95th-percentile value of `xs` by the nearest-rank method (`0.0` for empty — dead
/// defence here, as in [`mean_of`]). Delegates to the shared
/// [`crate::percentile::percentile`] (issue #455) — the single copy of the nearest-rank math.
fn p95_of(xs: &[f64]) -> f64 {
    crate::percentile::percentile(xs, P95)
}

/// The daily summary of one dimension over `xs`, the day's readings that carried it —
/// built per dimension the bucket's readings carried; omitted when none did, so `xs` is
/// never empty here.
fn day_stat(xs: &[f64]) -> DayStat {
    DayStat {
        max: max_of(xs),
        mean: mean_of(xs),
        p95: p95_of(xs),
        cap_hits: xs.iter().filter(|&&x| x >= CAP_FRACTION).count() as u32,
        samples: Some(xs.len() as u32),
    }
}

/// The secret-free serialize error — reachable for a non-finite usage fraction, which
/// JSON cannot represent: the error comes from the field serializer
/// ([`serialize_finite_fraction`]), which refuses it rather than write the `null` that
/// means "no window". The store holds no secret, so the static hint leaks nothing.
fn serialize_err() -> Error {
    Error::UsageStoreSerialize("a usage value was not a finite number")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A minimal reading: `provider="claude"`, `acct="work"`, no optionals.
    fn sample(ts: i64, session: f64, weekly: f64) -> Sample {
        Sample::new(ts, "claude", "work", Some(session), Some(weekly))
    }

    /// The two store paths under a fresh temp dir.
    fn store_paths(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        (
            dir.join("usage-samples.jsonl"),
            dir.join("usage-rollup.json"),
        )
    }

    /// Reads a window's fraction the same way before and after the `f64` → `Option<f64>` lift
    /// (issue #1590), so one assertion text compiles, and passes `clippy -D warnings`, on both sides.
    fn optional_fraction(window: impl Into<Option<f64>>) -> Option<f64> {
        window.into()
    }

    /// Write `lines` to `path` by hand, one JSON object per line, each ending in `\n`: the raw
    /// file exactly as a build may have left it (issue #1598).
    fn write_raw_lines(path: &Path, lines: &[&str]) {
        let mut text = String::new();
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        fs::write(path, text).unwrap();
    }

    /// The sorted top-level keys of a JSON object, which the store's key-set assertions read
    /// (issue #1598).
    fn sorted_keys(value: &serde_json::Value) -> Vec<&str> {
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("a store record is a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
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

        // An account without a window carries that axis as `null`: the key is written, never
        // omitted and never a fabricated `0`, and the sample round-trips equal (issue #1598).
        let weekly_only = Sample::new(1_700_000_000, "claude", "work", None, Some(0.2));
        let json = serde_json::to_string(&weekly_only).unwrap();
        assert!(
            json.contains("\"session\":null"),
            "T5: an absent session must be written as null: {json}"
        );
        for absent in ["session_resets_at", "weekly_resets_at", "severity", "spend"] {
            assert!(!json.contains(absent), "T5: absent optional leaked: {json}");
        }
        assert_eq!(serde_json::from_str::<Sample>(&json).unwrap(), weekly_only);
        let session_only = Sample::new(1_700_000_000, "claude", "work", Some(0.1), None);
        let json = serde_json::to_string(&session_only).unwrap();
        assert!(
            json.contains("\"weekly\":null"),
            "T5: an absent weekly must be written as null: {json}"
        );
        for absent in ["session_resets_at", "weekly_resets_at", "severity", "spend"] {
            assert!(!json.contains(absent), "T5: absent optional leaked: {json}");
        }
        assert_eq!(serde_json::from_str::<Sample>(&json).unwrap(), session_only);
    }

    // --- issue #1598: an absent quota window is written and read as `null` ------------

    #[test]
    fn a_weekly_only_line_decodes_with_its_session_reading_absent() {
        let line = r#"{"ts":0,"provider":"claude","acct":"work","session":null,"weekly":0.1}"#;
        let s: Sample = serde_json::from_str(line).expect("T1: a weekly-only line must decode");
        assert_eq!(
            optional_fraction(s.session),
            None,
            "T1: the session reading must be absent"
        );
        let weekly = optional_fraction(s.weekly).expect("T1: the weekly reading must be present");
        assert!(
            (weekly - 0.1).abs() < 1e-9,
            "T1: the weekly reading must read as written"
        );
    }

    #[test]
    fn a_session_only_line_decodes_with_its_weekly_reading_absent() {
        let line = r#"{"ts":0,"provider":"claude","acct":"work","session":0.42,"weekly":null}"#;
        let s: Sample = serde_json::from_str(line).expect("T2: a session-only line must decode");
        let session =
            optional_fraction(s.session).expect("T2: the session reading must be present");
        assert!(
            (session - 0.42).abs() < 1e-9,
            "T2: the session reading must read as written"
        );
        assert_eq!(
            optional_fraction(s.weekly),
            None,
            "T2: the weekly reading must be absent"
        );
    }

    #[test]
    fn a_weekly_only_line_round_trips_with_the_session_key_written_as_null() {
        let line = r#"{"ts":0,"provider":"claude","acct":"work","session":null,"weekly":0.1}"#;
        let decoded: Sample =
            serde_json::from_str(line).expect("T3: a weekly-only line must decode");
        let encoded =
            serde_json::to_string(&decoded).expect("T3: a weekly-only sample must serialize");
        let value: serde_json::Value =
            serde_json::from_str(&encoded).expect("T3: the re-encoded line must parse");
        assert_eq!(
            sorted_keys(&value),
            ["acct", "provider", "session", "ts", "weekly"],
            "T3: the session key must be written, never omitted: {encoded}"
        );
        assert!(
            encoded.contains("\"session\":null"),
            "T3: the absent session must be written as null: {encoded}"
        );
        let again: Sample =
            serde_json::from_str(&encoded).expect("T3: the re-encoded line must decode");
        assert_eq!(
            again, decoded,
            "T3: the re-encoded line must decode to the first decode"
        );
    }

    #[test]
    fn a_line_omitting_the_session_key_reads_as_no_session_reading() {
        let line = r#"{"ts":0,"provider":"claude","acct":"work","weekly":0.1}"#;
        let s: Sample =
            serde_json::from_str(line).expect("T4: a line omitting the session key must decode");
        assert_eq!(
            optional_fraction(s.session),
            None,
            "T4: an omitted session key must read as no session reading"
        );
        assert_eq!(
            optional_fraction(s.weekly),
            Some(0.1),
            "T4: and the weekly reading the line carries is read"
        );
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
        // The raw file is 0600 (created through the private-file path). Only this ASSERTION is
        // Unix-gated, not the test around it (issue #974): everything above is target-neutral.
        #[cfg(unix)]
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
            session: Some(DayStat {
                max: 0.9,
                mean: 0.5,
                p95: 0.85,
                cap_hits: 0,
                samples: None,
            }),
            weekly: Some(DayStat {
                max: 0.4,
                mean: 0.3,
                p95: 0.38,
                cap_hits: 0,
                samples: None,
            }),
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

    /// The rollup `fd8640b`'s `write_rollup` emitted, both axes in every bucket and no count:
    /// the D6 ruling's committed fixture for the first read after the upgrade (issue #1598,
    /// condition C1). Inline on purpose, never a file under `build/fixtures/`.
    const FD8640B_ROLLUP: &str = r#"{"rolled_through_ts":9,"hourly":[{"hour_start":0,"count":2,"session":{"max":0.5,"mean":0.4},"weekly":{"max":0.8,"mean":0.7}}],"daily":[{"day_start":0,"count":2,"coverage":0.5,"session":{"max":0.5,"mean":0.4,"p95":0.5,"cap_hits":0},"weekly":{"max":0.8,"mean":0.7,"p95":0.8,"cap_hits":0}}]}"#;

    #[test]
    fn a_rollup_written_before_the_lift_reads_back_with_both_axes_present() {
        let dir = tempfile::tempdir().unwrap();
        let (_, rollup_path) = store_paths(dir.path());
        fs::write(&rollup_path, FD8640B_ROLLUP).unwrap();

        let rollup = read_rollup(&rollup_path)
            .expect("T10: a rollup written before the lift must read back");
        assert_eq!(
            rollup.rolled_through_ts, 9,
            "T10: the watermark must read as written"
        );
        assert_eq!(
            rollup.hourly.len(),
            1,
            "T10: the hourly bucket must read back"
        );
        let hour = &rollup.hourly[0];
        assert_eq!(
            (hour.hour_start, hour.count),
            (0, 2),
            "T10: the hour's key and count must read as written"
        );
        for (axis, stat, max, mean) in [
            ("session", hour.session.as_ref(), 0.5, 0.4),
            ("weekly", hour.weekly.as_ref(), 0.8, 0.7),
        ] {
            let stat =
                stat.unwrap_or_else(|| panic!("T10: the hour's {axis} statistic must be present"));
            assert!(
                (stat.max - max).abs() < 1e-9 && (stat.mean - mean).abs() < 1e-9,
                "T10: the hour's {axis} statistic must read as written: {stat:?}"
            );
            assert_eq!(
                stat.samples, None,
                "T10: the hour's {axis} statistic must read as not recorded"
            );
        }
        assert_eq!(
            rollup.daily.len(),
            1,
            "T10: the daily bucket must read back"
        );
        let day = &rollup.daily[0];
        assert_eq!(
            (day.day_start, day.count),
            (0, 2),
            "T10: the day's key and count must read as written"
        );
        assert!(
            (day.coverage - 0.5).abs() < 1e-9,
            "T10: the day's coverage must read as written"
        );
        for (axis, stat, max, mean, p95) in [
            ("session", day.session.as_ref(), 0.5, 0.4, 0.5),
            ("weekly", day.weekly.as_ref(), 0.8, 0.7, 0.8),
        ] {
            let stat =
                stat.unwrap_or_else(|| panic!("T10: the day's {axis} statistic must be present"));
            assert!(
                (stat.max - max).abs() < 1e-9
                    && (stat.mean - mean).abs() < 1e-9
                    && (stat.p95 - p95).abs() < 1e-9,
                "T10: the day's {axis} statistic must read as written: {stat:?}"
            );
            assert_eq!(
                stat.cap_hits, 0,
                "T10: the day's {axis} cap_hits must read as written"
            );
            assert_eq!(
                stat.samples, None,
                "T10: the day's {axis} statistic must read as not recorded"
            );
        }

        // Re-encoded, every bucket keeps both axes and no statistic gains a count: the text
        // parses to the same JSON value as the input (RV-04).
        let reencoded = serde_json::to_string(&rollup).expect("T10: the rollup must re-encode");
        let value: serde_json::Value = serde_json::from_str(&reencoded).unwrap();
        let buckets = value["hourly"]
            .as_array()
            .expect("T10: the re-encoded hourly tier")
            .iter()
            .chain(
                value["daily"]
                    .as_array()
                    .expect("T10: the re-encoded daily tier"),
            );
        for bucket in buckets {
            for axis in ["session", "weekly"] {
                let stat = bucket.get(axis).unwrap_or_else(|| {
                    panic!("T10: a re-encoded bucket must keep its {axis} statistic: {reencoded}")
                });
                assert!(
                    stat.get("samples").is_none(),
                    "T10: re-encoding must add no {axis} count: {reencoded}"
                );
            }
        }
        assert_eq!(
            value,
            serde_json::from_str::<serde_json::Value>(FD8640B_ROLLUP).unwrap(),
            "T10: re-encoding must neither add nor lose a field"
        );
    }

    #[test]
    fn a_rollup_without_the_count_reads_every_statistic_as_not_recorded() {
        // #1610's shape two: one axis omitted in a bucket, and one statistic present in a
        // bucket of larger `count`, all without a recorded count (issue #1598, absorbing
        // #1610's `buckets-written-before-the-field-read-as-not-recorded`).
        let shape_two = r#"{"rolled_through_ts":9,"hourly":[{"hour_start":0,"count":4,"weekly":{"max":0.76,"mean":0.73}},{"hour_start":3600,"count":4,"session":{"max":0.5,"mean":0.4},"weekly":{"max":0.76,"mean":0.73}}],"daily":[]}"#;
        let dir = tempfile::tempdir().unwrap();
        let (_, rollup_path) = store_paths(dir.path());
        fs::write(&rollup_path, shape_two).unwrap();

        let rollup = read_rollup(&rollup_path)
            .expect("AC-25: a rollup written without the count must read back");
        assert_eq!(
            rollup.hourly.len(),
            2,
            "AC-25: both hourly buckets must read back"
        );
        assert_eq!(
            rollup.hourly[0].session, None,
            "AC-25: the first hour's omitted session axis must stay absent"
        );
        let statistics = [
            ("hour 0 weekly", rollup.hourly[0].weekly.as_ref()),
            ("hour 3600 session", rollup.hourly[1].session.as_ref()),
            ("hour 3600 weekly", rollup.hourly[1].weekly.as_ref()),
        ];
        for (what, stat) in statistics {
            let stat =
                stat.unwrap_or_else(|| panic!("AC-25: the {what} statistic must be present"));
            assert_eq!(
                stat.samples, None,
                "AC-25: the {what} statistic must read as not recorded, never as the bucket's count or 0"
            );
        }
        assert!(
            rollup.daily.is_empty(),
            "AC-25: the empty daily tier must read back empty"
        );

        let reencoded = serde_json::to_string(&rollup).expect("AC-25: the rollup must re-encode");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&reencoded).unwrap(),
            serde_json::from_str::<serde_json::Value>(shape_two).unwrap(),
            "AC-25: re-encoding must gain no count and lose no field: {reencoded}"
        );
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
                    session: Some(DayStat {
                        max: 0.99,
                        mean: 0.55,
                        p95: 0.92,
                        cap_hits: 2,
                        samples: None,
                    }),
                    weekly: Some(DayStat {
                        max: 0.61,
                        mean: 0.40,
                        p95: 0.58,
                        cap_hits: 0,
                        samples: None,
                    }),
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

    // Unix-only: it asserts the mode bits `crate::file_policy` writes on this target. The
    // property is cross-platform; the Windows arm of it — an explicit, PROTECTED DACL — is
    // asserted by that module's own `#[cfg(windows)]` tests (issue #974 AC3/AC4).
    #[cfg(unix)]
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
        let session = bucket.session.as_ref().expect("session readings present");
        assert!((session.max - 1.2).abs() < 1e-9, "max");
        assert!((session.mean - 0.7).abs() < 1e-9, "mean");
        // Nearest-rank p95 of 6 values: ceil(0.95*6)=6 → the 6th (largest) = 1.2.
        assert!((session.p95 - 1.2).abs() < 1e-9, "p95");
        assert_eq!(session.cap_hits, 2, "1.0 and 1.2 are cap hits");
        // Coverage = 6 observed ÷ (86400/300 = 288 expected).
        assert!((bucket.coverage - 6.0 / 288.0).abs() < 1e-9, "coverage");
    }

    // --- issue #1598: weekly-only lines survive compaction and fold honestly ------------

    /// The raw file T8a and T8b compact, in file order (issue #1598 § 6): two aged days, each
    /// wholly older than the default 14-day raw window at `200 * DAY_SECS` — day 170 holds
    /// four weekly-only readings, day 171 two weekly-only readings and two with both axes —
    /// then a weekly-only cohort inside the window. `14695200` is
    /// `170 * DAY_SECS + 2 * HOUR_SECS`, `14781600` is `171 * DAY_SECS + 2 * HOUR_SECS`, and
    /// `17107200` is `200 * DAY_SECS - 2 * DAY_SECS`.
    const WEEKLY_ONLY_STORE_LINES: [&str; 11] = [
        r#"{"ts":14695200,"provider":"claude","acct":"work","session":null,"weekly":0.6}"#,
        r#"{"ts":14695800,"provider":"claude","acct":"work","session":null,"weekly":0.62}"#,
        r#"{"ts":14696400,"provider":"claude","acct":"work","session":null,"weekly":0.64}"#,
        r#"{"ts":14697000,"provider":"claude","acct":"work","session":null,"weekly":0.66}"#,
        r#"{"ts":14781600,"provider":"claude","acct":"work","session":null,"weekly":0.7}"#,
        r#"{"ts":14782200,"provider":"claude","acct":"work","session":null,"weekly":0.72}"#,
        r#"{"ts":14782800,"provider":"claude","acct":"work","session":0.3,"weekly":0.74}"#,
        r#"{"ts":14783400,"provider":"claude","acct":"work","session":0.5,"weekly":0.76}"#,
        r#"{"ts":17107200,"provider":"claude","acct":"work","session":null,"weekly":0.8}"#,
        r#"{"ts":17107800,"provider":"claude","acct":"work","session":null,"weekly":0.81}"#,
        r#"{"ts":17108400,"provider":"claude","acct":"work","session":null,"weekly":0.82}"#,
    ];

    /// A temp store holding `WEEKLY_ONLY_STORE_LINES`, compacted once at `now = 200 * DAY_SECS`
    /// under the default policy. Returns the temp dir, which must outlive the two paths.
    fn compacted_weekly_only_store() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf)
    {
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, rollup_path) = store_paths(dir.path());
        write_raw_lines(&samples_path, &WEEKLY_ONLY_STORE_LINES);
        compact_and_roll(
            &samples_path,
            &rollup_path,
            200 * DAY_SECS,
            &RetentionPolicy::default(),
        )
        .expect("the weekly-only store must compact");
        (dir, samples_path, rollup_path)
    }

    #[test]
    fn compaction_retains_weekly_only_lines_and_leaves_the_rollup_readable() {
        let (_dir, samples_path, rollup_path) = compacted_weekly_only_store();

        let raw = fs::read_to_string(&samples_path).unwrap();
        let lines: Vec<serde_json::Value> = raw
            .lines()
            .map(|line| serde_json::from_str(line).expect("T8a: every retained line must parse"))
            .collect();
        let retained: Vec<i64> = lines
            .iter()
            .map(|line| {
                line["ts"]
                    .as_i64()
                    .expect("T8a: a retained line must carry its ts")
            })
            .collect();
        assert_eq!(
            retained,
            [17_107_200, 17_107_800, 17_108_400],
            "T8a: the weekly-only lines must survive compaction"
        );
        for (line, weekly) in lines.iter().zip([0.8, 0.81, 0.82]) {
            assert_eq!(
                line.get("session"),
                Some(&serde_json::Value::Null),
                "T8a: a retained line must keep its session key, as null: {line}"
            );
            let kept = line["weekly"]
                .as_f64()
                .expect("T8a: the weekly reading must survive compaction");
            assert!(
                (kept - weekly).abs() < 1e-9,
                "T8a: the weekly reading must survive as written: {line}"
            );
            assert_eq!(
                sorted_keys(line),
                ["acct", "provider", "session", "ts", "weekly"],
                "T8a: a retained line must keep the exact key set: {line}"
            );
        }
        assert_eq!(
            read_samples(&samples_path).unwrap().len(),
            3,
            "T8a: every retained line must read back"
        );

        let rollup = read_rollup(&rollup_path).expect("T8a: the rollup must stay readable");
        let text = fs::read_to_string(&rollup_path).unwrap();
        for token in ["null", "inf", "NaN"] {
            assert!(
                !text.contains(token),
                "T8a: the rollup must carry no `{token}`: {text}"
            );
        }
        let days: Vec<i64> = rollup.daily.iter().map(|d| d.day_start).collect();
        assert_eq!(
            days,
            [14_688_000, 14_774_400],
            "T8a: both aged days must fold into the daily tier"
        );
        let hours: Vec<i64> = rollup.hourly.iter().map(|h| h.hour_start).collect();
        assert_eq!(
            hours,
            [14_695_200, 14_781_600],
            "T8a: both aged hours must fold into the hourly tier"
        );
    }

    #[test]
    fn a_bucket_whose_readings_carry_no_session_window_omits_the_session_statistic() {
        let (_dir, _, rollup_path) = compacted_weekly_only_store();
        let rollup = read_rollup(&rollup_path).expect("T8b: the rollup must read back");
        let close = |actual: f64, expected: f64| (actual - expected).abs() < 1e-9;
        let day = |start: i64| {
            rollup
                .daily
                .iter()
                .find(|d| d.day_start == start)
                .unwrap_or_else(|| panic!("T8b: day {start} must fold"))
        };
        let hour = |start: i64| {
            rollup
                .hourly
                .iter()
                .find(|h| h.hour_start == start)
                .unwrap_or_else(|| panic!("T8b: hour {start} must fold"))
        };
        // Expected (max, mean, p95, cap_hits, samples) of a daily statistic.
        let assert_day = |stat: &DayStat, expected: (f64, f64, f64, u32, u32), what: &str| {
            let (max, mean, p95, cap_hits, samples) = expected;
            assert!(
                close(stat.max, max) && close(stat.mean, mean) && close(stat.p95, p95),
                "T8b: {what} is {stat:?}, expected max {max}, mean {mean}, p95 {p95}"
            );
            assert_eq!(stat.cap_hits, cap_hits, "T8b: {what} cap_hits");
            assert_eq!(stat.samples, Some(samples), "T8b: {what} samples");
        };
        // Expected (max, mean, samples) of an hourly statistic.
        let assert_hour = |stat: &HourStat, expected: (f64, f64, u32), what: &str| {
            let (max, mean, samples) = expected;
            assert!(
                close(stat.max, max) && close(stat.mean, mean),
                "T8b: {what} is {stat:?}, expected max {max}, mean {mean}"
            );
            assert_eq!(stat.samples, Some(samples), "T8b: {what} samples");
        };

        // Day 170 and its hour: four readings, none of them with a session window.
        let d = day(14_688_000);
        assert_eq!(
            d.count, 4,
            "T8b: day 14688000 count is its whole sample count"
        );
        assert!(
            close(d.coverage, 4.0 / 288.0),
            "T8b: day 14688000 coverage is over its whole sample count"
        );
        assert_eq!(
            d.session, None,
            "T8b: day 14688000 carried no session reading, so it has no session statistic"
        );
        let weekly = d
            .weekly
            .as_ref()
            .expect("T8b: day 14688000 weekly statistic");
        assert_day(weekly, (0.66, 0.63, 0.66, 0, 4), "day 14688000 weekly");
        let h = hour(14_695_200);
        assert_eq!(
            h.count, 4,
            "T8b: hour 14695200 count is its whole sample count"
        );
        assert_eq!(
            h.session, None,
            "T8b: hour 14695200 carried no session reading, so it has no session statistic"
        );
        let weekly = h
            .weekly
            .as_ref()
            .expect("T8b: hour 14695200 weekly statistic");
        assert_hour(weekly, (0.66, 0.63, 4), "hour 14695200 weekly");

        // Day 171 and its hour: four readings, two of them with a session window.
        let d = day(14_774_400);
        assert_eq!(
            d.count, 4,
            "T8b: day 14774400 count is its whole sample count"
        );
        assert!(
            close(d.coverage, 4.0 / 288.0),
            "T8b: day 14774400 coverage is over its whole sample count"
        );
        let session = d
            .session
            .as_ref()
            .expect("T8b: day 14774400 session statistic");
        assert_day(session, (0.5, 0.4, 0.5, 0, 2), "day 14774400 session");
        let weekly = d
            .weekly
            .as_ref()
            .expect("T8b: day 14774400 weekly statistic");
        assert_day(weekly, (0.76, 0.73, 0.76, 0, 4), "day 14774400 weekly");
        let h = hour(14_781_600);
        assert_eq!(
            h.count, 4,
            "T8b: hour 14781600 count is its whole sample count"
        );
        let session = h
            .session
            .as_ref()
            .expect("T8b: hour 14781600 session statistic");
        assert_hour(session, (0.5, 0.4, 2), "hour 14781600 session");
        let weekly = h
            .weekly
            .as_ref()
            .expect("T8b: hour 14781600 weekly statistic");
        assert_hour(weekly, (0.76, 0.73, 4), "hour 14781600 weekly");

        assert_eq!(
            rollup.rolled_through_ts, 14_783_400,
            "T8b: the watermark is the newest folded reading"
        );
    }

    #[test]
    fn count_invariants_hold_for_every_folded_bucket() {
        // Five aged days, each one hour of four readings at two hours past midnight. As the
        // stub's outline gives it, one axis is carried by none, exactly one or all four of a
        // day's readings while the other axis is carried by all four, so each tier sees the
        // none / exactly-one / all-four boundary on both axes and readings carrying both axes
        // sit beside readings carrying one (issue #1598, absorbing #1610's
        // `count-invariants-hold-for-every-folded-bucket`). Each pair is (session, weekly).
        let carried_by: [(u32, u32); 5] = [(0, 4), (1, 4), (4, 4), (4, 0), (4, 1)];
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, rollup_path) = store_paths(dir.path());
        let reading = |present: bool, fraction: f64| {
            if present {
                fraction.to_string()
            } else {
                "null".to_owned()
            }
        };
        let mut lines = Vec::new();
        for (day, &(session_n, weekly_n)) in (170_i64..).zip(&carried_by) {
            let hour = day * DAY_SECS + 2 * HOUR_SECS;
            for k in 0..4_u32 {
                lines.push(format!(
                    r#"{{"ts":{},"provider":"claude","acct":"work","session":{},"weekly":{}}}"#,
                    hour + i64::from(k) * 600,
                    reading(k < session_n, 0.1 + 0.1 * f64::from(k)),
                    reading(k < weekly_n, 0.5 + 0.1 * f64::from(k)),
                ));
            }
        }
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_raw_lines(&samples_path, &lines);
        compact_and_roll(
            &samples_path,
            &rollup_path,
            200 * DAY_SECS,
            &RetentionPolicy::default(),
        )
        .expect("AC-24: the five aged days must compact");
        let rollup = read_rollup(&rollup_path).expect("AC-24: the rollup must read back");

        for (day, &(session_n, weekly_n)) in (170_i64..).zip(&carried_by) {
            let daily = rollup
                .daily
                .iter()
                .find(|d| d.day_start == day * DAY_SECS)
                .unwrap_or_else(|| panic!("AC-24: day {day} must fold"));
            let hourly = rollup
                .hourly
                .iter()
                .find(|h| h.hour_start == day * DAY_SECS + 2 * HOUR_SECS)
                .unwrap_or_else(|| panic!("AC-24: day {day}'s hour must fold"));
            assert_eq!(
                (daily.count, hourly.count),
                (4, 4),
                "AC-24: day {day}: a bucket's count is every sample, whichever windows it carried"
            );
            // (tier, axis, readings that carried it, the bucket's count, (samples, max, mean)).
            let statistics = [
                (
                    "daily",
                    "session",
                    session_n,
                    daily.count,
                    daily.session.as_ref().map(|s| (s.samples, s.max, s.mean)),
                ),
                (
                    "daily",
                    "weekly",
                    weekly_n,
                    daily.count,
                    daily.weekly.as_ref().map(|s| (s.samples, s.max, s.mean)),
                ),
                (
                    "hourly",
                    "session",
                    session_n,
                    hourly.count,
                    hourly.session.as_ref().map(|s| (s.samples, s.max, s.mean)),
                ),
                (
                    "hourly",
                    "weekly",
                    weekly_n,
                    hourly.count,
                    hourly.weekly.as_ref().map(|s| (s.samples, s.max, s.mean)),
                ),
            ];
            for (tier, axis, carried, count, statistic) in statistics {
                match (carried, statistic) {
                    (0, None) => {}
                    (0, Some(statistic)) => panic!(
                        "AC-24: day {day} {tier} {axis}: no reading carried the axis, yet a statistic was folded: {statistic:?}"
                    ),
                    (_, None) => panic!(
                        "AC-24: day {day} {tier} {axis}: {carried} readings carried the axis, yet no statistic was folded"
                    ),
                    (_, Some((samples, max, mean))) => {
                        assert_eq!(
                            samples,
                            Some(carried),
                            "AC-24: day {day} {tier} {axis}: the count must be the readings that carried the axis"
                        );
                        assert!(
                            (1..=count).contains(&carried),
                            "AC-24: day {day} {tier} {axis}: a recorded count is at least 1 and at most the bucket's count"
                        );
                        if carried == 1 {
                            assert!(
                                (max - mean).abs() < 1e-12,
                                "AC-24: day {day} {tier} {axis}: a statistic over one reading has max equal to mean"
                            );
                        }
                    }
                }
            }
        }
    }

    // --- AC 5: store carries redacted handles + provenance-scoped labels only -----
    //
    // The store's `acct` is the account's roster label (copied verbatim by the
    // daemon collector). Since #444 legitimized an operator-*authored* email label,
    // and #447 makes such labels common (the capture prompt pre-fills the email),
    // the store's bar is the SAME provenance-scoped rule the render/event channels
    // carry: NO UNAUTHORED email (a stranger's address, a credential-blob spill) may
    // reach the store, but an operator-authored email label may. Tokens are NEVER
    // waived. See `redaction::meter::unauthored_emails`.

    #[test]
    fn persisted_store_carries_no_unauthored_email_or_token() {
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
                // No authored labels here (the fixture uses the `work` handle), so an
                // empty allow-set is the strict bar: any `@`-shape would be UNAUTHORED
                // and fail — identical strength to the former `!contains('@')`, now in
                // the provenance vocabulary rather than a blanket no-`@` claim.
                assert!(
                    crate::redaction::meter::unauthored_emails(&text, &[]).is_empty(),
                    "no unauthored email may reach the store: {text}"
                );
                assert!(
                    !text.contains("sk-ant"),
                    "no token may reach the store: {text}"
                );
            }
        }
    }

    #[test]
    fn persisted_store_permits_an_operator_authored_email_label() {
        // #447: once the capture prompt pre-fills the email, an operator-authored
        // email label reaches the raw samples tier verbatim (the daemon copies the
        // roster label into `Sample.acct`). That authored email is PERMITTED — the
        // same provenance-scoped waiver #444 established for the render/event channels
        // — while a stray UNAUTHORED email would still fail.
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, rollup_path) = store_paths(dir.path());
        let authored = "alice@example.com";
        append_sample(
            &samples_path,
            &Sample::new(1_700_000_000, "claude", authored, Some(0.9), Some(0.7))
                .with_severity(Some("critical".to_owned())),
        )
        .unwrap();

        // Assert on the RAW samples file — where `acct` is written verbatim — BEFORE
        // any compaction (which folds samples into the acct-free rollup tier below).
        let samples = fs::read_to_string(&samples_path).unwrap();
        // The authored email label IS carried by the raw store (runtime honesty — the
        // former `!contains('@')` bar was only green because fixtures used handles).
        assert!(
            samples.contains(authored),
            "the authored email label reaches the store: {samples}"
        );
        // Provenance-scoped: permitted WHEN authored…
        assert!(
            crate::redaction::meter::unauthored_emails(&samples, &[authored]).is_empty(),
            "an operator-authored email label is permitted: {samples}"
        );
        // …and the assertion is not vacuous: WITHOUT the provenance allow-set the very
        // same bytes surface as a leaking (unauthored) email shape.
        assert_eq!(
            crate::redaction::meter::unauthored_emails(&samples, &[]),
            vec![authored.to_owned()],
            "without provenance the label reads as an unauthored email: {samples}"
        );
        // Tokens are never waived, authored or not.
        assert!(!samples.contains("sk-ant"), "no token: {samples}");

        // The aggregate rollup tier drops `acct` entirely (it buckets by time only),
        // so it is email-free by construction — the label never reaches the
        // lowest-resolution, lifetime-kept tier, authored or not.
        compact_and_roll(
            &samples_path,
            &rollup_path,
            1_700_000_000 + 400 * DAY_SECS,
            &RetentionPolicy::default(),
        )
        .unwrap();
        let rollup = fs::read_to_string(&rollup_path).unwrap();
        assert!(
            !rollup.contains('@'),
            "the acct-free rollup carries no label at all: {rollup}"
        );
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

    #[test]
    fn a_same_key_merge_keeps_an_absent_axis_absent() {
        // The merge arms over operands with no recorded count (issue #1598): the bucket
        // counts 2 and 2 weight a pooled mean.
        let a = HourStat {
            max: 0.5,
            mean: 0.4,
            samples: None,
        };
        let b = HourStat {
            max: 0.7,
            mean: 0.6,
            samples: None,
        };
        assert_eq!(
            merge_hour_stat(None, 2, None, 2),
            None,
            "T11: two absent hour statistics merge to an absent one"
        );
        assert_eq!(
            merge_hour_stat(Some(&a), 2, None, 2),
            Some(a.clone()),
            "T11: an absent side leaves the present hour statistic unchanged"
        );
        assert_eq!(
            merge_hour_stat(None, 2, Some(&a), 2),
            Some(a.clone()),
            "T11: an absent side leaves the present hour statistic unchanged, in either order"
        );
        let merged = merge_hour_stat(Some(&a), 2, Some(&b), 2)
            .expect("T11: two present hour statistics merge to one");
        assert!(
            (merged.max - 0.7).abs() < 1e-9 && (merged.mean - 0.5).abs() < 1e-9,
            "T11: the merged hour statistic is {merged:?}, expected max 0.7, mean 0.5"
        );
        assert_eq!(
            merged.samples, None,
            "T11: neither hour operand recorded a count, so the merge records none"
        );

        let c = DayStat {
            max: 0.5,
            mean: 0.4,
            p95: 0.5,
            cap_hits: 0,
            samples: None,
        };
        let d = DayStat {
            max: 1.2,
            mean: 0.6,
            p95: 1.2,
            cap_hits: 1,
            samples: None,
        };
        assert_eq!(
            merge_day_stat(None, 2, None, 2),
            None,
            "T11: two absent day statistics merge to an absent one"
        );
        assert_eq!(
            merge_day_stat(Some(&c), 2, None, 2),
            Some(c.clone()),
            "T11: an absent side leaves the present day statistic unchanged"
        );
        assert_eq!(
            merge_day_stat(None, 2, Some(&c), 2),
            Some(c.clone()),
            "T11: an absent side leaves the present day statistic unchanged, in either order"
        );
        let merged = merge_day_stat(Some(&c), 2, Some(&d), 2)
            .expect("T11: two present day statistics merge to one");
        assert!(
            (merged.max - 1.2).abs() < 1e-9
                && (merged.mean - 0.5).abs() < 1e-9
                && (merged.p95 - 1.2).abs() < 1e-9,
            "T11: the merged day statistic is {merged:?}, expected max 1.2, mean 0.5, p95 1.2"
        );
        assert_eq!(
            merged.cap_hits, 1,
            "T11: the merged day statistic sums cap_hits"
        );
        assert_eq!(
            merged.samples, None,
            "T11: neither day operand recorded a count, so the merge records none"
        );
    }

    #[test]
    fn a_same_key_merge_weights_each_axis_by_its_recorded_count() {
        // Both counts recorded (issue #1598, absorbing #1610's
        // `same-key-merge-weights-each-axis-by-its-recorded-count`): each operand's bucket
        // counts 4, its statistic is over 2 and 4 readings.
        let a = HourStat {
            max: 0.5,
            mean: 0.4,
            samples: Some(2),
        };
        let b = HourStat {
            max: 0.9,
            mean: 0.8,
            samples: Some(4),
        };
        let merged = merge_hour_stat(Some(&a), 4, Some(&b), 4)
            .expect("AC-26: two present hour statistics merge to one");
        assert!(
            (merged.max - 0.9).abs() < 1e-9 && (merged.mean - 2.0 / 3.0).abs() < 1e-9,
            "AC-26: the hour merge must weight each mean by its recorded count, (0.4·2 + 0.8·4) / 6, not by the bucket counts: {merged:?}"
        );
        assert_eq!(
            merged.samples,
            Some(6),
            "AC-26: the merged hour statistic records the sum of the two counts"
        );
        let c = DayStat {
            max: 0.5,
            mean: 0.4,
            p95: 0.5,
            cap_hits: 0,
            samples: Some(2),
        };
        let d = DayStat {
            max: 1.2,
            mean: 0.8,
            p95: 1.2,
            cap_hits: 1,
            samples: Some(4),
        };
        let merged = merge_day_stat(Some(&c), 4, Some(&d), 4)
            .expect("AC-26: two present day statistics merge to one");
        assert!(
            (merged.max - 1.2).abs() < 1e-9
                && (merged.mean - 2.0 / 3.0).abs() < 1e-9
                && (merged.p95 - 1.2).abs() < 1e-9,
            "AC-26: the day merge must weight each mean by its recorded count: {merged:?}"
        );
        assert_eq!(
            merged.cap_hits, 1,
            "AC-26: the merged day statistic sums cap_hits"
        );
        assert_eq!(
            merged.samples,
            Some(6),
            "AC-26: the merged day statistic records the sum of the two counts"
        );

        // Either count not recorded: the bucket counts weight the mean, and the merged count
        // is not recorded.
        let a_unrecorded = HourStat {
            samples: None,
            ..a.clone()
        };
        let b_unrecorded = HourStat {
            samples: None,
            ..b.clone()
        };
        for (left, right) in [(&a_unrecorded, &b), (&a, &b_unrecorded)] {
            let merged = merge_hour_stat(Some(left), 4, Some(right), 4)
                .expect("AC-26: two present hour statistics merge to one");
            assert!(
                (merged.mean - 0.6).abs() < 1e-9,
                "AC-26: with a count not recorded, the bucket counts weight the hour mean: {merged:?}"
            );
            assert_eq!(
                merged.samples, None,
                "AC-26: with a count not recorded, the merged hour count is not recorded"
            );
        }
        let c_unrecorded = DayStat {
            samples: None,
            ..c.clone()
        };
        let d_unrecorded = DayStat {
            samples: None,
            ..d.clone()
        };
        for (left, right) in [(&c_unrecorded, &d), (&c, &d_unrecorded)] {
            let merged = merge_day_stat(Some(left), 4, Some(right), 4)
                .expect("AC-26: two present day statistics merge to one");
            assert!(
                (merged.mean - 0.6).abs() < 1e-9,
                "AC-26: with a count not recorded, the bucket counts weight the day mean: {merged:?}"
            );
            assert_eq!(
                merged.samples, None,
                "AC-26: with a count not recorded, the merged day count is not recorded"
            );
        }

        // One side absent: the present side unchanged, count included, in both orders. Both
        // absent: absent.
        assert_eq!(merge_hour_stat(Some(&a), 4, None, 4), Some(a.clone()));
        assert_eq!(merge_hour_stat(None, 4, Some(&a), 4), Some(a.clone()));
        assert_eq!(merge_hour_stat(None, 4, None, 4), None);
        assert_eq!(merge_day_stat(Some(&c), 4, None, 4), Some(c.clone()));
        assert_eq!(merge_day_stat(None, 4, Some(&c), 4), Some(c.clone()));
        assert_eq!(merge_day_stat(None, 4, None, 4), None);

        // The merged bucket's own count is the sum of the two, and its coverage is recomputed
        // from that sum.
        let mut rollup = Rollup::default();
        for stat in [&c, &d] {
            upsert_day(
                &mut rollup,
                DayBucket {
                    day_start: 0,
                    count: 4,
                    coverage: 4.0 / 288.0,
                    session: Some(stat.clone()),
                    weekly: None,
                },
                288.0,
            );
        }
        assert_eq!(
            rollup.daily.len(),
            1,
            "AC-26: a same-key day merges into one bucket"
        );
        let merged = &rollup.daily[0];
        assert_eq!(
            merged.count, 8,
            "AC-26: the merged bucket's count is the sum of the two"
        );
        assert!(
            (merged.coverage - 8.0 / 288.0).abs() < 1e-9,
            "AC-26: the merged bucket's coverage is recomputed from its count"
        );
        let session = merged
            .session
            .as_ref()
            .expect("AC-26: the merged bucket keeps its session statistic");
        assert!(
            (session.mean - 2.0 / 3.0).abs() < 1e-9,
            "AC-26: the merged bucket's session mean is weighted by the recorded counts: {session:?}"
        );
        assert_eq!(
            session.samples,
            Some(6),
            "AC-26: the merged bucket's session count"
        );
        assert_eq!(
            merged.weekly, None,
            "AC-26: an axis absent on both sides of the merge stays absent"
        );
    }

    /// The not-recorded arm over UNEQUAL bucket counts (issue #1598): with a count not recorded,
    /// each operand's own bucket count weights the pooled mean, so buckets of 1 and 3 samples
    /// pool `(0.4·1 + 0.8·3) / 4 = 0.7`, and the same operands with the counts the other way
    /// round pool `(0.4·3 + 0.8·1) / 4 = 0.5`. Every other merge test passes equal bucket
    /// counts, under which the plain mean 0.6, a dropped weight and a swapped weight all read
    /// the same; a legacy bucket read from a generation-1 rollup always takes this arm.
    #[test]
    fn a_not_recorded_merge_weights_the_mean_by_each_operand_s_own_bucket_count() {
        let a = HourStat {
            max: 0.5,
            mean: 0.4,
            samples: None,
        };
        let b = HourStat {
            max: 0.9,
            mean: 0.8,
            samples: Some(4),
        };
        for (a_count, b_count, expected) in [(1, 3, 0.7), (3, 1, 0.5)] {
            let merged = merge_hour_stat(Some(&a), a_count, Some(&b), b_count)
                .expect("two present hour statistics merge to one");
            assert!(
                (merged.mean - expected).abs() < 1e-9,
                "the hour mean is weighted by the bucket counts {a_count} and {b_count}: {merged:?}, expected {expected}"
            );
            assert_eq!(
                merged.samples, None,
                "with a count not recorded, the merged hour count is not recorded"
            );
        }
        let c = DayStat {
            max: 0.5,
            mean: 0.4,
            p95: 0.5,
            cap_hits: 0,
            samples: None,
        };
        let d = DayStat {
            max: 1.2,
            mean: 0.8,
            p95: 1.2,
            cap_hits: 1,
            samples: Some(4),
        };
        for (c_count, d_count, expected) in [(1, 3, 0.7), (3, 1, 0.5)] {
            let merged = merge_day_stat(Some(&c), c_count, Some(&d), d_count)
                .expect("two present day statistics merge to one");
            assert!(
                (merged.mean - expected).abs() < 1e-9,
                "the day mean is weighted by the bucket counts {c_count} and {d_count}: {merged:?}, expected {expected}"
            );
            assert_eq!(
                merged.samples, None,
                "with a count not recorded, the merged day count is not recorded"
            );
        }
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
        // Two handshakes replace the wall-clock pacing this test used to rely on
        // (issue #1492), which was load-dependent in both directions at once: a
        // yield short enough to keep the compactor colliding with the appender also
        // let it hold the unfairly-granted `flock` for ~the whole run, so on a loaded
        // machine the backing-off appender could miss for the entire
        // `STORE_LOCK_MAX_WAIT` and fail closed with `UsageStoreBusy`.
        //
        // 1. A capacity-0 rendezvous fixes the run's SHAPE: one compaction pass per
        //    append, so neither writer runs ahead and each queues behind at most ONE
        //    of the other's critical sections rather than a whole run of them. That
        //    is a structural bound on WHAT is waited for; the MARGIN it leaves is
        //    finite and load-dependent, so do not read it as unbounded headroom.
        //    Instrumenting `StoreLock::acquire` with a per-acquire retry counter, the
        //    worst single contended acquire consumed 1 of the 100-retry budget
        //    unloaded, 7 alongside three concurrent suites and 31 alongside seven —
        //    growing with load, never exhausted. The comments on `:113`/`:121` call
        //    these sections sub-millisecond, which holds for the daemon and not for a
        //    loaded test host; that gap is the margin, and a future change that
        //    lengthens a critical section spends it.
        // 2. The appender then holds each append until the pass it just released is
        //    demonstrably UNDERWAY, so the append is issued INSIDE the rewrite's
        //    read-modify-write rather than merely near it — the interleaving the lock
        //    exists to forbid. A rendezvous alone releases both threads together and
        //    an append wins by microseconds every time, which silently stops covering
        //    the guard in `append_sample`.
        //
        // Both arms of that wait are EVENTS, never an interval: an unavailable store
        // lock (probed with a zero budget, so it never blocks) means the pass is in
        // flight, and a completion on `done_rx` means it has finished. The compactor
        // must produce one or the other, so the wait cannot hang and there is no
        // duration to tune — which matters, because an interval short enough to
        // catch a compactor that locks is long enough to miss one that does not.
        // Completions are COUNTED rather than drained, so none is ever discarded.
        // Both `unwrap`s stay: a fail-closed refusal from either writer must still
        // fail this test loudly.
        //
        // The appends ALTERNATE between the two timings because neither covers both
        // halves of the lock. Measured 40 runs per cell against builds with one
        // guard removed in turn — the pre-#188 race, from each side:
        //
        //     guard removed from    early only   late only   alternating
        //     `compact_and_roll`      40/40        0/40         40/40
        //     `append_sample`         27/40       40/40         40/40
        //
        // An early-only append wins by microseconds and never enters the rewrite
        // window; a late-only one waits out a compactor that does not lock, so it
        // never overlaps either. The wall-clock pacing this replaced also scored
        // 40/40 on both, and failed 16 of 120 full-suite runs under concurrent load;
        // alternating holds 40/40 on both at 0 of 120.
        //
        // It costs time, and that is the price rather than an oversight: this test
        // runs ~1.6 s isolated against the predecessor's ~0.24 s, because an appender
        // that waits for a pass to be underway then queues behind it. Suite wall time
        // is unchanged (both ~5 s at default parallelism), so it is not a CI cost. Do
        // not trade the waiting away for a faster run — the waiting IS the exercise.
        let (tick_tx, tick_rx) = std::sync::mpsc::sync_channel::<()>(0);
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let appender = {
            let samples_path = samples_path.clone();
            std::thread::spawn(move || {
                let lock = lock_path(&samples_path);
                let mut passes_done: i64 = 0;
                for k in 0..N {
                    // Keep the completion count current so the guard below reflects
                    // the compactor's real progress rather than a stale reading.
                    while done_rx.try_recv().is_ok() {
                        passes_done += 1;
                    }
                    if k % 2 == 1 {
                        // `is_ok()` drops the probe's guard at the end of the
                        // condition, so a probe that wins the lock never holds it
                        // against the compaction it is waiting on. Note this arm
                        // READS the very guard the test checks: were the store lock
                        // ever a no-op, it would degenerate to the late timing, which
                        // scores 0/40 against the `compact_and_roll` mutant. The
                        // even-`k` appends are what cover that, so the alternation is
                        // load-bearing — do not simplify it to this branch alone.
                        while passes_done < k && StoreLock::acquire(&lock, Duration::ZERO).is_ok() {
                            if done_rx.try_recv().is_ok() {
                                passes_done += 1;
                            } else {
                                std::thread::yield_now();
                            }
                        }
                    }
                    append_sample(&samples_path, &sample(base + k, 0.1, 0.2)).unwrap();
                    // A dropped receiver means the compactor has already panicked and
                    // the main thread is unwinding, so the assertion below is
                    // unreachable and there is nothing left to append FOR. Stop:
                    // carrying on races `dir`'s removal and buries the real failure
                    // under a spurious `NotFound` from this thread (measured in 27 of
                    // 30 fault-injected runs before this `break`, 0 of 30 after).
                    if tick_tx.send(()).is_err() {
                        break;
                    }
                }
            })
        };
        // One pass per append; the loop ends when the appender drops its sender.
        for _ in tick_rx {
            compact_and_roll(&samples_path, &rollup_path, now, &policy).unwrap();
            let _ = done_tx.send(());
        }
        appender.join().unwrap();
        // A final pass, which settles NOTHING: the rendezvous orders the last append
        // before the last in-loop pass, so that pass already saw all N. It is kept as
        // a free idempotence check — a pass over the full retained window must not
        // change the count the assertion reads.
        compact_and_roll(&samples_path, &rollup_path, now, &policy).unwrap();
        let remaining = read_samples(&samples_path).unwrap();
        assert_eq!(
            remaining.len(),
            N as usize,
            "every concurrently-appended sample survived the compaction rewrites"
        );
    }

    // --- issue #1598: the rollup fence (decision D6, amendment 1) -----------------------

    #[test]
    fn every_rollup_write_is_the_format_2_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let alone = dir.path().join("written-by-write-rollup.json");
        write_rollup(&alone, &Rollup::default()).expect("T12: write_rollup must write");
        let (_store, _, compacted) = compacted_weekly_only_store();

        for (writer, path) in [("write_rollup", &alone), ("compact_and_roll", &compacted)] {
            let text = fs::read_to_string(path).unwrap();
            let file: serde_json::Value =
                serde_json::from_str(&text).expect("T12: a rollup file must parse");
            assert_eq!(
                sorted_keys(&file),
                ["format", "rollup"],
                "T12: {writer} must write the format-2 envelope: {text}"
            );
            assert_eq!(
                file["format"], 2,
                "T12: {writer} must write format 2: {text}"
            );
            assert_eq!(
                sorted_keys(&file["rollup"]),
                ["daily", "hourly", "rolled_through_ts"],
                "T12: {writer}'s envelope must hold the rollup itself: {text}"
            );
            let body: Rollup = serde_json::from_value(file["rollup"].clone())
                .expect("T12: the envelope's rollup must decode");
            assert_eq!(
                read_rollup(path).expect("T12: read_rollup must read the envelope"),
                body,
                "T12: read_rollup must read back what {writer} wrote"
            );
        }
        assert_eq!(
            read_rollup(&alone).unwrap(),
            Rollup::default(),
            "T12: read_rollup must read back the rollup write_rollup was given"
        );
    }

    #[test]
    fn a_corrupt_envelope_is_reported_at_its_own_position() {
        let dir = tempfile::tempdir().unwrap();
        let (_, rollup_path) = store_paths(dir.path());

        // A format-2 envelope whose body is corrupt: refused at the bad field's own position.
        fs::write(
            &rollup_path,
            r#"{"format":2,"rollup":{"rolled_through_ts":"x","hourly":[],"daily":[]}}"#,
        )
        .unwrap();
        match read_rollup(&rollup_path) {
            Err(Error::UsageRollupMalformed { line, column }) => {
                assert_eq!(
                    line, 1,
                    "T13: a corrupt envelope must be reported on its own line"
                );
                assert!(
                    column > 0,
                    "T13: a corrupt envelope must be reported at its own column, never 0"
                );
            }
            other => panic!("T13: a corrupt envelope must read as UsageRollupMalformed: {other:?}"),
        }

        // A `format` that is not a number: refused, never read as the legacy shape.
        fs::write(
            &rollup_path,
            r#"{"format":"2","rolled_through_ts":0,"hourly":[],"daily":[]}"#,
        )
        .unwrap();
        let non_numeric = read_rollup(&rollup_path);
        assert!(
            matches!(non_numeric, Err(Error::UsageRollupMalformed { .. })),
            "T13: a format that is not a number must be refused as malformed: {non_numeric:?}"
        );
    }

    // Frozen at `fd8640b`: the tier types every pre-#1598 build decodes. Never edit: issue
    // #1598's fence is graded against them.
    #[allow(dead_code)]
    mod fd8640b_tiers {
        use serde::Deserialize;

        /// The rolled-aggregate object — one atomically-rewritten JSON document holding the
        /// hourly and daily tiers plus the roll watermark.
        ///
        /// `rolled_through_ts` is the newest sample epoch already folded into the tiers: a
        /// sample with `ts <= rolled_through_ts` has been rolled and must never be folded
        /// again (the exactly-once guarantee). Non-secret throughout, so `Debug` is safe.
        #[derive(Deserialize)]
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
        #[derive(Deserialize)]
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
        #[derive(Deserialize)]
        pub(crate) struct HourStat {
            /// Peak utilisation fraction over the bucket.
            pub(crate) max: f64,
            /// Mean utilisation fraction over the bucket.
            pub(crate) mean: f64,
        }

        /// One day's aggregate: max / mean / p95 / cap-hits per dimension, plus a coverage
        /// ratio. The lowest-resolution tier — kept for the store's lifetime, so it is what
        /// a "lifetime" reader consults.
        #[derive(Deserialize)]
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
        #[derive(Deserialize)]
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
    }

    #[test]
    fn a_rollup_this_build_writes_is_refused_by_the_fd8640b_tier_types() {
        // (a) The rollup a compaction writes over an empty store.
        let empty = tempfile::tempdir().unwrap();
        let (empty_samples, empty_rollup) = store_paths(empty.path());
        compact_and_roll(
            &empty_samples,
            &empty_rollup,
            200 * DAY_SECS,
            &RetentionPolicy::default(),
        )
        .expect("T14: a compaction over an empty store must succeed");
        // (b) The rollup it writes over an aged cohort that carries both axes on every line,
        // so no bucket omits an axis.
        let both = tempfile::tempdir().unwrap();
        let (both_samples, both_rollup) = store_paths(both.path());
        write_raw_lines(
            &both_samples,
            &[
                r#"{"ts":14695200,"provider":"claude","acct":"work","session":0.3,"weekly":0.6}"#,
                r#"{"ts":14695800,"provider":"claude","acct":"work","session":0.5,"weekly":0.62}"#,
            ],
        );
        compact_and_roll(
            &both_samples,
            &both_rollup,
            200 * DAY_SECS,
            &RetentionPolicy::default(),
        )
        .expect("T14: a compaction over a both-axes store must succeed");

        let mut decoded_by_fd8640b = Vec::new();
        for (store, path) in [
            ("empty store", &empty_rollup),
            ("both-axes store", &both_rollup),
        ] {
            let bytes = fs::read(path).unwrap();
            match serde_json::from_slice::<fd8640b_tiers::Rollup>(&bytes) {
                Err(err) => assert!(
                    err.to_string().contains("missing field"),
                    "T14: the fd8640b tier types must refuse the {store}'s rollup for a missing field: {err}"
                ),
                Ok(_) => decoded_by_fd8640b.push(store),
            }
        }
        assert!(
            decoded_by_fd8640b.is_empty(),
            "T14: the fd8640b tier types decode the rollup this build writes for {decoded_by_fd8640b:?}, so an older build would compact past the fence"
        );
    }

    #[test]
    fn a_legacy_rollup_survives_its_first_fenced_compaction_bucket_for_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, rollup_path) = store_paths(dir.path());
        fs::write(&rollup_path, FD8640B_ROLLUP).unwrap();
        fs::write(&samples_path, "").unwrap();
        compact_and_roll(
            &samples_path,
            &rollup_path,
            DAY_SECS,
            &RetentionPolicy::default(),
        )
        .expect("T15: the first fenced compaction over a legacy rollup must succeed");

        // The legacy literal as `read_rollup` reads it, from a copy the pass never touched.
        let untouched = dir.path().join("fd8640b-rollup.json");
        fs::write(&untouched, FD8640B_ROLLUP).unwrap();
        let before = read_rollup(&untouched).expect("T15: the legacy literal must read back");
        let after = read_rollup(&rollup_path).expect("T15: the rewritten rollup must read back");
        assert_eq!(
            after.rolled_through_ts, before.rolled_through_ts,
            "T15: the watermark must survive the pass"
        );
        assert_eq!(
            after.hourly, before.hourly,
            "T15: the hourly tier must survive bucket for bucket"
        );
        assert_eq!(
            after.daily, before.daily,
            "T15: the daily tier must survive bucket for bucket"
        );
        let file: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&rollup_path).unwrap()).unwrap();
        assert_eq!(
            file["format"], 2,
            "T15: the pass must rewrite the rollup as format 2: {file}"
        );
    }

    #[test]
    fn a_newer_store_format_is_refused_and_both_files_are_left_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let (samples_path, rollup_path) = store_paths(dir.path());
        fs::write(
            &rollup_path,
            r#"{"format":3,"rollup":{"rolled_through_ts":9,"hourly":[],"daily":[]}}"#,
        )
        .unwrap();
        // One aged line and one inside the raw window, so a pass would both fold and rewrite.
        write_raw_lines(
            &samples_path,
            &[WEEKLY_ONLY_STORE_LINES[0], WEEKLY_ONLY_STORE_LINES[8]],
        );
        let samples_before = fs::read(&samples_path).unwrap();
        let rollup_before = fs::read(&rollup_path).unwrap();

        let compaction = compact_and_roll(
            &samples_path,
            &rollup_path,
            200 * DAY_SECS,
            &RetentionPolicy::default(),
        )
        .map(|_| ());
        let read = read_rollup(&rollup_path).map(|_| ());
        for (call, outcome) in [("compact_and_roll", compaction), ("read_rollup", read)] {
            let err = match outcome {
                Err(err) => err,
                Ok(()) => panic!("T16: {call} must refuse a newer store format"),
            };
            assert!(
                !matches!(err, Error::UsageRollupMalformed { .. }),
                "T16: {call} must not report a newer store as malformed: {err}"
            );
            let text = err.to_string();
            assert!(
                text.contains("newer sessiometer"),
                "T16: {call}'s error must say a newer sessiometer wrote the file: {text}"
            );
            assert!(
                text.contains("must not be deleted"),
                "T16: {call}'s error must say the file must not be deleted: {text}"
            );
        }
        assert_eq!(
            fs::read(&samples_path).unwrap(),
            samples_before,
            "T16: the raw file must be left byte-identical"
        );
        assert_eq!(
            fs::read(&rollup_path).unwrap(),
            rollup_before,
            "T16: the rollup file must be left byte-identical"
        );
    }

    /// One generation's pinned written shapes: see `GENERATION_2_SHAPE`.
    struct StoreShape {
        generation: u32,
        full_sample: &'static str,
        weekly_only_sample: &'static str,
        rollup: &'static str,
    }

    /// The written shape of each store file at generation 2, byte for byte: a sample that sets
    /// every optional field, a sample whose session window is absent, and a rollup whose
    /// statistics carry every field, its daily session omitted (issue #1598, condition C5).
    /// Never edit these pins: a change to a pinned value fails the test below, and the remedy
    /// is to bump `ROLLUP_FORMAT` and add the new generation's pins beside these. The writer
    /// assertions then move to the new pins; these stay read back through `read_rollup` and
    /// the `Sample` decoder, which is what a retired generation's pins guard.
    const GENERATION_2_SHAPE: StoreShape = StoreShape {
        generation: 2,
        full_sample: r#"{"ts":1700000000,"provider":"claude","acct":"work","session":0.25,"weekly":0.5,"session_resets_at":1700018000,"weekly_resets_at":1700600000,"severity":"warning","spend":1.5}"#,
        weekly_only_sample: r#"{"ts":1700000000,"provider":"claude","acct":"work","session":null,"weekly":0.5,"weekly_resets_at":1700600000}"#,
        rollup: r#"{"format":2,"rollup":{"rolled_through_ts":7200,"hourly":[{"hour_start":3600,"count":4,"session":{"max":0.5,"mean":0.25,"samples":2},"weekly":{"max":0.75,"mean":0.5,"samples":4}}],"daily":[{"day_start":0,"count":4,"coverage":0.5,"weekly":{"max":1.0,"mean":0.5,"p95":1.0,"cap_hits":1,"samples":4}}]}}"#,
    };

    #[test]
    fn the_written_store_shape_is_pinned_to_its_generation() {
        let full = Sample::new(1_700_000_000, "claude", "work", Some(0.25), Some(0.5))
            .with_resets(Some(1_700_018_000), Some(1_700_600_000))
            .with_severity(Some("warning".to_owned()))
            .with_spend(Some(1.5));
        let weekly_only = Sample::new(1_700_000_000, "claude", "work", None, Some(0.5))
            .with_resets(None, Some(1_700_600_000));
        assert_eq!(
            serde_json::to_string(&full).unwrap(),
            GENERATION_2_SHAPE.full_sample,
            "T17: a sample's written shape moved without a ROLLUP_FORMAT bump"
        );
        assert_eq!(
            serde_json::to_string(&weekly_only).unwrap(),
            GENERATION_2_SHAPE.weekly_only_sample,
            "T17: a weekly-only sample's written shape moved without a ROLLUP_FORMAT bump"
        );

        let rollup = Rollup {
            rolled_through_ts: 7200,
            hourly: vec![HourBucket {
                hour_start: 3600,
                count: 4,
                session: Some(HourStat {
                    max: 0.5,
                    mean: 0.25,
                    samples: Some(2),
                }),
                weekly: Some(HourStat {
                    max: 0.75,
                    mean: 0.5,
                    samples: Some(4),
                }),
            }],
            daily: vec![DayBucket {
                day_start: 0,
                count: 4,
                coverage: 0.5,
                session: None,
                weekly: Some(DayStat {
                    max: 1.0,
                    mean: 0.5,
                    p95: 1.0,
                    cap_hits: 1,
                    samples: Some(4),
                }),
            }],
        };
        let dir = tempfile::tempdir().unwrap();
        let (_, rollup_path) = store_paths(dir.path());
        write_rollup(&rollup_path, &rollup).unwrap();
        assert_eq!(
            fs::read_to_string(&rollup_path).unwrap(),
            GENERATION_2_SHAPE.rollup,
            "T17: the rollup's written shape moved without a ROLLUP_FORMAT bump"
        );
        assert_eq!(
            ROLLUP_FORMAT, GENERATION_2_SHAPE.generation,
            "T17: ROLLUP_FORMAT must equal the newest pinned generation"
        );

        // The read side: this build decodes each generation's pinned text back to the value
        // that wrote it. Once a later generation retires these pins from the writer
        // assertions above, this is what they keep guarding.
        assert_eq!(
            serde_json::from_str::<Sample>(GENERATION_2_SHAPE.full_sample).unwrap(),
            full,
            "T17: the pinned generation-2 sample must decode back to the sample that wrote it"
        );
        assert_eq!(
            serde_json::from_str::<Sample>(GENERATION_2_SHAPE.weekly_only_sample).unwrap(),
            weekly_only,
            "T17: the pinned generation-2 weekly-only sample must decode back to the sample that wrote it"
        );
        fs::write(&rollup_path, GENERATION_2_SHAPE.rollup).unwrap();
        assert_eq!(
            read_rollup(&rollup_path).unwrap(),
            rollup,
            "T17: the pinned generation-2 rollup must read back bucket for bucket"
        );
    }
}
