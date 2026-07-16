// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Nearest-rank percentile over `f64` samples — the single copy of the computation
//! `usage_stats`, `usage_store`, and `reliability` all share (issue #455).
//!
//! Hand-rolled on purpose (the minimal-dependency line, `CONTRIBUTING.md`): a one-function
//! nearest-rank percentile is not worth a statistics crate in a credential-adjacent supply
//! chain. The method is the textbook nearest-rank estimate — sort ascending, take the
//! `ceil(p·n)`-th value (1-indexed) — so it agrees, value-for-value, with the two former
//! `p95_of` copies it replaces (`usage_stats`, `usage_store`).

/// The nearest-rank percentile of `xs` at fraction `p` (`0.0..=1.0`); `0.0` for an empty
/// slice.
///
/// Sort ascending and return the `ceil(p·n)`-th value, 1-indexed and clamped into `1..=n`,
/// so `p == 0.0` yields the minimum, `p == 0.5` the (nearest-rank) median, `p == 0.95` the
/// 95th percentile, and `p == 1.0` the true maximum. Total and pure — NaN orders via
/// [`f64::total_cmp`], and no input can panic (the empty case returns early, and the clamp
/// keeps the index in bounds for every finite `p`).
pub(crate) fn percentile(xs: &[f64], p: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(f64::total_cmp);
    // Nearest-rank: the ceil(p·n)-th value (1-indexed), clamped into range so p=0 → min and
    // p=1 → max. Matches the pre-#455 `p95_of` rank arithmetic exactly.
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

#[cfg(test)]
mod tests {
    use super::percentile;

    #[test]
    fn empty_is_zero_at_every_fraction() {
        assert_eq!(percentile(&[], 0.0), 0.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
        assert_eq!(percentile(&[], 0.95), 0.0);
        assert_eq!(percentile(&[], 1.0), 0.0);
    }

    #[test]
    fn p95_matches_the_former_nearest_rank() {
        // ceil(0.95·6) = 6 → the 6th (largest) value; preserves the pre-#455 `p95_of` result
        // so the two refactored call sites are behavior-identical.
        let xs = [10.0, 20.0, 30.0, 40.0, 50.0, 60.0];
        assert_eq!(percentile(&xs, 0.95), 60.0);
    }

    #[test]
    fn p50_is_the_nearest_rank_median_and_input_is_sorted_first() {
        // ceil(0.5·6) = 3 → the 3rd value of the SORTED input (given unsorted).
        let xs = [50.0, 10.0, 30.0, 60.0, 20.0, 40.0];
        assert_eq!(percentile(&xs, 0.50), 30.0);
        // Odd n: ceil(0.5·5) = 3 → the 3rd of five.
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0, 5.0], 0.50), 3.0);
    }

    #[test]
    fn p100_is_the_max_and_p0_is_the_min() {
        let xs = [40.0, 10.0, 90.0, 30.0];
        assert_eq!(percentile(&xs, 1.0), 90.0);
        assert_eq!(percentile(&xs, 0.0), 10.0);
    }

    #[test]
    fn single_element_is_itself_at_every_fraction() {
        assert_eq!(percentile(&[42.0], 0.0), 42.0);
        assert_eq!(percentile(&[42.0], 0.5), 42.0);
        assert_eq!(percentile(&[42.0], 1.0), 42.0);
    }
}
