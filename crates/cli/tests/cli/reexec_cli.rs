//! CLI integration test: `ecaa-workflow reexec`.
//!
//! Stages two tempdir packages — a parent with a result table under
//! `runtime/outputs/` and a byte-identical replay — then drives `reexec`
//! and asserts that `runtime/reexecution.json` is written with a non-empty
//! `per_artifact`. Also covers the `--into` redirect, the help surface, and
//! (mirroring `replay_cli.rs`'s fixture style) the `--reseal` fold-back path
//! against the `cross-graph-ok` conformance fixture.

use assert_cmd::Command;
use predicates::str;

/// Write a file, creating parent dirs first.
fn write_file(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dirs");
    }
    std::fs::write(path, contents).expect("write fixture file");
}

/// Recursively copy `src` into `dst` (mirrors `replay_cli.rs`'s helper).
fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).expect("create_dir_all");
    for entry in std::fs::read_dir(src).expect("read_dir src") {
        let entry = entry.expect("entry");
        let ty = entry.file_type().expect("file_type");
        let dest = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dest);
        } else {
            std::fs::copy(&entry.path(), &dest).expect("copy file");
        }
    }
}

/// Copy the named conformance fixture into `dst`.
fn copy_fixture(name: &str, dst: &std::path::Path) {
    let fixtures_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ecaa-conformance/tests/fixtures")
        .join(name);
    copy_dir_all(&fixtures_root, dst);
}

