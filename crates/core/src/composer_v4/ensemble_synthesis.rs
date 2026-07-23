//! Fan out the schema-bearing statistical node into K method variants
//! plus a statistical-distribution aggregator stub on a v4
//! [`WorkflowDag`].
//!
//! Gated by `ctx.compose_ensemble` (`ECAA_COMPOSE_ENSEMBLE`, default
//! off) — mirrors the shape of `interpretation_synthesis.rs` and
//! `report_data_synthesis.rs`: a self-contained post-pass invoked from
//! `planner::plan` once a roster is available for the composed
//! modality, no-op otherwise.
//!
//! # Method-neutrality reconciliation
//!
//! The rest of this codebase forbids the LLM from ever selecting a
//! methodological choice (`prompt_role.txt`; `set_intake_method` is
//! allowed only when the SME names a method unprompted). This pass does
//! not violate that contract: the composer — not the LLM — instantiates
//! a config-declared method multiverse. Every variant's tool comes from
//! the modality's shipped `config/ensemble-rosters/<modality>.yaml`
//! (validated against the base atom's `candidate_tools` by
//! `ensemble_roster::validate_variant_tools`), so no runtime inference
//! chooses a method; the roster is authored and reviewed ahead of time,
//! same as an atom or archetype YAML.
//!
//! # Shape
//!
//! The pass collects every node whose atom (resolved by id, or by its
//! `attributes["atom_id"]` back-reference) declares a `result_schema`
//! (the same fan-out-target selector `report_data_synthesis` uses),
//! sorts them, and fans out ONLY the primary — the sorted-first
//! schema-bearing target, the same node the interpretation grid centers
//! on. Any additional schema-bearing targets (e.g. `pathway_enrichment`
//! alongside `differential_expression`) are left untouched — their
//! roster of statistical methods is disjoint from the primary's, so
//! fanning them over the primary's variants would be nonsense.
//!
//! For the primary:
//!
//! - Replace it with one `<primary>__v_<variant.id>` clone per
//!   `roster.statistical_variants` entry. Each clone keeps
//!   `attributes["atom_id"] = <primary>` (so schema/safety resolution
//!   still finds the underlying atom), stamps a UNIQUE
//!   `attributes["stage_id"] = <clone id>` (the lifted base carries a
//!   `stage_id` attr and a bare clone would inherit it, collapsing all K
//!   variants onto one stage_id and tripping `validate_composition`'s
//!   acyclicity check), and stamps `attributes["ensemble_variant"] =
//!   { axis: "statistical", method, variant_id, bootstrap_replicates }`.
//! - Rewire every inbound edge that fed the primary onto EACH variant
//!   (fan-out on the input side).
//! - Wire every variant to the `assemble_statistical_distribution`
//!   builtin aggregator (fan-in on the output side; Plan 3 fills in the
//!   aggregation logic — this pass only stubs the node + edges).
//!
//! The base `contextualize_findings_with_literature` node (when present)
//! is likewise fanned into `__v_<lens>` lens variants and its base node
//! removed. For every removed base (the primary and the base
//! contextualize) the stale `validate_<base>` companion the pre-ensemble
//! pass synthesized is removed too.
//!
//! Edge cleanup when the primary is removed: an outbound edge to a
//! `reporting` / `final_reporting` terminal is DROPPED (the cross-axis
//! ensemble aggregator now feeds those terminals — see below); an
//! outbound edge to a node that is itself being removed (the base
//! contextualize) is DROPPED (its lens variants read the statistical
//! aggregator instead); any other analytical consumer (e.g.
//! `pathway_enrichment` reading the primary's result) is REDIRECTED onto
//! the statistical aggregator so no surviving edge cites the removed
//! primary.
//!
//! Reporting wiring: this pass wires the cross-axis ensemble aggregator
//! (`assemble_ensemble_distribution`) directly to every `reporting` /
//! `final_reporting` terminal present in the DAG, mirroring
//! `report_data_synthesis`'s `REPORTING_TERMINAL_IDS` wiring — the
//! ensemble grid would otherwise be a dead-end sink.
//!
//! # Skip rule
//!
//! No-op when no node in the DAG resolves to an atom with a declared
//! `result_schema` — mirrors `report_data_synthesis`'s reduced
//! contract for a workflow with no tabular analytical result.
//!
//! # Determinism + idempotency
//!
//! - A node id `"assemble_statistical_distribution"` already present in
//!   the DAG is the idempotency guard — re-running the pass is a no-op.
//! - Fan-out targets are collected then sorted by id before iteration,
//!   so variant/edge generation order never depends on `dag.nodes`'
//!   incoming order.
//! - After appending the new nodes + edges and removing the base
//!   targets, `dag.nodes` and `dag.edges` are re-sorted by the same
//!   canonical keys the sibling synthesis passes use (id for nodes;
//!   `(from_node, from_port, to_node, to_port)` for edges), so the
//!   emitted DAG stays byte-stable.

