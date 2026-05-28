//! Live host-resource probing + per-pick budget allocation.
//!
//! The pre-existing `HardwareEnvelopeInputs::local_serial()` (in
//! `hardware_envelope.rs`) reports static `nproc` + `MemTotal` — every
//! agent sees the full host as if it were the only consumer. With the
//! validation lane shipping two agents in parallel, that produces
//! oversubscription: both agents' BLAS/OMP layers spawn `nproc` threads
//! and both heap-allocate as if they own the box.
//!
//! This module replaces those inputs at dispatch time with live free
//! resources (cgroup-aware `/proc/meminfo::MemAvailable` +
//! `/proc/loadavg`), reserves a small overhead margin so the harness +
//! server + UI survive a heavy iteration, and splits the remaining
//! budget across the picked tasks proportional to their declared
//! per-stage high-water requirements (from
//! `executor::sizing::compute_high_water`).
//!
//! Falls back gracefully on non-Linux hosts and when the package is
//! missing `policies/compute-resource-policy.json` / `intake-facts.json`
//! — the caller receives a sensible "full host minus overhead" budget
//! and the existing serial-mode behaviour holds.

use super::sizing::{merge_resource_requirements_max, ComputeProfiles, SizingIntakeFacts};
use super::{GpuRequirement, ResourceRequirements};
use scripps_workflow_core::dag::{TaskId, DAG};
use std::collections::BTreeMap;
use std::path::Path;

// ── Live host probe ─────────────────────────────────────────────────────────

/// Snapshot of host capacity + currently-free resources at probe time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostState {
    /// Total logical CPUs on the host.
    pub total_vcpus: u32,
    /// `total_vcpus - load_avg_1m`, clamped to `[1, total_vcpus]`. A
    /// lagging indicator (load_avg includes ~60s of decayed history),
    /// but the safest cheap heuristic without sampling `/proc/stat`
    /// twice with a sleep between.
    pub free_vcpus_estimate: u32,
    /// Total physical memory installed on the host (gigabytes).
    pub total_memory_gb: u32,
    /// Real-time `MemAvailable / 1024 / 1024` in GB. cgroup-aware on
    /// modern kernels — survives Docker / SLURM cgroup limits.
    pub free_memory_gb: u32,
    /// Live GPU state; one entry per device returned by `nvidia-smi`.
    pub gpus: Vec<GpuState>,
}

/// Live state of a single GPU device reported by `nvidia-smi`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuState {
    /// Zero-based device index as reported by `nvidia-smi`.
    pub index: u32,
    /// GPU product name (e.g. `"Tesla T4"`).
    pub kind: String,
    /// Total VRAM in megabytes.
    pub total_mb: u32,
    /// Free (unallocated) VRAM in megabytes at probe time.
    pub free_mb: u32,
}

/// Probe live host resources. Single call — re-probe per iteration.
pub fn probe() -> HostState {
    let total_vcpus = probe_total_vcpus();
    HostState {
        total_vcpus,
        free_vcpus_estimate: probe_free_vcpus_estimate(total_vcpus),
        total_memory_gb: probe_total_memory_gb(),
        free_memory_gb: probe_free_memory_gb(),
        gpus: probe_gpus(),
    }
}

fn probe_total_vcpus() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
}

#[cfg(target_os = "linux")]
fn probe_free_vcpus_estimate(total: u32) -> u32 {
    let raw = match std::fs::read_to_string("/proc/loadavg") {
        Ok(s) => s,
        Err(_) => return total,
    };
    parse_loadavg_first(&raw)
        .map(|load| {
            let used = load.ceil() as u32;
            total.saturating_sub(used).max(1)
        })
        .unwrap_or(total)
}

#[cfg(not(target_os = "linux"))]
fn probe_free_vcpus_estimate(total: u32) -> u32 {
    total
}

fn parse_loadavg_first(raw: &str) -> Option<f64> {
    raw.split_whitespace().next().and_then(|s| s.parse().ok())
}

#[cfg(target_os = "linux")]
fn probe_total_memory_gb() -> u32 {
    parse_meminfo(&std::fs::read_to_string("/proc/meminfo").unwrap_or_default())
        .0
        .unwrap_or(8)
}

#[cfg(not(target_os = "linux"))]
fn probe_total_memory_gb() -> u32 {
    8
}

