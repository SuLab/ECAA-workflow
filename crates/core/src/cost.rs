//! Typed cost arithmetic in micro-USD.
//!
//! Replaces f64 cost arithmetic which silently propagated NaN/INF from
//! agent-reported usage and pricing-table fail-open paths.
//!
//! Unit: 1 USD = 1_000_000 micro-USD. Saturating arithmetic prevents
//! overflow; per-call cap of $1000 prevents agent-reported absurdities
//! from polluting the total.

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MICRO_USD_PER_USD: u64 = 1_000_000;

/// Per-call cap. A single LLM/agent invocation cannot register more
/// than this cost; values above are clamped + logged. Bound chosen
/// well above any realistic single-call cost so legitimate operations
/// never hit it, while agent-reported `u64::MAX` does.
pub const MAX_CALL_MICRO_USD: u64 = 1_000 * MICRO_USD_PER_USD;

#[derive(Debug, Error)]
/// CostError discriminant.
pub enum CostError {
    #[error("cost value must be finite, got {0}")]
    /// NotFinite variant.
    NotFinite(f64),
    #[error("cost value must be non-negative, got {0}")]
    /// Negative variant.
    Negative(f64),
}

/// A cost in micro-USD. Strongly-typed so accidental `f64` arithmetic
/// can't silently propagate NaN/INF into telemetry.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct Cost(u64);

impl Cost {
    /// Zero constant.
    pub const ZERO: Cost = Cost(0);

    /// Construct from a literal micro-USD count. Use only when you
    /// already have an integer source; for f64 amounts use `from_usd_f64`.
    pub const fn from_micro_usd(micro: u64) -> Cost {
        Cost(micro)
    }

    /// Construct from a USD-denominated f64. Rejects non-finite +
    /// negative values.
    pub fn from_usd_f64(usd: f64) -> Result<Cost, CostError> {
        if !usd.is_finite() {
            return Err(CostError::NotFinite(usd));
        }
        if usd < 0.0 {
            return Err(CostError::Negative(usd));
        }
        let micro = (usd * MICRO_USD_PER_USD as f64) as u64;
        Ok(Cost(micro))
    }

    /// Compute `tokens × per_token_micro` with saturating multiplication
    /// AND per-call cap. Closes the agent-reported token-inflation
    /// failure mode (audit P1-213).
    ///
    /// Emits `target = "swfc::cost_cap"` at warn level when the per-call
    /// cap fires, so operators can alert on agent-reported absurdities
    /// polluting telemetry. Conceptual metrics counter:
    /// `swfc_cost_cap_total`. The structured-log channel is the source
    /// of truth until a metrics framework is wired in.
    pub fn from_token_count(tokens: u64, per_token_micro: Cost) -> Cost {
        let raw = (tokens as u128).saturating_mul(per_token_micro.0 as u128);
        let was_capped = raw > MAX_CALL_MICRO_USD as u128;
        let capped = raw.min(MAX_CALL_MICRO_USD as u128);
        if was_capped {
            tracing::warn!(
                target: "swfc::cost_cap",
                reported = tokens,
                per_token_micro_usd = per_token_micro.0,
                raw_micro_usd = %raw,
                capped_at = MAX_CALL_MICRO_USD,
                "implausible token count from agent; capped at per-call ceiling"
            );
        }
        Cost(capped as u64)
    }

    /// Saturating add.
    pub fn saturating_add(self, other: Cost) -> Cost {
        Cost(self.0.saturating_add(other.0))
    }

    /// As usd.
    pub fn as_usd(self) -> f64 {
        self.0 as f64 / MICRO_USD_PER_USD as f64
    }

    /// As micro usd.
    pub fn as_micro_usd(self) -> u64 {
        self.0
    }

    /// Returns true if this cost exceeds the per-call cap. Callers
    /// should `tracing::warn!` when this fires.
    pub fn is_at_call_cap(self) -> bool {
        self.0 >= MAX_CALL_MICRO_USD
    }
}

impl std::fmt::Display for Cost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "${:.6}", self.as_usd())
    }
}

impl std::ops::Add for Cost {
    type Output = Cost;
    /// Note: uses saturating_add. Cost arithmetic never silently
    /// overflows. (Counter-argument: `+` should panic on overflow per
    /// Rust convention. Counter-counter: cost arithmetic operates on
    /// agent-reported values whose corruption shouldn't crash the
    /// process. Document this contract loudly.)
    fn add(self, other: Cost) -> Cost {
        self.saturating_add(other)
    }
}

impl std::iter::Sum for Cost {
    fn sum<I: Iterator<Item = Cost>>(iter: I) -> Cost {
        iter.fold(Cost::ZERO, |a, b| a.saturating_add(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_nan() {
        assert!(matches!(
            Cost::from_usd_f64(f64::NAN),
            Err(CostError::NotFinite(_))
        ));
    }

    #[test]
    fn rejects_infinity() {
        assert!(matches!(
            Cost::from_usd_f64(f64::INFINITY),
            Err(CostError::NotFinite(_))
        ));
        assert!(matches!(
            Cost::from_usd_f64(f64::NEG_INFINITY),
            Err(CostError::NotFinite(_))
        ));
    }

    #[test]
    fn rejects_negative() {
        assert!(matches!(
            Cost::from_usd_f64(-0.01),
            Err(CostError::Negative(_))
        ));
    }

    #[test]
    fn accepts_zero() {
        let c = Cost::from_usd_f64(0.0).unwrap();
        assert_eq!(c.as_micro_usd(), 0);
        assert_eq!(c, Cost::ZERO);
    }

    #[test]
    fn round_trip_usd_micro() {
        let c = Cost::from_usd_f64(1.234567).unwrap();
        assert_eq!(c.as_micro_usd(), 1_234_567);
        assert!((c.as_usd() - 1.234567).abs() < 1e-9);
    }

    #[test]
    fn saturating_add_at_max() {
        let max = Cost::from_micro_usd(u64::MAX);
        let one = Cost::from_micro_usd(1);
        assert_eq!(max.saturating_add(one), max);
        assert_eq!(max + one, max);
    }

    #[test]
    fn from_token_count_caps_at_max_call() {
        let per_token = Cost::from_usd_f64(3.0e-6).unwrap();
        let cost = Cost::from_token_count(u64::MAX, per_token);
        assert!(cost.as_micro_usd() <= MAX_CALL_MICRO_USD);
        assert!(cost.is_at_call_cap());
    }

    #[test]
    fn from_token_count_legitimate_value_uncapped() {
        let per_input_token = Cost::from_usd_f64(3.0e-6).unwrap();
        let cost = Cost::from_token_count(1_000_000, per_input_token);
        assert!(!cost.is_at_call_cap());
        let usd = cost.as_usd();
        assert!((usd - 3.0).abs() < 0.001, "expected ~$3, got {usd}");
    }

    #[test]
    fn display_formats_usd() {
        let c = Cost::from_usd_f64(0.05).unwrap();
        assert_eq!(c.to_string(), "$0.050000");
    }

    #[test]
    fn iter_sum_saturates() {
        let costs: Vec<Cost> = vec![Cost::from_micro_usd(u64::MAX / 2); 4];
        let total: Cost = costs.into_iter().sum();
        assert_eq!(total, Cost::from_micro_usd(u64::MAX));
    }

    #[test]
    fn serde_round_trip() {
        let c = Cost::from_usd_f64(1.5).unwrap();
        let j = serde_json::to_string(&c).unwrap();
        assert_eq!(j, "1500000");
        let parsed: Cost = serde_json::from_str(&j).unwrap();
        assert_eq!(parsed, c);
    }
}