use crate::atom_registry::AtomRegistry;
use crate::ensemble_roster::EnsembleRoster;
use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
use crate::workflow_contracts::port::PortContract;
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

/// The synthesized statistical-aggregator node id (Plan 3 fills the
/// aggregation logic; here it is a `builtin`-marked stub).
pub const ENSEMBLE_STAT_AGGREGATOR_ID: &str = "assemble_statistical_distribution";

/// The synthesized cross-axis ensemble-aggregator node id: the fan-in
/// over the K×M interpretation grid (`builtin`-marked stub; Plan 3 fills
/// the aggregation logic).
pub const ENSEMBLE_AGGREGATOR_ID: &str = "assemble_ensemble_distribution";

/// Reporting-terminal ids the cross-axis aggregator feeds when present.
/// Exact-id match only — mirrors `report_data_synthesis`'s constant of
/// the same name (the two canonical terminal ids, no alias matching).
const REPORTING_TERMINAL_IDS: [&str; 2] = ["reporting", "final_reporting"];

/// The base contextualization node id fanned into per-lens variants and
/// removed by this pass. Kept as a named constant so the fan-out, the
/// base-node removal, and the stale-companion removal cannot drift.
const CONTEXTUALIZE_BASE_ID: &str = "contextualize_findings_with_literature";

