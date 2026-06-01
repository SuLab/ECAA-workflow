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

/// The non-PubMed validators (`claim_support_satisfied`, `doc_page_matches_tool`)
/// are attached once their obligations land.
#[test]
fn survey_method_landscape_carries_non_pubmed_validators() {
    let reg = AtomRegistry::load_from_dir(&config_root().join("stage-atoms"))
        .expect("registry loads with the new atom");
    let atom = reg
        .get("survey_method_landscape")
        .expect("survey_method_landscape atom must be registered");
    for v in ["claim_support_satisfied", "doc_page_matches_tool"] {
        assert!(
            atom.validators.iter().any(|x| x == v),
            "survey atom must carry {v}; got {:?}",
            atom.validators
        );
    }
}

/// The survey atom's bounded egress allowlist must be a superset of every
/// host the retrieval-routes config declares for the literature + tool-doc
/// source classes — otherwise `enforce_safety_policy` would refuse a route
/// the agent is told to use.
#[test]
fn survey_egress_allowlist_covers_retrieval_routes() {
    use ecaa_workflow_core::retrieval_routes::RetrievalRoutes;
    let cfg = config_root();
    let reg = AtomRegistry::load_from_dir(&cfg.join("stage-atoms")).expect("registry loads");
    let atom = reg
        .get("survey_method_landscape")
        .expect("survey atom present");
    let allow = match &atom.safety.network {
        NetworkPolicy::None { allowlist } => allowlist.clone(),
        other => panic!("expected None{{allowlist}}, got {other:?}"),
    };
    let routes = RetrievalRoutes::load(&cfg.join("downstream-policy/retrieval-routes.json"))
        .expect("retrieval-routes.json loads");
    for class in [
        "primary_literature",
        "conference_proceedings",
        "tool_documentation",
    ] {
        for h in routes.hosts_for_class(class) {
            assert!(
                allow.contains(&h),
                "survey egress allowlist missing host {h} (class {class}); allowlist={allow:?}"
            );
        }
    }
}
