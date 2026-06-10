use super::*;
use crate::builder::{build_dag_from_composition, build_dag_from_workflow_dag};
use crate::ids::TaskId;
use std::collections::BTreeMap;
use tempfile::TempDir;

/// Phase B4 — synthesize a representative DAG for emitter tests
/// without loading a legacy taxonomy YAML. Uses the v4 composer
/// against `config/archetypes/` (canonical post-B4 source of truth).
fn dag_from_archetype(archetype_id: &str, modality: &str) -> DAG {
    use crate::archetype_registry::ArchetypeRegistry;
    use crate::atom_registry::AtomRegistry;
    use crate::composer::compose_with_modalities_full;
    use crate::goal_spec::GoalSpec;
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let atoms = AtomRegistry::load_from_dir(&workspace.join("config/stage-atoms")).expect("atoms");
    let archetypes =
        ArchetypeRegistry::load_from_dir(&workspace.join("config/archetypes")).expect("archetypes");
    let goal = GoalSpec {
        edam_data: "data:9999".into(),
        edam_format: None,
        modifiers: Default::default(),
        source_prose: Some("emitter test (bare-modality fallback)".into()),
        confidence: 0.0,
    };
    let output = compose_with_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        &[modality],
        None,
        None,
        None,
    )
    .expect("v4 dispatch");
    let workflow_id = format!("test-emit-{}", archetype_id);
    if let Some(workflow_dag) = output.workflow_dag.as_ref() {
        build_dag_from_workflow_dag(workflow_dag, &workflow_id).expect("lower")
    } else {
        build_dag_from_composition(&output.composition, &workflow_id, &BTreeMap::new(), &[])
            .expect("compose")
    }
}

fn test_classification() -> ClassificationResult {
    ClassificationResult {
        modality: "bulk_rnaseq".into(),
        taxonomy_path: "config/stage-taxonomies/rnaseq-de.yaml".into(),
        domain: "computational biology".into(),
        workflow_description: "Bulk RNA-seq differential expression analysis".into(),
        edam_topic: "topic:3308".into(),
        edam_operation: "operation:3223".into(),
        confidence: 0.85,
        confidence_label: "high".into(),
        organisms: vec![crate::classify::OrganismInfo {
            name: "Homo sapiens".into(),
            taxon_id: 9606,
        }],
        methods_specified: vec![],
        data_sources: vec![crate::classify::DataSourceRef {
            accession: "GSE123456".into(),
            kind: "NCBI GEO Series".into(),
            qualifier: None,
            children: vec![],
        }],
        intake_text: "bulk RNA-seq differential expression from GSE123456".into(),
        goal: None,
        archetype_id: None,
        additional_modalities: vec![],
        tie_candidates: vec![],
    }
}

fn rnaseq_dag() -> DAG {
    dag_from_archetype("bulk_rnaseq_de", "bulk_rnaseq")
}

#[test]
fn emit_creates_required_files() {
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();
    let policies_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy");

    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
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
        edge_kinds: None,
    })
    .expect("emit should succeed");

    assert!(tmp.path().join("WORKFLOW.json").exists(), "WORKFLOW.json");
    assert!(tmp.path().join("PROMPT.md").exists(), "PROMPT.md");
    assert!(tmp.path().join("CONTEXT.md").exists(), "CONTEXT.md");
    assert!(
        tmp.path().join("ro-crate-metadata.json").exists(),
        "ro-crate-metadata.json"
    );
    assert!(
        tmp.path().join("runtime/LOG.jsonl").exists(),
        "runtime/LOG.jsonl"
    );
    assert!(
        tmp.path().join("runtime/outputs").is_dir(),
        "runtime/outputs/"
    );
}

#[test]
fn emit_package_writes_ecaa_runtime_artifacts() {
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();
    let policies_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy");

    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
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
        edge_kinds: None,
    })
    .expect("emit");

    for rel in [
        "runtime/intake-conversation.jsonl",
        "runtime/decisions.jsonl",
        "runtime/proofs.jsonl",
        "runtime/claim-verification.json",
        "runtime/verifier-decisions.jsonl",
        "runtime/assumptions.jsonl",
        "runtime/determinism-shim.json",
        "runtime/security-policy.json",
        "runtime/audit-proof-report.json",
        "runtime/validation-summary.json",
    ] {
        assert!(tmp.path().join(rel).exists(), "{rel}");
    }

    let intake = std::fs::read_to_string(tmp.path().join("runtime/intake-conversation.jsonl"))
        .expect("intake conversation sidecar");
    let intake_first: serde_json::Value =
        serde_json::from_str(intake.lines().next().expect("one intake row")).unwrap();
    assert_eq!(intake_first["type"].as_str(), Some("Question"));
    assert!(
        intake_first["id"]
            .as_str()
            .unwrap_or("")
            .starts_with("intent:turn:"),
        "intake rows should carry a JSON-LD id"
    );

    let proofs = std::fs::read_to_string(tmp.path().join("runtime/proofs.jsonl"))
        .expect("dependency proof sidecar");
    let proof_first: serde_json::Value =
        serde_json::from_str(proofs.lines().next().expect("one dependency proof")).unwrap();
    assert_eq!(proof_first["type"].as_str(), Some("WorkflowStep"));
    assert!(
        proof_first["id"]
            .as_str()
            .unwrap_or("")
            .starts_with("workflow:"),
        "proof rows should carry a JSON-LD workflow id"
    );
    assert!(
        proof_first["computed_from"]
            .as_str()
            .unwrap_or("")
            .starts_with("workflow:"),
        "proof rows should expose the prerequisite edge to the ECAA JSON-LD projection"
    );

    let manifest =
        std::fs::read_to_string(tmp.path().join("manifest-sha512.txt")).expect("BagIt manifest");
    assert!(
        !manifest.contains("runtime/proofs.jsonl"),
        "runtime ECAA sidecars are post-manifest artifacts so conversation emits can overwrite them"
    );
    assert!(
        !manifest.contains("runtime/validation-summary.json"),
        "runtime ECAA sidecars should stay out of the BagIt payload manifest"
    );
}

/// W1 — core emit_package writes a byte-deterministic
/// runtime/workflow-typed.json carrying D.2 keys. Ports degrade to
/// out/in on the core path (no WorkflowDag), which is the documented
/// graceful-degrade. The artifact is byte-deterministic so it stays IN
/// the BagIt manifest.
#[test]
fn core_emit_writes_workflow_typed_json() {
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();
    let policies_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy");

    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
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
        edge_kinds: None,
    })
    .expect("emit");

    let path = tmp.path().join("runtime/workflow-typed.json");
    assert!(path.exists(), "workflow-typed.json not emitted");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for key in [
        "workflow_id",
        "name",
        "steps",
        "edges",
        "parameter_mappings",
        "parameters",
        "validation_rules",
        "metadata",
    ] {
        assert!(v.get(key).is_some(), "missing D.2 key {key}");
    }
    // Byte-deterministic companion stays IN the BagIt manifest (unlike
    // the post-manifest jsonl sidecars).
    let manifest =
        std::fs::read_to_string(tmp.path().join("manifest-sha512.txt")).expect("BagIt manifest");
    assert!(
        manifest.contains("runtime/workflow-typed.json"),
        "workflow-typed.json should be IN the BagIt manifest (it is byte-deterministic)"
    );
}

/// WG4b — when the caller threads the lifted EdgeKind map from the composed
/// WorkflowDag, runtime/proofs.jsonl carries the REAL kind for each
/// node-pair dependency instead of the strict `Unproven` placeholder.
#[test]
fn proofs_jsonl_carries_real_edge_kind_when_map_threaded() {
    use crate::workflow_contracts::edge::EdgeKind;
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();
    let policies_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy");

    // Build a node-pair edge-kind map marking every depends_on edge as a
    // typed data flow (mirrors what edge_kind_map_from_edges produces from a
    // fully-typed WorkflowDag).
    let mut edge_kinds: std::collections::BTreeMap<(String, String), EdgeKind> =
        std::collections::BTreeMap::new();
    for (to_node, task) in &dag.tasks {
        for from_node in &task.depends_on {
            edge_kinds.insert(
                (from_node.to_string(), to_node.to_string()),
                EdgeKind::TypedDataFlow,
            );
        }
    }
    assert!(
        !edge_kinds.is_empty(),
        "fixture DAG must have at least one dependency edge"
    );

    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
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
        edge_kinds: Some(&edge_kinds),
    })
    .expect("emit");

    let proofs =
        std::fs::read_to_string(tmp.path().join("runtime/proofs.jsonl")).expect("proofs sidecar");
    let mut saw_typed = false;
    for line in proofs.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        assert_eq!(
            kind, "typed_data_flow",
            "every threaded dependency edge must carry the lifted kind, not the Unproven placeholder"
        );
        saw_typed = true;
    }
    assert!(saw_typed, "expected at least one proof row");

    // Control: with NO map, the placeholder remains (back-compat).
    let tmp2 = TempDir::new().unwrap();
    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp2.path(),
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
        edge_kinds: None,
    })
    .expect("emit");
    let proofs2 =
        std::fs::read_to_string(tmp2.path().join("runtime/proofs.jsonl")).expect("proofs sidecar");
    for line in proofs2.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            v.get("kind").and_then(|k| k.as_str()),
            Some("unproven"),
            "without a threaded map, dependency edges keep the strict Unproven placeholder"
        );
    }
}

#[test]
fn emitted_audit_proof_report_carries_version_declaration() {
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();
    let policies_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy");
    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
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
        edge_kinds: None,
    })
    .expect("emit");
    let raw = std::fs::read_to_string(tmp.path().join("runtime/audit-proof-report.json"))
        .expect("audit-proof-report.json");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["ecaa_version"].as_str(), Some("0.2"));
    assert_eq!(v["min_reader_version"].as_str(), Some("0.2"));
    assert!(v["evaluator"]["impl"].as_str().is_some());
    assert!(
        v["evaluated_at"].as_str().is_some(),
        "evaluated_at should be stamped"
    );
}

