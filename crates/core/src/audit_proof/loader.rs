//! Reads the 8 ECAA subgraph sidecars from a package root.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Default)]
/// LoadedPackage data.
pub struct LoadedPackage {
    /// Intake.
    pub intake: Vec<Value>, // intake-conversation.jsonl
    /// Decisions.
    pub decisions: Vec<Value>, // decisions.jsonl
    /// Validation reports.
    pub validation_reports: Vec<Value>, // validation-reports.jsonl
    /// Proofs.
    pub proofs: Vec<Value>, // proofs.jsonl
    /// Claims.
    pub claims: Option<Value>, // claim-verification.json
    /// Verifier decisions. The compile-time port-unification trace
    /// (`event:"prove"` rows) ONLY — the five-class re-execution `RerunOutcome`
    /// rows live in [`Self::reexecution`], not here.
    pub verifier_decisions: Vec<Value>, // verifier-decisions.jsonl
    /// The re-execution report (`runtime/reexecution.json`): the real
    /// five-class `RerunOutcome` Q sub-graph
    /// (`{schema_version, bucket_counts, per_artifact:[{artifact_path, bucket}]}`).
    /// `None` when the file is absent (no parent to replay against);
    /// present-but-empty `per_artifact` means re-execution was not performed.
    /// Inv 4 (`equivalence_failure`) ranges over this, not `verifier_decisions`.
    pub reexecution: Option<Value>, // reexecution.json
    /// Assumptions.
    pub assumptions: Vec<Value>, // assumptions.jsonl
    /// Determinism shim.
    pub determinism_shim: Option<Value>, // determinism-shim.json
    /// Security policy.
    pub security_policy: Option<Value>, // security-policy.json
    /// Plot affordances.
    pub plot_affordances: Option<Vec<Value>>, // plot_affordances.jsonl (optional)
    /// The RO-Crate `@graph` analytical-output entities — the
    /// `ImageObject`/`schema:Image` figure entities (declared figure
    /// obligations at emit; produced figures post-execution) plus any
    /// `Dataset`/`File` entity rooted under `runtime/outputs/`. This is the
    /// real-output source the Evidence (V) sub-graph projection and Invariant 3
    /// (`evidence_coverage`) both range over (see
    /// [`crate::audit_proof::output_source`]). Empty when
    /// `ro-crate-metadata.json` is absent.
    pub output_entities: Vec<Value>, // from ro-crate-metadata.json::@graph
    /// Result artifacts explicitly declared as narrative evidence by a
    /// task-level `result_schema` or by the report assembler's
    /// `report_schemas`. Values are the artifact identifiers exactly as
    /// recorded in `WORKFLOW.json` (usually basenames). Invariant 3 uses this
    /// declaration, rather than every retained scientific file, as its
    /// prospective evidence denominator.
    pub declared_claim_evidence: BTreeSet<String>, // from WORKFLOW.json
    /// True iff a signed verdict sink was present but failed HMAC
    /// verification (tampered or written by an unauthorized writer).
    /// Inv 1 maps this to `Fail`.
    pub claims_tampered: bool,
}

impl LoadedPackage {
    /// From root, with no signed-sink verifier (the signed sink, if any,
    /// is ignored and the top-level stub is used). Back-compat entry point.
    pub fn from_root(root: &Path) -> Result<Self> {
        Self::from_root_with_verifier(root, None)
    }

    /// From root. When `verifier` is `Some`, the signed verdict sink at
    /// `runtime/verification-reports/claim-verification.signed.json` takes
    /// priority over the agent-writable stub: a valid signature binds
    /// `claims` to the verified payload; a signature failure sets
    /// `claims_tampered`; absence falls back to the stub.
    pub fn from_root_with_verifier(
        root: &Path,
        verifier: Option<&crate::audit_writer::AuditWriter>,
    ) -> Result<Self> {
        let rt = root.join("runtime");
        let (claims, claims_tampered) = load_claims(&rt, verifier)?;
        let output_entities = load_output_entities(root)?;
        let declared_claim_evidence = load_declared_claim_evidence(root)?;
        Ok(Self {
            intake: load_jsonl_opt(&rt.join("intake-conversation.jsonl"))?.unwrap_or_default(),
            decisions: load_jsonl_opt(&rt.join("decisions.jsonl"))?.unwrap_or_default(),
            validation_reports: load_jsonl_opt(&rt.join("validation-reports.jsonl"))?
                .unwrap_or_default(),
            proofs: load_jsonl_opt(&rt.join("proofs.jsonl"))?.unwrap_or_default(),
            claims,
            claims_tampered,
            verifier_decisions: load_jsonl_opt(&rt.join("verifier-decisions.jsonl"))?
                .unwrap_or_default(),
            reexecution: load_json_opt(&rt.join("reexecution.json"))?,
            assumptions: load_jsonl_opt(&rt.join("assumptions.jsonl"))?.unwrap_or_default(),
            determinism_shim: load_json_opt(&rt.join("determinism-shim.json"))?,
            security_policy: load_json_opt(&rt.join("security-policy.json"))?,
            plot_affordances: load_jsonl_opt(&rt.join("plot_affordances.jsonl"))?,
            output_entities,
            declared_claim_evidence,
        })
    }
}

