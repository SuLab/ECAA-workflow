//! Pre-flight sizing pilot. Runs a handful of representative tasks on a
//! small instance, measures actual resource use, and projects the
//! full-run compute requirements at a configurable multiplier.
//!
//! The pilot is harness-owned (sync, no tokio) and plugs into the
//! `Executor` trait as an optional `pilot(dag, cfg)` method with a
//! default no-op impl. The main loop calls it between `read_dag` and
//! `provision(&dag)` so projected requirements can feed back into the
//! real provision shape.
//!
//! Artefacts land under `runtime/pilot/` in the package — excluded from
//! byte-diff baselines per plan §2.4.
//!
//! See §2 for the full
//! design.

use super::sizing::{ComputeProfiles, SizingIntakeFacts};
use super::ResourceRequirements;
use anyhow::{Context, Result};
use scripps_workflow_core::dag::{TaskState, DAG};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;

/// Runtime knobs for the pilot. Populated from env vars (primary) with
/// hard-coded defaults per plan §2.2.
#[derive(Debug, Clone, PartialEq)]
pub struct PilotConfig {
    /// When `false`, the pilot step is skipped entirely.
    pub enabled: bool,
    /// Number of representative tasks to execute during the pilot run.
    pub task_count: usize,
    /// Multiplier applied to pilot measurements when projecting full-run requirements.
    pub projection_multiplier: f64,
    /// EC2 instance type used for the pilot run (e.g. "t3.medium").
    pub pilot_instance_type: String,
    /// How often (seconds) the pilot samples instance metrics during task execution.
    pub measurement_interval_secs: u64,
}

const DEFAULT_PILOT_TASK_COUNT: usize = 3;
const DEFAULT_PROJECTION_MULTIPLIER: f64 = 1.5;
const DEFAULT_PILOT_INSTANCE_TYPE: &str = "t3.medium";
const DEFAULT_MEASUREMENT_INTERVAL_SECS: u64 = 5;
const MIN_READY_TASKS_FOR_QUARTILE: usize = 4;

impl Default for PilotConfig {
    /// W6.2: this impl is now pure-constant. Env reads live exclusively
    /// in `from_env` so the dual long-form / short-form names are
    /// applied in one place. Callers that want env-aware defaults
    /// should call `PilotConfig::from_env()`.
    fn default() -> Self {
        Self {
            enabled: false,
            task_count: DEFAULT_PILOT_TASK_COUNT,
            projection_multiplier: DEFAULT_PROJECTION_MULTIPLIER,
            pilot_instance_type: DEFAULT_PILOT_INSTANCE_TYPE.to_string(),
            measurement_interval_secs: DEFAULT_MEASUREMENT_INTERVAL_SECS,
        }
    }
}

/// W6.2: short-form names are canonical; long-form names are deprecated
/// aliases that emit a `tracing::warn!` when set. The short form wins
/// on collision (operators who set both intentionally see the short
/// form's value applied).
const PILOT_TASK_COUNT_ENV: &str = "SWFC_PILOT_TASKS";
const PILOT_TASK_COUNT_ENV_DEPRECATED: &str = "SWFC_PILOT_TASK_COUNT";
const PILOT_MULTIPLIER_ENV: &str = "SWFC_PILOT_MULTIPLIER";
const PILOT_MULTIPLIER_ENV_DEPRECATED: &str = "SWFC_PILOT_PROJECTION_MULT";
const PILOT_INSTANCE_ENV: &str = "SWFC_PILOT_INSTANCE";
const PILOT_INSTANCE_ENV_DEPRECATED: &str = "SWFC_PILOT_INSTANCE_TYPE";
const PILOT_INTERVAL_ENV: &str = "SWFC_PILOT_INTERVAL_SECS";
const PILOT_INTERVAL_ENV_DEPRECATED: &str = "SWFC_PILOT_MEASUREMENT_INTERVAL_SECS";

/// W6.2 — warn when a deprecated long-form env var is set. Pure no-op
/// when the var is unset.
fn warn_if_deprecated_pilot_env(deprecated: &'static str, canonical: &'static str) {
    if env::var(deprecated).is_ok() {
        tracing::warn!(
            target: "pilot_config",
            deprecated = deprecated,
            canonical = canonical,
            "deprecated pilot env var set; rename to canonical short form"
        );
    }
}

