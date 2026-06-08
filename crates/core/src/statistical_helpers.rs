//! Pure, deterministic descriptive-statistics helpers shared by
//! `claim_verifier` (numeric distribution claims) and the harness
//! (domain-correctness assertion arms).
//!
//! # Invariants
//! - **No NaN escapes.** Empty / degenerate inputs return all-zero
//!   `DistributionStats` rather than NaN so downstream comparisons are
//!   total. A single-element set has zero spread (population moments).
//! - **Deterministic.** No randomness, no clock, no allocation order
//!   dependence; percentiles use linear interpolation over the sorted
//!   rank `(n - 1) * q` so two runs over the same slice are identical.
//! - **Method-neutral.** These helpers compute statistics; they never
//!   carry or imply a threshold. Thresholds live in the validation
//!   contract (operator-authored), never in code.

/// Descriptive statistics over a numeric sample. All moments are
/// population (divide by `n`, not `n - 1`) so the values are total for
/// any non-empty input. Empty input yields all zeros.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DistributionStats {
    /// Arithmetic mean.
    pub mean: f64,
    /// Population standard deviation.
    pub stdev: f64,
    /// Population skewness (third standardized moment); 0 when stdev is 0.
    pub skewness: f64,
    /// Population excess-free kurtosis (fourth standardized moment);
    /// 0 when stdev is 0.
    pub kurtosis: f64,
    /// 5th percentile (linear interpolation over sorted rank).
    pub p5: f64,
    /// 50th percentile (median).
    pub p50: f64,
    /// 95th percentile.
    pub p95: f64,
}

impl DistributionStats {
    /// All-zero stats — returned for empty input so callers never see NaN.
    fn zero() -> Self {
        DistributionStats {
            mean: 0.0,
            stdev: 0.0,
            skewness: 0.0,
            kurtosis: 0.0,
            p5: 0.0,
            p50: 0.0,
            p95: 0.0,
        }
    }
}

