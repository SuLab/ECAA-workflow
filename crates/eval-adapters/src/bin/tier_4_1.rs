//! tier-4-1 — claim_verifier fabrication-catch runner.
//!
//! Run from the workspace root (scenario YAML paths are workspace-root-relative).

use anyhow::{Context, Result};
use ecaa_workflow_eval_adapters::tier_4_1_claim_verifier_fabrications as tier41;
use std::path::Path;

fn main() -> Result<()> {
    let corpus = Path::new("crates/eval-adapters/tests/tier-4-1-corpus");
    let scenarios = tier41::load_corpus(corpus)
        .with_context(|| format!("loading corpus from {}", corpus.display()))?;

    let (
        mut checked,
        mut verified,
        mut mismatch,
        mut unverifiable,
        mut suspicious,
        mut expected,
        mut passed,
    ) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);

    for s in &scenarios {
        let r = tier41::run_one(s).with_context(|| format!("scoring scenario {}", s.scenario_id))?;
        println!(
            "{:<28} checked={:>2} verified={:>2} mismatch={:>2} unverifiable={:>2} suspicious={:>2} expected_mismatch={:>2} {}",
            r.scenario_id, r.n_checked, r.n_verified, r.n_mismatch, r.n_unverifiable,
            r.n_suspicious, r.expected_mismatch_count, if r.passed { "PASS" } else { "FAIL" }
        );
        checked += r.n_checked;
        verified += r.n_verified;
        mismatch += r.n_mismatch;
        unverifiable += r.n_unverifiable;
        suspicious += r.n_suspicious;
        expected += r.expected_mismatch_count;
        if r.passed {
            passed += 1;
        }
    }

    println!("\n=== TOTALS over {} scenarios ===", scenarios.len());
    println!("claims checked (n_checked):       {checked}");
    println!("verified:                         {verified}");
    println!("mismatches caught (fabrications): {mismatch}");
    println!("unverifiable:                     {unverifiable}");
    println!("suspicious (review-required):     {suspicious}");
    println!("expected_mismatch (authored):     {expected}");
    println!("scenarios passing ground truth:   {passed}/{}", scenarios.len());
    Ok(())
}
