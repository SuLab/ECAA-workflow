//! `WorkflowDag → WORKFLOW.json` lowering pass per ADR 0029.
//!
//! Lowering rules (alignment plan §11 work item 2 table):
//!
//! | WorkflowDag source | Task destination |
//! |---|---|
//! | `TaskNode.id` | `TaskId` (BTreeMap key in `DAG.tasks`) |
//! | `TaskNode.machine_name` / role attribute | `Task.kind` / `Task.assignee` |
//! | `TaskNode.intent` (one-line summary) | `Task.description` |
//! | `Implementation::ContainerCommand.image` | `Task.container` |
//! | `TaskNode.preconditions` (`requires_sme_review`-equivalent) | `Task.requires_sme_review` |
//! | `TaskNode.postconditions` (artifact-shape obligations) | `Task.required_artifacts` |
//! | resource_class attribute | `Task.resource_class` |
//! | parent `TaskNode` ids of incoming edges | `Task.depends_on` |
//! | `EdgeContract::CompatibilityProof` | sidecar `runtime/proofs.jsonl` |
//! | `WorkflowDag.assumptions` | `runtime/decisions.jsonl` |
//!
//! Lowering is deterministic (sorted iteration over `BTreeMap`),
//! pure (no IO), and round-trippable for `Task`-bearing fields.

use std::collections::BTreeMap;

use crate::atom::{AtomAssignee, AtomRole};
use crate::dag::{
    Assignee, DiscoveryKind, RequiredArtifact, ResourceClass, Task, TaskKind, TaskState, DAG,
};
use crate::ids::TaskId;
use crate::plot_affordance::{AffordanceFallbackRecord, PlotAffordance};
use crate::workflow_contracts::edge::EdgeContract;
use crate::workflow_contracts::implementation::Implementation;
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

use super::capability_report::{BackendCapabilityReport, ConstraintLossAck};
use super::{CompileError, EmitError};

/// Per-port affordance record written to `runtime/plot_affordances.jsonl`.
///
/// The sidecar follows the same pattern as `runtime/proofs.jsonl` and
/// `runtime/assumptions.jsonl`: one JSON line per record, sorted by
/// `task_id` then `port_name` for byte-determinism.
///
/// The resolution call (walking the `PlotAffordanceRegistry` per
/// output port of each `TaskNode`) is not yet wired at lowering time
/// — it will be added by the v4 planner integration or the
/// atom-catalog lowering pass. For now, callers that have already
/// resolved affordances pass them in via
/// `EmitContext::emit_affordances`; callers that haven't (the current
/// `lower_to_workflow_json` default path) pass `None` and the sidecar
/// is omitted.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PlotAffordanceRecord {
    /// Task id.
    pub task_id: TaskId,
    /// Port name.
    pub port_name: String,
    /// Affordance.
    pub affordance: PlotAffordance,
    /// Provisional.
    pub provisional: bool,
}

/// Carrier for the lowering pass result. Contains the lowered
/// `DAG` plus the sidecar payloads that the emitter writes to
/// disk alongside `WORKFLOW.json`.
#[derive(Debug, Clone)]
pub struct BackendArtifact {
    /// The lowered `DAG`. Byte-compatible with today's
    /// `WORKFLOW.json` schema.
    pub dag: DAG,
    /// `runtime/proofs.jsonl` content. One line per edge proof;
    /// empty when `EmitContext.emit_proofs == false`.
    pub proofs_jsonl: String,
    /// `runtime/assumptions.jsonl` content. One line per
    /// assumption; empty when `WorkflowDag.assumptions` is empty
    /// or `EmitContext.emit_assumptions == false`.
    pub assumptions_jsonl: String,
    /// `runtime/plot_affordances.jsonl` content. One line per
    /// `PlotAffordanceRecord`; empty when
    /// `EmitContext.emit_affordances` is `None` or empty.
    ///
    /// Records are sorted by `(task_id, port_name)` for
    /// byte-determinism. The sidecar is excluded from the
    /// BagIt manifest and the verify-reproducibility byte-diff
    /// check (same discipline as proofs.jsonl and assumptions.jsonl).
    pub plot_affordances_jsonl: String,
    /// `runtime/affordance_fallbacks.jsonl` content. One line per
    /// `AffordanceFallbackRecord`; empty when
    /// `EmitContext.emit_fallbacks` is `None` or empty.
    ///
    /// Records are sorted by `(task_id, port_name)` for
    /// byte-determinism — same sort key as `plot_affordances_jsonl`.
    /// The sidecar is excluded from the BagIt manifest and the
    /// verify-reproducibility byte-diff check (already in
    /// `emitter.rs::walk_for_manifest`'s exclusion list).
    pub affordance_fallbacks_jsonl: String,
}

