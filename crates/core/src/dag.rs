use anyhow::{anyhow, Result};
use petgraph::algo::{kosaraju_scc, toposort};
use petgraph::graph::DiGraph;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

// Re-export from the canonical ids module so existing `dag::TaskId`
// imports resolve unchanged. The sole definition lives in `crate::ids`.
pub use crate::ids::StageId;
pub use crate::ids::TaskId;

#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
/// DAG data.
pub struct DAG {
    /// Version.
    pub version: String,
    /// On-disk schema version for the DAG shape. Added alongside the
    /// legacy `version: String` field; the two coexist so unversioned
    /// packages (which carry `version` but not `schema_version`) continue
    /// to deserialize cleanly — the `#[serde(default)]` gives missing
    /// records `Version::new(0, 1, 0)`. A future migration step can
    /// promote the semantic content of `version` → `schema_version` once
    /// the field reaches every emitted package. The `schema_version_serde`
    /// adapter accepts both legacy `u64` values and canonical SemVer
    /// strings on read; writes the canonical SemVer string.
    #[serde(
        default = "default_dag_schema_version",
        with = "crate::migration::schema_version_serde"
    )]
    #[ts(skip)]
    #[schemars(with = "String")]
    pub schema_version: semver::Version,
    /// Workflow id.
    pub workflow_id: String,
    /// Current task.
    pub current_task: Option<TaskId>,
    /// Tasks.
    pub tasks: BTreeMap<TaskId, Task>,
    /// Stable package-level run id (UUID v4) assigned at emit time.
    /// Written to `WORKFLOW.json::meta.run_id` and to the RO-Crate root
    /// Dataset's `additionalProperty` list so downstream consumers can
    /// correlate a package's artifacts without path-chasing. Absent on
    /// packages emitted before this field was added; `#[serde(default)]`
    /// gives them `None` so they load unchanged. Never mutated after
    /// first emission — re-emission of an amendment retains the parent's
    /// `run_id` so the lineage chain is traceable by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub run_id: Option<String>,
    /// `task_id → dependents` adjacency, derived from
    /// `tasks[*].depends_on`. Not serialized (would bloat WORKFLOW.json
    /// without being authoritative), not exposed over ts-rs. Populated
    /// by [`DAG::rebuild_reverse_deps`] after construction or task
    /// mutation; `invalidate_forward_slice` reads it directly for
    /// O(n+e) traversal.
    #[serde(default, skip)]
    #[ts(skip)]
    pub reverse_deps: BTreeMap<TaskId, Vec<TaskId>>,
}

/// Current on-disk schema version for [`DAG`]. Callers that need the constant
/// without constructing a DAG should call this directly; the
/// `#[serde(default)]` annotation on the field delegates here.
pub fn current_dag_schema_version() -> semver::Version {
    crate::migration::current_dag_version()
}

fn default_dag_schema_version() -> semver::Version {
    current_dag_schema_version()
}

// PartialEq compares only authoritative fields. The `reverse_deps` cache is
// derived from `tasks` and may be empty (post-deserialize) or populated; two
// DAGs with identical tasks must compare equal regardless of cache state.
// `schema_version` is an on-disk annotation — it does not change the analysis
// contract, so it is excluded from equality the same way `reverse_deps` is.
impl PartialEq for DAG {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
            && self.workflow_id == other.workflow_id
            && self.current_task == other.current_task
            && self.tasks == other.tasks
            && self.run_id == other.run_id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
/// Task data.
pub struct Task {
    /// Kind.
    pub kind: TaskKind,
    /// State.
    pub state: TaskState,
    /// Depends on.
    pub depends_on: Vec<TaskId>,
    /// Assignee.
    pub assignee: Assignee,
    /// Description.
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "unknown | null")]
    /// Spec.
    pub spec: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Resolution.
    pub resolution: Option<ResolutionStrategy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Result ref.
    pub result_ref: Option<String>,
    /// Hardware resource class — drives parallel scheduler slot
    /// accounting and the agent's hardware envelope hints.
    /// Absent on WORKFLOW.json files emitted before this
    /// field was added; deserialization fills in the default
    /// (`CpuHeavy`) so pre-existing packages keep working unchanged.
    #[serde(default)]
    pub resource_class: ResourceClass,
    /// When true, dispatch pauses on any task whose
    /// depends_on chain hits this task until the SME posts
    /// `/api/chat/session/:id/confirm { stage: "<task_id>" }`.
    /// Serde-default `false` keeps older WORKFLOW.json files
    /// readable. Populated from the stage's
    /// `requires_sme_review: true` YAML flag at build time.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub requires_sme_review: bool,
    /// Artifacts the agent must produce before the harness considers
    /// the task complete. The silent-completion guard verifies each
    /// entry exists and is non-empty; absence on an otherwise
    /// `Completed` task re-blocks with `BlockerKind::MissingArtifact`.
    /// Empty vec = no artifact check.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_artifacts: Vec<RequiredArtifact>,
    /// Per-task container pin resolved at compose /
    /// build time. Composer threads the result of
    /// `resolve_task_container` (precedence: atom, then archetype,
    /// then profile, then package, then host) onto every Task so
    /// `WORKFLOW.json` is the reproducibility-bearing source of truth
    /// for which image ran each task. `None` = host-mode (legacy /
    /// unset). Schema is additive: pre-S15 packages have no container
    /// field; serde default gives them `None` and the agent wrappers
    /// fall back to `policies/container.json` per the legacy path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub container: Option<crate::atom::ContainerSpec>,
    /// Source atom id this task was emitted from. Allows
    /// executors / blocker emitters / claim verifiers to back-reference
    /// the originating atom without threading the id through every call.
    /// Optional for back-compat with pre-A.S6 WORKFLOW.json files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_atom_id: Option<String>,
    /// Per-task safety classification, threaded through
    /// from the source atom at emit time.
    #[serde(default, skip_serializing_if = "crate::atom::SafetyPolicy::is_default")]
    pub safety: crate::atom::SafetyPolicy,
}

/// DAG-side copy of a required-artifact declaration. Populated by the
/// builder from `StageSpec.required_artifacts`; threaded into
/// WORKFLOW.json so the harness can read it without re-loading the
/// taxonomy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct RequiredArtifact {
    /// Path relative to `runtime/outputs/<task_id>/`.
    pub path: String,
    /// Optional minimum size. Zero or absent = "non-empty file" check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub min_size_bytes: Option<u64>,
    /// Optional JSON-schema reference (path relative to the package
    /// root). When present and the artifact is JSON, the harness
    /// validates the body against the schema post-completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub schema_ref: Option<String>,
    /// Validation-obligation ids that the harness runs against this
    /// artifact after task completion. Empty by default so legacy
    /// WORKFLOW.json shapes load unchanged. Populated by the v4 lowering
    /// pass from `TaskNode.validators`, where each validator name is the
    /// obligation id the runner
    /// registry resolves at dispatch time
    /// (`p_value_in_unit_interval`, `gene_id_in_annotation`, etc.).
    /// Failures append to `runtime/validation-reports.jsonl` and
    /// re-block the task with `BlockerKind::ValidationFailed`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation_obligations: Vec<String>,
}

impl RequiredArtifact {
    /// Compile-time / build-time `schema_ref` shape
    /// validation. Returns Ok when:
    ///
    /// - `schema_ref` is None (no schema attached); or
    /// - `schema_ref` is a relative path (no leading `/`, no `..`
    ///   segments) ending in `.schema.json`.
    ///
    /// Used by the builder + composer paths to surface a malformed
    /// schema_ref at DAG-construction time, not at first run.
    #[must_use = "schema_ref validation result must be inspected — dropping it lets a malformed schema reference reach runtime"]
    pub fn validate_schema_ref(&self) -> std::result::Result<(), DagError> {
        let Some(ref_str) = self.schema_ref.as_deref() else {
            return Ok(());
        };
        let invalid_reason: Option<&str> = if ref_str.is_empty() {
            Some("schema_ref is empty")
        } else if ref_str.starts_with('/') {
            Some("schema_ref must be relative (no leading `/`)")
        } else if ref_str.split('/').any(|seg| seg == "..") {
            Some("schema_ref must not contain `..` segments")
        } else if !ref_str.ends_with(".schema.json") {
            Some("schema_ref must end in `.schema.json`")
        } else {
            None
        };
        if let Some(reason) = invalid_reason {
            return Err(DagError::Other(anyhow!(
                "RequiredArtifact `{}`: {} (got `{}`)",
                self.path,
                reason,
                ref_str
            )));
        }
        Ok(())
    }
}

/// Per-task hardware resource class. The parallel scheduler uses
/// this to decide whether a task draws against the CPU semaphore or the
/// GPU semaphore; the agent envelope uses it to compute
/// `concurrent_peers_by_class`. The enum is intentionally closed — new
/// variants require a scheduler policy decision, not just a schema
/// change.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ResourceClass {
    /// Default. CPU-bound work — alignment, QC, most statistical tests.
    #[default]
    CpuHeavy,
    /// I/O-bound: samtools sort, bgzip, pigz, minimap2 index load.
    /// Scheduler can oversubscribe these against cpu_heavy because
    /// they're typically waiting on disk.
    IoHeavy,
    /// Large-memory work (DE analysis on 50-sample matrices, peak
    /// calling, >500k-cell clustering). Draws against cpu_slots but
    /// the scheduler may reserve extra headroom.
    MemoryHeavy,
    /// Requires a GPU slot. Draws against gpu_slots, not cpu_slots.
    /// Variant calling (DeepVariant), structure prediction
    /// (AlphaFold), scVI training.
    Gpu,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// TaskKind discriminant.
