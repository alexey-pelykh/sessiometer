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
//! `account_uuid` / `label`, never by token or email (issues #9, #15, #17). The
//! keychain stash name is derived from `account_uuid`, not stored (issue #70).
//! Error messages quote only those non-secret fields.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::paths;
use crate::timing::{Jitter, Strategy};

/// Default seconds between re-polling a given account — the per-account cadence.
/// Issue #38 lengthened this from the original fixed 60 s to a longer base that the
/// normal poll jitter then decorrelates across accounts/cycles; issue #76 confirmed
/// 5 min as a comfortable steady-state cadence (the rate-limit / transient back-off
/// widens it further under sustained `429` / `5xx`, so the base need not be
/// conservative). Issue #80 spreads the roster WITHIN this cadence — one account per
/// `poll_secs / N` sub-interval — rather than bursting all N at once.
const DEFAULT_POLL_SECS: u64 = 300;
/// Default standard deviation (seconds) of the poll interval's normal jitter —
/// ~20% of [`DEFAULT_POLL_SECS`]. Poll is the one tunable that jitters by default
/// (issue #38). Issue #76 WIDENED this from 30 s to 60 s so concurrent accounts
/// (and successive cycles) decorrelate over a ~1–3 min spread rather than clustering
/// within ~±30 s of the 5 min mark.
const DEFAULT_POLL_JITTER_STDDEV: f64 = 60.0;
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

/// Default seconds between periodic isolated-refresh ticks (issue #105). **Provisional**:
/// the refresh-token TTL is unproven (#101 deliberately ran no real refresh), so this is
/// NOT pinned to a specific `TTL/3` — it is a conservative one-hour default that keeps a
/// parked token fresh across a wide range of plausible access-token lifetimes without
/// churning refresh-token rotations, and doubles as the near-expiry SELECTION horizon (an
/// account is due when its stored token would expire before the next tick — see
/// [`RefreshConfig::cadence`]). Re-tune once the engine's own first-run telemetry
/// (`expiresAt`-delta + RT-rotation, the #102 [`crate::refresh::RefreshReport`]) pins the
/// server TTL; the #104 `poke` all-accounts horizon used the same one hour for the same reason.
const DEFAULT_REFRESH_CADENCE_SECS: u64 = 3600;
/// Default seconds the daemon must sit idle (no poll/usage/swap tick) before a refresh
/// tick fires (issue #105). **Provisional** like the cadence: one minute keeps the refresh
/// off the freshly-finished poll→usage→swap seam — it runs in the quiet part of the idle
/// gap — without waiting out a whole poll interval. Re-tune with the cadence.
const DEFAULT_REFRESH_IDLE_AFTER_SECS: u64 = 60;
/// Default seconds bounding ONE account's whole isolated-refresh cycle (issue #105). The
/// engine's internal `claude -p` spawn budget is ~40 s (#102); ninety seconds leaves
/// comfortable headroom for the seed, read-back and CAS re-stash around it, so a healthy
/// cycle is never truncated while a wedged one (a stuck keychain) still cannot stall the
/// daemon's return to polling. Cancelling mid-cycle is safe — the engine's RAII teardown
/// always runs (#102) and a forfeited token is bounded/recoverable (the engine Caller contract).
const DEFAULT_REFRESH_TIMEOUT_SECS: u64 = 90;

/// Default seconds bounding one whole interactive `login` capture (issue #135). Mirrors
/// [`crate::login::DEFAULT_LOGIN_TIMEOUT`] — the engine's per-call fallback — kept in sync by
/// the `default_login_timeout_matches_the_engine_default` test. Far longer than the refresh
/// timeout because a `/login` waits on a human completing a browser OAuth handoff, not a
/// headless `claude -p` spawn; the operator-tunable range (60..=600) bounds both an impatient
/// and a very patient operator.
const DEFAULT_LOGIN_TIMEOUT_SECS: u64 = 180;

/// The keychain service-name namespace every account's stash lives under; the
/// full name is `Sessiometer/<account_uuid>` ([`Account::stash`]). Kept as one
/// shared constant so the prefix has a single definition (issue #70).
pub(crate) const STASH_PREFIX: &str = "Sessiometer/";

/// One captured account in the roster. Keyed by `account_uuid` alone.
///
/// The fields beyond the uniqueness key are read by the write path
/// ([`Config::render`], for `capture` #4) and by `list` / `status` (#17 / #9);
/// the swap engine (#6 / #7) rotates across the roster. They are validated and
/// persisted here ahead of those consumers.
///
/// The keychain stash name is NOT stored: it is definitionally
/// `Sessiometer/<account_uuid>` (issue #70), so [`Account::stash`] derives it
/// from `account_uuid` rather than carrying a duplicate, separately-persisted
/// copy that could drift out of sync.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct Account {
    /// Stable per-account identifier (the Claude `account_uuid`); the sole roster
    /// key, and the basis for the derived [`stash`](Account::stash) name.
    pub(crate) account_uuid: String,
    /// Human-readable label shown by `list` / `status`.
    pub(crate) label: String,
    /// Whether this account participates in the rotation (issue #36). A disabled
    /// account stays in the roster (its keychain stash is untouched), but the
    /// daemon never swaps TO it and does not poll it; `sessiometer enable` returns
    /// it to the candidate pool. Defaults to `true` — a config entry that omits the
    /// key (every pre-#36 one) loads fully enabled. The reversible sibling of
    /// removal (#13), which instead deletes the stash.
    pub(crate) enabled: bool,
}

