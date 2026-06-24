// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The swap decision.
//!
//! Scaffolding scope: deciding *whether* to swap from a usage reading. The
//! out-of-band swap engine that *acts* on a [`SwapDecision::Swap`] (cooldown,
//! credential rotation, terminal state) lands in issues #6, #7, #10 and #11.

use crate::usage::Usage;

/// What the poll loop decided to do about the active account this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwapDecision {
    /// Stay on the current account.
    Hold,
    /// The active account crossed the usage threshold; the swap engine should
    /// rotate to the next account.
    Swap,
}

/// Decide whether to swap, based on the worst-case usage dimension.
pub(crate) fn decide(usage: &Usage, threshold: f64) -> SwapDecision {
    if usage.max_ratio() >= threshold {
        SwapDecision::Swap
    } else {
        SwapDecision::Hold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_below_threshold() {
        let usage = Usage {
            session: 0.5,
            weekly: 0.5,
        };
        assert_eq!(decide(&usage, 0.95), SwapDecision::Hold);
    }

    #[test]
    fn swaps_at_threshold_boundary() {
        let usage = Usage {
            session: 0.95,
            weekly: 0.1,
        };
        assert_eq!(decide(&usage, 0.95), SwapDecision::Swap);
    }
}
