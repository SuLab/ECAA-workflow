//! Integration tests for the literature validator runners
//! against the 6 fixture scenarios from `tests/conversation-fixtures/literature/`.
//! These exercise the harness-side validator pipeline end-to-end (CSV
//! parse + manifest parse + substring check + concordance flag check)
//! without needing E-utilities network access.
//!
//! Each test constructs the package layout the runners expect and
//! verifies the runner's pass/fail outcome matches `expected_outcome.json`.
//!
//! Scenarios covered:
//! 1. oa_hit_bulk_de — green path, gene entity kind
//! 2. oa_hit_chip_peaks — cross-modality green, region entity kind
//! 3. oa_hit_variant — cross-modality green, variant entity kind
//! 4. abstract_only_fallback — mixed source_kind rows (oa + abstract)
//! 5. quote_mismatch_blocks — tampered quote → typed QuoteNotInSource cause
//! 6. adversarial_concordance — out-of-set concordance_flag → typed cause

use ecaa_workflow_core::blocker::{LiteratureClaimFailureKind, ValidationFailureCause};
use ecaa_workflow_harness::literature_validators::{
    run_concordance_flag_in_closed_set, run_evidence_quote_substring_match, run_pmid_resolves,
    run_redistributable_or_marked,
};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn write(p: &Path, s: &str) {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, s).unwrap();
}

/// Build the minimal green-path layout for a prior_claims_matrix row with the
/// given `entity`, `entity_kind`, `pmid`, `evidence_quote`, and `source_xml`.
/// Returns (csv_path, manifest_path).
fn scaffold_prior_green(
    dir: &Path,
    entity: &str,
    entity_kind: &str,
    pmid: &str,
    evidence_quote: &str,
    source_xml: &str,
) -> (PathBuf, PathBuf) {
    let task = dir.join("runtime/outputs/review_prior_work");
    let evidence = task.join("evidence");
    fs::create_dir_all(&evidence).unwrap();

    let csv = task.join("prior_claims_matrix.csv");
    write(
        &csv,
        &format!(
            "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n\
{entity},{entity_kind},{pmid},{evidence_quote},0,pmc_oa_full_text,sha256:aa,2026-05-14T00:00:00Z,true,true\n"
        ),
    );

    let manifest = evidence.join("manifest.json");
    write(
        &manifest,
        &format!(
            r#"{{"schema_version":1,"entries":[{{"pmid":"{pmid}","source_kind":"pmc_oa_full_text","path":"{pmid}.xml","sha256_binary":"aa","sha256_extracted_text":"cc","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":{},"retrieval_ts":"2026-05-14T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"CC-BY-4.0"}}]}}"#,
            source_xml.len()
        ),
    );

    write(&evidence.join(format!("{pmid}.xml")), source_xml);
    (csv, manifest)
}

// ── Scenario 1: oa_hit_bulk_de — green path, gene entity kind ────────────────

#[test]
fn fixture_oa_hit_bulk_de_all_validators_pass() {
    let tmp = TempDir::new().unwrap();
    let (csv, manifest) = scaffold_prior_green(
        tmp.path(),
        "ACAN",
        "gene",
        "28123456",
        "acan reduction in disc tissue",
        "ACAN reduction in disc tissue is well established in IVD degeneration studies.",
    );

    assert!(
        run_pmid_resolves(&csv, &manifest).is_ok(),
        "pmid_resolves should pass for oa_hit_bulk_de"
    );
    assert!(
        run_evidence_quote_substring_match(&csv, &manifest).is_ok(),
        "evidence_quote_substring_match should pass for oa_hit_bulk_de"
    );
    assert!(
        run_redistributable_or_marked(&csv, &manifest).is_ok(),
        "redistributable_or_marked should pass for oa_hit_bulk_de"
    );
}

// ── Scenario 2: oa_hit_chip_peaks — cross-modality green, region entity kind ─