impl Account {
    /// The keychain service name the captured credential lives under,
    /// `Sessiometer/<account_uuid>`. Derived, never stored (issue #70): the stash
    /// is definitionally a function of `account_uuid`, so the roster keeps one
    /// source of truth and `config.toml` no longer carries a redundant
    /// `stash = …` line. See [`crate::stash`] for the stash layout itself.
    pub(crate) fn stash(&self) -> String {
        format!("{STASH_PREFIX}{}", self.account_uuid)
    }
}

/// The daemon tunables, validated into their typed ranges.
///
/// `Eq` is intentionally not derived: the timing strategies (issue #38) carry
/// `f64` magnitudes, so only `PartialEq` is available — sufficient for the tests'
/// `assert_eq!` and for the render round-trip check.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Tunables {
    /// Seconds between re-polling a GIVEN account (`5..=3600`) — the per-account
    /// cadence and the base of the rate-limit back-off. The daemon polls one account
    /// per `poll_secs / N` sub-interval (issue #80), so a roster of N accounts is swept
    /// once per `poll_secs` without bursting all N requests at once.
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
    /// restored to the rotation (`1..=20`, issue #42). This governs the
    /// spontaneous-revival path only: a dead ACTIVE account whose own token starts
    /// answering again (WITHOUT a re-login) must poll successfully this many times in a
    /// row before it is un-quarantined. A re-login un-quarantines immediately instead
    /// (the #13 canonical-change re-stash clears the flag directly, issue #107).
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

/// The periodic isolated-refresh schedule (issue #105) — the in-daemon counterpart of the
/// one-shot `poke` (#104), wiring the #102 refresh engine into the `run` loop's idle path.
///
/// **Opt-in**: `enabled` defaults `false`, so a daemon refreshes nothing unless an operator
/// turns it on. When on, between poll→usage→swap ticks the daemon keeps PARKED accounts'
/// stored tokens fresh by letting Claude Code refresh them in an isolated `CLAUDE_CONFIG_DIR`
/// (the engine's whole job), never touching the live session's canonical credential.
///
/// The tick honors the engine's Caller contract: it refreshes parked accounts only (the
/// active account and the imminent swap target are excluded; the swap lock the engine holds
/// enforces the mid-swap case), and a refresh `Err` is non-fatal — logged, with the
/// dead-credential recovery path (#13/#42) absorbing a forfeited token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefreshConfig {
    /// Whether the periodic refresh tick runs at all. **Off by default** (opt-in): the
    /// refresh-token TTL is unproven (#101) and each refresh may rotate it, so an operator
    /// turns this on deliberately.
    pub(crate) enabled: bool,
    /// The parked accounts the tick is allowed to refresh, each named by its `list` label OR
    /// `account_uuid` (the same resolution `poke` and `use` key on). **Empty = all** parked
    /// accounts. A non-empty list narrows the candidate set; the near-expiry horizon still
    /// gates within it (a listed account is refreshed only when actually due). An entry that
    /// matches no roster account is ignored (the selection is best-effort — a stale entry
    /// never stops the daemon).
    pub(crate) accounts: Vec<String>,
    /// Seconds between refresh ticks AND the near-expiry selection horizon (issue #105): an
    /// account is *due* when its stored token would expire within one cadence of now — i.e.
    /// it would not survive until the next tick — so the cadence IS the configurable,
    /// TTL-aware threshold (#104 left the all-accounts horizon provisional for #105 to own).
    /// **Provisional default** ([`DEFAULT_REFRESH_CADENCE_SECS`]) pending #101's TTL.
    pub(crate) cadence_secs: u64,
    /// Seconds the daemon must sit idle (since the last poll→usage→swap tick) before a refresh
    /// fires (issue #105) — so the refresh runs in the quiet part of the idle gap, off the
    /// seam. **Provisional default** ([`DEFAULT_REFRESH_IDLE_AFTER_SECS`]).
    pub(crate) idle_after_secs: u64,
    /// Seconds bounding ONE account's whole isolated-refresh cycle (issue #105); a cycle that
    /// exceeds it is cancelled (engine RAII teardown still runs) and reported as a non-fatal
    /// error. Default ([`DEFAULT_REFRESH_TIMEOUT_SECS`]) leaves headroom over the engine's
    /// ~40 s `claude -p` spawn budget.
    pub(crate) timeout_secs: u64,
    /// The `claude` binary the engine spawns, overriding the `$CLAUDE_BIN` / `$PATH`
    /// resolution (issue #105) — `None` (the default) defers to that resolution. Resolved
    /// (absolutized, validated to exist) before any spawn by
    /// [`crate::paths::claude_binary_with_override`].
    pub(crate) claude_bin: Option<PathBuf>,
}

