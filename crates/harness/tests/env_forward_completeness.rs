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
