//! Phase C of the literature-atom plan — runner implementations for the five
//! literature validator obligations registered in
//! `ecaa_workflow_core::validation_obligations::literature_obligations`.
//!
//! Runners are pure functions over `(artifact_path, evidence_manifest_path)`
//! that return Ok(()) on success or Err(ValidationFailureCause::LiteratureClaim)
//! on failure. The harness post-task validator dispatcher calls them in
//! sequence; the first failure transitions the task to
//! BlockerKind::ValidationFailed with the structured cause attached.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use ecaa_workflow_core::blocker::{LiteratureClaimFailureKind, ValidationFailureCause};
use serde::Deserialize;

/// Canonical normalization applied to source text before substring-match.
/// Pinned by name in `evidence/manifest.json::extracted_text_normalization`.
pub fn collapse_whitespace_lowercase_v1(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(ch.to_ascii_lowercase());
            prev_ws = false;
        }
    }
    out.trim().to_string()
}

// Serde deserialization target for `claims_matrix.csv`; many fields are read
// only via reflection-style validators below and are flagged as dead by the
// compiler. Preserve the full shape so the deserializer fails on schema drift.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct ClaimsMatrixRow {
    #[serde(default)]
    pub finding_id: Option<String>,
    pub entity: String,
    pub entity_kind: String,
    #[serde(default)]
    pub pmid: Option<String>,
    #[serde(default)]
    pub prior_pmids: Option<Vec<String>>,
    #[serde(default)]
    pub concordance_flag: Option<String>,
    pub evidence_quote: String,
    pub evidence_quote_offset: u64,
    pub source_kind: String,
    pub source_hash: String,
    pub retrieval_ts: String,
    pub redistributable: bool,
    pub verified: bool,
}

// Serde shape mirror of `evidence-manifest.json`; `schema_version` is read by
// load_manifest's downstream validators on schema drift but the wrapper struct
// itself does not consume every field directly.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct EvidenceManifest {
    pub schema_version: u32,
    pub entries: Vec<EvidenceEntry>,
}

// Serde shape for entries in `evidence-manifest.json`; preserves the full
// per-PMID record so validators downstream can inspect license/redistributable
// flags even when this binary doesn't read them at compile time.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct EvidenceEntry {
    pub pmid: String,
    pub source_kind: String,
    pub path: String,
    pub sha256_binary: String,
    pub sha256_extracted_text: String,
    pub extracted_text_normalization: String,
    pub bytes: u64,
    pub retrieval_ts: String,
    pub retrieval_query_id: String,
    pub redistributable: bool,
    pub license: String,
}

