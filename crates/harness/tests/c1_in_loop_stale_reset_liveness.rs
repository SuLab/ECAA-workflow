//! C1 (H8): the in-loop stale-Running reset must consult the same
//! LivenessProbe the WAL recovery uses, so a stale-by-timeout task whose
//! detached agent is still touching .heartbeat is NOT reset to Ready and
//! double-dispatched.
use ecaa_workflow_harness::dispatch_wal::LivenessProbe;
use ecaa_workflow_harness::stale_reset::should_reset_stale_running;
use std::collections::HashSet;

struct MockProbe {
    live_ids: HashSet<String>,
}
impl LivenessProbe for MockProbe {
    fn is_live(&self, task_id: &str) -> bool {
        self.live_ids.contains(task_id)
    }
}

#[test]
fn stale_timeout_but_live_heartbeat_is_not_reset() {
    let probe = MockProbe {
        live_ids: ["seurat_cca".to_string()].into_iter().collect(),
    };
    // timeout_elapsed == true (executor.is_task_stale fired) but probe live
    assert!(
        !should_reset_stale_running("seurat_cca", true, &probe),
        "a stale-by-timeout task with a fresh heartbeat must NOT be reset"
    );
}

#[test]
fn stale_timeout_and_dead_heartbeat_is_reset() {
    let probe = MockProbe {
        live_ids: HashSet::new(),
    };
    assert!(
        should_reset_stale_running("alignment", true, &probe),
        "a stale-by-timeout task with no heartbeat must be reset to Ready"
    );
}

#[test]
fn not_timed_out_is_never_reset_regardless_of_probe() {
    let probe = MockProbe {
        live_ids: HashSet::new(),
    };
    assert!(
        !should_reset_stale_running("normalize", false, &probe),
        "a task the executor does not consider stale must never be reset"
    );
}
