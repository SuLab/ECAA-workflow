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
use crate::reexecution_bounds::ModalityBounds;
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

/// Documented per-task agent-budget-cap ESTIMATE for one statistical
/// method-variant task (an analytical task, mirroring the analytical
/// `--max-budget-usd` calibration). A rough guardrail figure, NOT precise
/// cost accounting — see [`project_ensemble_cost_usd`].
pub const STAT_VARIANT_EST_USD: f64 = 3.0;

/// Documented per-task agent-budget-cap ESTIMATE for one contextualization
/// variant task. See [`project_ensemble_cost_usd`].
pub const CONTEXTUALIZE_EST_USD: f64 = 2.0;

/// Documented per-task agent-budget-cap ESTIMATE for one interpretation
/// cell task. See [`project_ensemble_cost_usd`].
pub const INTERPRETATION_CELL_EST_USD: f64 = 2.0;

/// Reporting-terminal ids the cross-axis aggregator feeds when present.
/// Exact-id match only — mirrors `report_data_synthesis`'s constant of
/// the same name (the two canonical terminal ids, no alias matching).
const REPORTING_TERMINAL_IDS: [&str; 2] = ["reporting", "final_reporting"];

/// The base contextualization node id fanned into per-lens variants and
/// removed by this pass. Kept as a named constant so the fan-out, the
/// base-node removal, and the stale-companion removal cannot drift.
const CONTEXTUALIZE_BASE_ID: &str = "contextualize_findings_with_literature";

/// Ensemble-only report-section ids APPENDED to every reporting terminal's
/// `required_report_sections` (union with the atom-declared base sections —
/// see [`merge_string_list`]). Read by `RC-SECTIONS`
/// (`reporting_invariants::check_rc_sections`) and by the reporting agent
/// (`scripts/agent-prompts/task-execution.md`).
const ENSEMBLE_REPORT_SECTIONS: [&str; 5] = [
    "method_robustness",
    "interpretive_agreement",
    "method_lens_interaction",
    "dissenting_lenses",
    "literature_coverage",
];

/// Ensemble-only supplementary-table ids APPENDED to every reporting
/// terminal's `required_tables`. See [`ENSEMBLE_REPORT_SECTIONS`].
const ENSEMBLE_REPORT_TABLES: [&str; 4] = [
    "method_robustness",
    "lens_agreement",
    "interaction_hotspots",
    "literature_union",
];

/// The package-relative paths the reporting agent reads instead of the
/// (in ensemble mode, absent) `reporting/report-data.json` — the two
/// aggregator artifacts this pass wires to the reporting terminals.
/// Stamped verbatim onto `attributes["ensemble_report_files"]`.
const ENSEMBLE_REPORT_FILES: [&str; 2] = [
    "assemble_ensemble_distribution/ensemble-distribution.json",
    "assemble_statistical_distribution/stat-distribution.json",
];