impl PilotConfig {
    /// Read env vars. Called once at harness startup.
    ///
    /// `SWFC_PILOT_ENABLED` defaults to `1` when
    /// `SWFC_EXECUTOR_MODE=aws`, `0` otherwise. Explicit override wins.
    ///
    /// W6.2: short-form env vars (`SWFC_PILOT_TASKS`,
    /// `SWFC_PILOT_MULTIPLIER`, `SWFC_PILOT_INSTANCE`,
    /// `SWFC_PILOT_INTERVAL_SECS`) are canonical. Long-form aliases
    /// remain readable for back-compat but emit a `tracing::warn!`
    /// when set. Short form wins on collision.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        let aws_mode = env::var("SWFC_EXECUTOR_MODE").as_deref() == Ok("aws");
        cfg.enabled = match env::var("SWFC_PILOT_ENABLED") {
            Ok(v) => parse_bool(&v).unwrap_or(aws_mode),
            Err(_) => aws_mode,
        };

        // Long-form aliases (deprecated): apply first, so the short
        // form below cleanly overrides.
        warn_if_deprecated_pilot_env(PILOT_TASK_COUNT_ENV_DEPRECATED, PILOT_TASK_COUNT_ENV);
        if let Ok(v) = env::var(PILOT_TASK_COUNT_ENV_DEPRECATED) {
            if let Ok(n) = v.trim().parse::<usize>() {
                if n > 0 {
                    cfg.task_count = n;
                }
            }
        }
        warn_if_deprecated_pilot_env(PILOT_MULTIPLIER_ENV_DEPRECATED, PILOT_MULTIPLIER_ENV);
        if let Ok(v) = env::var(PILOT_MULTIPLIER_ENV_DEPRECATED) {
            if let Ok(m) = v.trim().parse::<f64>() {
                if m > 0.0 {
                    cfg.projection_multiplier = m;
                }
            }
        }
        warn_if_deprecated_pilot_env(PILOT_INSTANCE_ENV_DEPRECATED, PILOT_INSTANCE_ENV);
        if let Ok(v) = env::var(PILOT_INSTANCE_ENV_DEPRECATED) {
            if !v.trim().is_empty() {
                cfg.pilot_instance_type = v.trim().to_string();
            }
        }
        warn_if_deprecated_pilot_env(PILOT_INTERVAL_ENV_DEPRECATED, PILOT_INTERVAL_ENV);
        if let Ok(v) = env::var(PILOT_INTERVAL_ENV_DEPRECATED) {
            if let Ok(n) = v.trim().parse::<u64>() {
                if n > 0 {
                    cfg.measurement_interval_secs = n;
                }
            }
        }

        // Short-form (canonical) — applied last so it wins on collision.
        if let Ok(v) = env::var(PILOT_TASK_COUNT_ENV) {
            if let Ok(n) = v.trim().parse::<usize>() {
                if n > 0 {
                    cfg.task_count = n;
                }
            }
        }
        if let Ok(v) = env::var(PILOT_MULTIPLIER_ENV) {
            if let Ok(m) = v.trim().parse::<f64>() {
                if m > 0.0 {
                    cfg.projection_multiplier = m;
                }
            }
        }
        if let Ok(v) = env::var(PILOT_INSTANCE_ENV) {
            if !v.trim().is_empty() {
                cfg.pilot_instance_type = v.trim().to_string();
            }
        }
        if let Ok(v) = env::var(PILOT_INTERVAL_ENV) {
            if let Ok(n) = v.trim().parse::<u64>() {
                if n > 0 {
                    cfg.measurement_interval_secs = n;
                }
            }
        }
        cfg
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// One pilot task's measured execution. Recorded whether the task
/// succeeded or failed — failed tasks still carry a signal about the
/// resource envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PilotMeasurement {
    /// Task that was measured.
    pub task_id: String,
    /// Stage class derived from the task id (used for projection grouping).
    pub stage_class: String,
    /// Peak resident set size in MiB observed during this task.
    pub peak_rss_mb: u64,
    /// Wall-clock seconds from task start to agent exit.
    pub wall_time_secs: u64,
    /// Disk used in MiB at the end of the task.
    pub disk_used_mb: u64,
    /// Agent process exit status code.
    pub exit_status: i32,
}

