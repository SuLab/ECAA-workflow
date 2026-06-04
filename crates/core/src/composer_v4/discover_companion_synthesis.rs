//! Synthesize `discover_<axis>` companion stages on a v4 [`WorkflowDag`]
//! after meet-in-the-middle (or after the archetype seed lifts a DAG).
//!
//! The v2 builder's `emit_stage` post-pass authors `discover_<id>`
//! tasks for any stage with `DiscoveryRequirement::EmpiricalRequired`
//! before the corresponding compute task runs (`builder.rs::emit_stage`
//! "Emit discovery task wrapper if required" block). That gives the
//! SME a discovery node to pin `set_intake_method(<stage>,...)`
//! against. The v4 archetype-fast-path doesn't author those companion
//! nodes — its archetypes carry only the `operation` atoms — which is
//! why 8 conversation fixtures had to pin `composer_version: 1` to
//! keep their `set_intake_method` / `IntakeFollowup` assertions
//! firing.
//!
//! # Signal for synthesizing a companion
//!
//! An operation atom carries one of two signals that it needs a
//! `discover_*` companion:
//!
//! 1. **Explicit pointer** — `AtomDefinition.method_choice.deferred_to`
//!    names a discovery atom by id (e.g. `discover_dtu_method`). When
//!    present we synthesize `discover_<axis>` where `axis` is the
//!    `deferred_to` id with its `discover_` prefix stripped.
//! 2. **Implicit signal** — `AtomDefinition.attributes.candidate_tools`
//!    is a non-empty list. The v2 builder treats every stage with a
//!    runtime method choice as needing a discovery wrapper (whether or
//!    not the atom declares an explicit `method_choice` pointer); the
//!    `candidate_tools` list is the canonical "this atom has multiple
//!    runtime alternatives" marker authored across the registry
//!    (`alignment`, `clustering`, `normalisation`,
//!    `differential_expression`, etc.). When present we synthesize
//!    `discover_<atom_id>` using the atom id as the axis.
//!
//! Skip rules:
//! - `node.id` already starts with `discover_` (no
//!   `discover_discover_*` recursion).
//! - `node.id` already starts with `validate_` or `select_` (those
//!   companions own different lifecycle slots; method discovery for
//!   them would be nonsensical).
//! - A `discover_<axis>` companion already exists in the DAG —
//!   idempotent.
//! - The node has no atom in the registry (`atom_reg.get` returns
//!   `None`) — the post-pass can't make a method-discovery claim
//!   about a synthesized node.
//!
//! # Why we add `EdgeContract`s
//!
//! The original B.3 landing trusted `taxonomy::derive_role_from_id`'s
//! id-prefix sniffing to wire the discover companion into the
//! dependency graph. The lowering pass
//! (`backend_emitters/workflow_json.rs::lower_to_workflow_json`)
//! actually builds `Task.depends_on` straight off `dag.edges`, so a
//! synthesized companion with no corresponding edge lowers into a
//! `WORKFLOW.json` where every `discover_*` task is an orphan
//! (`depends_on=[]` and zero reverse-deps). That is the live bug the
//! SME hit: the agent never reads its `discover_alignment` output
//! before running `alignment`, defeating the
//! `set_intake_method(<stage>,...)` gate.
//!
//! The fix mirrors `companion_synthesis.rs::synthesize_validate_companions`:
//! every synthesized `discover_<axis>` node is paired with an
//! `EdgeContract { from_node: discover_id, to_node: target_id,.. }`
//! so the lowering pass sees the dependency. Port strings are left
//! empty — discover nodes have no typed output port today, and
//! `lower_to_workflow_json` reads `from_node`/`to_node` for
//! `depends_on`, not the port names (matching the validator
//! synthesis precedent).
//!
//! # Determinism
//!
//! - We iterate the original node ids in their existing slice order
//!   (the planner's earlier passes already sort by `id`) and append
//!   new nodes + edges in their natural traversal order, then
//!   re-sort `dag.nodes` by id and `dag.edges` by
//!   `(from_node, from_port, to_node, to_port)` — same discipline as
//!   `meet_in_middle.rs` and `companion_synthesis.rs`.

use std::collections::BTreeSet;

use crate::atom::AtomDefinition;
use crate::atom_registry::AtomRegistry;
use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

