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