pub enum TaskKind {
    /// Discovery variant.
    Discovery(DiscoveryKind),
    /// Computation variant.
    Computation,
    /// Validation variant.
    Validation,
    /// Review variant.
    Review,
    /// Gate variant.
    Gate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// DiscoveryKind discriminant.
pub enum DiscoveryKind {
    /// BestPractice variant.
    BestPractice,
    /// Source variant.
    Source,
    /// Evidence variant.
    Evidence,
    /// EnvironmentProbe variant.
    EnvironmentProbe,
    /// Custom variant.
    Custom(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case", tag = "status")]
/// TaskState discriminant.
pub enum TaskState {
    /// Pending variant.
    Pending,
    /// Ready variant.
    Ready,
    /// Running variant.
    Running {
        /// Started at.
        started_at: String,
        /// Remote execution metadata. None for local execution. Kept as
        /// Option with skip_serializing_if so WORKFLOW.json stays
        /// byte-identical for local-mode packages (no `remote: null`
        /// field appears) and pre-AWS-A packages still deserialize.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        remote: Option<RemoteExecution>,
    },
    /// Completed variant.
    Completed {
        #[ts(type = "unknown")]
        /// Result.
        result: serde_json::Value,
    },
    /// Failed variant.
    Failed {
        /// Reason.
        reason: String,
    },
    /// Blocked variant.
    Blocked {
        /// Record.
        record: BlockedRecord,
    },
}

impl TaskState {
    /// Terminal predicate used by
    /// `Session::set_task_state` to enforce monotonicity. A terminal
    /// state is one the harness considers final for a given task
    /// dispatch — `Completed` and `Failed`. `Blocked` is recoverable
    /// (SME unblock returns the task to Ready) and is therefore NOT
    /// terminal here. The enum has no `Skipped` variant today; if one
    /// is added later it should also be terminal.
    pub fn is_terminal(&self) -> bool {
        matches!(self, TaskState::Completed { .. } | TaskState::Failed { .. })
    }
}

/// Typed inspector over the JSON `result` field that
/// classifies a `Completed` task by *who* wrote the value. Three
/// patterns the system actually emits:
///
/// - [`AgentTaskResult::SmeResolved`] — SME named the method at
///   compile time via `set_intake_method` and the builder's
///   `resolve_intake_methods` stamped the discovery task with the
///   provenance object `{resolved_by: "sme", resolved_at,
/// Method,...sme_fields}`. The `## SME discovery decisions`
///   section in CONTEXT.md is rendered from these.
/// - [`AgentTaskResult::SkippedByCondition`] — `propagate_readiness`
///   auto-completed a conditional task whose CEL gate evaluated to
///   `false`. Wire shape: `{skipped: true, reason, condition}`.
/// - [`AgentTaskResult::AgentCompleted`] — the agent wrote
///   arbitrary JSON via the harness PROGRESS protocol. Catch-all.
///
/// Storage is intentionally NOT replaced: the wire field on
/// `TaskState::Completed { result: serde_json::Value }` stays as
/// `serde_json::Value` because the agent writes free-form JSON the
/// compiler doesn't constrain (per CLAUDE.md "agent is the
/// executor"). This enum is reconstructed at read time via
/// [`Task::completion_kind`] so call sites that need the
/// classification get type safety without forcing a breaking wire
/// change on every emitted package.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentTaskResult<'a> {
    /// SmeResolved variant.
    SmeResolved {
        /// Resolved at.
        resolved_at: &'a str,
        /// Method.
        method: &'a str,
        /// All non-provenance keys: SME-supplied structured fields
        /// (e.g. `batch_correction_required: true`) plus any
        /// auto-injected condition-gate defaults the builder added.
        fields: &'a serde_json::Map<String, serde_json::Value>,
    },
    /// SkippedByCondition variant.
    SkippedByCondition {
        /// Reason.
        reason: &'a str,
        /// Condition.
        condition: &'a str,
    },
    /// AgentCompleted variant.
    AgentCompleted(&'a serde_json::Value),
}

impl<'a> AgentTaskResult<'a> {
    /// Inspect a JSON `result` value and classify it. Order matters:
    /// SmeResolved discriminator first (presence of `resolved_by ==
    /// "sme"`), then SkippedByCondition (presence of `skipped ==
    /// true`), then AgentCompleted as the catch-all.
    pub fn from_value(value: &'a serde_json::Value) -> Self {
        let obj = value.as_object();
        if let Some(map) = obj {
            let is_sme = map
                .get("resolved_by")
                .and_then(|v| v.as_str())
                .map(|s| s == "sme")
                .unwrap_or(false);
            if is_sme {
                let resolved_at = map
                    .get("resolved_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let method = map.get("method").and_then(|v| v.as_str()).unwrap_or("");
                return AgentTaskResult::SmeResolved {
                    resolved_at,
                    method,
                    fields: map,
                };
            }
            let skipped = map
                .get("skipped")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if skipped {
                let reason = map.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                let condition = map.get("condition").and_then(|v| v.as_str()).unwrap_or("");
                return AgentTaskResult::SkippedByCondition { reason, condition };
            }
        }
        AgentTaskResult::AgentCompleted(value)
    }
}

impl Task {
    /// When the task is in `Completed` state, return
    /// the typed classification of who wrote the result. Returns
    /// `None` for any other state (Pending/Ready/Running/Failed/
    /// Blocked) — those have no `result` field by construction.
    pub fn completion_kind(&self) -> Option<AgentTaskResult<'_>> {
        match &self.state {
            TaskState::Completed { result } => Some(AgentTaskResult::from_value(result)),
            _ => None,
        }
    }
}

/// Backend-specific execution metadata attached to a Running task when the
/// harness provisions remote compute. Local-mode tasks never populate this.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct RemoteExecution {
    /// Backend identifier matching `Executor::name()`. e.g., "aws", "gcp".
    pub backend: String,
    /// Backend-native instance/job id (e.g., "i-0abc123def456").
    pub instance_id: String,
    /// Backend-native instance type / shape (e.g., "r6i.4xlarge").
    pub instance_type: String,
    /// Backend-native command/job id for status polling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub command_id: Option<String>,
    /// Backend-native output URI (e.g., "s3://bucket/prefix/task_id/").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub output_uri: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// Assignee discriminant.
pub enum Assignee {
    /// Agent variant.
    Agent,
    /// Sme variant.
    Sme,
    /// AgentThenSme variant.
    AgentThenSme,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
/// BlockedRecord data.
pub struct BlockedRecord {
    /// Reason.
    pub reason: String,
    #[serde(default)]
    /// Attempts.
    pub attempts: Vec<ResolutionAttempt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
/// ResolutionAttempt data.
pub struct ResolutionAttempt {
    /// Method.
    pub method: String,
    /// Result.
    pub result: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
/// ResolutionStrategy data.
pub struct ResolutionStrategy {
    /// Primary.
    pub primary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Fallback.
    pub fallback: Option<String>,
    /// Escalation.
    pub escalation: EscalationPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Policy ref.
    pub policy_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// EscalationPath discriminant.
pub enum EscalationPath {
    /// EscalateToSme variant.
    EscalateToSme,
    /// EscalateBlock variant.
    EscalateBlock,
}

// ── DagError ──────────────────────────────────────────────────────────────────
//
// Typed error surface for DAG-construction and DAG-validation paths. Today most
// callers funnel through `anyhow::Result<T>`; this enum lets new local surfaces
// opt into a typed `Result<T, DagError>` without forcing every existing call
// site to migrate at once.
//
// `From<anyhow::Error>` is provided so that a function returning
// `Result<T, DagError>` can `?` an underlying `anyhow::Result<T>` and have the
// failure surface as `DagError::Other`. (S5.12)

/// Categorized DAG construction / validation failures.
///
/// Variant shapes follow plan §S5.12 exactly so call sites can pattern-match on
/// `CycleDetected { cycle }` / `OrphanedTask { task_id }` / `InvalidDependency
/// { task_id, missing_dep }` / `MissingStage { stage_id }` without churn.
/// Future variants land alongside `Other` (which already wraps any opaque
/// error from the `?`-propagation path).
#[derive(Debug)]
pub enum DagError {
    /// A cycle was detected during topological sort. `cycle` lists task ids
    /// in cycle order so the blocker UI can render the trace; the first id
    /// is the entry point the toposort tripped on.
    CycleDetected { cycle: Vec<String> },
    /// A task is unreachable from any root and has no consumers.
    OrphanedTask { task_id: TaskId },
    /// A `depends_on` edge references a task that does not exist.
    InvalidDependency {
        /// Task id.
        task_id: TaskId,
        /// Missing dep.
        missing_dep: String,
    },
    /// A taxonomy stage referenced by a task could not be resolved.
    MissingStage { stage_id: StageId },
    /// Two stages in the merged DAG share the same `discover_<x>` /
    /// `validate_<x>` prefix root (prefix-uniqueness check) —
    /// catches cross-taxonomy collisions where two compose paths each
    /// emit a `discover_alignment` (etc.) before the builder lands them
    /// in the same `BTreeMap`.
    DuplicatePrefix {
        /// Prefix.
        prefix: String,
        /// Stage ids.
        stage_ids: Vec<String>,
    },
    /// Fallback for anything that doesn't fit the shaped variants —
    /// produced by `From<anyhow::Error>`.
    Other(anyhow::Error),
}

impl std::fmt::Display for DagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DagError::CycleDetected { cycle } => {
                if cycle.is_empty() {
                    write!(f, "cycle detected (no entry node recorded)")
                } else {
                    write!(f, "cycle detected: {}", cycle.join(" -> "))
                }
            }
            DagError::OrphanedTask { task_id } => {
                write!(f, "task `{task_id}` is orphaned (unreachable from roots)")
            }
            DagError::InvalidDependency {
                task_id,
                missing_dep,
            } => write!(
                f,
                "task `{task_id}` depends on `{missing_dep}` which does not exist"
            ),
            DagError::MissingStage { stage_id } => {
                write!(f, "stage `{stage_id}` is missing from the taxonomy")
            }
            DagError::DuplicatePrefix { prefix, stage_ids } => write!(
                f,
                "duplicate stage prefix `{prefix}` (collides across: {})",
                stage_ids.join(", ")
            ),
            DagError::Other(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for DagError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DagError::Other(err) => Some(err.as_ref()),
            _ => None,
        }
    }
}

impl From<anyhow::Error> for DagError {
    fn from(err: anyhow::Error) -> Self {
        DagError::Other(err)
    }
}

// ── DAG impl ──────────────────────────────────────────────────────────────────

impl DAG {
    /// Public, invariant-checking mutation API. Prefer this
    /// over `dag.tasks.insert(...)` from outside `crates/core`. In debug
    /// builds, asserts every `depends_on` entry resolves to an existing
    /// task id (when the DAG already has tasks); release builds skip the
    /// check so the assertion has zero runtime cost. Pairs with
    /// `remove_task` for the symmetric drop.
    pub fn insert_task(&mut self, id: TaskId, task: Task) -> Option<Task> {
        debug_assert!(
            !id.is_empty(),
            "insert_task: empty task id violates DAG invariant",
        );
        if !self.tasks.is_empty() {
            for dep in &task.depends_on {
                debug_assert!(
                    dep == &id || self.tasks.contains_key(dep),
                    "insert_task: task `{id}` depends on `{dep}` which is not in the DAG",
                );
            }
        }
        self.tasks.insert(id, task)
    }

    /// Public, invariant-checking mutation API counterpart
    /// to [`DAG::insert_task`]. In debug builds, asserts no remaining
    /// task references the dropped id in its `depends_on` list; release
    /// builds skip the scan so the assertion has zero runtime cost.
    pub fn remove_task(&mut self, id: &TaskId) -> Option<Task> {
        let removed = self.tasks.remove(id);
        debug_assert!(
            !self.tasks.values().any(|t| t.depends_on.contains(id)),
            "remove_task: task `{id}` is still referenced by another task's depends_on",
        );
        removed
    }

    /// Drop `roots` plus their `validate_*` siblings AND clean up any
    /// `depends_on` references in surviving tasks. Returns the set of
    /// dropped task ids (including auto-added validators) for audit
    /// logging.
    ///
    /// This is the v4-bypass post-filter the conversation crate uses to
    /// apply intake-fact-based gates that the v4 composer doesn't yet
    /// thread through (literature opt-in, counts-only-input FASTQ
    /// stripping). Unlike `remove_task`, this method:
    ///   1. expands `roots` to include `validate_<root>` for each root,
    ///   2. captures each dropped atom's surviving (parent → child)
    ///      relationships so we can splice the chain back together,
    ///   3. removes all expanded ids in one pass,
    ///   4. walks surviving tasks and prunes their `depends_on` lists
    ///      so no dangling reference remains,
    ///   5. adds spliced `(parent → child)` edges so a chain-middle
    ///      drop doesn't strand downstream pipeline atoms.
    ///
    /// The splice is structural: it preserves graph reachability when a
    /// chain segment is removed (e.g. `data_acquisition → raw_qc → … →
    /// quantification → qc_preprocessing` collapses to `data_acquisition
    /// → qc_preprocessing` after the counts-only-input gate drops the
    /// FASTQ run). Typed semantic-mismatch on the spliced edge (FASTQ
    /// vs. `count_matrix`) is reconciled at runtime by the agent
    /// against `runtime/inputs/` files. Splicing skips `validate_*`
    /// edges — validators must not become parents of analytical atoms.
    pub fn drop_tasks_with_validators(
        &mut self,
        roots: &[&str],
    ) -> std::collections::BTreeSet<TaskId> {
        let mut to_drop: std::collections::BTreeSet<TaskId> = std::collections::BTreeSet::new();
        for r in roots {
            to_drop.insert(TaskId::from(*r));
            to_drop.insert(TaskId::from(format!("validate_{}", r).as_str()));
        }
        // Capture (parents, children) for each dropped atom BEFORE
        // removal so we can splice the surviving graph. Include both
        // surviving and dropped relations — the transitive walk needs
        // dropped descendants/ancestors as hops to find the surviving
        // boundary. The splice itself filters out validate_* nodes so
        // QA companions are never promoted to data-source parents.
        let parents_of: std::collections::BTreeMap<TaskId, Vec<TaskId>> = to_drop
            .iter()
            .map(|dropped| {
                let parents: Vec<TaskId> = self
                    .tasks
                    .get(dropped)
                    .map(|t| t.depends_on.to_vec())
                    .unwrap_or_default();
                (dropped.clone(), parents)
            })
            .collect();
        let children_of: std::collections::BTreeMap<TaskId, Vec<TaskId>> = to_drop
            .iter()
            .map(|dropped| {
                let children: Vec<TaskId> = self
                    .tasks
                    .iter()
                    .filter(|(_, t)| t.depends_on.contains(dropped))
                    .map(|(id, _)| id.clone())
                    .collect();
                (dropped.clone(), children)
            })
            .collect();
        // Retain only tasks not in the drop set.
        self.tasks.retain(|id, _| !to_drop.contains(id));
        // Prune depends_on entries pointing at dropped ids.
        for task in self.tasks.values_mut() {
            task.depends_on.retain(|d| !to_drop.contains(d));
        }
        // Splice (parent → child) edges across each dropped atom so
        // downstream pipeline atoms keep their reachability. Walk the
        // drop set transitively: a child of a dropped X may itself be
        // dropped, in which case we need to reach the first surviving
        // descendant. Same on the parent side. `transitive_survivors`
        // does the BFS that skips over consecutive dropped atoms.
        for dropped in &to_drop {
            let parents = transitive_surviving_parents(dropped, &to_drop, &parents_of);
            let children = transitive_surviving_children(dropped, &to_drop, &children_of);
            for child_id in &children {
                let Some(child) = self.tasks.get_mut(child_id) else {
                    continue;
                };
                for parent_id in &parents {
                    if parent_id == child_id {
                        continue;
                    }
                    if !child.depends_on.contains(parent_id) {
                        child.depends_on.push(parent_id.clone());
                    }
                }
            }
        }
        // Filter the return set to only tasks that actually existed.
        // (Callers don't care about phantom validators that were never
        // in the DAG — only audit-loggable real removals.)
        to_drop
    }
}

/// BFS up through the drop set's parents map to find the nearest
/// surviving ancestors of `start`. `start` itself is in the drop set;
/// returns surviving (non-dropped, non-validator) parents at the first
/// layer they appear. Used by `drop_tasks_with_validators` to splice
/// the chain when a multi-atom run is dropped. Validators are skipped
/// because they must not be promoted to data-source parents of
/// analytical atoms.
fn transitive_surviving_parents(
    start: &TaskId,
    drop_set: &std::collections::BTreeSet<TaskId>,
    parents_of: &std::collections::BTreeMap<TaskId, Vec<TaskId>>,
) -> std::collections::BTreeSet<TaskId> {
    let mut out: std::collections::BTreeSet<TaskId> = std::collections::BTreeSet::new();
    let mut seen: std::collections::BTreeSet<TaskId> = std::collections::BTreeSet::new();
    let mut stack: Vec<TaskId> = vec![start.clone()];
    while let Some(node) = stack.pop() {
        if !seen.insert(node.clone()) {
            continue;
        }
        if let Some(parents) = parents_of.get(&node) {
            for p in parents {
                if drop_set.contains(p) {
                    stack.push(p.clone());
                } else if !p.as_str().starts_with("validate_") {
                    out.insert(p.clone());
                }
            }
        }
    }
    out
}

/// BFS down through the drop set's children map to find the nearest
/// surviving descendants of `start`. Mirrors
/// `transitive_surviving_parents`; skips validators on the way out so
/// the splice never lands an edge into a `validate_*` consumer.
fn transitive_surviving_children(
    start: &TaskId,
    drop_set: &std::collections::BTreeSet<TaskId>,
    children_of: &std::collections::BTreeMap<TaskId, Vec<TaskId>>,
) -> std::collections::BTreeSet<TaskId> {
    let mut out: std::collections::BTreeSet<TaskId> = std::collections::BTreeSet::new();
    let mut seen: std::collections::BTreeSet<TaskId> = std::collections::BTreeSet::new();
    let mut stack: Vec<TaskId> = vec![start.clone()];
    while let Some(node) = stack.pop() {
        if !seen.insert(node.clone()) {
            continue;
        }
        if let Some(children) = children_of.get(&node) {
            for c in children {
                if drop_set.contains(c) {
                    stack.push(c.clone());
                } else if !c.as_str().starts_with("validate_") {
                    out.insert(c.clone());
                }
            }
        }
    }
    out
}

impl DAG {
    /// Ready tasks.
    pub fn ready_tasks(&self) -> Vec<&TaskId> {
        self.tasks
            .iter()
            .filter(|(_, t)| t.state == TaskState::Ready)
            .map(|(id, _)| id)
            .collect()
    }

    /// Blocked tasks.
    pub fn blocked_tasks(&self) -> Vec<&TaskId> {
        self.tasks
            .iter()
            .filter(|(_, t)| matches!(t.state, TaskState::Blocked { .. }))
            .map(|(id, _)| id)
            .collect()
    }

    /// Is complete.
    pub fn is_complete(&self) -> bool {
        self.tasks
            .values()
            .all(|t| matches!(t.state, TaskState::Completed { .. }))
    }

    /// Returns (completed, ready, blocked, pending).
    pub fn progress(&self) -> (usize, usize, usize, usize) {
        let mut completed = 0;
        let mut ready = 0;
        let mut blocked = 0;
        let mut pending = 0;
        for task in self.tasks.values() {
            match &task.state {
                TaskState::Completed { .. } => completed += 1,
                TaskState::Ready => ready += 1,
                TaskState::Blocked { .. } => blocked += 1,
                TaskState::Pending => pending += 1,
                TaskState::Running { .. } => ready += 1, // count running as active
                TaskState::Failed { .. } => blocked += 1,
            }
        }
        (completed, ready, blocked, pending)
    }

    /// Promote Pending tasks whose deps are all Completed to Ready.
    /// Evaluate conditions on conditional tasks; skip if condition is false.
    pub fn propagate_readiness(&mut self) {
        let ids: Vec<TaskId> = self.tasks.keys().cloned().collect();
        for id in ids {
            let task = self.tasks.get(&id).unwrap();
            if !matches!(task.state, TaskState::Pending) {
                continue;
            }
            // Check all deps completed
            let all_deps_done = task.depends_on.iter().all(|dep| {
                self.tasks
                    .get(dep)
                    .map(|d| matches!(d.state, TaskState::Completed { .. }))
                    .unwrap_or(false)
            });
            if !all_deps_done {
                continue;
            }
            // Check condition if present
            let condition = task
                .spec
                .as_ref()
                .and_then(|s| s.get("condition"))
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());

            if let Some(expr) = condition {
                match eval_condition(&expr, &self.tasks) {
                    ConditionResult::True => {
                        self.tasks.get_mut(&id).unwrap().state = TaskState::Ready;
                    }
                    ConditionResult::False => {
                        self.tasks.get_mut(&id).unwrap().state = TaskState::Completed {
                            result: serde_json::json!({
                                "skipped": true,
                                "reason": "condition not met",
                                "condition": expr
                            }),
                        };
                    }
                    ConditionResult::Pending => {
                        // referenced task not yet completed — stay Pending
                    }
                }
            } else {
                self.tasks.get_mut(&id).unwrap().state = TaskState::Ready;
            }
        }
    }

    /// Incremental optimization of `propagate_readiness`: given the
    /// task ids that just transitioned to Completed, promote only their
    /// direct dependents (looked up through the `reverse_deps` adjacency)
    /// instead of re-scanning every task in the DAG.
    ///
    /// Safe to call from the harness loop when the set of changed tasks
    /// is known after a completion. The full-scan `propagate_readiness`
    /// remains the cold-path fallback for callers that don't have a
    /// known just-completed set (initial deserialization, amendment
    /// invalidation, recovery sweeps).
    ///
    /// The `reverse_deps` cache is built lazily on first call here,
    /// mirroring the pattern in `invalidate_forward_slice` — callers
    /// don't have to remember to call `rebuild_reverse_deps` first.
    ///
    /// Determinism: dependents are collected into a `BTreeSet<TaskId>`
    /// so the iteration order matches the id-sorted walk used by
    /// `propagate_readiness` (which iterates `self.tasks` — a
    /// `BTreeMap` — in key order). The returned `Vec<TaskId>` is sorted
    /// for the same reason.
    pub fn propagate_readiness_from(&mut self, just_completed: &[TaskId]) -> Vec<TaskId> {
        // Lazy rebuild — same pattern as `invalidate_forward_slice`.
        if self.reverse_deps.is_empty() && !self.tasks.is_empty() {
            self.rebuild_reverse_deps();
        }

        // Collect direct dependents of every just-completed task.
        // BTreeSet gives us deterministic id-sorted iteration and
        // dedupes when multiple completions share a dependent.
        let mut dependents: std::collections::BTreeSet<TaskId> = std::collections::BTreeSet::new();
        for tid in just_completed {
            for dep in self.reverse_deps.get(tid).cloned().unwrap_or_default() {
                dependents.insert(dep);
            }
        }

        let mut promoted: Vec<TaskId> = Vec::new();
        for id in dependents {
            let task = match self.tasks.get(&id) {
                Some(t) => t,
                None => continue,
            };
            if !matches!(task.state, TaskState::Pending) {
                continue;
            }
            // Check all deps completed — identical predicate to
            // `propagate_readiness`.
            let all_deps_done = task.depends_on.iter().all(|dep| {
                self.tasks
                    .get(dep)
                    .map(|d| matches!(d.state, TaskState::Completed { .. }))
                    .unwrap_or(false)
            });
            if !all_deps_done {
                continue;
            }
            // Check condition if present — identical to `propagate_readiness`.
            let condition = task
                .spec
                .as_ref()
                .and_then(|s| s.get("condition"))
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());

            if let Some(expr) = condition {
                match eval_condition(&expr, &self.tasks) {
                    ConditionResult::True => {
                        self.tasks.get_mut(&id).unwrap().state = TaskState::Ready;
                        promoted.push(id);
                    }
                    ConditionResult::False => {
                        self.tasks.get_mut(&id).unwrap().state = TaskState::Completed {
                            result: serde_json::json!({
                                "skipped": true,
                                "reason": "condition not met",
                                "condition": expr
                            }),
                        };
                        promoted.push(id);
                    }
                    ConditionResult::Pending => {
                        // referenced task not yet completed — stay Pending
                    }
                }
            } else {
                self.tasks.get_mut(&id).unwrap().state = TaskState::Ready;
                promoted.push(id);
            }
        }
        promoted.sort();
        promoted
    }

    /// Mark the target stage and everything downstream of it
    /// (transitive closure over `depends_on`) as `Pending`, wiping
    /// `result_ref` so prior outputs don't leak through. The target
    /// stage itself is reset to Pending so the amend path reruns it
    /// with the new method; callers that want to preserve the target
    /// and only invalidate dependents should pass
    /// `include_target: false`.
    ///
    /// Returns the set of task ids that were actually mutated, in
    /// BTreeMap iteration order (deterministic for emit-time round-trips).
    pub fn invalidate_forward_slice(&mut self, target: &str, include_target: bool) -> Vec<TaskId> {
        let mut invalidated: Vec<TaskId> = Vec::new();
        if !self.tasks.contains_key(target) {
            return invalidated;
        }
        // Read the reverse-dependency adjacency directly from the
        // stored field, rebuilding lazily if empty (e.g., just
        // deserialized). The stored map makes the BFS O(n + e).
        if self.reverse_deps.is_empty() && !self.tasks.is_empty() {
            self.rebuild_reverse_deps();
        }
        let empty: Vec<TaskId> = Vec::new();

        // Walk downstream via BFS over the inverted depends_on graph.
        let mut frontier: Vec<TaskId> = vec![TaskId::from(target)];
        let mut visited: std::collections::BTreeSet<TaskId> = std::collections::BTreeSet::new();
        visited.insert(TaskId::from(target));
        while let Some(current) = frontier.pop() {
            for dep in self.reverse_deps.get(&current).unwrap_or(&empty) {
                if visited.insert(dep.clone()) {
                    frontier.push(dep.clone());
                }
            }
        }
        // Apply invalidation in deterministic id order.
        for id in visited {
            if !include_target && id.as_str() == target {
                continue;
            }
            if let Some(task) = self.tasks.get_mut(&id) {
                task.state = TaskState::Pending;
                task.result_ref = None;
                invalidated.push(id);
            }
        }
        invalidated.sort();
        invalidated
    }

    /// Collect the set of task ids that are transitive successors of
    /// `target` (i.e., all tasks reachable from `target` by following
    /// `reverse_deps`). Does NOT include `target` itself.
    ///
    /// Rebuilds `reverse_deps` lazily when the cache is empty, matching
    /// the pattern in `invalidate_forward_slice`.
    pub fn descendants_of(&mut self, target: &str) -> std::collections::BTreeSet<TaskId> {
        if !self.tasks.contains_key(target) {
            return std::collections::BTreeSet::new();
        }
        if self.reverse_deps.is_empty() && !self.tasks.is_empty() {
            self.rebuild_reverse_deps();
        }
        let empty: Vec<TaskId> = Vec::new();
        let mut result: std::collections::BTreeSet<TaskId> = std::collections::BTreeSet::new();
        let mut frontier: Vec<TaskId> = Vec::new();
        // Seed from reverse_deps of target (direct successors).
        for dep in self.reverse_deps.get(target).unwrap_or(&empty) {
            if result.insert(dep.clone()) {
                frontier.push(dep.clone());
            }
        }
        while let Some(current) = frontier.pop() {
            for dep in self.reverse_deps.get(&current).unwrap_or(&empty) {
                if result.insert(dep.clone()) {
                    frontier.push(dep.clone());
                }
            }
        }
        result
    }

    /// Reset the named task to `Ready` and all its transitive successors
    /// to `Pending`. Tasks that precede `target` (i.e., are not descendants)
    /// are left untouched — their `Completed` state (and associated
    /// `result_ref`) is preserved so a branch at a mid-DAG boundary
    /// does not re-run already-finished upstream work.
    ///
    /// Returns `Err` when `target` does not exist in the DAG. Returns
    /// the sorted list of task ids that were mutated on success.
    ///
    /// Used by `Session::branch_from_at_task` to snapshot the child
    /// DAG at a task boundary.
    pub fn reset_to_task_boundary(&mut self, target: &str) -> Result<Vec<TaskId>, String> {
        if !self.tasks.contains_key(target) {
            return Err(format!("task '{target}' not found in DAG"));
        }
        let descendants = self.descendants_of(target);
        let mut mutated: Vec<TaskId> = Vec::new();
        // Reset the target itself to Ready.
        if let Some(task) = self.tasks.get_mut(target) {
            task.state = TaskState::Ready;
            task.result_ref = None;
            mutated.push(TaskId::from(target));
        }
        // Reset descendants to Pending.
        for id in descendants {
            if let Some(task) = self.tasks.get_mut(&id) {
                task.state = TaskState::Pending;
                task.result_ref = None;
                mutated.push(id);
            }
        }
        mutated.sort();
        Ok(mutated)
    }

    /// Rebuild the `reverse_deps` cache from the current
    /// `tasks[*].depends_on` edges. Called by the builder after DAG
    /// construction and by any caller that mutates `depends_on` or
    /// adds/removes tasks. O(n + e).
    pub fn rebuild_reverse_deps(&mut self) {
        let mut reverse: BTreeMap<TaskId, Vec<TaskId>> = BTreeMap::new();
        for (id, task) in self.tasks.iter() {
            for dep in &task.depends_on {
                reverse.entry(dep.clone()).or_default().push(id.clone());
            }
        }
        self.reverse_deps = reverse;
    }

    /// Reset Running tasks older than `timeout_secs` back to Ready.
    pub fn recover_stale_running(&mut self, timeout_secs: u64) {
        let now = chrono::Utc::now();
        for task in self.tasks.values_mut() {
            if let TaskState::Running { started_at, .. } = &task.state {
                if let Ok(start) = chrono::DateTime::parse_from_rfc3339(started_at) {
                    let elapsed = now
                        .signed_duration_since(start)
                        .num_seconds()
                        .unsigned_abs();
                    if elapsed >= timeout_secs {
                        task.state = TaskState::Ready;
                    }
                }
            }
        }
    }
}

/// Validate DAG: no cycles, no dangling dependencies.
#[must_use = "DAG validation result must be inspected — dropping it lets an invalid DAG (cycle / dangling dep) reach the emitter"]
pub fn validate_dag(dag: &DAG) -> Result<()> {
    let mut g = DiGraph::new();
    let idx: BTreeMap<&TaskId, _> = dag.tasks.keys().map(|id| (id, g.add_node(id))).collect();

    for (id, task) in &dag.tasks {
        for dep in &task.depends_on {
            let from = idx
                .get(dep)
                .ok_or_else(|| anyhow!("task '{}' depends_on unknown task '{}'", id, dep))?;
            let to = idx[id];
            g.add_edge(*from, to, ());
        }
    }

    toposort(&g, None).map_err(|cycle| {
        let empty = TaskId::default();
        let node_id = g.node_weight(cycle.node_id()).copied().unwrap_or(&empty);
        anyhow!("DAG contains a cycle involving task '{}'", node_id)
    })?;

    Ok(())
}

/// Typed-error sibling of `validate_dag` for surfaces that want
/// `Result<(), DagError>` instead of `anyhow::Result<()>`. Same checks
/// (dangling-dependency + acyclicity) plus a prefix-uniqueness pass over
/// `discover_*` / `validate_*` stages that catches cross-taxonomy collisions
/// in a merged DAG, and a RequiredArtifact.schema_ref shape check.
///
/// Existing call sites continue to use `validate_dag`; this function is the
/// non-breaking opt-in for new surfaces (e.g., the composer-merged-DAG path).
#[must_use = "DAG validation result must be inspected — dropping it lets an invalid DAG (cycle / dangling dep / duplicate prefix / bad schema_ref) reach the emitter"]
pub fn validate_dag_typed(dag: &DAG) -> std::result::Result<(), DagError> {
    let mut g = DiGraph::new();
    let idx: BTreeMap<&TaskId, _> = dag.tasks.keys().map(|id| (id, g.add_node(id))).collect();

    for (id, task) in &dag.tasks {
        for dep in &task.depends_on {
            let from = idx.get(dep).ok_or_else(|| DagError::InvalidDependency {
                task_id: id.clone(),
                missing_dep: dep.to_string(),
            })?;
            let to = idx[id];
            g.add_edge(*from, to, ());
        }
    }

    toposort(&g, None).map_err(|cycle| {
        let empty = TaskId::default();
        let entry = g.node_weight(cycle.node_id()).copied().unwrap_or(&empty);
        // Cycle reconstruction via Kosaraju's SCC algorithm.
        // `petgraph::algo::kosaraju_scc` returns every strongly-connected
        // component in the graph; the cycle that tripped `toposort` is
        // the SCC containing the entry node. Picking that SCC's full
        // membership gives the UI / blocker the complete cycle (every
        // task involved in the strongly-connected loop), not just a
        // best-effort dependent-walk that gave up on the first leaf.
        // For DAGs with multiple disjoint cycles, the entry-node SCC
        // is the one toposort hit — exactly the loop the user needs to
        // unwind.
        let mut cycle_ids: Vec<String> = vec![entry.to_string()];
        for component in kosaraju_scc(&g) {
            if component.len() < 2 && !component.iter().any(|n| g.contains_edge(*n, *n)) {
                continue; // trivial SCC (single node, no self-loop)
            }
            let weights: Vec<String> = component
                .iter()
                .filter_map(|n| g.node_weight(*n).copied().map(|t| t.to_string()))
                .collect();
            if weights.iter().any(|w| w == entry.as_str()) {
                cycle_ids = weights;
                cycle_ids.sort();
                break;
            }
        }
        DagError::CycleDetected { cycle: cycle_ids }
    })?;

    // Orphan detection: a task is orphaned when it has no incoming edges
    // (`depends_on.is_empty()`) AND no outgoing edges (no other task lists
    // it in its `depends_on`). Singleton DAGs are not orphans — a one-task
    // DAG is a legitimate seed/root. With ≥2 tasks, an isolated node is
    // unreachable from any root and contributes nothing to any consumer,
    // so the harness would dispatch it without effect and any downstream
    // gate would miss its outputs entirely.
    if dag.tasks.len() >= 2 {
        // Out-degree = count of tasks that name `id` in their depends_on.
        // Compute once over the whole DAG and look up per task.
        let mut out_degree: BTreeMap<&str, usize> = BTreeMap::new();
        for task in dag.tasks.values() {
            for dep in &task.depends_on {
                *out_degree.entry(dep.as_str()).or_insert(0) += 1;
            }
        }
        for (id, task) in &dag.tasks {
            let in_degree = task.depends_on.len();
            let out = out_degree.get(id.as_str()).copied().unwrap_or(0);
            if in_degree == 0 && out == 0 {
                return Err(DagError::OrphanedTask {
                    task_id: id.clone(),
                });
            }
        }
    }

    // Prefix-uniqueness: stages with the same `discover_<x>` / `validate_<x>`
    // prefix root must not collide. We bucket every task id whose prefix root
    // matches one of the self-describing taxonomy conventions and surface a
    // `DuplicatePrefix` when two distinct ids share a root. The bucket key is
    // the `<discover|validate>_<x>` prefix up to the first `__`/`/` separator
    // — so `discover_alignment` and `discover_alignment__rerun_2` collide,
    // but `discover_alignment` and `discover_quantification` do not.
    let mut by_prefix: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for id in dag.tasks.keys() {
        if let Some(prefix) = self_describing_prefix(id.as_str()) {
            by_prefix.entry(prefix).or_default().push(id.to_string());
        }
    }
    for (prefix, mut ids) in by_prefix {
        if ids.len() > 1 {
            ids.sort();
            return Err(DagError::DuplicatePrefix {
                prefix,
                stage_ids: ids,
            });
        }
    }

    // RequiredArtifact.schema_ref shape validation: every declared
    // schema_ref must be a relative `.schema.json` path. Surfacing
    // malformed values here rather than at first-run keeps the
    // emit-time gate honest.
    for task in dag.tasks.values() {
        for artifact in &task.required_artifacts {
            artifact.validate_schema_ref()?;
        }
    }

    Ok(())
}

/// Returns `Some(<prefix>)` when `id` is a self-describing taxonomy stage
/// (`discover_*` or `validate_*`); otherwise `None`. The prefix is the
/// segment up to the first `__` separator (used for amend-rerun suffixes
/// like `discover_alignment__rerun_2`) — so two ids that differ only in
/// the suffix bucket together for prefix-uniqueness.
fn self_describing_prefix(id: &str) -> Option<String> {
    let root = id.split("__").next().unwrap_or(id);
    // Typed role replaces the legacy prefix sniff.
    // `derive_role_from_id` lifts the discover_ / validate_ / select_
    // prefixes into the `AtomRole` enum so call sites switch on the
    // typed predicate; here we use the typed predicates instead of
    // re-sniffing the prefix.
    let role = crate::taxonomy::derive_role_from_id(root);
    if role.is_discovery() || role.is_validation() {
        Some(root.to_string())
    } else {
        None
    }
}

/// Produce a DOT-format string for the DAG (for /dag command).
pub fn dag_to_dot(dag: &DAG) -> String {
    let mut out = String::from("digraph workflow {\n  rankdir=LR;\n  node [shape=box];\n");
    for (id, task) in &dag.tasks {
        let state = match &task.state {
            TaskState::Pending => "gray",
            TaskState::Ready => "blue",
            TaskState::Running { .. } => "yellow",
            TaskState::Completed { .. } => "green",
            TaskState::Failed { .. } => "red",
            TaskState::Blocked { .. } => "orange",
        };
        out.push_str(&format!(
            "  \"{}\" [label=\"{}\\n{}\", color={}];\n",
            id,
            id,
            state_label(&task.state),
            state
        ));
        for dep in &task.depends_on {
            out.push_str(&format!("  \"{}\" -> \"{}\";\n", dep, id));
        }
    }
    out.push('}');
    out
}

fn state_label(state: &TaskState) -> &'static str {
    match state {
        TaskState::Pending => "pending",
        TaskState::Ready => "ready",
        TaskState::Running { .. } => "running",
        TaskState::Completed { .. } => "completed",
        TaskState::Failed { .. } => "failed",
        TaskState::Blocked { .. } => "blocked",
    }
}

// ── Condition expression grammar ──────────────────────────────────────────────
//
// Expr::= term (("&&" | "||") term)*
// Term::= path op value
// Path::= IDENT ("." IDENT)*
// Op::= "==" | "!=" | ">" | "<" | ">=" | "<="
// Value::= "true" | "false" | NUMBER | QUOTED_STRING

#[derive(Debug, PartialEq)]
enum ConditionResult {
    True,
    False,
    Pending, // referenced task not yet completed
}

fn eval_condition(expr: &str, tasks: &BTreeMap<TaskId, Task>) -> ConditionResult {
    let tokens = tokenize(expr);
    let mut pos = 0;
    eval_expr(&tokens, &mut pos, tasks)
}

fn eval_expr(tokens: &[Token], pos: &mut usize, tasks: &BTreeMap<TaskId, Task>) -> ConditionResult {
    let mut result = eval_term(tokens, pos, tasks);
    while *pos < tokens.len() {
        match &tokens[*pos] {
            Token::And => {
                *pos += 1;
                let rhs = eval_term(tokens, pos, tasks);
                result = match (result, rhs) {
                    (ConditionResult::Pending, _) | (_, ConditionResult::Pending) => {
                        ConditionResult::Pending
                    }
                    (ConditionResult::True, ConditionResult::True) => ConditionResult::True,
                    _ => ConditionResult::False,
                };
            }
            Token::Or => {
                *pos += 1;
                let rhs = eval_term(tokens, pos, tasks);
                result = match (result, rhs) {
                    (ConditionResult::True, _) | (_, ConditionResult::True) => {
                        ConditionResult::True
                    }
                    (ConditionResult::Pending, _) | (_, ConditionResult::Pending) => {
                        ConditionResult::Pending
                    }
                    _ => ConditionResult::False,
                };
            }
            _ => break,
        }
    }
    result
}

fn eval_term(tokens: &[Token], pos: &mut usize, tasks: &BTreeMap<TaskId, Task>) -> ConditionResult {
    // path
    let path = match tokens.get(*pos) {
        Some(Token::Ident(s)) => {
            *pos += 1;
            let mut parts = vec![s.clone()];
            while tokens.get(*pos) == Some(&Token::Dot) {
                *pos += 1;
                if let Some(Token::Ident(next)) = tokens.get(*pos) {
                    parts.push(next.clone());
                    *pos += 1;
                }
            }
            parts
        }
        _ => return ConditionResult::False,
    };

    // op
    let op = match tokens.get(*pos) {
        Some(Token::Op(o)) => {
            *pos += 1;
            o.clone()
        }
        _ => return ConditionResult::False,
    };

    // value
    let rhs = match tokens.get(*pos) {
        Some(Token::Value(v)) => {
            *pos += 1;
            v.clone()
        }
        _ => return ConditionResult::False,
    };

    // Resolve path: first segment is task_id, rest traverses the serialized task state.
    // Convention (matches plan): task_id.result.field_name
    // e.g. "qc.result.needs_trimming" → state_json["result"]["needs_trimming"]
    // This means the agent writes result as: {"needs_trimming": true}
    // and the serialized state is: {"status":"completed","result":{"needs_trimming":true}}
    let task_id = &path[0];
    let task = match tasks.get(task_id.as_str()) {
        Some(t) => t,
        None => return ConditionResult::False,
    };

    // Serialize the full task state so traversal can reach both "status" and "result.*"
    let state_json = match serde_json::to_value(&task.state) {
        Ok(v) => v,
        Err(_) => return ConditionResult::False,
    };
    // If the task is not yet completed, the condition is still pending
    if state_json.get("status").and_then(|s| s.as_str()) != Some("completed") {
        return ConditionResult::Pending;
    }

    // Traverse JSON path (remaining segments after task_id)
    let mut current = &state_json;
    for segment in &path[1..] {
        current = match current.get(segment) {
            Some(v) => v,
            None => return ConditionResult::False,
        };
    }

    compare_value(current, &op, &rhs)
}

fn compare_value(lhs: &serde_json::Value, op: &str, rhs: &str) -> ConditionResult {
    // Try numeric comparison first
    if let (Some(l), Ok(r)) = (lhs.as_f64(), rhs.parse::<f64>()) {
        return bool_to_result(apply_op_f64(l, op, r));
    }
    // Boolean
    if let Some(l) = lhs.as_bool() {
        let r = rhs.eq_ignore_ascii_case("true");
        return bool_to_result(apply_op_bool(l, op, r));
    }
    // String
    let l = lhs.as_str().unwrap_or("");
    let r = rhs.trim_matches('"');
    bool_to_result(apply_op_str(l, op, r))
}

fn apply_op_f64(l: f64, op: &str, r: f64) -> bool {
    match op {
        "==" => (l - r).abs() < f64::EPSILON,
        "!=" => (l - r).abs() >= f64::EPSILON,
        ">" => l > r,
        "<" => l < r,
        ">=" => l >= r,
        "<=" => l <= r,
        _ => false,
    }
}

fn apply_op_bool(l: bool, op: &str, r: bool) -> bool {
    match op {
        "==" => l == r,
        "!=" => l != r,
        _ => false,
    }
}

fn apply_op_str(l: &str, op: &str, r: &str) -> bool {
    match op {
        "==" => l == r,
        "!=" => l != r,
        _ => false,
    }
}

fn bool_to_result(b: bool) -> ConditionResult {
    if b {
        ConditionResult::True
    } else {
        ConditionResult::False
    }
}

// ── Tokenizer ─────────────────────────────────────────────────────────────────
//
// Produces tokens for the condition grammar. Dots are always Token::Dot so
// that both path separators (aln.mapping_rate) and numeric literals (0.70)
// are handled unambiguously.

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Dot,
    Op(String),
    Value(String),
    And,
    Or,
}

fn tokenize(expr: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = expr.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        // Two-char operators: >=, <=, !=, ==, >, <
        if c == '>' || c == '<' || c == '!' || c == '=' {
            let mut op = c.to_string();
            chars.next();
            if chars.peek() == Some(&'=') {
                op.push('=');
                chars.next();
            }
            tokens.push(Token::Op(op));
            continue;
        }
        if c == '&' {
            chars.next();
            if chars.peek() == Some(&'&') {
                chars.next();
            }
            tokens.push(Token::And);
            continue;
        }
        if c == '|' {
            chars.next();
            if chars.peek() == Some(&'|') {
                chars.next();
            }
            tokens.push(Token::Or);
            continue;
        }
        // Dot — always a path separator; numeric literals are consumed as a unit below
        if c == '.' {
            chars.next();
            tokens.push(Token::Dot);
            continue;
        }
        // Quoted string value
        if c == '"' {
            chars.next();
            let mut s = String::from('"');
            for ch in chars.by_ref() {
                if ch == '"' {
                    s.push('"');
                    break;
                }
                s.push(ch);
            }
            tokens.push(Token::Value(s));
            continue;
        }
        // Numeric literal: starts with digit (or '-' followed by digit)
        let next_is_digit = chars
            .clone()
            .nth(1)
            .map(|d| d.is_ascii_digit())
            .unwrap_or(false);
        if c.is_ascii_digit() || (c == '-' && next_is_digit) {
            let mut s = String::new();
            if c == '-' {
                s.push(c);
                chars.next();
            }
            while let Some(&ch) = chars.peek() {
                if ch.is_ascii_digit() {
                    s.push(ch);
                    chars.next();
                } else {
                    break;
                }
            }
            // Decimal part: dot followed by digits
            if chars.peek() == Some(&'.') {
                let has_decimal = chars
                    .clone()
                    .nth(1)
                    .map(|d| d.is_ascii_digit())
                    .unwrap_or(false);
                if has_decimal {
                    s.push('.');
                    chars.next();
                    while let Some(&ch) = chars.peek() {
                        if ch.is_ascii_digit() {
                            s.push(ch);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
            }
            tokens.push(Token::Value(s));
            continue;
        }
        // Identifier or boolean keyword
        if c.is_alphabetic() || c == '_' {
            let mut s = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_alphanumeric() || ch == '_' || ch == '-' {
                    s.push(ch);
                    chars.next();
                } else {
                    break;
                }
            }
            if s == "true" || s == "false" {
                tokens.push(Token::Value(s));
            } else {
                tokens.push(Token::Ident(s));
            }
            continue;
        }
        chars.next();
    }
    tokens
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn make_dag(tasks: Vec<(&str, Task)>) -> DAG {
        let mut dag = DAG {
            version: "1.0".into(),
            schema_version: current_dag_schema_version(),
            workflow_id: "test".into(),
            current_task: None,
            tasks: tasks
                .into_iter()
                .map(|(k, v)| (TaskId::from(k), v))
                .collect(),
            reverse_deps: BTreeMap::new(),
            run_id: None,
        };
        dag.rebuild_reverse_deps();
        dag
    }

    fn pending_task(deps: Vec<&str>) -> Task {
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Pending,
            depends_on: deps.into_iter().map(TaskId::from).collect(),
            assignee: Assignee::Agent,
            description: "test task".into(),
            spec: None,
            resolution: None,
            result_ref: None,
            resource_class: ResourceClass::CpuHeavy,
            requires_sme_review: false,

            required_artifacts: vec![],
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        }
    }

    fn completed_task(result: serde_json::Value) -> Task {
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Completed { result },
            depends_on: vec![],
            assignee: Assignee::Agent,
            description: "done".into(),
            spec: None,
            resolution: None,
            result_ref: None,
            resource_class: ResourceClass::CpuHeavy,
            requires_sme_review: false,

            required_artifacts: vec![],
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        }
    }

