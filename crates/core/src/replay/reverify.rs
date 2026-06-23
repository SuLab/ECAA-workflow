//! Tier-1 re-verify: re-run deterministic verifiers on the downloaded bytes
//! and diff fresh verdicts against the package's recorded ones (tamper /
//! version-drift detection).
//!
//! # Claim-verification check strategy
//!
//! Rather than re-running the full claim verifier (which would require the
//! original data tables and LLM extraction pipeline), we count `n_mismatch`
//! and `n_suspicious` by scanning the on-disk `verdicts` array in
//! `runtime/claim-verification.json` — each verdict's `"status"` field
//! is a tagged enum that serializes to `"mismatch"` / `"suspicious"` /
//! `"verified"` / etc. (§`ClaimStatus` `snake_case`). These counts are
//! internally consistent by construction; the offline check tests whether the
//! recorded summary header matches the actual verdict rows, detecting edits
//! that flip a mismatch row without decrementing the counter. The check is
//! purely deterministic and offline (no LLM call, no data tables).

use crate::audit_proof::run_audit_proof;
use crate::clock::WallClock;
use crate::replay::report::{ReverifyResult, VerifierDiff};
use crate::wrroc_validator::NoopWrrocValidator;
use anyhow::Context;
use std::path::Path;

/// Re-run the package's deterministic verifiers and diff fresh verdicts
/// against the recorded ones.
///
/// * `pkg` — package root (must contain `ro-crate-metadata.json` +
///   `runtime/` sidecars).
/// * `reader_version` — the ECAA spec version this build of the reader
///   implements; compared against the `ecaa_version` field in the recorded
///   `runtime/audit-proof-report.json` to detect version drift vs. real
///   tampering.
pub fn reverify(pkg: &Path, reader_version: &str) -> anyhow::Result<ReverifyResult> {
    let mut checks: Vec<VerifierDiff> = Vec::new();

    // ── 1. Audit-proof report ─────────────────────────────────────────────

    let recorded_report_path = pkg.join("runtime").join("audit-proof-report.json");

    let writer_version: Option<String>;

    if recorded_report_path.exists() {
        let raw = std::fs::read_to_string(&recorded_report_path).with_context(|| {
            format!(
                "reading recorded audit-proof report: {}",
                recorded_report_path.display()
            )
        })?;
        let recorded: serde_json::Value = serde_json::from_str(&raw).with_context(|| {
            format!(
                "parsing recorded audit-proof report: {}",
                recorded_report_path.display()
            )
        })?;

        // Extract writer version for reader_matches_writer.
        writer_version = recorded
            .get("ecaa_version")
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        // Build a map from invariant id → recorded status string.
        let recorded_verdicts: std::collections::HashMap<String, serde_json::Value> = recorded
            .get("verdicts")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|entry| {
                        let id = entry.get("id")?.as_str()?.to_owned();
                        let status = entry.get("status")?.clone();
                        Some((id, status))
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Run fresh audit-proof (offline — NoopWrrocValidator; substrate_validity
        // → Unverified is the expected, non-fatal outcome for offline replay).
        let fresh_report = run_audit_proof(pkg, &NoopWrrocValidator, &WallClock)
            .context("running fresh audit-proof")?;

        for verdict in &fresh_report.verdicts {
            // id serializes as snake_case (e.g. "cross_graph_integrity")
            let id_str = serde_json::to_value(verdict.id)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("{:?}", verdict.id).to_lowercase());

            let fresh_status = serde_json::to_value(verdict.status)
                .unwrap_or(serde_json::Value::Null);

            if let Some(rec_status) = recorded_verdicts.get(&id_str) {
                let diverged = rec_status != &fresh_status;
                checks.push(VerifierDiff {
                    check: format!("audit_proof.{id_str}"),
                    recorded: rec_status.clone(),
                    fresh: fresh_status,
                    diverged,
                });
            }
            // If the recorded report does not mention this id, skip: a new
            // invariant added after the package was emitted is not a tamper
            // signal.
        }
    } else {
        // No recorded report: nothing to compare.
        writer_version = None;
        checks.push(VerifierDiff {
            check: "audit_proof".to_string(),
            recorded: serde_json::Value::Null,
            fresh: serde_json::Value::Null,
            diverged: false,
        });
    }

    // ── 2. Claim-verification summary ─────────────────────────────────────

    let cv_path = pkg.join("runtime").join("claim-verification.json");

    if cv_path.exists() {
        let raw = std::fs::read_to_string(&cv_path)
            .with_context(|| format!("reading claim-verification: {}", cv_path.display()))?;
        let cv: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("parsing claim-verification: {}", cv_path.display()))?;

        // Derive n_mismatch and n_suspicious from the on-disk verdict rows —
        // this is cheaper and offline. Each verdict has `"status": "mismatch"`
        // (or `"suspicious"`, `"verified"`, etc.) serialized by ClaimStatus's
        // snake_case tag. We count and compare against the recorded header
        // fields (defaulting to 0 if absent, e.g. older on-disk formats that
        // embed only the verdicts array without summary fields).
        let (fresh_mismatch, fresh_suspicious) = count_verdict_statuses(&cv);

        for (field, fresh_count) in [
            ("n_mismatch", fresh_mismatch),
            ("n_suspicious", fresh_suspicious),
        ] {
            let recorded_count = cv
                .get(field)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let fresh_val = serde_json::Value::Number(fresh_count.into());
            let recorded_val =
                serde_json::Value::Number(serde_json::Number::from(recorded_count));
            let diverged = recorded_count != fresh_count as u64;
            checks.push(VerifierDiff {
                check: format!("claim_verification.{field}"),
                recorded: recorded_val,
                fresh: fresh_val,
                diverged,
            });
        }
    } else {
        checks.push(VerifierDiff {
            check: "claim_verification".to_string(),
            recorded: serde_json::Value::Null,
            fresh: serde_json::Value::Null,
            diverged: false,
        });
    }

    // ── 3. reader_matches_writer ──────────────────────────────────────────

    let reader_matches_writer = writer_version
        .as_deref()
        .map(|w| w == reader_version)
        .unwrap_or(false);

    Ok(ReverifyResult {
        checks,
        reader_matches_writer,
    })
}

