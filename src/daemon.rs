// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The poll loop and its decision state.
//!
//! [`Daemon`] is generic over its three seams — [`UsageSource`],
//! [`CredentialStore`] and [`Clock`] — so the whole loop runs hermetically
//! against in-memory fakes in tests: no live quota, no keychain, no real time.
//! The current-thread runtime (see `main`) is what lets the seams stay free of
//! `Send` bounds.
//!
//! Scaffolding scope: one [`Daemon::tick`] reads usage and computes a swap
//! decision. Acting on the decision (the out-of-band swap, cooldown, terminal
//! state) lands in issues #6, #7, #10 and #11.

use std::time::{Duration, Instant};

use crate::error::Result;
use crate::keychain::CredentialStore;
use crate::swap::{self, SwapDecision};
use crate::usage::{Usage, UsageSource};

/// Time seam: the daemon reads "now" and waits for the next poll through this,
/// so a fake can freeze time and make the loop run instantly in tests.
pub(crate) trait Clock {
    /// The current instant.
    fn now(&self) -> Instant;
    /// Wait until the next poll is due.
    async fn tick(&self);
}

/// Real clock: monotonic `Instant::now` and a Tokio sleep between polls.
pub(crate) struct RealClock {
    interval: Duration,
}

impl RealClock {
    pub(crate) fn new(interval: Duration) -> Self {
        Self { interval }
    }
}

impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    async fn tick(&self) {
        tokio::time::sleep(self.interval).await;
    }
}

/// Per-account decision state carried across polls. Scaffolding scope: a tick
/// counter; cooldown and exhaustion tracking land in issues #10 and #11.
#[derive(Default)]
struct DecisionState {
    ticks: u64,
}

/// The result of one poll iteration.
#[derive(Debug)]
pub(crate) struct TickOutcome {
    /// 1-based sequence number of this poll.
    pub(crate) tick: u64,
    /// When the reading was taken.
    pub(crate) at: Instant,
    /// The usage observed this tick.
    pub(crate) usage: Usage,
    /// What the loop decided to do about it.
    pub(crate) decision: SwapDecision,
}

/// The poll loop, generic over its three injectable seams.
pub(crate) struct Daemon<U, C, K> {
    usage: U,
    /// Held for the out-of-band swap engine (#6/#7), which reads and rewrites
    /// the active credential through this seam.
    #[allow(dead_code)]
    store: C,
    clock: K,
    threshold: f64,
    state: DecisionState,
}

impl<U, C, K> Daemon<U, C, K>
where
    U: UsageSource,
    C: CredentialStore,
    K: Clock,
{
    pub(crate) fn new(usage: U, store: C, clock: K, threshold: f64) -> Self {
        Self {
            usage,
            store,
            clock,
            threshold,
            state: DecisionState::default(),
        }
    }

    /// Run one poll iteration: read usage, advance state, decide.
    pub(crate) async fn tick(&mut self) -> Result<TickOutcome> {
        let at = self.clock.now();
        let usage = self.usage.usage().await?;
        self.state.ticks += 1;
        let decision = swap::decide(&usage, self.threshold);
        Ok(TickOutcome {
            tick: self.state.ticks,
            at,
            usage,
            decision,
        })
    }

    /// Wait until the next poll is due (delegates to the [`Clock`] seam).
    pub(crate) async fn wait_for_next_poll(&self) {
        self.clock.tick().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keychain::FakeCredentialStore;
    use crate::usage::FakeUsageSource;

    /// A clock frozen at construction: `now` is constant and `tick` returns
    /// immediately, so a loop driven by it runs instantly.
    struct FakeClock {
        at: Instant,
    }

    impl FakeClock {
        fn frozen() -> Self {
            Self { at: Instant::now() }
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.at
        }

        async fn tick(&self) {}
    }

    fn test_daemon(
        session: f64,
        weekly: f64,
        threshold: f64,
    ) -> Daemon<FakeUsageSource, FakeCredentialStore, FakeClock> {
        Daemon::new(
            FakeUsageSource::new(session, weekly),
            FakeCredentialStore::empty(),
            FakeClock::frozen(),
            threshold,
        )
    }

    #[tokio::test]
    async fn tick_holds_below_threshold() {
        let mut daemon = test_daemon(0.10, 0.20, 0.95);
        let outcome = daemon
            .tick()
            .await
            .expect("the fake usage source never errors");
        assert_eq!(outcome.decision, SwapDecision::Hold);
        assert_eq!(
            outcome.usage,
            Usage {
                session: 0.10,
                weekly: 0.20
            }
        );
        assert_eq!(outcome.tick, 1);
    }

    #[tokio::test]
    async fn tick_swaps_at_or_above_threshold() {
        let mut daemon = test_daemon(0.97, 0.50, 0.95);
        let outcome = daemon
            .tick()
            .await
            .expect("the fake usage source never errors");
        assert_eq!(outcome.decision, SwapDecision::Swap);
        assert_eq!(outcome.tick, 1);
    }

    #[tokio::test]
    async fn wait_for_next_poll_is_immediate_with_a_frozen_clock() {
        let mut daemon = test_daemon(0.0, 0.0, 0.95);
        let outcome = daemon
            .tick()
            .await
            .expect("the fake usage source never errors");
        // The frozen clock's `tick` is a no-op, so the wait must resolve
        // immediately rather than sleeping; bound it to prove the loop never
        // blocks on real time.
        tokio::time::timeout(Duration::from_secs(1), daemon.wait_for_next_poll())
            .await
            .expect("wait_for_next_poll must not sleep under a frozen clock");
        assert_eq!(outcome.tick, 1);
    }
}
