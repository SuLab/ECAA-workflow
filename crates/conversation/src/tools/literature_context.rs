//! `get_literature_context` chat tool — part of the literature-atom
//! plan. Read-only; reads PMID-anchored rows from the session's emitted
//! package. No live PubMed call, no LLM dispatch, sub-millisecond. Spec §10.1.

use crate::errors::{ToolError, ToolResult};
use crate::session::Session;
use serde::{Deserialize, Serialize};
use std::path::Path;
use ts_rs::TS;

/// Returns false when `SWFC_LITERATURE_CONTEXT_DISABLED` is set to `1` or
/// `true` (case-insensitive). External eval runs (BiomniBench Arms B / D)
/// set this to honor Phylo's "do not search for the source paper" instruction.
pub fn literature_context_enabled() -> bool {
    match std::env::var("SWFC_LITERATURE_CONTEXT_DISABLED") {
        Ok(v) => {
            let v = v.trim().to_lowercase();
            !(v == "1" || v == "true" || v == "yes")
        }
        Err(_) => true,
    }
}

/// Biological entity type for a literature query or CSV row.
#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    /// Protein-coding or non-coding gene symbol.
    Gene,
    /// Genomic region (chromosome arm, locus, or coordinate range).
    Region,
    /// Sequence variant (SNP, indel, structural variant).
    Variant,
    /// Biological pathway name.
    Pathway,
    /// Cell type or cell-state label.
    CellType,
}

/// Where the literature evidence was sourced from.
#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Full-text article retrieved from PMC Open Access.
    PmcOaFullText,
    /// Abstract text only (full text not accessible).
    AbstractOnly,
    /// PDF sourced from an external location and stored locally only.
    ExternalPdfLocalOnly,
    /// No evidence source (placeholder row).
    None,
}

/// Direction agreement between a prior published claim and a new finding.
#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ConcordanceFlag {
    /// New finding agrees with the prior publication.
    SameDirection,
    /// New finding disagrees with the prior publication.
    OppositeDirection,
    /// No prior published finding exists for this entity.
    NoPriorFinding,
    /// Agreement cannot be determined from the available evidence.
    Unverifiable,
}

/// Literature retrieval scope used when this session was emitted.
#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum LiteratureScope {
    /// PMC Open Access full-text corpus only.
    PmcOa,
    /// PMC OA full-text plus title+abstract records.
    PmcOaPlusAbstracts,
    /// Any locally available source (no live network calls).
    AllSourcesLocalOnly,
}

/// One row from `prior_claims_matrix.csv`: a prior published claim
/// about a specific entity.
#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct PriorClaimRow {
    /// Entity name (e.g. gene symbol, pathway name).
    pub entity: String,
    /// Biological entity type.
    pub entity_kind: EntityKind,
    /// PubMed ID of the source article.
    pub pmid: String,
    /// Verbatim quote from the source article supporting the claim.
    pub evidence_quote: String,
    /// Where the evidence text was retrieved from.
    pub source_kind: SourceKind,
    /// SHA-256 hash of the source document.
    pub source_hash: String,
    /// Whether the source document may be included in the emitted package
    /// (license allows redistribution).
    pub redistributable: bool,
}

/// One row from `claims_evidence_matrix.csv`: a contextualized
/// finding cross-referenced against prior literature.
#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ClaimsEvidenceRow {
    /// Unique finding identifier within this analysis run.
    pub finding_id: String,
    /// Entity name (gene, pathway, etc.).
    pub entity: String,
    /// Biological entity type.
    pub entity_kind: EntityKind,
    /// PMIDs of prior publications that reported findings on this entity.
    pub prior_pmids: Vec<String>,
    /// How the new finding relates to the prior literature.
    pub concordance_flag: ConcordanceFlag,
    /// Verbatim quote from the evidence source.
    pub evidence_quote: String,
    /// Where the evidence text was retrieved from.
    pub source_kind: SourceKind,
}

/// One entry from `evidence/manifest.json`: describes a source
/// document included in the literature evidence bundle.
#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct EvidenceManifestEntry {
    /// PubMed ID of the source article.
    pub pmid: String,
    /// Where the evidence text was retrieved from.
    pub source_kind: SourceKind,
    /// Relative path to the evidence file within the bundle.
    pub path: String,
    /// Whether the source may be redistributed with the package.
    pub redistributable: bool,
}