/// Count `"mismatch"` and `"suspicious"` status values in the `verdicts`
/// array of a `claim-verification.json` value. Returns `(n_mismatch,
/// n_suspicious)`.
///
/// The status field is a `serde(tag = "status", rename_all = "snake_case")`
/// enum, so it serializes as a JSON string within the object.
fn count_verdict_statuses(cv: &serde_json::Value) -> (u64, u64) {
    let Some(verdicts) = cv.get("verdicts").and_then(|v| v.as_array()) else {
        return (0, 0);
    };
    let mut n_mismatch: u64 = 0;
    let mut n_suspicious: u64 = 0;
    for verdict in verdicts {
        match verdict.get("status").and_then(|s| s.as_str()) {
            Some("mismatch") => n_mismatch += 1,
            Some("suspicious") => n_suspicious += 1,
            _ => {}
        }
    }
    (n_mismatch, n_suspicious)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    /// Recursively copy `src` into `dst` (creating `dst` if needed).
    fn copy_dir_all(src: &Path, dst: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            let dest = dst.join(entry.file_name());
            if ty.is_dir() {
                copy_dir_all(&entry.path(), &dest)?;
            } else {
                fs::copy(&entry.path(), &dest)?;
            }
        }
        Ok(())
    }

    /// Copy a named conformance fixture into `dst`.
    fn copy_fixture(name: &str, dst: &Path) {
        let fixtures_root =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../ecaa-conformance/tests/fixtures")
                .join(name);
        copy_dir_all(&fixtures_root, dst).expect("copy_fixture");
    }

    /// Write a synthetic `runtime/audit-proof-report.json` to `pkg` with the
    /// given `(id, status)` pairs and `ecaa_version = "0.2"`.
    fn write_recorded_audit(pkg: &Path, verdicts: &[(&str, &str)]) {
        let runtime = pkg.join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        let verdict_arr: Vec<serde_json::Value> = verdicts
            .iter()
            .map(|(id, status)| {
                serde_json::json!({
                    "id": id,
                    "status": status,
                    "detail": null,
                    "n_inspected": 0,
                    "n_violations": 0
                })
            })
            .collect();
        let report = serde_json::json!({
            "schema_version": "0.1",
            "ecaa_version": "0.2",
            "min_reader_version": "0.2",
            "evaluator": {
                "impl": "ecaa-workflow-audit-proof",
                "version": "0.1.0",
                "policy": "warn-only"
            },
            "verdicts": verdict_arr
        });
        let path = runtime.join("audit-proof-report.json");
        fs::write(&path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    }

    /// The package body contains a dangling cross-graph reference, but the
    /// recorded audit report lies and says `cross_graph_integrity` passed.
    /// The fresh re-run detects the dangle and the check must diverge.
    #[test]
    fn reverify_flags_tampered_cross_graph() {
        let tmp = tempfile::tempdir().unwrap();
        copy_fixture("cross-graph-dangling", tmp.path());
        write_recorded_audit(tmp.path(), &[("cross_graph_integrity", "pass")]);
        let res = reverify(tmp.path(), "0.2").unwrap();
        let c = res
            .checks
            .iter()
            .find(|c| c.check.contains("cross_graph_integrity"))
            .unwrap();
        assert!(c.diverged, "tampered cross_graph must diverge");
    }

    /// The package body is clean; the recorded report correctly says
    /// `cross_graph_integrity` passed. No divergence expected.
    #[test]
    fn reverify_clean_when_recorded_matches() {
        let tmp = tempfile::tempdir().unwrap();
        copy_fixture("cross-graph-ok", tmp.path());
        write_recorded_audit(tmp.path(), &[("cross_graph_integrity", "pass")]);
        let res = reverify(tmp.path(), "0.2").unwrap();
        let c = res
            .checks
            .iter()
            .find(|c| c.check.contains("cross_graph_integrity"))
            .unwrap();
        assert!(!c.diverged);
    }
}
