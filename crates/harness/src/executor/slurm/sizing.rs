//! Load + validate `slurm-mapping.yaml` and resolve
//! `ResourceRequirements` to a `ResourceClass`. Site-specific file —
//! each cluster ships its own. Validated at load against
//! `slurm-mapping.schema.json` per the schema-sidecar convention,
//! so shape drift fails loud at executor startup rather than on a compute node.

use super::super::ResourceRequirements;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// One SLURM resource class. Maps onto `#SBATCH` directive flags
/// directly; `sbatch.rs::SbatchSpec` consumes this 1:1.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourceClass {
    pub partition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qos: Option<String>,
    pub cpus_per_task: u32,
    /// Memory in SLURM's native `--mem=` syntax. Numeric part is
    /// parsed by `parse_mem_gb` so the resolver can compare
    /// requirements in GB.
    pub mem: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gres: Option<String>,
    pub time: String,
}

/// Per-partition container-runtime allow-list.
/// `slurm/sbatch.rs::validate_submission` rejects with
/// `BlockerKind::SlurmRuntimeUnavailable` when an atom's
/// `preferred_container` requires a runtime not on the partition's
/// list. Optional at the YAML level — when omitted, the validator
/// falls back to the agent script's auto-detect chain (S15.4).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct PartitionPolicy {
    /// Allow-list of container runtimes. `none` means host execution
    /// allowed (no container required); `apptainer-1.4`, `singularity-3.x`,
    /// `podman`, `docker` follow the agent-claude-slurm.sh probe order.
    pub container_runtimes: Vec<String>,
}

/// Top-level structure of `slurm-mapping.yaml`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SlurmMapping {
    pub version: u32,
    /// Named classes keyed by arbitrary identifier (e.g. `cpu_small`,
    /// `gpu_large`). `BTreeMap` for deterministic iteration order so
    /// tie-breaking during resolution is stable.
    pub resource_classes: BTreeMap<String, ResourceClass>,
    pub fallback: ResourceClass,
    /// Per-partition container-runtime allow-list.
    /// Keys are partition names referenced by `resource_classes[*].partition`;
    /// values declare which container runtimes the partition supports.
    /// Optional — when omitted, the validator falls back to the agent
    /// script's auto-detect chain.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub partitions: BTreeMap<String, PartitionPolicy>,
}

impl SlurmMapping {
    /// Read the YAML file, validate its shape against the schema, and
    /// return the parsed mapping.
    ///
    /// `yaml_path` points at the `.yaml`; `schema_path` at the sidecar
    /// `.schema.json`. Both paths are required (`None` on the schema
    /// path disables validation — useful in tests but not in prod;
    /// the executor's `from_env` constructor wires up both).
    pub fn load(yaml_path: &Path, schema_path: Option<&Path>) -> Result<Self> {
        let as_json = ecaa_workflow_core::fs_helpers::read_yaml_as_json(yaml_path)?;

        if let Some(schema_path) = schema_path {
            validate_against_schema(schema_path, yaml_path, &as_json)?;
        }

        // Serde's deny_unknown_fields + required-fields checks give a
        // second layer of validation even when the schema passes.
        serde_json::from_value(as_json)
            .with_context(|| format!("deserializing {} into SlurmMapping", yaml_path.display()))
    }

    /// Pick the smallest `ResourceClass` that satisfies `req`. If no
    /// class matches, returns `self.fallback`.
    ///
    /// "Smallest" = fewest vCPUs, then lowest memory. The BTreeMap
    /// iteration order makes this deterministic across runs — two
    /// identical requirements always resolve to the same class.
    pub fn pick(&self, req: &ResourceRequirements) -> &ResourceClass {
        self.pick_detailed(req).0
    }

    /// Like `pick` but also reports whether the result is the mapping's
    /// `fallback` class. Callers that want to apply the env-var
    /// `ECAA_SLURM_DEFAULT_TIME_LIMIT` ("fallback `--time=` when
    /// sizing mapping is silent") use the boolean to decide.
    pub fn pick_detailed(&self, req: &ResourceRequirements) -> (&ResourceClass, bool) {
        let mut candidates: Vec<&ResourceClass> = self
            .resource_classes
            .values()
            .filter(|c| class_satisfies(c, req))
            .collect();
        candidates.sort_by_key(|c| (c.cpus_per_task, parse_mem_gb(&c.mem).unwrap_or(u32::MAX)));
        match candidates.first().copied() {
            Some(c) => (c, false),
            None => (&self.fallback, true),
        }
    }
}