/// Fan the schema-bearing statistical node(s) out over the roster's
/// method variants, and inject the statistical-distribution aggregator
/// stub. No-op when: no schema-bearing node exists, or the pass has
/// already run (idempotent). See module docs.
pub fn synthesize_ensemble_fanout(
    dag: &mut WorkflowDag,
    atom_reg: &AtomRegistry,
    roster: &EnsembleRoster,
) {
    // Idempotency guard.
    if dag
        .nodes
        .iter()
        .any(|n| n.id == ENSEMBLE_STAT_AGGREGATOR_ID)
    {
        return;
    }

    // Fan-out targets: schema-bearing analytical nodes (verbatim
    // selector from `report_data_synthesis.rs`).
    let mut targets: Vec<String> = Vec::new();
    for node in &dag.nodes {
        let atom = atom_reg.get(&node.id).or_else(|| {
            node.attributes
                .get("atom_id")
                .and_then(|v| v.as_str())
                .and_then(|id| atom_reg.get(id))
        });
        if atom.map(|a| a.result_schema.is_some()).unwrap_or(false) {
            targets.push(node.id.clone());
        }
    }
    targets.sort();
    if targets.is_empty() {
        return;
    }

    let mut new_nodes: Vec<TaskNode> = Vec::new();
    let mut new_edges: Vec<EdgeContract> = Vec::new();

    // Statistical aggregator stub (builtin -> skipped by validate-companion synthesis).
    let agg = builtin_aggregator(
        ENSEMBLE_STAT_AGGREGATOR_ID,
        "Aggregate cross-method statistical distribution (Plan 3 fills the logic)",
        "aggregates every method variant's result artifact off disk",
        "stat_distribution",
    );

    // Fan out ONLY the primary — the sorted-first schema-bearing target,
    // the same node the interpretation grid centers on. Any additional
    // schema-bearing targets (e.g. `pathway_enrichment`) keep their own
    // nodes: their statistical roster is disjoint from the primary's, so
    // fanning them over the primary's method variants would be nonsense.
    let primary = targets[0].clone();

    // Capture the primary's inbound edges (upstream -> primary) to rewire onto each variant.
    let inbound: Vec<EdgeContract> = dag
        .edges
        .iter()
        .filter(|e| e.to_node == primary)
        .cloned()
        .collect();
    let base_node = dag.nodes.iter().find(|n| n.id == primary).cloned();
    for sv in &roster.statistical_variants {
        let vid = format!("{primary}__v_{}", sv.id);
        let mut vn = base_node
            .clone()
            .unwrap_or_else(|| TaskNode::skeleton(&vid, "statistical variant"));
        vn.id = vid.clone();
        vn.human_name = vid.clone();
        vn.machine_name = vid.clone();
        // Keep atom_id pointing at the base atom (schema/safety inherited; resolver still works).
        vn.attributes
            .insert("atom_id".into(), serde_json::Value::String(primary.clone()));
        // Stamp a UNIQUE stage_id per variant. The lifted base carries
        // `attributes["stage_id"]`; a bare clone inherits it, so without
        // this every variant collapses onto one stage_id in
        // `lower_dag_to_composition_result` (which keys `ComposedAtom.stage_id`
        // off this attr, falling back to node.id only when absent) and
        // `validate_composition`'s acyclicity check reports CycleDetected.
        vn.attributes
            .insert("stage_id".into(), serde_json::Value::String(vid.clone()));
        vn.attributes.insert(
            "ensemble_variant".into(),
            serde_json::json!({
                "axis": "statistical",
                "method": sv.tool,
                "variant_id": sv.id,
                "bootstrap_replicates": sv.bootstrap_replicates,
            }),
        );
        // Upstream -> variant (rewire each inbound edge onto the variant).
        for e in &inbound {
            new_edges.push(ordering_edge(&e.from_node, &e.from_port, &vid, "input"));
        }
        // Variant -> statistical aggregator.
        new_edges.push(ordering_edge(
            &vid,
            "report",
            ENSEMBLE_STAT_AGGREGATOR_ID,
            "method_result",
        ));
        new_nodes.push(vn);
    }

    // ---- Contextualization fan-out (M) + K×M interpretation grid + ensemble aggregator ----
    //
    // The interpretation grid centers on the same `primary` statistical
    // base fanned out above.
    let base_id = &primary;

    // M contextualization variants — each reads the pooled statistical
    // distribution off the statistical aggregator.
    for lens in &roster.interpretive_lenses {
        let ctx_id = format!("contextualize_findings_with_literature__v_{}", lens.id);
        let mut cn = variant_node(
            atom_reg,
            "contextualize_findings_with_literature",
            &ctx_id,
            "contextualization variant",
        );
        cn.attributes.insert(
            "ensemble_variant".into(),
            serde_json::json!({
                "axis": "contextualization",
                "persona_ref": lens.persona_ref,
                "retrieval": lens.retrieval,
                "lens": lens.id,
            }),
        );
        new_edges.push(ordering_edge(
            ENSEMBLE_STAT_AGGREGATOR_ID,
            "stat_distribution",
            &ctx_id,
            "findings",
        ));
        new_nodes.push(cn);
    }

    // K×M interpretation grid (or the fractional balanced subset). Each
    // cell reads its method variant's result and its lens's literature
    // concordance, and feeds the cross-axis ensemble aggregator.
    for (k, m) in roster.selected_cells() {
        let method = &roster.statistical_variants[k];
        let lens = &roster.interpretive_lenses[m];
        let cell_id = format!(
            "biological_interpretation__m_{}__lens_{}",
            method.id, lens.id
        );
        let mut cell = variant_node(
            atom_reg,
            "biological_interpretation",
            &cell_id,
            "interpretation cell",
        );
        cell.attributes.insert(
            "ensemble_variant".into(),
            serde_json::json!({
                "axis": "interpretive",
                "method": method.tool,
                "method_variant": method.id,
                "persona_ref": lens.persona_ref,
                "model_tier": lens.model_tier,
                "lens": lens.id,
            }),
        );
        // Method-variant result -> cell.
        new_edges.push(ordering_edge(
            &format!("{base_id}__v_{}", method.id),
            "report",
            &cell_id,
            "method_result",
        ));
        // Lens contextualization -> cell.
        new_edges.push(ordering_edge(
            &format!("contextualize_findings_with_literature__v_{}", lens.id),
            "findings",
            &cell_id,
            "literature_concordance",
        ));
        // Cell -> cross-axis ensemble aggregator.
        new_edges.push(ordering_edge(
            &cell_id,
            "interpretation",
            ENSEMBLE_AGGREGATOR_ID,
            "cell_result",
        ));
        new_nodes.push(cell);
    }

    // Cross-axis ensemble aggregator (builtin -> skipped by validate-companion synthesis).
    new_nodes.push(builtin_aggregator(
        ENSEMBLE_AGGREGATOR_ID,
        "Aggregate cross-axis ensemble distribution over the K×M interpretation grid",
        "aggregates every interpretation cell's result artifact off disk",
        "ensemble_distribution",
    ));

    // Wire the cross-axis aggregator onward to the reporting terminals it
    // supersedes. The base primary's direct reporting feed is dropped
    // below, so without this the whole ensemble grid is a dead-end sink.
    // Mirrors report_data_synthesis's REPORTING_TERMINAL_IDS wiring.
    for terminal in REPORTING_TERMINAL_IDS {
        if dag.nodes.iter().any(|n| n.id == terminal) {
            new_edges.push(ordering_edge(
                ENSEMBLE_AGGREGATOR_ID,
                "ensemble_distribution",
                terminal,
                "tributaries",
            ));
        }
    }

    // Base nodes the ensemble expansion supersedes: the primary
    // statistical target (fanned into K method variants) and — when
    // present — the base contextualize node (fanned into M lens
    // variants). Their stale `validate_<base>` companions, synthesized by
    // the pre-ensemble pass for a node that no longer exists, go with
    // them.
    let mut removed_ids: Vec<String> = vec![primary.clone()];
    if dag.nodes.iter().any(|n| n.id == CONTEXTUALIZE_BASE_ID) {
        removed_ids.push(CONTEXTUALIZE_BASE_ID.to_string());
    }
    for base in removed_ids.clone() {
        let companion = format!("validate_{base}");
        if dag.nodes.iter().any(|n| n.id == companion) {
            removed_ids.push(companion);
        }
    }

    // Re-home the primary's outbound consumers before dropping it. A
    // reporting-terminal consumer is dropped (the cross-axis aggregator
    // now feeds reporting, above); a consumer that is itself being removed
    // (the base contextualize) is dropped (its lens variants read the
    // statistical aggregator instead); any other analytical consumer
    // (e.g. `pathway_enrichment` reading the primary's result) is
    // redirected onto the statistical aggregator so no surviving edge
    // cites the removed primary (keeps the original to_node + ports).
    for e in &dag.edges {
        if e.from_node != primary {
            continue;
        }
        if REPORTING_TERMINAL_IDS.contains(&e.to_node.as_str()) || removed_ids.contains(&e.to_node)
        {
            continue;
        }
        new_edges.push(ordering_edge(
            ENSEMBLE_STAT_AGGREGATOR_ID,
            &e.from_port,
            &e.to_node,
            &e.to_port,
        ));
    }

    // Remove the superseded base nodes + their stale validate companions,
    // and every original edge that still touches one of them. Inbound
    // edges to the primary were rewired onto the variants above; its
    // outbound edges were re-homed or dropped just above.
    dag.nodes.retain(|n| !removed_ids.contains(&n.id));
    dag.edges
        .retain(|e| !removed_ids.contains(&e.from_node) && !removed_ids.contains(&e.to_node));

    dag.nodes.push(agg);
    dag.nodes.extend(new_nodes);
    dag.edges.extend(new_edges);

    resort(dag);
}

