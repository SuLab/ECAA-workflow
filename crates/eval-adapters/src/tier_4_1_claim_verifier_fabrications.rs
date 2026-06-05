//! Tier 4.1 — claim_verifier fabrication-catch evaluation.
//!
//! Wires the deterministic `claim_extractor` + `claim_verifier`
//! pipeline against hand-curated
//! narratives that plant a known number of fabricated claims, plus a
//! reference TSV result table. The runner asserts the verifier's
//! mismatch count equals the scenario's expected count — i.e. that the
//! pipeline catches exactly the planted fabrications and no more.
//!
//! Closes the lotz v1-style fabrication pattern in measurable form.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

use ecaa_workflow_core::claim_extractor::{self, ExtractorConfig};
use ecaa_workflow_core::claim_verifier::{verify_claims, ClaimVerificationReport};

/// One Tier 4.1 fabrication-catch scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tier4_1Scenario {
    /// Stable scenario identifier.
    pub scenario_id: String,
    /// Path (relative to the corpus dir's *workspace root*) of the
    /// narrative `.md` file.
    pub narrative_path: PathBuf,
    /// Path of the TSV result table (entity + log2fc + pvalue + padj).
    /// The verifier scans the table's parent directory so the table's
    /// file-stem is also used by the narrative's "(Table N)" citation.
    pub result_table_path: PathBuf,
    /// Path of the `interpretation-policy.json` to drive extraction.
    pub interpretation_policy: PathBuf,
    /// How many `Mismatch`-status verdicts the verifier should produce.
    /// Authored from ground truth in the narrative.
    pub expected_mismatch_count: usize,
    /// Optional minimum number of verified (non-mismatch, non-unverifiable)
    /// claims. When omitted, no floor is applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_min_verified: Option<usize>,
}

/// Per-scenario result of a Tier 4.1 fabrication-catch run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tier4_1Result {
    /// Scenario identifier.
    pub scenario_id: String,
    /// Total claims checked.
    pub n_checked: usize,
    /// Claims verified against the result table.
    pub n_verified: usize,
    /// Claims with a mismatch verdict.
    pub n_mismatch: usize,
    /// Claims that could not be verified.
    pub n_unverifiable: usize,
    /// Authored expected mismatch count from the scenario YAML.
    pub expected_mismatch_count: usize,
    /// Whether `n_mismatch == expected_mismatch_count`.
    pub passed: bool,
    /// Precision: fraction of predicted mismatches that were correct (TP / (TP + FP)).
    /// In the fabrication-catch framing: the fraction of reported mismatches that are
    /// genuine, where TP = min(n_mismatch, expected_mismatch_count) and
    /// FP = max(0, n_mismatch - expected_mismatch_count).
    pub precision: f64,
    /// Recall: fraction of planted fabrications caught (TP / (TP + FN)).
    /// TP = min(n_mismatch, expected_mismatch_count);
    /// FN = max(0, expected_mismatch_count - n_mismatch).
    pub recall: f64,
}

/// Run a single scenario through extract → verify and compare against
/// the expected mismatch count.
pub fn check(scenario: &Tier4_1Scenario) -> Result<bool> {
    let result = run_one(scenario)?;
    Ok(result.passed)
}

