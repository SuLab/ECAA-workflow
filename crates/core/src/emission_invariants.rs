//! Architectural invariant.
//!
//! The compiler ALWAYS emits a package. Only the four conditions
//! enumerated below prevent emission. Everything else becomes a DAG
//! task and is handled at execution time, not at emission time.
//!
//! This module is the load-bearing canonical statement of the rule.
//! Grant v19 §A.S2 contains the same enumeration as prose; the
//! parity test in `crates/core/tests/four_conditions_parity.rs`
//! enforces grant↔code text alignment so drift in either is caught
//! in CI.

/// The four human-required conditions that prevent the compiler
/// from emitting a package. Order is stable; numbering matches
/// grant v19 §A.S2.
pub const FOUR_CONDITIONS_PREVENTING_EMISSION: [&str; 4] = [
    "Missing or contradictory SME intent that cannot be classified into any modality",
    "Deterministic schema-validation failure on a required intake field where no default exists",
    "Explicit SME rejection at the confirmation gate (`reject` endpoint)",
    "Explicit operator kill-switch (an emission-side analogue to ECAA_GIT_ENABLED=0, possibly unwired today)",
];
