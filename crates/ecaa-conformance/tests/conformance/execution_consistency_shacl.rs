//! Inv-6 sub-check (execution-consistency, F11) as runnable SHACL.
//!
//! Execution is materialized twice — the authoritative WRROC `@graph`
//! HowToStep lineage and the E sidecar (`proofs.jsonl`) WorkflowStep set.
//! `_project.py` derives one `ecaa:WorkflowStep` node per step tagged with the
//! materialization(s) it appears in; the `ExecutionConsistencyShape` (folded
//! UNDER Invariant 6, not a 7th invariant) flags any step in one
//! materialization but not the other. An agreeing pair PASSES; an extra
//! `@graph` step with no E counterpart FAILS. Probe-skips LOUDLY when the
//! toolchain is absent.

use crate::_shacl_harness::{loud_skip, run_projection, validators_available};

#[test]
fn shacl_passes_when_graph_and_e_agree() {
    if !validators_available() {
        loud_skip("shacl_passes_when_graph_and_e_agree");
        return;
    }
    let (status, stdout, stderr) = run_projection("exec-consistency-ok");
    eprintln!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    assert!(
        status.success(),
        "agreeing @graph/E must exit 0 (got {status:?})"
    );
    assert!(
        stdout.contains("SHACL conformance: PASS"),
        "agreeing @graph/E must PASS:\n{stdout}"
    );
}

#[test]
fn shacl_fails_on_execution_step_drift() {
    if !validators_available() {
        loud_skip("shacl_fails_on_execution_step_drift");
        return;
    }
    let (status, stdout, stderr) = run_projection("exec-consistency-drift");
    eprintln!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    assert!(
        !status.success(),
        "an @graph step absent from E must exit non-zero"
    );
    assert!(
        stdout.contains("SHACL conformance: FAIL"),
        "execution-step drift must FAIL:\n{stdout}"
    );
}