impl Default for RefreshConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            accounts: Vec::new(),
            cadence_secs: DEFAULT_REFRESH_CADENCE_SECS,
            idle_after_secs: DEFAULT_REFRESH_IDLE_AFTER_SECS,
            timeout_secs: DEFAULT_REFRESH_TIMEOUT_SECS,
            claude_bin: None,
        }
    }
}

impl RefreshConfig {
    /// The refresh cadence — which is also the near-expiry selection horizon — as a
    /// [`Duration`].
    pub(crate) fn cadence(&self) -> Duration {
        Duration::from_secs(self.cadence_secs)
    }

    /// The post-tick idle the daemon must accrue before a refresh fires, as a [`Duration`].
    pub(crate) fn idle_after(&self) -> Duration {
        Duration::from_secs(self.idle_after_secs)
    }

    /// The per-account whole-cycle timeout, as a [`Duration`].
    pub(crate) fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

/// The `login` verb's settings (issue #135): how long one interactive `login` capture may run,
/// and an optional `claude` binary override — the user-facing counterpart of the `[refresh]`
/// block for the one-shot `sessiometer login [label]` command.
///
/// Both keys are optional with documented defaults, so a config with no `[login]` table (or none
/// at all — the first `login` runs before any `config.toml` exists) uses [`LoginConfig::default`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoginConfig {
    /// Seconds bounding ONE whole interactive login capture (issue #135 AC): on expiry the
    /// isolated `claude /login` child is killed and teardown runs (the operator gets a cancelled
    /// outcome). Default [`DEFAULT_LOGIN_TIMEOUT_SECS`] (180), operator-tunable within `60..=600`
    /// — low enough to abandon an unattended login, high enough for a deliberate browser handoff.
    pub(crate) timeout_secs: u64,
    /// The `claude` binary the login engine spawns, overriding the `$CLAUDE_BIN` / `$PATH`
    /// resolution — `None` (the default) defers to that resolution. Resolved (absolutized,
    /// validated to exist) before the spawn by the SAME resolver the refresh path uses,
    /// [`crate::paths::claude_binary_with_override`] (issue #135 AC: "reusing the existing
    /// binary-override resolver; no new config mechanism").
    pub(crate) claude_bin: Option<PathBuf>,
}

impl Default for LoginConfig {
    fn default() -> Self {
        Self {
            timeout_secs: DEFAULT_LOGIN_TIMEOUT_SECS,
            claude_bin: None,
        }
    }
}

impl LoginConfig {
    /// The whole-capture timeout, as a [`Duration`] — the bound the `login` verb threads into
    /// [`crate::login::login_account`].
    pub(crate) fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

/// The validated configuration: the captured roster plus tunables.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// Captured accounts (unique `account_uuid`; no fixed upper bound —
    /// #35, and possibly empty — the daemon's "at least one" precondition is
    /// [`Config::require_roster`], not a parse-time rule, so `capture` can load a
    /// tunables-only file to add the first account). Consumed by the swap engine
    /// (#6 / #7) and by `list` / `status` (#17 / #9).
    #[allow(dead_code)]
    pub(crate) roster: Vec<Account>,
    /// Poll/swap tunables.
    pub(crate) tunables: Tunables,
    /// The periodic isolated-refresh schedule (issue #105); opt-in, disabled by default.
    pub(crate) refresh: RefreshConfig,
    /// The one-shot `login` verb's settings (issue #135): capture timeout + optional `claude`
    /// binary override. Consumed by `crate::capture::login`, never by the daemon.
    pub(crate) login: LoginConfig,
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
        // Uniqueness keys on `account_uuid` alone: the stash is derived from it
        // ([`Account::stash`]), so distinct uuids imply distinct stashes and a
        // non-empty uuid implies a non-empty stash — the former empty-/duplicate-
        // stash checks are now redundant (issue #70).
        let mut uuids = HashSet::new();
        let mut roster = Vec::with_capacity(raw.account.len());
        for account in raw.account {
            if account.account_uuid.trim().is_empty() {
                return Err(Error::ConfigInvalid(
                    "account_uuid must not be empty".into(),
                ));
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
            roster.push(Account {
                account_uuid: account.account_uuid,
                label: account.label,
                enabled: account.enabled,
            });
        }

        // The periodic isolated-refresh schedule (issue #105). Bounds-checked like the
        // tunables; `enabled` / `accounts` / `claude_bin` are free-form (a bad `claude_bin`
        // surfaces at spawn-resolution time, an unmatched `accounts` entry at selection).
        // An empty/whitespace `claude_bin` collapses to `None` — same as omitting it — so a
        // stray `claude_bin = ""` defers to `$CLAUDE_BIN`/`$PATH` rather than erroring.
        let r = raw.refresh;
        range("refresh.cadence_secs", r.cadence_secs, 60, 86_400)?;
        range("refresh.idle_after_secs", r.idle_after_secs, 0, 3_600)?;
        range("refresh.timeout_secs", r.timeout_secs, 10, 600)?;
        let refresh = RefreshConfig {
            enabled: r.enabled,
            accounts: r.accounts,
            cadence_secs: r.cadence_secs as u64,
            idle_after_secs: r.idle_after_secs as u64,
            timeout_secs: r.timeout_secs as u64,
            claude_bin: r
                .claude_bin
                .filter(|bin| !bin.trim().is_empty())
                .map(PathBuf::from),
        };

        // The one-shot `login` verb's settings (issue #135). The timeout is bounds-checked like the
        // refresh timeout; `claude_bin` is free-form and an empty/whitespace value collapses to
        // `None` — the SAME override-resolver contract as `[refresh].claude_bin` (a bad path
        // surfaces at spawn-resolution time, never here).
        let l = raw.login;
        range("login.timeout_secs", l.timeout_secs, 60, 600)?;
        let login = LoginConfig {
            timeout_secs: l.timeout_secs as u64,
            claude_bin: l
                .claude_bin
                .filter(|bin| !bin.trim().is_empty())
                .map(PathBuf::from),
        };

        Ok(Config {
            roster,
            tunables,
            refresh,
            login,
        })
    }

