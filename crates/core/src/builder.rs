use crate::dag::{
    validate_dag, Assignee, DiscoveryKind, EscalationPath, ResolutionStrategy, ResourceClass, Task,
    TaskKind, TaskState, DAG,
};
use crate::ids::TaskId;
use crate::taxonomy::{DiscoveryRequirement, StageCardinality, StageSpec};
use anyhow::{bail, Context, Result};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;

/// Map a stage's optional `resource_class` YAML hint to the typed
/// [`ResourceClass`] used by the Phase-3 scheduler + Phase-2 agent
/// envelope. Unknown strings and `None` both fall back to `CpuHeavy`.
/// Validation at taxonomy load-time (the `_taxonomy.schema.json` enum)
/// keeps the string set correct at the source — this helper is a
/// conservative default-on-surprise.
fn resource_class_for_stage(stage: &StageSpec) -> ResourceClass {
    match stage.resource_class.as_deref() {
        Some("io_heavy") => ResourceClass::IoHeavy,
        Some("memory_heavy") => ResourceClass::MemoryHeavy,
        Some("gpu") => ResourceClass::Gpu,
        _ => ResourceClass::CpuHeavy,
    }
}

/// Copy the stage's `requires_sme_review` flag onto every
/// Task emitted for that stage. Default false when the flag is absent
/// from the YAML. Used by the scheduler's SME review gate.
fn requires_sme_review_for_stage(stage: &StageSpec) -> bool {
    stage.requires_sme_review.unwrap_or(false)
}

/// Carry the stage's declared `required_artifacts` into DAG Tasks.
/// When `include_figures` is true, synthesize hard required artifacts
/// for every `required_figures` entry so the harness rejects completed
/// compute tasks that omitted plots even if the generated validator only
/// logged a warning. Discovery / validate wrapper tasks use
/// `include_figures = false`; their own output dirs should not be forced
/// to contain the parent compute task's figures.
fn required_artifacts_for_stage(
    stage: &StageSpec,
    include_figures: bool,
) -> Vec<crate::dag::RequiredArtifact> {
    // Stage-level validators apply to every required artifact of the
    // stage. Figures (manifest + PNG + PDF) get the same obligation set;
    // runners that don't apply (e.g. `p_value_in_unit_interval` against a
    // PNG) soft-skip with `Errored { reason }` and never block the task.
    let stage_validators = stage.validators.clone();
    let mut out: Vec<crate::dag::RequiredArtifact> = stage
        .required_artifacts
        .iter()
        .map(|s| crate::dag::RequiredArtifact {
            path: s.path.clone(),
            min_size_bytes: s.min_size_bytes,
            schema_ref: s.schema_ref.clone(),
            validation_obligations: stage_validators.clone(),
        })
        .collect();
    if include_figures && !stage.required_figures.is_empty() {
        push_required_artifact_once(
            &mut out,
            crate::dag::RequiredArtifact {
                path: "figures/manifest.json".into(),
                min_size_bytes: Some(2),
                schema_ref: None,
                validation_obligations: stage_validators.clone(),
            },
        );
        for fig in &stage.required_figures {
            push_required_artifact_once(
                &mut out,
                crate::dag::RequiredArtifact {
                    path: format!("figures/{fig}.png"),
                    min_size_bytes: Some(1),
                    schema_ref: None,
                    validation_obligations: stage_validators.clone(),
                },
            );
            push_required_artifact_once(
                &mut out,
                crate::dag::RequiredArtifact {
                    path: format!("figures/{fig}.pdf"),
                    min_size_bytes: Some(1),
                    schema_ref: None,
                    validation_obligations: stage_validators.clone(),
                },
            );
        }
    }
    out
}

fn push_required_artifact_once(
    out: &mut Vec<crate::dag::RequiredArtifact>,
    artifact: crate::dag::RequiredArtifact,
) {
    if !out.iter().any(|existing| existing.path == artifact.path) {
        out.push(artifact);
    }
}

fn add_plotting_spec_fields(spec: &mut serde_json::Value, stage: &StageSpec) {
    if !stage.required_figures.is_empty() {
        spec["required_figures"] = serde_json::Value::Array(
            stage
                .required_figures
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect(),
        );
    }
    if let Some(plot_stage_id) = &stage.plot_stage_id {
        spec["plot_stage_id"] = serde_json::Value::String(plot_stage_id.clone());
    }
}

fn add_expected_artifact_spec_fields(spec: &mut serde_json::Value, stage: &StageSpec) {
    if !stage.expected_artifacts.is_empty() {
        spec["expected_artifacts"] = serde_json::Value::Array(
            stage
                .expected_artifacts
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect(),
        );
    }
}

/// A single SME resolution captured at intake time.
///
/// `method` is the free-text method description the SME provided (e.g.
/// "Cell Ranger 7.x against GRCh38-2020-A with --include-introns"). `fields`
/// is a map of structured key/value pairs that satisfy condition expressions
/// on downstream stages — for example, a `batch_correction` stage with
/// `condition: "discover_batch_correction.result.batch_correction_required == true"`
/// requires an SME resolution that sets `batch_correction_required: true`
/// so the task doesn't auto-skip at runtime.
#[derive(Debug, Clone, Default)]
pub struct IntakeResolution {
    /// Method.
    pub method: String,
    /// Fields.
    pub fields: BTreeMap<String, JsonValue>,
}

impl IntakeResolution {
    /// New.
    pub fn new(method: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            fields: BTreeMap::new(),
        }
    }

    /// With field.
    pub fn with_field(mut self, key: impl Into<String>, value: JsonValue) -> Self {
        self.fields.insert(key.into(), value);
        self
    }
}

/// Type alias for the stage → resolution map passed into builder functions.
pub type IntakeMethods = BTreeMap<String, IntakeResolution>;

