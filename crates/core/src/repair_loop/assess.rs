//! Package assessment: turn a finalized package's claim-repair plan and
//! audit-proof report into a [`FailureSet`] the repair loop can drive.
//!
//! `core` CANNOT depend on `harness`, so the three finalize-input helpers
//! (`load_decisions`, `derive_is_confirmatory`, `audit_secret_from_env`) are
//! small re-implementations of their `harness::end_of_run_finalize` twins,
//! copying the exact semantics (line-delimited decisions skipping malformed
//! lines; confirmatory-stem scan of `WORKFLOW.json`; 64-hex → 32-byte secret).

use std::collections::BTreeMap;
use std::path::Path;

use crate::audit_proof::{
    run_audit_proof_with_verifier, AuditProofReport, InvariantStatus,
};
use crate::audit_writer::AuditWriter;
use crate::claim_repair::{RepairAction, RepairItem};
use crate::clock::WallClock;
use crate::decision_log::DecisionRecord;
use crate::project_class::ProjectClass;
use crate::wrroc_validator::NoopWrrocValidator;

use super::failure::{Failure, FailureSet, FailureSource, RepairClass};

/// Confirmatory stage stems (copied from `harness::end_of_run_finalize`).
const CONFIRMATORY_STAGE_STEMS: &[&str] = &[
    "differential_expression",
    "differential_accessibility",
    "variant_calling",
    "primary_endpoint",
];

/// Read `runtime/decisions.jsonl` back into records, skipping malformed lines.
/// Empty vec when absent. Copy of the harness twin's semantics.
fn load_decisions(root: &Path) -> Vec<DecisionRecord> {
    let path = root.join("runtime").join("decisions.jsonl");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<DecisionRecord>(line) {
            out.push(rec);
        }
    }
    out
}

/// True when any task id / `source_atom_id` stem in `WORKFLOW.json` matches a
/// confirmatory stage. Conservative `false` when the file is absent/unparsable.
/// Copy of the harness twin's semantics.
fn derive_is_confirmatory(root: &Path) -> bool {
    let Ok(bytes) = std::fs::read(root.join("WORKFLOW.json")) else {
        return false;
    };
    let Ok(wf) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    let Some(tasks) = wf.get("tasks").and_then(|t| t.as_object()) else {
        return false;
    };
    let is_confirmatory_stem =
        |s: &str| CONFIRMATORY_STAGE_STEMS.iter().any(|stem| s.contains(stem));
    tasks.iter().any(|(task_id, t)| {
        if is_confirmatory_stem(task_id) {
            return true;
        }
        t.get("source_atom_id")
            .and_then(|v| v.as_str())
            .map(is_confirmatory_stem)
            .unwrap_or(false)
    })
}

