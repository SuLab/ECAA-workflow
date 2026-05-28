//! Shared confidence-gate thresholds used by intake classification.
//!
//! These boundaries are the same semantic value in three places — duplicating
//! the literal `0.5` in `cli/chat.rs` (auto-proceed + confirmation) and
//! `core/workflow_contracts/from_intake.rs` (uncertainty flag) silently
//! created drift risk. Centralized here.

/// Below this, the SME must explicitly confirm before the compiler emits.
/// Used in CLI chat and intake-to-workflow translation.
pub const CONFIDENCE_GATE_MEDIUM: f32 = 0.5;

/// At or above this, classification confidence is labeled "high" in
/// human-facing surfaces (e.g., the State Inspector's Plan tab).
pub const CONFIDENCE_GATE_HIGH: f32 = 0.7;

/// Below this, classification confidence is labeled "low" — triggers the
/// remediation proposer + Opus escalation (model_policy.rs).
pub const CONFIDENCE_GATE_LOW: f32 = 0.3;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_thresholds_are_ordered() {
        assert!(CONFIDENCE_GATE_LOW < CONFIDENCE_GATE_MEDIUM);
        assert!(CONFIDENCE_GATE_MEDIUM < CONFIDENCE_GATE_HIGH);
    }

    #[test]
    fn gate_thresholds_are_in_probability_range() {
        for g in [
            CONFIDENCE_GATE_LOW,
            CONFIDENCE_GATE_MEDIUM,
            CONFIDENCE_GATE_HIGH,
        ] {
            assert!(g > 0.0 && g < 1.0, "gate {} out of [0, 1]", g);
        }
    }
}
