// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Getting a [`Config`] IN: the file/text ingest seam and the two-stage parse entry point
//! (issue #638's per-concern decomposition of the one 1,253-line `impl Config`).
//!
//! The four doors this module owns — the standard-path load, the explicit-path seam, the
//! migration-artifact text seam, and the `config show --origin` diagnostic — all funnel
//! through [`Config::parse`], so a malformed file is rejected identically whichever one it
//! came through. `parse` is the STAGING half only: it deserializes into the permissive raw
//! form and hands the bounds off to [`Config::validate`] — which is the gate EVERY path
//! crosses, this module's four included. The `config-set` edit ([`super::settings`]) is the
//! one path that only half-crosses: it takes its BEFORE baseline through `parse` here, then
//! re-enters `validate` directly with its overlaid raw layer. So `validate`, not `parse`, is
//! where an invariant belongs.

use super::*;

impl Config {
    /// Load and validate `config.toml` from its standard path.
    ///
    /// Returns [`Error::ConfigNotFound`] if the file is absent (the daemon has
    /// nothing to run until `capture` writes one), [`Error::ConfigParse`] /
    /// [`Error::ConfigInvalid`] / [`Error::ConfigTargetMaxSessionAboveTrigger`] for a file
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

    /// Parse and validate a config from its rendered TOML TEXT (not a file) — the seam
    /// the `import` verb (issue #149) uses to read the roster + tunables carried verbatim
    /// inside a migration artifact's `config_toml` ([`crate::migration::Payload::config_toml`]).
    /// Mirrors [`load_path`](Config::load_path) minus the file read, funnelling through the
    /// same [`parse`](Config::parse) validation so an artifact's config is held to the
    /// identical invariants (unique non-empty `account_uuid`, tunable ranges).
    pub(crate) fn from_toml_str(text: &str) -> Result<Self> {
        Self::parse(text)
    }