#[test]
fn emits_ed_cf_self_assessment_sidecar() {
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();
    let config_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config");
    let policies_dir = config_root.join("downstream-policy");
    let stage_atoms = config_root.join("stage-atoms");
    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
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
        stage_atoms_dir: Some(&stage_atoms),
        experimental_archetype: false,
        edge_kinds: None,
    })
    .expect("emit");
    let path = tmp.path().join("runtime/ed-cf-self-assessment.json");
    assert!(path.exists(), "ed-cf-self-assessment.json must be emitted");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(v.get("extensibility").is_some());
    assert!(v.get("counterfactual_floor").is_some());
    let disc = v["disclaimer"].as_str().unwrap();
    assert!(disc.contains("not validity") || disc.contains("does NOT"));
    // atom_count was derived from the real catalog → ED axis is non-empty.
    assert!(v["extensibility"]["score"].as_f64().unwrap() > 0.0);
}

#[test]
fn emit_copies_plotting_library_into_runtime() {
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();
    let policies_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy");

    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
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
        edge_kinds: None,
    })
    .expect("emit");

    // core.py is the minimum — stage modules layer over it.
    assert!(
        tmp.path().join("runtime/plotting/core.py").is_file(),
        "runtime/plotting/core.py should be copied from lib/plotting/"
    );
    assert!(
        tmp.path().join("runtime/plotting/__init__.py").is_file(),
        "runtime/plotting/__init__.py should be copied"
    );
    assert!(
        tmp.path()
            .join("runtime/plotting/stages/__init__.py")
            .is_file(),
        "runtime/plotting/stages/__init__.py should be copied"
    );
    // Phase-1 stage modules
    for stage in &[
        "quality_control.py",
        "dimensionality_reduction.py",
        "differential_expression.py",
        "batch_correction.py",
    ] {
        assert!(
            tmp.path()
                .join("runtime/plotting/stages")
                .join(stage)
                .is_file(),
            "runtime/plotting/stages/{} should be copied",
            stage
        );
    }
    // Test directories + pycache are filtered out to keep the
    // emitted package minimal + reproducible.
    assert!(
        !tmp.path().join("runtime/plotting/tests").exists(),
        "tests/ should be filtered out of the emitted package"
    );
}

#[test]
fn emit_package_deterministic_contents_across_repeated_emissions() {
    // The P5 deterministic-emission guarantee: repeated emit_package
    // calls with the same inputs must produce byte-identical
    // WORKFLOW.json, CONTEXT.md, and PROMPT.md. ro-crate-metadata.json
    // embeds a dateCreated timestamp so it's intentionally excluded
    // from this byte check.
    let policies_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy");
    let clf = test_classification();
    let dag = rnaseq_dag();

    let mk = || -> Vec<Vec<u8>> {
        let tmp = TempDir::new().unwrap();
        emit_package(&EmitConfig {
            objective: None,
            output_dir: tmp.path(),
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
            edge_kinds: None,
        })
        .expect("emit");
        vec![
            std::fs::read(tmp.path().join("WORKFLOW.json")).unwrap(),
            std::fs::read(tmp.path().join("CONTEXT.md")).unwrap(),
            std::fs::read(tmp.path().join("PROMPT.md")).unwrap(),
        ]
    };
    let run_a = mk();
    let run_b = mk();
    for (i, (a, b)) in run_a.iter().zip(run_b.iter()).enumerate() {
        assert_eq!(
            a.len(),
            b.len(),
            "file {} byte length drifted across runs",
            i
        );
        assert_eq!(a, b, "file {} bytes diverged across runs", i);
    }
}

#[test]
fn emit_ro_crate_registers_required_figures_as_image_objects() {
    // Build a small DAG with one stage that declares
    // required_figures; the emitter should render one ImageObject
    // entity per (stage, figure) pair and thread them through the
    // root Dataset's hasPart.
    use crate::classify::ClassificationResult;
    use crate::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState, DAG};
    use std::collections::BTreeMap;

    let mut tasks: BTreeMap<TaskId, Task> = BTreeMap::new();
    tasks.insert(
        TaskId::from("qc"),
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Ready,
            depends_on: vec![],
            assignee: Assignee::Agent,
            description: "QC".to_string(),
            spec: Some(serde_json::json!({
                "stage_class": "qc",
                "required_figures": ["per_sample_metric_violin", "per_sample_metric_bar"],
            })),
            resolution: None,
            result_ref: None,
            resource_class: ResourceClass::CpuHeavy,
            requires_sme_review: false,

            required_artifacts: vec![],
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        },
    );
    let dag = DAG {
        version: "1.0".to_string(),
        schema_version: crate::dag::current_dag_schema_version(),
        workflow_id: "test".to_string(),
        current_task: None,
        tasks,
        reverse_deps: BTreeMap::new(),
        run_id: None,
    };
    let clf = ClassificationResult {
        intake_text: "t".to_string(),
        modality: "bulk_rnaseq".to_string(),
        taxonomy_path: "t".to_string(),
        domain: "RNA-seq".to_string(),
        workflow_description: "t".to_string(),
        confidence: 0.9,
        confidence_label: "high".to_string(),
        methods_specified: vec![],
        organisms: vec![],
        data_sources: vec![],
        edam_topic: "topic:3170".to_string(),
        edam_operation: "operation:3435".to_string(),
        goal: None,
        archetype_id: None,
        additional_modalities: vec![],
        tie_candidates: vec![],
    };
    let meta = crate::ro_crate::build_metadata(&dag, &clf, &crate::clock::FrozenClock::default());
    let graph = meta["@graph"].as_array().expect("graph");
    let image_objects: Vec<&serde_json::Value> = graph
        .iter()
        .filter(|e| {
            let t = e.get("@type").and_then(|v| v.as_array());
            t.is_some_and(|arr| arr.iter().any(|x| x.as_str() == Some("ImageObject")))
        })
        .collect();
    assert_eq!(image_objects.len(), 2);
    let ids: Vec<&str> = image_objects
        .iter()
        .filter_map(|e| e.get("@id").and_then(|v| v.as_str()))
        .collect();
    assert!(ids.contains(&"runtime/outputs/qc/figures/per_sample_metric_violin.png"));
    assert!(ids.contains(&"runtime/outputs/qc/figures/per_sample_metric_bar.png"));
    let root = graph
        .iter()
        .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("./"))
        .expect("root");
    let parts = root["hasPart"].as_array().expect("hasPart");
    let part_ids: Vec<Option<&str>> = parts
        .iter()
        .map(|p| p.get("@id").and_then(|v| v.as_str()))
        .collect();
    assert!(part_ids.contains(&Some(
        "runtime/outputs/qc/figures/per_sample_metric_violin.png"
    )));
}

#[test]
fn emit_plotting_library_is_idempotent_on_reemit() {
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();
    let policies_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy");
    let cfg = EmitConfig {
        objective: None,
        output_dir: tmp.path(),
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
        edge_kinds: None,
    };
    emit_package(&cfg).expect("first emit");
    // Introduce a stray file that must get cleaned up on re-emit
    std::fs::write(
        tmp.path().join("runtime/plotting/core.py"),
        "# stale contents\n",
    )
    .unwrap();
    emit_package(&cfg).expect("second emit");
    let contents = std::fs::read_to_string(tmp.path().join("runtime/plotting/core.py")).unwrap();
    assert!(
        contents.contains("matplotlib.use(\"Agg\")"),
        "re-emit must overwrite the stale file with the real core.py"
    );
}

#[test]
fn workflow_json_round_trips() {
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();
    let policies_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy");

    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
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
        edge_kinds: None,
    })
    .expect("emit");

    let content = std::fs::read_to_string(tmp.path().join("WORKFLOW.json")).unwrap();
    let decoded: DAG = serde_json::from_str(&content).expect("should round-trip");
    // The emitter injects a deterministic package_run_id (derived from a
    // SHA-256 hash of the composition inputs) into WORKFLOW.json at emit
    // time — the source DAG has `run_id: None` because the field is an
    // emit-time annotation, not a compiler-time one. Verify that (a) the
    // emitter did populate it (non-None), (b) it is stable across two
    // emits from the same inputs, and (c) all other fields round-trip
    // unchanged.
    assert!(
        decoded.run_id.is_some(),
        "emitter must write a non-None run_id into WORKFLOW.json"
    );
    // Second emit — same inputs must produce the same run_id.
    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
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
        edge_kinds: None,
    })
    .expect("re-emit");
    let content2 = std::fs::read_to_string(tmp.path().join("WORKFLOW.json")).unwrap();
    let decoded2: DAG = serde_json::from_str(&content2).expect("second round-trip");
    assert_eq!(
        decoded.run_id, decoded2.run_id,
        "run_id must be deterministic across re-emits of the same intake"
    );
    // Non-run_id fields must be identical to the source DAG.
    let mut dag_with_id = dag.clone();
    dag_with_id.run_id = decoded.run_id.clone();
    assert_eq!(
        dag_with_id, decoded,
        "all fields except run_id must round-trip unchanged"
    );
}

#[test]
fn policy_allowlist_copies_only_listed_files() {
    let tmp = TempDir::new().unwrap();
    let policies_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy");
    let allowlist = vec![
        "best-practice-scoring-policy.json".to_string(),
        "source-discovery-policy.json".to_string(),
    ];
    copy_policies(&policies_dir, tmp.path(), Some(&allowlist)).expect("copy");

    assert!(tmp
        .path()
        .join("policies/best-practice-scoring-policy.json")
        .exists());
    assert!(tmp
        .path()
        .join("policies/source-discovery-policy.json")
        .exists());
    // unlisted file should NOT be copied
    assert!(!tmp
        .path()
        .join("policies/literature-grounding-policy.json")
        .exists());
}

#[test]
fn policy_no_allowlist_copies_all() {
    let tmp = TempDir::new().unwrap();
    let policies_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy");
    copy_policies(&policies_dir, tmp.path(), None).expect("copy");

    // Should have copied multiple policy files
    let count = std::fs::read_dir(tmp.path().join("policies"))
        .unwrap()
        .count();
    assert!(count > 3, "should copy multiple policies, got {}", count);
}

// ── Hardware utilization ─────────────────────────────────────

fn compute_profiles_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/compute-profiles")
}