#[test]
fn fixture_oa_hit_chip_peaks_region_shape_passes() {
    let tmp = TempDir::new().unwrap();
    let (csv, manifest) = scaffold_prior_green(
        tmp.path(),
        "chr1:1000-2000",
        "region",
        "28123456",
        "enriched myc binding at chr1:1000-2000",
        "Enriched MYC binding at chr1:1000-2000 was observed in K562 cells.",
    );

    assert!(
        run_pmid_resolves(&csv, &manifest).is_ok(),
        "pmid_resolves should pass for oa_hit_chip_peaks"
    );
    assert!(
        run_evidence_quote_substring_match(&csv, &manifest).is_ok(),
        "evidence_quote_substring_match should pass for region entity kind"
    );
    assert!(
        run_redistributable_or_marked(&csv, &manifest).is_ok(),
        "redistributable_or_marked should pass for oa_hit_chip_peaks"
    );
}

// ── Scenario 3: oa_hit_variant — cross-modality green, variant entity kind ───

#[test]
fn fixture_oa_hit_variant_shape_passes() {
    let tmp = TempDir::new().unwrap();
    let (csv, manifest) = scaffold_prior_green(
        tmp.path(),
        "APOE",
        "variant",
        "28123456",
        "rs429358 in apoe is associated with risk",
        "rs429358 in APOE is associated with risk of late-onset Alzheimer disease.",
    );

    assert!(
        run_pmid_resolves(&csv, &manifest).is_ok(),
        "pmid_resolves should pass for oa_hit_variant"
    );
    assert!(
        run_evidence_quote_substring_match(&csv, &manifest).is_ok(),
        "evidence_quote_substring_match should pass for variant entity kind"
    );
    assert!(
        run_redistributable_or_marked(&csv, &manifest).is_ok(),
        "redistributable_or_marked should pass for oa_hit_variant"
    );
}

// ── Scenario 4: abstract_only_fallback — mixed source_kind rows ──────────────

#[test]
fn fixture_abstract_only_fallback_mixed_source_kinds_pass() {
    let tmp = TempDir::new().unwrap();
    let task = tmp.path().join("runtime/outputs/review_prior_work");
    let evidence = task.join("evidence");
    fs::create_dir_all(&evidence).unwrap();

    // Row 0: pmc_oa_full_text source
    // Row 1: abstract_only source (NLM public-domain abstract)
    let csv = task.join("prior_claims_matrix.csv");
    write(
        &csv,
        "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n\
ACAN,gene,28123456,acan reduction in disc tissue,0,pmc_oa_full_text,sha256:aa,2026-05-14T00:00:00Z,true,true\n\
COMP,gene,28123457,comp degradation in cartilage,0,abstract_only,sha256:bb,2026-05-14T00:00:00Z,true,true\n",
    );

    let manifest = evidence.join("manifest.json");
    write(
        &manifest,
        r#"{"schema_version":1,"entries":[
{"pmid":"28123456","source_kind":"pmc_oa_full_text","path":"28123456.xml","sha256_binary":"aa","sha256_extracted_text":"cc","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":60,"retrieval_ts":"2026-05-14T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"CC-BY-4.0"},
{"pmid":"28123457","source_kind":"abstract_only","path":"28123457.abstract.json","sha256_binary":"bb","sha256_extracted_text":"dd","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":90,"retrieval_ts":"2026-05-14T00:00:00Z","retrieval_query_id":"q002","redistributable":true,"license":"NLM-public-domain-abstract"}
]}"#,
    );

    write(
        &evidence.join("28123456.xml"),
        "ACAN reduction in disc tissue is present in this OA full-text article.",
    );
    // abstract_only entries use a JSON wrapper around the NLM XML fragment.
    write(
        &evidence.join("28123457.abstract.json"),
        r#"{"pmid":"28123457","raw_xml":"<MedlineCitation><Abstract>COMP degradation in cartilage is reported here</Abstract></MedlineCitation>"}"#,
    );

    assert!(
        run_pmid_resolves(&csv, &manifest).is_ok(),
        "pmid_resolves should pass for both OA and abstract-only rows"
    );
    assert!(
        run_redistributable_or_marked(&csv, &manifest).is_ok(),
        "redistributable_or_marked should accept abstract_only rows with redistributable: true"
    );
    // evidence_quote_substring_match: Row 0 (pmc_oa_full_text) has the quote in 28123456.xml.
    // Row 1 (abstract_only): the quote is checked against the raw JSON file contents;
    // the normalized JSON contains "comp degradation in cartilage" so it passes.
    assert!(
        run_evidence_quote_substring_match(&csv, &manifest).is_ok(),
        "evidence_quote_substring_match should pass for mixed source_kind rows"
    );
}

// ── Scenario 5: quote_mismatch_blocks — typed QuoteNotInSource cause ──────────

