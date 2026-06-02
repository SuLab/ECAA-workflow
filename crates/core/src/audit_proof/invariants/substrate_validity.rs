//! Invariant 6: substrate-validity.
//! Delegates to the WRROC v0.5 Tier-3 validator already in core, then folds in
//! the execution-consistency sub-check (F11): the WRROC `@graph` HowToStep set
//! and the E sidecar (`proofs.jsonl`) WorkflowStep set must denote the same
//! execution steps. This is a SUB-CHECK of Invariant 6 — it adds no 7th
//! invariant and no ablation flag.

use crate::audit_proof::invariants::execution_consistency::check_execution_consistency;
use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};
use crate::wrroc_validator::{WrrocOutcome, WrrocValidator};
use serde_json::Value;
use std::path::Path;

/// Check substrate validity.
///
/// Delegates to the injected validator's three-valued
/// [`WrrocValidator::validate_outcome`]. The mapping is:
/// - `Pass`        → `InvariantStatus::Pass`
/// - `Fail(msgs)`  → `InvariantStatus::Fail` (one violation per package run)
/// - `Unverified`  → `InvariantStatus::Unverified` — including the case
///   where the injected validator is the no-op adapter (runcrate not run).
///   A non-run must NOT be recorded as a substrate-validity pass.
pub fn check_substrate_validity(root: &Path, validator: &dyn WrrocValidator) -> InvariantVerdict {
    let descriptor = root.join("ro-crate-metadata.json");
    if !descriptor.exists() {
        return InvariantVerdict {
            id: InvariantId::SubstrateValidity,
            status: InvariantStatus::Unverified,
            detail: Some("ro-crate-metadata.json absent".into()),
            n_inspected: 0,
            n_violations: 0,
        };
    }
    let base = match validator.validate_outcome(&[root]) {
        WrrocOutcome::Pass => InvariantVerdict {
            id: InvariantId::SubstrateValidity,
            status: InvariantStatus::Pass,
            detail: None,
            n_inspected: 1,
            n_violations: 0,
        },
        WrrocOutcome::Fail(msgs) => InvariantVerdict {
            id: InvariantId::SubstrateValidity,
            status: InvariantStatus::Fail,
            detail: Some(msgs.join("; ")),
            n_inspected: 1,
            n_violations: msgs.len(),
        },
        WrrocOutcome::Unverified(reason) => InvariantVerdict {
            id: InvariantId::SubstrateValidity,
            // The descriptor IS present (we got past the early return), but
            // no real validation ran — count it as inspected-but-unverified.
            status: InvariantStatus::Unverified,
            detail: Some(reason),
            n_inspected: 1,
            n_violations: 0,
        },
    };
    fold_execution_consistency(root, base)
}

/// Read the WRROC `@graph` (`ro-crate-metadata.json`) and the E sidecar
/// (`proofs.jsonl`), run the [`check_execution_consistency`] sub-check, and
/// MERGE any drift into the base Invariant-6 verdict. The WRROC outcome still
/// drives the base verdict; drift only ADDS violations and, when the base was
/// a `Pass`, downgrades it to `Warn` (a soft signal — the SHACL
/// `ExecutionConsistencyShape` second-impl is the hard gate). An absent
/// `proofs.jsonl` (un-executed package) yields an empty E set and so no drift,
/// preserving the freshly-emitted-package contract.
fn fold_execution_consistency(root: &Path, base: InvariantVerdict) -> InvariantVerdict {
    let graph_nodes = read_graph_nodes(root);
    // Execution-consistency only applies when the WRROC @graph materializes
    // execution lineage (≥1 HowToStep). A descriptor with no HowToSteps has
    // nothing to reconcile against the E sidecar — mirrors the `_project.py`
    // gate so the Rust verdict and the SHACL ExecutionConsistencyShape agree.
    let has_howtosteps = graph_nodes.iter().any(|n| match n.get("@type") {
        Some(Value::String(s)) => s == "HowToStep",
        Some(Value::Array(a)) => a.iter().any(|t| t.as_str() == Some("HowToStep")),
        _ => false,
    });
    if !has_howtosteps {
        return base;
    }
    let e_steps = read_e_steps(root);
    let (n_steps, drift) = check_execution_consistency(&graph_nodes, &e_steps);
    if drift.is_empty() {
        return base;
    }
    let mut merged = base;
    merged.n_inspected += n_steps;
    merged.n_violations += drift.len();
    if merged.status == InvariantStatus::Pass {
        merged.status = InvariantStatus::Warn;
    }
    let drift_detail = drift.join("; ");
    merged.detail = Some(match merged.detail.take() {
        Some(d) if !d.is_empty() => format!("{d}; {drift_detail}"),
        _ => drift_detail,
    });
    merged
}

/// Read the WRROC `@graph` array from `ro-crate-metadata.json`. Returns an
/// empty vec on any read/parse failure (the descriptor-present early return
/// already guards the verdict; a malformed `@graph` simply yields no
/// HowToStep focus nodes).
fn read_graph_nodes(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(root.join("ro-crate-metadata.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("@graph").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

/// Build the E (Evidence) execution-step set from `proofs.jsonl`. Each row
/// names a producing step (`id` = `workflow:<to>`) and its source
/// (`computed_from` = `workflow:<from>`); BOTH endpoints are emitted as
/// step entries so a root step (which appears only as a `computed_from`
/// source, never as an `id`) is still represented and does not register as
/// spurious drift against the `@graph`. Only `workflow:`-prefixed values are
/// step refs — a `computed_from` that is a file path (the evidence-coverage
/// form) is an output, not a step, and is excluded. Returns one `{"id": ...}`
/// value per distinct endpoint.
fn read_e_steps(root: &Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(root.join("runtime/proofs.jsonl")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        for key in ["id", "computed_from"] {
            if let Some(s) = row.get(key).and_then(Value::as_str) {
                if s.starts_with("workflow:") {
                    out.push(serde_json::json!({"id": s}));
                }
            }
        }
    }
    out
}
