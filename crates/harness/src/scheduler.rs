//! Parallel task scheduler for the harness.
//!
//! Serial behavior (one Ready task per iteration) is preserved when
//! `SWFC_HARNESS_CONCURRENCY=1` (the default). `auto` or an explicit
//! integer opts in to bounded K-way dispatch where each picked task
//! runs in its own thread via `std::thread::scope`. Tasks with
//! `resource_class: Gpu` draw against `gpu_slots`; all others against
//! `cpu_slots`.
//!
//! No tokio here — the harness stays sync (see
//! `memory:feedback_simplicity`).

use scripps_workflow_core::dag::{ResourceClass, TaskId, TaskKind, TaskState, DAG};

/// Per-iteration semaphore budget. The earlier static
/// `{cpu_heavy: 1}` becomes dynamic here — `pick_ready_respecting_budgets`
/// walks Ready tasks in deterministic id order and draws slots until
/// either budget is exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerBudget {
    /// Maximum concurrent CPU (and memory/IO) tasks per iteration.
    pub cpu_slots: usize,
    /// Maximum concurrent GPU tasks per iteration.
    pub gpu_slots: usize,
}

impl SchedulerBudget {
    /// Serial-compatible default used when `SWFC_HARNESS_CONCURRENCY=1`.
    /// Preserved for byte-identical behavior against the pre-parallel
    /// baseline.
    pub fn serial() -> Self {
        Self {
            cpu_slots: 1,
            gpu_slots: 0,
        }
    }
}

/// `SWFC_HARNESS_CONCURRENCY` resolution:
/// - `1` (default when unset) → serial behavior, one pick per iteration.
/// - `auto` → use `executor.cpu_budget()` + `executor.gpu_budget()`.
/// - an integer N → clamp cpu_slots to N (preserves the ability to dial
///   down without disabling concurrency entirely).
/// - any other value → WARN to stderr and fall back to serial so a typo
///   can't accidentally disable dispatch entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcurrencyMode {
    /// One task per iteration; matches the pre-parallel baseline.
    Serial,
    /// Use the executor's declared CPU and GPU budgets.
    Auto,
    /// Pin CPU concurrency to exactly N tasks (GPU budget still comes from the executor).
    Fixed(usize),
}

impl ConcurrencyMode {
    /// Reads `SWFC_HARNESS_CONCURRENCY` and returns the corresponding mode.
    pub fn from_env() -> Self {
        let raw = std::env::var("SWFC_HARNESS_CONCURRENCY").unwrap_or_else(|_| "1".into());
        Self::parse(&raw)
    }

    /// Parses a `SWFC_HARNESS_CONCURRENCY` value string.
    pub fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        if trimmed == "auto" {
            return Self::Auto;
        }
        match trimmed.parse::<usize>() {
            Ok(0) => Self::Serial, // 0 makes no sense — treat as serial
            Ok(1) => Self::Serial,
            Ok(n) => Self::Fixed(n),
            Err(_) => {
                eprintln!(
                    "[scheduler] SWFC_HARNESS_CONCURRENCY={} not recognized; using serial",
                    raw
                );
                Self::Serial
            }
        }
    }

    /// Apply this mode against an executor's declared budgets.
    pub fn resolve_budget(self, exec_cpu: usize, exec_gpu: usize) -> SchedulerBudget {
        match self {
            Self::Serial => SchedulerBudget::serial(),
            Self::Auto => SchedulerBudget {
                cpu_slots: exec_cpu.max(1),
                gpu_slots: exec_gpu,
            },
            Self::Fixed(n) => SchedulerBudget {
                cpu_slots: n.max(1).min(exec_cpu.max(1)),
                gpu_slots: exec_gpu,
            },
        }
    }
}