fn validate_against_schema(
    schema_path: &Path,
    yaml_path: &Path,
    as_json: &serde_json::Value,
) -> Result<()> {
    let compiled =
        ecaa_workflow_core::schema_helpers::compile_schema_from_path_cached(schema_path)?;
    let messages: Vec<String> = match compiled.validate(as_json) {
        Ok(()) => return Ok(()),
        Err(errs) => errs
            .map(|e| format!("  at {}: {}", e.instance_path, e))
            .collect(),
    };
    Err(anyhow!(
        "{} failed schema validation ({} violation(s)):\n{}",
        yaml_path.display(),
        messages.len(),
        messages.join("\n")
    ))
}

/// Parse SLURM's `--mem=` value to GB. `16G` → 16, `64000M` → 62
/// (rounded down), `1T` → 1024. Returns `None` on unparseable input.
pub fn parse_mem_gb(mem: &str) -> Option<u32> {
    let trimmed = mem.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (num_part, multiplier_gb): (&str, u32) = if let Some(n) = trimmed.strip_suffix('T') {
        (n, 1024)
    } else if let Some(n) = trimmed.strip_suffix('G') {
        (n, 1)
    } else if let Some(n) = trimmed.strip_suffix('M') {
        // Convert to GB by integer division. 500M → 0 GB, which is
        // correct for "does not satisfy 1 GB+ requirement".
        return n.parse::<u32>().ok().map(|mb| mb / 1024);
    } else if let Some(n) = trimmed.strip_suffix('K') {
        return n.parse::<u32>().ok().map(|kb| kb / (1024 * 1024));
    } else {
        // Bare integer = MB per SLURM convention.
        return trimmed.parse::<u32>().ok().map(|mb| mb / 1024);
    };
    num_part.parse::<u32>().ok().map(|n| n * multiplier_gb)
}

