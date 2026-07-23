// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The daemon-routed `config-get` / `config-set` backend (issue #268), split out by issue
//! #638's per-concern decomposition of the one 1,253-line `impl Config`.
//!
//! The read half ([`Config::view`] / [`Config::view_from_text`]) projects a NON-SECRET view;
//! the write half ([`Config::apply_settings`]) overlays an allow-listed edit onto the raw
//! layer and re-runs the WHOLE result through [`Config::validate`], so a batch is valid iff
//! its resulting config is. Neither half can reach a credential or re-key the roster — the
//! #268 safety boundary — which is why the overlays live here, next to the surface they serve.

use super::*;

impl Config {
    /// Apply a `config-set` control command's edits (issue #268) to the config `text`
    /// read from disk, re-validating the WHOLE result through the same
    /// [`validate`](Config::validate) that [`load`](Config::load) runs — so every range
    /// and cross-field rule (`target_max_session_usage <= session_ceiling`,
    /// `exhausted_poll_secs >= poll_secs`, the `near_limit_poll_secs` 0-or-band shape, …)
    /// is enforced atomically over the FINAL state. An invalid batch is rejected with
    /// nothing written; a batch is valid iff its resulting config is (an individually
    /// out-of-order pair — e.g. raising `poll_secs` past the old `exhausted_poll_secs`
    /// while also raising the latter — validates because both land before the check).
    ///
    /// `tunables` carries only the scalar `[tunables]` edits the settings UI may make
    /// ([`SetTunables`] is the allow-list — a credential, an `[[account]]`, or any other
    /// key is unrepresentable there). `labels` maps `account_uuid` → a new label; a uuid
    /// matching no roster account is [`Error::AccountUuidNotFound`]. ONLY an existing
    /// account's `label` is touched — the roster is never grown, shrunk, or re-keyed, and
    /// no credential is reachable (the #268 safety boundary).
    ///
    /// Returns the re-validated [`Config`] to persist (via [`save_to`](Config::save_to))
    /// plus a [`SettingsChange`] recording which classes actually changed, so the daemon
    /// picks the reload semantics: a tunable change is reload-by-restart (the daemon
    /// derives its strategy fields once at construction), a label change adopts live.
    pub(crate) fn apply_settings(
        text: &str,
        tunables: &SetTunables,
        labels: &BTreeMap<String, String>,
    ) -> Result<(Config, SettingsChange)> {
        // Baseline: the current on-disk config, fully validated. A currently-invalid file
        // (hand-broken) fails HERE, so config-set refuses rather than overwrite a file it
        // cannot understand — the daemon maps this to a `config-unreadable` rejection.
        let before = Config::parse(text)?;
        // Overlay the edits onto the raw layer so the SINGLE validate() sees the final
        // state; the file is re-parsed (tiny) rather than cloning the non-`Clone` raw.
        let mut raw: RawConfig =
            toml::from_str(text).map_err(|err| Error::ConfigParse(err.to_string()))?;
        overlay_tunables(&mut raw.tunables, tunables);
        overlay_labels(&mut raw.account, labels)?;
        let after = Config::validate(raw)?;
        let change = SettingsChange {
            tunables_changed: after.tunables != before.tunables,
            labels_changed: after.roster != before.roster,
        };
        Ok((after, change))
    }

    /// A non-secret projection of the effective config for the `config-get` control
    /// command (issue #268): the scalar tunables the settings UI edits + each roster
    /// account's non-secret `account_uuid` / `label` / `enabled`. Carries NO credential
    /// (the roster keys on uuid + label only, issue #15), so it is exactly as safe to
    /// return over the same-user control socket as the `status` / `watch` snapshots.
    pub(crate) fn view(&self) -> ConfigView {
        ConfigView {
            tunables: TunablesView::from(&self.tunables),
            accounts: self
                .roster
                .iter()
                .map(|account| AccountView {
                    account_uuid: account.account_uuid.clone(),
                    label: account.label.clone(),
                    enabled: account.enabled,
                })
                .collect(),
        }
    }

    /// Parse `text` and project it to a [`ConfigView`] (issue #268) — the `config-get` read path's
    /// one-call text→view seam, keeping [`parse`](Config::parse) private while giving the daemon a
    /// non-secret projection to serialize. Errors exactly as [`load`](Config::load) would (a parse or
    /// validation failure), which `config-get` maps to a `config unreadable` envelope.
    pub(crate) fn view_from_text(text: &str) -> Result<ConfigView> {
        Ok(Config::parse(text)?.view())
    }
}