/// Pick up to `budget.cpu_slots` + `budget.gpu_slots` Ready tasks from
/// the DAG, drawing against each semaphore by resource class.
///
/// Picks are deterministic: Ready tasks are walked in BTreeMap order
/// (id-sorted), and a task is selected when its class's semaphore
/// still has a free slot. `IoHeavy` and `MemoryHeavy` both draw
/// against `cpu_slots` — memory pressure is a sizing concern, not a
/// scheduling concern; the scheduler just avoids oversubscribing
/// threads.
pub fn pick_ready_respecting_budgets(dag: &DAG, budget: SchedulerBudget) -> Vec<TaskId> {
    let mut cpu_remaining = budget.cpu_slots;
    let mut gpu_remaining = budget.gpu_slots;
    let mut picks = Vec::new();
    for (id, task) in dag.tasks.iter() {
        if !matches!(task.state, TaskState::Ready) {
            continue;
        }
        match task.resource_class {
            ResourceClass::Gpu => {
                if gpu_remaining > 0 {
                    picks.push(id.clone());
                    gpu_remaining -= 1;
                }
            }
            _ => {
                if cpu_remaining > 0 {
                    picks.push(id.clone());
                    cpu_remaining -= 1;
                }
            }
        }
        if cpu_remaining == 0 && gpu_remaining == 0 {
            break;
        }
    }
    picks
}

/// Two-lane budget for the role-aware picker. The validation lane is
/// reserved exclusively for `TaskKind::Validation` tasks (the builder
/// assigns this kind to every `validate_*` stage); the processing
/// lane handles everything else. Unused validation capacity does not
/// promote to processing: in local execution, pre-marking a second
/// processing task as Running while it waits on the same executor lock
/// creates misleading wall-clock timeouts and nonproductive executor
/// state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneBudget {
    /// Maximum concurrent processing/analysis tasks per iteration.
    pub processing_slots: usize,
    /// Maximum concurrent validation tasks per iteration; unused slots do NOT promote to processing.
    pub validation_slots: usize,
    /// Maximum concurrent GPU tasks per iteration (draws from processing lane).
    pub gpu_slots: usize,
}

impl LaneBudget {
    /// The shape the user asked for: one primary executor advancing
    /// processing/analysis nodes, one secondary executor running
    /// validators in parallel. Activated by `SWFC_HARNESS_VALIDATION_LANE=1`.
    pub fn one_plus_one() -> Self {
        Self {
            processing_slots: 1,
            validation_slots: 1,
            gpu_slots: 0,
        }
    }
}

/// Resolve the validation-lane env opt-in. Returns `Some(LaneBudget)`
/// when `SWFC_HARNESS_VALIDATION_LANE=1`, otherwise `None`. Any other
/// value is treated as off — typos shouldn't accidentally enable it.
pub fn lane_mode_from_env() -> Option<LaneBudget> {
    match std::env::var("SWFC_HARNESS_VALIDATION_LANE")
        .ok()
        .as_deref()
    {
        Some("1") => Some(LaneBudget::one_plus_one()),
        _ => None,
    }
}

/// Pick up to one validation task + one processing task per iteration.
/// Validation lane fills first (deterministic id order over Ready
/// `TaskKind::Validation` tasks); processing lane fills next (Ready
/// non-validation). Unused lane slots do not cross-promote; this keeps
/// local execution from pre-marking extra tasks that cannot yet make
/// progress and lets each task's timeout window begin when the harness
/// can actually dispatch it.
///
/// Falls through to `pick_ready_respecting_budgets` semantics for the
/// per-resource-class semaphore: GPU tasks always draw against
/// `gpu_slots`; everything else against the lane it's queued in.
pub fn pick_ready_with_lanes(dag: &DAG, budget: LaneBudget) -> Vec<TaskId> {
    let mut p_left = budget.processing_slots;
    let mut v_left = budget.validation_slots;
    let mut g_left = budget.gpu_slots;
    let mut picks: Vec<TaskId> = Vec::new();

    let is_validation =
        |task: &scripps_workflow_core::dag::Task| matches!(task.kind, TaskKind::Validation);

    // Pass 1: validators claim the validation lane (id-sorted).
    for (id, task) in dag.tasks.iter() {
        if !matches!(task.state, TaskState::Ready) || !is_validation(task) {
            continue;
        }
        match task.resource_class {
            ResourceClass::Gpu if g_left > 0 => {
                picks.push(id.clone());
                g_left -= 1;
            }
            ResourceClass::Gpu => {}
            _ if v_left > 0 => {
                picks.push(id.clone());
                v_left -= 1;
            }
            _ => {}
        }
    }

    // Pass 2: non-validators claim the processing lane.
    for (id, task) in dag.tasks.iter() {
        if !matches!(task.state, TaskState::Ready) || is_validation(task) {
            continue;
        }
        match task.resource_class {
            ResourceClass::Gpu if g_left > 0 => {
                picks.push(id.clone());
                g_left -= 1;
            }
            ResourceClass::Gpu => {}
            _ if p_left > 0 => {
                picks.push(id.clone());
                p_left -= 1;
            }
            _ => {}
        }
    }

    picks
}