/// Fan the schema-bearing statistical node(s) out over the roster's
/// method variants, and inject the statistical-distribution aggregator
/// stub. No-op when: no schema-bearing node exists, or the pass has
/// already run (idempotent). See module docs.
///
/// `bounds` is the caller-resolved [`ModalityBounds`] for `roster.modality`
/// (the pass itself has no config-dir access, so callers resolve it —
/// `synthesize_ensemble_fanout_validated` derives the config root from its
/// `persona_dir` argument). Stamped onto the statistical aggregator as
/// `relative_tolerance`/`absolute_tolerance` so the Plan-3 harness runner
/// can classify cross-method robustness without its own config lookup.
pub fn synthesize_ensemble_fanout(
    dag: &mut WorkflowDag,
    atom_reg: &AtomRegistry,
    roster: &EnsembleRoster,
    bounds: &ModalityBounds,
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
    let mut agg = builtin_aggregator(
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
    // Resolve the primary's base atom the same way the target-selection
    // loop above does (id first, `atom_id` back-reference fallback) so the
    // stat aggregator can carry the schema the Plan-3 harness runner reads
    // to know which columns are entity/effect/significance.
    let base_atom = atom_reg.get(&primary).or_else(|| {
        base_node
            .as_ref()
            .and_then(|n| n.attributes.get("atom_id"))
            .and_then(|v| v.as_str())
            .and_then(|id| atom_reg.get(id))
    });
    let mut variant_stage_ids: Vec<String> = Vec::new();
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
        variant_stage_ids.push(vid.clone());
        new_nodes.push(vn);
    }
    variant_stage_ids.sort();

    // Stamp the Plan-3 harness runner's inputs onto the statistical
    // aggregator: the K variant node ids to read off disk, the base atom's
    // `result_schema` (so the runner knows which columns are
    // entity/effect/significance; `null` when the base atom declares
    // none), and the modality's re-execution tolerances (the same bounds
    // `reexecution_bounds` uses to classify SemanticEquivalent, reused here
    // to classify cross-method robustness).
    agg.attributes.insert(
        "variant_stage_ids".into(),
        serde_json::to_value(&variant_stage_ids).unwrap_or(serde_json::Value::Null),
    );
    agg.attributes.insert(
        "result_schema".into(),
        base_atom
            .map(|a| serde_json::to_value(&a.result_schema).unwrap_or(serde_json::Value::Null))
            .unwrap_or(serde_json::Value::Null),
    );
    agg.attributes.insert(
        "relative_tolerance".into(),
        serde_json::json!(bounds.relative_tolerance),
    );
    agg.attributes.insert(
        "absolute_tolerance".into(),
        serde_json::json!(bounds.absolute_tolerance),
    );

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
    let mut interpretation_cell_ids: Vec<String> = Vec::new();
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
        interpretation_cell_ids.push(cell_id.clone());
        new_nodes.push(cell);
    }
    interpretation_cell_ids.sort();

    // Cross-axis ensemble aggregator (builtin -> skipped by validate-companion synthesis).
    // Stamp the K×M interpretation cell node ids so the Plan-3 harness
    // runner knows which per-cell result artifacts to roll up.
    let mut cross_agg = builtin_aggregator(
        ENSEMBLE_AGGREGATOR_ID,
        "Aggregate cross-axis ensemble distribution over the K×M interpretation grid",
        "aggregates every interpretation cell's result artifact off disk",
        "ensemble_distribution",
    );
    cross_agg.attributes.insert(
        "interpretation_cell_ids".into(),
        serde_json::to_value(&interpretation_cell_ids).unwrap_or(serde_json::Value::Null),
    );
    // The primary statistical base stage_id, so the harness runner can locate
    // each cell's method-variant result table
    // (`runtime/outputs/<primary_stage_id>__v_<method>/`) for per-cell claim
    // verification.
    cross_agg.attributes.insert(
        "primary_stage_id".into(),
        serde_json::Value::String(primary.clone()),
    );
    // Per-axis quorum floor (roster.caps.min_quorum_per_axis) — the harness
    // runner blocks with `BlockerKind::EnsembleQuorumNotMet` if either the
    // method axis or the lens axis has fewer distinct readable cells than
    // this after per-cell verifier pruning. `0` means "never block".
    cross_agg.attributes.insert(
        "min_quorum_per_axis".into(),
        serde_json::json!(roster.caps.min_quorum_per_axis),
    );
    // Compile-time budget projection (guardrail: warn + provenance, NEVER a
    // hard emission block — the four-conditions rule, CLAUDE.md). ALWAYS
    // stamped, regardless of whether the projection exceeds the ceiling, so
    // the projection is surfaceable even on a well-provisioned roster. A
    // runtime hard-stop against actual accrued cost is deferred (no
    // per-task USD accumulator exists today — see
    // `docs/known-limitations.md`).
    let projected_cost_usd = project_ensemble_cost_usd(roster);
    let budget_ceiling_usd = roster.caps.per_ensemble_budget_usd;
    cross_agg.attributes.insert(
        "projected_cost_usd".into(),
        serde_json::json!(projected_cost_usd),
    );
    cross_agg.attributes.insert(
        "budget_ceiling_usd".into(),
        serde_json::json!(budget_ceiling_usd),
    );
    if budget_ceiling_usd > 0.0 && projected_cost_usd > budget_ceiling_usd {
        tracing::warn!(
            "[ensemble] projected cost ${projected_cost_usd:.2} exceeds \
             per_ensemble_budget_usd ${budget_ceiling_usd:.2}"
        );
    }
    new_nodes.push(cross_agg);

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

    // Stamp ensemble-mode report obligations onto every reporting terminal
    // present. `required_report_sections`/`required_tables` were already
    // stamped onto `node.attributes` from the atom's declared lists back
    // at initial DAG lift time (`TaskNode::from_atom`,
    // `workflow_contracts::from_atom.rs`) — long before this post-pass
    // runs — so this is a UNION, never a first write: the base sections
    // (e.g. `primary_results`, `qc_preprocessing`) survive alongside the
    // 5 ensemble-only sections. `ensemble_mode` + `ensemble_report_files`
    // tell the reporting agent to narrate over the two aggregator
    // artifacts instead of the (in ensemble mode, absent)
    // `reporting/report-data.json` — see
    // `scripts/agent-prompts/task-execution.md`.
    for node in dag.nodes.iter_mut() {
        if !REPORTING_TERMINAL_IDS.contains(&node.id.as_str()) {
            continue;
        }
        let sections = merge_string_list(
            node.attributes.get("required_report_sections"),
            &ENSEMBLE_REPORT_SECTIONS,
        );
        node.attributes
            .insert("required_report_sections".into(), sections);
        let tables = merge_string_list(
            node.attributes.get("required_tables"),
            &ENSEMBLE_REPORT_TABLES,
        );
        node.attributes.insert("required_tables".into(), tables);
        node.attributes
            .insert("ensemble_mode".into(), serde_json::Value::Bool(true));
        node.attributes.insert(
            "ensemble_report_files".into(),
            serde_json::to_value(ENSEMBLE_REPORT_FILES).unwrap_or(serde_json::Value::Null),
        );
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

    // Resolve `roster.modality`'s re-execution bounds for the stat
    // aggregator. `persona_dir` is always
    // `<config>/ensemble-rosters/personas` (see
    // `EnsembleRosterProvider::personas_dir`), so its grandparent is the
    // config root; `ModalityBoundsProvider::from_dir` degrades to the
    // fallback-only provider on a missing/malformed dir (never panics),
    // so an unexpected `persona_dir` shape just yields the generic ±5%
    // bounds rather than an error here.
    let config_root = persona_dir.parent().and_then(std::path::Path::parent);
    let bounds = config_root
        .map(|root| {
            crate::reexecution_bounds::ModalityBoundsProvider::from_dir(
                &root.join("reexecution-bounds"),
            )
            .bounds_for(&roster.modality)
        })
        .unwrap_or_default();

    synthesize_ensemble_fanout(dag, atom_reg, roster, &bounds);
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

/// Unions `additions` into whatever string array already lives at
/// `existing` (typically a `node.attributes` lookup), deduplicates, and
/// returns the result SORTED — deterministic regardless of the existing
/// array's on-disk order or `additions`' declaration order. A non-array or
/// absent `existing` degrades to just `additions` (still sorted + deduped)
/// rather than erroring, so a reporting terminal with no atom-declared
/// sections still gets the ensemble-only ones.
fn merge_string_list(
    existing: Option<&serde_json::Value>,
    additions: &[&str],
) -> serde_json::Value {
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(serde_json::Value::Array(arr)) = existing {
        for v in arr {
            if let Some(s) = v.as_str() {
                set.insert(s.to_string());
            }
        }
    }
    for a in additions {
        set.insert((*a).to_string());
    }
    serde_json::to_value(set.into_iter().collect::<Vec<String>>()).unwrap_or(serde_json::Value::Null)
}

/// Deterministic COMPILE-TIME projected ensemble cost: `K` statistical
/// variants + `M` contextualizations + the roster's selected interpretation
/// cells (`roster.selected_cells().len()`, factorial-mode-aware — `Full` =
/// K*M, `Fractional` = max(K,M)), each priced at its documented per-member
/// ESTIMATE ([`STAT_VARIANT_EST_USD`] / [`CONTEXTUALIZE_EST_USD`] /
/// [`INTERPRETATION_CELL_EST_USD`]). The two in-process builtin aggregators
/// (`assemble_statistical_distribution` / `assemble_ensemble_distribution`)
/// cost $0 — they run as core-assembler builtins, not agent tasks.
///
/// This is a rough guardrail projection from documented per-task agent
/// budget CAPS, not precise cost accounting: no per-task USD accumulator
/// exists today to true this up against actual accrued spend (see
/// `docs/known-limitations.md`). Pure function of `roster` — identical
/// output on every call, independent of DAG state.
pub fn project_ensemble_cost_usd(roster: &EnsembleRoster) -> f64 {
    let k = roster.statistical_variants.len() as f64;
    let m = roster.interpretive_lenses.len() as f64;
    let cells = roster.selected_cells().len() as f64;
    k * STAT_VARIANT_EST_USD + m * CONTEXTUALIZE_EST_USD + cells * INTERPRETATION_CELL_EST_USD
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

        synthesize_ensemble_fanout(&mut dag, &reg, &roster, &ModalityBounds::default());

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
        synthesize_ensemble_fanout(&mut dag, &reg, &roster, &ModalityBounds::default());
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

        synthesize_ensemble_fanout(&mut dag, &reg, &roster, &ModalityBounds::default());

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

        synthesize_ensemble_fanout(&mut dag, &reg, &roster, &ModalityBounds::default());

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
            persona_text: None,
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

    /// Plan-3 Task-1: the two aggregator nodes carry the inputs their
    /// harness runners need — the stat aggregator gets the sorted K
    /// variant ids, the base atom's `result_schema`, and the shipped
    /// `bulk_rnaseq` re-execution bounds (0.02 / 0.001); the ensemble
    /// aggregator gets the sorted K×M cell ids. Goes through the real
    /// `*_validated` entry point so bounds resolution (persona_dir ->
    /// config root -> reexecution-bounds) is exercised end-to-end.
    #[test]
    fn aggregator_inputs_stamped() {
        let (reg, roster, mut dag, persona_dir) = validated_fixture();

        synthesize_ensemble_fanout_validated(&mut dag, &reg, &roster, &persona_dir)
            .expect("shipped bulk_rnaseq roster + real personas must validate");

        let stat_agg = dag
            .nodes
            .iter()
            .find(|n| n.id == ENSEMBLE_STAT_AGGREGATOR_ID)
            .expect("stat aggregator present");

        let variant_ids: Vec<String> = stat_agg
            .attributes
            .get("variant_stage_ids")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .expect("variant_stage_ids present + deserializes to Vec<String>");
        assert_eq!(
            variant_ids,
            vec![
                "differential_expression__v_deseq2".to_string(),
                "differential_expression__v_edger".to_string(),
                "differential_expression__v_limma".to_string(),
            ],
            "variant_stage_ids == sorted K=3 DE variant ids"
        );

        let result_schema = stat_agg
            .attributes
            .get("result_schema")
            .expect("result_schema key present");
        assert!(!result_schema.is_null(), "base atom declares a result_schema");
        let schema: crate::report_contract::ResultSchema =
            serde_json::from_value(result_schema.clone())
                .expect("result_schema deserializes to ResultSchema");
        let base_atom = reg
            .get("differential_expression")
            .expect("differential_expression atom");
        assert_eq!(
            Some(schema),
            base_atom.result_schema.clone(),
            "stamped result_schema matches the DE atom's declared schema"
        );

        assert_eq!(
            stat_agg
                .attributes
                .get("relative_tolerance")
                .and_then(|v| v.as_f64()),
            Some(0.02),
            "bulk_rnaseq relative_tolerance"
        );
        assert_eq!(
            stat_agg
                .attributes
                .get("absolute_tolerance")
                .and_then(|v| v.as_f64()),
            Some(0.001),
            "bulk_rnaseq absolute_tolerance"
        );

        let ensemble_agg = dag
            .nodes
            .iter()
            .find(|n| n.id == ENSEMBLE_AGGREGATOR_ID)
            .expect("ensemble aggregator present");
        let cell_ids: Vec<String> = ensemble_agg
            .attributes
            .get("interpretation_cell_ids")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .expect("interpretation_cell_ids present + deserializes to Vec<String>");
        assert_eq!(cell_ids.len(), 9, "K*M = 9 interpretation cell ids");
        let mut sorted = cell_ids.clone();
        sorted.sort();
        assert_eq!(cell_ids, sorted, "interpretation_cell_ids is sorted");
    }

    /// Task C — `reporting`/`final_reporting` are built via `TaskNode::from_atom`
    /// (as they are at real DAG-lift time, well before this post-pass runs),
    /// so they already carry the atom-declared base `required_report_sections`/
    /// `required_tables` on `attributes`. After synthesis over the shipped
    /// `bulk_rnaseq` roster, both terminals' sections/tables are the UNION of
    /// the base list and the 5/4 ensemble-only ids (no dup), plus
    /// `ensemble_mode: true` and the stamped `ensemble_report_files`.
    #[test]
    fn ensemble_reporting_terminals_get_ensemble_sections() {
        use crate::atom_registry::AtomRegistry;
        use crate::ensemble_roster::EnsembleRosterProvider;
        use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        let cfg = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config"));
        let reg = AtomRegistry::load_from_dir(&cfg.join("stage-atoms")).unwrap();
        let roster = EnsembleRosterProvider::from_dir(&cfg.join("ensemble-rosters"))
            .roster_for("bulk_rnaseq")
            .cloned()
            .unwrap();

        let reporting_atom = reg.get("reporting").expect("reporting atom present");
        let final_reporting_atom = reg
            .get("final_reporting")
            .expect("final_reporting atom present");
        assert!(
            !reporting_atom.required_report_sections.is_empty(),
            "precondition: reporting atom declares base sections"
        );

        let de = TaskNode::from_atom(reg.get("differential_expression").unwrap());
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                de,
                TaskNode::from_atom(reporting_atom),
                TaskNode::from_atom(final_reporting_atom),
            ],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };

        synthesize_ensemble_fanout(&mut dag, &reg, &roster, &ModalityBounds::default());

        for (terminal, base_atom) in [
            ("reporting", reporting_atom),
            ("final_reporting", final_reporting_atom),
        ] {
            let node = dag
                .nodes
                .iter()
                .find(|n| n.id == terminal)
                .unwrap_or_else(|| panic!("{terminal} node present"));

            let sections: Vec<String> = node
                .attributes
                .get("required_report_sections")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_else(|| panic!("{terminal} required_report_sections present"));
            for base in &base_atom.required_report_sections {
                assert!(
                    sections.contains(base),
                    "{terminal} keeps base section {base}: {sections:?}"
                );
            }
            for ens in ENSEMBLE_REPORT_SECTIONS {
                assert!(
                    sections.contains(&ens.to_string()),
                    "{terminal} gains ensemble section {ens}: {sections:?}"
                );
            }
            let unique: std::collections::BTreeSet<&String> = sections.iter().collect();
            assert_eq!(
                unique.len(),
                sections.len(),
                "{terminal} required_report_sections has no dup: {sections:?}"
            );

            let tables: Vec<String> = node
                .attributes
                .get("required_tables")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_else(|| panic!("{terminal} required_tables present"));
            for base in &base_atom.required_tables {
                assert!(
                    tables.contains(base),
                    "{terminal} keeps base table {base}: {tables:?}"
                );
            }
            for ens in ENSEMBLE_REPORT_TABLES {
                assert!(
                    tables.contains(&ens.to_string()),
                    "{terminal} gains ensemble table {ens}: {tables:?}"
                );
            }
            let unique_tables: std::collections::BTreeSet<&String> = tables.iter().collect();
            assert_eq!(
                unique_tables.len(),
                tables.len(),
                "{terminal} required_tables has no dup: {tables:?}"
            );

            assert_eq!(
                node.attributes.get("ensemble_mode").and_then(|v| v.as_bool()),
                Some(true),
                "{terminal} ensemble_mode stamped"
            );
            let files: Vec<String> = node
                .attributes
                .get("ensemble_report_files")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_else(|| panic!("{terminal} ensemble_report_files present"));
            assert_eq!(
                files,
                vec![
                    "assemble_ensemble_distribution/ensemble-distribution.json".to_string(),
                    "assemble_statistical_distribution/stat-distribution.json".to_string(),
                ],
                "{terminal} ensemble_report_files"
            );
        }
    }

    /// Task C lowered-spec check: the emitted `reporting` task's
    /// `spec.required_report_sections` includes the ensemble sections and
    /// `spec.ensemble_mode == true` — proves the chain: synthesis-time
    /// attribute stamp -> `workflow_json.rs` emit-time allowlist -> the
    /// lowered task's `spec`.
    #[test]
    fn lowered_reporting_task_spec_carries_ensemble_sections_and_mode() {
        use crate::atom_registry::AtomRegistry;
        use crate::backend_emitters::workflow_json::{lower_to_workflow_json, EmitContext};
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
                TaskNode::from_atom(reg.get("reporting").unwrap()),
                TaskNode::from_atom(reg.get("final_reporting").unwrap()),
            ],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };

        synthesize_ensemble_fanout(&mut dag, &reg, &roster, &ModalityBounds::default());

        let artifact = lower_to_workflow_json(&dag, &EmitContext::defaults())
            .expect("lowering the ensemble dag must succeed");
        let task = artifact
            .dag
            .tasks
            .get("reporting")
            .expect("reporting task must be present in the lowered DAG");
        let spec = task
            .spec
            .as_ref()
            .expect("reporting task must carry a spec");

        let sections: Vec<String> = spec
            .get("required_report_sections")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .expect("lowered spec carries required_report_sections");
        for ens in ENSEMBLE_REPORT_SECTIONS {
            assert!(
                sections.contains(&ens.to_string()),
                "lowered reporting spec gains ensemble section {ens}: {sections:?}"
            );
        }
        assert_eq!(
            spec.get("ensemble_mode").and_then(|v| v.as_bool()),
            Some(true),
            "lowered reporting spec.ensemble_mode == true"
        );
        let files: Vec<String> = spec
            .get("ensemble_report_files")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .expect("lowered spec carries ensemble_report_files");
        assert_eq!(files.len(), 2, "both aggregator files listed");
    }

    /// Non-ensemble control: a DAG with no schema-bearing analytical node
    /// (`synthesize_ensemble_fanout`'s `targets` selector finds nothing, so
    /// the whole pass no-ops) leaves the `reporting` node's sections/tables
    /// exactly as `TaskNode::from_atom` stamped them — no ensemble
    /// sections, no `ensemble_mode`.
    #[test]
    fn non_ensemble_reporting_node_unchanged() {
        use crate::atom_registry::AtomRegistry;
        use crate::ensemble_roster::EnsembleRosterProvider;
        use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        let cfg = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config"));
        let reg = AtomRegistry::load_from_dir(&cfg.join("stage-atoms")).unwrap();
        let roster = EnsembleRosterProvider::from_dir(&cfg.join("ensemble-rosters"))
            .roster_for("bulk_rnaseq")
            .cloned()
            .unwrap();
        let reporting_atom = reg.get("reporting").expect("reporting atom present");

        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![TaskNode::from_atom(reporting_atom)],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };

        synthesize_ensemble_fanout(&mut dag, &reg, &roster, &ModalityBounds::default());

        let node = dag
            .nodes
            .iter()
            .find(|n| n.id == "reporting")
            .expect("reporting node present");
        let sections: Vec<String> = node
            .attributes
            .get("required_report_sections")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .expect("required_report_sections present");
        assert_eq!(
            sections, reporting_atom.required_report_sections,
            "sections unchanged when the ensemble pass no-ops"
        );
        for ens in ENSEMBLE_REPORT_SECTIONS {
            assert!(
                !sections.contains(&ens.to_string()),
                "no ensemble section leaks in without a schema-bearing target: {sections:?}"
            );
        }
        assert!(
            !node.attributes.contains_key("ensemble_mode"),
            "ensemble_mode not stamped when the pass no-ops"
        );
        assert!(
            !node.attributes.contains_key("ensemble_report_files"),
            "ensemble_report_files not stamped when the pass no-ops"
        );
    }

    /// Task F fixture: a real `differential_expression` node plus real
    /// `reporting`/`final_reporting` terminals, and the shipped
    /// `bulk_rnaseq` roster (K=3, M=3, full factorial -> 9 cells) with
    /// `caps.per_ensemble_budget_usd` overridden by the caller.
    fn budget_fixture(
        per_ensemble_budget_usd: f64,
    ) -> (
        crate::atom_registry::AtomRegistry,
        EnsembleRoster,
        crate::workflow_contracts::task_node::WorkflowDag,
    ) {
        use crate::atom_registry::AtomRegistry;
        use crate::ensemble_roster::EnsembleRosterProvider;
        use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        let cfg = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config"));
        let reg = AtomRegistry::load_from_dir(&cfg.join("stage-atoms")).unwrap();
        let mut roster = EnsembleRosterProvider::from_dir(&cfg.join("ensemble-rosters"))
            .roster_for("bulk_rnaseq")
            .cloned()
            .unwrap();
        roster.caps.per_ensemble_budget_usd = per_ensemble_budget_usd;
        assert_eq!(roster.statistical_variants.len(), 3, "K=3 fixture");
        assert_eq!(roster.interpretive_lenses.len(), 3, "M=3 fixture");
        assert_eq!(roster.selected_cells().len(), 9, "full K*M=9 cells fixture");

        let de = TaskNode::from_atom(reg.get("differential_expression").unwrap());
        let dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                de,
                TaskNode::from_atom(reg.get("reporting").unwrap()),
                TaskNode::from_atom(reg.get("final_reporting").unwrap()),
            ],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };
        (reg, roster, dag)
    }

    /// K=3, M=3, full factorial -> 9 cells: 3*3.0 + 3*2.0 + 9*2.0 = 33.0.
    const BULK_RNASEQ_PROJECTED_USD: f64 = 33.0;

    /// A LOW `per_ensemble_budget_usd` (5.0) against the shipped
    /// bulk_rnaseq roster's member counts: `projected_cost_usd` /
    /// `budget_ceiling_usd` are stamped on the cross-axis aggregator node,
    /// `projected_cost_usd > budget_ceiling_usd`, and — critically —
    /// emission still succeeds end-to-end (the guardrail never blocks;
    /// see the four-conditions rule in CLAUDE.md).
    #[test]
    fn budget_projection_stamped_and_warns_over_ceiling() {
        use crate::backend_emitters::workflow_json::{lower_to_workflow_json, EmitContext};

        let (reg, roster, mut dag) = budget_fixture(5.0);
        assert_eq!(
            project_ensemble_cost_usd(&roster),
            BULK_RNASEQ_PROJECTED_USD
        );

        synthesize_ensemble_fanout(&mut dag, &reg, &roster, &ModalityBounds::default());

        let agg = dag
            .nodes
            .iter()
            .find(|n| n.id == ENSEMBLE_AGGREGATOR_ID)
            .expect("cross-axis ensemble aggregator present");
        let projected = agg
            .attributes
            .get("projected_cost_usd")
            .and_then(|v| v.as_f64())
            .expect("projected_cost_usd stamped");
        let ceiling = agg
            .attributes
            .get("budget_ceiling_usd")
            .and_then(|v| v.as_f64())
            .expect("budget_ceiling_usd stamped");
        assert_eq!(projected, BULK_RNASEQ_PROJECTED_USD);
        assert_eq!(ceiling, 5.0);
        assert!(
            projected > ceiling,
            "fixture must exceed the low ceiling: {projected} vs {ceiling}"
        );

        // Emission still succeeds — the guardrail is warn + provenance
        // only, never a hard emission block.
        let artifact = lower_to_workflow_json(&dag, &EmitContext::defaults())
            .expect("emission must still succeed when projected cost exceeds the ceiling");
        let task = artifact
            .dag
            .tasks
            .get(ENSEMBLE_AGGREGATOR_ID)
            .expect("aggregator task present in the lowered DAG");
        let spec = task.spec.as_ref().expect("aggregator task carries a spec");
        assert_eq!(
            spec.get("projected_cost_usd").and_then(|v| v.as_f64()),
            Some(BULK_RNASEQ_PROJECTED_USD),
            "lowered spec.projected_cost_usd"
        );
        assert_eq!(
            spec.get("budget_ceiling_usd").and_then(|v| v.as_f64()),
            Some(5.0),
            "lowered spec.budget_ceiling_usd"
        );
    }

    /// A HIGH `per_ensemble_budget_usd` (1000.0): still stamped, and
    /// `projected_cost_usd <= budget_ceiling_usd` (no over-budget flag).
    #[test]
    fn budget_projection_within_ceiling_no_flag() {
        let (reg, roster, mut dag) = budget_fixture(1000.0);

        synthesize_ensemble_fanout(&mut dag, &reg, &roster, &ModalityBounds::default());

        let agg = dag
            .nodes
            .iter()
            .find(|n| n.id == ENSEMBLE_AGGREGATOR_ID)
            .expect("cross-axis ensemble aggregator present");
        let projected = agg
            .attributes
            .get("projected_cost_usd")
            .and_then(|v| v.as_f64())
            .expect("projected_cost_usd stamped");
        let ceiling = agg
            .attributes
            .get("budget_ceiling_usd")
            .and_then(|v| v.as_f64())
            .expect("budget_ceiling_usd stamped");
        assert_eq!(projected, BULK_RNASEQ_PROJECTED_USD);
        assert_eq!(ceiling, 1000.0);
        assert!(
            projected <= ceiling,
            "fixture must be within the high ceiling: {projected} vs {ceiling}"
        );
    }

    /// Determinism: the projection is a pure function of the roster's
    /// member counts, byte-stable across independent synthesis runs.
    #[test]
    fn budget_projection_deterministic_across_runs() {
        let (reg_a, roster_a, mut dag_a) = budget_fixture(5.0);
        let (reg_b, roster_b, mut dag_b) = budget_fixture(5.0);

        synthesize_ensemble_fanout(&mut dag_a, &reg_a, &roster_a, &ModalityBounds::default());
        synthesize_ensemble_fanout(&mut dag_b, &reg_b, &roster_b, &ModalityBounds::default());

        let get = |dag: &crate::workflow_contracts::task_node::WorkflowDag| -> (f64, f64) {
            let agg = dag
                .nodes
                .iter()
                .find(|n| n.id == ENSEMBLE_AGGREGATOR_ID)
                .unwrap();
            (
                agg.attributes
                    .get("projected_cost_usd")
                    .and_then(|v| v.as_f64())
                    .unwrap(),
                agg.attributes
                    .get("budget_ceiling_usd")
                    .and_then(|v| v.as_f64())
                    .unwrap(),
            )
        };
        assert_eq!(get(&dag_a), get(&dag_b), "identical across two synthesis runs");

        // Also directly deterministic at the pure-function level, called twice.
        assert_eq!(
            project_ensemble_cost_usd(&roster_a),
            project_ensemble_cost_usd(&roster_a)
        );
    }
}
