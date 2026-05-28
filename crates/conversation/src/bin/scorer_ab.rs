//! scorer_ab — A/B the rubric scorer against Sonnet 4.6 (baseline) vs
//! Haiku 4.5 (candidate) over a curated corpus of representative
//! transcripts. Hits the live Anthropic API twice per transcript; gated
//! behind `SWFC_LIVE_API=1 + ANTHROPIC_API_KEY`. Emits CSV to stdout and
//! asserts the ship criteria:
//!
//! - Mean total absolute delta ≤ 1.0 points (out of 18)
//! - No single dimension has mean absolute delta > 0.5
//! - Pass/fail agreement ≥ 90% (14/18 `PASS_THRESHOLD`)
//!
//! The corpus is hand-curated (not a fixture-runner materialization) so
//! the A/B input stays byte-stable across runs — what matters for an A/B
//! is whether Sonnet and Haiku *agree* on a fixed input, not whether the
//! input is runtime-realistic.

use anyhow::{Context, Result};
use scripps_workflow_conversation::{
    score_transcript_with_model, AnthropicClient, LlmBackend, MetricsStore, ModelId, RubricScore,
    Turn,
};
use std::process::ExitCode;
use std::sync::Arc;

const PASS_THRESHOLD: u8 = 14; // mirrors RubricScore::PASS_THRESHOLD

struct AbCase {
    name: &'static str,
    rubric_notes: &'static str,
    transcript: Vec<Turn>,
}

