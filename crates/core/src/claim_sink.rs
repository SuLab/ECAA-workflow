//! Projects live `ClaimVerificationReport` verdicts into the audit-proof
//! C-graph shape (`{claim_id, status, supported_by}`) and persists them as
//! an HMAC-signed, agent-unforgeable sink the loader verifies.

use crate::audit_writer::AuditWriter;
use crate::claim_verifier::{ClaimStatus, ClaimVerificationReport};
use crate::coverage::CoverageResult;
use ecaa_workflow_types::consts::{ECAA_VERSION, MIN_READER_VERSION};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Sink path under the BagIt-excluded, never-agent-trusted reports dir.
pub const SIGNED_SINK_REL: &str = "runtime/verification-reports/claim-verification.signed.json";

/// Build the package-relative Evidence (V) reference for a verified claim's
/// `source_table`. The runtime verifier records the table it confirmed against
/// by basename (e.g. `de_results.tsv`); a bare basename is resolved to its
/// produced location under the task's output dir
/// (`runtime/outputs/<task>/<file>`) — the SAME `@id`
/// [`crate::ro_crate::register_produced_output_tables`] assigns — so the C→V
/// `supported_by` reference resolves in `cross_graph_integrity` (Inv 5) and
/// `evidence_coverage` (Inv 3) agree on the same link. A reference that already
/// carries a path separator is treated as an explicit package-relative path and
/// kept verbatim (e.g. a claim citing `results/tables/de.csv`).
fn evidence_ref_for(task_id: &str, source_table: &str) -> String {
    if source_table.contains('/') {
        source_table.to_string()
    } else {
        format!("runtime/outputs/{task_id}/{source_table}")
    }
}

/// Project live verdicts into the audit-proof C-graph row shape
/// (`{claim_id, status, supported_by}`). Deterministic; `claim_id` is
/// positional (`<task_id>#claim-<i>`) so it is collision-free within a task.
pub fn project_verdict_rows(report: &ClaimVerificationReport, task_id: &str) -> Vec<Value> {
    report
        .verdicts
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let (status, supported_by): (&str, Vec<String>) = match &v.status {
                ClaimStatus::Verified => {
                    let supported = v
                        .claim
                        .source_table
                        .iter()
                        .map(|t| evidence_ref_for(task_id, t))
                        .collect();
                    ("verified", supported)
                }
                ClaimStatus::Unverifiable { .. } => ("pending", Vec::new()),
                ClaimStatus::Mismatch { .. } => ("mismatch", Vec::new()),
            };
            json!({
                "claim_id": format!("{task_id}#claim-{i}"),
                "status": status,
                "supported_by": supported_by,
            })
        })
        .collect()
}

/// Build the full claim-verification document that gets HMAC-signed and
/// written to the sink. Shape is a superset of the emit-time stub
/// (`schema_version` + counts + `verdicts`) plus the version triple and
/// provenance discriminators distinguishing it from the agent-writable
/// stub the loader must NOT trust, and — when present — the
/// structured-claims-only `coverage` block the reframed Inv 1 reads
/// (recall floor). The `coverage` block is computed only from structured
/// `result.json claims[]` verdicts (deterministic), never the regex path.
pub fn build_sink_doc(
    report: &ClaimVerificationReport,
    task_id: &str,
    coverage: Option<&CoverageResult>,
) -> Value {
    use crate::ablation::{AblationFlag, AblationFlagExt};
    // Site 1 of the two-site benchmark toggle (Aim 3A). Under
    // ECAA_ABLATE_CLAIM_CONSISTENCY the populated signed sink is suppressed:
    // the doc carries ZERO verdicts, zeroed counts, a suppressed coverage
    // block, and an explicit `ablated: true` marker — distinct from the
    // legacy emit-time empty stub (which has no such field). The A-vs-B'
    // contrast therefore measures the PRESENCE of carried verdicts/coverage
    // (enforcement), not a status-enum flip on a perpetually-empty file.
    let ablated = AblationFlag::ClaimConsistency.is_active();
    let mut doc = json!({
        "schema_version": "1",
        "source": "runtime-verifier",
        "task_id": task_id,
        "ecaa_version": ECAA_VERSION,
        "min_reader_version": MIN_READER_VERSION,
        "ablated": ablated,
        "n_checked": if ablated { 0 } else { report.n_checked },
        "n_verified": if ablated { 0 } else { report.n_verified },
        "n_mismatch": if ablated { 0 } else { report.n_mismatch },
        "n_unverifiable": if ablated { 0 } else { report.n_unverifiable },
        "verdicts": if ablated { Vec::new() } else { project_verdict_rows(report, task_id) },
    });
    // The coverage block (recall floor) is signed-sink content the reframed
    // Inv 1 reads; suppress it on the ablated arm alongside the verdicts.
    if !ablated {
        if let Some(cov) = coverage {
            doc["coverage"] = serde_json::to_value(cov).unwrap_or(Value::Null);
        }
    }
    doc
}