/// Full literature context for a single entity, returned by the
/// `get_literature_context` tool.
#[derive(Debug, Clone, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct LiteratureContext {
    /// Entity name that was queried (lowercased for matching; original
    /// case preserved from the query).
    pub entity: String,
    /// Entity type inferred or provided by the caller.
    pub entity_kind: EntityKind,
    /// Prior published claims for this entity.
    pub prior_rows: Vec<PriorClaimRow>,
    /// New-finding cross-references for this entity.
    pub finding_rows: Vec<ClaimsEvidenceRow>,
    /// Evidence source documents associated with the results.
    pub source_artifacts: Vec<EvidenceManifestEntry>,
    /// Retrieval scope in effect when this package was emitted.
    pub source_scope: LiteratureScope,
}

/// Error from `read_literature_context`.
#[derive(Debug, thiserror::Error)]
pub enum LiteratureContextError {
    /// The session has no emitted package path.
    #[error("session has no emitted package")]
    NoEmittedPackage,
    /// Neither `prior_claims_matrix.csv` nor `claims_evidence_matrix.csv`
    /// was found — the package was emitted without literature atoms.
    #[error("no literature atoms ran in this session")]
    NoLiteratureAtoms,
    /// A CSV or manifest file had an unexpected schema.
    #[error("csv schema mismatch: {0}")]
    CsvSchemaMismatch(String),
}

/// Tool dispatch entry point: reads `entity` (and optional `entity_kind`)
/// from the session's most recently emitted package. Returns a
/// `LiteratureContext` serialized as `ToolResult::ok`, or a
/// `ToolError::PreconditionFailure` / `ToolError::InternalError` on
/// failure.
pub(super) fn get_literature_context(
    session: &Session,
    entity: &str,
    entity_kind: Option<EntityKind>,
) -> ToolResult {
    if !literature_context_enabled() {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "literature_context is disabled via SWFC_LITERATURE_CONTEXT_DISABLED".into(),
            hint: "External eval run requested no literature lookups; unset \
                   SWFC_LITERATURE_CONTEXT_DISABLED to re-enable."
                .into(),
        });
    }
    let Some(ref package_root) = session.emitted_package_path else {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "session has no emitted package — literature context requires a completed emit"
                .into(),
            hint: "Emit the package first, then ask about literature context.".into(),
        });
    };

    match read_literature_context(package_root, entity, entity_kind) {
        Ok(ctx) => match serde_json::to_value(&ctx) {
            Ok(v) => ToolResult::ok(v),
            Err(e) => ToolResult::err(ToolError::InternalError {
                reason: format!("failed to serialize LiteratureContext: {}", e),
            }),
        },
        Err(LiteratureContextError::NoEmittedPackage) => {
            ToolResult::err(ToolError::PreconditionFailure {
                reason: "session has no emitted package".into(),
                hint: "Emit the package first.".into(),
            })
        }
        Err(LiteratureContextError::NoLiteratureAtoms) => {
            ToolResult::err(ToolError::PreconditionFailure {
                reason: "no literature atoms ran in this session — prior_claims_matrix.csv and \
                         claims_evidence_matrix.csv are both absent"
                    .into(),
                hint: "This package was built without the review_prior_work or \
                       contextualize_findings_with_literature atoms. Literature context is only \
                       available after those tasks complete."
                    .into(),
            })
        }
        Err(LiteratureContextError::CsvSchemaMismatch(msg)) => {
            ToolResult::err(ToolError::InternalError {
                reason: format!("literature CSV parse error: {}", msg),
            })
        }
    }
}

