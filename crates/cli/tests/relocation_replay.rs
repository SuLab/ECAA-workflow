//! Relocation replay — a deposit must survive being MOVED.
//!
//! The property under test is the one a Zenodo/Dryad downloader exercises on
//! first contact: the deposit they unpack lands at a path that has nothing to
//! do with the compiling operator's machine layout. Two things must hold at
//! the new location:
//!
//! 1. **No absolute host path survives in the executable surface.** Every file
//!    under `runtime/outputs/<task>/scripts/` is rewritten by
//!    `ecaa-workflow export` into a self-locating form (the relocation
//!    prologue binds `PKG_ROOT` from the environment, else by walking up from
//!    the script's OWN directory). A script that still spells out
//!    `/home/<operator>/.ecaa-workflow/packages/<id>` silently reads and
//!    writes back into the compiling machine's tree — or, off that machine,
//!    fails on a path that does not exist.
//! 2. **The offline verify still passes.** BagIt checksums are computed over
//!    deposit-RELATIVE paths and the RO-Crate content hashes over file bytes,
//!    so neither may depend on where the tree sits. `validate_deposit_tier1`
//!    is the same Layer-1 gate `export` itself runs; re-running it after the
//!    move is the assertion that the seal is location-independent.
//!
//! The fixture is deliberately modality-agnostic (`analysis_stage`, a generic
//! `results_table.tsv`) and covers all three script languages the relocation
//! prologue knows how to emit (R / Python / shell) plus a non-code transcript
//! under `scripts/`, which is tokenized rather than rewritten.
//!
//! Scope note: this test is written to the INTENDED post-fix behaviour of the
//! export-time relocation. The generalization of that rewrite beyond the
//! per-task `scripts/` surface is in flight; assertions here are confined to
//! `runtime/outputs/*/scripts/*` plus the deposit-wide portability scan, which
//! is the contract this test is meant to lock.

use std::path::{Path, PathBuf};

use assert_cmd::Command;

/// Task id every fixture artifact hangs off. Generic on purpose — nothing here
/// encodes differential expression (or any other) shape.
const TASK: &str = "analysis_stage";

/// Write `path` with `contents`, creating parent directories first.
fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dirs");
    }
    std::fs::write(path, contents).expect("write fixture file");
}

/// Canonical absolute spelling of `p`, matching what the exporter records as
/// the package root (`canonical_root_string`).
fn canonical(p: &Path) -> String {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
}

/// Stage a small completed package at `root` whose per-task scripts bake the
/// package's own absolute path in — the shape a real agent-authored script has
/// after execution.
///
/// The absolute root appears in four distinct syntactic positions, because the
/// relocation has to handle each differently: an R string literal, a Python
/// string literal, a shell variable assignment, and free prose in a
/// non-executable transcript.
fn build_fixture_package(root: &Path) {
    let abs = canonical(root);

    // ── Root-level crate + BagIt tags the exporter reseals over ──────────
    write_file(
        &root.join("ro-crate-metadata.json"),
        r#"{"@context":"https://w3id.org/ro/crate/1.1/context","@graph":[{"@id":"ro-crate-metadata.json","@type":"CreativeWork","about":{"@id":"./"}},{"@id":"./","@type":"Dataset","hasPart":[]}]}"#,
    );
    write_file(
        &root.join("bagit.txt"),
        "BagIt-Version: 1.0\nTag-File-Character-Encoding: UTF-8\n",
    );
    // A non-`workflow-<uuid>` id keeps the portability scan on its host-path
    // axis only (the session-id axis does not apply to a CLI-built package).
    write_file(
        &root.join("WORKFLOW.json"),
        r#"{"workflow_id":"relocation-fixture","tasks":{}}"#,
    );
    write_file(&root.join("manifest-sha512.txt"), "stale manifest\n");

    // ── Task outputs ─────────────────────────────────────────────────────
    let out = root.join("runtime/outputs").join(TASK);
    write_file(
        &out.join("results_table.tsv"),
        "feature\tvalue\nfeature_a\t1.5\nfeature_b\t0.25\n",
    );
    write_file(&out.join("result.json"), r#"{"status":"completed"}"#);

    // The recorded package root the exporter reads to build its substitution
    // table. In production the agent writes this from inside the container.
    write_file(
        &out.join("determinism-env.json"),
        &format!(
            r#"{{"schema_version":"1","source_date_epoch":"1784937600","lang":"C.UTF-8","lc_all":"C.UTF-8","tz":"UTC","pythonhashseed":"0","pkg_root":"{abs}"}}"#
        ),
    );

    // R: string-literal splice + a bare interpolation in a comment.
    write_file(
        &out.join("scripts/01_analysis.R"),
        &format!(
            "#!/usr/bin/env Rscript\n\
             # Reads from {abs}/runtime/outputs/{TASK}\n\
             PKG <- \"{abs}\"\n\
             tbl <- read.delim(file.path(PKG, \"runtime/outputs/{TASK}/results_table.tsv\"))\n\
             setwd(\"{abs}\")\n\
             write.csv(tbl, \"{abs}/runtime/outputs/{TASK}/summary.csv\")\n"
        ),
    );

    // Python: a plain literal, an f-string, and an os.path.join argument.
    write_file(
        &out.join("scripts/02_analysis.py"),
        &format!(
            "#!/usr/bin/env python3\n\
             import os\n\
             PKG_ROOT_DIR = \"{abs}\"\n\
             TABLE = f\"{abs}/runtime/outputs/{TASK}/results_table.tsv\"\n\
             OUT = os.path.join(\"{abs}\", \"runtime\", \"outputs\", \"{TASK}\")\n\
             print(PKG_ROOT_DIR, TABLE, OUT)\n"
        ),
    );

    // Shell: a variable assignment plus an inline path.
    write_file(
        &out.join("scripts/03_analysis.sh"),
        &format!(
            "#!/usr/bin/env bash\n\
             set -euo pipefail\n\
             PKG=\"{abs}\"\n\
             wc -l \"{abs}/runtime/outputs/{TASK}/results_table.tsv\"\n\
             echo \"$PKG\"\n"
        ),
    );

    // A non-code transcript under `scripts/`: tokenized, not rewritten, but it
    // must still lose the host path.
    write_file(
        &out.join("scripts/NOTES.txt"),
        &format!("Ran under {abs} on the compiling host.\n"),
    );
}

/// Every file under `<root>/runtime/outputs/*/scripts/`, sorted for a stable
/// failure message.
fn task_script_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(tasks) = std::fs::read_dir(root.join("runtime/outputs")) else {
        return out;
    };
    let mut task_dirs: Vec<PathBuf> = tasks.flatten().map(|e| e.path()).collect();
    task_dirs.sort();
    for task_dir in task_dirs {
        let Ok(scripts) = std::fs::read_dir(task_dir.join("scripts")) else {
            continue;
        };
        let mut files: Vec<PathBuf> = scripts
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        out.append(&mut files);
    }
    out
}

