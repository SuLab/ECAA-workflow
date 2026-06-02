//! Rust↔pyshacl per-invariant disagreement gate (design §6/§8).
//!
//! Runs BOTH implementations over the SAME freshly-emitted package — the Rust
//! `audit_proof/` invariants and pyshacl over the projected ABox — and FAILS
//! on any per-invariant disagreement. The existing
//! `conformance_external_validators.rs` asserts pyshacl *passes*; this gate
//! asserts the two *implementations agree*, so a future edit that makes one
//! shape vacuous (or one Rust check over-strict) is caught.
//!
//! ## Reconciliation table (R4, design §10)
//!
//! The Rust verdict is 4-valued (`Pass`/`Warn`/`Fail`/`Unverified`); SHACL is
//! 2-valued (`PASS`/`FAIL`). The mapping the gate enforces, per invariant:
//!
//! | Rust         | expectation                                            |
//! |--------------|--------------------------------------------------------|
//! | `Pass`       | SHACL shape must `PASS`                                 |
//! | `Warn`       | SHACL shape must `PASS` (soft signal; severity-Warning)|
//! | `Fail`       | SHACL shape must `FAIL`                                 |
//! | `Unverified` | SKIP — SHACL has no "unverified" state (e.g. NoopWrroc  |
//! |              | substrate, or an absent C sink); not a disagreement.   |
//!
//! Invariant 6 (`SubstrateValidity`) maps to TWO SHACL shapes —
//! `SubstrateValidityShape` AND the folded `ExecutionConsistencyShape`; the
//! invariant agrees iff NEITHER shape disagrees with the Rust verdict.
//!
//! Probe-skips LOUDLY when the pyld/rdflib/pyshacl toolchain is absent.

use crate::_shacl_harness::{loud_skip, validators_available};
use ecaa_workflow_core::audit_proof::{run_audit_proof, InvariantId, InvariantStatus};
use ecaa_workflow_core::clock::FrozenClock;
use ecaa_workflow_core::emitter::{emit_package, EmitConfig};
use ecaa_workflow_core::wrroc_validator::NoopWrrocValidator;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config")
}

fn project_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .join("scripts")
        .join("spec-check")
        .join("project_package.py")
}

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
        source_prose: Some("rust-pyshacl agreement fixture".into()),
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
        build_dag_from_workflow_dag(wf, "rust-pyshacl-agreement").expect("lower")
    } else {
        build_dag_from_composition(&out.composition, "rust-pyshacl-agreement", &BTreeMap::new(), &[])
            .expect("lower")
    }
}

fn classification() -> ecaa_workflow_core::classify::ClassificationResult {
    use ecaa_workflow_core::classify::ClassificationResult;
    ClassificationResult {
        modality: "bulk_rnaseq".into(),
        taxonomy_path: "config/stage-taxonomies/rnaseq-de.yaml".into(),
        domain: "computational biology".into(),
        workflow_description: "rust-pyshacl agreement fixture".into(),
        edam_topic: "topic:3308".into(),
        edam_operation: "operation:3223".into(),
        confidence: 0.85,
        confidence_label: "high".into(),
        organisms: vec![],
        methods_specified: vec![],
        data_sources: vec![],
        intake_text: "bulk RNA-seq rust-pyshacl agreement fixture".into(),
        goal: None,
        archetype_id: None,
        additional_modalities: vec![],
        tie_candidates: vec![],
    }
}

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
    .expect("emit_package");
}

/// The SHACL shape name(s) each invariant maps to.
fn shapes_for(id: InvariantId) -> &'static [&'static str] {
    match id {
        InvariantId::ClaimCompleteness => &["ClaimCompletenessShape"],
        InvariantId::DecisionJustification => &["DecisionJustificationShape"],
        InvariantId::EvidenceCoverage => &["EvidenceCoverageShape"],
        InvariantId::EquivalenceFailure => &["EquivalenceFailureShape"],
        InvariantId::CrossGraphIntegrity => &["CrossGraphIntegrityShape"],
        // Inv 6 folds the execution-consistency sub-check; both shapes count.
        InvariantId::SubstrateValidity => {
            &["SubstrateValidityShape", "ExecutionConsistencyShape"]
        }
        _ => &[],
    }
}

/// Parse the `SHACL-INVARIANT: <Shape>=PASS|FAIL` lines into a map.
fn parse_shape_verdicts(stdout: &str) -> BTreeMap<String, bool> {
    let mut out = BTreeMap::new();
    for line in stdout.lines() {
        if let Some(rest) = line.trim().strip_prefix("SHACL-INVARIANT:") {
            if let Some((shape, verdict)) = rest.trim().split_once('=') {
                out.insert(shape.trim().to_string(), verdict.trim() == "PASS");
            }
        }
    }
    out
}

#[test]
fn rust_and_pyshacl_agree_per_invariant() {
    if !validators_available() {
        loud_skip("rust_and_pyshacl_agree_per_invariant");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    emit_into(dir.path());

    // Rust path.
    let report = run_audit_proof(dir.path(), &NoopWrrocValidator, &FrozenClock::default())
        .expect("run_audit_proof");

    // pyshacl path.
    let output = Command::new("python3")
        .arg(project_script())
        .arg(dir.path())
        .output()
        .expect("spawn project_package.py");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("--- project_package.py stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    let shape_verdicts = parse_shape_verdicts(&stdout);
    assert!(
        !shape_verdicts.is_empty(),
        "project_package.py must emit SHACL-INVARIANT lines:\n{stdout}"
    );

    let mut compared = 0usize;
    for verdict in &report.verdicts {
        // Unverified → SHACL has no equivalent; document-skip (not a disagreement).
        let rust_pass = match verdict.status {
            InvariantStatus::Pass | InvariantStatus::Warn => true,
            InvariantStatus::Fail => false,
            // Unverified (SHACL has no equivalent) + any future #[non_exhaustive]
            // variant → document-skip rather than risk a spurious disagreement.
            InvariantStatus::Unverified => continue,
            _ => continue,
        };
        for shape in shapes_for(verdict.id) {
            let Some(&shacl_pass) = shape_verdicts.get(*shape) else {
                panic!("no SHACL verdict for shape {shape}:\n{stdout}");
            };
            assert_eq!(
                rust_pass, shacl_pass,
                "Rust↔pyshacl disagreement on {:?}/{shape}: rust={:?} (pass={rust_pass}) \
                 shacl_pass={shacl_pass}",
                verdict.id, verdict.status
            );
            compared += 1;
        }
    }
    assert!(
        compared > 0,
        "the gate must compare at least one invariant (all were Unverified?): {:?}",
        report.verdicts
    );
}
