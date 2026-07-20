use crate::ablation::{AblationFlag, AblationFlagExt};
use crate::classify::ClassificationResult;
use crate::clock::Clock;
use crate::dag::DAG;
use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
use anyhow::{anyhow, Context, Result};
use jsonschema::JSONSchema;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
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
    edge_kinds: Option<&BTreeMap<(String, String), EdgeKind>>,
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
        &render_dependency_proofs_jsonl(dag, edge_kinds)?,
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

    // `reexecution.json` — the five-class re-execution `RerunOutcome` Q
    // sub-graph (Invariant 4's source). Written present-but-empty at every
    // emit so the file is uniformly present and the invariant has a defined
    // source; an empty `per_artifact` means "no re-execution performed" →
    // Inv 4 `Unverified`. The conversation emit path overwrites this with the
    // classified buckets on an amend/branch re-emit that has a parent package.
    write_pretty_json(
        &runtime.join("reexecution.json"),
        &json!({
            "schema_version": "0.1",
            "bucket_counts": {},
            "per_artifact": [],
        }),
    )?;

    // `determinism-shim.json` is a HOST-VARYING forensic env capture
    // (locale/timezone/seed policy + the applied-policy env-var names). It is
    // intentionally on the BagIt manifest exclusion list (see `emitter::bagit`,
    // `runtime/determinism-shim.json`) because its bytes genuinely differ by
    // compiler host and are refreshed at finalize by
    // `determinism_shim::merge_container_env` — it is a diagnostic, NOT a
    // re-verify input, so manifesting it would break cross-host
    // byte-reproducibility (it is surfaced instead as an RO-Crate `@graph`
    // CreativeWork, like `DEPOSIT-READINESS.json`). The one field that could
    // leak a host filesystem path — `temp_path_policy.root` — is normalized to
    // the deterministic package-relative `runtime/scratch` in
    // `serialize_active_settings`, so it never bakes the host `$TMPDIR` into
    // the package (det-07).
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

/// Generate the audit-proof report, persist it to
/// `runtime/audit-proof-report.json`, and return the report JSON so the
/// emit pipeline can project its verdicts into the RO-Crate `@graph` as
/// first-class `InvariantVerdict` nodes (see
/// `ro_crate::inject_audit_proof_verdict_nodes`). Returns `Ok(None)` when
/// the AuditProof ablation flag suppresses the report.
pub(super) fn write_audit_proof_report(output_dir: &Path) -> Result<Option<Value>> {
    if AblationFlag::AuditProof.is_active() {
        return Ok(None);
    }
    let validator = crate::wrroc_validator::NoopWrrocValidator;
    // DR-4: `evaluated_at` is anchored to the deterministic RUN epoch
    // (`run_epoch_clock`, = `SOURCE_DATE_EPOCH` or the `2026-01-01` base),
    // NOT the wall clock. This makes `audit-proof-report.json` byte-identical
    // across two emits of the same input, so the report is now a first-class
    // BagIt-manifested payload file at BOTH emit and reseal (rather than being
    // held off the manifest to hide a per-emit timestamp). The value matches
    // `ro-crate-metadata.json::dateCreated`, which is anchored to the same
    // run-epoch clock, so the two are CONSISTENT. "Manifest only at reseal"
    // does not make the manifest reproducible; stable bytes do.
    //
    // The projected `@graph` verdict nodes (`inject_audit_proof_verdict_nodes`)
    // still drop `evaluated_at`; the deterministic value also reaches
    // `ro-crate-metadata.json` cleanly if ever surfaced there.
    let report = crate::audit_proof::run_audit_proof(
        output_dir,
        &validator,
        &crate::clock::run_epoch_clock(),
    )
    .context("running audit-proof invariants")?;
    let report = serde_json::to_value(&report).context("serializing audit-proof report")?;
    write_pretty_json(
        &output_dir.join("runtime").join("audit-proof-report.json"),
        &report,
    )?;
    Ok(Some(report))
}

/// Core-side baseline for the closed tool-vocabulary size. The conversation
/// crate owns the authoritative `Tool::COUNT`; core cannot depend on it, so
/// the core emit path records this documented baseline. The conversation
/// emit path may re-emit the sidecar with the live count.
const ED_CF_TOOL_COUNT_BASELINE: usize = 22;
/// Core-side baseline for the high-impact alone-in-turn tool count.
const ED_CF_HIGH_IMPACT_TOOL_BASELINE: usize = 6;