/// Move `from` to `to`. Prefers a rename; falls back to a recursive copy when
/// the two paths straddle filesystems (`EXDEV`), so the test is not tied to
/// how the runner's `TMPDIR` is mounted.
fn move_dir(from: &Path, to: &Path) {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).expect("create move destination parent");
    }
    if std::fs::rename(from, to).is_ok() {
        return;
    }
    copy_tree(from, to);
    std::fs::remove_dir_all(from).expect("removing the original after a cross-device move");
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create copy destination");
    for entry in std::fs::read_dir(from).expect("read source dir").flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).expect("copy file during move fallback");
        }
    }
}

/// Export a fixture package, move the deposit somewhere unrelated, and assert
/// both relocation properties at the new location.
#[test]
fn moved_deposit_keeps_no_host_paths_and_still_verifies() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src_pkg = tmp.path().join("compiling-host/packages/fixture-pkg");
    let exported = tmp.path().join("compiling-host/deposits/exported");
    // Deliberately unrelated: a different subtree, a different depth, and a
    // different directory name from anything the export saw.
    let moved = tmp
        .path()
        .join("downloader/Downloads/zenodo-record-1234/unpacked-deposit");

    build_fixture_package(&src_pkg);
    let src_abs = canonical(&src_pkg);

    // `--dir` (not `--out`) so the deposit is a tree we can actually move.
    // `--profile full` keeps Layer 2 (the re-execution replay) out of scope;
    // this test is about location-independence, not re-execution.
    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .args([
            "export",
            "--package",
            src_pkg.to_str().unwrap(),
            "--dir",
            exported.to_str().unwrap(),
            "--profile",
            "full",
        ])
        .assert()
        .success();

    let exported_abs = canonical(&exported);
    move_dir(&exported, &moved);
    assert!(
        moved.join("ro-crate-metadata.json").is_file(),
        "the deposit must have moved intact to {}",
        moved.display()
    );

    // ── 1. Zero absolute host paths in the executable surface ────────────
    let scripts = task_script_files(&moved);
    assert!(
        scripts.len() >= 4,
        "expected the fixture's four scripts to survive the export; found {scripts:?}"
    );
    // Every absolute root the export could have baked in: the source package,
    // the pre-move export location, and (belt-and-braces) the post-move one.
    let moved_abs = canonical(&moved);
    let forbidden = [
        ("source package root", src_abs.as_str()),
        ("pre-move export root", exported_abs.as_str()),
        ("post-move deposit root", moved_abs.as_str()),
    ];
    // No stray absolute path anywhere in the test's own tree either: every
    // path a relocated script resolves must come from the prologue-bound root.
    let tmp_abs = canonical(tmp.path());
    for script in &scripts {
        let rel = script
            .strip_prefix(&moved)
            .unwrap_or(script.as_path())
            .display();
        let body = std::fs::read_to_string(script)
            .unwrap_or_else(|e| panic!("reading relocated script {rel}: {e}"));
        for (label, root) in &forbidden {
            assert!(
                !body.contains(root),
                "{rel} still bakes in the {label} ({root}) after relocation — \
                 the deposit points back at the compiling host.\n--- body ---\n{body}"
            );
        }
        assert!(
            !body.contains(&tmp_abs),
            "{rel} still contains an absolute path under {tmp_abs} after relocation\n\
             --- body ---\n{body}"
        );
    }

    // Deposit-wide portability scan (`/home/`, `/Users/`, `/root/` roots), the
    // same axis the export attestation records as `portability_warnings`.
    let portability = ecaa_workflow_core::deposit_readiness::scan_portability(&moved);
    assert!(
        portability.host_paths.is_empty(),
        "moved deposit still carries absolute host paths: {:?}",
        portability.host_paths
    );

    // ── 2. Offline verify still passes from the new location ─────────────
    // Layer 1 = recorded-verdict re-verify + RO-Crate content-hash recheck +
    // BagIt manifest checksums. All three are location-independent by
    // construction; this asserts they actually are.
    let tier1 = ecaa_workflow_core::deposit_readiness::validate_deposit_tier1(
        &moved,
        ecaa_workflow_types::consts::ECAA_VERSION,
    )
    .expect("Layer-1 validation must run against the moved deposit");
    assert!(
        tier1.passed(),
        "offline verify failed after relocation: ro_crate={:?} bagit={:?} detail={:?}",
        tier1.ro_crate,
        tier1.bagit,
        tier1.detail
    );

    // The attestation `export` stamped must still read clean at the new
    // location — a downstream `deposit-check` reads exactly these two fields.
    let readiness = ecaa_workflow_core::deposit_readiness::read_deposit_readiness(&moved)
        .expect("reading DEPOSIT-READINESS.json from the moved deposit")
        .expect("export must have stamped DEPOSIT-READINESS.json");
    assert_eq!(
        readiness.ro_crate,
        ecaa_workflow_core::deposit_readiness::CheckStatus::Pass,
        "stamped RO-Crate verdict must survive the move"
    );
    assert_eq!(
        readiness.bagit,
        ecaa_workflow_core::deposit_readiness::CheckStatus::Pass,
        "stamped BagIt verdict must survive the move"
    );
}