#[cfg(target_os = "linux")]
fn probe_free_memory_gb() -> u32 {
    parse_meminfo(&std::fs::read_to_string("/proc/meminfo").unwrap_or_default())
        .1
        .unwrap_or(probe_total_memory_gb())
}

#[cfg(not(target_os = "linux"))]
fn probe_free_memory_gb() -> u32 {
    probe_total_memory_gb()
}

/// Returns `(total_gb, available_gb)` parsed from `/proc/meminfo`. Both
/// `None` if the file is malformed.
fn parse_meminfo(raw: &str) -> (Option<u32>, Option<u32>) {
    let mut total_kb: Option<u64> = None;
    let mut avail_kb: Option<u64> = None;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail_kb = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        }
        if total_kb.is_some() && avail_kb.is_some() {
            break;
        }
    }
    (kb_to_gb(total_kb), kb_to_gb(avail_kb))
}

fn kb_to_gb(kb: Option<u64>) -> Option<u32> {
    kb.map(|k| (k / 1024 / 1024) as u32)
}

/// Probe NVIDIA GPUs via `nvidia-smi --query-gpu=index,name,memory.total,memory.free
/// --format=csv,noheader,nounits`. Returns an empty Vec on any failure
/// (no GPU, driver missing, non-zero exit, parse error) so CPU-only
/// hosts get an empty list.
fn probe_gpus() -> Vec<GpuState> {
    let output = match std::process::Command::new("nvidia-smi")
        .arg("--query-gpu=index,name,memory.total,memory.free")
        .arg("--format=csv,noheader,nounits")
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_nvidia_smi(&stdout)
}

fn parse_nvidia_smi(raw: &str) -> Vec<GpuState> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() < 4 {
            continue;
        }
        let Ok(index) = parts[0].parse::<u32>() else {
            continue;
        };
        let Ok(total_mb) = parts[2].parse::<u32>() else {
            continue;
        };
        let Ok(free_mb) = parts[3].parse::<u32>() else {
            continue;
        };
        out.push(GpuState {
            index,
            kind: normalize_gpu_kind(parts[1]),
            total_mb,
            free_mb,
        });
    }
    out
}

/// `NVIDIA L4` → `nvidia-l4`; matches the `kind` strings the
/// compute-profiles use (`nvidia-l4`, `nvidia-a100`, etc.).
fn normalize_gpu_kind(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace(' ', "-")
}

// ── Overhead policy ─────────────────────────────────────────────────────────

/// How much of the live free budget to hold back for the harness +
/// server + UI + OS. Two complementary controls — the larger of the
/// absolute or percentage reserve wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverheadPolicy {
    /// Absolute vCPUs always held back regardless of percentage.
    pub reserved_vcpus: u32,
    /// Absolute memory gigabytes always held back regardless of percentage.
    pub reserved_memory_gb: u32,
    /// Percentage of total resources held back (complements the absolute values).
    pub reserved_pct: u32,
}

impl OverheadPolicy {
    /// Default absolute vCPU reserve.
    pub const DEFAULT_VCPUS: u32 = 1;
    /// Default absolute memory reserve in gigabytes.
    pub const DEFAULT_MEMORY_GB: u32 = 2;
    /// Default percentage reserve applied to all resource dimensions.
    pub const DEFAULT_PCT: u32 = 10;

    /// Construct an `OverheadPolicy` with the compiled-in defaults.
    pub fn defaults() -> Self {
        Self {
            reserved_vcpus: Self::DEFAULT_VCPUS,
            reserved_memory_gb: Self::DEFAULT_MEMORY_GB,
            reserved_pct: Self::DEFAULT_PCT,
        }
    }

    /// Read overrides from env. Unrecognised values fall back to the
    /// default — a typo can't accidentally remove the overhead margin.
    pub fn from_env() -> Self {
        Self {
            reserved_vcpus: env_u32("SWFC_HW_OVERHEAD_VCPUS", Self::DEFAULT_VCPUS),
            reserved_memory_gb: env_u32("SWFC_HW_OVERHEAD_MEMORY_GB", Self::DEFAULT_MEMORY_GB),
            reserved_pct: env_u32("SWFC_HW_OVERHEAD_PCT", Self::DEFAULT_PCT).min(50),
        }
    }

