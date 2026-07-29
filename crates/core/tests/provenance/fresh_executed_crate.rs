//! Offline driver: produce a FRESH executed/finalized RO-Crate from the CURRENT
//! emitter and assert it carries the REAL structure the
//! `provenance-run-crate-0.5` SHACL shape requires.
//!
//! ## Why this exists
//!
//! The committed `testdata/replay/himes-parent` executed fixture is a trimmed
//! two-stage replay slice, not a full finalized crate from the current emitter.
//! To validate the FINALIZE path's provenance shape against the shape *as the
//! current emitter produces it*, we drive the real functions end to end with no
//! live compute / network / containers:
//!
//!   1. `ro_crate::build_metadata` emits a PLAN crate (the real pre-execution
//!      emit — a workflow definition with HowToSteps + ParameterConnections).
//!   2. We lay down realistic per-task execution artifacts on disk
//!      (`runtime/outputs/<task>/<table>.tsv`, `.container-state.json`,
//!      `env.lock`, `runtime/dependency-lock.json`) — the byproducts a real run
//!      leaves behind.
//!   3. `ro_crate::finalize_evidence_registration` registers the real
//!      per-output `CreateAction`s + tool `SoftwareApplication`s and upgrades
//!      `conformsTo` to the executed 6-profile set.
//!
//! The resulting `ro-crate-metadata.json` is the artifact the strict
//! provenance gate validates. `ECAA_FRESH_EXECUTED_CRATE_OUT`, when set, dumps
//! the finalized crate to that directory so an out-of-process validator
//! (`scripts/roc-validate-strict.py`) can be pointed at it.

use ecaa_workflow_core::classify::ClassificationResult;
use ecaa_workflow_core::clock::FrozenClock;
use ecaa_workflow_core::dag::{Task, DAG};
use ecaa_workflow_core::ids::TaskId;
use ecaa_workflow_core::ro_crate;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// A small chain DAG that mirrors a real bulk-RNA-seq pipeline shape: three
/// compute tasks with a linear dependency chain so the emitted graph carries
/// HowToSteps + ParameterConnections (the edges whose parameter @ids must
/// resolve to real FormalParameters).
fn chain_dag() -> DAG {
    let mk = |desc: &str, deps: Vec<&str>| -> Task {
        serde_json::from_value(json!({
            "kind": "computation",
            "state": {"status": "completed", "result": {}},
            "depends_on": deps,
            "assignee": "agent",
            "description": desc,
            "spec": {"edam_operation": "operation:3223"}
        }))
        .expect("task deserializes")
    };
    let mut tasks: BTreeMap<TaskId, Task> = BTreeMap::new();
    tasks.insert(
        TaskId::from("data_acquisition"),
        mk("Acquire raw counts", vec![]),
    );
    tasks.insert(
        TaskId::from("normalisation"),
        mk("Normalise counts", vec!["data_acquisition"]),
    );
    tasks.insert(
        TaskId::from("differential_expression"),
        mk("Run differential expression", vec!["normalisation"]),
    );
    let mut dag = DAG {
        version: "1.0".into(),
        schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
        workflow_id: "bulk-rnaseq-de".into(),
        current_task: None,
        tasks,
        // No package_run_id: keep the root free of an inline `additionalProperty`
        // PropertyValue, matching the CLI-emitted baseline crate so the
        // structural assertions isolate the provenance-shape concern.
        run_id: None,
        reverse_deps: BTreeMap::new(),
        execution_order: Vec::new(),
    };
    dag.rebuild_reverse_deps();
    dag
}

fn classification() -> ClassificationResult {
    ClassificationResult {
        domain: "genomics".into(),
        workflow_description: "bulk RNA-seq differential expression".into(),
        intake_text: "test intake".into(),
        edam_topic: "topic:3170".into(),
        edam_operation: "operation:3223".into(),
        ..Default::default()
    }
}

