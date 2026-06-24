// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Runtime configuration.
//!
//! Scaffolding scope: the shape of the daemon's tunables plus sane defaults.
//! Loading and persisting `config.toml` lands in issue #3 — [`Config::load`] is
//! the seam for it.

use std::time::Duration;

use crate::error::{Error, Result};

/// Daemon tunables.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// How long to wait between usage polls.
    pub(crate) poll_interval: Duration,
    /// Usage fraction in `[0.0, 1.0]` at or above which the active account is
    /// considered exhausted and a swap is warranted.
    pub(crate) swap_threshold: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(60),
            swap_threshold: 0.95,
        }
    }
}

impl Config {
    /// Load configuration from disk. Behavior lands in issue #3; until then
    /// callers fall back to [`Config::default`].
    pub(crate) fn load() -> Result<Self> {
        Err(Error::Unimplemented("config load (#3)"))
    }
}