#[test]
fn fixture_quote_mismatch_blocks_with_typed_cause() {
    let tmp = TempDir::new().unwrap();
    let task = tmp.path().join("runtime/outputs/review_prior_work");
    let evidence = task.join("evidence");
    fs::create_dir_all(&evidence).unwrap();

    // The evidence_quote claims a substring that is NOT present in the source XML.
    let csv = task.join("prior_claims_matrix.csv");
    write(
        &csv,
        "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n\
ACAN,gene,28123456,this string is fabricated and not in the source,0,pmc_oa_full_text,sha256:aa,2026-05-14T00:00:00Z,true,true\n",
    );

    let manifest = evidence.join("manifest.json");
    write(
        &manifest,
        r#"{"schema_version":1,"entries":[{"pmid":"28123456","source_kind":"pmc_oa_full_text","path":"28123456.xml","sha256_binary":"aa","sha256_extracted_text":"cc","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":26,"retrieval_ts":"2026-05-14T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"CC-BY-4.0"}]}"#,
    );
    write(
        &evidence.join("28123456.xml"),
        "some other content entirely",
    );

    let err = run_evidence_quote_substring_match(&csv, &manifest)
        .expect_err("should fail with QuoteNotInSource for tampered quote");

    assert!(
        matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::QuoteNotInSource,
                ..
            }
        ),
        "expected LiteratureClaim {{ kind: QuoteNotInSource }}, got: {:?}",
        err.1
    );
}

// ── Scenario 6: adversarial — out-of-set concordance_flag ────────────────────
//
// This test mirrors the adversarial_unretrieved_pmid fixture at the
// concordance_flag validator level: an agent attempting to launder a
// hallucinated citation might write a bogus concordance_flag string.
// The run_concordance_flag_in_closed_set runner must reject it with
// the typed InvalidConcordanceFlag cause.

#[test]
fn fixture_adversarial_concordance_flag_is_closed_set() {
    let tmp = TempDir::new().unwrap();
    let task = tmp
        .path()
        .join("runtime/outputs/contextualize_findings_with_literature");
    let evidence = task.join("evidence");
    fs::create_dir_all(&evidence).unwrap();

    // Agent invents a concordance_flag outside the allowed closed set.
    let csv = task.join("claims_evidence_matrix.csv");
    write(
        &csv,
        "finding_id,entity,entity_kind,prior_pmids,concordance_flag,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n\
gene_1,ACAN,gene,,hallucinated_flag,,0,none,none,2026-05-14T00:00:00Z,true,true\n",
    );

    let manifest = evidence.join("manifest.json");
    write(&manifest, r#"{"schema_version":1,"entries":[]}"#);

    let err = run_concordance_flag_in_closed_set(&csv, &manifest)
        .expect_err("should fail with InvalidConcordanceFlag for out-of-set value");

    assert!(
        matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::InvalidConcordanceFlag,
                ..
            }
        ),
        "expected LiteratureClaim {{ kind: InvalidConcordanceFlag }}, got: {:?}",
        err.1
    );
}

// ── Closed-set boundary: valid concordance flags are accepted ─────────────────

#[test]
fn valid_concordance_flags_all_accepted() {
    let tmp = TempDir::new().unwrap();
    let task = tmp
        .path()
        .join("runtime/outputs/contextualize_findings_with_literature");
    let evidence = task.join("evidence");
    fs::create_dir_all(&evidence).unwrap();

    let flags = [
        "same_direction",
        "opposite_direction",
        "no_prior_finding",
        "unverifiable",
    ];
    let rows: String = flags
        .iter()
        .enumerate()
        .map(|(i, flag)| {
            format!("gene_{i},GENE{i},gene,,{flag},,0,none,none,2026-05-14T00:00:00Z,true,true\n")
        })
        .collect();
    let csv = task.join("claims_evidence_matrix.csv");
    write(
        &csv,
        &format!(
            "finding_id,entity,entity_kind,prior_pmids,concordance_flag,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n{rows}"
        ),
    );
    let manifest = evidence.join("manifest.json");
    write(&manifest, r#"{"schema_version":1,"entries":[]}"#);

    assert!(
        run_concordance_flag_in_closed_set(&csv, &manifest).is_ok(),
        "all four valid concordance flags should be accepted"
    );
}