/// The compute-resource-policy carries through the new
/// `tool_thread_curves`, `env_overrides_template`, and
/// `phase_thread_counts` fields for the profiles that declare
/// them. The agent reads this policy to size `--threads` / BLAS
/// env vars / DeepVariant phase budgets.
#[test]
fn compute_resource_policy_carries_phase_1_fields() {
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();
    let profiles = compute_profiles_root();

    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir(),
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: Some(&profiles),
        intake_facts: None,
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: None,
        per_atom_runtime_prereqs: None,
        stage_atoms_dir: None,
        experimental_archetype: false,
        edge_kinds: None,
    })
    .expect("emit");

    let policy_path = tmp.path().join("policies/compute-resource-policy.json");
    assert!(policy_path.exists(), "compute-resource-policy.json");
    let raw = std::fs::read_to_string(&policy_path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();

    let alignment = v
        .get("profiles")
        .and_then(|p| p.get("alignment_quantification"))
        .expect("alignment_quantification profile");
    assert!(
        alignment.get("tool_thread_curves").is_some(),
        "tool_thread_curves must flow through"
    );
    assert!(
        alignment.get("env_overrides_template").is_some(),
        "env_overrides_template must flow through"
    );

    let variant = v
        .get("profiles")
        .and_then(|p| p.get("variant_calling"))
        .expect("variant_calling profile");
    let phase = variant
        .get("phase_thread_counts")
        .and_then(|p| p.get("deepvariant"))
        .expect("deepvariant phase_thread_counts");
    assert_eq!(phase.get("call_variants").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(
        phase.get("make_examples").and_then(|v| v.as_i64()),
        Some(16)
    );
}

/// The `gpu-capability-policy.json` is emitted
/// alongside the other policies when the compute-profiles dir
/// carries a `gpu-capability-map.yaml`. Shape must survive the
/// YAML → JSON round-trip so the agent can read it.
#[test]
fn gpu_capability_policy_is_emitted() {
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();
    let profiles = compute_profiles_root();

    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir(),
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: Some(&profiles),
        intake_facts: None,
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: None,
        per_atom_runtime_prereqs: None,
        stage_atoms_dir: None,
        experimental_archetype: false,
        edge_kinds: None,
    })
    .expect("emit");

    let policy_path = tmp.path().join("policies/gpu-capability-policy.json");
    assert!(policy_path.exists(), "gpu-capability-policy.json");
    let raw = std::fs::read_to_string(&policy_path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();

    let methods = v.get("methods").expect("methods");
    let deepvariant = methods.get("deepvariant").expect("deepvariant entry");
    assert_eq!(
        deepvariant.get("gpu_impl").and_then(|s| s.as_str()),
        Some("run_deepvariant --use_accelerator=True")
    );
    assert!(deepvariant
        .get("requires")
        .and_then(|r| r.as_array())
        .is_some_and(|a| a.iter().any(|x| x.as_str() == Some("nvidia"))));

    // AlphaFold has cpu_impl: null — confirm the null survives.
    let alphafold = methods.get("alphafold").expect("alphafold entry");
    assert!(alphafold.get("cpu_impl").is_some_and(|v| v.is_null()));
}

/// Schema-sidecar violation on gpu-capability-map must
/// fail emission loudly (not silently misroute the agent). Builds
/// a minimal broken YAML in a scratch dir with the real schema
/// sidecar alongside; emit_package must return an error.
#[test]
fn gpu_capability_schema_violation_fails_emission() {
    let tmp = TempDir::new().unwrap();
    let scratch_profiles = TempDir::new().unwrap();

    // Copy the real schema so validation actually runs.
    let real_schema = compute_profiles_root().join("gpu-capability-map.schema.json");
    std::fs::copy(
        &real_schema,
        scratch_profiles
            .path()
            .join("gpu-capability-map.schema.json"),
    )
    .unwrap();

    // Write a malformed map that violates the schema — a method
    // entry missing the required `gpu_impl` field.
    std::fs::write(
        scratch_profiles.path().join("gpu-capability-map.yaml"),
        "methods:\n  busted:\n    requires: [nvidia]\n",
    )
    .unwrap();

    let dag = rnaseq_dag();
    let clf = test_classification();
    let err = emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir(),
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: Some(scratch_profiles.path()),
        intake_facts: None,
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: None,
        per_atom_runtime_prereqs: None,
        stage_atoms_dir: None,
        experimental_archetype: false,
        edge_kinds: None,
    })
    .unwrap_err();
    let msg = format!("{:#}", err);
    assert!(
        msg.contains("gpu-capability-map") && msg.contains("schema validation"),
        "expected schema validation error, got: {}",
        msg
    );
}

/// When no compute-profiles dir is passed, neither
/// policy lands in the package. Preserves byte-identical output
/// for emit callers that don't opt in.
#[test]
fn no_compute_profiles_dir_skips_both_policies() {
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();

    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir(),
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
        edge_kinds: None,
    })
    .expect("emit");

    assert!(!tmp
        .path()
        .join("policies/compute-resource-policy.json")
        .exists());
    assert!(!tmp
        .path()
        .join("policies/gpu-capability-policy.json")
        .exists());
}

#[test]
fn ro_crate_is_valid_json_ld() {
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();
    let policies_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy");

    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
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
        edge_kinds: None,
    })
    .expect("emit");

    let content = std::fs::read_to_string(tmp.path().join("ro-crate-metadata.json")).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&content).expect("valid JSON");

    assert!(meta.get("@context").is_some(), "@context required");
    let graph = meta
        .get("@graph")
        .expect("@graph required")
        .as_array()
        .unwrap();
    assert!(!graph.is_empty(), "@graph must be non-empty");

    // Must have the metadata descriptor and root Dataset
    let ids: Vec<&str> = graph
        .iter()
        .filter_map(|e| e.get("@id").and_then(|v| v.as_str()))
        .collect();
    assert!(ids.contains(&"ro-crate-metadata.json"));
    assert!(ids.contains(&"./"));
    assert!(ids.contains(&"WORKFLOW.json"));

    // WORKFLOW.json must be ComputationalWorkflow
    let wf = graph
        .iter()
        .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("WORKFLOW.json"))
        .expect("WORKFLOW.json entity");
    let types = wf.get("@type").expect("@type");
    let type_list = if types.is_array() {
        types
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
    } else {
        vec![types.as_str().unwrap_or("")]
    };
    assert!(
        type_list.contains(&"ComputationalWorkflow"),
        "@type must include ComputationalWorkflow"
    );

    // Must have conformsTo Bioschemas
    let conforms = wf.get("conformsTo").expect("conformsTo");
    let conforms_id = conforms.get("@id").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        conforms_id.contains("bioschemas"),
        "must conform to Bioschemas profile"
    );
}

#[test]
fn ro_crate_has_howto_steps_with_positions() {
    let dag = rnaseq_dag();
    let clf = test_classification();
    let meta = crate::ro_crate::build_metadata(&dag, &clf, &crate::clock::FrozenClock::default());
    let graph = meta.get("@graph").unwrap().as_array().unwrap();

    let steps: Vec<_> = graph
        .iter()
        .filter(|e| e.get("@type").and_then(|t| t.as_str()) == Some("HowToStep"))
        .collect();

    assert!(!steps.is_empty(), "must have HowToStep entities");

    // Positions must be sequential starting from 1
    let mut positions: Vec<u64> = steps
        .iter()
        .filter_map(|s| s.get("position").and_then(|p| p.as_u64()))
        .collect();
    positions.sort_unstable();
    for (i, pos) in positions.iter().enumerate() {
        assert_eq!(
            *pos,
            (i + 1) as u64,
            "positions must be 1-indexed sequential"
        );
    }
}

#[test]
fn prompt_md_includes_claim_boundary_and_directives() {
    // Phase B4 — the legacy `single-cell.yaml` taxonomy provided the
    // claim_boundary string. After B4 the archetype catalog doesn't
    // carry per-archetype claim_boundary fields (the StageSpec on the
    // ComposedAtom does, but the v4 path doesn't surface a single
    // taxonomy-level boundary). This test now passes a representative
    // boundary explicitly to exercise the rendering path.
    let dag = dag_from_archetype("single_cell_rnaseq", "single_cell_rnaseq");
    let clf = ClassificationResult {
        modality: "single_cell_rnaseq".into(),
        taxonomy_path: "single_cell_rnaseq".into(),
        domain: "computational biology".into(),
        workflow_description: "Single-cell RNA-seq composition".into(),
        edam_topic: "topic:3308".into(),
        edam_operation: "operation:3432".into(),
        confidence: 0.67,
        confidence_label: "medium".into(),
        organisms: vec![],
        methods_specified: vec![],
        data_sources: vec![],
        intake_text: String::new(),
        goal: None,
        archetype_id: None,
        additional_modalities: vec![],
        tie_candidates: vec![],
    };
    let claim_boundary = "Do not claim biological causality; \
                              report statistical associations only.";
    let prompt = render_prompt(&dag, &clf, Some(claim_boundary), None);

    assert!(
        prompt.contains("## Claim boundary"),
        "PROMPT.md must surface claim boundary"
    );
    assert!(
        !prompt.contains("## Analysis objective"),
        "no SME objective passed -> objective section must be absent"
    );
    assert!(
        prompt.contains("statistical associations"),
        "claim boundary content must be verbatim"
    );
}

#[test]
fn prompt_md_surfaces_sme_objective_verbatim() {
    // The SME's stated analysis objective (chat-intake prose) must reach the
    // execution agent verbatim as a project brief — otherwise a goal like
    // low-frequency heteroplasmy detection never influences the per-task agent.
    let dag = rnaseq_dag();
    let clf = test_classification();
    let objective =
        "Detect the full variant spectrum including low-frequency heteroplasmic variants; \
         input FASTQs and the chrM reference are provided in inputs/.";
    let prompt = render_prompt(&dag, &clf, None, Some(objective));
    assert!(
        prompt.contains("## Analysis objective (as stated by the SME at intake)"),
        "objective section header missing"
    );
    assert!(
        prompt.contains("low-frequency heteroplasmic variants"),
        "objective must be surfaced verbatim"
    );
    // Empty / whitespace objective is treated as absent (no empty section).
    let blank = render_prompt(&dag, &clf, None, Some("   "));
    assert!(
        !blank.contains("## Analysis objective"),
        "blank objective must not emit a section"
    );
}

