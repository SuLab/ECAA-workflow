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
    // Planted-keyed precision/recall (over scenarios declaring planted_fabrications).
    // tp = caught plants (min mismatch, planted); mismatch_on_planted = ALL
    // mismatches those scenarios reported (so any FP shows up as tp < reported).
    let (
        mut planted_total,
        mut tp_on_planted,
        mut mismatch_on_planted,
        mut flagged_on_planted,
        mut planted_scenarios,
    ) = (0usize, 0usize, 0usize, 0usize, 0usize);

    for s in &scenarios {
        let r =
            tier41::run_one(s).with_context(|| format!("scoring scenario {}", s.scenario_id))?;
        println!(
            "{:<28} checked={:>2} verified={:>2} mismatch={:>2} unverifiable={:>2} suspicious={:>2} planted={:>2} {}",
            r.scenario_id, r.n_checked, r.n_verified, r.n_mismatch, r.n_unverifiable,
            r.n_suspicious,
            s.planted_fabrications.map(|p| p as i64).unwrap_or(-1),
            if r.passed { "PASS" } else { "FAIL" }
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
        if let Some(p) = s.planted_fabrications {
            planted_total += p;
            tp_on_planted += r.n_mismatch.min(p);
            mismatch_on_planted += r.n_mismatch;
            flagged_on_planted += (r.n_mismatch + r.n_suspicious).min(p);
            planted_scenarios += 1;
        }
    }

    println!("\n=== TOTALS over {} scenarios ===", scenarios.len());
    println!("claims checked (n_checked):       {checked}");
    println!("verified:                         {verified}");
    println!("mismatches caught (fabrications): {mismatch}");
    println!("unverifiable:                     {unverifiable}");
    println!("suspicious (review-required):     {suspicious}");
    println!("expected_mismatch (authored):     {expected}");
    println!(
        "scenarios passing ground truth:   {passed}/{}",
        scenarios.len()
    );
    // Planted-keyed metrics (the roadmap's G6 gate). Precision = caught hard
    // mismatches / total mismatches reported on planted scenarios (must be 1.0:
    // no false positive). Recall = caught (Mismatch) / planted; flagged-recall
    // additionally credits soft Suspicious flags.
    if planted_scenarios > 0 {
        let precision = if mismatch_on_planted == 0 {
            1.0
        } else {
            tp_on_planted as f64 / mismatch_on_planted as f64
        };
        let recall = if planted_total == 0 {
            1.0
        } else {
            tp_on_planted as f64 / planted_total as f64
        };
        let flagged_recall = if planted_total == 0 {
            1.0
        } else {
            flagged_on_planted as f64 / planted_total as f64
        };
        println!(
            "\n=== PLANTED-KEYED METRICS over {planted_scenarios} scenarios with planted_fabrications ==="
        );
        println!("planted fabrications:             {planted_total}");
        println!("caught as Mismatch (TP):          {tp_on_planted}");
        println!("mismatches reported on these:     {mismatch_on_planted}");
        println!("flagged (Mismatch+Suspicious):    {flagged_on_planted}");
        println!("precision (TP / reported):        {precision:.3}  (gate: 1.000)");
        println!("recall   (TP / planted):          {recall:.3}");
        println!("flagged-recall (incl Suspicious): {flagged_recall:.3}");
    }
    Ok(())
}