/// Does `class` meet `req`? Compares cpus, memory, and gpu presence.
/// GPU kind/count matching is a soft check: if `req.gpu` is `Some`,
/// the class must declare a `gres` string; we don't try to parse the
/// gres-count subfield because site-specific gres names drift.
fn class_satisfies(class: &ResourceClass, req: &ResourceRequirements) -> bool {
    if class.cpus_per_task < req.vcpus {
        return false;
    }
    match parse_mem_gb(&class.mem) {
        Some(mem_gb) if mem_gb >= req.memory_gb => {}
        _ => return false,
    }
    if req.gpu.is_some() && class.gres.is_none() {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::super::super::GpuRequirement;
    use super::*;
    use std::fs;

    fn write_temp_yaml(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("slurm-mapping.yaml");
        fs::write(&path, contents).unwrap();
        (dir, path)
    }

    fn schema_path() -> std::path::PathBuf {
        // Walk up from CARGO_MANIFEST_DIR (crate root) to the repo
        // root, then into config/compute-profiles/.
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest
            .parent() // crates/
            .unwrap()
            .parent() // repo root
            .unwrap()
            .join("config/compute-profiles/slurm-mapping.schema.json")
    }

    #[test]
    fn parse_mem_gb_accepts_g_t_m_k_and_bare_integers() {
        assert_eq!(parse_mem_gb("16G"), Some(16));
        assert_eq!(parse_mem_gb("1T"), Some(1024));
        assert_eq!(parse_mem_gb("64000M"), Some(62)); // 64000 / 1024
        assert_eq!(parse_mem_gb("2097152K"), Some(2)); // 2 GB
        assert_eq!(parse_mem_gb("8192"), Some(8)); // bare = MB → 8 GB
    }

    #[test]
    fn parse_mem_gb_rejects_garbage() {
        assert_eq!(parse_mem_gb(""), None);
        assert_eq!(parse_mem_gb("G"), None);
        assert_eq!(parse_mem_gb("abc"), None);
        assert_eq!(parse_mem_gb("16X"), None);
    }

    #[test]
    fn class_satisfies_checks_cpus_and_mem() {
        let class = ResourceClass {
            partition: "p".into(),
            qos: None,
            cpus_per_task: 8,
            mem: "32G".into(),
            gres: None,
            time: "02:00:00".into(),
        };
        // cpus ok, mem ok, no gpu required → satisfied.
        let req = ResourceRequirements {
            vcpus: 4,
            memory_gb: 16,
            storage_gb: 0,
            gpu: None,
        };
        assert!(class_satisfies(&class, &req));
        // cpus insufficient
        let req = ResourceRequirements {
            vcpus: 16,
            memory_gb: 16,
            storage_gb: 0,
            gpu: None,
        };
        assert!(!class_satisfies(&class, &req));
        // mem insufficient
        let req = ResourceRequirements {
            vcpus: 4,
            memory_gb: 64,
            storage_gb: 0,
            gpu: None,
        };
        assert!(!class_satisfies(&class, &req));
    }

    #[test]
    fn class_satisfies_requires_gres_when_req_has_gpu() {
        let req_with_gpu = ResourceRequirements {
            vcpus: 2,
            memory_gb: 4,
            storage_gb: 0,
            gpu: Some(GpuRequirement {
                kind: "nvidia-a100".into(),
                count: 1,
            }),
        };
        let cpu_only = ResourceClass {
            partition: "p".into(),
            qos: None,
            cpus_per_task: 8,
            mem: "32G".into(),
            gres: None,
            time: "02:00:00".into(),
        };
        assert!(!class_satisfies(&cpu_only, &req_with_gpu));
        let gpu_class = ResourceClass {
            partition: "gpu".into(),
            qos: None,
            cpus_per_task: 8,
            mem: "32G".into(),
            gres: Some("gpu:a100:1".into()),
            time: "02:00:00".into(),
        };
        assert!(class_satisfies(&gpu_class, &req_with_gpu));
    }

    #[test]
    fn load_accepts_valid_yaml_and_validates_against_schema() {
        let yaml = r#"
version: 1
resource_classes:
  cpu_small:
    partition: short
    cpus_per_task: 4
    mem: 16G
    time: "02:00:00"
fallback:
  partition: normal
  cpus_per_task: 2
  mem: 8G
  time: "01:00:00"
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let mapping = SlurmMapping::load(&path, Some(&schema_path())).expect("valid yaml");
        assert_eq!(mapping.version, 1);
        assert_eq!(mapping.resource_classes.len(), 1);
        assert_eq!(mapping.fallback.cpus_per_task, 2);
    }

    #[test]
    fn load_rejects_unknown_fields_via_serde() {
        let yaml = r#"
version: 1
resource_classes:
  cpu_small:
    partition: short
    cpus_per_task: 4
    mem: 16G
    time: "02:00:00"
    unexpected_field: oops
fallback:
  partition: normal
  cpus_per_task: 2
  mem: 8G
  time: "01:00:00"
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let err = SlurmMapping::load(&path, Some(&schema_path())).unwrap_err();
        let msg = err.to_string() + &err.chain().map(|e| e.to_string()).collect::<String>();
        // Either the schema (additionalProperties:false) or serde
        // (deny_unknown_fields) catches this.
        assert!(
            msg.contains("unexpected_field")
                || msg.contains("additionalProperties")
                || msg.contains("Additional properties")
                || msg.contains("unknown field"),
            "got: {msg}"
        );
    }

    #[test]
    fn load_rejects_missing_required_fields() {
        // Omit `partition` from fallback — required per schema + serde.
        let yaml = r#"
version: 1
resource_classes:
  cpu_small:
    partition: short
    cpus_per_task: 4
    mem: 16G
    time: "02:00:00"
fallback:
  cpus_per_task: 2
  mem: 8G
  time: "01:00:00"
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let err = SlurmMapping::load(&path, Some(&schema_path())).unwrap_err();
        let msg = err.to_string() + &err.chain().map(|e| e.to_string()).collect::<String>();
        assert!(
            msg.contains("partition") || msg.contains("required"),
            "got: {msg}"
        );
    }

    #[test]
    fn load_rejects_wrong_version() {
        let yaml = r#"
version: 2
resource_classes:
  cpu_small:
    partition: short
    cpus_per_task: 4
    mem: 16G
    time: "02:00:00"
fallback:
  partition: normal
  cpus_per_task: 2
  mem: 8G
  time: "01:00:00"
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let err = SlurmMapping::load(&path, Some(&schema_path())).unwrap_err();
        let msg = err.to_string() + &err.chain().map(|e| e.to_string()).collect::<String>();
        // Schema pins version: 1.
        assert!(msg.contains("version") || msg.contains("1"), "got: {msg}");
    }

    #[test]
    fn pick_returns_smallest_satisfying_class() {
        let yaml = r#"
version: 1
resource_classes:
  tiny:
    partition: short
    cpus_per_task: 2
    mem: 4G
    time: "01:00:00"
  small:
    partition: short
    cpus_per_task: 4
    mem: 16G
    time: "02:00:00"
  big:
    partition: long
    cpus_per_task: 48
    mem: 256G
    time: "24:00:00"
fallback:
  partition: normal
  cpus_per_task: 1
  mem: 1G
  time: "00:30:00"
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let mapping = SlurmMapping::load(&path, Some(&schema_path())).unwrap();

        // Needs 3 vCPU, 8 GB → tiny is too small, small is smallest fit.
        let req = ResourceRequirements {
            vcpus: 3,
            memory_gb: 8,
            storage_gb: 0,
            gpu: None,
        };
        let pick = mapping.pick(&req);
        assert_eq!(pick.cpus_per_task, 4);
        assert_eq!(pick.mem, "16G");
    }

    #[test]
    fn pick_falls_back_when_nothing_satisfies() {
        let yaml = r#"
version: 1
resource_classes:
  tiny:
    partition: short
    cpus_per_task: 2
    mem: 4G
    time: "01:00:00"
fallback:
  partition: huge
  cpus_per_task: 128
  mem: 512G
  time: "48:00:00"
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let mapping = SlurmMapping::load(&path, Some(&schema_path())).unwrap();
        let req = ResourceRequirements {
            vcpus: 64,
            memory_gb: 128,
            storage_gb: 0,
            gpu: None,
        };
        let pick = mapping.pick(&req);
        // Must be fallback — no other class satisfies the requirement.
        assert_eq!(pick.partition, "huge");
        assert_eq!(pick.cpus_per_task, 128);
    }

    #[test]
    fn pick_resolves_same_input_to_same_class_deterministically() {
        // Two entries that both satisfy, same cpu count, same mem.
        // BTreeMap ordering means the result is stable.
        let yaml = r#"
version: 1
resource_classes:
  alpha:
    partition: alpha-p
    cpus_per_task: 4
    mem: 16G
    time: "02:00:00"
  beta:
    partition: beta-p
    cpus_per_task: 4
    mem: 16G
    time: "02:00:00"
fallback:
  partition: normal
  cpus_per_task: 1
  mem: 1G
  time: "00:30:00"
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let mapping = SlurmMapping::load(&path, Some(&schema_path())).unwrap();
        let req = ResourceRequirements {
            vcpus: 2,
            memory_gb: 8,
            storage_gb: 0,
            gpu: None,
        };
        let pick1 = mapping.pick(&req).clone();
        let pick2 = mapping.pick(&req).clone();
        assert_eq!(pick1, pick2);
        // Alphabetical order → "alpha" wins the tie.
        assert_eq!(pick1.partition, "alpha-p");
    }

    #[test]
    fn pick_chooses_gpu_class_when_gpu_required() {
        let yaml = r#"
version: 1
resource_classes:
  cpu_only:
    partition: normal
    cpus_per_task: 8
    mem: 32G
    time: "04:00:00"
  gpu:
    partition: gpu
    gres: "gpu:1"
    cpus_per_task: 8
    mem: 32G
    time: "04:00:00"
fallback:
  partition: normal
  cpus_per_task: 1
  mem: 1G
  time: "00:30:00"
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let mapping = SlurmMapping::load(&path, Some(&schema_path())).unwrap();
        let req = ResourceRequirements {
            vcpus: 2,
            memory_gb: 4,
            storage_gb: 0,
            gpu: Some(GpuRequirement {
                kind: "nvidia".into(),
                count: 1,
            }),
        };
        let pick = mapping.pick(&req);
        assert_eq!(pick.partition, "gpu");
        assert!(pick.gres.is_some());
    }

    #[test]
    fn shipped_config_file_loads_and_validates() {
        // The repo's config/compute-profiles/slurm-mapping.yaml must
        // itself pass schema validation. Catches the shape drifting in
        // the generic template before it hits production clusters.
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest.parent().unwrap().parent().unwrap();
        let yaml = repo_root.join("config/compute-profiles/slurm-mapping.yaml");
        if !yaml.exists() {
            return; // Repo-layout guard — skip when run from a package export.
        }
        let mapping = SlurmMapping::load(&yaml, Some(&schema_path()))
            .expect("shipped slurm-mapping.yaml must pass validation");
        assert!(
            !mapping.resource_classes.is_empty(),
            "shipped mapping should declare at least one class"
        );
    }
}