/// Linear-interpolation percentile over a pre-sorted ascending slice.
/// `q` in `[0.0, 1.0]`. Returns 0.0 for an empty slice.
fn percentile_sorted(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let q = q.clamp(0.0, 1.0);
    let rank = (sorted.len() as f64 - 1.0) * q;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = rank - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

/// Compute descriptive statistics over `values`. Empty input returns
/// `DistributionStats::zero()`. NaN values in the input are filtered out
/// (a NaN sample is treated as absent) so the result is always finite.
pub fn compute_distribution_stats(values: &[f64]) -> DistributionStats {
    let mut clean: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    let n = clean.len();
    if n == 0 {
        return DistributionStats::zero();
    }
    let mean = clean.iter().sum::<f64>() / n as f64;
    let var = clean.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
    let stdev = var.sqrt();
    let (skewness, kurtosis) = if stdev > 0.0 {
        let m3 = clean.iter().map(|v| ((v - mean) / stdev).powi(3)).sum::<f64>() / n as f64;
        let m4 = clean.iter().map(|v| ((v - mean) / stdev).powi(4)).sum::<f64>() / n as f64;
        (m3, m4)
    } else {
        (0.0, 0.0)
    };
    clean.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    DistributionStats {
        mean,
        stdev,
        skewness,
        kurtosis,
        p5: percentile_sorted(&clean, 0.05),
        p50: percentile_sorted(&clean, 0.50),
        p95: percentile_sorted(&clean, 0.95),
    }
}

/// True when `observed` is inside `[reference_min, reference_max]` after
/// widening the band by `zscore_tolerance` fractions of the band width on
/// each side. A `zscore_tolerance` of 0.0 is an exact closed-interval
/// check. The widening is symmetric and band-relative so a contract can
/// say "within 10% of the band edges" without naming an absolute value.
pub fn is_within_reference_range(
    observed: f64,
    reference_min: f64,
    reference_max: f64,
    zscore_tolerance: f64,
) -> bool {
    if !observed.is_finite() {
        return false;
    }
    let (lo, hi) = if reference_min <= reference_max {
        (reference_min, reference_max)
    } else {
        (reference_max, reference_min)
    };
    let pad = (hi - lo).abs() * zscore_tolerance.max(0.0);
    observed >= lo - pad && observed <= hi + pad
}

/// Return the indices of `values` whose absolute z-score exceeds
/// `threshold`. Uses population mean/stdev. A zero-spread sample (all
/// equal, or a single element) has no outliers. Indices are returned in
/// ascending order for determinism.
pub fn detect_outliers_zscore(values: &[f64], threshold: f64) -> Vec<usize> {
    let stats = compute_distribution_stats(values);
    if stats.stdev <= 0.0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, v) in values.iter().enumerate() {
        if !v.is_finite() {
            continue;
        }
        let z = (v - stats.mean).abs() / stats.stdev;
        if z > threshold {
            out.push(i);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn stats_on_symmetric_set() {
        // 1..=9 — mean 5, population stdev sqrt(60/9)=2.5819889
        let v: Vec<f64> = (1..=9).map(|x| x as f64).collect();
        let s = compute_distribution_stats(&v);
        assert!(approx(s.mean, 5.0, 1e-9), "mean {}", s.mean);
        assert!(approx(s.stdev, 2.581988897, 1e-6), "stdev {}", s.stdev);
        assert!(approx(s.skewness, 0.0, 1e-9), "skewness {}", s.skewness);
        assert!(approx(s.p50, 5.0, 1e-9), "p50 {}", s.p50);
        // Linear-interpolation percentiles over rank (n-1): p5 of 1..9 = 1.4
        assert!(approx(s.p5, 1.4, 1e-9), "p5 {}", s.p5);
        assert!(approx(s.p95, 8.6, 1e-9), "p95 {}", s.p95);
    }

    #[test]
    fn empty_input_is_all_zero_not_nan() {
        let s = compute_distribution_stats(&[]);
        assert_eq!(s.mean, 0.0);
        assert_eq!(s.stdev, 0.0);
        assert_eq!(s.skewness, 0.0);
        assert_eq!(s.kurtosis, 0.0);
        assert_eq!(s.p5, 0.0);
        assert_eq!(s.p50, 0.0);
        assert_eq!(s.p95, 0.0);
    }

    #[test]
    fn single_value_has_zero_spread() {
        let s = compute_distribution_stats(&[42.0]);
        assert_eq!(s.mean, 42.0);
        assert_eq!(s.stdev, 0.0);
        assert_eq!(s.skewness, 0.0);
        assert_eq!(s.kurtosis, 0.0);
        assert_eq!(s.p50, 42.0);
    }

    #[test]
    fn reference_range_exact_and_padded() {
        assert!(is_within_reference_range(0.5, 0.0, 1.0, 0.0));
        assert!(!is_within_reference_range(1.1, 0.0, 1.0, 0.0));
        // 10% band padding widens [0,1] to [-0.1, 1.1].
        assert!(is_within_reference_range(1.05, 0.0, 1.0, 0.10));
        assert!(!is_within_reference_range(1.2, 0.0, 1.0, 0.10));
        // Reversed bounds are normalized.
        assert!(is_within_reference_range(0.5, 1.0, 0.0, 0.0));
        // NaN observed is never in range.
        assert!(!is_within_reference_range(f64::NAN, 0.0, 1.0, 1.0));
    }

    #[test]
    fn outliers_detected_by_zscore() {
        // 0,0,0,0,0,0,0,0,0,100 — the 100 is the only outlier at z>2.
        let mut v = vec![0.0; 9];
        v.push(100.0);
        let out = detect_outliers_zscore(&v, 2.0);
        assert_eq!(out, vec![9]);
    }

    #[test]
    fn zero_spread_has_no_outliers() {
        let out = detect_outliers_zscore(&[7.0, 7.0, 7.0], 0.5);
        assert!(out.is_empty());
    }
}