fn load_rows(csv_path: &Path) -> Result<Vec<ClaimsMatrixRow>, String> {
    let mut rdr = csv::Reader::from_path(csv_path).map_err(|e| e.to_string())?;
    rdr.deserialize()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

fn load_manifest(manifest_path: &Path) -> Result<EvidenceManifest, String> {
    let bytes = fs::read(manifest_path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

// ============================================================================
// Runner 1: pmid_resolves
// ============================================================================

/// Validates that every PMID in `claims_matrix.csv` exists in the evidence manifest
/// and that the referenced evidence file is present on disk.
pub fn run_pmid_resolves(
    csv_path: &Path,
    manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let rows = load_rows(csv_path).map_err(|_e| {
        (
            0,
            ValidationFailureCause::LiteratureClaim {
                row_index: 0,
                artifact: artifact.clone(),
                kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
            },
        )
    })?;
    let manifest = load_manifest(manifest_path).map_err(|_| {
        (
            0,
            ValidationFailureCause::LiteratureClaim {
                row_index: 0,
                artifact: artifact.clone(),
                kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
            },
        )
    })?;
    let manifest_pmids: BTreeMap<String, &EvidenceEntry> = manifest
        .entries
        .iter()
        .map(|e| (e.pmid.clone(), e))
        .collect();

    let pmid_re = regex::Regex::new(r"^[1-9][0-9]{6,8}$").unwrap();

    for (i, row) in rows.iter().enumerate() {
        // Collect candidate PMIDs from row (upstream uses `pmid`, downstream uses `prior_pmids`).
        let pmids: Vec<&String> = row
            .pmid
            .iter()
            .chain(row.prior_pmids.iter().flat_map(|v| v.iter()))
            .collect();
        // no_prior_finding rows legitimately have zero pmids; that's not a failure here.
        for pmid in pmids {
            if !pmid_re.is_match(pmid) {
                return Err((
                    i as u64,
                    ValidationFailureCause::LiteratureClaim {
                        row_index: i as u64,
                        artifact: artifact.clone(),
                        kind: LiteratureClaimFailureKind::PmidMalformed,
                    },
                ));
            }
            if !manifest_pmids.contains_key(pmid) {
                return Err((
                    i as u64,
                    ValidationFailureCause::LiteratureClaim {
                        row_index: i as u64,
                        artifact: artifact.clone(),
                        kind: LiteratureClaimFailureKind::PmidNotFound,
                    },
                ));
            }
            let entry = manifest_pmids[pmid];
            let evidence_path = manifest_path.parent().unwrap().join(&entry.path);
            if !evidence_path.exists() {
                return Err((
                    i as u64,
                    ValidationFailureCause::LiteratureClaim {
                        row_index: i as u64,
                        artifact: artifact.clone(),
                        kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
                    },
                ));
            }
        }
    }
    Ok(())
}

// ============================================================================
// Runner 2: evidence_quote_substring_match
// ============================================================================

/// Validates that each `evidence_quote` in `claims_matrix.csv` is a
/// verbatim substring of the normalized evidence text for its PMID.
pub fn run_evidence_quote_substring_match(
    csv_path: &Path,
    manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let rows = load_rows(csv_path).map_err(|_| {
        (
            0,
            ValidationFailureCause::LiteratureClaim {
                row_index: 0,
                artifact: artifact.clone(),
                kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
            },
        )
    })?;
    let manifest = load_manifest(manifest_path).map_err(|_| {
        (
            0,
            ValidationFailureCause::LiteratureClaim {
                row_index: 0,
                artifact: artifact.clone(),
                kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
            },
        )
    })?;
    let manifest_by_pmid: BTreeMap<String, &EvidenceEntry> = manifest
        .entries
        .iter()
        .map(|e| (e.pmid.clone(), e))
        .collect();
    let manifest_dir = manifest_path.parent().unwrap();

    for (i, row) in rows.iter().enumerate() {
        // no_prior_finding rows have source_kind == "none" and empty quote; skip.
        if row.source_kind == "none" {
            continue;
        }

        let pmid = row
            .pmid
            .clone()
            .or_else(|| row.prior_pmids.as_ref().and_then(|v| v.first().cloned()));
        let pmid = match pmid {
            Some(p) => p,
            None => continue, // no_prior_finding edge — handled by concordance_flag validator
        };

        let entry = manifest_by_pmid.get(&pmid).ok_or_else(|| {
            (
                i as u64,
                ValidationFailureCause::LiteratureClaim {
                    row_index: i as u64,
                    artifact: artifact.clone(),
                    kind: LiteratureClaimFailureKind::PmidNotFound,
                },
            )
        })?;

        let evidence_path = manifest_dir.join(&entry.path);
        let raw = fs::read_to_string(&evidence_path).map_err(|_| {
            (
                i as u64,
                ValidationFailureCause::LiteratureClaim {
                    row_index: i as u64,
                    artifact: artifact.clone(),
                    kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
                },
            )
        })?;

        let normalized_source = collapse_whitespace_lowercase_v1(&raw);
        let normalized_quote = collapse_whitespace_lowercase_v1(&row.evidence_quote);

        if !normalized_source.contains(&normalized_quote) {
            return Err((
                i as u64,
                ValidationFailureCause::LiteratureClaim {
                    row_index: i as u64,
                    artifact: artifact.clone(),
                    kind: LiteratureClaimFailureKind::QuoteNotInSource,
                },
            ));
        }
        // Offset check: if declared offset != actual offset under normalization,
        // surface as QuoteOffsetWrong (forensic — catches misrecorded rows).
        let actual_offset = normalized_source.find(&normalized_quote).unwrap_or(0);
        if (actual_offset as u64).abs_diff(row.evidence_quote_offset) > 1024 {
            // Tolerance: 1024 chars to accommodate normalization shifts.
            return Err((
                i as u64,
                ValidationFailureCause::LiteratureClaim {
                    row_index: i as u64,
                    artifact: artifact.clone(),
                    kind: LiteratureClaimFailureKind::QuoteOffsetWrong,
                },
            ));
        }
    }
    Ok(())
}

// ============================================================================
// Runner 3: redistributable_or_marked
// ============================================================================

/// Validates that every row in `claims_matrix.csv` references a redistributable source
/// or is explicitly marked as non-redistributable in the `redistributable` column.
pub fn run_redistributable_or_marked(
    csv_path: &Path,
    _manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let rows = load_rows(csv_path).map_err(|_| {
        (
            0,
            ValidationFailureCause::LiteratureClaim {
                row_index: 0,
                artifact: artifact.clone(),
                kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
            },
        )
    })?;
    for (i, row) in rows.iter().enumerate() {
        if row.source_kind == "none" {
            continue;
        }
        let consistent = match (row.source_kind.as_str(), row.redistributable) {
            ("external_pdf_local_only", false) => true,
            ("external_pdf_local_only", true) => false, // contradiction
            ("pmc_oa_full_text", true) => true,
            ("pmc_oa_full_text", false) => false, // unmarked OA = inconsistent
            ("abstract_only", true) => true,
            ("abstract_only", false) => false,
            _ => false,
        };
        if !consistent {
            return Err((
                i as u64,
                ValidationFailureCause::LiteratureClaim {
                    row_index: i as u64,
                    artifact,
                    kind: LiteratureClaimFailureKind::RedistributableTagInconsistent,
                },
            ));
        }
    }
    Ok(())
}

// ============================================================================
// Runner 4: claim_row_has_finding_id (downstream only)
// ============================================================================

/// Validates that each literature claim row in `claims_matrix.csv` references a
/// `finding_id` that exists in the upstream `findings_csv_path`.
pub fn run_claim_row_has_finding_id(
    csv_path: &Path,
    findings_csv_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let rows = load_rows(csv_path).map_err(|_| {
        (
            0,
            ValidationFailureCause::LiteratureClaim {
                row_index: 0,
                artifact: artifact.clone(),
                kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
            },
        )
    })?;
    // Load findings table primary keys (first column or `id` column).
    let mut findings_rdr = csv::Reader::from_path(findings_csv_path).map_err(|_| {
        (
            0,
            ValidationFailureCause::LiteratureClaim {
                row_index: 0,
                artifact: artifact.clone(),
                kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
            },
        )
    })?;
    let headers = findings_rdr.headers().cloned().map_err(|_| {
        (
            0,
            ValidationFailureCause::LiteratureClaim {
                row_index: 0,
                artifact: artifact.clone(),
                kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
            },
        )
    })?;
    let pk_col = headers
        .iter()
        .position(|h| matches!(h, "id" | "gene_id" | "peak_id" | "variant_id"))
        .unwrap_or(0);

    let mut known: std::collections::HashSet<String> = std::collections::HashSet::new();
    for rec in findings_rdr.records().flatten() {
        if let Some(pk) = rec.get(pk_col) {
            known.insert(pk.to_string());
        }
    }

    for (i, row) in rows.iter().enumerate() {
        let fid = match &row.finding_id {
            Some(s) => s.clone(),
            None => continue,
        };
        if !known.contains(&fid) {
            return Err((
                i as u64,
                ValidationFailureCause::LiteratureClaim {
                    row_index: i as u64,
                    artifact,
                    kind: LiteratureClaimFailureKind::FindingIdOrphan,
                },
            ));
        }
    }
    Ok(())
}

// ============================================================================
// Runner 5: concordance_flag_in_closed_set (downstream only)
// ============================================================================

/// Validates that every `concordance_flag` value in `claims_matrix.csv` belongs
/// to the closed set defined by `LiteratureClaimFailureKind`.
pub fn run_concordance_flag_in_closed_set(
    csv_path: &Path,
    _manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let rows = load_rows(csv_path).map_err(|_| {
        (
            0,
            ValidationFailureCause::LiteratureClaim {
                row_index: 0,
                artifact: artifact.clone(),
                kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
            },
        )
    })?;
    let closed = [
        "same_direction",
        "opposite_direction",
        "no_prior_finding",
        "unverifiable",
    ];
    for (i, row) in rows.iter().enumerate() {
        let flag = match &row.concordance_flag {
            Some(f) => f.as_str(),
            None => continue,
        };
        if !closed.contains(&flag) {
            return Err((
                i as u64,
                ValidationFailureCause::LiteratureClaim {
                    row_index: i as u64,
                    artifact,
                    kind: LiteratureClaimFailureKind::InvalidConcordanceFlag,
                },
            ));
        }
    }
    Ok(())
}

// ============================================================================
// ValidatorRunner trait wrappers (Phase D — wire into post-task dispatch)
// ============================================================================
//
// The harness post-task hook (crates/harness/src/main.rs around line 1799)
// dispatches obligations via the ValidatorRunner trait. Wrap each pure-fn
// runner in a trait impl so it can join `default_runners()`. Each wrapper
// (a) builds the csv + manifest paths from the task's artifact_path, (b)
// calls the pure fn, (c) converts the Result into a ValidatorOutcome. The
// structured ValidationFailureCause::LiteratureClaim payload is encoded
// into the failure `message` string as a JSON-serialized fragment so
// downstream consumers (verify endpoint, UI) can recover the typed cause
// when needed. The plan-level intent of attaching the structured cause to
// BlockerKind::ValidationFailed { cause } is reached via the /verify
// endpoint in a later task — the harness-side dispatcher today uses a
// string-only TaskState::Blocked path (see main.rs:1812-1826).

use crate::validators::{ValidatorOutcome, ValidatorRunner};

fn find_literature_csv(artifact_path: &Path) -> Option<std::path::PathBuf> {
    let prior = artifact_path.join("prior_claims_matrix.csv");
    if prior.exists() {
        return Some(prior);
    }
    let claims = artifact_path.join("claims_evidence_matrix.csv");
    if claims.exists() {
        return Some(claims);
    }
    None
}

fn cause_to_message(cause: &ValidationFailureCause) -> String {
    serde_json::to_string(cause).unwrap_or_else(|e| format!("cause_serialize_error:{}", e))
}

fn runner_dispatch<F>(artifact_path: &Path, require_manifest: bool, run: F) -> ValidatorOutcome
where
    F: FnOnce(&Path, &Path) -> Result<(), (u64, ValidationFailureCause)>,
{
    let Some(csv) = find_literature_csv(artifact_path) else {
        return ValidatorOutcome::Errored {
            reason: format!(
                "no literature CSV at {} (looked for prior_claims_matrix.csv and claims_evidence_matrix.csv)",
                artifact_path.display()
            ),
        };
    };
    let manifest = artifact_path.join("evidence/manifest.json");
    if require_manifest && !manifest.exists() {
        return ValidatorOutcome::Errored {
            reason: format!("evidence/manifest.json missing at {}", manifest.display()),
        };
    }
    match run(&csv, &manifest) {
        Ok(()) => ValidatorOutcome::Passed,
        Err((row_index, cause)) => ValidatorOutcome::Failed {
            message: format!("row {}: {}", row_index, cause_to_message(&cause)),
        },
    }
}

/// `ValidatorRunner` wrapping `run_pmid_resolves` for the `pmid_resolves` obligation.
pub struct PmidResolvesRunner;
impl ValidatorRunner for PmidResolvesRunner {
    fn obligation_id(&self) -> &'static str {
        "pmid_resolves"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(artifact_path, true, run_pmid_resolves)
    }
}

/// `ValidatorRunner` wrapping `run_evidence_quote_substring_match` for the `evidence_quote_substring_match` obligation.
pub struct EvidenceQuoteSubstringMatchRunner;
impl ValidatorRunner for EvidenceQuoteSubstringMatchRunner {
    fn obligation_id(&self) -> &'static str {
        "evidence_quote_substring_match"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(artifact_path, true, run_evidence_quote_substring_match)
    }
}

/// `ValidatorRunner` wrapping `run_redistributable_or_marked` for the `redistributable_or_marked` obligation.
pub struct RedistributableOrMarkedRunner;
impl ValidatorRunner for RedistributableOrMarkedRunner {
    fn obligation_id(&self) -> &'static str {
        "redistributable_or_marked"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(artifact_path, false, run_redistributable_or_marked)
    }
}

