use crate::ablation::{AblationFlag, AblationFlagExt};
use crate::classify::ClassificationResult;
use crate::clock::Clock;
use crate::dag::DAG;
use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
use anyhow::{anyhow, Context, Result};
use jsonschema::JSONSchema;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

const INTENT_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/intent.schema.json"
));
const DECISION_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/decision.schema.json"
));
const EXECUTION_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/execution.schema.json"
));
const EVIDENCE_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/evidence.schema.json"
));
const CLAIM_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/claim.schema.json"
));
const EQUIVALENCE_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/equivalence.schema.json"
));
const FAILURE_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/failure.schema.json"
));
const AUDIT_PROOF_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/audit-proof.schema.json"
));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ValidationMode {
    Disabled,
    SchemaOnly,
    Full,
}

impl ValidationMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::SchemaOnly => "schema_only",
            Self::Full => "full",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidecarSource {
    EmitTime,
    HarnessRuntime,
}

pub(super) fn write_emit_time_sidecars(
    output_dir: &Path,
    dag: &DAG,
    classification: &ClassificationResult,
    clock: &dyn Clock,
) -> Result<()> {
    let runtime = output_dir.join("runtime");
    std::fs::create_dir_all(&runtime).context("creating runtime dir for ECAA sidecars")?;

    write_text(
        &runtime.join("intake-conversation.jsonl"),
        &render_intake_conversation_jsonl(classification, clock)?,
    )?;
    if !AblationFlag::DecisionRecords.is_active() {
        write_text(&runtime.join("decisions.jsonl"), "")?;
    }
    write_text(
        &runtime.join("proofs.jsonl"),
        &render_dependency_proofs_jsonl(dag)?,
    )?;
    write_pretty_json(
        &runtime.join("claim-verification.json"),
        &json!({
            "schema_version": "1",
            "n_checked": 0,
            "n_verified": 0,
            "n_unverifiable": 0,
            "n_mismatch": 0,
            "verdicts": [],
        }),
    )?;
    write_text(&runtime.join("verifier-decisions.jsonl"), "")?;
    write_text(&runtime.join("assumptions.jsonl"), "")?;
    write_text(&runtime.join("validation-reports.jsonl"), "")?;

    let determinism = crate::determinism_shim::serialize_active_settings();
    write_pretty_json(&runtime.join("determinism-shim.json"), &determinism)?;

    let mut digests = BTreeSet::new();
    for task in dag.tasks.values() {
        if let Some(container) = &task.container {
            if !container.digest.trim().is_empty() {
                digests.insert(container.digest.clone());
            }
        }
    }
    let digests: Vec<String> = digests.into_iter().collect();
    let security = crate::atom_safety::aggregate_for_package(&[], digests);
    write_pretty_json(&runtime.join("security-policy.json"), &security)?;

    Ok(())
}

pub(super) fn write_audit_proof_report(output_dir: &Path) -> Result<()> {
    if AblationFlag::AuditProof.is_active() {
        return Ok(());
    }
    let validator = crate::wrroc_validator::NoopWrrocValidator;
    let report = crate::audit_proof::run_audit_proof(output_dir, &validator)
        .context("running audit-proof invariants")?;
    write_pretty_json(
        &output_dir.join("runtime").join("audit-proof-report.json"),
        &report,
    )
}

pub(super) fn write_validation_summary(output_dir: &Path) -> Result<()> {
    let mode = read_validation_mode();
    let (passed, failed, skipped_pending_harness) = if mode == ValidationMode::Disabled {
        (0usize, Vec::new(), 0usize)
    } else {
        validate_sidecar_schemas(output_dir)?
    };
    let schema_failed = !failed.is_empty();

    let external_validation = if mode == ValidationMode::Full {
        Some(json!({
            "shacl_projection": unavailable_external_check(),
            "owl_consistency": unavailable_external_check(),
            "runcrate_validate": unavailable_external_check(),
        }))
    } else {
        None
    };

    let summary = json!({
        "schema_version": "0.1",
        "mode": mode.as_str(),
        "schema_validation": {
            "passed": passed,
            "failed": failed,
            "skipped_pending_harness": skipped_pending_harness,
        },
        "external_validation": external_validation,
        "duration_ms": 0,
    });
    write_pretty_json(
        &output_dir.join("runtime").join("validation-summary.json"),
        &summary,
    )?;

    if schema_failed && validation_blocks_on_fail() {
        return Err(anyhow!(
            "ECAA emit-time validation blocked: schema failure(s) and ECAA_VALIDATION_BLOCK_ON_FAIL=1"
        ));
    }
    Ok(())
}

