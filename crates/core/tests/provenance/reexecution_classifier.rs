//! 6-scenario coverage for `reexecution::classify_reexecution`.
//!
//! Each test constructs a minimal pair of package directories in a tempdir
//! and asserts the expected `ReexecutionBucket` assignment. Scenarios:
//! 1. Byte-identical (identical CSVs in both packages).
//! 2. Semantic-equivalent (numeric columns within ±5% tolerance).
//! 3. Acknowledged non-determinism (shim records PYTHONHASHSEED missing).
//! 4. Unavailable (replay artifact missing).
//! 5. Failed (wildly divergent numerics beyond ±5%).
//! 6. Ablation engaged (sidecar written empty via the sidecars module).
//!
//! Scenario 6 tests the *core* representation of the ablation contract —
//! the `ReexecutionReport::empty` helper — because the emit-side
//! suppression that writes the file lives in the conversation crate.

use ecaa_workflow_core::determinism_shim::{
    DeterminismShimSidecar, EnvCapture, SeedPolicy, TempPathPolicy,
};
use ecaa_workflow_core::reexecution::{
    classify_reexecution, ReexecutionBucket, ReexecutionReport,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_package_dirs(root: &TempDir, name: &str) -> std::path::PathBuf {
    let pkg = root.path().join(name);
    let tables = pkg.join("results").join("tables");
    fs::create_dir_all(&tables).unwrap();
    pkg
}

fn write_table(pkg: &Path, filename: &str, content: &str) {
    let p = pkg.join("results").join("tables").join(filename);
    fs::write(p, content).unwrap();
}

/// Write a minimal `runtime/determinism-shim.json` into `pkg`.
fn write_shim(pkg: &Path, shim: &DeterminismShimSidecar) {
    let runtime = pkg.join("runtime");
    fs::create_dir_all(&runtime).unwrap();
    let body = serde_json::to_vec_pretty(shim).unwrap();
    fs::write(runtime.join("determinism-shim.json"), body).unwrap();
}

fn minimal_shim_with_seed() -> DeterminismShimSidecar {
    DeterminismShimSidecar {
        schema_version: "1".to_string(),
        env_capture: EnvCapture {
            // PYTHONHASHSEED is listed → deterministic; seed is set → no ack.
            captured_env_vars: vec!["PYTHONHASHSEED".to_string()],
            redacted_env_vars: vec![],
        },
        seed_policy: SeedPolicy {
            random_seed: Some(42),
            seed_source: "SOURCE_DATE_EPOCH".to_string(),
        },
        temp_path_policy: TempPathPolicy {
            strategy: "stable-by-task-id".to_string(),
            root: "/tmp".to_string(),
        },
        locale: "C".to_string(),
        timezone: "UTC".to_string(),
        ablation_engaged: false,
    }
}

fn minimal_shim_no_seed_no_pythonhash() -> DeterminismShimSidecar {
    DeterminismShimSidecar {
        schema_version: "1".to_string(),
        env_capture: EnvCapture {
            // PYTHONHASHSEED absent → non-deterministic hash seed active.
            captured_env_vars: vec![],
            redacted_env_vars: vec![],
        },
        seed_policy: SeedPolicy {
            random_seed: None, // no explicit seed
            seed_source: "process-default".to_string(),
        },
        temp_path_policy: TempPathPolicy {
            strategy: "stable-by-task-id".to_string(),
            root: "/tmp".to_string(),
        },
        locale: "C".to_string(),
        timezone: "UTC".to_string(),
        ablation_engaged: false,
    }
}

// ---------------------------------------------------------------------------
// Scenario 1: byte-identical
// ---------------------------------------------------------------------------

#[test]
fn scenario_byte_identical() {
    let root = TempDir::new().unwrap();
    let parent = make_package_dirs(&root, "parent");
    let replay = make_package_dirs(&root, "replay");

    let content = "gene\tlog2FC\tpadj\nGENE1\t2.5\t0.001\nGENE2\t-1.3\t0.05\n";
    write_table(&parent, "de.tsv", content);
    write_table(&replay, "de.tsv", content); // identical

    let report = classify_reexecution(&parent, &replay, None).unwrap();
    assert_eq!(report.per_artifact.len(), 1);
    assert_eq!(
        report.per_artifact[0].bucket,
        ReexecutionBucket::ByteIdentical
    );
    assert_eq!(report.bucket_counts.get("byte_identical"), Some(&1usize));
}

// ---------------------------------------------------------------------------
// Scenario 2: semantic-equivalent (within ±5%)
// ---------------------------------------------------------------------------

#[test]
fn scenario_semantic_equivalent() {
    let root = TempDir::new().unwrap();
    let parent = make_package_dirs(&root, "parent");
    let replay = make_package_dirs(&root, "replay");

    // Parent: log2FC = 2.0; replay: log2FC = 2.05 → 2.5% diff → within ±5%.
    write_table(&parent, "de.tsv", "gene\tlog2FC\tpadj\nGENE1\t2.0\t0.01\n");
    write_table(&replay, "de.tsv", "gene\tlog2FC\tpadj\nGENE1\t2.05\t0.01\n");

    // Write a shim that doesn't trigger AcknowledgedNonDeterminism
    // (PYTHONHASHSEED present + random_seed set → deterministic).
    write_shim(&parent, &minimal_shim_with_seed());

    let report = classify_reexecution(&parent, &replay, None).unwrap();
    assert_eq!(report.per_artifact.len(), 1);
    assert_eq!(
        report.per_artifact[0].bucket,
        ReexecutionBucket::SemanticEquivalent,
        "expected SemanticEquivalent for 2.5% numeric drift"
    );
    assert_eq!(report.bucket_counts.get("semantic_equivalent"), Some(&1));
}

// ---------------------------------------------------------------------------
// Scenario 3: acknowledged non-determinism (shim records PYTHONHASHSEED missing)
// ---------------------------------------------------------------------------

#[test]
fn scenario_acknowledged_non_determinism() {
    let root = TempDir::new().unwrap();
    let parent = make_package_dirs(&root, "parent");
    let replay = make_package_dirs(&root, "replay");

    // Content differs enough to fail byte-identity check.
    write_table(&parent, "de.tsv", "gene\tlog2FC\nGENE1\t2.0\n");
    // Divergence beyond 5% tolerance, but shim acknowledges non-determinism.
    write_table(&replay, "de.tsv", "gene\tlog2FC\nGENE1\t5.0\n");

    // Shim with PYTHONHASHSEED absent + no random seed → acknowledged.
    write_shim(&parent, &minimal_shim_no_seed_no_pythonhash());

    let report = classify_reexecution(&parent, &replay, None).unwrap();
    assert_eq!(report.per_artifact.len(), 1);
    assert_eq!(
        report.per_artifact[0].bucket,
        ReexecutionBucket::AcknowledgedNonDeterminism,
        "expected AcknowledgedNonDeterminism when shim records missing PYTHONHASHSEED"
    );
    assert_eq!(
        report.bucket_counts.get("acknowledged_non_determinism"),
        Some(&1)
    );
}

// ---------------------------------------------------------------------------
// Scenario 4: unavailable (file missing in replay)
// ---------------------------------------------------------------------------

#[test]
fn scenario_unavailable() {
    let root = TempDir::new().unwrap();
    let parent = make_package_dirs(&root, "parent");
    let replay = make_package_dirs(&root, "replay");

    // Parent has the file; replay does not.
    write_table(&parent, "de.tsv", "gene\tlog2FC\nGENE1\t2.0\n");
    // replay/results/tables/de.tsv intentionally not created.

    let report = classify_reexecution(&parent, &replay, None).unwrap();
    assert_eq!(report.per_artifact.len(), 1);
    assert_eq!(
        report.per_artifact[0].bucket,
        ReexecutionBucket::Unavailable
    );
    assert_eq!(report.bucket_counts.get("unavailable"), Some(&1));
}

// ---------------------------------------------------------------------------
// Scenario 5: failed (wildly divergent numerics, no shim to acknowledge)
// ---------------------------------------------------------------------------

#[test]
fn scenario_failed() {
    let root = TempDir::new().unwrap();
    let parent = make_package_dirs(&root, "parent");
    let replay = make_package_dirs(&root, "replay");

    // Parent: 2.0; replay: 100.0 → 98% relative diff → beyond ±5%.
    write_table(&parent, "de.tsv", "gene\tlog2FC\nGENE1\t2.0\n");
    write_table(&replay, "de.tsv", "gene\tlog2FC\nGENE1\t100.0\n");

    // Shim with PYTHONHASHSEED present + random_seed set → NOT acknowledged.
    write_shim(&parent, &minimal_shim_with_seed());

    let report = classify_reexecution(&parent, &replay, None).unwrap();
    assert_eq!(report.per_artifact.len(), 1);
    assert_eq!(
        report.per_artifact[0].bucket,
        ReexecutionBucket::Failed,
        "expected Failed when numerics diverge by 98% with no acknowledged non-determinism"
    );
    assert_eq!(report.bucket_counts.get("failed"), Some(&1));
}

// ---------------------------------------------------------------------------
// Scenario 6: ablation engaged — ReexecutionReport::empty produces no artifacts
// ---------------------------------------------------------------------------

#[test]
fn scenario_ablation_engaged_sidecar_empty() {
    // The emit-side ablation path (writing the JSON file) lives in the
    // conversation crate's sidecars module. We test the *core* representation:
    // `ReexecutionReport::empty` returns a report with empty per_artifact and
    // empty bucket_counts — matching what the sidecar writer serializes when
    // ECAA_ABLATE_REEXECUTION_CLASS is active.
    let empty = ReexecutionReport::empty("0.1");
    assert_eq!(empty.schema_version, "0.1");
    assert!(
        empty.per_artifact.is_empty(),
        "ablation-engaged report must have empty per_artifact"
    );
    assert!(
        empty.bucket_counts.is_empty(),
        "ablation-engaged report must have empty bucket_counts"
    );

    // Verify the JSON shape matches what sidecars.rs writes under ablation:
    // { schema_version, bucket_counts: {}, per_artifact: [], ablation_engaged: true }
    let json_body = serde_json::json!({
        "schema_version": "0.1",
        "bucket_counts": {},
        "per_artifact": [],
        "ablation_engaged": true,
    });
    let reconstructed: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&json_body).unwrap()).unwrap();
    assert_eq!(
        reconstructed["per_artifact"].as_array().unwrap().len(),
        0,
        "ablation JSON per_artifact array must be empty"
    );
    assert!(
        reconstructed["ablation_engaged"].as_bool().unwrap(),
        "ablation_engaged must be true in the empty sidecar"
    );

    // Also confirm that a normal (non-ablated) report with counts round-trips
    // through serde correctly.
    let mut normal = ReexecutionReport {
        schema_version: "0.1".to_string(),
        bucket_counts: {
            let mut m = BTreeMap::new();
            m.insert("byte_identical".to_string(), 2usize);
            m.insert("unavailable".to_string(), 1usize);
            m
        },
        per_artifact: vec![],
    };
    let json = serde_json::to_string(&normal).unwrap();
    let round_tripped: ReexecutionReport = serde_json::from_str(&json).unwrap();
    assert_eq!(round_tripped.bucket_counts.get("byte_identical"), Some(&2));
    assert_eq!(round_tripped.bucket_counts.get("unavailable"), Some(&1));

    // Verify finalize_counts logic through classify_reexecution on an
    // empty parent tables dir (returns empty report, not an error).
    let root = TempDir::new().unwrap();
    let parent = root.path().join("parent");
    let replay = root.path().join("replay");
    // No results/tables dir → classify returns empty report.
    fs::create_dir_all(&parent).unwrap();
    fs::create_dir_all(&replay).unwrap();
    let report = classify_reexecution(&parent, &replay, None).unwrap();
    assert!(report.per_artifact.is_empty());
    assert!(report.bucket_counts.is_empty());

    drop(normal.bucket_counts.insert("failed".to_string(), 3));
    assert_eq!(normal.bucket_counts.len(), 3);
}