/// Walk every node in `dag.nodes` and synthesize a `discover_<axis>`
/// companion (plus its `discover_<axis> → <target>` edge) for any
/// atom that signals runtime method discovery (see module docs for
/// the signal rules). Mutates `dag` in place; idempotent when re-run
/// on a DAG that already carries companions (the existing-id guard
/// skips both the node AND the edge).
///
/// Mirrors `synthesize_validate_companions` — snapshot existing ids,
/// iterate, append, then sort `dag.nodes` and `dag.edges` for
/// byte-stable replay.
pub fn synthesize_discover_companions(
    dag: &mut WorkflowDag,
    atom_reg: &AtomRegistry,
    preferred: &crate::preferred_methods::PreferredMethods,
) {
    // Snapshot the existing node ids so we can detect existing
    // companions without rescanning per iteration.
    let existing_ids: BTreeSet<String> = dag.nodes.iter().map(|n| n.id.clone()).collect();

    let mut new_nodes: Vec<TaskNode> = Vec::new();
    let mut new_edges: Vec<EdgeContract> = Vec::new();
    let mut new_assumptions: Vec<crate::workflow_contracts::evidence::Assumption> = Vec::new();
    let mut emitted_axes: BTreeSet<String> = BTreeSet::new();

    // Snapshot the originals so we can iterate without borrowing
    // `dag.nodes` while mutating.
    let originals: Vec<TaskNode> = dag.nodes.clone();
    for node in &originals {
        // Skip companion / validator / selector ids — these never
        // need their own method-discovery wrapper.
        if node.id.starts_with("discover_")
            || node.id.starts_with("validate_")
            || node.id.starts_with("select_")
        {
            continue;
        }

        let Some(atom) = atom_reg.get(&node.machine_name) else {
            continue;
        };

        let Some(axis) = derive_axis(atom) else {
            continue;
        };

        let discover_id = format!("discover_{axis}");
        // Idempotency: skip BOTH the node AND the edge when the
        // discover_id is already in the DAG, or when this synthesis
        // pass already emitted the same axis (two atoms can share a
        // discovery axis when method_choice.deferred_to points to the
        // same id).
        if existing_ids.contains(&discover_id) || !emitted_axes.insert(axis.clone()) {
            continue;
        }

        let curated = candidate_tools(atom).unwrap_or_default();

        // Augment the candidate pool with an out-of-catalog requested
        // method when the SME/classifier named one for this axis that is
        // not already a curated member (compared on normalized ids).
        // Determinism: curated ids keep their authored order; a single
        // augmented id is appended last.
        let mut options = curated.clone();
        let mut candidate_pool_augmented = false;
        let mut spec_preferred: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
        if let Some(pref) = preferred.get(&axis) {
            let pref_id = crate::preferred_methods::normalize_method_id(&pref.id);
            spec_preferred.insert(
                pref_id.clone(),
                serde_json::Value::String(pref.source.rationale().to_string()),
            );
            let already_present = curated
                .iter()
                .any(|o| crate::preferred_methods::normalize_method_id(o) == pref_id);
            if !already_present {
                options.push(pref_id);
                candidate_pool_augmented = true;
            }
        }

        let mut discover_node =
            TaskNode::synthesize_discover(&discover_id, &axis, &options, &node.id);
        // Stamp the atom-id back-reference so `lower_to_workflow_json`
        // can populate `Task.source_atom_id` for per-task image
        // selection, safety enforcement, and plot-affordance lookup.
        discover_node.attributes.insert(
            "atom_id".into(),
            serde_json::Value::String(discover_id.clone()),
        );
        // Stamp the bare axis as `stage_class` so the agent prompt's
        // auto-approve gate (`scripts/agent-prompts/task-execution.md`)
        // has a string to match against the allow/deny lists written to
        // `runtime/.sme-auto-approve-discoveries`. Without this stamp the
        // BlockerCard checkbox is a no-op: the marker file is written
        // but the agent has no `stage_class` to compare. Axis is the
        // post-`discover_`-prefix-stripped form (e.g. `peptide_search`,
        // not `discover_peptide_search`) to match the contract enforced
        // by `crates/core/src/atom_registry.rs::AtomRegistry::discover_axes`.
        discover_node.attributes.insert(
            "stage_class".into(),
            serde_json::Value::String(axis.clone()),
        );
        // Stamp the SME/intake preference so the lowering pass folds it
        // into `Task.spec.spec_preferred_methods` (turns ON the specMatch
        // axis + the rank-#1/auto-advance prompt rule) and flags the pool
        // augmentation so the agent honors an out-of-catalog requested
        // method as a first-class candidate. Guarded by a non-empty map so
        // the no-preference path is byte-identical to today.
        if !spec_preferred.is_empty() {
            discover_node.attributes.insert(
                "spec_preferred_methods".into(),
                serde_json::Value::Object(spec_preferred),
            );
            discover_node.attributes.insert(
                "candidate_pool_augmented".into(),
                serde_json::Value::Bool(candidate_pool_augmented),
            );
        }

        // Wire the discover node as an upstream gate for the target
        // node. The lowering pass (`lower_to_workflow_json`) only
        // reads `from_node` / `to_node` to build `Task.depends_on`;
        // port strings are diagnostic. Mirrors the validator
        // synthesis precedent (`companion_synthesis.rs` lines
        // 118-127) where the same empty-port convention is used for
        // nodes whose ports don't expose a typed contract.
        // Mirror the planner's `workflow_ordering_edge` convention
        // (planner.rs:1099-1110): a method-discovery signal isn't a
        // port-typed data flow, but `score_dag` rejects any edge
        // whose `proof.producer_type` is empty
        // (`required_contract_unsatisfied = Reject`). Set both
        // producer_type and consumer_type to a stable sentinel and
        // attach the `workflow_ordering_edge` warning so downstream
        // policy / scoring treats this as ordering — not as
        // mis-typed data flow.
        let proof = CompatibilityProof {
            producer_type: "ecaax:method_discovery_signal".into(),
            consumer_type: "ecaax:method_discovery_signal".into(),
            warnings: vec![
                "workflow_ordering_edge: discover_companion method-discovery signal, no port-typed data flow"
                    .into(),
            ],
            rationale: Some(format!(
                "discover_companion: synthesized method-discovery edge for {} \
                 (mirrors v2 builder's emit_stage discover-task wrapper)",
                node.id
            )),
            ..Default::default()
        };

        new_edges.push(EdgeContract {
            from_node: discover_id.clone(),
            from_port: String::new(),
            to_node: node.id.clone(),
            to_port: String::new(),
            proof,
            // Author-intended ordering edge (method-discovery signal,
            // no port-typed data flow) — non-blocking in Draft.
            kind: EdgeKind::OrderingOnly,
            chain_of_custody: None,
        });
        // Record an OntologyAdapterInserted assumption for the discover
        // companion: it uses a sentinel IRI to bridge the "method not yet
        // chosen" gap, making the method-discovery decision visible in
        // the assumption ledger.
        //
        // Resolution is `Accepted` — not `Unresolved` (the default) — because
        // the discover companion is a deterministic structural insertion by the
        // v4 post-pass, not a pending SME choice. An `Unresolved` entry would
        // cause `score_dag` to count it in `unresolved_assumptions`, degrading
        // any DAG with discover companions from `ValidatedExecutableDag` to
        // `DraftDag` and blocking intake for all modalities that use `discover_*`
        // atoms (single_cell_rnaseq, chip_seq, atac_seq, etc.).
        new_assumptions.push(crate::workflow_contracts::evidence::Assumption {
            id: format!("discover_companion:{}:{}", discover_id, node.id),
            statement: format!(
                "Discover companion '{discover_id}' inserted as a method-discovery gate \
                 for '{}' using sentinel IRI 'ecaax:method_discovery_signal'.",
                node.id,
            ),
            source:
                crate::workflow_contracts::evidence::AssumptionSource::OntologyAdapterInserted {
                    from_iri: "ecaax:method_discovery_signal".into(),
                    to_iri: "ecaax:method_discovery_signal".into(),
                    reason: format!(
                        "discover_companion synthesis: '{discover_id}' gates runtime method \
                     selection for '{}'",
                        node.id
                    ),
                },
            affects_nodes: vec![node.id.clone()],
            risk: crate::workflow_contracts::evidence::RiskClass::Low,
            resolution: crate::workflow_contracts::evidence::AssumptionResolution::Accepted {
                rationale: "System-inserted discover companion; no SME decision required.".into(),
            },
            chain_of_custody: None,
        });
        new_nodes.push(discover_node);
    }

    if new_nodes.is_empty() && new_edges.is_empty() {
        return;
    }

    dag.nodes.extend(new_nodes);
    dag.edges.extend(new_edges);
    dag.assumptions.entries.extend(new_assumptions);

    // Re-sort to keep WorkflowDag byte-stable. Same sort keys as
    // `companion_synthesis.rs::synthesize_validate_companions` /
    // `meet_in_middle.rs::meet_in_the_middle`.
    dag.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    dag.edges.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.from_port.cmp(&b.from_port))
            .then_with(|| a.to_node.cmp(&b.to_node))
            .then_with(|| a.to_port.cmp(&b.to_port))
    });
}

