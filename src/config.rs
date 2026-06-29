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
use crate::timing::{Jitter, Strategy};

/// Default seconds between usage polls. Issue #38 lengthened this from the
/// original fixed 60 s to a longer base that the normal poll jitter then
/// decorrelates across accounts/cycles.
const DEFAULT_POLL_SECS: u64 = 300;
/// Default standard deviation (seconds) of the poll interval's normal jitter —
/// ~10% of [`DEFAULT_POLL_SECS`]. Poll is the one tunable that jitters by
/// default (issue #38 AC: "poll interval uses a longer base + normal jitter").
const DEFAULT_POLL_JITTER_STDDEV: f64 = 30.0;
/// Default seconds to wait after a swap before another is allowed.
const DEFAULT_COOLDOWN_SECS: u64 = 60;
/// Default `session_trigger` percent.
const DEFAULT_SESSION_TRIGGER: u8 = 95;
/// Default `weekly_trigger` percent — separate from and higher than
/// `session_trigger` (issue #41): the weekly window is the longer, harder limit,
/// so the active account is allowed closer to full on it before a swap-away.
const DEFAULT_WEEKLY_TRIGGER: u8 = 98;
/// Default consecutive-401 count before an account is treated as rejected.
const DEFAULT_MONITOR_401_N: u8 = 3;
/// Default consecutive recovery-probe successes before a quarantined (dead)
/// account is restored to the rotation (issue #42).
const DEFAULT_MONITOR_RECOVERY_M: u8 = 2;

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
    /// (`Sessiometer/<account_uuid>`); roster key.
    pub(crate) stash: String,
    /// Human-readable label shown by `list` / `status`.
    pub(crate) label: String,
    /// Whether this account participates in the rotation (issue #36). A disabled
    /// account stays in the roster and keeps its stash, but the daemon never swaps
    /// TO it and does not poll it; `sessiometer enable` returns it to the candidate
    /// pool. Defaults to `true` — a config entry that omits the key (every pre-#36
    /// one) loads fully enabled. The reversible sibling of removal (#13), which
    /// instead deletes the stash.
    pub(crate) enabled: bool,
}

/// The daemon tunables, validated into their typed ranges.
///
/// `Eq` is intentionally not derived: the timing strategies (issue #38) carry
/// `f64` magnitudes, so only `PartialEq` is available — sufficient for the tests'
/// `assert_eq!` and for the render round-trip check.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Tunables {
    /// Seconds between usage polls (`5..=3600`).
    pub(crate) poll_secs: u64,
    /// Seconds to wait after a swap before another is allowed (`0..=3600`).
    /// Consumed by the cooldown logic (#10 / #11).
    #[allow(dead_code)]
    pub(crate) cooldown_secs: u64,
    /// Opt-in swap-target session guard (#10): when `Some(pct)`, only swap *to* an
    /// account whose session usage is below `pct` percent (`0..=session_trigger`);
    /// `None` (the default) disables the guard, so target choice rests on the
    /// soonest-reset selection alone (issue #37) — the configuration under which the
    /// post-swap cooldown alone bounds oscillation. OFF by default: operators opt
    /// in, and a sensible enabled value mirrors `session_trigger`.
    pub(crate) session_floor: Option<u8>,
    /// Swap *away* from the active account at or above this session percent
    /// (`50..=99`).
    pub(crate) session_trigger: u8,
    /// Swap *away* from the active account at or above this WEEKLY percent
    /// (`50..=99`) — the second, independent trigger dimension (issue #41).
    /// Separate from `session_trigger` (no cross-field constraint), typically set
    /// higher; the daemon swaps when EITHER dimension reaches its own trigger.
    pub(crate) weekly_trigger: u8,
    /// Consecutive non-scope 401s before an account is treated as DEAD (`1..=20`).
    /// Consumed by the daemon's per-account health state (issue #42): the Nth
    /// consecutive 401 on an account's stored token quarantines it (stop polling /
    /// selecting it) and, if it is the active account, triggers an emergency swap.
    /// Distinct from #13's re-auth re-stash, which is driven by canonical-change
    /// detection ([`crate::keychain::CanonicalWatch`]), not by 401s.
    pub(crate) monitor_401_n: u8,
    /// Consecutive recovery-probe successes before a quarantined (dead) account is
    /// restored to the rotation (`1..=20`, issue #42). After the operator re-logs-in
    /// a quarantined account (a canonical-change re-stash, #13), the account must
    /// poll successfully this many times in a row before it is un-quarantined.
    pub(crate) monitor_recovery_m: u8,
    /// Poll-interval timing strategy (issue #38): base = `poll_secs` (seconds),
    /// normal jitter by default. The daemon draws + clamps to `5..=3600` each
    /// cycle instead of sleeping a fixed interval.
    pub(crate) poll_strategy: Strategy,
    /// Swap-away trigger timing strategy (issue #38), in the PERCENT domain:
    /// base = `session_trigger`, no jitter unless configured. Drawn + clamped to
    /// `50..=99` each cycle, then divided by 100 for the swap decision.
    pub(crate) trigger_strategy: Strategy,
    /// Weekly swap-away trigger timing strategy (issue #41), in the PERCENT
    /// domain: base = `weekly_trigger`, no jitter unless configured. Drawn +
    /// clamped to `50..=99` each cycle, then divided by 100 for the swap decision
    /// — the weekly-dimension counterpart of `trigger_strategy`.
    pub(crate) weekly_trigger_strategy: Strategy,
    /// Post-swap cooldown timing strategy (issue #38), in seconds: base =
    /// `cooldown_secs`, no jitter unless configured. Drawn + clamped to `0..=3600`
    /// each cycle.
    pub(crate) cooldown_strategy: Strategy,
}