/// Aggregated pilot output. Written to `runtime/pilot/report.json` in
/// the child package. Consumed by:
///
/// * the AWS executor, which uses `projected_requirements` to size the
///   real provision shape,
/// * the server Metrics tab (via `GET /api/chat/session/:id/pilot`),
/// * the `cost_guard::check_ceiling` gate, which may raise
///   `BlockerKind::PilotOversize`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PilotReport {
    /// Per-task resource measurements collected during the pilot run.
    pub measurements: Vec<PilotMeasurement>,
    /// Projected resource requirements per stage class, keyed by task id.
    pub projected_requirements: BTreeMap<String, ResourceRequirements>,
    /// Ratio of measured-to-baseline resource usage, clamped to [0, 1].
    /// High values mean the pilot measurements agreed closely with
    /// baseline expectations; low values suggest the projection is
    /// extrapolating from limited data. Surfaced in the Metrics tab.
    pub confidence: f64,
}

/// Rank Ready tasks by per-class compute weight and pick `cfg.task_count`
/// from the median band — drop the bottom and top quartiles so the pilot
/// measurement is representative, not dominated by discovery or
/// alignment outliers. Tie-break by deterministic `task_id` ordering.
///
/// Falls back to topological order filtered on non-`discover_*` when the
/// Ready set has fewer than 4 tasks (too small to quartile).
pub fn select_pilot_tasks(
    dag: &DAG,
    profiles: &ComputeProfiles,
    facts: &SizingIntakeFacts,
    cfg: &PilotConfig,
) -> Vec<String> {
    use scripps_workflow_core::ids::TaskId;
    let mut ready: Vec<(&TaskId, &scripps_workflow_core::dag::Task)> = dag
        .tasks
        .iter()
        .filter(|(_, t)| matches!(t.state, TaskState::Ready))
        .collect();
    ready.sort_by(|a, b| a.0.cmp(b.0));

    if ready.len() < MIN_READY_TASKS_FOR_QUARTILE {
        // Typed role via `derive_role_from_id`.
        return ready
            .into_iter()
            .filter(|(id, _)| {
                !scripps_workflow_core::taxonomy::derive_role_from_id(id.as_str()).is_discovery()
            })
            .take(cfg.task_count)
            .map(|(id, _)| id.to_string())
            .collect();
    }

    // Compute a single-number weight per task using the sizing profile.
    // Tasks whose stage is review-only (no requirements) get weight 0.
    // `stage_class` is read from the task's `spec` JSON payload — the
    // same extraction pattern used by `aws.rs::task_stage_class`.
    let weighted: Vec<(&TaskId, u64)> = ready
        .iter()
        .map(|(id, task)| {
            let stage_class = task_stage_class(task);
            let req = super::sizing::compute_high_water(profiles, &stage_class, facts, &[]);
            let w = match req {
                Some(r) => (r.vcpus as u64) * (r.memory_gb as u64),
                None => 0,
            };
            (*id, w)
        })
        .collect();

    // Sort ascending by weight (primary), task_id (tie-break).
    let mut sorted = weighted;
    sorted.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));

    // Drop bottom + top quartiles, sample from the middle band.
    let n = sorted.len();
    let q = n / 4;
    let middle = &sorted[q..n.saturating_sub(q)];

    // Pick `task_count` from the middle band, spread across its range so
    // we sample at different weight strata rather than clumping.
    let mut picks: Vec<String> = Vec::new();
    let span = middle.len().max(1);
    for i in 0..cfg.task_count {
        if picks.len() >= middle.len() {
            break;
        }
        let idx = (i * span) / cfg.task_count.max(1);
        let idx = idx.min(middle.len().saturating_sub(1));
        let candidate = middle[idx].0.to_string();
        if !picks.contains(&candidate) {
            picks.push(candidate);
        }
    }
    // Fill out to cfg.task_count from the remaining middle band if
    // spacing collided on duplicates.
    for (id, _) in middle {
        if picks.len() >= cfg.task_count {
            break;
        }
        let id_str = id.to_string();
        if !picks.contains(&id_str) {
            picks.push(id_str);
        }
    }
    picks
}

