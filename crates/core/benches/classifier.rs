//! Criterion microbenchmark for the classifier hot path.
//!
//! The classifier is on the chat critical path: every `append_intake_
//! prose` call runs it, so its p50 / p99 directly affects intake
//! responsiveness. The benchmark runs three representative intake
//! prose lengths (a one-line modality hint, a paragraph IVD-flavored
//! intake, and a multi-paragraph clinical-trial-flavored intake) so
//! regressions in any of the three are visible separately.
//!
//! Run with: `cargo bench -p scripps-workflow-core --bench classifier`.
//! Outputs to `target/criterion/classifier/<test-name>/report/index.html`.
//!
//! Per Round-4 §2, this benchmark is opt-in (not gated by `make all`)
//! so PRs don't pay the bench-time cost; future Bencher.dev /
//! iai-callgrind work moves continuous benching off the developer's
//! laptop.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use scripps_workflow_core::classify::Classifier;
use std::path::PathBuf;

fn config_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config/modality-keywords.yaml")
}

const SHORT_PROSE: &str = "single-cell RNA-seq, human PBMC samples, 10x Genomics 5'.";

const PARAGRAPH_PROSE: &str = "We have 47 single-cell RNA-seq libraries from a Phase II IVD \
    trial, prepared with 10x Genomics 5' chemistry against human PBMC samples. The primary \
    question is differential expression between degenerated cohort vs healthy controls; we \
    want clustering, marker discovery, and a pathway enrichment overlay on the differential \
    table. Data lives at GSE-scratch/ on the cluster, organized by sample.";

const LONG_PROSE: &str = "This is a Phase III randomized clinical trial registered as \
    NCT04567890. Primary endpoint: progression-free survival at 24 months, analyzed via \
    stratified Cox regression on the ITT analysis set. Secondary endpoints include overall \
    response rate and quality-of-life scores. We have multiomics data: bulk RNA-seq from \
    tumor biopsies (Illumina NovaSeq 6000, 100bp paired-end), targeted DNA panel \
    sequencing for variant calling, and flow cytometry for immune cell phenotyping. \
    Subgroup analyses planned by ECOG status, prior treatment line, and biomarker \
    expression. SAP frozen at intake; deviations would be post-hoc and require \
    pre-specified rationale. Reference: GRCh38.p14 with GENCODE 47 annotation. Sample \
    sizes: arm A n=240, arm B n=240, mITT after exclusions ~460. We need a deterministic \
    pipeline with full provenance suitable for FDA submission, including subgroup \
    fairness breakdowns by ethnicity, sex, and age band per CONSORT-AI.";

fn bench_classify(c: &mut Criterion) {
    let classifier = Classifier::load(&config_path()).expect("load classifier config");
    let mut group = c.benchmark_group("classifier");
    group.bench_function("short_prose", |b| {
        b.iter(|| classifier.classify(black_box(SHORT_PROSE)))
    });
    group.bench_function("paragraph_prose", |b| {
        b.iter(|| classifier.classify(black_box(PARAGRAPH_PROSE)))
    });
    group.bench_function("long_prose", |b| {
        b.iter(|| classifier.classify(black_box(LONG_PROSE)))
    });
    group.finish();
}

criterion_group!(benches, bench_classify);
criterion_main!(benches);