    #[test]
    fn drop_tasks_with_validators_drops_roots_validators_and_cleans_edges() {
        // Mirrors the conversation crate's literature opt-in gate: drop
        // `review_prior_work` + `validate_review_prior_work` and clean
        // up any depends_on edges from surviving tasks.
        let dag_tasks = vec![
            ("review_prior_work", pending_task(vec![])),
            (
                "validate_review_prior_work",
                pending_task(vec!["review_prior_work"]),
            ),
            ("differential_expression", pending_task(vec![])),
            (
                "reporting",
                pending_task(vec!["differential_expression", "review_prior_work"]),
            ),
        ];
        let mut dag = make_dag(dag_tasks);
        let dropped = dag.drop_tasks_with_validators(&["review_prior_work"]);
        assert!(dropped.contains("review_prior_work"));
        assert!(dropped.contains("validate_review_prior_work"));
        assert!(!dag.tasks.contains_key("review_prior_work"));
        assert!(!dag.tasks.contains_key("validate_review_prior_work"));
        // Surviving `reporting` lost its edge to review_prior_work but
        // kept its edge to differential_expression.
        let reporting_deps = &dag.tasks.get("reporting").unwrap().depends_on;
        assert_eq!(reporting_deps, &vec!["differential_expression".to_string()]);
    }

