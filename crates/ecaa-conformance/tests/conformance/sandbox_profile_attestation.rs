//! G5 — package-level sandbox-profile attestation. For every Exec-level
//! atom in the catalog, the defence-in-depth chain must hold across the
//! whole stack: rollup requires sandbox, the lowered implementation is
//! reviewed GeneratedCode, and a no-sandbox executor refuses dispatch.
//! Offline: drives aggregate_for_package + enforce_safety_policy +
//! the from_atom lowering as oracles. Iterates over ALL Exec atoms so
//! future Exec atoms are auto-covered.

use ecaa_workflow_core::atom::{
    AtomDefinition, NetworkPolicy, SafetyLevel, SandboxRequirement,
};
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::atom_safety::aggregate_for_package;
use ecaa_workflow_core::blocker::BlockerKind;
use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
use ecaa_workflow_core::workflow_contracts::implementation::{Implementation, ReviewStatus};
use ecaa_workflow_core::workflow_contracts::task_node::TaskNode;
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

/// Build a dispatch-shaped `Task` carrying the atom's safety policy +
/// source-atom back-reference, mirroring the emit-time threading. `Task`
/// has no public constructor; the struct literal matches the harness
/// `safety_tests::task_with_safety` shape.
fn task_from_atom(atom: &AtomDefinition) -> Task {
    Task {
        kind: TaskKind::Computation,
        state: TaskState::Pending,
        depends_on: vec![],
        assignee: Assignee::Agent,
        description: format!("attestation probe for {}", atom.id),
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
fn exec_atoms_satisfy_full_sandbox_attestation() {
    let reg = AtomRegistry::load_from_dir(&stage_atoms_dir()).expect("atom registry must load");

    let exec_atoms: Vec<&AtomDefinition> = reg
        .iter()
        .map(|(_, a)| a)
        .filter(|a| a.safety.level == SafetyLevel::Exec)
        .collect();

    // Non-vacuity guard: G1 ships at least one Exec atom.
    assert!(
        !exec_atoms.is_empty(),
        "expected >=1 Exec atom in the catalog (G1); attestation gate would be vacuous"
    );

    for atom in &exec_atoms {
        // (a) Package rollup requires sandbox when this Exec atom is in use.
        let agg = aggregate_for_package(&[atom], vec![]);
        assert!(
            agg.package_requires_sandbox,
            "atom {}: Exec in package but package_requires_sandbox == false",
            atom.id
        );
        assert_eq!(
            agg.package_max_safety_level, "Exec",
            "atom {}: rollup level",
            atom.id
        );

        // (b) Lowered implementation is reviewed GeneratedCode.
        let node = TaskNode::from_atom(atom);
        match node.implementation {
            Implementation::GeneratedCode { review_status, .. } => assert_ne!(
                review_status,
                ReviewStatus::Unreviewed,
                "atom {}: Exec task lowered to Unreviewed GeneratedCode",
                atom.id
            ),
            other => panic!(
                "atom {}: Exec must lower to GeneratedCode, got {:?}",
                atom.id, other
            ),
        }

        // (c) No-sandbox executor refuses dispatch (fail-closed).
        let task = task_from_atom(atom);
        let caps = ExecutorCapabilities {
            sandbox: SandboxRequirement::None,
            network: NetworkPolicy::None { allowlist: vec![] },
            kind: "local",
            forwards_to_external_llm: true,
        };
        assert!(
            matches!(
                enforce_safety_policy(&task, &caps),
                Some(BlockerKind::SandboxRequired { .. })
            ),
            "atom {}: no-sandbox executor did not refuse Exec dispatch",
            atom.id
        );

        // (d) Exec atom must be network-denied (no Bridge egress); the
        // deny form (None with empty allowlist) is allowed.
        assert!(
            matches!(atom.safety.network, NetworkPolicy::None { .. }),
            "atom {}: Exec atom must be network-denied (no Bridge egress)",
            atom.id
        );
    }
}
