//! G1 — the agent_generated_analysis Exec atom lint-passes at
//! registry load (Exec branch: sandbox != None + GeneratedByAgent)
//! and carries reviewed governance (G4 load gate).

use ecaa_workflow_core::atom::{CodeExecution, GovernanceStatus, SafetyLevel, SandboxRequirement};
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
fn agent_generated_analysis_atom_lint_passes() {
    let reg = AtomRegistry::load_from_dir(&config_stage_atoms())
        .expect("registry must load with the Exec atom present");
    let atom = reg
        .get("agent_generated_analysis")
        .expect("agent_generated_analysis atom must be in the catalog");
    assert_eq!(atom.safety.level, SafetyLevel::Exec);
    assert_eq!(atom.safety.code_execution, CodeExecution::GeneratedByAgent);
    assert_ne!(atom.safety.sandbox, SandboxRequirement::None);
    // G4 — the Exec atom must declare reviewed governance to load.
    assert_eq!(
        atom.governance.as_ref().map(|g| g.status),
        Some(GovernanceStatus::Reviewed),
        "Exec atom must carry governance.status: reviewed"
    );
}
