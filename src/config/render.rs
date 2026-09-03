// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Getting a [`Config`] OUT: the hand-written TOML emitter, the `0600` write seam, and the
//! origin-tagged read-only view (issue #638's per-concern decomposition of the one
//! 1,253-line `impl Config`).
//!
//! [`Config::render`] and [`Config::origin_report`] sit together deliberately: the origin
//! report mirrors `render`'s field walk — same sections, same order — and formats through
//! the SAME [`basic_string`] / [`render_str_array`] / [`render_jitter`] helpers, so the
//! `config show` view and the persisted file can never speak different syntax. That mirroring
//! is a standing OBLIGATION on anyone growing the schema, not a one-time coincidence: a
//! tunable added to one walk and forgotten in the other is silently DROPPED from `config show`
//! rather than failing loudly, which is why this module's
//! `origin_report_reports_every_key_render_writes` drift guard (issue #401) exists. Only the
//! report BODY lives here; the file read that feeds it is
//! [`Config::load_with_origin`], over in [`super::load`].

use super::*;

use crate::stats::FLEET_RUNWAY_PLAUSIBLE_MAX_SECS;

impl Config {
    /// Build the origin report from the effective config (`self`) and the raw TOML
    /// `table` (the presence source). Mirrors [`render`](Config::render)'s field walk —
    /// same sections, same order, same value formatting — but emits `(key, value,
    /// origin)` triples instead of persisted TOML. The schema's single source of truth
    /// stays with the structs here; the CLI only formats what this returns.
    pub(super) fn origin_report(&self, table: &toml::Table) -> OriginReport {
        // Is `[section].key` present in the raw file? An absent section (or key) →
        // the value the effective config carries is a compiled-in default.
        let present = |section: &str, key: &str| -> Origin {
            match table
                .get(section)
                .and_then(toml::Value::as_table)
                .map(|t| t.contains_key(key))
            {
                Some(true) => Origin::FromFile,
                _ => Origin::Default,
            }
        };
        let entry =
            |key: &'static str, value: String, origin: Origin| OriginEntry { key, value, origin };

        let t = &self.tunables;
        let tunables = OriginSection {
            header: "[tunables]",
            present: table.contains_key("tunables"),
            entries: vec![
                entry(
                    "poll_secs",
                    t.poll_secs.to_string(),
                    present("tunables", "poll_secs"),
                ),
                entry(
                    "exhausted_poll_secs",
                    t.exhausted_poll_secs.to_string(),
                    present("tunables", "exhausted_poll_secs"),
                ),
                entry(
                    "near_limit_poll_secs",
                    t.near_limit_poll_secs.to_string(),
                    present("tunables", "near_limit_poll_secs"),
                ),
                entry(
                    "cooldown_secs",
                    t.cooldown_secs.to_string(),
                    present("tunables", "cooldown_secs"),
                ),
                entry(
                    "target_max_session_usage",
                    t.target_max_session_usage.to_string(),
                    present("tunables", "target_max_session_usage"),
                ),
                entry(
                    "session_ceiling",
                    t.session_ceiling.to_string(),
                    present("tunables", "session_ceiling"),
                ),
                entry(
                    "weekly_ceiling",
                    t.weekly_ceiling.to_string(),
                    present("tunables", "weekly_ceiling"),
                ),
                entry(
                    "session_blind_swap_secs",
                    t.session_blind_swap_secs.to_string(),
                    present("tunables", "session_blind_swap_secs"),
                ),
                entry(
                    "session_blind_risk_band",
                    t.session_blind_risk_band.to_string(),
                    present("tunables", "session_blind_risk_band"),
                ),
                entry(
                    "session_velocity_horizon_secs",
                    t.session_velocity_horizon_secs.to_string(),
                    present("tunables", "session_velocity_horizon_secs"),
                ),
                entry(
                    "session_velocity_min_project_above",
                    t.session_velocity_min_project_above.to_string(),
                    present("tunables", "session_velocity_min_project_above"),
                ),
                entry(
                    "session_velocity_ema_alpha_pct",
                    t.session_velocity_ema_alpha_pct.to_string(),
                    present("tunables", "session_velocity_ema_alpha_pct"),
                ),
                entry(
                    "monitor_401_n",
                    t.monitor_401_n.to_string(),
                    present("tunables", "monitor_401_n"),
                ),
                entry(
                    "monitor_recovery_m",
                    t.monitor_recovery_m.to_string(),
                    present("tunables", "monitor_recovery_m"),
                ),
                entry(
                    "fleet_runway_warn_secs",
                    t.fleet_runway_warn_secs.to_string(),
                    present("tunables", "fleet_runway_warn_secs"),
                ),
                entry(
                    "canary_drift_override",
                    t.canary_drift_override.to_string(),
                    present("tunables", "canary_drift_override"),
                ),
                entry(
                    "canary_nostashmatch_override",
                    t.canary_nostashmatch_override.to_string(),
                    present("tunables", "canary_nostashmatch_override"),
                ),
                entry(
                    "canary_online_probe",
                    t.canary_online_probe.to_string(),
                    present("tunables", "canary_online_probe"),
                ),
                entry(
                    "canary_online_probe_strict",
                    t.canary_online_probe_strict.to_string(),
                    present("tunables", "canary_online_probe_strict"),
                ),
                entry(
                    "verbose",
                    t.verbose.to_string(),
                    present("tunables", "verbose"),
                ),
            ],
        };

        let jitter = OriginSection {
            header: "[jitter]",
            present: table.contains_key("jitter"),
            entries: vec![
                entry(
                    "poll",
                    render_jitter(&t.poll_strategy.jitter),
                    present("jitter", "poll"),
                ),
                entry(
                    "session_ceiling",
                    render_jitter(&t.session_ceiling_strategy.jitter),
                    present("jitter", "session_ceiling"),
                ),
                entry(
                    "weekly_ceiling",
                    render_jitter(&t.weekly_ceiling_strategy.jitter),
                    present("jitter", "weekly_ceiling"),
                ),
                entry(
                    "cooldown",
                    render_jitter(&t.cooldown_strategy.jitter),
                    present("jitter", "cooldown"),
                ),
            ],
        };

        let r = &self.refresh;
        let refresh = OriginSection {
            header: "[refresh]",
            present: table.contains_key("refresh"),
            entries: vec![
                entry(
                    "enabled",
                    r.enabled.to_string(),
                    present("refresh", "enabled"),
                ),
                entry(
                    "accounts",
                    render_str_array(&r.accounts),
                    present("refresh", "accounts"),
                ),
                entry(
                    "cadence_secs",
                    r.cadence_secs.to_string(),
                    present("refresh", "cadence_secs"),
                ),
                entry(
                    "idle_after_secs",
                    r.idle_after_secs.to_string(),
                    present("refresh", "idle_after_secs"),
                ),
                entry(
                    "timeout_secs",
                    r.timeout_secs.to_string(),
                    present("refresh", "timeout_secs"),
                ),
                entry(
                    "systemic_failure_n",
                    r.systemic_failure_n.to_string(),
                    present("refresh", "systemic_failure_n"),
                ),
                entry(
                    "proactive_keep_warm",
                    r.proactive_keep_warm.to_string(),
                    present("refresh", "proactive_keep_warm"),
                ),
                entry(
                    "claude_bin",
                    render_optional_bin(&r.claude_bin),
                    present("refresh", "claude_bin"),
                ),
            ],
        };

        let l = &self.login;
        let login = OriginSection {
            header: "[login]",
            present: table.contains_key("login"),
            entries: vec![
                entry(
                    "timeout_secs",
                    l.timeout_secs.to_string(),
                    present("login", "timeout_secs"),
                ),
                entry(
                    "claude_bin",
                    render_optional_bin(&l.claude_bin),
                    present("login", "claude_bin"),
                ),
            ],
        };

        let s = &self.stats;
        let stats = OriginSection {
            header: "[stats]",
            present: table.contains_key("stats"),
            entries: vec![
                entry(
                    "raw_retention_secs",
                    s.raw_retention_secs.to_string(),
                    present("stats", "raw_retention_secs"),
                ),
                entry(
                    "hourly_retention_secs",
                    s.hourly_retention_secs.to_string(),
                    present("stats", "hourly_retention_secs"),
                ),
                entry(
                    "daily_retention_secs",
                    s.daily_retention_secs.to_string(),
                    present("stats", "daily_retention_secs"),
                ),
                entry(
                    "default_period",
                    basic_string(&s.default_period),
                    present("stats", "default_period"),
                ),
            ],
        };

        let mi = &self.migration;
        let migration = OriginSection {
            header: "[migration]",
            present: table.contains_key("migration"),
            entries: vec![
                entry(
                    "kdf_memory_kib",
                    mi.kdf_memory_kib.to_string(),
                    present("migration", "kdf_memory_kib"),
                ),
                entry(
                    "kdf_iterations",
                    mi.kdf_iterations.to_string(),
                    present("migration", "kdf_iterations"),
                ),
                entry(
                    "conflict_policy",
                    basic_string(mi.conflict_policy.as_str()),
                    present("migration", "conflict_policy"),
                ),
            ],
        };

        let cr = &self.credential;
        let credential = OriginSection {
            header: "[credential]",
            present: table.contains_key("credential"),
            entries: vec![
                entry(
                    "expiry_horizon_secs",
                    cr.expiry_horizon_secs.to_string(),
                    present("credential", "expiry_horizon_secs"),
                ),
                entry(
                    "expiry_cohort_window_secs",
                    cr.expiry_cohort_window_secs.to_string(),
                    present("credential", "expiry_cohort_window_secs"),
                ),
            ],
        };

        OriginReport {
            sections: vec![
                tunables, jitter, refresh, login, stats, migration, credential,
            ],
            roster_count: self.roster.len(),
            // The roster is the `[[account]]` array-of-tables (RawConfig's `account`).
            roster_present: table.contains_key("account"),
        }
    }

