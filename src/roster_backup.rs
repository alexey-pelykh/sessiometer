// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The roster backup ring (issue #1439, design D-3) — a private, fixed-depth ring of previous
//! `config.toml` contents, and the one rule that decides what is allowed into it.
//!
//! On 2026-08-27 the roster went from six accounts to one and there was nothing to restore
//! from. The credentials survived in the Keychain; the roster that indexes them did not, and
//! the deletion that started it is still **unattributed** — the investigation abstained rather
//! than guess. Every other guard in this scope bounds the *amplification* of such a loss; this
//! one makes the loss survivable.
//!
//! **Survivable to the last legitimate save, not to the instant of the loss.** Retention is
//! keyed on replacement, so the file that is live when a deletion happens was never itself
//! retained: recovery returns the roster as of the previous qualifying write. That is D-3 as
//! ratified — its closing argument is that the last *good* save "is the entry an operator
//! actually wants" — and it is stated here because the bound is not obvious from the rule.
//!
//! # The rule, and why the obvious version is worse than nothing
//!
//! The file being replaced enters the ring **iff it parses as a valid config carrying a
//! non-empty roster**. A file that is absent, unreadable, malformed, or zero-account is
//! neither retained **nor allowed to evict** — both halves, which is the whole design.
//!
//! "Retain the previous contents on every write" — the version reached for first — fails on the
//! incident's own sequence. That sequence was *delete → `login` → save*, so the previous
//! contents at save time were **nothing**. A ring keyed on the *write* would have faithfully
//! recorded that nothing and, at depth, evicted the last good copy to do it: a recoverable loss
//! converted into an unrecoverable one. Keying **retention and eviction both** on the replaced
//! file's own quality is what lets the ring survive its own worst day.
//! [`retain_if_qualifying`] is that rule, and the test named for the incident replays exactly
//! that sequence against it.
//!
//! # Shape
//!
//! | property | value | why |
//! |---|---|---|
//! | depth | [`RING_DEPTH`] (3) | small on purpose — the value is surviving one bad write, not keeping history |
//! | location | `backups/` beside `config.toml` | a subdirectory, so no entry is ever a candidate for the loader, which only ever opens the one exact path |
//! | mode | `0600`, via [`paths::write_private_file`] | `config.toml` carries no secret material, but the roster indexes credentials: a wider backup of a `0600` file is a new disclosure |
//! | write | atomic temp-and-rename, the same [`paths::write_private_file`] the live file uses | a torn backup is worse than none — it looks restorable |
//!
//! # What this module does NOT own
//!
//! It never decides *which verb* may write; the rule keys on the replaced file's quality
//! precisely because the cause is unattributed and the write paths cannot be enumerated.
//! Whether a write happens at all is [`crate::witness`]' question (issue #1440).
//!
//! State only what the code guarantees here, because a comment is where a later reader will look
//! for it. Every guarantee in the table above holds for a SERIALIZED writer — and since issue
//! #1445 the writer IS serialized: [`Config::save_to`](crate::config::Config::save_to) holds a
//! dedicated config-write lock across the whole read-modify-write it performs here, so retention,
//! the replacing write, eviction and [`prune`] all run inside one critical section per config
//! file. Every path into this module runs under it, because `save_to` is the only caller.
//!
//! That is what closes the three degradations this module could previously only describe, and
//! they are recorded because each is what an unserialized writer would reintroduce:
//!
//! - Two writers sharing a staging name. [`paths::write_private_file`] opens by unlinking that
//!   name, so an unlink landing between the winner's `fsync` and its `rename` publishes the
//!   LOSER's half-written file — a TORN entry the ring believed it had. (`config restore`
//!   re-validates before installing, so a torn entry was refused rather than restored even then.)
//! - [`prune`]'s sweep unlinking a concurrent writer's in-flight staging file, aborting that
//!   write. See [`prune`] for why the ordering that would prevent it does not hold on its own.
//! - [`Retention::roll_back`] removing an entry by path that another process had replaced at that
//!   same name.
//!
//! Two processes could also compute the same retention stamp — [`stamp_for`] reads the ring and
//! clamps above what it sees, so a collision was DETERMINISTIC for two processes that read it
//! before either had landed, not a nanosecond coincidence. Serialization removes the window
//! rather than making the collision less likely.
//!
//! None of that ever lost the LIVE roster, which is what this module exists to protect: a
//! replacing write that cannot retain is refused, never completed. The lock is what closes the
//! rest, and it is deliberately NOT this module's — it belongs to the write seam every caller
//! reaches this module through.

use std::cmp::Reverse;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::paths;
use crate::paths::FILE_MODE;

/// How many previous configs the ring retains. Small on purpose (design D-3): the value is
/// surviving one bad write, not keeping history.
pub(crate) const RING_DEPTH: usize = 3;

/// The ring's directory name, under the config file's own parent.
///
/// A SUBDIRECTORY rather than a sibling family of `config.toml.1`-style names, so that
/// "a stale backup is never loadable as the live config" holds structurally: the loader opens
/// [`paths::config_file`] and nothing else, and nothing in this crate scans the config
/// directory, so a name under here cannot be reached by accident whatever it is called.
const RING_DIR: &str = "backups";

