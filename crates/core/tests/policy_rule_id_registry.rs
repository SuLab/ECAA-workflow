//! Typed `PolicyRuleId` newtype + registry validation.
//!
//! The newtype wraps a stable id string but enforces at *construction*
//! time that the id is registered in `config/assumption-policy.yaml`'s
//! `policy_rules:` block. Sites that don't have access to the
//! registry at construction time use [`PolicyRuleId::unchecked`] (see
//! [`crate::workflow_contracts::policy_rule_id`]).

use scripps_workflow_core::assumption_policy::AssumptionPolicyTable;
use scripps_workflow_core::workflow_contracts::policy_rule_id::PolicyRuleId;
use std::path::Path;

#[test]
fn known_policy_rule_constructs() {
    let table =
        AssumptionPolicyTable::load_from_path(Path::new("../../config/assumption-policy.yaml"))
            .expect("load assumption-policy.yaml");
    let id = PolicyRuleId::new("phi_strict_v1", &table).expect("registered rule");
    assert_eq!(id.as_str(), "phi_strict_v1");
}

#[test]
fn unknown_policy_rule_rejected() {
    let table =
        AssumptionPolicyTable::load_from_path(Path::new("../../config/assumption-policy.yaml"))
            .expect("load assumption-policy.yaml");
    let err = PolicyRuleId::new("nonexistent_rule", &table).expect_err("unknown rule");
    assert!(format!("{err}").contains("not in registry"));
}
