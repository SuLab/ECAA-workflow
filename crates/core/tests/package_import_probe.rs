use ecaa_workflow_core::package_import::{probe_package_capabilities, PackageTier};
use std::fs;
use std::path::Path;

fn touch(p: &Path) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, b"{}").unwrap();
}

/// Minimal-audit shape: crate substrate + audit sidecars, NO scripts / determinism-env.
fn make_minimal(root: &Path) {
    touch(&root.join("WORKFLOW.json"));
    touch(&root.join("ro-crate-metadata.json"));
    touch(&root.join("runtime/audit-proof-report.json"));
    touch(&root.join("runtime/claim-verification.json"));
    touch(&root.join("runtime/proofs.jsonl"));
    touch(&root.join("runtime/assumptions.jsonl"));
}

#[test]
fn minimal_package_allows_explore_reverify_t1_but_not_t2() {
    let dir = tempfile::tempdir().unwrap();
    make_minimal(dir.path());
    let caps = probe_package_capabilities(dir.path());
    assert!(caps.explore);
    assert!(caps.reverify);
    assert!(caps.replay_tier1);
    assert!(
        !caps.replay_tier2,
        "minimal has no scripts/determinism-env → no Tier-2"
    );
    assert_eq!(caps.tier_label, PackageTier::MinimalAudit);
    assert_eq!(caps.tabs.get("composer_trace"), Some(&false));
    assert_eq!(caps.tabs.get("composition"), Some(&false));
}

#[test]
fn reexecutable_package_allows_tier2() {
    let dir = tempfile::tempdir().unwrap();
    make_minimal(dir.path());
    // re-execution surface for task_a:
    let t = dir.path().join("runtime/outputs/task_a");
    touch(&t.join("scripts/run.py"));
    touch(&t.join("results.tsv"));
    touch(&t.join("determinism-env.json"));
    fs::write(
        t.join("determinism-env.json"),
        br#"{"task_container_digest":"sha256:abc"}"#,
    )
    .unwrap();
    touch(&dir.path().join("runtime/execution-order.json"));
    touch(&dir.path().join("runtime/verifier-decisions.jsonl"));
    let caps = probe_package_capabilities(dir.path());
    assert!(
        caps.replay_tier2,
        "scripts+table+digest+execution-order → Tier-2 available"
    );
    assert_eq!(caps.tier_label, PackageTier::ReExecutable);
    assert_eq!(caps.tabs.get("composer_trace"), Some(&true));
}

#[test]
fn full_package_labeled_full() {
    let dir = tempfile::tempdir().unwrap();
    make_minimal(dir.path());
    touch(&dir.path().join("package.ttl"));
    touch(&dir.path().join("policies/best-practice.json"));
    let caps = probe_package_capabilities(dir.path());
    assert_eq!(caps.tier_label, PackageTier::Full);
}

#[test]
fn non_ecaa_dir_is_not_explorable() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("random.txt"), b"hi").unwrap();
    let caps = probe_package_capabilities(dir.path());
    assert!(!caps.explore);
}
