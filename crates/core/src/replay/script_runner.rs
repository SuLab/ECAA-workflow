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
/// - `pkg` — root of the downloaded ECAA package.
/// - `scratch` — empty (or pre-existing) scratch directory; output tree is
///   written here.
/// - `tasks` — tasks to run (from `select::select_compute_tasks`).
/// - `order` — task ids in topological dependency order.  Tasks not
///   listed run after, in their original stable order.
/// - `env` — execution environment (from `env_provision::provision`).
/// - `recorded_root` — the absolute path recorded inside the package's scripts
///   (i.e. where the package was originally executed).
/// - `recorded_env` — environment variables captured at record time.
///
/// # Agent-free guarantee
/// No script whose name matches a known agent entrypoint (see `AGENT_ENTRYPOINTS`)
/// is ever executed.  Such scripts are skipped and their task is marked `ok: false`.
///
/// # Per-task error isolation
/// A missing scripts directory, copy failure, unknown interpreter, or spawn
/// failure for one task yields `RunOutcome { ok: false, stderr: <reason> }` and
/// the run continues to the next task.  Only a failure that makes continuing
/// impossible (e.g. cannot create the scratch root) is returned as `Err`.
pub fn stage_and_run(
    pkg: &Path,
    scratch: &Path,
    tasks: &[ComputeTask],
    order: &[String],
    env: &ExecEnv,
    recorded_root: &str,
    recorded_env: &BTreeMap<String, String>,
) -> io::Result<Vec<RunOutcome>> {
    // Build run environment: recorded vars overridden by PKG_ROOT=scratch
    // and PACKAGE=scratch (real ECAA scripts may read either env var).
    let scratch_root = scratch.display().to_string();
    let mut run_env = recorded_env.clone();
    run_env.insert("PKG_ROOT".to_string(), scratch_root.clone());
    run_env.insert("PACKAGE".to_string(), scratch_root.clone());

    // Stage the entire data_acquisition/data/ subtree once (idempotent).
    // The input directory label is chosen by the package author (e.g. "himes-inputs/",
    // "inputs/", etc.); copying the whole data/ tree preserves whatever label(s) exist.
    let data_src = pkg.join("runtime/outputs/data_acquisition/data");
    let data_dst = scratch.join("runtime/outputs/data_acquisition/data");
    if data_src.is_dir() {
        copy_dir_all(&data_src, &data_dst)?;
    }

    // Stage the top-level `inputs/` tree (the registered user inputs). Data
    // ingestion scripts read from `$PACKAGE/inputs/<file>`; without this they
    // re-execute against a missing path and the task fails.
    let inputs_src = pkg.join("inputs");
    let inputs_dst = scratch.join("inputs");
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
        // Per-task errors do NOT abort the whole run — they become ok:false outcomes.
        let outcome = run_task(task, pkg, scratch, &run_env, &scratch_root, env, recorded_root);
        outcomes.push(outcome);
    }

    Ok(outcomes)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Stage and run a single task.  Always returns a `RunOutcome` — errors that
