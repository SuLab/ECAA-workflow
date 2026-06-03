//! G2 — the controlled_access_data_acquisition atom carries
//! controlled_access: true, loads clean, and is level-gate
//! pass-through (the LLM-forwarding refusal lives in the harness
//! collect_safety_policy_refusals layer, exercised by Task 9's harness
//! test; the conformance crate cannot reach that private binary fn).

use ecaa_workflow_core::atom::{
    AtomDefinition, NetworkPolicy, SafetyLevel, SandboxRequirement,
};
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::atom_safety::aggregate_for_package;
use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
use ecaa_workflow_harness::executor::{enforce_safety_policy, ExecutorCapabilities};
use std::path::PathBuf;

fn stage_atoms_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/stage-atoms")
}

fn task_from_atom(atom: &AtomDefinition) -> Task {
    Task {
        kind: TaskKind::Computation,
        state: TaskState::Pending,
        depends_on: vec![],
        assignee: Assignee::Agent,
        description: format!("controlled-access probe for {}", atom.id),
        spec: None,
        resolution: None,
        result_ref: None,
        resource_class: ResourceClass::CpuHeavy,
        requires_sme_review: false,
        required_artifacts: vec![],
        container: None,
        source_atom_id: Some(atom.id.clone()),
        safety: atom.safety.clone(),
    }
}

#[test]
fn controlled_access_atom_is_flagged_and_refused() {
    let reg = AtomRegistry::load_from_dir(&stage_atoms_dir()).expect("registry loads");
    let atom = reg
        .get("controlled_access_data_acquisition")
        .expect("controlled_access_data_acquisition atom must exist");
    assert!(
        atom.safety.controlled_access,
        "atom must declare controlled_access: true"
    );
    // Compute-level: the controlled-access flag does not raise the
    // package safety level (it is an orthogonal egress constraint).
    assert_eq!(atom.safety.level, SafetyLevel::Compute);

    // Surfaces in the package safety aggregate (the controlled-access
    // task is present; rollup is Compute, no sandbox required).
    let agg = aggregate_for_package(&[atom], vec![]);
    assert_eq!(agg.package_max_safety_level, "Compute");
    assert!(!agg.package_requires_sandbox);

    // enforce_safety_policy itself does not check controlled_access
    // (that is the harness collect_safety_policy_refusals layer); the
    // SafetyLevel gate must pass for a Compute controlled-access atom.
    let task = task_from_atom(atom);
    let caps = ExecutorCapabilities {
        sandbox: SandboxRequirement::None,
        network: NetworkPolicy::None { allowlist: vec![] },
        kind: "local",
        forwards_to_external_llm: true,
    };
    assert!(
        enforce_safety_policy(&task, &caps).is_none(),
        "controlled-access atom is Compute-level; no sandbox/network blocker expected"
    );
}