fn render_intake_conversation_jsonl(
    classification: &ClassificationResult,
    clock: &dyn Clock,
) -> Result<String> {
    let turn = json!({
        "id": "intent:turn:00000000-0000-0000-0000-000000000001",
        "type": "Question",
        "turn_id": "00000000-0000-0000-0000-000000000001",
        "role": "user",
        "timestamp": clock.now_rfc3339(),
        "content": classification.intake_text,
    });
    let mut line = serde_json::to_string(&turn).context("serializing intake conversation turn")?;
    line.push('\n');
    Ok(line)
}

fn render_dependency_proofs_jsonl(dag: &DAG) -> Result<String> {
    let mut out = String::new();
    for (to_node, task) in &dag.tasks {
        let mut deps = task.depends_on.clone();
        deps.sort();
        deps.dedup();
        for from_node in deps {
            let edge = serde_json::to_value(EdgeContract {
                from_node: from_node.to_string(),
                from_port: "output".to_string(),
                to_node: to_node.to_string(),
                to_port: "input".to_string(),
                proof: CompatibilityProof {
                    producer_type: "workflow_artifact".to_string(),
                    consumer_type: "workflow_artifact".to_string(),
                    rationale: Some(format!(
                        "WORKFLOW.json dependency {} -> {} emitted as ECAA evidence.",
                        from_node, to_node
                    )),
                    ..CompatibilityProof::default()
                },
                chain_of_custody: None,
            })
            .context("serializing dependency proof edge")?;
            let mut edge = edge
                .as_object()
                .cloned()
                .context("dependency proof edge should serialize as an object")?;
            edge.insert("id".to_string(), json!(format!("workflow:{to_node}")));
            edge.insert("type".to_string(), json!("WorkflowStep"));
            edge.insert(
                "computed_from".to_string(),
                json!(format!("workflow:{from_node}")),
            );
            out.push_str(&serde_json::to_string(&edge).context("serializing dependency proof")?);
            out.push('\n');
        }
    }
    Ok(out)
}

fn read_validation_mode() -> ValidationMode {
    match std::env::var("ECAA_VALIDATE_ON_EMIT")
        .unwrap_or_default()
        .as_str()
    {
        "off" | "0" | "false" | "no" => ValidationMode::Disabled,
        "full" => ValidationMode::Full,
        "schema_only" | "" => ValidationMode::SchemaOnly,
        _ => ValidationMode::SchemaOnly,
    }
}

fn validation_blocks_on_fail() -> bool {
    matches!(
        std::env::var("ECAA_VALIDATION_BLOCK_ON_FAIL")
            .as_deref()
            .unwrap_or("0"),
        "1" | "true" | "yes" | "on"
    )
}

fn unavailable_external_check() -> Value {
    json!({
        "status": "unavailable",
        "reason": "core emit path performs deterministic schema validation; run external validators from the conversation or harness validation gate",
    })
}

