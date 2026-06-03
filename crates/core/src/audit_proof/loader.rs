//! Reads the 8 ECAA subgraph sidecars from a package root.

use anyhow::{Context, Result};
use serde_json::Value;
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
        })
    }
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
fn load_claims(
    rt: &Path,
    verifier: Option<&crate::audit_writer::AuditWriter>,
) -> Result<(Option<Value>, bool)> {
    let signed = rt.join("verification-reports/claim-verification.signed.json");
    if let Some(v) = verifier {
        if signed.exists() {
            let raw = fs::read_to_string(&signed)
                .with_context(|| format!("read {}", signed.display()))?;
            let line = raw.trim_end();
            let parsed: Value = serde_json::from_str(line)
                .with_context(|| format!("parse {}", signed.display()))?;
            return match v.verify_row(&parsed) {
                Ok(inner) => Ok((Some(inner), false)),
                Err(_) => Ok((None, true)), // tampered → Inv 1 Fail
            };
        }
    }
    Ok((load_json_opt(&rt.join("claim-verification.json"))?, false))
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
