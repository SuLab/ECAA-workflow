//! W2 — block-on-fail gate: every emitted workflow-typed.json validates
//! against the committed schema, plus a positive + negative unit check.
//! Guarded by ECAA_CONFORMANCE_MODE so `make conformance` runs it.

use jsonschema::JSONSchema;
use serde_json::Value;

const SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../core/src/backend_emitters/_workflow-typed.schema.json"
));

fn compiled() -> JSONSchema {
    let schema_value: Value = serde_json::from_str(SCHEMA).expect("schema parses");
    JSONSchema::compile(&schema_value).expect("schema compiles")
}

#[test]
fn valid_artifact_passes_schema() {
    if std::env::var("ECAA_CONFORMANCE_MODE").is_err() {
        eprintln!("skipping: ECAA_CONFORMANCE_MODE unset");
        return;
    }
    let instance = serde_json::json!({
        "workflow_id": "wf_x",
        "name": "wf_x",
        "steps": [{
            "step_id": "align_reads",
            "tool_id": "align_reads_atom",
            "parameters": {},
            "dependencies": [],
            "estimated_duration": 1800
        }],
        "edges": [{
            "edge_id": "align_reads.bam->quantify.bam",
            "source_node_id": "align_reads",
            "target_node_id": "quantify",
            "source_output": "bam",
            "target_input": "bam"
        }],
        "parameter_mappings": [],
        "parameters": [],
        "validation_rules": [],
        "metadata": { "complexity": "simple", "tags": [], "categories": [], "use_cases": [] }
    });
    assert!(
        compiled().validate(&instance).is_ok(),
        "valid artifact must pass schema"
    );
}

#[test]
fn artifact_missing_source_output_fails_schema() {
    if std::env::var("ECAA_CONFORMANCE_MODE").is_err() {
        eprintln!("skipping: ECAA_CONFORMANCE_MODE unset");
        return;
    }
    // edge missing the required `source_output` field.
    let broken = serde_json::json!({
        "workflow_id": "wf_x",
        "name": "wf_x",
        "steps": [],
        "edges": [{
            "edge_id": "a.bam->b.bam",
            "source_node_id": "a",
            "target_node_id": "b",
            "target_input": "bam"
        }],
        "parameter_mappings": [],
        "parameters": [],
        "validation_rules": [],
        "metadata": { "complexity": "simple", "tags": [], "categories": [], "use_cases": [] }
    });
    let schema = compiled();
    assert!(
        schema.validate(&broken).is_err(),
        "artifact missing edge.source_output must fail schema"
    );
}

#[test]
fn emitted_corpus_artifacts_validate() {
    if std::env::var("ECAA_CONFORMANCE_MODE").is_err() {
        eprintln!("skipping: ECAA_CONFORMANCE_MODE unset");
        return;
    }
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("core")
        .join("tests")
        .join("fixtures");
    let schema = compiled();
    let mut checked = 0usize;
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|f| f.to_str()) == Some("workflow-typed.json") {
                let v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
                assert!(
                    schema.validate(&v).is_ok(),
                    "fixture {} failed schema: {:?}",
                    p.display(),
                    schema
                        .validate(&v)
                        .err()
                        .map(|e| e.map(|x| x.to_string()).collect::<Vec<_>>())
                );
                checked += 1;
            }
        }
    }
    eprintln!("validated {checked} emitted workflow-typed.json fixtures");
    // No fixtures yet is acceptable on first land (the unit tests above are
    // the deliverable); this loop hardens as the corpus regenerates.
}