/// Retention-stamp filename affixes: `config.<secs>.<nanos>.toml`.
///
/// The stamp is the epoch instant, zero-padded to FIXED WIDTH so that a lexicographic sort of
/// the directory is a chronological one — and encoded as digits rather than as RFC 3339
/// because `HH:MM:SS`'s colons are not portable filename characters. Operators read the
/// timestamp through `config backups`, which renders it back with
/// [`crate::observability::rfc3339`]; the name is for the machine's ordering, not for eyes.
const NAME_PREFIX: &str = "config.";
/// See [`NAME_PREFIX`]. Kept as `.toml` so a retained entry opens in a TOML-aware editor.
const NAME_SUFFIX: &str = ".toml";
/// The suffix [`paths::write_private_file`] appends to build its staging name. Restated here
/// because the sweep in [`prune`] has to recognize what that function leaves behind; if the two
/// ever disagree the sweep silently stops finding anything, which is why
/// `the_temp_sweep_recognizes_the_staging_name_write_private_file_actually_uses` drives the real
/// writer rather than this constant.
const TMP_SUFFIX: &str = ".tmp";
/// Seconds field width — 11 digits, which stays fixed-width until the year 5138.
const SECS_DIGITS: usize = 11;
/// Nanoseconds field width — always 9 digits.
const NANOS_DIGITS: usize = 9;

/// One retained config, as `config backups` reports it and `config restore` selects it.
///
/// `accounts` is a COUNT and never a label: the listing is a more public surface than the file
/// (it is what an operator pastes into a bug report), so it carries enough to choose between
/// entries and nothing more (`docs/specs/roster-backup-qualifying-write.feature.md`, Rule 3 — *"each is identified by timestamp and account count … no
/// account label appears in the listing"*; D-3 and AC-5 govern the ring's mode, depth and
/// restore, not its listing). `None` means the entry no longer parses as a
/// config — reachable across a schema change, and the reason `restore` re-validates rather
/// than trusting the ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Retained {
    /// The absolute path of the retained file.
    pub(crate) path: PathBuf,
    /// When it was retained, decoded from the filename rather than from the file's mtime, so
    /// a copied or restored ring keeps its own history.
    pub(crate) taken_at: SystemTime,
    /// How many accounts it holds, or `None` if it no longer parses.
    pub(crate) accounts: Option<usize>,
}

/// The ring directory for a given config file — `<config dir>/backups`.
pub(crate) fn ring_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .expect("a config path always has a parent directory")
        .join(RING_DIR)
}

/// A retention that has been WRITTEN but not yet COMMITTED — the ring's half of a two-phase
/// write.
///
/// Retention has to happen BEFORE the replacing write (afterwards the file it copies is gone),
/// but a replacing write that then FAILS must leave the ring exactly as it found it. A failed
/// `config.toml` write that had already evicted an entry would be the "fixed-size countdown"
/// D-3 forbids, reached by qualifying writes rather than by bad ones — and it would break the
/// `perform_config_set` contract in `src/daemon/commands.rs`, which promises that a refusal is
/// a true no-op with zero writes.
///
/// So [`retain_if_qualifying`] writes the entry and stops; the caller then calls
/// [`commit`](Retention::commit) once the replacement has landed, or
/// [`roll_back`](Retention::roll_back) if it has not. Eviction lives in `commit` alone, so an
/// aborted write can evict nothing.
#[must_use = "a retention must be committed once the replacing write lands, or rolled back"]
pub(crate) struct Retention {
    /// The entry written into the ring, still uncommitted.
    target: PathBuf,
}

impl Retention {
    /// The replacing write landed: keep the entry and prune the ring to [`RING_DEPTH`].
    ///
    /// This is the ONLY eviction site, which is what makes "eviction happens only on a
    /// qualifying write" true by construction rather than by inspection.
    pub(crate) fn commit(self) {
        if let Some(dir) = self.target.parent() {
            prune(dir);
        }
    }

    /// The replacing write failed: remove the entry, so the ring is byte-identical to what it
    /// was before. Best-effort — the alternative to a failed unlink here is one extra entry,
    /// which the next commit's prune resolves.
    pub(crate) fn roll_back(self) {
        let _ = fs::remove_file(&self.target);
    }
}