    /// Render the config back to TOML with the inline tunable-documenting
    /// comments (issue #3 N2). `serde` serialization cannot emit comments, so
    /// the file is rendered by hand; integers need no escaping and roster
    /// strings go through [`basic_string`].
    ///
    /// `pub(crate)` so the `export` verb (issue #148) can serialize the canonical
    /// config text into a migration artifact ([`crate::migration::Payload`]).
    pub(crate) fn render(&self) -> String {
        let t = &self.tunables;
        let mut out = String::new();
        out.push_str("# sessiometer configuration.\n");
        out.push_str(
            "# The roster is managed by `sessiometer capture`; the [tunables] block is\n\
             # safe to hand-edit. Percentages are of the rolling session window.\n\n",
        );

        out.push_str("[tunables]\n");
        out.push_str(
            "# Seconds between re-polling a given account (5..=3600) — the per-account\n\
             # cadence. The default 300 (5 min) plus the normal `poll` jitter below\n\
             # decorrelates cycles; the daemon staggers the roster within it, polling one\n\
             # account per poll_secs/N sub-interval so requests do not burst all at once.\n\
             # Under sustained 429/5xx it backs off automatically — widening this and\n\
             # honouring any Retry-After — instead of re-polling at the fixed interval.\n",
        );
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
             # whose own token starts working again (without a re-login) is restored to\n\
             # the rotation (1..=20). A re-login restores it immediately.\n",
        );
        out.push_str(&format!("monitor_recovery_m = {}\n", t.monitor_recovery_m));

