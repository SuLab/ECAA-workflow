//! M7 — least-privilege default-deny posture proof.
//!
//! An atom with NO explicit `safety:` block must resolve to the
//! most-restrictive enforcement posture: no network egress, no
//! agent-generated code execution, and it must lint-pass
//! `validate_atom_safety`. The package-level rollup must NOT claim a
//! sandbox is required unless an Exec atom is actually present.
//!
//! Anchored on `atom_safety::validate_atom_safety` +
//! `aggregate_for_package` (the emit-time oracle) so a future change
//! to the default that silently opened egress or code-exec fails here.

use ecaa_workflow_core::atom::{
    AtomDefinition, CodeExecution, NetworkPolicy, SafetyLevel, SafetyPolicy, SandboxRequirement,
};
use ecaa_workflow_core::atom_safety::{aggregate_for_package, validate_atom_safety};

/// An atom YAML with no `safety:` key deserialises to this exact
/// shape via `#[serde(default)]` on the field. Construct the same
/// shape and assert the deny-by-default posture.
fn safety_less_atom() -> AtomDefinition {
    let a = AtomDefinition::test_default("no_safety_block");
    // test_default does not set `safety`, so it is SafetyPolicy::default().
    assert_eq!(
        a.safety,
        SafetyPolicy::default(),
        "test_default must use the SafetyPolicy default"
    );
    a
}

#[test]
fn default_safety_is_deny_by_default_no_egress() {
    let a = safety_less_atom();
    // Network: deny-all (None with empty allowlist).
    assert_eq!(
        a.safety.network,
        NetworkPolicy::None { allowlist: vec![] },
        "default network must be deny-all egress"
    );
}

#[test]
fn default_safety_forbids_agent_code_execution() {
    let a = safety_less_atom();
    assert_eq!(
        a.safety.code_execution,
        CodeExecution::None,
        "default must forbid agent-generated code execution"
    );
}

#[test]
fn default_safety_lint_passes() {
    let a = safety_less_atom();
    assert!(
        validate_atom_safety(&a).is_empty(),
        "default safety policy must lint-pass: {:?}",
        validate_atom_safety(&a)
    );
}

#[test]
fn default_package_rollup_does_not_require_sandbox() {
    let a = safety_less_atom();
    let agg = aggregate_for_package(&[&a], vec![]);
    assert!(
        !agg.package_requires_sandbox,
        "a safety-less package must not claim a sandbox is required"
    );
    assert_eq!(agg.package_max_network_policy, "None");
}

#[test]
fn exec_atom_flips_rollup_to_sandbox_required() {
    // Negative control: only an actual Exec atom requires sandbox.
    let mut exec = AtomDefinition::test_default("exec_probe");
    exec.safety = SafetyPolicy {
        level: SafetyLevel::Exec,
        network: NetworkPolicy::None { allowlist: vec![] },
        code_execution: CodeExecution::GeneratedByAgent,
        sandbox: SandboxRequirement::ProcessIsolation,
        provisioning: ecaa_workflow_core::atom::ProvisioningPolicy::DeclaredOnly,
        controlled_access: false,
    };
    assert!(
        validate_atom_safety(&exec).is_empty(),
        "exec probe must lint-pass"
    );
    let agg = aggregate_for_package(&[&exec], vec![]);
    assert!(
        agg.package_requires_sandbox,
        "Exec atom must flip package_requires_sandbox"
    );
    assert_eq!(agg.package_max_safety_level, "Exec");
}
