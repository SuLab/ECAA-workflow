//! Tests for `modality_bounds`:
//!
//! 1. Every real modality manifest in `config/modalities/` either has at
//!    least one `semantic_equivalence_bounds` entry or an explicit empty list.
//! 2. `check_bound` correctness across all five operators.
//! 3. `load_bounds_for_modality` round-trips through the schema-extended YAML.

use ecaa_workflow_core::modality_bounds::{
    check_bound, load_bounds_for_modality, BoundOperator, SemanticEquivalenceBound,
};
use std::path::Path;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn modalities_dir() -> std::path::PathBuf {
    repo_root().join("config/modalities")
}

/// All 21 modality YAMLs must be loadable and must have at least one bound
/// (or an explicit empty list — the schema permits omission; this test
/// verifies that E19 wired bounds into every manifest).
#[test]
fn all_21_modalities_have_bounds_key() {
    let dir = modalities_dir();
    let expected_ids = [
        "bulk_rnaseq",
        "single_cell_rnaseq",
        "variant_calling",
        "chip_seq",
        "atac_seq",
        "metagenomics",
        "proteomics",
        "cut_tag",
        "chip_exo",
        "ribo_seq",
        "immunopeptidomics",
        "hi_chip",
        "starr_seq",
        "single_cell_vdj",
        "crispr_screen_scrnaseq",
        "methylation",
        "spatial_transcriptomics",
        "long_read_rnaseq",
        "gwas",
        "ehr_clinical_prediction",
        "generic_omics",
    ];
    for id in expected_ids {
        let bounds = load_bounds_for_modality(id, &dir)
            .unwrap_or_else(|e| panic!("load_bounds_for_modality({id}) failed: {e}"));
        // Every manifest in E19 received at least one bound; empty list is
        // intentional only when the manifest explicitly declares
        // `semantic_equivalence_bounds: []`.  Omission (missing key) also
        // deserialises to an empty Vec, so this assertion catches any
        // manifest that was silently skipped during E19.
        assert!(
            !bounds.is_empty(),
            "modality '{id}' has no semantic_equivalence_bounds entries — \
             add at least one bound or document the intentional omission"
        );
    }
}

/// `load_bounds_for_modality` deserialises metric / operator / threshold
/// fields correctly for a few spot-checked modalities.
#[test]
fn bulk_rnaseq_bounds_parse_correctly() {
    let dir = modalities_dir();
    let bounds = load_bounds_for_modality("bulk_rnaseq", &dir).expect("load bulk_rnaseq");
    let log2fc = bounds
        .iter()
        .find(|b| b.metric == "log2fc_abs_delta")
        .expect("log2fc_abs_delta bound must exist");
    assert_eq!(log2fc.operator, BoundOperator::Lte);
    assert!((log2fc.threshold - 0.01).abs() < f64::EPSILON);
    assert!(log2fc.applies_to.as_deref().is_some());
    assert!(log2fc.rationale.as_deref().is_some());
}

#[test]
fn variant_calling_vcf_concordance_parses_correctly() {
    let dir = modalities_dir();
    let bounds = load_bounds_for_modality("variant_calling", &dir).expect("load variant_calling");
    let vcf = bounds
        .iter()
        .find(|b| b.metric == "vcf_concordance")
        .expect("vcf_concordance bound must exist");
    assert_eq!(vcf.operator, BoundOperator::Gte);
    assert!((vcf.threshold - 0.995).abs() < f64::EPSILON);
}

#[test]
fn methylation_per_cpg_rho_uses_gt_operator() {
    let dir = modalities_dir();
    let bounds = load_bounds_for_modality("methylation", &dir).expect("load methylation");
    let cpg = bounds
        .iter()
        .find(|b| b.metric == "per_cpg_rho")
        .expect("per_cpg_rho bound must exist");
    assert_eq!(cpg.operator, BoundOperator::Gt);
    assert!((cpg.threshold - 0.99).abs() < f64::EPSILON);
}

#[test]
fn proteomics_cv_max_uses_lt_operator() {
    let dir = modalities_dir();
    let bounds = load_bounds_for_modality("proteomics", &dir).expect("load proteomics");
    let cv = bounds
        .iter()
        .find(|b| b.metric == "cv_max")
        .expect("cv_max bound must exist");
    assert_eq!(cv.operator, BoundOperator::Lt);
}

// ---------------------------------------------------------------------------
// check_bound correctness tests
// ---------------------------------------------------------------------------

fn make_bound(metric: &str, operator: BoundOperator, threshold: f64) -> SemanticEquivalenceBound {
    SemanticEquivalenceBound {
        metric: metric.to_string(),
        operator,
        threshold,
        applies_to: None,
        rationale: None,
    }
}

#[test]
fn check_bound_lte_passes_at_exact_threshold() {
    let b = make_bound("log2fc_abs_delta", BoundOperator::Lte, 0.01);
    assert!(check_bound(&b, 0.01), "exact threshold must pass Lte");
    assert!(check_bound(&b, 0.009), "below threshold must pass Lte");
    assert!(!check_bound(&b, 0.011), "above threshold must fail Lte");
}

#[test]
fn check_bound_gte_passes_at_exact_threshold() {
    let b = make_bound("ari", BoundOperator::Gte, 0.90);
    assert!(check_bound(&b, 0.90), "exact threshold must pass Gte");
    assert!(check_bound(&b, 0.95), "above threshold must pass Gte");
    assert!(!check_bound(&b, 0.89), "below threshold must fail Gte");
}

#[test]
fn check_bound_lt_strict() {
    let b = make_bound("cv_max", BoundOperator::Lt, 0.20);
    assert!(check_bound(&b, 0.19), "strictly below must pass Lt");
    assert!(
        !check_bound(&b, 0.20),
        "equal to threshold must fail Lt (strict)"
    );
    assert!(!check_bound(&b, 0.21), "above threshold must fail Lt");
}

#[test]
fn check_bound_gt_strict() {
    let b = make_bound("per_cpg_rho", BoundOperator::Gt, 0.99);
    assert!(check_bound(&b, 0.991), "strictly above must pass Gt");
    assert!(
        !check_bound(&b, 0.99),
        "equal to threshold must fail Gt (strict)"
    );
    assert!(!check_bound(&b, 0.989), "below threshold must fail Gt");
}

#[test]
fn check_bound_approx_within_one_percent() {
    let b = make_bound("some_metric", BoundOperator::Approx, 1.0);
    // 1 % tolerance: [0.99, 1.01]
    assert!(check_bound(&b, 1.0), "exact value passes approx");
    assert!(check_bound(&b, 1.009), "within 1% passes approx");
    assert!(check_bound(&b, 0.991), "within 1% below passes approx");
    assert!(!check_bound(&b, 1.02), "2% above fails approx");
    assert!(!check_bound(&b, 0.98), "2% below fails approx");
}

#[test]
fn check_bound_approx_zero_threshold_special_case() {
    let b = make_bound("delta", BoundOperator::Approx, 0.0);
    assert!(
        check_bound(&b, 0.0),
        "zero observed vs zero threshold passes"
    );
    assert!(
        !check_bound(&b, 0.001),
        "nonzero observed vs zero threshold fails"
    );
}