    /// Persist this config to the canonical `config.toml` (`0600`, parent `0700`), with the
    /// inline tunable-documenting comments. The write path for the standalone `capture` (#4).
    #[allow(dead_code)]
    pub(crate) async fn save(&self) -> Result<()> {
        self.save_to(&paths::config_file()?).await
    }

    /// Persist this config to an EXPLICIT `path` (`0600`, parent `0700`) — the injectable-path
    /// write seam, the counterpart of [`load_path`](Config::load_path). The daemon-routed
    /// `cmd:capture` (#359) writes back through its wired `config_path` (so a hermetic test lands
    /// the new roster in a temp file, not the real support dir), exactly as [`save`](Config::save)
    /// writes the canonical location for the standalone `capture` (#4).
    ///
    /// Every replacement of a config file passes through here, which is why the roster backup
    /// ring hangs off THIS function rather than off its callers (issue #1439, design D-3): the
    /// 2026-08-27 deletion is unattributed, so the write paths that could destroy a roster
    /// cannot be enumerated, and a per-caller guard would structurally miss the next one. The
    /// ring keys on the REPLACED file's own quality instead —
    /// [`retain_if_qualifying`](crate::roster_backup::retain_if_qualifying) retains it only if
    /// it parses with a non-empty roster, and only such a write may evict, so a bad write can
    /// never displace a good backup. A qualifying file that cannot be retained aborts the write
    /// rather than overwriting the last good roster; a non-qualifying one touches nothing and
    /// so cannot fail here.
    ///
    /// The retention is TWO-PHASE, and the second phase is what keeps this function's failure a
    /// true no-op. Retaining has to precede the write — afterwards the file it copies is gone —
    /// but eviction lives in
    /// [`Retention::commit`](crate::roster_backup::Retention::commit), which runs only once the
    /// replacement has landed. A failed write rolls the entry back instead, so the ring is
    /// byte-identical to what it was. Without that split, a config write failing repeatedly
    /// (a full volume, a `config.toml.tmp` that is not a file) would evict a good entry per
    /// attempt while every attempt reported writing nothing — the "fixed-size countdown to
    /// losing everything" D-3 forbids, reached through QUALIFYING writes, which is exactly the
    /// path the ring's own non-qualifying-write guard cannot see. `perform_config_set` in
    /// `src/daemon/commands.rs` states the contract this preserves: a refusal is a true no-op.
    pub(crate) async fn save_to(&self, path: &Path) -> Result<()> {
        self.save_to_within(path, super::write_lock::CONFIG_WRITE_LOCK_MAX_WAIT)
            .await
    }

    /// [`save_to`](Config::save_to) with an explicit config-write-lock budget.
    ///
    /// Split out so the FAIL-CLOSED return can be observed THROUGH this function in
    /// milliseconds. Waiting out the production budget would put a 5 s sleep in the suite, and
    /// cancelling the future with an outer `timeout` instead observes a cancellation — it never
    /// reaches the [`Error::ConfigWriteLockBusy`] return, so it cannot tell that variant apart
    /// from any other way of not finishing.
    async fn save_to_within(&self, path: &Path, lock_budget: std::time::Duration) -> Result<()> {
        paths::ensure_private_dir(
            path.parent()
                .expect("a config path always has a parent directory"),
        )?;
        // The config-write lock (#1445, D-8), acquired BEFORE the file about to be replaced is
        // read and held until this function returns — so what it serializes is the whole
        // read-modify-write (retain the replaced file → publish the replacement → prune the
        // ring), not merely the write in the middle. A contended acquire fails closed HERE,
        // ahead of every effect below, which is what makes a refusal a true no-op: nothing
        // retained, nothing written, nothing evicted. Dropped on return, releasing the lock.
        let _write_lock = super::write_lock::ConfigWriteLock::acquire(
            &super::write_lock::lock_path(path),
            lock_budget,
        )
        .await?;
        let retention = crate::roster_backup::retain_if_qualifying(path)?;
        match paths::write_private_file(path, self.render().as_bytes()) {
            Ok(()) => {
                if let Some(retention) = retention {
                    retention.commit();
                }
                Ok(())
            }
            Err(err) => {
                if let Some(retention) = retention {
                    retention.roll_back();
                }
                Err(err)
            }
        }
    }

