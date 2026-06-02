//! Projects live `ClaimVerificationReport` verdicts into the audit-proof
//! C-graph shape (`{claim_id, status, supported_by}`) and persists them as
//! an HMAC-signed, agent-unforgeable sink the loader verifies.

use crate::claim_verifier::{ClaimStatus, ClaimVerificationReport};
use serde_json::{json, Value};

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
                    ("verified", v.claim.source_table.iter().cloned().collect())
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
}