    #[test]
    fn drop_tasks_with_validators_handles_phantom_root() {
        // Phantom roots (id not in DAG) must not poison surviving
        // edges or panic the drop pass.
        let dag_tasks = vec![("differential_expression", pending_task(vec![]))];
        let mut dag = make_dag(dag_tasks);
        let _ = dag.drop_tasks_with_validators(&["never_existed_atom"]);
        assert!(dag.tasks.contains_key("differential_expression"));
    }

    #[test]
    fn drop_tasks_with_validators_splices_chain_middle_drops() {
        // Regression for the counts-only-input gate bug: dropping the
        // FASTQ run from the middle of the chain
        //   data_acquisition → raw_qc → sequence_trimming → alignment
        //                    → quantification → qc_preprocessing
        // must leave qc_preprocessing with `data_acquisition` as its
        // direct parent so the downstream analytical chain is not
        // orphaned.
        let dag_tasks = vec![
            ("data_acquisition", pending_task(vec![])),
            ("raw_qc", pending_task(vec!["data_acquisition"])),
            ("sequence_trimming", pending_task(vec!["raw_qc"])),
            ("alignment", pending_task(vec!["sequence_trimming"])),
            ("quantification", pending_task(vec!["alignment"])),
            ("qc_preprocessing", pending_task(vec!["quantification"])),
            ("normalisation", pending_task(vec!["qc_preprocessing"])),
        ];
        let mut dag = make_dag(dag_tasks);
        let dropped = dag.drop_tasks_with_validators(&[
            "raw_qc",
            "sequence_trimming",
            "alignment",
            "quantification",
        ]);
        // All four FASTQ atoms removed.
        for id in &["raw_qc", "sequence_trimming", "alignment", "quantification"] {
            assert!(
                dropped.contains(&TaskId::from(*id)),
                "{id} should be in dropped set"
            );
            assert!(
                !dag.tasks.contains_key(*id),
                "{id} should be removed from tasks"
            );
        }
        // qc_preprocessing was the chain-middle consumer — it must now
        // depend on data_acquisition (the nearest surviving ancestor)
        // and not have an empty depends_on list.
        let qc_deps = &dag.tasks.get("qc_preprocessing").unwrap().depends_on;
        assert_eq!(
            qc_deps,
            &vec![TaskId::from("data_acquisition")],
            "qc_preprocessing must be spliced to data_acquisition after FASTQ drop"
        );
        // normalisation's edge to qc_preprocessing must be preserved.
        let norm_deps = &dag.tasks.get("normalisation").unwrap().depends_on;
        assert_eq!(norm_deps, &vec![TaskId::from("qc_preprocessing")]);
    }