/// Read PriorClaimRow + ClaimsEvidenceRow rows matching `entity` from the
/// session's most recently emitted package. `entity_kind` filters when
/// provided.
///
/// Empty matches are a valid Ok result — distinct from "no literature
/// atoms ran" which returns `LiteratureContextError::NoLiteratureAtoms`.
pub fn read_literature_context(
    package_root: &Path,
    entity: &str,
    entity_kind: Option<EntityKind>,
) -> Result<LiteratureContext, LiteratureContextError> {
    let upstream_csv =
        package_root.join("runtime/outputs/review_prior_work/prior_claims_matrix.csv");
    let upstream_manifest =
        package_root.join("runtime/outputs/review_prior_work/evidence/manifest.json");
    let downstream_csv = package_root
        .join("runtime/outputs/contextualize_findings_with_literature/claims_evidence_matrix.csv");
    let downstream_manifest = package_root
        .join("runtime/outputs/contextualize_findings_with_literature/evidence/manifest.json");

    if !upstream_csv.exists() && !downstream_csv.exists() {
        return Err(LiteratureContextError::NoLiteratureAtoms);
    }

    let entity_lc = entity.to_lowercase();
    let prior_rows = read_prior_rows(&upstream_csv, &entity_lc, entity_kind.as_ref())?;
    let finding_rows = read_claims_rows(&downstream_csv, &entity_lc, entity_kind.as_ref())?;
    let source_artifacts = read_manifest_entries(&[&upstream_manifest, &downstream_manifest])?;

    Ok(LiteratureContext {
        entity: entity.to_string(),
        entity_kind: entity_kind.unwrap_or(EntityKind::Gene),
        prior_rows,
        finding_rows,
        source_artifacts,
        // Session-level scope plumbing is a follow-up.
        source_scope: LiteratureScope::PmcOa,
    })
}

