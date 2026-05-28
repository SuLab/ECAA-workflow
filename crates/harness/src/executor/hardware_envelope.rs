//! Render the per-task hardware envelope the agent
//! subprocess receives as `SWFC_HW_*` environment variables. The
//! envelope is inline (env vars, not written into `PROMPT.md` which
//! stays byte-reproducible from emit time).
//!
//! See
//! §5.1. `prompt_role.txt`'s "Hardware-aware execution" section tells
//! the agent how to consume these variables.
//!
//! The envelope is computed per-task (not per-iteration) because the
//! `tool_thread_curves` depend on the picked task's `stage_class`.
//! The initial implementation leaves `concurrent_peers_by_class` as a
//! static `{cpu_heavy: 1}` — a later iteration makes it dynamic when
//! the scheduler dispatches K parallel tasks.

use ecaa_workflow_core::dag::{ResourceClass, Task, DAG};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::path::Path;

/// Keys the harness always sets on the agent subprocess. Exhaustive
/// list so tests can assert every entry without re-reading the spec
/// from prompt_role.txt.
/// Env var key: available vCPUs for this task invocation.
pub const HW_VCPUS_AVAILABLE: &str = "SWFC_HW_VCPUS_AVAILABLE";
/// Env var key: total memory in GiB for this task invocation.
pub const HW_MEMORY_GB: &str = "SWFC_HW_MEMORY_GB";
/// Env var key: GPU descriptor string (e.g. "none" or "nvidia-l4:1").
pub const HW_GPU: &str = "SWFC_HW_GPU";
/// Env var key: recommended thread count for this task's resource class.
pub const HW_RECOMMENDED_THREADS: &str = "SWFC_HW_RECOMMENDED_THREADS";
/// Env var key: JSON map of tool → thread-count curves for this stage class.
pub const HW_TOOL_THREAD_CURVES: &str = "SWFC_HW_TOOL_THREAD_CURVES";
/// Env var key: JSON map of additional per-task env-var overrides.
pub const HW_ENV_OVERRIDES: &str = "SWFC_HW_ENV_OVERRIDES";
/// Env var key: JSON GPU-capability reference for the provisioned instance.
pub const HW_GPU_CAPABILITY_REF: &str = "SWFC_HW_GPU_CAPABILITY_REF";
/// Env var key: JSON-serialised `SizingIntakeFacts` for the current package.
pub const HW_INTAKE_FACTS: &str = "SWFC_HW_INTAKE_FACTS";
/// Env var key: JSON map of concurrent peer counts by resource class.
pub const HW_CONCURRENT_PEERS_BY_CLASS: &str = "SWFC_HW_CONCURRENT_PEERS_BY_CLASS";
/// Env var key: resource class string of the dispatched task (e.g. "cpu_heavy").
pub const HW_TASK_RESOURCE_CLASS: &str = "SWFC_HW_TASK_RESOURCE_CLASS";

/// BLAS / OpenMP / numerical-library thread-budget env vars the harness
/// always exports as bare env vars on the agent subprocess. Each is
/// read by the corresponding shared library at LIBRARY-INIT time
/// (when BLAS is dlopen'd as the language runtime starts), so the
/// harness MUST set them before spawning the agent — `Sys.setenv()` in
/// R or `os.environ[...] =...` in Python is too late if BLAS has
/// already loaded.
///
/// Coverage spans every BLAS implementation we've encountered in
/// bioinformatics stacks (OpenBLAS, MKL, BLIS, Apple Accelerate,
/// reference netlib via LD_PRELOAD) plus closely-adjacent thread
/// pools (OpenMP, NumExpr, Numba, Rayon, TBB, Julia, Polars).
///
/// All keys are set to the same value (`recommended_threads`) so a
/// single-process Rscript or Python script gets the full thread
/// budget by default. For multi-worker fan-out (BiocParallel mclapply,
/// joblib/loky), the agent is responsible for constraining per-worker
/// BLAS at runtime via `RhpcBLASctl::blas_set_num_threads(N)` inside
/// each worker — see `prompt_role.txt` "Hardware-aware execution".
pub const BLAS_THREAD_ENV_KEYS: &[&str] = &[
    "OMP_NUM_THREADS",
    "OPENBLAS_NUM_THREADS",
    "GOTO_NUM_THREADS",
    "MKL_NUM_THREADS",
    "BLIS_NUM_THREADS",
    "VECLIB_MAXIMUM_THREADS",
    "NUMEXPR_NUM_THREADS",
    "NUMEXPR_MAX_THREADS",
    "TBB_NUM_THREADS",
    "RAYON_NUM_THREADS",
    "NUMBA_NUM_THREADS",
    "JULIA_NUM_THREADS",
    "POLARS_MAX_THREADS",
];
/// the dispatched task's id. The agent script reads
/// this to drive the heartbeat-touch background loop. Required for the
/// heartbeat stall detector; absent ⇒ the detector falls back to
/// `started_at` age only.
pub const TASK_ID_ENV: &str = "SWFC_TASK_ID";

