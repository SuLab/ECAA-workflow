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
use ecaa_workflow_core::audit_proof::{
    run_audit_proof, run_audit_proof_with_verifier, InvariantId, InvariantStatus, InvariantVerdict,
};
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
        stage_atoms_dir: None,
        experimental_archetype: false,
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
        InvariantId::SubstrateValidity => &["SubstrateValidityShape", "ExecutionConsistencyShape"],
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

/// Map a 4-valued Rust verdict status onto the 2-valued SHACL expectation
/// (`Some(true)` ⇒ SHACL must PASS; `Some(false)` ⇒ SHACL must FAIL; `None`
/// ⇒ no SHACL equivalent, document-skip). The reconciliation table in the
/// module doc.
fn rust_expectation(status: InvariantStatus) -> Option<bool> {
    match status {
        InvariantStatus::Pass | InvariantStatus::Warn => Some(true),
        InvariantStatus::Fail => Some(false),
        // Unverified (no SHACL equivalent) + any future #[non_exhaustive]
        // variant → document-skip rather than risk a spurious disagreement.
        InvariantStatus::Unverified => None,
        _ => None,
    }
}

/// The gate's per-invariant comparison, factored so both the live emit-only
/// gate and the non-vacuous Inv-1 teeth test exercise the SAME logic. Returns
/// `Ok(n)` with the number of shape comparisons that AGREED, or `Err(msg)`
/// on the FIRST per-shape disagreement (the gate's failure path). Panics —
/// like the live gate — if a mapped shape has no SHACL verdict line.
fn compare_invariant(
    verdict: &InvariantVerdict,
    shape_verdicts: &BTreeMap<String, bool>,
    stdout: &str,
) -> Result<usize, String> {
    let Some(rust_pass) = rust_expectation(verdict.status) else {
        return Ok(0); // Unverified → no SHACL equivalent; not a comparison.
    };
    let mut compared = 0usize;
    for shape in shapes_for(verdict.id) {
        let Some(&shacl_pass) = shape_verdicts.get(*shape) else {
            panic!("no SHACL verdict for shape {shape}:\n{stdout}");
        };
        if rust_pass != shacl_pass {
            return Err(format!(
                "Rust↔pyshacl disagreement on {:?}/{shape}: rust={:?} (pass={rust_pass}) \
                 shacl_pass={shacl_pass}",
                verdict.id, verdict.status
            ));
        }
        compared += 1;
    }
    Ok(compared)
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
        match compare_invariant(verdict, &shape_verdicts, &stdout) {
            Ok(n) => compared += n,
            Err(msg) => panic!("{msg}"),
        }
    }
    assert!(
        compared > 0,
        "the gate must compare at least one invariant (all were Unverified?): {:?}",
        report.verdicts
    );
}

