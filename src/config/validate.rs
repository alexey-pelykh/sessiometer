// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Stage two of the load: bounds-check the permissive [`RawConfig`] into the typed
//! [`Config`] (issue #638's per-concern decomposition of the one 1,253-line `impl Config`).
//!
//! [`Config::validate`] is the SINGLE gate every entry point crosses — the daemon's load,
//! the `import` verb's artifact text, and the `config-set` overlay alike — so a range or
//! cross-field rule cannot be enforced on one path and skipped on another. Its rejection
//! ORDER is observable (a multi-invalid file reports its first offending field), so the
//! checks below are sequenced deliberately; the field-naming helpers it emits through
//! ([`range`], [`parse_jitter`], [`non_negative`]) live here with it.

use super::*;

impl Config {
    /// Stage two: bounds-check every tunable and the roster, producing the typed
    /// `Config`. Each rejection names the offending field; the cross-field rule
    /// (`target_max_session_usage <= session_ceiling`) gets its own distinct error.
    pub(super) fn validate(raw: RawConfig) -> Result<Self> {
        let t = raw.tunables;

        range("session_ceiling", t.session_ceiling, 50, 99)?;
        // The weekly trigger is independent of the session trigger (issue #41):
        // its own 50..=99 bound, with NO cross-field rule — weekly may sit below
        // session (an unusual but valid operator choice), so both are configurable
        // independently (AC #3).
        range("weekly_ceiling", t.weekly_ceiling, 50, 99)?;
        // Issue #452 (ADR-0017) bounded-blindness preemptive-swap gate. `session_blind_swap_secs`
        // is `T` in seconds: floored at 60 (at least one poll cycle blind) and capped at 86400 — a
        // 24 h ceiling far beyond any real blind window, so setting it there disables the path (the
        // config kill-switch). `session_blind_risk_band` is a session percent, 50..=99 like the
        // triggers but conventionally set BELOW `session_ceiling` (the gate fires preemptively on a
        // stale anchor). No NEW cross-field: ADR-0017's `target_max_session_usage <= session_ceiling`
        // is the existing reserve invariant enforced below — the blind path's target still needs
        // runway below its own reactive trigger, which that invariant already guarantees.
        range(
            "session_blind_swap_secs",
            t.session_blind_swap_secs,
            60,
            86_400,
        )?;
        range("session_blind_risk_band", t.session_blind_risk_band, 50, 99)?;
        // Issue #539 (ADR-0017) velocity-projection preemptive trigger. `session_velocity_horizon_secs`
        // is the projection horizon `H` in seconds: `0..=600`, where `0` disables the path (the
        // projection reduces to `last`, which — the reactive path having already held — never crosses,
        // the config kill-switch), and 600 is a sanity ceiling (the #538 spike validated H ≈ 120 and
        // showed over-fire creeping in above ~150, so a large H is a foot-gun the false-projection SLI
        // surfaces; the ceiling just bounds the absurd). `session_velocity_min_project_above` is a
        // session percent (`50..=99`) conventionally set BELOW `session_ceiling` — the projective peer
        // fires in the band beneath the reactive trigger — exactly like `session_blind_risk_band`, so no
        // NEW cross-field. `session_velocity_ema_alpha_pct` is the EMA weight α (`1..=100`); the `0`
        // floor is excluded because α=0 would freeze the EMA (never integrate a new sample), a
        // degenerate value, while 100 is the valid "no smoothing" (raw last-interval) end.
        range(
            "session_velocity_horizon_secs",
            t.session_velocity_horizon_secs,
            0,
            600,
        )?;
        range(
            "session_velocity_min_project_above",
            t.session_velocity_min_project_above,
            50,
            99,
        )?;
        range(
            "session_velocity_ema_alpha_pct",
            t.session_velocity_ema_alpha_pct,
            1,
            100,
        )?;
        // target_max_session_usage is default-on (#398): absent → DEFAULT_TARGET_MAX_SESSION_USAGE, clamped
        // down to session_ceiling so the default honors the SAME
        // `target_max_session_usage <= session_ceiling` invariant the present-value arm enforces
        // (#417 — without the clamp a `session_ceiling < 80` config loads with an
        // unchecked reserve of 80 and then bricks after a render→parse round-trip, since
        // #398 renders the default as a live line; an equal reserve is inert per
        // ADR-0013). When present, its lower bound is 1 (an explicit 0 admits no
        // target, silently disabling proactive swapping) and its upper bound is
        // session_ceiling (a higher reserve could never admit a target), the latter a
        // distinct cross-field error.
        let target_max_session_usage = match t.target_max_session_usage {
            None => DEFAULT_TARGET_MAX_SESSION_USAGE.min(t.session_ceiling as u8),
            Some(value) => {
                if value == 0 {
                    // The swap predicate is `usage.session < target_max_session_usage`, so 0
                    // admits NO account and silently disables proactive swapping (the
                    // daemon just holds). 0 is the natural wrong guess for "no
                    // restriction" — its exact opposite (#414) — so reject it with the
                    // remedy spelled out, rather than let a live, hand-editable line
                    // brick swapping in silence.
                    return Err(Error::ConfigInvalid(format!(
                        "target_max_session_usage = 0 admits no swap target and silently disables \
                         proactive swapping; it must be in 1..={}. Raise it toward \
                         session_ceiling to admit more targets.",
                        t.session_ceiling
                    )));
                }
                if value < 0 {
                    return Err(Error::ConfigInvalid(format!(
                        "target_max_session_usage must be in 1..={}, got {value}",
                        t.session_ceiling
                    )));
                }
                if value > t.session_ceiling {
                    return Err(Error::ConfigTargetMaxSessionAboveTrigger {
                        target_max_session_usage: value,
                        trigger: t.session_ceiling,
                    });
                }
                value as u8
            }
        };
        range("poll_secs", t.poll_secs, 5, 3600)?;
        // The widened exhausted-peer cadence (issue #537) is bounded BELOW by `poll_secs` (a
        // cross-field rule, checked after `poll_secs` above so the bound is the validated
        // value): a slow-polled peer must never re-poll FASTER than the normal cadence — an
        // `exhausted_poll_secs < poll_secs` would defeat the whole point (poll MORE often, not
        // less). The 86400 s (24 h) ceiling is a sanity bound far beyond any real quota window.
        // The lower bound is dynamic, so `range` cannot express it — spell the cross-field
        // remedy out, mirroring `target_max_session_usage`'s message.
        if !(t.poll_secs..=86_400).contains(&t.exhausted_poll_secs) {
            return Err(Error::ConfigInvalid(format!(
                "exhausted_poll_secs must be in {}..=86400 (>= poll_secs so a slow-polled \
                 exhausted peer never re-polls faster than the normal cadence), got {}",
                t.poll_secs, t.exhausted_poll_secs
            )));
        }
        // The near-limit active-poll sub-interval cap (issue #540). `0` disables the path (the
        // kill-switch, like `session_velocity_horizon_secs` below); a non-zero value is a poll
        // cadence in the SAME `5..=3600` s band as `poll_secs` (a sub-5 s cadence sits below the
        // daemon's own poll floor). Deliberately NOT cross-fielded to `poll_secs`: the daemon
        // applies it as `min(poll_secs / N, near_limit_poll_secs)`, so a value ABOVE the base
        // sub-interval is simply inert (the `min` never binds) rather than an error — a lowered
        // `poll_secs` whose base already sits below this cap just leaves #540 inert, which is
        // correct (the steady cadence is already tight). `range` cannot express the `0`-or-band
        // shape, so spell it out, mirroring the `exhausted_poll_secs` message above.
        if t.near_limit_poll_secs != 0 && !(5..=3600).contains(&t.near_limit_poll_secs) {
            return Err(Error::ConfigInvalid(format!(
                "near_limit_poll_secs must be 0 (disabled) or in 5..=3600, got {}",
                t.near_limit_poll_secs
            )));
        }
        // The peak-velocity runway coupling (issue #608, discharging ADR-0023 § Alternatives 3).
        // Checked HERE — after `session_ceiling`, `session_velocity_horizon_secs` and
        // `near_limit_poll_secs` are each range-validated above — so the bound is computed from
        // validated values, the same "cross-field rule after its own fields" placement as
        // `exhausted_poll_secs`.
        //
        // ONLY the UNSATISFIABLE stack is rejected: a bound at/below 0 means no
        // `target_max_session_usage` in its legal `1..=session_ceiling` range keeps a swapped-to
        // account runway at peak velocity — the composed fire point has collapsed to 0, so every
        // account would swap at any usage (ADR-0023 § Consequences' absurd-config corner). A merely
        // EXCEEDED bound (positive, but under the configured reserve) is deliberately NOT an error:
        // the shipped default sits there by design (target_max 80 vs a ~52 bound at the default
        // ceiling), so rejecting it would brick every stock install — the #417 bricking failure mode.
        // That case surfaces as the non-fatal `config validate` advisory instead.
        let peak_runway_bound = crate::swap::peak_runway_reserve_bound(
            f64::from(t.session_ceiling as u8) / 100.0,
            t.near_limit_poll_secs as u64,
            t.session_velocity_horizon_secs as u64,
        );
        if peak_runway_bound <= 0.0 {
            // The SAME lookahead `peak_runway_reserve_bound` computed the bound over, so the error's
            // reported window matches the value that made it unsatisfiable (one source of truth).
            let window_secs = crate::swap::composed_swap_lookahead_secs(
                t.near_limit_poll_secs as u64,
                t.session_velocity_horizon_secs as u64,
            );
            return Err(Error::ConfigPeakRunwayUnsatisfiable {
                trigger: t.session_ceiling,
                near_limit_poll_secs: t.near_limit_poll_secs as u64,
                horizon_secs: t.session_velocity_horizon_secs as u64,
                // Both are bounded well inside i64/u64 by the range checks above; the renders are
                // for the operator message only, never fed back into the math.
                window_secs: window_secs as u64,
                bound_pct: (peak_runway_bound * 100.0).floor() as i64,
                v_peak_pct_per_min: crate::swap::V_PEAK_SESSION_PCT_PER_MIN,
            });
        }
        // cooldown_secs has a NON-ZERO floor (issue #272): it is configurable ABOVE
        // COOLDOWN_SECS_FLOOR but not below it, so swap pacing can never be tuned down
        // to zero. The daemon's per-cycle draw clamps to the same floor, so a jitter
        // spread cannot bypass it either.
        range(
            "cooldown_secs",
            t.cooldown_secs,
            COOLDOWN_SECS_FLOOR as i64,
            3600,
        )?;
        range("monitor_401_n", t.monitor_401_n, 1, 20)?;
        range("monitor_recovery_m", t.monitor_recovery_m, 1, 20)?;
        // The proactive fleet-runway warning threshold (issue #650). `0` disables the path (the
        // kill-switch AND the opt-in default, like `session_velocity_horizon_secs`); a non-zero
        // value is a runway threshold in `60..=2_592_000` s (1 min..30 d — a warn line above 30
        // days is always-on noise, not a warning; below a minute is indistinguishable from the
        // all-exhausted signal itself). `range` cannot express the `0`-or-band shape, so spell it
        // out, mirroring the `near_limit_poll_secs` message above.
        if t.fleet_runway_warn_secs != 0 && !(60..=2_592_000).contains(&t.fleet_runway_warn_secs) {
            return Err(Error::ConfigInvalid(format!(
                "fleet_runway_warn_secs must be 0 (disabled) or in 60..=2592000, got {}",
                t.fleet_runway_warn_secs
            )));
        }

        // Jitter specs (issue #38): each optional and validated to a clear load
        // error (parse-or-error). Poll jitters normally by default; session_ceiling,
        // weekly_ceiling and cooldown are fixed unless the operator configures a
        // strategy.
        let poll_jitter = parse_jitter("poll", raw.jitter.poll, default_poll_jitter())?;
        let session_ceiling_jitter =
            parse_jitter("session_ceiling", raw.jitter.session_ceiling, Jitter::None)?;
        let weekly_ceiling_jitter =
            parse_jitter("weekly_ceiling", raw.jitter.weekly_ceiling, Jitter::None)?;
        let cooldown_jitter = parse_jitter("cooldown", raw.jitter.cooldown, Jitter::None)?;

        // Ranges are checked above, so these narrowing casts cannot truncate. The
        // strategy bases are the same validated scalars (issue #38): the daemon
        // draws + clamps from the strategy each cycle.
        let tunables = Tunables {
            poll_secs: t.poll_secs as u64,
            exhausted_poll_secs: t.exhausted_poll_secs as u64,
            near_limit_poll_secs: t.near_limit_poll_secs as u64,
            cooldown_secs: t.cooldown_secs as u64,
            target_max_session_usage,
            session_ceiling: t.session_ceiling as u8,
            weekly_ceiling: t.weekly_ceiling as u8,
            session_blind_swap_secs: t.session_blind_swap_secs as u64,
            session_blind_risk_band: t.session_blind_risk_band as u8,
            session_velocity_horizon_secs: t.session_velocity_horizon_secs as u64,
            session_velocity_min_project_above: t.session_velocity_min_project_above as u8,
            session_velocity_ema_alpha_pct: t.session_velocity_ema_alpha_pct as u8,
            monitor_401_n: t.monitor_401_n as u8,
            monitor_recovery_m: t.monitor_recovery_m as u8,
            fleet_runway_warn_secs: t.fleet_runway_warn_secs as u64,
            poll_strategy: Strategy {
                base: t.poll_secs as f64,
                jitter: poll_jitter,
            },
            session_ceiling_strategy: Strategy {
                base: t.session_ceiling as f64,
                jitter: session_ceiling_jitter,
            },
            weekly_ceiling_strategy: Strategy {
                base: t.weekly_ceiling as f64,
                jitter: weekly_ceiling_jitter,
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
        range("refresh.systemic_failure_n", r.systemic_failure_n, 1, 100)?;
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
            systemic_failure_n: r.systemic_failure_n as u32,
            proactive_keep_warm: r.proactive_keep_warm,
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

        // The usage-stats subsystem's settings (issue #161). Each retention horizon is
        // bounds-checked like the tunables; the daily horizon's lower bound is 0 (its
        // lifetime sentinel). `default_period` is validated against the fixed `stats`
        // vocabulary so a typo fails at load, not at `stats`-run. No cross-field rules.
        let s = raw.stats;
        range(
            "stats.raw_retention_secs",
            s.raw_retention_secs,
            3_600,
            31_536_000,
        )?;
        range(
            "stats.hourly_retention_secs",
            s.hourly_retention_secs,
            86_400,
            315_360_000,
        )?;
        range(
            "stats.daily_retention_secs",
            s.daily_retention_secs,
            0,
            315_360_000,
        )?;
        if !STATS_PERIODS.contains(&s.default_period.as_str()) {
            return Err(Error::ConfigInvalid(format!(
                "stats.default_period must be one of {STATS_PERIODS:?}, got {:?}",
                s.default_period
            )));
        }
        let stats = StatsConfig {
            raw_retention_secs: s.raw_retention_secs as u64,
            hourly_retention_secs: s.hourly_retention_secs as u64,
            daily_retention_secs: s.daily_retention_secs as u64,
            default_period: s.default_period,
        };

        // The migration subsystem's settings (issue #150). The KDF cost knobs are bounds-checked
        // to sit WITHIN migration.rs's decrypt-time cost guards (memory `> 1<<20`, iterations
        // `> 16`), so an artifact written at any in-range cost still decrypts. The conflict policy
        // is validated against its fixed `skip|overwrite` vocabulary so a typo fails at load, not
        // at import-run. No cross-field rules — the lane count is fixed at production (not a key).
        let m = raw.migration;
        range("migration.kdf_memory_kib", m.kdf_memory_kib, 8, 1_048_576)?;
        range("migration.kdf_iterations", m.kdf_iterations, 1, 16)?;
        let conflict_policy = match m.conflict_policy.as_str() {
            "skip" => ConflictPolicy::Skip,
            "overwrite" => ConflictPolicy::Overwrite,
            _ => {
                return Err(Error::ConfigInvalid(format!(
                    "migration.conflict_policy must be one of {MIGRATION_CONFLICT_POLICIES:?}, got {:?}",
                    m.conflict_policy
                )));
            }
        };
        let migration = MigrationConfig {
            kdf_memory_kib: m.kdf_memory_kib as u32,
            kdf_iterations: m.kdf_iterations as u32,
            conflict_policy,
        };

        Ok(Config {
            roster,
            tunables,
            refresh,
            login,
            stats,
            migration,
        })
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
///
/// `pub(super)` unlike its private siblings [`range`] / [`non_negative`], and NOT because
/// anything outside this module calls it — nothing does. The parent's [`RawJitterSpec`] doc
/// names it, so narrowing this back to private turns that intra-doc link into an unresolved
/// one, which `RUSTDOCFLAGS=-D warnings` fails the build on.
pub(super) fn parse_jitter(
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
