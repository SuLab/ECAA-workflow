//! Debug: dump per-claim verdicts for one tier-4-1 scenario.
//! Usage (from workspace root): cargo run --bin dump_verdicts -- <scenario_id>

use anyhow::{Context, Result};
use ecaa_workflow_core::claim_extractor::{self, ExtractorConfig};
use ecaa_workflow_core::claim_verifier::{verify_claims, ClaimStatus};
use ecaa_workflow_eval_adapters::tier_4_1_claim_verifier_fabrications as t;
use serde_json::Value;
use std::path::Path;

fn main() -> Result<()> {
    let id = std::env::args().nth(1).context("need scenario_id arg")?;
    let scenarios = t::load_corpus(Path::new("crates/eval-adapters/tests/tier-4-1-corpus"))?;
    let s = scenarios
        .iter()
        .find(|s| s.scenario_id == id)
        .with_context(|| format!("scenario {id} not found"))?;

    let narrative = std::fs::read_to_string(&s.narrative_path)?;
    let policy: Value = serde_json::from_slice(&std::fs::read(&s.interpretation_policy)?)?;
    let cfg = ExtractorConfig::from_policy(&policy)?;
    let claims = claim_extractor::extract_claims(&narrative, &cfg);
    let tables_root = s.result_table_path.parent().unwrap().to_path_buf();
    let report = verify_claims(&claims, &tables_root, &cfg);

    println!(
        "scenario {id}: extracted={} checked={} verified={} mismatch={} unverifiable={} (expected_mismatch={})",
        claims.len(), report.n_checked, report.n_verified, report.n_mismatch,
        report.n_unverifiable, s.expected_mismatch_count,
    );
    for v in &report.verdicts {
        let kind = match &v.status {
            ClaimStatus::Verified => "VERIFIED",
            ClaimStatus::Mismatch { .. } => "MISMATCH",
            ClaimStatus::Unverifiable { .. } => "UNVERIF ",
        };
        println!(
            "  {kind} entity={:<10} dir={:?} eff={:?} contract={:?}  | {:?}",
            v.claim.entity, v.claim.direction, v.claim.effect_size, v.claim.contract, v.status
        );
    }
    Ok(())
}