/// Lay down one produced output table + recorded container-state + env.lock for
/// a task, mirroring what a real execution leaves under `runtime/outputs/`.
fn seed_task_outputs(root: &Path, task: &str, table: &str) {
    let dir = root.join("runtime/outputs").join(task);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(table), "gene\tlog2fc\tpadj\nA\t1.2\t0.01\n").unwrap();
    // The REAL code the step ran, recorded per the executor brief. This is the
    // tool the finalize path registers as the CreateAction's `instrument`.
    let scripts = dir.join("scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    std::fs::write(
        scripts.join("01_run.R"),
        "#!/usr/bin/env Rscript\n# tool versions: DESeq2 1.50.2\nlibrary(DESeq2)\n",
    )
    .unwrap();
    std::fs::write(
        dir.join(".container-state.json"),
        serde_json::to_vec_pretty(&json!({
            "image": "ghcr.io/scripps/scripps-bio-base:1.4.4",
            "runtime": "docker",
            "backend": "local",
            "ended_at": "2026-06-25T12:00:00Z"
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("env.lock"),
        "pydeseq2==0.5.4\nbioconductor-deseq2: 1.50.2\n",
    )
    .unwrap();
}

/// Emit a real PLAN crate then finalize it into an executed crate, returning the
/// package root.
fn build_finalized_crate(root: &Path) {
    let dag = chain_dag();
    let metadata = ro_crate::build_metadata(&dag, &classification(), &FrozenClock::default());
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(
        root.join("ro-crate-metadata.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    // Minimal BagIt tag files so the manifest re-seal has something to write.
    std::fs::write(root.join("bagit.txt"), "BagIt-Version: 1.0\n").unwrap();
    std::fs::write(root.join("manifest-sha512.txt"), "").unwrap();
    std::fs::write(root.join("WORKFLOW.json"), "{\"version\":\"1.0\"}").unwrap();

    // Realistic dependency lock so register_software_dependencies emits tools.
    std::fs::create_dir_all(root.join("runtime")).unwrap();
    std::fs::write(
        root.join("runtime/dependency-lock.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": "1.0",
            "r": [{"name": "DESeq2", "requested": ">=1.40.0", "resolved": "1.50.2"}],
            "python": [{"name": "pydeseq2", "requested": ">=0.5.0", "resolved": "0.5.4"}]
        }))
        .unwrap(),
    )
    .unwrap();

    // One produced table per compute task.
    seed_task_outputs(root, "data_acquisition", "counts.tsv");
    seed_task_outputs(root, "normalisation", "normalised.tsv");
    seed_task_outputs(root, "differential_expression", "de_results.tsv");

    let clock = FrozenClock::default();
    ro_crate::finalize_evidence_registration(root, &clock).unwrap();
}

fn read_graph(root: &Path) -> Vec<Value> {
    let bytes = std::fs::read(root.join("ro-crate-metadata.json")).unwrap();
    let doc: Value = serde_json::from_slice(&bytes).unwrap();
    doc["@graph"].as_array().unwrap().clone()
}

fn types_of(e: &Value) -> Vec<String> {
    match e.get("@type") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => vec![],
    }
}

fn find_by_id<'a>(graph: &'a [Value], id: &str) -> Option<&'a Value> {
    graph
        .iter()
        .find(|e| e.get("@id").and_then(Value::as_str) == Some(id))
}

