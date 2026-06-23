// crates/core/src/replay/script_runner.rs
//
// Agent-free script runner for the replay path.
//
// Stages a downloaded package's compute scripts into an isolated scratch tree,
// rewrites recorded absolute paths to the scratch location, and re-runs them
// in dependency order — WITHOUT ever invoking an LLM or agent.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use crate::replay::select::ComputeTask;
use crate::replay::env_provision::ExecEnv;

// ---------------------------------------------------------------------------
// Agent-free guard: entrypoint names that must never be executed.
// ---------------------------------------------------------------------------

/// Script base-names that would invoke an LLM agent.  Any staged script
/// whose file name (without path) matches one of these is refused.
const AGENT_ENTRYPOINTS: &[&str] = &[
    "agent-claude.sh",
    "agent.sh",
    "run-agent.sh",
    "claude",
    "claude.sh",
];

/// Return `true` when the script's file name matches a known agent entrypoint.
fn is_agent_script(script: &Path) -> bool {
    let name = script
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    AGENT_ENTRYPOINTS
        .iter()
        .any(|&entry| name == entry.to_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Per-task outcome from `stage_and_run`.
pub struct RunOutcome {
    pub task_id: String,
    /// `true` when every script in the task exited successfully (status 0).
    pub ok: bool,
    /// Concatenated stderr from all scripts in the task.
    pub stderr: String,
}

/// Stage, path-rewrite, and run a set of compute tasks inside a scratch tree.
///
/// # Parameters
/// - `pkg`           — root of the downloaded ECAA package.
/// - `scratch`       — empty (or pre-existing) scratch directory; output tree is
///                     written here.
/// - `tasks`         — tasks to run (from `select::select_compute_tasks`).
/// - `order`         — task ids in topological dependency order.  Tasks not
///                     listed run after, in their original stable order.
/// - `env`           — execution environment (from `env_provision::provision`).
/// - `recorded_root` — the absolute path recorded inside the package's scripts
///                     (i.e. where the package was originally executed).
/// - `recorded_env`  — environment variables captured at record time.
///
/// # Agent-free guarantee
/// No script whose name matches a known agent entrypoint (see `AGENT_ENTRYPOINTS`)
/// is ever executed.  Such scripts are skipped and their task is marked `ok: false`.
pub fn stage_and_run(
    pkg: &Path,
    scratch: &Path,
    tasks: &[ComputeTask],
    order: &[String],
    env: &ExecEnv,
    recorded_root: &str,
    recorded_env: &BTreeMap<String, String>,
) -> io::Result<Vec<RunOutcome>> {
    // Build run environment: recorded vars overridden by PKG_ROOT=scratch.
    let scratch_root = scratch.display().to_string();
    let mut run_env = recorded_env.clone();
    run_env.insert("PKG_ROOT".to_string(), scratch_root.clone());

    // Stage the shared inputs subtree once (idempotent).
    let inputs_src = pkg.join("runtime/outputs/data_acquisition/data/inputs");
    let inputs_dst = scratch.join("runtime/outputs/data_acquisition/data/inputs");
    if inputs_src.is_dir() {
        copy_dir_all(&inputs_src, &inputs_dst)?;
    }

    // Resolve run order: tasks listed in `order` first (in that order), then
    // remaining tasks in stable (original slice) order.
    let ordered_ids: Vec<&str> = order.iter().map(|s| s.as_str()).collect();
    let order_set: BTreeSet<&str> = ordered_ids.iter().copied().collect();

    let mut task_order: Vec<&ComputeTask> = Vec::with_capacity(tasks.len());
    for id in &ordered_ids {
        if let Some(t) = tasks.iter().find(|t| t.task_id == *id) {
            task_order.push(t);
        }
    }
    for t in tasks {
        if !order_set.contains(t.task_id.as_str()) {
            task_order.push(t);
        }
    }

    let mut outcomes: Vec<RunOutcome> = Vec::with_capacity(task_order.len());

    for task in task_order {
        let outcome = run_task(task, pkg, scratch, &run_env, &scratch_root, env, recorded_root)?;
        outcomes.push(outcome);
    }

    Ok(outcomes)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Stage and run a single task.
fn run_task(
    task: &ComputeTask,
    pkg: &Path,
    scratch: &Path,
    run_env: &BTreeMap<String, String>,
    scratch_root: &str,
    env: &ExecEnv,
    recorded_root: &str,
) -> io::Result<RunOutcome> {
    // Mirror scripts into scratch.
    let staged_scripts_dir = scratch
        .join("runtime/outputs")
        .join(&task.task_id)
        .join("scripts");
    std::fs::create_dir_all(&staged_scripts_dir)?;

    // Enumerate source scripts in sorted order (deterministic within task).
    let src_scripts_dir = pkg
        .join("runtime/outputs")
        .join(&task.task_id)
        .join("scripts");

    let mut scripts: Vec<PathBuf> = std::fs::read_dir(&src_scripts_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|ext| matches!(ext, "R" | "py" | "sh"))
                .unwrap_or(false)
        })
        .collect();
    scripts.sort();

    // Also create the output directory for the task so scripts can write there.
    let task_out_dir = scratch.join("runtime/outputs").join(&task.task_id);
    std::fs::create_dir_all(&task_out_dir)?;

    let mut task_ok = true;
    let mut task_stderr = String::new();

    for src_script in &scripts {
        let script_name = src_script.file_name().unwrap();
        let staged_script = staged_scripts_dir.join(script_name);

        // Agent-free guard: refuse agent entrypoints.
        if is_agent_script(src_script) {
            task_ok = false;
            task_stderr.push_str(&format!(
                "SKIPPED (agent entrypoint): {}\n",
                src_script.display()
            ));
            continue;
        }

        // Copy and rewrite the script.
        let content = std::fs::read(src_script)?;
        let content_str = String::from_utf8_lossy(&content);
        let rewritten = content_str.replace(recorded_root, scratch_root);
        std::fs::write(&staged_script, rewritten.as_bytes())?;
        // Ensure the staged script is executable.
        set_executable(&staged_script)?;

        // Run via the execution environment.
        let output = env.run_script(&staged_script, run_env, scratch)?;
        let script_stderr = String::from_utf8_lossy(&output.stderr).to_string();
        task_stderr.push_str(&script_stderr);
        if !output.status.success() {
            task_ok = false;
        }
    }

    Ok(RunOutcome {
        task_id: task.task_id.clone(),
        ok: task_ok,
        stderr: task_stderr,
    })
}

/// Recursively copy a directory tree from `src` to `dst`.
fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)?;
    }
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Set the executable bit on a file (Unix only; no-op on other platforms).
#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    let mode = perms.mode();
    perms.set_mode(mode | 0o111);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::env_provision::ExecEnv;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    // ---- Hermetic execution seam ----
    // ExecEnv::Shell is added in env_provision.rs under #[cfg(test)].
    // It runs `bash <script>` directly — no docker, no conda required.

    fn shell_env() -> ExecEnv {
        ExecEnv::Shell
    }

    /// Build a minimal fake package with a `differential_expression` task.
    ///
    /// The task's `01.sh` script:
    ///   - writes `$PKG_ROOT` into `$PKG_ROOT/runtime/outputs/differential_expression/out.tsv`
    ///   - contains a comment with the literal `recorded_root` so the rewrite test can verify it
    fn build_fake_pkg(pkg: &Path, recorded_root: &str) {
        let scripts_dir = pkg
            .join("runtime/outputs/differential_expression/scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();

        // Create a result table so select_compute_tasks would include this task.
        std::fs::write(
            pkg.join("runtime/outputs/differential_expression/de_results.tsv"),
            "gene\tpadj\n",
        )
        .unwrap();

        let script_content = format!(
            "#!/usr/bin/env bash\n\
             # recorded path: {recorded_root}\n\
             mkdir -p \"$PKG_ROOT/runtime/outputs/differential_expression\"\n\
             echo -n \"$PKG_ROOT\" > \"$PKG_ROOT/runtime/outputs/differential_expression/out.tsv\"\n"
        );
        std::fs::write(scripts_dir.join("01.sh"), &script_content).unwrap();
    }

    #[test]
    fn path_rewrite_and_execution() {
        let pkg_tmp = tempdir().unwrap();
        let scratch_tmp = tempdir().unwrap();
        let pkg = pkg_tmp.path();
        let scratch = scratch_tmp.path();

        let recorded_root = "/original/package/root";
        build_fake_pkg(pkg, recorded_root);

        let task = ComputeTask {
            task_id: "differential_expression".to_string(),
            scripts_dir: pkg.join("runtime/outputs/differential_expression/scripts"),
            result_tables: vec!["de_results.tsv".to_string()],
        };

        let outcomes = stage_and_run(
            pkg,
            scratch,
            &[task],
            &[],
            &shell_env(),
            recorded_root,
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(outcomes.len(), 1);
        let outcome = &outcomes[0];
        assert_eq!(outcome.task_id, "differential_expression");
        assert!(
            outcome.ok,
            "task should succeed; stderr: {}",
            outcome.stderr
        );

        // (a) out.tsv exists under the scratch root
        let out_tsv = scratch
            .join("runtime/outputs/differential_expression/out.tsv");
        assert!(out_tsv.exists(), "out.tsv should exist under scratch root");

        // (b) its content equals the scratch root (PKG_ROOT was redirected)
        let content = std::fs::read_to_string(&out_tsv).unwrap();
        let scratch_str = scratch.display().to_string();
        assert_eq!(
            content, scratch_str,
            "out.tsv content should be scratch root; got: {:?}",
            content
        );

        // (c) the staged 01.sh no longer contains `recorded_root`
        let staged = scratch
            .join("runtime/outputs/differential_expression/scripts/01.sh");
        let staged_content = std::fs::read_to_string(&staged).unwrap();
        assert!(
            !staged_content.contains(recorded_root),
            "staged script should not contain recorded_root; got:\n{}",
            staged_content
        );
        // The staged script should contain the scratch root instead.
        assert!(
            staged_content.contains(&scratch_str),
            "staged script should contain scratch root; got:\n{}",
            staged_content
        );
    }

    #[test]
    fn agent_free_guard_refuses_agent_entrypoint() {
        let pkg_tmp = tempdir().unwrap();
        let scratch_tmp = tempdir().unwrap();
        let pkg = pkg_tmp.path();
        let scratch = scratch_tmp.path();

        let recorded_root = "/original/package/root";

        // Create a task with an agent entrypoint script named `agent-claude.sh`.
        let scripts_dir = pkg.join("runtime/outputs/suspicious_task/scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/suspicious_task/results.tsv"),
            "a\tb\n",
        )
        .unwrap();
        // Write an agent entrypoint — should be refused.
        std::fs::write(
            scripts_dir.join("agent-claude.sh"),
            "#!/bin/bash\nclaude --model sonnet 'do things'\n",
        )
        .unwrap();

        let task = ComputeTask {
            task_id: "suspicious_task".to_string(),
            scripts_dir: scripts_dir.clone(),
            result_tables: vec!["results.tsv".to_string()],
        };

        let outcomes = stage_and_run(
            pkg,
            scratch,
            &[task],
            &[],
            &shell_env(),
            recorded_root,
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(outcomes.len(), 1);
        let outcome = &outcomes[0];
        // Task must fail because the only script was refused.
        assert!(
            !outcome.ok,
            "task with agent entrypoint should not be ok"
        );
        assert!(
            outcome.stderr.contains("SKIPPED (agent entrypoint)"),
            "stderr should explain refusal; got: {}",
            outcome.stderr
        );
    }

    #[test]
    fn dependency_order_respected() {
        let pkg_tmp = tempdir().unwrap();
        let scratch_tmp = tempdir().unwrap();
        let pkg = pkg_tmp.path();
        let scratch = scratch_tmp.path();

        // Build two tasks: `task_b` runs first in order, then `task_a`.
        // Each script appends its id to a shared log file.
        let log = scratch.join("order.log");
        // Pre-create scratch so scripts can write to it.
        std::fs::create_dir_all(scratch).unwrap();

        for id in &["task_a", "task_b"] {
            let scripts_dir = pkg.join("runtime/outputs").join(id).join("scripts");
            std::fs::create_dir_all(&scripts_dir).unwrap();
            std::fs::write(
                pkg.join("runtime/outputs").join(id).join("results.tsv"),
                "x\n",
            )
            .unwrap();
            let log_str = log.display().to_string();
            let script = format!(
                "#!/usr/bin/env bash\necho {id} >> \"{log_str}\"\n"
            );
            std::fs::write(scripts_dir.join("01.sh"), &script).unwrap();
        }

        let tasks = vec![
            ComputeTask {
                task_id: "task_a".to_string(),
                scripts_dir: pkg.join("runtime/outputs/task_a/scripts"),
                result_tables: vec!["results.tsv".to_string()],
            },
            ComputeTask {
                task_id: "task_b".to_string(),
                scripts_dir: pkg.join("runtime/outputs/task_b/scripts"),
                result_tables: vec!["results.tsv".to_string()],
            },
        ];

        // Request task_b before task_a in order.
        let outcomes = stage_and_run(
            pkg,
            scratch,
            &tasks,
            &["task_b".to_string(), "task_a".to_string()],
            &shell_env(),
            "/irrelevant",
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(outcomes.len(), 2);
        assert!(outcomes[0].ok, "task_b stderr: {}", outcomes[0].stderr);
        assert!(outcomes[1].ok, "task_a stderr: {}", outcomes[1].stderr);
        assert_eq!(outcomes[0].task_id, "task_b");
        assert_eq!(outcomes[1].task_id, "task_a");

        let log_content = std::fs::read_to_string(&log).unwrap_or_default();
        let lines: Vec<&str> = log_content.lines().collect();
        assert_eq!(lines, vec!["task_b", "task_a"], "wrong order: {:?}", lines);
    }
}