/// Retain the file at `config_path` **iff it qualifies**, returning the uncommitted
/// [`Retention`]. Call immediately before replacing the file, and resolve the retention either
/// way once the replacement has been attempted.
///
/// Qualifying means [`Config::from_toml_str`] accepts the file's text AND its roster is
/// non-empty — the same [`Config::parse`] seam the daemon loads through, so "valid" here means
/// exactly what it means everywhere else. Absent, unreadable, malformed and zero-account all
/// resolve to `Ok(None)`: nothing retained, and — the half that matters — nothing evicted.
///
/// # Errors
///
/// A failure to retain a file that DOES qualify aborts the replacing write: overwriting the
/// last good roster after failing to copy it is precisely the loss this module exists to
/// prevent. That failure mode cannot exist for a non-qualifying write, which never touches the
/// ring at all — so the guard adds no new way for a first run, or a run over a damaged file, to
/// fail.
///
/// The mode of the written entry is READ BACK rather than assumed. `write_private_file` creates
/// it `0600`, but only on a filesystem that honours POSIX modes; a ring directory that is a
/// symlink to an exFAT volume or a sync-provider shim is an operator affordance nothing here
/// forbids, and AC-5's "BUT NOT by retaining a backup readable by another user" is a property of
/// what is on disk, not of the writer that was used. A wider entry is removed and the write
/// aborts.
///
/// The comparison is against [`paths::FILE_MODE`] itself — the constant AC-5 names — and not a
/// second copy of `0600` spelled here, so the two cannot drift apart. That is why this module
/// widened it to `pub(crate)`.
pub(crate) fn retain_if_qualifying(config_path: &Path) -> Result<Option<Retention>> {
    let Some(contents) = qualifying_contents(config_path) else {
        return Ok(None);
    };
    let dir = ring_dir(config_path);
    paths::ensure_private_dir(&dir)?;
    let target = dir.join(file_name(representable(stamp_for(&dir))?));
    paths::write_private_file(&target, contents.as_bytes())?;
    let mode = fs::metadata(&target)?.permissions().mode() & 0o777;
    if mode != FILE_MODE {
        // Not a retention at all: it is a disclosure wearing a retention's name.
        let _ = fs::remove_file(&target);
        return Err(Error::Io(std::io::Error::other(format!(
            "refusing this roster write: its backup landed at mode {mode:o} because {} does \
             not preserve {FILE_MODE:o}; move the config directory to a filesystem that honours \
             POSIX modes, or the roster cannot be replaced without widening a copy of it",
            dir.display()
        ))));
    }
    Ok(Some(Retention { target }))
}

/// A stamp this module can round-trip through a filename, or an error.
///
/// [`file_name`]'s `{:0N}` is a MINIMUM width while [`stamp_of`] demands an exact one, so a
/// seconds field past [`SECS_DIGITS`] renders a name the reader silently skips: the entry would
/// be invisible to `config backups`, unreachable by `config restore`, and never pruned — the
/// ring frozen at its current contents while every write reported success, and growing without
/// bound, which AC-5 forbids. Unreachable before the year 5138 unless a file at the very top of
/// the range is planted in the ring, at which point [`stamp_for`]'s clamp walks into it. Failing
/// loudly is the module's stated contract for a retention it cannot perform.
fn representable(stamp: (u64, u32)) -> Result<(u64, u32)> {
    if stamp.0 >= 10u64.pow(SECS_DIGITS as u32) {
        return Err(Error::Io(std::io::Error::other(format!(
            "roster backup stamp {} exceeds {SECS_DIGITS} digits and could not be read back",
            stamp.0
        ))));
    }
    Ok(stamp)
}

/// The text of `config_path` if it qualifies for retention, else `None`.
///
/// Reads the bytes and parses them separately so the RETAINED text is the file's own, byte for
/// byte, rather than a re-render of it: a backup that round-tripped through this build's
/// emitter would silently drop anything the emitter no longer writes, which is the opposite of
/// what a backup is for.
fn qualifying_contents(config_path: &Path) -> Option<String> {
    let text = fs::read_to_string(config_path).ok()?;
    let config = Config::from_toml_str(&text).ok()?;
    (!config.roster.is_empty()).then_some(text)
}

/// Enumerate retained configs, newest first.
///
/// An absent ring directory is an empty ring, not an error — the common case on a machine that
/// has never had a qualifying write. Any other read failure propagates, so a listing never
/// reports "nothing retained" when the truth is "could not look".
pub(crate) fn list(config_path: &Path) -> Result<Vec<Retained>> {
    let dir = ring_dir(config_path);
    let mut stamps = scan(&dir)?;
    // Newest first: the stamp is `(secs, nanos)`, so a plain reverse ordering is chronological.
    stamps.sort_unstable_by_key(|(_, stamp)| Reverse(*stamp));
    Ok(stamps
        .into_iter()
        .map(|(path, (secs, nanos))| Retained {
            accounts: Config::load_path(&path).ok().map(|c| c.roster.len()),
            taken_at: UNIX_EPOCH + Duration::new(secs, nanos),
            path,
        })
        .collect())
}

/// Every retained file in `dir` with its decoded stamp, unordered. A name that is not a
/// retention name is skipped rather than rejected: the directory is the operator's too.
fn scan(dir: &Path) -> Result<Vec<(PathBuf, (u64, u32))>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let mut found = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if let Some(stamp) = path.file_name().and_then(|n| n.to_str()).and_then(stamp_of) {
            found.push((path, stamp));
        }
    }
    Ok(found)
}

/// Decode a retention filename back to its `(secs, nanos)` stamp, or `None` if it is not one.
fn stamp_of(name: &str) -> Option<(u64, u32)> {
    let body = name.strip_prefix(NAME_PREFIX)?.strip_suffix(NAME_SUFFIX)?;
    let (secs, nanos) = body.split_once('.')?;
    if secs.len() != SECS_DIGITS || nanos.len() != NANOS_DIGITS {
        return None;
    }
    Some((secs.parse().ok()?, nanos.parse().ok()?))
}