#[test]
fn prompt_md_omits_sections_when_nothing_to_say() {
    let dag = rnaseq_dag(); // no SME resolutions
    let clf = test_classification();
    let prompt = render_prompt(&dag, &clf, None, None);
    assert!(!prompt.contains("## Claim boundary"));
    assert!(!prompt.contains("## SME directives"));
}

#[test]
fn prompt_md_includes_auto_detect_compute_and_fanout_section() {
    // The agent prompt must instruct runtime detection (nproc /
    // os.cpu_count / detectCores), thread-budget math, and an
    // explicit fan-out path for R, Python, and shell. Without
    // this section the agent defaults to serial loops and leaves
    // most cores idle on multi-sample stages.
    let dag = rnaseq_dag();
    let clf = test_classification();
    let prompt = render_prompt(&dag, &clf, None, None);

    // Section header
    assert!(
        prompt.contains("## Auto-detect compute and fan out embarrassingly parallel work"),
        "PROMPT.md must include the auto-detect + fan-out section header"
    );

    // Detection probes — at least one per language/runtime
    assert!(prompt.contains("nproc --all"), "must instruct nproc probe");
    assert!(
        prompt.contains("os.cpu_count()"),
        "must instruct Python cpu_count probe"
    );
    assert!(
        prompt.contains("parallel::detectCores"),
        "must instruct R detectCores probe"
    );
    assert!(
        prompt.contains("nvidia-smi"),
        "must instruct GPU presence probe"
    );

    // Worker-pool math
    assert!(
        prompt.contains("outer_workers"),
        "must define the outer_workers variable"
    );
    assert!(
        prompt.contains("inner_threads_per_unit"),
        "must define the inner_threads_per_unit variable"
    );

    // Per-language fan-out APIs
    assert!(
        prompt.contains("BiocParallel::bplapply"),
        "must instruct R bplapply path"
    );
    assert!(
        prompt.contains("ProcessPoolExecutor"),
        "must instruct Python ProcessPoolExecutor path"
    );
    assert!(
        prompt.contains("parallel -j"),
        "must instruct GNU parallel path"
    );

    // Required logging line so the SME can verify the budget was used
    assert!(
        prompt.contains("parallelism: detected_cores="),
        "must require a structured parallelism log line"
    );
}

#[test]
fn prompt_md_includes_package_containment_and_git_sections() {
    // Containment + git versioning are non-negotiable for
    // reproducibility. Without these sections the agent can leave
    // scripts in /tmp, downloads in ~/.cache, and produce a
    // package whose runtime/outputs/ tree is missing the source
    // code that produced it. Every package is a git repo.
    let dag = rnaseq_dag();
    let clf = test_classification();
    let prompt = render_prompt(&dag, &clf, None, None);

    // Containment section
    assert!(prompt.contains("## Package containment"));
    assert!(
        prompt.contains("Required layout under runtime/outputs/<task_id>/"),
        "must declare the layout heading"
    );
    assert!(
        prompt.contains("`scripts/`"),
        "must require scripts subdir bullet"
    );
    assert!(
        prompt.contains("`data/`"),
        "must require data subdir bullet"
    );
    assert!(
        prompt.contains("`intermediates/`"),
        "must require intermediates subdir bullet"
    );
    assert!(
        prompt.contains("test -d runtime/outputs/<task_id>/scripts/"),
        "must include verification step that asserts scripts dir exists"
    );
    assert!(
        prompt.contains("env.lock"),
        "must require env.lock artifact"
    );
    assert!(
        prompt.contains("export TMPDIR="),
        "must redirect TMPDIR into package"
    );
    assert!(
        prompt.contains("export XDG_CACHE_HOME="),
        "must redirect XDG_CACHE_HOME"
    );
    assert!(
        prompt.contains("export R_LIBS_USER="),
        "must redirect R_LIBS_USER"
    );
    assert!(
        prompt.contains("export PIP_CACHE_DIR="),
        "must redirect PIP_CACHE_DIR"
    );
    assert!(
        prompt.contains("containment_deviations"),
        "must define a deviations channel for impossible containment"
    );
    assert!(
        prompt.contains("containment_violation"),
        "must define a blocker_kind for failed verification"
    );

    // Git versioning section
    assert!(prompt.contains("Local git versioning"));
    assert!(prompt.contains("git init -q -b main"));
    assert!(prompt.contains("ecaa-workflow-agent"));
    assert!(prompt.contains("git commit -q -m"));
    assert!(prompt.contains(".gitignore"));
    assert!(
        prompt.contains("runtime/cache/"),
        "gitignore must exclude runtime/cache/"
    );
    assert!(
        prompt.contains("git diff --cached --quiet"),
        "must skip empty-diff commits cleanly"
    );
    assert!(
        prompt.contains("NEVER `git reset --hard`"),
        "must forbid history rewriting"
    );
}

#[test]
fn prompt_gitignore_excludes_agent_home() {
    // Belt-and-suspenders (C1): the per-task agent home holds
    // credentials and provider state. The emitted package's
    // `.gitignore` must exclude `runtime/agent-home/` so a credential
    // leak can never be committed into the package git history.
    let dag = rnaseq_dag();
    let clf = test_classification();
    let prompt = render_prompt(&dag, &clf, None, None);
    assert!(
        prompt.contains("runtime/agent-home/"),
        "gitignore must exclude runtime/agent-home/"
    );
}

// Phase B4 — the `context_md_surfaces_sme_decisions` and
// `ro_crate_carries_sme_decisions_on_steps` tests exercised the
// legacy `resolve_intake_methods` taxonomy path that auto-completes
// discovery tasks with SME-supplied method prose + auto-injected
// condition fields. The composer-driven v4 path doesn't carry that
// SME resolution mechanism in the builder (the conversation crate
// handles discovery resolution via tool calls instead). Both tests
// were deleted with the legacy `build_dag_from_taxonomy` entry
// point in B4. The replacement coverage lives in:
// - `crates/conversation/tests/fixture_runner.rs` (SME path
// including discover_<stage> resolution via tools)
// - `scripts/test-ivd-chat.sh` (drift-report match=7 closes the
// loop on resolve → emit → parse).

#[test]
fn ro_crate_contains_edam_annotations() {
    let dag = rnaseq_dag();
    let clf = test_classification();
    let meta = crate::ro_crate::build_metadata(&dag, &clf, &crate::clock::FrozenClock::default());
    let graph = meta.get("@graph").unwrap().as_array().unwrap();

    let wf = graph
        .iter()
        .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("WORKFLOW.json"))
        .expect("WORKFLOW.json entity");

    // EDAM topic annotation — the dereferenceable IRI uses an underscore
    // in the local id (`topic_3308`), never the raw CURIE colon (which
    // 404s on edamontology.org).
    let sub_cat = wf
        .get("applicationSubCategory")
        .expect("applicationSubCategory");
    let sub_cat_id = sub_cat.get("@id").and_then(|v| v.as_str()).unwrap_or("");
    assert_eq!(
        sub_cat_id, "https://edamontology.org/topic_3308",
        "must have bulk_rnaseq EDAM topic in underscore form"
    );

    // EDAM operation annotation
    let feature = wf.get("featureList").expect("featureList");
    let feat_id = feature.get("@id").and_then(|v| v.as_str()).unwrap_or("");
    assert_eq!(
        feat_id, "https://edamontology.org/operation_3223",
        "must have DE EDAM operation in underscore form"
    );
}

// Amend lineage tests

fn emit_plain(dir: &std::path::Path, policies_dir: &std::path::Path) {
    let dag = rnaseq_dag();
    let clf = test_classification();
    emit_package(&EmitConfig {
        objective: None,
        output_dir: dir,
        dag: &dag,
        classification: &clf,
        policies_dir,
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
        edge_kinds: None,
    })
    .expect("emit");
}

#[test]
fn emit_writes_runtime_prereqs_empty_when_none_provided() {
    let tmp = TempDir::new().unwrap();
    emit_plain(tmp.path(), &policies_dir());
    let path = tmp.path().join("policies/runtime-prereqs.json");
    assert!(
        path.exists(),
        "runtime-prereqs.json must always be emitted (legacy callers pass None)"
    );
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["schema_version"].as_u64(), Some(1));
    assert!(
        v["base_image"].is_null() || v.get("base_image").is_none(),
        "default manifest must not declare a base image"
    );
}

#[test]
fn emit_writes_runtime_prereqs_with_passed_baseline() {
    use crate::runtime_prereqs::{LanguagePackages, RuntimePrereqs, SystemPackages};
    let tmp = TempDir::new().unwrap();
    let mut prereqs = RuntimePrereqs::new();
    prereqs.base_image = Some("ghcr.io/scripps/scripps-bio-base:0.1.0".into());
    prereqs.modality = Some("single_cell_rnaseq".into());
    prereqs.system_packages = SystemPackages {
        apt: ["libcurl4-openssl-dev".into()].into(),
        ..Default::default()
    };
    prereqs.language_packages = LanguagePackages {
        r: ["Seurat>=5.0".into()].into(),
        python: ["scanpy>=1.10".into()].into(),
        ..Default::default()
    };

    let dag = rnaseq_dag();
    let clf = test_classification();
    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir(),
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: None,
        intake_facts: None,
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: Some(&prereqs),
        per_atom_runtime_prereqs: None,
        stage_atoms_dir: None,
        experimental_archetype: false,
        edge_kinds: None,
    })
    .expect("emit");

    let v: serde_json::Value = serde_json::from_slice(
        &std::fs::read(tmp.path().join("policies/runtime-prereqs.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        v["base_image"], "ghcr.io/scripps/scripps-bio-base:0.1.0",
        "passed-in base_image must round-trip into the emitted manifest"
    );
    assert_eq!(v["modality"], "single_cell_rnaseq");
    assert_eq!(v["system_packages"]["apt"][0], "libcurl4-openssl-dev");
    assert_eq!(v["language_packages"]["r"][0], "Seurat>=5.0");
    assert_eq!(v["language_packages"]["python"][0], "scanpy>=1.10");
}

#[test]
fn emit_writes_dockerfile_when_manifest_is_buildable() {
    use crate::runtime_prereqs::{LanguagePackages, RuntimePrereqs, SystemPackages};
    let tmp = TempDir::new().unwrap();
    let mut prereqs = RuntimePrereqs::new();
    prereqs.base_image = Some("ghcr.io/test/base:1".into());
    prereqs.system_packages = SystemPackages {
        apt: ["libcurl4-openssl-dev".into()].into(),
        ..Default::default()
    };
    prereqs.language_packages = LanguagePackages {
        r: ["BPCells".into()].into(),
        ..Default::default()
    };

    let dag = rnaseq_dag();
    let clf = test_classification();
    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir(),
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: None,
        intake_facts: None,
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: Some(&prereqs),
        per_atom_runtime_prereqs: None,
        stage_atoms_dir: None,
        experimental_archetype: false,
        edge_kinds: None,
    })
    .expect("emit");

    let df_path = tmp.path().join("runtime/derived-image.Dockerfile");
    assert!(
        df_path.exists(),
        "buildable manifest must emit runtime/derived-image.Dockerfile"
    );
    let df = std::fs::read_to_string(df_path).unwrap();
    assert!(df.contains("FROM ghcr.io/test/base:1"));
    assert!(df.contains("libcurl4-openssl-dev"));
    // Per directive language packages do NOT bake into
    // the derived image. They install at task time via the agent's
    // per-session cache mount. Manifest still records BPCells in
    // runtime-prereqs.json as an executor hint.
    assert!(!df.contains("BPCells"));
}

