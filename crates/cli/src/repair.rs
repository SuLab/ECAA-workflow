//! `ecaa-workflow repair <package>` — drive the iterative repair loop over
//! an emitted package, then surface the verdict + the review list.
//!
//! Two runner modes:
//! - default (no `--agent`): [`ReviewRoutingRunner`] appends agentic needs to
//!   `<pkg>/runtime/repair-requests.jsonl` for human review (offline).
//! - `--agent <cmd>`: a [`HarnessRunner`] that, on each agentic directive,
//!   sets the target task back to `Ready` in `WORKFLOW.json` and re-runs the
//!   harness as a subprocess (PATH binary, with a `cargo run` fallback).
//!
//! The package's own `policies/` directory is the `config_dir` the core loop
//! threads into `finalize_package`/`assess_package` (where
//! `interpretation-policy.json` lives).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ecaa_workflow_core::repair_loop::runner::{RepairDirective, ReviewRoutingRunner, TaskRunner};
use ecaa_workflow_core::repair_loop::{run_repair_loop, RepairVerdict};

#[derive(clap::Args, Debug)]
pub(crate) struct RepairArgs {
    /// Path to an emitted package directory (must contain `WORKFLOW.json`).
    pub package: String,
    /// Agent command to drive the harness with (e.g. `claude`). When set,
    /// agentic repair directives re-run the harness as a subprocess instead
    /// of being routed to offline review.
    #[arg(long)]
    pub agent: Option<String>,
    /// Cap on harness iterations per agentic re-run (passed to
    /// `--max-iterations`). Defaults to 4.
    #[arg(long)]
    pub max_rounds: Option<usize>,
    /// HMAC audit secret (hex). Sets `ECAA_AUDIT_SECRET` for the finalize step
    /// that `assess_package` performs each round. When unset the env value (if
    /// any) is used.
    #[arg(long)]
    pub secret: Option<String>,
}

pub(crate) fn run(args: RepairArgs) -> Result<()> {
    use colored::Colorize;

    let package = PathBuf::from(&args.package);
    if !package.join("WORKFLOW.json").exists() {
        return Err(anyhow::anyhow!(
            "no WORKFLOW.json in package dir '{}'",
            package.display()
        ));
    }
    // The package's own policies/ dir is the config_dir the core loop threads
    // into finalize_package/assess_package (interpretation-policy.json lives
    // here).
    let config_dir = package.join("policies");

    // Surface the audit secret for the per-round finalize step.
    if let Some(secret) = args.secret.as_ref() {
        // SAFETY note: single-threaded CLI entry, set before the loop runs.
        std::env::set_var("ECAA_AUDIT_SECRET", secret);
    }

    // Pick a runner: agent-driven harness subprocess, or offline review.
    let status = if let Some(agent_cmd) = args.agent.clone() {
        let runner = HarnessRunner {
            agent_cmd,
            max_iterations: args.max_rounds.unwrap_or(4),
        };
        run_repair_loop(&package, &config_dir, &runner)?
    } else {
        let runner = ReviewRoutingRunner;
        run_repair_loop(&package, &config_dir, &runner)?
    };

    let verdict_str = match status.verdict {
        RepairVerdict::FullyPassing => "fully-passing".green().bold(),
        RepairVerdict::MostlyPassing => "mostly-passing".yellow().bold(),
        RepairVerdict::Failing => "failing".red().bold(),
    };
    println!(
        "repair verdict: {} ({} round{}, {} review item{})",
        verdict_str,
        status.rounds,
        if status.rounds == 1 { "" } else { "s" },
        status.review.len(),
        if status.review.len() == 1 { "" } else { "s" },
    );
    for item in &status.review {
        // One line per ReviewItem: id / class / why.
        println!(
            "  {} [{:?}] {}",
            item.failure.id.cyan(),
            item.failure.class,
            item.why.dimmed(),
        );
    }

    // Exit code: 0 for FullyPassing/MostlyPassing, non-zero for Failing.
    match status.verdict {
        RepairVerdict::FullyPassing | RepairVerdict::MostlyPassing => Ok(()),
        RepairVerdict::Failing => Err(anyhow::anyhow!(
            "repair failed: {} unresolved review item(s)",
            status.review.len()
        )),
    }
}

/// Agent-backed [`TaskRunner`]: on each agentic directive, set the target task
/// back to `Ready` in `WORKFLOW.json` and re-run the harness as a subprocess.
///
/// Not unit-tested — the re-run path needs a live agent. The deterministic
/// piece (the `Ready`-state rewrite) is covered by [`set_task_ready`]'s test.
struct HarnessRunner {
    /// Agent command passed to the harness `--agent` flag (e.g. `claude`).
    agent_cmd: String,
    /// Cap passed to the harness `--max-iterations` flag.
    max_iterations: usize,
}

impl TaskRunner for HarnessRunner {
    fn rerun(&self, pkg: &Path, directive: &RepairDirective) -> Result<()> {
        // 1. Mark the directive's task Ready so the harness re-dispatches it.
        set_task_ready(pkg, &directive.task)?;

        // 2. Re-run the harness over the package, scoped to the agent cmd.
        let pkg_str = pkg
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("package path is not valid UTF-8: {}", pkg.display()))?;
        let max_iters = self.max_iterations.to_string();
        run_harness_subprocess(pkg_str, &self.agent_cmd, &max_iters)
    }
}