/// Pick a discovery axis for `atom`. Returns `Some(axis)` when the
/// Return the bare discover axis for `atom` (the `<axis>` in the
/// synthesized `discover_<axis>` task id), or `None` if the atom never
/// triggers a discover companion.
///
/// Two paths matter (mirror `synthesize_discover_companions` above):
///
/// 1. Explicit `method_choice.deferred_to` present → axis is its value
///    with any leading `discover_` prefix stripped.
/// 2. Otherwise, non-empty `attributes.candidate_tools` → axis is `atom.id`
///    (matches the v2 builder's `discover_<stage_id>` shape).
///
/// Exposed `pub(crate)` so `crate::atom_registry::AtomRegistry::discover_axes`
/// can compute the authoritative server-side auto-approve allowlist from
/// the same source.
pub(crate) fn derive_axis(atom: &AtomDefinition) -> Option<String> {
    if let Some(mc) = &atom.method_choice {
        let axis = mc
            .deferred_to
            .strip_prefix("discover_")
            .unwrap_or(&mc.deferred_to);
        return Some(axis.to_string());
    }
    if candidate_tools(atom).is_some_and(|t| !t.is_empty()) {
        return Some(atom.id.clone());
    }
    None
}

/// Read the candidate-tools list from `attributes.candidate_tools`.
/// Returns `Some(_)` only when the value is a JSON array of strings
/// (the canonical authoring shape across the registry).
fn candidate_tools(atom: &AtomDefinition) -> Option<Vec<String>> {
    let raw = atom.attributes.get("candidate_tools")?;
    let arr = raw.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        out.push(v.as_str()?.to_string());
    }
    Some(out)
}

/// WG3 strict-mode (C4): type the method-discovery / survey companion
/// edges (`survey_method_landscape → discover_X`, `discover_X → X`, and
/// `<characterization> → survey_method_landscape`). The consumer
/// genuinely consumes the producer's artifact — the method-landscape
/// table or the method recommendation (each task writes a `result.json`
/// downstream reads) — so these are honest data flows, not mere
/// ordering. Discover companions are port-less skeletons, so we first
/// give them a typed method-recommendation output; then each consumer
/// gets an ADDITIVE input port accepting its companion producers (never
/// replacing real data inputs — `pick_best_port_pair` tries all ports,
/// so real analytical edges keep their existing typing) and the edges
/// are re-proven through the compatibility engine. `OrderingOnly`
/// companion edges land as `TypedDataFlow`, which `RiskMode::Production`
/// accepts. Mutates `dag` in place; idempotent.
pub(crate) fn type_companion_edges(dag: &mut WorkflowDag) {
    use crate::compatibility::engine::DeterministicCompatibilityEngine;
    use crate::composer_v4::planner::pick_best_port_pair;
    use crate::workflow_contracts::port::PortContract;
    use crate::workflow_contracts::semantic_type::SemanticType;
    use std::collections::BTreeMap;

    let is_discover = |id: &str| id.starts_with("discover_") || id.contains("_discover_");
    let is_companion_edge = |from: &str, to: &str| {
        is_discover(from) || is_discover(to) || to.contains("survey_method_landscape")
    };

    // 1. Discover companions are port-less skeletons; give each a typed
    //    method-recommendation output so the discover_X -> X edge has a
    //    producer artifact to flow.
    for node in dag.nodes.iter_mut() {
        if !node.outputs.is_empty() {
            continue;
        }
        // Companion producers reach here port-less: discover companions are
        // bare skeletons, and the survey atom declares a rich `outputs:`
        // block but no top-level `edam_data` (which is all `from_atom`'s
        // `synthesize_outputs` reads). Give each its canonical method-axis
        // artifact so the companion edges have a producer type to flow.
        let synthesized = if is_discover(&node.id) {
            Some((
                "method_recommendation",
                "ecaax:method_recommendation",
                "Method recommendation",
            ))
        } else if node.id.contains("survey_method_landscape") {
            Some((
                "method_landscape",
                "ecaax:method_landscape_matrix",
                "Method landscape matrix",
            ))
        } else {
            None
        };
        if let Some((pname, iri, label)) = synthesized {
            node.outputs = vec![PortContract {
                name: pname.into(),
                semantic_type: SemanticType::OntologyTerm {
                    iri: iri.into(),
                    label: label.into(),
                    ontology_version: Some("ecaax-1".into()),
                },
                privacy_class: crate::workflow_contracts::port::PortPrivacyClass::Internal,
                ..Default::default()
            }];
        }
    }

    // 2. Add an ADDITIVE input port to each consumer for every distinct
    //    companion-producer type it receives (only when not already
    //    accepted), so the engine can prove the edge via that port.
    let outputs_by_id: BTreeMap<String, Vec<PortContract>> = dag
        .nodes
        .iter()
        .map(|n| (n.id.clone(), n.outputs.clone()))
        .collect();
    let mut needed: BTreeMap<String, Vec<PortContract>> = BTreeMap::new();
    for e in &dag.edges {
        if e.kind == EdgeKind::OrderingOnly && is_companion_edge(&e.from_node, &e.to_node) {
            if let Some(p) = outputs_by_id.get(&e.from_node).and_then(|o| o.first()) {
                needed.entry(e.to_node.clone()).or_default().push(p.clone());
            }
        }
    }
    for (cid, mut ports) in needed {
        ports.sort_by(|a, b| a.semantic_type.stable_id().cmp(&b.semantic_type.stable_id()));
        ports.dedup_by(|a, b| a.semantic_type.stable_id() == b.semantic_type.stable_id());
        if let Some(node) = dag.nodes.iter_mut().find(|n| n.id == cid) {
            for p in ports {
                let already = node
                    .inputs
                    .iter()
                    .any(|ip| ip.semantic_type.stable_id() == p.semantic_type.stable_id());
                if !already {
                    // Clone the producer's FULL output port (preserving
                    // physical_format + privacy_class so the engine can prove
                    // the pair) as an additive consumer input.
                    let mut inp = p.clone();
                    inp.name = format!("companion_in_{}", node.inputs.len());
                    node.inputs.push(inp);
                }
            }
        }
    }

    // 3. Re-prove the companion edges now that the ports line up.
    let inputs_by_id: BTreeMap<String, Vec<PortContract>> = dag
        .nodes
        .iter()
        .map(|n| (n.id.clone(), n.inputs.clone()))
        .collect();
    let engine = DeterministicCompatibilityEngine::new();
    for edge in &mut dag.edges {
        if edge.kind != EdgeKind::OrderingOnly
            || !is_companion_edge(&edge.from_node, &edge.to_node)
        {
            continue;
        }
        let prod_outs = outputs_by_id
            .get(&edge.from_node)
            .cloned()
            .unwrap_or_default();
        let cons_ins = inputs_by_id.get(&edge.to_node).cloned().unwrap_or_default();
        let (out_port, in_port, proof, kind) =
            pick_best_port_pair(&engine, &prod_outs, &cons_ins, true);
        edge.from_port = out_port.name;
        edge.to_port = in_port.name;
        edge.proof = proof;
        edge.kind = kind;
    }

    // Keep the WorkflowDag byte-stable (ports/edges may have changed).
    dag.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    dag.edges.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.from_port.cmp(&b.from_port))
            .then_with(|| a.to_node.cmp(&b.to_node))
            .then_with(|| a.to_port.cmp(&b.to_port))
    });
}

