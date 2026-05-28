//! High-water sizing — reads `config/compute-profiles/profiles.yaml` +
//! `policies/intake-facts.json` (emitted from a package) and resolves
//! the per-stage `ResourceRequirements` the harness should provision
//! against. Cloud-agnostic: the output is an abstract shape, mapped to
//! concrete EC2 types by `resolve_instance_type`.
//!
//! Pure computation, no AWS calls. Unit-testable without any network.

use super::{GpuRequirement, ResourceRequirements};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

// ── profiles.yaml shape ──────────────────────────────────────────────────────

/// Deserialized shape of `config/compute-profiles/profiles.yaml`.
/// Provides per-stage compute profiles, a global default, and
/// per-method overrides.
#[derive(Debug, Clone, Deserialize)]
pub struct ComputeProfiles {
    /// Per-stage profile map, keyed by the stage's resource class name.
    pub profiles: BTreeMap<String, StageProfile>,
    /// Fallback profile used when no per-stage entry matches.
    pub default: DefaultProfile,
    /// Optional per-method resource bumps applied on top of the stage profile.
    #[serde(default)]
    pub method_overrides: BTreeMap<String, MethodOverride>,
}

/// Per-stage resource profile read from `profiles.yaml`.
#[derive(Debug, Clone, Deserialize)]
pub struct StageProfile {
    #[serde(default)]
    #[allow(dead_code)]
    /// Human-readable description carried by the on-disk profile (informational; not used by the resolver).
    pub description: Option<String>,
    /// None when the stage is human-only (class "review"). The resolver
    /// treats a None base as "no instance needed".
    pub requirements: Option<BaseRequirements>,
    /// List of input-dimension-based scaling adjustments applied on top of
    /// `requirements` when the intake fact exceeds the threshold.
    #[serde(default)]
    pub scaling_factors: Vec<ScalingFactor>,
    #[serde(default)]
    #[allow(dead_code)]
    /// Free-form authoring notes (informational; not part of the resolver inputs).
    pub notes: Option<String>,
    /// Per-stage override for the SSM RunCommand timeout. When None,
    /// the AwsExecutor falls back to the session-level
    /// SWFC_AWS_SSM_TIMEOUT_SECS env var (default 3600). Stages
    /// known to run longer than an hour — alignment_quantification,
    /// variant_calling — should bump this.
    #[serde(default)]
    pub ssm_timeout_secs: Option<u64>,
    /// Per-tool thread budget hint. Populates the agent's
    /// `SWFC_HW_TOOL_THREAD_CURVES` envelope so STAR / BWA /
    /// samtools / salmon / GATK pick the right `--threads` value
    /// instead of defaulting to 1 or `$(nproc)`. Keys are the lowercased
    /// tool name the agent invokes (`bwa`, `star`, `samtools_sort`,
    /// `salmon`, `gatk_haplotypecaller`); values are thread counts.
    /// Empty map means "no per-tool guidance, agent falls back to
    /// recommended_threads".
    #[serde(default)]
    pub tool_thread_curves: BTreeMap<String, u32>,
    /// BLAS/OMP thread stampede prevention. Each key is an
    /// env var the agent should export before invoking numerical code;
    /// the value is a template string. `${recommended_threads}` is the
    /// only currently-supported substitution. Empty map = no overrides.
    #[serde(default)]
    pub env_overrides_template: BTreeMap<String, String>,
    /// Multi-phase tools (DeepVariant, Parabricks). Outer
    /// key is the tool name; inner map is phase_name → thread count.
    /// Surfaced into the compute-resource-policy the agent reads for
    /// phase-specific thread budgets. Empty map = single-phase tools
    /// only.
    #[serde(default)]
    pub phase_thread_counts: BTreeMap<String, BTreeMap<String, u32>>,
}

/// Global fallback profile applied when no per-stage entry matches
/// the task's resource class.
#[derive(Debug, Clone, Deserialize)]
pub struct DefaultProfile {
    /// Baseline resource shape for any unrecognised stage.
    pub requirements: BaseRequirements,
    #[serde(default)]
    #[allow(dead_code)]
    /// Free-form authoring notes (informational; not consumed by the resolver).
    pub notes: Option<String>,
}

/// Minimum compute shape required by a stage or method.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BaseRequirements {
    /// Missing vcpus in a method_override means "leave base unchanged".
    /// Stage-level profiles usually fill this; method_overrides often
    /// only bump memory or GPU.
    #[serde(default)]
    pub vcpus: u32,
    /// Minimum memory in gigabytes.
    #[serde(default)]
    pub memory_gb: u32,
    /// Minimum attached storage in gigabytes.
    #[serde(default)]
    pub storage_gb: u32,
    /// Optional GPU requirement; `None` means CPU-only.
    #[serde(default)]
    pub gpu: Option<GpuHint>,
}

/// GPU type and count required by a stage.
#[derive(Debug, Clone, Deserialize)]
pub struct GpuHint {
    /// GPU family string (e.g. `"nvidia-a10g"`) used to pick an
    /// accelerated instance type.
    pub kind: String,
    /// Number of GPU devices required.
    pub count: u32,
}