/// Set `tasks[<task>].state = {"status":"ready"}` in `<pkg>/WORKFLOW.json`,
/// matching the serde shape of [`ecaa_workflow_core::dag::TaskState::Ready`]
/// (`#[serde(rename_all = "snake_case", tag = "status")]`). Read → mutate →
/// atomic write.
fn set_task_ready(pkg: &Path, task: &str) -> Result<()> {
    let wf_path = pkg.join("WORKFLOW.json");
    let bytes =
        std::fs::read(&wf_path).with_context(|| format!("reading {}", wf_path.display()))?;
    let mut doc: serde_json::Value =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", wf_path.display()))?;

    let task_obj = doc
        .get_mut("tasks")
        .and_then(|t| t.get_mut(task))
        .ok_or_else(|| anyhow::anyhow!("task '{}' not found in {}", task, wf_path.display()))?;
    let obj = task_obj
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("task '{}' is not a JSON object", task))?;
    obj.insert(
        "state".to_string(),
        serde_json::json!({ "status": "ready" }),
    );

    let serialized = serde_json::to_vec_pretty(&doc).context("serializing WORKFLOW.json")?;
    ecaa_workflow_core::fs_helpers::atomic_write_bytes_sync(&wf_path, &serialized)
        .with_context(|| format!("atomic write {}", wf_path.display()))?;
    Ok(())
}

/// Spawn the harness over `pkg` with `agent_cmd`/`max_iterations`. Tries the
/// PATH binary first; on a genuine PATH miss falls back to `cargo run`. Mirrors
/// `run_serve`'s spawn-with-fallback shape.
fn run_harness_subprocess(pkg: &str, agent_cmd: &str, max_iters: &str) -> Result<()> {
    use std::process::Command;

    let harness_bin = "ecaa-workflow-harness";
    let status = Command::new(harness_bin)
        .args([
            "--package",
            pkg,
            "--agent",
            agent_cmd,
            "--max-iterations",
            max_iters,
        ])
        .status();

    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err(anyhow::anyhow!("harness exited with non-zero status")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Genuine PATH miss — fall back to `cargo run`. Other spawn
            // failures (perms/EAGAIN) fall through to the final arm so we
            // surface them rather than mask with a misleading "not found".
            let s = Command::new("cargo")
                .args([
                    "run",
                    "-p",
                    "ecaa-workflow-harness",
                    "--",
                    "--package",
                    pkg,
                    "--agent",
                    agent_cmd,
                    "--max-iterations",
                    max_iters,
                ])
                .status()
                .context("spawning `cargo run -p ecaa-workflow-harness`")?;
            if s.success() {
                Ok(())
            } else {
                Err(anyhow::anyhow!("cargo run harness failed"))
            }
        }
        Err(e) => Err(anyhow::anyhow!(
            "failed to spawn '{}': {} (kind: {:?})",
            harness_bin,
            e,
            e.kind()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::dag::{TaskState, DAG};

    /// Minimal WORKFLOW.json with one Failed task, exercised through the real
    /// `DAG` deserializer so the rewrite is validated against the production
    /// serde shape, not a hand-rolled one.
    fn write_minimal_workflow(pkg: &Path) {
        let json = serde_json::json!({
            "version": "1.0",
            "schema_version": "1.0.0",
            "workflow_id": "workflow-deadbeef",
            "current_task": null,
            "tasks": {
                "deseq": {
                    "kind": "computation",
                    "depends_on": [],
                    "assignee": "agent",
                    "description": "differential expression",
                    "state": { "status": "failed", "reason": "contrast mismatch" }
                }
            }
        });
        std::fs::write(
            pkg.join("WORKFLOW.json"),
            serde_json::to_vec_pretty(&json).expect("serialize fixture"),
        )
        .expect("write fixture");
    }

    #[test]
    fn set_task_ready_flips_failed_to_ready() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_minimal_workflow(dir.path());

        set_task_ready(dir.path(), "deseq").expect("set ready");

        // Re-read through the real DAG deserializer: the task is now Ready.
        let bytes = std::fs::read(dir.path().join("WORKFLOW.json")).expect("read back");
        let dag: DAG = serde_json::from_slice(&bytes).expect("DAG round-trips");
        let task = dag.tasks.get("deseq").expect("task present");
        assert!(
            matches!(task.state, TaskState::Ready),
            "Failed task must be flipped to Ready, got {:?}",
            task.state
        );
    }

    #[test]
    fn set_task_ready_errors_on_unknown_task() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_minimal_workflow(dir.path());

        let err = set_task_ready(dir.path(), "no_such_task")
            .expect_err("unknown task must error, not silently no-op");
        assert!(
            err.to_string().contains("no_such_task"),
            "error must name the missing task, got: {err}"
        );
    }

    /// Faithful twin: a task that is ALREADY Ready must round-trip to Ready
    /// (the rewrite is idempotent and must not corrupt the state shape).
    #[test]
    fn set_task_ready_is_idempotent_on_already_ready() {
        let dir = tempfile::tempdir().expect("tempdir");
        let json = serde_json::json!({
            "version": "1.0",
            "schema_version": "1.0.0",
            "workflow_id": "workflow-deadbeef",
            "current_task": null,
            "tasks": {
                "deseq": {
                    "kind": "computation",
                    "depends_on": [],
                    "assignee": "agent",
                    "description": "differential expression",
                    "state": { "status": "ready" }
                }
            }
        });
        std::fs::write(
            dir.path().join("WORKFLOW.json"),
            serde_json::to_vec_pretty(&json).expect("serialize"),
        )
        .expect("write");

        set_task_ready(dir.path(), "deseq").expect("set ready on already-ready");

        let bytes = std::fs::read(dir.path().join("WORKFLOW.json")).expect("read back");
        let dag: DAG = serde_json::from_slice(&bytes).expect("DAG round-trips");
        assert!(
            matches!(
                dag.tasks.get("deseq").expect("task").state,
                TaskState::Ready
            ),
            "already-Ready task must stay Ready"
        );
    }
}