/// The retention filename for a stamp — the inverse of [`stamp_of`].
fn file_name((secs, nanos): (u64, u32)) -> String {
    format!("{NAME_PREFIX}{secs:0SECS_DIGITS$}.{nanos:0NANOS_DIGITS$}{NAME_SUFFIX}")
}

/// The stamp for a retention about to be written into `dir`.
///
/// The wall clock, CLAMPED to strictly after the newest entry already retained. The clamp is
/// what makes "evicts oldest-first" well-defined: a clock that steps backwards (NTP, a VM
/// resume, a manual set) would otherwise write an entry that sorts as the oldest and gets
/// pruned on the very next write, quietly costing the ring a slot. It also guarantees the name
/// is free, since it is above every name already there.
fn stamp_for(dir: &Path) -> (u64, u32) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let stamp = (now.as_secs(), now.subsec_nanos());
    let newest = scan(dir)
        .unwrap_or_default()
        .into_iter()
        .map(|(_, s)| s)
        .max();
    match newest {
        Some(newest) if stamp <= newest => next_after(newest),
        _ => stamp,
    }
}

/// The next representable stamp after `(secs, nanos)`.
fn next_after((secs, nanos): (u64, u32)) -> (u64, u32) {
    match nanos + 1 {
        1_000_000_000 => (secs + 1, 0),
        next => (secs, next),
    }
}

/// Drop everything past the newest [`RING_DEPTH`] entries, and sweep abandoned temp files.
/// Best-effort per the [`Retention`] contract; not incremental, so it recovers from a prior
/// failure.
///
/// The temp sweep is not housekeeping — without it the ring grows without bound, which AC-5
/// forbids outright. [`paths::write_private_file`] self-cleans by unlinking ITS OWN temp name
/// before writing, which is enough for every other caller in this crate because they all write
/// a FIXED path and so reuse one temp name. This module is the exception: [`stamp_for`] makes
/// every target unique, so a crash between the temp's `fsync` and its rename strands a file
/// that no later write will ever name again, and that [`stamp_of`] skips (it demands a `.toml`
/// suffix) so pruning never reaches it either. One full roster copy per crashed write,
/// accumulating for the life of the machine.
///
/// Only temps STRICTLY OLDER than the newest retained entry are swept — which makes the sweep
/// safe against the case it is for (a temp stranded by a crash, older than everything that has
/// landed since) and does NOT make it safe in general. [`stamp_for`] clamps above the newest
/// entry it sees WHEN IT RUNS, not when this runs: a writer that computed its stamp, then had a
/// later writer land an entry and reach here, would have an in-flight temp below the new newest
/// and could have it swept out from under it. Since issue #1445 that interleaving is unreachable
/// — the config-write lock this module is always entered under means no other writer has an
/// in-flight temp while this runs — but the rule stays as it is rather than widening: it is
/// correct on its own terms, and a sweep that trusted the lock would be a second place the lock's
/// scope had to hold.
fn prune(dir: &Path) {
    let mut found = scan(dir).unwrap_or_default();
    found.sort_unstable_by_key(|(_, stamp)| Reverse(*stamp));
    let newest = found.first().map(|(_, stamp)| *stamp);
    for (stale, _) in found.into_iter().skip(RING_DEPTH) {
        let _ = fs::remove_file(stale);
    }
    if let Some(newest) = newest {
        sweep_abandoned_temps(dir, newest);
    }
}