        // Per-cycle timing jitter (issue #38): drawn each cycle and clamped to the
        // tunable's valid range, to decorrelate polling/swaps across cycles.
        out.push_str("\n[jitter]\n");
        out.push_str(
            "# Randomization drawn each cycle and clamped to the tunable's range.\n\
             # kind = \"none\" | \"uniform\" (with `spread`) | \"normal\" (with `stddev`).\n\
             # poll defaults to normal jitter (stddev ~20% of poll_secs) so accounts\n\
             # decorrelate; trigger, weekly_trigger and cooldown default to none.\n",
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

        // The periodic isolated-refresh schedule (issue #105). Opt-in and OFF by default;
        // the cadence/idle defaults are provisional pending the refresh-token TTL (#101).
        let r = &self.refresh;
        out.push_str("\n[refresh]\n");
        out.push_str(
            "# Periodically let Claude Code refresh PARKED accounts' stored tokens in an\n\
             # isolated config dir (the in-daemon counterpart of `poke`), off the\n\
             # poll/usage/swap seam — the live session's credential is never touched. The\n\
             # active account and the imminent swap target are always excluded. OFF by\n\
             # default (opt-in): each refresh may rotate the refresh token, whose durable\n\
             # lifetime is not yet pinned.\n",
        );
        out.push_str(&format!("enabled = {}\n", r.enabled));
        out.push_str(
            "# Parked accounts to keep fresh, by `list` label or account-uuid. Empty = all\n\
             # parked accounts (the near-expiry horizon still applies to each).\n",
        );
        out.push_str(&format!("accounts = {}\n", render_str_array(&r.accounts)));
        out.push_str(
            "# Seconds between refresh ticks AND the near-expiry horizon (60..=86400): an\n\
             # account is refreshed when its stored token would expire within one cadence\n\
             # (i.e. before the next tick). Provisional default pending the token TTL.\n",
        );
        out.push_str(&format!("cadence_secs = {}\n", r.cadence_secs));
        out.push_str(
            "# Seconds the daemon must sit idle (no poll/swap) before a refresh fires\n\
             # (0..=3600) — keeps it in the quiet part of the idle gap. Provisional.\n",
        );
        out.push_str(&format!("idle_after_secs = {}\n", r.idle_after_secs));
        out.push_str(
            "# Seconds bounding one account's whole refresh cycle (10..=600); a slower\n\
             # cycle is cancelled and reported (non-fatal). Keep above the ~40s spawn.\n",
        );
        out.push_str(&format!("timeout_secs = {}\n", r.timeout_secs));
        out.push_str(
            "# The `claude` binary to spawn, overriding $CLAUDE_BIN/$PATH. Omit (or leave\n\
             # empty) to resolve from $CLAUDE_BIN then $PATH.\n",
        );
        match &r.claude_bin {
            Some(bin) => out.push_str(&format!(
                "claude_bin = {}\n",
                basic_string(&bin.to_string_lossy())
            )),
            None => out.push_str("# claude_bin = \"/absolute/path/to/claude\"\n"),
        }

        // The one-shot `login` verb's settings (issue #135): capture timeout + optional binary
        // override. Independent of `[refresh]` (a login is interactive, not a daemon tick).
        let l = &self.login;
        out.push_str("\n[login]\n");
        out.push_str(
            "# Settings for `sessiometer login [label]`, the interactive re-auth verb: run\n\
             # `claude /login` in an isolated config dir, harvest the fresh credential, and land\n\
             # it in the roster (onboarding a new account or reviving a parked one).\n",
        );
        out.push_str(
            "# Seconds bounding one whole login capture (60..=600); on expiry the login is\n\
             # cancelled (nothing captured). Longer than the refresh timeout — a login waits on a\n\
             # human completing a browser OAuth handoff.\n",
        );
        out.push_str(&format!("timeout_secs = {}\n", l.timeout_secs));
        out.push_str(
            "# The `claude` binary to spawn, overriding $CLAUDE_BIN/$PATH. Omit (or leave empty)\n\
             # to resolve from $CLAUDE_BIN then $PATH.\n",
        );
        match &l.claude_bin {
            Some(bin) => out.push_str(&format!(
                "claude_bin = {}\n",
                basic_string(&bin.to_string_lossy())
            )),
            None => out.push_str("# claude_bin = \"/absolute/path/to/claude\"\n"),
        }

        for account in &self.roster {
            out.push_str("\n[[account]]\n");
            out.push_str(&format!(
                "account_uuid = {}\n",
                basic_string(&account.account_uuid)
            ));
            // No `stash` line: it is derived from `account_uuid` on load
            // ([`Account::stash`]), never persisted (issue #70).
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

/// Render a list of strings as a single-line TOML array of basic strings, e.g.
/// `["work", "spare"]` (issue #105 `[refresh].accounts`). Each element goes through
/// [`basic_string`], so labels/uuids needing escapes round-trip; an empty list renders
/// `[]`.
#[allow(dead_code)]
fn render_str_array(items: &[String]) -> String {
    let mut out = String::from("[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&basic_string(item));
    }
    out.push(']');
    out
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
    #[serde(default)]
    refresh: RawRefresh,
    #[serde(default)]
    login: RawLogin,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAccount {
    account_uuid: String,
    /// Legacy back-compat (issue #70): pre-#70 `config.toml` files persisted a
    /// `stash = "Sessiometer/<account_uuid>"` line. The stash is now derived
    /// ([`Account::stash`]) and never re-written, but the key is still ACCEPTED
    /// here — and ignored — so existing files keep parsing under
    /// `deny_unknown_fields`; the next `save` drops it. Absent (a post-#70 file) →
    /// empty via `#[serde(default)]`.
    #[serde(default)]
    #[allow(dead_code)]
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

/// Permissive deserialization of the optional `[refresh]` table (issue #105): every key
/// optional with a documented default, integers kept wide so an out-of-range value reaches
/// [`Config::validate`] with a clear message rather than a bare `serde` type error.
/// `deny_unknown_fields` rejects a stray key as a parse error.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRefresh {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    accounts: Vec<String>,
    #[serde(default = "default_refresh_cadence_secs")]
    cadence_secs: i64,
    #[serde(default = "default_refresh_idle_after_secs")]
    idle_after_secs: i64,
    #[serde(default = "default_refresh_timeout_secs")]
    timeout_secs: i64,
    #[serde(default)]
    claude_bin: Option<String>,
}

impl Default for RawRefresh {
    fn default() -> Self {
        Self {
            enabled: false,
            accounts: Vec::new(),
            cadence_secs: default_refresh_cadence_secs(),
            idle_after_secs: default_refresh_idle_after_secs(),
            timeout_secs: default_refresh_timeout_secs(),
            claude_bin: None,
        }
    }
}

fn default_refresh_cadence_secs() -> i64 {
    DEFAULT_REFRESH_CADENCE_SECS as i64
}
fn default_refresh_idle_after_secs() -> i64 {
    DEFAULT_REFRESH_IDLE_AFTER_SECS as i64
}
fn default_refresh_timeout_secs() -> i64 {
    DEFAULT_REFRESH_TIMEOUT_SECS as i64
}

/// Permissive deserialization of the optional `[login]` table (issue #135): both keys optional
/// with a documented default, the timeout kept wide (`i64`) so an out-of-range value reaches
/// [`Config::validate`] with a clear message rather than a bare `serde` type error.
/// `deny_unknown_fields` rejects a stray key as a parse error.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLogin {
    #[serde(default = "default_login_timeout_secs")]
    timeout_secs: i64,
    #[serde(default)]
    claude_bin: Option<String>,
}

impl Default for RawLogin {
    fn default() -> Self {
        Self {
            timeout_secs: default_login_timeout_secs(),
            claude_bin: None,
        }
    }
}

fn default_login_timeout_secs() -> i64 {
    DEFAULT_LOGIN_TIMEOUT_SECS as i64
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
label = "work"

[[account]]
account_uuid = "22222222-2222-2222-2222-222222222222"
label = "personal"
"#;

    /// A minimal valid roster body with one account and the given `[tunables]`
    /// fragment spliced in.
    fn with_tunables(fragment: &str) -> String {
        format!(
            "[tunables]\n{fragment}\n\
             [[account]]\n\
             account_uuid = \"u\"\n\
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
        // The stash name is derived from `account_uuid`, not parsed from the file.
        assert_eq!(
            config.roster[1].stash(),
            "Sessiometer/22222222-2222-2222-2222-222222222222"
        );
    }

    #[test]
    fn tunables_default_when_table_absent() {
        let toml = "[[account]]\n\
                    account_uuid = \"u\"\n\
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
                "[[account]]\naccount_uuid = \"u{i}\"\nlabel = \"l{i}\"\n"
            ));
        }
        let config = Config::parse(&toml).unwrap();
        assert_eq!(config.roster.len(), 8);
    }

    #[test]
    fn rejects_duplicate_uuid() {
        let toml = "[[account]]\naccount_uuid = \"same\"\nlabel = \"a\"\n\
                    [[account]]\naccount_uuid = \"same\"\nlabel = \"b\"\n";
        assert!(matches!(Config::parse(toml), Err(Error::ConfigInvalid(_))));
    }

    // (Pre-#70 there was a `rejects_duplicate_stash` test; the stash is now derived
    // from `account_uuid`, so duplicate stashes cannot occur independently of
    // duplicate uuids — the check, and its test, are gone. See
    // `stash_is_derived_from_account_uuid` and `legacy_stash_field_is_ignored`.)

    #[test]
    fn rejects_empty_label() {
        let toml = "[[account]]\naccount_uuid = \"u\"\nlabel = \"\"\n";
        assert!(matches!(Config::parse(toml), Err(Error::ConfigInvalid(_))));
    }

    #[test]
    fn round_trips_render_then_parse() {
        let original = Config::parse(VALID).unwrap();
        let reparsed = Config::parse(&original.render()).unwrap();
        assert_eq!(original.tunables, reparsed.tunables);
        assert_eq!(original.roster, reparsed.roster);
        // The (default) refresh schedule round-trips too (issue #105).
        assert_eq!(original.refresh, reparsed.refresh);
        // …and the (default) [login] settings (issue #135).
        assert_eq!(original.login, reparsed.login);
    }

    // --- [refresh] schedule (issue #105) ------------------------------------

    #[test]
    fn refresh_defaults_when_table_absent() {
        // No [refresh] table → the opt-in feature is OFF with provisional defaults.
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.refresh, RefreshConfig::default());
        assert!(!config.refresh.enabled);
        assert!(config.refresh.accounts.is_empty());
        assert_eq!(config.refresh.cadence_secs, DEFAULT_REFRESH_CADENCE_SECS);
        assert_eq!(
            config.refresh.idle_after_secs,
            DEFAULT_REFRESH_IDLE_AFTER_SECS
        );
        assert_eq!(config.refresh.timeout_secs, DEFAULT_REFRESH_TIMEOUT_SECS);
        assert_eq!(config.refresh.claude_bin, None);
    }