/// Validated entry point for the ensemble pass — Plan-1 safety handoff
/// N1. Runs every Plan-1 validator ([`crate::ensemble_roster::EnsembleRoster::validate_caps`],
/// [`crate::ensemble_roster::validate_variant_tools`] against the resolved
/// base atom, and [`crate::ensemble_roster::lint_persona_text`] over every
/// interpretive lens's persona file) BEFORE touching `dag`. If any
/// validator fails, `dag` is returned unmutated and the error is
/// propagated — the honest-lens safety property only holds if these
/// checks run before expansion, not after.
///
/// The four-conditions emission rule (see `CLAUDE.md`) means this
/// function must never itself block emission: on `Err`, the caller (the
/// planner) logs and degrades to the normal non-ensemble DAG — it does
/// not abort emit.
///
/// When no schema-bearing fan-out target exists in `dag`, the core pass
/// would no-op anyway, so this returns `Ok(())` without resolving a base
/// atom or running `validate_variant_tools`.
pub fn synthesize_ensemble_fanout_validated(
    dag: &mut WorkflowDag,
    atom_reg: &AtomRegistry,
    roster: &EnsembleRoster,
    persona_dir: &std::path::Path,
) -> Result<(), String> {
    roster.validate_caps()?;

    // Resolve the first (sorted) schema-bearing fan-out target's base atom
    // — same selector `synthesize_ensemble_fanout` uses. No target means
    // the core pass would no-op, so there is nothing to validate against.
    let mut target_ids: Vec<&str> = Vec::new();
    for node in &dag.nodes {
        let atom = atom_reg.get(&node.id).or_else(|| {
            node.attributes
                .get("atom_id")
                .and_then(|v| v.as_str())
                .and_then(|id| atom_reg.get(id))
        });
        if atom.map(|a| a.result_schema.is_some()).unwrap_or(false) {
            target_ids.push(node.id.as_str());
        }
    }
    target_ids.sort_unstable();
    if let Some(&base_id) = target_ids.first() {
        let base_atom = atom_reg
            .get(base_id)
            .or_else(|| {
                dag.nodes
                    .iter()
                    .find(|n| n.id == base_id)
                    .and_then(|n| n.attributes.get("atom_id"))
                    .and_then(|v| v.as_str())
                    .and_then(|id| atom_reg.get(id))
            })
            .ok_or_else(|| format!("ensemble validation: base atom '{base_id}' not resolvable"))?;
        crate::ensemble_roster::validate_variant_tools(roster, base_atom)?;
    } else {
        return Ok(());
    }

    for lens in &roster.interpretive_lenses {
        let path = persona_dir.join(&lens.persona_ref);
        let text = std::fs::read_to_string(&path)
            .map_err(|_| format!("persona file missing: {}", path.display()))?;
        crate::ensemble_roster::lint_persona_text(&lens.id, &text)?;
    }

    synthesize_ensemble_fanout(dag, atom_reg, roster);
    Ok(())
}