/// Lowering context. Controls whether sidecars are emitted (the
/// harness ignores them but the RO-Crate emitter consumes them).
#[derive(Debug, Clone, Default)]
pub struct EmitContext {
    /// Emit `runtime/proofs.jsonl`.
    pub emit_proofs: bool,
    /// Emit `runtime/assumptions.jsonl`.
    pub emit_assumptions: bool,
    /// Workflow JSON schema version label (`"1"` is today's
    /// shape). Set explicitly for byte-stability.
    pub workflow_version: String,
    /// Pre-resolved affordance records to write to
    /// `runtime/plot_affordances.jsonl`. When `None`, the sidecar
    /// is omitted entirely. When `Some(_)`, even an empty `Vec`
    /// writes an empty sidecar file (allowing the RO-Crate emitter
    /// to register the entity unconditionally on the presence of
    /// the file).
    ///
    /// The resolution call (walking the `PlotAffordanceRegistry` per
    /// output port) is not yet wired at lowering time — it will be
    /// threaded in by the atom-catalog lowering. Callers that have
    /// already resolved affordances (e.g. test helpers) pass them
    /// here; all existing call-sites pass `None` and get the same
    /// `BackendArtifact` shape they had before with
    /// `plot_affordances_jsonl` set to an empty string.
    pub emit_affordances: Option<Vec<PlotAffordanceRecord>>,
    /// Structural-fallback records to write to
    /// `runtime/affordance_fallbacks.jsonl`. When `None`, the sidecar
    /// is omitted entirely. When `Some(_)`, even an empty `Vec`
    /// writes an empty sidecar file.
    ///
    /// Populated from a session-scoped `AffordanceFallbackCounter`
    /// when the affordance resolver is wired at lowering time. All
    /// existing call-sites pass `None` and get
    /// `affordance_fallbacks_jsonl` set to an empty string.
    pub emit_fallbacks: Option<Vec<AffordanceFallbackRecord>>,
    /// SME-authored authorizations for the
    /// `UnsupportedConstraint`s the backend reports. When the
    /// emit-time check finds any unauthorized loss, the emit
    /// pipeline fails with `CompileError::SemanticLossNotAuthorized`.
    /// Empty (the default) is correct for `WorkflowJsonEmitter` —
    /// the custom harness preserves every IR constraint so its
    /// capability report is unconditionally empty. Not currently
    /// serialized (`EmitContext` is the in-process lowering carrier,
    /// not an on-disk policy file); a future ADR introducing per-emit
    /// authorization persistence will derive Serialize/Deserialize at
    /// that point.
    pub authorized_losses: Vec<ConstraintLossAck>,
}

impl EmitContext {
    /// Defaults.
    pub fn defaults() -> Self {
        Self {
            emit_proofs: true,
            emit_assumptions: true,
            workflow_version: "1".into(),
            emit_affordances: None,
            emit_fallbacks: None,
            authorized_losses: vec![],
        }
    }
}

/// Lower a `WorkflowDag` to a `WORKFLOW.json`-shaped `DAG`.
pub fn lower_to_workflow_json(
    dag: &WorkflowDag,
    ctx: &EmitContext,
) -> Result<BackendArtifact, EmitError> {
    let mut tasks: BTreeMap<TaskId, Task> = BTreeMap::new();

    // Build incoming-edge index for depends_on. Sorted by
    // BTreeMap iteration order so output is byte-stable.
    let mut incoming: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for edge in &dag.edges {
        incoming
            .entry(edge.to_node.clone())
            .or_default()
            .push(edge.from_node.clone());
    }
    for v in incoming.values_mut() {
        v.sort();
        v.dedup();
    }

    // Lower nodes in stable id order.
    let mut sorted_nodes: Vec<&TaskNode> = dag.nodes.iter().collect();
    sorted_nodes.sort_by(|a, b| a.id.cmp(&b.id));

    for node in sorted_nodes {
        let depends_on: Vec<TaskId> = incoming
            .remove(&node.id)
            .unwrap_or_default()
            .into_iter()
            .map(TaskId::from)
            .collect();
        let task = lower_task(node, depends_on)?;
        tasks.insert(TaskId::from(node.id.as_str()), task);
    }

    // Defensive orphan-branch repair (raw_qc / per_perturbation_pseudobulk_de
    // / multiome_arc_demultiplex style strands). When the upstream
    // `wire_dangling_analytical_atoms_to_reporting` pass misses an
    // analytical atom — typically because pruning happened AFTER the
    // wire-dangling pass ran, or because the lift path didn't reach it
    // — we still see `raw_qc → (only validate_raw_qc)` islands in
    // emitted DAGs. The downstream-audit gate `unreached:raw_qc` then
    // fires. This pass closes the loop at the lowest lowering layer:
    // any analytical task whose only consumers are validate_*/discover_*
    // gets its id appended to the canonical reporting node's
    // depends_on. Pure additions, idempotent, cycle-safe.
    repair_orphan_analytical_strands(&mut tasks);

    let dag_out = DAG {
        version: ctx.workflow_version.clone(),
        schema_version: crate::dag::current_dag_schema_version(),
        workflow_id: dag.id.clone(),
        current_task: None,
        tasks,
        reverse_deps: BTreeMap::new(),
        run_id: None,
    };

    let proofs_jsonl = if ctx.emit_proofs {
        emit_proofs_jsonl(&dag.edges)
    } else {
        String::new()
    };

    let assumptions_jsonl = if ctx.emit_assumptions {
        emit_assumptions_jsonl(dag)
    } else {
        String::new()
    };

    let plot_affordances_jsonl = match &ctx.emit_affordances {
        Some(records) => emit_plot_affordances_jsonl(records),
        None => String::new(),
    };

    let affordance_fallbacks_jsonl = match &ctx.emit_fallbacks {
        Some(records) => emit_affordance_fallbacks_jsonl(records),
        None => String::new(),
    };

    Ok(BackendArtifact {
        dag: dag_out,
        proofs_jsonl,
        assumptions_jsonl,
        plot_affordances_jsonl,
        affordance_fallbacks_jsonl,
    })
}

