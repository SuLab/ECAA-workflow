//! G4 — atom-registry governance load gate. An Exec-level atom that
//! does not declare `governance.status: reviewed` is refused by
//! `validate_consistency`; the catalog (whose only Exec atom carries
//! `governance: reviewed`) passes the same gate.

use ecaa_workflow_core::atom::{
    AtomDefinition, CodeExecution, SafetyLevel, SafetyPolicy, SandboxRequirement,
};
use ecaa_workflow_core::atom_registry::AtomRegistry;
use std::path::{Path, PathBuf};

fn config_stage_atoms() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/stage-atoms")
}

#[test]
fn real_catalog_passes_governance_gate() {
    let reg = AtomRegistry::load_from_dir(&config_stage_atoms()).expect("real catalog must load");
    reg.validate_consistency().expect(
        "real catalog must pass governance lint: its Exec atom carries governance.status: reviewed",
    );
}

#[test]
fn exec_atom_without_reviewed_governance_is_refused() {
    // An Exec atom that lint-passes validate_atom_safety
    // (sandbox != None + GeneratedByAgent) but carries NO reviewed
    // governance block. with_promoted_overlay is the public mutation
    // API; validate_consistency is the load-time gate.
    let mut exec = AtomDefinition::test_default("exec_no_governance_probe");
    exec.safety = SafetyPolicy {
        level: SafetyLevel::Exec,
        network: ecaa_workflow_core::atom::NetworkPolicy::None { allowlist: vec![] },
        code_execution: CodeExecution::GeneratedByAgent,
        sandbox: SandboxRequirement::ProcessIsolation,
        provisioning: ecaa_workflow_core::atom::ProvisioningPolicy::DeclaredOnly,
        controlled_access: false,
    };
    // governance left None.
    let registry = AtomRegistry::default().with_promoted_overlay([exec]);

    let err = registry
        .validate_consistency()
        .expect_err("an Exec atom with no reviewed governance must be refused");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("governance lint failed"),
        "expected governance-lint failure, got: {msg}"
    );
}
