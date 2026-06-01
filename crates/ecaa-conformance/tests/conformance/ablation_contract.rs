//! §8.4 ablation contract — one flag at a time changes exactly one
//! sidecar (or one inline field); every other emitted sidecar stays
//! byte-identical to the un-ablated baseline.

use ecaa_workflow_core::ablation::AblationFlag;
use ecaa_workflow_core::emitter::{emit_package, EmitConfig};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config")
}

/// Build a real DAG via the v4 composer (mirrors wrroc_v05_conformance.rs).
fn minimal_dag() -> ecaa_workflow_core::dag::DAG {
    use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
    use ecaa_workflow_core::atom_registry::AtomRegistry;
    use ecaa_workflow_core::builder::{build_dag_from_composition, build_dag_from_workflow_dag};
    use ecaa_workflow_core::composer::compose_with_modalities_full;
    use ecaa_workflow_core::goal_spec::GoalSpec;
    let config = config_root();
    let atoms = AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("atoms");
    let archetypes =
        ArchetypeRegistry::load_from_dir(&config.join("archetypes")).expect("archetypes");
    let goal = GoalSpec {
        edam_data: "data:9999".into(),
        edam_format: None,
        modifiers: Default::default(),
        source_prose: Some("ablation contract fixture".into()),
        confidence: 0.0,
    };
    let out = compose_with_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        &["bulk_rnaseq"],
        None,
        None,
        None,
    )
    .expect("compose");
    if let Some(wf) = out.workflow_dag.as_ref() {
        build_dag_from_workflow_dag(wf, "ablation-fixture").expect("lower")
    } else {
        build_dag_from_composition(&out.composition, "ablation-fixture", &BTreeMap::new(), &[])
            .expect("compose")
    }
}

fn classification() -> ecaa_workflow_core::classify::ClassificationResult {
    use ecaa_workflow_core::classify::ClassificationResult;
    ClassificationResult {
        modality: "bulk_rnaseq".into(),
        taxonomy_path: "config/stage-taxonomies/rnaseq-de.yaml".into(),
        domain: "computational biology".into(),
        workflow_description: "ablation fixture".into(),
        edam_topic: "topic:3308".into(),
        edam_operation: "operation:3223".into(),
        confidence: 0.85,
        confidence_label: "high".into(),
        organisms: vec![],
        methods_specified: vec![],
        data_sources: vec![],
        intake_text: "bulk RNA-seq ablation fixture".into(),
        goal: None,
        archetype_id: None,
        additional_modalities: vec![],
        tie_candidates: vec![],
    }
}

/// Emit into `out` with the current process env. Caller controls flags.
fn emit_into(out: &Path) {
    let dag = minimal_dag();
    let clf = classification();
    let policies_dir = config_root().join("downstream-policy");
    emit_package(&EmitConfig {
        output_dir: out,
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir,
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: None,
        intake_facts: None,
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: None,
        per_atom_runtime_prereqs: None,
    })
    .expect("emit");
}

/// The 8 ECAA runtime sidecars we compare for byte-identity. `audit-proof-report.json`
/// is excluded from the byte-identity *baseline* comparison because it embeds an
/// `evaluated_at` timestamp (see C6); its presence/absence is asserted separately.
const COMPARED_SIDECARS: &[&str] = &[
    "runtime/intake-conversation.jsonl",
    "runtime/decisions.jsonl",
    "runtime/proofs.jsonl",
    "runtime/claim-verification.json",
    "runtime/verifier-decisions.jsonl",
    "runtime/assumptions.jsonl",
    "runtime/determinism-shim.json",
    "runtime/security-policy.json",
];

fn read_opt(root: &Path, rel: &str) -> Option<Vec<u8>> {
    std::fs::read(root.join(rel)).ok()
}

/// Map: which flag changes which artifact in the core emit path.
/// `None` ⇒ the flag's effect lives in the conversation/server emit
/// path; core emit must leave every compared sidecar byte-identical.
fn core_effect(flag: AblationFlag) -> Option<&'static str> {
    match flag {
        AblationFlag::DecisionRecords => Some("runtime/decisions.jsonl"),
        AblationFlag::AuditProof => Some("runtime/audit-proof-report.json"),
        AblationFlag::ReexecutionClass => Some("runtime/determinism-shim.json"),
        AblationFlag::AmendmentProvenance
        | AblationFlag::TypedBlockers
        | AblationFlag::ClaimConsistency => None,
    }
}

#[test]
#[serial_test::serial]
fn ablation_one_flag_changes_exactly_one_artifact() {
    // 1. Baseline emit with all flags off.
    for f in ecaa_workflow_core::ablation::all_flags() {
        std::env::remove_var(f.env_var());
    }
    let base = tempfile::tempdir().unwrap();
    emit_into(base.path());
    let baseline: BTreeMap<&str, Option<Vec<u8>>> = COMPARED_SIDECARS
        .iter()
        .map(|&rel| (rel, read_opt(base.path(), rel)))
        .collect();

    // 2. For each flag, emit once with only that flag set.
    for flag in ecaa_workflow_core::ablation::all_flags() {
        std::env::set_var(flag.env_var(), "1");
        let dir = tempfile::tempdir().unwrap();
        emit_into(dir.path());
        std::env::remove_var(flag.env_var());

        let changed = core_effect(flag);
        for &rel in COMPARED_SIDECARS {
            let now = read_opt(dir.path(), rel);
            if changed == Some(rel) {
                assert_ne!(now, baseline[rel], "flag {:?} must change {}", flag, rel);
            } else {
                assert_eq!(
                    now, baseline[rel],
                    "flag {:?} must leave {} byte-identical to baseline",
                    flag, rel
                );
            }
        }

        // DecisionRecords + AuditProof SUPPRESS their file entirely.
        match flag {
            AblationFlag::DecisionRecords => assert!(
                read_opt(dir.path(), "runtime/decisions.jsonl").is_none()
                    || read_opt(dir.path(), "runtime/decisions.jsonl") == Some(vec![]),
                "DecisionRecords flag must suppress decisions.jsonl"
            ),
            AblationFlag::AuditProof => assert!(
                read_opt(dir.path(), "runtime/audit-proof-report.json").is_none(),
                "AuditProof flag must suppress audit-proof-report.json"
            ),
            _ => {}
        }
    }
}