/// A single dimension-based scaling rule. When the intake fact named by
/// `dimension` exceeds `threshold` (or `threshold_gb`), the resolver
/// increases the resource requirements proportionally or by a fixed bump.
#[derive(Debug, Clone, Deserialize)]
pub struct ScalingFactor {
    /// Intake-fact field name this rule applies to (e.g. `"sample_count"`).
    pub dimension: String,
    /// Threshold in raw units (e.g. number of samples).
    #[serde(default)]
    pub threshold: Option<u64>,
    /// Threshold expressed in gigabytes (used for `genome_size_gb`).
    #[serde(default)]
    pub threshold_gb: Option<f64>,
    /// Additional vCPUs per unit of input above the threshold.
    #[serde(default)]
    pub per_unit_vcpus: Option<f64>,
    /// Additional storage gigabytes per unit of input above the threshold.
    #[serde(default)]
    pub per_unit_storage_gb: Option<u32>,
    /// Hard ceiling on vCPUs even when `per_unit_vcpus` would push higher.
    #[serde(default)]
    pub max_vcpus: Option<u32>,
    /// Fixed resource bump applied once when the threshold is crossed.
    #[serde(default)]
    pub above: Option<BumpValues>,
}

/// Fixed resource additions applied when a `ScalingFactor` threshold is
/// crossed. Fields are additive — `None` means "leave unchanged".
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BumpValues {
    /// Additional vCPUs to add.
    #[serde(default)]
    pub vcpus: Option<u32>,
    /// Additional memory gigabytes to add.
    #[serde(default)]
    pub memory_gb: Option<u32>,
    /// Additional storage gigabytes to add.
    #[serde(default)]
    pub storage_gb: Option<u32>,
}

/// Per-method resource override. Applied on top of the stage profile
/// when the task's `method` field matches this override's `applies_to` list.
#[derive(Debug, Clone, Deserialize)]
pub struct MethodOverride {
    /// Stage-class filter. When non-empty, this override is applied only
    /// to tasks whose `stage_class` appears in the list — see
    /// `apply_method_overrides`. Empty means "any stage where the SME
    /// named this method" (the historical default).
    #[serde(default)]
    #[allow(dead_code)]
    /// List of method names this override applies to (informational YAML field; not consumed by the resolver).
    pub applies_to: Vec<String>,
    /// Minimum resource shape when this method is selected.
    pub requires: BaseRequirements,
    #[serde(default)]
    #[allow(dead_code)]
    /// Free-form authoring notes (informational YAML field; not consumed by the resolver).
    pub notes: Option<String>,
}

/// Subset of `crates/core::intake_facts::IntakeFacts` the sizing layer
/// actually reads. Kept as a separate struct so the harness doesn't
/// cycle-depend on `ecaa-workflow-core::intake_facts`'s ts-rs
/// derivation when all we need are six scalar fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SizingIntakeFacts {
    /// Number of samples in the analysis; drives per-sample scaling rules.
    #[serde(default)]
    pub sample_count: Option<u32>,
    /// Sequencing coverage depth; drives memory / storage scaling.
    #[serde(default)]
    pub coverage_depth: Option<u32>,
    /// Single-cell cell count; used by scRNA-seq / scATAC sizing rules.
    #[serde(default)]
    pub cell_count: Option<u32>,
    /// Size of the reference database in gigabytes (e.g. Kraken2 DB).
    #[serde(default)]
    pub database_size_gb: Option<u32>,
    /// Organism genome size in GB — resolved from
    /// `config/compute-profiles/organism-sizes.yaml` by the caller before
    /// invoking the sizing layer. None means "assume default (human)".
    #[serde(default)]
    pub genome_size_gb: Option<f64>,
}