/// Run `project_package.py <pkg>` and return its parsed per-shape verdicts.
fn pyshacl_shape_verdicts(pkg: &Path) -> BTreeMap<String, bool> {
    let output = Command::new("python3")
        .arg(project_script())
        .arg(pkg)
        .output()
        .expect("spawn project_package.py");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("--- project_package.py stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    let v = parse_shape_verdicts(&stdout);
    assert!(
        v.contains_key("ClaimCompletenessShape"),
        "project_package.py must emit a ClaimCompletenessShape verdict:\n{stdout}"
    );
    v
}

/// Build a `ClaimVerificationReport` carrying ONE `pending` (Unverifiable)
/// verdict so the projected ABox contains a real `ecaa:Claim` node — i.e.
/// `ClaimCompletenessShape` binds NON-vacuously (it has a focus node and
/// actively evaluates the `pending OR supported_by` `sh:or`). The Rust
/// `check_claim_completeness` reads the same signed sink.
fn pending_one_claim_report() -> ecaa_workflow_core::claim_verifier::ClaimVerificationReport {
    use ecaa_workflow_core::claim_contract::ClaimContract;
    use ecaa_workflow_core::claim_extractor::Claim;
    use ecaa_workflow_core::claim_verifier::{
        ClaimStatus, ClaimStrength, ClaimVerdict, ClaimVerificationReport,
    };
    ClaimVerificationReport {
        n_checked: 1,
        n_verified: 0,
        n_mismatch: 0,
        n_unverifiable: 1,
        verdicts: vec![ClaimVerdict {
            claim: Claim {
                entity: "differential_expression".into(),
                direction: None,
                effect_size: None,
                pvalue: None,
                source_table: None,
                excerpt: String::new(),
                contract: ClaimContract::NumericTableLookup,
            },
            // Projects to `status: "pending"` with empty supported_by →
            // satisfies ClaimCompletenessShape AND leaves Inv 5 (no
            // targetObjectsOf supported_by focus nodes) untouched.
            status: ClaimStatus::Unverifiable {
                reason: "no cited table — pending".into(),
            },
            strength: ClaimStrength::Exploratory,
        }],
        runtime_decision_log_path: None,
    }
}

/// The agreement gate's TEETH on a NON-vacuous invariant (Inv 1,
/// claim-completeness).
///
/// The live `rust_and_pyshacl_agree_per_invariant` runs over an emit-only
/// package whose C sink is empty, so Inv 1 is `Unverified` and the gate
/// document-skips it — the comparison binds non-vacuously ONLY for
/// SubstrateValidity. This test seeds a populated, host-signed verdict sink so
/// `ClaimCompletenessShape` AND the Rust `check_claim_completeness` both bind
/// non-vacuously over the SAME package, proves they AGREE in the clean case,
/// then injects a ONE-SIDED mutation (a recall-floor coverage gap, which the
/// Rust Inv-1 reads but the SHACL shape does NOT) and asserts
/// `compare_invariant` — the gate's comparison logic — DETECTS the
/// disagreement (returns `Err`).
#[test]
fn agreement_gate_detects_one_sided_inv1_disagreement() {
    use ecaa_workflow_core::audit_writer::AuditWriter;
    use ecaa_workflow_core::claim_sink::persist_signed_verdicts;
    use ecaa_workflow_core::coverage::{CoverageResult, EntityCoverage};
    use std::collections::BTreeMap as StdBTreeMap;

    if !validators_available() {
        loud_skip("agreement_gate_detects_one_sided_inv1_disagreement");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    emit_into(dir.path());

    // Host-signed verdict sink: one pending claim → Inv 1 binds non-vacuously.
    let writer = AuditWriter::for_session();
    let report = pending_one_claim_report();
    let task_id = "differential_expression";

    // --- BASELINE (no coverage block): both sides PASS Inv 1, and AGREE. ---
    persist_signed_verdicts(dir.path(), task_id, &report, None, &writer)
        .expect("persist baseline signed sink");

    let baseline_rust = run_audit_proof_with_verifier(
        dir.path(),
        &NoopWrrocValidator,
        &FrozenClock::default(),
        Some(&writer),
    )
    .expect("run_audit_proof_with_verifier (baseline)");
    let inv1_baseline = baseline_rust
        .verdicts
        .iter()
        .find(|v| v.id == InvariantId::ClaimCompleteness)
        .expect("ClaimCompleteness verdict present");
    // The sink populated Inv 1 — it is NOT Unverified, so the gate compares it.
    assert_ne!(
        inv1_baseline.status,
        InvariantStatus::Unverified,
        "the populated signed sink must make Inv 1 bind non-vacuously (got Unverified): {inv1_baseline:?}"
    );
    assert_eq!(
        inv1_baseline.status,
        InvariantStatus::Pass,
        "a single pending claim with no recall gap must Pass Inv 1: {inv1_baseline:?}"
    );

    let baseline_shapes = pyshacl_shape_verdicts(dir.path());
    assert_eq!(
        baseline_shapes.get("ClaimCompletenessShape"),
        Some(&true),
        "pending claim must PASS ClaimCompletenessShape in pyshacl"
    );
    // The gate's own comparison logic AGREES (no disagreement) in the clean case.
    let n = compare_invariant(inv1_baseline, &baseline_shapes, "<baseline>")
        .expect("baseline Inv 1 must agree");
    assert_eq!(n, 1, "exactly the ClaimCompletenessShape comparison binds");

    // --- ONE-SIDED MUTATION: add a recall-floor coverage gap. The Rust Inv 1
    //     reads the signed sink's `coverage` block and FAILS; the SHACL
    //     ClaimCompletenessShape does NOT encode the recall floor, so it still
    //     PASSES on the same (still-pending) Claim node. This is the exact
    //     one-sided break the agreement gate exists to catch. ---
    let mut per_entity = StdBTreeMap::new();
    per_entity.insert(task_id.to_string(), EntityCoverage::Absent);
    let coverage = CoverageResult {
        required_total: 1,
        required_addressed: 0,
        required_unverifiable: 0,
        required_absent: 1,
        per_entity,
    };
    persist_signed_verdicts(dir.path(), task_id, &report, Some(&coverage), &writer)
        .expect("persist mutated signed sink");

    let mutated_rust = run_audit_proof_with_verifier(
        dir.path(),
        &NoopWrrocValidator,
        &FrozenClock::default(),
        Some(&writer),
    )
    .expect("run_audit_proof_with_verifier (mutated)");
    let inv1_mutated = mutated_rust
        .verdicts
        .iter()
        .find(|v| v.id == InvariantId::ClaimCompleteness)
        .expect("ClaimCompleteness verdict present");
    assert_eq!(
        inv1_mutated.status,
        InvariantStatus::Fail,
        "a Required recall gap must Fail Rust Inv 1: {inv1_mutated:?}"
    );

    let mutated_shapes = pyshacl_shape_verdicts(dir.path());
    assert_eq!(
        mutated_shapes.get("ClaimCompletenessShape"),
        Some(&true),
        "SHACL ClaimCompletenessShape does not read the coverage floor, so it \
         must still PASS — that one-sidedness is the whole point"
    );

    // THE TEETH: the gate's comparison logic now reports a disagreement.
    let outcome = compare_invariant(inv1_mutated, &mutated_shapes, "<mutated>");
    let err = outcome.expect_err(
        "the agreement gate MUST detect the one-sided Inv-1 disagreement \
         (rust=Fail, shacl=PASS) — if this is Ok the gate has no teeth on a \
         non-vacuous invariant",
    );
    assert!(
        err.contains("disagreement") && err.contains("ClaimCompletenessShape"),
        "disagreement message must name the invariant/shape: {err}"
    );
    eprintln!("agreement gate correctly flagged: {err}");
}
