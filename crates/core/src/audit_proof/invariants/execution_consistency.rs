//! Inv 6 sub-check (substrate-validity): the WRROC `@graph` HowToStep set and
//! the E sidecar WorkflowStep set must denote the same execution steps.
//! Referential agreement only (F11) — NOT step-ordering or numeric
//! equivalence. Execution is materialized twice (the authoritative WRROC
//! `@graph` `HowToStep` lineage in `ro-crate-metadata.json`, and the E sidecar
//! `proofs.jsonl` `WorkflowStep` set); this sub-check asserts they agree and
//! folds the result into Invariant 6 without adding a 7th invariant.
use serde_json::Value;
use std::collections::BTreeSet;

/// Reduce an execution-step id to its bare token so the two materializations
/// compare on the same key. The WRROC `@graph` HowToStep id is `#step-<id>`
/// (the `ro_crate.rs` emit form); the E sidecar (`proofs.jsonl`) carries
/// `workflow:<id>`. Both reduce to the bare `<id>` token
/// (`#step-de` ↔ `workflow:de` ↔ `de`). `#step/` is also accepted for
/// forward-compat with a slash-delimited @graph id form.
fn bare(id: &str) -> String {
    id.trim_start_matches("#step-")
        .trim_start_matches("#step/")
        .trim_start_matches("workflow:")
        .to_string()
}

/// Returns `(n_inspected, violations)`. A violation is a step present in one
/// materialization but absent from the other (the symmetric difference of the
/// `@graph` HowToStep set and the E WorkflowStep set). `n_inspected` is the
/// size of the union (the total distinct execution steps across both views).
pub fn check_execution_consistency(
    graph_nodes: &[Value],
    e_steps: &[Value],
) -> (usize, Vec<String>) {
    let g: BTreeSet<String> = graph_nodes
        .iter()
        .filter(|n| graph_type_is_howtostep(n))
        .filter_map(|n| n.get("@id").and_then(Value::as_str))
        .map(bare)
        .collect();
    let e: BTreeSet<String> = e_steps
        .iter()
        .filter_map(|n| n.get("id").and_then(Value::as_str))
        .map(bare)
        .collect();
    let mut viol = Vec::new();
    for s in g.symmetric_difference(&e) {
        viol.push(format!("execution-step drift: {s}"));
    }
    let n_inspected = g.union(&e).count();
    (n_inspected, viol)
}

/// A WRROC `@graph` node is a HowToStep when its `@type` is `HowToStep`
/// (string) or contains `HowToStep` (array form — RO-Crate entities may carry
/// multiple types).
fn graph_type_is_howtostep(n: &Value) -> bool {
    match n.get("@type") {
        Some(Value::String(s)) => s == "HowToStep",
        Some(Value::Array(a)) => a.iter().any(|t| t.as_str() == Some("HowToStep")),
        _ => false,
    }
}
