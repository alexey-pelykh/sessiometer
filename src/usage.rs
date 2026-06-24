// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Per-account usage polling.
//!
//! Scaffolding scope: the [`Usage`] reading (both quota dimensions) and the
//! [`UsageSource`] seam. The real poller (the Claude usage API, both windows)
//! lands in issue #5.

use crate::error::{Error, Result};

/// A point-in-time usage reading for one account, across both quota windows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Usage {
    /// Fraction in `[0.0, 1.0]` of the rolling 5-hour session window consumed.
    pub(crate) session: f64,
    /// Fraction in `[0.0, 1.0]` of the weekly window consumed.
    pub(crate) weekly: f64,
}

impl Usage {
    /// The worst-case (highest) of the two dimensions — the one that drives a
    /// swap decision.
    pub(crate) fn max_ratio(self) -> f64 {
        self.session.max(self.weekly)
    }
}

/// Seam: reads one account's usage quota. The real impl polls the usage API
/// (#5); the test impl returns scripted readings.
pub(crate) trait UsageSource {
    async fn usage(&self) -> Result<Usage>;
}

/// Real usage poller. Behavior lands in issue #5.
pub(crate) struct RealUsageSource;

impl UsageSource for RealUsageSource {
    async fn usage(&self) -> Result<Usage> {
        Err(Error::Unimplemented("usage polling (#5)"))
    }
}

#[cfg(test)]
pub(crate) struct FakeUsageSource {
    reading: Usage,
}

#[cfg(test)]
impl FakeUsageSource {
    pub(crate) fn new(session: f64, weekly: f64) -> Self {
        Self {
            reading: Usage { session, weekly },
        }
    }
}

#[cfg(test)]
impl UsageSource for FakeUsageSource {
    async fn usage(&self) -> Result<Usage> {
        Ok(self.reading)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_ratio_picks_the_worst_dimension() {
        let usage = Usage {
            session: 0.3,
            weekly: 0.8,
        };
        assert_eq!(usage.max_ratio(), 0.8);
    }

    #[tokio::test]
    async fn real_source_reports_unimplemented() {
        let err = RealUsageSource.usage().await.unwrap_err();
        assert!(matches!(err, Error::Unimplemented(_)));
    }
}