/// WG3 strict-mode (C5): catch-all typing for any declared-`OrderingOnly`
/// edge left after the validate (C1) / aggregator (C2) / companion (C4)
/// passes. These are archetype-declared dependencies whose consumer input
/// IRI did not unify with the producer output IRI for the same artifact
/// family, OR where the consumer reads the producer's `result.json` as a
/// SECONDARY input (a QC flag; a cross-branch barrier inspecting upstream
/// outputs). In this execution model every task writes a `result.json`
/// that downstream agents read, so adding the producer's output as an
/// ADDITIVE consumer input (never replacing the consumer's primary data
/// inputs — `pick_best_port_pair` tries all ports so the primary edges
/// keep their typing) is an honest typing; the edge then re-proves to
/// TypedDataFlow/AdapterMediated. Producers that are genuinely output-less
/// (none expected post-C2/C4) leave their edge OrderingOnly. Mutates
/// `dag` in place; idempotent.
pub(crate) fn type_residual_ordering_edges(dag: &mut WorkflowDag) {
    use crate::compatibility::engine::{
        CompatibilityEngine, CompatibilityResult, DeterministicCompatibilityEngine,
        PlanningContext as CompatibilityContext,
    };
    use crate::composer_v4::planner::pick_best_port_pair;
    use crate::workflow_contracts::port::PortContract;
    use std::collections::BTreeMap;

    let outputs_by_id: BTreeMap<String, Vec<PortContract>> = dag
        .nodes
        .iter()
        .map(|n| (n.id.clone(), n.outputs.clone()))
        .collect();

    // For each remaining OrderingOnly edge whose producer has an output,
    // collect the producer's output port per consumer.
    let mut needed: BTreeMap<String, Vec<PortContract>> = BTreeMap::new();
    for e in &dag.edges {
        if e.kind != EdgeKind::OrderingOnly {
            continue;
        }
        if let Some(p) = outputs_by_id.get(&e.from_node).and_then(|o| o.first()) {
            needed.entry(e.to_node.clone()).or_default().push(p.clone());
        }
    }
    // Add ADDITIVE inputs (full-clone, preserving format + privacy) for any
    // producer output the consumer does not ALREADY accept. The "already"
    // check uses the engine — NOT a bare EDAM-IRI match — so a consumer that
    // declares a same-IRI but facet-incompatible input (e.g.
    // translation_efficiency_de's raw rpf/mrna `data:3917` vs normalisation's
    // normalized `data:3917`) still gains the producer's exact port and the
    // edge proves.
    let engine = DeterministicCompatibilityEngine::new();
    let cctx = CompatibilityContext::default();
    for (cid, mut ports) in needed {
        ports.sort_by(|a, b| a.semantic_type.stable_id().cmp(&b.semantic_type.stable_id()));
        ports.dedup_by(|a, b| a.semantic_type == b.semantic_type);
        if let Some(node) = dag.nodes.iter_mut().find(|n| n.id == cid) {
            for p in ports {
                let already = node.inputs.iter().any(|ip| {
                    matches!(
                        engine.prove(&p, ip, &cctx),
                        CompatibilityResult::Compatible(_)
                            | CompatibilityResult::CompatibleWithAdapters { .. }
                    )
                });
                if !already {
                    let mut inp = p.clone();
                    inp.name = format!("residual_in_{}", node.inputs.len());
                    node.inputs.push(inp);
                }
            }
        }
    }

    // Re-prove every remaining OrderingOnly edge.
    let inputs_by_id: BTreeMap<String, Vec<PortContract>> = dag
        .nodes
        .iter()
        .map(|n| (n.id.clone(), n.inputs.clone()))
        .collect();
    for edge in &mut dag.edges {
        if edge.kind != EdgeKind::OrderingOnly {
            continue;
        }
        let prod_outs = outputs_by_id
            .get(&edge.from_node)
            .cloned()
            .unwrap_or_default();
        let cons_ins = inputs_by_id.get(&edge.to_node).cloned().unwrap_or_default();
        let (out_port, in_port, proof, kind) =
            pick_best_port_pair(&engine, &prod_outs, &cons_ins, true);
        edge.from_port = out_port.name;
        edge.to_port = in_port.name;
        edge.proof = proof;
        edge.kind = kind;
    }

    dag.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    dag.edges.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.from_port.cmp(&b.from_port))
            .then_with(|| a.to_node.cmp(&b.to_node))
            .then_with(|| a.to_port.cmp(&b.to_port))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::evidence::AssumptionLedger;
    use std::path::Path;

    fn real_registry() -> AtomRegistry {
        AtomRegistry::load_from_dir(Path::new("../../config/stage-atoms"))
            .expect("load stage-atoms registry")
    }

    /// Wrap a slice of TaskNodes into a WorkflowDag for the new
    /// `synthesize_discover_companions(&mut dag, &reg)` signature.
    fn dag_with(nodes: Vec<TaskNode>) -> WorkflowDag {
        WorkflowDag {
            id: "test".into(),
            nodes,
            edges: Vec::new(),
            assumptions: AssumptionLedger::default(),
            source_template: None,
        }
    }

    /// WG3 strict-mode (C4): survey→discover_X and discover_X→X companion
    /// edges are retyped from OrderingOnly to engine-proven TypedDataFlow.
    #[test]
    fn companion_edges_are_typed_not_ordering_only() {
        use crate::workflow_contracts::port::PortContract;
        // Port-less survey node — as `from_atom` produces for the
        // survey_method_landscape atom (rich `outputs:` block, no top-level
        // edam_data). `type_companion_edges` must synthesize its
        // (internal-privacy) method-landscape output so survey→discover types.
        let survey = TaskNode::skeleton("survey_method_landscape", "survey");
        // Port-less discover skeleton (as synthesize_discover produces).
        let discover = TaskNode::skeleton("discover_alignment", "discover aligner");
        let mut alignment = TaskNode::skeleton("alignment", "align");
        alignment.inputs = vec![PortContract::from_edam(
            "reads",
            Some("data:2044"),
            Some("format:1930"),
        )];
        alignment.outputs = vec![PortContract::from_edam(
            "bam",
            Some("data:0863"),
            Some("format:2572"),
        )];
        let oo = |from: &str, to: &str| EdgeContract {
            from_node: from.into(),
            from_port: String::new(),
            to_node: to.into(),
            to_port: String::new(),
            proof: CompatibilityProof::default(),
            kind: EdgeKind::OrderingOnly,
            chain_of_custody: None,
        };
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![survey, discover, alignment],
            edges: vec![
                oo("survey_method_landscape", "discover_alignment"),
                oo("discover_alignment", "alignment"),
            ],
            assumptions: AssumptionLedger::default(),
            source_template: None,
        };
        type_companion_edges(&mut dag);
        for e in &dag.edges {
            assert_ne!(
                e.kind,
                EdgeKind::OrderingOnly,
                "companion edge {}->{} must be engine-proven (WG3 C4), not OrderingOnly",
                e.from_node,
                e.to_node
            );
        }
        // The discover skeleton now carries a typed recommendation output.
        let disc = dag.nodes.iter().find(|n| n.id == "discover_alignment").unwrap();
        assert!(!disc.outputs.is_empty(), "discover node should gain a method-recommendation output");
    }

    /// WG3 strict-mode (C5): a residual within-pipeline ordering edge
    /// (consumer's declared input IRI differs from the producer's output
    /// IRI for the same artifact) is retyped from OrderingOnly to
    /// engine-proven via an additive consumer input.
    #[test]
    fn residual_ordering_edges_are_typed() {
        use crate::workflow_contracts::port::PortContract;
        let mut de = TaskNode::skeleton("differential_expression", "DE");
        de.outputs = vec![PortContract::from_edam(
            "de_results",
            Some("data:3134"),
            Some("format:3475"),
        )];
        let mut pathway = TaskNode::skeleton("pathway_enrichment", "pathway");
        // pathway's declared input is data:0951 — does NOT unify with DE's
        // data:3134, so the archetype edge lands OrderingOnly.
        pathway.inputs = vec![PortContract::from_edam(
            "ranked_de_results",
            Some("data:0951"),
            Some("format:3475"),
        )];
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![de, pathway],
            edges: vec![EdgeContract {
                from_node: "differential_expression".into(),
                from_port: String::new(),
                to_node: "pathway_enrichment".into(),
                to_port: String::new(),
                proof: crate::workflow_contracts::edge::CompatibilityProof::default(),
                kind: EdgeKind::OrderingOnly,
                chain_of_custody: None,
            }],
            assumptions: AssumptionLedger::default(),
            source_template: None,
        };
        type_residual_ordering_edges(&mut dag);
        assert_ne!(
            dag.edges[0].kind,
            EdgeKind::OrderingOnly,
            "residual within-pipeline edge must be engine-proven (WG3 C5), not OrderingOnly"
        );
    }

    /// Assert that for every synthesized discover_X node there is a
    /// matching EdgeContract with `from_node == discover_X && to_node
    /// == target`. The matching target id is whichever original node
    /// (id NOT starting with `discover_`) the planner saw for that
    /// axis; the helper accepts a list of acceptable target ids.
    fn assert_discover_edges_present(dag: &WorkflowDag, expected_targets: &[(&str, &[&str])]) {
        for (discover_id, possible_targets) in expected_targets {
            let edges: Vec<&EdgeContract> = dag
                .edges
                .iter()
                .filter(|e| e.from_node.as_str() == *discover_id)
                .collect();
            assert!(
                !edges.is_empty(),
                "expected an edge with from_node = {discover_id}; got edges {:?}",
                dag.edges
            );
            for e in &edges {
                assert!(
                    possible_targets.contains(&e.to_node.as_str()),
                    "edge {discover_id} -> {} not in acceptable targets {:?}",
                    e.to_node,
                    possible_targets
                );
            }
        }
    }

    /// `method_choice.deferred_to` → discover companion named after
    /// the stripped axis. `differential_transcript_usage` is
    /// authored with `method_choice.deferred_to: discover_dtu_method`.
    #[test]
    fn synthesize_uses_method_choice_deferred_to() {
        let reg = real_registry();
        let mut dag = dag_with(vec![TaskNode::skeleton(
            "differential_transcript_usage",
            "test",
        )]);
        synthesize_discover_companions(
            &mut dag,
            &reg,
            &crate::preferred_methods::PreferredMethods::new(),
        );

        let companions: Vec<&TaskNode> = dag
            .nodes
            .iter()
            .filter(|n| n.id.starts_with("discover_"))
            .collect();
        assert_eq!(
            companions.len(),
            1,
            "expected one companion; got {companions:?}"
        );
        assert_eq!(companions[0].id, "discover_dtu_method");
        assert_eq!(
            companions[0].attributes.get("method_axis").unwrap(),
            &serde_json::Value::String("dtu_method".into()),
            "method_axis must be the deferred_to id with discover_ prefix stripped"
        );

        // The synthesized companion must carry a matching edge.
        assert_discover_edges_present(
            &dag,
            &[("discover_dtu_method", &["differential_transcript_usage"])],
        );
    }

    /// `attributes.candidate_tools` → discover companion named
    /// after the atom id (no `method_choice` field). `alignment`
    /// is authored with `candidate_tools: [star, hisat2, salmon,
    /// bwa_mem, minimap2]`.
    #[test]
    fn synthesize_uses_candidate_tools_when_no_method_choice() {
        let reg = real_registry();
        let mut dag = dag_with(vec![TaskNode::skeleton("alignment", "test")]);
        synthesize_discover_companions(
            &mut dag,
            &reg,
            &crate::preferred_methods::PreferredMethods::new(),
        );

        let companions: Vec<&TaskNode> = dag
            .nodes
            .iter()
            .filter(|n| n.id.starts_with("discover_"))
            .collect();
        assert_eq!(
            companions.len(),
            1,
            "expected one companion; got {companions:?}"
        );
        assert_eq!(companions[0].id, "discover_alignment");
        assert_eq!(
            companions[0].attributes.get("method_axis").unwrap(),
            &serde_json::Value::String("alignment".into())
        );
        let options = companions[0]
            .attributes
            .get("method_options")
            .and_then(|v| v.as_array())
            .expect("method_options must be a JSON array");
        assert!(
            !options.is_empty(),
            "candidate_tools-derived options must be populated"
        );

        // Edge must be present.
        assert_discover_edges_present(&dag, &[("discover_alignment", &["alignment"])]);
    }

    /// An atom with neither signal produces no companion.
    /// `data_acquisition` is an SME-assignee intake stage with no
    /// runtime method choice — no candidate_tools, no method_choice.
    #[test]
    fn synthesize_skips_atoms_without_signal() {
        let reg = real_registry();
        let mut dag = dag_with(vec![TaskNode::skeleton("data_acquisition", "test")]);
        synthesize_discover_companions(
            &mut dag,
            &reg,
            &crate::preferred_methods::PreferredMethods::new(),
        );
        let companions: Vec<&TaskNode> = dag
            .nodes
            .iter()
            .filter(|n| n.id.starts_with("discover_"))
            .collect();
        assert!(
            companions.is_empty(),
            "data_acquisition has no method discovery signal; got {companions:?}"
        );
        assert!(
            dag.edges.is_empty(),
            "no companion synthesized but edges were emitted: {:?}",
            dag.edges
        );
    }

    /// `discover_*` / `validate_*` / `select_*` ids are skipped
    /// regardless of registry shape — the prefix guard fires before
    /// the registry lookup.
    #[test]
    fn synthesize_skips_companion_prefix_ids() {
        let reg = real_registry();
        for skip_id in [
            "discover_alignment",
            "validate_alignment",
            "select_alignment",
        ] {
            let mut dag = dag_with(vec![TaskNode::skeleton(skip_id, "test")]);
            synthesize_discover_companions(
                &mut dag,
                &reg,
                &crate::preferred_methods::PreferredMethods::new(),
            );
            // The dag should still contain only the original node;
            // any new node would have to be a `discover_*` companion
            // for the skip_id, and the prefix-skip rule fires first.
            let added: Vec<&TaskNode> = dag.nodes.iter().filter(|n| n.id != skip_id).collect();
            assert!(
                added.is_empty(),
                "{skip_id} produced a companion despite the prefix-skip rule: {added:?}"
            );
            assert!(
                dag.edges.is_empty(),
                "{skip_id} produced edges despite the prefix-skip rule: {:?}",
                dag.edges
            );
        }
    }

    /// Re-running the synthesis on a DAG that already carries
    /// companions is a no-op (idempotent).
    #[test]
    fn synthesize_is_idempotent() {
        let reg = real_registry();
        let mut dag = dag_with(vec![TaskNode::skeleton("alignment", "test")]);
        synthesize_discover_companions(
            &mut dag,
            &reg,
            &crate::preferred_methods::PreferredMethods::new(),
        );
        let after_first_n = dag.nodes.len();
        let after_first_e = dag.edges.len();
        assert_eq!(after_first_n, 2, "expected one companion added");
        assert_eq!(after_first_e, 1, "expected one edge added");

        // Run again; nothing should change.
        synthesize_discover_companions(
            &mut dag,
            &reg,
            &crate::preferred_methods::PreferredMethods::new(),
        );
        assert_eq!(
            dag.nodes.len(),
            after_first_n,
            "second pass added nodes (not idempotent)"
        );
        assert_eq!(
            dag.edges.len(),
            after_first_e,
            "second pass added edges (not idempotent)"
        );
    }

    /// Companions are sorted by id in `dag.nodes`. Two
    /// candidate-tools atoms in reverse-alphabetical input order
    /// must produce a `dag.nodes` sorted by id after synthesis.
    #[test]
    fn synthesize_returns_companions_sorted_by_id() {
        let reg = real_registry();
        // `quantification` and `alignment` both have candidate_tools.
        // Input order: quantification first, alignment second — output
        // must be sorted (alignment, discover_alignment,
        // discover_quantification, quantification) post-synthesis.
        let mut dag = dag_with(vec![
            TaskNode::skeleton("quantification", "test"),
            TaskNode::skeleton("alignment", "test"),
        ]);
        synthesize_discover_companions(
            &mut dag,
            &reg,
            &crate::preferred_methods::PreferredMethods::new(),
        );

        let companions: Vec<&TaskNode> = dag
            .nodes
            .iter()
            .filter(|n| n.id.starts_with("discover_"))
            .collect();
        assert_eq!(companions.len(), 2, "got {companions:?}");
        assert_eq!(companions[0].id, "discover_alignment");
        assert_eq!(companions[1].id, "discover_quantification");

        // dag.nodes must be sorted by id for byte determinism.
        let actual: Vec<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        let mut sorted = actual.clone();
        sorted.sort();
        assert_eq!(
            actual, sorted,
            "dag.nodes must be sorted by id post-synthesis"
        );

        // Both edges present and pointing to the correct targets.
        assert_discover_edges_present(
            &dag,
            &[
                ("discover_alignment", &["alignment"]),
                ("discover_quantification", &["quantification"]),
            ],
        );

        // dag.edges must be sorted by (from_node, from_port, to_node, to_port).
        let actual_edges: Vec<(&str, &str, &str, &str)> = dag
            .edges
            .iter()
            .map(|e| {
                (
                    e.from_node.as_str(),
                    e.from_port.as_str(),
                    e.to_node.as_str(),
                    e.to_port.as_str(),
                )
            })
            .collect();
        let mut sorted_edges = actual_edges.clone();
        sorted_edges.sort();
        assert_eq!(
            actual_edges, sorted_edges,
            "dag.edges must be sorted post-synthesis"
        );
    }

    /// Integration-style guard for the live user-hit bug: synthesize
    /// on a multi-node DAG (alignment + quantification +
    /// differential_expression) and assert that none of the
    /// synthesized `discover_*` nodes are orphans — every discover_X
    /// must carry at least one out-edge whose `to_node` is the
    /// original method-bearing target. Without the edges, the lowering
    /// pass produces a `WORKFLOW.json` where every `discover_*` task has
    /// `depends_on=[]` and zero reverse-deps, which is the bug the SME
    /// hit on the bulk-rnaseq run.
    #[test]
    fn synthesize_emits_edges_for_every_discover_node() {
        let reg = real_registry();
        // Three method-choice atoms — all three have candidate_tools
        // in the registry.
        let mut dag = dag_with(vec![
            TaskNode::skeleton("alignment", "test"),
            TaskNode::skeleton("quantification", "test"),
            TaskNode::skeleton("differential_expression", "test"),
        ]);
        synthesize_discover_companions(
            &mut dag,
            &reg,
            &crate::preferred_methods::PreferredMethods::new(),
        );

        let discover_ids: Vec<String> = dag
            .nodes
            .iter()
            .filter(|n| n.id.starts_with("discover_"))
            .map(|n| n.id.clone())
            .collect();
        assert!(
            discover_ids.len() >= 3,
            "expected ≥3 discover_* companions, got {discover_ids:?}"
        );

        // Every synthesized discover_X must carry at least one
        // out-edge with from_node == discover_X. This is the
        // load-bearing assertion: if any discover_X is orphan, the
        // bug regresses.
        for discover_id in &discover_ids {
            let out_edges: Vec<&EdgeContract> = dag
                .edges
                .iter()
                .filter(|e| e.from_node == *discover_id)
                .collect();
            assert!(
                !out_edges.is_empty(),
                "discover_X node {discover_id} is an orphan (out_degree=0) — \
                 lowering will emit an unedged discover_* task in WORKFLOW.json. \
                 dag.edges = {:?}",
                dag.edges
            );
            // The target node must exist in the dag.
            for e in out_edges {
                assert!(
                    dag.nodes.iter().any(|n| n.id == e.to_node),
                    "edge {discover_id} -> {} points at a non-existent node",
                    e.to_node
                );
                // The target id must NOT itself be a discover_*
                // node — discover wrappers gate their non-discover
                // target.
                assert!(
                    !e.to_node.starts_with("discover_"),
                    "discover companion edge {discover_id} -> {} points at another discover_* node",
                    e.to_node
                );
            }
        }

        // Specific axes from the registry must surface.
        assert!(discover_ids.contains(&"discover_alignment".to_string()));
        assert!(discover_ids.contains(&"discover_quantification".to_string()));
        assert!(discover_ids.contains(&"discover_differential_expression".to_string()));
    }

    /// Load-bearing assertion: after discover-companion synthesis, the
    /// lowering pass (`build_dag_from_workflow_dag` →
    /// `lower_to_workflow_json`) must produce a `DAG` where the
    /// target node's `depends_on` includes the synthesized
    /// `discover_<axis>` id. Without the edges this round-trip
    /// fails — every `discover_*` task ends up an orphan in
    /// `WORKFLOW.json`, which is the live SME-hit bug.
    #[test]
    fn lowered_workflow_json_contains_discover_target_dependency() {
        let reg = real_registry();
        let mut dag = dag_with(vec![TaskNode::skeleton("alignment", "test")]);
        synthesize_discover_companions(
            &mut dag,
            &reg,
            &crate::preferred_methods::PreferredMethods::new(),
        );

        // Lower the v4 WorkflowDag through the same path the
        // emitter uses. The lowering pass reads `dag.edges` to
        // build `Task.depends_on`.
        let lowered =
            crate::builder::build_dag_from_workflow_dag(&dag, "wf-test").expect("lower v4 dag");

        // `alignment` must depend on `discover_alignment`.
        let alignment_task = lowered
            .tasks
            .get("alignment")
            .expect("alignment task must lower");
        assert!(
            alignment_task
                .depends_on
                .iter()
                .any(|d| d.as_str() == "discover_alignment"),
            "lowered alignment.depends_on must include discover_alignment, \
             got {:?}. Bug regression: without edges, the lowering pass \
             leaves discover_alignment orphaned in WORKFLOW.json.",
            alignment_task.depends_on
        );

        // The `discover_alignment` task must lower with empty
        // depends_on — it is the gating root for its axis.
        let discover_task = lowered
            .tasks
            .get("discover_alignment")
            .expect("discover_alignment task must lower");
        assert!(
            discover_task.depends_on.is_empty(),
            "discover_alignment must be a gating root (depends_on=[]), got {:?}",
            discover_task.depends_on
        );
    }

    /// SME/intake-preferred method gets stamped onto the discover node
    /// spec and, when out-of-catalog, augments the candidate pool — the
    /// mechanism that makes a named method (e.g. `lofreq`) rank #1.
    #[test]
    fn preferred_method_stamps_spec_and_augments_pool() {
        use crate::classify::MethodSpec;
        use crate::preferred_methods::PreferredMethods;
        let reg = real_registry();

        // (a) out-of-pool (octopus is NOT a curated variant_calling tool):
        // stamped on spec_preferred_methods AND appended to method_options,
        // with candidate_pool_augmented = true.
        let mut dag = dag_with(vec![TaskNode::skeleton("variant_calling", "test")]);
        let pref = PreferredMethods::from_method_specs(&[MethodSpec {
            stage: "variant_calling".into(),
            method: "Octopus".into(),
        }]);
        synthesize_discover_companions(&mut dag, &reg, &pref);
        let node = dag
            .nodes
            .iter()
            .find(|n| n.id == "discover_variant_calling")
            .expect("discover_variant_calling synthesized");
        let spm = node
            .attributes
            .get("spec_preferred_methods")
            .and_then(|v| v.as_object())
            .expect("spec_preferred_methods stamped");
        assert!(
            spm.contains_key("octopus"),
            "octopus must be a preferred key, got {spm:?}"
        );
        assert_eq!(
            node.attributes.get("candidate_pool_augmented"),
            Some(&serde_json::Value::Bool(true)),
            "out-of-pool method must flag candidate_pool_augmented=true"
        );
        let opts = node
            .attributes
            .get("method_options")
            .and_then(|v| v.as_array())
            .expect("method_options present");
        assert!(
            opts.iter().any(|o| o.as_str() == Some("octopus")),
            "augmented pool must contain octopus, got {opts:?}"
        );

        // Lowering round-trip: the stamp folds into Task.spec.
        let lowered =
            crate::builder::build_dag_from_workflow_dag(&dag, "wf-test").expect("lower v4 dag");
        let spec = lowered
            .tasks
            .get("discover_variant_calling")
            .and_then(|t| t.spec.as_ref())
            .and_then(|s| s.as_object())
            .expect("discover_variant_calling spec lowered");
        assert!(
            spec.get("spec_preferred_methods")
                .and_then(|v| v.as_object())
                .map(|m| m.contains_key("octopus"))
                .unwrap_or(false),
            "lowered spec must carry spec_preferred_methods[octopus], got {spec:?}"
        );
        assert_eq!(
            spec.get("candidate_pool_augmented"),
            Some(&serde_json::Value::Bool(true)),
        );

        // (b) in-pool (mutect2 IS curated): stamped, but pool NOT augmented.
        let mut dag_b = dag_with(vec![TaskNode::skeleton("variant_calling", "test")]);
        let pref_b = PreferredMethods::from_method_specs(&[MethodSpec {
            stage: "variant_calling".into(),
            method: "mutect2".into(),
        }]);
        synthesize_discover_companions(&mut dag_b, &reg, &pref_b);
        let node_b = dag_b
            .nodes
            .iter()
            .find(|n| n.id == "discover_variant_calling")
            .unwrap();
        assert_eq!(
            node_b.attributes.get("candidate_pool_augmented"),
            Some(&serde_json::Value::Bool(false)),
            "in-pool method must NOT augment the pool"
        );

        // (c) no preference: NEITHER attribute present (byte-identical to today).
        let mut dag_c = dag_with(vec![TaskNode::skeleton("variant_calling", "test")]);
        synthesize_discover_companions(&mut dag_c, &reg, &PreferredMethods::new());
        let node_c = dag_c
            .nodes
            .iter()
            .find(|n| n.id == "discover_variant_calling")
            .unwrap();
        assert!(node_c.attributes.get("spec_preferred_methods").is_none());
        assert!(node_c.attributes.get("candidate_pool_augmented").is_none());
    }

    /// Every synthesized `discover_*` task must carry `stage_class` on
    /// both the `TaskNode.attributes` map (so the lowering pass can
    /// see it) AND the lowered `Task.spec` (so the agent's auto-approve
    /// matcher in `scripts/agent-prompts/task-execution.md` finds a
    /// non-null string when reading `task-spec.json`).
    ///
    /// Without this stamp the BlockerCard "auto-approve routine
    /// discoveries" checkbox is a no-op — the marker file lands on
    /// disk but the agent has no stage_class to match against the
    /// allow / deny lists.
    #[test]
    fn synthesized_companions_carry_stage_class_through_lowering() {
        let reg = real_registry();
        // Pick axes from both paths: `candidate_tools` (alignment) and
        // `method_choice.deferred_to` (time_series_model_fitting → axis
        // `time_series_method`).
        let mut dag = dag_with(vec![
            TaskNode::skeleton("alignment", "test"),
            TaskNode::skeleton("time_series_model_fitting", "test"),
        ]);
        synthesize_discover_companions(
            &mut dag,
            &reg,
            &crate::preferred_methods::PreferredMethods::new(),
        );

        for (companion_id, expected_axis) in [
            ("discover_alignment", "alignment"),
            ("discover_time_series_method", "time_series_method"),
        ] {
            let companion = dag
                .nodes
                .iter()
                .find(|n| n.id == companion_id)
                .unwrap_or_else(|| {
                    panic!(
                        "expected synthesized {companion_id}; got {:?}",
                        dag.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
                    )
                });
            assert_eq!(
                companion.attributes.get("stage_class"),
                Some(&serde_json::Value::String(expected_axis.into())),
                "{companion_id} must stamp attributes.stage_class = {expected_axis:?}"
            );
        }

        // Lower and assert the stamp survived into Task.spec.
        let lowered =
            crate::builder::build_dag_from_workflow_dag(&dag, "wf-test").expect("lower v4 dag");
        for (companion_id, expected_axis) in [
            ("discover_alignment", "alignment"),
            ("discover_time_series_method", "time_series_method"),
        ] {
            let task = lowered
                .tasks
                .get(companion_id)
                .unwrap_or_else(|| panic!("{companion_id} must lower into Task"));
            let stage_class = task
                .spec
                .as_ref()
                .and_then(|s| s.get("stage_class"))
                .and_then(|v| v.as_str());
            assert_eq!(
                stage_class,
                Some(expected_axis),
                "{companion_id}.spec.stage_class must round-trip to {expected_axis:?}; \
                 got spec={:?}. Without this round-trip the agent's auto-approve gate \
                 cannot find a stage_class to match the BlockerCard marker.",
                task.spec
            );
        }
    }
}