/// Read artifacts explicitly selected for report verification.
///
/// A package may retain normalized matrices, plotting data, summaries,
/// validation reports, copied inputs, and alternate table views. Their
/// presence does not mean that every file was intended to support a narrative
/// claim. The workflow contract makes that intent explicit in two places:
/// stage-local `result_schema.artifact` and the assembler's
/// `report_schemas.*.artifact`. Both are collected here without guessing from
/// file extensions.
fn load_declared_claim_evidence(root: &Path) -> Result<BTreeSet<String>> {
    let Some(workflow) = load_json_opt(&root.join("WORKFLOW.json"))? else {
        return Ok(BTreeSet::new());
    };
    let mut artifacts = BTreeSet::new();
    let Some(tasks) = workflow.get("tasks").and_then(Value::as_object) else {
        return Ok(artifacts);
    };
    for task in tasks.values() {
        let Some(spec) = task.get("spec").and_then(Value::as_object) else {
            continue;
        };
        if let Some(artifact) = spec
            .get("result_schema")
            .and_then(|schema| schema.get("artifact"))
            .and_then(Value::as_str)
            .filter(|artifact| !artifact.trim().is_empty())
        {
            artifacts.insert(artifact.to_string());
        }
        if let Some(schemas) = spec.get("report_schemas").and_then(Value::as_object) {
            for schema in schemas.values() {
                if let Some(artifact) = schema
                    .get("artifact")
                    .and_then(Value::as_str)
                    .filter(|artifact| !artifact.trim().is_empty())
                {
                    artifacts.insert(artifact.to_string());
                }
            }
        }
    }
    Ok(artifacts)
}