/// Build the sink doc for `task_id`, HMAC-sign it with `writer`, and APPEND
/// it as one signed JSONL row to
/// `<package_root>/runtime/verification-reports/claim-verification.signed.json`.
/// Returns the written path.
///
/// The sink is append-only: one independently-signed row per task
/// verification. The loader (`audit_proof::loader::load_claims`) unions all
/// rows so a recall gap recorded by an earlier task can never be erased by a
/// later coverage-less task — a last-writer REPLACE silently dropped it (the
/// F2 at-rest erasure). Because each row carries its own HMAC, appending
/// needs no rewrite of prior rows.
///
/// The reports dir is already excluded from the BagIt manifest
/// (`emitter/bagit.rs`) and is never trusted from the agent side
/// (`server::chat_routes::events::rate_limit`). Written post-execution by
/// the host (which holds the session secret), so it is outside the emit
/// byte-diff baseline and cannot be forged by the agent.
pub fn persist_signed_verdicts(
    package_root: &Path,
    task_id: &str,
    report: &ClaimVerificationReport,
    coverage: Option<&CoverageResult>,
    writer: &AuditWriter,
) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let path = package_root.join(SIGNED_SINK_REL);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let doc = build_sink_doc(report, task_id, coverage);
    let mut buf = Vec::new();
    writer.write_signed_row(&mut buf, &doc)?;

    // `buf` already ends in '\n' (write_signed_row uses writeln!). Host writes
    // are sequential and post-execution, so a single appended line per call
    // accumulates one row per task verification.
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(&buf)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim_contract::ClaimContract;
    use crate::claim_extractor::Claim;
    use crate::claim_verifier::{ClaimStrength, ClaimVerdict};

    fn claim(entity: &str, table: Option<&str>) -> Claim {
        Claim {
            entity: entity.to_string(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: table.map(|t| t.to_string()),
            excerpt: String::new(),
            contract: ClaimContract::NumericTableLookup,
        }
    }

    fn verdict(c: Claim, status: ClaimStatus) -> ClaimVerdict {
        ClaimVerdict {
            claim: c,
            status,
            strength: ClaimStrength::default(),
        }
    }

    #[test]
    fn verified_claim_projects_with_supported_by() {
        let report = ClaimVerificationReport {
            n_checked: 1,
            n_verified: 1,
            n_mismatch: 0,
            n_unverifiable: 0,
            verdicts: vec![verdict(
                claim("TP53", Some("results/tables/de.csv")),
                ClaimStatus::Verified,
            )],
            runtime_decision_log_path: None,
        };
        let rows = project_verdict_rows(&report, "diff_expr");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["claim_id"], json!("diff_expr#claim-0"));
        assert_eq!(rows[0]["status"], json!("verified"));
        assert_eq!(rows[0]["supported_by"], json!(["results/tables/de.csv"]));
    }

    #[test]
    fn unverifiable_projects_pending_empty() {
        let report = ClaimVerificationReport {
            n_checked: 1,
            n_verified: 0,
            n_mismatch: 0,
            n_unverifiable: 1,
            verdicts: vec![verdict(
                claim("BRCA1", None),
                ClaimStatus::Unverifiable {
                    reason: "no table".into(),
                },
            )],
            runtime_decision_log_path: None,
        };
        let rows = project_verdict_rows(&report, "diff_expr");
        assert_eq!(rows[0]["status"], json!("pending"));
        assert_eq!(rows[0]["supported_by"], json!([]));
    }

    #[test]
    fn mismatch_projects_nonpending_empty() {
        let report = ClaimVerificationReport {
            n_checked: 1,
            n_verified: 0,
            n_mismatch: 1,
            n_unverifiable: 0,
            verdicts: vec![verdict(
                claim("IL6", Some("results/tables/de.csv")),
                ClaimStatus::Mismatch {
                    detail: "sign flip".into(),
                },
            )],
            runtime_decision_log_path: None,
        };
        let rows = project_verdict_rows(&report, "diff_expr");
        assert_eq!(rows[0]["status"], json!("mismatch"));
        assert_eq!(rows[0]["supported_by"], json!([]));
    }

    #[test]
    fn sink_doc_carries_counts_version_and_verdicts() {
        let report = ClaimVerificationReport {
            n_checked: 2,
            n_verified: 1,
            n_mismatch: 1,
            n_unverifiable: 0,
            verdicts: vec![
                verdict(
                    claim("TP53", Some("results/tables/de.csv")),
                    ClaimStatus::Verified,
                ),
                verdict(
                    claim("IL6", Some("results/tables/de.csv")),
                    ClaimStatus::Mismatch {
                        detail: "sign flip".into(),
                    },
                ),
            ],
            runtime_decision_log_path: None,
        };
        let doc = build_sink_doc(&report, "diff_expr", None);
        assert_eq!(doc["schema_version"], json!("1"));
        assert_eq!(doc["n_checked"], json!(2));
        assert_eq!(doc["n_mismatch"], json!(1));
        assert_eq!(doc["ecaa_version"], json!("0.2"));
        assert_eq!(doc["min_reader_version"], json!("0.2"));
        assert_eq!(doc["source"], json!("runtime-verifier"));
        assert_eq!(doc["task_id"], json!("diff_expr"));
        let verdicts = doc["verdicts"].as_array().unwrap();
        assert_eq!(verdicts.len(), 2);
        assert_eq!(verdicts[0]["status"], json!("verified"));
    }

    #[test]
    fn sink_doc_carries_coverage_when_present() {
        use crate::coverage::{CoverageResult, EntityCoverage};
        use std::collections::BTreeMap;
        let report = ClaimVerificationReport::empty();
        let mut per_entity = BTreeMap::new();
        per_entity.insert(
            "differential_expression".to_string(),
            EntityCoverage::Absent,
        );
        let cov = CoverageResult {
            required_total: 1,
            required_addressed: 0,
            required_unverifiable: 0,
            required_absent: 1,
            per_entity,
        };
        let doc = build_sink_doc(&report, "diff_expr", Some(&cov));
        assert_eq!(doc["coverage"]["required_absent"], json!(1));
        assert_eq!(doc["coverage"]["required_total"], json!(1));
        // Absent the coverage arg, no coverage key is written (Phase-1 shape).
        let bare = build_sink_doc(&report, "diff_expr", None);
        assert!(bare.get("coverage").is_none());
    }

    #[test]
    fn persisted_sink_verifies_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let report = ClaimVerificationReport {
            n_checked: 1,
            n_verified: 1,
            n_mismatch: 0,
            n_unverifiable: 0,
            verdicts: vec![verdict(
                claim("TP53", Some("results/tables/de.csv")),
                ClaimStatus::Verified,
            )],
            runtime_decision_log_path: None,
        };
        let writer = AuditWriter::for_session();

        let path =
            persist_signed_verdicts(dir.path(), "diff_expr", &report, None, &writer).unwrap();
        assert_eq!(
            path,
            dir.path()
                .join("runtime/verification-reports/claim-verification.signed.json")
        );

        // The on-disk line is a single signed JSON row; verify_row strips _mac.
        let line = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert!(parsed.get("_mac").is_some(), "sink must be signed");
        let inner = writer.verify_row(&parsed).expect("valid HMAC");
        assert_eq!(inner["verdicts"].as_array().unwrap().len(), 1);
        assert_eq!(inner["source"], json!("runtime-verifier"));
    }

    #[test]
    #[serial_test::serial]
    fn ablated_sink_doc_is_explicitly_empty_and_marked() {
        let env = crate::ablation::AblationFlag::ClaimConsistency.env_var();
        std::env::set_var(env, "1");
        let report = ClaimVerificationReport {
            n_checked: 2,
            n_verified: 2,
            n_mismatch: 0,
            n_unverifiable: 0,
            verdicts: vec![
                verdict(
                    claim("TP53", Some("results/tables/de.csv")),
                    ClaimStatus::Verified,
                ),
                verdict(
                    claim("IL6", Some("results/tables/de.csv")),
                    ClaimStatus::Verified,
                ),
            ],
            runtime_decision_log_path: None,
        };
        let doc = build_sink_doc(&report, "diff_expr", None);
        std::env::remove_var(env);
        // Site 1: under ClaimConsistency the populated sink is suppressed —
        // the doc carries ZERO verdicts and an explicit ablation marker that
        // is distinct from the legacy emit-time stub (which has no such field).
        assert_eq!(doc["ablated"], json!(true));
        assert_eq!(doc["verdicts"].as_array().unwrap().len(), 0);
        assert_eq!(doc["n_verified"], json!(0));
        assert_eq!(doc["n_checked"], json!(0));
        assert_eq!(doc["source"], json!("runtime-verifier"));
    }

    #[test]
    #[serial_test::serial]
    fn ablated_sink_doc_suppresses_coverage_block() {
        use crate::coverage::{CoverageResult, EntityCoverage};
        use std::collections::BTreeMap;
        let env = crate::ablation::AblationFlag::ClaimConsistency.env_var();
        std::env::set_var(env, "1");
        let report = ClaimVerificationReport::empty();
        let mut per_entity = BTreeMap::new();
        per_entity.insert(
            "differential_expression".to_string(),
            EntityCoverage::Absent,
        );
        let cov = CoverageResult {
            required_total: 1,
            required_addressed: 0,
            required_unverifiable: 0,
            required_absent: 1,
            per_entity,
        };
        let doc = build_sink_doc(&report, "diff_expr", Some(&cov));
        std::env::remove_var(env);
        // The recall floor reads the coverage block from the signed sink; the
        // ablated arm must not carry it, else the floor sees real content and
        // the contrast reduces to a status flip.
        assert_eq!(doc["ablated"], json!(true));
        assert!(doc.get("coverage").is_none());
    }

    #[test]
    #[serial_test::serial]
    fn unablated_sink_doc_has_no_ablated_marker() {
        // Belt-and-braces: with the flag off, the marker is false and verdicts
        // populate — proving the contrast is enforcement, not enum.
        let env = crate::ablation::AblationFlag::ClaimConsistency.env_var();
        std::env::remove_var(env);
        let report = ClaimVerificationReport {
            n_checked: 1,
            n_verified: 1,
            n_mismatch: 0,
            n_unverifiable: 0,
            verdicts: vec![verdict(
                claim("TP53", Some("results/tables/de.csv")),
                ClaimStatus::Verified,
            )],
            runtime_decision_log_path: None,
        };
        let doc = build_sink_doc(&report, "diff_expr", None);
        assert_eq!(doc["ablated"], json!(false));
        assert_eq!(doc["verdicts"].as_array().unwrap().len(), 1);
    }
}
