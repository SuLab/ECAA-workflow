use serde::{Deserialize, Serialize};
use crate::reexecution::{ReexecutionReport, ReexecutionBucket};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ReplayVerdict { Pass, Partial, Fail }

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct VerifierDiff {
    pub check: String,
    #[ts(type = "unknown")]
    pub recorded: serde_json::Value,
    #[ts(type = "unknown")]
    pub fresh: serde_json::Value,
    pub diverged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct ReverifyResult { pub checks: Vec<VerifierDiff>, pub reader_matches_writer: bool }

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct ReexecuteResult { pub env_tier: String, pub report: ReexecutionReport,
    pub unprovisionable: bool }

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct SkippedStage { pub task: String, pub reason: String }

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct ReplayReport {
    pub schema_version: String, pub package_iri: String,
    pub reader_version: String, pub min_reader_version: Option<String>,
    pub reverify: Option<ReverifyResult>, pub reexecute: Option<ReexecuteResult>,
    pub skipped: Vec<SkippedStage>, pub verdict: ReplayVerdict,
}

/// FAIL beats PARTIAL beats PASS. Re-verify divergence is FAIL only when the
/// reader version matches the writer (a real tamper signal); under a version
/// mismatch a divergence is drift → PARTIAL. A `failed`/over-tolerance table is
/// always FAIL; `unavailable`/unprovisionable is PARTIAL.
pub fn compute_verdict(r: &ReplayReport) -> ReplayVerdict {
    let mut v = ReplayVerdict::Pass;
    let bump = |cur: &mut ReplayVerdict, to: ReplayVerdict| {
        let rank = |x: &ReplayVerdict| match x { ReplayVerdict::Pass=>0, ReplayVerdict::Partial=>1, ReplayVerdict::Fail=>2 };
        if rank(&to) > rank(cur) { *cur = to; }
    };
    if let Some(rv) = &r.reverify {
        if rv.checks.iter().any(|c| c.diverged) {
            bump(&mut v, if rv.reader_matches_writer { ReplayVerdict::Fail } else { ReplayVerdict::Partial });
        }
    }
    if let Some(re) = &r.reexecute {
        if re.unprovisionable { bump(&mut v, ReplayVerdict::Partial); }
        for a in &re.report.per_artifact {
            match a.bucket {
                ReexecutionBucket::Failed => bump(&mut v, ReplayVerdict::Fail),
                ReexecutionBucket::Unavailable => bump(&mut v, ReplayVerdict::Partial),
                _ => {}
            }
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reexecution::{ReexecutionReport, ArtifactClassification, ReexecutionBucket};

    fn base() -> ReplayReport {
        ReplayReport { schema_version: "0.1".into(), package_iri: "ro-crate-metadata.json".into(),
            reader_version: "0.2".into(), min_reader_version: Some("0.2".into()),
            reverify: None, reexecute: None, skipped: vec![], verdict: ReplayVerdict::Pass }
    }

    #[test]
    fn pass_when_reverify_clean_and_tables_nondivergent() {
        let mut r = base();
        r.reverify = Some(ReverifyResult { reader_matches_writer: true, checks: vec![
            VerifierDiff { check: "claim_verification".into(), recorded: serde_json::json!(0),
                fresh: serde_json::json!(0), diverged: false, note: None }]});
        let mut rep = ReexecutionReport::empty("0.1");
        rep.per_artifact.push(ArtifactClassification { artifact_path: "a.tsv".into(),
            bucket: ReexecutionBucket::ByteIdentical, reason: None });
        r.reexecute = Some(ReexecuteResult { env_tier: "container".into(), report: rep, unprovisionable: false });
        assert_eq!(compute_verdict(&r), ReplayVerdict::Pass);
    }

    #[test]
    fn fail_when_reverify_diverges_and_versions_match() {
        let mut r = base();
        r.reverify = Some(ReverifyResult { reader_matches_writer: true, checks: vec![
            VerifierDiff { check: "audit_proof.cross_graph_integrity".into(),
                recorded: serde_json::json!("pass"), fresh: serde_json::json!("fail"), diverged: true, note: None }]});
        assert_eq!(compute_verdict(&r), ReplayVerdict::Fail);
    }

    #[test]
    fn partial_when_reverify_diverges_under_version_mismatch() {
        let mut r = base();
        r.reverify = Some(ReverifyResult { reader_matches_writer: false, checks: vec![
            VerifierDiff { check: "claim_verification".into(), recorded: serde_json::json!(24),
                fresh: serde_json::json!(90), diverged: true, note: None }]});
        assert_eq!(compute_verdict(&r), ReplayVerdict::Partial);
    }

    #[test]
    fn fail_when_a_table_failed_bucket() {
        let mut r = base();
        let mut rep = ReexecutionReport::empty("0.1");
        rep.per_artifact.push(ArtifactClassification { artifact_path: "de.tsv".into(),
            bucket: ReexecutionBucket::Failed, reason: Some("nonzero exit".into()) });
        r.reexecute = Some(ReexecuteResult { env_tier: "host".into(), report: rep, unprovisionable: false });
        assert_eq!(compute_verdict(&r), ReplayVerdict::Fail);
    }

    #[test]
    fn partial_when_env_unprovisionable() {
        let mut r = base();
        r.reexecute = Some(ReexecuteResult { env_tier: "none".into(),
            report: ReexecutionReport::empty("0.1"), unprovisionable: true });
        assert_eq!(compute_verdict(&r), ReplayVerdict::Partial);
    }
}