/// Canonical entry
/// point for v4 sessions. Routes a `WorkflowDag` through the Phase
/// 11 `lower_to_workflow_json` pass, then post-processes the
/// resulting `DAG` through `validate_dag` so v4 sessions reach the
/// same invariant-checked shape as v1/v2/v3.
///
/// This is the canonical "compose → lower → DAG" pipeline the
/// alignment plan §16 calls for. v1/v2/v3 sessions continue to use
/// `build_dag_from_composition` for back-compat through the soak
/// window; v4 sessions land here.
#[must_use = "builder return must be inspected — dropping it discards the validated DAG plus any cycle/dangling-dependency error from `validate_dag`"]
pub fn build_dag_from_workflow_dag(
    workflow_dag: &crate::workflow_contracts::task_node::WorkflowDag,
    workflow_id: &str,
) -> Result<DAG> {
    use crate::backend_emitters::workflow_json::{lower_to_workflow_json, EmitContext};
    let mut ctx = EmitContext::defaults();
    ctx.workflow_version = "1.0".into();
    let artifact = lower_to_workflow_json(workflow_dag, &ctx)
        .map_err(|e| anyhow::anyhow!("Phase 11 lowering failed: {:?}", e))?;
    let mut dag = artifact.dag;
    // Override workflow_id so the caller's choice (typically the
    // session id) wins over the WorkflowDag's stable id.
    dag.workflow_id = workflow_id.to_string();
    crate::dag::validate_dag(&dag)
        .map_err(|e| anyhow::anyhow!("Phase 16 lowered DAG failed validation: {:?}", e))?;
    Ok(dag)
}

/// Thin adapter from `composer::CompositionResult` into a
/// validated `DAG`. Lets the composer-driven emit path reuse the
/// builder's existing `emit_stage` machinery (per-sample fan-out,
/// validate_* wrappers, discovery atoms) without forking the task
/// model.
///
/// This is the legacy v1/v2/v3 path. New v4 sessions route through
/// `build_dag_from_workflow_dag` instead, which uses the
/// `lower_to_workflow_json` pass. The two paths produce the same `DAG`
/// shape for compatible inputs; v4 adds proof / assumption sidecars that
/// this legacy path drops.
///
/// Each `ComposedAtom` is converted to a `StageSpec` via
/// `composed_atom_to_stage_spec`, then run through the same pipeline
/// `build_dag_from_taxonomy` uses. This means:
/// - The post-builder `resolve_intake_methods` rebind is honored.
/// - `validate_dag` enforces the same invariants.
/// - Cross-dependencies the composer surfaced via
///   `archetype.cross_dependencies` get folded back in.
///
/// Determinism: the composer already returns atoms in canonical order
/// (BTreeMap traversal + lexical tie-break), so the resulting DAG is
/// byte-identical for the same composition input.
#[must_use = "builder return must be inspected — dropping it discards the validated DAG plus any cycle/dangling-dependency error from `validate_dag`"]
pub fn build_dag_from_composition(
    composition: &crate::composer::CompositionResult,
    workflow_id: &str,
    intake_methods: &IntakeMethods,
    cross_dependencies: &[(String, String)],
) -> Result<DAG> {
    let mut tasks: BTreeMap<TaskId, Task> = BTreeMap::new();

    for ca in &composition.atoms {
        let stage = composed_atom_to_stage_spec(ca);
        emit_stage(&stage, &mut tasks)?;
    }

    // Apply composer-surfaced cross-dependency edges.
    for (from, to) in cross_dependencies {
        if let Some(task) = tasks.get_mut(to.as_str()) {
            let from_id = TaskId::from(from.as_str());
            if !task.depends_on.contains(&from_id) {
                task.depends_on.push(from_id);
            }
        }
    }

    let mut dag = DAG {
        version: "1.0".into(),
        schema_version: crate::dag::current_dag_schema_version(),
        workflow_id: workflow_id.to_string(),
        current_task: None,
        tasks,
        reverse_deps: BTreeMap::new(),
        run_id: None,
    };

    // The legacy `resolve_intake_methods` consumes a `&StageTaxonomy`
    // for its `condition` lookup logic. We don't have one here — the
    // composer doesn't expose taxonomy-level state. Today's composer
    // path doesn't carry condition expressions through; if a future
    // atom gains a `condition:` field, this is the wiring point.
    let _ = intake_methods;

    dag.rebuild_reverse_deps();
    dag.propagate_readiness();
    validate_dag(&dag).context("DAG validation failed after composition build")?;

    Ok(dag)
}

