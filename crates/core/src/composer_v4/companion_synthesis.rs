//! Synthesize `validate_*` companion stages on a
//! v4 [`WorkflowDag`] after meet-in-the-middle (or after the
//! archetype seed lifts a DAG).
//!
//! Mirrors the v2 builder's validate-companion post-pass
//! (`builder.rs::emit_stage`'s "Emit validate task for stages that
//! aren't already reviews, discoveries, or self-validations" block).
//! The v2 builder synthesizes the companion regardless of whether a
//! `validate_<id>` atom exists in the registry — `validate_<id>` is a
//! generic `TaskKind::Validation` task that depends on the upstream
//! stage and asserts its outputs. v4's lowering pass
//! (`backend_emitters/workflow_json.rs::lower_task_kind`) reads the
//! synthesized node's `attributes["role"] = "validation"` and
//! Produces the same `Task { kind: TaskKind::Validation,.. }` shape
//! as the v2 path.
//!
//! # Why this lives at the WorkflowDag level
//!
//! The v4 dispatch path lowers the planner's `WorkflowDag` two ways:
//!
//! 1. `build_dag_from_workflow_dag` → `lower_to_workflow_json`
//!    (v4-only path).
//! 2. `lower_dag_to_composition_result` →
//!    `build_dag_from_composition` → `emit_stage` (legacy v2 builder
//!    path; emit_stage already synthesizes validate companions).
//!
//! Synthesizing companions on the `WorkflowDag` makes path (1)
//! produce parity with path (2) without forking the lowering logic.
//! Path (2) sees the companion already present and the v2 builder's
//! `if !tasks.contains_key(&validate_id)` guard keeps it idempotent.
//!
//! # Skip rules
//!
//! Do NOT synthesize a companion when:
//!
//! - The node id already starts with `validate_` (it's already a
//!   validator — no `validate_validate_<x>` recursion).
//! - The node id already starts with `discover_` (the v2 builder also
//!   skips discovery atoms; their output IS the result, and any
//!   downstream consumer's companion will catch downstream issues).
//! - The node's role attribute is `Validation` / `Selection`.
//!   `Aggregator` is NOT skipped: v2's
//!   `emit_stage` post-pass synthesizes validators for aggregator
//!   atoms too (e.g. `integration` is `role: aggregator` but v2
//!   emits `validate_integration` because the aggregator's
//!   concordance metrics need downstream validation). The skip rules
//!   mirror v2's `is_review || is_validation || EmpiricalRequired`,
//!   not the atom-role labels.
//! - The node has no output ports (an aggregator-style node with no
//!   inputs already has no observable outputs to validate).
//! - A `validate_<id>` already exists in the DAG (idempotent).
//! - The node's id starts with `adapter_` or contains `_adapter_`
//!   (lossless port adapters — they don't produce SME-relevant
//!   results, and v2 doesn't emit companions for them either).
//!
//! # Idempotency
//!
//! Running the post-pass on a DAG that already carries companions is
//! a no-op (the `dag.nodes.iter().any(...)` guard makes both the
//! companion check and the duplicate-edge check cheap).
//!
//! # Determinism
//!
//! - We iterate the original node ids in their existing slice order
//!   (the planner's earlier passes already sort by `id`), so the
//!   added nodes follow a stable order.
//! - After insertion we sort the full `nodes` and `edges` lists by
//!   their canonical keys (id for nodes; `(from, to, port)` for
//!   edges) — same discipline as `meet_in_middle.rs`.

use std::collections::BTreeSet;

use crate::atom_registry::AtomRegistry;
use crate::compatibility::engine::DeterministicCompatibilityEngine;
use crate::composer_v4::planner::pick_best_port_pair;
use crate::workflow_contracts::edge::{EdgeContract, EdgeKind};
use crate::workflow_contracts::evidence::{
    Assumption, AssumptionResolution, AssumptionSource, RiskClass,
};
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