/// Emit the ED/CF self-assessment sidecar to
/// `runtime/ed-cf-self-assessment.json`. Deterministic, warn-only,
/// informational — never blocks emission. Excluded from the BagIt
/// manifest (like the audit-proof report) because the conversation
/// emit path may re-emit it with the live tool counts.
///
/// `atom_count` / `modality_count` are derived from the config dirs when
/// available (atom registry dir + sibling `modalities/` dir); the tool
/// counts use the core-side baseline constants.
pub(super) fn write_ed_cf_self_assessment(
    output_dir: &Path,
    stage_atoms_dir: Option<&Path>,
) -> Result<()> {
    let atom_count = stage_atoms_dir
        .and_then(|d| crate::atom_registry::AtomRegistry::load_from_dir(d).ok())
        .map(|r| r.len())
        .unwrap_or(0);
    // The modality manifests live in a sibling `modalities/` dir of the
    // config root (stage-atoms' parent).
    let modality_count = stage_atoms_dir
        .and_then(|d| d.parent())
        .map(|cfg| cfg.join("modalities"))
        .map(|md| count_modality_manifests(&md))
        .unwrap_or(0);
    let inputs = crate::rubric_self_assessment::AssessmentInputs::from_package_facts(
        atom_count,
        modality_count,
        ED_CF_TOOL_COUNT_BASELINE,
        ED_CF_HIGH_IMPACT_TOOL_BASELINE,
    );
    let report = crate::rubric_self_assessment::EdCfSelfAssessment::from_inputs(&inputs);
    let value = serde_json::to_value(&report).context("serializing ED/CF self-assessment")?;
    write_pretty_json(
        &output_dir
            .join("runtime")
            .join("ed-cf-self-assessment.json"),
        &value,
    )
}

/// Count `<id>.yaml` modality manifests in `dir` (excluding `_*.yaml`
/// schema sidecars). Returns 0 when the dir is absent/unreadable.
fn count_modality_manifests(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let p = e.path();
            p.extension().and_then(|s| s.to_str()) == Some("yaml")
                && !p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with('_'))
                    .unwrap_or(false)
        })
        .count()
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
        // The product (non-conformance) build stays runcrate/python-free:
        // core emits the deterministic schema validation and records the
        // external checks as `unavailable`, deferring them to the
        // conversation/harness validation gate. Under ECAA_CONFORMANCE_MODE
        // a conformant build MUST actually run the external SHACL/OWL
        // validators, so we shell them out here over the emitted package.
        if read_conformance_mode() {
            Some(run_external_validators(output_dir))
        } else {
            Some(json!({
                "shacl_projection": unavailable_external_check(),
                "owl_consistency": unavailable_external_check(),
                "runcrate_validate": unavailable_external_check(),
            }))
        }
    } else {
        None
    };

    // Under conformance mode the external SHACL/OWL validators actually ran;
    // a real `fail` (not `unavailable`/`error`/`pass`) means the package
    // ABox does not conform, which must block the conformant emit alongside
    // schema failures. Outside conformance mode the external checks are
    // `unavailable` stubs and never block. Computed before `summary` moves
    // `external_validation` into the serialized JSON.
    let external_failed = read_conformance_mode()
        && external_validation
            .as_ref()
            .map(external_check_failed)
            .unwrap_or(false);

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

    if (schema_failed || external_failed) && validation_blocks_on_fail() {
        return Err(anyhow!(
            "ECAA emit-time validation blocked: conformance failure(s) (schema={schema_failed}, external={external_failed}) with block-on-fail engaged"
        ));
    }
    Ok(())
}