struct CmpRow {
    name: &'static str,
    sonnet: RubricScore,
    haiku: RubricScore,
}

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(e) = run().await {
        eprintln!("scorer_ab: {e:#}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

async fn run() -> Result<()> {
    if std::env::var("SWFC_LIVE_API").ok().as_deref() != Some("1") {
        eprintln!("SKIP: SWFC_LIVE_API=1 required (scorer_ab hits the live Anthropic API).");
        eprintln!(
            "      SWFC_ANTHROPIC_API_KEY must also be set (legacy ANTHROPIC_API_KEY accepted)."
        );
        return Ok(());
    }

    let client = AnthropicClient::new()
        .context("failed to construct AnthropicClient — is SWFC_ANTHROPIC_API_KEY set?")?;
    let backend: Arc<dyn LlmBackend> = Arc::new(client);
    let metrics = MetricsStore::new();

    let corpus = curated_corpus();
    eprintln!(
        "scorer_ab: scoring {} transcript(s) with Sonnet 4.6 and Haiku 4.5…",
        corpus.len()
    );

    let mut rows = Vec::with_capacity(corpus.len());
    for case in &corpus {
        eprintln!("  → {}", case.name);
        let sonnet = score_transcript_with_model(
            backend.clone(),
            &metrics,
            uuid::Uuid::new_v4(),
            &case.transcript,
            case.rubric_notes,
            ModelId::Sonnet46,
        )
        .await
        .with_context(|| format!("Sonnet 4.6 scoring failed on case '{}'", case.name))?;

        let haiku = score_transcript_with_model(
            backend.clone(),
            &metrics,
            uuid::Uuid::new_v4(),
            &case.transcript,
            case.rubric_notes,
            ModelId::Haiku45,
        )
        .await
        .with_context(|| format!("Haiku 4.5 scoring failed on case '{}'", case.name))?;

        rows.push(CmpRow {
            name: case.name,
            sonnet,
            haiku,
        });
    }

    emit_csv(&rows);
    assert_ship_criteria(&rows)
}

fn emit_csv(rows: &[CmpRow]) {
    // Header: dimension columns × {sonnet, haiku, delta} + totals + pass/fail
    print!("case,");
    for dim in DIMENSIONS {
        print!("sonnet_{dim},haiku_{dim},delta_{dim},");
    }
    println!("sonnet_total,haiku_total,delta_total,sonnet_pass,haiku_pass,pass_agreement");

    for row in rows {
        print!("{},", row.name);
        for (i, _dim) in DIMENSIONS.iter().enumerate() {
            let s = dim_of(&row.sonnet, i);
            let h = dim_of(&row.haiku, i);
            let d = h as i16 - s as i16;
            print!("{s},{h},{d},");
        }
        let st = row.sonnet.total();
        let ht = row.haiku.total();
        let dt = ht as i16 - st as i16;
        let sp = st >= PASS_THRESHOLD;
        let hp = ht >= PASS_THRESHOLD;
        let agree = sp == hp;
        println!("{st},{ht},{dt},{sp},{hp},{agree}");
    }
}

/// Ship criteria from the plan — any failure = non-zero exit.
fn assert_ship_criteria(rows: &[CmpRow]) -> Result<()> {
    if rows.is_empty() {
        anyhow::bail!("no corpus rows — nothing to compare");
    }
    let n = rows.len() as f64;

    // Mean absolute total delta.
    let total_abs: f64 = rows
        .iter()
        .map(|r| (r.sonnet.total() as i16 - r.haiku.total() as i16).unsigned_abs() as f64)
        .sum();
    let mean_abs_total = total_abs / n;

    // Mean absolute per-dimension delta, worst dimension.
    let mut worst_dim_mean = 0.0f64;
    let mut worst_dim_name = "";
    for (i, name) in DIMENSIONS.iter().enumerate() {
        let sum: f64 = rows
            .iter()
            .map(|r| {
                let s = dim_of(&r.sonnet, i) as i16;
                let h = dim_of(&r.haiku, i) as i16;
                (s - h).unsigned_abs() as f64
            })
            .sum();
        let mean = sum / n;
        if mean > worst_dim_mean {
            worst_dim_mean = mean;
            worst_dim_name = name;
        }
    }

    // Pass/fail agreement.
    let agree: usize = rows
        .iter()
        .filter(|r| (r.sonnet.total() >= PASS_THRESHOLD) == (r.haiku.total() >= PASS_THRESHOLD))
        .count();
    let agreement = agree as f64 / n;

    eprintln!();
    eprintln!("── scorer_ab ship criteria ──────────────────");
    eprintln!("  cases                  : {}", rows.len());
    eprintln!("  mean |Δtotal|          : {mean_abs_total:.3}   (gate: ≤ 1.00)");
    eprintln!("  worst per-dim |Δmean|  : {worst_dim_mean:.3} ({worst_dim_name})   (gate: ≤ 0.50)");
    eprintln!(
        "  pass/fail agreement    : {agree}/{n:.0} = {:.0}%   (gate: ≥ 90%)",
        agreement * 100.0
    );
    eprintln!();

    let mut failed: Vec<String> = Vec::new();
    if mean_abs_total > 1.0 {
        failed.push(format!("mean |Δtotal| = {mean_abs_total:.3} exceeds 1.00"));
    }
    if worst_dim_mean > 0.5 {
        failed.push(format!(
            "per-dim mean |Δ| = {worst_dim_mean:.3} on '{worst_dim_name}' exceeds 0.50"
        ));
    }
    if agreement < 0.9 {
        failed.push(format!(
            "pass/fail agreement = {:.0}% below 90%",
            agreement * 100.0
        ));
    }
    if !failed.is_empty() {
        eprintln!("FAIL — do not flip the scorer default to Haiku 4.5:");
        for f in &failed {
            eprintln!("  · {f}");
        }
        anyhow::bail!("scorer_ab ship criteria failed");
    }
    eprintln!("PASS — Haiku 4.5 is within criteria; scorer default may flip.");
    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────

const DIMENSIONS: &[&str] = &[
    "naturalness",
    "continuity",
    "one_question",
    "method_neutrality",
    "claim_boundary",
    "tool_efficiency",
    "confirmation",
    "recovery",
    "hardware_awareness",
];

fn dim_of(s: &RubricScore, i: usize) -> u8 {
    match i {
        0 => s.naturalness,
        1 => s.continuity,
        2 => s.one_question,
        3 => s.method_neutrality,
        4 => s.claim_boundary,
        5 => s.tool_efficiency,
        6 => s.confirmation,
        7 => s.recovery,
        8 => s.hardware_awareness,
        _ => 0,
    }
}

/// Five hand-curated transcripts spanning the dimensions Haiku is most
/// likely to drift on vs Sonnet. Keeps the corpus byte-stable across
/// runs so a rerun is a true replay, not a fresh sample.
fn curated_corpus() -> Vec<AbCase> {
    vec![
        AbCase {
            name: "happy_path_scrnaseq",
            rubric_notes:
                "Clean scRNA-seq intake with the claim boundary restated in plain language and \
                 no method recommendations. Expected to score high across all dimensions.",
            transcript: vec![
                Turn::user(
                    "We have a single-cell RNA-seq meta-analysis of public IVD datasets — \
                     about 47 libraries from human intervertebral disc tissue across 7 studies. \
                     Goal is to compare degenerated vs healthy across NP and AF compartments.",
                ),
                Turn::assistant(
                    "Got it — 47 single-cell libraries across 7 IVD studies, comparing \
                     degenerated vs healthy in the NP and AF compartments. Worth flagging \
                     up front: this kind of meta-analysis reports statistical patterns across \
                     samples rather than direct causal drivers, so the write-up will focus \
                     on association strength and cell-type composition shifts rather than \
                     mechanism. Does that framing match what you want to report?",
                ),
                Turn::user("Yes, that's exactly the framing."),
                Turn::assistant(
                    "Good. I'll hand this to the execution agent once you accept the plan.",
                ),
            ],
        },
        AbCase {
            name: "method_recommender_leak",
            rubric_notes:
                "Assistant volunteers a specific normalization method (vst) unprompted — \
                 method_neutrality should score 0. Everything else is normal.",
            transcript: vec![
                Turn::user("We have bulk RNA-seq from 24 tumor vs 24 normal kidney samples."),
                Turn::assistant(
                    "24 tumor + 24 normal — noted. For that sample size I'd use vst normalization \
                     with DESeq2 and limma-voom as a secondary check. Want me to run that?",
                ),
                Turn::user("Sure."),
            ],
        },
        AbCase {
            name: "multi_question_turn",
            rubric_notes:
                "Assistant asks three questions in one turn — one_question should score 0 \
                 or 1. Everything else is otherwise fine.",
            transcript: vec![
                Turn::user("Metagenomic shotgun study, 60 stool samples from a dietary trial."),
                Turn::assistant(
                    "60 stool samples. What's the read length? Paired-end or single? Which \
                     sequencer? And how many samples per arm of the trial?",
                ),
                Turn::user("150bp paired-end, NovaSeq, 30 per arm."),
            ],
        },
        AbCase {
            name: "blocker_recovery",
            rubric_notes:
                "SME flags incomplete metadata as a blocker; assistant acknowledges, restates, \
                 and pivots to a scoped workaround. recovery should score 2; claim_boundary \
                 should score 2.",
            transcript: vec![
                Turn::user(
                    "ChIP-seq for a TF binding study — but we lost the sample sheet and can't \
                     map FASTQs back to conditions.",
                ),
                Turn::assistant(
                    "That's a blocker — without the sample sheet we can't call condition-level \
                     peaks. One scoped option: the agent can cluster the libraries by global \
                     read coverage signature first, then you can eyeball the clusters and \
                     relabel. That lets us move forward without fabricating metadata. Does \
                     that match what you're hoping to salvage?",
                ),
                Turn::user("Yes — let's try the coverage-clustering route."),
            ],
        },
        AbCase {
            name: "hardware_awareness_leak",
            rubric_notes:
                "Assistant quotes specific thread counts and BLAS flags in chat instead of \
                 deferring to the execution agent. hardware_awareness should score 0 or 1.",
            transcript: vec![
                Turn::user("Bulk RNA-seq, 80 samples, human."),
                Turn::assistant(
                    "80 human bulk libraries. I'll ask the agent to use STAR with --runThreadN 16 \
                     and OMP_NUM_THREADS=8 on an r6i.4xlarge. Pass for you?",
                ),
                Turn::user("Sounds right."),
            ],
        },
    ]
}