    #[test]
    fn parses_a_custom_refresh_table() {
        let toml = format!(
            "{VALID}\n[refresh]\n\
             enabled = true\n\
             accounts = [\"work\", \"22222222-2222-2222-2222-222222222222\"]\n\
             cadence_secs = 7200\n\
             idle_after_secs = 120\n\
             timeout_secs = 60\n\
             claude_bin = \"/opt/claude/bin/claude\"\n"
        );
        let config = Config::parse(&toml).unwrap();
        assert_eq!(
            config.refresh,
            RefreshConfig {
                enabled: true,
                accounts: vec![
                    "work".to_owned(),
                    "22222222-2222-2222-2222-222222222222".to_owned()
                ],
                cadence_secs: 7200,
                idle_after_secs: 120,
                timeout_secs: 60,
                claude_bin: Some(PathBuf::from("/opt/claude/bin/claude")),
            }
        );
        // The cadence is also the near-expiry horizon, exposed as a Duration.
        assert_eq!(config.refresh.cadence(), Duration::from_secs(7200));
        assert_eq!(config.refresh.idle_after(), Duration::from_secs(120));
        assert_eq!(config.refresh.timeout(), Duration::from_secs(60));
    }

    #[test]
    fn refresh_missing_key_takes_its_default() {
        // A partial [refresh] table fills only the named keys; the rest default.
        let toml = format!("{VALID}\n[refresh]\nenabled = true\n");
        let config = Config::parse(&toml).unwrap();
        assert!(config.refresh.enabled);
        assert_eq!(config.refresh.cadence_secs, DEFAULT_REFRESH_CADENCE_SECS);
        assert_eq!(config.refresh.timeout_secs, DEFAULT_REFRESH_TIMEOUT_SECS);
    }