/// Project each distinct `stage_class` in the DAG to the pilot-measured
/// peak resources × `projection_multiplier`. Stages the pilot didn't
/// observe fall back to the baseline `compute_high_water` output.
pub fn project_requirements(
    dag: &DAG,
    profiles: &ComputeProfiles,
    facts: &SizingIntakeFacts,
    measurements: &[PilotMeasurement],
    cfg: &PilotConfig,
) -> BTreeMap<String, ResourceRequirements> {
    let mut observed: BTreeMap<String, &PilotMeasurement> = BTreeMap::new();
    for m in measurements {
        observed
            .entry(m.stage_class.clone())
            .and_modify(|existing| {
                if m.peak_rss_mb > existing.peak_rss_mb {
                    *existing = m;
                }
            })
            .or_insert(m);
    }

    let mut out: BTreeMap<String, ResourceRequirements> = BTreeMap::new();
    for task in dag.tasks.values() {
        let stage_class = task_stage_class(task);
        if stage_class.is_empty() || out.contains_key(&stage_class) {
            continue;
        }
        let baseline = super::sizing::compute_high_water(profiles, &stage_class, facts, &[]);
        let Some(mut req) = baseline else { continue };
        if let Some(m) = observed.get(&stage_class) {
            let measured_memory_gb =
                ((m.peak_rss_mb as f64 / 1024.0) * cfg.projection_multiplier).ceil() as u32;
            if measured_memory_gb > req.memory_gb {
                req.memory_gb = measured_memory_gb;
            }
        }
        out.insert(stage_class, req);
    }
    out
}