/// Build an `OrderingOnly` edge; the port strings are diagnostic — the
/// lowering pass only reads `from_node`/`to_node` for `depends_on`.
/// Mirrors `report_data_synthesis.rs::ordering_edge` /
/// `interpretation_synthesis.rs::ordering_edge` exactly.
fn ordering_edge(from: &str, from_port: &str, to: &str, to_port: &str) -> EdgeContract {
    EdgeContract {
        from_node: from.into(),
        from_port: from_port.into(),
        to_node: to.into(),
        to_port: to_port.into(),
        proof: CompatibilityProof {
            rationale: Some(format!(
                "ensemble_synthesis: wired {from} -> {to} ({to_port})"
            )),
            ..Default::default()
        },
        kind: EdgeKind::OrderingOnly,
        chain_of_custody: None,
        mutually_exclusive_group: None,
    }
}

/// Build a `builtin`-marked aggregator stub: `AtomRole::Operation`,
/// `AnyUpstreamStage` read allowance (it reads every upstream variant's
/// result artifact off disk), `Production` lifecycle, and a single EDAM
/// output port. Shared by the statistical (`stat_distribution`) and the
/// cross-axis ensemble (`ensemble_distribution`) aggregators.
fn builtin_aggregator(id: &str, intent: &str, read_rationale: &str, output_name: &str) -> TaskNode {
    let mut agg = TaskNode::skeleton(id, intent);
    agg.attributes.insert(
        "role".into(),
        serde_json::to_value(crate::atom::AtomRole::Operation).unwrap_or(serde_json::Value::Null),
    );
    agg.attributes
        .insert("builtin".into(), serde_json::Value::String(id.into()));
    // Stamp stage_id = node id defensively so no future clone path can
    // regress a duplicate stage_id (see the statistical-variant loop).
    agg.attributes
        .insert("stage_id".into(), serde_json::Value::String(id.into()));
    agg.attributes.insert(
        "read_allowance".into(),
        serde_json::to_value(vec![crate::atom::ReadAllowance {
            scope: crate::atom::ReadAllowanceScope::AnyUpstreamStage,
            rationale: read_rationale.into(),
        }])
        .unwrap_or(serde_json::Value::Null),
    );
    agg.lifecycle_state = crate::workflow_contracts::lifecycle::LifecycleState::Production;
    agg.outputs = vec![PortContract::from_edam(
        output_name,
        Some("data:2048"),
        Some("format:3464"),
    )];
    agg
}

/// Build an ensemble variant node from its base atom (schema/safety
/// inherited) — falling back to a skeleton when the atom is absent — and
/// stamp its id/human_name/machine_name plus the `atom_id` back-reference
/// so schema/safety resolution still finds the underlying atom. The
/// caller stamps `ensemble_variant` and wires edges.
fn variant_node(
    atom_reg: &AtomRegistry,
    atom_id: &str,
    node_id: &str,
    skeleton_intent: &str,
) -> TaskNode {
    let mut n = match atom_reg.get(atom_id) {
        Some(atom) => TaskNode::from_atom(atom),
        None => TaskNode::skeleton(node_id, skeleton_intent),
    };
    n.id = node_id.to_string();
    n.human_name = node_id.to_string();
    n.machine_name = node_id.to_string();
    n.attributes
        .insert("atom_id".into(), serde_json::Value::String(atom_id.into()));
    // Stamp a UNIQUE stage_id = node id defensively — from_atom may carry
    // no stage_id attr, but a future lift/clone path could, and a
    // duplicate stage_id trips validate_composition's acyclicity check
    // (see the statistical-variant loop). Covers the contextualize lens
    // variants and the K×M interpretation cells.
    n.attributes
        .insert("stage_id".into(), serde_json::Value::String(node_id.into()));
    n
}

