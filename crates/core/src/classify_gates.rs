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

// The confidence gates must form a strictly increasing ladder inside the
// unit interval. These are compile-time invariants over `const` values, so
// they are enforced as `const` assertions rather than runtime tests.
const _: () = assert!(
    CONFIDENCE_GATE_LOW < CONFIDENCE_GATE_MEDIUM,
    "CONFIDENCE_GATE_LOW must be below CONFIDENCE_GATE_MEDIUM"
);
const _: () = assert!(
    CONFIDENCE_GATE_MEDIUM < CONFIDENCE_GATE_HIGH,
    "CONFIDENCE_GATE_MEDIUM must be below CONFIDENCE_GATE_HIGH"
);
const _: () = assert!(
    CONFIDENCE_GATE_LOW > 0.0 && CONFIDENCE_GATE_HIGH < 1.0,
    "confidence gates must lie within (0, 1)"
);