impl Default for Tunables {
    fn default() -> Self {
        Self {
            poll_secs: DEFAULT_POLL_SECS,
            cooldown_secs: DEFAULT_COOLDOWN_SECS,
            session_floor: None,
            session_trigger: DEFAULT_SESSION_TRIGGER,
            weekly_trigger: DEFAULT_WEEKLY_TRIGGER,
            monitor_401_n: DEFAULT_MONITOR_401_N,
            monitor_recovery_m: DEFAULT_MONITOR_RECOVERY_M,
            poll_strategy: Strategy {
                base: DEFAULT_POLL_SECS as f64,
                jitter: default_poll_jitter(),
            },
            trigger_strategy: Strategy::fixed(f64::from(DEFAULT_SESSION_TRIGGER)),
            weekly_trigger_strategy: Strategy::fixed(f64::from(DEFAULT_WEEKLY_TRIGGER)),
            cooldown_strategy: Strategy::fixed(DEFAULT_COOLDOWN_SECS as f64),
        }
    }
}

/// The default poll-interval jitter: normal, so polls decorrelate out of the box
/// (issue #38). Trigger, weekly trigger, and cooldown default to [`Jitter::None`].
fn default_poll_jitter() -> Jitter {
    Jitter::Normal {
        stddev: DEFAULT_POLL_JITTER_STDDEV,
    }
}