/// Run a single scenario and return the full result including mismatch counts.
pub fn run_one(scenario: &Tier4_1Scenario) -> Result<Tier4_1Result> {
    let narrative = std::fs::read_to_string(&scenario.narrative_path)
        .with_context(|| format!("reading narrative `{}`", scenario.narrative_path.display()))?;

    let policy_bytes = std::fs::read(&scenario.interpretation_policy).with_context(|| {
        format!(
            "reading interpretation policy `{}`",
            scenario.interpretation_policy.display()
        )
    })?;
    let policy: Value = serde_json::from_slice(&policy_bytes).with_context(|| {
        format!(
            "parsing interpretation policy `{}`",
            scenario.interpretation_policy.display()
        )
    })?;
    let cfg =
        ExtractorConfig::from_policy(&policy).context("building ExtractorConfig from policy")?;

    let claims = claim_extractor::extract_claims(&narrative, &cfg);

    let tables_root = scenario
        .result_table_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    if !scenario.result_table_path.exists() {
        anyhow::bail!(
            "result table `{}` does not exist",
            scenario.result_table_path.display()
        );
    }
    let report: ClaimVerificationReport = verify_claims(&claims, &tables_root, &cfg);

    let mut passed = report.n_mismatch == scenario.expected_mismatch_count;
    if let Some(floor) = scenario.expected_min_verified {
        if report.n_verified < floor {
            passed = false;
        }
    }

    // Precision and recall under the fabrication-catch framing.
    // TP: mismatches that were genuine (capped at expected count).
    // FP: mismatches reported in excess of the planted count.
    // FN: planted fabrications that were not caught.
    let tp = report.n_mismatch.min(scenario.expected_mismatch_count);
    let fp = report
        .n_mismatch
        .saturating_sub(scenario.expected_mismatch_count);
    let fn_ = scenario
        .expected_mismatch_count
        .saturating_sub(report.n_mismatch);

    let precision = if tp + fp == 0 {
        // No mismatches reported; precision is undefined — treat as 1.0 when
        // there are also no planted fabrications, 0.0 otherwise.
        if scenario.expected_mismatch_count == 0 {
            1.0_f64
        } else {
            0.0_f64
        }
    } else {
        tp as f64 / (tp + fp) as f64
    };

    let recall = if tp + fn_ == 0 {
        // No fabrications were planted and none caught — perfect recall.
        1.0_f64
    } else {
        tp as f64 / (tp + fn_) as f64
    };

    Ok(Tier4_1Result {
        scenario_id: scenario.scenario_id.clone(),
        n_checked: report.n_checked,
        n_verified: report.n_verified,
        n_mismatch: report.n_mismatch,
        n_unverifiable: report.n_unverifiable,
        expected_mismatch_count: scenario.expected_mismatch_count,
        passed,
        precision,
        recall,
    })
}

/// Load every `*.yaml` scenario file under `corpus_dir`.
pub fn load_corpus(corpus_dir: &Path) -> Result<Vec<Tier4_1Scenario>> {
    let mut out: Vec<Tier4_1Scenario> = Vec::new();
    let rd = std::fs::read_dir(corpus_dir)
        .with_context(|| format!("opening {}", corpus_dir.display()))?;
    let mut paths: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s == "yaml" || s == "yml")
                .unwrap_or(false)
        })
        .collect();
    paths.sort();
    for p in &paths {
        let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        let scenario: Tier4_1Scenario = serde_yaml_ng::from_str(&text)
            .with_context(|| format!("parsing scenario {}", p.display()))?;
        out.push(scenario);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_corpus_loads_to_empty_vec() {
        let tmp = tempfile::tempdir().unwrap();
        let scenarios = load_corpus(tmp.path()).unwrap();
        assert!(scenarios.is_empty());
    }

    /// Regression gate: every scenario's verifier outcome must match its
    /// adjudicated `expected_mismatch_count` (and `expected_min_verified`
    /// floor). The labels are derived from the narrative-vs-table rubric, so a
    /// failure here means EITHER the verifier drifted OR a label rotted — both
    /// require re-adjudication, not a silent bump. Scenario paths are
    /// workspace-root-relative; rebase them so the test is CWD-independent.
    #[test]
    fn corpus_passes_authored_ground_truth() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("resolve workspace root");
        let corpus = root.join("crates/eval-adapters/tests/tier-4-1-corpus");
        let scenarios = load_corpus(&corpus).expect("load corpus");
        assert_eq!(scenarios.len(), 50, "corpus size drifted from 50 scenarios");
        let mut failures = Vec::new();
        for mut s in scenarios {
            s.narrative_path = root.join(&s.narrative_path);
            s.result_table_path = root.join(&s.result_table_path);
            s.interpretation_policy = root.join(&s.interpretation_policy);
            let r = run_one(&s).expect("run scenario");
            if !r.passed {
                failures.push(format!(
                    "{}: got mismatch={} verified={} but expected_mismatch={}",
                    r.scenario_id, r.n_mismatch, r.n_verified, r.expected_mismatch_count
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{} scenario(s) diverged from adjudicated ground truth:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}
