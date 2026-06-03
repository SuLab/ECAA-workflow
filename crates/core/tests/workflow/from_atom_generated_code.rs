//! G1 — an atom with code_execution: generated_by_agent lowers to
//! Implementation::GeneratedCode with a non-Unreviewed review_status,
//! so the harness sandbox enforcer wraps it and the strict planner
//! sweep does not refuse it.

use ecaa_workflow_core::atom::{
    AtomDefinition, CodeExecution, NetworkPolicy, ProvisioningPolicy, SafetyLevel, SafetyPolicy,
    SandboxRequirement,
};
use ecaa_workflow_core::workflow_contracts::implementation::{Implementation, ReviewStatus};
use ecaa_workflow_core::workflow_contracts::task_node::TaskNode;

fn exec_atom() -> AtomDefinition {
    let mut a = AtomDefinition::test_default("agent_generated_analysis");
    a.safety = SafetyPolicy {
        level: SafetyLevel::Exec,
        network: NetworkPolicy::None { allowlist: vec![] },
        code_execution: CodeExecution::GeneratedByAgent,
        sandbox: SandboxRequirement::ProcessIsolation,
        provisioning: ProvisioningPolicy::DeclaredOnly,
        controlled_access: false,
    };
    a.preferred_container = None; // Exec lowers on code_execution, not a container.
    a
}

#[test]
fn generated_by_agent_lowers_to_generated_code() {
    let node = TaskNode::from_atom(&exec_atom());
    match node.implementation {
        Implementation::GeneratedCode { review_status, .. } => {
            assert_ne!(
                review_status,
                ReviewStatus::Unreviewed,
                "Exec atom must seed a non-Unreviewed review_status so the strict planner sweep \
                 does not refuse it"
            );
        }
        other => panic!("expected GeneratedCode, got {:?}", other),
    }
}
