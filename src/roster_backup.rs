// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The roster backup ring (issue #1439, design D-3) — a private, fixed-depth ring of previous
//! `config.toml` contents, and the one rule that decides what is allowed into it.
//!
//! On 2026-08-27 the roster went from six accounts to one and there was nothing to restore
//! from. The credentials survived in the Keychain; the roster that indexes them (uuid + label
//! + enabled, per account) did not, and the deletion that started it is still **unattributed**
//! — the investigation abstained rather than guess. Every other guard in this scope bounds the
//! *amplification* of such a loss; this one makes the loss survivable without needing to know
//! what caused it.
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
//! Two processes replacing the config at the same instant can compute the same retention
//! stamp, and one of the two backups then loses the rename race — the config-write path is not
//! serialized today, which is issue #1445's subject, not this module's. The ring degrades by
//! retaining one entry instead of two; it cannot corrupt one.

use std::cmp::Reverse;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::error::Result;
use crate::paths;

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
/// Seconds field width — 11 digits, which stays fixed-width until the year 5138.
const SECS_DIGITS: usize = 11;
/// Nanoseconds field width — always 9 digits.
const NANOS_DIGITS: usize = 9;

/// One retained config, as `config backups` reports it and `config restore` selects it.
///
/// `accounts` is a COUNT and never a label: the listing is a more public surface than the file
/// (it is what an operator pastes into a bug report), so it carries enough to choose between
/// entries and nothing more (design D-3, AC-5). `None` means the entry no longer parses as a
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