    #[test]
    fn refresh_unknown_field_is_rejected() {
        // deny_unknown_fields: a stray key is a parse error, not a silent ignore.
        let toml = format!("{VALID}\n[refresh]\nenabled = true\nthreshold_secs = 99\n");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigParse(_))));
    }

    #[test]
    fn refresh_cadence_out_of_range_is_rejected() {
        // Below the 60 s floor and above the 1-day ceiling both fail, naming the field.
        for bad in ["cadence_secs = 30", "cadence_secs = 100000"] {
            let toml = format!("{VALID}\n[refresh]\n{bad}\n");
            let err = Config::parse(&toml).unwrap_err();
            assert!(
                matches!(&err, Error::ConfigInvalid(msg) if msg.contains("refresh.cadence_secs")),
                "expected a refresh.cadence_secs range error, got {err:?}"
            );
        }
    }

    #[test]
    fn refresh_idle_after_and_timeout_ranges_are_enforced() {
        let idle = format!("{VALID}\n[refresh]\nidle_after_secs = 5000\n");
        assert!(
            matches!(Config::parse(&idle), Err(Error::ConfigInvalid(msg)) if msg.contains("refresh.idle_after_secs"))
        );
        // 0 idle is allowed (refresh as soon as the tick settles).
        let zero = format!("{VALID}\n[refresh]\nidle_after_secs = 0\n");
        assert_eq!(Config::parse(&zero).unwrap().refresh.idle_after_secs, 0);
        let timeout = format!("{VALID}\n[refresh]\ntimeout_secs = 5\n");
        assert!(
            matches!(Config::parse(&timeout), Err(Error::ConfigInvalid(msg)) if msg.contains("refresh.timeout_secs"))
        );
    }

    #[test]
    fn empty_claude_bin_collapses_to_none() {
        // A stray `claude_bin = ""` defers to $CLAUDE_BIN/$PATH (None), like omitting it.
        let toml = format!("{VALID}\n[refresh]\nclaude_bin = \"   \"\n");
        assert_eq!(Config::parse(&toml).unwrap().refresh.claude_bin, None);
    }

    #[test]
    fn refresh_round_trips_render_then_parse() {
        // A fully-customised refresh schedule survives render → parse byte-equivalently.
        let toml = format!(
            "{VALID}\n[refresh]\n\
             enabled = true\n\
             accounts = [\"work\"]\n\
             cadence_secs = 5400\n\
             idle_after_secs = 90\n\
             timeout_secs = 120\n\
             claude_bin = \"/usr/local/bin/claude\"\n"
        );
        let original = Config::parse(&toml).unwrap();
        let reparsed = Config::parse(&original.render()).unwrap();
        assert_eq!(original.refresh, reparsed.refresh);
    }

    #[test]
    fn rendered_default_refresh_is_off_with_commented_claude_bin() {
        // The rendered default [refresh] block is disabled and leaves claude_bin commented
        // (so a fresh `capture` writes an inert, self-documenting block) yet round-trips.
        let config = Config::parse(VALID).unwrap();
        let text = config.render();
        assert!(
            text.contains("[refresh]"),
            "render must emit [refresh]: {text}"
        );
        assert!(
            text.contains("enabled = false"),
            "default refresh must render disabled: {text}"
        );
        assert!(
            text.contains("# claude_bin ="),
            "an unset claude_bin must render commented: {text}"
        );
        assert_eq!(
            Config::parse(&text).unwrap().refresh,
            RefreshConfig::default()
        );
    }

    // --- [login] settings (issue #135) --------------------------------------

    #[test]
    fn login_defaults_when_table_absent() {
        // No [login] table → the default 180 s timeout and no binary override.
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.login, LoginConfig::default());
        assert_eq!(config.login.timeout_secs, DEFAULT_LOGIN_TIMEOUT_SECS);
        assert_eq!(config.login.claude_bin, None);
    }

    #[test]
    fn default_login_timeout_matches_the_engine_default() {
        // The config default and the engine's per-call fallback ([`login::DEFAULT_LOGIN_TIMEOUT`])
        // must agree, so an operator who never writes a [login] block gets the SAME 180 s the
        // engine would use standalone. A drift guard: if either constant moves, this fails.
        assert_eq!(
            LoginConfig::default().timeout(),
            crate::login::DEFAULT_LOGIN_TIMEOUT
        );
    }

    #[test]
    fn parses_a_custom_login_table() {
        let toml = format!(
            "{VALID}\n[login]\n\
             timeout_secs = 300\n\
             claude_bin = \"/opt/claude/bin/claude\"\n"
        );
        let config = Config::parse(&toml).unwrap();
        assert_eq!(
            config.login,
            LoginConfig {
                timeout_secs: 300,
                claude_bin: Some(PathBuf::from("/opt/claude/bin/claude")),
            }
        );
        assert_eq!(config.login.timeout(), Duration::from_secs(300));
    }

    #[test]
    fn login_missing_key_takes_its_default() {
        // A partial [login] table fills only the named keys; the rest default.
        let toml = format!("{VALID}\n[login]\ntimeout_secs = 240\n");
        let config = Config::parse(&toml).unwrap();
        assert_eq!(config.login.timeout_secs, 240);
        assert_eq!(config.login.claude_bin, None);
    }

    #[test]
    fn login_timeout_out_of_range_is_rejected() {
        // Below the 60 s floor and above the 600 s ceiling both fail, naming the field.
        for bad in ["timeout_secs = 59", "timeout_secs = 601"] {
            let toml = format!("{VALID}\n[login]\n{bad}\n");
            let err = Config::parse(&toml).unwrap_err();
            assert!(
                matches!(&err, Error::ConfigInvalid(msg) if msg.contains("login.timeout_secs")),
                "expected a login.timeout_secs range error, got {err:?}"
            );
        }
        // The inclusive bounds themselves are accepted.
        for ok in ["timeout_secs = 60", "timeout_secs = 600"] {
            let toml = format!("{VALID}\n[login]\n{ok}\n");
            assert!(
                Config::parse(&toml).is_ok(),
                "an inclusive bound must be accepted: {ok}"
            );
        }
    }

    #[test]
    fn login_empty_claude_bin_collapses_to_none() {
        // A stray `claude_bin = ""` defers to $CLAUDE_BIN/$PATH (None), like omitting it —
        // the same override-resolver contract as [refresh].claude_bin (issue #135 AC).
        let toml = format!("{VALID}\n[login]\nclaude_bin = \"   \"\n");
        assert_eq!(Config::parse(&toml).unwrap().login.claude_bin, None);
    }

    #[test]
    fn login_unknown_field_is_rejected() {
        // deny_unknown_fields: a stray key is a parse error, not a silent ignore.
        let toml = format!("{VALID}\n[login]\ntimeout_secs = 200\nwait_loop = true\n");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigParse(_))));
    }

    #[test]
    fn login_round_trips_render_then_parse() {
        // A fully-customised [login] block survives render → parse byte-equivalently.
        let toml = format!(
            "{VALID}\n[login]\n\
             timeout_secs = 420\n\
             claude_bin = \"/usr/local/bin/claude\"\n"
        );
        let original = Config::parse(&toml).unwrap();
        let reparsed = Config::parse(&original.render()).unwrap();
        assert_eq!(original.login, reparsed.login);
    }

    #[test]
    fn rendered_default_login_documents_timeout_and_commented_claude_bin() {
        // The rendered default [login] block carries the 180 s timeout and leaves claude_bin
        // commented (a self-documenting, inert override), and round-trips to the default.
        let config = Config::parse(VALID).unwrap();
        let text = config.render();
        assert!(text.contains("[login]"), "render must emit [login]: {text}");
        assert!(
            text.contains("timeout_secs = 180"),
            "default login must render the 180 s timeout: {text}"
        );
        assert!(
            text.contains("# claude_bin ="),
            "an unset login claude_bin must render commented: {text}"
        );
        assert_eq!(Config::parse(&text).unwrap().login, LoginConfig::default());
    }

    #[test]
    fn stash_is_derived_from_account_uuid() {
        // The stash name is a pure function of `account_uuid` (issue #70): there is
        // no stored field to read, so a roster entry with only the required keys
        // still resolves its keychain service name.
        let config =
            Config::parse("[[account]]\naccount_uuid = \"abc-123\"\nlabel = \"work\"\n").unwrap();
        assert_eq!(config.roster[0].stash(), "Sessiometer/abc-123");
    }

    #[test]
    fn legacy_stash_field_is_ignored() {
        // Back-compat (issue #70): a pre-#70 file carrying a `stash = …` line still
        // PARSES (the key is accepted, not rejected by `deny_unknown_fields`), and
        // the stored value is IGNORED — the stash is derived from `account_uuid`. A
        // deliberately mismatched stored value proves the field is not read.
        let toml = "[[account]]\n\
                    account_uuid = \"u\"\n\
                    stash = \"Sessiometer/STALE-IGNORED\"\n\
                    label = \"work\"\n";
        let config = Config::parse(toml).expect("a legacy stash line must still parse");
        assert_eq!(config.roster[0].stash(), "Sessiometer/u");
    }

    #[test]
    fn rendered_config_omits_the_derived_stash() {
        // `render` no longer emits a `stash = …` line (issue #70), so the next save
        // of a legacy file drops it. The derived stash survives the render→parse
        // round-trip because it rides on `account_uuid`.
        let config = Config::parse(VALID).unwrap();
        let text = config.render();
        assert!(
            !text.contains("stash ="),
            "render must not emit a stash line: {text}"
        );
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(reparsed.roster[0].stash(), config.roster[0].stash());
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
        let toml = "[[account]]\naccount_uuid = \"u\"\nlabel = \"l\"\nenabled = false\n";
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
        assert!(text.contains("stddev = 60.0"));
    }

    #[test]
    fn round_trips_a_label_that_needs_escaping() {
        let toml = "[[account]]\n\
                    account_uuid = \"u\"\n\
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
        // Issue #76 AC3: the poll_secs comment documents the default cadence + jitter
        // AND the rate-limit / transient back-off (incl. Retry-After) — so an operator
        // hand-editing poll_secs learns the spacing widens automatically under 429/5xx.
        assert!(
            text.contains("The default 300 (5 min)"),
            "poll_secs comment must document the default cadence: {text:?}"
        );
        assert!(
            text.contains("backs off automatically"),
            "poll_secs comment must document the back-off: {text:?}"
        );
        assert!(
            text.contains("Retry-After"),
            "poll_secs comment must document honouring Retry-After: {text:?}"
        );
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