    /// Final reserved vCPUs = max(absolute, ceil(total * pct / 100)).
    pub fn reserved_vcpus_for(&self, total: u32) -> u32 {
        let pct_share = (u64::from(total) * u64::from(self.reserved_pct)).div_ceil(100) as u32;
        self.reserved_vcpus.max(pct_share)
    }

    /// Final reserved memory_gb = max(absolute, ceil(total * pct / 100)).
    pub fn reserved_memory_gb_for(&self, total: u32) -> u32 {
        let pct_share = (u64::from(total) * u64::from(self.reserved_pct)).div_ceil(100) as u32;
        self.reserved_memory_gb.max(pct_share)
    }
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

// ── Per-pick allocator ──────────────────────────────────────────────────────

/// Final allocated slice for one agent invocation. Plugged into
/// `HardwareEnvelopeInputs` so the agent's `SWFC_HW_*` env vars
/// reflect the agent-specific budget rather than the full host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAllocation {
    /// Allocated vCPUs for this agent invocation.
    pub vcpus: u32,
    /// Allocated memory in gigabytes for this agent invocation.
    pub memory_gb: u32,
    /// "none" or "<kind>:<count>" matching `SWFC_HW_GPU` shape.
    pub gpu_descriptor: String,
}

impl AgentAllocation {
    /// Minimum vCPU allocation; enforced as a floor even under contention.
    pub const MIN_VCPUS: u32 = 1;
    /// Minimum memory allocation in gigabytes; enforced as a floor under contention.
    pub const MIN_MEMORY_GB: u32 = 2;

    /// Construct a CPU-only allocation with no GPU descriptor.
    pub fn cpu_only(vcpus: u32, memory_gb: u32) -> Self {
        Self {
            vcpus: vcpus.max(Self::MIN_VCPUS),
            memory_gb: memory_gb.max(Self::MIN_MEMORY_GB),
            gpu_descriptor: "none".into(),
        }
    }
}

/// Allocate a per-pick budget from the live host state.
///
/// `requested` is `(task_id, ResourceRequirements)`. Order is
/// preserved — when proportional scaling is required, ties break by
/// the order in which picks were supplied (which the scheduler
/// already establishes deterministically).
///
/// Algorithm:
/// 1. Compute `usable = max(host.free - overhead, (1, MIN_MEMORY_GB))`.
/// 2. If `Σ requested ≤ usable`: each pick gets exactly its requested
///    requirement (right-sized — best case).
/// 3. Otherwise, scale each pick proportionally and clamp to floors:
///    `allocated_i = max(MIN_FLOOR, requested_i * usable / Σ requested)`.
/// 4. GPUs are winner-take-all per device — pick #1 gets device 0,
///    pick #2 gets device 1 if free, otherwise its GPU is dropped (the
///    agent falls back to CPU-only if it can; otherwise re-blocks on
///    its own).
pub fn allocate_for_picks(
    host: &HostState,
    overhead: &OverheadPolicy,
    requested: &[(TaskId, ResourceRequirements)],
) -> BTreeMap<TaskId, AgentAllocation> {
    let mut out = BTreeMap::new();
    if requested.is_empty() {
        return out;
    }

    let reserved_vcpus = overhead.reserved_vcpus_for(host.total_vcpus);
    let reserved_memory = overhead.reserved_memory_gb_for(host.total_memory_gb);
    let usable_vcpus = host
        .free_vcpus_estimate
        .saturating_sub(reserved_vcpus)
        .max(AgentAllocation::MIN_VCPUS);
    let usable_memory = host
        .free_memory_gb
        .saturating_sub(reserved_memory)
        .max(AgentAllocation::MIN_MEMORY_GB);

    let total_requested_vcpus: u64 = requested
        .iter()
        .map(|(_, r)| u64::from(r.vcpus.max(AgentAllocation::MIN_VCPUS)))
        .sum();
    let total_requested_memory: u64 = requested
        .iter()
        .map(|(_, r)| u64::from(r.memory_gb.max(AgentAllocation::MIN_MEMORY_GB)))
        .sum();

    let scale_vcpus = total_requested_vcpus > u64::from(usable_vcpus);
    let scale_memory = total_requested_memory > u64::from(usable_memory);

    // GPU bookkeeping — assign devices in pick order. Each request for
    // GPU consumes one free device; once they're exhausted, subsequent
    // GPU requests fall back to "none".
    let mut free_gpu_indices: Vec<usize> = (0..host.gpus.len()).collect();

    for (id, req) in requested {
        let req_vcpus = req.vcpus.max(AgentAllocation::MIN_VCPUS);
        let req_memory = req.memory_gb.max(AgentAllocation::MIN_MEMORY_GB);

        let vcpus = if scale_vcpus {
            scale_share(
                req_vcpus,
                total_requested_vcpus,
                usable_vcpus,
                AgentAllocation::MIN_VCPUS,
            )
        } else {
            req_vcpus
        };
        let memory_gb = if scale_memory {
            scale_share(
                req_memory,
                total_requested_memory,
                usable_memory,
                AgentAllocation::MIN_MEMORY_GB,
            )
        } else {
            req_memory
        };

        let gpu_descriptor = match req.gpu.as_ref() {
            Some(want) => {
                if let Some(slot) = free_gpu_indices
                    .iter()
                    .position(|i| host.gpus[*i].kind == want.kind)
                {
                    let dev = free_gpu_indices.remove(slot);
                    format!("{}:{}", host.gpus[dev].kind, want.count.max(1))
                } else if !free_gpu_indices.is_empty() {
                    // Wrong kind, but at least give them a free GPU.
                    let dev = free_gpu_indices.remove(0);
                    format!("{}:{}", host.gpus[dev].kind, want.count.max(1))
                } else {
                    "none".into()
                }
            }
            None => "none".into(),
        };

        out.insert(
            id.clone(),
            AgentAllocation {
                vcpus,
                memory_gb,
                gpu_descriptor,
            },
        );
    }
    out
}

