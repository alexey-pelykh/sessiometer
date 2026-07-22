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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::*;

    #[test]
    fn tunables_default_when_table_absent() {
        let toml = "[[account]]\n\
                    account_uuid = \"u\"\n\
                    label = \"only\"\n";
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.tunables, Tunables::default());
        // Issue #597: the default session_ceiling is the CEILING, 95 — a landing target set
        // below the P100 < 99 SLO so backward derivation keeps the SLO reachable with headroom
        // over re-observation-gap staleness (ADR-0023; the pre-#597 fire-AT default was also 95
        // but meant "swap when observed reaches 95", not "land below 95").
        assert_eq!(config.tunables.session_ceiling, 95);
        // #398: the target_max_session_usage reserve is default-on at 80.
        assert_eq!(
            config.tunables.target_max_session_usage,
            DEFAULT_TARGET_MAX_SESSION_USAGE
        );
    }

    #[test]
    fn missing_tunable_key_takes_its_default() {
        let toml = with_tunables("poll_secs = 120");
        let config = Config::parse(&toml).unwrap();
        assert_eq!(config.tunables.poll_secs, 120);
        assert_eq!(config.tunables.cooldown_secs, DEFAULT_COOLDOWN_SECS);
    }

    #[test]
    fn rejects_out_of_range_session_ceiling() {
        for trigger in ["49", "100", "120"] {
            let toml = with_tunables(&format!("session_ceiling = {trigger}"));
            assert!(
                matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))),
                "session_ceiling = {trigger} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_out_of_range_weekly_ceiling() {
        // #41: the weekly trigger carries the same 50..=99 bound as the session one.
        for trigger in ["49", "100", "120"] {
            let toml = with_tunables(&format!("weekly_ceiling = {trigger}"));
            assert!(
                matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))),
                "weekly_ceiling = {trigger} should be rejected"
            );
        }
    }

    #[test]
    fn session_and_weekly_ceilings_are_independently_configurable() {
        // AC #3: the two triggers are set independently — there is NO cross-field
        // rule, so weekly may even sit BELOW session (unlike target_max_session_usage, which
        // is capped at session_ceiling).
        let t = Config::parse(&with_tunables("session_ceiling = 90\nweekly_ceiling = 99"))
            .unwrap()
            .tunables;
        assert_eq!(t.session_ceiling, 90);
        assert_eq!(t.weekly_ceiling, 99);
        assert_eq!(t.session_ceiling_strategy.base, 90.0);
        assert_eq!(t.weekly_ceiling_strategy.base, 99.0);

        // weekly BELOW session is accepted (no target_max_session_usage-style cross-field constraint).
        let inverted = Config::parse(&with_tunables("session_ceiling = 95\nweekly_ceiling = 60"))
            .unwrap()
            .tunables;
        assert_eq!(inverted.session_ceiling, 95);
        assert_eq!(inverted.weekly_ceiling, 60);
    }

    #[test]
    fn weekly_ceiling_takes_its_default_when_absent() {
        // An absent weekly_ceiling takes its compiled-in default, independent of session_ceiling.
        // (Their magnitudes are NOT comparable: since #597 session_ceiling is a CEILING both swap
        // arms derive backward from — effective fire ~0.89 at the default 95 — and since #607
        // weekly_ceiling is a CEILING too, but fired backward by only 1 pp (vs session's 6 pp) over
        // the weekly window, so the raw "weekly vs session" magnitude comparison is apples-to-oranges
        // regardless of ordering (98 > 95 for the current defaults, but they estimate different
        // quantities). The two dimensions stay independent; this test only pins the absent-field
        // default.)
        let t = Config::parse(&with_tunables("session_ceiling = 95"))
            .unwrap()
            .tunables;
        assert_eq!(t.weekly_ceiling, DEFAULT_WEEKLY_CEILING);
        assert_eq!(
            t.weekly_ceiling_strategy.base,
            f64::from(DEFAULT_WEEKLY_CEILING)
        );
    }

    #[test]
    fn rejects_target_max_above_session_ceiling_with_a_distinct_error() {
        let toml = with_tunables("target_max_session_usage = 95\nsession_ceiling = 90");
        assert!(matches!(
            Config::parse(&toml),
            Err(Error::ConfigTargetMaxSessionAboveTrigger {
                target_max_session_usage: 95,
                trigger: 90
            })
        ));
    }

    #[test]
    fn rejects_negative_target_max() {
        let toml = with_tunables("target_max_session_usage = -1");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))));
    }

    #[test]
    fn rejects_zero_target_max_naming_the_consequence() {
        // #414: target_max_session_usage = 0 makes the swap predicate `usage.session < 0` admit no
        // account, so proactive swapping is silently disabled and the daemon just holds.
        // Since #398 made target_max_session_usage a live, hand-editable line, 0 is the natural
        // (wrong) guess for "no restriction" — its exact opposite. validate must reject it
        // with a message that names the consequence AND points at the remedy (raise it
        // toward session_ceiling to admit more targets).
        let toml = with_tunables("target_max_session_usage = 0\nsession_ceiling = 90");
        match Config::parse(&toml) {
            Err(Error::ConfigInvalid(msg)) => assert!(
                msg.contains("disables proactive swapping") && msg.contains("session_ceiling"),
                "rejection must name the consequence and the remedy, got: {msg}"
            ),
            Ok(_) => panic!("target_max_session_usage = 0 must be rejected, not accepted"),
            Err(e) => panic!("target_max_session_usage = 0 must be ConfigInvalid, got: {e}"),
        }

        // The reject is precisely 0, not "any low value": 1 is the valid lower edge and
        // still parses (inert-but-valid — admits only accounts at 0% session).
        let one = Config::parse(&with_tunables(
            "target_max_session_usage = 1\nsession_ceiling = 90",
        ))
        .expect("target_max_session_usage = 1 is the valid lower bound and must parse");
        assert_eq!(one.tunables.target_max_session_usage, 1);

        // …and the absent-key default path (#417 clamp) is untouched by the reject: an
        // absent target_max_session_usage still yields the default-on reserve, never 0.
        let absent = Config::parse(&with_tunables("session_ceiling = 90")).unwrap();
        assert_eq!(
            absent.tunables.target_max_session_usage,
            DEFAULT_TARGET_MAX_SESSION_USAGE
        );
    }

    #[test]
    fn target_max_session_usage_defaults_to_80_when_absent() {
        // #398: an absent target_max_session_usage takes the default-on reserve (80), even when
        // other tunables are set…
        let absent = Config::parse(&with_tunables("session_ceiling = 95")).unwrap();
        assert_eq!(
            absent.tunables.target_max_session_usage,
            DEFAULT_TARGET_MAX_SESSION_USAGE
        );
        // …and a present value overrides it at that percent.
        let set = Config::parse(&with_tunables(
            "target_max_session_usage = 90\nsession_ceiling = 95",
        ))
        .unwrap();
        assert_eq!(set.tunables.target_max_session_usage, 90);
    }

    #[test]
    fn absent_target_max_default_clamps_to_session_ceiling_below_80_and_survives_round_trip() {
        // #417 (regression from #398): with session_ceiling < 80 and NO target_max_session_usage
        // key, the absent-key default (80) MUST clamp down to session_ceiling — honoring
        // the same target_max_session_usage <= session_ceiling invariant the present-value arm
        // already enforces (ADR-0013 Decision 1). Without the clamp the first load
        // silently yields a reserve of 80 (> trigger — the cross-field check is skipped on
        // the absent-key arm), render() then emits it as a LIVE line (#398), and the SECOND
        // parse rejects the config with ConfigTargetMaxSessionAboveTrigger — bricking a valid config
        // after any save/export round-trip (enable/disable/remove account, capture
        // write-back, export→import). The existing round-trip test above only covers the
        // default trigger = 95 (where 80 < 95), so it never reached this corner.
        let toml = with_tunables("session_ceiling = 70"); // no target_max_session_usage key
        let config = Config::parse(&toml).unwrap();
        // The default is clamped to the trigger — the maximally-permissive inert value
        // (ADR-0013: an equal reserve admits exactly what the always-on gate admits),
        // never left at 80.
        assert_eq!(config.tunables.target_max_session_usage, 70);
        assert!(config.tunables.target_max_session_usage <= config.tunables.session_ceiling);

        // …and it survives a render → parse round-trip: the exact path that bricked.
        let text = config.render();
        assert!(text.contains("target_max_session_usage = 70"), "got {text}");
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(reparsed.tunables.target_max_session_usage, 70);
        assert_eq!(reparsed.tunables.session_ceiling, 70);
    }

    #[test]
    fn deprecated_aliases_parse_and_render_as_target_max_session_usage() {
        // Schema-migration guard (ADR-0006). The target-reserve key has been renamed twice:
        // `session_floor` → `target_max_usage` (#415) → `target_max_session_usage` (#443, the
        // unqualified `usage` hid the session axis). Each rename kept the prior key as a serde
        // deprecation alias, and #443 is the LAST rename (the alias stack stops at two). Every
        // existing config.toml carries a persisted, operator-visible line, so BOTH deprecated
        // keys MUST still parse onto the new field, and render MUST rewrite them to the new
        // canonical key.

        // All three spellings load onto the new field with the same value (AC: assert all three).
        for key in [
            "session_floor",
            "target_max_usage",
            "target_max_session_usage",
        ] {
            let cfg = Config::parse(&with_tunables(&format!("{key} = 70\nsession_ceiling = 90")))
                .unwrap_or_else(|e| panic!("a config written with `{key}` must still parse: {e}"));
            assert_eq!(
                cfg.tunables.target_max_session_usage, 70,
                "`{key}` must map onto target_max_session_usage",
            );
        }

        // A deprecated-key file is REWRITTEN to the new key on render (the one-way key rewrite,
        // mirroring the #70 stash drop): the emitted file carries `target_max_session_usage`,
        // never either old key.
        let old = Config::parse(&with_tunables("session_floor = 70\nsession_ceiling = 90"))
            .expect("a config written with the deprecated `session_floor` key must still parse");
        let rendered = old.render();
        assert!(
            rendered.contains("target_max_session_usage = 70"),
            "render must emit the new key: {rendered}"
        );
        assert!(
            !rendered.contains("session_floor") && !rendered.contains("target_max_usage"),
            "render must NOT emit either deprecated key: {rendered}"
        );

        // Export → import round-trip survives the deprecated-key input: parsing the render
        // of an old-key file yields the same value under the new field.
        let reimported = Config::parse(&rendered).expect("the rendered new-key file re-imports");
        assert_eq!(reimported.tunables.target_max_session_usage, 70);
    }

    #[test]
    fn rejects_each_out_of_range_tunable() {
        for (key, value) in [
            ("poll_secs", "4"),
            ("poll_secs", "3601"),
            ("exhausted_poll_secs", "86401"), // above the 24 h ceiling (#537)
            ("near_limit_poll_secs", "4"), // below the 5 s floor, yet not the 0 kill-switch (#540)
            ("near_limit_poll_secs", "3601"), // above the 3600 s ceiling (#540)
            ("session_velocity_horizon_secs", "601"), // above the 600 s sanity ceiling (#539)
            ("session_velocity_min_project_above", "49"), // below the 50 % floor (#539)
            ("session_velocity_min_project_above", "100"), // above the 99 % ceiling (#539)
            ("session_velocity_ema_alpha_pct", "0"), // alpha=0 freezes the EMA — degenerate (#539)
            ("session_velocity_ema_alpha_pct", "101"), // above 100 % (#539)
            ("cooldown_secs", "0"),        // below the non-zero floor (#272)
            ("cooldown_secs", "4"),        // still below the floor (#272)
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
    fn exhausted_poll_secs_defaults_to_one_hour() {
        // Issue #537: an absent `exhausted_poll_secs` defaults to the compiled-in 3600 s (1 h)
        // ceiling — the slow-poll cadence is on by default without an operator opting in.
        let config = Config::parse(&with_tunables("poll_secs = 300")).unwrap();
        assert_eq!(config.tunables.exhausted_poll_secs, 3600);
    }

    #[test]
    fn exhausted_poll_secs_must_be_at_least_poll_secs_and_below_the_ceiling() {
        // Issue #537: the widened cadence is bounded BELOW by `poll_secs` (a slow-polled peer
        // must never re-poll FASTER than the normal cadence — that would defeat the point) and
        // ABOVE by 86400 s. The lower bound is a CROSS-FIELD rule, so the rejection names both
        // the field and `poll_secs`, mirroring `target_max_session_usage`'s message.
        let below = with_tunables("poll_secs = 600\nexhausted_poll_secs = 599");
        match Config::parse(&below) {
            Err(Error::ConfigInvalid(msg)) => assert!(
                msg.contains("exhausted_poll_secs") && msg.contains("600"),
                "rejection must name the field and poll_secs, got: {msg}"
            ),
            other => panic!("exhausted_poll_secs < poll_secs must be rejected, got: {other:?}"),
        }

        // The lower edge (== poll_secs) LOADS — an equal cadence is the inert boundary, not a
        // slow-down, but it is a valid operator choice and threads through to the tunable.
        let at_floor = Config::parse(&with_tunables("poll_secs = 600\nexhausted_poll_secs = 600"))
            .expect("exhausted_poll_secs == poll_secs is the valid lower edge");
        assert_eq!(at_floor.tunables.exhausted_poll_secs, 600);

        // The upper edge (the 24 h ceiling) loads; one over is rejected.
        let at_ceiling = Config::parse(&with_tunables("exhausted_poll_secs = 86400"))
            .expect("the 86400 s ceiling is valid");
        assert_eq!(at_ceiling.tunables.exhausted_poll_secs, 86_400);
        assert!(matches!(
            Config::parse(&with_tunables("exhausted_poll_secs = 86401")),
            Err(Error::ConfigInvalid(_))
        ));

        // A mid-range value >= poll_secs loads and threads through verbatim.
        let mid = Config::parse(&with_tunables(
            "poll_secs = 300\nexhausted_poll_secs = 7200",
        ))
        .expect("a value in poll_secs..=86400 loads");
        assert_eq!(mid.tunables.exhausted_poll_secs, 7200);
    }

    #[test]
    fn near_limit_poll_secs_accepts_zero_disabled_or_the_5_to_3600_band() {
        // Issue #540: the near-limit fast-poll cap is `0` (disabled — the kill-switch) OR in the
        // 5..=3600 s band. The `0`-OR-band shape is the load-bearing subtlety: a naive
        // `(5..=3600).contains()` WITHOUT the `!= 0` guard would reject the documented kill-switch.
        // There is deliberately NO cross-field bound against `poll_secs` (unlike #537's
        // `exhausted_poll_secs`): an above-base value is INERT via the `min(poll_secs / N, cap)` in
        // `next_subinterval`, not a load-time error — so no default-vs-configured footgun.

        // Absent → the compiled-in 60 s default.
        let default = Config::parse(&with_tunables("poll_secs = 300")).unwrap();
        assert_eq!(default.tunables.near_limit_poll_secs, 60);

        // 0 is the disabled kill-switch and MUST load — it is NOT a sub-floor rejection.
        let disabled = Config::parse(&with_tunables("near_limit_poll_secs = 0"))
            .expect("0 is the valid disabled kill-switch, not a sub-floor rejection");
        assert_eq!(disabled.tunables.near_limit_poll_secs, 0);

        // The lower band edge, and the largest value that keeps the reactive poll gap at its 313 s
        // floor (`2 × 156 = 312 <= 313`), both load and thread through verbatim. Above ~156 the
        // `2 × near_limit_poll_secs` term overtakes the floor and starts widening the reactive
        // lookahead — see the peak-runway interaction below.
        for edge in [5u64, 156] {
            let cfg = Config::parse(&with_tunables(&format!("near_limit_poll_secs = {edge}")))
                .unwrap_or_else(|e| panic!("near_limit_poll_secs = {edge} is a valid edge: {e:?}"));
            assert_eq!(cfg.tunables.near_limit_poll_secs, edge);
        }

        // The UPPER band edge (3600) passes THIS field's own range check but now trips the issue
        // #608 cross-field peak-runway rule: a 3600 s near-limit poll widens the reactive
        // re-observation-gap lookahead (`2 × 3600 = 7200 s`, the raw value the daemon's fire path
        // feeds `swap::reactive_poll_gap_secs`, daemon.rs) so far that at peak velocity NO reserve
        // keeps runway — ADR-0023 § Consequences' absurd corner. That it surfaces as
        // `ConfigPeakRunwayUnsatisfiable`, NOT the field-range `ConfigInvalid`, is the proof 3600 is
        // still WITHIN the 5..=3600 band (the field check passed; the later cross-field rule caught
        // the combination). A value truly ABOVE the band (3601) still trips the field range first.
        match Config::parse(&with_tunables("near_limit_poll_secs = 3600")) {
            Err(Error::ConfigPeakRunwayUnsatisfiable {
                near_limit_poll_secs,
                ..
            }) => assert_eq!(
                near_limit_poll_secs, 3600,
                "the field value threaded into the bound"
            ),
            other => {
                panic!("3600 must pass field-range and trip the peak-runway rule, got: {other:?}")
            }
        }
        match Config::parse(&with_tunables("near_limit_poll_secs = 3601")) {
            Err(Error::ConfigInvalid(msg)) => assert!(
                msg.contains("near_limit_poll_secs") && msg.contains("5..=3600"),
                "above-band must trip the field range first, got: {msg}"
            ),
            other => {
                panic!("3601 is above the field band — expected ConfigInvalid, got: {other:?}")
            }
        }

        // An above-base cap LOADS (no cadence cross-field bound, and satisfiable for peak-runway):
        // with poll_secs = 30 the base sub-interval is already < 60, so a 60 s cap can never bind —
        // but it is inert, not a rejection. 60 keeps the reactive gap at the 313 s floor, so the
        // peak-runway bound stays satisfiable too.
        let inert = Config::parse(&with_tunables("poll_secs = 30\nnear_limit_poll_secs = 60"))
            .expect("an above-base cap is inert, not an error (no cross-field bound)");
        assert_eq!(inert.tunables.near_limit_poll_secs, 60);
    }

    #[test]
    fn fleet_runway_warn_secs_accepts_zero_disabled_or_the_60_to_2592000_band() {
        // Issue #650: the proactive fleet-runway warn threshold is `0` (disabled — the shipped
        // DEFAULT and the kill-switch, since the warning is opt-in) OR in the 60..=2_592_000 s
        // band (one minute of lead time up to a 30-day cap). The `0`-OR-band shape mirrors
        // `near_limit_poll_secs` above: a naive `(60..=2_592_000).contains()` WITHOUT the `!= 0`
        // guard would reject the documented default/kill-switch. There is deliberately NO
        // cross-field bound — it is a pure operator-visibility line, coupled to no decision field.

        // Absent → the compiled-in `0` default (OFF).
        let default = Config::parse(&with_tunables("poll_secs = 300")).unwrap();
        assert_eq!(default.tunables.fleet_runway_warn_secs, 0);

        // 0 is the disabled default/kill-switch and MUST load — not a sub-floor rejection.
        let disabled = Config::parse(&with_tunables("fleet_runway_warn_secs = 0"))
            .expect("0 is the valid disabled default, not a sub-floor rejection");
        assert_eq!(disabled.tunables.fleet_runway_warn_secs, 0);

        // Both band edges load and thread through verbatim.
        for edge in [60u64, 2_592_000] {
            let cfg = Config::parse(&with_tunables(&format!("fleet_runway_warn_secs = {edge}")))
                .unwrap_or_else(|e| {
                    panic!("fleet_runway_warn_secs = {edge} is a valid edge: {e:?}")
                });
            assert_eq!(cfg.tunables.fleet_runway_warn_secs, edge);
        }

        // A non-zero sub-floor (1, 59) and an above-cap value (2_592_001) each trip the field
        // range: the `!= 0` guard admits ONLY 0, never a sub-60 warn line.
        for bad in [1u64, 59, 2_592_001] {
            match Config::parse(&with_tunables(&format!("fleet_runway_warn_secs = {bad}"))) {
                Err(Error::ConfigInvalid(msg)) => assert!(
                    msg.contains("fleet_runway_warn_secs") && msg.contains("60..=2592000"),
                    "out-of-band {bad} must trip the field range, got: {msg}"
                ),
                other => panic!("{bad} is out of band — expected ConfigInvalid, got: {other:?}"),
            }
        }
    }

    // ── issue #608: the peak-velocity runway coupling (validator + advisory) ──

    #[test]
    fn the_shipped_defaults_load_despite_sitting_in_the_advisory_band() {
        // The load-bearing severity-split fact: the shipped default (session_ceiling 95, target_max
        // 80, near_limit 60, horizon 120) is in the EXCEEDED-but-satisfiable state, so it MUST load —
        // erroring here would brick every stock install (the #417 failure mode). But `config
        // validate`'s advisory accessor DOES flag it, since 80 > the ~52 bound.
        let cfg =
            Config::parse(&with_tunables("session_ceiling = 95")).expect("defaults must load");
        let advisory = cfg
            .peak_runway_advisory()
            .expect("the default reserve 80 exceeds its ~52 peak-runway bound");
        assert_eq!(advisory.target_max_session_usage, 80);
        assert_eq!(advisory.bound_pct, 52);
        assert_eq!(
            advisory.window_secs, 313,
            "default window is the 313 s p90 floor"
        );
    }

    #[test]
    fn rejects_the_unsatisfiable_peak_runway_stack_with_a_distinct_error() {
        // near_limit 3600 → reactive poll_gap 7200 s; at peak velocity NO reserve in 1..=95 keeps
        // runway, so the composed fire point has collapsed below 0. A DISTINCT variant from
        // ConfigTargetMaxSessionAboveTrigger (the looser ceiling bound) and ConfigInvalid, carrying
        // the offending tunables so the operator sees exactly what to lower.
        let toml = with_tunables("near_limit_poll_secs = 3600");
        match Config::parse(&toml) {
            Err(Error::ConfigPeakRunwayUnsatisfiable {
                trigger,
                near_limit_poll_secs,
                horizon_secs,
                window_secs,
                bound_pct,
                v_peak_pct_per_min,
            }) => {
                assert_eq!(trigger, 95);
                assert_eq!(near_limit_poll_secs, 3600);
                assert_eq!(horizon_secs, 120);
                assert_eq!(window_secs, 7200, "window = max(2×3600, 313, 120)");
                assert!(bound_pct < 0, "the bound must be non-positive: {bound_pct}");
                assert!((v_peak_pct_per_min - 6.95).abs() < 1e-9);
            }
            other => {
                panic!("the absurd stack must be ConfigPeakRunwayUnsatisfiable, got: {other:?}")
            }
        }
    }

    #[test]
    fn peak_runway_unsatisfiable_message_names_the_tunables_and_the_remedy() {
        // The load error is operator-facing: it must name the three offending tunables and the
        // remedy (lower a lookahead knob or raise the ceiling), and stay secret-free (bare numbers).
        let err = Config::parse(&with_tunables("near_limit_poll_secs = 3600")).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("near_limit_poll_secs"),
            "names the knob: {msg}"
        );
        assert!(
            msg.contains("session_velocity_horizon_secs"),
            "names the knob: {msg}"
        );
        assert!(msg.contains("session_ceiling"), "names the ceiling: {msg}");
        assert!(msg.contains("peak"), "explains the mechanism: {msg}");
        // No internal cross-references leak into the operator string (CLAUDE.md audience-fidelity).
        assert!(
            !msg.contains("ADR-"),
            "no ADR pointer in a user string: {msg}"
        );
        assert!(
            !msg.contains("#608"),
            "no issue pointer in a user string: {msg}"
        );
    }

    #[test]
    fn no_peak_runway_advisory_when_the_reserve_honors_the_bound() {
        // A reserve AT or below the bound produces no advisory — the None arm. With near_limit 60
        // (window 313, bound ~52) a target_max of 50 sits under the bound, so nothing to warn about.
        let cfg = Config::parse(&with_tunables(
            "session_ceiling = 95\ntarget_max_session_usage = 50",
        ))
        .expect("a reserve under the bound loads");
        assert_eq!(
            cfg.peak_runway_advisory(),
            None,
            "a reserve honoring its bound must produce no advisory"
        );
    }

    #[test]
    fn peak_runway_advisory_tracks_a_narrowed_lookahead() {
        // Lowering the lookahead RAISES the bound: with fast-poll disabled (near_limit 0) and a
        // short horizon 30, the window is just 30 s, so the bound = 0.89 − 0.0011583×30 ≈ 0.855 → 85.
        // A default reserve 80 then sits UNDER that higher bound — no advisory. This proves the
        // advisory reflects the actual composed lookahead, not a fixed number.
        let cfg = Config::parse(&with_tunables(
            "session_ceiling = 95\nnear_limit_poll_secs = 0\nsession_velocity_horizon_secs = 30",
        ))
        .expect("a tight-lookahead config loads");
        assert_eq!(
            cfg.peak_runway_advisory(),
            None,
            "an 80 reserve is safe once the lookahead is only 30 s (bound ~85)"
        );
        // But push the reserve above even that tight bound and the advisory returns, naming the 30 s window.
        let cfg = Config::parse(&with_tunables(
            "session_ceiling = 95\nnear_limit_poll_secs = 0\nsession_velocity_horizon_secs = 30\ntarget_max_session_usage = 90",
        ))
        .expect("loads");
        let advisory = cfg
            .peak_runway_advisory()
            .expect("90 exceeds the ~85 tight bound");
        assert_eq!(advisory.bound_pct, 85);
        assert_eq!(advisory.window_secs, 30);
    }

    #[test]
    fn cooldown_secs_has_a_non_zero_floor_it_cannot_be_configured_below() {
        // Issue #272: the swap cooldown is tunable ABOVE a non-zero floor but can
        // never be disabled to zero. A sub-floor `cooldown_secs` (including 0) is a
        // load-time rejection whose message names the field and the floor, and the
        // floor edge itself loads and is preserved through to the drawn strategy base.
        // (The floor's non-zero-ness is a compile-time guard at COOLDOWN_SECS_FLOOR.)
        for below in [0, COOLDOWN_SECS_FLOOR - 1] {
            let toml = with_tunables(&format!("cooldown_secs = {below}"));
            match Config::parse(&toml) {
                Err(Error::ConfigInvalid(msg)) => assert!(
                    msg.contains("cooldown_secs") && msg.contains(&COOLDOWN_SECS_FLOOR.to_string()),
                    "rejection must name the field and the floor, got: {msg}"
                ),
                Ok(_) => panic!("cooldown_secs = {below} must be rejected, not accepted"),
                Err(e) => panic!("cooldown_secs = {below} must be ConfigInvalid, got: {e}"),
            }
        }

        // The floor edge loads and threads through to the timing-strategy base.
        let at_floor = with_tunables(&format!("cooldown_secs = {COOLDOWN_SECS_FLOOR}"));
        let config = Config::parse(&at_floor).unwrap();
        assert_eq!(config.tunables.cooldown_secs, COOLDOWN_SECS_FLOOR);
        assert_eq!(
            config.tunables.cooldown_strategy.base,
            COOLDOWN_SECS_FLOOR as f64
        );
    }

    #[test]
    fn accepts_a_roster_less_config_and_preserves_tunables() {
        // Regression (the `capture` bootstrap bug, #58): a well-formed tunables-only
        // file must PARSE (empty roster) and PRESERVE the operator's tunables, so
        // `capture` can load it to add the first account. The "at least one account"
        // rule is the daemon's `require_roster` precondition, not a parse rejection.
        let config =
            Config::parse("[tunables]\npoll_secs = 120\ntarget_max_session_usage = 80\n").unwrap();
        assert!(config.roster.is_empty());
        assert_eq!(config.tunables.poll_secs, 120);
        assert_eq!(config.tunables.target_max_session_usage, 80);
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

    // --- [refresh] schedule (issue #105) ------------------------------------

    #[test]
    fn refresh_defaults_when_table_absent() {
        // No [refresh] table → the feature is ON by default (#409) with its standard defaults.
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.refresh, RefreshConfig::default());
        assert!(config.refresh.enabled);
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
                systemic_failure_n: 3,
                enabled: true,
                accounts: vec![
                    "work".to_owned(),
                    "22222222-2222-2222-2222-222222222222".to_owned()
                ],
                cadence_secs: 7200,
                idle_after_secs: 120,
                timeout_secs: 60,
                claude_bin: Some(PathBuf::from("/opt/claude/bin/claude")),
                // Absent from the parsed TOML above → the #468 default (proactive keep-warm off).
                proactive_keep_warm: false,
            }
        );
        // The cadence is also the near-expiry horizon, exposed as a Duration.
        assert_eq!(config.refresh.cadence(), Duration::from_secs(7200));
        assert_eq!(config.refresh.idle_after(), Duration::from_secs(120));
        assert_eq!(config.refresh.timeout(), Duration::from_secs(60));
    }

    #[test]
    fn refresh_missing_key_takes_its_default() {
        // A partial [refresh] table fills only the named keys; the rest default — and an absent
        // `enabled` key now takes the on-by-default (#409), not off.
        let toml = format!("{VALID}\n[refresh]\ncadence_secs = 7200\n");
        let config = Config::parse(&toml).unwrap();
        assert!(config.refresh.enabled);
        assert_eq!(config.refresh.cadence_secs, 7200);
        assert_eq!(config.refresh.timeout_secs, DEFAULT_REFRESH_TIMEOUT_SECS);
    }

    #[test]
    fn refresh_explicit_false_still_disables() {
        // Backward-compat (#409): an operator can still opt OUT — an explicit `enabled = false`
        // parses to a disabled refresh even though the default is now on. A present key is never
        // overridden by the on-by-default serde default.
        let toml = format!("{VALID}\n[refresh]\nenabled = false\n");
        let config = Config::parse(&toml).unwrap();
        assert!(!config.refresh.enabled);
    }

    #[test]
    fn refresh_proactive_keep_warm_defaults_off() {
        // Issue #468 / finding #476 predicate C: an absent `proactive_keep_warm` key resolves to
        // OFF even with `[refresh]` maintenance ON — the active account is then kept warm reactively
        // (on a real 401) + recovered by #467, not by the pre-emptive live-canonical mint.
        let toml = format!("{VALID}\n[refresh]\nenabled = true\n");
        let config = Config::parse(&toml).unwrap();
        assert!(config.refresh.enabled);
        assert!(
            !config.refresh.proactive_keep_warm,
            "proactive keep-warm is off by default (#468)"
        );
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
    fn refresh_systemic_failure_n_out_of_range_is_rejected() {
        // The #378 systemic threshold is bounded `1..=100`: a `0` (which would arm the detector
        // before a single failed sweep) and an above-ceiling value both fail, naming the field.
        for bad in ["systemic_failure_n = 0", "systemic_failure_n = 101"] {
            let toml = format!("{VALID}\n[refresh]\n{bad}\n");
            let err = Config::parse(&toml).unwrap_err();
            assert!(
                matches!(&err, Error::ConfigInvalid(msg) if msg.contains("refresh.systemic_failure_n")),
                "expected a refresh.systemic_failure_n range error, got {err:?}"
            );
        }
        // Both inclusive endpoints parse.
        for ok in ["systemic_failure_n = 1", "systemic_failure_n = 100"] {
            let toml = format!("{VALID}\n[refresh]\n{ok}\n");
            assert!(Config::parse(&toml).is_ok(), "{ok} should parse");
        }
    }

    #[test]
    fn empty_claude_bin_collapses_to_none() {
        // A stray `claude_bin = ""` defers to $CLAUDE_BIN/$PATH (None), like omitting it.
        let toml = format!("{VALID}\n[refresh]\nclaude_bin = \"   \"\n");
        assert_eq!(Config::parse(&toml).unwrap().refresh.claude_bin, None);
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

    // --- timing jitter strategies (issue #38) ------------------------------

    #[test]
    fn poll_jitter_defaults_to_normal_session_ceiling_and_cooldown_stay_fixed() {
        // AC: poll interval uses normal jitter by default; session_ceiling, weekly_ceiling
        // and cooldown are fixed unless the operator configures a strategy. Bases
        // mirror the validated scalar tunables.
        let t = Config::parse(VALID).unwrap().tunables;
        assert_eq!(
            t.poll_strategy.jitter,
            Jitter::Normal {
                stddev: DEFAULT_POLL_JITTER_STDDEV
            }
        );
        assert_eq!(t.session_ceiling_strategy.jitter, Jitter::None);
        assert_eq!(t.weekly_ceiling_strategy.jitter, Jitter::None);
        assert_eq!(t.cooldown_strategy.jitter, Jitter::None);
        assert_eq!(t.poll_strategy.base, 30.0);
        assert_eq!(t.session_ceiling_strategy.base, 90.0);
        assert_eq!(t.weekly_ceiling_strategy.base, 97.0);
        assert_eq!(t.cooldown_strategy.base, 45.0);
    }

    #[test]
    fn parses_a_full_jitter_table() {
        let toml = with_jitter(
            "poll = { kind = \"normal\", stddev = 25.0 }\n\
             session_ceiling = { kind = \"uniform\", spread = 2.5 }\n\
             weekly_ceiling = { kind = \"normal\", stddev = 1.0 }\n\
             cooldown = { kind = \"none\" }",
        );
        let t = Config::parse(&toml).unwrap().tunables;
        assert_eq!(t.poll_strategy.jitter, Jitter::Normal { stddev: 25.0 });
        assert_eq!(
            t.session_ceiling_strategy.jitter,
            Jitter::Uniform { spread: 2.5 }
        );
        assert_eq!(
            t.weekly_ceiling_strategy.jitter,
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
    fn accepts_inclusive_bounds() {
        // Each bound's edge is valid: trigger 50/99, target_max_session_usage 1 (the non-zero lower bound;
        // 0 admits no target) and floor == trigger, poll 5/3600, cooldown 5/3600 (5 =
        // the non-zero floor, #272), monitor 1/20.
        for fragment in [
            "session_ceiling = 50\ntarget_max_session_usage = 1",
            "session_ceiling = 99\ntarget_max_session_usage = 99", // target_max_session_usage == trigger is allowed
            "weekly_ceiling = 50",
            "weekly_ceiling = 99",
            "poll_secs = 5",
            "poll_secs = 3600",
            "cooldown_secs = 5",
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

    // --- [stats] block (issue #161) -----------------------------------------

    #[test]
    fn stats_defaults_when_the_table_is_absent() {
        // A config with no `[stats]` table (VALID has none) loads the documented defaults —
        // the same opt-out contract as `[refresh]` / `[login]`.
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.stats, StatsConfig::default());
        assert_eq!(config.stats.raw_retention_secs, 14 * 86_400);
        assert_eq!(config.stats.hourly_retention_secs, 90 * 86_400);
        assert_eq!(config.stats.daily_retention_secs, 0); // 0 = lifetime
        assert_eq!(config.stats.default_period, "week");
    }

    #[test]
    fn parses_a_full_stats_override() {
        // Every key set to a non-default the operator chose, all within bounds.
        let toml = format!(
            "{VALID}\n[stats]\n\
             raw_retention_secs = 604800\n\
             hourly_retention_secs = 2592000\n\
             daily_retention_secs = 31536000\n\
             default_period = \"month\"\n"
        );
        let config = Config::parse(&toml).unwrap();
        assert_eq!(
            config.stats,
            StatsConfig {
                raw_retention_secs: 604_800,
                hourly_retention_secs: 2_592_000,
                daily_retention_secs: 31_536_000,
                default_period: "month".to_owned(),
            }
        );
    }

    #[test]
    fn a_partial_stats_table_fills_only_named_keys() {
        // Like `[refresh]`, a partial table sets only the named key; the rest default.
        let toml = format!("{VALID}\n[stats]\ndefault_period = \"lifetime\"\n");
        let config = Config::parse(&toml).unwrap();
        assert_eq!(config.stats.default_period, "lifetime");
        assert_eq!(config.stats.raw_retention_secs, 14 * 86_400); // untouched → default
        assert_eq!(config.stats.daily_retention_secs, 0);
    }

    #[test]
    fn rejects_each_out_of_range_stats_horizon() {
        for (key, value) in [
            ("raw_retention_secs", "3599"),     // below 1h
            ("raw_retention_secs", "31536001"), // above 365d
            ("hourly_retention_secs", "86399"), // below 1d
            ("hourly_retention_secs", "315360001"),
            ("daily_retention_secs", "-1"), // below the 0 = lifetime floor
            ("daily_retention_secs", "315360001"), // above the cap
        ] {
            let toml = format!("{VALID}\n[stats]\n{key} = {value}\n");
            assert!(
                matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))),
                "stats.{key} = {value} should be rejected"
            );
        }
    }

    #[test]
    fn accepts_zero_daily_retention_as_lifetime() {
        // The 0 = lifetime sentinel is IN range (it is the default), not rejected.
        let toml = format!("{VALID}\n[stats]\ndaily_retention_secs = 0\n");
        let config = Config::parse(&toml).unwrap();
        assert_eq!(config.stats.daily_retention_secs, 0);
    }

    #[test]
    fn rejects_an_unknown_stats_default_period() {
        let toml = format!("{VALID}\n[stats]\ndefault_period = \"fortnight\"\n");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))));
    }

    // --- [migration] block (issue #150) -------------------------------------

    #[test]
    fn migration_defaults_when_the_table_is_absent() {
        // A config with no `[migration]` table (VALID has none) loads the documented defaults —
        // the same opt-out contract as `[stats]` / `[refresh]` / `[login]`.
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.migration, MigrationConfig::default());
        assert_eq!(config.migration.kdf_memory_kib, 65_536);
        assert_eq!(config.migration.kdf_iterations, 3);
        assert_eq!(config.migration.conflict_policy, ConflictPolicy::Skip);
    }

    #[test]
    fn parses_a_full_migration_override() {
        // Every key set to a non-default the operator chose, all within bounds.
        let toml = format!(
            "{VALID}\n[migration]\n\
             kdf_memory_kib = 131072\n\
             kdf_iterations = 4\n\
             conflict_policy = \"overwrite\"\n"
        );
        let config = Config::parse(&toml).unwrap();
        assert_eq!(
            config.migration,
            MigrationConfig {
                kdf_memory_kib: 131_072,
                kdf_iterations: 4,
                conflict_policy: ConflictPolicy::Overwrite,
            }
        );
    }

    #[test]
    fn a_partial_migration_table_fills_only_named_keys() {
        // Like the other blocks, a partial table sets only the named key; the rest default.
        let toml = format!("{VALID}\n[migration]\nconflict_policy = \"overwrite\"\n");
        let config = Config::parse(&toml).unwrap();
        assert_eq!(config.migration.conflict_policy, ConflictPolicy::Overwrite);
        assert_eq!(config.migration.kdf_memory_kib, 65_536); // untouched → default
        assert_eq!(config.migration.kdf_iterations, 3);
    }

    #[test]
    fn rejects_each_out_of_range_migration_kdf_cost() {
        for (key, value) in [
            ("kdf_memory_kib", "7"),       // below the 8 KiB floor
            ("kdf_memory_kib", "1048577"), // above the 1 GiB decrypt-time guard
            ("kdf_iterations", "0"),       // below 1
            ("kdf_iterations", "17"),      // above the 16 decrypt-time guard
        ] {
            let toml = format!("{VALID}\n[migration]\n{key} = {value}\n");
            assert!(
                matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))),
                "migration.{key} = {value} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_an_unknown_migration_conflict_policy() {
        let toml = format!("{VALID}\n[migration]\nconflict_policy = \"merge\"\n");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigInvalid(_))));
    }
}