fn sidecar_schemas() -> [(&'static str, bool, &'static str, SidecarSource); 8] {
    [
        (
            "runtime/intake-conversation.jsonl",
            true,
            INTENT_SCHEMA,
            SidecarSource::EmitTime,
        ),
        (
            "runtime/decisions.jsonl",
            true,
            DECISION_SCHEMA,
            SidecarSource::EmitTime,
        ),
        (
            "runtime/validation-reports.jsonl",
            true,
            EXECUTION_SCHEMA,
            SidecarSource::HarnessRuntime,
        ),
        (
            "runtime/proofs.jsonl",
            true,
            EVIDENCE_SCHEMA,
            SidecarSource::EmitTime,
        ),
        (
            "runtime/claim-verification.json",
            false,
            CLAIM_SCHEMA,
            SidecarSource::EmitTime,
        ),
        (
            "runtime/verifier-decisions.jsonl",
            true,
            EQUIVALENCE_SCHEMA,
            SidecarSource::HarnessRuntime,
        ),
        (
            "runtime/assumptions.jsonl",
            true,
            FAILURE_SCHEMA,
            SidecarSource::EmitTime,
        ),
        (
            "runtime/audit-proof-report.json",
            false,
            AUDIT_PROOF_SCHEMA,
            SidecarSource::EmitTime,
        ),
    ]
}

fn ablated_sidecar(relpath: &str) -> bool {
    match relpath {
        "runtime/decisions.jsonl" => AblationFlag::DecisionRecords.is_active(),
        "runtime/audit-proof-report.json" => AblationFlag::AuditProof.is_active(),
        _ => false,
    }
}

fn validate_sidecar_schemas(output_dir: &Path) -> Result<(usize, Vec<Value>, usize)> {
    let mut passed = 0usize;
    let mut failed = Vec::new();
    let mut skipped_pending_harness = 0usize;

    for (relpath, is_jsonl, schema_src, source) in sidecar_schemas() {
        if matches!(source, SidecarSource::HarnessRuntime) {
            skipped_pending_harness += 1;
            continue;
        }

        let path = output_dir.join(relpath);
        if !path.exists() {
            if ablated_sidecar(relpath) {
                continue;
            }
            failed.push(json!({
                "sidecar": relpath,
                "line_index": null,
                "error": "sidecar missing",
            }));
            continue;
        }

        let schema_value: Value = serde_json::from_str(schema_src)
            .with_context(|| format!("parsing embedded schema for {}", relpath))?;
        let validator = JSONSchema::compile(&schema_value)
            .map_err(|e| anyhow!("compiling embedded schema for {}: {}", relpath, e))?;
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;

        let instance = if is_jsonl {
            let mut entries = Vec::new();
            let mut parse_error = None;
            for (idx, line) in raw.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(value) => entries.push(value),
                    Err(e) => {
                        parse_error = Some((idx, e.to_string()));
                        break;
                    }
                }
            }
            if let Some((idx, err)) = parse_error {
                failed.push(json!({
                    "sidecar": relpath,
                    "line_index": idx,
                    "error": format!("JSON parse error: {err}"),
                }));
                continue;
            }
            Value::Array(entries)
        } else if raw.trim().is_empty() {
            Value::Null
        } else {
            match serde_json::from_str::<Value>(raw.trim()) {
                Ok(value) => value,
                Err(e) => {
                    failed.push(json!({
                        "sidecar": relpath,
                        "line_index": null,
                        "error": format!("JSON parse error: {e}"),
                    }));
                    continue;
                }
            }
        };

        let messages: Vec<String> = match validator.validate(&instance) {
            Ok(()) => Vec::new(),
            Err(errors) => errors.map(|e| e.to_string()).collect(),
        };
        if messages.is_empty() {
            passed += 1;
        } else {
            failed.push(json!({
                "sidecar": relpath,
                "line_index": null,
                "error": format!("schema validation: {}", messages.join("; ")),
            }));
        }
    }

    Ok((passed, failed, skipped_pending_harness))
}

fn write_text(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    crate::fs_helpers::atomic_write_bytes_sync(path, text.as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

fn write_pretty_json<T: serde::Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .with_context(|| format!("serializing {}", path.display()))?;
    bytes.push(b'\n');
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    crate::fs_helpers::atomic_write_bytes_sync(path, &bytes)
        .with_context(|| format!("writing {}", path.display()))
}