/// Word-boundary match: returns true iff `needle` appears in `haystack`
/// with non-alphanumeric (or start/end of string) characters on both
/// sides. Closes the "T matches Treg cell" surface for free-text
/// CellType / Pathway names — those types still want a substring-style
/// match (a row "exhausted CD8 T cell" should match query "exhausted")
/// but ONLY at token boundaries.
fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let mut start = 0usize;
    while let Some(rel) = haystack[start..].find(needle) {
        let abs = start + rel;
        let before_ok = abs == 0
            || !bytes
                .get(abs - 1)
                .map(|b| b.is_ascii_alphanumeric())
                .unwrap_or(false);
        let after_idx = abs + needle.len();
        let after_ok = after_idx == bytes.len()
            || !bytes
                .get(after_idx)
                .map(|b| b.is_ascii_alphanumeric())
                .unwrap_or(false);
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

fn matches_entity(
    row_entity: &str,
    query_lc: &str,
    row_kind: &EntityKind,
    query_kind: Option<&EntityKind>,
) -> bool {
    if let Some(qk) = query_kind {
        // Compare via serde-style snake_case strings so the matching is
        // robust to the borrow shape.
        let row_kind_s = serde_json::to_value(row_kind)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        let qk_s = serde_json::to_value(qk)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        if row_kind_s != qk_s {
            return false;
        }
    }
    // Allocate the lowercased row once per call, not twice as in the
    // previous shape; lowercasing every row across a large CSV with
    // many entity kinds was the quadratic-memory surface
    // `matches_entity` was charged with.
    let row_lc = row_entity.to_ascii_lowercase();
    match row_kind {
        EntityKind::Gene | EntityKind::Region | EntityKind::Variant => row_lc == query_lc,
        EntityKind::Pathway | EntityKind::CellType => contains_word(&row_lc, query_lc),
    }
}

fn read_prior_rows(
    csv_path: &Path,
    entity_lc: &str,
    query_kind: Option<&EntityKind>,
) -> Result<Vec<PriorClaimRow>, LiteratureContextError> {
    if !csv_path.exists() {
        return Ok(Vec::new());
    }
    let mut rdr = csv::Reader::from_path(csv_path)
        .map_err(|e| LiteratureContextError::CsvSchemaMismatch(e.to_string()))?;
    let mut out = Vec::new();
    for rec in rdr.deserialize::<RawPriorRow>() {
        let row = rec.map_err(|e| LiteratureContextError::CsvSchemaMismatch(e.to_string()))?;
        let entity_kind = parse_entity_kind(&row.entity_kind)?;
        if !matches_entity(&row.entity, entity_lc, &entity_kind, query_kind) {
            continue;
        }
        out.push(PriorClaimRow {
            entity: row.entity,
            entity_kind,
            pmid: row.pmid,
            evidence_quote: row.evidence_quote,
            source_kind: parse_source_kind(&row.source_kind)?,
            source_hash: row.source_hash,
            redistributable: row.redistributable,
        });
    }
    Ok(out)
}

fn read_claims_rows(
    csv_path: &Path,
    entity_lc: &str,
    query_kind: Option<&EntityKind>,
) -> Result<Vec<ClaimsEvidenceRow>, LiteratureContextError> {
    if !csv_path.exists() {
        return Ok(Vec::new());
    }
    let mut rdr = csv::Reader::from_path(csv_path)
        .map_err(|e| LiteratureContextError::CsvSchemaMismatch(e.to_string()))?;
    let mut out = Vec::new();
    for rec in rdr.deserialize::<RawClaimsRow>() {
        let row = rec.map_err(|e| LiteratureContextError::CsvSchemaMismatch(e.to_string()))?;
        let entity_kind = parse_entity_kind(&row.entity_kind)?;
        if !matches_entity(&row.entity, entity_lc, &entity_kind, query_kind) {
            continue;
        }
        let prior_pmids: Vec<String> = row
            .prior_pmids
            .split([',', ';', '|'])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        out.push(ClaimsEvidenceRow {
            finding_id: row.finding_id,
            entity: row.entity,
            entity_kind,
            prior_pmids,
            concordance_flag: parse_concordance_flag(&row.concordance_flag)?,
            evidence_quote: row.evidence_quote,
            source_kind: parse_source_kind(&row.source_kind)?,
        });
    }
    Ok(out)
}

fn read_manifest_entries(
    manifest_paths: &[&Path],
) -> Result<Vec<EvidenceManifestEntry>, LiteratureContextError> {
    let mut out = Vec::new();
    for p in manifest_paths {
        if !p.exists() {
            continue;
        }
        let body = std::fs::read_to_string(p)
            .map_err(|e| LiteratureContextError::CsvSchemaMismatch(e.to_string()))?;
        let parsed: RawManifest = serde_json::from_str(&body)
            .map_err(|e| LiteratureContextError::CsvSchemaMismatch(e.to_string()))?;
        for entry in parsed.entries {
            out.push(EvidenceManifestEntry {
                pmid: entry.pmid,
                source_kind: parse_source_kind(&entry.source_kind)?,
                path: entry.path,
                redistributable: entry.redistributable,
            });
        }
    }
    Ok(out)
}

fn parse_entity_kind(s: &str) -> Result<EntityKind, LiteratureContextError> {
    match s {
        "gene" => Ok(EntityKind::Gene),
        "region" => Ok(EntityKind::Region),
        "variant" => Ok(EntityKind::Variant),
        "pathway" => Ok(EntityKind::Pathway),
        "cell_type" => Ok(EntityKind::CellType),
        _ => Err(LiteratureContextError::CsvSchemaMismatch(format!(
            "bad entity_kind: {}",
            s
        ))),
    }
}

fn parse_source_kind(s: &str) -> Result<SourceKind, LiteratureContextError> {
    match s {
        "pmc_oa_full_text" => Ok(SourceKind::PmcOaFullText),
        "abstract_only" => Ok(SourceKind::AbstractOnly),
        "external_pdf_local_only" => Ok(SourceKind::ExternalPdfLocalOnly),
        "none" => Ok(SourceKind::None),
        _ => Err(LiteratureContextError::CsvSchemaMismatch(format!(
            "bad source_kind: {}",
            s
        ))),
    }
}

fn parse_concordance_flag(s: &str) -> Result<ConcordanceFlag, LiteratureContextError> {
    match s {
        "same_direction" => Ok(ConcordanceFlag::SameDirection),
        "opposite_direction" => Ok(ConcordanceFlag::OppositeDirection),
        "no_prior_finding" => Ok(ConcordanceFlag::NoPriorFinding),
        "unverifiable" => Ok(ConcordanceFlag::Unverifiable),
        _ => Err(LiteratureContextError::CsvSchemaMismatch(format!(
            "bad concordance_flag: {}",
            s
        ))),
    }
}

// The `#[allow(dead_code)]` fields below are reserved-for-serde: the
// on-wire CSV/manifest schema carries them, so the structs must declare
// them to deserialize cleanly even though this module only reads a
// subset. Removing them would force the deserializer to error on inputs
// that include the extra columns.

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RawPriorRow {
    entity: String,
    entity_kind: String,
    pmid: String,
    evidence_quote: String,
    #[serde(default)]
    #[allow(dead_code)]
    evidence_quote_offset: u64,
    source_kind: String,
    source_hash: String,
    #[serde(default)]
    #[allow(dead_code)]
    retrieval_ts: String,
    redistributable: bool,
    #[serde(default)]
    #[allow(dead_code)]
    verified: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RawClaimsRow {
    finding_id: String,
    entity: String,
    entity_kind: String,
    #[serde(default)]
    prior_pmids: String,
    concordance_flag: String,
    #[serde(default)]
    evidence_quote: String,
    #[serde(default)]
    #[allow(dead_code)]
    evidence_quote_offset: u64,
    source_kind: String,
    #[serde(default)]
    #[allow(dead_code)]
    source_hash: String,
    #[serde(default)]
    #[allow(dead_code)]
    retrieval_ts: String,
    #[allow(dead_code)]
    redistributable: bool,
    #[serde(default)]
    #[allow(dead_code)]
    verified: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RawManifest {
    #[serde(default)]
    #[allow(dead_code)]
    schema_version: u32,
    entries: Vec<RawManifestEntry>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RawManifestEntry {
    pmid: String,
    source_kind: String,
    path: String,
    #[serde(default)]
    #[allow(dead_code)]
    sha256_binary: String,
    #[serde(default)]
    #[allow(dead_code)]
    sha256_extracted_text: String,
    #[serde(default)]
    #[allow(dead_code)]
    extracted_text_normalization: String,
    #[serde(default)]
    #[allow(dead_code)]
    bytes: u64,
    #[serde(default)]
    #[allow(dead_code)]
    retrieval_ts: String,
    #[serde(default)]
    #[allow(dead_code)]
    retrieval_query_id: String,
    redistributable: bool,
    #[serde(default)]
    #[allow(dead_code)]
    license: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    fn write_minimal_package(dir: &Path, entity: &str, kind: &str, pmid: &str) {
        let task_dir = dir.join("runtime/outputs/review_prior_work");
        let evidence_dir = task_dir.join("evidence");
        std::fs::create_dir_all(&evidence_dir).unwrap();

        let csv_content = format!(
            "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n{},{},{},quote,0,pmc_oa_full_text,sha256:abc,2026-05-14T00:00:00Z,true,true\n",
            entity, kind, pmid
        );
        std::fs::write(task_dir.join("prior_claims_matrix.csv"), csv_content).unwrap();

        let manifest_content = serde_json::json!({
            "schema_version": 1,
            "entries": [{
                "pmid": pmid,
                "source_kind": "pmc_oa_full_text",
                "path": format!("{}.xml", pmid),
                "sha256_binary": "00",
                "sha256_extracted_text": "00",
                "extracted_text_normalization": "collapse_whitespace_lowercase_v1",
                "bytes": 0,
                "retrieval_ts": "2026-05-14T00:00:00Z",
                "retrieval_query_id": "q001",
                "redistributable": true,
                "license": "CC-BY-4.0"
            }]
        });
        std::fs::write(
            evidence_dir.join("manifest.json"),
            manifest_content.to_string(),
        )
        .unwrap();
    }

    #[test]
    fn returns_no_literature_atoms_when_csvs_absent() {
        let dir = TempDir::new().unwrap();
        let err = read_literature_context(dir.path(), "ACAN", Some(EntityKind::Gene)).unwrap_err();
        assert!(matches!(err, LiteratureContextError::NoLiteratureAtoms));
    }

    #[test]
    fn returns_matching_prior_rows() {
        let dir = TempDir::new().unwrap();
        write_minimal_package(dir.path(), "ACAN", "gene", "28123456");
        let ctx = read_literature_context(dir.path(), "ACAN", Some(EntityKind::Gene)).unwrap();
        assert_eq!(ctx.prior_rows.len(), 1);
        assert_eq!(ctx.prior_rows[0].pmid, "28123456");
        assert_eq!(ctx.source_artifacts.len(), 1);
    }

    #[test]
    fn returns_empty_rows_when_entity_not_present() {
        let dir = TempDir::new().unwrap();
        write_minimal_package(dir.path(), "ACAN", "gene", "28123456");
        let ctx =
            read_literature_context(dir.path(), "UNKNOWN_GENE", Some(EntityKind::Gene)).unwrap();
        assert!(ctx.prior_rows.is_empty());
        // Still Ok — not an error to have no matching rows.
    }
}