/// A relocated script must not merely have LOST the host path — it must have
/// gained a working replacement. Without the prologue the rewrite would turn a
/// runnable script into one referencing an unbound variable.
#[test]
fn relocated_scripts_bind_a_self_locating_package_root() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src_pkg = tmp.path().join("pkg");
    let exported = tmp.path().join("deposit");
    let moved = tmp.path().join("far/away/deposit");

    build_fixture_package(&src_pkg);

    Command::cargo_bin("ecaa-workflow")
        .expect("cargo bin ecaa-workflow")
        .args([
            "export",
            "--package",
            src_pkg.to_str().unwrap(),
            "--dir",
            exported.to_str().unwrap(),
            "--profile",
            "full",
        ])
        .assert()
        .success();
    move_dir(&exported, &moved);

    let scripts_dir = moved.join("runtime/outputs").join(TASK).join("scripts");

    // Each executable language binds its own root variable from the
    // environment first, then from the script's own location.
    let r = std::fs::read_to_string(scripts_dir.join("01_analysis.R")).expect("read R script");
    assert!(
        r.contains("Sys.getenv(\"PKG_ROOT\"") && r.contains(".ECAA_SELF_DIR"),
        "R script must bind PKG_ROOT from env with a self-locating fallback:\n{r}"
    );

    let py = std::fs::read_to_string(scripts_dir.join("02_analysis.py")).expect("read py script");
    assert!(
        py.contains("_ecaa_os.environ.get(\"PKG_ROOT\")") && py.contains("_ECAA_DEPOSIT_ROOT"),
        "Python script must bind PKG_ROOT from env with a self-locating fallback:\n{py}"
    );

    let sh = std::fs::read_to_string(scripts_dir.join("03_analysis.sh")).expect("read sh script");
    assert!(
        sh.contains("_ECAA_DEPOSIT_ROOT=") && sh.contains("${PKG_ROOT:-"),
        "shell script must bind PKG_ROOT from env with a self-locating fallback:\n{sh}"
    );

    // A non-code transcript gets the documentation token instead of a
    // prologue — inert to read, honest about what the path was.
    let notes = std::fs::read_to_string(scripts_dir.join("NOTES.txt")).expect("read notes");
    assert!(
        notes.contains("${PKG_ROOT}"),
        "non-code transcript must carry the ${{PKG_ROOT}} token:\n{notes}"
    );
}