/// Overlay a `config-set`'s scalar tunable edits (issue #268) onto the raw layer — each
/// `Some(v)` replaces that key, each `None` leaves it. `target_max_session_usage` maps to
/// the raw `Option` (its absence sentinel); every other key to its plain scalar. Ranges are
/// NOT checked here — [`Config::validate`] does that atomically over the overlaid result.
fn overlay_tunables(raw: &mut RawTunables, edits: &SetTunables) {
    if let Some(v) = edits.poll_secs {
        raw.poll_secs = v;
    }
    if let Some(v) = edits.exhausted_poll_secs {
        raw.exhausted_poll_secs = v;
    }
    if let Some(v) = edits.near_limit_poll_secs {
        raw.near_limit_poll_secs = v;
    }
    if let Some(v) = edits.cooldown_secs {
        raw.cooldown_secs = v;
    }
    if let Some(v) = edits.target_max_session_usage {
        raw.target_max_session_usage = Some(v);
    }
    if let Some(v) = edits.session_ceiling {
        raw.session_ceiling = v;
    }
    if let Some(v) = edits.weekly_ceiling {
        raw.weekly_ceiling = v;
    }
    if let Some(v) = edits.session_blind_swap_secs {
        raw.session_blind_swap_secs = v;
    }
    if let Some(v) = edits.session_blind_risk_band {
        raw.session_blind_risk_band = v;
    }
    if let Some(v) = edits.session_velocity_horizon_secs {
        raw.session_velocity_horizon_secs = v;
    }
    if let Some(v) = edits.session_velocity_min_project_above {
        raw.session_velocity_min_project_above = v;
    }
    if let Some(v) = edits.session_velocity_ema_alpha_pct {
        raw.session_velocity_ema_alpha_pct = v;
    }
    if let Some(v) = edits.monitor_401_n {
        raw.monitor_401_n = v;
    }
    if let Some(v) = edits.monitor_recovery_m {
        raw.monitor_recovery_m = v;
    }
    if let Some(v) = edits.fleet_runway_warn_secs {
        raw.fleet_runway_warn_secs = v;
    }
    if let Some(v) = edits.canary_drift_override {
        raw.canary_drift_override = v;
    }
}