fn scale_share(want: u32, total_want: u64, usable: u32, floor: u32) -> u32 {
    if total_want == 0 {
        return floor;
    }
    let share = (u64::from(want) * u64::from(usable)) / total_want;
    (share as u32).max(floor)
}

// ── Bridge: per-pick ResourceRequirements from package config ─────────────

/// Resolve a pick's per-task `ResourceRequirements` by looking up its
/// `stage_class` in `policies/compute-resource-policy.json`, applying
/// scaling factors from `policies/intake-facts.json`, and the SME's
/// chosen methods (when present in the task spec). Falls back to a
/// conservative baseline (2 vcpu, 4 GB) when any input is missing —
/// the allocator then proportionally scales that against the live
/// budget so we never starve a pick.
pub fn resolve_high_water_for(package: &Path, dag: &DAG, task_id: &str) -> ResourceRequirements {
    let task = dag.tasks.get(task_id);
    let stage_class = task
        .and_then(|t| t.spec.as_ref())
        .and_then(|s| s.get("stage_class"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let methods: Vec<String> = task
        .and_then(|t| t.spec.as_ref())
        .and_then(|s| s.get("method"))
        .and_then(|m| m.as_str())
        .map(|m| vec![m.to_string()])
        .unwrap_or_default();

    let pilot = if stage_class.is_empty() {
        None
    } else {
        super::pilot::load_pilot_projected_requirements(package)
            .and_then(|m| m.get(stage_class).cloned())
    };
    let profiles = load_compute_profiles(package);
    let facts = load_intake_facts(package).unwrap_or_default();

    let baseline = profiles
        .as_ref()
        .and_then(|p| super::sizing::compute_high_water(p, stage_class, &facts, &methods));
    match (baseline, pilot) {
        (Some(base), Some(projected)) => merge_resource_requirements_max(&base, &projected),
        (Some(base), None) => base,
        (None, Some(projected)) => projected,
        (None, None) => baseline_requirements(),
    }
}

fn baseline_requirements() -> ResourceRequirements {
    ResourceRequirements {
        vcpus: 2,
        memory_gb: 4,
        storage_gb: 0,
        gpu: None,
    }
}

fn load_compute_profiles(package: &Path) -> Option<ComputeProfiles> {
    let p = package.join("policies/compute-resource-policy.json");
    if !p.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(&p).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let yaml = serde_yml::to_string(&value).ok()?;
    serde_yml::from_str(&yaml).ok()
}

fn load_intake_facts(package: &Path) -> Option<SizingIntakeFacts> {
    let p = package.join("policies/intake-facts.json");
    if !p.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(&p).ok()?;
    serde_json::from_str(&raw).ok()
}

#[allow(dead_code)] // reserved-for-public-contract: keeps GpuRequirement live in the type tree
fn _gpu_requirement_used() {
    // Suppress unused-import lint for GpuRequirement on builds where
    // the allocator's GPU path isn't exercised — the type is part of
    // the public contract via ResourceRequirements::gpu.
    let _ = std::mem::size_of::<GpuRequirement>();
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use scripps_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
    use serde_json::json;

    fn host(total_vcpus: u32, free_vcpus: u32, total_mem: u32, free_mem: u32) -> HostState {
        HostState {
            total_vcpus,
            free_vcpus_estimate: free_vcpus,
            total_memory_gb: total_mem,
            free_memory_gb: free_mem,
            gpus: vec![],
        }
    }

    fn req(vcpus: u32, memory_gb: u32) -> ResourceRequirements {
        ResourceRequirements {
            vcpus,
            memory_gb,
            storage_gb: 0,
            gpu: None,
        }
    }

    fn req_gpu(vcpus: u32, memory_gb: u32, kind: &str, count: u32) -> ResourceRequirements {
        ResourceRequirements {
            vcpus,
            memory_gb,
            storage_gb: 0,
            gpu: Some(GpuRequirement {
                kind: kind.into(),
                count,
            }),
        }
    }

    fn ready_task(stage: &str, method: Option<&str>) -> Task {
        let mut spec = json!({ "stage_class": stage });
        if let Some(method) = method {
            spec["method"] = json!(method);
        }
        Task {
            description: stage.into(),
            kind: TaskKind::Computation,
            state: TaskState::Ready,
            depends_on: vec![],
            assignee: Assignee::Agent,
            spec: Some(spec),
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
    fn parses_meminfo_total_and_available() {
        let raw =
            "MemTotal:       65937060 kB\nMemFree:         1234 kB\nMemAvailable:   54321024 kB\n";
        let (total, avail) = parse_meminfo(raw);
        assert_eq!(total, Some(62)); // 65937060 / 1024 / 1024 ≈ 62
        assert_eq!(avail, Some(51)); // 54321024 / 1024 / 1024 ≈ 51
    }

    #[test]
    fn parses_meminfo_returns_none_when_missing_fields() {
        let raw = "Buffers: 1234 kB\n";
        assert_eq!(parse_meminfo(raw), (None, None));
    }

    #[test]
    fn parses_loadavg_first_field() {
        assert_eq!(parse_loadavg_first("0.87 1.23 2.05 1/123 4567"), Some(0.87));
        assert_eq!(parse_loadavg_first(""), None);
        assert_eq!(parse_loadavg_first("not-a-number"), None);
    }

    #[test]
    fn parses_nvidia_smi_csv_and_normalizes_kind() {
        let raw = "0, NVIDIA L4, 24564, 23001\n1, NVIDIA L4, 24564, 24564\n";
        let gpus = parse_nvidia_smi(raw);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].index, 0);
        assert_eq!(gpus[0].kind, "nvidia-l4");
        assert_eq!(gpus[0].total_mb, 24564);
        assert_eq!(gpus[0].free_mb, 23001);
        assert_eq!(gpus[1].free_mb, 24564);
    }

    #[test]
    fn parses_nvidia_smi_skips_malformed_lines() {
        let raw = "0, NVIDIA L4, 24564, 23001\nbogus line\n2, NVIDIA H100, 81920, 80000\n";
        let gpus = parse_nvidia_smi(raw);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].index, 0);
        assert_eq!(gpus[1].index, 2);
    }

    #[test]
    fn overhead_default_reserves_one_vcpu_and_two_gb() {
        let p = OverheadPolicy::defaults();
        assert_eq!(p.reserved_vcpus, 1);
        assert_eq!(p.reserved_memory_gb, 2);
        assert_eq!(p.reserved_pct, 10);
    }

    #[test]
    fn overhead_pct_dominates_when_larger() {
        // 32 vCPU host, 10% = 4 vCPU reserve, beats absolute 1.
        let p = OverheadPolicy::defaults();
        assert_eq!(p.reserved_vcpus_for(32), 4);
        // 64 GB host, 10% = 7 GB reserve, beats absolute 2.
        assert_eq!(p.reserved_memory_gb_for(64), 7);
    }

    #[test]
    fn overhead_absolute_dominates_when_pct_is_smaller() {
        // 4 vCPU host, 10% = 1 vCPU, equal to absolute 1 — both fine.
        let p = OverheadPolicy::defaults();
        assert_eq!(p.reserved_vcpus_for(4), 1);
        // 8 GB host, 10% = 1 GB, beaten by absolute 2.
        assert_eq!(p.reserved_memory_gb_for(8), 2);
    }

    #[test]
    fn overhead_percent_capped_at_50_to_prevent_typos() {
        std::env::set_var("SWFC_HW_OVERHEAD_PCT", "200");
        let p = OverheadPolicy::from_env();
        assert!(p.reserved_pct <= 50);
        std::env::remove_var("SWFC_HW_OVERHEAD_PCT");
    }

    #[test]
    fn allocator_returns_empty_for_no_picks() {
        let h = host(8, 8, 16, 16);
        let p = OverheadPolicy::defaults();
        assert!(allocate_for_picks(&h, &p, &[]).is_empty());
    }

    #[test]
    fn allocator_grants_full_high_water_when_under_budget() {
        // Host: 32 vCPU free, 64 GB free; overhead default ~4 vCPU + 7 GB.
        // Two picks summing to 18 vCPU + 36 GB ≤ 28 vCPU + 57 GB → exact match.
        let h = host(32, 32, 64, 64);
        let picks = vec![
            ("compute".into(), req(16, 32)),
            ("validate".into(), req(2, 4)),
        ];
        let alloc = allocate_for_picks(&h, &OverheadPolicy::defaults(), &picks);
        assert_eq!(alloc["compute"], AgentAllocation::cpu_only(16, 32));
        assert_eq!(alloc["validate"], AgentAllocation::cpu_only(2, 4));
    }

    #[test]
    fn allocator_proportionally_splits_when_over_budget() {
        // Tiny host: 8 vCPU free, 16 GB free; overhead 1+2 → usable 7+14.
        // Picks want 16+2 = 18 vCPU and 32+4 = 36 GB. Each gets a
        // share scaled by usable/sum.
        let h = host(8, 8, 16, 16);
        let picks = vec![
            ("compute".into(), req(16, 32)),
            ("validate".into(), req(2, 4)),
        ];
        let p = OverheadPolicy {
            reserved_vcpus: 1,
            reserved_memory_gb: 2,
            reserved_pct: 0,
        };
        let alloc = allocate_for_picks(&h, &p, &picks);
        let c = &alloc["compute"];
        let v = &alloc["validate"];
        // Validate stays at floor (1 vCPU, 2 GB); compute takes the rest
        // up to its proportional share (~6 vCPU, ~12 GB).
        assert!(v.vcpus >= AgentAllocation::MIN_VCPUS);
        assert!(v.memory_gb >= AgentAllocation::MIN_MEMORY_GB);
        assert!(c.vcpus > v.vcpus);
        assert!(c.memory_gb > v.memory_gb);
        // Sum cannot exceed usable (modulo rounding-up to floors).
        // With scaling, compute gets (16/18)*7 ≈ 6 vCPU; validate (2/18)*7 ≈ 0 → floored to 1.
        // 6 + 1 = 7, equals usable.
        assert_eq!(c.vcpus + v.vcpus, 7);
        assert_eq!(c.memory_gb + v.memory_gb, 14);
    }

    #[test]
    fn allocator_enforces_floors_when_proportional_share_is_zero() {
        let h = host(2, 2, 4, 4);
        let picks = vec![("a".into(), req(64, 128)), ("b".into(), req(64, 128))];
        let p = OverheadPolicy {
            reserved_vcpus: 0,
            reserved_memory_gb: 0,
            reserved_pct: 0,
        };
        let alloc = allocate_for_picks(&h, &p, &picks);
        for v in alloc.values() {
            assert!(v.vcpus >= AgentAllocation::MIN_VCPUS);
            assert!(v.memory_gb >= AgentAllocation::MIN_MEMORY_GB);
        }
    }

    #[test]
    fn allocator_clamps_oversized_single_pick_to_usable() {
        // Single pick wants 100 vCPU on an 8 vCPU host → clamp to usable.
        let h = host(8, 8, 16, 16);
        let picks = vec![("monster".into(), req(100, 200))];
        let p = OverheadPolicy::defaults();
        let alloc = allocate_for_picks(&h, &p, &picks);
        // Usable: 8 - max(1, ceil(8*10/100)) = 8 - 1 = 7. Memory: 16 - 2 = 14.
        assert_eq!(alloc["monster"].vcpus, 7);
        assert_eq!(alloc["monster"].memory_gb, 14);
    }

    #[test]
    fn allocator_winner_takes_first_gpu_device() {
        let mut h = host(16, 16, 64, 64);
        h.gpus = vec![
            GpuState {
                index: 0,
                kind: "nvidia-l4".into(),
                total_mb: 24564,
                free_mb: 24564,
            },
            GpuState {
                index: 1,
                kind: "nvidia-l4".into(),
                total_mb: 24564,
                free_mb: 24564,
            },
        ];
        let picks = vec![
            ("gpu_a".into(), req_gpu(4, 16, "nvidia-l4", 1)),
            ("gpu_b".into(), req_gpu(4, 16, "nvidia-l4", 1)),
            ("gpu_c".into(), req_gpu(4, 16, "nvidia-l4", 1)), // no device left
        ];
        let alloc = allocate_for_picks(&h, &OverheadPolicy::defaults(), &picks);
        assert_eq!(alloc["gpu_a"].gpu_descriptor, "nvidia-l4:1");
        assert_eq!(alloc["gpu_b"].gpu_descriptor, "nvidia-l4:1");
        assert_eq!(alloc["gpu_c"].gpu_descriptor, "none");
    }

    #[test]
    fn allocator_does_not_assign_gpu_when_request_omits_one() {
        let mut h = host(16, 16, 64, 64);
        h.gpus = vec![GpuState {
            index: 0,
            kind: "nvidia-l4".into(),
            total_mb: 24564,
            free_mb: 24564,
        }];
        let picks = vec![("cpu_only".into(), req(4, 16))];
        let alloc = allocate_for_picks(&h, &OverheadPolicy::defaults(), &picks);
        assert_eq!(alloc["cpu_only"].gpu_descriptor, "none");
    }

    #[test]
    fn resolve_high_water_merges_enabled_pilot_projection_with_static_profile() {
        let _lock = super::super::SWFC_AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let prior = std::env::var("SWFC_PILOT_ENABLED").ok();
        std::env::set_var("SWFC_PILOT_ENABLED", "1");

        let pkg = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(pkg.path().join("policies")).unwrap();
        std::fs::create_dir_all(pkg.path().join("runtime/pilot")).unwrap();
        std::fs::write(
            pkg.path().join("policies/compute-resource-policy.json"),
            serde_json::to_string(&json!({
                "default": { "requirements": { "vcpus": 2, "memory_gb": 4, "storage_gb": 50 } },
                "profiles": {
                    "variant_calling": {
                        "requirements": { "vcpus": 8, "memory_gb": 32, "storage_gb": 200 }
                    }
                },
                "method_overrides": {
                    "deepvariant": {
                        "requires": {
                            "gpu": { "kind": "nvidia-l4", "count": 1 }
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            pkg.path().join("runtime/pilot/report.json"),
            serde_json::to_string(&json!({
                "measurements": [],
                "projected_requirements": {
                    "variant_calling": {
                        "vcpus": 4,
                        "memory_gb": 96,
                        "storage_gb": 100
                    }
                },
                "confidence": 0.9
            }))
            .unwrap(),
        )
        .unwrap();
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "vc".into(),
            ready_task("variant_calling", Some("deepvariant")),
        );
        let dag = DAG {
            version: "1".into(),
            schema_version: scripps_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "wf".into(),
            current_task: None,
            tasks,
            reverse_deps: BTreeMap::new(),
            run_id: None,
        };

        let req = resolve_high_water_for(pkg.path(), &dag, "vc");
        assert_eq!(req.vcpus, 8, "static CPU floor should remain");
        assert_eq!(req.memory_gb, 96, "pilot memory projection should raise");
        assert_eq!(req.storage_gb, 200, "static storage floor should remain");
        let gpu = req.gpu.expect("method-specific GPU must be preserved");
        assert_eq!(gpu.kind, "nvidia-l4");
        assert_eq!(gpu.count, 1);

        match prior {
            Some(v) => std::env::set_var("SWFC_PILOT_ENABLED", v),
            None => std::env::remove_var("SWFC_PILOT_ENABLED"),
        }
    }
}