/// Re-sort nodes/edges by their canonical keys so the DAG stays
/// byte-stable regardless of iteration order. Same keys as the sibling
/// synthesis passes.
fn resort(dag: &mut WorkflowDag) {
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

    /// Real `differential_expression` node (declares `result_schema`)
    /// plus `reporting`/`final_reporting` terminals, fanned out over
    /// the shipped `bulk_rnaseq` roster's 3 statistical variants.
    #[test]
    fn statistical_fanout_creates_k_variants_and_aggregator() {
        use crate::atom_registry::AtomRegistry;
        use crate::ensemble_roster::EnsembleRosterProvider;
        use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        let cfg = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config"));
        let reg = AtomRegistry::load_from_dir(&cfg.join("stage-atoms")).unwrap();
        let roster = EnsembleRosterProvider::from_dir(&cfg.join("ensemble-rosters"))
            .roster_for("bulk_rnaseq")
            .cloned()
            .unwrap();
        let de = TaskNode::from_atom(reg.get("differential_expression").unwrap());
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                de,
                TaskNode::skeleton("reporting", "r"),
                TaskNode::skeleton("final_reporting", "f"),
            ],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };

        synthesize_ensemble_fanout(&mut dag, &reg, &roster);

        let variant_ids: Vec<&str> = dag
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .filter(|id| id.starts_with("differential_expression__v_"))
            .collect();
        assert_eq!(variant_ids.len(), 3, "K=3 variants: {variant_ids:?}");
        assert!(
            !dag.nodes.iter().any(|n| n.id == "differential_expression"),
            "base node replaced"
        );
        assert!(
            dag.nodes
                .iter()
                .any(|n| n.id == "assemble_statistical_distribution"
                    && n.attributes.contains_key("builtin")),
            "stat aggregator is builtin"
        );
        for v in &variant_ids {
            assert!(
                dag.edges
                    .iter()
                    .any(|e| e.from_node == **v && e.to_node == "assemble_statistical_distribution"),
                "{v} -> stat aggregator edge"
            );
            let node = dag.nodes.iter().find(|n| &n.id == v).unwrap();
            assert_eq!(
                node.attributes.get("atom_id").and_then(|x| x.as_str()),
                Some("differential_expression")
            );
            assert_eq!(
                node.attributes
                    .get("ensemble_variant")
                    .and_then(|x| x.get("axis"))
                    .and_then(|x| x.as_str()),
                Some("statistical")
            );
        }

        let n0 = dag.nodes.len();
        let e0 = dag.edges.len();
        synthesize_ensemble_fanout(&mut dag, &reg, &roster);
        assert_eq!(dag.nodes.len(), n0, "idempotent nodes");
        assert_eq!(dag.edges.len(), e0, "idempotent edges");
    }

    /// Full K×M grid over the shipped `bulk_rnaseq` roster (K=3, M=3):
    /// 3 contextualization variants, 9 interpretation cells (each with
    /// both inbound edges + `ensemble_variant.axis="interpretive"`), and
    /// the `assemble_ensemble_distribution` builtin aggregator fed by
    /// every cell. Also asserts the pass never synthesizes
    /// `assemble_report_data`.
    #[test]
    fn interpretation_grid_full_topology() {
        use crate::atom_registry::AtomRegistry;
        use crate::ensemble_roster::EnsembleRosterProvider;
        use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        let cfg = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config"));
        let reg = AtomRegistry::load_from_dir(&cfg.join("stage-atoms")).unwrap();
        let roster = EnsembleRosterProvider::from_dir(&cfg.join("ensemble-rosters"))
            .roster_for("bulk_rnaseq")
            .cloned()
            .unwrap();
        assert_eq!(roster.statistical_variants.len(), 3, "K=3 fixture");
        assert_eq!(roster.interpretive_lenses.len(), 3, "M=3 fixture");

        let de = TaskNode::from_atom(reg.get("differential_expression").unwrap());
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                de,
                TaskNode::skeleton("reporting", "r"),
                TaskNode::skeleton("final_reporting", "f"),
            ],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };

        synthesize_ensemble_fanout(&mut dag, &reg, &roster);

        // M contextualization variants.
        for lens in &roster.interpretive_lenses {
            let cid = format!("contextualize_findings_with_literature__v_{}", lens.id);
            let node = dag
                .nodes
                .iter()
                .find(|n| n.id == cid)
                .unwrap_or_else(|| panic!("contextualize variant {cid} present"));
            assert_eq!(
                node.attributes
                    .get("ensemble_variant")
                    .and_then(|v| v.get("axis"))
                    .and_then(|v| v.as_str()),
                Some("contextualization"),
                "{cid} axis"
            );
            assert_eq!(
                node.attributes.get("atom_id").and_then(|v| v.as_str()),
                Some("contextualize_findings_with_literature")
            );
            // Fed by the stat aggregator.
            assert!(
                dag.edges.iter().any(|e| e.from_node == ENSEMBLE_STAT_AGGREGATOR_ID
                    && e.to_node == cid),
                "stat aggregator -> {cid}"
            );
        }
        let ctx_count = dag
            .nodes
            .iter()
            .filter(|n| n.id.starts_with("contextualize_findings_with_literature__v_"))
            .count();
        assert_eq!(ctx_count, 3, "3 contextualization variants");

        // K×M interpretation cells.
        let mut cell_count = 0;
        for method in &roster.statistical_variants {
            for lens in &roster.interpretive_lenses {
                let cell_id =
                    format!("biological_interpretation__m_{}__lens_{}", method.id, lens.id);
                let node = dag
                    .nodes
                    .iter()
                    .find(|n| n.id == cell_id)
                    .unwrap_or_else(|| panic!("cell {cell_id} present"));
                cell_count += 1;
                let variant = node.attributes.get("ensemble_variant").unwrap();
                assert_eq!(
                    variant.get("axis").and_then(|v| v.as_str()),
                    Some("interpretive"),
                    "{cell_id} axis"
                );
                assert_eq!(
                    variant.get("method_variant").and_then(|v| v.as_str()),
                    Some(method.id.as_str())
                );
                assert_eq!(
                    variant.get("persona_ref").and_then(|v| v.as_str()),
                    Some(lens.persona_ref.as_str())
                );
                assert_eq!(
                    variant.get("model_tier").and_then(|v| v.as_str()),
                    Some(lens.model_tier.as_str())
                );
                assert_eq!(
                    node.attributes.get("atom_id").and_then(|v| v.as_str()),
                    Some("biological_interpretation")
                );

                // Inbound: method variant -> cell (method_result).
                let method_src = format!("differential_expression__v_{}", method.id);
                assert!(
                    dag.edges.iter().any(|e| e.from_node == method_src
                        && e.to_node == cell_id
                        && e.to_port == "method_result"),
                    "{method_src} -> {cell_id} (method_result)"
                );
                // Inbound: lens contextualize -> cell (literature_concordance).
                let lens_src =
                    format!("contextualize_findings_with_literature__v_{}", lens.id);
                assert!(
                    dag.edges.iter().any(|e| e.from_node == lens_src
                        && e.to_node == cell_id
                        && e.to_port == "literature_concordance"),
                    "{lens_src} -> {cell_id} (literature_concordance)"
                );
                // Outbound: cell -> ensemble aggregator (cell_result).
                assert!(
                    dag.edges.iter().any(|e| e.from_node == cell_id
                        && e.to_node == ENSEMBLE_AGGREGATOR_ID
                        && e.to_port == "cell_result"),
                    "{cell_id} -> ensemble aggregator (cell_result)"
                );
            }
        }
        assert_eq!(cell_count, 9, "K*M = 9 interpretation cells");

        // Ensemble aggregator is a builtin node.
        let agg = dag
            .nodes
            .iter()
            .find(|n| n.id == ENSEMBLE_AGGREGATOR_ID)
            .expect("ensemble aggregator present");
        assert!(agg.attributes.contains_key("builtin"), "aggregator is builtin");

        // The pass never synthesizes assemble_report_data.
        assert!(
            !dag.nodes.iter().any(|n| n.id == "assemble_report_data"),
            "ensemble pass does not depend on assemble_report_data"
        );
    }

    /// Fractional roster: the grid has exactly `selected_cells().len()`
    /// interpretation cells, and every method + lens is still covered.
    #[test]
    fn interpretation_grid_fractional_cell_count() {
        use crate::atom_registry::AtomRegistry;
        use crate::ensemble_roster::EnsembleRosterProvider;
        use crate::ensemble_roster::FactorialMode;
        use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        let cfg = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config"));
        let reg = AtomRegistry::load_from_dir(&cfg.join("stage-atoms")).unwrap();
        let mut roster = EnsembleRosterProvider::from_dir(&cfg.join("ensemble-rosters"))
            .roster_for("bulk_rnaseq")
            .cloned()
            .unwrap();
        roster.factorial = FactorialMode::Fractional;
        let expected = roster.selected_cells().len(); // max(3,3) = 3

        let de = TaskNode::from_atom(reg.get("differential_expression").unwrap());
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![de, TaskNode::skeleton("final_reporting", "f")],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };

        synthesize_ensemble_fanout(&mut dag, &reg, &roster);

        let cells = dag
            .nodes
            .iter()
            .filter(|n| n.id.starts_with("biological_interpretation__m_"))
            .count();
        assert_eq!(cells, expected, "fractional cell count == selected_cells().len()");
    }

    /// Shared fixture for the `*_validated` tests: a real
    /// `differential_expression` node plus a real `final_reporting`
    /// terminal, and the shipped `bulk_rnaseq` roster (cloned so callers
    /// can mutate it freely).
    fn validated_fixture() -> (
        crate::atom_registry::AtomRegistry,
        EnsembleRoster,
        crate::workflow_contracts::task_node::WorkflowDag,
        std::path::PathBuf,
    ) {
        use crate::atom_registry::AtomRegistry;
        use crate::ensemble_roster::EnsembleRosterProvider;
        use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        let cfg = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config"));
        let reg = AtomRegistry::load_from_dir(&cfg.join("stage-atoms")).unwrap();
        let roster = EnsembleRosterProvider::from_dir(&cfg.join("ensemble-rosters"))
            .roster_for("bulk_rnaseq")
            .cloned()
            .unwrap();
        let de = TaskNode::from_atom(reg.get("differential_expression").unwrap());
        let dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![de, TaskNode::skeleton("final_reporting", "f")],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };
        let persona_dir = cfg.join("ensemble-rosters/personas");
        (reg, roster, dag, persona_dir)
    }

    fn no_v_nodes(dag: &crate::workflow_contracts::task_node::WorkflowDag) -> bool {
        !dag.nodes.iter().any(|n| n.id.contains("__v_"))
    }

    #[test]
    fn validated_rejects_too_small_caps() {
        let (reg, mut roster, mut dag, persona_dir) = validated_fixture();
        roster.caps.max_ensemble_members = 1;

        let err = synthesize_ensemble_fanout_validated(&mut dag, &reg, &roster, &persona_dir)
            .expect_err("caps too small must reject");
        assert!(
            err.contains("max_ensemble_members"),
            "explains the cap: {err}"
        );
        assert!(no_v_nodes(&dag), "dag unchanged on validator failure");
        assert!(
            dag.nodes.iter().any(|n| n.id == "differential_expression"),
            "base node still present, untouched"
        );
    }

    #[test]
    fn validated_rejects_unknown_tool() {
        use crate::ensemble_roster::StatisticalVariant;

        let (reg, mut roster, mut dag, persona_dir) = validated_fixture();
        roster.statistical_variants.push(StatisticalVariant {
            id: "x".into(),
            tool: "not_a_de_tool".into(),
            bootstrap_replicates: 0,
        });

        let err = synthesize_ensemble_fanout_validated(&mut dag, &reg, &roster, &persona_dir)
            .expect_err("unknown tool must reject");
        assert!(err.contains("not_a_de_tool"), "names the bad tool: {err}");
        assert!(no_v_nodes(&dag), "dag unchanged on validator failure");
    }

    #[test]
    fn validated_rejects_forbidden_persona() {
        let (reg, mut roster, mut dag, _real_persona_dir) = validated_fixture();

        let tmp = std::env::temp_dir().join(format!(
            "ens_synth_personas_{}_{}",
            std::process::id(),
            "forbidden"
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("bad_lens.md"),
            "You are a reviewer. Your job is to maximize the evidence for the hypothesis.",
        )
        .unwrap();

        roster.interpretive_lenses = vec![crate::ensemble_roster::InterpretiveLens {
            id: "bad_lens".into(),
            persona_ref: "bad_lens.md".into(),
            model_tier: "opus".into(),
            retrieval: "recent".into(),
            model: None,
        }];

        let err = synthesize_ensemble_fanout_validated(&mut dag, &reg, &roster, &tmp)
            .expect_err("forbidden persona language must reject");
        assert!(err.contains("bad_lens"), "names the persona: {err}");
        assert!(no_v_nodes(&dag), "dag unchanged on validator failure");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn validated_passes_shipped_roster_and_expands() {
        let (reg, roster, mut dag, persona_dir) = validated_fixture();

        synthesize_ensemble_fanout_validated(&mut dag, &reg, &roster, &persona_dir)
            .expect("shipped bulk_rnaseq roster + real personas must validate");

        let variant_count = dag
            .nodes
            .iter()
            .filter(|n| n.id.starts_with("differential_expression__v_"))
            .count();
        assert_eq!(variant_count, 3, "K=3 variants expanded");
        assert!(
            dag.nodes.iter().any(|n| n.id == ENSEMBLE_AGGREGATOR_ID),
            "cross-axis ensemble aggregator present"
        );
    }
}