/// Walk the lowered `tasks` map. For each analytical task that has no
/// non-validate / non-discover consumer (i.e. it's a strand whose only
/// downstream is its own QA companion), append it to the canonical
/// reporting node's `depends_on` so its output flows into the final
/// SME report.
///
/// Conservative rules:
/// - Skip tasks whose id starts with `validate_` / `discover_` /
///   `adapter_` (QA / adapter probes are leaves by design).
/// - Skip tasks that are themselves reporting terminals.
/// - Skip when no canonical reporting target exists in the DAG
///   (`reporting` / `final_reporting` / `generic_summary`).
/// - Skip if the proposed edge already exists.
/// - Skip if adding the edge would form a cycle (the reporting node
///   transitively reaches the strand already — typical for a misorder
///   in the upstream chain).
///
/// Idempotent. Only adds edges, never removes. Determinism: BTreeMap
/// iteration order is stable; the `dedup` after `sort` collapses
/// duplicate adds.
fn repair_orphan_analytical_strands(tasks: &mut BTreeMap<TaskId, Task>) {
    // Pick the canonical reporting target. Prefer `reporting` (the
    // intermediate report) over `final_reporting` (which then
    // aggregates `reporting`). Falls back to `generic_summary` for the
    // time_series_forecast / generic_omics archetypes.
    let reporting_target: TaskId = if tasks.contains_key(&TaskId::from("reporting")) {
        TaskId::from("reporting")
    } else if tasks.contains_key(&TaskId::from("generic_summary")) {
        TaskId::from("generic_summary")
    } else if tasks.contains_key(&TaskId::from("final_reporting")) {
        TaskId::from("final_reporting")
    } else {
        return;
    };

    // Build reverse adjacency: for each task, the set of tasks that
    // have it in depends_on.
    let mut consumers: BTreeMap<TaskId, Vec<TaskId>> = BTreeMap::new();
    for (tid, t) in tasks.iter() {
        for dep in &t.depends_on {
            consumers.entry(dep.clone()).or_default().push(tid.clone());
        }
    }

    let task_ids: Vec<TaskId> = tasks.keys().cloned().collect();
    let mut to_add_to_reporting: Vec<TaskId> = Vec::new();

    let is_qa = |id: &str| -> bool {
        id.starts_with("validate_") || id.starts_with("discover_") || id.starts_with("adapter_")
    };
    let is_reporting = |id: &str| -> bool {
        id == "reporting"
            || id == "final_reporting"
            || id == "generic_summary"
            || id.ends_with("_reporting")
            || id.ends_with("_thematic_comparison")
            || id.ends_with("_final_reporting")
    };

    for tid in &task_ids {
        let tid_str = tid.as_str();
        if is_qa(tid_str) || is_reporting(tid_str) {
            continue;
        }
        // Strand-check: does this task have any non-QA consumer?
        let cs = consumers.get(tid).map(|v| v.as_slice()).unwrap_or(&[]);
        let has_real_consumer = cs.iter().any(|c| !is_qa(c.as_str()) && c != tid);
        if has_real_consumer {
            continue;
        }
        // Cycle-safety: if reporting transitively reaches this task,
        // wiring it back would close a cycle. Walk reporting's
        // depends_on transitively.
        if reaches_via_deps(&reporting_target, tid, tasks) {
            continue;
        }
        // Idempotency: skip if already there.
        if tasks
            .get(&reporting_target)
            .is_some_and(|t| t.depends_on.contains(tid))
        {
            continue;
        }
        to_add_to_reporting.push(tid.clone());
    }

    if to_add_to_reporting.is_empty() {
        return;
    }
    if let Some(report) = tasks.get_mut(&reporting_target) {
        for id in to_add_to_reporting {
            report.depends_on.push(id);
        }
        report.depends_on.sort();
        report.depends_on.dedup();
    }
}

/// Transitive-reachability check: does `start` reach `target` via
/// `depends_on` chains within `tasks`? Used to skip orphan-repair
/// edges that would close a cycle.
fn reaches_via_deps(start: &TaskId, target: &TaskId, tasks: &BTreeMap<TaskId, Task>) -> bool {
    if start == target {
        return true;
    }
    let mut seen: std::collections::BTreeSet<TaskId> = std::collections::BTreeSet::new();
    let mut stack: Vec<TaskId> = vec![start.clone()];
    while let Some(cur) = stack.pop() {
        if !seen.insert(cur.clone()) {
            continue;
        }
        if &cur == target {
            return true;
        }
        if let Some(t) = tasks.get(&cur) {
            for d in &t.depends_on {
                if !seen.contains(d) {
                    stack.push(d.clone());
                }
            }
        }
    }
    false
}