/// Drop picks whose transitive dependency chain includes a
/// Completed task with `requires_sme_review: true` that the SME has
/// not yet confirmed. `confirmed_stages` is the set of stage (task)
/// ids the SME has already confirmed via
/// `POST /api/chat/session/:id/confirm { stage: "<id>" }`.
///
/// Walk is upward (ancestor) over the DAG's `depends_on` edges. Any
/// pick whose ancestor walk hits an unconfirmed review gate is
/// dropped — the scheduler will retry on a later iteration once the
/// gate clears.
pub fn filter_picks_respecting_sme_gate(
    dag: &DAG,
    picks: Vec<TaskId>,
    confirmed_stages: &std::collections::BTreeSet<TaskId>,
) -> Vec<TaskId> {
    picks
        .into_iter()
        .filter(|id| !has_unconfirmed_review_ancestor(dag, id.as_str(), confirmed_stages))
        .collect()
}

fn has_unconfirmed_review_ancestor(
    dag: &DAG,
    task_id: &str,
    confirmed_stages: &std::collections::BTreeSet<TaskId>,
) -> bool {
    let mut stack: Vec<&str> = vec![task_id];
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.to_string()) {
            continue;
        }
        let Some(task) = dag.tasks.get(id) else {
            continue;
        };
        // Each parent dependency: if it's a completed review gate and
        // not in confirmed_stages, we can't dispatch past it.
        for parent_id in &task.depends_on {
            if let Some(parent) = dag.tasks.get(parent_id) {
                if parent.requires_sme_review
                    && matches!(parent.state, TaskState::Completed { .. })
                    && !confirmed_stages.contains(parent_id)
                {
                    return true;
                }
            }
            stack.push(parent_id.as_str());
        }
    }
    false
}

/// Scan the package's `runtime/` directory for
/// `sme-review-confirmed-<stage>.json` sidecars written by the
/// server's `/confirm` handler. Returns the set of stage ids the
/// harness should treat as unblocked. Called once per iteration in
/// main.rs so a fresh confirm is picked up on the next dispatch tick.
///
/// Best-effort: a missing directory returns an empty set; malformed
/// sidecars are silently skipped.
pub fn read_confirmed_review_stages(
    package: &std::path::Path,
) -> std::collections::BTreeSet<TaskId> {
    let mut out = std::collections::BTreeSet::new();
    let runtime = package.join("runtime");
    let entries = match std::fs::read_dir(&runtime) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let Some(stage) = name
            .strip_prefix("sme-review-confirmed-")
            .and_then(|s| s.strip_suffix(".json"))
        else {
            continue;
        };
        out.insert(TaskId::from(stage));
    }
    // Test and operator harnesses can pre-authorize routine discovery
    // choices by writing runtime/.sme-auto-approve-discoveries. The
    // marker stores stage ids such as "normalisation"; the review gate
    // lives on the corresponding discover_* task. Treat the marker as
    // an explicit confirmation source so downstream tasks do not strand
    // when no interactive /confirm sidecar is expected.
    let auto_marker = runtime.join(".sme-auto-approve-discoveries");
    if let Ok(raw) = std::fs::read(&auto_marker) {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&raw) {
            // Defensive double-entry: any stage that appears in `deny`
            // ALWAYS blocks, even when it also appears in `allow`. The
            // agent prompt at `scripts/agent-prompts/task-execution.md`
            // promises this contract; honor it scheduler-side too so a
            // misconfigured allow list can't silently clear a
            // high-stakes gate.
            let denied: std::collections::BTreeSet<String> = value
                .get("deny")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .filter(|s| is_safe_stage_token(s))
                        .map(|s| s.to_string())
                        .collect()
                })
                .unwrap_or_default();
            if let Some(allow) = value.get("allow").and_then(|v| v.as_array()) {
                for stage in allow.iter().filter_map(|v| v.as_str()) {
                    if !is_safe_stage_token(stage) {
                        continue;
                    }
                    if denied.contains(stage) {
                        continue;
                    }
                    out.insert(TaskId::from(stage));
                    out.insert(TaskId::from(format!("discover_{stage}").as_str()));
                }
            }
        }
    }
    // Auto-approve path (scripts/agent-prompts/task-execution.md):
    // when an agent processes a discover_* task under the
    // `runtime/.sme-auto-approve-discoveries` marker, it completes
    // the task with `decision.auto_picked = true` and proceeds. The
    // server-side `/sme-selection` handler is the only path that
    // writes the `sme-review-confirmed-<task_id>.json` sidecar, so
    // an auto-approved discover task left no review-gate signature
    // for `filter_picks_respecting_sme_gate` to consume —
    // every downstream compute task stayed pinned at Ready and the
    // harness idle-looped until max-iterations. Promote any
    // `runtime/outputs/<task_id>/decision.json` carrying
    // `auto_picked = true` to a confirmed stage so the gate clears
    // without an explicit SME click.
    let outputs = runtime.join("outputs");
    if let Ok(out_entries) = std::fs::read_dir(&outputs) {
        for entry in out_entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let decision_path = entry.path().join("decision.json");
            if !decision_path.is_file() {
                continue;
            }
            let Ok(decision_bytes) = std::fs::read(&decision_path) else {
                continue;
            };
            let Ok(decision_json) = serde_json::from_slice::<serde_json::Value>(&decision_bytes)
            else {
                continue;
            };
            if decision_json
                .get("auto_picked")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                out.insert(TaskId::from(name.as_str()));
            }
        }
    }
    out
}

