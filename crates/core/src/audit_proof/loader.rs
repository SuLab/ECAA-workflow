//! Reads the 8 ECAA subgraph sidecars from a package root.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
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
    /// Verifier decisions.
    pub verifier_decisions: Vec<Value>, // verifier-decisions.jsonl
    /// Assumptions.
    pub assumptions: Vec<Value>, // assumptions.jsonl
    /// Determinism shim.
    pub determinism_shim: Option<Value>, // determinism-shim.json
    /// Security policy.
    pub security_policy: Option<Value>, // security-policy.json
    /// Plot affordances.
    pub plot_affordances: Option<Vec<Value>>, // plot_affordances.jsonl (optional)
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
            assumptions: load_jsonl_opt(&rt.join("assumptions.jsonl"))?.unwrap_or_default(),
            determinism_shim: load_json_opt(&rt.join("determinism-shim.json"))?,
            security_policy: load_json_opt(&rt.join("security-policy.json"))?,
            plot_affordances: load_jsonl_opt(&rt.join("plot_affordances.jsonl"))?,
        })
    }
}

/// Returns `(claims, claims_tampered)`. Signed sink wins when present and a
/// verifier is supplied; otherwise the top-level stub.
///
/// The signed sink is append-only JSONL: one HMAC-signed row per task
/// verification. EVERY row is verified — any HMAC failure ⇒ `claims_tampered`
/// (Inv 1 Fail). A single row is returned as-is (byte-identical to the
/// pre-accumulator behavior); multiple rows are unioned so an earlier recall
/// gap survives a later coverage-less task (the F2 at-rest erasure fix).
fn load_claims(
    rt: &Path,
    verifier: Option<&crate::audit_writer::AuditWriter>,
) -> Result<(Option<Value>, bool)> {
    let signed = rt.join("verification-reports/claim-verification.signed.json");
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
/// Verdicts are concatenated in file order — `claim_id` embeds the task id
/// (`<task_id>#claim-<i>`), so rows do not collide across tasks. Coverage is
/// unioned per-entity by BEST outcome (`addressed` > `unverifiable` >
/// `absent`): a later task addressing an entity RESOLVES an earlier gap,
/// while a coverage-LESS row contributes nothing and therefore can never
/// erase a recorded gap. The `coverage` key is omitted entirely when no row
/// carried one, preserving the verdict-only predicate for un-anchored runs.
fn union_signed_rows(rows: &[Value]) -> Value {
    let mut verdicts: Vec<Value> = Vec::new();
    for r in rows {
        if let Some(arr) = r.get("verdicts").and_then(Value::as_array) {
            verdicts.extend(arr.iter().cloned());
        }
    }

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
