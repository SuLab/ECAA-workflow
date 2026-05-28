//! Registry-load integration tests for safety lint.
//!
//! Task 2.2 wires `validate_atom_safety` into
//! `AtomRegistry::validate_consistency` so a registry refuses to load
//! any atom carrying a per-level or cross-field safety violation. The
//! tests below verify both the lint function (callable in isolation)
//! and the registry-level gate (lint fires through the consistency
//! check, error message identifies the rule).

use scripps_workflow_core::atom::{AtomDefinition, CodeExecution, NetworkPolicy, SafetyLevel};
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::atom_safety::{validate_atom_safety, SafetyConsistencyError};

#[test]
fn happy_path_compute_atom_passes() {
    let atom = AtomDefinition::test_default("happy");
    let errors = validate_atom_safety(&atom);
    assert!(
        errors.is_empty(),
        "default atom should pass lint: {errors:?}"
    );
}

#[test]
fn bad_atom_with_generated_code_and_compute_level_fails() {
    let mut atom = AtomDefinition::test_default("bad");
    atom.safety.code_execution = CodeExecution::GeneratedByAgent;
    let errors = validate_atom_safety(&atom);
    assert!(!errors.is_empty());
    assert!(errors.iter().any(|e| matches!(
        e,
        SafetyConsistencyError::GeneratedCodeWithoutExecLevel { .. }
    )));
    // Also expects the per-Compute-level rule to fire alongside the
    // cross-field implication rule — the lint surfaces all
    // violations, not just the first.
    assert!(errors
        .iter()
        .any(|e| matches!(e, SafetyConsistencyError::ComputeAtomGeneratedCode { .. })));
}

#[test]
fn registry_refuses_atom_with_safety_violation() {
    // Build an empty registry via Default + add one deliberately-bad
    // atom via `with_promoted_overlay` (the only public mutation API
    // on AtomRegistry — it appends overlay atoms onto the base set).
    // Bad atom: Compute level + Bridge network = ComputeAtomHasNetwork.
    let registry = AtomRegistry::default();
    let mut bad_atom = AtomDefinition::test_default("bad_compute");
    bad_atom.safety.level = SafetyLevel::Compute;
    bad_atom.safety.network = NetworkPolicy::Bridge;
    let registry = registry.with_promoted_overlay([bad_atom]);

    let result = registry.validate_consistency();
    assert!(result.is_err(), "expected lint failure, got Ok(())");
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("Compute level requires no network egress"),
        "expected ComputeAtomHasNetwork lint error in: {msg}"
    );
    // Aggregated-error envelope wraps every violation in one anyhow!.
    assert!(
        msg.contains("atom registry safety lint failed"),
        "expected aggregated lint envelope in: {msg}"
    );
}
