//! Revives the TODO at scripts/_agent-blas-bootstrap.sh:96 — the agent
//! env-forward allowlist must be a superset of every key the harness
//! stamps onto the per-task env. A key stamped but not forwarded is
//! silent behavioral degradation across the container boundary.

use std::fs;
use std::path::PathBuf;

fn bootstrap_script() -> String {
    let p =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/_agent-blas-bootstrap.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn determinism_seed_keys_are_in_forward_allowlist() {
    let script = bootstrap_script();
    // These are exactly the keys core::determinism_seeds stamps.
    for key in [
        "PYTHONHASHSEED",
        "SOURCE_DATE_EPOCH",
        "TZ",
        "LANG",
        "LC_ALL",
    ] {
        assert!(
            script.contains(key),
            "scripts/_agent-blas-bootstrap.sh forward allowlist is missing {key} — \
             stamp_determinism_env sets it but the container won't see it"
        );
    }
}

#[test]
fn dispatch_identity_keys_are_in_forward_allowlist() {
    let script = bootstrap_script();
    for key in ["ECAA_HARNESS_RUN_ID", "ECAA_DISPATCH_EPOCH"] {
        assert!(script.contains(key), "forward allowlist missing {key}");
    }
}

#[test]
fn provenance_keys_are_in_forward_allowlist() {
    // stamp_provenance_env sets these onto the per-task env; the
    // plotting subprocess (spawned by the agent inside the container)
    // reads them for the figure footer. If the container forward list
    // drops them the footer silently degrades to `git@unknown` (RP-7).
    let script = bootstrap_script();
    for key in ["ECAA_GIT_SHA", "ECAA_PACKAGE_ID"] {
        assert!(
            script.contains(key),
            "scripts/_agent-blas-bootstrap.sh forward allowlist is missing {key} — \
             stamp_provenance_env sets it but the container (and the plotting \
             subprocess) won't see it, so figure footers stamp git@unknown"
        );
    }
}

fn agent_claude_script() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/agent-claude.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The thread-budget key list exists as one Rust const
/// (`determinism_shim::THREAD_BUDGET_ENV_VARS`, aliased by the harness) and as
/// three shell literals: the bootstrap script's apply loop, its container
/// forward allowlist, and `agent-claude.sh`'s determinism-env capture loop.
/// Shell can't import the const, so these tests are the anti-drift guard: a key
/// added to the Rust list but not to a script is silent degradation — the
/// harness would declare a thread budget the container never receives, or
/// receives but never records for replay.
#[test]
fn thread_budget_keys_are_in_bootstrap_script() {
    let script = bootstrap_script();
    for key in ecaa_workflow_core::determinism_shim::THREAD_BUDGET_ENV_VARS {
        assert!(
            script.contains(key),
            "scripts/_agent-blas-bootstrap.sh is missing {key} — it is declared in \
             determinism_shim::THREAD_BUDGET_ENV_VARS but the container won't see it"
        );
    }
}

#[test]
fn thread_budget_keys_are_recorded_by_the_agent_wrapper() {
    let script = agent_claude_script();
    // Scope the assertion to the determinism-env capture loop so a mention
    // elsewhere in the script can't satisfy it.
    let block = script
        .split_once("_DET_THREADS=\"\"")
        .and_then(|(_, rest)| rest.split_once("_DET_THREADS=\"$(printf"))
        .map(|(block, _)| block)
        .expect("agent-claude.sh determinism-env thread_budget capture loop");
    for key in ecaa_workflow_core::determinism_shim::THREAD_BUDGET_ENV_VARS {
        assert!(
            block.contains(key),
            "scripts/agent-claude.sh does not record {key} into determinism-env.json \
             thread_budget — replay cannot re-inject a value it never captured"
        );
    }
}
