// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Runtime configuration: the roster and tunables persisted in `config.toml`.
//!
//! `config.toml` (at [`paths::config_file`]) is the daemon's source of truth —
//! the captured account roster plus the poll/swap tunables. It is loaded at
//! daemon start and written by `capture` (issue #4).
//!
//! Loading is two-stage so invalid states are unrepresentable: TOML is first
//! deserialized into a permissive [`RawConfig`] (wide integer types, every key
//! optional with a documented default), then [`validate`](Config::validate)d
//! into the typed [`Config`]. A malformed file is a hard error — there are no
//! silent fallbacks to defaults for a file that exists but does not parse or
//! does not satisfy the bounds.
//!
//! Nothing here carries secret material: the roster keys accounts by
//! `account_uuid` / `stash` / `label`, never by token or email (issues #9, #15,
//! #17). Error messages quote only those non-secret fields.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::paths;

/// Default seconds between usage polls.
const DEFAULT_POLL_SECS: u64 = 60;
/// Default seconds to wait after a swap before another is allowed.
const DEFAULT_COOLDOWN_SECS: u64 = 60;
/// Default `session_floor` percent.
const DEFAULT_SESSION_FLOOR: u8 = 80;
/// Default `session_trigger` percent.
const DEFAULT_SESSION_TRIGGER: u8 = 95;
/// Default consecutive-401 count before an account is treated as rejected.
const DEFAULT_MONITOR_401_N: u8 = 3;

/// Maximum accounts the roster may hold (the rotation size). The single source
/// of truth for this bound — `capture` (#4) reads it to cap new captures and to
/// render its "N of {MAX_ACCOUNTS}" confirmation.
pub(crate) const MAX_ACCOUNTS: usize = 5;

/// One captured account in the roster. Keyed by non-secret fields only.
///
/// The fields beyond the uniqueness keys are read by the write path
/// ([`Config::render`], for `capture` #4) and by `list` / `status` (#17 / #9);
/// the swap engine (#6 / #7) rotates across the roster. They are validated and
/// persisted here ahead of those consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct Account {
    /// Stable per-account identifier (the Claude `account_uuid`); roster key.
    pub(crate) account_uuid: String,
    /// Keychain stash name the captured credential lives under
    /// (`Sessiometer/acct-N`); roster key.
    pub(crate) stash: String,
    /// Human-readable label shown by `list` / `status`.
    pub(crate) label: String,
}

/// The daemon tunables, validated into their typed ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tunables {
    /// Seconds between usage polls (`5..=3600`).
    pub(crate) poll_secs: u64,
    /// Seconds to wait after a swap before another is allowed (`0..=3600`).
    /// Consumed by the cooldown logic (#10 / #11).
    #[allow(dead_code)]
    pub(crate) cooldown_secs: u64,
    /// Only swap *to* an account whose session usage is below this percent
    /// (`0..=session_trigger`). Consumed by swap-target selection (#6 / #7).
    #[allow(dead_code)]
    pub(crate) session_floor: u8,
    /// Swap *away* from the active account at or above this session percent
    /// (`50..=99`).
    pub(crate) session_trigger: u8,
    /// Consecutive 401s before an account is treated as rejected (`1..=20`).
    /// Consumed by the usage poller's 401 monitor (#5); the re-stash it triggers
    /// lands in #13 / #6.
    pub(crate) monitor_401_n: u8,
}

impl Default for Tunables {
    fn default() -> Self {
        Self {
            poll_secs: DEFAULT_POLL_SECS,
            cooldown_secs: DEFAULT_COOLDOWN_SECS,
            session_floor: DEFAULT_SESSION_FLOOR,
            session_trigger: DEFAULT_SESSION_TRIGGER,
            monitor_401_n: DEFAULT_MONITOR_401_N,
        }
    }
}

/// The validated configuration: a non-empty roster plus tunables.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// Captured accounts (`1..=5`, unique `account_uuid` + `stash`). Consumed by
    /// the swap engine (#6 / #7) and by `list` / `status` (#17 / #9).
    #[allow(dead_code)]
    pub(crate) roster: Vec<Account>,
    /// Poll/swap tunables.
    pub(crate) tunables: Tunables,
}

impl Config {
    /// Load and validate `config.toml` from its standard path.
    ///
    /// Returns [`Error::ConfigNotFound`] if the file is absent (the daemon has
    /// nothing to run until `capture` writes one), [`Error::ConfigParse`] /
    /// [`Error::ConfigInvalid`] / [`Error::ConfigFloorAboveTrigger`] for a file
    /// that exists but is malformed. Never silently substitutes defaults for a
    /// malformed file.
    pub(crate) fn load() -> Result<Self> {
        Self::load_path(&paths::config_file()?)
    }