/// Seed `<pkg>/runtime/audit-proof-report.json` with a deliberately synthetic
/// `claim_completeness` verdict (`n_inspected`/`n_violations` values no real
/// check would ever produce). `--reseal` only refreshes `equivalence_failure`
/// + `substrate_validity` (see `reseal()` in `crates/cli/src/reexec.rs`), so a
/// post-run match on these exact values proves the verdict was PRESERVED
/// verbatim, not recomputed.
fn seed_audit_report(pkg: &std::path::Path) {
    let runtime = pkg.join("runtime");
    std::fs::create_dir_all(&runtime).expect("create runtime dir");
    let report = serde_json::json!({
        "schema_version": "0.1",
        "ecaa_version": "0.2",
        "min_reader_version": "0.2",
        "evaluator": {
            "impl": "ecaa-workflow-audit-proof",
            "version": "0.1.0",
            "policy": "warn-only"
        },
        "verdicts": [
            {
                "id": "claim_completeness",
                "status": "warn",
                "detail": "seed-marker: must survive --reseal unchanged",
                "n_inspected": 12345,
                "n_violations": 6789
            }
        ]
    });
    std::fs::write(
        runtime.join("audit-proof-report.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .expect("write seeded audit-proof-report.json");
}

#[test]
fn reexec_help_succeeds() {
    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .args(["reexec", "--help"])
        .assert()
        .success()
        .stdout(str::contains("--parent"))
        .stdout(str::contains("--replay"));
}

#[test]
fn reexec_writes_report_with_nonempty_per_artifact() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let parent = tmp.path().join("parent");
    let replay = tmp.path().join("replay");
    let rel = "runtime/outputs/differential_expression/de_results.tsv";
    let body = "gene\tlog2fc\tpadj\nGENE1\t1.5\t0.01\nGENE2\t-2.0\t0.04\n";
    write_file(&parent.join(rel), body);
    // Byte-identical replay copy → every artifact passes → exit 0.
    write_file(&replay.join(rel), body);

    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .args([
            "reexec",
            "--parent",
            parent.to_str().unwrap(),
            "--replay",
            replay.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Report written under --replay (no --into given).
    let report_path = replay.join("runtime").join("reexecution.json");
    let raw = std::fs::read_to_string(&report_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", report_path.display()));
    let json: serde_json::Value =
        serde_json::from_str(&raw).expect("reexecution.json is valid JSON");
    let per_artifact = json
        .get("per_artifact")
        .and_then(|v| v.as_array())
        .expect("per_artifact array present");
    assert!(
        !per_artifact.is_empty(),
        "per_artifact must be non-empty, got: {raw}"
    );
    assert_eq!(
        per_artifact[0].get("bucket").and_then(|b| b.as_str()),
        Some("byte_identical"),
        "identical table must be byte_identical, got: {raw}"
    );
}

#[test]
fn reexec_into_redirects_report_destination() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let parent = tmp.path().join("parent");
    let replay = tmp.path().join("replay");
    let into = tmp.path().join("into-pkg");
    let rel = "results/tables/x.tsv";
    let body = "gene\tvalue\nGENE1\t42\n";
    write_file(&parent.join(rel), body);
    write_file(&replay.join(rel), body);

    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .args([
            "reexec",
            "--parent",
            parent.to_str().unwrap(),
            "--replay",
            replay.to_str().unwrap(),
            "--into",
            into.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Report lands under --into, not --replay.
    assert!(
        into.join("runtime").join("reexecution.json").exists(),
        "report must be written under --into"
    );
    assert!(
        !replay.join("runtime").join("reexecution.json").exists(),
        "report must NOT be written under --replay when --into is given"
    );
}

#[test]
fn reexec_no_tables_exits_nonzero() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let parent = tmp.path().join("parent");
    let replay = tmp.path().join("replay");
    // Neither package has any result tables → empty report → non-zero exit.
    std::fs::create_dir_all(&parent).expect("mkdir parent");
    std::fs::create_dir_all(&replay).expect("mkdir replay");

    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .args([
            "reexec",
            "--parent",
            parent.to_str().unwrap(),
            "--replay",
            replay.to_str().unwrap(),
        ])
        .assert()
        .failure();
}

/// `--reseal` must (a) write a populated `runtime/reexecution.json`, (b)
/// re-record `runtime/audit-proof-report.json` — refreshing
/// `equivalence_failure` from the fresh re-execution result while PRESERVING
/// the pre-existing `claim_completeness` verdict verbatim (the CLI's `reseal`
/// scopes the refresh to `equivalence_failure` + `substrate_validity`, per
/// `crates/cli/src/reexec.rs`) — and (c) regenerate `AUDIT-REPORT.md` with a
/// re-execution section. Run with no `ECAA_CONFORMANCE_MODE` (Noop WRROC
/// validator, no network/runcrate) and no `ECAA_AUDIT_SECRET` (legacy
/// stub-only claim path), matching an offline operator invocation.
#[test]
fn reexec_reseal_records_reexecution_and_regenerates_audit_report() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pkg = tmp.path().join("pkg");
    let replay = tmp.path().join("replay");
    std::fs::create_dir_all(&pkg).expect("mkdir pkg");

    copy_fixture("cross-graph-ok", &pkg);
    seed_audit_report(&pkg);

    let rel = "runtime/outputs/differential_expression/de_results.tsv";
    let body = "gene\tlog2fc\tpadj\nGENE1\t1.5\t0.01\nGENE2\t-2.0\t0.04\n";
    write_file(&pkg.join(rel), body);
    // Byte-identical replay copy → the sole artifact lands in the passing
    // byte_identical bucket → equivalence_failure must PASS (no divergence
    // to acknowledge) and the overall command must exit 0.
    write_file(&replay.join(rel), body);

    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .env_remove("ECAA_CONFORMANCE_MODE")
        .env_remove("ECAA_AUDIT_SECRET")
        .args([
            "reexec",
            "--parent",
            pkg.to_str().unwrap(),
            "--replay",
            replay.to_str().unwrap(),
            "--into",
            pkg.to_str().unwrap(),
            "--reseal",
        ])
        .assert()
        .success();

    // (a) runtime/reexecution.json exists and is populated.
    let reexec_path = pkg.join("runtime").join("reexecution.json");
    let reexec_raw = std::fs::read_to_string(&reexec_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", reexec_path.display()));
    let reexec_json: serde_json::Value =
        serde_json::from_str(&reexec_raw).expect("reexecution.json is valid JSON");
    let per_artifact = reexec_json
        .get("per_artifact")
        .and_then(|v| v.as_array())
        .expect("per_artifact array present");
    assert!(
        !per_artifact.is_empty(),
        "reexecution.json must be populated, got: {reexec_raw}"
    );
    assert_eq!(
        per_artifact[0].get("bucket").and_then(|b| b.as_str()),
        Some("byte_identical"),
        "identical table must be byte_identical, got: {reexec_raw}"
    );

    // (b) audit-proof-report.json was re-recorded: still valid JSON, parses
    // as the report shape, equivalence_failure reflects the fresh
    // re-execution, and claim_completeness was preserved (not regraded).
    let audit_path = pkg.join("runtime").join("audit-proof-report.json");
    let audit_raw = std::fs::read_to_string(&audit_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", audit_path.display()));
    let audit_json: serde_json::Value =
        serde_json::from_str(&audit_raw).expect("audit-proof-report.json is valid JSON");
    let verdicts = audit_json
        .get("verdicts")
        .and_then(|v| v.as_array())
        .expect("verdicts array present");

    let equivalence = verdicts
        .iter()
        .find(|v| v.get("id").and_then(|i| i.as_str()) == Some("equivalence_failure"))
        .unwrap_or_else(|| panic!("equivalence_failure verdict must be present, got: {audit_raw}"));
    assert_eq!(
        equivalence.get("status").and_then(|s| s.as_str()),
        Some("pass"),
        "equivalence_failure must PASS when re-execution had no unacknowledged \
         divergence, got: {audit_raw}"
    );

    let claim_completeness = verdicts
        .iter()
        .find(|v| v.get("id").and_then(|i| i.as_str()) == Some("claim_completeness"))
        .unwrap_or_else(|| panic!("claim_completeness verdict must be present, got: {audit_raw}"));
    assert_eq!(
        claim_completeness.get("status").and_then(|s| s.as_str()),
        Some("warn"),
        "claim_completeness must be PRESERVED verbatim by a scoped reseal, got: {audit_raw}"
    );
    assert_eq!(
        claim_completeness
            .get("n_inspected")
            .and_then(|n| n.as_u64()),
        Some(12345),
        "the seeded synthetic n_inspected must survive --reseal unchanged, got: {audit_raw}"
    );
    assert_eq!(
        claim_completeness.get("detail").and_then(|d| d.as_str()),
        Some("seed-marker: must survive --reseal unchanged"),
        "the seeded detail must survive --reseal unchanged, got: {audit_raw}"
    );

    // (c) AUDIT-REPORT.md exists / was regenerated and covers re-execution.
    let md_path = pkg.join("AUDIT-REPORT.md");
    let md = std::fs::read_to_string(&md_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", md_path.display()));
    assert!(
        md.contains("re-execution"),
        "AUDIT-REPORT.md must contain the re-execution-equivalence section, got: {md}"
    );
}

/// A hard `failed` bucket (an unacknowledged numeric divergence beyond the
/// per-modality semantic-equivalence bounds, with no determinism-shim source
/// declared) must still make `--reseal` exit non-zero — only `unavailable`
/// artifacts are tolerated alongside a successful reseal, per the exit-code
/// contract documented at the top of `crates/cli/src/reexec.rs`.
#[test]
fn reexec_reseal_still_fails_on_hard_failed_bucket() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pkg = tmp.path().join("pkg");
    let replay = tmp.path().join("replay");
    std::fs::create_dir_all(&pkg).expect("mkdir pkg");

    copy_fixture("cross-graph-ok", &pkg);
    seed_audit_report(&pkg);

    let rel = "runtime/outputs/differential_expression/de_results.tsv";
    // No determinism-shim.json is present in the fixture, and the replay
    // value diverges by 10x — far beyond the default ±5% relative band with
    // no absolute slack — so classify_reexecution has no acknowledged source
    // to fall back on and must land this artifact in ReexecutionBucket::Failed.
    write_file(&pkg.join(rel), "gene\tvalue\nGENE1\t100.0\n");
    write_file(&replay.join(rel), "gene\tvalue\nGENE1\t1000.0\n");

    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .env_remove("ECAA_CONFORMANCE_MODE")
        .env_remove("ECAA_AUDIT_SECRET")
        .args([
            "reexec",
            "--parent",
            pkg.to_str().unwrap(),
            "--replay",
            replay.to_str().unwrap(),
            "--into",
            pkg.to_str().unwrap(),
            "--reseal",
        ])
        .assert()
        .failure();

    // The reseal fold-back still runs before the exit-code decision, so the
    // audit trail is refreshed even on this failing path.
    let reexec_raw = std::fs::read_to_string(pkg.join("runtime").join("reexecution.json"))
        .expect("reexecution.json must still be written on the failing path");
    assert!(
        reexec_raw.contains("\"failed\""),
        "the diverging table must classify as failed, got: {reexec_raw}"
    );
}
