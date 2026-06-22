//! CLI integration test: `ecaa-workflow export`.
//!
//! Stages a tiny fixture package in a tempdir (exercising tier A/B keep +
//! tier C/E drop), runs `export --package <DIR> --out <FILE.zip>`, and
//! asserts the produced `.zip` exists, re-opens as a valid archive, carries
//! the tier-A `ro-crate-metadata.json`, and carries NO `*.log` (tier C) or
//! `runtime/cache/` (tier E) entry.

use assert_cmd::Command;
use predicates::str;

/// Write a file, creating parent dirs first.
fn write_file(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dirs");
    }
    std::fs::write(path, contents).expect("write fixture file");
}

/// Lay down a minimal package exercising each deposit tier.
fn build_fixture_package(root: &std::path::Path) {
    write_file(
        &root.join("runtime/outputs/t/de_results.tsv"),
        "gene\tpadj\nA\t0.01\n",
    ); // A
    write_file(&root.join("runtime/outputs/t/agent-claude.log"), "log line\n"); // C
    write_file(&root.join("runtime/cache/x/y"), "cached bytes\n"); // E
    write_file(&root.join("runtime/outputs/t/scripts/s.R"), "print('hi')\n"); // B
    write_file(&root.join("manifest-sha512.txt"), "stale manifest\n"); // D
    write_file(
        &root.join("ro-crate-metadata.json"),
        r#"{"@context":"https://w3id.org/ro/crate/1.1/context","@graph":[{"@id":"ro-crate-metadata.json","@type":"CreativeWork","about":{"@id":"./"}},{"@id":"./","@type":"Dataset","hasPart":[]}]}"#,
    ); // A
    write_file(
        &root.join("bagit.txt"),
        "BagIt-Version: 1.0\nTag-File-Character-Encoding: UTF-8\n",
    );
}

#[test]
fn export_help_succeeds() {
    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .args(["export", "--help"])
        .assert()
        .success()
        .stdout(str::contains("--package"))
        .stdout(str::contains("--out"));
}

#[test]
fn export_writes_zip_with_kept_tiers_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pkg = tmp.path().join("pkg");
    let out = tmp.path().join("deposit.zip");
    build_fixture_package(&pkg);

    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .args([
            "export",
            "--package",
            pkg.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .success();

    // The .zip exists on disk.
    assert!(
        out.is_file(),
        "export must write the --out zip at {}",
        out.display()
    );

    // It re-opens as a valid archive.
    let file = std::fs::File::open(&out).expect("open out zip");
    let mut archive = zip::ZipArchive::new(file).expect("out is a valid zip archive");

    let names: Vec<String> = (0..archive.len())
        .map(|i| {
            archive
                .by_index(i)
                .expect("zip entry by index")
                .name()
                .to_string()
        })
        .collect();

    // Tier-A ro-crate-metadata.json is present.
    assert!(
        names.iter().any(|n| n == "ro-crate-metadata.json"),
        "zip must contain ro-crate-metadata.json; entries: {names:?}"
    );

    // No tier-C log entry.
    assert!(
        !names.iter().any(|n| n.ends_with(".log")),
        "zip must NOT contain any *.log entry; entries: {names:?}"
    );

    // No tier-E runtime/cache/ entry.
    assert!(
        !names.iter().any(|n| n.starts_with("runtime/cache/")),
        "zip must NOT contain any runtime/cache/ entry; entries: {names:?}"
    );
}