/// Backend identity the envelope describes. Keeps the renderer
/// backend-agnostic: local takes these from `nproc` / `/proc/meminfo`;
/// AWS paths take them from the provisioned instance type.
#[derive(Debug, Clone)]
/// Inputs that render the `SWFC_HW_*` env-var envelope for a single task dispatch.
pub struct HardwareEnvelopeInputs {
    /// Number of vCPUs available to the task on the host.
    pub vcpus_available: u32,
    /// Total host memory in GiB available to the task.
    pub memory_gb: u32,
    /// "none" or "<kind>:<count>" (e.g., "nvidia-l4:1").
    pub gpu_descriptor: String,
    /// How many tasks of each class are currently running concurrently,
    /// including the task this envelope describes. The serial-scheduler
    /// path always passes `{cpu_heavy: 1}`.
    pub concurrent_peers_by_class: BTreeMap<String, u32>,
}

impl HardwareEnvelopeInputs {
    /// Defaults for serial local scheduling. Probes `nproc`
    /// and meminfo on Linux; falls back to conservative constants
    /// elsewhere. `concurrent_peers_by_class` is static
    /// `{cpu_heavy: 1}` — parallel scheduling computes this per-launch.
    pub fn local_serial() -> Self {
        let vcpus_available = probe_vcpus();
        let memory_gb = probe_memory_gb();
        let mut peers: BTreeMap<String, u32> = BTreeMap::new();
        peers.insert("cpu_heavy".into(), 1);
        peers.insert("io_heavy".into(), 0);
        peers.insert("memory_heavy".into(), 0);
        peers.insert("gpu".into(), 0);
        Self {
            vcpus_available,
            memory_gb,
            gpu_descriptor: "none".into(),
            concurrent_peers_by_class: peers,
        }
    }
}