/// are local to this task (missing scripts dir, unknown interpreter, spawn
/// failure) are captured as `ok: false` rather than propagated.
fn run_task(
    task: &ComputeTask,
    pkg: &Path,
    scratch: &Path,
    run_env: &BTreeMap<String, String>,
    scratch_root: &str,
    env: &ExecEnv,
    recorded_root: &str,
) -> RunOutcome {
    // Mirror scripts into scratch.
    let staged_scripts_dir = scratch
        .join("runtime/outputs")
        .join(&task.task_id)
        .join("scripts");

    if let Err(e) = std::fs::create_dir_all(&staged_scripts_dir) {
        return RunOutcome {
            task_id: task.task_id.clone(),
            ok: false,
            stderr: format!("could not create staged scripts dir: {e}"),
        };
    }

    // Enumerate source scripts in sorted order (deterministic within task).
    let src_scripts_dir = pkg
        .join("runtime/outputs")
        .join(&task.task_id)
        .join("scripts");

    let read_dir = match std::fs::read_dir(&src_scripts_dir) {
        Ok(rd) => rd,
        Err(e) => {
            return RunOutcome {
                task_id: task.task_id.clone(),
                ok: false,
                stderr: format!(
                    "could not read scripts dir {}: {e}",
                    src_scripts_dir.display()
                ),
            };
        }
    };

    // Collect the files we will attempt to run. The recorded runs write logs,
    // manifests, and data files (`.log`, `.json`, `.tsv`, …) into `scripts/`
    // alongside the real compute scripts; executing those as scripts made the
    // whole task fail on an unknown-interpreter error even though the real
    // script succeeded. Keep only files with a runnable interpreter extension
    // — plus any agent entrypoint (by name), so the agent-free guard below
    // still fires loudly rather than silently ignoring a `claude` invocation.
    let mut scripts: Vec<PathBuf> = read_dir
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| crate::replay::env_provision::is_runnable_script(p) || is_agent_script(p))
        .collect();
    scripts.sort();

    // A task with no runnable scripts is a vacuous success that would mislead
    // the comparator into treating a re-execution of nothing as successful.
    if scripts.is_empty() {
        return RunOutcome {
            task_id: task.task_id.clone(),
            ok: false,
            stderr: "no runnable scripts found".to_string(),
        };
    }

    // Also create the output directory for the task so scripts can write there.
    let scratch_task_dir = scratch.join("runtime/outputs").join(&task.task_id);
    if let Err(e) = std::fs::create_dir_all(&scratch_task_dir) {
        return RunOutcome {
            task_id: task.task_id.clone(),
            ok: false,
            stderr: format!("could not create task output dir: {e}"),
        };
    }

    // Mirror the recorded task output dir's SUBDIRECTORY tree (dirs only, no
    // files) into the scratch so a script that writes into a pre-existing
    // subdir (e.g. `intermediates/`, `figures/`) without creating it first
    // behaves as it did on the recorded run. Best-effort; idempotent.
    mirror_subdirs(&pkg.join("runtime/outputs").join(&task.task_id), &scratch_task_dir);

    // The deposit export drops `intermediates/` (Tier E — regenerable bloat),
    // so mirror_subdirs above cannot recreate it from the package. But agent
    // scripts commonly write into `intermediates/` (e.g. `saveRDS(dds,
    // "intermediates/dds.rds")`) without `dir.create`, relying on the dir that
    // existed at record time. Recreate it so those writes succeed on replay
    // and the task is not spuriously classified Failed.
    let _ = std::fs::create_dir_all(scratch_task_dir.join("intermediates"));

    let mut task_ok = true;
    let mut task_stderr = String::new();

    for src_script in &scripts {
        let script_name = src_script.file_name().unwrap();
        let staged_script = staged_scripts_dir.join(script_name);

        // Agent-free guard: refuse agent entrypoints (checked before dispatch).
        if is_agent_script(src_script) {
            task_ok = false;
            task_stderr.push_str(&format!(
                "SKIPPED (agent entrypoint): {}\n",
                src_script.display()
            ));
            continue;
        }

        // Copy and rewrite the script.
        let content = match std::fs::read(src_script) {
            Ok(c) => c,
            Err(e) => {
                task_ok = false;
                task_stderr.push_str(&format!(
                    "could not read {}: {e}\n",
                    src_script.display()
                ));
                continue;
            }
        };
        let content_str = String::from_utf8_lossy(&content);
        // Guard against empty recorded_root: replacing "" inserts scratch_root
        // between every character.  If recorded_root is empty, leave content
        // unchanged.
        let rewritten: std::borrow::Cow<str> = if recorded_root.is_empty() {
            content_str
        } else {
            std::borrow::Cow::Owned(content_str.replace(recorded_root, scratch_root))
        };

        if let Err(e) = std::fs::write(&staged_script, rewritten.as_bytes()) {
            task_ok = false;
            task_stderr.push_str(&format!(
                "could not write staged script {}: {e}\n",
                staged_script.display()
            ));
            continue;
        }

        // Ensure the staged script is executable.
        if let Err(e) = set_executable(&staged_script) {
            task_ok = false;
            task_stderr.push_str(&format!(
                "could not set executable on {}: {e}\n",
                staged_script.display()
            ));
            continue;
        }

        // Run via the execution environment.
        let output = match env.run_script(&staged_script, run_env, scratch) {
            Ok(o) => o,
            Err(e) => {
                // Determine whether the extension is unknown or the spawn itself failed.
                let ext = src_script
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                let reason = if ext.is_empty() {
                    format!(
                        "cannot dispatch extensionless script {}: {e}\n",
                        src_script.display()
                    )
                } else {
                    format!(
                        "cannot dispatch script {} (extension={ext:?}): {e}\n",
                        src_script.display()
                    )
                };
                task_ok = false;
                task_stderr.push_str(&reason);
                continue;
            }
        };
        let script_stderr = String::from_utf8_lossy(&output.stderr).to_string();
        task_stderr.push_str(&script_stderr);
        if !output.status.success() {
            task_ok = false;
        }
    }

    RunOutcome {
        task_id: task.task_id.clone(),
        ok: task_ok,
        stderr: task_stderr,
    }
}

