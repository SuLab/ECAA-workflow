//! In-loop stale-Running reset gating (C1 / H8).
//!
//! The in-loop reset (`run_loop`) resets a `Running` task to `Ready` when
//! the active executor's `is_task_stale` timestamp threshold fires. That
//! reset alone is a double-dispatch hazard: a detached-compute task
//! (Seurat CCA, BPCells) whose agent-side bash keeps touching
//! `.heartbeat` every 30s can exceed `task_timeout` while still actively
//! progressing. Gate the reset on the SAME `LivenessProbe` the WAL
//! orphan recovery uses (`.heartbeat` mtime + `.agent-pid` kill(0)) so a
//! live detached task is left Running, not re-dispatched.
use crate::dispatch_wal::LivenessProbe;

/// Decide whether a `Running` task the executor flagged as timeout-stale
/// should actually be reset to `Ready`. `timeout_elapsed` is the
/// `Executor::is_task_stale` verdict. Returns `false` (keep Running) when
/// the liveness probe reports a fresh heartbeat — the agent is alive and
/// re-dispatch would race two agents on the same task.
pub fn should_reset_stale_running(
    task_id: &str,
    timeout_elapsed: bool,
    probe: &dyn LivenessProbe,
) -> bool {
    if !timeout_elapsed {
        return false;
    }
    !probe.is_live(task_id)
}