/// True when any external check in the summary's `external_validation`
/// object reported `status == "fail"`. `unavailable`/`error`/`pass` are not
/// failures (a missing optional toolchain must never block).
fn external_check_failed(external: &Value) -> bool {
    let Some(obj) = external.as_object() else {
        return false;
    };
    obj.values().any(|check| {
        check
            .get("status")
            .and_then(Value::as_str)
            .map(|s| s == "fail")
            .unwrap_or(false)
    })
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

fn render_dependency_proofs_jsonl(
    dag: &DAG,
    edge_kinds: Option<&BTreeMap<(String, String), EdgeKind>>,
) -> Result<String> {
    let mut out = String::new();
    for (to_node, task) in &dag.tasks {
        let mut deps = task.depends_on.clone();
        deps.sort();
        deps.dedup();
        for from_node in deps {
            // WG4b — lift the real composer-assigned EdgeKind for this
            // node pair when the caller threaded a map from the composed
            // WorkflowDag. The core emit path holds only the depends_on
            // graph (no typed edges), so a missing entry (or no map at all
            // — the legacy/test path) falls back to the strict `Unproven`
            // placeholder, leaving behavior unchanged when no typed-edge
            // data is available.
            let kind = edge_kinds
                .and_then(|m| m.get(&(from_node.to_string(), to_node.to_string())))
                .copied()
                .unwrap_or(EdgeKind::Unproven);
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
                kind,
                chain_of_custody: None,
                mutually_exclusive_group: None,
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
    let mode = match std::env::var("ECAA_VALIDATE_ON_EMIT")
        .unwrap_or_default()
        .as_str()
    {
        "off" | "0" | "false" | "no" => ValidationMode::Disabled,
        "full" => ValidationMode::Full,
        "schema_only" | "" => ValidationMode::SchemaOnly,
        _ => ValidationMode::SchemaOnly,
    };
    // A conformant build runs the FULL external SHACL/OWL suite (and blocks
    // on failure via `validation_blocks_on_fail`); it must never run at a
    // lower tier or be skipped. `ECAA_CONFORMANCE_MODE` therefore forces Full
    // regardless of `ECAA_VALIDATE_ON_EMIT`.
    if read_conformance_mode() {
        ValidationMode::Full
    } else {
        mode
    }
}

/// Read `ECAA_CONFORMANCE_MODE`. When on, validation is forced to block
/// on failure and `Disabled` is upgraded to `SchemaOnly`.
fn read_conformance_mode() -> bool {
    matches!(
        std::env::var("ECAA_CONFORMANCE_MODE")
            .as_deref()
            .unwrap_or("0"),
        "1" | "true" | "yes" | "on"
    )
}

/// Sibling-module accessor for `ECAA_CONFORMANCE_MODE` so `render_readme`
/// (DR-5) can gate the `package.ttl` file-map row on whether that file will
/// actually be produced (it is emitted only by the conformance validator).
pub(super) fn conformance_mode_active() -> bool {
    read_conformance_mode()
}

fn validation_blocks_on_fail() -> bool {
    if read_conformance_mode() {
        return true;
    }
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

/// Resolve `scripts/spec-check/` for the conformance external validators.
/// Order: `ECAA_SPEC_SCRIPTS_DIR` (if it is an existing directory), then
/// `CARGO_MANIFEST_DIR/../../scripts/spec-check/`. Returns `None` when
/// neither resolves — callers then record the checks as `unavailable`.
fn spec_scripts_dir() -> Option<std::path::PathBuf> {
    if let Ok(env_dir) = std::env::var("ECAA_SPEC_SCRIPTS_DIR") {
        let pb = std::path::PathBuf::from(env_dir);
        if pb.is_dir() {
            return Some(pb);
        }
    }
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/spec-check")
        .canonicalize()
        .ok()
        .filter(|p| p.is_dir())
}

/// Run one external Python validator (`script` under `scripts_dir`, passed
/// `args`) and map its exit/stdout/stderr onto the `ExternalCheckOutcome`
/// wire shape (`{status, details|reason}`). Missing python / missing deps
/// map to `unavailable`, never `fail`, so the product build never hard-fails
/// for want of an optional toolchain.
/// Default wall-clock cap for an external validator subprocess. Generous —
/// a healthy SHACL/OWL run is ~1-2s; the cap exists only so a hung pyshacl /
/// HermiT can never wedge `emit_package` indefinitely, not to bound a normal
/// run. Override with `ECAA_VALIDATOR_TIMEOUT_SECS`.
const DEFAULT_VALIDATOR_TIMEOUT_SECS: u64 = 120;

/// Resolve the per-validator subprocess timeout from
/// `ECAA_VALIDATOR_TIMEOUT_SECS` (positive integer seconds); unset/zero/
/// non-numeric falls back to [`DEFAULT_VALIDATOR_TIMEOUT_SECS`].
fn validator_timeout() -> std::time::Duration {
    let secs = std::env::var("ECAA_VALIDATOR_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_VALIDATOR_TIMEOUT_SECS);
    std::time::Duration::from_secs(secs)
}

/// Spawn `python3 <script_path> <args>`, capture stdout+stderr, and kill the
/// child if it has not exited within `timeout`. stdout/stderr are drained on
/// dedicated threads so a chatty validator can never fill the pipe buffer and
/// deadlock the wait. `Instant` is a monotonic timeout clock (NOT an emitted
/// timestamp), so it does not affect package determinism. Returns
/// `Ok((None, ..))` when the child was killed on timeout, `Ok((Some(status), ..))`
/// otherwise.
#[allow(clippy::type_complexity)]
fn run_python_capped(
    script_path: &Path,
    args: &[&str],
    timeout: std::time::Duration,
) -> std::io::Result<(Option<std::process::ExitStatus>, Vec<u8>, Vec<u8>)> {
    use std::io::Read;
    let mut child = std::process::Command::new("python3")
        .arg(script_path)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut out_pipe = child.stdout.take().expect("stdout was piped");
    let mut err_pipe = child.stderr.take().expect("stderr was piped");
    let out_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out_pipe.read_to_end(&mut b);
        b
    });
    let err_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err_pipe.read_to_end(&mut b);
        b
    });
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(st) => break Some(st),
            None => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    Ok((status, stdout, stderr))
}

fn run_python_validator(label: &str, script: &str, args: &[&str], scripts_dir: &Path) -> Value {
    let script_path = scripts_dir.join(script);
    if !script_path.exists() {
        return json!({
            "status": "unavailable",
            "reason": format!("{label} script {} not found at {}", script, scripts_dir.display()),
        });
    }
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        return json!({
            "status": "unavailable",
            "reason": "python3 not on PATH",
        });
    }
    let timeout = validator_timeout();
    let (status, stdout_bytes, stderr_bytes) = match run_python_capped(&script_path, args, timeout) {
        Ok(triple) => triple,
        Err(e) => {
            return json!({
                "status": "error",
                "reason": format!("{label} subprocess spawn failed: {e}"),
            });
        }
    };
    let stdout = String::from_utf8_lossy(&stdout_bytes);
    let stderr = String::from_utf8_lossy(&stderr_bytes);
    let Some(status) = status else {
        // A hung validator must never wedge emit_package. Treat a timeout
        // like a missing toolchain — `unavailable` (non-blocking), so a
        // stuck pyshacl/HermiT degrades gracefully instead of hanging emit.
        return json!({
            "status": "unavailable",
            "reason": format!(
                "{label} timed out after {}s and was killed (raise ECAA_VALIDATOR_TIMEOUT_SECS if intentional)",
                timeout.as_secs()
            ),
        });
    };
    if status.success() {
        return json!({ "status": "pass", "details": stdout.trim() });
    }
    // exit 2 (or a ModuleNotFoundError) is the scripts' "deps missing" signal.
    if status.code() == Some(2)
        || stderr.contains("ModuleNotFoundError")
        || stdout.contains("ModuleNotFoundError")
    {
        return json!({
            "status": "unavailable",
            "reason": format!(
                "{label} deps missing — install via: pip install --user --break-system-packages pyld rdflib pyshacl owlready2"
            ),
        });
    }
    json!({
        "status": "fail",
        "details": format!("exit {}: {} {}", status.code().unwrap_or(-1), stdout.trim(), stderr.trim()),
    })
}

