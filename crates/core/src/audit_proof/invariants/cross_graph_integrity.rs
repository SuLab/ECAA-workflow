//! Invariant 5: cross-graph-integrity.
//!
//! Spec (`docs/ecaa-spec/invariants.md` §5): *every cross-sub-graph reference
//! dereferences to an existing node.* A reference is "cross-graph" when its
//! target is prefix-tagged with a sub-graph letter (`I:`/`D:`/`E:`/`V:`/`C:`/
//! `Q:`/`F:`/`A:`, §4.2). The predicate is one-directional — a reference MUST
//! resolve to a node in the named sub-graph; nodes need not be referenced.
//!
//! This is the sidecar-level realization of that projected-graph predicate.
//! The audit framework reads the `runtime/` sidecars (not the projected
//! RO-Crate `@graph`), so we build a per-sub-graph node-id registry deriving
//! local ids the same way the projector (`emitter::ecaa_projection`) does, and
//! resolve every cross-graph reference against the set named by its letter
//! prefix. The Evidence (V) set is derived from the SAME shared source the V
//! projection and Invariant 3 (`evidence_coverage`) use —
//! [`crate::audit_proof::output_source::analytical_outputs`] (the RO-Crate
//! `@graph` output entities + real-path proofs rows) — so a C→V `supported_by`
//! reference resolves consistently with Inv 3. The two invariants therefore
//! never disagree about the same C→V evidence link on a production/executed
//! package, where the produced output is an `@graph` entity and `proofs.jsonl`
//! is a bare `EdgeContract` with no `computed_from`.
//!
//! Two reference encodings are checked:
//!   * the concrete forms emitted today — a Claim verdict's `supported_by`
//!     output reference (C→V) resolved against the Evidence outputs, and a
//!     Failure assumption's `edge_id` (F→ proof edge) resolved against the
//!     proof-edge set; and
//!   * the general spec form — any `<letter>:<id>` prefix-tagged value in a
//!     reference-bearing field of any sub-graph row, resolved against the
//!     named sub-graph's node-id set.

use crate::audit_proof::loader::LoadedPackage;
use crate::audit_proof::output_source::{analytical_outputs, same_task_basename_match};
use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Reference-bearing fields scanned for `<letter>:<id>` cross-graph targets.
/// Restricted to fields that carry references by contract — free-text fields
/// (`statement`, `rationale`, `narrative_text`, …) are deliberately excluded
/// so a prose value that happens to begin `E: …` is never read as an edge.
/// `supported_by` and `edge_id` are handled by the concrete passes below in
/// their un-prefixed encoding; their prefix-tagged form is caught here.
const REFERENCE_FIELDS: &[&str] = &[
    "supported_by",
    "target",
    "target_id",
    "ref",
    "refs",
    "references",
    "derived_from",
    "prov:wasDerivedFrom",
    "wasDerivedFrom",
    "evaluates",
    "evaluated_against",
    "affects_nodes",
];

/// True when `id` carries a `^(I|D|E|V|C|Q|F|A):` prefix tag with a non-empty
/// local part — mirrors `ecaa_projection::is_prefix_tagged`.
fn is_prefix_tagged(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(
        chars.next(),
        Some('I' | 'D' | 'E' | 'V' | 'C' | 'Q' | 'F' | 'A')
    ) && chars.next() == Some(':')
        && id.len() > 2
}

/// Reduce a string to the §4.2 id grapheme set (`[A-Za-z0-9_\-]`), mirroring
/// `ecaa_projection::sanitize_id` so resolution matches the projector's ids.
fn sanitize_id(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "x".to_string()
    } else {
        cleaned
    }
}

fn str_field<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

/// True when an RO-Crate `@graph` entity is a `Claim` node (its `@type` is, or
/// includes, `"Claim"`). The embedded Claim nodes carry the `supported_by`
/// references whose referential integrity is validated above.
fn entity_is_claim(entity: &Value) -> bool {
    match entity.get("@type") {
        Some(Value::String(s)) => s == "Claim",
        Some(Value::Array(a)) => a.iter().any(|t| t.as_str() == Some("Claim")),
        _ => false,
    }
}