/// Walk every result-producing node in `dag` and add a downstream
/// `validate_<id>` companion if one isn't already present. Mirrors
/// the v2 builder's post-stage validate-task synthesis. See module
/// docs for skip rules and determinism guarantees.
pub fn synthesize_validate_companions(dag: &mut WorkflowDag, atom_reg: &AtomRegistry) {
    // Snapshot the existing node ids so we can iterate without
    // borrowing `dag.nodes` while mutating.
    let existing_ids: BTreeSet<String> = dag.nodes.iter().map(|n| n.id.clone()).collect();

    let mut new_nodes: Vec<TaskNode> = Vec::new();
    let mut new_edges: Vec<EdgeContract> = Vec::new();
    let mut new_assumptions: Vec<Assumption> = Vec::new();

    // Walk the existing nodes (snapshot of the slice at entry) and
    // synthesize a companion for each that passes the skip rules.
    let originals: Vec<TaskNode> = dag.nodes.clone();
    for node in &originals {
        if !is_eligible_for_validate_companion(node) {
            continue;
        }
        let validate_id = format!("validate_{}", node.id);
        if existing_ids.contains(&validate_id) {
            continue; // already present — idempotent
        }
        // Build the companion node. Prefer an authored
        // `validate_<id>` atom when one exists in the registry (e.g.
        // `validate_sample_alignment_n_way` for cross-omics), so the
        // companion picks up the atom's typed ports / claim_boundary
        // / validators. Otherwise synthesize a minimal validator node
        // that the lowering pass will route to `TaskKind::Validation`
        // via the role attribute.
        let mut validator_node = match atom_reg.get(&validate_id) {
            Some(atom) => TaskNode::from_atom(atom),
            None => synthesize_validator_node(&validate_id, &node.id, &node.intent),
        };
        // Stamp the atom-id back-reference so `lower_to_workflow_json`
        // can populate `Task.source_atom_id` for per-task image
        // selection, safety enforcement, and plot-affordance lookup.
        validator_node.attributes.insert(
            "atom_id".into(),
            serde_json::Value::String(validate_id.clone()),
        );

        // A validator legitimately needs the same broad read scope as
        // the thing it validates — e.g. `validate_final_reporting`
        // independently cross-checks `final_reporting`'s aggregated
        // numbers against the same upstream stage outputs
        // `final_reporting` itself reads (RCA I-1). This holds for EVERY
        // validator, not only those whose stage declared a broad scope: a
        // validator re-reads the upstream outputs its stage consumed to
        // verify the transformation (e.g. `validate_normalisation`
        // re-reads `data_acquisition`'s counts), and those re-reads are
        // NOT declared edges of the validator — even though the validated
        // stage itself needs no allowance because ITS upstream reads ARE
        // declared edges. So unless the validator already carries its own
        // (an authored atom's own facet wins), grant it the scope: prefer
        // the validated stage's declared allowance (preserving its
        // rationale), else synthesize the default `AnyUpstreamStage`.
        // Without this, the validator's cross-stage re-reads surface as
        // genuine observed-read divergences and block the deposit (§G-B2).
        if !validator_node.attributes.contains_key("read_allowance") {
            let allowance = node
                .attributes
                .get("read_allowance")
                .cloned()
                .unwrap_or_else(|| {
                    serde_json::to_value(vec![crate::atom::ReadAllowance {
                        scope: crate::atom::ReadAllowanceScope::AnyUpstreamStage,
                        rationale: format!(
                            "Synthesized validator independently cross-checks {}'s outputs \
                             against the upstream stage outputs it consumed; re-reading any \
                             upstream stage is intrinsic to validation.",
                            node.id
                        ),
                    }])
                    .unwrap_or(serde_json::Value::Null)
                });
            validator_node
                .attributes
                .insert("read_allowance".into(), allowance);
        }

        // Wire the validator as a downstream consumer of the producer
        // node. We use the producer's primary output port (or empty
        // when none) and the validator's primary input port (or empty
        // when none) as the port names — `lower_to_workflow_json` only
        // reads `from_node` / `to_node` for `depends_on`, so the port
        // strings are mostly diagnostic.
        // WG3 strict-mode: a synthesized validator has no authored input
        // ports, so give it one that consumes the producer's output (the
        // artifact it validates). Registry `validate_*` atoms keep their
        // authored inputs. The producer-output / validator-input pair is
        // then proven through the compatibility engine so the edge lands
        // as TypedDataFlow / AdapterMediated — accepted under
        // RiskMode::Production — instead of a hardcoded OrderingOnly.
        if validator_node.inputs.is_empty() {
            if let Some(out) = node.outputs.first() {
                let mut validated_in = out.clone();
                validated_in.name = "validated_artifact".into();
                validator_node.inputs = vec![validated_in];
            }
        }
        let engine = DeterministicCompatibilityEngine::new();
        let (out_port, in_port, proof, kind) = pick_best_port_pair(
            &engine,
            &node.outputs,
            &validator_node.inputs,
            // The validate companion is an author-intended downstream
            // (v2-parity post-pass); `declared_ordering` preserves a clean
            // OrderingOnly fallback only if a registry validator's input
            // genuinely cannot consume the producer's output.
            true,
        );
        let from_iri = proof.producer_type.clone();
        let to_iri = proof.consumer_type.clone();
        new_edges.push(EdgeContract {
            from_node: node.id.clone(),
            from_port: out_port.name.clone(),
            to_node: validate_id.clone(),
            to_port: in_port.name.clone(),
            proof,
            kind,
            chain_of_custody: None,
            mutually_exclusive_group: None,
        });
        // Record an OntologyAdapterInserted assumption when the companion
        // bridges different semantic types. This surfaces in the ledger so
        // the SME review surface shows which validate nodes bridge IRIs.
        //
        // Resolution is `Accepted` — not `Unresolved` — because the
        // validate companion is a deterministic structural insertion made
        // by the v2-parity post-pass, not a pending SME decision. Leaving
        // it `Unresolved` would cause `score_dag` to count it, degrading
        // any DAG that has validate companions from `ValidatedExecutableDag`
        // to `DraftDag` and blocking intake from completing.
        new_assumptions.push(Assumption {
            id: format!("ontology_adapter_inserted:{}", validate_id),
            statement: format!(
                "Synthesized validate companion {} wires producer type '{}' to \
                 consumer type '{}' via the v2-parity post-pass.",
                validate_id, from_iri, to_iri
            ),
            source: AssumptionSource::OntologyAdapterInserted {
                from_iri: from_iri.clone(),
                to_iri: to_iri.clone(),
                reason: format!(
                    "validate_companion synthesis: '{validate_id}' inserted downstream of '{}'",
                    node.id
                ),
            },
            affects_nodes: vec![node.id.clone(), validate_id],
            risk: RiskClass::Low,
            resolution: AssumptionResolution::Accepted {
                rationale: "System-inserted validate companion; no SME decision required.".into(),
            },
            chain_of_custody: None,
        });
        new_nodes.push(validator_node);
    }

    if new_nodes.is_empty() && new_edges.is_empty() {
        return;
    }

    dag.nodes.extend(new_nodes);
    dag.edges.extend(new_edges);
    dag.assumptions.entries.extend(new_assumptions);

    // Re-sort to keep WorkflowDag byte-stable. Same sort keys as
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

/// Skip-rule predicate. Mirrors the v2 builder's
/// `let skip_validate = is_review || is_validation || matches!(stage.discovery, EmpiricalRequired)`
/// check, plus the adapter-id heuristic the v4 scorer already uses.
fn is_eligible_for_validate_companion(node: &TaskNode) -> bool {
    // Self-validation rule — `validate_*` ids never get a companion.
    if node.id.starts_with("validate_") {
        return false;
    }
    // Discovery atoms (`discover_*`) are self-describing per the
    // builder taxonomy convention; v2's post-pass also skips them.
    if node.id.starts_with("discover_") {
        return false;
    }
    // Adapter atoms — lossless port lifters; v2 doesn't synthesize
    // companions for them either.
    if node.id.starts_with("adapter_") || node.id.contains("_adapter_") {
        return false;
    }

    // Builtin nodes (e.g. the deterministic `assemble_report_data` core
    // assembler, marked with a `builtin` attribute) run in-process, not via an
    // agent. A validate companion would spawn an AGENT task to check a
    // deterministic core builtin — a role-separation smell — so skip them.
    if node.attributes.contains_key("builtin") {
        return false;
    }

    // Role-based skip. The atom's role is preserved in
    // `attributes["role"]` (see `from_atom.rs::preserve_attributes`).
    //
    // Only Validation and Selection roles get
    // skipped. The v2 builder's `emit_stage` post-pass also emits
    // validate companions for Aggregator atoms (e.g. `integration` is
    // role: aggregator but v2 still emits `validate_integration`),
    // because aggregator outputs have observable concordance metrics
    // that need the same downstream validation a regular operation
    // gets. Aligning v4's skip rules with v2 brings scrnaseq from
    // GAPS (1 missing) to GREEN.
    //
    // - Validation: a validator already; v2 skips via `is_validation`.
    // - Selection: the SME picks among alternatives; the validator
    // would be redundant (and v2 routes selection through review
    // tasks, not validate tasks).
    if let Some(role_value) = node.attributes.get("role") {
        if let Ok(role) = serde_json::from_value::<crate::atom::AtomRole>(role_value.clone()) {
            if role.is_validation() || role.is_selection() {
                return false;
            }
        }
    }

    // Nodes with no output ports have nothing observable to validate.
    if node.outputs.is_empty() {
        return false;
    }

    true
}

/// Build a minimal validator `TaskNode` when no authored
/// `validate_<id>` atom exists in the registry. The lowering pass
/// reads `attributes["role"] = "validation"` and produces a
/// `TaskKind::Validation` task; the `intent` string surfaces in the
/// `description` field.
fn synthesize_validator_node(validate_id: &str, target_id: &str, target_intent: &str) -> TaskNode {
    let mut node = TaskNode::skeleton(validate_id, format!("Validate outputs of: {target_intent}"));
    // Stamp the role + assignee + stage_id attributes so the
    // lowering pass produces a `Task { kind: TaskKind::Validation,
    // assignee: Agent }` exactly like the v2 builder's synthesized
    // validator task.
    node.attributes.insert(
        "role".into(),
        serde_json::to_value(crate::atom::AtomRole::Validation).unwrap_or(serde_json::Value::Null),
    );
    node.attributes.insert(
        "assignee".into(),
        serde_json::to_value(crate::atom::AtomAssignee::Agent).unwrap_or(serde_json::Value::Null),
    );
    // Record the target id so any downstream tooling that wants to
    // walk validators back to their producer can do so without
    // string-stripping the prefix.
    node.attributes.insert(
        "validate_target".into(),
        serde_json::Value::String(target_id.to_string()),
    );
    // The validator is a Validation atom; mark its lifecycle as
    // Production so the v4 scorer's `untrusted_node_count` doesn't
    // penalize the synthesized companions (they're a v2-parity
    // requirement, not a speculative addition).
    node.lifecycle_state = crate::workflow_contracts::lifecycle::LifecycleState::Production;
    node
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::evidence::AssumptionLedger;
    use crate::workflow_contracts::port::PortContract;

    fn dummy_dag_with_node(id: &str, role: crate::atom::AtomRole) -> WorkflowDag {
        let mut node = TaskNode::skeleton(id, format!("intent for {id}"));
        node.outputs = vec![PortContract::from_edam(
            "out",
            Some("data:0006"),
            Some("format:1915"),
        )];
        node.attributes.insert(
            "role".into(),
            serde_json::to_value(role).unwrap_or(serde_json::Value::Null),
        );
        WorkflowDag {
            id: "test".into(),
            nodes: vec![node],
            edges: Vec::new(),
            assumptions: AssumptionLedger::default(),
            source_template: None,
        }
    }

    /// Operation atoms get a companion.
    #[test]
    fn synthesize_adds_validator_for_operation_atom() {
        let mut dag =
            dummy_dag_with_node("differential_expression", crate::atom::AtomRole::Operation);
        let reg = AtomRegistry::default();
        synthesize_validate_companions(&mut dag, &reg);

        let ids: BTreeSet<String> = dag.nodes.iter().map(|n| n.id.clone()).collect();
        assert!(
            ids.contains("validate_differential_expression"),
            "missing validator for operation atom; got {ids:?}"
        );
        assert_eq!(
            dag.edges.len(),
            1,
            "expected one synthesized edge; got {:?}",
            dag.edges
        );
        assert_eq!(dag.edges[0].from_node, "differential_expression");
        assert_eq!(dag.edges[0].to_node, "validate_differential_expression");
    }

    /// Regression (§G-B2 deposit gate): a synthesized validator MUST carry an
    /// `AnyUpstreamStage` read-allowance even when the validated stage declares
    /// NONE. A validator re-reads the upstream outputs its stage consumed to
    /// cross-check the transformation (e.g. `validate_normalisation` re-reads
    /// `data_acquisition`'s counts), and those re-reads are not declared edges of
    /// the validator — while the stage itself needs no allowance (its upstream
    /// reads ARE declared edges). Without the validator's allowance those
    /// re-reads are recorded as GENUINE observed-read divergences and flip
    /// `deposit_ready` to false.
    #[test]
    fn synthesized_validator_gets_upstream_read_allowance_when_stage_declares_none() {
        let mut dag = dummy_dag_with_node("normalisation", crate::atom::AtomRole::Operation);
        // Precondition: the validated stage declares NO read_allowance.
        assert!(
            !dag.nodes[0].attributes.contains_key("read_allowance"),
            "precondition: validated stage must declare no allowance to inherit"
        );

        let reg = AtomRegistry::default();
        synthesize_validate_companions(&mut dag, &reg);

        let validator = dag
            .nodes
            .iter()
            .find(|n| n.id == "validate_normalisation")
            .expect("validator must be synthesized");
        let raw = validator
            .attributes
            .get("read_allowance")
            .expect("validator must carry a read_allowance even though its stage declares none");
        let parsed: Vec<crate::atom::ReadAllowance> =
            serde_json::from_value(raw.clone()).expect("read_allowance deserializes");
        assert!(
            parsed
                .iter()
                .any(|a| a.scope == crate::atom::ReadAllowanceScope::AnyUpstreamStage),
            "validator allowance must include AnyUpstreamStage: {parsed:?}"
        );
        assert!(
            parsed.iter().all(|a| !a.rationale.is_empty()),
            "each sanctioned-read allowance must carry a rationale"
        );
    }

    /// Validation atoms do NOT get a companion.
    #[test]
    fn synthesize_skips_validator_atoms() {
        let mut dag = dummy_dag_with_node("validate_qc", crate::atom::AtomRole::Validation);
        let reg = AtomRegistry::default();
        synthesize_validate_companions(&mut dag, &reg);

        // Either the role-skip or the prefix-skip must fire; both
        // should keep the DAG single-node.
        assert_eq!(dag.nodes.len(), 1, "validator received a self-companion");
    }

    /// Discovery atoms do NOT get a companion.
    #[test]
    fn synthesize_skips_discovery_atoms() {
        let mut dag = dummy_dag_with_node("discover_alignment", crate::atom::AtomRole::Discovery);
        let reg = AtomRegistry::default();
        synthesize_validate_companions(&mut dag, &reg);
        assert_eq!(dag.nodes.len(), 1, "discovery atom received a companion");
    }

    /// Adapter atoms do NOT get a companion.
    #[test]
    fn synthesize_skips_adapter_atoms() {
        let mut dag = dummy_dag_with_node("adapter_count_matrix", crate::atom::AtomRole::Operation);
        let reg = AtomRegistry::default();
        synthesize_validate_companions(&mut dag, &reg);
        assert_eq!(dag.nodes.len(), 1, "adapter received a companion");
    }

    /// Builtin nodes do NOT get a companion. The deterministic
    /// `assemble_report_data` core builtin (marked with a `builtin`
    /// attribute) runs in-process; an agent validate companion checking a
    /// deterministic core builtin is a role-separation smell.
    #[test]
    fn synthesize_skips_builtin_nodes() {
        let mut dag = dummy_dag_with_node("assemble_report_data", crate::atom::AtomRole::Operation);
        dag.nodes[0].attributes.insert(
            "builtin".into(),
            serde_json::Value::String("assemble_report_data".into()),
        );
        let reg = AtomRegistry::default();
        synthesize_validate_companions(&mut dag, &reg);
        let ids: BTreeSet<String> = dag.nodes.iter().map(|n| n.id.clone()).collect();
        assert!(
            !ids.contains("validate_assemble_report_data"),
            "a builtin node must not receive an agent validate companion; got {ids:?}"
        );
        assert_eq!(dag.nodes.len(), 1, "builtin node received a companion");
    }

    /// Idempotency: running twice doesn't duplicate.
    #[test]
    fn synthesize_is_idempotent() {
        let mut dag =
            dummy_dag_with_node("differential_expression", crate::atom::AtomRole::Operation);
        let reg = AtomRegistry::default();
        synthesize_validate_companions(&mut dag, &reg);
        let after_first_n = dag.nodes.len();
        let after_first_e = dag.edges.len();
        synthesize_validate_companions(&mut dag, &reg);
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

    /// Nodes without output ports get no companion (no observable
    /// outputs to validate).
    #[test]
    fn synthesize_skips_zero_output_atoms() {
        let mut node = TaskNode::skeleton("aggregator_node", "no outputs");
        node.attributes.insert(
            "role".into(),
            serde_json::to_value(crate::atom::AtomRole::Operation)
                .unwrap_or(serde_json::Value::Null),
        );
        // Note: outputs left empty.
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![node],
            edges: Vec::new(),
            assumptions: AssumptionLedger::default(),
            source_template: None,
        };
        let reg = AtomRegistry::default();
        synthesize_validate_companions(&mut dag, &reg);
        assert_eq!(
            dag.nodes.len(),
            1,
            "zero-output atom received a companion despite having no observable outputs"
        );
    }

    /// WS-2 — the injected biological_interpretation node is an
    /// Operation atom with an output port, so it IS eligible for a
    /// validate companion. This guards against the interpretation node
    /// silently slipping past validation (it produces SME-facing claims
    /// that need the same downstream check a regular operation gets).
    #[test]
    fn biological_interpretation_gets_validate_companion() {
        let mut dag = dummy_dag_with_node(
            "biological_interpretation",
            crate::atom::AtomRole::Operation,
        );
        let reg = AtomRegistry::default();
        synthesize_validate_companions(&mut dag, &reg);
        let ids: BTreeSet<String> = dag.nodes.iter().map(|n| n.id.clone()).collect();
        assert!(
            ids.contains("validate_biological_interpretation"),
            "interpretation node must receive a validate companion; got {ids:?}"
        );
    }

    /// Workstream-B (B1) atom-contract guard: the contextualize atom's
    /// `claim_boundary` must forbid hardcoded/from-memory Ensembl IDs and mandate
    /// annotation/adapter-based resolution. Reads the YAML file directly (the
    /// committed source the emitter copies verbatim) and parses it with the
    /// crate's YAML dep so the assertion tracks the on-disk contract, not a Rust
    /// constant. Guards the root-cause fix for the wrong-gene hallucination
    /// (CRISPLD2 bound to the ACSL5 Ensembl id by a hardcoded map).
    #[test]
    fn contextualize_atom_claim_boundary_forbids_hardcoded_ensembl_ids() {
        // Test CWD is the crate dir (crates/core); the committed atom lives at
        // the repo root under config/stage-atoms/.
        let yaml_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/stage-atoms/contextualize_findings_with_literature.yaml");
        let raw = std::fs::read_to_string(&yaml_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", yaml_path.display()));
        // Parse with the crate's YAML dep so the file is confirmed to still parse
        // (B1 "verify the YAML still parses by reading it back").
        let doc: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&raw).expect("contextualize atom YAML must parse");
        let cb = doc
            .get("claim_boundary")
            .and_then(|v| v.as_str())
            .expect("contextualize atom must declare a claim_boundary block");
        let cb_l = cb.to_ascii_lowercase();
        assert!(
            cb_l.contains("ensembl"),
            "claim_boundary must reference Ensembl IDs"
        );
        assert!(
            cb_l.contains("must not hardcode") || cb_l.contains("do not hardcode"),
            "claim_boundary must forbid hardcoded Ensembl IDs (a 'must not hardcode' / 'do not hardcode' clause)"
        );
        assert!(
            cb_l.contains("annotation")
                || cb_l.contains("org.hs.eg.db")
                || cb_l.contains("adapter"),
            "claim_boundary must mandate annotation/adapter-based resolution"
        );
    }

    /// WG3 strict-mode: the validate-companion edge must be engine-proven
    /// (`TypedDataFlow`), not a hardcoded `OrderingOnly`. The synthesized
    /// validator consumes the producer's output, so the producer-output /
    /// validator-input pair unifies losslessly — and Production/strict
    /// compose then accepts the edge instead of rejecting it.
    #[test]
    fn synthesize_validate_companion_edge_is_proven_not_ordering_only() {
        let mut dag =
            dummy_dag_with_node("differential_expression", crate::atom::AtomRole::Operation);
        let reg = AtomRegistry::default();
        synthesize_validate_companions(&mut dag, &reg);
        let edge = dag
            .edges
            .iter()
            .find(|e| e.to_node == "validate_differential_expression")
            .expect("validate companion edge present");
        assert_ne!(
            edge.kind,
            EdgeKind::OrderingOnly,
            "validate-companion edge must be engine-proven, not OrderingOnly (WG3 strict-mode)"
        );
        assert_eq!(
            edge.kind,
            EdgeKind::TypedDataFlow,
            "producer output and validator input share a type → lossless TypedDataFlow"
        );
    }
}
