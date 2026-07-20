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