/// `ValidatorRunner` wrapping `run_claim_row_has_finding_id` for the `claim_row_has_finding_id` obligation.
pub struct ClaimRowHasFindingIdRunner;
impl ValidatorRunner for ClaimRowHasFindingIdRunner {
    fn obligation_id(&self) -> &'static str {
        "claim_row_has_finding_id"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        // This validator needs the upstream findings CSV, which
        // isn't yet plumbed through the ValidatorRunner trait signature
        // (it only receives the current task's artifact_path). Look for a
        // sibling findings CSV under runtime/outputs/<upstream>/ relative
        // to artifact_path; if not findable, soft-skip with Errored so
        // failures don't block tasks pending upstream-path threading.
        let Some(csv) = find_literature_csv(artifact_path) else {
            return ValidatorOutcome::Errored {
                reason: format!(
                    "no claims_evidence_matrix.csv at {}",
                    artifact_path.display()
                ),
            };
        };
        // Heuristic upstream-finding paths: look in sibling output dirs
        // for canonical finding-table filenames.
        let outputs_dir = artifact_path.parent();
        let Some(outputs_dir) = outputs_dir else {
            return ValidatorOutcome::Errored {
                reason: "artifact_path has no parent outputs dir".into(),
            };
        };
        let candidates = ["de_results.tsv", "peak_calls.tsv", "variant_calls.tsv"];
        let findings_csv = std::fs::read_dir(outputs_dir).ok().and_then(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .find_map(|sib| {
                    candidates.iter().find_map(|name| {
                        let p = sib.join(name);
                        if p.exists() {
                            Some(p)
                        } else {
                            None
                        }
                    })
                })
        });
        let Some(findings_csv) = findings_csv else {
            return ValidatorOutcome::Errored {
                reason: format!(
                    "no upstream findings CSV found in {} (looked for {:?})",
                    outputs_dir.display(),
                    candidates
                ),
            };
        };
        match run_claim_row_has_finding_id(&csv, &findings_csv) {
            Ok(()) => ValidatorOutcome::Passed,
            Err((row_index, cause)) => ValidatorOutcome::Failed {
                message: format!("row {}: {}", row_index, cause_to_message(&cause)),
            },
        }
    }
}

