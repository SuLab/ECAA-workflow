//! Per-task scratch cleanup.
//!
//! The lifecycle for `runtime/scratch/<task_id>/` is:
//! created by `scripts/agent-claude.sh` at task dispatch (line ~250,
//! exported as `SWFC_TASK_SCRATCH_DIR`), used as ephemeral working area
//! by the agent, and removed when the task reaches a terminal state
//! (Completed / Failed). Without this hook the directory persists past
//! the task — visible to a re-dispatched task in the same package, and
//! consuming disk indefinitely when the package never finishes.
//!
//! Cleanup fires from `main::run_loop` after a task transition is
//! observed and persisted; by definition the agent subprocess has
//! exited and no concurrent reader of the scratch dir remains for the
//! same `task_id` on this host (the dispatch WAL guarantees at most
//! one in-flight dispatch per `task_id`).
//!
//! Bypass: `SWFC_SCRATCH_KEEP=1` skips removal for forensic debugging.
//! Any other value (or unset) = cleanup runs.

use std::path::Path;

/// Remove the per-task scratch directory `<package>/runtime/scratch/<task_id>/`.
///
/// Returns `true` if a removal was attempted (directory existed and
/// `SWFC_SCRATCH_KEEP` was not set), `false` if skipped. The boolean
/// is primarily for tests — production callers ignore it. Errors during
/// `remove_dir_all` are logged via `tracing::warn!` and swallowed: a
/// stale scratch directory is undesirable but never load-bearing for
/// task state.
pub fn cleanup_task_scratch(package_root: &Path, task_id: &str) -> bool {
    if ecaa_workflow_core::env_helpers::env_bool("SWFC_SCRATCH_KEEP") {
        return false;
    }
    let scratch = package_root.join("runtime").join("scratch").join(task_id);
    if !scratch.exists() {
        return false;
    }
    match std::fs::remove_dir_all(&scratch) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(
                task_id = %task_id,
                path = %scratch.display(),
                error = %e,
                "failed to clean per-task scratch dir",
            );
            // We still attempted; report true so the caller's log
            // reflects the intent. Callers that distinguish success
            // from attempt-with-error need to check the path again.
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // SWFC_SCRATCH_KEEP is process-global; serialize the env mutators
    // so the parallel test runner doesn't clobber. Each test that
    // touches the env var must hold this lock for its entire duration.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn make_scratch(package_root: &Path, task_id: &str) -> std::path::PathBuf {
        let scratch = package_root.join("runtime").join("scratch").join(task_id);
        std::fs::create_dir_all(&scratch).expect("mkdir scratch");
        std::fs::write(scratch.join("garbage.tmp"), b"x").expect("write garbage");
        scratch
    }

    #[test]
    fn cleans_existing_scratch_dir_on_completion() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("SWFC_SCRATCH_KEEP");
        }
        let pkg = tempfile::tempdir().unwrap();
        let scratch = make_scratch(pkg.path(), "task_alpha");
        assert!(scratch.exists());

        let attempted = cleanup_task_scratch(pkg.path(), "task_alpha");
        assert!(attempted, "should have attempted removal");
        assert!(!scratch.exists(), "scratch dir must be gone");
    }

    #[test]
    fn no_op_when_scratch_dir_absent() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("SWFC_SCRATCH_KEEP");
        }
        let pkg = tempfile::tempdir().unwrap();
        // No scratch dir created. Should silently return false.
        let attempted = cleanup_task_scratch(pkg.path(), "task_missing");
        assert!(!attempted, "no scratch present = no attempt");
    }

    #[test]
    fn preserves_when_keep_env_set() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: tests serialized via ENV_LOCK.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("SWFC_SCRATCH_KEEP", "1");
        }
        let pkg = tempfile::tempdir().unwrap();
        let scratch = make_scratch(pkg.path(), "task_beta");

        let attempted = cleanup_task_scratch(pkg.path(), "task_beta");
        assert!(!attempted, "keep=1 must skip cleanup");
        assert!(scratch.exists(), "scratch dir must survive");

        // Cleanup the env var so other tests don't see it.
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("SWFC_SCRATCH_KEEP");
        }
    }

    #[test]
    fn does_not_remove_other_tasks_scratch() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("SWFC_SCRATCH_KEEP");
        }
        let pkg = tempfile::tempdir().unwrap();
        let scratch_a = make_scratch(pkg.path(), "task_a");
        let scratch_b = make_scratch(pkg.path(), "task_b");

        cleanup_task_scratch(pkg.path(), "task_a");
        assert!(!scratch_a.exists(), "task_a scratch must be gone");
        assert!(scratch_b.exists(), "task_b scratch must survive");
    }
}