    /// [`load`](Config::load) against an explicit path — the injectable seam, so
    /// the file-I/O branches (absent → [`Error::ConfigNotFound`], other read
    /// failure → [`Error::Io`]) are testable without touching the real config
    /// location.
    fn load_path(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Err(Error::ConfigNotFound {
                    path: path.to_path_buf(),
                });
            }
            Err(err) => return Err(Error::Io(err)),
        };
        Self::parse(&text)
    }

    /// Persist this config to `config.toml` (`0600`, parent `0700`), with the
    /// inline tunable-documenting comments. The write path for `capture` (#4).
    #[allow(dead_code)]
    pub(crate) fn save(&self) -> Result<()> {
        let path = paths::config_file()?;
        paths::ensure_private_dir(
            path.parent()
                .expect("config_file() always has a parent directory"),
        )?;
        paths::write_private_file(&path, self.render().as_bytes())
    }

    /// How long to wait between usage polls.
    pub(crate) fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.tunables.poll_secs)
    }

    /// The usage fraction in `[0.0, 1.0]` at or above which the active account
    /// is considered exhausted — `session_trigger` as a fraction.
    ///
    /// The daemon derives its own trigger / floor / cooldown uniformly from
    /// [`Tunables`] (issue #7), so this Config-level accessor is currently a
    /// tested seam for the `status` view (#9) rather than the run loop.
    #[allow(dead_code)]
    pub(crate) fn swap_threshold(&self) -> f64 {
        f64::from(self.tunables.session_trigger) / 100.0
    }

    /// Stage one: deserialize TOML into the permissive raw form, then validate.
    /// Pure (no filesystem) so the whole parse-and-validate policy is testable
    /// without touching real paths.
    fn parse(text: &str) -> Result<Self> {
        let raw: RawConfig =
            toml::from_str(text).map_err(|err| Error::ConfigParse(err.to_string()))?;
        Self::validate(raw)
    }

    /// Stage two: bounds-check every tunable and the roster, producing the typed
    /// `Config`. Each rejection names the offending field; the cross-field rule
    /// (`session_floor <= session_trigger`) gets its own distinct error.
    fn validate(raw: RawConfig) -> Result<Self> {
        let t = raw.tunables;

        range("session_trigger", t.session_trigger, 50, 99)?;
        // session_floor's lower bound is 0; its upper bound is session_trigger,
        // surfaced as a distinct cross-field error.
        if t.session_floor < 0 {
            return Err(Error::ConfigInvalid(format!(
                "session_floor must be in 0..={}, got {}",
                t.session_trigger, t.session_floor
            )));
        }
        if t.session_floor > t.session_trigger {
            return Err(Error::ConfigFloorAboveTrigger {
                floor: t.session_floor,
                trigger: t.session_trigger,
            });
        }
        range("poll_secs", t.poll_secs, 5, 3600)?;
        range("cooldown_secs", t.cooldown_secs, 0, 3600)?;
        range("monitor_401_n", t.monitor_401_n, 1, 20)?;

        // Ranges are checked above, so these narrowing casts cannot truncate.
        let tunables = Tunables {
            poll_secs: t.poll_secs as u64,
            cooldown_secs: t.cooldown_secs as u64,
            session_floor: t.session_floor as u8,
            session_trigger: t.session_trigger as u8,
            monitor_401_n: t.monitor_401_n as u8,
        };

        if !(1..=MAX_ACCOUNTS).contains(&raw.account.len()) {
            return Err(Error::ConfigInvalid(format!(
                "roster must have 1..={MAX_ACCOUNTS} accounts, got {}",
                raw.account.len()
            )));
        }

        let mut uuids = HashSet::new();
        let mut stashes = HashSet::new();
        let mut roster = Vec::with_capacity(raw.account.len());
        for account in raw.account {
            if account.account_uuid.trim().is_empty() {
                return Err(Error::ConfigInvalid(
                    "account_uuid must not be empty".into(),
                ));
            }
            if account.stash.trim().is_empty() {
                return Err(Error::ConfigInvalid("stash must not be empty".into()));
            }
            if account.label.trim().is_empty() {
                return Err(Error::ConfigInvalid("label must not be empty".into()));
            }
            if !uuids.insert(account.account_uuid.clone()) {
                return Err(Error::ConfigInvalid(format!(
                    "duplicate account_uuid: {}",
                    account.account_uuid
                )));
            }
            if !stashes.insert(account.stash.clone()) {
                return Err(Error::ConfigInvalid(format!(
                    "duplicate stash: {}",
                    account.stash
                )));
            }
            roster.push(Account {
                account_uuid: account.account_uuid,
                stash: account.stash,
                label: account.label,
            });
        }

        Ok(Config { roster, tunables })
    }

    /// Render the config back to TOML with the inline tunable-documenting
    /// comments (issue #3 N2). `serde` serialization cannot emit comments, so
    /// the file is rendered by hand; integers need no escaping and roster
    /// strings go through [`basic_string`].
    #[allow(dead_code)]
    fn render(&self) -> String {
        let t = &self.tunables;
        let mut out = String::new();
        out.push_str("# sessiometer configuration.\n");
        out.push_str(
            "# The roster is managed by `sessiometer capture`; the [tunables] block is\n\
             # safe to hand-edit. Percentages are of the rolling session window.\n\n",
        );

        out.push_str("[tunables]\n");
        out.push_str("# Seconds between usage polls (5..=3600).\n");
        out.push_str(&format!("poll_secs = {}\n", t.poll_secs));
        out.push_str("# Seconds to wait after a swap before another swap is allowed (0..=3600).\n");
        out.push_str(&format!("cooldown_secs = {}\n", t.cooldown_secs));
        out.push_str(
            "# Only swap TO an account whose session usage is below this percent\n\
             # (0..=session_trigger): a candidate must be at most this full to receive the\n\
             # active session. This is NOT the level that triggers a swap.\n",
        );
        out.push_str(&format!("session_floor = {}\n", t.session_floor));
        out.push_str(
            "# Swap AWAY from the active account at or above this session percent (50..=99).\n",
        );
        out.push_str(&format!("session_trigger = {}\n", t.session_trigger));
        out.push_str("# Consecutive 401s before an account is treated as rejected (1..=20).\n");
        out.push_str(&format!("monitor_401_n = {}\n", t.monitor_401_n));

        for account in &self.roster {
            out.push_str("\n[[account]]\n");
            out.push_str(&format!(
                "account_uuid = {}\n",
                basic_string(&account.account_uuid)
            ));
            out.push_str(&format!("stash = {}\n", basic_string(&account.stash)));
            out.push_str(&format!("label = {}\n", basic_string(&account.label)));
        }
        out
    }
}

