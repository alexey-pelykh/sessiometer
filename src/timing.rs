// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Parameterized timing strategies: a base value plus optional per-cycle jitter.
//!
//! Each tunable that paces the run loop — the poll interval, the swap-away
//! trigger, and the post-swap cooldown — is modeled as a [`Strategy`]: a `base`
//! value plus a [`Jitter`] law. Every cycle the daemon *draws* a fresh value and
//! CLAMPS it to the parameter's valid range, so the cadence varies
//! cycle-to-cycle within safe bounds. The point is decorrelation: independent
//! daemons (and successive cycles of one daemon) do not fall into
//! lockstep-synchronized polling across accounts/cycles (issue #38).
//!
//! Randomness enters through the [`Rng`] seam, so the draws are deterministic
//! under a fixed seed — the whole sampler is unit-testable without wall-clock
//! flakiness. The jitter is decorrelation noise, not a security primitive, so a
//! small, fully-deterministic PRNG ([`SplitMix64`]) is exactly right and adds no
//! dependency (keeping `cargo deny check` trivially green): production
//! seeds it from coarse process entropy, tests seed it from a constant.

/// Randomness seam for jitter draws: a stream of uniform samples in `[0, 1)`.
///
/// Behind a trait so the daemon's per-cycle draws can be driven from a fixed-seed
/// PRNG in tests (deterministic) and from process entropy in production — the
/// same injectable-seam pattern as the daemon's [`Clock`](crate::contract::Clock) /
/// poller seams.
pub(crate) trait Rng {
    /// The next uniform sample in `[0, 1)`.
    fn next_unit(&mut self) -> f64;
}