/// Retain the file at `config_path` **iff it qualifies**, then prune the ring to
/// [`RING_DEPTH`]. Call immediately before replacing it.
///
/// Qualifying means [`Config::load_path`] accepts it AND its roster is non-empty — the same
/// seam the daemon loads through, so "valid" here means exactly what it means everywhere else.
/// Absent, unreadable, malformed and zero-account all resolve to `Ok(None)`: nothing retained,
/// and — the half that matters — nothing evicted.
///
/// # Errors
///
/// A failure to retain a file that DOES qualify is an error, and it deliberately aborts the
/// replacing write: overwriting the last good roster after failing to copy it is precisely the
/// loss this module exists to prevent. That failure mode cannot exist for a non-qualifying
/// write, which never touches the ring at all — so the guard adds no new way for a first run,
/// or a run over a damaged file, to fail.
///
/// Pruning is best-effort by contrast: once the good copy is in, a failed unlink leaves the
/// ring one entry over depth, and the next qualifying write prunes to depth again (the prune
/// is not incremental — it keeps the newest [`RING_DEPTH`] and drops the rest).
pub(crate) fn retain_if_qualifying(config_path: &Path) -> Result<Option<PathBuf>> {
    let Some(contents) = qualifying_contents(config_path) else {
        return Ok(None);
    };
    let dir = ring_dir(config_path);
    paths::ensure_private_dir(&dir)?;
    let target = dir.join(file_name(stamp_for(&dir)));
    paths::write_private_file(&target, contents.as_bytes())?;
    prune(&dir);
    Ok(Some(target))
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

/// Drop everything past the newest [`RING_DEPTH`] entries. Best-effort per the
/// [`retain_if_qualifying`] contract; not incremental, so it recovers from a prior failure.
fn prune(dir: &Path) {
    let mut found = scan(dir).unwrap_or_default();
    found.sort_unstable_by_key(|(_, stamp)| Reverse(*stamp));
    for (stale, _) in found.into_iter().skip(RING_DEPTH) {
        let _ = fs::remove_file(stale);
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
    #[test]
    fn the_incident_sequence_leaves_the_six_account_backup_unevicted() {
        let (_dir, path) = scratch();

        // A six-account roster on disk, then an ordinary legitimate save over it — which is how
        // the six-account file enters the ring in the first place.
        fs::write(&path, roster_of(6)).unwrap();
        config_of(6).save_to(&path).unwrap();
        assert_eq!(retained_counts(&path), vec![Some(6)]);

        // Something outside sessiometer removes the file. Unattributed, then and here.
        fs::remove_file(&path).unwrap();

        // `login` finds nothing, concludes first run, and writes one account.
        config_of(1).save_to(&path).unwrap();

        assert_eq!(
            retained_counts(&path),
            vec![Some(6)],
            "the six-account backup is still present and unevicted"
        );
        assert_eq!(
            Config::load_path(&path).unwrap().roster.len(),
            1,
            "the live config is what login wrote — the ring changes nothing about the write"
        );
    }

    /// The ordinary path: a file that parses and holds accounts is retained before it is
    /// replaced, and what is retained is the REPLACED contents, not the replacing ones.
    #[test]
    fn a_valid_populated_file_is_retained_before_it_is_replaced() {
        let (_dir, path) = scratch();
        fs::write(&path, roster_of(3)).unwrap();

        config_of(2).save_to(&path).unwrap();

        assert_eq!(retained_counts(&path), vec![Some(3)]);
        assert_eq!(Config::load_path(&path).unwrap().roster.len(), 2);
    }

    /// Three shapes, one rule: a file that cannot be vouched for is not evidence of anything, so
    /// it neither enters the ring nor displaces what is already there.
    #[test]
    fn absent_malformed_and_empty_files_neither_retain_nor_evict() {
        for (label, seed) in [
            ("absent", None),
            ("malformed", Some("][".to_string())),
            ("zero-account", Some(String::new())),
        ] {
            let (_dir, path) = scratch();
            // One good entry in the ring to have something to lose.
            fs::write(&path, roster_of(4)).unwrap();
            config_of(4).save_to(&path).unwrap();
            assert_eq!(retained_counts(&path), vec![Some(4)], "{label}: seeded");

            match seed {
                Some(text) => fs::write(&path, text).unwrap(),
                None => fs::remove_file(&path).unwrap(),
            }
            config_of(1).save_to(&path).unwrap();

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
    #[test]
    fn repeated_non_qualifying_writes_cannot_drain_the_ring() {
        let (_dir, path) = scratch();
        // Fill the ring: three qualifying writes over files holding 6, 5 then 4 accounts.
        fs::write(&path, roster_of(6)).unwrap();
        for accounts in [5, 4, 3] {
            config_of(accounts).save_to(&path).unwrap();
        }
        assert_eq!(retained_counts(&path), vec![Some(4), Some(5), Some(6)]);

        // Five in succession — more than the ring is deep, so a per-write ring would have
        // cycled every good entry out with two writes to spare.
        for _ in 0..5 {
            fs::remove_file(&path).unwrap();
            config_of(1).save_to(&path).unwrap();
        }

        assert_eq!(retained_counts(&path), vec![Some(4), Some(5), Some(6)]);
    }

    /// Depth, and the eviction order — asserted on CONTENT rather than on a count, so a ring
    /// that evicted newest-first would fail rather than pass at three.
    #[test]
    fn the_ring_holds_at_most_three_and_evicts_oldest_first() {
        let (_dir, path) = scratch();
        // Four qualifying writes: each replaces a file holding 4, 5, 6 then 7 accounts.
        fs::write(&path, roster_of(4)).unwrap();
        for accounts in 5..=8 {
            config_of(accounts).save_to(&path).unwrap();
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
    #[test]
    fn every_retained_file_carries_the_config_file_mode() {
        let (_dir, path) = scratch();
        fs::write(&path, roster_of(2)).unwrap();
        for accounts in 3..=5 {
            config_of(accounts).save_to(&path).unwrap();
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
    #[test]
    fn a_retained_backup_is_never_a_candidate_for_the_live_config() {
        let (_dir, path) = scratch();
        fs::write(&path, roster_of(6)).unwrap();
        config_of(1).save_to(&path).unwrap();

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
    #[test]
    fn the_retained_text_is_the_replaced_files_own_bytes() {
        let (_dir, path) = scratch();
        // Valid, and deliberately unlike anything `Config::render` emits: a comment, an inline
        // spelling of a defaulted tunable, and no trailing structure.
        let authored = "# hand-authored\n[[account]]\naccount_uuid = \"x\"\nlabel = \"l\"\n";
        fs::write(&path, authored).unwrap();

        config_of(2).save_to(&path).unwrap();

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