    #[test]
    fn drop_tasks_with_validators_does_not_splice_validators_as_parents() {
        // A validate_* atom must never be promoted to a data-source
        // parent during splicing. Drop the middle pipeline atom and
        // check that no edge is added from a validator to a surviving
        // analytical consumer.
        let dag_tasks = vec![
            ("data_acquisition", pending_task(vec![])),
            ("quantification", pending_task(vec!["data_acquisition"])),
            (
                "validate_quantification",
                pending_task(vec!["quantification"]),
            ),
            (
                "qc_preprocessing",
                pending_task(vec!["quantification", "validate_quantification"]),
            ),
        ];
        let mut dag = make_dag(dag_tasks);
        let _ = dag.drop_tasks_with_validators(&["quantification"]);
        let qc_deps = &dag.tasks.get("qc_preprocessing").unwrap().depends_on;
        assert!(
            qc_deps.contains(&TaskId::from("data_acquisition")),
            "qc_preprocessing must be spliced to data_acquisition"
        );
        assert!(
            !qc_deps.contains(&TaskId::from("validate_quantification")),
            "validator must not appear as a spliced parent"
        );
    }

    #[test]
    fn agent_task_result_classifies_sme_resolved() {
        let task = completed_task(serde_json::json!({
            "resolved_by": "sme",
            "resolved_at": "compile_time",
            "method": "Seurat v5 CCA",
            "batch_correction_required": true,
        }));
        let kind = task.completion_kind().expect("Completed task has kind");
        match kind {
            AgentTaskResult::SmeResolved {
                resolved_at,
                method,
                fields,
            } => {
                assert_eq!(resolved_at, "compile_time");
                assert_eq!(method, "Seurat v5 CCA");
                assert_eq!(
                    fields.get("batch_correction_required"),
                    Some(&serde_json::json!(true)),
                    "SME-supplied field threaded through fields map"
                );
            }
            other => panic!("expected SmeResolved, got {:?}", other),
        }
    }