    /// Load the effective config AND classify every value's origin (file vs default),
    /// for the read-only `config show [--origin]` diagnostics verb (issue #401).
    ///
    /// Purely additive — it changes nothing about how the daemon loads or defaults
    /// config, and every error class matches [`load`](Config::load) exactly: the file
    /// read maps [`Error::ConfigNotFound`] / [`Error::Io`] just as
    /// [`load_path`](Config::load_path) does, and the SAME [`parse`](Config::parse) →
    /// [`validate`](Config::validate) seam maps [`Error::ConfigParse`] /
    /// [`Error::ConfigInvalid`] / [`Error::ConfigTargetMaxSessionAboveTrigger`]. It then re-reads
    /// the raw text into a permissive [`toml::Table`] PURELY to detect key presence,
    /// which the typed `#[serde(default)]` layer cannot report.
    pub(crate) fn load_with_origin(path: &Path) -> Result<OriginReport> {
        // Deliberately mirrors `load_path`'s read (absent → `ConfigNotFound`, other →
        // `Io`) rather than calling it — the raw text is needed twice (typed parse +
        // presence table), and this keeps the change additive to the daemon's load path.
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Err(Error::ConfigNotFound {
                    path: path.to_path_buf(),
                });
            }
            Err(err) => return Err(Error::Io(err)),
        };
        // The effective, validated config (defaults filled). Any parse/validate error
        // surfaces here first, byte-identically to what `load_path` would return.
        let config = Self::parse(&text)?;
        // A second, permissive parse into a raw table — key presence only. `parse`
        // above already accepted `text` under `deny_unknown_fields`, so this re-parse
        // of the same input cannot fail; map defensively regardless.
        let raw: toml::Table =
            toml::from_str(&text).map_err(|err| Error::ConfigParse(err.to_string()))?;
        Ok(config.origin_report(&raw))
    }

    /// Stage one: deserialize TOML into the permissive raw form, then validate.
    /// Pure (no filesystem) so the whole parse-and-validate policy is testable
    /// without touching real paths.
    pub(super) fn parse(text: &str) -> Result<Self> {
        let raw: RawConfig =
            toml::from_str(text).map_err(|err| Error::ConfigParse(err.to_string()))?;
        Self::validate(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::*;

    #[test]
    fn parses_a_valid_config() {
        let config = Config::parse(VALID).unwrap();
        assert_eq!(config.roster.len(), 2);
        assert_eq!(
            config.tunables,
            Tunables {
                poll_secs: 30,
                // VALID omits exhausted_poll_secs → the compiled-in default (issue #537).
                exhausted_poll_secs: 3600,
                // VALID omits near_limit_poll_secs → the compiled-in default (issue #540). 60 > the
                // configured poll_secs (30) here, so it is inert for this config (min never binds) —
                // valid, not an error (no cross-field bound to poll_secs).
                near_limit_poll_secs: 60,
                cooldown_secs: 45,
                target_max_session_usage: 70,
                session_ceiling: 90,
                weekly_ceiling: 97,
                // VALID sets no blind-swap keys → the compiled-in defaults (issue #452).
                session_blind_swap_secs: 300,
                session_blind_risk_band: 60,
                // VALID sets no velocity-projection keys → the compiled-in defaults (issue #539).
                session_velocity_horizon_secs: 120,
                session_velocity_min_project_above: 85,
                session_velocity_ema_alpha_pct: 50,
                monitor_401_n: 5,
                monitor_recovery_m: 4,
                // VALID omits fleet_runway_warn_secs → the compiled-in default (issue #650):
                // 0, the proactive fleet-runway warning OFF (opt-in).
                fleet_runway_warn_secs: 0,
                // VALID omits canary_drift_override → the compiled-in default (issue #714):
                // false, the canary's drift refusal ARMED (the override is opt-in).
                canary_drift_override: false,
                // VALID omits canary_nostashmatch_override → the compiled-in default (issue
                // #730): false, the NoStashMatch shape-gate ARMED (the override is opt-in).
                canary_nostashmatch_override: false,
                // VALID omits both #736 keys → the compiled-in defaults: the Layer-3 online
                // liveness probe DISARMED (no network call on the swap path at all) and its
                // strict mode OFF (a failed probe would degrade gracefully).
                canary_online_probe: false,
                canary_online_probe_strict: false,
                // VALID omits verbose → the compiled-in default (issue #775): a managed daemon
                // stays silent on the diagnostic channel.
                verbose: false,
                // No [jitter] table in VALID → default strategies: poll jitters
                // normally (base from poll_secs), session_ceiling/weekly_ceiling/cooldown
                // are fixed at their respective bases.
                poll_strategy: Strategy {
                    base: 30.0,
                    jitter: default_poll_jitter(),
                },
                session_ceiling_strategy: Strategy::fixed(90.0),
                weekly_ceiling_strategy: Strategy::fixed(97.0),
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
    fn malformed_toml_is_a_parse_error() {
        assert!(matches!(Config::parse("]["), Err(Error::ConfigParse(_))));
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

    /// `load_with_origin` funnels through the SAME parse→validate seam as `load`, so a
    /// bad value fails identically (never a silent default) and an absent file is
    /// `ConfigNotFound` — the read-only diagnostics verb inherits the daemon's contract.
    #[test]
    fn load_with_origin_surfaces_the_same_errors_as_load_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        std::fs::write(&path, "[tunables]\npoll_secs = 1\n").unwrap();
        let err = Config::load_with_origin(&path).expect_err("poll_secs=1 is out of range");
        assert!(matches!(err, Error::ConfigInvalid(_)), "got {err:?}");

        let missing = dir.path().join("nope.toml");
        assert!(matches!(
            Config::load_with_origin(&missing),
            Err(Error::ConfigNotFound { .. })
        ));
    }

    /// End-to-end through disk. A rendered config reports every value `FromFile`
    /// (render writes every key live), so the ONLY way a tunable reads `Default` is a
    /// genuinely absent key — which is exactly why the drift #401 surfaces is real.
    #[test]
    fn load_with_origin_reports_a_rendered_config_all_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let rendered = Config::parse(VALID).unwrap().render();
        std::fs::write(&path, &rendered).unwrap();

        let report = Config::load_with_origin(&path).unwrap();
        for section in &report.sections {
            // `claude_bin` is the one key `render` leaves COMMENTED when unset, so it
            // is legitimately absent (`Default`); every other rendered key is live.
            for entry in &section.entries {
                if entry.key == "claude_bin" {
                    continue;
                }
                assert_eq!(
                    entry.origin,
                    Origin::FromFile,
                    "rendered {}.{} should read FromFile",
                    section.header,
                    entry.key,
                );
            }
        }
    }

    /// The externally-deleted-block scenario #401 names verbatim: a config that OMITS
    /// `[tunables]` entirely (but is otherwise valid) loads fine, and every tunable
    /// reads `Default` with the section flagged absent — the drift made visible.
    #[test]
    fn load_with_origin_surfaces_a_missing_tunables_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[refresh]\nenabled = true\n\n[[account]]\naccount_uuid = \"11111111-1111\"\nlabel = \"work\"\n",
        )
        .unwrap();

        let report = Config::load_with_origin(&path).unwrap();
        let tunables = report
            .sections
            .iter()
            .find(|s| s.header == "[tunables]")
            .unwrap();
        assert!(!tunables.present, "[tunables] is absent from the file");
        assert!(
            tunables.entries.iter().all(|e| e.origin == Origin::Default),
            "a missing [tunables] block reads as all-Default — the #401 drift signal",
        );
        // The present [refresh].enabled still reads FromFile — absence is per-section.
        let refresh = report
            .sections
            .iter()
            .find(|s| s.header == "[refresh]")
            .unwrap();
        assert!(refresh.present);
    }

    #[test]
    fn unknown_field_is_rejected() {
        let toml = with_tunables("poll_secs = 60\nbogus = 1");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigParse(_))));
    }

    #[test]
    fn the_pre_rename_trigger_keys_are_rejected_not_silently_defaulted() {
        // Issue #606 renamed `session_trigger` → `session_ceiling` and `weekly_trigger` →
        // `weekly_ceiling` with NO migration (maintainer decision, ADR-0023 § Alternatives 4):
        // the break is DELIBERATE, so it is pinned here rather than left to chance.
        //
        // Two things this asserts, both load-bearing. First, the failure is LOUD and
        // SELF-REMEDYING: `RawTunables` is `deny_unknown_fields`, so an old config.toml is a
        // parse error that names the offending key AND — via serde's "expected one of" field
        // list — the replacement to write. It is NOT "ignored and falls back to the default",
        // the framing issue #606's own text used. An operator who deletes the stale key gets
        // the default; one who renames it keeps their value. Second, this is the falsifier for
        // the no-back-compat decision: a later well-meaning `#[serde(alias = "session_trigger")]`
        // would silently restore back-compat, and nothing else in the suite would go red.
        // (Contrast `deprecated_aliases_parse_and_render_as_target_max_session_usage`, which
        // pins the OPPOSITE decision for the target-reserve key — the asymmetry is intentional.)
        for (stale, replacement) in [
            ("session_trigger = 90", "session_ceiling"),
            ("weekly_trigger = 95", "weekly_ceiling"),
        ] {
            let stale_key = stale.split(' ').next().expect("fragment is `key = value`");
            match Config::parse(&with_tunables(stale)) {
                Err(Error::ConfigParse(msg)) => {
                    assert!(
                        msg.contains(stale_key),
                        "the rejection must NAME the stale key, got: {msg}"
                    );
                    assert!(
                        msg.contains(replacement),
                        "…and point at `{replacement}` so the operator can rename it, got: {msg}"
                    );
                }
                other => panic!(
                    "pre-rename key `{stale}` must be rejected, never silently defaulted; got {other:?}"
                ),
            }
        }
    }

    #[test]
    fn multiple_reserve_key_spellings_present_is_a_parse_error() {
        // Mid-migration an operator might leave more than one spelling of the reserve key in
        // one file. serde maps the canonical `target_max_session_usage` and both deprecated
        // aliases (`target_max_usage` #415, `session_floor` pre-#415) onto the same field, so
        // ANY two present at once is a duplicate-field parse error rather than a silent winner
        // — the operator is told to pick one (the issue's precedence choice). Cover every
        // collision-capable pair plus all three at once.
        let collisions = [
            "session_floor = 70\ntarget_max_usage = 80",
            "session_floor = 70\ntarget_max_session_usage = 80",
            "target_max_usage = 70\ntarget_max_session_usage = 80",
            "session_floor = 70\ntarget_max_usage = 75\ntarget_max_session_usage = 80",
        ];
        for combo in collisions {
            let toml = with_tunables(&format!("{combo}\nsession_ceiling = 90"));
            assert!(
                matches!(Config::parse(&toml), Err(Error::ConfigParse(_))),
                "multiple reserve-key spellings present must be a ConfigParse error for `{combo}`, got: {:?}",
                Config::parse(&toml)
            );
        }
    }

    #[test]
    fn refresh_unknown_field_is_rejected() {
        // deny_unknown_fields: a stray key is a parse error, not a silent ignore.
        let toml = format!("{VALID}\n[refresh]\nenabled = true\nthreshold_secs = 99\n");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigParse(_))));
    }

    #[test]
    fn login_unknown_field_is_rejected() {
        // deny_unknown_fields: a stray key is a parse error, not a silent ignore.
        let toml = format!("{VALID}\n[login]\ntimeout_secs = 200\nwait_loop = true\n");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigParse(_))));
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
        // poll/session_ceiling/weekly_ceiling/cooldown (issue #41 added weekly_ceiling); a
        // bare `weekly` (≠ the actual `weekly_ceiling` key) is still unknown.
        assert!(matches!(
            Config::parse(&with_jitter("weekly = { kind = \"none\" }")),
            Err(Error::ConfigParse(_))
        ));
    }

    #[test]
    fn the_pre_rename_jitter_trigger_key_is_rejected_not_silently_defaulted() {
        // Issue #629 renamed the `[jitter].trigger` key → `session_ceiling`, finishing the
        // #606 dimension rename under the SAME no-migration posture. This is a fourth
        // breaking config key and the break is DELIBERATE, so it is pinned here rather than
        // left to chance.
        //
        // `RawJitter` is `deny_unknown_fields`, so a pre-#629 config.toml is a parse error
        // that NAMES the stale key AND — via serde's "expected one of" field list — the
        // replacement to write. That is also the falsifier for the no-back-compat decision:
        // a later well-meaning `#[serde(alias = "trigger")]` would silently restore
        // back-compat and nothing else in the suite would go red. Sibling of
        // `the_pre_rename_trigger_keys_are_rejected_not_silently_defaulted`, which pins the
        // same decision for the `[tunables]` half of the rename.
        match Config::parse(&with_jitter("trigger = { kind = \"none\" }")) {
            Err(Error::ConfigParse(msg)) => {
                assert!(
                    msg.contains("trigger"),
                    "the rejection must NAME the stale key, got: {msg}"
                );
                assert!(
                    msg.contains("session_ceiling"),
                    "…and point at `session_ceiling` so the operator can rename it, got: {msg}"
                );
            }
            other => panic!(
                "pre-rename `[jitter].trigger` must be rejected, never silently defaulted; got {other:?}"
            ),
        }
    }

    #[test]
    fn the_pre_rename_jitter_weekly_trigger_key_is_rejected_not_silently_defaulted() {
        // Issue #606 renamed the `[jitter].weekly_trigger` key → `weekly_ceiling` (the WEEKLY
        // half of the `[jitter]` dimension rename) under the no-migration posture. It is the
        // third of #606's three breaking key renames and the one left unpinned: the `[tunables]`
        // pair is pinned by `the_pre_rename_trigger_keys_are_rejected_not_silently_defaulted`,
        // and #629's later `[jitter].trigger` → `session_ceiling` by
        // `the_pre_rename_jitter_trigger_key_is_rejected_not_silently_defaulted`. With this test
        // every deliberate #606/#629 config-key break is falsifier-covered.
        //
        // `RawJitter` is `deny_unknown_fields`, so a pre-#606 config.toml is a parse error that
        // NAMES the stale key AND — via serde's "expected one of" field list — the replacement to
        // write. That is also the falsifier for the no-back-compat decision: a later well-meaning
        // `#[serde(alias = "weekly_trigger")]` on `RawJitter::weekly_ceiling` would silently
        // restore back-compat and nothing else in the suite would go red.
        match Config::parse(&with_jitter("weekly_trigger = { kind = \"none\" }")) {
            Err(Error::ConfigParse(msg)) => {
                assert!(
                    msg.contains("weekly_trigger"),
                    "the rejection must NAME the stale key, got: {msg}"
                );
                assert!(
                    msg.contains("weekly_ceiling"),
                    "…and point at `weekly_ceiling` so the operator can rename it, got: {msg}"
                );
            }
            other => panic!(
                "pre-rename `[jitter].weekly_trigger` must be rejected, never silently defaulted; got {other:?}"
            ),
        }
    }

    #[test]
    fn rejects_an_unknown_stats_key() {
        // `deny_unknown_fields` rejects a stray key as a parse error, like the other tables.
        let toml = format!("{VALID}\n[stats]\nbogus = 1\n");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigParse(_))));
    }

    #[test]
    fn rejects_an_unknown_migration_key() {
        // `deny_unknown_fields` rejects a stray key as a parse error, like the other tables. In
        // particular there is deliberately no `kdf_parallelism` key (lanes are fixed).
        let toml = format!("{VALID}\n[migration]\nkdf_parallelism = 2\n");
        assert!(matches!(Config::parse(&toml), Err(Error::ConfigParse(_))));
    }

    /// **The issue #775 backward-compatibility guard.** `RawTunables` is `deny_unknown_fields`,
    /// so adding `verbose` is a deliberate schema change — and the direction that matters is the
    /// other one: no `config.toml` on any operator's disk carries the new key, so it MUST be
    /// `#[serde(default)]`. A regression here does not degrade a feature, it stops every
    /// existing daemon from starting.
    #[test]
    fn a_config_written_before_the_verbose_knob_still_loads() {
        // A realistic pre-#775 file: a `[tunables]` table with real values and no `verbose`.
        let legacy = with_tunables(
            "poll_secs = 45\n\
             session_ceiling = 92\n\
             canary_drift_override = true",
        );
        let config = Config::parse(&legacy).expect("a pre-#775 config must still load");
        assert_eq!(config.tunables.poll_secs, 45, "the old keys still apply");
        assert!(config.tunables.canary_drift_override);
        assert!(
            !config.tunables.verbose,
            "an absent key must take the documented default (silent), not fail the parse"
        );

        // The same for a file with NO `[tunables]` table at all — the older shape still in the
        // wild — and for the `VALID` fixture the rest of this module parses.
        assert!(!Config::parse(VALID).expect("VALID parses").tunables.verbose);
    }

    /// The knob itself: the hand-edit the help text and `config.toml` comment both name arms it,
    /// and it survives a render → re-parse round trip (so `config-set`/`capture` rewriting the
    /// file cannot silently drop an operator's setting).
    #[test]
    fn the_verbose_knob_parses_and_survives_a_render_round_trip() {
        let armed = Config::parse(&with_tunables("verbose = true")).expect("parses");
        assert!(armed.tunables.verbose, "the documented hand-edit arms it");

        let reparsed = Config::parse(&armed.render()).expect("the rendered config re-parses");
        assert!(
            reparsed.tunables.verbose,
            "a render must not drop the operator's setting"
        );
        // The emitted file names the key and explains the restart semantics, since the knob is
        // useless to an operator who does not know it needs one.
        let rendered = armed.render();
        assert!(rendered.contains("verbose = true"));
        assert!(
            rendered.contains("NEXT daemon start"),
            "the emitted comment must state the no-hot-reload semantics"
        );

        // Default-off round-trips too, so a config written by a daemon that never armed it does
        // not come back armed.
        let off = Config::parse(VALID).expect("parses");
        assert!(
            !Config::parse(&off.render())
                .expect("re-parses")
                .tunables
                .verbose
        );

        // A non-bool is a parse error, not a coerced truthy value.
        assert!(matches!(
            Config::parse(&with_tunables("verbose = \"yes\"")),
            Err(Error::ConfigParse(_))
        ));
    }

    /// **A migration artifact has a SECOND compatibility surface, and it is not versioned**
    /// (issue #1053). The container's `format_version` gate is exact-match and ADR-0006 froze
    /// v1; but the config rides inside as TEXT and is re-parsed by the importing build's own
    /// [`RawConfig`], which carries `deny_unknown_fields` — as does every other `Raw*` struct.
    /// So a build whose parser predates any rendered **key** rejects every artifact a later
    /// build mints, while `format_version` reads `1` on both sides and says compatible. That
    /// disagreement is why nobody noticed.
    ///
    /// **The unit of breakage is a KEY, not a block — which is why the floor is not where the
    /// block arrived.** `[credential]` was added in `6fe3457` (2026-07-29), but a build pinned
    /// at exactly that commit *still* refuses a current artifact: `expiry_cohort_window_secs`
    /// landed inside the same block 14 commits later the same day (`81bd4f2`, issue #879). The
    /// two arms below witness both, which is what makes the pair falsify "the block's commit is
    /// the floor" rather than merely illustrate it.
    ///
    /// **What this can and cannot witness.** The failing parser lives in an already-BUILT
    /// binary, which no test in this tree can reach — ADR-0006 § Consequences → Negative /
    /// trade-offs says exactly that ("not catchable by any in-repo test … CI has no old
    /// binary"), and issue #1053 forbids trying. So each incompatibility is pinned against a
    /// hand-built mirror of the historical raw shape. The fixtures are the witnesses; the built
    /// binary stays out of reach, and nothing here claims to repair it.
    #[test]
    fn a_pre_credential_block_parser_rejects_a_config_a_current_build_renders() {
        /// The pre-`6fe3457` `RawConfig`, field-for-field, MINUS `credential`. Written out by
        /// hand rather than derived from the live struct — a derived mirror would track the
        /// very drift this exists to witness. Nothing reads the fields; the parse verdict is
        /// the whole assertion, so `dead_code` is expected here.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct PreCredentialRawConfig {
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
            #[serde(default)]
            stats: RawStats,
            #[serde(default)]
            migration: RawMigration,
        }

        // What `export` actually writes into an artifact: a current build's own render.
        let current = Config::parse(VALID)
            .expect("the baseline config parses")
            .render();
        assert!(
            current.contains("[credential]"),
            "render must emit [credential] or this test witnesses nothing: {current}"
        );

        // A current build reads what a current build renders — the version number is honest
        // about THIS direction.
        Config::parse(&current).expect("a current build must read its own render");

        // A build that predates the block does not, and says so by naming the block.
        let rejected = match toml::from_str::<PreCredentialRawConfig>(&current) {
            Err(err) => err,
            Ok(_) => panic!("a pre-6fe3457 parser must REJECT a [credential]-bearing config"),
        };
        assert!(
            rejected.to_string().contains("credential"),
            "the rejection must name the block responsible: {rejected}"
        );

        // `[credential]` is the only top-level BLOCK that crosses the floor: drop it and the
        // same pre-6fe3457 parser accepts the rest of a current render. This arm is a
        // recurrence gate, and its scope is exactly one class — a new TOP-LEVEL BLOCK. A key
        // added inside an existing block is invisible to it, which is how the second break got
        // through; that class is witnessed by the sibling test below, for `[credential]` only.
        //
        // Dropped through a `toml::Table` rather than by cutting lines, so the arm cannot be
        // silently defeated by a renderer whose formatting stops matching the cut (a strip that
        // over-reaches would fail below wearing the recurrence message, which is the wrong
        // diagnosis in the loudest possible place).
        let mut table: toml::Table = toml::from_str(&current).expect("a render is valid TOML");
        assert!(
            table.remove("credential").is_some(),
            "the [credential] key must be present to remove: {current}"
        );
        let without = toml::to_string(&table).expect("re-serializing a parsed table cannot fail");
        toml::from_str::<PreCredentialRawConfig>(&without).expect(
            "a NEW top-level rendered config block crosses the import version floor (issue \
             #1053): every build older than it will refuse every artifact this build mints, \
             exactly as `[credential]` already does. Decide that deliberately — see ADR-0006 \
             § Status",
        );
    }

    /// **The floor is NOT where the block arrived** (issue #1053). A build pinned at `6fe3457`
    /// — the commit that introduced `[credential]`, and the commit an earlier draft of this
    /// work named as the floor — still refuses a current artifact, because
    /// `expiry_cohort_window_secs` landed inside that same block 14 commits later on the same
    /// day (`81bd4f2`, issue #879). `git rev-list --count 6fe3457..81bd4f2` is 14.
    ///
    /// This is the second breakage class and the more dangerous one: every `Raw*` struct
    /// carries `deny_unknown_fields`, so an unknown **key inside a known block** is refused
    /// exactly as a whole unknown block is — but nothing about it looks like a schema change,
    /// which is how it passed unremarked. The sibling test above gates new top-level blocks and
    /// is structurally blind to this class.
    ///
    /// It also pins the *actionable* half. An operator told to build from `6fe3457` lands on a
    /// binary that still fails AND predates the version-floor message, so they get the bare
    /// serde line — the defect in its least legible form. That is what this asserts against.
    #[test]
    fn a_build_at_the_blocks_own_commit_still_rejects_a_current_render() {
        /// `RawCredential` exactly as it stood at `6fe3457`: one field, `deny_unknown_fields`.
        /// Hand-written for the same reason as its sibling — deriving it from the live struct
        /// would track the drift it exists to witness.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct CredentialAtBlockIntroduction {
            #[serde(default)]
            expiry_horizon_secs: i64,
        }

        let current = Config::parse(VALID)
            .expect("the baseline config parses")
            .render();
        let mut table: toml::Table = toml::from_str(&current).expect("a render is valid TOML");
        let credential = table
            .remove("credential")
            .expect("render must emit [credential] or this test witnesses nothing");

        let rejected = match credential.try_into::<CredentialAtBlockIntroduction>() {
            Err(err) => err,
            Ok(_) => panic!(
                "a parser at 6fe3457 must REJECT a current [credential] block — if this passes, \
                 the floor really is the block's own commit and the wording in \
                 `migration::CONFIG_BLOCK_VERSION_FLOOR` must move back to it"
            ),
        };
        assert!(
            rejected.to_string().contains("expiry_cohort_window_secs"),
            "the rejection must name the key that moved the floor past the block: {rejected}"
        );
    }
}