/// Recreate (empty) every subdirectory of `src` under `dst`, recursively —
/// directories only, never files. Reproduces the recorded task output
/// directory layout in the replay scratch so a script that writes into a
/// pre-existing subdir without `mkdir`-ing it first behaves as it did on the
/// recorded run. Best-effort: individual failures are ignored, and existing
/// directories (e.g. already-staged `data/`) are left untouched.
fn mirror_subdirs(src: &Path, dst: &Path) {
    let Ok(entries) = std::fs::read_dir(src) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let sub_dst = dst.join(entry.file_name());
            let _ = std::fs::create_dir_all(&sub_dst);
            mirror_subdirs(&entry.path(), &sub_dst);
        }
    }
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

    /// Important 2 — empty `recorded_root` must leave staged content unchanged.
    /// `str::replace("")` inserts the replacement between every character.
    /// The guard must prevent that corruption.
    #[test]
    fn empty_recorded_root_leaves_content_unchanged() {
        let pkg_tmp = tempdir().unwrap();
        let scratch_tmp = tempdir().unwrap();
        let pkg = pkg_tmp.path();
        let scratch = scratch_tmp.path();

        let scripts_dir = pkg.join("runtime/outputs/task_empty_root/scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/task_empty_root/results.tsv"),
            "x\n",
        )
        .unwrap();

        let original_content = "#!/usr/bin/env bash\necho hello\n";
        std::fs::write(scripts_dir.join("run.sh"), original_content).unwrap();

        let task = ComputeTask {
            task_id: "task_empty_root".to_string(),
            scripts_dir: scripts_dir.clone(),
            result_tables: vec!["results.tsv".to_string()],
        };

        // Pass empty recorded_root — must NOT corrupt the script.
        let outcomes = stage_and_run(
            pkg,
            scratch,
            &[task],
            &[],
            &shell_env(),
            "",  // empty recorded_root
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(outcomes.len(), 1);
        let outcome = &outcomes[0];
        // Script should execute successfully.
        assert!(outcome.ok, "task should succeed; stderr: {}", outcome.stderr);

        // Staged content must be byte-for-byte identical to original.
        let staged = scratch
            .join("runtime/outputs/task_empty_root/scripts/run.sh");
        let staged_content = std::fs::read_to_string(&staged).unwrap();
        assert_eq!(
            staged_content, original_content,
            "empty recorded_root must leave staged content unchanged; got:\n{}",
            staged_content
        );
    }

    /// Important 3 — a missing scripts dir for task 1 must NOT abort task 2.
    /// Task 1 yields ok:false; task 2 still runs and yields ok:true.
    #[test]
    fn missing_scripts_dir_does_not_abort_subsequent_tasks() {
        let pkg_tmp = tempdir().unwrap();
        let scratch_tmp = tempdir().unwrap();
        let pkg = pkg_tmp.path();
        let scratch = scratch_tmp.path();

        // task_missing: scripts dir intentionally absent.
        std::fs::write(
            {
                let d = pkg.join("runtime/outputs/task_missing");
                std::fs::create_dir_all(&d).unwrap();
                d.join("results.tsv")
            },
            "x\n",
        )
        .unwrap();
        // Do NOT create scripts/ for task_missing.

        // task_ok: has a valid script.
        let scripts_dir_ok = pkg.join("runtime/outputs/task_ok/scripts");
        std::fs::create_dir_all(&scripts_dir_ok).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/task_ok/results.tsv"),
            "x\n",
        )
        .unwrap();
        std::fs::write(
            scripts_dir_ok.join("01.sh"),
            "#!/usr/bin/env bash\necho ok\n",
        )
        .unwrap();

        let tasks = vec![
            ComputeTask {
                task_id: "task_missing".to_string(),
                scripts_dir: pkg.join("runtime/outputs/task_missing/scripts"),
                result_tables: vec!["results.tsv".to_string()],
            },
            ComputeTask {
                task_id: "task_ok".to_string(),
                scripts_dir: scripts_dir_ok.clone(),
                result_tables: vec!["results.tsv".to_string()],
            },
        ];

        let outcomes = stage_and_run(
            pkg,
            scratch,
            &tasks,
            &[],
            &shell_env(),
            "/irrelevant",
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(outcomes.len(), 2);

        let first = &outcomes[0];
        assert_eq!(first.task_id, "task_missing");
        assert!(
            !first.ok,
            "task_missing should be ok:false due to missing scripts dir"
        );
        assert!(
            !first.stderr.is_empty(),
            "task_missing stderr should explain the failure"
        );

        let second = &outcomes[1];
        assert_eq!(second.task_id, "task_ok");
        assert!(
            second.ok,
            "task_ok should still succeed after task_missing failed; stderr: {}",
            second.stderr
        );
    }

    /// A non-"inputs" input label (e.g. "my-inputs") under data_acquisition/data/
    /// must be staged to scratch so the compute script can find its counts file.
    /// Regression: the old code hardcoded "inputs/" and silently skipped any other
    /// label, causing Tier-2 re-execution to fail for packages like Himes.
    #[test]
    fn custom_input_label_subtree_is_staged() {
        let pkg_tmp = tempdir().unwrap();
        let scratch_tmp = tempdir().unwrap();
        let pkg = pkg_tmp.path();
        let scratch = scratch_tmp.path();

        // Build a fake package with a custom label "my-inputs" (not "inputs").
        let custom_label = "my-inputs";
        let data_dir = pkg
            .join("runtime/outputs/data_acquisition/data")
            .join(custom_label);
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("counts.tsv"), "gene\tcount\nGENE1\t42\n").unwrap();

        // Build a compute task whose script asserts the counts file exists under PKG_ROOT.
        let scripts_dir = pkg.join("runtime/outputs/differential_expression/scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/differential_expression/de_results.tsv"),
            "gene\tpadj\n",
        )
        .unwrap();

        // The script checks the file exists and exits non-zero if it doesn't.
        let script_content = format!(
            "#!/usr/bin/env bash\n\
             set -e\n\
             counts=\"$PKG_ROOT/runtime/outputs/data_acquisition/data/{custom_label}/counts.tsv\"\n\
             if [ ! -f \"$counts\" ]; then\n\
               echo \"MISSING: $counts\" >&2\n\
               exit 1\n\
             fi\n\
             mkdir -p \"$PKG_ROOT/runtime/outputs/differential_expression\"\n\
             echo ok > \"$PKG_ROOT/runtime/outputs/differential_expression/done.txt\"\n"
        );
        std::fs::write(scripts_dir.join("01.sh"), &script_content).unwrap();

        let task = ComputeTask {
            task_id: "differential_expression".to_string(),
            scripts_dir: scripts_dir.clone(),
            result_tables: vec!["de_results.tsv".to_string()],
        };

        let outcomes = stage_and_run(
            pkg,
            scratch,
            &[task],
            &[],
            &shell_env(),
            "/irrelevant",
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(outcomes.len(), 1);
        let outcome = &outcomes[0];

        // The counts file must have been staged to scratch.
        let staged_counts = scratch
            .join("runtime/outputs/data_acquisition/data")
            .join(custom_label)
            .join("counts.tsv");
        assert!(
            staged_counts.exists(),
            "counts.tsv under custom label '{custom_label}' must be staged to scratch"
        );

        // The compute task must have run successfully (found its input file).
        assert!(
            outcome.ok,
            "task should succeed when custom-label inputs are staged; stderr: {}",
            outcome.stderr
        );
    }

    /// Important 2 — both `PKG_ROOT` and `PACKAGE` must be injected into the
    /// run environment and set to the scratch root.  Real ECAA scripts (e.g.
    /// the Himes DESeq2 script) read `PACKAGE` via `Sys.getenv("PACKAGE", …)`;
    /// relying on only `PKG_ROOT` leaves that path un-redirected.
    ///
    /// The test script writes the values of both variables to separate files
    /// under the scratch output dir, then we assert both equal the scratch root.
    #[test]
    fn both_pkg_root_and_package_are_injected_into_run_env() {
        let pkg_tmp = tempdir().unwrap();
        let scratch_tmp = tempdir().unwrap();
        let pkg = pkg_tmp.path();
        let scratch = scratch_tmp.path();

        let scripts_dir = pkg.join("runtime/outputs/env_check_task/scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/env_check_task/results.tsv"),
            "x\n",
        )
        .unwrap();

        // Script writes $PKG_ROOT and $PACKAGE to separate files so the test
        // can assert both are set to the scratch root.
        let script = format!(
            "#!/usr/bin/env bash\n\
             set -e\n\
             mkdir -p \"$PKG_ROOT/runtime/outputs/env_check_task\"\n\
             echo -n \"$PKG_ROOT\" > \"$PKG_ROOT/runtime/outputs/env_check_task/pkg_root.txt\"\n\
             echo -n \"$PACKAGE\" > \"$PKG_ROOT/runtime/outputs/env_check_task/package.txt\"\n"
        );
        std::fs::write(scripts_dir.join("01.sh"), &script).unwrap();

        let task = ComputeTask {
            task_id: "env_check_task".to_string(),
            scripts_dir: scripts_dir.clone(),
            result_tables: vec!["results.tsv".to_string()],
        };

        let outcomes = stage_and_run(
            pkg,
            scratch,
            &[task],
            &[],
            &shell_env(),
            "/irrelevant",
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(outcomes.len(), 1);
        let outcome = &outcomes[0];
        assert!(
            outcome.ok,
            "env_check_task should succeed; stderr: {}",
            outcome.stderr
        );

        let scratch_str = scratch.display().to_string();

        let pkg_root_val = std::fs::read_to_string(
            scratch.join("runtime/outputs/env_check_task/pkg_root.txt"),
        )
        .expect("pkg_root.txt must exist");
        assert_eq!(
            pkg_root_val, scratch_str,
            "PKG_ROOT must equal scratch root; got: {pkg_root_val:?}"
        );

        let package_val = std::fs::read_to_string(
            scratch.join("runtime/outputs/env_check_task/package.txt"),
        )
        .expect("package.txt must exist");
        assert_eq!(
            package_val, scratch_str,
            "PACKAGE must equal scratch root; got: {package_val:?}"
        );
    }

    /// A co-located NON-script file in `scripts/` (e.g. the agent's own
    /// `deseq2_run.log` or `00_install.log`) must be IGNORED, not executed.
    /// The recorded runs write logs/manifests alongside the real `.R`/`.py`/`.sh`
    /// scripts; treating those as executable scripts made every such task fail
    /// (unknown-interpreter error → ok:false) even though the real script
    /// succeeded.  Only files with a runnable interpreter extension are executed.
    #[test]
    fn co_located_non_script_files_are_not_executed() {
        let pkg_tmp = tempdir().unwrap();
        let scratch_tmp = tempdir().unwrap();
        let pkg = pkg_tmp.path();
        let scratch = scratch_tmp.path();

        let scripts_dir = pkg.join("runtime/outputs/differential_expression/scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/differential_expression/de_results.tsv"),
            "gene\tpadj\n",
        )
        .unwrap();

        // The real script — succeeds. (The test `Shell` env runs every script
        // via `bash`, so the body is bash; the point is that the `.R` extension
        // is treated as runnable while the `.log`/`.json` below are not.)
        std::fs::write(
            scripts_dir.join("01_run_deseq2.R"),
            "#!/usr/bin/env bash\necho ok\n",
        )
        .unwrap();
        // Co-located artifacts the agent wrote into scripts/ — must be ignored.
        std::fs::write(scripts_dir.join("deseq2_run.log"), "[12:00] running\n").unwrap();
        std::fs::write(scripts_dir.join("manifest.json"), "{}\n").unwrap();

        let task = ComputeTask {
            task_id: "differential_expression".to_string(),
            scripts_dir: scripts_dir.clone(),
            result_tables: vec!["de_results.tsv".to_string()],
        };

        let outcomes = stage_and_run(
            pkg,
            scratch,
            &[task],
            &[],
            &shell_env(),
            "/irrelevant",
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(outcomes.len(), 1);
        let outcome = &outcomes[0];
        assert!(
            outcome.ok,
            "co-located non-script files must be ignored; task should succeed. stderr: {}",
            outcome.stderr
        );
        // The non-script files must NOT have been staged into scratch scripts/
        // as executable scripts (they are not scripts).
        let staged_log = scratch
            .join("runtime/outputs/differential_expression/scripts/deseq2_run.log");
        assert!(
            !staged_log.exists(),
            "a co-located .log must not be staged/executed as a script"
        );
    }

    /// A task whose `scripts/` directory exists but contains no files must
    /// yield `ok: false` with a non-empty stderr reason.  An empty scripts dir
    /// is not a legitimate execution and must never be treated as success.
    #[test]
    fn zero_script_task_is_not_ok() {
        let pkg_tmp = tempdir().unwrap();
        let scratch_tmp = tempdir().unwrap();
        let pkg = pkg_tmp.path();
        let scratch = scratch_tmp.path();

        // Create the scripts dir but leave it empty.
        let scripts_dir = pkg.join("runtime/outputs/empty_task/scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/empty_task/results.tsv"),
            "x\n",
        )
        .unwrap();

        let task = ComputeTask {
            task_id: "empty_task".to_string(),
            scripts_dir: scripts_dir.clone(),
            result_tables: vec!["results.tsv".to_string()],
        };

        let outcomes = stage_and_run(
            pkg,
            scratch,
            &[task],
            &[],
            &shell_env(),
            "/irrelevant",
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(outcomes.len(), 1);
        let outcome = &outcomes[0];
        assert_eq!(outcome.task_id, "empty_task");
        assert!(
            !outcome.ok,
            "a task with zero runnable scripts must not be ok"
        );
        assert!(
            !outcome.stderr.is_empty(),
            "stderr must contain a reason; got empty string"
        );
    }
}
