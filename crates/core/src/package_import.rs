//! Import-side helpers: probe a package's completeness (which reproducibility
//! features it physically supports) and reconstruct a `WorkflowDag` from the
//! on-disk crate. Pure, sync, no tokio — safe to call under `spawn_blocking`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::dag::{TaskState, DAG};
use crate::workflow_contracts::task_node::WorkflowDag;

/// Cosmetic completeness label derived from file presence. Gating uses the
/// granular boolean flags, not this label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PackageTier {
    /// A fully-populated package: audit substrate + re-execution surface +
    /// policies + a Turtle serialization.
    Full,
    /// Carries a re-execution surface (scripts + determinism env) but no
    /// policy/best-practice layer.
    ReExecutable,
    /// Audit substrate only — no scripts, no determinism env.
    MinimalAudit,
    /// Some other mix of files that doesn't match a canonical tier.
    Custom,
}

/// Which features an imported package supports, decided by physical file
/// presence. `tabs` carries per-tab availability keyed by a stable tab id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[non_exhaustive]
pub struct PackageCapabilities {
    /// Cosmetic completeness label. Gating reads the boolean flags, not this.
    pub tier_label: PackageTier,
    /// Package carries the minimum crate substrate needed to explore
    /// (`WORKFLOW.json` + `ro-crate-metadata.json`).
    pub explore: bool,
    /// Audit-proof re-verification can run (crate metadata + audit report).
    pub reverify: bool,
    /// Replay Tier-1 (integrity) is available (audit report + claim
    /// verification present).
    pub replay_tier1: bool,
    /// Replay Tier-2 (reproduce) is available (execution order + at least one
    /// re-executable task surface).
    pub replay_tier2: bool,
    /// Per-tab availability keyed by a stable tab id.
    pub tabs: BTreeMap<String, bool>,
}

fn exists(root: &Path, rel: &str) -> bool {
    root.join(rel).exists()
}

/// True if any `runtime/outputs/<task>/` dir is Tier-2 re-executable: has a
/// script (`.R`/`.py`/`.sh` under `scripts/`), a result table (`.tsv`/`.csv`
/// directly in the task dir), and a provisionable env (non-empty
/// `task_container_digest` in `determinism-env.json`, or an `env.lock`).
fn any_task_reexecutable(outputs: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(outputs) else {
        return false;
    };
    for entry in rd.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let has_script = dir_has_ext(&dir.join("scripts"), &["R", "py", "sh"]);
        let has_table = dir_has_ext(&dir, &["tsv", "csv"]);
        let provisionable = determinism_env_has_digest(&dir.join("determinism-env.json"))
            || dir.join("env.lock").exists();
        if has_script && has_table && provisionable {
            return true;
        }
    }
    false
}

fn any_task_has_scripts(outputs: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(outputs) else {
        return false;
    };
    rd.flatten()
        .any(|e| e.path().is_dir() && dir_has_ext(&e.path().join("scripts"), &["R", "py", "sh"]))
}

fn any_task_has_file(outputs: &Path, name: &str) -> bool {
    let Ok(rd) = std::fs::read_dir(outputs) else {
        return false;
    };
    rd.flatten()
        .any(|e| e.path().is_dir() && e.path().join(name).exists())
}

fn dir_has_ext(dir: &Path, exts: &[&str]) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    rd.flatten().any(|e| {
        e.path()
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| exts.contains(&x))
    })
}

fn determinism_env_has_digest(path: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| {
            v.get("task_container_digest")
                .and_then(|d| d.as_str())
                .map(|s| !s.is_empty())
        })
        .unwrap_or(false)
}

/// Probe which features a package on disk supports. See `PackageCapabilities`.
pub fn probe_package_capabilities(root: &Path) -> PackageCapabilities {
    let outputs = root.join("runtime/outputs");

    let explore = exists(root, "WORKFLOW.json") && exists(root, "ro-crate-metadata.json");
    let reverify =
        exists(root, "ro-crate-metadata.json") && exists(root, "runtime/audit-proof-report.json");
    let replay_tier1 = exists(root, "runtime/audit-proof-report.json")
        && exists(root, "runtime/claim-verification.json");
    let replay_tier2 =
        exists(root, "runtime/execution-order.json") && any_task_reexecutable(&outputs);

    let has_scripts = any_task_has_scripts(&outputs);
    let has_determinism = any_task_has_file(&outputs, "determinism-env.json");
    let has_policies = root.join("policies").is_dir();
    let has_ttl = exists(root, "package.ttl");

    let tier_label = if has_policies && has_ttl {
        PackageTier::Full
    } else if has_scripts && has_determinism && !has_policies {
        PackageTier::ReExecutable
    } else if !has_scripts && !has_determinism {
        PackageTier::MinimalAudit
    } else {
        PackageTier::Custom
    };

    let mut tabs = BTreeMap::new();
    tabs.insert(
        "composer_trace".to_string(),
        exists(root, "runtime/verifier-decisions.jsonl"),
    );
    // These tabs are backed by intake/chat-time state that never lands in a
    // package, so they always degrade to empty for imported packages.
    tabs.insert("composition".to_string(), false);
    tabs.insert("metrics".to_string(), false);
    tabs.insert("compare".to_string(), false);

    PackageCapabilities {
        tier_label,
        explore,
        reverify,
        replay_tier1,
        replay_tier2,
        tabs,
    }
}

/// Rebuild a `WorkflowDag` + task-state map from an emitted package on disk.
///
/// Primary path: `workflow_dag_from_artifact` over a `BackendArtifact` carrying
/// the lowered `DAG` (`WORKFLOW.json`) plus `proofs.jsonl` (typed edges) and
/// `assumptions.jsonl` (ledger) — the highest-fidelity reconstruction from disk.
/// When `proofs.jsonl` is empty/absent, fall back to `dag_to_workflow_dag`
/// (edges back-projected from `depends_on`). Task states come from the DAG.
pub fn reconstruct_workflow_dag_from_package(
    root: &Path,
) -> anyhow::Result<(WorkflowDag, BTreeMap<String, TaskState>)> {
    let wf_json = std::fs::read_to_string(root.join("WORKFLOW.json"))
        .with_context(|| format!("read {}/WORKFLOW.json", root.display()))?;
    let dag: DAG = serde_json::from_str(&wf_json).context("parse WORKFLOW.json as DAG")?;

    let task_states: BTreeMap<String, TaskState> = dag
        .tasks
        .iter()
        .map(|(id, t)| (id.to_string(), t.state.clone()))
        .collect();

    let proofs = std::fs::read_to_string(root.join("runtime/proofs.jsonl")).unwrap_or_default();
    let assumptions =
        std::fs::read_to_string(root.join("runtime/assumptions.jsonl")).unwrap_or_default();

    let workflow_dag = if proofs.trim().is_empty() {
        crate::backend_emitters::workflow_json::dag_to_workflow_dag(&dag)
    } else {
        let artifact = crate::backend_emitters::workflow_json::BackendArtifact {
            dag: dag.clone(),
            proofs_jsonl: proofs,
            assumptions_jsonl: assumptions,
            plot_affordances_jsonl: String::new(),
            affordance_fallbacks_jsonl: String::new(),
        };
        crate::backend_emitters::workflow_json::workflow_dag_from_artifact(&artifact)
    };

    Ok((workflow_dag, task_states))
}