#[test]
fn emit_skips_dockerfile_when_manifest_is_empty() {
    let tmp = TempDir::new().unwrap();
    emit_plain(tmp.path(), &policies_dir());
    let df_path = tmp.path().join("runtime/derived-image.Dockerfile");
    assert!(
            !df_path.exists(),
            "empty/legacy manifest must NOT emit a Dockerfile (keeps byte-identical with pre-S15.x packages)"
        );
}

#[test]
fn emit_copies_install_proxy_when_manifest_is_buildable() {
    // When the manifest is buildable, the emitter
    // must copy the install-proxy shim tree alongside the
    // Dockerfile so `COPY runtime/install-proxy/...` resolves
    // against the build context. Mirrors the
    // copy_plotting_library invariant. Non-buildable manifests
    // skip the copy (next test).
    use crate::runtime_prereqs::{RuntimePrereqs, SystemPackages};
    let tmp = TempDir::new().unwrap();
    let mut prereqs = RuntimePrereqs::new();
    prereqs.base_image = Some("ghcr.io/test/base:1".into());
    prereqs.system_packages = SystemPackages {
        apt: ["libcurl4-openssl-dev".into()].into(),
        ..Default::default()
    };

    let dag = rnaseq_dag();
    let clf = test_classification();
    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir(),
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: None,
        intake_facts: None,
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: Some(&prereqs),
        per_atom_runtime_prereqs: None,
        stage_atoms_dir: None,
        experimental_archetype: false,
        edge_kinds: None,
    })
    .expect("emit");

    let shim_dir = tmp.path().join("runtime/install-proxy");
    for shim in &[
        "_common.py",
        "apt.py",
        "pip.py",
        "conda.py",
        "npm.py",
        "rscript.py",
        "gem.py",
    ] {
        assert!(
            shim_dir.join(shim).is_file(),
            "runtime/install-proxy/{shim} should be copied from runtime/install-proxy/"
        );
    }
    // Test directories + pycache filtered out so the emitted
    // tree stays minimal + reproducible (same convention as
    // copy_plotting_library).
    assert!(
        !shim_dir.join("tests").exists(),
        "install-proxy/tests/ should be filtered out of the emitted package"
    );
    assert!(
        !shim_dir.join("__pycache__").exists(),
        "install-proxy/__pycache__/ should be filtered out of the emitted package"
    );
}

#[test]
fn emit_skips_install_proxy_when_manifest_is_empty() {
    // Gated on is_buildable. A legacy / empty
    // manifest must NOT drop the install-proxy tree into runtime/
    // (keeps test/non-bio packages byte-identical to pre-Task-5.8
    // emits).
    let tmp = TempDir::new().unwrap();
    emit_plain(tmp.path(), &policies_dir());
    assert!(
        !tmp.path().join("runtime/install-proxy").exists(),
        "empty/legacy manifest must NOT copy install-proxy shims"
    );
}

#[test]
fn emit_runtime_prereqs_is_byte_deterministic_across_calls() {
    // Two emits of the same package config must produce
    // byte-identical manifest JSON. The harness pre-flight relies
    // on content-hash stability for cache hits.
    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    emit_plain(tmp_a.path(), &policies_dir());
    emit_plain(tmp_b.path(), &policies_dir());
    let bytes_a = std::fs::read(tmp_a.path().join("policies/runtime-prereqs.json")).unwrap();
    let bytes_b = std::fs::read(tmp_b.path().join("policies/runtime-prereqs.json")).unwrap();
    assert_eq!(
        bytes_a, bytes_b,
        "runtime-prereqs.json must serialize identically across calls"
    );
}

// ── Per-atom runtime-prereqs ───────────

fn buildable_prereqs(apt: &[&str]) -> crate::runtime_prereqs::RuntimePrereqs {
    use crate::runtime_prereqs::{RuntimePrereqs, SystemPackages};
    let mut m = RuntimePrereqs::new();
    m.base_image = Some("ghcr.io/test/base:1".into());
    m.system_packages = SystemPackages {
        apt: apt.iter().map(|s| (*s).to_string()).collect(),
        ..Default::default()
    };
    m
}

#[test]
fn emit_per_atom_runtime_prereqs_writes_one_file_per_buildable_atom() {
    let tmp = TempDir::new().unwrap();
    let mut map: std::collections::BTreeMap<String, crate::runtime_prereqs::RuntimePrereqs> =
        Default::default();
    map.insert("atom_a".into(), buildable_prereqs(&["tool-a"]));
    map.insert("atom_b".into(), buildable_prereqs(&["tool-b"]));
    emit_per_atom_runtime_prereqs(tmp.path(), &map, None).expect("ok");
    let dir = tmp.path().join("policies/atom-prereqs");
    assert!(dir.is_dir(), "policies/atom-prereqs/ must be created");
    assert!(
        dir.join("atom_a.json").is_file(),
        "atom_a.json must be written"
    );
    assert!(
        dir.join("atom_b.json").is_file(),
        "atom_b.json must be written"
    );
}

#[test]
fn emit_per_atom_runtime_prereqs_skips_unbuildable_atoms() {
    // Atoms whose manifest is_buildable() returns false (no base
    // image, or no system delta) must NOT land on disk — the
    // harness falls back to host mode or
    // atom.preferred_container.image for these.
    let tmp = TempDir::new().unwrap();
    let mut map: std::collections::BTreeMap<String, crate::runtime_prereqs::RuntimePrereqs> =
        Default::default();
    // Buildable
    map.insert("atom_real".into(), buildable_prereqs(&["tool-real"]));
    // Empty — no base image, no packages
    map.insert(
        "atom_empty".into(),
        crate::runtime_prereqs::RuntimePrereqs::new(),
    );
    // Base image set but no system packages — also not buildable
    let mut base_only = crate::runtime_prereqs::RuntimePrereqs::new();
    base_only.base_image = Some("ghcr.io/test/base:2".into());
    map.insert("atom_base_only".into(), base_only);
    emit_per_atom_runtime_prereqs(tmp.path(), &map, None).expect("ok");
    let dir = tmp.path().join("policies/atom-prereqs");
    assert!(
        dir.join("atom_real.json").is_file(),
        "buildable atom_real.json should be written"
    );
    assert!(
        !dir.join("atom_empty.json").exists(),
        "empty manifest must NOT be written"
    );
    assert!(
        !dir.join("atom_base_only.json").exists(),
        "base-image-only manifest must NOT be written"
    );
}

#[test]
fn emit_per_atom_runtime_prereqs_skips_dir_entirely_when_all_unbuildable() {
    // If no entry in the map is buildable, no policies/atom-prereqs/
    // directory should be created — keeps the package surface
    // minimal for legacy modalities (and byte-identical to the
    // legacy emit path).
    let tmp = TempDir::new().unwrap();
    let mut map: std::collections::BTreeMap<String, crate::runtime_prereqs::RuntimePrereqs> =
        Default::default();
    map.insert(
        "atom_e".into(),
        crate::runtime_prereqs::RuntimePrereqs::new(),
    );
    emit_per_atom_runtime_prereqs(tmp.path(), &map, None).expect("ok");
    assert!(
        !tmp.path().join("policies/atom-prereqs").exists(),
        "no per-atom dir when zero atoms are buildable"
    );
}

#[test]
fn emit_per_atom_runtime_prereqs_is_byte_deterministic_across_calls() {
    // Two emits with the same input map must produce byte-identical
    // per-atom files. The harness reads the file bytes through
    // content_hash_from_file so determinism here is load-bearing
    // for image cache hits.
    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    let mut map: std::collections::BTreeMap<String, crate::runtime_prereqs::RuntimePrereqs> =
        Default::default();
    map.insert("atom_x".into(), buildable_prereqs(&["pkg-1", "pkg-2"]));
    emit_per_atom_runtime_prereqs(tmp_a.path(), &map, None).expect("ok");
    emit_per_atom_runtime_prereqs(tmp_b.path(), &map, None).expect("ok");
    let bytes_a = std::fs::read(tmp_a.path().join("policies/atom-prereqs/atom_x.json")).unwrap();
    let bytes_b = std::fs::read(tmp_b.path().join("policies/atom-prereqs/atom_x.json")).unwrap();
    assert_eq!(
        bytes_a, bytes_b,
        "per-atom manifest must serialize identically across calls"
    );
}

#[test]
fn emit_per_atom_runtime_prereqs_refuses_invalid_atom_id() {
    // Defense-in-depth: atom_id flows into a file path so it must
    // not contain `/`, `..`, or NUL. A malformed key in the
    // caller's map should refuse to land on disk rather than
    // escape the package directory.
    let tmp = TempDir::new().unwrap();
    for bad in &["", "../escape", "with/slash", "with\0null"] {
        let mut map: std::collections::BTreeMap<String, crate::runtime_prereqs::RuntimePrereqs> =
            Default::default();
        map.insert((*bad).to_string(), buildable_prereqs(&["x"]));
        let res = emit_per_atom_runtime_prereqs(tmp.path(), &map, None);
        assert!(
            res.is_err(),
            "atom_id {bad:?} should refuse to land on disk"
        );
    }
}