/// Remove retention temp files left behind by a crashed write — those, and nothing else: a name
/// is swept only if stripping [`TMP_SUFFIX`] leaves something [`stamp_of`] decodes, and only if
/// that stamp is strictly older than `newest`.
fn sweep_abandoned_temps(dir: &Path, newest: (u64, u32)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let stranded = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(TMP_SUFFIX))
            .and_then(stamp_of)
            .is_some_and(|stamp| stamp < newest);
        if stranded {
            let _ = fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    /// A valid config carrying `accounts` accounts, as TOML text. Tunables are omitted
    /// throughout — every one has a compiled-in default, so the roster is the only thing these
    /// fixtures vary, which is the only thing the qualifying rule reads.
    fn roster_of(accounts: usize) -> String {
        let mut out = String::new();
        for n in 0..accounts {
            out.push_str(&format!(
                "[[account]]\naccount_uuid = \"{n}\"\nlabel = \"a{n}\"\n\n"
            ));
        }
        out
    }

    /// The parsed form of [`roster_of`], for driving a real [`Config::save_to`].
    fn config_of(accounts: usize) -> Config {
        Config::from_toml_str(&roster_of(accounts)).expect("the fixture roster parses")
    }

    /// A temp dir plus the config path inside it. Every test drives the REAL write seam
    /// ([`Config::save_to`]) against this path, so what is asserted is the shipped behaviour of
    /// the hook rather than a re-implementation of it.
    fn scratch() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("config.toml");
        (dir, path)
    }

    /// How many accounts each retained entry holds, newest first.
    fn retained_counts(config_path: &Path) -> Vec<Option<usize>> {
        list(config_path)
            .expect("the ring lists")
            .into_iter()
            .map(|e| e.accounts)
            .collect()
    }

    /// **The point of issue #1439.** The incident's own sequence — a good six-account file that
    /// has been retained, then a deletion from outside this tool, then `login`'s one-account
    /// write — leaves the six-account entry present and unevicted.
    ///
    /// A back-up-what-was-there rule passes every other test in this module and fails this one:
    /// the previous contents at the moment of the one-account write were NOTHING, so such a rule
    /// would retain the empty state and, at depth, evict a good entry to do it. This is the only
    /// sequence that actually happened, which is why it is asserted first and by itself.
    #[tokio::test]
    async fn the_incident_sequence_leaves_the_six_account_backup_unevicted() {
        let (_dir, path) = scratch();

        // A six-account roster on disk, then an ordinary legitimate save over it — which is how
        // the six-account file enters the ring in the first place.
        fs::write(&path, roster_of(6)).unwrap();
        config_of(6).save_to(&path).await.unwrap();
        assert_eq!(retained_counts(&path), vec![Some(6)]);

        // Something outside sessiometer removes the file. Unattributed, then and here.
        fs::remove_file(&path).unwrap();

        // `login` finds nothing, concludes first run, and writes one account.
        config_of(1).save_to(&path).await.unwrap();

        assert_eq!(
            retained_counts(&path),
            vec![Some(6)],
            "the six-account backup is still present and unevicted"
        );
        // The "unevicted" half is stated here because the scenario states it, but it is NOT
        // independently gated here: one entry against a depth of three means no eviction mutant
        // can manifest, since `skip(RING_DEPTH)` over one entry removes nothing whatever the
        // predicate is. `repeated_non_qualifying_writes_cannot_drain_the_ring` is what actually
        // holds that property, and it is driven against a FULL ring for exactly this reason.
        assert_eq!(
            Config::load_path(&path).unwrap().roster.len(),
            1,
            "the live config is what login wrote — the ring changes nothing about the write"
        );
    }

    /// The ordinary path: a file that parses and holds accounts is retained before it is
    /// replaced, and what is retained is the REPLACED contents, not the replacing ones.
    #[tokio::test]
    async fn a_valid_populated_file_is_retained_before_it_is_replaced() {
        let (_dir, path) = scratch();
        fs::write(&path, roster_of(3)).unwrap();

        config_of(2).save_to(&path).await.unwrap();

        assert_eq!(retained_counts(&path), vec![Some(3)]);
        assert_eq!(Config::load_path(&path).unwrap().roster.len(), 2);
    }

    /// Three shapes, one rule: a file that cannot be vouched for is not evidence of anything, so
    /// it neither enters the ring nor displaces what is already there.
    #[tokio::test]
    async fn absent_malformed_and_empty_files_neither_retain_nor_evict() {
        for (label, seed) in [
            ("absent", None),
            ("malformed", Some("][".to_string())),
            ("zero-account", Some(String::new())),
        ] {
            let (_dir, path) = scratch();
            // One good entry in the ring to have something to lose.
            fs::write(&path, roster_of(4)).unwrap();
            config_of(4).save_to(&path).await.unwrap();
            assert_eq!(retained_counts(&path), vec![Some(4)], "{label}: seeded");

            match seed {
                Some(text) => fs::write(&path, text).unwrap(),
                None => fs::remove_file(&path).unwrap(),
            }
            config_of(1).save_to(&path).await.unwrap();

            // As in the incident replay above, the "nothing evicted" half is carried by
            // `repeated_non_qualifying_writes_cannot_drain_the_ring`, not by this one-entry ring.
            assert_eq!(
                retained_counts(&path),
                vec![Some(4)],
                "{label}: nothing retained, and nothing evicted"
            );
        }
    }

    /// The eviction predicate is the QUALIFYING write, not the write. A ring that evicted
    /// per-write would be a fixed-size countdown to losing everything.
    ///
    /// Driven against a FULL ring rather than a single entry, which is what makes it bite on
    /// the whole family: a ring that drops its oldest per write empties in three, and one that
    /// merely prunes to depth-minus-one per write loses its oldest immediately — neither is
    /// observable when only one entry is seeded, because `skip(n)` over one entry removes
    /// nothing. Both mutants were run against this test; both are caught here and neither was
    /// caught by the single-entry form.
    #[tokio::test]
    async fn repeated_non_qualifying_writes_cannot_drain_the_ring() {
        let (_dir, path) = scratch();
        // Fill the ring: three qualifying writes over files holding 6, 5 then 4 accounts.
        fs::write(&path, roster_of(6)).unwrap();
        for accounts in [5, 4, 3] {
            config_of(accounts).save_to(&path).await.unwrap();
        }
        assert_eq!(retained_counts(&path), vec![Some(4), Some(5), Some(6)]);

        // Five in succession — more than the ring is deep, so a per-write ring would have
        // cycled every good entry out with two writes to spare.
        for _ in 0..5 {
            fs::remove_file(&path).unwrap();
            config_of(1).save_to(&path).await.unwrap();
        }

        assert_eq!(retained_counts(&path), vec![Some(4), Some(5), Some(6)]);
    }

    /// Depth, and the eviction order — asserted on CONTENT rather than on a count, so a ring
    /// that evicted newest-first would fail rather than pass at three.
    #[tokio::test]
    async fn the_ring_holds_at_most_three_and_evicts_oldest_first() {
        let (_dir, path) = scratch();
        // Four qualifying writes: each replaces a file holding 4, 5, 6 then 7 accounts.
        fs::write(&path, roster_of(4)).unwrap();
        for accounts in 5..=8 {
            config_of(accounts).save_to(&path).await.unwrap();
        }

        assert_eq!(
            retained_counts(&path),
            vec![Some(7), Some(6), Some(5)],
            "newest three retained; the four-account entry is the one evicted"
        );
        assert_eq!(list(&path).unwrap().len(), RING_DEPTH);
    }

    /// The mode is read off the filesystem, never inferred from the writer used. A `0644`
    /// backup of a `0600` file is a disclosure the original deliberately prevented.
    #[tokio::test]
    async fn every_retained_file_carries_the_config_file_mode() {
        let (_dir, path) = scratch();
        fs::write(&path, roster_of(2)).unwrap();
        for accounts in 3..=5 {
            config_of(accounts).save_to(&path).await.unwrap();
        }

        let retained = list(&path).unwrap();
        assert_eq!(retained.len(), 3);
        for entry in retained {
            let mode = fs::metadata(&entry.path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o600,
                "{} is {mode:o}, not 0600",
                entry.path.display()
            );
        }
    }

    /// A stale backup is never loadable as the live config by accident: the ring is a
    /// SUBDIRECTORY, so no entry shares the config file's name or its directory, and the loader
    /// — which opens one exact path and never scans — reads the live file with the ring in place.
    #[tokio::test]
    async fn a_retained_backup_is_never_a_candidate_for_the_live_config() {
        let (_dir, path) = scratch();
        fs::write(&path, roster_of(6)).unwrap();
        config_of(1).save_to(&path).await.unwrap();

        let retained = list(&path).unwrap();
        assert_eq!(retained.len(), 1);
        let dir = ring_dir(&path);
        assert_ne!(
            dir,
            path.parent().unwrap(),
            "the ring is not the config dir"
        );
        for entry in &retained {
            assert_eq!(entry.path.parent(), Some(dir.as_path()));
            assert_ne!(entry.path, path);
            assert_ne!(entry.path.file_name(), path.file_name());
        }
        assert_eq!(
            Config::load_path(&path).unwrap().roster.len(),
            1,
            "loading normally reads the live file, not the six-account entry beside it"
        );
    }

    /// What is retained is the replaced file's OWN bytes, not a re-render of them: a backup that
    /// round-tripped through this build's emitter would silently drop anything the emitter no
    /// longer writes, which is the opposite of what a backup is for.
    #[tokio::test]
    async fn the_retained_text_is_the_replaced_files_own_bytes() {
        let (_dir, path) = scratch();
        // Valid, and deliberately unlike anything `Config::render` emits: a comment, an inline
        // spelling of a defaulted tunable, and no trailing structure.
        let authored = "# hand-authored\n[[account]]\naccount_uuid = \"x\"\nlabel = \"l\"\n";
        fs::write(&path, authored).unwrap();

        config_of(2).save_to(&path).await.unwrap();

        let retained = list(&path).unwrap();
        assert_eq!(retained.len(), 1);
        assert_eq!(fs::read_to_string(&retained[0].path).unwrap(), authored);
    }

    /// A retention filename round-trips its stamp, and nothing else is mistaken for one — the
    /// ring directory is the operator's too, so a stray file is skipped rather than decoded.
    #[test]
    fn a_retention_name_round_trips_and_nothing_else_decodes_as_one() {
        for stamp in [
            (0, 0),
            (1_756_900_000, 123_456_789),
            (99_999_999_999, 999_999_999),
        ] {
            let name = file_name(stamp);
            assert_eq!(stamp_of(&name), Some(stamp), "{name} did not round-trip");
        }
        for stray in [
            "config.toml",
            "config.toml.tmp",
            "notes.txt",
            // Right affixes, wrong field widths — an unpadded stamp would sort wrongly, so it
            // is refused rather than accepted and mis-ordered.
            "config.1756900000.123456789.toml",
            "config.01756900000.12345678.toml",
            // Right widths, not digits.
            "config.0175690000x.123456789.toml",
        ] {
            assert_eq!(stamp_of(stray), None, "{stray} decoded as a retention name");
        }
    }

    /// The stamp is clamped strictly above the newest entry, so "oldest-first" stays
    /// well-defined when the wall clock steps backwards (NTP, a VM resume, a manual set).
    /// Without the clamp such a write sorts as the oldest and is pruned on the very next one,
    /// quietly costing the ring a slot.
    #[test]
    fn the_stamp_is_clamped_above_the_newest_retained_entry() {
        let (dir, _path) = scratch();
        let ring = dir.path().join(RING_DIR);
        fs::create_dir_all(&ring).unwrap();
        // A stamp far in the future — the state a backwards clock step leaves behind.
        let future = (99_999_999_999, 0);
        fs::write(ring.join(file_name(future)), roster_of(1)).unwrap();

        let next = stamp_for(&ring);

        assert_eq!(next, next_after(future));
        assert!(next > future);
    }

    /// The nanosecond field carries into the seconds field rather than overflowing.
    #[test]
    fn the_stamp_successor_carries_across_the_second_boundary() {
        assert_eq!(next_after((7, 999_999_998)), (7, 999_999_999));
        assert_eq!(next_after((7, 999_999_999)), (8, 0));
    }

    /// An absent ring is an empty ring, not an error — the common case on a machine that has
    /// never had a qualifying write.
    #[test]
    fn an_absent_ring_lists_as_empty() {
        let (_dir, path) = scratch();
        assert_eq!(list(&path).unwrap(), Vec::new());
    }

    /// A replacing write that FAILS leaves the ring byte-identical — nothing added, and
    /// critically nothing evicted.
    ///
    /// This is the path the qualifying-write rule cannot see: these writes all QUALIFY, so
    /// keying eviction on quality does not help. Eviction is keyed on the replacement having
    /// LANDED instead. The failure is provoked the way it actually arises — a `config.toml.tmp`
    /// that is a directory, so `write_private_file`'s `remove_file` and `create_new` both fail
    /// exactly as they would on a full volume.
    #[tokio::test]
    async fn a_failed_replacing_write_neither_adds_to_nor_evicts_from_the_ring() {
        let (_dir, path) = scratch();
        fs::write(&path, roster_of(6)).unwrap();
        for accounts in [5, 4, 3] {
            config_of(accounts).save_to(&path).await.unwrap();
        }
        let before = retained_counts(&path);
        assert_eq!(before, vec![Some(4), Some(5), Some(6)]);

        // Wedge the live config's own staging path so the replacement cannot land.
        let mut wedge = path.clone().into_os_string();
        wedge.push(".tmp");
        fs::create_dir(PathBuf::from(&wedge)).unwrap();

        // Three attempts: at depth, a per-attempt eviction would take the ring apart.
        for _ in 0..3 {
            assert!(
                config_of(2).save_to(&path).await.is_err(),
                "the wedged staging path must fail the write"
            );
        }

        assert_eq!(
            retained_counts(&path),
            before,
            "a refused write is a true no-op on the ring"
        );
    }

    /// Restoring is a roster write like any other, and that is what makes it reversible: the
    /// config a restore replaces enters the ring first, so an operator who restored the wrong
    /// entry can get back to where they were.
    ///
    /// Drives the mechanics `config restore` performs — validate the retained text, then write
    /// it through the same seam — at the layer where they can be driven hermetically.
    #[tokio::test]
    async fn restoring_through_the_write_seam_retains_the_config_it_replaces() {
        let (_dir, path) = scratch();
        fs::write(&path, roster_of(6)).unwrap();
        config_of(1).save_to(&path).await.unwrap();
        assert_eq!(retained_counts(&path), vec![Some(6)]);

        // What `config restore 1` does: re-validate the chosen entry, then write it back.
        let chosen = list(&path).unwrap().remove(0);
        let restored = Config::from_toml_str(&fs::read_to_string(&chosen.path).unwrap()).unwrap();
        restored.save_to(&path).await.unwrap();

        assert_eq!(Config::load_path(&path).unwrap().roster.len(), 6);
        assert_eq!(
            retained_counts(&path),
            vec![Some(1), Some(6)],
            "the displaced one-account roster is retained, so the restore is itself undoable"
        );
    }

    /// The retention path WRITES through the atomic primitive and READS the mode back — asserted
    /// on this module's own source, because neither property is reachable from a unit test here.
    ///
    /// Gherkin scenario "a backup is never torn" is otherwise ungated: `fs::write` followed by a
    /// `set_permissions(0o600)` is non-atomic, mode-correct, and green across every other test in
    /// this module. And the mode READ-BACK cannot be provoked at all on a local filesystem —
    /// `write_private_file` passes `0600` to `open(2)`, and a umask can only remove bits, so no
    /// fixture produces a widened entry. What the check defends against is a ring directory that
    /// does not honour POSIX modes (a symlink to exFAT, a sync-provider shim), which a test in a
    /// `TempDir` structurally cannot stage.
    ///
    /// So the subject is the source, in the idiom this crate already uses for properties no
    /// value can carry. Cut to `retain_if_qualifying`'s body so a sibling function's `fs::write`
    /// cannot satisfy it.
    #[test]
    fn the_retention_path_writes_atomically_and_reads_the_mode_back() {
        let source = include_str!("roster_backup.rs");
        let body = source
            .split_once("pub(crate) fn retain_if_qualifying")
            .expect("the retention entry point is named in this file")
            .1
            .split_once("\n}\n")
            .expect("a top-level fn body ends at a column-0 brace")
            .0;

        assert!(
            body.contains("paths::write_private_file("),
            "the retention must go through the atomic temp-and-rename writer — a partially \
             written backup is worse than none, because it looks restorable:\n{body}"
        );
        assert!(
            !body.contains("fs::write("),
            "a plain `fs::write` is not atomic; it would pass every behavioural test here \
             while leaving a torn entry reachable:\n{body}"
        );
        assert!(
            body.contains("permissions().mode()") && body.contains("FILE_MODE"),
            "the written entry's mode must be read back and compared against FILE_MODE, not \
             inferred from the writer that was used:\n{body}"
        );
    }

    /// The canary for the test above: the identical predicate over a deliberately broken subject
    /// must reject it, so a green there is evidence rather than a lexer that matched nothing.
    #[test]
    fn the_atomicity_gate_rejects_a_non_atomic_retention_body() {
        let mutant = "pub(crate) fn retain_if_qualifying(p: &Path) -> Result<()> {\n                          fs::write(p, b\"x\")?;\n    Ok(())\n}\n";
        let body = mutant
            .split_once("pub(crate) fn retain_if_qualifying")
            .unwrap()
            .1
            .split_once("\n}\n")
            .unwrap()
            .0;
        assert!(!body.contains("paths::write_private_file("));
        assert!(body.contains("fs::write("));
        assert!(!(body.contains("permissions().mode()") && body.contains("FILE_MODE")));
    }

    /// A stamp too wide to be read back is refused rather than written. Without this the entry
    /// lands under a name `stamp_of` skips: invisible to the listing, unreachable by restore,
    /// never pruned, and reported as a successful retention.
    #[test]
    fn a_stamp_wider_than_the_name_can_hold_is_refused() {
        assert!(
            representable((99_999_999_999, 0)).is_ok(),
            "the top of the range is legal"
        );
        assert!(
            representable((100_000_000_000, 0)).is_err(),
            "one past it cannot be read back, so it must not be written"
        );
        // And it is reachable from a legal stamp, which is why this is a guard and not a comment.
        assert_eq!(
            next_after((99_999_999_999, 999_999_999)),
            (100_000_000_000, 0)
        );
    }

    /// The temp sweep recognizes the staging name the REAL writer leaves behind, not a name this
    /// module guessed. Drives `paths::write_private_file` and inspects what it stages, so the
    /// sweep cannot silently stop matching if that function renames its temp.
    #[test]
    fn the_temp_sweep_recognizes_the_staging_name_write_private_file_actually_uses() {
        let (dir, path) = scratch();
        let ring = ring_dir(&path);
        fs::create_dir_all(&ring).unwrap();
        let target = ring.join(file_name((100, 0)));

        // The staging name, as this module predicts it.
        let mut predicted = target.clone().into_os_string();
        predicted.push(TMP_SUFFIX);
        let predicted = PathBuf::from(predicted);

        // Wedge the rename by making the TARGET a non-empty directory, so the writer's staging
        // file is left on disk exactly as a crash would leave it.
        fs::create_dir(&target).unwrap();
        fs::write(target.join("occupant"), "x").unwrap();
        assert!(paths::write_private_file(&target, b"x").is_err());
        assert!(
            predicted.exists(),
            "write_private_file no longer stages at <target>{TMP_SUFFIX} — the sweep in `prune` \
             keys on that name and would silently stop finding anything"
        );
        drop(dir);
    }

    /// An abandoned staging file is swept, so a crashed retention cannot accumulate one full
    /// roster copy per crash forever — AC-5 forbids growing without bound, and every other
    /// `write_private_file` caller self-heals only because it reuses one fixed temp name.
    #[test]
    fn abandoned_staging_files_are_swept_but_a_newer_one_is_left_alone() {
        let (_dir, path) = scratch();
        let ring = ring_dir(&path);
        fs::create_dir_all(&ring).unwrap();
        let stranded = ring.join(format!("{}{TMP_SUFFIX}", file_name((100, 0))));
        let retained = ring.join(file_name((200, 0)));
        // Newer than the newest entry: the shape a crash cannot produce, so the sweep leaves it
        // alone. NOT a claim that a live writer's temp is always up here — see `prune`.
        let in_flight = ring.join(format!("{}{TMP_SUFFIX}", file_name((300, 0))));
        let bystander = ring.join("operator-notes.txt");
        for f in [&stranded, &retained, &in_flight, &bystander] {
            fs::write(f, roster_of(1)).unwrap();
        }

        prune(&ring);

        assert!(!stranded.exists(), "the abandoned staging file is swept");
        assert!(
            retained.exists(),
            "the entry it was staging for is untouched"
        );
        assert!(
            in_flight.exists(),
            "a temp newer than the ring is still being written"
        );
        assert!(
            bystander.exists(),
            "the ring directory is the operator's too"
        );
    }

    /// An entry that no longer parses reports `None` rather than being hidden or panicking:
    /// hiding it would make the numbering `config restore` accepts disagree with the listing.
    #[test]
    fn an_unparseable_entry_reports_no_account_count() {
        let (dir, path) = scratch();
        let ring = dir.path().join(RING_DIR);
        fs::create_dir_all(&ring).unwrap();
        fs::write(ring.join(file_name((100, 0))), "][").unwrap();

        assert_eq!(retained_counts(&path), vec![None]);
    }
}