/// The validated configuration: the captured roster plus tunables.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// Captured accounts (unique `account_uuid` + `stash`; no fixed upper bound —
    /// #35, and possibly empty — the daemon's "at least one" precondition is
    /// [`Config::require_roster`], not a parse-time rule, so `capture` can load a
    /// tunables-only file to add the first account). Consumed by the swap engine
    /// (#6 / #7) and by `list` / `status` (#17 / #9).
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
    /// malformed file. A well-formed file with an *empty* roster loads
    /// successfully (tunables preserved) — the "at least one account" rule is the
    /// daemon's [`Config::require_roster`] precondition, so `capture` can load a
    /// tunables-only file to add the first account.
    pub(crate) fn load() -> Result<Self> {
        Self::load_path(&paths::config_file()?)
    }

    /// [`load`](Config::load) against an explicit path — the injectable seam, so
    /// the file-I/O branches (absent → [`Error::ConfigNotFound`], other read
    /// failure → [`Error::Io`]) are testable without touching the real config
    /// location. `pub(crate)` so [`capture`](crate::capture)'s `load_existing_from`
    /// routes through the same seam rather than re-implementing the read (#59).
    pub(crate) fn load_path(path: &Path) -> Result<Self> {
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

    /// The base poll interval — the un-jittered `poll_secs`. The run loop now
    /// draws a jittered interval each cycle from the poll strategy (issue #38),
    /// so this is a tested accessor for the base rather than the live cadence.
    #[allow(dead_code)]
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

    /// Ensure the roster holds at least one account — the daemon's precondition.
    ///
    /// The non-empty-roster invariant belongs to the *daemon* (`run` has nothing to
    /// rotate across with an empty roster), not to parsing: `capture` and the
    /// roster-editing commands must load a possibly-empty config precisely to
    /// populate it (a brand-new tunables-only file, or one whose last account was
    /// just `remove`d). Enforcing it here — at the one consumer that requires it —
    /// lets `capture` bootstrap the first account while `run` still refuses to start
    /// on an empty roster. Maps to the friendly [`Error::RosterEmpty`], the same
    /// empty-state the offline `list` view reports.
    pub(crate) fn require_roster(&self) -> Result<()> {
        if self.roster.is_empty() {
            Err(Error::RosterEmpty)
        } else {
            Ok(())
        }
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
        // The weekly trigger is independent of the session trigger (issue #41):
        // its own 50..=99 bound, with NO cross-field rule — weekly may sit below
        // session (an unusual but valid operator choice), so both are configurable
        // independently (AC #3).
        range("weekly_trigger", t.weekly_trigger, 50, 99)?;
        // session_floor is opt-in (#10): absent → None (the guard is off). When
        // present, its lower bound is 0 and its upper bound is session_trigger (a
        // higher floor could never admit a target), the latter a distinct
        // cross-field error.
        let session_floor = match t.session_floor {
            None => None,
            Some(floor) => {
                if floor < 0 {
                    return Err(Error::ConfigInvalid(format!(
                        "session_floor must be in 0..={}, got {floor}",
                        t.session_trigger
                    )));
                }
                if floor > t.session_trigger {
                    return Err(Error::ConfigFloorAboveTrigger {
                        floor,
                        trigger: t.session_trigger,
                    });
                }
                Some(floor as u8)
            }
        };
        range("poll_secs", t.poll_secs, 5, 3600)?;
        range("cooldown_secs", t.cooldown_secs, 0, 3600)?;
        range("monitor_401_n", t.monitor_401_n, 1, 20)?;
        range("monitor_recovery_m", t.monitor_recovery_m, 1, 20)?;

        // Jitter specs (issue #38): each optional and validated to a clear load
        // error (parse-or-error). Poll jitters normally by default; trigger and
        // cooldown are fixed unless the operator configures a strategy.
        let poll_jitter = parse_jitter("poll", raw.jitter.poll, default_poll_jitter())?;
        let trigger_jitter = parse_jitter("trigger", raw.jitter.trigger, Jitter::None)?;
        let weekly_trigger_jitter =
            parse_jitter("weekly_trigger", raw.jitter.weekly_trigger, Jitter::None)?;
        let cooldown_jitter = parse_jitter("cooldown", raw.jitter.cooldown, Jitter::None)?;

        // Ranges are checked above, so these narrowing casts cannot truncate. The
        // strategy bases are the same validated scalars (issue #38): the daemon
        // draws + clamps from the strategy each cycle.
        let tunables = Tunables {
            poll_secs: t.poll_secs as u64,
            cooldown_secs: t.cooldown_secs as u64,
            session_floor,
            session_trigger: t.session_trigger as u8,
            weekly_trigger: t.weekly_trigger as u8,
            monitor_401_n: t.monitor_401_n as u8,
            monitor_recovery_m: t.monitor_recovery_m as u8,
            poll_strategy: Strategy {
                base: t.poll_secs as f64,
                jitter: poll_jitter,
            },
            trigger_strategy: Strategy {
                base: t.session_trigger as f64,
                jitter: trigger_jitter,
            },
            weekly_trigger_strategy: Strategy {
                base: t.weekly_trigger as f64,
                jitter: weekly_trigger_jitter,
            },
            cooldown_strategy: Strategy {
                base: t.cooldown_secs as f64,
                jitter: cooldown_jitter,
            },
        };

        // The roster has neither a lower nor an upper bound at PARSE time. An empty
        // roster is a valid intermediate state — a fresh tunables-only file, or one
        // whose last account was just `remove`d — and `capture` must be able to load
        // such a file to add the first account (otherwise it can never bootstrap).
        // The "at least one account" rule is the DAEMON's precondition, enforced by
        // its consumer via [`Config::require_roster`] (called from `run`), NOT here.
        // And there is deliberately no upper bound: the operator rotates across as
        // many accounts as they capture (#35).
        //
        // Poll-cost note (document, don't cap): the daemon polls every roster
        // account with its own `curl` each `poll_secs` tick (see
        // `daemon::Daemon::tick`), so a larger roster grows per-tick work and
        // outbound request volume linearly. The operator self-limits by choice
        // (smaller roster, or a larger `poll_secs`); the tool enforces no ceiling.
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
                enabled: account.enabled,
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
             # active session. This is NOT the level that triggers a swap. OFF by default\n\
             # (opt-in, #10): uncomment to enable; a sensible value mirrors session_trigger.\n",
        );
        match t.session_floor {
            Some(floor) => out.push_str(&format!("session_floor = {floor}\n")),
            None => out.push_str(&format!("# session_floor = {}\n", t.session_trigger)),
        }
        out.push_str(
            "# Swap AWAY from the active account at or above this session percent (50..=99).\n",
        );
        out.push_str(&format!("session_trigger = {}\n", t.session_trigger));
        out.push_str(
            "# Swap AWAY from the active account at or above this WEEKLY percent (50..=99).\n\
             # Independent of session_trigger (typically higher): a swap fires when EITHER\n\
             # dimension reaches its own trigger.\n",
        );
        out.push_str(&format!("weekly_trigger = {}\n", t.weekly_trigger));
        out.push_str(
            "# Consecutive non-scope 401s before an account is treated as DEAD and\n\
             # quarantined (1..=20).\n",
        );
        out.push_str(&format!("monitor_401_n = {}\n", t.monitor_401_n));
        out.push_str(
            "# Consecutive recovery-probe successes before a quarantined (dead) account\n\
             # is restored to the rotation after a re-login (1..=20).\n",
        );
        out.push_str(&format!("monitor_recovery_m = {}\n", t.monitor_recovery_m));

        // Per-cycle timing jitter (issue #38): drawn each cycle and clamped to the
        // tunable's valid range, to decorrelate polling/swaps across cycles.
        out.push_str("\n[jitter]\n");
        out.push_str(
            "# Randomization drawn each cycle and clamped to the tunable's range.\n\
             # kind = \"none\" | \"uniform\" (with `spread`) | \"normal\" (with `stddev`).\n",
        );
        out.push_str(&format!(
            "poll = {}\n",
            render_jitter(&t.poll_strategy.jitter)
        ));
        out.push_str(&format!(
            "trigger = {}\n",
            render_jitter(&t.trigger_strategy.jitter)
        ));
        out.push_str(&format!(
            "weekly_trigger = {}\n",
            render_jitter(&t.weekly_trigger_strategy.jitter)
        ));
        out.push_str(&format!(
            "cooldown = {}\n",
            render_jitter(&t.cooldown_strategy.jitter)
        ));

        for account in &self.roster {
            out.push_str("\n[[account]]\n");
            out.push_str(&format!(
                "account_uuid = {}\n",
                basic_string(&account.account_uuid)
            ));
            out.push_str(&format!("stash = {}\n", basic_string(&account.stash)));
            out.push_str(&format!("label = {}\n", basic_string(&account.label)));
            // Issue #36: in the rotation? A disabled account is kept (and keeps its
            // stash) but is never polled or swapped to — `sessiometer enable`
            // returns it. Defaults to true; omitting the key leaves it enabled.
            out.push_str(
                "# In the rotation? false parks it (kept, but never polled or swapped to). Default true.\n",
            );
            out.push_str(&format!("enabled = {}\n", account.enabled));
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

/// Validate one tunable's optional `[jitter]` spec into a [`Jitter`], or fail at
/// load (issue #38 parse-or-error). `field` names the tunable in any error;
/// `default` applies when the spec is absent. Enforces the `none|uniform|normal`
/// vocabulary, the correct magnitude key per kind (`spread` for uniform, `stddev`
/// for normal, none for `none`), and a non-negative, finite magnitude.
fn parse_jitter(
    field: &'static str,
    spec: Option<RawJitterSpec>,
    default: Jitter,
) -> Result<Jitter> {
    let Some(spec) = spec else {
        return Ok(default);
    };
    match spec.kind.as_str() {
        "none" => {
            if spec.spread.is_some() || spec.stddev.is_some() {
                return Err(Error::ConfigInvalid(format!(
                    "{field} jitter \"none\" takes no magnitude (drop spread/stddev)"
                )));
            }
            Ok(Jitter::None)
        }
        "uniform" => {
            if spec.stddev.is_some() {
                return Err(Error::ConfigInvalid(format!(
                    "{field} jitter \"uniform\" takes `spread`, not `stddev`"
                )));
            }
            let spread = spec.spread.ok_or_else(|| {
                Error::ConfigInvalid(format!("{field} jitter \"uniform\" requires `spread`"))
            })?;
            non_negative(field, "spread", spread)?;
            Ok(Jitter::Uniform { spread })
        }
        "normal" => {
            if spec.spread.is_some() {
                return Err(Error::ConfigInvalid(format!(
                    "{field} jitter \"normal\" takes `stddev`, not `spread`"
                )));
            }
            let stddev = spec.stddev.ok_or_else(|| {
                Error::ConfigInvalid(format!("{field} jitter \"normal\" requires `stddev`"))
            })?;
            non_negative(field, "stddev", stddev)?;
            Ok(Jitter::Normal { stddev })
        }
        other => Err(Error::ConfigInvalid(format!(
            "{field} jitter kind must be none|uniform|normal, got \"{other}\""
        ))),
    }
}

/// Reject a negative or non-finite jitter magnitude, naming the field/param.
fn non_negative(field: &str, param: &str, value: f64) -> Result<()> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(Error::ConfigInvalid(format!(
            "{field} jitter {param} must be a non-negative number, got {value}"
        )))
    }
}

