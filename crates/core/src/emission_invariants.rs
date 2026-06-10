//! Architectural invariant.
//!
//! The compiler ALWAYS emits a package. Only the four human-required
//! conditions enumerated below prevent emission. Everything else becomes
//! a DAG task and is handled at execution time, not at emission time.
//!
//! There is one additional deterministic, non-SME-facing gate that is
//! defense-in-depth rather than a human decision: emission fails when a
//! task's container reference is not digest-pinned. That check is
//! `crate::emitter::validate_container_digests_pinned`, invoked as the
//! first statement of `crate::emitter::emit_package` before any IO. It is
//! the 5th emission-blocking condition; it is kept OUT of the array below
//! because that array enumerates only the four SME/operator-facing
//! conditions (the architectural "only four conditions block emission"
//! rule is about those human-required gates).
//!
//! This module is the load-bearing canonical statement of the four-
//! condition rule. The parity test in
//! `crates/core/tests/misc/four_conditions_parity.rs` checks the const
//! against a vendored fixture so drift is caught by the local gate (this
//! slim OSS surface has no CI).

/// The four human-required conditions that prevent the compiler
/// from emitting a package. Order is stable; numbering matches
/// grant v19 §A.S2.
pub const FOUR_CONDITIONS_PREVENTING_EMISSION: [&str; 4] = [
    "Missing or contradictory SME intent that cannot be classified into any modality",
    "Deterministic schema-validation failure on a required intake field where no default exists",
    "Explicit SME rejection at the confirmation gate (`reject` endpoint)",
    "Explicit operator kill-switch (an emission-side analogue to ECAA_GIT_ENABLED=0, possibly unwired today)",
];