/// Render the envelope for `task_id` into a ready-to-inject env var
/// map. Missing compute-resource-policy.json / intake-facts.json are
/// not errors — the envelope just omits the tool-thread / env-override
/// / intake-facts keys. That way a package emitted without
/// `compute_profiles_dir` still gets a usable (if minimal) envelope.
pub fn render_envelope(
    package: &Path,
    task_id: &str,
    dag: &DAG,
    inputs: &HardwareEnvelopeInputs,
) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = BTreeMap::new();

    env.insert(
        HW_VCPUS_AVAILABLE.into(),
        inputs.vcpus_available.to_string(),
    );
    env.insert(HW_MEMORY_GB.into(), inputs.memory_gb.to_string());
    env.insert(HW_GPU.into(), inputs.gpu_descriptor.clone());
    env.insert(TASK_ID_ENV.into(), task_id.to_string());
    env.insert(
        HW_RECOMMENDED_THREADS.into(),
        inputs.vcpus_available.to_string(),
    );
    env.insert(
        HW_CONCURRENT_PEERS_BY_CLASS.into(),
        serde_json::to_string(&inputs.concurrent_peers_by_class).unwrap_or_else(|_| "{}".into()),
    );

    // Bare BLAS / thread-pool env vars — set on every task regardless of
    // whether a stage profile exists in compute-resource-policy.json.
    // Critical: BLAS reads these at.so init, so they MUST be present
    // in the parent shell environment before Rscript/python starts.
    apply_blas_thread_envelope(&mut env, inputs.vcpus_available);

    // Per-task resource class (for the agent + Phase-3 scheduler).
    let task = dag.tasks.get(task_id);
    let resource_class = task
        .map(|t| t.resource_class)
        .unwrap_or(ResourceClass::CpuHeavy);
    env.insert(
        HW_TASK_RESOURCE_CLASS.into(),
        resource_class_str(resource_class).into(),
    );

    // Static reference — the agent resolves this relative to the
    // package root when probing `which <binary>` for GPU routing.
    env.insert(
        HW_GPU_CAPABILITY_REF.into(),
        "policies/gpu-capability-policy.json".into(),
    );

    // Stage-specific tool/env guidance from compute-resource-policy.json.
    if let Some(stage_class) = task.and_then(task_stage_class) {
        if let Some(compute) = load_compute_policy(package) {
            if let Some(profile) = compute.get("profiles").and_then(|p| p.get(&stage_class)) {
                if let Some(curves) = profile.get("tool_thread_curves") {
                    env.insert(HW_TOOL_THREAD_CURVES.into(), curves.to_string());
                }
                if let Some(overrides) = profile.get("env_overrides_template") {
                    let resolved =
                        resolve_env_overrides_template(overrides, inputs.vcpus_available);
                    // Bundled JSON (back-compat for callers parsing
                    // SWFC_HW_ENV_OVERRIDES).
                    env.insert(HW_ENV_OVERRIDES.into(), resolved.to_string());
                    // Promote each override key to a bare env var. The
                    // bundled JSON alone is useless for thread control
                    // Because BLAS reads at.so init — the bare key has
                    // to be present in the parent process env. Operator
                    // overrides (e.g. CUDA_VISIBLE_DEVICES) win over the
                    // BLAS defaults set above.
                    if let Some(obj) = resolved.as_object() {
                        for (k, v) in obj.iter() {
                            if let Some(s) = v.as_str() {
                                env.insert(k.clone(), s.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(facts) = load_intake_facts(package) {
        env.insert(HW_INTAKE_FACTS.into(), facts.to_string());
    }

    env
}

/// Set every key in [`BLAS_THREAD_ENV_KEYS`] to `recommended_threads`.
/// Called unconditionally from [`render_envelope`] so single-process
/// numerical Rscript/python work always picks up the full thread
/// budget at BLAS-init time. Per-stage `env_overrides_template` runs
/// after this and last-write-wins, so an operator can pin any single
/// key to a different value via `compute-resource-policy.json`.
fn apply_blas_thread_envelope(env: &mut BTreeMap<String, String>, recommended_threads: u32) {
    let val = recommended_threads.to_string();
    for k in BLAS_THREAD_ENV_KEYS {
        env.insert((*k).to_string(), val.clone());
    }
}

fn resource_class_str(rc: ResourceClass) -> &'static str {
    match rc {
        ResourceClass::CpuHeavy => "cpu_heavy",
        ResourceClass::IoHeavy => "io_heavy",
        ResourceClass::MemoryHeavy => "memory_heavy",
        ResourceClass::Gpu => "gpu",
    }
}

fn task_stage_class(task: &Task) -> Option<String> {
    task.spec
        .as_ref()
        .and_then(|s| s.get("stage_class"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn load_compute_policy(package: &Path) -> Option<JsonValue> {
    let p = package.join("policies/compute-resource-policy.json");
    if !p.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(&p).ok()?;
    serde_json::from_str(&raw).ok()
}

fn load_intake_facts(package: &Path) -> Option<JsonValue> {
    let p = package.join("policies/intake-facts.json");
    if !p.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(&p).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Substitute `${recommended_threads}` into env-override string values.
/// Non-string values are passed through unchanged so a malformed
/// template can't drop valid keys.
fn resolve_env_overrides_template(template: &JsonValue, recommended_threads: u32) -> JsonValue {
    let Some(obj) = template.as_object() else {
        return template.clone();
    };
    let mut out = serde_json::Map::new();
    let marker = "${recommended_threads}";
    for (k, v) in obj.iter() {
        let Some(s) = v.as_str() else {
            out.insert(k.clone(), v.clone());
            continue;
        };
        let resolved = s.replace(marker, &recommended_threads.to_string());
        out.insert(k.clone(), JsonValue::String(resolved));
    }
    JsonValue::Object(out)
}

#[cfg(target_os = "linux")]
fn probe_vcpus() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
}

#[cfg(not(target_os = "linux"))]
fn probe_vcpus() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
}

#[cfg(target_os = "linux")]
fn probe_memory_gb() -> u32 {
    let contents = match std::fs::read_to_string("/proc/meminfo") {
        Ok(s) => s,
        Err(_) => return 8,
    };
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            if let Some(kb_str) = rest.split_whitespace().next() {
                if let Ok(kb) = kb_str.parse::<u64>() {
                    return ((kb / 1024) / 1024) as u32;
                }
            }
        }
    }
    8
}

#[cfg(not(target_os = "linux"))]
fn probe_memory_gb() -> u32 {
    8
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::dag::{
        Assignee, ResourceClass, Task, TaskId, TaskKind, TaskState, DAG,
    };
    use std::collections::BTreeMap as BT;
    use tempfile::TempDir;

    fn minimal_dag(task_id: &str, stage_class: &str, rc: ResourceClass) -> DAG {
        let mut tasks = BT::new();
        tasks.insert(
            TaskId::from(task_id),
            Task {
                kind: TaskKind::Computation,
                state: TaskState::Ready,
                depends_on: vec![],
                assignee: Assignee::Agent,
                description: "envelope-test task".into(),
                spec: Some(serde_json::json!({"stage_class": stage_class})),
                resolution: None,
                result_ref: None,
                resource_class: rc,
                requires_sme_review: false,

                required_artifacts: vec![],
                container: None,
                source_atom_id: None,
                safety: Default::default(),
            },
        );
        DAG {
            version: "1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "envelope-test".into(),
            current_task: None,
            tasks,
            reverse_deps: BT::new(),
            run_id: None,
        }
    }

    fn inputs_phase2_cpu() -> HardwareEnvelopeInputs {
        let mut peers = BTreeMap::new();
        peers.insert("cpu_heavy".into(), 1);
        peers.insert("io_heavy".into(), 0);
        peers.insert("memory_heavy".into(), 0);
        peers.insert("gpu".into(), 0);
        HardwareEnvelopeInputs {
            vcpus_available: 16,
            memory_gb: 64,
            gpu_descriptor: "none".into(),
            concurrent_peers_by_class: peers,
        }
    }

    #[test]
    fn envelope_always_sets_core_keys() {
        let tmp = TempDir::new().unwrap();
        let dag = minimal_dag(
            "alignment_t1",
            "alignment_quantification",
            ResourceClass::CpuHeavy,
        );
        let env = render_envelope(tmp.path(), "alignment_t1", &dag, &inputs_phase2_cpu());
        for key in [
            HW_VCPUS_AVAILABLE,
            HW_MEMORY_GB,
            HW_GPU,
            HW_RECOMMENDED_THREADS,
            HW_CONCURRENT_PEERS_BY_CLASS,
            HW_GPU_CAPABILITY_REF,
            HW_TASK_RESOURCE_CLASS,
        ] {
            assert!(env.contains_key(key), "envelope must always set {}", key);
        }
        assert_eq!(env[HW_VCPUS_AVAILABLE], "16");
        assert_eq!(env[HW_MEMORY_GB], "64");
        assert_eq!(env[HW_GPU], "none");
        assert_eq!(env[HW_RECOMMENDED_THREADS], "16");
        assert_eq!(env[HW_TASK_RESOURCE_CLASS], "cpu_heavy");
        assert_eq!(
            env[HW_GPU_CAPABILITY_REF],
            "policies/gpu-capability-policy.json"
        );
    }

    #[test]
    fn envelope_surfaces_task_resource_class_for_gpu_task() {
        let tmp = TempDir::new().unwrap();
        let dag = minimal_dag("vc_t1", "variant_calling", ResourceClass::Gpu);
        let mut gpu_inputs = inputs_phase2_cpu();
        gpu_inputs.gpu_descriptor = "nvidia-l4:1".into();
        let env = render_envelope(tmp.path(), "vc_t1", &dag, &gpu_inputs);
        assert_eq!(env[HW_TASK_RESOURCE_CLASS], "gpu");
        assert_eq!(env[HW_GPU], "nvidia-l4:1");
    }

    #[test]
    fn envelope_reads_tool_thread_curves_from_policy() {
        let tmp = TempDir::new().unwrap();
        let policies = tmp.path().join("policies");
        std::fs::create_dir_all(&policies).unwrap();
        std::fs::write(
            policies.join("compute-resource-policy.json"),
            r#"{
                "profiles": {
                    "alignment_quantification": {
                        "tool_thread_curves": {"bwa": 8, "star": 16},
                        "env_overrides_template": {
                            "OMP_NUM_THREADS": "${recommended_threads}"
                        }
                    }
                }
            }"#,
        )
        .unwrap();
        let dag = minimal_dag(
            "alignment_t1",
            "alignment_quantification",
            ResourceClass::CpuHeavy,
        );
        let env = render_envelope(tmp.path(), "alignment_t1", &dag, &inputs_phase2_cpu());
        assert!(env.contains_key(HW_TOOL_THREAD_CURVES));
        let curves: JsonValue =
            serde_json::from_str(env.get(HW_TOOL_THREAD_CURVES).unwrap()).unwrap();
        assert_eq!(curves["bwa"], 8);
        assert_eq!(curves["star"], 16);

        let overrides: JsonValue =
            serde_json::from_str(env.get(HW_ENV_OVERRIDES).unwrap()).unwrap();
        // ${recommended_threads} was 16 in inputs_phase2_cpu().
        assert_eq!(overrides["OMP_NUM_THREADS"], "16");
    }

    #[test]
    fn envelope_soft_skips_when_policies_absent() {
        let tmp = TempDir::new().unwrap();
        let dag = minimal_dag(
            "alignment_t1",
            "alignment_quantification",
            ResourceClass::CpuHeavy,
        );
        let env = render_envelope(tmp.path(), "alignment_t1", &dag, &inputs_phase2_cpu());
        // Missing compute-resource-policy → no tool_thread_curves /
        // env_overrides bundled JSON; core keys still present.
        assert!(!env.contains_key(HW_TOOL_THREAD_CURVES));
        assert!(!env.contains_key(HW_ENV_OVERRIDES));
        assert!(!env.contains_key(HW_INTAKE_FACTS));
        assert!(env.contains_key(HW_VCPUS_AVAILABLE));
        // Bare BLAS env vars are NOT skipped — they're set
        // unconditionally so even policy-less stages get parallel BLAS.
        for key in BLAS_THREAD_ENV_KEYS {
            assert!(
                env.contains_key(*key),
                "bare BLAS key {} must be set even without compute-resource-policy.json",
                key
            );
        }
    }

    #[test]
    fn envelope_always_sets_bare_blas_thread_env_vars() {
        // Reference netlib BLAS reads OPENBLAS_NUM_THREADS at.so init,
        // so the harness MUST export bare keys (not just the bundled
        // SWFC_HW_ENV_OVERRIDES JSON) to avoid single-threaded BLAS in
        // every Rscript / numpy invocation. Verify the contract for a
        // representative inputs shape and a representative stage.
        let tmp = TempDir::new().unwrap();
        let dag = minimal_dag("clust_t1", "clustering", ResourceClass::CpuHeavy);
        let env = render_envelope(tmp.path(), "clust_t1", &dag, &inputs_phase2_cpu());
        for key in BLAS_THREAD_ENV_KEYS {
            assert!(env.contains_key(*key), "bare BLAS key {} missing", key);
            assert_eq!(
                env[*key], "16",
                "bare BLAS key {} must equal recommended_threads (16)",
                key
            );
        }
        // Sanity: the universal BLAS envs everyone hits.
        assert_eq!(env["OMP_NUM_THREADS"], "16");
        assert_eq!(env["OPENBLAS_NUM_THREADS"], "16");
        assert_eq!(env["MKL_NUM_THREADS"], "16");
    }

    #[test]
    fn envelope_promotes_policy_overrides_to_bare_keys() {
        // When compute-resource-policy.json declares an
        // env_overrides_template, every key resolves to a bare env var
        // on the agent subprocess (not just the bundled JSON). An
        // operator can pin a custom key (e.g. CUDA_VISIBLE_DEVICES) or
        // override the BLAS default by setting it in the template.
        let tmp = TempDir::new().unwrap();
        let policies = tmp.path().join("policies");
        std::fs::create_dir_all(&policies).unwrap();
        std::fs::write(
            policies.join("compute-resource-policy.json"),
            r#"{
                "profiles": {
                    "alignment_quantification": {
                        "tool_thread_curves": {"star": 16},
                        "env_overrides_template": {
                            "OMP_NUM_THREADS": "${recommended_threads}",
                            "CUDA_VISIBLE_DEVICES": "0",
                            "OPENBLAS_NUM_THREADS": "8"
                        }
                    }
                }
            }"#,
        )
        .unwrap();
        let dag = minimal_dag(
            "aln_t1",
            "alignment_quantification",
            ResourceClass::CpuHeavy,
        );
        let env = render_envelope(tmp.path(), "aln_t1", &dag, &inputs_phase2_cpu());
        // Bundled JSON kept for back-compat.
        assert!(env.contains_key(HW_ENV_OVERRIDES));
        // Bare-key promotion: substituted (`${recommended_threads}` → "16"),
        // custom (CUDA_VISIBLE_DEVICES), and override-of-default
        // (OPENBLAS_NUM_THREADS=8 wins over the BLAS default of 16).
        assert_eq!(env["OMP_NUM_THREADS"], "16");
        assert_eq!(env["CUDA_VISIBLE_DEVICES"], "0");
        assert_eq!(env["OPENBLAS_NUM_THREADS"], "8");
        // BLAS keys not in the template stay at the recommended default.
        assert_eq!(env["MKL_NUM_THREADS"], "16");
    }

    #[test]
    fn envelope_reads_intake_facts_when_present() {
        let tmp = TempDir::new().unwrap();
        let policies = tmp.path().join("policies");
        std::fs::create_dir_all(&policies).unwrap();
        std::fs::write(
            policies.join("intake-facts.json"),
            r#"{"modality":"bulk_rnaseq","sample_count":50,"methods":[]}"#,
        )
        .unwrap();
        let dag = minimal_dag("t1", "preprocessing_qc", ResourceClass::CpuHeavy);
        let env = render_envelope(tmp.path(), "t1", &dag, &inputs_phase2_cpu());
        let facts: JsonValue = serde_json::from_str(&env[HW_INTAKE_FACTS]).unwrap();
        assert_eq!(facts["modality"], "bulk_rnaseq");
        assert_eq!(facts["sample_count"], 50);
    }

    #[test]
    fn env_overrides_template_substitution() {
        let tmpl = serde_json::json!({
            "OMP_NUM_THREADS": "${recommended_threads}",
            "MKL_NUM_THREADS": "${recommended_threads}",
            "STATIC_VAR": "8"
        });
        let out = resolve_env_overrides_template(&tmpl, 32);
        assert_eq!(out["OMP_NUM_THREADS"], "32");
        assert_eq!(out["MKL_NUM_THREADS"], "32");
        assert_eq!(out["STATIC_VAR"], "8");
    }
}
