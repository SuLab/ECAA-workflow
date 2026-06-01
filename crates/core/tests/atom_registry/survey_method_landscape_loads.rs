//! The `survey_method_landscape` atom loads from the registry, is
//! agent-assigned, carries the locator-anchored literature validators,
//! and declares a bounded egress allowlist (non-Bridge) plus the
//! `retrieval_tools` attribute the harness reads to inject the
//! literature runbook.

use ecaa_workflow_core::atom::{AtomAssignee, NetworkPolicy, SafetyLevel};
use ecaa_workflow_core::atom_registry::AtomRegistry;
use std::path::PathBuf;

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

#[test]
fn survey_method_landscape_atom_loads_and_is_agent_assigned() {
    let reg = AtomRegistry::load_from_dir(&config_root().join("stage-atoms"))
        .expect("registry loads with the new atom");
    let atom = reg
        .get("survey_method_landscape")
        .expect("survey_method_landscape atom must be registered");

    // Agent-assigned: the execution agent runs the retrieval.
    assert_eq!(atom.assignee, AtomAssignee::Agent);

    // Locator-anchored literature validators present (Wave-1 obligations
    // only; later waves append claim_support_satisfied / doc_page_matches_tool).
    assert!(
        atom.validators.iter().any(|v| v == "source_resolves"),
        "must carry source_resolves; got {:?}",
        atom.validators
    );
    assert!(
        atom.validators
            .iter()
            .any(|v| v == "evidence_quote_substring_match"),
        "must carry evidence_quote_substring_match; got {:?}",
        atom.validators
    );
    assert!(
        atom.validators
            .iter()
            .any(|v| v == "redistributable_or_marked"),
        "must carry redistributable_or_marked; got {:?}",
        atom.validators
    );

    // Bounded egress allowlist is declared (must be non-Bridge: a
    // `None { allowlist }` policy, not the inherited bridge network).
    assert_eq!(atom.safety.level, SafetyLevel::Network);
    match &atom.safety.network {
        NetworkPolicy::None { allowlist } => {
            assert!(
                !allowlist.is_empty(),
                "survey atom must declare a non-empty egress allowlist"
            );
        }
        other => panic!("survey atom must declare a non-Bridge allowlist, got {other:?}"),
    }

    // The harness reads `retrieval_tools` to inject the literature runbook.
    assert!(
        atom.attributes.contains_key("retrieval_tools"),
        "survey atom must declare retrieval_tools attribute"
    );
}
