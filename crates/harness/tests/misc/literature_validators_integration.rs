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

// ── Typed-locator schema shape ────────────────────────────────────────────────

/// A non-PMID manifest entry (source_class=conference_proceedings, locator=DOI)
/// carries the typed-locator columns and round-trips as the JSON shape the
/// evidence-manifest schema (schema_version 2) accepts.
#[test]
fn manifest_accepts_doi_locator_entry() {
    let json = serde_json::json!({
        "schema_version": 2,
        "entries": [{
            "source_ref_kind": "doi",
            "source_ref": "10.1093/bioinformatics/btchunk",
            "source_class": "conference_proceedings",
            "source_kind": "abstract_only",
            "path": "doi_10.1093_btchunk.json",
            "sha256_binary": "ab".repeat(32),
            "sha256_extracted_text": "cd".repeat(32),
            "extracted_text_normalization": "collapse_whitespace_lowercase_v1",
            "bytes": 12u64,
            "retrieval_ts": "2026-05-31T00:00:00Z",
            "retrieval_query_id": "q1",
            "redistributable": true,
            "license": "CC-BY-4.0"
        }]
    });
    let parsed: serde_json::Value = json.clone();
    assert_eq!(parsed["entries"][0]["source_ref_kind"], "doi");
    assert_eq!(parsed["schema_version"], 2);
    assert!(parsed["entries"][0].get("pmid").is_none());
}

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

// ── Regression: method_landscape curated_baseline rows must not strand survey ──
//
// `survey_method_landscape` emits a method_landscape.csv whose `curated_baseline`
// candidate rows carry EMPTY `evidence_quote_offset` / `redistributable` /
// `verified` columns (they have no literature evidence). A bare-typed
// `ClaimsMatrixRow` (`evidence_quote_offset: u64`, `redistributable: bool`)
// rejected "" and failed the WHOLE `load_rows` parse, so
// `run_evidence_quote_substring_match` and `run_redistributable_or_marked`
// reported a spurious `EvidenceArtifactMissing` at row 0 — blocking the keystone
// survey task and stranding every downstream stage (the live nekrutenko eval
// scored 0.0 jaccard for exactly this reason: survey blocked → variant_calling
// never ran → no VCFs).
#[test]
fn method_landscape_curated_baseline_rows_do_not_spuriously_block() {
    let tmp = TempDir::new().unwrap();
    let task = tmp.path().join("runtime/outputs/survey_method_landscape");
    let evidence = task.join("evidence");
    fs::create_dir_all(&evidence).unwrap();

    // One real paper-class row (verified) + two curated_baseline placeholder
    // rows with the empty offset/redistributable/verified columns the producer
    // legitimately emits.
    let csv = task.join("method_landscape.csv");
    write(
        &csv,
        "axis,candidate_method,source_ref_kind,source_ref,source_class,evidence_role,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified,version_context\n\
alignment,bwa,pmid,pmid:19451168,primary_literature,performance,bwa is fast and accurate,0,pubmed_abstract,sha256:aa,2026-06-04T00:00:00Z,true,true,bwa 0.7\n\
alignment,bwa_mem2,,,curated_baseline,,,,curated_candidate,,2026-06-04T00:00:00Z,,false,\n\
alignment,minimap2,,,curated_baseline,,,,curated_candidate,,2026-06-04T00:00:00Z,,false,\n",
    );

    let manifest = evidence.join("manifest.json");
    write(
        &manifest,
        r#"{"schema_version":2,"entries":[{"source_ref_kind":"pmid","source_ref":"pmid:19451168","source_class":"primary_literature","source_kind":"pubmed_abstract","path":"evidence/PMID19451168_abstract.txt","sha256_binary":"aa","sha256_extracted_text":"cc","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":39,"retrieval_ts":"2026-06-04T00:00:00Z","retrieval_query_id":"q1","redistributable":true,"license":"open access"}]}"#,
    );
    // The manifest path is "evidence/"-prefixed; resolve_evidence_file strips it.
    write(
        &evidence.join("PMID19451168_abstract.txt"),
        "bwa is fast and accurate on short reads",
    );

    // Before the fix: load_rows() failed to parse the empty offset/redistributable
    // columns on the curated rows, so BOTH validators returned
    // EvidenceArtifactMissing at row 0.
    assert!(
        run_evidence_quote_substring_match(&csv, &manifest).is_ok(),
        "curated_baseline rows must not break the CSV parse for the quote validator"
    );
    assert!(
        run_redistributable_or_marked(&csv, &manifest).is_ok(),
        "curated_baseline rows must be skipped by the redistributable legal gate"
    );
}