/// Build the per-sub-graph local-node-id registry from the sidecars. Ids are
/// sanitized so they compare equal to a sanitized reference local part.
fn collect_node_ids(pkg: &LoadedPackage) -> BTreeMap<char, BTreeSet<String>> {
    let mut nodes: BTreeMap<char, BTreeSet<String>> = BTreeMap::new();
    let mut add = |letter: char, id: &str| {
        nodes.entry(letter).or_default().insert(sanitize_id(id));
    };

    // I — Intent nodes.
    for r in &pkg.intake {
        if let Some(id) = str_field(r, "id") {
            add('I', id);
        }
    }
    // D — Decision nodes.
    for r in &pkg.decisions {
        if let Some(id) = str_field(r, "id") {
            add('D', id);
        }
    }
    // E — WorkflowStep nodes: validation-report task/step ids plus the
    // producer/consumer step ids the proof edges connect.
    for r in &pkg.validation_reports {
        for k in ["task_id", "step_id", "id"] {
            if let Some(id) = str_field(r, k) {
                add('E', id);
            }
        }
    }
    for r in &pkg.proofs {
        for k in ["from_node", "to_node", "id"] {
            if let Some(id) = str_field(r, k) {
                add('E', id);
            }
        }
    }
    // V — Evidence nodes. Derived from the SAME shared source the V projection
    // and Inv 3 (`evidence_coverage`) use — `output_source::analytical_outputs`
    // (the RO-Crate `@graph` output entities + real-path proofs rows, rejecting
    // `workflow:*` dependency-node edges) — so a `V:<id>` reference resolves
    // against the same ids `project_evidence_subgraph` assigns: the sanitized
    // output basename, with the same deterministic collision-disambiguation
    // suffix (`<base>_<n>`). Keying on `proofs.jsonl::computed_from` alone left
    // this empty on production/executed packages, where the produced evidence is
    // carried as an `@graph` entity and proofs are bare `EdgeContract`s.
    {
        let mut seen: BTreeMap<String, u32> = BTreeMap::new();
        for (idx, output) in analytical_outputs(&pkg.output_entities, &pkg.proofs)
            .into_iter()
            .enumerate()
        {
            let base = sanitize_id(output.path.rsplit('/').next().unwrap_or(&output.path));
            let base = if base.is_empty() {
                format!("evidence_{idx:03}")
            } else {
                base
            };
            let count = seen.entry(base.clone()).or_insert(0);
            let id = if *count == 0 {
                base.clone()
            } else {
                format!("{base}_{count}")
            };
            *count += 1;
            add('V', &id);
        }
    }
    // C — Claim nodes (one per verdict).
    if let Some(verdicts) = pkg
        .claims
        .as_ref()
        .and_then(|c| c.get("verdicts"))
        .and_then(Value::as_array)
    {
        for (idx, v) in verdicts.iter().enumerate() {
            let id = v
                .get("claim_id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("claim_{idx:03}"));
            add('C', &id);
        }
    }
    // Q — Equivalence (RerunOutcome) nodes.
    for r in &pkg.verifier_decisions {
        if let Some(id) = str_field(r, "id") {
            add('Q', id);
        }
    }
    // F — Failure (Blocker/Assumption) nodes.
    for r in &pkg.assumptions {
        if let Some(id) = str_field(r, "id") {
            add('F', id);
        }
    }
    // A (Audit-proof) node ids live in `audit-proof-report.json`, which is not
    // part of `LoadedPackage`; an `A:` reference therefore cannot resolve here.
    // No emitted sub-graph references the A nodes, so this is not exercised.
    nodes
}

/// Resolve a prefix-tagged `<letter>:<local>` reference against the registry.
fn resolves(target: &str, nodes: &BTreeMap<char, BTreeSet<String>>) -> bool {
    let letter = target.chars().next().expect("prefix-tagged");
    let local = sanitize_id(&target[2..]);
    nodes.get(&letter).is_some_and(|set| set.contains(&local))
}

/// Visit every prefix-tagged value in `row`'s reference-bearing fields (and,
/// for the claim-verification object, its nested `verdicts[]` rows).
fn for_each_prefixed_ref(row: &Value, visit: &mut dyn FnMut(&str)) {
    let Some(obj) = row.as_object() else {
        return;
    };
    let scan = |v: &Value, visit: &mut dyn FnMut(&str)| match v {
        Value::String(s) if is_prefix_tagged(s) => visit(s),
        Value::Array(arr) => {
            for x in arr {
                if let Some(s) = x.as_str() {
                    if is_prefix_tagged(s) {
                        visit(s);
                    }
                }
            }
        }
        _ => {}
    };
    for &field in REFERENCE_FIELDS {
        if let Some(v) = obj.get(field) {
            scan(v, visit);
        }
    }
    if let Some(verdicts) = obj.get("verdicts").and_then(Value::as_array) {
        for vd in verdicts {
            if let Some(vo) = vd.as_object() {
                for &field in REFERENCE_FIELDS {
                    if let Some(v) = vo.get(field) {
                        scan(v, visit);
                    }
                }
            }
        }
    }
}

/// Check cross graph integrity.
pub fn check_cross_graph_integrity(pkg: &LoadedPackage) -> InvariantVerdict {
    let nodes = collect_node_ids(pkg);

    // Concrete sidecar reference forms emitted today. Known V outputs derive
    // from the SAME `output_source::analytical_outputs` derivation Inv 3
    // (`evidence_coverage`) uses — the RO-Crate `@graph` output entities plus
    // any real-path proofs row — so a claim's un-prefixed `supported_by` path
    // (e.g. `runtime/outputs/de/de.csv` or a produced `.png`) resolves
    // consistently with Inv 3, and the two invariants never contradict each
    // other about the same C→V link. (The output paths already have any
    // `#fragment` stripped by `analytical_outputs`.)
    let outputs = analytical_outputs(&pkg.output_entities, &pkg.proofs);
    let known_outputs: BTreeSet<String> = outputs.iter().map(|o| o.path.clone()).collect();
    // A verified claim's `supported_by` is recorded by the runtime verifier as a
    // BASENAME (SME-path-safety: see `claim_verifier::table_label`), then
    // path-reconstructed by `claim_sink::evidence_ref_for`. When the agent nested
    // the table (`…/<task>/tables/de.tsv`), the reconstructed path does not equal
    // the registered `@id` even though the basename is registered UNDER THE SAME
    // TASK. `same_task_basename_match` resolves exactly that intra-task gap as a
    // fallback — but a cross-task, wrong-directory ref (basename registered under
    // a DIFFERENT task) does NOT resolve and stays a violation. Inv 3
    // (`evidence_coverage`) applies the identical rule, so the two never disagree
    // about the same C→V link.
    let known_edges: BTreeSet<String> = pkg
        .proofs
        .iter()
        .filter_map(|p| p.get("edge_id").and_then(|s| s.as_str()).map(String::from))
        .collect();

    let mut violators = Vec::new();
    let mut n_inspected = 0;

    // C → V: a Claim verdict's `supported_by` output reference (un-prefixed
    // path form). The prefix-tagged form is handled by the general pass.
    if let Some(claims) = &pkg.claims {
        if let Some(verdicts) = claims.get("verdicts").and_then(|v| v.as_array()) {
            for v in verdicts {
                if let Some(refs) = v.get("supported_by").and_then(|s| s.as_array()) {
                    for r in refs.iter().filter_map(|r| r.as_str()) {
                        if is_prefix_tagged(r) {
                            continue;
                        }
                        n_inspected += 1;
                        let path = r.split('#').next().unwrap_or(r);
                        // Exact path first; then the SAME-TASK basename fallback for
                        // the nested-table / direct-child-reconstruction mismatch. A
                        // basename that only matches an output under a DIFFERENT task
                        // (a wrong-directory ref) is NOT accepted.
                        if !known_outputs.contains(path)
                            && !outputs.iter().any(|o| same_task_basename_match(path, &o.path))
                        {
                            violators.push(format!("supported_by: {r}"));
                        }
                    }
                }
            }
        }
    }
    // F → proof edge: a Failure assumption's `edge_id` (un-prefixed form).
    for a in &pkg.assumptions {
        if let Some(eid) = a.get("edge_id").and_then(|s| s.as_str()) {
            if is_prefix_tagged(eid) {
                continue;
            }
            n_inspected += 1;
            if !known_edges.contains(eid) {
                violators.push(format!("assumption edge_id: {eid}"));
            }
        }
    }

    // Embedded RO-Crate `@graph` referential integrity: every `supported_by`
    // `@id` carried on an embedded `Claim` node MUST resolve to a real `@graph`
    // node `@id`. This closes a real gap — the C→V pass above validates the
    // SIDECAR verdict rows, but the injected `@graph` `Claim` nodes
    // (`ecaa_projection::project_claim_jsonld`) carry their OWN folded
    // `supported_by` references, and a dangling one (a `V:<basename>` handle with
    // no matching node, or a path that names no registered output File) used to
    // pass unchecked. `pkg.output_entities` is the full `@graph` (every entity
    // with an `@id`), so we build the node-id set once and resolve each embedded
    // Claim's `supported_by` against it. A reference resolving to no `@graph`
    // node FAILS the invariant.
    let graph_ids: BTreeSet<&str> = pkg
        .output_entities
        .iter()
        .filter_map(|e| e.get("@id").and_then(Value::as_str))
        .collect();
    for entity in &pkg.output_entities {
        if !entity_is_claim(entity) {
            continue;
        }
        let Some(refs) = entity.get("supported_by").and_then(Value::as_array) else {
            continue;
        };
        for r in refs {
            // Each fold is `{ "@id": "<ref>" }`; tolerate a bare string too.
            let target = r
                .get("@id")
                .and_then(Value::as_str)
                .or_else(|| r.as_str());
            let Some(target) = target else { continue };
            n_inspected += 1;
            if !graph_ids.contains(target) {
                let claim_id = entity.get("@id").and_then(Value::as_str).unwrap_or("?");
                violators.push(format!(
                    "embedded Claim {claim_id} supported_by dangling @id: {target}"
                ));
            }
        }
    }

    // General spec predicate: every `<letter>:<id>` reference in any sub-graph
    // row resolves to a node in the named sub-graph.
    let row_sets: [&[Value]; 6] = [
        &pkg.intake,
        &pkg.decisions,
        &pkg.validation_reports,
        &pkg.proofs,
        &pkg.verifier_decisions,
        &pkg.assumptions,
    ];
    let mut inspect = |target: &str| {
        n_inspected += 1;
        if !resolves(target, &nodes) {
            violators.push(format!("dangling ref: {target}"));
        }
    };
    for rows in row_sets {
        for row in rows {
            for_each_prefixed_ref(row, &mut inspect);
        }
    }
    if let Some(claims) = &pkg.claims {
        for_each_prefixed_ref(claims, &mut inspect);
    }

    let n_violations = violators.len();
    // A ∀-over-empty-set is vacuous: when the package carries no cross-graph
    // references at all (a fresh emit, no supported_by / edge_id / prefix-tagged
    // refs), there is nothing to dereference. Report Unverified rather than a
    // coerced Pass — the preprint must not certify integrity over an empty set.
    let status = if n_inspected == 0 {
        InvariantStatus::Unverified
    } else if n_violations == 0 {
        InvariantStatus::Pass
    } else {
        InvariantStatus::Fail
    };
    let detail = if n_inspected == 0 {
        Some("no cross-graph references present".into())
    } else if n_violations == 0 {
        None
    } else {
        Some(format!(
            "{} dangling cross-graph reference(s): {}",
            n_violations,
            violators.join("; ")
        ))
    };
    InvariantVerdict {
        id: InvariantId::CrossGraphIntegrity,
        status,
        detail,
        n_inspected,
        n_violations,
    }
}