fn lower_task(node: &TaskNode, depends_on: Vec<TaskId>) -> Result<Task, EmitError> {
    let role = node
        .attributes
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("operation");
    let assignee_str = node
        .attributes
        .get("assignee")
        .and_then(|v| v.as_str())
        .unwrap_or("agent");
    let kind = lower_task_kind(role, node);
    let assignee = lower_assignee(assignee_str);

    let resource_class = node
        .attributes
        .get("resource_profile")
        .and_then(|v| v.get("cpu"))
        .and_then(|v| v.as_str())
        .map(|cpu| match cpu {
            "very_heavy" | "heavy" => ResourceClass::MemoryHeavy,
            _ => ResourceClass::CpuHeavy,
        })
        .unwrap_or_default();

    let requires_sme_review = matches!(node.implementation, Implementation::ManualProtocol { .. })
        || node
            .preconditions
            .iter()
            .any(|c| c.id == "requires_sme_review");

    // `TaskNode.validators` declares the validation
    // obligation ids the harness runs after task completion. Thread
    // them through onto every required artifact so the harness's
    // post-completion hook (`append_validation_reports_sidecar`) can
    // pick them up regardless of which artifact triggered the bundle.
    // Today's harness flattens across artifacts; once per-artifact
    // obligation routing exists, the splitting will happen in the
    // lowering pass instead.
    let validation_obligations: Vec<String> =
        node.validators.iter().map(|v| v.id.clone()).collect();

    let required_artifacts = node
        .attributes
        .get("required_artifacts")
        .and_then(|v| {
            serde_json::from_value::<Vec<crate::taxonomy::RequiredArtifactSpec>>(v.clone()).ok()
        })
        .map(|specs| {
            specs
                .into_iter()
                .map(|s| RequiredArtifact {
                    path: s.path,
                    min_size_bytes: s.min_size_bytes,
                    schema_ref: s.schema_ref,
                    validation_obligations: validation_obligations.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    // ContainerSpec.network is `#[deprecated]`; the back-compat emit
    // path constructs it with None so the field stays empty.
    #[allow(deprecated)]
    let container = match &node.implementation {
        Implementation::ContainerCommand { image, .. } => Some(crate::atom::ContainerSpec {
            image: image.image.clone(),
            tag: image.tag.clone(),
            digest: image.digest.clone(),
            arch: if image.arch.is_empty() {
                vec!["amd64".into()]
            } else {
                image.arch.clone()
            },
            gpu_required: image.gpu,
            network: None,
            source: crate::atom::ContainerSource::default(),
        }),
        _ => None,
    };

    // Build the per-task spec object. The agent reads task-spec.json
    // on its first turn and needs `required_figures` + `plot_stage_id`
    // to honour the Figures section of scripts/agent-prompts/task-execution.md.
    // The v4 planner stashed these on `node.attributes` when building
    // the TaskNode (see `composer_v4::planner` near the
    // `stage_id`/`atom_id` inserts) so the lowering pass can fold
    // them back into `Task.spec` without taking a runtime dep on the
    // AtomRegistry.
    let mut spec_map = serde_json::Map::new();
    if let Some(rf) = node.attributes.get("required_figures") {
        spec_map.insert("required_figures".into(), rf.clone());
    }
    if let Some(psid) = node.attributes.get("plot_stage_id") {
        spec_map.insert("plot_stage_id".into(), psid.clone());
    }
    if let Some(ea) = node.attributes.get("expected_artifacts") {
        spec_map.insert("expected_artifacts".into(), ea.clone());
    }
    // `stage_class` is the bare axis name (e.g. `peptide_search`,
    // `dimensionality_reduction`) used by the agent prompt's auto-approve
    // gate to match against `runtime/.sme-auto-approve-discoveries`.
    // Synthesized discover companions stamp this in
    // `composer_v4::discover_companion_synthesis::synthesize_discover_companions`.
    if let Some(sc) = node.attributes.get("stage_class") {
        spec_map.insert("stage_class".into(), sc.clone());
    }
    let spec = if spec_map.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(spec_map))
    };

    Ok(Task {
        kind,
        state: TaskState::Pending,
        depends_on,
        assignee,
        description: node.intent.clone(),
        spec,
        resolution: None,
        result_ref: None,
        resource_class,
        requires_sme_review,
        required_artifacts,
        container,
        // The v4 planner stores the underlying atom id on the
        // TaskNode's `attributes` map (key "atom_id") so the lowered
        // Task can recover the registry atom for per-task image
        // selection, safety enforcement, and plot-affordance lookup.
        source_atom_id: node
            .attributes
            .get("atom_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        safety: Default::default(),
    })
}

fn lower_task_kind(role: &str, node: &TaskNode) -> TaskKind {
    let parsed_role: AtomRole =
        serde_json::from_value(serde_json::json!(role)).unwrap_or(AtomRole::Operation);
    match parsed_role.default_behavior_class() {
        AtomRole::Discovery => {
            let kind_label = node
                .attributes
                .get("discovery_kind")
                .and_then(|v| v.as_str())
                .unwrap_or("custom");
            TaskKind::Discovery(match kind_label {
                "best_practice" => DiscoveryKind::BestPractice,
                "source" => DiscoveryKind::Source,
                "evidence" => DiscoveryKind::Evidence,
                "environment_probe" => DiscoveryKind::EnvironmentProbe,
                other => DiscoveryKind::Custom(other.to_string()),
            })
        }
        AtomRole::Validation => TaskKind::Validation,
        AtomRole::Aggregator => TaskKind::Gate,
        AtomRole::Selection => TaskKind::Review,
        _ => TaskKind::Computation,
    }
}

fn lower_assignee(s: &str) -> Assignee {
    let parsed: AtomAssignee =
        serde_json::from_value(serde_json::json!(s)).unwrap_or(AtomAssignee::Agent);
    match parsed {
        AtomAssignee::Agent => Assignee::Agent,
        AtomAssignee::Sme => Assignee::Sme,
    }
}

fn emit_proofs_jsonl(edges: &[EdgeContract]) -> String {
    let mut out = String::new();
    let mut sorted: Vec<&EdgeContract> = edges.iter().collect();
    sorted.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.from_port.cmp(&b.from_port))
            .then_with(|| a.to_node.cmp(&b.to_node))
            .then_with(|| a.to_port.cmp(&b.to_port))
    });
    for e in sorted {
        if let Ok(line) = serde_json::to_string(e) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

fn emit_assumptions_jsonl(dag: &WorkflowDag) -> String {
    let mut out = String::new();
    for entry in &dag.assumptions.entries {
        if let Ok(line) = serde_json::to_string(entry) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Emit `runtime/plot_affordances.jsonl` content. Records are sorted
/// by `(task_id, port_name)` for byte-determinism across runs.
fn emit_plot_affordances_jsonl(records: &[PlotAffordanceRecord]) -> String {
    let mut sorted: Vec<&PlotAffordanceRecord> = records.iter().collect();
    sorted.sort_by(|a, b| {
        a.task_id
            .cmp(&b.task_id)
            .then_with(|| a.port_name.cmp(&b.port_name))
    });
    let mut out = String::new();
    for rec in sorted {
        if let Ok(line) = serde_json::to_string(rec) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Emit `runtime/affordance_fallbacks.jsonl` content. Records are sorted
/// by `(task_id, port_name)` for byte-determinism — same sort key as
/// `emit_plot_affordances_jsonl` so the two sidecars can be
/// co-iterated cheaply.
fn emit_affordance_fallbacks_jsonl(records: &[AffordanceFallbackRecord]) -> String {
    let mut sorted: Vec<&AffordanceFallbackRecord> = records.iter().collect();
    sorted.sort_by(|a, b| {
        a.task_id
            .cmp(&b.task_id)
            .then_with(|| a.port_name.cmp(&b.port_name))
    });
    let mut out = String::new();
    for rec in sorted {
        if let Ok(line) = serde_json::to_string(rec) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Reconstruct a `WorkflowDag` from a
/// `BackendArtifact`, re-attaching sidecar-only fields
/// (compatibility proofs on edges, assumption ledger entries) from
/// the artifact's JSONL strings. Inverse of `lower_to_workflow_json`
/// for sidecar-bearing fields; `Task`-bearing fields go through
/// `dag_to_workflow_dag`.
///
/// The round-trip guarantee: for any `WorkflowDag` whose edges carry
/// `CompatibilityProof`s and whose `assumptions.entries` is
/// non-empty,
///
/// ```text
/// wf_dag
/// |> lower_to_workflow_json -> BackendArtifact { dag, proofs_jsonl, assumptions_jsonl,.. }
/// |> workflow_dag_from_artifact -> wf_dag'
/// ```
///
/// satisfies `wf_dag'.edges == wf_dag.edges` (proofs preserved) and
/// `wf_dag'.assumptions == wf_dag.assumptions` (ledger preserved),
/// modulo edge-order normalization (both passes sort by
/// `(from_node, from_port, to_node, to_port)`).
///
/// Port-level information that wasn't emitted into the `DAG` (e.g.
/// `inputs` / `outputs` declarations on a `TaskNode`) still drops on
/// the structural shell side because the lowering doesn't write
/// them into WORKFLOW.json. That's a separate plumbing gap tracked
/// under the same §9.4 row — the v3 P4 close is the **sidecar-only**
/// closure (proofs + assumptions), not full port-shape preservation.
pub fn workflow_dag_from_artifact(artifact: &BackendArtifact) -> WorkflowDag {
    use crate::workflow_contracts::evidence::Assumption;

    // Structural shell: nodes + topology from the DAG.
    let mut wf = dag_to_workflow_dag(&artifact.dag);

    // Re-attach proofs onto edges by parsing the JSONL. The JSONL was
    // produced by `emit_proofs_jsonl` over `dag.edges`, so each line
    // is a full `EdgeContract`. Replace the depends_on-synthesized
    // edges with the proof-bearing ones; keep the same sort order
    // (`(from_node, from_port, to_node, to_port)`) so the round-trip
    // stays byte-stable.
    if !artifact.proofs_jsonl.is_empty() {
        let mut edges: Vec<EdgeContract> = Vec::new();
        for line in artifact.proofs_jsonl.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(edge) = serde_json::from_str::<EdgeContract>(line) {
                edges.push(edge);
            }
        }
        edges.sort_by(|a, b| {
            a.from_node
                .cmp(&b.from_node)
                .then_with(|| a.from_port.cmp(&b.from_port))
                .then_with(|| a.to_node.cmp(&b.to_node))
                .then_with(|| a.to_port.cmp(&b.to_port))
        });
        wf.edges = edges;
    }

    // Re-attach assumption ledger entries.
    if !artifact.assumptions_jsonl.is_empty() {
        let mut entries: Vec<Assumption> = Vec::new();
        for line in artifact.assumptions_jsonl.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<Assumption>(line) {
                entries.push(entry);
            }
        }
        wf.assumptions.entries = entries;
    }

    wf
}

/// Reconstruct a `WorkflowDag` shape from a lowered `DAG`.
/// Round-trip helper — recovers `Task`-bearing fields (id, intent /
/// description, depends_on, container, requires_sme_review). Sidecar
/// fields (compatibility proofs, assumption ledger, ranked
/// alternatives) are not in the DAG; the caller re-attaches them
/// from `proofs.jsonl` / `assumptions.jsonl` / etc. (see
/// `workflow_dag_from_artifact` for the §9.4 round-trip helper that
/// folds both passes in one call). Returns a `WorkflowDag` with
/// empty assumptions and edges synthesized from each task's
/// `depends_on` (one edge per parent), preserving the graph
/// topology but losing port-level information that wasn't emitted
/// into the DAG.
pub fn dag_to_workflow_dag(dag: &DAG) -> WorkflowDag {
    use crate::workflow_contracts::edge::CompatibilityProof;
    use crate::workflow_contracts::evidence::AssumptionLedger;
    use crate::workflow_contracts::task_node::TaskNode;

    let mut nodes: Vec<TaskNode> = Vec::with_capacity(dag.tasks.len());
    let mut edges: Vec<EdgeContract> = Vec::new();

    for (id, task) in &dag.tasks {
        let mut node = TaskNode::skeleton(id, &task.description);
        // Re-attach role attribute so subsequent re-lowering picks
        // the same TaskKind.
        let role_str = match &task.kind {
            crate::dag::TaskKind::Discovery(_) => "discovery",
            crate::dag::TaskKind::Validation => "validation",
            crate::dag::TaskKind::Gate => "aggregator",
            crate::dag::TaskKind::Review => "selection",
            _ => "operation",
        };
        node.attributes
            .insert("role".into(), serde_json::Value::String(role_str.into()));
        let assignee_str = match task.assignee {
            crate::dag::Assignee::Agent => "agent",
            crate::dag::Assignee::Sme => "sme",
            crate::dag::Assignee::AgentThenSme => "agent_then_sme",
        };
        node.attributes.insert(
            "assignee".into(),
            serde_json::Value::String(assignee_str.into()),
        );
        if let crate::dag::TaskKind::Discovery(disc) = &task.kind {
            let kind_str = match disc {
                crate::dag::DiscoveryKind::BestPractice => "best_practice".to_string(),
                crate::dag::DiscoveryKind::Source => "source".to_string(),
                crate::dag::DiscoveryKind::Evidence => "evidence".to_string(),
                crate::dag::DiscoveryKind::EnvironmentProbe => "environment_probe".to_string(),
                crate::dag::DiscoveryKind::Custom(name) => name.clone(),
            };
            node.attributes
                .insert("discovery_kind".into(), serde_json::Value::String(kind_str));
        }
        if let Some(c) = &task.container {
            node.implementation = Implementation::ContainerCommand {
                image: crate::workflow_contracts::implementation::OciImageRef {
                    image: c.image.clone(),
                    tag: c.tag.clone(),
                    digest: c.digest.clone(),
                    arch: c.arch.clone(),
                    gpu: c.gpu_required,
                },
                command_template: vec![],
            };
        } else if task.requires_sme_review {
            node.implementation = Implementation::ManualProtocol {
                sop_ref: format!("sop:{}", id),
            };
        }
        // `requires_sme_review` is an independent gate from the
        // implementation branch above. A containerised task can still
        // require SME sign-off before dispatch, and that obligation
        // must survive the round-trip through `WorkflowDag`. Emit it as
        // a hard precondition on the node so re-lowering and any
        // compatibility-engine pass observes the same gate.
        if task.requires_sme_review {
            node.preconditions
                .push(crate::workflow_contracts::port::Constraint {
                    id: "requires_sme_review".into(),
                    statement: "Task requires SME review before dispatch".into(),
                    expression: None,
                    severity: crate::workflow_contracts::port::ConstraintSeverity::Hard,
                });
        }
        nodes.push(node);

        // Synthesize edges from depends_on.
        for parent_id in &task.depends_on {
            edges.push(EdgeContract {
                from_node: parent_id.to_string(),
                from_port: "out".into(),
                to_node: id.to_string(),
                to_port: "in".into(),
                proof: CompatibilityProof::default(),
                chain_of_custody: None,
            });
        }
    }

    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    edges.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.to_node.cmp(&b.to_node))
    });

    WorkflowDag {
        id: dag.workflow_id.clone(),
        nodes,
        edges,
        assumptions: AssumptionLedger::default(),
        source_template: None,
    }
}

/// WORKFLOW.json target emitter.
///
/// R2-N21 — `emit`/`compile` were originally trait
/// methods on a `BackendEmitter` trait; that trait had exactly one
/// production impl (this one) and was deleted as forced abstraction.
/// The methods stayed inherent on the concrete type so the call
/// sites (`emitter.compile(...)`) remained identical.
#[derive(Debug, Clone, Default)]
pub struct WorkflowJsonEmitter;

impl WorkflowJsonEmitter {
    /// Backend identifier — stamped into `BackendCapabilityReport.backend`
    /// and the audit trail.
    pub fn name(&self) -> &'static str {
        "workflow_json"
    }

    /// Lower the IR `WorkflowDag` into a `BackendArtifact` (the
    /// `WORKFLOW.json` carrier shape + sidecar strings). Pure, no IO.
    pub fn emit(&self, dag: &WorkflowDag, ctx: &EmitContext) -> Result<BackendArtifact, EmitError> {
        lower_to_workflow_json(dag, ctx)
    }

    /// Emit-with-capability-report. The WORKFLOW.json
    /// backend (custom harness) consumes every IR constraint natively,
    /// so its `BackendCapabilityReport` is unconditionally empty. The day
    /// an external emitter (CWL / WDL / Nextflow / etc.) ships, that
    /// emitter populates `losses` honestly and the F17 contract gate at
    /// the call site refuses unauthorized losses.
    pub fn compile(
        &self,
        dag: &WorkflowDag,
        ctx: &EmitContext,
    ) -> Result<(BackendArtifact, BackendCapabilityReport), CompileError> {
        let artifact = self.emit(dag, ctx)?;
        let report = BackendCapabilityReport {
            backend: "workflow_json".into(),
            losses: vec![],
        };
        Ok((artifact, report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::edge::CompatibilityProof;
    use crate::workflow_contracts::evidence::{
        Assumption, AssumptionLedger, AssumptionResolution, AssumptionSource, RiskClass,
    };
    use crate::workflow_contracts::implementation::OciImageRef;
    use crate::workflow_contracts::semantic_type::SemanticType;

    fn align_node() -> TaskNode {
        let mut n = TaskNode::skeleton("align_reads", "Align reads");
        n.implementation = Implementation::ContainerCommand {
            image: OciImageRef {
                image: "ghcr.io/scripps/bio-base".into(),
                tag: "v0.4.0".into(),
                digest: "sha256:abc".into(),
                arch: vec!["amd64".into()],
                gpu: false,
            },
            command_template: vec![],
        };
        n.attributes
            .insert("role".into(), serde_json::Value::String("operation".into()));
        n.attributes
            .insert("assignee".into(), serde_json::Value::String("agent".into()));
        n
    }

    fn quantify_node() -> TaskNode {
        let mut n = TaskNode::skeleton("quantify_features", "Count features");
        n.attributes
            .insert("role".into(), serde_json::Value::String("operation".into()));
        n.attributes
            .insert("assignee".into(), serde_json::Value::String("agent".into()));
        n
    }

    fn simple_dag() -> WorkflowDag {
        WorkflowDag {
            id: "test_dag".into(),
            nodes: vec![align_node(), quantify_node()],
            edges: vec![EdgeContract {
                from_node: "align_reads".into(),
                from_port: "bam".into(),
                to_node: "quantify_features".into(),
                to_port: "bam".into(),
                proof: CompatibilityProof {
                    producer_type: "data:0863".into(),
                    consumer_type: "data:0863".into(),
                    ..Default::default()
                },
                chain_of_custody: None,
            }],
            assumptions: AssumptionLedger::default(),
            source_template: None,
        }
    }

    #[test]
    fn lowering_produces_two_tasks() {
        let dag = simple_dag();
        let result = lower_to_workflow_json(&dag, &EmitContext::defaults()).unwrap();
        assert_eq!(result.dag.tasks.len(), 2);
        assert!(result.dag.tasks.contains_key("align_reads"));
        assert!(result.dag.tasks.contains_key("quantify_features"));
    }

    #[test]
    fn depends_on_threads_through_edges() {
        let dag = simple_dag();
        let result = lower_to_workflow_json(&dag, &EmitContext::defaults()).unwrap();
        let q = result.dag.tasks.get("quantify_features").unwrap();
        assert_eq!(q.depends_on, vec![TaskId::from("align_reads")]);
        let a = result.dag.tasks.get("align_reads").unwrap();
        assert!(a.depends_on.is_empty());
    }

    #[test]
    fn container_command_lowers_to_container_field() {
        let dag = simple_dag();
        let result = lower_to_workflow_json(&dag, &EmitContext::defaults()).unwrap();
        let a = result.dag.tasks.get("align_reads").unwrap();
        assert!(a.container.is_some());
        let c = a.container.as_ref().unwrap();
        assert_eq!(c.image, "ghcr.io/scripps/bio-base");
        assert_eq!(c.tag, "v0.4.0");
        assert_eq!(c.digest, "sha256:abc");
    }

    #[test]
    fn unimplemented_node_lowers_to_no_container() {
        let dag = simple_dag();
        let result = lower_to_workflow_json(&dag, &EmitContext::defaults()).unwrap();
        let q = result.dag.tasks.get("quantify_features").unwrap();
        assert!(q.container.is_none());
    }

    #[test]
    fn manual_protocol_sets_requires_sme_review() {
        let mut node = TaskNode::skeleton("sme_step", "SME action");
        node.implementation = Implementation::ManualProtocol {
            sop_ref: "sop-001".into(),
        };
        node.attributes
            .insert("assignee".into(), serde_json::Value::String("sme".into()));
        let dag = WorkflowDag {
            id: "x".into(),
            nodes: vec![node],
            edges: vec![],
            assumptions: AssumptionLedger::default(),
            source_template: None,
        };
        let result = lower_to_workflow_json(&dag, &EmitContext::defaults()).unwrap();
        let s = result.dag.tasks.get("sme_step").unwrap();
        assert!(s.requires_sme_review);
        assert_eq!(s.assignee, Assignee::Sme);
    }

    #[test]
    fn proofs_sidecar_is_jsonl_per_edge() {
        let dag = simple_dag();
        let result = lower_to_workflow_json(&dag, &EmitContext::defaults()).unwrap();
        let line_count = result
            .proofs_jsonl
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        assert_eq!(line_count, 1);
    }

    #[test]
    fn assumptions_sidecar_emits_one_line_per_entry() {
        let mut dag = simple_dag();
        dag.assumptions.entries.push(Assumption {
            id: "a_1".into(),
            statement: "Reads are unstranded".into(),
            source: AssumptionSource::LlmInferred {
                confidence: "low".into(),
            },
            affects_nodes: vec!["quantify_features".into()],
            risk: RiskClass::Moderate,
            resolution: AssumptionResolution::Unresolved,
            chain_of_custody: None,
        });
        let result = lower_to_workflow_json(&dag, &EmitContext::defaults()).unwrap();
        let lines = result
            .assumptions_jsonl
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        assert_eq!(lines, 1);
    }

    #[test]
    fn lowering_is_deterministic() {
        let dag = simple_dag();
        let r1 = lower_to_workflow_json(&dag, &EmitContext::defaults()).unwrap();
        let r2 = lower_to_workflow_json(&dag, &EmitContext::defaults()).unwrap();
        let j1 = serde_json::to_string(&r1.dag).unwrap();
        let j2 = serde_json::to_string(&r2.dag).unwrap();
        assert_eq!(j1, j2);
        assert_eq!(r1.proofs_jsonl, r2.proofs_jsonl);
    }

    #[test]
    fn workflow_version_threads_through() {
        let dag = simple_dag();
        let mut ctx = EmitContext::defaults();
        ctx.workflow_version = "2-test".into();
        let result = lower_to_workflow_json(&dag, &ctx).unwrap();
        assert_eq!(result.dag.version, "2-test");
    }

    #[test]
    fn emit_proofs_disabled_yields_empty_sidecar() {
        let dag = simple_dag();
        let mut ctx = EmitContext::defaults();
        ctx.emit_proofs = false;
        let result = lower_to_workflow_json(&dag, &ctx).unwrap();
        assert!(result.proofs_jsonl.is_empty());
    }

    #[test]
    fn workflow_json_emitter_implements_trait() {
        let emitter = WorkflowJsonEmitter::default();
        assert_eq!(emitter.name(), "workflow_json");
        let dag = simple_dag();
        let result = emitter.emit(&dag, &EmitContext::defaults()).unwrap();
        assert_eq!(result.dag.tasks.len(), 2);
    }

    #[test]
    fn discovery_role_lowers_to_discovery_task_kind() {
        let mut node = TaskNode::skeleton("discover_method", "Pick best method");
        node.attributes
            .insert("role".into(), serde_json::Value::String("discovery".into()));
        node.attributes.insert(
            "discovery_kind".into(),
            serde_json::Value::String("best_practice".into()),
        );
        let dag = WorkflowDag {
            id: "x".into(),
            nodes: vec![node],
            edges: vec![],
            assumptions: AssumptionLedger::default(),
            source_template: None,
        };
        let result = lower_to_workflow_json(&dag, &EmitContext::defaults()).unwrap();
        let t = result.dag.tasks.get("discover_method").unwrap();
        assert!(matches!(
            t.kind,
            TaskKind::Discovery(DiscoveryKind::BestPractice)
        ));
    }

    /// Use the data-product-contract semantic type just to keep the
    /// import alive; ensures the public surface compiles cleanly.
    #[test]
    fn semantic_type_module_link() {
        let _ = SemanticType::edam("data:0863", "");
    }

    /// WorkflowDag -> DAG -> WorkflowDag reconstruction recovers
    /// Task-bearing fields. Sidecar-only
    /// fields (compatibility proofs, assumption ledger) are not
    /// reconstructed by the inverse helper; they live in their
    /// Adjacent.jsonl files.
    #[test]
    fn round_trip_recovers_task_bearing_fields() {
        let original = simple_dag();
        let lowered = lower_to_workflow_json(&original, &EmitContext::defaults()).unwrap();
        let reconstructed = dag_to_workflow_dag(&lowered.dag);

        // Top-level id round-trips.
        assert_eq!(reconstructed.id, original.id);

        // Node count matches.
        assert_eq!(reconstructed.nodes.len(), original.nodes.len());

        // Each node has the same id + intent (description) + role.
        for orig in &original.nodes {
            let recon = reconstructed
                .nodes
                .iter()
                .find(|n| n.id == orig.id)
                .unwrap_or_else(|| panic!("missing node {} after round-trip", orig.id));
            assert_eq!(recon.intent, orig.intent, "intent drifted for {}", orig.id);
        }

        // Edge topology preserved (depends_on derived from from_node ->
        // to_node parent links).
        assert_eq!(reconstructed.edges.len(), original.edges.len());
        for orig in &original.edges {
            assert!(
                reconstructed
                    .edges
                    .iter()
                    .any(|e| e.from_node == orig.from_node && e.to_node == orig.to_node),
                "edge {} -> {} missing after round-trip",
                orig.from_node,
                orig.to_node
            );
        }
    }

    /// Re-lowering the round-tripped WorkflowDag produces the same
    /// DAG (modulo sidecar payloads).
    #[test]
    fn double_round_trip_is_byte_stable_for_dag_fields() {
        let original = simple_dag();
        let first_lower = lower_to_workflow_json(&original, &EmitContext::defaults()).unwrap();
        let reconstructed = dag_to_workflow_dag(&first_lower.dag);
        let second_lower =
            lower_to_workflow_json(&reconstructed, &EmitContext::defaults()).unwrap();

        let j1 = serde_json::to_string(&first_lower.dag).unwrap();
        let j2 = serde_json::to_string(&second_lower.dag).unwrap();
        assert_eq!(j1, j2, "WorkflowDag -> DAG -> WorkflowDag -> DAG drifted");
    }

    /// Container TaskNode survives both directions.
    #[test]
    fn container_round_trips_through_dag() {
        let dag = simple_dag();
        let lowered = lower_to_workflow_json(&dag, &EmitContext::defaults()).unwrap();
        let reconstructed = dag_to_workflow_dag(&lowered.dag);
        let align = reconstructed
            .nodes
            .iter()
            .find(|n| n.id == "align_reads")
            .unwrap();
        match &align.implementation {
            Implementation::ContainerCommand { image, .. } => {
                assert_eq!(image.image, "ghcr.io/scripps/bio-base");
                assert_eq!(image.tag, "v0.4.0");
                assert_eq!(image.digest, "sha256:abc");
            }
            other => panic!("expected ContainerCommand, got {other:?}"),
        }
    }

    /// Manual-protocol assignee=Sme survives the round-trip via
    /// requires_sme_review re-derivation.
    #[test]
    fn manual_protocol_round_trips_through_dag() {
        let mut node = TaskNode::skeleton("sme_step", "SME action");
        node.implementation = Implementation::ManualProtocol {
            sop_ref: "sop-001".into(),
        };
        node.attributes
            .insert("assignee".into(), serde_json::Value::String("sme".into()));
        let dag = WorkflowDag {
            id: "x".into(),
            nodes: vec![node],
            edges: vec![],
            assumptions: AssumptionLedger::default(),
            source_template: None,
        };
        let lowered = lower_to_workflow_json(&dag, &EmitContext::defaults()).unwrap();
        let reconstructed = dag_to_workflow_dag(&lowered.dag);
        let recon = reconstructed
            .nodes
            .iter()
            .find(|n| n.id == "sme_step")
            .unwrap();
        assert!(matches!(
            recon.implementation,
            Implementation::ManualProtocol { .. }
        ));
    }

    /// Re-emitting the same WorkflowDag against the same registry
    /// snapshot is byte-identical 100x in a row (determinism contract).
    #[test]
    fn lowering_byte_stable_across_100_replays() {
        let dag = simple_dag();
        let first = serde_json::to_string(
            &lower_to_workflow_json(&dag, &EmitContext::defaults())
                .unwrap()
                .dag,
        )
        .unwrap();
        for i in 0..100 {
            let result = lower_to_workflow_json(&dag, &EmitContext::defaults()).unwrap();
            let json = serde_json::to_string(&result.dag).unwrap();
            assert_eq!(first, json, "byte-stability lost at iteration {i}");
        }
    }
}