    /// Render the config back to TOML with the inline tunable-documenting
    /// comments (issue #3 N2). Emitted by hand *by design* (issue #181, ADR-0005):
    /// `serde` serialization cannot emit comments at all, and `toml_edit` (not a
    /// current dependency) would still hand-author every comment as node decor and
    /// re-express the OFF-state opt-ins as injected text — for more ceremony and a
    /// new direct dep. So the file is rendered by hand; integers need no escaping
    /// and roster strings go through [`basic_string`].
    ///
    /// `pub(crate)` so the `export` verb (issue #148) can serialize the canonical
    /// config text into a migration artifact ([`crate::migration::Payload`]).
    pub(crate) fn render(&self) -> String {
        let t = &self.tunables;
        let mut out = String::new();
        out.push_str("# sessiometer configuration.\n");
        out.push_str(
            "# The roster is managed by `sessiometer capture`; the [tunables] block is\n\
             # safe to hand-edit. Percentages are of the rolling session window.\n\
             #\n\
             # Single-machine boundary (issue #613): sessiometer coordinates only WITHIN one\n\
             # machine — the single-owner lock is a per-machine flock. Running this same roster\n\
             # on a second machine at once is possible, and each daemon is blind to the other's\n\
             # usage: two machines can co-consume an account (the swap tail margin is\n\
             # single-machine-calibrated) and a landing can overshoot unseen by the local\n\
             # signal. Velocity-spike detection reads the account-global usage and reduces —\n\
             # does not remove — this exposure. Prefer one roster per machine.\n\n",
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
        out.push_str(
            "# Widened re-poll cadence (poll_secs..=86400) for an out-of-rotation peer — one\n\
             # that is weekly- or session-exhausted (issue #537). Its usage can only change\n\
             # when its server-side window resets (a time the daemon already knows) or on a\n\
             # rare out-of-band reset, so re-polling it every poll_secs wastes a request. The\n\
             # default 3600 (1 h) is the ceiling; a known resets_at sooner than this polls\n\
             # earlier. The ACTIVE account is never slow-polled. Must be >= poll_secs.\n",
        );
        out.push_str(&format!(
            "exhausted_poll_secs = {}\n",
            t.exhausted_poll_secs
        ));
        out.push_str(
            "# Tightened poll sub-interval (0 to disable, else 5..=3600) for the ACTIVE account\n\
             # while it is near its limit (issue #540) — the mirror of exhausted_poll_secs, which\n\
             # WIDENS an idle peer. On the active account's final climb its cadence tightens to\n\
             # this so no long poll gap opens near the limit; below the near-limit band the cadence\n\
             # is the unchanged poll_secs/N, so the steady rate is flat. Default 60. Applied as\n\
             # min(poll_secs/N, this), so a value above the base sub-interval is inert.\n",
        );
        out.push_str(&format!(
            "near_limit_poll_secs = {}\n",
            t.near_limit_poll_secs
        ));
        out.push_str(&format!(
            "# Seconds to wait after a swap before another swap is allowed \
             ({COOLDOWN_SECS_FLOOR}..=3600; a non-zero floor — pacing can't be disabled to zero).\n"
        ));
        out.push_str(&format!("cooldown_secs = {}\n", t.cooldown_secs));
        out.push_str(
            "# The most-full an account may be to receive the active session: only swap\n\
             # TO an account whose session usage is below this percent (1..=session_ceiling).\n\
             # This is NOT the level that triggers a swap. Default-on (#398); 0 is rejected\n\
             # — it admits no target and would disable proactive swapping.\n",
        );
        out.push_str(&format!(
            "target_max_session_usage = {}\n",
            t.target_max_session_usage
        ));
        out.push_str(
            "# The session CEILING (50..=99): the settled line the active account must not\n\
             # cross, NOT a fire-at trigger. Both swap estimators (reactive + projected) derive\n\
             # their fire point BACKWARD from it, covering the LARGER unseen window — ceiling\n\
             # minus a tail margin minus velocity*max(poll_gap, H) — so the account lands BELOW\n\
             # the ceiling even after its post-swap committed tail (up to +5 pp: in-flight work\n\
             # keeps billing the parked account). The reactive arm looks ahead over the measured\n\
             # p90 re-observation gap (313 s floor, issue #609), so the default 95 is a conservative\n\
             # lever — 99 is reachable (raise it to spend the margin as runway). One knob, two\n\
             # estimators (not two knobs). See ADR-0023 + ADR-0024 (docs/adr).\n",
        );
        out.push_str(&format!("session_ceiling = {}\n", t.session_ceiling));
        out.push_str(
            "# The settled WEEKLY CEILING (50..=99) — the weekly line the active account must\n\
             # NOT cross. Independent of session_ceiling (typically higher): a swap fires when\n\
             # EITHER dimension reaches its own fire point. Like session_ceiling this is a\n\
             # ceiling, not a fire-at value (issue #607): the swap fires BACKWARD from it, 1 pp\n\
             # early, so the outgoing account LANDS below this line after its post-swap committed\n\
             # tail (the same in-flight work that bills the session window bills the weekly one).\n\
             # The 1 pp weekly margin is much smaller than session's 6 pp because that tail is a\n\
             # far smaller fraction of a 7-day window. See ADR-0025 (docs/adr).\n",
        );
        out.push_str(&format!("weekly_ceiling = {}\n", t.weekly_ceiling));
        out.push_str(
            "# Bounded-blindness preemptive swap (issue #452, ADR-0017): when the active\n\
             # account's usage poll stays blind (429/5xx) longer than this many seconds AND\n\
             # its last good reading was at/over session_blind_risk_band, swap it away before\n\
             # it can self-exhaust unobserved. Floor 60; set to the 86400 ceiling to disable.\n",
        );
        out.push_str(&format!(
            "session_blind_swap_secs = {}\n",
            t.session_blind_swap_secs
        ));
        out.push_str(
            "# The last-known session percent (50..=99) at/over which a blind active account\n\
             # is eligible for the preemptive swap above. Set BELOW session_ceiling — it acts\n\
             # on the stale pre-blind reading, before the reactive trigger would fire.\n",
        );
        out.push_str(&format!(
            "session_blind_risk_band = {}\n",
            t.session_blind_risk_band
        ));
        out.push_str(
            "# Velocity-projection preemptive swap (issue #539, ADR-0017): swap the active\n\
             # account away when its PROJECTED session usage (last + velocity * H) crosses the\n\
             # effective ceiling (session_ceiling minus the tail margin, issue #597) before the\n\
             # observed reading does — H is this horizon in seconds (~ the active poll cadence;\n\
             # 120 validated by #538). Set to 0 to disable.\n",
        );
        out.push_str(&format!(
            "session_velocity_horizon_secs = {}\n",
            t.session_velocity_horizon_secs
        ));
        out.push_str(
            "# Only project when the observed session percent (50..=99) is at/over this — the\n\
             # projection can't reach lower anyway, so it is a free guard. Set BELOW\n\
             # session_ceiling (the projective peer fires in the band beneath it).\n",
        );
        out.push_str(&format!(
            "session_velocity_min_project_above = {}\n",
            t.session_velocity_min_project_above
        ));
        out.push_str(
            "# EMA smoothing weight alpha (1..=100 percent) for the session-velocity signal,\n\
             # to damp a single-interval spike so the projection keys off sustained motion.\n\
             # ~50 validated by #538; 100 means no smoothing (raw last-interval velocity).\n",
        );
        out.push_str(&format!(
            "session_velocity_ema_alpha_pct = {}\n",
            t.session_velocity_ema_alpha_pct
        ));
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
        out.push_str(&format!(
            "# Proactive fleet-runway warning (issue #650): when the roster's combined weekly\n\
             # head-room over its combined observed burn (the `stats` fleet runway) drops BELOW\n\
             # this many seconds, the daemon logs ONE edge-triggered fleet_runway_low event —\n\
             # lead time BEFORE the all-exhausted terminal state. 0 disables (the default; the\n\
             # warning is opt-in), else 60..={FLEET_RUNWAY_PLAUSIBLE_MAX_SECS} (1 min..7 d). The\n\
             # ceiling is one weekly quota window, because the runway is measured as a drain with\n\
             # no reset intervening: past that window there is nothing to measure, so a longer\n\
             # warn line could be crossed but never cleared. Purely a visibility signal: it never\n\
             # triggers a swap.\n",
        ));
        out.push_str(&format!(
            "fleet_runway_warn_secs = {}\n",
            t.fleet_runway_warn_secs
        ));
        out.push_str(
            "# Keychain-identity canary override (issue #714): when the pre-swap canary\n\
             # detects DRIFT (the resolved keychain credential byte-matches a DIFFERENT\n\
             # account's stash than the one Claude Code's state names active), the daemon\n\
             # refuses the credential write. Set true ONLY to clear a diagnosed false\n\
             # alarm: swaps proceed despite drift, each logged with overridden=true.\n\
             # Never bypasses the zero/ambiguous resolution guards. Default false.\n",
        );
        out.push_str(&format!(
            "canary_drift_override = {}\n",
            t.canary_drift_override
        ));
        out.push_str(
            "# Keychain-identity canary NoStashMatch shape-gate override (issue #730): when\n\
             # the pre-swap canary finds the resolved canonical matches NO account stash AND\n\
             # does not parse as a Claude Code credential (overwhelmingly an UNRELATED secret\n\
             # under the derived service), the daemon refuses the credential write so an atomic\n\
             # in-place overwrite cannot clobber it. Set true ONLY once you have vetted the\n\
             # canonical as safe (e.g. a legitimate NEW Claude Code credential format to be\n\
             # re-stashed): the write then proceeds, each logged with overridden=true. Separate\n\
             # from canary_drift_override; a well-formed unmatched canonical fails open\n\
             # regardless. Default false.\n",
        );
        out.push_str(&format!(
            "canary_nostashmatch_override = {}\n",
            t.canary_nostashmatch_override
        ));
        out.push_str(
            "# Keychain-identity canary ONLINE liveness probe (issue #736), the canary's only\n\
             # networked layer and therefore opt-in. When true, one bounded /oauth/usage GET\n\
             # runs on the resolved canonical immediately before the swap's credential write,\n\
             # asking only whether that bearer still authenticates. LIVENESS, not identity:\n\
             # the endpoint names no account, so a pass never says WHOSE session it is. It\n\
             # narrows — does not close — the Layer-3 same-account silent-relocation residual,\n\
             # catching a relocated canonical that has gone dead but not one still valid.\n\
             # When false NO request is issued at all. Default false.\n",
        );
        out.push_str(&format!(
            "canary_online_probe = {}\n",
            t.canary_online_probe
        ));
        out.push_str(
            "# Strict mode for that probe (issue #736); only meaningful with\n\
             # canary_online_probe = true. Default false: a probe that comes back rejected or\n\
             # inconclusive is logged and the swap PROCEEDS, so a network outage never becomes\n\
             # a swap outage. Set true to opt INTO that network failure mode — anything short\n\
             # of a confirmed-live bearer then refuses the write before any mutation. Note a\n\
             # 401 alone is weak evidence: Claude Code refreshes its accessToken in place, so a\n\
             # momentarily expired-but-refreshable token answers 401 while it is healthy.\n",
        );
        out.push_str(&format!(
            "canary_online_probe_strict = {}\n",
            t.canary_online_probe_strict
        ));
        out.push_str(
            "# Diagnostic channel for a LAUNCHD-MANAGED daemon (issue #775). The per-poll,\n\
             # per-tick and lifecycle diagnostics (issue #77) are reachable interactively with\n\
             # `sessiometer run -v`, but the installed agent runs `run --managed` with no -v, so\n\
             # a background daemon emits nothing. Set true to turn them on there WITHOUT editing\n\
             # the plist (`service install` would overwrite it). They land on the agent's stderr\n\
             # file and are read back with `sessiometer log --channel diag`. Scoped to --managed:\n\
             # an interactive `run` is unchanged, and -v still wins on either. Takes effect at the\n\
             # NEXT daemon start (no hot reload) — `sessiometer daemon restart`. That channel is\n\
             # raw stderr, so it can also carry panic output: unlike the event log it is NOT\n\
             # redaction-metered. Default false.\n",
        );
        out.push_str(&format!("verbose = {}\n", t.verbose));

        // Per-cycle timing jitter (issue #38): drawn each cycle and clamped to the
        // tunable's valid range, to decorrelate polling/swaps across cycles.
        out.push_str("\n[jitter]\n");
        out.push_str(
            "# Randomization drawn each cycle and clamped to the tunable's range.\n\
             # kind = \"none\" | \"uniform\" (with `spread`) | \"normal\" (with `stddev`).\n\
             # poll defaults to normal jitter (stddev ~20% of poll_secs) so accounts\n\
             # decorrelate; session_ceiling, weekly_ceiling and cooldown default to none.\n",
        );
        out.push_str(&format!(
            "poll = {}\n",
            render_jitter(&t.poll_strategy.jitter)
        ));
        out.push_str(&format!(
            "session_ceiling = {}\n",
            render_jitter(&t.session_ceiling_strategy.jitter)
        ));
        out.push_str(&format!(
            "weekly_ceiling = {}\n",
            render_jitter(&t.weekly_ceiling_strategy.jitter)
        ));
        out.push_str(&format!(
            "cooldown = {}\n",
            render_jitter(&t.cooldown_strategy.jitter)
        ));

        // The periodic isolated-refresh schedule (issue #105). ON by default (#409): each refresh
        // slides the stored expiry forward and re-stashes any rotated token, so it is self-sustaining
        // (#101's TTL question is resolved — a sliding window, not a fixed cap).
        let r = &self.refresh;
        out.push_str("\n[refresh]\n");
        out.push_str(
            "# Periodically let Claude Code refresh PARKED accounts' stored tokens in an\n\
             # isolated config dir (the in-daemon counterpart of `poke`), off the\n\
             # poll/usage/swap seam — the live session's credential is never touched. The\n\
             # active account and the imminent swap target are always excluded. ON by\n\
             # default: each refresh slides the stored token's expiry forward and re-stashes\n\
             # any rotated refresh token, so the schedule is self-sustaining. Set enabled\n\
             # = false to turn it off.\n",
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
             # (i.e. before the next tick). A conservative one-hour default.\n",
        );
        out.push_str(&format!("cadence_secs = {}\n", r.cadence_secs));
        out.push_str(
            "# Seconds the daemon must idle before the first refresh sweep after start-up\n\
             # (0..=3600); anchored absolutely (#260), then sweeps recur on cadence.\n",
        );
        out.push_str(&format!("idle_after_secs = {}\n", r.idle_after_secs));
        out.push_str(
            "# Seconds bounding one account's whole refresh cycle (10..=600); a slower\n\
             # cycle is cancelled and reported (non-fatal). Keep above the ~40s spawn.\n",
        );
        out.push_str(&format!("timeout_secs = {}\n", r.timeout_secs));
        out.push_str(
            "# Consecutive refresh sweeps failing with error across ALL eligible accounts before\n\
             # the daemon flags a SYSTEMIC refresh-mechanism failure (1..=100) — a mechanism-down\n\
             # signal (event + `status` indicator) distinct from per-account at-risk.\n",
        );
        out.push_str(&format!("systemic_failure_n = {}\n", r.systemic_failure_n));
        out.push_str(
            "# Pre-emptively refresh the ACTIVE account's token in place before it nears expiry\n\
             # (issue #468). OFF by default: this rotates the live shared credential every cadence,\n\
             # and the active account is instead kept warm reactively (on a real 401) and recovered\n\
             # by autonomous adopt-target. Set true to restore the pre-emptive mint. Only takes\n\
             # effect when enabled = true. See docs/findings/0476-keep-warm-scrub-risk-tradeoff.md.\n",
        );
        out.push_str(&format!(
            "proactive_keep_warm = {}\n",
            r.proactive_keep_warm
        ));
        out.push_str(
            "# The `claude` binary to spawn, overriding $CLAUDE_BIN and your login shell's\n\
             # PATH. Omit (or leave empty) to resolve from $CLAUDE_BIN then that PATH.\n",
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
            "# The `claude` binary to spawn, overriding $CLAUDE_BIN and your login shell's PATH.\n\
             # Omit (or leave empty) to resolve from $CLAUDE_BIN then that PATH.\n",
        );
        match &l.claude_bin {
            Some(bin) => out.push_str(&format!(
                "claude_bin = {}\n",
                basic_string(&bin.to_string_lossy())
            )),
            None => out.push_str("# claude_bin = \"/absolute/path/to/claude\"\n"),
        }

        // The usage-stats subsystem (issue #161): retention horizons the daemon threads into
        // the sample store's compaction, plus the offline `stats` verb's default period. The
        // next block ([migration], #150) renders after this one, before [[account]].
        let s = &self.stats;
        out.push_str("\n[stats]\n");
        out.push_str(
            "# The usage-stats store: the daemon records one sample per poll and periodically\n\
             # rolls aged raw samples down into hourly then daily aggregates. These horizons bound\n\
             # each tier; the `stats` verb reads the store offline.\n",
        );
        out.push_str(
            "# Seconds a raw per-poll sample is kept before its whole aged-out day is folded into\n\
             # the aggregates (3600..=31536000, i.e. 1h..365d).\n",
        );
        out.push_str(&format!("raw_retention_secs = {}\n", s.raw_retention_secs));
        out.push_str(
            "# Seconds an hourly-aggregate bucket is kept before it is pruned\n\
             # (86400..=315360000, i.e. 1d..10y).\n",
        );
        out.push_str(&format!(
            "hourly_retention_secs = {}\n",
            s.hourly_retention_secs
        ));
        out.push_str(
            "# Seconds a daily-aggregate bucket is kept, or 0 for lifetime (0..=315360000). The\n\
             # daily tier is kept for the store's lifetime by default; set non-zero to bound it.\n",
        );
        out.push_str(&format!(
            "daily_retention_secs = {}\n",
            s.daily_retention_secs
        ));
        out.push_str(
            "# Default `stats` reporting period when --period/--since are omitted:\n\
             # day | week | month | lifetime.\n",
        );
        out.push_str(&format!(
            "default_period = {}\n",
            basic_string(&s.default_period)
        ));

        // The migration subsystem (issue #150): the Argon2id KDF cost `export` writes an
        // encrypted artifact at, and the default `import` conflict policy. Renders after
        // [stats], before [credential] + [[account]].
        let mi = &self.migration;
        out.push_str("\n[migration]\n");
        out.push_str(
            "# Defaults for `export` / `import`. The KDF cost is recorded IN each encrypted\n\
             # artifact, so changing it never breaks reading a file already written.\n",
        );
        out.push_str(
            "# Argon2id memory cost in KiB when `export` encrypts an artifact (8..=1048576,\n\
             # i.e. 8KiB..1GiB). Higher resists offline brute-force harder, at more time and\n\
             # memory to encrypt AND decrypt.\n",
        );
        out.push_str(&format!("kdf_memory_kib = {}\n", mi.kdf_memory_kib));
        out.push_str(
            "# Argon2id time cost in iterations when `export` encrypts an artifact (1..=16).\n",
        );
        out.push_str(&format!("kdf_iterations = {}\n", mi.kdf_iterations));
        out.push_str(
            "# Default `import` conflict policy when --overwrite is omitted: skip (leave an\n\
             # account already on the target untouched) | overwrite (replace it). --overwrite\n\
             # on the command line always forces overwrite.\n",
        );
        out.push_str(&format!(
            "conflict_policy = {}\n",
            basic_string(mi.conflict_policy.as_str())
        ));

        // Credential continuity (issue #878): how far ahead to watch each account's REFRESH-token
        // deadline. Renders after [migration], before [[account]] — the last tunables block.
        let cr = &self.credential;
        out.push_str("\n[credential]\n");
        out.push_str(
            "# Foresight over your accounts' REFRESH tokens. A refresh token has its own FIXED\n\
             # expiry that refreshing does NOT slide forward; once it lapses, only `sessiometer\n\
             # login` (a `claude /login`) can recover the account. The daemon reads that deadline\n\
             # from the credential itself and warns ahead of it.\n",
        );
        out.push_str(
            "# Seconds of lookahead (86400..=7776000, i.e. 1d..90d): a refresh token expiring\n\
             # within this window is flagged while it is still working, so you can re-login on\n\
             # your schedule instead of discovering it after a refresh has already failed. There\n\
             # is no `off` — a zero window would only report credentials that had ALREADY lapsed.\n",
        );
        out.push_str(&format!(
            "expiry_horizon_secs = {}\n",
            cr.expiry_horizon_secs
        ));
        out.push_str(
            "# Seconds of spread (60..=604800, i.e. 1m..7d) still counted as ONE synchronized\n\
             # cohort. Accounts captured in a single sitting tend to expire together, so the pool\n\
             # can lose several members inside one span — a fleet-level fact no single row shows.\n\
             # Widening this past a token's own lifetime would put every account in one group and\n\
             # say nothing.\n",
        );
        out.push_str(&format!(
            "expiry_cohort_window_secs = {}\n",
            cr.expiry_cohort_window_secs
        ));

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

/// Render an optional `claude_bin` override for the `config show` origin view
/// (issue #401): the quoted path when set, or a `(unset)` sentinel when it defers
/// to `$CLAUDE_BIN` / the harvested login-shell PATH. Diagnostic-only — this view never
/// round-trips to a file, so an absent override reads as a clear sentinel rather than a blank.
fn render_optional_bin(bin: &Option<PathBuf>) -> String {
    match bin {
        Some(path) => basic_string(&path.to_string_lossy()),
        None => "(unset)".to_string(),
    }
}

/// Render a list of strings as a single-line TOML array of basic strings, e.g.
/// `["work", "spare"]` (issue #105 `[refresh].accounts`). Each element goes through
/// [`basic_string`], so labels/uuids needing escapes round-trip; an empty list renders
/// `[]`.
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
///
/// Delegated to `toml_writer` (issue #403, refining ADR-0005). The *emitter* stays
/// hand-written — it interleaves doc-comments a serializer would drop — but the
/// escaping itself is a spec'd grammar (`basic-unescaped`), and `toml_writer` is the
/// reference implementation, already compiled as a dependency of `toml`. It supersedes a
/// hand-rolled `match` that had to re-derive which C0 controls take `\uXXXX` and that
/// non-ASCII stays literal.
///
/// `as_basic()` always quotes with `"` (never a literal `'…'` string), which keeps the
/// output shape identical to the hand-rolled emitter's. Pinned by
/// `basic_string_escapes_specials` and `rendered_strings_round_trip_through_the_parser`,
/// both written against the old implementation and re-run unchanged against this one.
pub(super) fn basic_string(s: &str) -> String {
    TomlStringBuilder::new(s).as_basic().to_toml_value()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::*;

    #[test]
    fn rendered_default_config_documents_target_max_session_usage_as_a_live_value() {
        // #398: render emits a LIVE target_max_session_usage line (default-on) that round-trips
        // back to the same value — never a commented-out opt-in.
        let mut config = Config::parse(VALID).unwrap();
        config.tunables.target_max_session_usage = DEFAULT_TARGET_MAX_SESSION_USAGE;
        let text = config.render();
        assert!(text.contains("target_max_session_usage = 80"), "got {text}");
        assert!(!text.contains("# target_max_session_usage ="), "got {text}");
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(
            reparsed.tunables.target_max_session_usage,
            DEFAULT_TARGET_MAX_SESSION_USAGE
        );
    }

    #[test]
    fn rendered_config_documents_the_cooldown_floor_on_one_clean_line() {
        // Operator-facing (#272): the generated `config.toml` cooldown comment states
        // the non-zero floor range and is a single, cleanly-joined `#` line — a guard
        // that the source line-continuation did not leave a torn double-space.
        let text = Config::parse(VALID).unwrap().render();
        let comment = text
            .lines()
            .find(|l| l.contains("Seconds to wait after a swap"))
            .expect("the cooldown comment must be rendered");
        assert!(
            comment.starts_with("# ") && !comment.contains("  "),
            "cooldown comment must be one clean line, got: {comment:?}"
        );
        assert!(
            comment.contains(&format!("{COOLDOWN_SECS_FLOOR}..=3600")),
            "cooldown comment must document the floor range, got: {comment:?}"
        );
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
        // …and the (default) [migration] settings (issue #150).
        assert_eq!(original.migration, reparsed.migration);
    }

    #[test]
    fn refresh_proactive_keep_warm_opt_in_parses_and_round_trips() {
        // An operator restores the pre-#468 pre-emptive mint (finding #476 fallback A's base) with
        // an explicit `proactive_keep_warm = true`; a present key is never overridden by the
        // off-by-default serde default, and the opt-in survives the render->parse round trip.
        let toml = format!("{VALID}\n[refresh]\nenabled = true\nproactive_keep_warm = true\n");
        let config = Config::parse(&toml).unwrap();
        assert!(config.refresh.proactive_keep_warm);
        let reparsed = Config::parse(&config.render()).unwrap();
        assert!(
            reparsed.refresh.proactive_keep_warm,
            "the opt-in survives emit->parse (#468)"
        );
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
    fn rendered_default_refresh_is_on_with_commented_claude_bin() {
        // The rendered default [refresh] block is enabled (#409) and leaves claude_bin commented
        // (so a fresh `capture` writes a self-documenting, on-by-default block) yet round-trips.
        let config = Config::parse(VALID).unwrap();
        let text = config.render();
        assert!(
            text.contains("[refresh]"),
            "render must emit [refresh]: {text}"
        );
        assert!(
            text.contains("enabled = true"),
            "default refresh must render enabled: {text}"
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

    /// Issue #878: a customised `[credential]` block survives render → parse, and the rendered
    /// DEFAULT block round-trips to the default. Without this, `sessiometer capture` (which
    /// rewrites the whole file) would silently drop an operator's horizon back to seven days.
    #[test]
    fn credential_round_trips_render_then_parse() {
        let toml = format!("{VALID}\n[credential]\nexpiry_horizon_secs = 1209600\n");
        let original = Config::parse(&toml).unwrap();
        let reparsed = Config::parse(&original.render()).unwrap();
        assert_eq!(original.credential, reparsed.credential);
        assert_eq!(reparsed.credential.expiry_horizon_secs, 1_209_600);

        // The rendered default block is emitted, documented, and reparses to the default.
        let default = Config::parse(VALID).unwrap();
        let text = default.render();
        assert!(
            text.contains("[credential]"),
            "render must emit [credential]: {text}"
        );
        assert!(
            text.contains("expiry_horizon_secs = 604800"),
            "default credential block must render the 7d horizon: {text}"
        );
        assert_eq!(
            Config::parse(&text).unwrap().credential,
            CredentialConfig::default()
        );
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

    #[test]
    fn round_trips_a_configured_jitter_table() {
        let toml = with_jitter(
            "poll = { kind = \"uniform\", spread = 12.5 }\n\
             session_ceiling = { kind = \"normal\", stddev = 1.5 }\n\
             weekly_ceiling = { kind = \"uniform\", spread = 0.5 }\n\
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
        for key in ["poll", "session_ceiling", "weekly_ceiling", "cooldown"] {
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
        // the target_max_session_usage "most-full a target may be to receive the session" semantics.
        assert!(text.contains("The most-full an account may be to receive"));
        for key in [
            "poll_secs",
            "exhausted_poll_secs",
            "near_limit_poll_secs",
            "cooldown_secs",
            "target_max_session_usage",
            "session_ceiling",
            "weekly_ceiling",
            "session_velocity_horizon_secs",
            "session_velocity_min_project_above",
            "session_velocity_ema_alpha_pct",
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

    /// Pins the full escape surface of [`basic_string`], not just the common cases.
    ///
    /// Written to characterize the hand-rolled emitter BEFORE #403 delegated it to
    /// `toml_writer`, then re-run unchanged against the delegated one: an identical
    /// pass across every escape class is the empirical evidence that the swap is
    /// behavior-preserving. Do not thin it out — each arm below is a distinct branch
    /// of the TOML `basic-unescaped` grammar.
    #[test]
    fn basic_string_escapes_specials() {
        assert_eq!(basic_string("plain"), "\"plain\"");
        assert_eq!(basic_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(basic_string("tab\tnl\n"), "\"tab\\tnl\\n\"");
        assert_eq!(basic_string("\u{0}"), "\"\\u0000\"");

        // The named escapes TOML defines, each on its own.
        assert_eq!(basic_string("\u{08}"), "\"\\b\"");
        assert_eq!(basic_string("\u{0c}"), "\"\\f\"");
        assert_eq!(basic_string("\r"), "\"\\r\"");

        // Remaining C0 controls and DEL take the \uXXXX form, upper-case hex.
        assert_eq!(basic_string("\u{1}"), "\"\\u0001\"");
        assert_eq!(basic_string("\u{1f}"), "\"\\u001F\"");
        assert_eq!(basic_string("\u{7f}"), "\"\\u007F\"");

        // Non-ASCII is valid literally in a basic string — never escaped. This is the
        // arm an operator's label most plausibly exercises (issue #176 wide glyphs).
        assert_eq!(basic_string("café"), "\"café\"");
        assert_eq!(basic_string("работа"), "\"работа\"");
        assert_eq!(basic_string("🟢 work"), "\"🟢 work\"");

        // Space and `'` stay literal; only `"` and `\` are structural.
        assert_eq!(basic_string("a b 'c'"), "\"a b 'c'\"");

        // Empty renders as an empty basic string, not a bare pair of nothing.
        assert_eq!(basic_string(""), "\"\"");
    }

    /// Every string [`Config::render`] emits must survive a render → parse round-trip.
    /// Guards the #403 delegation at the level that actually matters: the emitted file
    /// re-parses to the same values, for the whole escape surface at once.
    ///
    /// The empty string is deliberately absent: `""` escapes fine (pinned above) but
    /// `validate` rejects an empty `label` outright, which is a roster invariant, not an
    /// escaping property.
    #[test]
    fn rendered_strings_round_trip_through_the_parser() {
        for label in [
            "plain",
            "a\"b\\c",
            "tab\there",
            "nl\nhere",
            "cr\rhere",
            "\u{08}\u{0c}",
            "\u{0}\u{1f}\u{7f}",
            "café ☕",
            "🟢 work",
        ] {
            let rendered = basic_string(label);
            let toml = format!("[[account]]\naccount_uuid = \"u\"\nlabel = {rendered}\n");
            let config = Config::parse(&toml)
                .unwrap_or_else(|e| panic!("{label:?} rendered as {rendered} must parse: {e}"));
            assert_eq!(
                config.roster[0].label, label,
                "{label:?} must survive render -> parse unchanged"
            );
        }
    }

    // --- config show --origin (issue #401) ---------------------------------

    /// The provenance test #401 exists for: a file that sets ONLY `session_ceiling`
    /// must show that one key `FromFile` and EVERY other tunable — plus every absent
    /// optional section — `Default`, so a silently-defaulted (absent) block is visible.
    #[test]
    fn origin_report_tags_absent_keys_default_and_present_keys_from_file() {
        let text = "[tunables]\nsession_ceiling = 90\n";
        let config = Config::from_toml_str(text).expect("a lone session_ceiling is valid");
        let table: toml::Table = toml::from_str(text).expect("valid TOML");
        let report = config.origin_report(&table);

        let tunables = &report.sections[0];
        assert_eq!(tunables.header, "[tunables]");
        assert!(tunables.present, "[tunables] is present");
        let by_key = |k: &str| {
            tunables
                .entries
                .iter()
                .find(|e| e.key == k)
                .unwrap_or_else(|| panic!("no `{k}` entry"))
        };
        assert_eq!(by_key("session_ceiling").origin, Origin::FromFile);
        assert_eq!(by_key("session_ceiling").value, "90");
        // Every OTHER tunable in the present section is still a compiled-in default.
        assert_eq!(by_key("poll_secs").origin, Origin::Default);
        assert_eq!(by_key("target_max_session_usage").origin, Origin::Default);
        assert_eq!(by_key("monitor_401_n").origin, Origin::Default);

        // Every optional section is absent → not present, all values Default.
        for header in [
            "[jitter]",
            "[refresh]",
            "[login]",
            "[stats]",
            "[migration]",
            "[credential]",
        ] {
            let section = report
                .sections
                .iter()
                .find(|s| s.header == header)
                .unwrap_or_else(|| panic!("no `{header}` section"));
            assert!(!section.present, "{header} is absent");
            assert!(
                section.entries.iter().all(|e| e.origin == Origin::Default),
                "{header} keys are all Default when the section is absent",
            );
        }
        assert_eq!(report.roster_count, 0);
        assert!(!report.roster_present, "no [[account]] in the file");
    }

    /// Keys and sections PRESENT in the file read `FromFile`; a key omitted from an
    /// otherwise-present section still reads `Default`; a populated roster is counted
    /// and flagged present.
    #[test]
    fn origin_report_marks_present_sections_keys_and_roster_from_file() {
        let text = "\
[tunables]
poll_secs = 45

[refresh]
enabled = true

[[account]]
account_uuid = \"11111111-1111\"
label = \"work\"
";
        let config = Config::from_toml_str(text).expect("valid config");
        let table: toml::Table = toml::from_str(text).expect("valid TOML");
        let report = config.origin_report(&table);

        let tunables = report
            .sections
            .iter()
            .find(|s| s.header == "[tunables]")
            .unwrap();
        let poll = tunables
            .entries
            .iter()
            .find(|e| e.key == "poll_secs")
            .unwrap();
        assert_eq!(poll.origin, Origin::FromFile);
        assert_eq!(poll.value, "45");
        // Present section, absent key → still Default.
        let cooldown = tunables
            .entries
            .iter()
            .find(|e| e.key == "cooldown_secs")
            .unwrap();
        assert_eq!(cooldown.origin, Origin::Default);

        let refresh = report
            .sections
            .iter()
            .find(|s| s.header == "[refresh]")
            .unwrap();
        assert!(refresh.present);
        let enabled = refresh.entries.iter().find(|e| e.key == "enabled").unwrap();
        assert_eq!(enabled.origin, Origin::FromFile);
        assert_eq!(enabled.value, "true");

        assert_eq!(report.roster_count, 1);
        assert!(report.roster_present);
    }

    /// #401 drift guard, the complement of `..._all_from_file` above: every key `render`
    /// writes for a full config MUST also appear in `origin_report`. Without this, a tunable
    /// added to `render` but forgotten in `origin_report` would be silently DROPPED from
    /// `config show` — the drift most likely as the schema grows (jitter #38, refresh #105,
    /// stats #161, migration #150, target_max_session_usage #398). Asserts `live ⊆ reported`.
    #[test]
    fn origin_report_reports_every_key_render_writes() {
        let config = Config::parse(VALID).unwrap();
        let table: toml::Table = toml::from_str(&config.render()).unwrap();
        let report = config.origin_report(&table);
        for (name, live) in &table {
            // The `[[account]]` roster is summarized, not key-listed — skip the array.
            let Some(live) = live.as_table() else {
                continue;
            };
            let want = format!("[{name}]");
            let section = report
                .sections
                .iter()
                .find(|s| s.header == want.as_str())
                .unwrap_or_else(|| panic!("render writes {want} but origin_report has no section"));
            let reported: std::collections::BTreeSet<&str> =
                section.entries.iter().map(|e| e.key).collect();
            for key in live.keys() {
                assert!(
                    reported.contains(key.as_str()),
                    "render writes {name}.{key} but origin_report omits it — config show would drop it",
                );
            }
        }
    }

    // --- the dedicated config-write lock (issue #1445, design D-8) -------------------

    /// A roster of `accounts` accounts, as a parsed [`Config`] — the size doubles as the
    /// writer's identity, so a lost update is visible as a missing number rather than as a
    /// byte diff.
    fn roster_of(accounts: usize) -> Config {
        let mut out = String::new();
        for n in 0..accounts {
            out.push_str(&format!(
                "[[account]]\naccount_uuid = \"u-{n}\"\nlabel = \"a{n}\"\n\n"
            ));
        }
        Config::from_toml_str(&out).expect("the fixture roster parses")
    }

    /// AC-5, and the DETERMINISTIC half of AC-2: `save_to` actually takes the config-write lock,
    /// a busy lock is REPORTED rather than silently skipped, and the refusal is a true no-op.
    ///
    /// This is the test that fails against the pre-change code, and it fails for the right
    /// reason: without the lock `save_to` completes immediately whatever anyone else is holding,
    /// so the `timeout` below returns `Ok` and the assertion that it was BLOCKED is what breaks.
    /// Scope, because "the proof" would overclaim: this proves the lock is TAKEN and that a
    /// contended writer neither proceeds nor writes. It does NOT prove the guard is HELD across
    /// the critical section — releasing it immediately after acquiring would pass every
    /// assertion here, since the acquire still blocks. That span is pinned structurally by
    /// `the_config_write_lock_guard_is_held_across_the_critical_section` below.
    ///
    /// It also demonstrates the no-deadlock property AC-5 asks for from the other side: the wait
    /// is BOUNDED, so a writer facing a daemon that holds the lock gets an error, never a hang.
    #[tokio::test]
    async fn save_to_blocks_on_a_held_config_write_lock_and_writes_nothing_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Fill the ring first. One entry against a depth of three leaves the eviction half of
        // the assertion below unfalsifiable — `prune`'s `skip(RING_DEPTH)` removes nothing from
        // a short ring whatever the implementation does.
        for accounts in 1..=(crate::roster_backup::RING_DEPTH + 1) {
            roster_of(accounts).save_to(&path).await.unwrap();
        }
        let before = std::fs::read_to_string(&path).unwrap();
        let ring_before: Vec<_> = crate::roster_backup::list(&path)
            .unwrap()
            .iter()
            .map(|e| e.accounts)
            .collect();
        assert_eq!(
            ring_before.len(),
            crate::roster_backup::RING_DEPTH,
            "the ring must be FULL before the blocked write, or `prune` cannot evict and the \
             no-eviction half of this test asserts nothing"
        );

        // Stand in for the other writer — a second CLI invocation, or the daemon's `config set`
        // handler — holding the lock across its own read-modify-write.
        let held = super::write_lock::ConfigWriteLock::acquire(
            &super::write_lock::lock_path(&path),
            std::time::Duration::from_millis(50),
        )
        .await
        .expect("the stand-in writer takes the lock");

        // Far shorter than `CONFIG_WRITE_LOCK_MAX_WAIT`, so this observes the BLOCK without
        // waiting out the production budget.
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            roster_of(9).save_to(&path),
        )
        .await;
        assert!(
            blocked.is_err(),
            "`save_to` completed while another writer held the config-write lock — it is not \
             taking the lock at all, so two writers can still interleave"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "a writer waiting on the lock has written nothing — the refusal is a true no-op"
        );
        let ring_after: Vec<_> = crate::roster_backup::list(&path)
            .unwrap()
            .iter()
            .map(|e| e.accounts)
            .collect();
        assert_eq!(
            ring_after, ring_before,
            "nor has it retained or evicted anything: the lock is acquired BEFORE the ring is \
             touched, which is what makes the ring's read-modify-write part of the critical \
             section"
        );

        // And it recovers: the lock is per-write, not per-process.
        drop(held);
        roster_of(9).save_to(&path).await.unwrap();
        assert_eq!(Config::load_path(&path).unwrap().roster.len(), 9);
    }

    /// AC-5's REPORTED half, observed as a returned value rather than as a cancellation: a
    /// writer that cannot take the lock within its budget gets [`Error::ConfigWriteLockBusy`]
    /// back OUT OF `save_to`, and the exit code an operator sees is the shared "busy, retry
    /// shortly" one.
    ///
    /// The two `timeout`-based tests either side of this one cannot establish this — a cancelled
    /// future returns nothing at all, so "did not finish" is all they can observe, and it is
    /// satisfied by any hang whatsoever.
    #[tokio::test]
    async fn a_held_lock_makes_save_to_return_config_write_lock_busy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        roster_of(2).save_to(&path).await.unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let held = super::write_lock::ConfigWriteLock::acquire(
            &super::write_lock::lock_path(&path),
            std::time::Duration::from_millis(50),
        )
        .await
        .expect("the stand-in writer takes the lock");

        let err = roster_of(9)
            .save_to_within(&path, std::time::Duration::from_millis(100))
            .await
            .expect_err("a save whose whole budget elapses under a held lock must fail closed");
        assert!(
            matches!(err, crate::error::Error::ConfigWriteLockBusy),
            "a busy config-write lock must surface as its OWN variant, not as an I/O error or a \
             swap-lock one — an operator reading the message has to be told which lock is busy; \
             got {err:?}"
        );
        assert_eq!(
            err.exit_code(),
            4,
            "a busy lock is the transient retry-shortly class, sharing an exit code with \
             `SwapLockBusy` and `UsageStoreBusy` so a script can branch on it"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "the refusal wrote nothing"
        );
        drop(held);
    }

    /// The SPAN of the config-write lock: the guard binds to a live name and every effect of the
    /// critical section happens after it.
    ///
    /// Structural on purpose. The mutation this exists to catch is `let _write_lock = …` becoming
    /// `let _ = …`, which drops the guard at the end of the statement while leaving the acquire —
    /// and therefore every runtime assertion about a contended writer BLOCKING — intact. No test
    /// that observes `save_to` from outside can separate those two programs.
    #[test]
    fn the_config_write_lock_guard_is_held_across_the_critical_section() {
        let source = std::fs::read_to_string("src/config/render.rs")
            .expect("cannot read src/config/render.rs");
        let from = source
            .find("async fn save_to_within(")
            .expect("`save_to_within` has been renamed — this gate's anchor has gone stale");
        let body = &source[from..][..source[from..]
            .find("\n    }\n")
            .expect("`save_to_within` has no closing brace at its own indentation")];

        let acquire = body
            .find("ConfigWriteLock::acquire(")
            .expect("`save_to_within` no longer acquires the config-write lock");
        let binding = body[..acquire]
            .rfind("let ")
            .expect("the acquire is not bound by a `let` at all");
        // The binding's NAME: everything between `let ` and the `=` opening the initializer.
        let bound_to = body[binding + 4..acquire]
            .split_once('=')
            .expect("a `let` binding the acquire always has an `=`")
            .0
            .trim();
        assert!(
            bound_to != "_",
            "the config-write lock is bound to `_`, which drops the guard at the end of its own \
             statement — the acquire still blocks a contended writer, so every runtime assertion \
             in this module still passes while the critical section runs UNLOCKED"
        );

        for effect in [
            "retain_if_qualifying(",
            "write_private_file(",
            "retention.commit()",
        ] {
            let at = body
                .find(effect)
                .unwrap_or_else(|| panic!("`{effect}` is no longer in `save_to_within`"));
            assert!(
                acquire < at,
                "`{effect}` runs BEFORE the config-write lock is acquired, so a refusal is no \
                 longer a true no-op"
            );
        }
    }

    /// AC-2 under REAL concurrency: several writers race one `config.toml` from separate OS
    /// threads (the closest in-process model of the separate PROCESSES the defect is about —
    /// `flock` contends across open file descriptions either way), while a reader parses the live
    /// file continuously.
    ///
    /// Three properties, which are AC-2's two clauses plus the ring accounting that makes "no
    /// lost update" mean something at this seam:
    ///
    /// - **No partial file is ever rendered live.** The reader only ever observes a config that
    ///   parses. Pre-change this is reachable because the writers share one staging name and
    ///   `write_private_file` opens it by unlinking: an unlink landing between the winner's
    ///   `fsync` and its `rename` publishes the LOSER's half-written file.
    /// - **No writer's output vanishes.** Every writer's roster is accounted for in live ∪ ring
    ///   — one live, the rest retained by whoever replaced them. Say it that way rather than "no
    ///   update is lost": this is RING accounting, and it is not the write-write sense, in which
    ///   a caller whose read-modify-write straddles another's publish does still lose its change
    ///   (issue #1482). What it does catch is a writer whose output never reached disk at all,
    ///   and two writers retaining the SAME displaced roster — the duplicate-plus-missing pair a
    ///   broken lock produces. `WRITERS` is held at [`RING_DEPTH`] so the ring can hold every
    ///   displaced entry and eviction never confounds the accounting.
    /// - **Exactly one writer's output is live**, complete and valid (AC-8 verbatim).
    ///
    /// Being a race, a green run is EVIDENCE and not proof — the deterministic proof that the
    /// lock is wired at all is the test above. This one is what would catch a lock that is taken
    /// but does not actually cover the ring's read-modify-write.
    #[test]
    fn concurrent_writers_publish_one_complete_config_and_none_of_their_output_vanishes() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Barrier};

        const WRITERS: usize = crate::roster_backup::RING_DEPTH;
        // Distinct, non-zero roster sizes: each writer's own signature in the accounting below.
        let sizes: Vec<usize> = (1..=WRITERS).collect();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let stop = Arc::new(AtomicBool::new(false));
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reader = {
            let path = path.clone();
            let stop = Arc::clone(&stop);
            let reads = Arc::clone(&reads);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    // An absent file is legitimate only before the first write lands; a file that
                    // EXISTS must always parse. A torn publish fails here.
                    match Config::load_path(&path) {
                        Ok(config) => {
                            assert!(
                                !config.roster.is_empty(),
                                "a live config with an empty roster is a torn or truncated \
                                 publish — every writer here writes a non-empty one"
                            );
                            reads.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(crate::error::Error::ConfigNotFound { .. }) => {}
                        Err(err) => panic!("the live config did not parse: {err}"),
                    }
                    // Bounded, not a busy spin: an unthrottled read+parse loop burns a core
                    // against the writers, and on a low-core runner that is what could push one
                    // of them past its lock budget into a spurious failure.
                    std::thread::sleep(std::time::Duration::from_micros(200));
                }
            })
        };

        // Force the race rather than hope for it. Without this the writers are only *likely* to
        // overlap — thread 1 can finish before thread 3's runtime is even built, and that
        // degenerate sequential run satisfies every assertion below having tested nothing about
        // concurrency. So: the main thread takes the lock first, every writer parks at the
        // barrier with its runtime already built, and the lock is released only once they are
        // all parked. Each writer therefore STARTS blocked, and its own elapsed time proves it.
        const HOLD: std::time::Duration = std::time::Duration::from_millis(150);
        let gate = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                super::write_lock::ConfigWriteLock::acquire(
                    &super::write_lock::lock_path(&path),
                    std::time::Duration::from_millis(50),
                )
                .await
            })
            .expect("the gate takes the lock on an uncontended file");

        let barrier = Arc::new(Barrier::new(WRITERS + 1));
        let writers: Vec<_> = sizes
            .iter()
            .copied()
            .map(|accounts| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                // One runtime per thread: genuinely parallel acquires, not a single-threaded
                // interleave that would serialize by accident and pass without a lock.
                std::thread::spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    barrier.wait();
                    let began = std::time::Instant::now();
                    runtime
                        .block_on(async { roster_of(accounts).save_to(&path).await })
                        .unwrap_or_else(|err| panic!("writer {accounts} failed: {err}"));
                    (accounts, began.elapsed())
                })
            })
            .collect();
        barrier.wait();
        std::thread::sleep(HOLD);
        drop(gate);

        for w in writers {
            let (accounts, took) = w.join().unwrap();
            assert!(
                took >= HOLD,
                "writer {accounts} finished in {took:?}, faster than the {HOLD:?} the gate held \
                 the lock for — it did not wait on the lock, so this run never exercised \
                 contention and every assertion below passed vacuously"
            );
        }
        stop.store(true, Ordering::Relaxed);
        reader.join().unwrap();
        assert!(
            reads.load(Ordering::Relaxed) > 0,
            "the reader never observed the file at all — the no-torn-read assertion above ran \
             against nothing"
        );

        // Exactly one writer's complete, valid output is live (AC-8).
        let live = Config::load_path(&path).unwrap().roster.len();
        assert!(
            sizes.contains(&live),
            "the live roster holds {live} accounts, which no writer wrote — the file is a blend \
             of two writers' output"
        );

        // And no update was lost: the live roster plus the ring account for every writer.
        let mut seen: Vec<usize> = crate::roster_backup::list(&path)
            .unwrap()
            .iter()
            .map(|e| e.accounts.expect("every retained entry still parses"))
            .collect();
        seen.push(live);
        seen.sort_unstable();
        assert_eq!(
            seen, sizes,
            "every writer's roster should be accounted for — one live, the rest retained by \
             whoever replaced them; a missing size is an update that vanished"
        );
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

    // --- [stats] block (issue #161) -----------------------------------------

    #[test]
    fn rendered_default_config_round_trips_the_stats_block() {
        // The rendered default config carries a `[stats]` block that reparses to the same
        // settings — the render → parse round-trip the other blocks hold to.
        let config = Config::parse(VALID).unwrap();
        let text = config.render();
        assert!(text.contains("[stats]"), "render must emit [stats]: {text}");
        assert!(
            text.contains("raw_retention_secs ="),
            "render must document raw retention: {text}"
        );
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(reparsed.stats, config.stats);
    }

    #[test]
    fn rendered_stats_round_trips_operator_overrides() {
        // Operator-set non-defaults survive render → parse unchanged (defaults + overrides,
        // the issue's round-trip AC).
        let mut config = Config::parse(VALID).unwrap();
        config.stats = StatsConfig {
            raw_retention_secs: 3_600,          // the lower bound
            hourly_retention_secs: 315_360_000, // the upper bound
            daily_retention_secs: 7_776_000,
            default_period: "day".to_owned(),
        };
        let text = config.render();
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(reparsed.stats, config.stats);
    }

    // --- [migration] block (issue #150) -------------------------------------

    #[test]
    fn rendered_default_config_round_trips_the_migration_block() {
        // The rendered default config carries a `[migration]` block that reparses to the same
        // settings — the render → parse round-trip the other blocks hold to.
        let config = Config::parse(VALID).unwrap();
        let text = config.render();
        assert!(
            text.contains("[migration]"),
            "render must emit [migration]: {text}"
        );
        assert!(
            text.contains("kdf_memory_kib ="),
            "render must document the KDF cost: {text}"
        );
        assert!(
            text.contains("conflict_policy ="),
            "render must document the conflict policy: {text}"
        );
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(reparsed.migration, config.migration);
    }

    #[test]
    fn rendered_migration_round_trips_operator_overrides() {
        // Operator-set non-defaults survive render → parse unchanged (defaults + overrides, the
        // issue's round-trip AC). Uses the exact range bounds to prove they render/reparse.
        let mut config = Config::parse(VALID).unwrap();
        config.migration = MigrationConfig {
            kdf_memory_kib: 1_048_576, // the upper bound
            kdf_iterations: 16,        // the upper bound
            conflict_policy: ConflictPolicy::Overwrite,
        };
        let text = config.render();
        let reparsed = Config::parse(&text).unwrap();
        assert_eq!(reparsed.migration, config.migration);

        // …and the lower bounds round-trip too.
        config.migration = MigrationConfig {
            kdf_memory_kib: 8,
            kdf_iterations: 1,
            conflict_policy: ConflictPolicy::Skip,
        };
        let reparsed = Config::parse(&config.render()).unwrap();
        assert_eq!(reparsed.migration, config.migration);
    }
}