impl ComputeProfiles {
    /// Load and deserialize `profiles.yaml` from `path`.
    pub fn load(path: &Path) -> Result<Self> {
        ecaa_workflow_core::fs_helpers::read_yaml(path)
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Resolve the abstract `ResourceRequirements` for a stage given the
/// classifier's stage class, the current `IntakeFacts`, and the SME's
/// named methods (which may trigger `method_overrides`).
///
/// Returns `None` when the stage is human-only (e.g. `class: review`)
/// and no compute should be provisioned at all.
pub fn compute_high_water(
    profiles: &ComputeProfiles,
    stage_class: &str,
    facts: &SizingIntakeFacts,
    methods: &[String],
) -> Option<ResourceRequirements> {
    let (base, scaling_factors) = match profiles.profiles.get(stage_class) {
        Some(profile) => {
            let base = profile.requirements.clone()?;
            (base, profile.scaling_factors.clone())
        }
        None => (profiles.default.requirements.clone(), Vec::new()),
    };

    let mut result = from_base(&base);
    apply_scaling_factors(&mut result, &scaling_factors, facts);
    apply_method_overrides(
        &mut result,
        &profiles.method_overrides,
        methods,
        stage_class,
    );
    Some(result)
}

fn from_base(base: &BaseRequirements) -> ResourceRequirements {
    ResourceRequirements {
        vcpus: base.vcpus,
        memory_gb: base.memory_gb,
        storage_gb: base.storage_gb,
        gpu: base.gpu.as_ref().map(|g| GpuRequirement {
            kind: g.kind.clone(),
            count: g.count,
        }),
    }
}

/// Merge two resource estimates by taking the larger CPU, memory, and
/// storage floors. GPU presence is preserved when either side requests
/// one; when both do, the overlay's kind wins and the count is maxed.
///
/// Used when a pilot projection is layered on top of the static profile:
/// the pilot can raise observed memory/CPU needs without accidentally
/// deleting method-specific GPU requirements from the profile.
pub fn merge_resource_requirements_max(
    base: &ResourceRequirements,
    overlay: &ResourceRequirements,
) -> ResourceRequirements {
    let gpu = match (&base.gpu, &overlay.gpu) {
        (Some(a), Some(b)) => Some(GpuRequirement {
            kind: b.kind.clone(),
            count: a.count.max(b.count),
        }),
        (Some(a), None) => Some(a.clone()),
        (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    };
    ResourceRequirements {
        vcpus: base.vcpus.max(overlay.vcpus),
        memory_gb: base.memory_gb.max(overlay.memory_gb),
        storage_gb: base.storage_gb.max(overlay.storage_gb),
        gpu,
    }
}

fn apply_scaling_factors(
    req: &mut ResourceRequirements,
    factors: &[ScalingFactor],
    facts: &SizingIntakeFacts,
) {
    for factor in factors {
        match factor.dimension.as_str() {
            "sample_count" => {
                if let Some(n) = facts.sample_count {
                    if let Some(per) = factor.per_unit_storage_gb {
                        // Add per-sample storage on top of the base.
                        req.storage_gb = req.storage_gb.saturating_add(per * n);
                    }
                    if let Some(per) = factor.per_unit_vcpus {
                        let bumped = (req.vcpus as f64) + per * (n as f64);
                        let max = factor.max_vcpus.unwrap_or(u32::MAX);
                        req.vcpus = (bumped.ceil() as u32).min(max);
                    }
                }
            }
            "cell_count" => {
                if let (Some(threshold), Some(above)) = (factor.threshold, &factor.above) {
                    if let Some(count) = facts.cell_count {
                        if u64::from(count) > threshold {
                            apply_bump(req, above);
                        }
                    }
                }
            }
            "coverage_depth" => {
                if let (Some(threshold), Some(above)) = (factor.threshold, &factor.above) {
                    if let Some(depth) = facts.coverage_depth {
                        if u64::from(depth) > threshold {
                            apply_bump(req, above);
                        }
                    }
                }
            }
            "genome_size" => {
                if let (Some(threshold_gb), Some(above)) = (factor.threshold_gb, &factor.above) {
                    // Default to human if organism isn't known so we
                    // stay conservatively oversized.
                    let genome_size = facts.genome_size_gb.unwrap_or(3.10);
                    if genome_size > threshold_gb {
                        apply_bump(req, above);
                    }
                }
            }
            "database_size" => {
                if let (Some(threshold_gb), Some(above)) = (factor.threshold_gb, &factor.above) {
                    if let Some(size) = facts.database_size_gb {
                        if f64::from(size) > threshold_gb {
                            apply_bump(req, above);
                        }
                    }
                }
            }
            other => {
                // W1.2/B11: previously silently ignored. An unknown
                // scaling dimension is most likely a typo in
                // profiles.yaml or a stale config file from an older
                // harness release — surfacing it lets operators fix
                // the YAML rather than wonder why their bump rule
                // didn't fire.
                tracing::warn!(
                    target: "sizing",
                    dimension = other,
                    "ignoring unknown scaling dimension in profile; \
                     supported: sample_count, cell_count, coverage_depth, \
                     genome_size, database_size"
                );
            }
        }
    }
}

fn apply_bump(req: &mut ResourceRequirements, above: &BumpValues) {
    if let Some(v) = above.vcpus {
        req.vcpus = req.vcpus.max(v);
    }
    if let Some(m) = above.memory_gb {
        req.memory_gb = req.memory_gb.max(m);
    }
    if let Some(s) = above.storage_gb {
        req.storage_gb = req.storage_gb.max(s);
    }
}

fn apply_method_overrides(
    req: &mut ResourceRequirements,
    overrides: &BTreeMap<String, MethodOverride>,
    methods: &[String],
    stage_class: &str,
) {
    for m in methods {
        if let Some(over) = overrides.get(m.as_str()) {
            // If the override declares applies_to, skip when the stage
            // class isn't in its list. Empty list means "applies to any
            // stage where the SME named this method".
            if !over.applies_to.is_empty() && !over.applies_to.iter().any(|c| c == stage_class) {
                continue;
            }
            req.vcpus = req.vcpus.max(over.requires.vcpus);
            req.memory_gb = req.memory_gb.max(over.requires.memory_gb);
            req.storage_gb = req.storage_gb.max(over.requires.storage_gb);
            // GPU override always takes precedence — CPU-only base can
            // upgrade to a GPU shape when the method requires it.
            if let Some(gpu) = over.requires.gpu.clone() {
                req.gpu = Some(GpuRequirement {
                    kind: gpu.kind,
                    count: gpu.count,
                });
            }
        }
    }
}

/// Resolve the SSM RunCommand timeout for a given stage. Priority
/// order:
/// 1. `profiles.yaml::profiles.<stage_class>.ssm_timeout_secs`
/// 2. `SWFC_AWS_SSM_TIMEOUT_SECS` env var
/// 3. Default: 3600 (one hour)
///
/// The env var is read at call time — the AWS executor consults this
/// fn once per task, so a session-wide override via the env var takes
/// effect without restarting the harness.
pub fn resolve_ssm_timeout_secs(profiles: &ComputeProfiles, stage_class: &str) -> u64 {
    if let Some(profile) = profiles.profiles.get(stage_class) {
        if let Some(override_secs) = profile.ssm_timeout_secs {
            return override_secs;
        }
    }
    if let Ok(raw) = std::env::var("SWFC_AWS_SSM_TIMEOUT_SECS") {
        if let Ok(n) = raw.trim().parse::<u64>() {
            if n > 0 {
                return n;
            }
        }
    }
    3600
}

/// P2-166 — capacity facts for the resolver's candidate instance
/// types. Used both for the picking ladder AND for the post-pick
/// "did we satisfy vcpus?" warn. Keeping (name, vcpus, memory_gb)
/// in one constant means a future entry only needs adding here +
/// in the picker arms, never two parallel tables that can drift.
const INSTANCE_CAPACITY: &[(&str, u32, u32)] = &[
    // burstable
    ("t3.medium", 2, 4),
    ("t3.large", 2, 8),
    // compute-optimized c6i
    ("c6i.xlarge", 4, 8),
    ("c6i.2xlarge", 8, 16),
    ("c6i.4xlarge", 16, 32),
    ("c6i.8xlarge", 32, 64),
    ("c6i.12xlarge", 48, 96),
    // memory-optimized r6i
    ("r6i.xlarge", 4, 32),
    ("r6i.2xlarge", 8, 64),
    ("r6i.4xlarge", 16, 128),
    ("r6i.8xlarge", 32, 256),
    ("r6i.16xlarge", 64, 512),
    // GPU shapes (count-primary picker — see pick_gpu_instance_type)
    ("g4dn.xlarge", 4, 16),     // 1× T4
    ("g4dn.12xlarge", 48, 192), // 4× T4
    ("g6.xlarge", 4, 16),       // 1× L4
    ("p4d.24xlarge", 96, 1152), // 8× A100
];

/// Look up `(vcpus, memory_gb)` for an instance type. Returns
/// `(0, 0)` for unknown types so callers can degrade safely (the
/// undersized-warn skips emitting a confusing diff against zero).
pub(super) fn instance_capacity(instance_type: &str) -> (u32, u32) {
    INSTANCE_CAPACITY
        .iter()
        .find(|(t, _, _)| *t == instance_type)
        .map(|(_, v, m)| (*v, *m))
        .unwrap_or((0, 0))
}

/// Best-fit EC2 instance type for a `ResourceRequirements`. Uses a
/// minimal hard-coded mapping.
///
/// P2-166 — extends the ladder to cover c6i.{8,12}xlarge and
/// r6i.16xlarge so high-vCPU + high-memory tasks no longer cap out
/// at c6i.4xlarge (16 vCPU) / r6i.8xlarge (256 GB). When the chosen
/// shape's vCPU count is still below the request, log a
/// `swfc::sizing_undersized` warn so the operator can act on the
/// gap (typically: widen the SWFC_AWS_INSTANCE_TYPE_ALLOWLIST so
/// the picker can climb further).
pub fn resolve_instance_type(req: &ResourceRequirements) -> String {
    let picked = pick_instance_type(req);
    let (picked_vcpus, picked_mem) = instance_capacity(&picked);
    // P2-166 — surface undersized picks so operators see when the
    // resolver couldn't meet the request. `picked_vcpus == 0` means
    // the picked type isn't in `INSTANCE_CAPACITY`; skip the diff in
    // that case to avoid a spurious "picked 0 < requested N" line.
    if picked_vcpus > 0 && req.vcpus > picked_vcpus {
        tracing::warn!(
            target: "swfc::sizing_undersized",
            requested_vcpus = req.vcpus,
            picked_vcpus,
            requested_memory_gb = req.memory_gb,
            picked_memory_gb = picked_mem,
            picked = %picked,
            "sizing resolver could not satisfy vcpus request; consider widening \
             SWFC_AWS_INSTANCE_TYPE_ALLOWLIST or adding a larger family to the picker"
        );
    }
    picked
}

/// Inner picker — pure function over the ladder. Returns the
/// instance-type name for the first arm that meets the request.
///
/// P2-168 — step within the chosen family FIRST, swap to a different
/// family only when the family's ceiling is reached. Prior code
/// mixed the family decision (memory ≥ 32 → r6i) with the size
/// decision in one waterfall, which silently demoted "24 vCPUs +
/// 16 GB memory" to c6i.4xlarge (16 vCPU) instead of climbing to
/// c6i.8xlarge (32 vCPU / 64 GB). Now the picker uses
/// `pick_in_family` to step within compute / memory / burstable
/// independently and only crosses families when no in-family shape
/// can satisfy.
fn pick_instance_type(req: &ResourceRequirements) -> String {
    if let Some(gpu) = &req.gpu {
        // P2-167 — GPU shapes step by count first, then refine by
        // kind when the operator pinned a specific architecture.
        return pick_gpu_instance_type(gpu);
    }

    // Family decision: if memory is the dominant constraint (>=32 GB)
    // start in the memory-optimized family; otherwise start in
    // compute-optimized; otherwise burstable. Within whichever family
    // is picked, climb until vcpus + memory are both satisfied or
    // the family's ceiling is hit; then (and only then) cross-family.
    if req.memory_gb >= 32 {
        if let Some(pick) = pick_in_family(req, R6I_FAMILY) {
            return pick;
        }
        // Memory family exhausted; the ceiling (r6i.16xlarge / 512 GB)
        // is the largest shape we ship. No fallback is meaningful,
        // so emit the top and let the warn-on-undersized observer
        // surface the gap.
        return "r6i.16xlarge".to_string();
    }
    // Light loads (≤2 vCPU, ≤4 GB): the burstable t3 family is the
    // right cost/perf point. Anything larger steps into c6i, climbing
    // within the family before considering a cross-family swap.
    if req.vcpus <= 2 && req.memory_gb <= 4 {
        return "t3.medium".to_string();
    }
    if let Some(pick) = pick_in_family(req, C6I_FAMILY) {
        return pick;
    }
    // Compute family exhausted — cross to memory-optimized so a
    // moderate-vCPU but high-memory tail still picks correctly.
    if let Some(pick) = pick_in_family(req, R6I_FAMILY) {
        return pick;
    }
    "r6i.16xlarge".to_string()
}

/// P2-168 — instance shapes within a family, ordered smallest →
/// largest. The picker walks the slice and returns the first shape
/// whose (vcpus, memory_gb) both meet the request. `None` signals
/// "no shape in this family can satisfy"; caller decides whether
/// to swap families or accept the ceiling.
const C6I_FAMILY: &[&str] = &[
    "c6i.xlarge",
    "c6i.2xlarge",
    "c6i.4xlarge",
    "c6i.8xlarge",
    "c6i.12xlarge",
];
const R6I_FAMILY: &[&str] = &[
    "r6i.xlarge",
    "r6i.2xlarge",
    "r6i.4xlarge",
    "r6i.8xlarge",
    "r6i.16xlarge",
];

fn pick_in_family(req: &ResourceRequirements, family: &[&str]) -> Option<String> {
    for name in family {
        let (vcpus, memory_gb) = instance_capacity(name);
        if vcpus >= req.vcpus && memory_gb >= req.memory_gb {
            return Some((*name).to_string());
        }
    }
    None
}

/// P2-167 — GPU picker keyed on `gpu.count` first, then refined by
/// `gpu.kind` when the operator pinned an architecture. Ladder:
///
///   1 GPU  → g4dn.xlarge (1× T4) or g6.xlarge (1× L4) by kind
///   4 GPUs → g4dn.12xlarge (4× T4) — closest fixed-shape for T4-class
///   8 GPUs → p4d.24xlarge (8× A100) — covers nvidia-a100 too
///
/// Counts that don't match a fixed shape round UP to the next
/// available size and emit a `swfc::sizing_undersized` warn so the
/// operator can decide whether to override. Falling back at the
/// kind level when AWS has no exact-count shape preserves the
/// "give the agent enough capacity" contract (never under-provision)
/// at the cost of a small over-provision when e.g. asked for 2 GPUs.
fn pick_gpu_instance_type(gpu: &GpuRequirement) -> String {
    let kind = gpu.kind.as_str();
    let count = gpu.count;
    // A100 family: only p4d.24xlarge exists as an 8× shape AWS lists
    // in our allowlisted regions. Single-A100 hosts (p3.2xlarge,
    // older arch) aren't part of the harness's allowlist.
    if kind == "nvidia-a100" {
        return "p4d.24xlarge".to_string();
    }
    // T4 family: step by count.
    if kind == "nvidia-t4" {
        return if count >= 8 {
            // Round up to A100 8× — T4 8-pack doesn't exist in
            // a single shape in our allowlisted regions.
            tracing::warn!(
                target: "swfc::sizing_undersized",
                requested_gpu_kind = "nvidia-t4",
                requested_gpu_count = count,
                picked = "p4d.24xlarge",
                "no fixed T4 shape with 8× GPUs; rounding up to A100"
            );
            "p4d.24xlarge".to_string()
        } else if count >= 4 {
            "g4dn.12xlarge".to_string()
        } else {
            "g4dn.xlarge".to_string()
        };
    }
    // L4 family: only g6.xlarge (1× L4) is in our table; counts >1
    // get a warn + fall back to g6.xlarge.
    if kind == "nvidia-l4" {
        if count > 1 {
            tracing::warn!(
                target: "swfc::sizing_undersized",
                requested_gpu_kind = "nvidia-l4",
                requested_gpu_count = count,
                picked = "g6.xlarge",
                "no fixed L4 shape with >1 GPU in capacity table; \
                 widen the sizing table or pin a different gpu kind"
            );
        }
        return "g6.xlarge".to_string();
    }
    // Unknown kind: step purely by count.
    if count >= 8 {
        "p4d.24xlarge".to_string()
    } else if count >= 4 {
        "g4dn.12xlarge".to_string()
    } else {
        "g4dn.xlarge".to_string()
    }
}

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
    // `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
    // edition because the env table is not thread-safe). All call sites
    // are single-threaded test setup/teardown; the bounded waiver is
    // scoped to this `mod tests` block.
    #![allow(unsafe_code)]
    use super::*;

    fn load_real_profiles() -> ComputeProfiles {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config/compute-profiles/profiles.yaml");
        ComputeProfiles::load(&path).expect("real profiles must load")
    }

    #[test]
    fn real_profiles_yaml_loads() {
        let p = load_real_profiles();
        assert!(p.profiles.contains_key("alignment_quantification"));
        assert!(p.profiles.contains_key("clustering"));
        assert!(!p.method_overrides.is_empty());
    }

    #[test]
    fn unknown_stage_falls_back_to_default() {
        let p = load_real_profiles();
        let req = compute_high_water(
            &p,
            "entirely_made_up_class",
            &SizingIntakeFacts::default(),
            &[],
        )
        .expect("default profile has Some requirements");
        // Default is 2/8/50 in the real YAML.
        assert_eq!(req.vcpus, 2);
        assert_eq!(req.memory_gb, 8);
        assert_eq!(req.storage_gb, 50);
    }

    #[test]
    fn alignment_quantification_bumps_memory_for_human_genome() {
        let p = load_real_profiles();
        let facts = SizingIntakeFacts {
            genome_size_gb: Some(3.10),
            ..SizingIntakeFacts::default()
        };
        let req = compute_high_water(&p, "alignment_quantification", &facts, &[]).unwrap();
        assert_eq!(
            req.memory_gb, 128,
            "human genome > 3 GB triggers 128 GB bump"
        );
    }

    #[test]
    fn alignment_quantification_stays_base_for_small_genome() {
        let p = load_real_profiles();
        let facts = SizingIntakeFacts {
            genome_size_gb: Some(0.14),
            ..SizingIntakeFacts::default()
        };
        let req = compute_high_water(&p, "alignment_quantification", &facts, &[]).unwrap();
        assert_eq!(
            req.memory_gb, 64,
            "sub-threshold genome keeps the base 64 GB"
        );
    }

    #[test]
    fn sample_count_scales_storage_additively() {
        let p = load_real_profiles();
        let facts = SizingIntakeFacts {
            sample_count: Some(20),
            ..SizingIntakeFacts::default()
        };
        let req = compute_high_water(&p, "data_acquisition", &facts, &[]).unwrap();
        // base 100 + 20 samples * 10 GB/sample = 300.
        assert_eq!(req.storage_gb, 300);
    }

    #[test]
    fn clustering_scales_memory_for_big_cell_counts() {
        let p = load_real_profiles();
        let facts = SizingIntakeFacts {
            cell_count: Some(800_000),
            ..SizingIntakeFacts::default()
        };
        let req = compute_high_water(&p, "clustering", &facts, &[]).unwrap();
        assert_eq!(req.memory_gb, 128);
    }

    #[test]
    fn deepvariant_method_upgrades_to_gpu_for_variant_calling() {
        let p = load_real_profiles();
        let facts = SizingIntakeFacts::default();
        let req = compute_high_water(&p, "variant_calling", &facts, &["deepvariant".into()])
            .expect("variant_calling profile exists");
        let gpu = req.gpu.expect("deepvariant triggers GPU");
        assert_eq!(gpu.kind, "nvidia-l4");
        assert_eq!(gpu.count, 1);
    }

    #[test]
    fn deepvariant_method_on_unrelated_stage_does_not_upgrade() {
        let p = load_real_profiles();
        let facts = SizingIntakeFacts::default();
        let req =
            compute_high_water(&p, "data_acquisition", &facts, &["deepvariant".into()]).unwrap();
        // deepvariant applies_to: [variant_calling], so data_acquisition is untouched.
        assert!(req.gpu.is_none());
    }

    #[test]
    fn star_2pass_bumps_memory_on_alignment_stage() {
        let p = load_real_profiles();
        let facts = SizingIntakeFacts::default();
        let req = compute_high_water(
            &p,
            "alignment_quantification",
            &facts,
            &["star-2pass".into()],
        )
        .unwrap();
        assert!(req.memory_gb >= 64);
    }

    #[test]
    fn review_stage_returns_none() {
        let p = load_real_profiles();
        let req = compute_high_water(&p, "review", &SizingIntakeFacts::default(), &[]);
        assert!(req.is_none(), "review is human-only — no compute");
    }

    #[test]
    fn resolve_instance_type_picks_gpu_for_gpu_requirement() {
        let req = ResourceRequirements {
            vcpus: 8,
            memory_gb: 64,
            storage_gb: 200,
            gpu: Some(GpuRequirement {
                kind: "nvidia-a100".into(),
                count: 1,
            }),
        };
        assert_eq!(resolve_instance_type(&req), "p4d.24xlarge");
    }

    #[test]
    fn resolve_instance_type_picks_memory_optimized_for_big_ram() {
        // P2-168 — the family-first resolver picks the smallest r6i
        // shape that satisfies BOTH vcpus and memory_gb. 128 GB ≤
        // r6i.4xlarge's 128 GB and 8 vCPUs ≤ r6i.4xlarge's 16 vCPUs,
        // so this fits exactly without over-provisioning to
        // r6i.8xlarge. Prior code unconditionally jumped to
        // r6i.8xlarge for any memory ≥ 128 GB — a 2× cost
        // multiplier the picker now avoids.
        let req = ResourceRequirements {
            vcpus: 8,
            memory_gb: 128,
            storage_gb: 200,
            gpu: None,
        };
        assert_eq!(resolve_instance_type(&req), "r6i.4xlarge");
    }

    #[test]
    fn resolve_instance_type_picks_burstable_for_tiny_load() {
        let req = ResourceRequirements {
            vcpus: 2,
            memory_gb: 4,
            storage_gb: 50,
            gpu: None,
        };
        assert_eq!(resolve_instance_type(&req), "t3.medium");
    }

    /// P2-166 — high-vCPU CPU-bound requests step into the larger
    /// c6i shapes instead of saturating at c6i.4xlarge.
    #[test]
    fn resolve_instance_type_climbs_to_c6i_8xlarge_for_32_vcpu() {
        let req = ResourceRequirements {
            vcpus: 32,
            memory_gb: 16,
            storage_gb: 50,
            gpu: None,
        };
        assert_eq!(resolve_instance_type(&req), "c6i.8xlarge");
    }

    #[test]
    fn resolve_instance_type_climbs_to_c6i_12xlarge_for_48_vcpu() {
        let req = ResourceRequirements {
            vcpus: 48,
            memory_gb: 16,
            storage_gb: 50,
            gpu: None,
        };
        assert_eq!(resolve_instance_type(&req), "c6i.12xlarge");
    }

    /// P2-166 — high-memory requests climb past r6i.8xlarge.
    #[test]
    fn resolve_instance_type_climbs_to_r6i_16xlarge_for_512gb() {
        let req = ResourceRequirements {
            vcpus: 32,
            memory_gb: 384,
            storage_gb: 1000,
            gpu: None,
        };
        assert_eq!(resolve_instance_type(&req), "r6i.16xlarge");
    }

    /// P2-166 — `instance_capacity` round-trips for every entry the
    /// resolver can emit.
    #[test]
    fn instance_capacity_table_covers_every_resolver_pick() {
        // Every distinct resolver output must have a non-zero
        // capacity row. New picker arms that forget to register
        // their type are caught here.
        let picks = [
            "t3.medium",
            "t3.large",
            "c6i.xlarge",
            "c6i.2xlarge",
            "c6i.4xlarge",
            "c6i.8xlarge",
            "c6i.12xlarge",
            "r6i.xlarge",
            "r6i.2xlarge",
            "r6i.4xlarge",
            "r6i.8xlarge",
            "r6i.16xlarge",
            "g4dn.xlarge",
            "g4dn.12xlarge",
            "g6.xlarge",
            "p4d.24xlarge",
        ];
        for p in picks {
            let (v, m) = instance_capacity(p);
            assert!(v > 0, "missing vcpu entry for {p}");
            assert!(m > 0, "missing memory entry for {p}");
        }
    }

    /// P2-167 — GPU picker steps by count, not just by kind.
    #[test]
    fn gpu_picker_single_t4_uses_g4dn_xlarge() {
        let req = ResourceRequirements {
            vcpus: 4,
            memory_gb: 16,
            storage_gb: 100,
            gpu: Some(GpuRequirement {
                kind: "nvidia-t4".into(),
                count: 1,
            }),
        };
        assert_eq!(resolve_instance_type(&req), "g4dn.xlarge");
    }

    #[test]
    fn gpu_picker_four_t4s_uses_g4dn_12xlarge() {
        let req = ResourceRequirements {
            vcpus: 48,
            memory_gb: 192,
            storage_gb: 500,
            gpu: Some(GpuRequirement {
                kind: "nvidia-t4".into(),
                count: 4,
            }),
        };
        // Should pick g4dn.12xlarge (4× T4) instead of g4dn.xlarge (1× T4).
        assert_eq!(resolve_instance_type(&req), "g4dn.12xlarge");
    }

    #[test]
    fn gpu_picker_eight_gpus_uses_p4d_24xlarge() {
        let req = ResourceRequirements {
            vcpus: 96,
            memory_gb: 1100,
            storage_gb: 1000,
            gpu: Some(GpuRequirement {
                kind: "nvidia-a100".into(),
                count: 8,
            }),
        };
        assert_eq!(resolve_instance_type(&req), "p4d.24xlarge");
    }

    #[test]
    fn gpu_picker_a100_routes_to_p4d_regardless_of_count() {
        for count in [1, 2, 4, 8] {
            let req = ResourceRequirements {
                vcpus: 96,
                memory_gb: 1100,
                storage_gb: 1000,
                gpu: Some(GpuRequirement {
                    kind: "nvidia-a100".into(),
                    count,
                }),
            };
            assert_eq!(
                resolve_instance_type(&req),
                "p4d.24xlarge",
                "A100 must always land on p4d.24xlarge (count={count})"
            );
        }
    }

    #[test]
    fn gpu_picker_unknown_kind_steps_by_count() {
        // Future GPU kinds the table doesn't enumerate fall through
        // to the count-based ladder so the request still picks a
        // sized shape (not the smallest fallback).
        let req = ResourceRequirements {
            vcpus: 48,
            memory_gb: 192,
            storage_gb: 500,
            gpu: Some(GpuRequirement {
                kind: "nvidia-future-gpu".into(),
                count: 4,
            }),
        };
        assert_eq!(resolve_instance_type(&req), "g4dn.12xlarge");
    }

    #[test]
    fn merge_resource_requirements_preserves_gpu_and_maxes_floors() {
        let base = ResourceRequirements {
            vcpus: 8,
            memory_gb: 32,
            storage_gb: 200,
            gpu: Some(GpuRequirement {
                kind: "nvidia-l4".into(),
                count: 1,
            }),
        };
        let projected = ResourceRequirements {
            vcpus: 4,
            memory_gb: 96,
            storage_gb: 100,
            gpu: None,
        };
        let merged = merge_resource_requirements_max(&base, &projected);
        assert_eq!(merged.vcpus, 8);
        assert_eq!(merged.memory_gb, 96);
        assert_eq!(merged.storage_gb, 200);
        let gpu = merged.gpu.unwrap();
        assert_eq!(gpu.kind, "nvidia-l4");
        assert_eq!(gpu.count, 1);
    }

    #[test]
    fn end_to_end_intake_facts_to_sizing_wires_scaling_fields() {
        // Drive the full pipeline: build IntakeFacts via
        // with_scaling_from_map using the exact canonical keys the UI
        // structured-capture card emits, then feed the four scaling
        // fields into the sizing layer and assert the high-water
        // calculation produces the expected shape.
        use ecaa_workflow_core::classify::{ClassificationResult, OrganismInfo};
        use ecaa_workflow_core::intake_facts::IntakeFacts;
        use std::collections::BTreeMap;

        let clf = ClassificationResult {
            modality: "single_cell_rnaseq".into(),
            taxonomy_path: String::new(),
            domain: String::new(),
            workflow_description: String::new(),
            edam_topic: String::new(),
            edam_operation: String::new(),
            confidence: 0.8,
            confidence_label: "high".into(),
            organisms: vec![OrganismInfo {
                name: "Homo sapiens".into(),
                taxon_id: 9606,
            }],
            methods_specified: vec![],
            data_sources: vec![],
            intake_text: String::new(),
            goal: None,
            archetype_id: None,
            additional_modalities: vec![],
            tie_candidates: vec![],
        };
        let mut captured = BTreeMap::new();
        captured.insert("sample_count".into(), "12".into());
        captured.insert("cell_count".into(), "800000".into());
        let facts = IntakeFacts::from_classification_with_scaling(&clf, &captured);

        // Verify the facts carried through before sizing.
        assert_eq!(facts.sample_count, Some(12));
        assert_eq!(facts.cell_count, Some(800_000));
        assert_eq!(facts.organism_taxon_id, Some(9606));

        // Translate to the sizing layer's facts shape. The real code
        // path is: IntakeFacts → organism_sizes.yaml lookup →
        // SizingIntakeFacts. For this test we translate directly.
        let sizing_facts = SizingIntakeFacts {
            sample_count: facts.sample_count,
            coverage_depth: facts.coverage_depth,
            cell_count: facts.cell_count,
            database_size_gb: facts.database_size_gb,
            genome_size_gb: Some(3.10), // human, matches taxon_id 9606
        };

        let profiles = load_real_profiles();

        // Clustering: 800k cells > 500k threshold → 128 GB bump.
        let cluster = compute_high_water(&profiles, "clustering", &sizing_facts, &[]).unwrap();
        assert_eq!(cluster.memory_gb, 128);

        // Data acquisition: 12 samples × 10 GB/sample + 100 GB base = 220.
        let dl = compute_high_water(&profiles, "data_acquisition", &sizing_facts, &[]).unwrap();
        assert_eq!(dl.storage_gb, 220);

        // Alignment: human genome > 3 GB → 128 GB bump; 12 samples × 20 GB
        // storage = 240 on top of base 200 = 440.
        let aln =
            compute_high_water(&profiles, "alignment_quantification", &sizing_facts, &[]).unwrap();
        assert_eq!(aln.memory_gb, 128);
        assert_eq!(aln.storage_gb, 200 + 12 * 20);
    }

    #[test]
    fn ssm_timeout_falls_back_to_default() {
        // Serialize via the shared env lock so parallel tests don't
        // race on SWFC_AWS_SSM_TIMEOUT_SECS.
        let _lock = super::super::SWFC_AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let profiles = load_real_profiles();
        // Temporarily clear the env var so the fallback path runs.
        let prior = std::env::var("SWFC_AWS_SSM_TIMEOUT_SECS").ok();
        unsafe { std::env::remove_var("SWFC_AWS_SSM_TIMEOUT_SECS") };
        let timeout = resolve_ssm_timeout_secs(&profiles, "clustering");
        if let Some(v) = prior {
            unsafe { std::env::set_var("SWFC_AWS_SSM_TIMEOUT_SECS", v) };
        }
        // No per-stage override + no env var = 3600.
        assert_eq!(timeout, 3600);
    }

    #[test]
    fn ssm_timeout_honors_env_override() {
        let _lock = super::super::SWFC_AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let profiles = load_real_profiles();
        let prior = std::env::var("SWFC_AWS_SSM_TIMEOUT_SECS").ok();
        unsafe { std::env::set_var("SWFC_AWS_SSM_TIMEOUT_SECS", "7200") };
        let timeout = resolve_ssm_timeout_secs(&profiles, "clustering");
        match prior {
            Some(v) => unsafe { std::env::set_var("SWFC_AWS_SSM_TIMEOUT_SECS", v) },
            None => unsafe { std::env::remove_var("SWFC_AWS_SSM_TIMEOUT_SECS") },
        }
        assert_eq!(timeout, 7200);
    }

    #[test]
    fn ssm_timeout_per_stage_override_wins_over_env() {
        let _lock = super::super::SWFC_AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // Build a profiles map with one stage that carries an override.
        let yaml = r#"
profiles:
  variant_calling:
    description: "long-running"
    requirements: { vcpus: 8, memory_gb: 64, storage_gb: 300 }
    ssm_timeout_secs: 14400
  other:
    description: "short"
    requirements: { vcpus: 2, memory_gb: 8, storage_gb: 50 }
default:
  description: "fallback"
  requirements: { vcpus: 2, memory_gb: 8, storage_gb: 50 }
"#;
        let profiles: ComputeProfiles = serde_yml::from_str(yaml).unwrap();

        let prior = std::env::var("SWFC_AWS_SSM_TIMEOUT_SECS").ok();
        unsafe { std::env::set_var("SWFC_AWS_SSM_TIMEOUT_SECS", "1800") };

        // variant_calling has a per-stage override → wins over env.
        assert_eq!(
            resolve_ssm_timeout_secs(&profiles, "variant_calling"),
            14400
        );
        // other has no override → env wins.
        assert_eq!(resolve_ssm_timeout_secs(&profiles, "other"), 1800);

        match prior {
            Some(v) => unsafe { std::env::set_var("SWFC_AWS_SSM_TIMEOUT_SECS", v) },
            None => unsafe { std::env::remove_var("SWFC_AWS_SSM_TIMEOUT_SECS") },
        }
    }

    #[test]
    fn ssm_timeout_ignores_malformed_env_var() {
        let _lock = super::super::SWFC_AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let profiles = load_real_profiles();
        let prior = std::env::var("SWFC_AWS_SSM_TIMEOUT_SECS").ok();
        unsafe { std::env::set_var("SWFC_AWS_SSM_TIMEOUT_SECS", "not-a-number") };
        let timeout = resolve_ssm_timeout_secs(&profiles, "clustering");
        match prior {
            Some(v) => unsafe { std::env::set_var("SWFC_AWS_SSM_TIMEOUT_SECS", v) },
            None => unsafe { std::env::remove_var("SWFC_AWS_SSM_TIMEOUT_SECS") },
        }
        assert_eq!(timeout, 3600);
    }

    #[test]
    fn combined_scaling_and_method_override() {
        // Worked example from aws-remote-compute-design-plan §8.2.3-ish:
        // alignment_quantification + human genome (memory 128 via scaling)
        // + star-2pass (memory 64 via method override) + 8 samples (storage
        // 200 + 8*20 = 360). Method override shouldn't lower the scaling
        // bump; storage should stack.
        let p = load_real_profiles();
        let facts = SizingIntakeFacts {
            sample_count: Some(8),
            genome_size_gb: Some(3.10),
            ..SizingIntakeFacts::default()
        };
        let req = compute_high_water(
            &p,
            "alignment_quantification",
            &facts,
            &["star-2pass".into()],
        )
        .unwrap();
        assert_eq!(
            req.memory_gb, 128,
            "genome bump wins over method 64 GB hint"
        );
        assert_eq!(req.storage_gb, 200 + 8 * 20);
    }
}