/// Run the external SHACL + OWL validators over the emitted package. Only
/// invoked under `ECAA_CONFORMANCE_MODE`; the product build never reaches
/// here (it records the checks as `unavailable`). `runcrate_validate` is left
/// `unavailable` from the core path — the runcrate round-trip lives in the
/// harness/conformance gate (it shells the heavier `runcrate` toolchain).
fn run_external_validators(output_dir: &Path) -> Value {
    let pkg_arg = output_dir.to_str().unwrap_or(".");
    let (shacl, owl) = match spec_scripts_dir() {
        // The SHACL and OWL validators are independent subprocesses; run them
        // concurrently so emit pays max(shacl, owl) instead of their sum
        // (~halves the conformance-mode validation latency).
        Some(dir) => std::thread::scope(|s| {
            let shacl_handle = s.spawn(|| {
                run_python_validator("shacl_projection", "project_package.py", &[pkg_arg], &dir)
            });
            let owl = run_python_validator("owl_consistency", "owl_consistency.py", &[pkg_arg], &dir);
            let shacl = shacl_handle.join().unwrap_or_else(|_| {
                json!({ "status": "error", "reason": "shacl_projection validator thread panicked" })
            });
            (shacl, owl)
        }),
        None => {
            let reason = json!({
                "status": "unavailable",
                "reason": "scripts/spec-check/ not found (set ECAA_SPEC_SCRIPTS_DIR)",
            });
            (reason.clone(), reason)
        }
    };
    json!({
        "shacl_projection": shacl,
        "owl_consistency": owl,
        "runcrate_validate": unavailable_external_check(),
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

/// Map a sidecar relpath to its ECAA sub-graph letter (`consts::SIDECAR_PATHS`).
/// `None` for the A audit-proof report (validated as a document).
fn sidecar_letter(relpath: &str) -> Option<char> {
    match relpath {
        "runtime/intake-conversation.jsonl" => Some('I'),
        "runtime/decisions.jsonl" => Some('D'),
        "runtime/validation-reports.jsonl" => Some('E'),
        "runtime/proofs.jsonl" => Some('V'),
        "runtime/claim-verification.json" => Some('C'),
        "runtime/verifier-decisions.jsonl" => Some('Q'),
        "runtime/assumptions.jsonl" => Some('F'),
        _ => None,
    }
}

fn empty_loaded_package() -> crate::audit_proof::loader::LoadedPackage {
    crate::audit_proof::loader::LoadedPackage::default()
}

/// Validate emitted sidecars against the hand-authored spec schemas. As of
/// C1 Phase-2 the schemas describe the SPEC node/edge object model, so the
/// raw impl-typed sidecars are first projected via
/// `crate::emitter::ecaa_projection::project_subgraph` and the PROJECTION
/// is validated. A present-but-malformed sidecar still fails via the
/// per-file raw-parse guard, preserving the conformance-gate contract.
fn validate_sidecar_schemas(output_dir: &Path) -> Result<(usize, Vec<Value>, usize)> {
    let mut passed = 0usize;
    let mut failed = Vec::new();
    let mut skipped_pending_harness = 0usize;

    // Lenient load: a malformed sidecar makes the strict loader error on
    // the whole package; fall back to empty so the per-file raw-parse guard
    // reports the precise malformed sidecar instead of an opaque error.
    let pkg = crate::audit_proof::loader::LoadedPackage::from_root(output_dir)
        .unwrap_or_else(|_| empty_loaded_package());

    for (relpath, is_jsonl, schema_src, source) in sidecar_schemas() {
        let path = output_dir.join(relpath);
        // A present file is always validated regardless of source: a
        // harness-runtime sidecar (subgraph E / Q) already written into the
        // package must be checked against its schema, not skipped. Only an
        // absent harness-runtime sidecar is skipped (it is produced post-emit).
        if !path.exists() {
            if matches!(source, SidecarSource::HarnessRuntime) {
                skipped_pending_harness += 1;
                continue;
            }
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

        // Raw-parse guard (catches malformed JSON / JSONL lines).
        if is_jsonl {
            let mut parse_error = None;
            for (idx, line) in raw.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Err(e) = serde_json::from_str::<Value>(trimmed) {
                    parse_error = Some((idx, e.to_string()));
                    break;
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
        } else if !raw.trim().is_empty() {
            if let Err(e) = serde_json::from_str::<Value>(raw.trim()) {
                failed.push(json!({
                    "sidecar": relpath,
                    "line_index": null,
                    "error": format!("JSON parse error: {e}"),
                }));
                continue;
            }
        }

        // Build the instance to validate: spec projection for the 7
        // node/edge sub-graphs; the report document for A.
        let instance = match sidecar_letter(relpath) {
            Some(letter) => Value::Array(crate::emitter::ecaa_projection::project_subgraph(
                letter, &pkg,
            )),
            None => {
                if raw.trim().is_empty() {
                    Value::Null
                } else {
                    serde_json::from_str::<Value>(raw.trim())
                        .with_context(|| format!("reparsing {}", relpath))?
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

#[cfg(test)]
mod tests {
    use super::*;

    fn python3_available() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn run_python_capped_kills_a_hung_validator() {
        if !python3_available() {
            eprintln!("SKIP run_python_capped_kills_a_hung_validator: python3 not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("hang.py");
        std::fs::write(&script, "import time\ntime.sleep(60)\n").unwrap();
        let start = std::time::Instant::now();
        let (status, _out, _err) =
            run_python_capped(&script, &[], std::time::Duration::from_millis(300)).unwrap();
        let elapsed = start.elapsed();
        assert!(status.is_none(), "a hung validator must be killed (got {status:?})");
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "kill should be prompt, took {elapsed:?}"
        );
    }

    #[test]
    fn run_python_capped_captures_output_of_a_fast_script() {
        if !python3_available() {
            eprintln!("SKIP run_python_capped_captures_output_of_a_fast_script: python3 not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("ok.py");
        std::fs::write(&script, "print('hello-capped')\n").unwrap();
        let (status, out, _err) =
            run_python_capped(&script, &[], std::time::Duration::from_secs(30)).unwrap();
        assert!(status.expect("should exit").success());
        assert!(String::from_utf8_lossy(&out).contains("hello-capped"));
    }
}