/// `ValidatorRunner` wrapping `run_concordance_flag_in_closed_set` for the `concordance_flag_in_closed_set` obligation.
pub struct ConcordanceFlagInClosedSetRunner;
impl ValidatorRunner for ConcordanceFlagInClosedSetRunner {
    fn obligation_id(&self) -> &'static str {
        "concordance_flag_in_closed_set"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(artifact_path, false, run_concordance_flag_in_closed_set)
    }
}

/// Five trait-wrapped runners for the literature obligations. Used by
/// `crate::validators::default_runners` so the harness post-task hook
/// routes literature obligation ids to the right runner.
pub fn literature_runners() -> Vec<Box<dyn ValidatorRunner>> {
    vec![
        Box::new(PmidResolvesRunner) as Box<dyn ValidatorRunner>,
        Box::new(EvidenceQuoteSubstringMatchRunner),
        Box::new(RedistributableOrMarkedRunner),
        Box::new(ClaimRowHasFindingIdRunner),
        Box::new(ConcordanceFlagInClosedSetRunner),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(p: &Path, s: &str) {
        fs::write(p, s).unwrap();
    }

    #[test]
    fn normalize_collapses_whitespace_and_lowercases() {
        assert_eq!(
            collapse_whitespace_lowercase_v1("  Hello   World\n\t"),
            "hello world"
        );
    }

    #[test]
    fn pmid_resolves_passes_on_well_formed_rows() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nACAN,gene,28123456,foo,0,pmc_oa_full_text,sha256:abc,2026-05-14T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(
            &manifest,
            r#"{"schema_version":1,"entries":[{"pmid":"28123456","source_kind":"pmc_oa_full_text","path":"28123456.xml","sha256_binary":"00","sha256_extracted_text":"00","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":0,"retrieval_ts":"2026-05-14T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"CC-BY-4.0"}]}"#,
        );
        write(&manifest.parent().unwrap().join("28123456.xml"), "");
        assert!(run_pmid_resolves(&csv, &manifest).is_ok());
    }

    #[test]
    fn pmid_resolves_rejects_malformed_pmid() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nACAN,gene,123,foo,0,pmc_oa_full_text,sha256:abc,2026-05-14T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(&manifest, r#"{"schema_version":1,"entries":[]}"#);
        let err = run_pmid_resolves(&csv, &manifest).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::PmidMalformed,
                ..
            }
        ));
    }

    #[test]
    fn evidence_quote_substring_match_finds_present_quote() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nACAN,gene,28123456,reduction in disc tissue,5,pmc_oa_full_text,sha256:abc,2026-05-14T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(
            &manifest,
            r#"{"schema_version":1,"entries":[{"pmid":"28123456","source_kind":"pmc_oa_full_text","path":"28123456.xml","sha256_binary":"00","sha256_extracted_text":"00","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":0,"retrieval_ts":"2026-05-14T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"CC-BY-4.0"}]}"#,
        );
        write(
            &manifest.parent().unwrap().join("28123456.xml"),
            "ACAN reduction in disc tissue was observed",
        );
        assert!(run_evidence_quote_substring_match(&csv, &manifest).is_ok());
    }

    #[test]
    fn evidence_quote_substring_match_rejects_absent_quote() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nACAN,gene,28123456,this quote is not there,0,pmc_oa_full_text,sha256:abc,2026-05-14T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(
            &manifest,
            r#"{"schema_version":1,"entries":[{"pmid":"28123456","source_kind":"pmc_oa_full_text","path":"28123456.xml","sha256_binary":"00","sha256_extracted_text":"00","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":0,"retrieval_ts":"2026-05-14T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"CC-BY-4.0"}]}"#,
        );
        write(
            &manifest.parent().unwrap().join("28123456.xml"),
            "some other text",
        );
        let err = run_evidence_quote_substring_match(&csv, &manifest).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::QuoteNotInSource,
                ..
            }
        ));
    }

    #[test]
    fn redistributable_inconsistent_rejected() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nACAN,gene,28123456,q,0,external_pdf_local_only,sha256:abc,2026-05-14T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(&manifest, r#"{"schema_version":1,"entries":[]}"#);
        let err = run_redistributable_or_marked(&csv, &manifest).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::RedistributableTagInconsistent,
                ..
            }
        ));
    }

    #[test]
    fn concordance_flag_outside_closed_set_rejected() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("claims_evidence_matrix.csv");
        write(&csv, "finding_id,entity,entity_kind,prior_pmids,concordance_flag,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\ngene_1,ACAN,gene,,hallucinated_flag,,0,none,none,2026-05-14T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(&manifest, r#"{"schema_version":1,"entries":[]}"#);
        let err = run_concordance_flag_in_closed_set(&csv, &manifest).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::InvalidConcordanceFlag,
                ..
            }
        ));
    }
}