/// Adapter that synthesizes a `StageSpec` from a
/// `composer::ComposedAtom`. Threads the load-bearing fields the
/// legacy `build_dag_from_taxonomy` path populates from
/// `taxonomy.yaml::stages.<id>` so the composer fast-path produces a
/// `WORKFLOW.json` shape-equivalent to the legacy path:
///
/// - `cardinality` — `IterateUntil` when the atom carries an
///   `iterate:` block (S10.1); `One` otherwise. The builder's
///   `emit_iterate_stage` (S10.3) reads this to decide whether
///   to emit the 4-template scaffold.
/// - `condition` — atom-level CEL gate (S7.3); the builder's
///   `propagate_readiness` evaluates this at runtime.
/// - `expansion_source` / `expansion_instructions` — atom-level
///   PerSample fan-out fields, threaded for parity (today's
///   atoms don't author them; the rail is here when they do).
/// - `requires_sme_review` — `discover_*` stages always require
///   SME review unless `auto_approve` opts out.
///
/// The atom's `iterate:` block carries the actual `IterateSpec`
/// the builder reads via `stage.iterate_spec()`; we only need to
/// flip the cardinality variant here so the builder routes
/// through `emit_iterate_stage`.
fn composed_atom_to_stage_spec(ca: &crate::composer::ComposedAtom) -> StageSpec {
    use crate::atom::AtomRole;
    // Branch on `default_behavior_class()` so new
    // speculative variants (Calibration, Pilot, Adversarial, Monitor)
    // and Sizing fall through to `operation`-equivalent classification
    // without needing a dedicated arm here.
    let class = match ca.atom.role.default_behavior_class() {
        AtomRole::Operation => "operation",
        AtomRole::Discovery => "discovery",
        AtomRole::Validation => "validation",
        AtomRole::Aggregator => "aggregator",
        AtomRole::Selection => "selection",
        // default_behavior_class only returns the five load-bearing
        // roles; the speculative variants are mapped above. Anything
        // else is unreachable but we cover it conservatively.
        _ => "operation",
    }
    .to_string();
    let resource_class = ca.atom.resource_profile.as_ref().and_then(|rp| {
        // Map the atom's resource profile flavor to the legacy
        // builder enum string. CPU-heavy is the conservative
        // default the builder already falls back to.
        if rp.gpu {
            Some("gpu".to_string())
        } else if matches!(rp.memory.as_deref(), Some("xl") | Some("large")) {
            Some("memory_heavy".to_string())
        } else {
            None
        }
    });
    let assignee = match ca.atom.assignee {
        crate::atom::AtomAssignee::Agent => Some("agent".to_string()),
        crate::atom::AtomAssignee::Sme => Some("sme".to_string()),
    };
    let discovery = match ca.atom.role {
        AtomRole::Discovery => DiscoveryRequirement::EmpiricalRequired,
        _ => DiscoveryRequirement::None,
    };
    // Flip cardinality to IterateUntil when the atom carries an
    // `iterate:` block. The builder's `emit_stage` dispatches to
    // `emit_iterate_stage` when this is set, producing the 4-template
    // scaffold.
    let cardinality = if ca.atom.iterate.is_some() {
        StageCardinality::IterateUntil
    } else {
        StageCardinality::default()
    };
    // Discovery atoms route through the existing
    // SME-review gate so the BlockerCard "Auto-approve discoveries"
    // checkbox can fast-pass routine stages. Operation +
    // validation atoms default to None (no review by default;
    // archetype slot-fill rules can still flip them on).
    let requires_sme_review = match ca.atom.role {
        AtomRole::Discovery => Some(true),
        _ => None,
    };
    StageSpec {
        id: ca.stage_id.to_string(),
        class,
        discovery,
        depends_on: ca.depends_on.clone(),
        assignee,
        description: ca.atom.description.clone(),
        // Composer-native stages carry the underlying
        // atom's role so consumers can switch on
        // `default_behavior_class()` without re-deriving from id.
        role: ca.atom.role,
        cardinality,
        expansion_source: None,
        expansion_instructions: None,
        // Thread atom-level CEL gate through to the StageSpec. The
        // builder's `propagate_readiness` evaluates this at runtime
        // against upstream task results.
        condition: ca.atom.condition.clone(),
        edam_operation: Some(ca.atom.edam_operation.clone()),
        method_prose: None,
        variants: Vec::new(),
        resource_class,
        requires_sme_review,
        required_figures: ca.atom.required_figures.clone(),
        plot_stage_id: ca.atom.plot_stage_id.clone().or_else(|| {
            (!ca.atom.required_figures.is_empty() && ca.stage_id != ca.atom.id)
                .then(|| ca.atom.id.clone())
        }),
        expected_artifacts: ca.atom.expected_artifacts.clone(),
        spec_preferred_methods: BTreeMap::new(),
        claim_boundary: ca.atom.claim_boundary.clone(),
        checkpoint_level: None,
        required_artifacts: ca.atom.required_artifacts.clone(),
        // Propagate atom-level validators to the stage so the legacy
        // v1/v2/v3 builder path populates
        // `RequiredArtifact.validation_obligations`.
        validators: ca.atom.validators.clone(),
        figure_exempt: ca.atom.figure_exempt.clone(),
    }
}