#[test]
fn emit_package_writes_atom_prereqs_when_map_provided() {
    // Integration: emit_package threads per_atom_runtime_prereqs
    // through to disk only when the map is provided. When None,
    // the legacy byte-baseline holds.
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();
    let policies = policies_dir();
    let mut map: std::collections::BTreeMap<String, crate::runtime_prereqs::RuntimePrereqs> =
        Default::default();
    map.insert("atom_a".into(), buildable_prereqs(&["libfoo"]));
    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies,
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: None,
        intake_facts: None,
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: None,
        per_atom_runtime_prereqs: Some(&map),
        stage_atoms_dir: None,
        experimental_archetype: false,
        edge_kinds: None,
    })
    .expect("emit");
    assert!(
        tmp.path()
            .join("policies/atom-prereqs/atom_a.json")
            .is_file(),
        "per-atom file should be written when map is provided"
    );
}

#[test]
fn emit_package_omits_atom_prereqs_when_map_is_none() {
    // The default-None case preserves byte-baseline for callers
    // that haven't opted in. No policies/atom-prereqs/ directory
    // should be created.
    let tmp = TempDir::new().unwrap();
    emit_plain(tmp.path(), &policies_dir());
    assert!(
        !tmp.path().join("policies/atom-prereqs").exists(),
        "per-atom-prereqs dir must NOT be created when the map is None"
    );
}

// ── Container spec ────────────────────────────────────

#[test]
fn emit_writes_container_spec_with_null_image_by_default() {
    let tmp = TempDir::new().unwrap();
    emit_plain(tmp.path(), &policies_dir());
    let spec_path = tmp.path().join("policies/container.json");
    assert!(spec_path.exists(), "container.json must be emitted");
    let spec: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&spec_path).unwrap()).unwrap();
    assert!(
        spec["image"].is_null(),
        "bio default container must be null, got {:?}",
        spec["image"]
    );
}

#[test]
fn emit_writes_memory_discipline_policy() {
    let tmp = TempDir::new().unwrap();
    emit_plain(tmp.path(), &policies_dir());
    let path = tmp.path().join("policies/memory-discipline.json");
    assert!(path.exists(), "memory-discipline.json must be emitted");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["schema_version"].as_u64(), Some(1));
    assert!(
        v["max_dense_matrix_gb"].as_u64().unwrap_or(0) > 0,
        "max_dense_matrix_gb must be a positive int",
    );
    assert!(
        v["large_cohort_cell_threshold_k"].as_u64().unwrap_or(0) > 0,
        "large_cohort_cell_threshold_k must be a positive int",
    );
    let r_hints = v["on_disk_library_hints"]["R"].as_array().unwrap();
    assert!(
        r_hints.iter().any(|h| h == "BPCells"),
        "R hints must include BPCells",
    );
    let py_hints = v["on_disk_library_hints"]["python"].as_array().unwrap();
    assert!(!py_hints.is_empty(), "python hints must not be empty");
    assert!(
        v["guidance"].as_array().unwrap().len() >= 3,
        "guidance must carry at least three bullet points",
    );
}

#[test]
fn emit_writes_container_spec_with_declared_image() {
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();
    let policies = policies_dir();
    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies,
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: None,
        intake_facts: None,
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: Some("scripps/clinical-trial:1.0"),
        runtime_prereqs: None,
        per_atom_runtime_prereqs: None,
        stage_atoms_dir: None,
        experimental_archetype: false,
        edge_kinds: None,
    })
    .expect("emit");
    let spec: serde_json::Value =
        serde_json::from_slice(&std::fs::read(tmp.path().join("policies/container.json")).unwrap())
            .unwrap();
    assert_eq!(spec["image"].as_str(), Some("scripps/clinical-trial:1.0"));
}

// Image-digest pinning. Emission must fail
// closed when any task carries a `ContainerSpec` with an empty
// (or all-zero-sentinel) `digest`. Test by injecting an unpinned
// container onto a DAG task and asserting `emit_package` errors
// out before any file is written.

// `ContainerSpec.network` is `#[deprecated]` in favor of
// `safety.network`, but the struct has no `Default` impl, so the literal
// must still name every field. Matches the back-compat construction
// pattern used elsewhere in `atom.rs`.
#[allow(deprecated)]
fn unpinned_container_spec(image: &str, tag: &str) -> crate::atom::ContainerSpec {
    crate::atom::ContainerSpec {
        image: image.into(),
        tag: tag.into(),
        digest: String::new(), // unpinned — this is the defect.
        arch: vec!["amd64".into()],
        gpu_required: false,
        network: None,
        source: crate::atom::ContainerSource::default(),
    }
}

#[allow(deprecated)]
fn pinned_container_spec(image: &str, tag: &str, digest: &str) -> crate::atom::ContainerSpec {
    crate::atom::ContainerSpec {
        image: image.into(),
        tag: tag.into(),
        digest: digest.into(),
        arch: vec!["amd64".into()],
        gpu_required: false,
        network: None,
        source: crate::atom::ContainerSource::default(),
    }
}

#[test]
fn validate_container_digests_pinned_accepts_no_container_tasks() {
    // The vast majority of today's DAGs have `container: None` on
    // every task (host-mode); the validator must not falsely flag
    // these.
    let dag = rnaseq_dag();
    assert!(validate_container_digests_pinned(&dag).is_ok());
}

#[test]
fn validate_container_digests_pinned_accepts_pinned_digest() {
    let mut dag = rnaseq_dag();
    // Pick any task and pin its container with a real-looking digest.
    let some_id = dag.tasks.keys().next().cloned().unwrap();
    dag.tasks.get_mut(&some_id).unwrap().container = Some(pinned_container_spec(
        "ghcr.io/scripps/test",
        "1.0",
        "sha256:aaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999",
    ));
    assert!(validate_container_digests_pinned(&dag).is_ok());
}

#[test]
fn validate_container_digests_pinned_rejects_empty_digest() {
    let mut dag = rnaseq_dag();
    let some_id = dag.tasks.keys().next().cloned().unwrap();
    dag.tasks.get_mut(&some_id).unwrap().container =
        Some(unpinned_container_spec("ghcr.io/scripps/test", "1.0"));
    let err = validate_container_digests_pinned(&dag).unwrap_err();
    match err {
        crate::backend_emitters::EmitError::ImageDigestUnresolved { task, image, tag } => {
            assert_eq!(
                task,
                some_id.to_string(),
                "EmitError must name the offending task"
            );
            assert_eq!(image, "ghcr.io/scripps/test");
            assert_eq!(tag, "1.0");
        }
        other => panic!("expected ImageDigestUnresolved, got {other:?}"),
    }
}

#[test]
fn validate_container_digests_pinned_rejects_all_zero_sentinel() {
    // Some upstream resolvers return the all-zeros sha256 when a
    // registry lookup fails. The guard must treat that the same as
    // an empty string.
    let mut dag = rnaseq_dag();
    let some_id = dag.tasks.keys().next().cloned().unwrap();
    dag.tasks.get_mut(&some_id).unwrap().container = Some(pinned_container_spec(
        "ghcr.io/scripps/test",
        "1.0",
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    ));
    let err = validate_container_digests_pinned(&dag).unwrap_err();
    assert!(matches!(
        err,
        crate::backend_emitters::EmitError::ImageDigestUnresolved { .. }
    ));
}

#[test]
fn emit_package_rejects_unpinned_container_digest() {
    // End-to-end: emit_package must surface the digest-unresolved
    // diagnostic via `anyhow` so chat / CLI sees an actionable
    // message that names the task and the image:tag.
    let tmp = TempDir::new().unwrap();
    let mut dag = rnaseq_dag();
    let some_id = dag.tasks.keys().next().cloned().unwrap();
    dag.tasks.get_mut(&some_id).unwrap().container =
        Some(unpinned_container_spec("ghcr.io/scripps/test", "1.0"));
    let clf = test_classification();
    let policies = policies_dir();
    let result = emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies,
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
        edge_kinds: None,
    });
    let err = result.expect_err("emit must reject unpinned digests");
    let msg = format!("{:#}", err);
    assert!(
        msg.contains(some_id.as_str()),
        "diagnostic must name the offending task: {msg}"
    );
    assert!(
        msg.contains("ghcr.io/scripps/test"),
        "diagnostic must echo the offending image: {msg}"
    );
    assert!(
        msg.contains("digest"),
        "diagnostic must mention the digest defect: {msg}"
    );
    // Defense-in-depth: emission must have aborted BEFORE we wrote
    // any package files. WORKFLOW.json is the canonical artifact.
    assert!(
        !tmp.path().join("WORKFLOW.json").exists(),
        "emit must abort before writing WORKFLOW.json on digest-unresolved"
    );
}

fn policies_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy")
}

fn load_ro_crate(dir: &std::path::Path) -> serde_json::Value {
    let s = std::fs::read_to_string(dir.join("ro-crate-metadata.json")).unwrap();
    serde_json::from_str(&s).unwrap()
}

/// Return the root Dataset (`@id == "./"`) entity from an emitted
/// ro-crate metadata graph.
fn root_dataset(meta: &serde_json::Value) -> &serde_json::Value {
    meta.get("@graph")
        .and_then(|g| g.as_array())
        .and_then(|g| {
            g.iter()
                .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("./"))
        })
        .expect("root Dataset entity")
}

/// Whether the root Dataset carries the experimental-archetype maturity
/// stamp as an `additionalProperty` PropertyValue.
fn has_experimental_stamp(meta: &serde_json::Value) -> bool {
    root_dataset(meta)
        .get("additionalProperty")
        .and_then(|p| p.as_array())
        .map(|props| {
            props.iter().any(|p| {
                p.get("name").and_then(|v| v.as_str()) == Some("archetypeMaturity")
                    && p.get("value").and_then(|v| v.as_str()) == Some("experimental")
            })
        })
        .unwrap_or(false)
}