fn is_safe_stage_token(stage: &str) -> bool {
    !stage.is_empty()
        && stage
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Compute the set of task IDs that transitively depend on any task in
/// `blocked_ids`. Uses the DAG's `reverse_deps` adjacency (parent →
/// children that depend on it) to walk forward from each blocked task.
/// The blocked tasks themselves are NOT included — only their
/// descendants.
///
/// When `reverse_deps` is empty but `tasks` is non-empty the cache has
/// not been built yet; the function falls back to iterating
/// `task.depends_on` edges directly so callers don't need to mutate
/// the DAG to force a rebuild.
///
/// Safety cap: iteration terminates after visiting at most
/// `dag.tasks.len()` distinct nodes, preventing an infinite loop on a
/// malformed cyclic graph.
pub fn pause_dependent_tasks(
    dag: &DAG,
    blocked_ids: &std::collections::HashSet<TaskId>,
) -> std::collections::HashSet<TaskId> {
    let mut dependents: std::collections::HashSet<TaskId> = std::collections::HashSet::new();
    let mut queue: std::collections::VecDeque<TaskId> = blocked_ids.iter().cloned().collect();
    let max_iter = dag.tasks.len().saturating_add(1);
    let mut iters = 0usize;

    // Prefer the pre-built reverse_deps cache; fall back to a linear
    // scan of depends_on edges when the cache is empty.
    let use_cache = !dag.reverse_deps.is_empty();

    while let Some(current) = queue.pop_front() {
        if iters >= max_iter {
            break;
        }
        iters += 1;

        if use_cache {
            for child in dag.reverse_deps.get(&current).cloned().unwrap_or_default() {
                if dependents.insert(child.clone()) {
                    queue.push_back(child);
                }
            }
        } else {
            // Fallback: scan all tasks for those that list `current`
            // in their depends_on.
            for (id, task) in dag.tasks.iter() {
                if task.depends_on.contains(&current) && dependents.insert(id.clone()) {
                    queue.push_back(id.clone());
                }
            }
        }
    }
    dependents
}

/// Count how many currently-Running tasks map to each `ResourceClass`
/// string key. Used to populate `SWFC_HW_CONCURRENT_PEERS_BY_CLASS`
/// in the Phase-2 envelope when the Phase-3 scheduler has dispatched
/// multiple tasks concurrently.
pub fn count_concurrent_peers_by_class(dag: &DAG) -> std::collections::BTreeMap<String, u32> {
    let mut counts: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    counts.insert("cpu_heavy".into(), 0);
    counts.insert("io_heavy".into(), 0);
    counts.insert("memory_heavy".into(), 0);
    counts.insert("gpu".into(), 0);
    for task in dag.tasks.values() {
        if !matches!(task.state, TaskState::Running { .. }) {
            continue;
        }
        let key = match task.resource_class {
            ResourceClass::CpuHeavy => "cpu_heavy",
            ResourceClass::IoHeavy => "io_heavy",
            ResourceClass::MemoryHeavy => "memory_heavy",
            ResourceClass::Gpu => "gpu",
        };
        *counts.get_mut(key).unwrap() += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use scripps_workflow_core::dag::{Assignee, Task, TaskKind};
    use std::collections::BTreeMap as BT;

    fn task(id: &str, state: TaskState, rc: ResourceClass) -> (TaskId, Task) {
        (
            TaskId::from(id),
            Task {
                kind: TaskKind::Computation,
                state,
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: id.into(),
                spec: None,
                resolution: None,
                result_ref: None,
                resource_class: rc,
                requires_sme_review: false,

                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        )
    }

    fn dag_from(tasks: Vec<(TaskId, Task)>) -> DAG {
        let mut t = BT::new();
        for (id, v) in tasks {
            t.insert(id, v);
        }
        DAG {
            version: "1".into(),
            schema_version: scripps_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "w".into(),
            current_task: None,
            tasks: t,
            reverse_deps: BT::new(),
            run_id: None,
        }
    }

    #[test]
    fn parse_concurrency_mode() {
        assert_eq!(ConcurrencyMode::parse("1"), ConcurrencyMode::Serial);
        assert_eq!(ConcurrencyMode::parse("0"), ConcurrencyMode::Serial);
        assert_eq!(ConcurrencyMode::parse("auto"), ConcurrencyMode::Auto);
        assert_eq!(ConcurrencyMode::parse("4"), ConcurrencyMode::Fixed(4));
        // Unknown → serial (with stderr warn).
        assert_eq!(ConcurrencyMode::parse("banana"), ConcurrencyMode::Serial);
    }

    #[test]
    fn resolve_budget_serial_ignores_executor_budgets() {
        let b = ConcurrencyMode::Serial.resolve_budget(32, 4);
        assert_eq!(b, SchedulerBudget::serial());
    }

    #[test]
    fn resolve_budget_auto_uses_executor_budgets() {
        let b = ConcurrencyMode::Auto.resolve_budget(8, 2);
        assert_eq!(b.cpu_slots, 8);
        assert_eq!(b.gpu_slots, 2);
    }

    #[test]
    fn resolve_budget_fixed_clamps_to_executor_max() {
        // Fixed(32) on an 8-cpu executor → clamp to 8.
        let b = ConcurrencyMode::Fixed(32).resolve_budget(8, 1);
        assert_eq!(b.cpu_slots, 8);
        assert_eq!(b.gpu_slots, 1);
    }

    #[test]
    fn pick_respects_cpu_slots() {
        let dag = dag_from(vec![
            task("a", TaskState::Ready, ResourceClass::CpuHeavy),
            task("b", TaskState::Ready, ResourceClass::CpuHeavy),
            task("c", TaskState::Ready, ResourceClass::CpuHeavy),
            task("d", TaskState::Ready, ResourceClass::CpuHeavy),
        ]);
        let picks = pick_ready_respecting_budgets(
            &dag,
            SchedulerBudget {
                cpu_slots: 2,
                gpu_slots: 0,
            },
        );
        assert_eq!(picks, vec![TaskId::from("a"), TaskId::from("b")]); // deterministic id order
    }

    #[test]
    fn pick_respects_gpu_slots_separately() {
        let dag = dag_from(vec![
            task("a", TaskState::Ready, ResourceClass::CpuHeavy),
            task("b", TaskState::Ready, ResourceClass::Gpu),
            task("c", TaskState::Ready, ResourceClass::Gpu),
        ]);
        let picks = pick_ready_respecting_budgets(
            &dag,
            SchedulerBudget {
                cpu_slots: 1,
                gpu_slots: 1,
            },
        );
        // One CPU + one GPU — deterministic id order.
        assert_eq!(picks, vec![TaskId::from("a"), TaskId::from("b")]);
    }

    #[test]
    fn pick_skips_non_ready_tasks() {
        let dag = dag_from(vec![
            task("a", TaskState::Pending, ResourceClass::CpuHeavy),
            task("b", TaskState::Ready, ResourceClass::CpuHeavy),
            task(
                "c",
                TaskState::Completed {
                    result: serde_json::json!({}),
                },
                ResourceClass::CpuHeavy,
            ),
        ]);
        let picks = pick_ready_respecting_budgets(
            &dag,
            SchedulerBudget {
                cpu_slots: 4,
                gpu_slots: 0,
            },
        );
        assert_eq!(picks, vec![TaskId::from("b")]);
    }

    #[test]
    fn pick_memory_and_io_heavy_draw_against_cpu_budget() {
        let dag = dag_from(vec![
            task("a", TaskState::Ready, ResourceClass::IoHeavy),
            task("b", TaskState::Ready, ResourceClass::MemoryHeavy),
            task("c", TaskState::Ready, ResourceClass::CpuHeavy),
        ]);
        let picks = pick_ready_respecting_budgets(
            &dag,
            SchedulerBudget {
                cpu_slots: 2,
                gpu_slots: 0,
            },
        );
        assert_eq!(picks.len(), 2);
    }

    #[test]
    fn filter_picks_drops_tasks_with_unconfirmed_review_ancestor() {
        let mut parent = task(
            "gate",
            TaskState::Completed {
                result: serde_json::json!({}),
            },
            ResourceClass::CpuHeavy,
        );
        parent.1.requires_sme_review = true;

        let mut downstream = task("downstream", TaskState::Ready, ResourceClass::CpuHeavy);
        downstream.1.depends_on = vec![TaskId::from("gate")];

        let mut sibling = task("sibling", TaskState::Ready, ResourceClass::CpuHeavy);
        sibling.1.depends_on = vec![TaskId::from("unrelated")];

        let dag = dag_from(vec![parent, downstream, sibling]);
        let empty: std::collections::BTreeSet<TaskId> = std::collections::BTreeSet::new();
        let picks = vec![TaskId::from("downstream"), TaskId::from("sibling")];
        let filtered = filter_picks_respecting_sme_gate(&dag, picks, &empty);
        // downstream blocked (review gate not confirmed); sibling flows.
        assert_eq!(filtered, vec![TaskId::from("sibling")]);
    }

    #[test]
    fn filter_picks_admits_downstream_once_gate_confirmed() {
        let mut parent = task(
            "gate",
            TaskState::Completed {
                result: serde_json::json!({}),
            },
            ResourceClass::CpuHeavy,
        );
        parent.1.requires_sme_review = true;

        let mut downstream = task("downstream", TaskState::Ready, ResourceClass::CpuHeavy);
        downstream.1.depends_on = vec![TaskId::from("gate")];

        let dag = dag_from(vec![parent, downstream]);
        let mut confirmed: std::collections::BTreeSet<TaskId> = std::collections::BTreeSet::new();
        confirmed.insert(TaskId::from("gate"));
        let picks = vec![TaskId::from("downstream")];
        let filtered = filter_picks_respecting_sme_gate(&dag, picks, &confirmed);
        assert_eq!(filtered, vec![TaskId::from("downstream")]);
    }

    #[test]
    fn read_confirmed_review_stages_picks_up_sidecars() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(
            runtime.join("sme-review-confirmed-align.json"),
            r#"{"stage":"align"}"#,
        )
        .unwrap();
        std::fs::write(
            runtime.join("sme-review-confirmed-validate_align.json"),
            r#"{"stage":"validate_align"}"#,
        )
        .unwrap();
        // Unrelated file shouldn't match.
        std::fs::write(runtime.join("LOG.jsonl"), "").unwrap();

        let stages = read_confirmed_review_stages(tmp.path());
        assert!(stages.contains("align"));
        assert!(stages.contains("validate_align"));
        assert_eq!(stages.len(), 2);
    }

    #[test]
    fn read_confirmed_review_stages_picks_up_auto_approved_decisions() {
        let tmp = tempfile::tempdir().unwrap();
        let decision_dir = tmp
            .path()
            .join("runtime")
            .join("outputs")
            .join("discover_normalisation");
        std::fs::create_dir_all(&decision_dir).unwrap();
        std::fs::write(
            decision_dir.join("decision.json"),
            r#"{"task_id":"discover_normalisation","auto_picked":true}"#,
        )
        .unwrap();

        let stages = read_confirmed_review_stages(tmp.path());
        assert!(stages.contains("discover_normalisation"));
        assert_eq!(stages.len(), 1);
    }

    #[test]
    fn read_confirmed_review_stages_expands_auto_approve_marker_to_discover_tasks() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(
            runtime.join(".sme-auto-approve-discoveries"),
            r#"{"allow":["normalisation","differential_expression","../escape"],"deny":[]}"#,
        )
        .unwrap();

        let stages = read_confirmed_review_stages(tmp.path());
        assert!(stages.contains("normalisation"));
        assert!(stages.contains("discover_normalisation"));
        assert!(stages.contains("differential_expression"));
        assert!(stages.contains("discover_differential_expression"));
        assert!(!stages.contains("../escape"));
    }

    #[test]
    fn read_confirmed_review_stages_handles_missing_runtime_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // No runtime/ dir.
        let stages = read_confirmed_review_stages(tmp.path());
        assert!(stages.is_empty());
    }

    /// Defensive double-entry: any axis that appears in both `allow`
    /// and `deny` must be blocked. Matches the agent prompt's contract
    /// at `scripts/agent-prompts/task-execution.md` so the deny side is
    /// enforced server-side as well as agent-side.
    #[test]
    fn read_confirmed_review_stages_honors_deny_double_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        // `differential_expression` appears in BOTH lists; deny wins.
        std::fs::write(
            runtime.join(".sme-auto-approve-discoveries"),
            r#"{"allow":["normalisation","differential_expression"],
                "deny":["differential_expression"]}"#,
        )
        .unwrap();

        let stages = read_confirmed_review_stages(tmp.path());
        // normalisation is allowed AND not denied — confirmed.
        assert!(stages.contains("normalisation"));
        assert!(stages.contains("discover_normalisation"));
        // differential_expression is allowed AND denied — blocked.
        assert!(
            !stages.contains("differential_expression"),
            "deny must veto allow; got {:?}",
            stages
        );
        assert!(
            !stages.contains("discover_differential_expression"),
            "deny must also veto the auto-prefixed discover_<axis>; got {:?}",
            stages
        );
    }

    fn validator_task(id: &str, state: TaskState) -> (TaskId, Task) {
        (
            TaskId::from(id),
            Task {
                kind: TaskKind::Validation,
                state,
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: id.into(),
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
        )
    }

    #[test]
    fn lane_picker_reserves_validation_slot_when_validator_ready() {
        let dag = dag_from(vec![
            task("compute_a", TaskState::Ready, ResourceClass::CpuHeavy),
            task("compute_b", TaskState::Ready, ResourceClass::CpuHeavy),
            task("compute_c", TaskState::Ready, ResourceClass::CpuHeavy),
            validator_task("validate_compute_a", TaskState::Ready),
        ]);
        let picks = pick_ready_with_lanes(&dag, LaneBudget::one_plus_one());
        // Exactly one validator + one processing task; processing
        // lane is bounded at 1 even though 3 are ready.
        assert_eq!(picks.len(), 2);
        assert!(picks.iter().any(|id| id.as_str() == "validate_compute_a"));
        assert!(picks.iter().any(|id| id.as_str() == "compute_a"));
        assert!(!picks.iter().any(|id| id.as_str() == "compute_b"));
    }

    #[test]
    fn lane_picker_does_not_promote_unused_validation_slot_to_processing() {
        // No ready validator → validation slot stays empty. With 4
        // ready processing tasks and lane(1,1), only the processing
        // lane should dispatch so local mode does not pre-mark a
        // second processing task as Running while it waits on the same
        // executor mutex.
        let dag = dag_from(vec![
            task("compute_a", TaskState::Ready, ResourceClass::CpuHeavy),
            task("compute_b", TaskState::Ready, ResourceClass::CpuHeavy),
            task("compute_c", TaskState::Ready, ResourceClass::CpuHeavy),
            task("compute_d", TaskState::Ready, ResourceClass::CpuHeavy),
        ]);
        let picks = pick_ready_with_lanes(&dag, LaneBudget::one_plus_one());
        assert_eq!(picks, vec![TaskId::from("compute_a")]);
    }

    #[test]
    fn lane_picker_caps_validation_lane_at_lane_size() {
        // 5 ready validators, 0 ready processing, lane(1,1):
        // 1 validator + processing slot empty (does NOT promote into
        // validation lane — asymmetric on purpose).
        let dag = dag_from(vec![
            validator_task("validate_a", TaskState::Ready),
            validator_task("validate_b", TaskState::Ready),
            validator_task("validate_c", TaskState::Ready),
            validator_task("validate_d", TaskState::Ready),
            validator_task("validate_e", TaskState::Ready),
        ]);
        let picks = pick_ready_with_lanes(&dag, LaneBudget::one_plus_one());
        assert_eq!(picks, vec![TaskId::from("validate_a")]);
    }

    #[test]
    fn lane_picker_skips_non_ready_tasks() {
        let dag = dag_from(vec![
            task("compute_a", TaskState::Pending, ResourceClass::CpuHeavy),
            validator_task(
                "validate_a",
                TaskState::Completed {
                    result: serde_json::json!({}),
                },
            ),
            task("compute_b", TaskState::Ready, ResourceClass::CpuHeavy),
            validator_task("validate_b", TaskState::Ready),
        ]);
        let picks = pick_ready_with_lanes(&dag, LaneBudget::one_plus_one());
        assert_eq!(picks.len(), 2);
        assert!(picks.iter().any(|id| id.as_str() == "validate_b"));
        assert!(picks.iter().any(|id| id.as_str() == "compute_b"));
    }

    #[test]
    fn lane_picker_validator_with_gpu_class_uses_gpu_slot() {
        // Validator on GPU (rare but possible). Should draw against
        // gpu_slots, not validation_slots.
        let mut v = validator_task("validate_gpu", TaskState::Ready);
        v.1.resource_class = ResourceClass::Gpu;
        let dag = dag_from(vec![
            v,
            task("compute_a", TaskState::Ready, ResourceClass::CpuHeavy),
        ]);
        let budget = LaneBudget {
            processing_slots: 1,
            validation_slots: 1,
            gpu_slots: 1,
        };
        let picks = pick_ready_with_lanes(&dag, budget);
        assert_eq!(picks.len(), 2);
        assert!(picks.iter().any(|id| id.as_str() == "validate_gpu"));
        assert!(picks.iter().any(|id| id.as_str() == "compute_a"));
    }

    #[test]
    fn lane_mode_from_env_off_by_default() {
        // Tests don't have a clean way to clear env atomically — we
        // just assert the off-by-default by inspecting the parser
        // for the value we'd actually set.
        std::env::remove_var("SWFC_HARNESS_VALIDATION_LANE");
        assert_eq!(lane_mode_from_env(), None);
        std::env::set_var("SWFC_HARNESS_VALIDATION_LANE", "0");
        assert_eq!(lane_mode_from_env(), None);
        std::env::set_var("SWFC_HARNESS_VALIDATION_LANE", "1");
        assert_eq!(lane_mode_from_env(), Some(LaneBudget::one_plus_one()));
        std::env::remove_var("SWFC_HARNESS_VALIDATION_LANE");
    }

    #[test]
    fn count_peers_ignores_non_running() {
        let dag = dag_from(vec![
            task(
                "a",
                TaskState::Running {
                    started_at: "2026-01-01T00:00:00Z".into(),
                    remote: None,
                },
                ResourceClass::CpuHeavy,
            ),
            task(
                "b",
                TaskState::Running {
                    started_at: "2026-01-01T00:00:00Z".into(),
                    remote: None,
                },
                ResourceClass::Gpu,
            ),
            task("c", TaskState::Ready, ResourceClass::CpuHeavy),
        ]);
        let counts = count_concurrent_peers_by_class(&dag);
        assert_eq!(counts["cpu_heavy"], 1);
        assert_eq!(counts["gpu"], 1);
        assert_eq!(counts["io_heavy"], 0);
        assert_eq!(counts["memory_heavy"], 0);
    }
}