/// Emit tasks for a single stage spec into the task map.
/// For `one` cardinality: emits discover (if needed) + compute + validate triple.
/// For `per_sample` cardinality: emits gate_fan_out + compute + gate_fan_in.
/// For `iterate_until` cardinality: emits the 4-template scaffold per Plan
/// §S10.3 (`iterate_gate_<id>`, `<id>` placeholder, `iterate_check_<id>`,
/// `validate_<id>`).
fn emit_stage(stage: &StageSpec, tasks: &mut BTreeMap<TaskId, Task>) -> Result<()> {
    // Validate per_sample constraints
    if stage.cardinality == StageCardinality::PerSample {
        if stage.expansion_source.is_none() {
            bail!(
                "stage '{}' has cardinality per_sample but is missing expansion_source",
                stage.id
            );
        }
        if stage.expansion_instructions.is_none() {
            bail!(
                "stage '{}' has cardinality per_sample but is missing expansion_instructions",
                stage.id
            );
        }
        emit_fan_out_stage(stage, tasks);
        return Ok(());
    }

    // Iterate-until 4-template scaffold. Emits
    // Iterate_gate_<id> — barrier the agent reads to spawn iter 1
    // <id> — placeholder; agent overwrites with iter outputs
    // Iterate_check_<id> — convergence check between passes
    // Validate_<id> — final validation against the converged result
    // The runtime expansion to <id>_iter_N happens in the agent
    // The builder shapes the compile-time scaffold so every iterate atom
    // carries the same observable DAG topology.
    if stage.cardinality == StageCardinality::IterateUntil {
        emit_iterate_stage(stage, tasks);
        return Ok(());
    }

    // Branch on the typed `AtomRole` rather than the
    // legacy filename prefix. `stage.role` is stamped from the
    // composer-native atom or backfilled by `derive_role_from_id` at
    // `build_dag_from_taxonomy` build time, so this works for both
    // composition paths.
    let self_discovery = stage.role.is_discovery()
        && matches!(stage.discovery, DiscoveryRequirement::EmpiricalRequired);

    let assignee = parse_assignee(stage.assignee.as_deref());

    if self_discovery {
        let mut spec = serde_json::json!({
            "stage_class": stage.class,
            "discovery_kind": "best_practice"
        });
        if let Some(edam) = &stage.edam_operation {
            spec["edam_operation"] = serde_json::Value::String(edam.clone());
        }
        tasks.insert(
            TaskId::from(stage.id.as_str()),
            Task {
                kind: TaskKind::Discovery(DiscoveryKind::BestPractice),
                state: TaskState::Pending,
                depends_on: stage
                    .depends_on
                    .iter()
                    .map(|s| TaskId::from(s.as_str()))
                    .collect(),
                assignee: assignee.clone(),
                description: stage.description.clone(),
                spec: Some(spec),
                resolution: Some(ResolutionStrategy {
                    primary: "Search literature and tool registries for best-practice method"
                        .into(),
                    fallback: Some(
                        "Use default method from best-practice-scoring-policy.json".into(),
                    ),
                    escalation: EscalationPath::EscalateToSme,
                    policy_ref: Some("best-practice-scoring-policy.json".into()),
                }),
                result_ref: None,
                resource_class: resource_class_for_stage(stage),
                requires_sme_review: requires_sme_review_for_stage(stage),
                required_artifacts: required_artifacts_for_stage(stage, false),
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        );
        return Ok(());
    }

    // Emit discovery task wrapper if required (only for stages NOT already named discover_*)
    let compute_deps = if matches!(stage.discovery, DiscoveryRequirement::EmpiricalRequired) {
        let discover_id = format!("discover_{}", stage.id);
        let mut spec = serde_json::json!({
            "stage_class": stage.class,
            "discovery_kind": "best_practice"
        });
        if let Some(edam) = &stage.edam_operation {
            spec["edam_operation"] = serde_json::Value::String(edam.clone());
        }
        tasks.insert(
            TaskId::from(discover_id.as_str()),
            Task {
                kind: TaskKind::Discovery(DiscoveryKind::BestPractice),
                state: TaskState::Pending,
                depends_on: stage
                    .depends_on
                    .iter()
                    .map(|s| TaskId::from(s.as_str()))
                    .collect(),
                assignee: assignee.clone(),
                description: format!("Discover best-practice method for: {}", stage.description),
                spec: Some(spec),
                resolution: Some(ResolutionStrategy {
                    primary: "Search literature and tool registries for best-practice method"
                        .into(),
                    fallback: Some(
                        "Use default method from best-practice-scoring-policy.json".into(),
                    ),
                    escalation: EscalationPath::EscalateToSme,
                    policy_ref: Some("best-practice-scoring-policy.json".into()),
                }),
                result_ref: None,
                resource_class: resource_class_for_stage(stage),
                requires_sme_review: requires_sme_review_for_stage(stage),
                required_artifacts: required_artifacts_for_stage(stage, false),
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        );
        let mut deps: Vec<TaskId> = stage
            .depends_on
            .iter()
            .map(|s| TaskId::from(s.as_str()))
            .collect();
        deps.push(TaskId::from(discover_id.as_str()));
        deps
    } else {
        stage
            .depends_on
            .iter()
            .map(|s| TaskId::from(s.as_str()))
            .collect()
    };

    // Emit compute task
    let mut compute_spec = serde_json::json!({
        "stage_class": stage.class,
    });
    if let Some(cond) = &stage.condition {
        compute_spec["condition"] = serde_json::Value::String(cond.clone());
    }
    if let Some(edam) = &stage.edam_operation {
        compute_spec["edam_operation"] = serde_json::Value::String(edam.clone());
    }
    add_plotting_spec_fields(&mut compute_spec, stage);
    add_expected_artifact_spec_fields(&mut compute_spec, stage);
    if !stage.spec_preferred_methods.is_empty() {
        let mut map = serde_json::Map::new();
        for (k, v) in &stage.spec_preferred_methods {
            map.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        compute_spec["spec_preferred_methods"] = serde_json::Value::Object(map);
    }
    let is_review = stage.class == "review";
    // Typed role replaces the prefix sniff.
    let is_validation = stage.role.is_validation();
    let (task_kind, task_assignee) = if is_review {
        (TaskKind::Review, Assignee::Sme)
    } else if is_validation {
        (TaskKind::Validation, Assignee::Agent)
    } else {
        (TaskKind::Computation, Assignee::Agent)
    };
    let include_figure_artifacts = !is_review && !is_validation;
    tasks.insert(
        TaskId::from(stage.id.as_str()),
        Task {
            kind: task_kind,
            state: TaskState::Pending,
            depends_on: compute_deps.clone(),
            assignee: task_assignee,
            description: stage.description.clone(),
            spec: Some(compute_spec),
            resolution: None,
            result_ref: None,
            resource_class: resource_class_for_stage(stage),
            requires_sme_review: requires_sme_review_for_stage(stage),
            required_artifacts: required_artifacts_for_stage(stage, include_figure_artifacts),
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        },
    );

    // Emit validate task for stages that aren't already reviews, discoveries, or
    // self-validations (stages whose ID starts with "validate_").
    let skip_validate = is_review
        || is_validation
        || matches!(stage.discovery, DiscoveryRequirement::EmpiricalRequired);
    if !skip_validate {
        let validate_id = format!("validate_{}", stage.id);
        if !tasks.contains_key(validate_id.as_str()) {
            let mut val_spec = serde_json::json!({"stage_class": stage.class});
            if let Some(edam) = &stage.edam_operation {
                val_spec["edam_operation"] = serde_json::Value::String(edam.clone());
            }
            // Propagate the upstream stage's figure contract into the
            // validator spec so it can assert figures/ contents.
            add_plotting_spec_fields(&mut val_spec, stage);
            tasks.insert(
                TaskId::from(validate_id.as_str()),
                Task {
                    kind: TaskKind::Validation,
                    state: TaskState::Pending,
                    depends_on: vec![TaskId::from(stage.id.as_str())],
                    assignee: Assignee::Agent,
                    description: format!("Validate outputs of: {}", stage.description),
                    spec: Some(val_spec),
                    resolution: None,
                    result_ref: None,
                    resource_class: resource_class_for_stage(stage),
                    requires_sme_review: requires_sme_review_for_stage(stage),
                    required_artifacts: required_artifacts_for_stage(stage, false),
                    container: None,
                    source_atom_id: None,
                    safety: Default::default(),
                },
            );
        }
    }

    Ok(())
}

/// Emit fan-out gate + compute placeholder + fan-in gate for per_sample stages.
fn emit_fan_out_stage(stage: &StageSpec, tasks: &mut BTreeMap<TaskId, Task>) {
    let fan_out_id = format!("gate_fan_out_{}", stage.id);
    let fan_in_id = format!("gate_fan_in_{}", stage.id);
    let validate_id = format!("validate_{}", stage.id);

    // Fan-out gate: instructs agent to enumerate samples and create per-sample tasks
    tasks.insert(
        TaskId::from(fan_out_id.as_str()),
        Task {
            kind: TaskKind::Gate,
            state: TaskState::Pending,
            depends_on: stage
                .depends_on
                .iter()
                .map(|s| TaskId::from(s.as_str()))
                .collect(),
            assignee: Assignee::Agent,
            description: format!(
                "Enumerate samples and expand per-sample tasks for: {}",
                stage.description
            ),
            spec: Some(serde_json::json!({
                "expansion_source": stage.expansion_source,
                "expansion_instructions": stage.expansion_instructions,
                "stage_class": stage.class,
            })),
            resolution: None,
            result_ref: None,
            resource_class: resource_class_for_stage(stage),
            requires_sme_review: requires_sme_review_for_stage(stage),
            required_artifacts: required_artifacts_for_stage(stage, false),
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        },
    );

    // Compute task placeholder (agent replaces with per-sample tasks at runtime)
    let mut compute_spec = serde_json::json!({
        "stage_class": stage.class,
        "cardinality": "per_sample",
        "note": "Agent expands this into N per-sample tasks based on gate_fan_out instructions"
    });
    add_plotting_spec_fields(&mut compute_spec, stage);
    add_expected_artifact_spec_fields(&mut compute_spec, stage);
    tasks.insert(
        TaskId::from(stage.id.as_str()),
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Pending,
            depends_on: vec![TaskId::from(fan_out_id.as_str())],
            assignee: Assignee::Agent,
            description: stage.description.clone(),
            spec: Some(compute_spec),
            resolution: None,
            result_ref: None,
            resource_class: resource_class_for_stage(stage),
            requires_sme_review: requires_sme_review_for_stage(stage),
            required_artifacts: required_artifacts_for_stage(stage, true),
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        },
    );

    // Fan-in gate: waits for all per-sample tasks to complete
    tasks.insert(
        TaskId::from(fan_in_id.as_str()),
        Task {
            kind: TaskKind::Gate,
            state: TaskState::Pending,
            depends_on: vec![TaskId::from(stage.id.as_str())],
            assignee: Assignee::Agent,
            description: format!(
                "Confirm all per-sample tasks completed for: {}",
                stage.description
            ),
            spec: Some(serde_json::json!({"stage_class": stage.class})),
            resolution: None,
            result_ref: None,
            resource_class: resource_class_for_stage(stage),
            requires_sme_review: requires_sme_review_for_stage(stage),
            required_artifacts: required_artifacts_for_stage(stage, false),
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        },
    );

    // Validation task depends on fan-in
    let mut validate_spec = serde_json::json!({"stage_class": stage.class});
    add_plotting_spec_fields(&mut validate_spec, stage);
    tasks.insert(
        TaskId::from(validate_id.as_str()),
        Task {
            kind: TaskKind::Validation,
            state: TaskState::Pending,
            depends_on: vec![TaskId::from(fan_in_id.as_str())],
            assignee: Assignee::Agent,
            description: format!("Validate per-sample outputs for: {}", stage.description),
            spec: Some(validate_spec),
            resolution: None,
            result_ref: None,
            resource_class: resource_class_for_stage(stage),
            requires_sme_review: requires_sme_review_for_stage(stage),
            required_artifacts: required_artifacts_for_stage(stage, false),
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        },
    );
}

/// Emit the 4-template iterate-until scaffold:
///
/// `iterate_gate_<id>` — barrier the agent reads to spawn iter 1
/// `<id>` — placeholder; agent overwrites with iter outputs
/// `iterate_check_<id>` — convergence check between passes
/// `validate_<id>` — final validation against the converged result
///
/// The runtime expansion of `<id>` into `<id>_iter_1`, `<id>_iter_2`,...
/// happens in the agent, not the builder. The compile-time
/// shape is fixed at 4 tasks regardless of `max_iterations` so the DAG
/// stays a deterministic byte-shape per intake — convergence count is
/// data-dependent (CLAUDE.md determinism rule), but the scaffold is
/// constant.
fn emit_iterate_stage(stage: &StageSpec, tasks: &mut BTreeMap<TaskId, Task>) {
    let gate_id = format!("iterate_gate_{}", stage.id);
    let check_id = format!("iterate_check_{}", stage.id);
    let validate_id = format!("validate_{}", stage.id);

    // Iterate gate: instructs the agent to start iteration 1 with the
    // atom's iterate.convergence rule. The agent reads the spec, spawns
    // <id>_iter_1, evaluates the metric, and either re-spawns <id>_iter_2
    // (continuing) or transitions iterate_check to Completed (converged)
    // or transitions to Blocked { IterationDidNotConverge } at
    // max_iterations.
    tasks.insert(
        TaskId::from(gate_id.as_str()),
        Task {
            kind: TaskKind::Gate,
            state: TaskState::Pending,
            depends_on: stage
                .depends_on
                .iter()
                .map(|s| TaskId::from(s.as_str()))
                .collect(),
            assignee: Assignee::Agent,
            description: format!(
                "Begin iteration loop for: {} (convergence rule in atom.iterate.convergence)",
                stage.description
            ),
            spec: Some(serde_json::json!({
                "stage_class": stage.class,
                "cardinality": "iterate_until",
                "iterate_role": "gate",
                "instructions": "Spawn <id>_iter_1 task; on completion, evaluate convergence; iterate or block",
            })),
            resolution: None,
            result_ref: None,
            resource_class: resource_class_for_stage(stage),
            requires_sme_review: requires_sme_review_for_stage(stage),
            required_artifacts: required_artifacts_for_stage(stage, false),
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        },
    );

    // Compute placeholder. The agent overwrites this with the converged
    // iteration's output (alias) — downstream tasks reference <id> as
    // they would in the One-cardinality case.
    let mut compute_spec = serde_json::json!({
        "stage_class": stage.class,
        "cardinality": "iterate_until",
        "iterate_role": "placeholder",
        "note": "Agent expands into <id>_iter_N tasks at runtime; converged result aliases here",
    });
    add_plotting_spec_fields(&mut compute_spec, stage);
    add_expected_artifact_spec_fields(&mut compute_spec, stage);
    tasks.insert(
        TaskId::from(stage.id.as_str()),
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Pending,
            depends_on: vec![TaskId::from(gate_id.as_str())],
            assignee: Assignee::Agent,
            description: stage.description.clone(),
            spec: Some(compute_spec),
            resolution: None,
            result_ref: None,
            resource_class: resource_class_for_stage(stage),
            requires_sme_review: requires_sme_review_for_stage(stage),
            required_artifacts: required_artifacts_for_stage(stage, true),
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        },
    );

    // Iterate check: barrier between the iteration loop and the validate
    // task. Marked Completed by the agent when convergence fires; carries
    // the per-iteration metric trail in its result for the
    // ResultReviewTurnCard convergence-trajectory chart (S10.8).
    tasks.insert(
        TaskId::from(check_id.as_str()),
        Task {
            kind: TaskKind::Gate,
            state: TaskState::Pending,
            depends_on: vec![TaskId::from(stage.id.as_str())],
            assignee: Assignee::Agent,
            description: format!(
                "Confirm iteration loop converged for: {}",
                stage.description
            ),
            spec: Some(serde_json::json!({
                "stage_class": stage.class,
                "cardinality": "iterate_until",
                "iterate_role": "check",
            })),
            resolution: None,
            result_ref: None,
            resource_class: resource_class_for_stage(stage),
            requires_sme_review: requires_sme_review_for_stage(stage),
            required_artifacts: required_artifacts_for_stage(stage, false),
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        },
    );

    // Validation against the converged result. Same shape as the
    // per-sample validate task — depends on the check, runs after.
    let mut validate_spec = serde_json::json!({
        "stage_class": stage.class,
        "iterate_role": "validate",
    });
    add_plotting_spec_fields(&mut validate_spec, stage);
    tasks.insert(
        TaskId::from(validate_id.as_str()),
        Task {
            kind: TaskKind::Validation,
            state: TaskState::Pending,
            depends_on: vec![TaskId::from(check_id.as_str())],
            assignee: Assignee::Agent,
            description: format!("Validate converged output for: {}", stage.description),
            spec: Some(validate_spec),
            resolution: None,
            result_ref: None,
            resource_class: resource_class_for_stage(stage),
            requires_sme_review: requires_sme_review_for_stage(stage),
            required_artifacts: required_artifacts_for_stage(stage, false),
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        },
    );
}

fn parse_assignee(s: Option<&str>) -> Assignee {
    match s {
        Some("sme") => Assignee::Sme,
        Some("agent_then_sme") => Assignee::AgentThenSme,
        _ => Assignee::Agent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// `build_dag_from_composition` produces a valid DAG
    /// from a composer result. Smoke-test against a hand-rolled
    /// composition so the test is independent of the live archetype
    /// catalog.
    #[test]
    fn build_dag_from_composition_smokes_a_two_atom_chain() {
        use crate::atom::{AtomAssignee, AtomDefinition, AtomRole, ResourceProfile};
        use crate::composer::{ComposedAtom, CompositionResult, ResourceEstimate};
        use crate::goal_spec::GoalSpec;

        let leaf = AtomDefinition {
            id: "import_data".into(),
            version: "1.0.0".into(),
            role: AtomRole::Operation,
            discovery_kind: None,
            description: "Import upstream data".into(),
            edam_operation: "operation:0335".into(),
            edam_data: Some("data:2044".into()),
            edam_format: Some("format:1930".into()),
            assignee: AtomAssignee::Agent,
            depends_on: vec![],
            excludes: vec![],
            attributes: BTreeMap::new(),
            joint_with: vec![],
            inputs: vec![],
            outputs: vec![],
            method_choice: None,
            resource_profile: Some(ResourceProfile {
                cpu: Some("light".into()),
                memory: Some("small".into()),
                gpu: false,
                runtime_class: Some("seconds".into()),
            }),
            preferred_container: None,
            claim_boundary: None,
            iterate: None,
            condition: None,
            required_figures: vec![],
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: vec![],
            required_artifacts: vec![],
            validators: vec![],
            runtime_packages: Default::default(),
            safety: Default::default(),
        };
        let downstream = AtomDefinition {
            id: "differential_expression".into(),
            version: "1.0.0".into(),
            role: AtomRole::Operation,
            discovery_kind: None,
            description: "Differential expression analysis".into(),
            edam_operation: "operation:3223".into(),
            edam_data: Some("data:0951".into()),
            edam_format: Some("format:3475".into()),
            assignee: AtomAssignee::Agent,
            depends_on: vec!["import_data".into()],
            excludes: vec![],
            attributes: BTreeMap::new(),
            joint_with: vec![],
            inputs: vec![],
            outputs: vec![],
            method_choice: None,
            resource_profile: Some(ResourceProfile {
                cpu: Some("moderate".into()),
                memory: Some("medium".into()),
                gpu: false,
                runtime_class: Some("hours".into()),
            }),
            preferred_container: None,
            claim_boundary: None,
            iterate: None,
            condition: None,
            required_figures: vec![],
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: vec![],
            required_artifacts: vec![],
            validators: vec![],
            runtime_packages: Default::default(),
            safety: Default::default(),
        };
        let composition = CompositionResult {
            matched_archetype: Some("test_archetype".into()),
            match_score: 6,
            atoms: vec![
                ComposedAtom {
                    stage_id: "import_data".into(),
                    atom: leaf,
                    depends_on: vec![],
                    required: true,
                    bindings: Vec::new(),
                    container: None,
                },
                ComposedAtom {
                    stage_id: "differential_expression".into(),
                    atom: downstream,
                    depends_on: vec!["import_data".into()],
                    required: true,
                    bindings: Vec::new(),
                    container: None,
                },
            ],
            goal: GoalSpec {
                edam_data: "data:0951".into(),
                edam_format: Some("format:3475".into()),
                modifiers: BTreeMap::new(),
                source_prose: None,
                confidence: 0.95,
            },
            rationale: "synthesized for test".into(),
            atom_rationales: BTreeMap::new(),
            resource_estimate: ResourceEstimate::default(),
        };

        let dag =
            build_dag_from_composition(&composition, "test-composition", &BTreeMap::new(), &[])
                .expect("DAG builds from composition");

        // Composer-produced atoms are emitted, plus their validate_*
        // wrappers per the standard one-cardinality emit_stage path.
        assert!(dag.tasks.contains_key("import_data"));
        assert!(dag.tasks.contains_key("differential_expression"));

        // Edge from downstream → leaf is preserved.
        let de = dag.tasks.get("differential_expression").unwrap();
        assert!(de.depends_on.contains(&TaskId::from("import_data")));

        // Workflow id propagates.
        assert_eq!(dag.workflow_id, "test-composition");
    }

    /// Atom-level `condition` field threads through
    /// `composed_atom_to_stage_spec` onto `StageSpec::condition`, which
    /// the builder writes to `Task::spec.condition`. The runtime CEL gate
    /// in `propagate_readiness` reads from that field at task-ready time.
    /// Asserts the field round-trips end-to-end.
    #[test]
    fn build_dag_from_composition_threads_atom_level_condition() {
        use crate::atom::{AtomAssignee, AtomDefinition, AtomRole, ResourceProfile};
        use crate::composer::{ComposedAtom, CompositionResult, ResourceEstimate};
        use crate::goal_spec::GoalSpec;

        let gated_atom = AtomDefinition {
            id: "batch_correction".into(),
            version: "1.0.0".into(),
            role: AtomRole::Operation,
            discovery_kind: None,
            description: "Apply batch correction (gated)".into(),
            edam_operation: "operation:3435".into(),
            edam_data: Some("data:3917".into()),
            edam_format: Some("format:3590".into()),
            assignee: AtomAssignee::Agent,
            depends_on: vec![],
            excludes: vec![],
            attributes: BTreeMap::new(),
            joint_with: vec![],
            inputs: vec![],
            outputs: vec![],
            method_choice: None,
            resource_profile: Some(ResourceProfile {
                cpu: Some("moderate".into()),
                memory: Some("medium".into()),
                gpu: false,
                runtime_class: Some("hours".into()),
            }),
            preferred_container: None,
            claim_boundary: None,
            iterate: None,
            condition: Some(
                "discover_batch_correction.result.batch_correction_required == true".into(),
            ),
            required_figures: vec![],
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: vec![],
            required_artifacts: vec![],
            validators: vec![],
            runtime_packages: Default::default(),
            safety: Default::default(),
        };
        let composition = CompositionResult {
            matched_archetype: Some("test_archetype".into()),
            match_score: 6,
            atoms: vec![ComposedAtom {
                stage_id: "batch_correction".into(),
                atom: gated_atom,
                depends_on: vec![],
                required: true,
                bindings: Vec::new(),
                container: None,
            }],
            goal: GoalSpec {
                edam_data: "data:3917".into(),
                edam_format: Some("format:3590".into()),
                modifiers: BTreeMap::new(),
                source_prose: None,
                confidence: 0.95,
            },
            rationale: "condition test".into(),
            atom_rationales: BTreeMap::new(),
            resource_estimate: ResourceEstimate::default(),
        };

        let dag = build_dag_from_composition(&composition, "test-condition", &BTreeMap::new(), &[])
            .expect("DAG builds from composition with condition");

        let task = dag.tasks.get("batch_correction").expect("task present");
        let spec = task.spec.as_ref().expect("spec present");
        assert_eq!(
            spec.get("condition").and_then(|v| v.as_str()),
            Some("discover_batch_correction.result.batch_correction_required == true"),
            "atom-level condition must thread onto Task::spec.condition; got {:?}",
            spec.get("condition")
        );
    }

    #[test]
    fn build_dag_from_composition_threads_atom_output_contracts() {
        use crate::atom::{AtomAssignee, AtomDefinition, AtomRole};
        use crate::composer::{ComposedAtom, CompositionResult, ResourceEstimate};
        use crate::goal_spec::GoalSpec;
        use crate::taxonomy::RequiredArtifactSpec;

        let atom = AtomDefinition {
            id: "differential_expression".into(),
            version: "1.0.0".into(),
            role: AtomRole::Operation,
            discovery_kind: None,
            description: "Differential abundance analysis".into(),
            edam_operation: "operation:3223".into(),
            edam_data: Some("data:0951".into()),
            edam_format: Some("format:3475".into()),
            assignee: AtomAssignee::Agent,
            depends_on: vec![],
            excludes: vec![],
            attributes: BTreeMap::new(),
            joint_with: vec![],
            inputs: vec![],
            outputs: vec![],
            method_choice: None,
            resource_profile: None,
            preferred_container: None,
            claim_boundary: None,
            iterate: None,
            condition: None,
            required_figures: vec!["volcano".into(), "top_features_heatmap".into()],
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: vec!["de_results.tsv".into(), "de_summary.json".into()],
            required_artifacts: vec![RequiredArtifactSpec {
                path: "de_results.tsv".into(),
                min_size_bytes: Some(1),
                schema_ref: None,
            }],
            validators: vec![],
            runtime_packages: Default::default(),
            safety: Default::default(),
        };
        let composition = CompositionResult {
            matched_archetype: Some("cross_omics_test".into()),
            match_score: 6,
            atoms: vec![ComposedAtom {
                stage_id: "proteomics_differential_abundance".into(),
                atom,
                depends_on: vec![],
                required: true,
                bindings: Vec::new(),
                container: None,
            }],
            goal: GoalSpec {
                edam_data: "data:0951".into(),
                edam_format: Some("format:3475".into()),
                modifiers: BTreeMap::new(),
                source_prose: None,
                confidence: 0.95,
            },
            rationale: "output contract test".into(),
            atom_rationales: BTreeMap::new(),
            resource_estimate: ResourceEstimate::default(),
        };

        let dag = build_dag_from_composition(&composition, "test-contract", &BTreeMap::new(), &[])
            .expect("DAG builds from composition with output contracts");
        let task = dag
            .tasks
            .get("proteomics_differential_abundance")
            .expect("aliased compute task");
        let spec = task.spec.as_ref().expect("compute spec");
        assert_eq!(
            spec["required_figures"],
            serde_json::json!(["volcano", "top_features_heatmap"])
        );
        assert_eq!(
            spec.get("plot_stage_id").and_then(|v| v.as_str()),
            Some("differential_expression")
        );
        assert_eq!(
            spec["expected_artifacts"],
            serde_json::json!(["de_results.tsv", "de_summary.json"])
        );
        let required_paths: std::collections::BTreeSet<_> = task
            .required_artifacts
            .iter()
            .map(|a| a.path.as_str())
            .collect();
        for expected in [
            "de_results.tsv",
            "figures/manifest.json",
            "figures/volcano.png",
            "figures/volcano.pdf",
            "figures/top_features_heatmap.png",
            "figures/top_features_heatmap.pdf",
        ] {
            assert!(
                required_paths.contains(expected),
                "aliased compute task missing required artifact {expected}; got {:?}",
                required_paths
            );
        }

        let validator = dag
            .tasks
            .get("validate_proteomics_differential_abundance")
            .expect("validator task");
        let val_spec = validator.spec.as_ref().expect("validator spec");
        assert_eq!(
            val_spec.get("plot_stage_id").and_then(|v| v.as_str()),
            Some("differential_expression")
        );
        assert!(
            validator
                .required_artifacts
                .iter()
                .all(|a| !a.path.starts_with("figures/")),
            "validator output dir must not require parent compute figures"
        );
    }

    /// Cross-dependency edges from `archetype.cross_dependencies` are
    /// re-applied after the per-atom emit pass.
    #[test]
    fn build_dag_from_composition_honors_cross_dependencies() {
        use crate::atom::{AtomAssignee, AtomDefinition, AtomRole};
        use crate::composer::{ComposedAtom, CompositionResult, ResourceEstimate};
        use crate::goal_spec::GoalSpec;

        let mk_atom = |id: &str| AtomDefinition {
            id: id.into(),
            version: "1.0.0".into(),
            role: AtomRole::Operation,
            discovery_kind: None,
            description: id.into(),
            edam_operation: "operation:0292".into(),
            edam_data: Some("data:2044".into()),
            edam_format: None,
            assignee: AtomAssignee::Agent,
            depends_on: vec![],
            excludes: vec![],
            attributes: BTreeMap::new(),
            joint_with: vec![],
            inputs: vec![],
            outputs: vec![],
            method_choice: None,
            resource_profile: None,
            preferred_container: None,
            claim_boundary: None,
            iterate: None,
            condition: None,
            required_figures: vec![],
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: vec![],
            required_artifacts: vec![],
            validators: vec![],
            runtime_packages: Default::default(),
            safety: Default::default(),
        };
        let composition = CompositionResult {
            matched_archetype: None,
            match_score: 0,
            atoms: vec![
                ComposedAtom {
                    stage_id: "step_a".into(),
                    atom: mk_atom("step_a"),
                    depends_on: vec![],
                    required: true,
                    bindings: Vec::new(),
                    container: None,
                },
                ComposedAtom {
                    stage_id: "step_b".into(),
                    atom: mk_atom("step_b"),
                    depends_on: vec![],
                    required: true,
                    bindings: Vec::new(),
                    container: None,
                },
            ],
            goal: GoalSpec {
                edam_data: "data:2044".into(),
                edam_format: None,
                modifiers: BTreeMap::new(),
                source_prose: None,
                confidence: 0.8,
            },
            rationale: "test cross-deps".into(),
            atom_rationales: BTreeMap::new(),
            resource_estimate: ResourceEstimate::default(),
        };

        let dag = build_dag_from_composition(
            &composition,
            "cross-dep-test",
            &BTreeMap::new(),
            &[("step_a".to_string(), "step_b".to_string())],
        )
        .expect("DAG builds with cross-dependencies");

        let b = dag.tasks.get("step_b").unwrap();
        assert!(
            b.depends_on.contains(&TaskId::from("step_a")),
            "cross-dependency edge added: {:?}",
            b.depends_on
        );
    }
}