/// When the chosen archetype is experimental (scaffolded /
/// not-production-validated), emission STAMPS the root Dataset with an
/// `archetypeMaturity: experimental` additionalProperty so a reviewer
/// sees the maturity. The stamp is deterministic (no wall-clock) — two
/// emits of the same inputs produce byte-identical metadata.
#[test]
fn emit_stamps_experimental_archetype_maturity() {
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let mut clf = test_classification();
    clf.archetype_id = Some("cross_omics_rnaseq_proteomics".into());
    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir(),
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
        experimental_archetype: true,
        edge_kinds: None,
    })
    .expect("emit should succeed");

    let meta = load_ro_crate(tmp.path());
    assert!(
        has_experimental_stamp(&meta),
        "experimental archetype must stamp the root Dataset with \
         archetypeMaturity: experimental; root = {:#}",
        root_dataset(&meta)
    );

    // Determinism: a second emit of the same inputs is byte-identical.
    let tmp2 = TempDir::new().unwrap();
    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp2.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir(),
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
        experimental_archetype: true,
        edge_kinds: None,
    })
    .expect("second emit should succeed");
    let a = std::fs::read_to_string(tmp.path().join("ro-crate-metadata.json")).unwrap();
    let b = std::fs::read_to_string(tmp2.path().join("ro-crate-metadata.json")).unwrap();
    assert_eq!(a, b, "stamped metadata must be byte-deterministic");
}

/// A normal (production-validated) archetype must NOT stamp the root
/// Dataset with the experimental marker.
#[test]
fn emit_does_not_stamp_production_archetype() {
    let tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let mut clf = test_classification();
    clf.archetype_id = Some("bulk_rnaseq_de".into());
    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir(),
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
        edge_kinds: None,
    })
    .expect("emit should succeed");

    let meta = load_ro_crate(tmp.path());
    assert!(
        !has_experimental_stamp(&meta),
        "production archetype must NOT carry the experimental maturity stamp; \
         root = {:#}",
        root_dataset(&meta)
    );
}

#[test]
fn amend_from_none_leaves_ro_crate_unchanged() {
    let tmp = TempDir::new().unwrap();
    emit_plain(tmp.path(), &policies_dir());
    let meta = load_ro_crate(tmp.path());
    let graph = meta.get("@graph").unwrap().as_array().unwrap();
    let root = graph
        .iter()
        .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("./"))
        .expect("root Dataset");
    assert!(
        root.get("prov:wasDerivedFrom").is_none(),
        "non-amendment emissions must not carry prov:wasDerivedFrom"
    );
    assert!(
        !tmp.path().join("policies/amendment-lineage.json").exists(),
        "non-amendment emissions must not write amendment-lineage.json"
    );
}

#[test]
fn amend_from_some_writes_lineage_policy() {
    let parent_tmp = TempDir::new().unwrap();
    emit_plain(parent_tmp.path(), &policies_dir());

    let child_tmp = TempDir::new().unwrap();
    let ctx = AmendContext {
        reason: Some("Switch DE method".into()),
        amended_stage: "differential_expression".into(),
        invalidated_tasks: vec![
            "differential_expression".into(),
            "validate_differential_expression".into(),
        ],
    };
    let dag = rnaseq_dag();
    let clf = test_classification();
    emit_package(&EmitConfig {
        objective: None,
        output_dir: child_tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir(),
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: None,
        intake_facts: None,
        amend_from: Some(parent_tmp.path()),
        amend_context: Some(&ctx),
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: None,
        per_atom_runtime_prereqs: None,
        stage_atoms_dir: None,
        experimental_archetype: false,
        edge_kinds: None,
    })
    .expect("amend emit");

    let lineage_path = child_tmp.path().join("policies/amendment-lineage.json");
    assert!(
        lineage_path.exists(),
        "amendment-lineage.json must be written"
    );
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&lineage_path).unwrap()).unwrap();
    assert_eq!(body["amended_stage"], "differential_expression");
    assert_eq!(
        body["invalidated_tasks"].as_array().unwrap().len(),
        2,
        "invalidated_tasks must match AmendContext"
    );
    assert_eq!(body["amendment_reason"], "Switch DE method");
    assert!(
        body.get("parent").is_some(),
        "lineage must carry parent link"
    );
}

#[test]
#[allow(non_snake_case)]
fn amend_from_some_adds_wasDerivedFrom_and_updateAction() {
    let parent_tmp = TempDir::new().unwrap();
    emit_plain(parent_tmp.path(), &policies_dir());

    let child_tmp = TempDir::new().unwrap();
    let ctx = AmendContext {
        reason: Some("SME swapped DE method".into()),
        amended_stage: "differential_expression".into(),
        invalidated_tasks: vec!["differential_expression".into()],
    };
    let dag = rnaseq_dag();
    let clf = test_classification();
    emit_package(&EmitConfig {
        objective: None,
        output_dir: child_tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir(),
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: None,
        intake_facts: None,
        amend_from: Some(parent_tmp.path()),
        amend_context: Some(&ctx),
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: None,
        per_atom_runtime_prereqs: None,
        stage_atoms_dir: None,
        experimental_archetype: false,
        edge_kinds: None,
    })
    .expect("amend emit");

    let meta = load_ro_crate(child_tmp.path());
    let graph = meta.get("@graph").unwrap().as_array().unwrap();

    // Root Dataset carries prov:wasDerivedFrom pointing at the parent.
    let root = graph
        .iter()
        .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("./"))
        .expect("root Dataset");
    assert!(
        root.get("prov:wasDerivedFrom").is_some(),
        "amendment root must carry prov:wasDerivedFrom"
    );

    // UpdateAction entity exists.
    let update_action = graph
        .iter()
        .find(|e| e.get("@type").and_then(|t| t.as_str()) == Some("UpdateAction"));
    assert!(
        update_action.is_some(),
        "amendment must register an UpdateAction entity"
    );
    let ua = update_action.unwrap();
    assert!(ua
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap()
        .contains("differential_expression"));
}

#[test]
#[allow(non_snake_case)]
fn branch_emit_adds_wasDerivedFrom_without_updateAction() {
    // A branch emission (amend_from set, amend_context = None) must
    // record `prov:wasDerivedFrom` on the root Dataset and register a
    // parent Dataset entry — but NOT an UpdateAction (a branch is not
    // a method amendment, it's a point-in-time copy).
    let parent_tmp = TempDir::new().unwrap();
    emit_plain(parent_tmp.path(), &policies_dir());

    let child_tmp = TempDir::new().unwrap();
    let dag = rnaseq_dag();
    let clf = test_classification();
    emit_package(&EmitConfig {
        objective: None,
        output_dir: child_tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir(),
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: None,
        intake_facts: None,
        amend_from: Some(parent_tmp.path()),
        amend_context: None, // branch — no method amendment
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: None,
        per_atom_runtime_prereqs: None,
        stage_atoms_dir: None,
        experimental_archetype: false,
        edge_kinds: None,
    })
    .expect("branch emit");

    let meta = load_ro_crate(child_tmp.path());
    let graph = meta.get("@graph").unwrap().as_array().unwrap();

    // Root Dataset carries prov:wasDerivedFrom pointing at the parent.
    let root = graph
        .iter()
        .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("./"))
        .expect("root Dataset");
    assert!(
        root.get("prov:wasDerivedFrom").is_some(),
        "branch root must carry prov:wasDerivedFrom"
    );
    let derived_from = root
        .get("prov:wasDerivedFrom")
        .and_then(|v| v.get("@id"))
        .and_then(|v| v.as_str())
        .expect("prov:wasDerivedFrom must point at the parent's @id");
    assert!(
        derived_from.starts_with("branch-parent:"),
        "branch lineage must use the branch-parent: prefix, got {derived_from}"
    );

    // No UpdateAction (branches are point-in-time copies, not amendments).
    let update_action = graph
        .iter()
        .find(|e| e.get("@type").and_then(|t| t.as_str()) == Some("UpdateAction"));
    assert!(
        update_action.is_none(),
        "branch emission must NOT register an UpdateAction (that's amend-only); got {update_action:?}"
    );

    // Parent Dataset entry is registered so consumers can resolve the
    // wasDerivedFrom edge.
    let parent_entry = graph
        .iter()
        .find(|e| {
            e.get("@id")
                .and_then(|v| v.as_str())
                .map(|s| s.starts_with("branch-parent:"))
                .unwrap_or(false)
        })
        .expect("branch parent dataset entry");
    assert_eq!(
        parent_entry.get("@type").and_then(|v| v.as_str()),
        Some("Dataset")
    );
}

// ── Whole-package byte-reproducibility ────────────────

/// Mirror of `bagit::walk_for_manifest`'s exclusion set, applied at the
/// walk in `collect_baseline`. A file matching this predicate is one of
/// the legitimately non-reproducible runtime/audit artifacts (or a BagIt
/// tag file written after the payload walk) and is intentionally kept out
/// of the byte-reproducibility baseline. If the emitter starts writing a
/// NEW non-deterministic payload file that is NOT in this set, the
/// whole-package test fails loudly — at which point the source
/// non-determinism is fixed, or (only if the file is a genuine runtime
/// artifact) the file is added to BOTH `walk_for_manifest`'s exclusion
/// set and this mirror.
fn is_excluded_from_baseline(rel: &std::path::Path) -> bool {
    // BagIt manifest + tag files: the manifest is the anchor (compared
    // separately), tag files are written after the payload walk.
    if rel == std::path::Path::new("manifest-sha512.txt")
        || rel == std::path::Path::new("bagit.txt")
        || rel == std::path::Path::new("bag-info.txt")
        || rel == std::path::Path::new("tagmanifest-sha512.txt")
    {
        return true;
    }
    // Ephemeral agent-write paths + the runtime execution log.
    if rel.starts_with("runtime/outputs") || rel.starts_with("runtime/LOG.jsonl") {
        return true;
    }
    // Per-task verification sidecars (written after emit_package returns).
    if rel.starts_with("runtime/verification-reports") {
        return true;
    }
    // Runtime audit/ECAA sidecars + affordance sidecars — emitted after
    // the BagIt manifest and overwritable by the conversation emit path.
    matches!(
        rel.to_string_lossy().as_ref(),
        "runtime/intake-conversation.jsonl"
            | "runtime/decisions.jsonl"
            | "runtime/proofs.jsonl"
            | "runtime/claim-verification.json"
            | "runtime/verifier-decisions.jsonl"
            | "runtime/assumptions.jsonl"
            | "runtime/validation-reports.jsonl"
            | "runtime/determinism-shim.json"
            | "runtime/security-policy.json"
            | "runtime/audit-proof-report.json"
            | "runtime/validation-summary.json"
            | "runtime/policy-decisions.jsonl"
            | "runtime/decisions.jsonl.mac"
            | "runtime/plot_affordances.jsonl"
            | "runtime/affordance_fallbacks.jsonl"
    )
}