/// Render a [`Jitter`] as the inline TOML table [`RawJitterSpec`] parses back
/// (issue #38). Magnitudes use the float-debug form so they always carry a
/// decimal point and round-trip as TOML floats (never as integers).
#[allow(dead_code)]
fn render_jitter(jitter: &Jitter) -> String {
    match jitter {
        Jitter::None => "{ kind = \"none\" }".to_string(),
        Jitter::Uniform { spread } => format!("{{ kind = \"uniform\", spread = {spread:?} }}"),
        Jitter::Normal { stddev } => format!("{{ kind = \"normal\", stddev = {stddev:?} }}"),
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
    #[serde(default)]
    jitter: RawJitter,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAccount {
    account_uuid: String,
    stash: String,
    label: String,
    /// In the rotation? (issue #36) Absent → `true`: a pre-#36 `[[account]]` entry
    /// omits the key and must stay fully enabled (backward-compatible default).
    #[serde(default = "default_account_enabled")]
    enabled: bool,
}

/// The backward-compatible default for [`RawAccount::enabled`] (issue #36): an
/// account entry that omits `enabled` is enabled.
fn default_account_enabled() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTunables {
    #[serde(default = "default_poll_secs")]
    poll_secs: i64,
    #[serde(default = "default_cooldown_secs")]
    cooldown_secs: i64,
    /// Opt-in (#10): absent → `None` (the session-floor guard is off by default).
    #[serde(default)]
    session_floor: Option<i64>,
    #[serde(default = "default_session_trigger")]
    session_trigger: i64,
    #[serde(default = "default_weekly_trigger")]
    weekly_trigger: i64,
    #[serde(default = "default_monitor_401_n")]
    monitor_401_n: i64,
    #[serde(default = "default_monitor_recovery_m")]
    monitor_recovery_m: i64,
}

impl Default for RawTunables {
    fn default() -> Self {
        Self {
            poll_secs: default_poll_secs(),
            cooldown_secs: default_cooldown_secs(),
            session_floor: None,
            session_trigger: default_session_trigger(),
            weekly_trigger: default_weekly_trigger(),
            monitor_401_n: default_monitor_401_n(),
            monitor_recovery_m: default_monitor_recovery_m(),
        }
    }
}

fn default_poll_secs() -> i64 {
    DEFAULT_POLL_SECS as i64
}
fn default_cooldown_secs() -> i64 {
    DEFAULT_COOLDOWN_SECS as i64
}
fn default_session_trigger() -> i64 {
    i64::from(DEFAULT_SESSION_TRIGGER)
}
fn default_weekly_trigger() -> i64 {
    i64::from(DEFAULT_WEEKLY_TRIGGER)
}
fn default_monitor_401_n() -> i64 {
    i64::from(DEFAULT_MONITOR_401_N)
}
fn default_monitor_recovery_m() -> i64 {
    i64::from(DEFAULT_MONITOR_RECOVERY_M)
}

/// Permissive deserialization of the optional `[jitter]` table (issue #38): each
/// tunable's spec is optional (absent → its default jitter). `deny_unknown_fields`
/// rejects a stray tunable name as a parse error.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawJitter {
    #[serde(default)]
    poll: Option<RawJitterSpec>,
    #[serde(default)]
    trigger: Option<RawJitterSpec>,
    #[serde(default)]
    weekly_trigger: Option<RawJitterSpec>,
    #[serde(default)]
    cooldown: Option<RawJitterSpec>,
}

/// One tunable's jitter spec: a `kind` plus its magnitude (`spread` for uniform,
/// `stddev` for normal). Both magnitudes are kept optional and wide here so a
/// kind/magnitude mismatch reaches [`parse_jitter`] as a clear domain error
/// rather than a bare `serde` type error. Magnitudes are TOML floats (write a
/// decimal, e.g. `spread = 2.0`).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJitterSpec {
    kind: String,
    #[serde(default)]
    spread: Option<f64>,
    #[serde(default)]
    stddev: Option<f64>,
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
weekly_trigger = 97
monitor_401_n = 5
monitor_recovery_m = 4

[[account]]
account_uuid = "11111111-1111-1111-1111-111111111111"
stash = "Sessiometer/11111111-1111-1111-1111-111111111111"
label = "work"

[[account]]
account_uuid = "22222222-2222-2222-2222-222222222222"
stash = "Sessiometer/22222222-2222-2222-2222-222222222222"
label = "personal"
"#;

    /// A minimal valid roster body with one account and the given `[tunables]`
    /// fragment spliced in.
    fn with_tunables(fragment: &str) -> String {
        format!(
            "[tunables]\n{fragment}\n\
             [[account]]\n\
             account_uuid = \"u\"\n\
             stash = \"Sessiometer/u\"\n\
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
                session_floor: Some(70),
                session_trigger: 90,
                weekly_trigger: 97,
                monitor_401_n: 5,
                monitor_recovery_m: 4,
                // No [jitter] table in VALID → default strategies: poll jitters
                // normally (base from poll_secs), trigger/weekly_trigger/cooldown
                // are fixed at their respective bases.
                poll_strategy: Strategy {
                    base: 30.0,
                    jitter: default_poll_jitter(),
                },
                trigger_strategy: Strategy::fixed(90.0),
                weekly_trigger_strategy: Strategy::fixed(97.0),
                cooldown_strategy: Strategy::fixed(45.0),
            }
        );
        assert_eq!(config.roster[0].label, "work");
        assert_eq!(
            config.roster[1].stash,
            "Sessiometer/22222222-2222-2222-2222-222222222222"
        );
    }

    #[test]
    fn tunables_default_when_table_absent() {
        let toml = "[[account]]\n\
                    account_uuid = \"u\"\n\
                    stash = \"Sessiometer/u\"\n\
                    label = \"only\"\n";
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.tunables, Tunables::default());
        assert_eq!(config.tunables.session_trigger, 95);
        // #10: the session-floor guard is OFF by default (opt-in).
        assert_eq!(config.tunables.session_floor, None);
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
    fn rejects_out_of_range_weekly_trigger() {
        // #41: the weekly trigger carries the same 50..=99 bound as the session one.
        for trigger in ["49", "100", "120"] {
            let toml = with_tunables(&format!("weekly_trigger = {trigger}"));
            assert!(
                matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))),
                "weekly_trigger = {trigger} should be rejected"
            );
        }
    }

    #[test]
    fn session_and_weekly_triggers_are_independently_configurable() {
        // AC #3: the two triggers are set independently — there is NO cross-field
        // rule, so weekly may even sit BELOW session (unlike session_floor, which
        // is capped at session_trigger).
        let t = Config::parse(&with_tunables("session_trigger = 90\nweekly_trigger = 99"))
            .unwrap()
            .tunables;
        assert_eq!(t.session_trigger, 90);
        assert_eq!(t.weekly_trigger, 99);
        assert_eq!(t.trigger_strategy.base, 90.0);
        assert_eq!(t.weekly_trigger_strategy.base, 99.0);

        // weekly BELOW session is accepted (no floor-style cross-field constraint).
        let inverted = Config::parse(&with_tunables("session_trigger = 95\nweekly_trigger = 60"))
            .unwrap()
            .tunables;
        assert_eq!(inverted.session_trigger, 95);
        assert_eq!(inverted.weekly_trigger, 60);
    }

    #[test]
    fn weekly_trigger_defaults_higher_than_session_when_absent() {
        // An absent weekly_trigger takes its (higher-than-session) default.
        let t = Config::parse(&with_tunables("session_trigger = 95"))
            .unwrap()
            .tunables;
        assert_eq!(t.weekly_trigger, DEFAULT_WEEKLY_TRIGGER);
        assert!(t.weekly_trigger > t.session_trigger);
        assert_eq!(
            t.weekly_trigger_strategy.base,
            f64::from(DEFAULT_WEEKLY_TRIGGER)
        );
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
    fn session_floor_is_off_by_default_and_opt_in() {
        // #10: an absent session_floor leaves the guard OFF (None), even when other
        // tunables are set…
        let off = Config::parse(&with_tunables("session_trigger = 95")).unwrap();
        assert_eq!(off.tunables.session_floor, None);
        // …and a present value opts in at that percent.
        let on = Config::parse(&with_tunables("session_floor = 90\nsession_trigger = 95")).unwrap();
        assert_eq!(on.tunables.session_floor, Some(90));
    }

    #[test]
    fn rendered_default_config_documents_session_floor_as_off() {
        // With the floor off, render emits a commented-out opt-in line (suggesting
        // the trigger value) and round-trips back to None — never a live assignment.
        let mut config = Config::parse(VALID).unwrap();
        config.tunables.session_floor = None;
        let text = config.render();
        assert!(text.contains("OFF by default"), "got {text}");
        assert!(text.contains("# session_floor ="), "got {text}");
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(reparsed.tunables.session_floor, None);
    }

    #[test]
    fn rejects_each_out_of_range_tunable() {
        for (key, value) in [
            ("poll_secs", "4"),
            ("poll_secs", "3601"),
            ("cooldown_secs", "3601"),
            ("monitor_401_n", "0"),
            ("monitor_401_n", "21"),
            ("monitor_recovery_m", "0"),
            ("monitor_recovery_m", "21"),
        ] {
            let toml = with_tunables(&format!("{key} = {value}"));
            assert!(
                matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))),
                "{key} = {value} should be rejected"
            );
        }
    }

    #[test]
    fn accepts_a_roster_less_config_and_preserves_tunables() {
        // Regression (the `capture` bootstrap bug, #58): a well-formed tunables-only
        // file must PARSE (empty roster) and PRESERVE the operator's tunables, so
        // `capture` can load it to add the first account. The "at least one account"
        // rule is the daemon's `require_roster` precondition, not a parse rejection.
        let config = Config::parse("[tunables]\npoll_secs = 120\nsession_floor = 80\n").unwrap();
        assert!(config.roster.is_empty());
        assert_eq!(config.tunables.poll_secs, 120);
        assert_eq!(config.tunables.session_floor, Some(80));
    }

    #[test]
    fn require_roster_rejects_an_empty_roster_with_the_friendly_empty_state() {
        // The daemon's precondition (#58): an empty roster is the friendly
        // `RosterEmpty` ("nothing captured yet"), the same state the offline `list`
        // reports — not a raw parse/validation error.
        let config = Config::parse("[tunables]\npoll_secs = 60\n").unwrap();
        assert!(matches!(config.require_roster(), Err(Error::RosterEmpty)));
    }

    #[test]
    fn require_roster_accepts_a_populated_roster() {
        let config = Config::parse(VALID).unwrap();
        assert!(config.require_roster().is_ok());
    }

    #[test]
    fn accepts_a_roster_larger_than_the_former_five_cap() {
        // #35: the roster has no fixed upper bound — a config well beyond the
        // former 5-account cap loads and validates.
        let mut toml = String::new();
        for i in 0..8 {
            toml.push_str(&format!(
                "[[account]]\naccount_uuid = \"u{i}\"\nstash = \"s{i}\"\nlabel = \"l{i}\"\n"
            ));
        }
        let config = Config::parse(&toml).unwrap();
        assert_eq!(config.roster.len(), 8);
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

    // --- account enable/disable (issue #36) --------------------------------

    #[test]
    fn account_enabled_defaults_to_true_when_the_key_is_absent() {
        // Backward-compat AC: every pre-#36 `[[account]]` omits `enabled`, so an
        // absent key must load fully enabled — VALID's two accounts have no key.
        let config = Config::parse(VALID).unwrap();
        assert!(
            config.roster.iter().all(|a| a.enabled),
            "default is enabled"
        );
    }

    #[test]
    fn account_enabled_false_parses_as_disabled() {
        let toml =
            "[[account]]\naccount_uuid = \"u\"\nstash = \"s\"\nlabel = \"l\"\nenabled = false\n";
        let config = Config::parse(toml).unwrap();
        assert!(!config.roster[0].enabled);
    }

    #[test]
    fn rendered_config_documents_and_round_trips_the_enabled_flag() {
        // The renderer writes `enabled` for every account (capture writes it; #36)
        // with an inline doc, and a disabled account survives a render→parse cycle.
        let mut config = Config::parse(VALID).unwrap();
        config.roster[1].enabled = false;
        let text = config.render();
        assert!(text.contains("enabled = true"), "got {text}");
        assert!(text.contains("enabled = false"), "got {text}");
        assert!(
            text.contains("# In the rotation?"),
            "documents enabled: {text}"
        );
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(reparsed.roster, config.roster);
        assert!(reparsed.roster[0].enabled);
        assert!(!reparsed.roster[1].enabled);
    }

    // --- timing jitter strategies (issue #38) ------------------------------

    /// A minimal valid roster body with one account and the given `[jitter]`
    /// fragment spliced in (the `[tunables]` table is absent → its defaults).
    fn with_jitter(fragment: &str) -> String {
        format!(
            "[jitter]\n{fragment}\n\
             [[account]]\n\
             account_uuid = \"u\"\n\
             stash = \"Sessiometer/u\"\n\
             label = \"l\"\n"
        )
    }

    #[test]
    fn poll_jitter_defaults_to_normal_trigger_and_cooldown_stay_fixed() {
        // AC: poll interval uses normal jitter by default; trigger, weekly_trigger
        // and cooldown are fixed unless the operator configures a strategy. Bases
        // mirror the validated scalar tunables.
        let t = Config::parse(VALID).unwrap().tunables;
        assert_eq!(
            t.poll_strategy.jitter,
            Jitter::Normal {
                stddev: DEFAULT_POLL_JITTER_STDDEV
            }
        );
        assert_eq!(t.trigger_strategy.jitter, Jitter::None);
        assert_eq!(t.weekly_trigger_strategy.jitter, Jitter::None);
        assert_eq!(t.cooldown_strategy.jitter, Jitter::None);
        assert_eq!(t.poll_strategy.base, 30.0);
        assert_eq!(t.trigger_strategy.base, 90.0);
        assert_eq!(t.weekly_trigger_strategy.base, 97.0);
        assert_eq!(t.cooldown_strategy.base, 45.0);
    }

    #[test]
    fn default_poll_base_is_longer_than_the_original_sixty_seconds() {
        // AC: the poll interval moves to a LONGER base than the original fixed
        // 60 s, with normal jitter.
        let t = Tunables::default();
        assert!(
            t.poll_secs > 60,
            "default poll base must exceed the old 60 s"
        );
        assert_eq!(t.poll_strategy.base, t.poll_secs as f64);
        assert!(matches!(t.poll_strategy.jitter, Jitter::Normal { .. }));
    }

    #[test]
    fn parses_a_full_jitter_table() {
        let toml = with_jitter(
            "poll = { kind = \"normal\", stddev = 25.0 }\n\
             trigger = { kind = \"uniform\", spread = 2.5 }\n\
             weekly_trigger = { kind = \"normal\", stddev = 1.0 }\n\
             cooldown = { kind = \"none\" }",
        );
        let t = Config::parse(&toml).unwrap().tunables;
        assert_eq!(t.poll_strategy.jitter, Jitter::Normal { stddev: 25.0 });
        assert_eq!(t.trigger_strategy.jitter, Jitter::Uniform { spread: 2.5 });
        assert_eq!(
            t.weekly_trigger_strategy.jitter,
            Jitter::Normal { stddev: 1.0 }
        );
        assert_eq!(t.cooldown_strategy.jitter, Jitter::None);
    }

    #[test]
    fn rejects_every_malformed_jitter_spec() {
        // parse-or-error: each malformed spec is rejected at load.
        for fragment in [
            "poll = { kind = \"gaussian\", stddev = 1.0 }", // unknown kind
            "poll = { kind = \"normal\", stddev = -1.0 }",  // negative magnitude
            "poll = { kind = \"uniform\", spread = -0.1 }", // negative magnitude
            "poll = { kind = \"normal\", spread = 1.0 }",   // wrong key for kind
            "poll = { kind = \"uniform\", stddev = 1.0 }",  // wrong key for kind
            "poll = { kind = \"none\", stddev = 1.0 }",     // none takes no magnitude
            "poll = { kind = \"normal\" }",                 // missing magnitude
            "poll = { kind = \"uniform\" }",                // missing magnitude
        ] {
            assert!(
                matches!(
                    Config::parse(&with_jitter(fragment)),
                    Err(Error::ConfigInvalid(_))
                ),
                "jitter spec should be rejected: {fragment}"
            );
        }
    }

    #[test]
    fn rejects_an_unknown_jitter_field_or_tunable() {
        // deny_unknown_fields: a stray key in a spec is a parse error…
        assert!(matches!(
            Config::parse(&with_jitter(
                "poll = { kind = \"normal\", stddev = 1.0, bogus = 2.0 }"
            )),
            Err(Error::ConfigParse(_))
        ));
        // …and so is an unrecognized tunable name. The jitter tunables are
        // poll/trigger/weekly_trigger/cooldown (issue #41 added weekly_trigger); a
        // bare `weekly` (≠ the actual `weekly_trigger` key) is still unknown.
        assert!(matches!(
            Config::parse(&with_jitter("weekly = { kind = \"none\" }")),
            Err(Error::ConfigParse(_))
        ));
    }

    #[test]
    fn round_trips_a_configured_jitter_table() {
        let toml = with_jitter(
            "poll = { kind = \"uniform\", spread = 12.5 }\n\
             trigger = { kind = \"normal\", stddev = 1.5 }\n\
             weekly_trigger = { kind = \"uniform\", spread = 0.5 }\n\
             cooldown = { kind = \"none\" }",
        );
        let original = Config::parse(&toml).unwrap();
        let reparsed = Config::parse(&original.render()).unwrap();
        assert_eq!(original.tunables, reparsed.tunables);
    }

    #[test]
    fn rendered_config_documents_the_jitter_table() {
        let text = Config::parse(VALID).unwrap().render();
        assert!(text.contains("[jitter]"));
        for key in ["poll", "trigger", "weekly_trigger", "cooldown"] {
            assert!(
                text.contains(key),
                "rendered config must mention jitter.{key}"
            );
        }
        // The default poll jitter renders as a normal strategy with a decimal
        // magnitude (so it re-parses as a TOML float).
        assert!(text.contains("kind = \"normal\""));
        assert!(text.contains("stddev = 30.0"));
    }

    #[test]
    fn round_trips_a_label_that_needs_escaping() {
        let toml = "[[account]]\n\
                    account_uuid = \"u\"\n\
                    stash = \"Sessiometer/u\"\n\
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
            "weekly_trigger",
            "monitor_401_n",
            "monitor_recovery_m",
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
            "weekly_trigger = 50",
            "weekly_trigger = 99",
            "poll_secs = 5",
            "poll_secs = 3600",
            "cooldown_secs = 0",
            "cooldown_secs = 3600",
            "monitor_401_n = 1",
            "monitor_401_n = 20",
            "monitor_recovery_m = 1",
            "monitor_recovery_m = 20",
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
