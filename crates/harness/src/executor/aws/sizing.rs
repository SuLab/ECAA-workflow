//! Package I/O + task-metadata helpers used across the aws/ submodules.
//!
//! Kept in one place so provisioning / ssm / cloudwatch paths can read
//! the emitted package's `WORKFLOW.json`,
//! `policies/compute-resource-policy.json`, and
//! `policies/intake-facts.json` via one canonical loader pair. Nothing
//! here shells out to AWS — these are pure file I/O + spec lookups.

use super::super::sizing::{BaseRequirements, ComputeProfiles, DefaultProfile, SizingIntakeFacts};
use anyhow::{Context, Result};
use scripps_workflow_core::dag::{Task, DAG};
use std::path::Path;

/// Load compute profiles for AWS executor paths. Returns a default
/// empty profile when the package has no policies/compute-resource-policy.json
/// — callers (pilot/stall) treat that as the "no data" path.
pub(super) fn load_aws_profiles(package: &Path) -> Result<ComputeProfiles> {
    let profiles_path = package.join("policies/compute-resource-policy.json");
    if !profiles_path.exists() {
        return Ok(ComputeProfiles {
            profiles: Default::default(),
            default: DefaultProfile {
                requirements: BaseRequirements::default(),
                notes: None,
            },
            method_overrides: Default::default(),
        });
    }
    let v: serde_json::Value = scripps_workflow_core::fs_helpers::read_json(&profiles_path)?;
    let yaml = serde_yml::to_string(&v)?;
    serde_yml::from_str::<ComputeProfiles>(&yaml).map_err(Into::into)
}

/// Load sizing facts from `policies/intake-facts.json`. Missing /
/// unparseable file yields the default facts so the caller can
/// always produce a projection.
pub(super) fn load_aws_facts(package: &Path) -> SizingIntakeFacts {
    let p = package.join("policies/intake-facts.json");
    let Ok(raw) = std::fs::read_to_string(&p) else {
        return SizingIntakeFacts::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Extract the `stage_class` hint from a task's spec payload, or
/// empty string when unavailable. Used by the SSM-aware sizing /
/// staleness paths to look up per-stage knobs in the emitted
/// compute-resource-policy profiles.
pub(super) fn task_stage_class(task: &Task) -> String {
    task.spec
        .as_ref()
        .and_then(|s| s.get("stage_class"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Extract the `task_id` encoded in the task's spec (if any). Added to
/// the Task payload at build time so the SSM `list-command-invocations
/// --filters Comment=scripps-task-<id>` query can find the running
/// invocation. Returns `None` when the spec doesn't carry the field.
pub(super) fn task_id_from_spec(task: &Task) -> Option<String> {
    task.spec
        .as_ref()
        .and_then(|s| s.get("task_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub(super) fn read_dag(dir: &Path) -> Result<DAG> {
    scripps_workflow_core::fs_helpers::read_json(&dir.join("WORKFLOW.json"))
}

pub(super) fn write_dag(dir: &Path, dag: &DAG) -> Result<()> {
    let path = dir.join("WORKFLOW.json");
    let body = serde_json::to_string_pretty(dag).context("serializing DAG")?;
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))
}