/// Read a task's `stage_class` out of its spec JSON payload, matching
/// the `aws.rs::task_stage_class` helper. Kept local so the pilot
/// module doesn't depend on aws.rs internals.
fn task_stage_class(task: &scripps_workflow_core::dag::Task) -> String {
    task.spec
        .as_ref()
        .and_then(|s| s.get("stage_class"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Compute a confidence score in [0, 1]. Higher = closer agreement
/// between measured peak RSS and baseline memory_gb. 0.0 = no
/// measurements; 1.0 = measured ≤ baseline for every pilot task.
pub fn compute_confidence(
    measurements: &[PilotMeasurement],
    profiles: &ComputeProfiles,
    facts: &SizingIntakeFacts,
) -> f64 {
    if measurements.is_empty() {
        return 0.0;
    }
    let mut total = 0.0;
    let mut n = 0.0;
    for m in measurements {
        let req = super::sizing::compute_high_water(profiles, &m.stage_class, facts, &[]);
        let Some(req) = req else {
            continue;
        };
        let baseline_mb = (req.memory_gb as f64) * 1024.0;
        if baseline_mb == 0.0 {
            continue;
        }
        let ratio = (baseline_mb / (m.peak_rss_mb as f64 + 1.0)).min(1.0);
        total += ratio;
        n += 1.0;
    }
    if n == 0.0 {
        0.0
    } else {
        (total / n).clamp(0.0, 1.0)
    }
}

/// Serialize a `PilotReport` and write it to the package's
/// `runtime/pilot/report.json`. Also writes per-task measurement files
/// under `runtime/pilot/measurements/<task_id>.json` for offline
/// inspection.
pub fn write_pilot_artifacts(package: &Path, report: &PilotReport) -> Result<()> {
    let pilot_dir = package.join("runtime").join("pilot");
    fs::create_dir_all(&pilot_dir).with_context(|| format!("creating {}", pilot_dir.display()))?;
    let report_path = pilot_dir.join("report.json");
    let serialized = serde_json::to_string_pretty(report).context("serializing pilot report")?;
    fs::write(&report_path, serialized)
        .with_context(|| format!("writing {}", report_path.display()))?;

    let measurements_dir = pilot_dir.join("measurements");
    fs::create_dir_all(&measurements_dir)
        .with_context(|| format!("creating {}", measurements_dir.display()))?;
    for m in &report.measurements {
        // Pilot bookkeeping records the task id
        // into a filename, NOT into a shell command — so we normalize
        // disallowed bytes to `_` rather than refusing the call.
        // Refuse-style validation lives in the shared `_id_validator`
        // module and is used by the shell-interpolation sites
        // (`slurm::polling::probe_container_state` etc).
        let safe = super::_id_validator::normalize_task_id_for_filename(&m.task_id);
        let path = measurements_dir.join(format!("{}.json", safe));
        let body = serde_json::to_string_pretty(m).context("serializing measurement")?;
        fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// Load the latest pilot projections from `runtime/pilot/report.json`
/// when pilot mode is enabled for the current run. Returning `None`
/// when `SWFC_PILOT_ENABLED=0` avoids accidentally applying a stale
/// report left behind by an older session.
pub fn load_pilot_projected_requirements(
    package: &Path,
) -> Option<BTreeMap<String, ResourceRequirements>> {
    if !PilotConfig::from_env().enabled {
        return None;
    }
    let report_path = package.join("runtime").join("pilot").join("report.json");
    let raw = fs::read_to_string(report_path).ok()?;
    let report: PilotReport = serde_json::from_str(&raw).ok()?;
    if report.projected_requirements.is_empty() {
        None
    } else {
        Some(report.projected_requirements)
    }
}

// The local `sanitize_task_id` was promoted to
// `super::_id_validator::{normalize_task_id_for_filename, sanitize_task_id}`
// so all executor paths share one well-tested validator. Callers now
// use the shared module directly:
// * filename-shaped uses (pilot bookkeeping) →
// `normalize_task_id_for_filename`
// * shell-interpolated uses (SSH polling) →
// `sanitize_task_id` (refuse-on-unsafe)

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
    // `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
    // edition because the env table is not thread-safe). All call sites
    // are single-threaded test setup; bounded waiver scoped to this
    // `mod tests` block.
    #![allow(unsafe_code)]
    use super::*;
    use scripps_workflow_core::dag::{Assignee, ResourceClass, Task, TaskId, TaskKind};
    use serde_json::json;
    use std::collections::BTreeMap as BT;

    fn task(id: &str, stage: &str, state: TaskState) -> (TaskId, Task) {
        (
            TaskId::from(id),
            Task {
                description: id.to_string(),
                kind: TaskKind::Computation,
                state,
                depends_on: vec![],
                assignee: Assignee::Agent,
                spec: Some(json!({ "stage_class": stage })),
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

    fn tiny_dag(tasks: Vec<(TaskId, Task)>) -> DAG {
        let mut t: BT<TaskId, Task> = BT::new();
        for (id, v) in tasks {
            t.insert(id, v);
        }
        DAG {
            version: "1.0".into(),
            schema_version: scripps_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "test".into(),
            current_task: None,
            tasks: t,
            reverse_deps: BT::new(),
            run_id: None,
        }
    }

    fn synth_profiles() -> ComputeProfiles {
        let yaml = r#"
default:
  requirements:
    vcpus: 4
    memory_gb: 16
    storage_gb: 50
profiles:
  discover_alignment:
    requirements:
      vcpus: 2
      memory_gb: 4
      storage_gb: 20
  alignment:
    requirements:
      vcpus: 16
      memory_gb: 128
      storage_gb: 200
  quantification:
    requirements:
      vcpus: 8
      memory_gb: 64
      storage_gb: 100
  differential_expression:
    requirements:
      vcpus: 4
      memory_gb: 32
      storage_gb: 50
  enrichment:
    requirements:
      vcpus: 2
      memory_gb: 16
      storage_gb: 20
"#;
        serde_yml::from_str(yaml).unwrap()
    }

    fn facts() -> SizingIntakeFacts {
        SizingIntakeFacts::default()
    }

    #[test]
    fn default_config_has_plan_values() {
        let c = PilotConfig::default();
        assert_eq!(c.task_count, 3);
        assert_eq!(c.projection_multiplier, 1.5);
        assert_eq!(c.pilot_instance_type, "t3.medium");
        assert_eq!(c.measurement_interval_secs, 5);
    }

    #[test]
    fn select_drops_top_and_bottom_quartiles() {
        // Five Ready tasks across weight range; bottom 1 (discover) and
        // top 1 (alignment) should be dropped.
        let dag = tiny_dag(vec![
            task("t1_discover", "discover_alignment", TaskState::Ready),
            task("t2_enrich", "enrichment", TaskState::Ready),
            task("t3_de", "differential_expression", TaskState::Ready),
            task("t4_quant", "quantification", TaskState::Ready),
            task("t5_align", "alignment", TaskState::Ready),
        ]);
        let cfg = PilotConfig::default();
        let picks = select_pilot_tasks(&dag, &synth_profiles(), &facts(), &cfg);
        assert_eq!(picks.len(), 3);
        assert!(
            !picks.iter().any(|p| p == "t1_discover"),
            "discover_* should be in the bottom quartile, got: {:?}",
            picks
        );
        assert!(
            !picks.iter().any(|p| p == "t5_align"),
            "alignment should be in the top quartile, got: {:?}",
            picks
        );
    }

    #[test]
    fn select_fallback_when_fewer_than_4_ready() {
        let dag = tiny_dag(vec![
            task("discover_xxx", "discover_alignment", TaskState::Ready),
            task("t2_enrich", "enrichment", TaskState::Ready),
        ]);
        let cfg = PilotConfig::default();
        let picks = select_pilot_tasks(&dag, &synth_profiles(), &facts(), &cfg);
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0], "t2_enrich");
    }

    #[test]
    fn select_is_deterministic_across_runs() {
        let dag = tiny_dag(vec![
            task("aaa", "differential_expression", TaskState::Ready),
            task("bbb", "differential_expression", TaskState::Ready),
            task("ccc", "differential_expression", TaskState::Ready),
            task("ddd", "differential_expression", TaskState::Ready),
            task("eee", "differential_expression", TaskState::Ready),
        ]);
        let cfg = PilotConfig::default();
        let a = select_pilot_tasks(&dag, &synth_profiles(), &facts(), &cfg);
        let b = select_pilot_tasks(&dag, &synth_profiles(), &facts(), &cfg);
        assert_eq!(a, b);
    }

    #[test]
    fn select_ignores_non_ready_tasks() {
        let dag = tiny_dag(vec![
            task(
                "t1",
                "enrichment",
                TaskState::Completed {
                    result: serde_json::Value::Null,
                },
            ),
            task("t2", "differential_expression", TaskState::Ready),
            task("t3", "quantification", TaskState::Pending),
        ]);
        let cfg = PilotConfig::default();
        let picks = select_pilot_tasks(&dag, &synth_profiles(), &facts(), &cfg);
        assert_eq!(picks, vec!["t2"]);
    }

    #[test]
    fn projection_scales_memory_by_multiplier() {
        let dag = tiny_dag(vec![task(
            "t1",
            "differential_expression",
            TaskState::Ready,
        )]);
        let profiles = synth_profiles();
        // Pilot observed 10 GB peak RSS for differential_expression; with
        // 1.5× that's 15 GB. Baseline is 32 GB, so we keep baseline.
        let measurements = vec![PilotMeasurement {
            task_id: "t1".into(),
            stage_class: "differential_expression".into(),
            peak_rss_mb: 10 * 1024,
            wall_time_secs: 100,
            disk_used_mb: 500,
            exit_status: 0,
        }];
        let cfg = PilotConfig::default();
        let proj = project_requirements(&dag, &profiles, &facts(), &measurements, &cfg);
        let req = proj.get("differential_expression").unwrap();
        assert_eq!(
            req.memory_gb, 32,
            "baseline should stick when measured < baseline"
        );

        // Now observe 40 GB peak; 1.5× = 60 GB which exceeds baseline.
        let heavy = vec![PilotMeasurement {
            task_id: "t1".into(),
            stage_class: "differential_expression".into(),
            peak_rss_mb: 40 * 1024,
            wall_time_secs: 100,
            disk_used_mb: 500,
            exit_status: 0,
        }];
        let proj2 = project_requirements(&dag, &profiles, &facts(), &heavy, &cfg);
        let req2 = proj2.get("differential_expression").unwrap();
        assert_eq!(
            req2.memory_gb, 60,
            "measured × 1.5 should override baseline"
        );
    }

    #[test]
    fn env_parsing_overrides_defaults() {
        // Serialize via the crate-wide env lock so parallel aws/sizing
        // tests don't observe our transient SWFC_PILOT_* overrides
        // (and vice versa).
        let _lock = super::super::SWFC_AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        unsafe {
            env::set_var("SWFC_PILOT_ENABLED", "1");
            env::set_var("SWFC_PILOT_TASKS", "5");
            env::set_var("SWFC_PILOT_MULTIPLIER", "2.0");
            env::set_var("SWFC_PILOT_INSTANCE", "t3.large");
        }
        let cfg = PilotConfig::from_env();
        assert!(cfg.enabled);
        assert_eq!(cfg.task_count, 5);
        assert!((cfg.projection_multiplier - 2.0).abs() < 1e-9);
        assert_eq!(cfg.pilot_instance_type, "t3.large");
        unsafe {
            env::remove_var("SWFC_PILOT_ENABLED");
            env::remove_var("SWFC_PILOT_TASKS");
            env::remove_var("SWFC_PILOT_MULTIPLIER");
            env::remove_var("SWFC_PILOT_INSTANCE");
        }
    }

    #[test]
    fn env_defaults_on_aws_mode() {
        // Serialize via the shared env lock.
        let _lock = super::super::SWFC_AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // env clean; with SWFC_EXECUTOR_MODE=aws, pilot should default on.
        unsafe {
            env::remove_var("SWFC_PILOT_ENABLED");
            env::set_var("SWFC_EXECUTOR_MODE", "aws");
        }
        assert!(PilotConfig::from_env().enabled);
        unsafe {
            env::set_var("SWFC_EXECUTOR_MODE", "local");
        }
        assert!(!PilotConfig::from_env().enabled);
        unsafe {
            env::remove_var("SWFC_EXECUTOR_MODE");
        }
    }

    #[test]
    fn confidence_one_when_measured_well_below_baseline() {
        let profiles = synth_profiles();
        let measurements = vec![PilotMeasurement {
            task_id: "t1".into(),
            stage_class: "differential_expression".into(),
            peak_rss_mb: 1024, // 1 GB — baseline is 32 GB
            wall_time_secs: 100,
            disk_used_mb: 50,
            exit_status: 0,
        }];
        let c = compute_confidence(&measurements, &profiles, &facts());
        assert!(c > 0.99, "got {}", c);
    }

    #[test]
    fn confidence_zero_when_no_measurements() {
        let c = compute_confidence(&[], &synth_profiles(), &facts());
        assert_eq!(c, 0.0);
    }

    #[test]
    fn write_pilot_artifacts_creates_files() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let report = PilotReport {
            measurements: vec![PilotMeasurement {
                task_id: "quant_01".into(),
                stage_class: "quantification".into(),
                peak_rss_mb: 4096,
                wall_time_secs: 120,
                disk_used_mb: 500,
                exit_status: 0,
            }],
            projected_requirements: BTreeMap::new(),
            confidence: 0.8,
        };
        write_pilot_artifacts(pkg, &report).unwrap();
        assert!(pkg.join("runtime/pilot/report.json").exists());
        assert!(pkg
            .join("runtime/pilot/measurements/quant_01.json")
            .exists());
    }

    #[test]
    fn normalize_task_id_for_filename_replaces_unsafe_chars() {
        // Regression for the prior local helper, now living
        // in `super::super::_id_validator`.
        use super::super::_id_validator::normalize_task_id_for_filename;
        assert_eq!(
            normalize_task_id_for_filename("task/with:slash"),
            "task_with_slash"
        );
        assert_eq!(
            normalize_task_id_for_filename("normal_task-01"),
            "normal_task-01"
        );
    }
}