/// Read the analytical-output entities from `ro-crate-metadata.json::@graph`.
/// Filtering to the actual output set (figures + `runtime/outputs/` artifacts)
/// is done by [`crate::audit_proof::output_source::analytical_outputs`]; this
/// loader returns every `@graph` entity that carries an `@id` so that single
/// filter stays the one source of truth. Returns an empty vec when the
/// descriptor is absent (so a pre-RO-Crate sidecar-only package still loads).
fn load_output_entities(root: &Path) -> Result<Vec<Value>> {
    let descriptor = root.join("ro-crate-metadata.json");
    let Some(meta) = load_json_opt(&descriptor)? else {
        return Ok(Vec::new());
    };
    let entities = meta
        .get("@graph")
        .and_then(Value::as_array)
        .map(|g| {
            g.iter()
                .filter(|e| e.get("@id").and_then(Value::as_str).is_some())
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    Ok(entities)
}

/// Returns `(claims, claims_tampered)`. Signed sink wins when present and a
/// verifier is supplied; otherwise the top-level stub.
///
/// The signed sink is append-only JSONL: one HMAC-signed row per task
/// verification. EVERY row is verified — any HMAC failure ⇒ `claims_tampered`
/// (Inv 1 Fail). A single row is returned as-is (byte-identical to the
/// pre-accumulator behavior); multiple rows are unioned (and verdicts
/// deduplicated by `claim_id`) so an earlier recall gap survives a later
/// coverage-less task (the F2 at-rest erasure fix) and a re-finalized task's
/// duplicate rows collapse instead of double-counting `n_inspected`.
fn load_claims(
    rt: &Path,
    verifier: Option<&crate::audit_writer::AuditWriter>,
) -> Result<(Option<Value>, bool)> {
    // Runtime-relative sink path, single-sourced from the writer's const so the
    // reader and writer paths cannot drift (NDJSON — parsed line-by-line below).
    let signed = rt.join(crate::claim_sink::SIGNED_SINK_UNDER_RUNTIME);
    if let Some(v) = verifier {
        if signed.exists() {
            let raw = fs::read_to_string(&signed)
                .with_context(|| format!("read {}", signed.display()))?;
            let mut inners: Vec<Value> = Vec::new();
            for line in raw.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let parsed: Value = serde_json::from_str(line)
                    .with_context(|| format!("parse {}", signed.display()))?;
                match v.verify_row(&parsed) {
                    Ok(inner) => inners.push(inner),
                    Err(_) => return Ok((None, true)), // any tampered row → Inv 1 Fail
                }
            }
            return match inners.len() {
                // Empty signed file: no non-vacuity can be claimed from zero
                // rows, so fall back to the (agent-writable) stub.
                0 => Ok((load_json_opt(&rt.join("claim-verification.json"))?, false)),
                // Single row: return as-is — the common single-task case is
                // byte-identical to the pre-accumulator loader.
                1 => Ok((Some(inners.into_iter().next().unwrap()), false)),
                // Multiple rows: cross-task union.
                _ => Ok((Some(union_signed_rows(&inners)), false)),
            };
        }
    }
    Ok((load_json_opt(&rt.join("claim-verification.json"))?, false))
}

/// Union the per-task signed rows into the single `claims` value the
/// invariants read (Inv 1 claim-completeness, Inv 5 cross-graph-integrity,
/// evidence-coverage).
///
/// Verdicts are unioned and deduplicated by `claim_id`, keeping the LAST
/// occurrence. The `claim_id` embeds the task id and a positional index
/// (`<task_id>#claim-<i>`), so distinct tasks (and distinct claims within a
/// task) yield distinct ids and are ALL preserved — the F2 cross-task union
/// is intact. The only collisions are exact-`claim_id` duplicates from
/// re-finalizing the SAME task (the per-task coverage gate plus the
/// end-of-run finalize append two rows in a standalone clean pass); collapsing
/// those keeps `n_inspected` equal to the true distinct-claim count instead of
/// double-counting. Keeping the LAST row preserves the most recent finalize
/// and a valid HMAC (each row is individually signed). Ordering is stable:
/// claim_ids appear in first-seen order, each carrying its last-seen content.
/// Coverage is unioned per-entity by BEST outcome (`addressed` >
/// `unverifiable` > `absent`): a later task addressing an entity RESOLVES an
/// earlier gap, while a coverage-LESS row contributes nothing and therefore
/// can never erase a recorded gap. The `coverage` key is omitted entirely when
/// no row carried one, preserving the verdict-only predicate for un-anchored
/// runs.
fn union_signed_rows(rows: &[Value]) -> Value {
    // First-seen order of claim_ids, each mapped to its LAST-seen verdict row.
    // A verdict with no `claim_id` (defensive — projected rows always carry
    // one) is kept positionally and never collapsed.
    let mut order: Vec<String> = Vec::new();
    let mut by_id: BTreeMap<String, Value> = BTreeMap::new();
    let mut unkeyed: Vec<Value> = Vec::new();
    for r in rows {
        if let Some(arr) = r.get("verdicts").and_then(Value::as_array) {
            for v in arr {
                match v.get("claim_id").and_then(Value::as_str) {
                    Some(id) => {
                        let id = id.to_string();
                        if by_id.insert(id.clone(), v.clone()).is_none() {
                            order.push(id);
                        }
                    }
                    None => unkeyed.push(v.clone()),
                }
            }
        }
    }
    let mut verdicts: Vec<Value> = order
        .into_iter()
        .map(|id| by_id.remove(&id).expect("claim_id recorded in order map"))
        .collect();
    verdicts.extend(unkeyed);

    // entity -> best-outcome rank (2=addressed, 1=unverifiable, 0=absent).
    let mut best: BTreeMap<String, u8> = BTreeMap::new();
    let mut any_coverage = false;
    for r in rows {
        let Some(per_entity) = r
            .get("coverage")
            .and_then(|c| c.get("per_entity"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        any_coverage = true;
        for (entity, outcome) in per_entity {
            let rank = match outcome.as_str().unwrap_or("absent") {
                "addressed" => 2u8,
                "unverifiable" => 1,
                _ => 0, // "absent" or unknown ⇒ worst (never erases a gap)
            };
            let slot = best.entry(entity.clone()).or_insert(0);
            if rank > *slot {
                *slot = rank;
            }
        }
    }

    let mut doc = json!({
        "schema_version": "1",
        "source": "runtime-verifier",
        "verdicts": verdicts,
    });
    if any_coverage {
        let (mut addressed, mut unverifiable, mut absent) = (0usize, 0usize, 0usize);
        let mut per_entity = serde_json::Map::new();
        for (entity, rank) in &best {
            let label = match rank {
                2 => {
                    addressed += 1;
                    "addressed"
                }
                1 => {
                    unverifiable += 1;
                    "unverifiable"
                }
                _ => {
                    absent += 1;
                    "absent"
                }
            };
            per_entity.insert(entity.clone(), Value::String(label.to_string()));
        }
        doc["coverage"] = json!({
            "required_total": best.len(),
            "required_addressed": addressed,
            "required_unverifiable": unverifiable,
            "required_absent": absent,
            "per_entity": Value::Object(per_entity),
        });
    }
    doc
}

fn load_json_opt(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let v: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(v))
}

fn load_jsonl_opt(path: &Path) -> Result<Option<Vec<Value>>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut out = Vec::new();
    for (lineno, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .with_context(|| format!("parse {}:{}", path.display(), lineno + 1))?;
        out.push(v);
    }
    Ok(Some(out))
}