/// Recursively read every non-excluded file under `root` into a
/// path-keyed map of bytes. Exclusion is applied AT the walk (mirroring
/// `bagit::walk_for_manifest`) so a newly non-deterministic payload file
/// surfaces as a file-set / byte mismatch rather than being silently
/// normalized away.
fn collect_baseline(root: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(
        root: &std::path::Path,
        current: &std::path::Path,
        out: &mut BTreeMap<String, Vec<u8>>,
    ) {
        for entry in std::fs::read_dir(current).expect("read_dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            let rel = path.strip_prefix(root).expect("strip_prefix").to_path_buf();
            if is_excluded_from_baseline(&rel) {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.is_file() {
                // POSIX-normalize the key so it is host-OS independent.
                let key = rel.to_string_lossy().replace('\\', "/");
                out.insert(key, std::fs::read(&path).expect("read file"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn emit_package_whole_package_byte_reproducible() {
    // The whole-package determinism guarantee: two emits of the same
    // intake must produce a byte-identical package across the entire
    // exclusion-walked file set — including ro-crate-metadata.json (which
    // the narrower deterministic test deliberately skipped) — and the
    // BagIt `manifest-sha512.txt` itself must be byte-identical. The
    // exclusion set is applied at the walk so a newly non-deterministic
    // payload file fails loudly rather than slipping through.
    let policies = policies_dir();
    let clf = test_classification();
    let dag = rnaseq_dag();

    let emit_into = || -> std::path::PathBuf {
        let tmp = TempDir::new().unwrap();
        emit_package(&EmitConfig {
            objective: None,
            output_dir: tmp.path(),
            dag: &dag,
            classification: &clf,
            policies_dir: &policies,
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
            edge_kinds: None,
        })
        .expect("emit");
        // Keep the dir alive past the closure so the post-walk reads
        // below see the package on disk.
        tmp.keep()
    };

    let dir_a = emit_into();
    let dir_b = emit_into();

    let base_a = collect_baseline(&dir_a);
    let base_b = collect_baseline(&dir_b);

    // Same set of files.
    let keys_a: Vec<&String> = base_a.keys().collect();
    let keys_b: Vec<&String> = base_b.keys().collect();
    assert_eq!(
        keys_a, keys_b,
        "two emits produced different file sets:\nA: {keys_a:?}\nB: {keys_b:?}"
    );

    // ro-crate-metadata.json is the file the narrow determinism test
    // skipped — assert it is in the walked set so this test actually
    // covers it.
    assert!(
        base_a.contains_key("ro-crate-metadata.json"),
        "ro-crate-metadata.json must be part of the byte-reproducibility baseline"
    );

    // Every non-excluded file is byte-identical.
    for (key, bytes_a) in &base_a {
        let bytes_b = base_b.get(key).expect("key present in both");
        assert_eq!(
            bytes_a.len(),
            bytes_b.len(),
            "file {key} byte length drifted across emits"
        );
        assert_eq!(bytes_a, bytes_b, "file {key} bytes diverged across emits");
    }

    // The BagIt payload manifest itself must be byte-identical.
    let manifest_a = std::fs::read(dir_a.join("manifest-sha512.txt")).unwrap();
    let manifest_b = std::fs::read(dir_b.join("manifest-sha512.txt")).unwrap();
    assert_eq!(
        manifest_a, manifest_b,
        "manifest-sha512.txt diverged across emits — the package is not byte-reproducible"
    );

    std::fs::remove_dir_all(&dir_a).ok();
    std::fs::remove_dir_all(&dir_b).ok();
}

#[test]
fn ro_crate_graph_carries_typed_invariant_verdict_nodes() {
    // The audit-proof verdicts must be FIRST-CLASS typed nodes inside the
    // RO-Crate @graph (preprint claim: "first-class triples inside the
    // package"), not merely a file-reference to runtime/audit-proof-report.json.
    // Emit a package, parse ro-crate-metadata.json, and assert:
    //   (a) the @graph carries one InvariantVerdict node per audit-proof
    //       invariant (the 6-invariant report), each with a verdict status
    //       and an evaluated_against edge to the package IRI; and
    //   (b) the file-reference CreativeWork for the report is STILL present.
    let tmp = TempDir::new().unwrap();
    emit_plain(tmp.path(), &policies_dir());

    let content = std::fs::read_to_string(tmp.path().join("ro-crate-metadata.json")).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&content).expect("valid JSON");
    let graph = meta
        .get("@graph")
        .expect("@graph required")
        .as_array()
        .unwrap();

    // (a) Typed InvariantVerdict nodes.
    let verdict_nodes: Vec<&serde_json::Value> = graph
        .iter()
        .filter(|e| e.get("@type").and_then(|t| t.as_str()) == Some("InvariantVerdict"))
        .collect();
    assert!(
        verdict_nodes.len() >= 6,
        "expected >= 6 InvariantVerdict nodes in @graph, found {}",
        verdict_nodes.len()
    );
    for node in &verdict_nodes {
        assert!(
            node.get("@id")
                .and_then(|v| v.as_str())
                .map(|s| s.starts_with("A:"))
                .unwrap_or(false),
            "InvariantVerdict node must carry a deterministic A:<id> @id, got {:?}",
            node.get("@id")
        );
        assert!(
            node.get("verdict").and_then(|v| v.as_str()).is_some(),
            "InvariantVerdict node must carry a verdict status property, got {node:?}"
        );
        // No wall-clock / uuid value may leak into the graph node body.
        assert!(
            node.get("evaluated_at").is_none(),
            "InvariantVerdict node must NOT carry evaluated_at (determinism hazard): {node:?}"
        );
        let evaluated_against = node
            .get("evaluated_against")
            .and_then(|v| v.get("@id"))
            .and_then(|v| v.as_str());
        assert_eq!(
            evaluated_against,
            Some("ro-crate-metadata.json"),
            "InvariantVerdict node must carry an evaluated_against edge to the package IRI"
        );
    }

    // (b) The file-reference CreativeWork pointer is still present.
    let file_ref = graph
        .iter()
        .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("runtime/audit-proof-report.json"));
    let file_ref = file_ref.expect("file-ref CreativeWork for the report must remain in @graph");
    assert_eq!(
        file_ref.get("@type").and_then(|v| v.as_str()),
        Some("CreativeWork"),
        "report file-ref must stay a CreativeWork"
    );
}

#[test]
fn amend_emit_is_byte_reproducible() {
    // Regression guard for D1: the amend/branch hashed payload must not
    // embed the parent's absolute on-disk path (which is host-rooted and
    // breaks cross-machine byte-reproducibility). Emit a parent once,
    // then re-emit a child twice with `amend_from` + an `AmendContext`,
    // and assert (a) no `/home/` / absolute parent path leaks into
    // `policies/amendment-lineage.json`, and (b) the child's
    // `manifest-sha512.txt` is byte-identical across the two re-emits.
    // Pre-D1 the leak assertion fails; post-D1 it passes (the leak is
    // gone). Same-machine re-emits would otherwise pass spuriously, so
    // (a) is what ties this test to the D1 fix.
    let parent_tmp = TempDir::new().unwrap();
    emit_plain(parent_tmp.path(), &policies_dir());
    let parent_path = parent_tmp.path().to_path_buf();
    let parent_abs = parent_path.to_string_lossy().to_string();

    // Reference real stage ids from the fixture DAG so the AmendContext
    // is realistic (rnaseq_dag carries a differential_expression stage).
    let ctx = AmendContext {
        reason: Some("Switch DE method".into()),
        amended_stage: "differential_expression".into(),
        invalidated_tasks: vec![
            "differential_expression".into(),
            "validate_differential_expression".into(),
        ],
    };
    let dag = rnaseq_dag();
    let clf = test_classification();

    let emit_child = || -> std::path::PathBuf {
        let child_tmp = TempDir::new().unwrap();
        emit_package(&EmitConfig {
            objective: None,
            output_dir: child_tmp.path(),
            dag: &dag,
            classification: &clf,
            policies_dir: &policies_dir(),
            policy_allowlist: None,
            claim_boundary: None,
            compute_profiles_dir: None,
            intake_facts: None,
            amend_from: Some(parent_path.as_path()),
            amend_context: Some(&ctx),
            validation_contract_ref: None,
            preferred_container: None,
            runtime_prereqs: None,
            per_atom_runtime_prereqs: None,
            stage_atoms_dir: None,
            experimental_archetype: false,
            edge_kinds: None,
        })
        .expect("amend emit");
        child_tmp.keep()
    };

    let child_a = emit_child();
    let child_b = emit_child();

    // (a) No absolute parent path leaks into the hashed lineage policy.
    let lineage_a =
        std::fs::read_to_string(child_a.join("policies/amendment-lineage.json")).unwrap();
    assert!(
        !lineage_a.contains("/home/"),
        "amendment-lineage.json leaked an absolute host path: {lineage_a}"
    );
    assert!(
        !lineage_a.contains(&parent_abs),
        "amendment-lineage.json leaked the parent's absolute path: {lineage_a}"
    );

    // The ro-crate metadata is also in the hashed payload — assert no
    // leak there either (covers the patch_ro_crate_with_amendment sink).
    let ro_crate_a = std::fs::read_to_string(child_a.join("ro-crate-metadata.json")).unwrap();
    assert!(
        !ro_crate_a.contains(&parent_abs),
        "ro-crate-metadata.json leaked the parent's absolute path"
    );

    // (b) The child manifest is byte-identical across the two re-emits.
    let manifest_a = std::fs::read(child_a.join("manifest-sha512.txt")).unwrap();
    let manifest_b = std::fs::read(child_b.join("manifest-sha512.txt")).unwrap();
    assert_eq!(
        manifest_a, manifest_b,
        "amend-emit manifest-sha512.txt diverged across re-emits — amend payload is not byte-reproducible"
    );

    std::fs::remove_dir_all(&child_a).ok();
    std::fs::remove_dir_all(&child_b).ok();
}