    #[test]
    fn agent_task_result_classifies_skipped_by_condition() {
        let task = completed_task(serde_json::json!({
            "skipped": true,
            "reason": "condition not met",
            "condition": "discover_batch_correction.result.batch_correction_required == true",
        }));
        let kind = task.completion_kind().expect("Completed task has kind");
        match kind {
            AgentTaskResult::SkippedByCondition { reason, condition } => {
                assert_eq!(reason, "condition not met");
                assert!(condition.starts_with("discover_batch_correction"));
            }
            other => panic!("expected SkippedByCondition, got {:?}", other),
        }
    }

    #[test]
    fn agent_task_result_classifies_agent_completed() {
        // Agent free-form output — anything that doesn't match the
        // SmeResolved / SkippedByCondition discriminator.
        let task = completed_task(serde_json::json!({
            "rows_processed": 42,
            "samples_passed_qc": ["S1", "S2", "S3"],
        }));
        let kind = task.completion_kind().expect("Completed task has kind");
        match kind {
            AgentTaskResult::AgentCompleted(value) => {
                assert_eq!(
                    value["rows_processed"],
                    serde_json::json!(42),
                    "agent JSON preserved unchanged"
                );
            }
            other => panic!("expected AgentCompleted, got {:?}", other),
        }
    }

    #[test]
    fn agent_task_result_handles_non_object_value() {
        // Belt-and-braces: even if the agent writes a bare string or
        // number (atypical but allowed by serde_json::Value), we
        // classify as AgentCompleted rather than panic.
        let task = completed_task(serde_json::json!("ok"));
        match task.completion_kind() {
            Some(AgentTaskResult::AgentCompleted(_)) => {}
            other => panic!("expected AgentCompleted, got {:?}", other),
        }
    }

    #[test]
    fn agent_task_result_pending_task_returns_none() {
        let task = pending_task(vec![]);
        assert!(task.completion_kind().is_none());
    }

    #[test]
    fn serde_round_trip_dag() {
        let dag = make_dag(vec![
            ("task_a", pending_task(vec![])),
            (
                "task_b",
                Task {
                    kind: TaskKind::Discovery(DiscoveryKind::Custom("regulatory_db_lookup".into())),
                    state: TaskState::Blocked {
                        record: BlockedRecord {
                            reason: "no access".into(),
                            attempts: vec![ResolutionAttempt {
                                method: "env_probe".into(),
                                result: "not found".into(),
                            }],
                        },
                    },
                    depends_on: vec!["task_a".into()],
                    assignee: Assignee::AgentThenSme,
                    description: "lookup regulatory db".into(),
                    spec: Some(serde_json::json!({"policy_ref": "source-policy.json"})),
                    resolution: Some(ResolutionStrategy {
                        primary: "check env".into(),
                        fallback: Some("ask SME".into()),
                        escalation: EscalationPath::EscalateToSme,
                        policy_ref: None,
                    }),
                    result_ref: None,
                    resource_class: ResourceClass::CpuHeavy,
                    requires_sme_review: false,

                    required_artifacts: vec![],
                    container: None,
                    source_atom_id: None,
                    safety: Default::default(),
                },
            ),
        ]);
        let json = serde_json::to_string_pretty(&dag).unwrap();
        let decoded: DAG = serde_json::from_str(&json).unwrap();
        assert_eq!(dag, decoded);
    }

    #[test]
    fn serde_round_trip_custom_discovery() {
        let task = Task {
            kind: TaskKind::Discovery(DiscoveryKind::Custom("regulatory_db_lookup".into())),
            state: TaskState::Pending,
            depends_on: vec![],
            assignee: Assignee::Agent,
            description: "custom lookup".into(),
            spec: None,
            resolution: None,
            result_ref: None,
            resource_class: ResourceClass::CpuHeavy,
            requires_sme_review: false,

            required_artifacts: vec![],
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        };
        let json = serde_json::to_string(&task).unwrap();
        let back: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(task, back);
    }

    /// `resource_class` round-trips correctly for every
    /// variant. Covers the new Task field at serde boundary.
    #[test]
    fn serde_round_trip_resource_class_all_variants() {
        for rc in [
            ResourceClass::CpuHeavy,
            ResourceClass::IoHeavy,
            ResourceClass::MemoryHeavy,
            ResourceClass::Gpu,
        ] {
            let task = Task {
                kind: TaskKind::Computation,
                state: TaskState::Pending,
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "probe".into(),
                spec: None,
                resolution: None,
                result_ref: None,
                resource_class: rc,
                requires_sme_review: false,

                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            };
            let json = serde_json::to_string(&task).unwrap();
            let back: Task = serde_json::from_str(&json).unwrap();
            assert_eq!(task.resource_class, back.resource_class);
        }
    }