/// Drive the whole pipeline and (optionally) dump the finalized crate to
/// `$ECAA_FRESH_EXECUTED_CRATE_OUT` so an out-of-process SHACL validator can be
/// pointed at it.
#[test]
fn fresh_executed_crate_satisfies_provenance_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("pkg");
    build_finalized_crate(&root);

    // Dump the finalized crate FIRST (before assertions) so the out-of-process
    // SHACL validator can be pointed at it even while the in-process structural
    // assertions are being iterated on.
    if let Ok(out) = std::env::var("ECAA_FRESH_EXECUTED_CRATE_OUT") {
        let out = Path::new(&out);
        if out.exists() {
            let _ = std::fs::remove_dir_all(out);
        }
        copy_tree(&root, out);
        eprintln!("dumped fresh executed crate to {}", out.display());
    }

    let graph = read_graph(&root);

    // ── conformsTo upgraded to the executed 6-profile set ──────────────────
    let root_entity = find_by_id(&graph, "./").expect("root entity");
    let declared: Vec<&str> = root_entity["conformsTo"]
        .as_array()
        .expect("root conformsTo is an array")
        .iter()
        .filter_map(|c| c.get("@id").and_then(Value::as_str))
        .collect();
    assert!(
        declared.contains(&"https://w3id.org/ro/wfrun/provenance/0.5"),
        "executed crate must claim provenance-run-crate; got {declared:?}"
    );

    // ── ParameterConnection sourceParameter/targetParameter resolve to real
    //    FormalParameter entities (provenance must/5_parameterconnection.ttl) ─
    let connections: Vec<&Value> = graph
        .iter()
        .filter(|e| types_of(e).iter().any(|t| t == "ParameterConnection"))
        .collect();
    assert!(!connections.is_empty(), "expected ParameterConnections");
    for pc in &connections {
        for endpoint in ["sourceParameter", "targetParameter"] {
            let id = pc[endpoint]["@id"]
                .as_str()
                .unwrap_or_else(|| panic!("{endpoint} has @id on {pc:?}"));
            let referenced = find_by_id(&graph, id)
                .unwrap_or_else(|| panic!("{endpoint} {id} must resolve to a graph entity"));
            assert!(
                types_of(referenced).iter().any(|t| t == "FormalParameter"),
                "{endpoint} {id} must be a FormalParameter; got {:?}",
                types_of(referenced)
            );
        }
    }

    // ── ComputationalWorkflow hasPart references real tool entities that are
    //    instruments of real CreateActions (provenance must/0_computational_workflow
    //    + must/0_tool.ttl "Tool inverse instrument") ─────────────────────────
    let wf = graph
        .iter()
        .find(|e| types_of(e).iter().any(|t| t == "ComputationalWorkflow"))
        .expect("ComputationalWorkflow entity");
    // workflow that links steps MUST also be typed HowTo.
    assert!(
        types_of(wf).iter().any(|t| t == "HowTo"),
        "ComputationalWorkflow that links steps must carry @type HowTo; got {:?}",
        types_of(wf)
    );
    let has_part: Vec<&str> = wf["hasPart"]
        .as_array()
        .expect("workflow hasPart is an array")
        .iter()
        .filter_map(|p| p.get("@id").and_then(Value::as_str))
        .collect();
    assert!(
        !has_part.is_empty(),
        "workflow hasPart must reference tools"
    );

    // Build the set of @ids that are an `instrument` of a real CreateAction.
    let create_action_instruments: std::collections::BTreeSet<String> = graph
        .iter()
        .filter(|e| types_of(e).iter().any(|t| t == "CreateAction"))
        .filter_map(|a| {
            a.get("instrument")
                .and_then(|i| i.get("@id"))
                .and_then(Value::as_str)
        })
        .map(String::from)
        .collect();

    let tool_types = [
        "SoftwareApplication",
        "SoftwareSourceCode",
        "ComputationalWorkflow",
    ];
    let mut tool_haspart = 0usize;
    for part_id in &has_part {
        let Some(part) = find_by_id(&graph, part_id) else {
            continue;
        };
        let pt = types_of(part);
        if pt.iter().any(|t| tool_types.contains(&t.as_str())) {
            tool_haspart += 1;
            // Tool inverse instrument: this hasPart tool MUST be an instrument
            // of a real CreateAction.
            assert!(
                create_action_instruments.contains(*part_id),
                "hasPart tool {part_id} must be the instrument of a real CreateAction"
            );
            // Tool input/output (if present) MUST be FormalParameters.
            for slot in ["input", "output"] {
                if let Some(arr) = part.get(slot).and_then(Value::as_array) {
                    for p in arr {
                        let id = p["@id"].as_str().expect("tool param @id");
                        let referenced = find_by_id(&graph, id).expect("tool param resolves");
                        assert!(
                            types_of(&referenced.clone())
                                .iter()
                                .any(|t| t == "FormalParameter"),
                            "tool {slot} {id} must be a FormalParameter"
                        );
                    }
                }
            }
        }
    }
    assert!(
        tool_haspart >= 1,
        "workflow hasPart must reference at least one real tool entity"
    );

    // ── Every HowToStep refers to its tool via workExample, typed as a tool
    //    (provenance must/1_howtostep.ttl "HowToStep workExample") ────────────
    for step in graph
        .iter()
        .filter(|e| types_of(e).iter().any(|t| t == "HowToStep"))
    {
        let we = step
            .get("workExample")
            .unwrap_or_else(|| panic!("HowToStep {:?} must have workExample", step.get("@id")));
        let we_id = we["@id"].as_str().expect("workExample @id");
        let referenced = find_by_id(&graph, we_id)
            .unwrap_or_else(|| panic!("workExample {we_id} must resolve to a graph entity"));
        assert!(
            types_of(referenced)
                .iter()
                .any(|t| tool_types.contains(&t.as_str())),
            "HowToStep workExample {we_id} must be a tool type; got {:?}",
            types_of(referenced)
        );
    }
}

/// A second finalize on an already-executed crate must be a no-op for the
/// provenance structure: no duplicate tool / FormalParameter / script entities,
/// and the `@graph` descriptor converges (idempotent registration).
#[test]
fn second_finalize_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("pkg");
    build_finalized_crate(&root);

    let graph_first = read_graph(&root);
    let first = std::fs::read(root.join("ro-crate-metadata.json")).unwrap();

    // Re-finalize. Registration is idempotent (existing @ids are skipped).
    ro_crate::finalize_evidence_registration(&root, &FrozenClock::default()).unwrap();
    let second = std::fs::read(root.join("ro-crate-metadata.json")).unwrap();
    assert_eq!(
        first, second,
        "second finalize must converge byte-identically"
    );

    // No duplicate @ids anywhere.
    let mut seen = std::collections::BTreeSet::new();
    for e in &graph_first {
        if let Some(id) = e.get("@id").and_then(Value::as_str) {
            assert!(seen.insert(id.to_string()), "duplicate @id {id} in graph");
        }
    }
    // Exactly one tool per produced task; each is the instrument of an action.
    let tools: Vec<&str> = graph_first
        .iter()
        .filter_map(|e| e.get("@id").and_then(Value::as_str))
        .filter(|id| id.starts_with("#tool/"))
        .collect();
    assert_eq!(
        tools.len(),
        3,
        "one tool per produced compute task; got {tools:?}"
    );
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}