/// Reject `value` if it falls outside `lo..=hi`, naming `field` in the error.
fn range(field: &'static str, value: i64, lo: i64, hi: i64) -> Result<()> {
    if (lo..=hi).contains(&value) {
        Ok(())
    } else {
        Err(Error::ConfigInvalid(format!(
            "{field} must be in {lo}..={hi}, got {value}"
        )))
    }
}

/// Render `s` as a TOML basic string (quoted, with the required escapes). Used
/// by [`Config::render`] for roster fields, which (unlike the integer tunables)
/// may contain characters needing escaping.
#[allow(dead_code)]
fn basic_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            // Remaining C0 controls and DEL must be escaped; everything else
            // (including non-ASCII) is valid literally in a basic string.
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Permissive deserialization target: every key optional (documented default),
/// integers kept wide so out-of-range values reach [`Config::validate`] with a
/// clear message rather than failing as a `serde` type error.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    account: Vec<RawAccount>,
    #[serde(default)]
    tunables: RawTunables,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAccount {
    account_uuid: String,
    stash: String,
    label: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTunables {
    #[serde(default = "default_poll_secs")]
    poll_secs: i64,
    #[serde(default = "default_cooldown_secs")]
    cooldown_secs: i64,
    #[serde(default = "default_session_floor")]
    session_floor: i64,
    #[serde(default = "default_session_trigger")]
    session_trigger: i64,
    #[serde(default = "default_monitor_401_n")]
    monitor_401_n: i64,
}

impl Default for RawTunables {
    fn default() -> Self {
        Self {
            poll_secs: default_poll_secs(),
            cooldown_secs: default_cooldown_secs(),
            session_floor: default_session_floor(),
            session_trigger: default_session_trigger(),
            monitor_401_n: default_monitor_401_n(),
        }
    }
}

fn default_poll_secs() -> i64 {
    DEFAULT_POLL_SECS as i64
}
fn default_cooldown_secs() -> i64 {
    DEFAULT_COOLDOWN_SECS as i64
}
fn default_session_floor() -> i64 {
    i64::from(DEFAULT_SESSION_FLOOR)
}
fn default_session_trigger() -> i64 {
    i64::from(DEFAULT_SESSION_TRIGGER)
}
fn default_monitor_401_n() -> i64 {
    i64::from(DEFAULT_MONITOR_401_N)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
[tunables]
poll_secs = 30
cooldown_secs = 45
session_floor = 70
session_trigger = 90
monitor_401_n = 5

[[account]]
account_uuid = "11111111-1111-1111-1111-111111111111"
stash = "Sessiometer/acct-1"
label = "work"

[[account]]
account_uuid = "22222222-2222-2222-2222-222222222222"
stash = "Sessiometer/acct-2"
label = "personal"
"#;

    /// A minimal valid roster body with one account and the given `[tunables]`
    /// fragment spliced in.
    fn with_tunables(fragment: &str) -> String {
        format!(
            "[tunables]\n{fragment}\n\
             [[account]]\n\
             account_uuid = \"u\"\n\
             stash = \"Sessiometer/acct-1\"\n\
             label = \"l\"\n"
        )
    }

    #[test]
    fn parses_a_valid_config() {
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.roster.len(), 2);
        assert_eq!(
            config.tunables,
            Tunables {
                poll_secs: 30,
                cooldown_secs: 45,
                session_floor: 70,
                session_trigger: 90,
                monitor_401_n: 5,
            }
        );
        assert_eq!(config.roster[0].label, "work");
        assert_eq!(config.roster[1].stash, "Sessiometer/acct-2");
    }

    #[test]
    fn tunables_default_when_table_absent() {
        let toml = "[[account]]\n\
                    account_uuid = \"u\"\n\
                    stash = \"Sessiometer/acct-1\"\n\
                    label = \"only\"\n";
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.tunables, Tunables::default());
        assert_eq!(config.tunables.session_trigger, 95);
        assert_eq!(config.tunables.session_floor, 80);
    }

    #[test]
    fn missing_tunable_key_takes_its_default() {
        let toml = with_tunables("poll_secs = 120");
        let config = Config::parse(&toml).unwrap();
        assert_eq!(config.tunables.poll_secs, 120);
        assert_eq!(config.tunables.cooldown_secs, DEFAULT_COOLDOWN_SECS);
    }

    #[test]
    fn malformed_toml_is_a_parse_error() {
        assert!(matches!(Config::parse("]["), Err(Error::ConfigParse(_))));
    }

    #[test]
    fn unknown_field_is_rejected() {
        let toml = with_tunables("poll_secs = 60\nbogus = 1");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigParse(_))));
    }

    #[test]
    fn rejects_out_of_range_trigger() {
        for trigger in ["49", "100", "120"] {
            let toml = with_tunables(&format!("session_trigger = {trigger}"));
            assert!(
                matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))),
                "session_trigger = {trigger} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_floor_above_trigger_with_a_distinct_error() {
        let toml = with_tunables("session_floor = 95\nsession_trigger = 90");
        assert!(matches!(
            Config::parse(&toml),
            Err(Error::ConfigFloorAboveTrigger {
                floor: 95,
                trigger: 90
            })
        ));
    }

    #[test]
    fn rejects_negative_floor() {
        let toml = with_tunables("session_floor = -1");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))));
    }

    #[test]
    fn rejects_each_out_of_range_tunable() {
        for (key, value) in [
            ("poll_secs", "4"),
            ("poll_secs", "3601"),
            ("cooldown_secs", "3601"),
            ("monitor_401_n", "0"),
            ("monitor_401_n", "21"),
        ] {
            let toml = with_tunables(&format!("{key} = {value}"));
            assert!(
                matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))),
                "{key} = {value} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_empty_roster() {
        assert!(matches!(
            Config::parse("[tunables]\npoll_secs = 60\n"),
            Err(Error::ConfigInvalid(_))
        ));
    }

    #[test]
    fn rejects_oversized_roster() {
        let mut toml = String::new();
        for i in 0..6 {
            toml.push_str(&format!(
                "[[account]]\naccount_uuid = \"u{i}\"\nstash = \"s{i}\"\nlabel = \"l{i}\"\n"
            ));
        }
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))));
    }

    #[test]
    fn rejects_duplicate_uuid() {
        let toml = "[[account]]\naccount_uuid = \"same\"\nstash = \"s1\"\nlabel = \"a\"\n\
                    [[account]]\naccount_uuid = \"same\"\nstash = \"s2\"\nlabel = \"b\"\n";
        assert!(matches!(Config::parse(toml), Err(Error::ConfigInvalid(_))));
    }

    #[test]
    fn rejects_duplicate_stash() {
        let toml = "[[account]]\naccount_uuid = \"u1\"\nstash = \"same\"\nlabel = \"a\"\n\
                    [[account]]\naccount_uuid = \"u2\"\nstash = \"same\"\nlabel = \"b\"\n";
        assert!(matches!(Config::parse(toml), Err(Error::ConfigInvalid(_))));
    }

    #[test]
    fn rejects_empty_label() {
        let toml = "[[account]]\naccount_uuid = \"u\"\nstash = \"s\"\nlabel = \"\"\n";
        assert!(matches!(Config::parse(toml), Err(Error::ConfigInvalid(_))));
    }

    #[test]
    fn round_trips_render_then_parse() {
        let original = Config::parse(VALID).unwrap();
        let reparsed = Config::parse(&original.render()).unwrap();
        assert_eq!(original.tunables, reparsed.tunables);
        assert_eq!(original.roster, reparsed.roster);
    }

    #[test]
    fn round_trips_a_label_that_needs_escaping() {
        let toml = "[[account]]\n\
                    account_uuid = \"u\"\n\
                    stash = \"Sessiometer/acct-1\"\n\
                    label = \"tab\\there \\\"quote\\\" and \\\\ slash\"\n";
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.roster[0].label, "tab\there \"quote\" and \\ slash");
        let reparsed = Config::parse(&config.render()).unwrap();
        assert_eq!(reparsed.roster[0].label, config.roster[0].label);
    }

    #[test]
    fn rendered_config_documents_the_tunables() {
        let text = Config::parse(VALID).unwrap().render();
        // AC #5: the written file carries the inline tunable docs, in particular
        // the session_floor "only swap TO an account below this %" semantics.
        assert!(text.contains("# Only swap TO an account"));
        for key in [
            "poll_secs",
            "cooldown_secs",
            "session_floor",
            "session_trigger",
            "monitor_401_n",
        ] {
            assert!(text.contains(key), "rendered config must mention {key}");
        }
    }

    #[test]
    fn accessors_map_tunables_to_daemon_inputs() {
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.poll_interval(), Duration::from_secs(30));
        assert!((config.swap_threshold() - 0.90).abs() < 1e-9);
    }

    #[test]
    fn basic_string_escapes_specials() {
        assert_eq!(basic_string("plain"), "\"plain\"");
        assert_eq!(basic_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(basic_string("tab\tnl\n"), "\"tab\\tnl\\n\"");
        assert_eq!(basic_string("\u{0}"), "\"\\u0000\"");
    }

    #[test]
    fn accepts_inclusive_bounds() {
        // Each bound's edge is valid: trigger 50/99, floor == trigger, poll 5/3600,
        // cooldown 0/3600, monitor 1/20.
        for fragment in [
            "session_trigger = 50\nsession_floor = 0",
            "session_trigger = 99\nsession_floor = 99", // floor == trigger is allowed
            "poll_secs = 5",
            "poll_secs = 3600",
            "cooldown_secs = 0",
            "cooldown_secs = 3600",
            "monitor_401_n = 1",
            "monitor_401_n = 20",
        ] {
            assert!(
                Config::parse(&with_tunables(fragment)).is_ok(),
                "inclusive bound should be accepted: {fragment:?}"
            );
        }
    }

    #[test]
    fn load_path_reports_not_found_for_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert!(matches!(
            Config::load_path(&path),
            Err(Error::ConfigNotFound { .. })
        ));
    }

    #[test]
    fn load_path_surfaces_a_malformed_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, b"][").unwrap();
        assert!(matches!(
            Config::load_path(&path),
            Err(Error::ConfigParse(_))
        ));
    }

    /// AC #3 + #4 end-to-end: a config written the way `capture` will write it
    /// (rendered → `write_private_file`) is read back identically by the daemon's
    /// `load`, and the on-disk file is `0600`.
    #[test]
    fn written_config_round_trips_through_disk_at_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let original = Config::parse(VALID).unwrap();
        paths::write_private_file(&path, original.render().as_bytes()).unwrap();

        let loaded = Config::load_path(&path).unwrap();
        assert_eq!(loaded.tunables, original.tunables);
        assert_eq!(loaded.roster, original.roster);

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