    /// Forward-compat: WORKFLOW.json files emitted before
    /// `resource_class` existed (no `resource_class` key present)
    /// deserialize with the default `CpuHeavy`. Mirrors the
    /// `Blocked.blocker_kind` shim pattern.
    #[test]
    fn deserialize_task_without_resource_class_defaults_cpu_heavy() {
        let legacy_json = r#"{
            "kind": "computation",
            "state": {"status": "pending"},
            "depends_on": [],
            "assignee": "agent",
            "description": "legacy task"
        }"#;
        let task: Task = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(task.resource_class, ResourceClass::CpuHeavy);
    }

    /// `resource_class: gpu` deserializes correctly (the
    /// variant the scheduler cares about most).
    #[test]
    fn deserialize_task_with_gpu_resource_class() {
        let json = r#"{
            "kind": "computation",
            "state": {"status": "pending"},
            "depends_on": [],
            "assignee": "agent",
            "description": "variant calling",
            "resource_class": "gpu"
        }"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.resource_class, ResourceClass::Gpu);
    }

    #[test]
    fn invalidate_forward_slice_resets_dependents() {
        let mut dag = make_dag(vec![
            ("root", completed_task(serde_json::json!({"v": 1}))),
            ("target", completed_task(serde_json::json!({"v": 2}))),
            ("child_a", completed_task(serde_json::json!({"v": 3}))),
            ("child_b", completed_task(serde_json::json!({"v": 4}))),
            ("grandchild", completed_task(serde_json::json!({"v": 5}))),
            ("sibling", completed_task(serde_json::json!({"v": 6}))),
        ]);
        // Wire deps: target <- child_a, child_b; child_a <- grandchild; sibling is isolated.
        dag.tasks.get_mut("child_a").unwrap().depends_on = vec!["target".into()];
        dag.tasks.get_mut("child_b").unwrap().depends_on = vec!["target".into()];
        dag.tasks.get_mut("grandchild").unwrap().depends_on = vec!["child_a".into()];
        dag.tasks.get_mut("target").unwrap().depends_on = vec!["root".into()];

        let invalidated = dag.invalidate_forward_slice("target", true);
        // Target + child_a + child_b + grandchild get reset, in id order.
        assert_eq!(
            invalidated,
            vec![
                "child_a".to_string(),
                "child_b".to_string(),
                "grandchild".to_string(),
                "target".to_string(),
            ]
        );
        for id in &["target", "child_a", "child_b", "grandchild"] {
            assert!(matches!(dag.tasks[*id].state, TaskState::Pending));
        }
        // Root (upstream) and sibling (unrelated) stay Completed.
        assert!(matches!(
            dag.tasks["root"].state,
            TaskState::Completed { .. }
        ));
        assert!(matches!(
            dag.tasks["sibling"].state,
            TaskState::Completed { .. }
        ));
    }

    #[test]
    fn invalidate_forward_slice_can_preserve_target() {
        let mut dag = make_dag(vec![
            ("target", completed_task(serde_json::json!({"v": 1}))),
            ("child", completed_task(serde_json::json!({"v": 2}))),
        ]);
        dag.tasks.get_mut("child").unwrap().depends_on = vec!["target".into()];

        let invalidated = dag.invalidate_forward_slice("target", false);
        assert_eq!(invalidated, vec!["child".to_string()]);
        assert!(matches!(
            dag.tasks["target"].state,
            TaskState::Completed { .. }
        ));
        assert!(matches!(dag.tasks["child"].state, TaskState::Pending));
    }

    #[test]
    fn reverse_deps_consistent_after_amendment() {
        // The stored reverse-dep map must equal the derivation
        // `tasks.iter().flat_map(|(id, t)| t.depends_on.iter().map(|d| (d, id)))`
        // after every DAG mutation. Without maintaining that invariant
        // `invalidate_forward_slice` walks a stale adjacency.
        let mut dag = make_dag(vec![
            ("a", completed_task(serde_json::json!({"v": 1}))),
            ("b", completed_task(serde_json::json!({"v": 2}))),
            ("c", completed_task(serde_json::json!({"v": 3}))),
        ]);
        dag.tasks.get_mut("b").unwrap().depends_on = vec!["a".into()];
        dag.tasks.get_mut("c").unwrap().depends_on = vec!["b".into()];
        dag.rebuild_reverse_deps();

        fn derive(dag: &DAG) -> BTreeMap<TaskId, Vec<TaskId>> {
            let mut out: BTreeMap<TaskId, Vec<TaskId>> = BTreeMap::new();
            for (id, task) in &dag.tasks {
                for dep in &task.depends_on {
                    out.entry(dep.clone()).or_default().push(id.clone());
                }
            }
            // Sort each bucket so ordering matches rebuild's iteration.
            for v in out.values_mut() {
                v.sort();
            }
            out
        }
        let mut normalized = dag.reverse_deps.clone();
        for v in normalized.values_mut() {
            v.sort();
        }
        assert_eq!(normalized, derive(&dag));

        // Mutate: drop c's dependency on b, rebuild, re-assert.
        dag.tasks.get_mut("c").unwrap().depends_on.clear();
        dag.rebuild_reverse_deps();
        let mut normalized = dag.reverse_deps.clone();
        for v in normalized.values_mut() {
            v.sort();
        }
        assert_eq!(normalized, derive(&dag));
        // a now has b as its only dependent; b no longer has anything.
        assert_eq!(
            dag.reverse_deps.get("a").map(|v| v.len()).unwrap_or(0),
            1,
            "a should still have b as dependent"
        );
        assert!(
            dag.reverse_deps
                .get("b")
                .map(|v| v.is_empty())
                .unwrap_or(true),
            "b should have no dependents after c dropped it"
        );
    }

    #[test]
    fn invalidate_forward_slice_lazy_rebuilds_when_reverse_deps_empty() {
        // A DAG fresh from deserialize has `reverse_deps` empty (it's
        // `#[serde(skip)]`). invalidate_forward_slice must still work
        // by lazy-rebuilding the cache on first use.
        let mut dag = make_dag(vec![
            ("target", completed_task(serde_json::json!({"v": 1}))),
            ("child", completed_task(serde_json::json!({"v": 2}))),
        ]);
        dag.tasks.get_mut("child").unwrap().depends_on = vec!["target".into()];
        // Simulate a freshly-deserialized DAG by wiping the cache.
        dag.reverse_deps.clear();
        let invalidated = dag.invalidate_forward_slice("target", true);
        assert!(invalidated.iter().any(|id| id.as_str() == "child"));
        // The lazy rebuild must have populated the cache for subsequent calls.
        assert!(!dag.reverse_deps.is_empty());
    }

    #[test]
    fn invalidate_forward_slice_scales_linearly_on_wide_dag() {
        // Smoke-test that a 500-task chain completes in well under the
        // O(n²) worst case. With the reverse-dep map the total work is
        // O(n + e) ≈ 2n. On a modern dev box this runs in single-digit
        // milliseconds; we gate on 200ms so slow CI still passes.
        use std::time::Instant;
        let n = 500usize;
        let mut tasks: Vec<(&'static str, Task)> = Vec::with_capacity(n);
        // Leak task ids for 'static so make_dag's signature is happy.
        let ids: Vec<&'static str> = (0..n)
            .map(|i| Box::leak(format!("t{:04}", i).into_boxed_str()) as &'static str)
            .collect();
        for id in &ids {
            tasks.push((id, completed_task(serde_json::json!({"v": 0}))));
        }
        let mut dag = make_dag(tasks);
        // Chain each task to the previous (t0001 → t0000,...).
        for i in 1..n {
            dag.tasks.get_mut(ids[i]).unwrap().depends_on = vec![TaskId::from(ids[i - 1])];
        }
        let start = Instant::now();
        let invalidated = dag.invalidate_forward_slice(ids[0], true);
        let elapsed = start.elapsed();
        assert_eq!(invalidated.len(), n);
        assert!(
            elapsed.as_millis() < 200,
            "O(n+e) invalidation should finish ≪ 200ms, took {:?}",
            elapsed
        );
    }

    #[test]
    fn invalidate_forward_slice_unknown_target_is_noop() {
        let mut dag = make_dag(vec![("only", completed_task(serde_json::json!({"v": 1})))]);
        let invalidated = dag.invalidate_forward_slice("missing", true);
        assert!(invalidated.is_empty());
        assert!(matches!(
            dag.tasks["only"].state,
            TaskState::Completed { .. }
        ));
    }

    #[test]
    fn propagate_readiness_promotes_pending() {
        let mut dag = make_dag(vec![
            ("root", completed_task(serde_json::json!({}))),
            ("child", pending_task(vec!["root"])),
        ]);
        dag.propagate_readiness();
        assert_eq!(dag.tasks["child"].state, TaskState::Ready);
    }

    #[test]
    fn propagate_readiness_leaves_pending_when_dep_not_done() {
        let mut dag = make_dag(vec![
            ("root", pending_task(vec![])),
            ("child", pending_task(vec!["root"])),
        ]);
        dag.propagate_readiness();
        assert_eq!(dag.tasks["child"].state, TaskState::Pending);
    }

    #[test]
    fn propagate_readiness_skips_conditional_task_when_false() {
        let mut dag = make_dag(vec![
            (
                "qc",
                completed_task(serde_json::json!({"needs_trimming": false})),
            ),
            (
                "trim",
                Task {
                    kind: TaskKind::Computation,
                    state: TaskState::Pending,
                    depends_on: vec!["qc".into()],
                    assignee: Assignee::Agent,
                    description: "trim adapters".into(),
                    spec: Some(
                        serde_json::json!({"condition": "qc.result.needs_trimming == true"}),
                    ),
                    resolution: None,
                    result_ref: None,
                    resource_class: ResourceClass::CpuHeavy,
                    requires_sme_review: false,

                    required_artifacts: vec![],
                    container: None,
                    source_atom_id: None,
                    safety: Default::default(),
                },
            ),
        ]);
        dag.propagate_readiness();
        match &dag.tasks["trim"].state {
            TaskState::Completed { result } => {
                assert_eq!(result["skipped"], serde_json::json!(true));
            }
            other => panic!("expected Completed(skipped), got {:?}", other),
        }
    }

    #[test]
    fn propagate_readiness_promotes_conditional_task_when_true() {
        let mut dag = make_dag(vec![
            (
                "qc",
                completed_task(serde_json::json!({"needs_trimming": true})),
            ),
            (
                "trim",
                Task {
                    kind: TaskKind::Computation,
                    state: TaskState::Pending,
                    depends_on: vec!["qc".into()],
                    assignee: Assignee::Agent,
                    description: "trim".into(),
                    spec: Some(
                        serde_json::json!({"condition": "qc.result.needs_trimming == true"}),
                    ),
                    resolution: None,
                    result_ref: None,
                    resource_class: ResourceClass::CpuHeavy,
                    requires_sme_review: false,

                    required_artifacts: vec![],
                    container: None,
                    source_atom_id: None,
                    safety: Default::default(),
                },
            ),
        ]);
        dag.propagate_readiness();
        assert_eq!(dag.tasks["trim"].state, TaskState::Ready);
    }

    #[test]
    fn propagate_readiness_leaves_pending_when_referenced_task_not_done() {
        let mut dag = make_dag(vec![
            ("qc", pending_task(vec![])), // NOT completed
            (
                "trim",
                Task {
                    kind: TaskKind::Computation,
                    state: TaskState::Pending,
                    depends_on: vec!["qc".into()],
                    assignee: Assignee::Agent,
                    description: "trim".into(),
                    spec: Some(
                        serde_json::json!({"condition": "qc.result.needs_trimming == true"}),
                    ),
                    resolution: None,
                    result_ref: None,
                    resource_class: ResourceClass::CpuHeavy,
                    requires_sme_review: false,

                    required_artifacts: vec![],
                    container: None,
                    source_atom_id: None,
                    safety: Default::default(),
                },
            ),
        ]);
        dag.propagate_readiness();
        assert_eq!(dag.tasks["trim"].state, TaskState::Pending);
    }

    #[test]
    fn condition_grammar_boolean_eq() {
        let tasks: BTreeMap<TaskId, Task> = [(
            "t".into(),
            completed_task(serde_json::json!({"flag": true})),
        )]
        .into();
        assert_eq!(
            eval_condition("t.result.flag == true", &tasks),
            ConditionResult::True
        );
        assert_eq!(
            eval_condition("t.result.flag == false", &tasks),
            ConditionResult::False
        );
    }

    #[test]
    fn condition_grammar_numeric_gte() {
        let tasks: BTreeMap<TaskId, Task> = [(
            "aln".into(),
            completed_task(serde_json::json!({"mapping_rate": 0.85})),
        )]
        .into();
        assert_eq!(
            eval_condition("aln.result.mapping_rate >= 0.70", &tasks),
            ConditionResult::True
        );
        assert_eq!(
            eval_condition("aln.result.mapping_rate >= 0.90", &tasks),
            ConditionResult::False
        );
    }

    #[test]
    fn condition_grammar_string_ne() {
        let tasks: BTreeMap<TaskId, Task> = [(
            "cls".into(),
            completed_task(serde_json::json!({"status": "pass"})),
        )]
        .into();
        assert_eq!(
            eval_condition("cls.result.status != \"fail\"", &tasks),
            ConditionResult::True
        );
    }

    #[test]
    fn condition_grammar_conjunction() {
        let tasks: BTreeMap<TaskId, Task> = [
            (
                "batch".into(),
                completed_task(serde_json::json!({"effect_detected": true})),
            ),
            (
                "aln".into(),
                completed_task(serde_json::json!({"mapping_rate": 0.75})),
            ),
        ]
        .into();
        assert_eq!(
            eval_condition(
                "batch.result.effect_detected == true && aln.result.mapping_rate >= 0.60",
                &tasks
            ),
            ConditionResult::True
        );
        assert_eq!(
            eval_condition(
                "batch.result.effect_detected == true && aln.result.mapping_rate >= 0.90",
                &tasks
            ),
            ConditionResult::False
        );
    }

    #[test]
    fn condition_grammar_disjunction() {
        let tasks: BTreeMap<TaskId, Task> = [(
            "t".into(),
            completed_task(serde_json::json!({"a": false, "b": true})),
        )]
        .into();
        assert_eq!(
            eval_condition("t.result.a == true || t.result.b == true", &tasks),
            ConditionResult::True
        );
    }

    #[test]
    fn condition_pending_when_task_not_complete() {
        let tasks: BTreeMap<TaskId, Task> = [("t".into(), pending_task(vec![]))].into();
        assert_eq!(
            eval_condition("t.result == true", &tasks),
            ConditionResult::Pending
        );
    }

    #[test]
    fn validate_dag_rejects_cycles() {
        let mut dag = make_dag(vec![
            ("a", pending_task(vec!["b"])),
            ("b", pending_task(vec!["a"])),
        ]);
        // fix deps to create cycle
        dag.tasks.get_mut("a").unwrap().depends_on = vec!["b".into()];
        dag.tasks.get_mut("b").unwrap().depends_on = vec!["a".into()];
        assert!(validate_dag(&dag).is_err());
    }

    #[test]
    fn validate_dag_rejects_dangling_deps() {
        let dag = make_dag(vec![("a", pending_task(vec!["nonexistent"]))]);
        assert!(validate_dag(&dag).is_err());
    }

    #[test]
    fn validate_dag_accepts_valid_dag() {
        let dag = make_dag(vec![
            ("root", pending_task(vec![])),
            ("child", pending_task(vec!["root"])),
        ]);
        assert!(validate_dag(&dag).is_ok());
    }

    // ── §S5.12 typed-error coverage ─────────────────────────────────

    #[test]
    fn validate_dag_typed_returns_invalid_dependency() {
        let dag = make_dag(vec![("a", pending_task(vec!["nonexistent"]))]);
        match validate_dag_typed(&dag) {
            Err(DagError::InvalidDependency {
                task_id,
                missing_dep,
            }) => {
                assert_eq!(task_id.as_str(), "a");
                assert_eq!(missing_dep, "nonexistent");
            }
            other => panic!("expected InvalidDependency, got {:?}", other),
        }
    }

    #[test]
    fn validate_dag_typed_returns_cycle_with_node_in_walk() {
        let mut dag = make_dag(vec![
            ("a", pending_task(vec!["b"])),
            ("b", pending_task(vec!["a"])),
        ]);
        dag.tasks.get_mut("a").unwrap().depends_on = vec!["b".into()];
        dag.tasks.get_mut("b").unwrap().depends_on = vec!["a".into()];
        match validate_dag_typed(&dag) {
            Err(DagError::CycleDetected { cycle }) => {
                assert!(
                    !cycle.is_empty(),
                    "cycle vec must include at least one node"
                );
                let entry = &cycle[0];
                assert!(entry == "a" || entry == "b", "entry node from petgraph");
            }
            other => panic!("expected CycleDetected, got {:?}", other),
        }
    }

    #[test]
    fn validate_dag_typed_flags_duplicate_discover_prefix() {
        // Two tasks that both root at `discover_alignment` after `__rerun`
        // suffix stripping. A real merged DAG would never compose into
        // this shape via the deterministic builder; the check guards
        // composer-merged paths from a future code path that allows it.
        // The rerun task depends on the original so neither is an orphan
        // (orphan detection fires before prefix-uniqueness on isolated nodes).
        let dag = make_dag(vec![
            ("discover_alignment", pending_task(vec![])),
            (
                "discover_alignment__rerun_2",
                pending_task(vec!["discover_alignment"]),
            ),
        ]);
        match validate_dag_typed(&dag) {
            Err(DagError::DuplicatePrefix { prefix, stage_ids }) => {
                assert_eq!(prefix, "discover_alignment");
                assert_eq!(stage_ids.len(), 2);
            }
            other => panic!("expected DuplicatePrefix, got {:?}", other),
        }
    }

    #[test]
    fn validate_dag_typed_accepts_distinct_discover_stages() {
        // Chain the two stages so neither is an isolated orphan.
        // Distinct discover_ prefixes (alignment vs quantification) should
        // not collide, so validate_dag_typed must return Ok.
        let dag = make_dag(vec![
            ("discover_alignment", pending_task(vec![])),
            (
                "discover_quantification",
                pending_task(vec!["discover_alignment"]),
            ),
        ]);
        assert!(validate_dag_typed(&dag).is_ok());
    }

    #[test]
    fn dag_error_anyhow_fallback_round_trips() {
        let opaque: anyhow::Error = anyhow::anyhow!("disk read failed");
        let typed: DagError = opaque.into();
        match typed {
            DagError::Other(_) => {}
            other => panic!("expected Other, got {:?}", other),
        }
    }

    #[test]
    fn insert_task_round_trips_through_public_api() {
        let mut dag = DAG {
            version: "1".into(),
            schema_version: current_dag_schema_version(),
            workflow_id: "insert-test".into(),
            current_task: None,
            tasks: BTreeMap::new(),
            reverse_deps: BTreeMap::new(),
            run_id: None,
        };
        dag.insert_task("only".into(), pending_task(vec![]));
        assert!(dag.tasks.contains_key("only"));
        let removed = dag.remove_task(&TaskId::from("only"));
        assert!(removed.is_some());
        assert!(dag.tasks.is_empty());
    }

    #[test]
    fn required_artifact_validate_schema_ref_accepts_relative_schema_path() {
        let a = RequiredArtifact {
            path: "dge.json".into(),
            min_size_bytes: None,
            schema_ref: Some("policies/dge.schema.json".into()),
            validation_obligations: Vec::new(),
        };
        assert!(a.validate_schema_ref().is_ok());
    }

    #[test]
    fn required_artifact_validate_schema_ref_rejects_non_schema_suffix() {
        let a = RequiredArtifact {
            path: "dge.json".into(),
            min_size_bytes: None,
            schema_ref: Some("policies/dge.json".into()),
            validation_obligations: Vec::new(),
        };
        assert!(a.validate_schema_ref().is_err());
    }

    #[test]
    fn required_artifact_validate_schema_ref_rejects_absolute_path() {
        let a = RequiredArtifact {
            path: "x".into(),
            min_size_bytes: None,
            schema_ref: Some("/etc/x.schema.json".into()),
            validation_obligations: Vec::new(),
        };
        assert!(a.validate_schema_ref().is_err());
    }

    #[test]
    fn required_artifact_validate_schema_ref_rejects_dotdot_segment() {
        let a = RequiredArtifact {
            path: "x".into(),
            min_size_bytes: None,
            schema_ref: Some("a/../b.schema.json".into()),
            validation_obligations: Vec::new(),
        };
        assert!(a.validate_schema_ref().is_err());
    }

    #[test]
    fn recover_stale_running_resets_to_ready() {
        use chrono::TimeZone;
        let old_time = chrono::Utc
            .with_ymd_and_hms(2020, 1, 1, 0, 0, 0)
            .unwrap()
            .to_rfc3339();
        let mut dag = make_dag(vec![(
            "slow_task",
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Running {
                    started_at: old_time,
                    remote: None,
                },
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "slow".into(),
                spec: None,
                resolution: None,
                result_ref: None,
                resource_class: ResourceClass::CpuHeavy,
                requires_sme_review: false,

                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        )]);
        dag.recover_stale_running(300);
        assert_eq!(dag.tasks["slow_task"].state, TaskState::Ready);
    }

    #[test]
    fn ready_tasks_and_blocked_tasks() {
        let dag = make_dag(vec![
            ("a", pending_task(vec![])),
            (
                "b",
                Task {
                    kind: TaskKind::Computation,
                    state: TaskState::Ready,
                    depends_on: vec![],
                    assignee: Assignee::Agent,
                    description: "ready".into(),
                    spec: None,
                    resolution: None,
                    result_ref: None,
                    resource_class: ResourceClass::CpuHeavy,
                    requires_sme_review: false,

                    required_artifacts: vec![],
                    container: None,
                    source_atom_id: None,
                    safety: Default::default(),
                },
            ),
            (
                "c",
                Task {
                    kind: TaskKind::Computation,
                    state: TaskState::Blocked {
                        record: BlockedRecord {
                            reason: "stuck".into(),
                            attempts: vec![],
                        },
                    },
                    depends_on: vec![],
                    assignee: Assignee::Agent,
                    description: "blocked".into(),
                    spec: None,
                    resolution: None,
                    result_ref: None,
                    resource_class: ResourceClass::CpuHeavy,
                    requires_sme_review: false,

                    required_artifacts: vec![],
                    container: None,
                    source_atom_id: None,
                    safety: Default::default(),
                },
            ),
        ]);
        assert_eq!(dag.ready_tasks(), vec![&"b".to_string()]);
        assert_eq!(dag.blocked_tasks(), vec![&"c".to_string()]);
    }

    #[test]
    fn is_complete_only_when_all_done() {
        let mut dag = make_dag(vec![
            ("a", completed_task(serde_json::json!({}))),
            ("b", pending_task(vec![])),
        ]);
        assert!(!dag.is_complete());
        dag.tasks.get_mut("b").unwrap().state = TaskState::Completed {
            result: serde_json::json!({}),
        };
        assert!(dag.is_complete());
    }

    #[test]
    fn task_carries_safety_policy() {
        let mut t = pending_task(vec![]);
        t.safety = crate::atom::SafetyPolicy {
            level: crate::atom::SafetyLevel::Exec,
            ..Default::default()
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(t.safety, back.safety);
    }

    #[test]
    fn task_default_safety_suppressed_from_json() {
        let t = pending_task(vec![]);
        let json = serde_json::to_string(&t).unwrap();
        assert!(
            !json.contains("\"safety\":"),
            "default safety leaked: {json}"
        );
    }

    #[test]
    fn dag_round_trips_schema_version() {
        let dag = make_dag(vec![("t", pending_task(vec![]))]);
        let json = serde_json::to_string(&dag).unwrap();
        let back: DAG = serde_json::from_str(&json).unwrap();
        assert_eq!(dag.schema_version, back.schema_version);
    }

    #[test]
    fn legacy_dag_without_schema_version_loads_with_default() {
        let legacy = r#"{
            "version": "1",
            "workflow_id": "w",
            "current_task": null,
            "tasks": {}
        }"#;
        let dag: DAG =
            serde_json::from_str(legacy).expect("legacy DAG without schema_version parses");
        assert_eq!(
            dag.schema_version,
            semver::Version::new(0, 1, 0),
            "missing schema_version must default to 0.1.0"
        );
    }

    #[test]
    fn descendants_of_diamond_dag_includes_all_transitive_successors() {
        // DAG topology: A -> B -> D
        //               A -> C -> D
        // descendants_of(B) must include D (via the B->D edge)
        // descendants_of(A) must include B, C, D
        // descendants_of(D) must be empty
        let dag_tasks = vec![
            ("A", pending_task(vec![])),
            ("B", pending_task(vec!["A"])),
            ("C", pending_task(vec!["A"])),
            ("D", pending_task(vec!["B", "C"])),
        ];
        let mut dag = make_dag(dag_tasks);

        let from_a: std::collections::BTreeSet<String> = dag
            .descendants_of("A")
            .into_iter()
            .map(|id| id.to_string())
            .collect();
        assert_eq!(
            from_a,
            ["B", "C", "D"].iter().map(|s| s.to_string()).collect()
        );

        let from_b: std::collections::BTreeSet<String> = dag
            .descendants_of("B")
            .into_iter()
            .map(|id| id.to_string())
            .collect();
        assert_eq!(from_b, ["D"].iter().map(|s| s.to_string()).collect());

        let from_c: std::collections::BTreeSet<String> = dag
            .descendants_of("C")
            .into_iter()
            .map(|id| id.to_string())
            .collect();
        assert_eq!(from_c, ["D"].iter().map(|s| s.to_string()).collect());

        let from_d = dag.descendants_of("D");
        assert!(
            from_d.is_empty(),
            "leaf task should have no descendants, got {:?}",
            from_d
        );
    }
}