/// Derive the 32-byte HMAC key from `ECAA_AUDIT_SECRET`: hex-decode the trimmed
/// value and require EXACTLY 32 bytes (64 hex chars). `None` otherwise. Copy of
/// the harness twin's semantics — must match the audit-proof verifier's key.
fn audit_secret_from_env() -> Option<[u8; 32]> {
    let raw = std::env::var("ECAA_AUDIT_SECRET").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let bytes = hex::decode(trimmed).ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

/// Map a claim-repair [`RepairAction`] to a repair-loop [`RepairClass`].
pub fn map_repair_action(a: RepairAction) -> RepairClass {
    match a {
        RepairAction::NarrativeCorrection => RepairClass::NarrativeCorrection,
        RepairAction::CitationFix => RepairClass::CitationFix,
        RepairAction::EvidenceCompletion => RepairClass::EvidenceCompletion,
        RepairAction::ReviewRequired => RepairClass::ReviewRequired,
    }
}

/// Map a failing invariant's snake_case id to its repair class.
fn invariant_class(name: &str) -> RepairClass {
    match name {
        "equivalence_failure" => RepairClass::EquivalenceRerun,
        "substrate_validity" => RepairClass::ConformanceFix,
        "evidence_coverage" => RepairClass::CoverageGap,
        "claim_completeness" => RepairClass::CoverageGap,
        "decision_justification" => RepairClass::ReviewRequired,
        "cross_graph_integrity" => RepairClass::ReviewRequired,
        _ => RepairClass::ReviewRequired,
    }
}

/// The snake_case wire string of an invariant id (e.g. `equivalence_failure`).
/// `InvariantId` serializes (serde `rename_all = "snake_case"`, no payload) as a
/// bare JSON string, so this is exact and stable.
fn invariant_id_snake(id: crate::audit_proof::InvariantId) -> String {
    match serde_json::to_value(id) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

/// One [`Failure`] per invariant verdict whose status is NOT `Pass`
/// (`Fail`/`Warn`/`Unverified`, plus any future variant — `InvariantStatus` is
/// `#[non_exhaustive]`). `subject` is the invariant id snake_case string,
/// `detail` the verdict detail, `task` is `"audit"`.
pub fn invariant_failures(report: &AuditProofReport) -> Vec<Failure> {
    report
        .verdicts
        .iter()
        .filter(|v| !matches!(v.status, InvariantStatus::Pass))
        .map(|v| {
            let subject = invariant_id_snake(v.id);
            let detail = v.detail.clone().unwrap_or_default();
            Failure::new(
                FailureSource::InvariantFailure(subject.clone()),
                invariant_class(&subject),
                "audit",
                &subject,
                &detail,
            )
        })
        .collect()
}

/// One [`Failure`] per claim-repair plan item across every task.
pub fn claim_failures(plan: &BTreeMap<String, Vec<RepairItem>>) -> Vec<Failure> {
    let mut out = Vec::new();
    for (task, items) in plan {
        for item in items {
            out.push(Failure::new(
                FailureSource::ClaimMismatch,
                map_repair_action(item.action),
                task,
                &item.excerpt,
                &item.detail,
            ));
        }
    }
    out
}

/// Assess a finalized package into a [`FailureSet`]:
///
/// 1. Best-effort `finalize_package` (logged + continued on error) so the
///    claim-repair plan and signed verdict sink are fresh.
/// 2. Read `runtime/claim-repair-plan.json` → claim failures.
/// 3. Run audit-proof (HMAC-verifying when a secret is present) → invariant
///    failures.
///
/// Returns the union. Never panics on a sparse package.
pub fn assess_package(root: &Path, config_dir: &Path) -> anyhow::Result<FailureSet> {
    // (a) Best-effort finalize: refresh the plan + signed sink. Non-fatal.
    let decisions = load_decisions(root);
    let is_confirmatory = derive_is_confirmatory(root);
    if let Err(e) = crate::finalize::finalize_package(
        root,
        config_dir,
        ProjectClass::default(),
        &decisions,
        is_confirmatory,
        audit_secret_from_env().as_ref(),
    ) {
        tracing::warn!(
            target: "repair-assess",
            error = %e,
            "finalize_package failed during assess — continuing with on-disk artifacts"
        );
    }

    // (b) Claim-repair plan → claim failures (empty when the file is absent).
    let plan_path = root.join("runtime").join("claim-repair-plan.json");
    let plan: BTreeMap<String, Vec<RepairItem>> = match std::fs::read(&plan_path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => BTreeMap::new(),
    };
    let mut failures = claim_failures(&plan);

    // (c) Audit-proof → invariant failures.
    let secret = audit_secret_from_env();
    let writer = secret.map(AuditWriter::with_secret);
    let report = run_audit_proof_with_verifier(
        root,
        &NoopWrrocValidator,
        &WallClock,
        writer.as_ref(),
    )?;
    failures.extend(invariant_failures(&report));

    Ok(FailureSet(failures))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit_proof::{EvaluatorInfo, InvariantId, InvariantVerdict};

    fn verdict(id: InvariantId, status: InvariantStatus, detail: &str) -> InvariantVerdict {
        InvariantVerdict {
            id,
            status,
            detail: Some(detail.to_string()),
            n_inspected: 1,
            n_violations: if matches!(status, InvariantStatus::Pass) { 0 } else { 1 },
        }
    }

    fn report_with(verdicts: Vec<InvariantVerdict>) -> AuditProofReport {
        AuditProofReport {
            schema_version: "0.1".to_string(),
            ecaa_version: "test".to_string(),
            min_reader_version: "test".to_string(),
            max_reader_version: None,
            package_iri: None,
            evaluated_at: None,
            evaluator: EvaluatorInfo::reference(),
            verdicts,
        }
    }

    #[test]
    fn invariant_failures_excludes_pass_includes_rest() {
        let report = report_with(vec![
            // Pass — must be EXCLUDED (faithful twin).
            verdict(InvariantId::ClaimCompleteness, InvariantStatus::Pass, "ok"),
            verdict(InvariantId::EquivalenceFailure, InvariantStatus::Fail, "drift"),
            verdict(InvariantId::EvidenceCoverage, InvariantStatus::Warn, "gap"),
            verdict(
                InvariantId::SubstrateValidity,
                InvariantStatus::Unverified,
                "no policy",
            ),
        ]);
        let failures = invariant_failures(&report);
        assert_eq!(
            failures.len(),
            3,
            "Pass excluded; Fail/Warn/Unverified each become a failure"
        );
        // No failure carries the passing invariant's id.
        assert!(
            !failures.iter().any(|f| f.subject == "claim_completeness"),
            "the Pass verdict must not produce a failure"
        );
        // equivalence_failure → EquivalenceRerun, subject is snake_case.
        let eq = failures
            .iter()
            .find(|f| f.subject == "equivalence_failure")
            .expect("equivalence_failure verdict must surface");
        assert_eq!(
            eq.class,
            RepairClass::EquivalenceRerun,
            "equivalence_failure invariant must map to EquivalenceRerun"
        );
        assert_eq!(eq.task, "audit", "invariant failures live under the audit task");
        assert_eq!(eq.detail, "drift", "verdict detail must be carried through");
        // evidence_coverage Warn → CoverageGap, substrate_validity Unverified → ConformanceFix.
        assert_eq!(
            failures
                .iter()
                .find(|f| f.subject == "evidence_coverage")
                .map(|f| f.class),
            Some(RepairClass::CoverageGap),
            "evidence_coverage must map to CoverageGap"
        );
        assert_eq!(
            failures
                .iter()
                .find(|f| f.subject == "substrate_validity")
                .map(|f| f.class),
            Some(RepairClass::ConformanceFix),
            "substrate_validity must map to ConformanceFix"
        );
    }

    #[test]
    fn claim_failures_one_per_item_with_mapped_class() {
        let mut plan: BTreeMap<String, Vec<RepairItem>> = BTreeMap::new();
        plan.insert(
            "task_de".to_string(),
            vec![
                RepairItem {
                    entity: "CRISPLD2".to_string(),
                    excerpt: "147 DE genes".to_string(),
                    source_table: Some("de.tsv".to_string()),
                    action: RepairAction::NarrativeCorrection,
                    detail: "table says 142".to_string(),
                },
                RepairItem {
                    entity: "ref".to_string(),
                    excerpt: "see [9]".to_string(),
                    source_table: None,
                    action: RepairAction::CitationFix,
                    detail: "citation does not exist".to_string(),
                },
            ],
        );
        let failures = claim_failures(&plan);
        assert_eq!(failures.len(), 2, "one failure per repair item");
        let narr = failures
            .iter()
            .find(|f| f.subject == "147 DE genes")
            .expect("narrative item must surface");
        assert_eq!(narr.task, "task_de", "task key carried as failure task");
        assert_eq!(
            narr.class,
            RepairClass::NarrativeCorrection,
            "NarrativeCorrection action maps to NarrativeCorrection class"
        );
        assert_eq!(narr.source, FailureSource::ClaimMismatch, "claim source");
        assert_eq!(narr.detail, "table says 142", "item detail carried through");
        assert_eq!(
            failures
                .iter()
                .find(|f| f.subject == "see [9]")
                .map(|f| f.class),
            Some(RepairClass::CitationFix),
            "CitationFix action maps to CitationFix class"
        );
    }

    #[test]
    fn map_repair_action_is_total() {
        assert_eq!(
            map_repair_action(RepairAction::EvidenceCompletion),
            RepairClass::EvidenceCompletion
        );
        assert_eq!(
            map_repair_action(RepairAction::ReviewRequired),
            RepairClass::ReviewRequired
        );
    }

    #[test]
    fn invariant_id_snake_matches_serde() {
        assert_eq!(
            invariant_id_snake(InvariantId::CrossGraphIntegrity),
            "cross_graph_integrity",
            "snake_case id must match the serde wire string"
        );
    }

    #[test]
    fn assess_package_ok_on_sparse_package() {
        // A package with no claim-repair-plan and no signed sink must not panic
        // and must return Ok — the assessor is best-effort over a sparse dir.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("runtime")).expect("mk runtime");
        // Minimal WORKFLOW.json with no completed tasks so finalize no-ops.
        std::fs::write(root.join("WORKFLOW.json"), r#"{"tasks":{}}"#).expect("write workflow");
        let config_dir = tmp.path().join("policies");
        std::fs::create_dir_all(&config_dir).expect("mk policies");

        let fs = assess_package(root, &config_dir).expect("assess must be Ok on sparse package");
        // No claim-repair-plan → no claim failures. Invariants may surface
        // (Unverified on a stub package) but the call itself must succeed and
        // carry no claim-mismatch failures.
        assert!(
            fs.0.iter().all(|f| f.source != FailureSource::ClaimMismatch),
            "no claim-repair-plan means no ClaimMismatch failures"
        );
    }
}