/// A tiny, fully-deterministic PRNG (Vigna's SplitMix64).
///
/// Chosen over a crate dependency deliberately: the jitter is decorrelation
/// noise (not cryptographic), so a short, well-distributed generator is
/// sufficient and keeps the security-advisory surface (and `cargo deny`) empty.
/// One generator serves both paths — only the seed differs ([`new`](Self::new)
/// for a reproducible test stream, [`from_entropy`](Self::from_entropy) for
/// production).
pub(crate) struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Seed from an explicit value — the reproducible stream used in tests.
    pub(crate) fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Seed from coarse process entropy (wall-clock nanos mixed with the pid).
    /// Adequate for decorrelation: two daemons started in the same instant still
    /// differ by pid, and a poor seed only de-correlates one process's jitter —
    /// never a security boundary.
    pub(crate) fn from_entropy() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let pid = u64::from(std::process::id());
        // Mix the two so a low-entropy clock reading still perturbs the pid bits.
        Self::new(nanos ^ pid.rotate_left(32))
    }

    /// Advance the state and return the next raw 64-bit output.
    ///
    /// `pub(crate)` so the daemon can draw a raw per-process seed from
    /// [`from_entropy`](Self::from_entropy) for the issue-#612 per-daemon target-selection
    /// tie-break, and derive a stable per-(seed, index) hash key from it — both want the full
    /// 64 bits, not the `[0, 1)` float [`Rng::next_unit`] exposes.
    pub(crate) fn next_u64(&mut self) -> u64 {
        // SplitMix64: a fixed odd increment, then an avalanche finalizer.
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

impl Rng for SplitMix64 {
    fn next_unit(&mut self) -> f64 {
        // Top 53 bits → an exact f64 in [0, 1) carrying the full mantissa.
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// How a [`Strategy`]'s base value is randomized each cycle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Jitter {
    /// No randomization — the base value, verbatim.
    None,
    /// Uniform over `[base - spread, base + spread]` (before clamping). `spread`
    /// is in the parameter's own units; a negative `spread` is rejected at config
    /// load.
    Uniform { spread: f64 },
    /// Gaussian centered on `base` with the given standard deviation (before
    /// clamping). `stddev` is in the parameter's own units; a negative `stddev`
    /// is rejected at config load.
    Normal { stddev: f64 },
}

/// A tunable's timing strategy: a `base` value plus a [`Jitter`] law. The daemon
/// holds one per jittered tunable and [`draw`](Self::draw)s a fresh value each
/// cycle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Strategy {
    /// The center value the jitter perturbs.
    pub(crate) base: f64,
    /// The randomization applied to `base` on each draw.
    pub(crate) jitter: Jitter,
}

impl Strategy {
    /// A fixed strategy — `base` with no jitter.
    pub(crate) fn fixed(base: f64) -> Self {
        Self {
            base,
            jitter: Jitter::None,
        }
    }

    /// Draw this cycle's value and CLAMP it to `[lo, hi]`.
    ///
    /// [`Jitter::None`] returns the (clamped) base and never touches `rng`. The
    /// clamp is applied AFTER the draw, every cycle, so an out-of-range tail of
    /// the jitter distribution can never push the value past the parameter's
    /// valid bounds. `lo <= hi` is the caller's contract (always true for the
    /// fixed per-tunable bounds in [`crate::daemon`]).
    pub(crate) fn draw(&self, rng: &mut impl Rng, lo: f64, hi: f64) -> f64 {
        let raw = match self.jitter {
            Jitter::None => self.base,
            Jitter::Uniform { spread } => self.base + (2.0 * rng.next_unit() - 1.0) * spread,
            Jitter::Normal { stddev } => self.base + standard_normal(rng) * stddev,
        };
        raw.clamp(lo, hi)
    }

    /// Draw this cycle's value folded DOWNWARD from `base`, then CLAMP it to `[lo, hi]`.
    ///
    /// Like [`draw`](Self::draw), but the jitter is reflected so the result is never ABOVE `base` —
    /// only ever at or below it. Used for a *ceiling* value (a not-cross line): its configured jitter
    /// must decorrelate swap timing across daemons/cycles WITHOUT ever eroding the safety margin by
    /// drawing the ceiling higher than the operator set it (issue #609). The session ceiling sits
    /// below the SLO on purpose, so an upward draw would spend that headroom; a downward-only draw
    /// keeps the decorrelation while only ever buying MORE margin (the cheap-early-swap direction).
    ///
    /// The fold is a proper one-sided distribution below `base` (`Uniform` → `[base - spread, base]`;
    /// `Normal` → a half-normal `base - |z|·stddev`), so there is NO point-mass at `base` — unlike
    /// clamping a symmetric [`draw`](Self::draw)'s upper bound to `base`, which would pile the whole
    /// upper half onto `base` and halve the decorrelation. It consumes the SAME number of `rng`
    /// samples as [`draw`](Self::draw) for each [`Jitter`] mode (`None`: 0, `Uniform`: 1, `Normal`:
    /// 2), so swapping a call site from `draw` to `draw_downward` never shifts the daemon's per-cycle
    /// draw stream for tunables drawn after it. The clamp is applied AFTER the fold, exactly as in
    /// [`draw`](Self::draw), keeping `[lo, hi]` the orthogonal range rail.
    pub(crate) fn draw_downward(&self, rng: &mut impl Rng, lo: f64, hi: f64) -> f64 {
        let raw = match self.jitter {
            Jitter::None => self.base,
            // [base - spread, base]: one sample, matching `draw`'s Uniform arm.
            Jitter::Uniform { spread } => self.base - rng.next_unit() * spread,
            // Half-normal below base (`|z|·stddev`): `standard_normal` draws two samples, matching
            // `draw`'s Normal arm, so the downstream draw stream is unshifted.
            Jitter::Normal { stddev } => self.base - standard_normal(rng).abs() * stddev,
        };
        raw.clamp(lo, hi)
    }
}

/// One standard-normal sample (mean 0, stddev 1) via the Box–Muller transform.
fn standard_normal(rng: &mut impl Rng) -> f64 {
    // u1 is drawn in (0, 1] — shift the [0, 1) sample off zero so `ln` is finite.
    let u1 = 1.0 - rng.next_unit();
    let u2 = rng.next_unit();
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted RNG that replays a fixed slice of `[0, 1)` samples, cycling —
    /// lets a test pin the exact draw sequence a [`Strategy`] sees.
    struct ScriptedRng {
        samples: Vec<f64>,
        next: usize,
    }

    impl ScriptedRng {
        fn new(samples: &[f64]) -> Self {
            Self {
                samples: samples.to_vec(),
                next: 0,
            }
        }
    }

    impl Rng for ScriptedRng {
        fn next_unit(&mut self) -> f64 {
            let value = self.samples[self.next % self.samples.len()];
            self.next += 1;
            value
        }
    }

    #[test]
    fn split_mix_64_samples_stay_in_the_unit_interval() {
        let mut rng = SplitMix64::new(0xC0FF_EE12_3456_789A);
        for _ in 0..10_000 {
            let u = rng.next_unit();
            assert!((0.0..1.0).contains(&u), "sample {u} out of [0, 1)");
        }
    }

    #[test]
    fn split_mix_64_is_deterministic_under_a_fixed_seed() {
        // AC: deterministic under an injected RNG seed. Same seed → same stream.
        let a: Vec<f64> = {
            let mut rng = SplitMix64::new(42);
            (0..16).map(|_| rng.next_unit()).collect()
        };
        let b: Vec<f64> = {
            let mut rng = SplitMix64::new(42);
            (0..16).map(|_| rng.next_unit()).collect()
        };
        assert_eq!(a, b);
        // A different seed yields a different stream (sanity, not a strict
        // guarantee — but a fixed regression against a silent constant seed).
        let c: Vec<f64> = {
            let mut rng = SplitMix64::new(43);
            (0..16).map(|_| rng.next_unit()).collect()
        };
        assert_ne!(a, c);
    }

    #[test]
    fn none_returns_the_clamped_base_and_ignores_the_rng() {
        let strat = Strategy::fixed(300.0);
        // A panicking RNG would fire if `None` ever drew — it must not.
        let mut rng = ScriptedRng::new(&[]);
        assert_eq!(strat.draw(&mut rng, 5.0, 3600.0), 300.0);
        // Base outside the range is still clamped.
        assert_eq!(Strategy::fixed(9000.0).draw(&mut rng, 5.0, 3600.0), 3600.0);
        assert_eq!(Strategy::fixed(1.0).draw(&mut rng, 5.0, 3600.0), 5.0);
    }

    #[test]
    fn uniform_spans_base_plus_or_minus_spread() {
        let strat = Strategy {
            base: 95.0,
            jitter: Jitter::Uniform { spread: 4.0 },
        };
        // next_unit() == 0.0 → base - spread; == ~1.0 → base + spread; 0.5 → base.
        let mut lo = ScriptedRng::new(&[0.0]);
        let mut mid = ScriptedRng::new(&[0.5]);
        let mut hi = ScriptedRng::new(&[1.0]);
        assert_eq!(strat.draw(&mut lo, 50.0, 99.0), 91.0);
        assert_eq!(strat.draw(&mut mid, 50.0, 99.0), 95.0);
        // base + spread == 99.0, exactly the clamp ceiling.
        assert_eq!(strat.draw(&mut hi, 50.0, 99.0), 99.0);
    }

    #[test]
    fn uniform_draws_are_always_clamped_to_the_valid_range() {
        // A spread wider than the range must never escape [lo, hi].
        let strat = Strategy {
            base: 95.0,
            jitter: Jitter::Uniform { spread: 1000.0 },
        };
        let mut rng = SplitMix64::new(7);
        for _ in 0..10_000 {
            let v = strat.draw(&mut rng, 50.0, 99.0);
            assert!((50.0..=99.0).contains(&v), "drew {v} outside [50, 99]");
        }
    }

    #[test]
    fn normal_draws_are_deterministic_and_clamped() {
        // AC: each cycle draws a jittered value within the valid range,
        // deterministic under an injected seed. Two identically-seeded streams
        // produce identical draw sequences, all within range.
        let strat = Strategy {
            base: 300.0,
            jitter: Jitter::Normal { stddev: 30.0 },
        };
        let draw_seq = |seed: u64| -> Vec<f64> {
            let mut rng = SplitMix64::new(seed);
            (0..1000)
                .map(|_| strat.draw(&mut rng, 5.0, 3600.0))
                .collect()
        };
        let first = draw_seq(2024);
        let second = draw_seq(2024);
        assert_eq!(first, second, "same seed must replay the same draws");
        for v in &first {
            assert!((5.0..=3600.0).contains(v), "drew {v} outside [5, 3600]");
        }
        // The jitter actually moves the value off the base for some cycles (it is
        // not silently degenerate).
        assert!(
            first.iter().any(|&v| (v - 300.0).abs() > 1.0),
            "normal jitter never perturbed the base"
        );
    }

    #[test]
    fn normal_mean_is_near_the_base_over_many_draws() {
        let strat = Strategy {
            base: 300.0,
            jitter: Jitter::Normal { stddev: 30.0 },
        };
        let mut rng = SplitMix64::new(99);
        let n = 50_000;
        // Wide bounds so the clamp does not bias the empirical mean.
        let sum: f64 = (0..n).map(|_| strat.draw(&mut rng, 0.0, 100_000.0)).sum();
        let mean = sum / f64::from(n);
        assert!(
            (mean - 300.0).abs() < 2.0,
            "empirical mean {mean} far from 300"
        );
    }

    #[test]
    fn draw_downward_none_returns_the_clamped_base_and_ignores_the_rng() {
        let strat = Strategy::fixed(95.0);
        // A panicking RNG would fire if `None` ever drew — it must not.
        let mut rng = ScriptedRng::new(&[]);
        assert_eq!(strat.draw_downward(&mut rng, 50.0, 99.0), 95.0);
        // A base above the range is still clamped DOWN to hi.
        assert_eq!(
            Strategy::fixed(120.0).draw_downward(&mut rng, 50.0, 99.0),
            99.0
        );
    }

    #[test]
    fn draw_downward_uniform_spans_base_minus_spread_to_base() {
        // Issue #609: the fold is one-sided — `[base - spread, base]`. next_unit()==0 → base (the
        // top, NOT above it); next_unit()→1 → base - spread (the bottom).
        let strat = Strategy {
            base: 95.0,
            jitter: Jitter::Uniform { spread: 4.0 },
        };
        let mut top = ScriptedRng::new(&[0.0]);
        let mut bottom = ScriptedRng::new(&[1.0]);
        assert_eq!(strat.draw_downward(&mut top, 50.0, 99.0), 95.0);
        assert_eq!(strat.draw_downward(&mut bottom, 50.0, 99.0), 91.0);
    }

    #[test]
    fn draw_downward_never_draws_above_base() {
        // The ceiling-safety property (issue #609): a configured jitter may only pull the ceiling at
        // or BELOW base — never above — so it can never erode the sub-SLO margin. A spread/stddev far
        // wider than the range must still never exceed base (and the `hi` bound is set ABOVE base so
        // the clamp is NOT what keeps it under base).
        for jitter in [
            Jitter::Uniform { spread: 1000.0 },
            Jitter::Normal { stddev: 1000.0 },
        ] {
            let strat = Strategy { base: 95.0, jitter };
            let mut rng = SplitMix64::new(7);
            for _ in 0..10_000 {
                let v = strat.draw_downward(&mut rng, 0.0, 100.0);
                assert!(v <= 95.0 + 1e-9, "drew {v} above base 95 for {jitter:?}");
                assert!((0.0..=100.0).contains(&v), "drew {v} outside [0, 100]");
            }
        }
    }

    #[test]
    fn draw_downward_consumes_the_same_rng_samples_as_draw() {
        // RNG-sample-count PARITY (issue #609): swapping a call site from `draw` to `draw_downward`
        // must NOT shift the draw stream for tunables drawn AFTER it (else fixed-seed goldens break).
        // For each jitter mode, the value a jittered DOWNSTREAM tunable draws off the same rng must be
        // identical whether the upstream ceiling used `draw` or `draw_downward` — i.e. both consumed
        // the same number of samples (None: 0, Uniform: 1, Normal: 2).
        for jitter in [
            Jitter::None,
            Jitter::Uniform { spread: 4.0 },
            Jitter::Normal { stddev: 3.0 },
        ] {
            let ceiling = Strategy { base: 95.0, jitter };
            let downstream = Strategy {
                base: 300.0,
                jitter: Jitter::Uniform { spread: 10.0 },
            };
            let after_draw = {
                let mut rng = SplitMix64::new(1234);
                let _ = ceiling.draw(&mut rng, 50.0, 99.0);
                downstream.draw(&mut rng, 0.0, 1000.0)
            };
            let after_downward = {
                let mut rng = SplitMix64::new(1234);
                let _ = ceiling.draw_downward(&mut rng, 50.0, 99.0);
                downstream.draw(&mut rng, 0.0, 1000.0)
            };
            assert_eq!(
                after_draw, after_downward,
                "draw_downward shifted the rng stream for {jitter:?}"
            );
        }
    }
}