/// Overlay a `config-set`'s label edits (issue #268): each `account_uuid` → new label is
/// written onto the MATCHING existing raw account. A uuid matching none is
/// [`Error::AccountUuidNotFound`]. Never appends/removes an entry — only an existing
/// account's `label` field is touched, so the roster structure (and every credential keyed
/// off it) is out of reach (the #268 safety boundary). The new label's non-emptiness is
/// enforced downstream by [`Config::validate`].
fn overlay_labels(accounts: &mut [RawAccount], labels: &BTreeMap<String, String>) -> Result<()> {
    for (account_uuid, new_label) in labels {
        let account = accounts
            .iter_mut()
            .find(|account| &account.account_uuid == account_uuid)
            .ok_or_else(|| Error::AccountUuidNotFound {
                account_uuid: account_uuid.clone(),
            })?;
        account.label = new_label.clone();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::*;

    // ── config-set / config-get backend (issue #268) ──

    /// A `BTreeMap<uuid, label>` for the `config-set` label edits (fully qualified so the
    /// test needs no extra `use`).
    fn labels(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(uuid, label)| (uuid.to_string(), label.to_string()))
            .collect()
    }

    #[test]
    fn apply_settings_overlays_a_tunable_and_revalidates() {
        let (after, change) = Config::apply_settings(
            VALID,
            &SetTunables {
                poll_secs: Some(300),
                ..SetTunables::default()
            },
            &labels(&[]),
        )
        .unwrap();
        assert_eq!(after.tunables.poll_secs, 300);
        assert!(change.tunables_changed);
        assert!(!change.labels_changed);
        // A tunables-only edit leaves the roster untouched.
        assert_eq!(after.roster.len(), 2);
    }

    #[test]
    fn apply_settings_relabels_an_account_by_uuid() {
        let (after, change) = Config::apply_settings(
            VALID,
            &SetTunables::default(),
            &labels(&[("11111111-1111-1111-1111-111111111111", "day-job")]),
        )
        .unwrap();
        assert!(change.labels_changed);
        assert!(!change.tunables_changed);
        let renamed = after
            .roster
            .iter()
            .find(|a| a.account_uuid == "11111111-1111-1111-1111-111111111111")
            .unwrap();
        assert_eq!(renamed.label, "day-job");
    }

    #[test]
    fn apply_settings_rejects_an_out_of_range_tunable() {
        // poll_secs floor is 5; 4 is out of range → the whole batch is rejected.
        let err = Config::apply_settings(
            VALID,
            &SetTunables {
                poll_secs: Some(4),
                ..SetTunables::default()
            },
            &labels(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConfigInvalid(_)));
    }

    #[test]
    fn apply_settings_validates_the_final_batch_not_intermediate_states() {
        // Current: poll_secs=30, exhausted_poll_secs=60. Raising poll_secs to 300 AND
        // exhausted to 7200 is valid as a WHOLE (300 <= 7200), even though applying poll
        // first would transiently violate `exhausted >= poll` (60 < 300). Atomic validation
        // over the final state is what lets a settings-form "Apply" move coupled fields.
        let base = with_tunables("poll_secs = 30\nexhausted_poll_secs = 60");
        let (after, _) = Config::apply_settings(
            &base,
            &SetTunables {
                poll_secs: Some(300),
                exhausted_poll_secs: Some(7200),
                ..SetTunables::default()
            },
            &labels(&[]),
        )
        .unwrap();
        assert_eq!(after.tunables.poll_secs, 300);
        assert_eq!(after.tunables.exhausted_poll_secs, 7200);
    }

    #[test]
    fn apply_settings_rejects_a_cross_field_invalid_batch() {
        // exhausted_poll_secs must be >= poll_secs; 200 < 300 → rejected as a whole.
        let err = Config::apply_settings(
            VALID,
            &SetTunables {
                poll_secs: Some(300),
                exhausted_poll_secs: Some(200),
                ..SetTunables::default()
            },
            &labels(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConfigInvalid(_)));
    }

    #[test]
    fn apply_settings_rejects_target_max_above_session_ceiling() {
        // VALID session_ceiling=90; target_max_session_usage=95 > 90 → the distinct cross-field error.
        let err = Config::apply_settings(
            VALID,
            &SetTunables {
                target_max_session_usage: Some(95),
                ..SetTunables::default()
            },
            &labels(&[]),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            Error::ConfigTargetMaxSessionAboveTrigger { .. }
        ));
    }

    #[test]
    fn apply_settings_rejects_an_unknown_account_uuid() {
        let err = Config::apply_settings(
            VALID,
            &SetTunables::default(),
            &labels(&[("no-such-uuid", "x")]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::AccountUuidNotFound { .. }));
    }

    #[test]
    fn apply_settings_rejects_an_empty_label() {
        let err = Config::apply_settings(
            VALID,
            &SetTunables::default(),
            &labels(&[("11111111-1111-1111-1111-111111111111", "  ")]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConfigInvalid(_)));
    }

    #[test]
    fn apply_settings_reports_no_change_for_a_noop_edit() {
        // Submitting the current value + current label changes nothing.
        let (_, change) = Config::apply_settings(
            VALID,
            &SetTunables {
                poll_secs: Some(30), // VALID's current poll_secs
                ..SetTunables::default()
            },
            &labels(&[("11111111-1111-1111-1111-111111111111", "work")]),
        )
        .unwrap();
        assert!(!change.tunables_changed);
        assert!(!change.labels_changed);
    }

    #[test]
    fn apply_settings_overlays_fleet_runway_warn_and_the_view_projects_it() {
        // Issue #650: the new tunable rides the SAME #268 config-set overlay + config-get view
        // as every other scalar — no parallel surface. Set it via `SetTunables`, read it back
        // through `view()` (the config-get projection).
        let (after, change) = Config::apply_settings(
            VALID,
            &SetTunables {
                fleet_runway_warn_secs: Some(7200),
                ..SetTunables::default()
            },
            &labels(&[]),
        )
        .unwrap();
        assert_eq!(after.tunables.fleet_runway_warn_secs, 7200);
        assert!(change.tunables_changed);
        assert!(!change.labels_changed);
        assert_eq!(after.view().tunables.fleet_runway_warn_secs, 7200);
        // The overlaid value renders and re-parses verbatim (config-set persists via `render`).
        let reparsed = Config::parse(&after.render()).unwrap();
        assert_eq!(reparsed.tunables.fleet_runway_warn_secs, 7200);

        // The atomic re-validate rejects an out-of-band set (59, a non-zero sub-floor) as a
        // whole batch — the config-set path enforces the same 0-or-band shape as load.
        let err = Config::apply_settings(
            VALID,
            &SetTunables {
                fleet_runway_warn_secs: Some(59),
                ..SetTunables::default()
            },
            &labels(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConfigInvalid(_)));

        // The key is a representable scalar edit (serde round-trip), unset stays None.
        let parsed: SetTunables =
            serde_json::from_str(r#"{"fleet_runway_warn_secs":7200}"#).unwrap();
        assert_eq!(parsed.fleet_runway_warn_secs, Some(7200));
        let empty: SetTunables = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.fleet_runway_warn_secs, None);
    }

    #[test]
    fn apply_settings_overlays_the_canary_drift_override_and_it_round_trips() {
        // Issue #714: the operator's documented false-drift recovery lever rides the SAME #268
        // config-set overlay + config-get view + render persistence as every other tunable — the
        // one plumbing chain the `Error::CanaryDrift` remedy text depends on ("set
        // `canary_drift_override = true` … and restart"), pinned end-to-end with the NON-default
        // value so a dropped overlay arm, a typo'd render key, or a miskeyed view field fails
        // here rather than stranding an unattended daemon on a refused swap.
        let (after, change) = Config::apply_settings(
            VALID,
            &SetTunables {
                canary_drift_override: Some(true),
                ..SetTunables::default()
            },
            &labels(&[]),
        )
        .unwrap();
        assert!(after.tunables.canary_drift_override);
        assert!(change.tunables_changed);
        assert!(after.view().tunables.canary_drift_override);
        // The overlaid value renders and re-parses verbatim (config-set persists via `render`).
        let reparsed = Config::parse(&after.render()).unwrap();
        assert!(reparsed.tunables.canary_drift_override);

        // The hand-edit path the error message actually names: a bare TOML line under
        // `[tunables]` parses to the armed override.
        assert!(
            Config::parse(&with_tunables("canary_drift_override = true"))
                .unwrap()
                .tunables
                .canary_drift_override,
            "the documented hand-edit arms the override"
        );

        // The key is a representable scalar edit (serde round-trip), unset stays None.
        let parsed: SetTunables =
            serde_json::from_str(r#"{"canary_drift_override":true}"#).unwrap();
        assert_eq!(parsed.canary_drift_override, Some(true));
        let empty: SetTunables = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.canary_drift_override, None);
    }

    #[test]
    fn apply_settings_refuses_a_currently_unreadable_config() {
        // A hand-broken file fails at the baseline parse — config-set never overwrites a
        // file it cannot understand.
        let err = Config::apply_settings(
            "this is not toml [[[",
            &SetTunables::default(),
            &labels(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConfigParse(_)));
    }

    #[test]
    fn set_tunables_rejects_a_forbidden_key() {
        // SAFETY invariant: only the scalar tunable keys are representable. A credential, a
        // roster field, or any other key is a hard parse error (deny_unknown_fields), so the
        // credential/roster-structure boundary cannot be crossed through config-set.
        for forbidden in [
            r#"{"account_uuid":"x"}"#,
            r#"{"credential":"secret"}"#,
            r#"{"label":"x"}"#,
            r#"{"enabled":true}"#,
            r#"{"poll_secs":300,"roster":[]}"#,
        ] {
            assert!(
                serde_json::from_str::<SetTunables>(forbidden).is_err(),
                "forbidden key accepted: {forbidden}"
            );
        }
        // A bare scalar tunable parses; unset keys stay None.
        let ok: SetTunables = serde_json::from_str(r#"{"poll_secs":300}"#).unwrap();
        assert_eq!(ok.poll_secs, Some(300));
        assert_eq!(ok.session_ceiling, None);
    }

    #[test]
    fn config_view_projects_tunables_and_roster() {
        let view = Config::parse(VALID).unwrap().view();
        assert_eq!(view.tunables.poll_secs, 30);
        assert_eq!(view.tunables.session_ceiling, 90);
        assert_eq!(view.accounts.len(), 2);
        assert_eq!(
            view.accounts[0].account_uuid,
            "11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(view.accounts[0].label, "work");
        assert!(view.accounts[0].enabled);
    }

    #[test]
    fn config_view_serde_round_trips() {
        let view = Config::parse(VALID).unwrap().view();
        let json = serde_json::to_string(&view).unwrap();
        let back: ConfigView = serde_json::from_str(&json).unwrap();
        assert_eq!(view, back);
    }
}
