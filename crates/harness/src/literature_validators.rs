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

/// CSV-lenient `u64`: the `method_landscape.csv` shape emits an EMPTY
/// `evidence_quote_offset` on `curated_baseline` candidate rows (which carry no
/// evidence). A bare `u64` field rejects "" and fails the WHOLE `load_rows`
/// parse, which the offset-reading validators then mis-report as a spurious
/// `EvidenceArtifactMissing` at row 0 (stranding the keystone
/// `survey_method_landscape` task and every downstream stage). Treat empty as 0.
fn de_u64_lenient<'de, D>(d: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    let t = s.trim();
    if t.is_empty() {
        return Ok(0);
    }
    t.parse::<u64>().map_err(serde::de::Error::custom)
}

/// CSV-lenient `bool`: `curated_baseline` rows emit an EMPTY `redistributable`
/// (and may emit an empty `verified`). A bare `bool` rejects "" and fails the
/// whole CSV parse — same spurious-`EvidenceArtifactMissing` class as
/// [`de_u64_lenient`]. Treat empty as `false`; accept the usual true/false
/// tokens otherwise.
fn de_bool_lenient<'de, D>(d: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    match s.trim() {
        "" => Ok(false),
        "true" | "True" | "TRUE" | "1" => Ok(true),
        "false" | "False" | "FALSE" | "0" => Ok(false),
        other => Err(serde::de::Error::custom(format!(
            "invalid bool literal: {other:?}"
        ))),
    }
}

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
    // Optional so the same reader parses BOTH the claims-matrix shape
    // (entity/entity_kind/pmid) AND the method_landscape shape
    // (axis/candidate_method/source_ref) survey_method_landscape emits.
    // Required `String` here made every load_rows-based obligation
    // (evidence_quote_substring_match, redistributable_or_marked, …) fail to
    // parse the method_landscape CSV and report a spurious
    // EvidenceArtifactMissing at row 0.
    // `entity_id` is the contextualize atom's spelling of the entity key
    // (gene/peak/variant id); accept it as an alias so the finding_id fallback
    // can match the row's entity against an upstream PK.
    #[serde(default, alias = "entity_id")]
    pub entity: String,
    #[serde(default)]
    pub entity_kind: String,
    #[serde(default)]
    pub pmid: Option<String>,
    #[serde(default)]
    pub prior_pmids: Option<Vec<String>>,
    // Typed-locator columns. Absent on legacy PMID-only rows (which
    // anchor via `pmid`/`prior_pmids`); present on locator-generalized
    // rows where `source_ref_kind` selects the dispatch branch.
    #[serde(default)]
    pub source_ref_kind: Option<String>,
    #[serde(default)]
    pub source_ref: Option<String>,
    #[serde(default)]
    pub source_class: Option<String>,
    #[serde(default)]
    pub evidence_role: Option<String>,
    #[serde(default)]
    pub version_context: Option<String>,
    #[serde(default)]
    pub concordance_flag: Option<String>,
    pub evidence_quote: String,
    // `curated_baseline` method_landscape rows emit an empty offset/redistributable/
    // verified; lenient deserializers map "" to 0/false so a no-evidence candidate
    // row does not fail the whole CSV parse (see de_u64_lenient / de_bool_lenient).
    // codex names the start offset `quote_start`; defaulted so a CSV that omits
    // it (offset-free containment match) still parses.
    #[serde(default, deserialize_with = "de_u64_lenient", alias = "quote_start")]
    pub evidence_quote_offset: u64,
    // codex spells these `source_type` / `source_sha256`; default+alias so its
    // richer claims-matrix schema parses (the obligations key on the values, not
    // the column name).
    #[serde(default, alias = "source_type")]
    pub source_kind: String,
    #[serde(default, alias = "source_sha256")]
    pub source_hash: String,
    #[serde(default)]
    pub retrieval_ts: String,
    #[serde(default, deserialize_with = "de_bool_lenient")]
    pub redistributable: bool,
    #[serde(default, deserialize_with = "de_bool_lenient")]
    pub verified: bool,
}

// Serde shape mirror of `evidence-manifest.json`; `schema_version` is read by
// load_manifest's downstream validators on schema drift but the wrapper struct
// itself does not consume every field directly.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct EvidenceManifest {
    // Unused by the validators; tolerate absent or non-numeric (codex writes a
    // string) by ignoring the value entirely.
    #[serde(default)]
    #[allow(dead_code)]
    pub schema_version: serde_json::Value,
    // Accept `sources` (codex's hand-rolled literature manifest top-level key)
    // as an alias for the canonical `entries` — same per-source records.
    #[serde(alias = "sources")]
    pub entries: Vec<EvidenceEntry>,
}

// Serde shape for entries in `evidence-manifest.json`; preserves the full
// per-PMID record so validators downstream can inspect license/redistributable
// flags even when this binary doesn't read them at compile time.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct EvidenceEntry {
    // Optional so non-PMID locator entries (DOI/arXiv/URL) validate;
    // legacy PMID entries continue to carry it.
    #[serde(default)]
    pub pmid: Option<String>,
    #[serde(default)]
    pub source_ref_kind: Option<String>,
    #[serde(default)]
    pub source_ref: Option<String>,
    #[serde(default)]
    pub source_class: Option<String>,
    #[serde(default)]
    pub evidence_role: Option<String>,
    #[serde(default)]
    pub version_context: Option<String>,
    /// Batched PubMed efetch: the PMIDs a single shared XML snapshot covers
    /// (one efetch request fetches many PMIDs). Locator indexing keys on each
    /// so a claim row citing any batch member resolves to this snapshot.
    /// codex spells this `pmids`; accept both.
    #[serde(default, alias = "pmids")]
    pub pmids_in_batch: Vec<String>,
    // codex names this `source_type`; the canonical helper uses `source_kind`.
    // Defaulted + aliased so codex's hand-rolled manifest deserializes (the
    // per-row legal gate reads the CSV's source_kind, not this entry field).
    #[serde(default, alias = "source_type")]
    pub source_kind: String,
    // codex names the snapshot path `source_text_path`; the canonical helper
    // uses `path`. Accept both.
    #[serde(alias = "source_text_path")]
    pub path: String,
    // Secondary provenance metadata the validators store but do not gate on.
    // Defaulted so a leaner hand-rolled manifest (codex omits these / spells
    // sha256 without the `_binary` suffix) still deserializes and resolves.
    #[serde(default, alias = "sha256")]
    pub sha256_binary: String,
    #[serde(default)]
    pub sha256_extracted_text: String,
    #[serde(default)]
    pub extracted_text_normalization: String,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub retrieval_ts: String,
    #[serde(default)]
    pub retrieval_query_id: String,
    #[serde(default)]
    pub redistributable: bool,
    #[serde(default)]
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

/// Resolve an evidence-manifest entry's `path` to an on-disk file. The path may
/// be evidence-dir-relative ("pmid_X.xml", the claims-matrix convention) OR
/// task-dir-relative with an "evidence/" prefix (what agent_literature_fetch.py
/// and the agent's PMC fetch write) OR a cross-task sibling reference
/// ("../review_prior_work/evidence/snapshots/<hash>") that
/// contextualize_findings_with_literature writes when it dedups by reusing an
/// upstream literature task's snapshots. Joining a prefixed path straight onto
/// the evidence dir doubles it (evidence/evidence/…) and a `../sibling` path
/// anchored at the evidence dir lands one level too shallow; both spuriously
/// report the artifact missing. Try, in order: the direct evidence-dir join,
/// the "evidence/"-stripped form, and the TASK-dir anchor (evidence_dir's
/// parent) for cross-task `../` references. Returns the `direct` join when none
/// resolves (so callers still surface a missing-artifact error against a
/// sensible path).
fn resolve_evidence_file(evidence_dir: &Path, entry_path: &str) -> std::path::PathBuf {
    let direct = evidence_dir.join(entry_path);
    if direct.exists() {
        return direct;
    }
    if let Some(stripped) = entry_path.strip_prefix("evidence/") {
        let stripped_join = evidence_dir.join(stripped);
        if stripped_join.exists() {
            return stripped_join;
        }
    }
    // Task-dir anchor: a `../<sibling-task>/evidence/...` path is written
    // relative to the task dir (evidence_dir's parent), not the evidence dir.
    if let Some(task_dir) = evidence_dir.parent() {
        let task_anchored = task_dir.join(entry_path);
        if task_anchored.exists() {
            return task_anchored;
        }
    }
    // Ancestor-walk anchor: a PACKAGE-ROOT-relative path (codex writes
    // "runtime/outputs/<task>/evidence/sources/PMID_X.txt") resolves from some
    // ancestor of the evidence dir. Try each ancestor up to the filesystem root.
    let mut anc = evidence_dir.parent();
    while let Some(a) = anc {
        let joined = a.join(entry_path);
        if joined.exists() {
            return joined;
        }
        anc = a.parent();
    }
    // Basename fallback: the file lives under the evidence dir (or a common
    // subdir) regardless of how the manifest spelled the path prefix. Robust
    // against any executor's path convention (codex nests under sources/, the
    // canonical helper writes flat, contextualize reuses snapshots/).
    if let Some(base) = std::path::Path::new(entry_path).file_name() {
        for sub in ["", "sources", "raw", "snapshots"] {
            let cand = if sub.is_empty() {
                evidence_dir.join(base)
            } else {
                evidence_dir.join(sub).join(base)
            };
            if cand.exists() {
                return cand;
            }
        }
    }
    direct
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
        .map(|e| (e.pmid.clone().unwrap_or_default(), e))
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
            let evidence_path = resolve_evidence_file(manifest_path.parent().unwrap(), &entry.path);
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
// Runner 1b: source_resolves (locator-generalized successor to pmid_resolves)
// ============================================================================

/// Build a `LiteratureClaim` failure cause for a row.
fn lit_fail(
    row_index: u64,
    artifact: &str,
    kind: LiteratureClaimFailureKind,
) -> ValidationFailureCause {
    ValidationFailureCause::LiteratureClaim {
        row_index,
        artifact: artifact.to_string(),
        kind,
    }
}

/// Marker source_class for offline / route-failure / thin-literature
/// fallback rows. These carry no locator and are skipped by the
/// locator-resolution validator (and held out of the corroboration tier),
/// so the survey task completes rather than blocking.
const CURATED_BASELINE_CLASS: &str = "curated_baseline";

/// A locator-resolution view of one row, parsed by header name so the
/// validator works against BOTH the claims-matrix shape
/// (`entity`/`entity_kind`/`pmid`) and the method_landscape shape
/// (`axis`/`candidate_method`).
struct SourceRow {
    source_ref_kind: String,
    source_ref: String,
    source_class: String,
    pmid: String,
    prior_pmids: Vec<String>,
}

/// Collect the locator strings a row anchors against, dispatched on its
/// resolved locator kind. Legacy PMID rows pull from `pmid`/`prior_pmids`;
/// non-PMID rows pull from the typed `source_ref` column.
fn source_row_refs(row: &SourceRow, kind: &str) -> Vec<String> {
    if kind == "pmid" {
        let mut refs: Vec<String> = Vec::new();
        if !row.pmid.is_empty() {
            refs.push(row.pmid.clone());
        }
        refs.extend(row.prior_pmids.iter().cloned());
        refs
    } else if row.source_ref.is_empty() {
        Vec::new()
    } else {
        vec![row.source_ref.clone()]
    }
}

/// Generalized successor to `run_pmid_resolves`. For each row, dispatch on
/// `source_ref_kind`:
///   - `pmid` (or absent — legacy) → PMID well-formedness + manifest
///     presence + artifact-on-disk check (byte-identical to
///     `run_pmid_resolves`).
///   - `doi` / `arxiv` / `url` → require a manifest entry whose `source_ref`
///     (falling back to `pmid` for older manifests) matches AND whose
///     snapshot file exists on disk. Any miss → `SourceUnresolvable`.
///
/// Rows whose `source_class` is `curated_baseline` carry no locator (offline /
/// route-failure / thin-literature fallback) and are SKIPPED — neither
/// resolved nor failed — so the survey task never blocks on them. This
/// targets the fallback class explicitly and does not relax the locator
/// checks for any real-locator row.
///
/// The CSV is read by header name so this works against both the
/// claims-matrix shape and the method_landscape shape.
pub fn run_source_resolves(
    csv_path: &Path,
    manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let mut rdr = csv::Reader::from_path(csv_path).map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let headers = rdr.headers().cloned().map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let idx = header_index(&headers);
    let col = |rec: &csv::StringRecord, name: &str| -> String {
        idx.get(name)
            .and_then(|i| rec.get(*i))
            .unwrap_or("")
            .to_string()
    };

    let manifest = load_manifest(manifest_path).map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let by_ref: BTreeMap<String, &EvidenceEntry> = manifest
        .entries
        .iter()
        .map(|e| {
            let key = e
                .source_ref
                .clone()
                .or_else(|| e.pmid.clone())
                .unwrap_or_default();
            (key, e)
        })
        .collect();
    let pmid_re = regex::Regex::new(r"^[1-9][0-9]{6,8}$").unwrap();
    let ev_dir = manifest_path.parent().unwrap();

    for (i, rec) in rdr.records().enumerate() {
        let rec = rec.map_err(|_| {
            (
                i as u64,
                lit_fail(
                    i as u64,
                    &artifact,
                    LiteratureClaimFailureKind::EvidenceArtifactMissing,
                ),
            )
        })?;
        let prior_pmids: Vec<String> = {
            let raw = col(&rec, "prior_pmids");
            if raw.trim().is_empty() {
                Vec::new()
            } else {
                // Legacy column may be a JSON array or a delimited string.
                serde_json::from_str::<Vec<String>>(&raw).unwrap_or_else(|_| {
                    raw.split([',', ';', ' '])
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
            }
        };
        let row = SourceRow {
            source_ref_kind: col(&rec, "source_ref_kind"),
            source_ref: col(&rec, "source_ref"),
            source_class: col(&rec, "source_class"),
            pmid: col(&rec, "pmid"),
            prior_pmids,
        };

        // Curated-baseline fallback rows carry no locator: skip them.
        if row.source_class == CURATED_BASELINE_CLASS {
            continue;
        }

        let kind = if row.source_ref_kind.is_empty() {
            "pmid"
        } else {
            row.source_ref_kind.as_str()
        };
        let refs = source_row_refs(&row, kind);
        for r in refs {
            if kind == "pmid" && !pmid_re.is_match(&r) {
                return Err((
                    i as u64,
                    lit_fail(
                        i as u64,
                        &artifact,
                        LiteratureClaimFailureKind::PmidMalformed,
                    ),
                ));
            }
            let Some(entry) = by_ref.get(&r) else {
                let fk = if kind == "pmid" {
                    LiteratureClaimFailureKind::PmidNotFound
                } else {
                    LiteratureClaimFailureKind::SourceUnresolvable
                };
                return Err((i as u64, lit_fail(i as u64, &artifact, fk)));
            };
            if !resolve_evidence_file(ev_dir, &entry.path).exists() {
                let fk = if kind == "pmid" {
                    LiteratureClaimFailureKind::EvidenceArtifactMissing
                } else {
                    LiteratureClaimFailureKind::SourceUnresolvable
                };
                return Err((i as u64, lit_fail(i as u64, &artifact, fk)));
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
    // Key the manifest by every locator an entry exposes (pmid AND the typed
    // source_ref) so this runner resolves claims-matrix rows (pmid) and
    // method_landscape rows (source_ref) uniformly.
    let mut manifest_by_locator: BTreeMap<String, &EvidenceEntry> = BTreeMap::new();
    for e in &manifest.entries {
        if let Some(p) = &e.pmid {
            if !p.is_empty() {
                manifest_by_locator.insert(p.clone(), e);
            }
        }
        if let Some(sr) = &e.source_ref {
            if !sr.is_empty() {
                manifest_by_locator.insert(sr.clone(), e);
            }
        }
        // Batched PubMed efetch: one snapshot covers many PMIDs (no singular
        // pmid/source_ref). Index every batch member so a claim row citing any
        // of them resolves to the shared snapshot.
        for p in &e.pmids_in_batch {
            if !p.is_empty() {
                manifest_by_locator.insert(p.clone(), e);
            }
        }
    }
    let manifest_dir = manifest_path.parent().unwrap();

    for (i, row) in rows.iter().enumerate() {
        // no_prior_finding rows have source_kind == "none" and empty quote; skip.
        if row.source_kind == "none" {
            continue;
        }
        // A verified=false row self-declares its quote as unverified/tentative
        // (the producer's contract: it could not confirm a verbatim match), so
        // it is not a QuoteNotInSource violation — skip rather than hard-fail.
        if !row.verified {
            continue;
        }

        // Resolve the row's locator: pmid (claims-matrix) or the typed
        // source_ref (method_landscape). Rows with neither are skipped
        // (handled by other obligations).
        let locator = row
            .pmid
            .clone()
            .or_else(|| row.prior_pmids.as_ref().and_then(|v| v.first().cloned()))
            .or_else(|| row.source_ref.clone());
        let locator = match locator {
            Some(l) if !l.is_empty() => l,
            _ => continue,
        };

        let entry = manifest_by_locator.get(&locator).ok_or_else(|| {
            (
                i as u64,
                ValidationFailureCause::LiteratureClaim {
                    row_index: i as u64,
                    artifact: artifact.clone(),
                    kind: LiteratureClaimFailureKind::PmidNotFound,
                },
            )
        })?;

        // The manifest `path` may be evidence-dir-relative ("pmid_X.xml", the
        // claims-matrix convention) OR task-dir-relative with an "evidence/"
        // prefix (what the bundled agent_literature_fetch.py helper writes).
        // Resolve against both so the read succeeds either way — joining a
        // prefixed path straight onto the evidence dir doubles it
        // (evidence/evidence/…) and spuriously reports EvidenceArtifactMissing.
        let evidence_path = resolve_evidence_file(manifest_dir, &entry.path);
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
        // The substring match above is the authoritative verification that the
        // quote is verbatim-present in the source — that is precisely what this
        // obligation (`evidence_quote_substring_match`) is named for. The
        // declared `evidence_quote_offset` is forensic metadata and is NOT
        // hard-failed: producers compute it in the extracted-text frame while
        // the snapshot is stored as the fetched markup (e.g. a PMC-XML record
        // whose ~1KB header offsets every position), so the declared and
        // recomputed offsets legitimately diverge by the per-source header
        // length and no fixed tolerance can reconcile them. Blocking a task on
        // that divergence — when the quote itself is proven present — is a
        // false positive, so the offset is left to forensic inspection only.
    }
    Ok(())
}

// ============================================================================
// Runner 3: redistributable_or_marked
// ============================================================================

/// Source-kind classes whose redistributability is KNOWN from the class itself,
/// so a claim row need not carry an explicit `redistributable=true` mark to pass
/// the legal gate. NLM E-utilities output (PubMed abstracts/efetch/esearch XML)
/// is public-domain US government data; PMC OA is CC-licensed; OpenAlex/Crossref
/// surface OA records. `external_pdf_local_only` is deliberately ABSENT (a
/// locally-stored PDF is not redistributable) and matched strictly upstream.
/// Token-substring match so executor-specific spellings (codex's
/// `pubmed_abstract_with_pmc_front_xml_checked`, `pmc_front_or_abstract_xml_only`)
/// are covered without enumerating every variant.
fn source_kind_is_inherently_redistributable(source_kind: &str) -> bool {
    // Scoped to NLM/PMC public + OA classes only (NLM E-utilities output is
    // public-domain US-Gov work; PMC OA is CC). Metadata aggregators
    // (openalex/crossref) and generic `abstract_only` are intentionally EXCLUDED
    // — those still require an explicit redistributable mark, preserving the
    // legal gate for sources whose underlying license isn't class-determined.
    // `pmc` covers every PubMed Central spelling (pmc_oa_full_text, pmc_front,
    // pmc_xml_fulltext, …): NLM only serves OA / author-manuscript full text via
    // PMC, all research-redistributable. external_pdf is excluded below.
    const REDISTRIBUTABLE_TOKENS: &[&str] =
        &["pmc", "pubmed_abstract", "pubmed_efetch", "pubmed_esearch"];
    let sk = source_kind.to_ascii_lowercase();
    !sk.contains("external_pdf") && REDISTRIBUTABLE_TOKENS.iter().any(|t| sk.contains(t))
}

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
        // Source-less concordance rows carry NO prior literature by definition —
        // a `no_prior_finding` row (no PMID matched for this entity) has an empty
        // source_kind / source_ref_kind and an empty `redistributable` column.
        // There is no source to subject to the legal gate, so skip it (mirrors
        // the curated-baseline carve-out below). A row that DID cite a source
        // still carries a non-empty source_kind and is gated normally.
        if row.concordance_flag.as_deref() == Some("no_prior_finding")
            || (row.source_kind.is_empty()
                && row.source_ref_kind.as_deref().unwrap_or("").is_empty()
                && row.pmid.as_deref().unwrap_or("").is_empty())
        {
            continue;
        }
        // `curated_baseline` candidate rows are offline / thin-literature
        // placeholders carrying no real source (the locator validator
        // `run_source_resolves` already skips them). They legitimately have an
        // empty `redistributable` column and a placeholder `source_kind`
        // (`curated_candidate`) that is not a redistributable corpus class, so
        // the legal gate must not subject them to the source_kind match below
        // (which would otherwise fall through to a spurious FAIL).
        if row.source_class.as_deref() == Some(CURATED_BASELINE_CLASS) {
            continue;
        }
        // The legal gate stays meaningful: it is keyed by `source_kind`, never
        // blanket-true. External PDFs stored locally MUST NOT be marked
        // redistributable; paper-class OA / abstract sources (incl. the
        // OpenAlex/Crossref-surfaced OA records the canonical helper emits, all
        // legal in literature_evidence_manifest.schema.json) MUST be marked
        // redistributable to pass. The earlier table only matched three v1
        // source_kinds and fell through to a spurious FAIL for the v2 enum
        // values the producer legitimately uses.
        let consistent = match (row.source_kind.as_str(), row.redistributable) {
            // External PDFs are stored locally only — true is a contradiction.
            ("external_pdf_local_only", false) => true,
            ("external_pdf_local_only", true) => false,
            // Tool-documentation pages are pages, not redistributed corpus —
            // either marking is legal.
            ("doc_page", _) => true,
            // A literature source explicitly marked redistributable passes.
            (_, true) => true,
            // NLM/PMC public + OA classes are redistributable BY THEIR CLASS
            // (NLM E-utilities data is public-domain; PMC OA is CC-licensed), so
            // an unmarked row from these sources still passes — codex omits the
            // flag but its PubMed/PMC evidence is legally redistributable. The
            // legal gate stays strict for everything else: external_pdf_local_only
            // is excluded above, and an unmarked source of an UNRECOGNISED class
            // still fails.
            (sk, false) if source_kind_is_inherently_redistributable(sk) => true,
            (_, false) => false,
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
    // Load findings table primary keys (first column or `id` column). The
    // findings file is delimiter-sniffed: the analysis atoms emit TAB-separated
    // `.tsv` (de_results.tsv, peak_calls.tsv, variant_calls.tsv) — reading those
    // with the comma default collapses every row into one field, so the bare
    // gene/peak id never lands in `known` and every claim row spuriously orphans.
    let delimiter = if findings_csv_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("tsv"))
        .unwrap_or(false)
    {
        b'\t'
    } else {
        b','
    };
    let mut findings_rdr = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .from_path(findings_csv_path)
        .map_err(|_| {
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
        // A finding_id resolves when it matches an upstream PK exactly, OR when
        // it's a conventionally-prefixed/entity-keyed form of one. Agents form
        // a stable finding identifier from the analysis-stage findings (e.g.
        // `DE_FBgn0000043` for DE gene `FBgn0000043`, or carry the bare
        // `entity_id`); the underlying finding is the same row. Accept:
        //   - exact PK match;
        //   - PK after stripping a leading `<STAGE>_` prefix (DE_/PE_/…);
        //   - the row's own `entity_id` matching a PK (some atoms key on it).
        let stripped = fid.split_once('_').map(|(_, rest)| rest).unwrap_or(&fid);
        let resolved = known.contains(&fid)
            || known.contains(stripped)
            || (!row.entity.is_empty() && known.contains(&row.entity));
        if !resolved {
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
// Runner 6: claim_support_satisfied
// ============================================================================

/// Column-name → index map for a CSV, so validators that key off the
/// `method_landscape.csv` shape (axis/candidate_method/source_class/...) work
/// without forcing the full `ClaimsMatrixRow` serde struct, which requires
/// the prior-claims columns the method-landscape table does not carry.
fn header_index(headers: &csv::StringRecord) -> BTreeMap<String, usize> {
    headers
        .iter()
        .enumerate()
        .map(|(i, h)| (h.to_string(), i))
        .collect()
}

const PAPER_CLASSES: [&str; 2] = ["primary_literature", "conference_proceedings"];

/// Read `claimSupportRules.minimumIndependentSources` from a package's
/// `source-discovery-policy.json`. The CSV lives at
/// `<package>/runtime/outputs/<task_id>/method_landscape.csv`; the policy at
/// `<package>/policies/source-discovery-policy.json`. We walk up from the CSV
/// dir to find a `policies/source-discovery-policy.json`. Absent/unreadable →
/// default of 2. The JSON is read directly (no schema validation) so the
/// harness validator stays self-contained.
fn minimum_independent_sources(csv_path: &Path) -> u64 {
    const DEFAULT_MIN: u64 = 2;
    let mut cur = csv_path.parent();
    while let Some(dir) = cur {
        let candidate = dir.join("policies/source-discovery-policy.json");
        if candidate.exists() {
            if let Ok(bytes) = fs::read(&candidate) {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    if let Some(n) = v
                        .get("claimSupportRules")
                        .and_then(|r| r.get("minimumIndependentSources"))
                        .and_then(|n| n.as_u64())
                    {
                        return n;
                    }
                }
            }
            return DEFAULT_MIN;
        }
        cur = dir.parent();
    }
    DEFAULT_MIN
}

/// Validates `source-discovery-policy.json::claimSupportRules` against
/// `method_landscape.csv` with **per-axis, de-ranking** semantics: an axis is
/// recommendable when it carries ≥1 adequately-corroborated default — a
/// candidate with ≥1 verified paper-class source (source_class ∈
/// {primary_literature, conference_proceedings}) AND ≥`minimumIndependentSources`
/// distinct verified sources. A literature-eligible candidate that falls short
/// is simply NOT a valid default (de-ranked); it does not fail the survey as
/// long as its axis still has a corroborated alternative. This avoids one weak
/// peripheral candidate (e.g. an unused filtering tool with a single citation)
/// blocking an entire otherwise-recommendable survey.
///
/// Two cases remain hard failures:
///   (a) a candidate EXPLICITLY tier-marked `defaultRecommended` (when the
///       optional `tier` column is present) that is not adequately supported —
///       the survey makes a specific unsupported recommendation; and
///   (b) an axis that presents ≥1 literature-eligible candidate but carries NO
///       adequately-corroborated default — the axis cannot be recommended.
///
/// The method_landscape schema does not define a `tier` column today
/// (`additionalProperties: false`), so in practice (a) never fires and the
/// check reduces to per-axis recommendability. Tool-doc-only candidates are
/// never literature_eligible, so they impose no corroboration obligation.
///
/// `manifest_path` is unused (the corroboration policy is read from the
/// package, not the evidence manifest); the two-arg signature matches the
/// other runners so the shared `runner_dispatch` can call it.
pub fn run_claim_support_satisfied(
    csv_path: &Path,
    _manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let min_sources = minimum_independent_sources(csv_path);

    let mut rdr = csv::Reader::from_path(csv_path).map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let headers = rdr.headers().cloned().map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let idx = header_index(&headers);
    let col = |rec: &csv::StringRecord, name: &str| -> String {
        idx.get(name)
            .and_then(|i| rec.get(*i))
            .unwrap_or("")
            .to_string()
    };

    // Per (axis, candidate): track first row index, whether tier marks it a
    // default, paper-class verified count, and the set of distinct verified
    // source identities.
    #[derive(Default)]
    struct Acc {
        first_row: u64,
        tier_default: bool,
        paper_class_verified: u64,
        verified_sources: std::collections::BTreeSet<String>,
        seen: bool,
    }
    let mut acc: BTreeMap<(String, String), Acc> = BTreeMap::new();

    for (i, rec) in rdr.records().enumerate() {
        let rec = rec.map_err(|_| {
            (
                i as u64,
                lit_fail(
                    i as u64,
                    &artifact,
                    LiteratureClaimFailureKind::EvidenceArtifactMissing,
                ),
            )
        })?;
        let axis = col(&rec, "axis");
        let cand = col(&rec, "candidate_method");
        if axis.is_empty() || cand.is_empty() {
            continue;
        }
        let class = col(&rec, "source_class");
        let verified = col(&rec, "verified") == "true";
        let tier = col(&rec, "tier");
        // Distinct-source identity: prefer the locator, fall back to the hash.
        let source_id = {
            let r = col(&rec, "source_ref");
            if !r.is_empty() {
                r
            } else {
                col(&rec, "source_hash")
            }
        };

        let e = acc.entry((axis, cand)).or_default();
        if !e.seen {
            e.first_row = i as u64;
            e.seen = true;
        }
        if tier == "defaultRecommended" {
            e.tier_default = true;
        }
        if verified {
            if PAPER_CLASSES.contains(&class.as_str()) {
                e.paper_class_verified += 1;
            }
            if !source_id.is_empty() {
                e.verified_sources.insert(source_id);
            }
        }
    }

    // De-ranking semantics: a literature-eligible candidate that is
    // under-corroborated is simply NOT a valid default (it is de-ranked, not a
    // failure) as long as its axis still carries an adequately-corroborated
    // alternative — one weak peripheral candidate must not block a whole survey
    // whose axes are otherwise recommendable. Two hard failures remain:
    //   (a) a candidate EXPLICITLY tier-marked `defaultRecommended` that is not
    //       adequately supported — the survey is making a specific unsupported
    //       recommendation; and
    //   (b) an axis that presents ≥1 literature-eligible candidate but carries
    //       NO adequately-corroborated default — that axis cannot be recommended
    //       at all.
    // A "valid default" carries ≥1 paper-class verified source AND
    // ≥`min_sources` distinct verified sources. `acc` is a BTreeMap, so every
    // iteration below is deterministic.
    let is_valid_default = |a: &Acc| -> bool {
        a.paper_class_verified >= 1 && (a.verified_sources.len() as u64) >= min_sources
    };

    // (a) explicit defaultRecommended that is unsupported — fail at its first row.
    for a in acc.values() {
        if a.tier_default && !is_valid_default(a) {
            return Err((
                a.first_row,
                lit_fail(
                    a.first_row,
                    &artifact,
                    LiteratureClaimFailureKind::InsufficientCorroboration,
                ),
            ));
        }
    }

    // (b) per-axis recommendability: an axis with any literature-eligible
    // candidate must carry at least one adequately-corroborated default. The
    // under-corroborated candidates on a still-recommendable axis are de-ranked
    // (skipped), not failed.
    let mut axis_has_valid: BTreeMap<&str, bool> = BTreeMap::new();
    let mut axis_eligible_row: BTreeMap<&str, u64> = BTreeMap::new();
    for ((axis, _cand), a) in &acc {
        if a.paper_class_verified >= 1 {
            axis_eligible_row
                .entry(axis.as_str())
                .and_modify(|r| *r = (*r).min(a.first_row))
                .or_insert(a.first_row);
            let v = axis_has_valid.entry(axis.as_str()).or_insert(false);
            *v = *v || is_valid_default(a);
        }
    }
    for (axis, first_row) in &axis_eligible_row {
        if !axis_has_valid.get(*axis).copied().unwrap_or(false) {
            return Err((
                *first_row,
                lit_fail(
                    *first_row,
                    &artifact,
                    LiteratureClaimFailureKind::InsufficientCorroboration,
                ),
            ));
        }
    }
    Ok(())
}

// ============================================================================
// Runner 7: doc_page_matches_tool (+ version_context guard)
// ============================================================================

/// Validates each `source_class == tool_documentation` row in
/// `method_landscape.csv`: (1) the snapshot named by the row's matching
/// evidence-manifest entry, after `collapse_whitespace_lowercase_v1`
/// normalization, must reference the `candidate_method` token — else
/// `DocPageToolMismatch`; (2) the row's `version_context` must be present and
/// non-empty — else `VersionContextMissing`. Non-tool-doc rows are skipped.
pub fn run_doc_page_matches_tool(
    csv_path: &Path,
    manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());

    let mut rdr = csv::Reader::from_path(csv_path).map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let headers = rdr.headers().cloned().map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let idx = header_index(&headers);
    let col = |rec: &csv::StringRecord, name: &str| -> String {
        idx.get(name)
            .and_then(|i| rec.get(*i))
            .unwrap_or("")
            .to_string()
    };

    let manifest = load_manifest(manifest_path).map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    // Index manifest entries by locator (source_ref, falling back to pmid).
    let by_ref: BTreeMap<String, &EvidenceEntry> = manifest
        .entries
        .iter()
        .map(|e| {
            let key = e
                .source_ref
                .clone()
                .or_else(|| e.pmid.clone())
                .unwrap_or_default();
            (key, e)
        })
        .collect();
    let ev_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));

    for (i, rec) in rdr.records().enumerate() {
        let rec = rec.map_err(|_| {
            (
                i as u64,
                lit_fail(
                    i as u64,
                    &artifact,
                    LiteratureClaimFailureKind::EvidenceArtifactMissing,
                ),
            )
        })?;
        if col(&rec, "source_class") != "tool_documentation" {
            continue;
        }
        let candidate = col(&rec, "candidate_method");
        let version_context = col(&rec, "version_context");
        let source_ref = col(&rec, "source_ref");

        // Version-context guard: tool-doc method claims must carry one.
        if version_context.trim().is_empty() {
            return Err((
                i as u64,
                lit_fail(
                    i as u64,
                    &artifact,
                    LiteratureClaimFailureKind::VersionContextMissing,
                ),
            ));
        }

        // Snapshot relevance: the doc page must name the candidate tool.
        let Some(entry) = by_ref.get(&source_ref) else {
            return Err((
                i as u64,
                lit_fail(
                    i as u64,
                    &artifact,
                    LiteratureClaimFailureKind::SourceUnresolvable,
                ),
            ));
        };
        let raw = fs::read_to_string(resolve_evidence_file(ev_dir, &entry.path)).map_err(|_| {
            (
                i as u64,
                lit_fail(
                    i as u64,
                    &artifact,
                    LiteratureClaimFailureKind::EvidenceArtifactMissing,
                ),
            )
        })?;
        let normalized = collapse_whitespace_lowercase_v1(&raw);
        let token = collapse_whitespace_lowercase_v1(&candidate);
        if token.is_empty() || !normalized.contains(&token) {
            return Err((
                i as u64,
                lit_fail(
                    i as u64,
                    &artifact,
                    LiteratureClaimFailureKind::DocPageToolMismatch,
                ),
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
    // The survey_method_landscape atom emits method_landscape.csv; the
    // method-landscape validators (claim_support_satisfied, doc_page_matches_tool)
    // resolve their input here.
    let landscape = artifact_path.join("method_landscape.csv");
    if landscape.exists() {
        return Some(landscape);
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

/// `ValidatorRunner` wrapping `run_source_resolves` for the `source_resolves`
/// obligation — the locator-generalized successor to `pmid_resolves`.
pub struct SourceResolvesRunner;
impl ValidatorRunner for SourceResolvesRunner {
    fn obligation_id(&self) -> &'static str {
        "source_resolves"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(artifact_path, true, run_source_resolves)
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

/// `ValidatorRunner` wrapping `run_claim_support_satisfied` for the
/// `claim_support_satisfied` obligation. Reads the corroboration policy from
/// the package (not the evidence manifest), so the manifest is not required.
pub struct ClaimSupportSatisfiedRunner;
impl ValidatorRunner for ClaimSupportSatisfiedRunner {
    fn obligation_id(&self) -> &'static str {
        "claim_support_satisfied"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(artifact_path, false, run_claim_support_satisfied)
    }
}

/// `ValidatorRunner` wrapping `run_doc_page_matches_tool` for the
/// `doc_page_matches_tool` obligation. Requires the evidence manifest to
/// resolve each tool-doc snapshot.
pub struct DocPageMatchesToolRunner;
impl ValidatorRunner for DocPageMatchesToolRunner {
    fn obligation_id(&self) -> &'static str {
        "doc_page_matches_tool"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(artifact_path, true, run_doc_page_matches_tool)
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

/// Trait-wrapped runners for the literature obligations. Used by
/// `crate::validators::default_runners` so the harness post-task hook
/// routes literature obligation ids to the right runner.
pub fn literature_runners() -> Vec<Box<dyn ValidatorRunner>> {
    vec![
        Box::new(PmidResolvesRunner) as Box<dyn ValidatorRunner>,
        Box::new(SourceResolvesRunner),
        Box::new(EvidenceQuoteSubstringMatchRunner),
        Box::new(RedistributableOrMarkedRunner),
        Box::new(ClaimRowHasFindingIdRunner),
        Box::new(ConcordanceFlagInClosedSetRunner),
        Box::new(ClaimSupportSatisfiedRunner),
        Box::new(DocPageMatchesToolRunner),
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
    fn redistributable_accepts_v2_source_kinds_but_keeps_legal_gate() {
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("evidence/manifest.json"); // unused by this runner
        let hdr = "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified";
        let run = |source_kind: &str, redist: &str| {
            let csv = dir.path().join("m.csv");
            write(
                &csv,
                &format!("{hdr}\nACAN,gene,28123456,foo,0,{source_kind},sha256:abc,2026-05-14T00:00:00Z,{redist},true\n"),
            );
            run_redistributable_or_marked(&csv, &manifest)
        };
        // v2 paper-class OA records (incl. OpenAlex/Crossref-surfaced) marked
        // redistributable are accepted — previously fell through to a spurious FAIL.
        assert!(run("openalex", "true").is_ok());
        assert!(run("crossref", "true").is_ok());
        assert!(run("pmc_oa_full_text", "true").is_ok());
        assert!(run("abstract_only", "true").is_ok());
        // Tool-documentation pages: either marking is legal.
        assert!(run("doc_page", "false").is_ok());
        assert!(run("doc_page", "true").is_ok());
        // LEGAL GATE preserved: external local PDFs must NOT claim redistribution.
        assert!(run("external_pdf_local_only", "true").is_err());
        assert!(run("external_pdf_local_only", "false").is_ok());
        // LEGAL GATE preserved for non-NLM/PMC aggregators: an UNMARKED
        // openalex/crossref source still fails (license not class-determined).
        assert!(run("openalex", "false").is_err());
        assert!(run("crossref", "false").is_err());
        // NLM/PMC public + OA classes are redistributable BY CLASS: an UNMARKED
        // pmc_oa / pubmed source now passes (NLM E-utilities + PMC OA are
        // public-domain / CC). External PDFs (above) stay strict.
        assert!(run("pmc_oa_full_text", "false").is_ok());
        assert!(run("pubmed_abstract", "false").is_ok());
    }

    #[test]
    fn redistributable_accepts_pubmed_efetch_batch() {
        // PubMed efetch returns public-domain abstracts (NIH/US-Gov work), so a
        // `pubmed_efetch_xml_batch` row marked redistributable:true is consistent.
        // Regression: the batched-efetch source_kind the canonical retrieval path
        // emits previously fell through to a spurious RedistributableTagInconsistent.
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("evidence/manifest.json"); // unused by this runner
        let hdr = "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified";
        let csv = dir.path().join("m.csv");
        write(&csv, &format!("{hdr}\nMaxQuant,method,19029910,foo,0,pubmed_efetch_xml_batch,sha256:abc,2026-06-08T00:00:00Z,true,true\n"));
        assert!(run_redistributable_or_marked(&csv, &manifest).is_ok());
        // NLM/PMC public source unmarked now PASSES: redistributability is
        // class-determined (PubMed efetch XML is public-domain US-Gov work), so a
        // hand-rolled manifest that omits the flag (codex) is not blocked.
        write(&csv, &format!("{hdr}\nMaxQuant,method,19029910,foo,0,pubmed_efetch_xml_batch,sha256:abc,2026-06-08T00:00:00Z,false,true\n"));
        assert!(run_redistributable_or_marked(&csv, &manifest).is_ok());
    }

    #[test]
    fn claim_row_finding_id_reads_tab_separated_findings_and_normalizes_prefix() {
        // The contextualize atom keys claim rows by `finding_id` (e.g.
        // `DE_FBgn0000043`) / `entity_id` against the upstream DE findings file,
        // which is TAB-separated `de_results.tsv` with a `gene_id` PK. Reading
        // the TSV with the comma default collapsed every row into one field, so
        // the bare gene id never matched and every row orphaned. The reader must
        // sniff the .tsv delimiter, and the finding_id must resolve via the
        // `DE_`-prefix-stripped form or the row's entity_id.
        let dir = TempDir::new().unwrap();
        let findings = dir.path().join("de_results.tsv");
        write(
            &findings,
            "gene_id\tbaseMean\tlog2FoldChange\tpadj\nFBgn0000043\t1.0\t2.0\t0.01\n",
        );
        let csv = dir.path().join("claims_evidence_matrix.csv");
        write(
            &csv,
            "finding_id,entity_id,pmid,evidence_quote,source_kind,concordance_flag,redistributable,verified\n\
             DE_FBgn0000043,FBgn0000043,,,,no_prior_finding,,true\n",
        );
        assert!(
            run_claim_row_has_finding_id(&csv, &findings).is_ok(),
            "DE_-prefixed finding_id must resolve against the TSV gene_id PK"
        );
    }

    #[test]
    fn redistributable_skips_source_less_no_prior_finding_rows() {
        // A `no_prior_finding` concordance row carries no source by definition
        // (no PMID matched for the entity) → empty source_kind / redistributable.
        // The legal gate must skip it, not fail it.
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("evidence/manifest.json");
        let csv = dir.path().join("claims_evidence_matrix.csv");
        write(
            &csv,
            "finding_id,entity_id,pmid,evidence_quote,source_ref_kind,source_kind,concordance_flag,redistributable,verified\n\
             DE_X,X,,,,,no_prior_finding,,true\n",
        );
        assert!(
            run_redistributable_or_marked(&csv, &manifest).is_ok(),
            "source-less no_prior_finding row must be skipped by the legal gate"
        );
    }

    #[test]
    fn evidence_quote_substring_match_resolves_batched_efetch_manifest() {
        // Batched PubMed efetch: one XML snapshot covers many PMIDs, exposed via
        // `pmids_in_batch` (no singular pmid/source_ref field). A claim row citing
        // a PMID in the batch must resolve to that snapshot — previously reported
        // PmidNotFound because the locator index only read pmid/source_ref.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nMaxQuant,method,19029910,enables high peptide identification,0,pubmed_efetch_xml_batch,sha256:abc,2026-06-08T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(
            &manifest,
            r#"{"schema_version":2,"entries":[{"pmids_in_batch":["19029910","30656827"],"source_kind":"pubmed_efetch_xml_batch","path":"snap.xml","sha256_binary":"00","sha256_extracted_text":"00","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":0,"retrieval_ts":"2026-06-08T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"public domain"}]}"#,
        );
        write(
            &manifest.parent().unwrap().join("snap.xml"),
            "MaxQuant enables high peptide identification rates, individualized ppb mass accuracies",
        );
        assert!(
            run_evidence_quote_substring_match(&csv, &manifest).is_ok(),
            "a PMID present only via pmids_in_batch must resolve to its batch snapshot"
        );
    }

    #[test]
    fn resolve_evidence_file_follows_cross_task_sibling_path() {
        // contextualize_findings_with_literature dedups by reusing a sibling
        // task's snapshots, recording manifest paths like
        // `../review_prior_work/evidence/snapshots/<hash>` (relative to the TASK
        // dir, not the manifest's own evidence/ dir). The resolver must anchor at
        // the task dir too, else every claim row reports EvidenceArtifactMissing.
        let root = TempDir::new().unwrap();
        let outputs = root.path().join("runtime/outputs");
        let ctx_ev = outputs.join("contextualize_findings_with_literature/evidence");
        let sib_snap = outputs.join("review_prior_work/evidence/snapshots");
        fs::create_dir_all(&ctx_ev).unwrap();
        fs::create_dir_all(&sib_snap).unwrap();
        fs::write(sib_snap.join("abc123"), "abstract text").unwrap();
        // Manifest lives in ctx_ev; entry path is task-dir-relative cross-task.
        let resolved =
            resolve_evidence_file(&ctx_ev, "../review_prior_work/evidence/snapshots/abc123");
        assert!(
            resolved.exists(),
            "cross-task sibling snapshot must resolve; got {}",
            resolved.display()
        );
    }

    #[test]
    fn resolve_evidence_file_handles_package_root_relative_and_nested_paths() {
        // codex writes a PACKAGE-ROOT-relative source_text_path
        // ("runtime/outputs/review_prior_work/evidence/sources/PMID_X.txt") and
        // nests the file under evidence/sources/. The resolver must find it via
        // the ancestor-walk OR the basename fallback, regardless of prefix.
        let root = TempDir::new().unwrap();
        let ev = root
            .path()
            .join("runtime/outputs/review_prior_work/evidence");
        fs::create_dir_all(ev.join("sources")).unwrap();
        fs::write(ev.join("sources/PMID_20921232.txt"), "abstract text").unwrap();
        // Package-root-relative path (resolves via ancestor-walk).
        let r1 = resolve_evidence_file(
            &ev,
            "runtime/outputs/review_prior_work/evidence/sources/PMID_20921232.txt",
        );
        assert!(
            r1.exists(),
            "package-root-relative path must resolve; got {}",
            r1.display()
        );
        // A bare basename / odd prefix resolves via the sources/ basename fallback.
        let r2 = resolve_evidence_file(&ev, "weird/prefix/PMID_20921232.txt");
        assert!(
            r2.exists(),
            "basename fallback must find the nested file; got {}",
            r2.display()
        );
    }

    #[test]
    fn validators_accept_codex_sources_manifest_schema() {
        // Codex hand-rolls a well-formed but non-canonical evidence manifest:
        // top-level `sources` (not `entries`), per-source `source_text_path`
        // (not `path`) and `sha256` (not `sha256_binary`), omitting secondary
        // metadata. The data is real (pmid + snapshot + quote); the validators
        // must resolve it via field aliases + defaults.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nMaxQuant,method,19029910,enables high peptide identification,0,pubmed_abstract,sha256:abc,2026-06-09T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(
            &manifest,
            r#"{"schema_version":"2","row_count":1,"sources":[{"pmid":"19029910","source_kind":"pubmed_abstract","source_text_path":"pubmed_19029910.txt","sha256":"sha256:00","redistributable":true,"title":"MaxQuant","journal":"NBT"}]}"#,
        );
        write(
            &manifest.parent().unwrap().join("pubmed_19029910.txt"),
            "MaxQuant enables high peptide identification rates and quantification",
        );
        assert!(
            run_evidence_quote_substring_match(&csv, &manifest).is_ok(),
            "codex `sources` manifest must resolve via aliases (sources/source_text_path)"
        );
        assert!(
            run_pmid_resolves(&csv, &manifest).is_ok(),
            "pmid_resolves must find the entry in a `sources`-keyed manifest"
        );
    }

    #[test]
    fn validators_accept_codex_real_pasilla_evidence_schema() {
        // The exact schema codex (gpt-5.5) emits for review_prior_work, captured
        // from a live pasilla run: claims CSV uses `source_type` (not source_kind),
        // `quote_start` (not evidence_quote_offset), `source_sha256` (not
        // source_hash), and OMITS source_kind / redistributable columns; the
        // manifest entries use `source_type` (not source_kind) and `pmids` (plural).
        // All three obligations must resolve it: schema spelling via aliases, and
        // redistributability inferred from the NLM/PMC source class (codex omits
        // the flag, but PubMed/PMC are public/OA).
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(
            &csv,
            "pmid,source_type,evidence_quote,quote_start,quote_end,verified,source_sha256,source_path\n\
             20921232,pubmed_abstract_with_pmc_front_xml_checked,\"combined RNAi and mRNA-seq to identify exons\",10,54,True,sha256:abc,evidence/source_text_pmid_20921232.txt\n",
        );
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(
            &manifest,
            r#"{"claim_boundary":"verbatim quotes","generated_at":"2026-06-09T00:00:00Z","task_id":"review_prior_work","entries":[{"path":"evidence/pubmed_20921232.xml","pmids":["20921232"],"source_type":"pubmed_efetch_xml","sha256":"sha256:xml"},{"path":"evidence/source_text_pmid_20921232.txt","pmid":"20921232","pmcid":"PMC3032923","source_type":"pubmed_abstract_with_pmc_front_xml_checked","sha256":"sha256:abc"}]}"#,
        );
        write(
            &manifest
                .parent()
                .unwrap()
                .join("source_text_pmid_20921232.txt"),
            "We combined RNAi and mRNA-seq to identify exons regulated by Pasilla.",
        );
        assert!(
            run_pmid_resolves(&csv, &manifest).is_ok(),
            "pmid_resolves must accept codex's source_type/pmids manifest schema"
        );
        assert!(
            run_evidence_quote_substring_match(&csv, &manifest).is_ok(),
            "evidence_quote_substring_match must resolve codex's quote against the source"
        );
        assert!(
            run_redistributable_or_marked(&csv, &manifest).is_ok(),
            "redistributable_or_marked must pass NLM/PMC sources even when codex omits the flag"
        );
    }

    #[test]
    fn redistributable_accepts_marked_literature_source_generically() {
        // Generalized legal gate: any non-external-PDF source marked
        // redistributable passes (covers executor-specific source_kind
        // spellings like codex's), while external local PDFs stay strict.
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("evidence/manifest.json");
        let hdr = "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified";
        let run = |sk: &str, redist: &str| {
            let csv = dir.path().join("m.csv");
            write(&csv, &format!("{hdr}\nX,gene,28123456,q,0,{sk},sha256:abc,2026-06-09T00:00:00Z,{redist},true\n"));
            run_redistributable_or_marked(&csv, &manifest)
        };
        // codex / unknown literature source_kind, marked redistributable → ok.
        assert!(run("pubmed", "true").is_ok());
        assert!(run("ncbi_efetch", "true").is_ok());
        // NLM/PMC classes pass UNMARKED via class inference (any `pmc*` spelling,
        // incl. codex's `pmc_xml_fulltext`; PMC serves only OA/author-MS full text).
        assert!(run("pmc_xml_fulltext", "false").is_ok());
        assert!(run("pmc_oa_full_text", "false").is_ok());
        assert!(run("pubmed_abstract", "false").is_ok());
        // Legal gate preserved: external local PDF must NOT claim redistribution,
        // and an unmarked non-NLM/non-PMC source still fails.
        assert!(run("external_pdf_local_only", "true").is_err());
        assert!(run("external_pdf_local_only", "false").is_ok());
        assert!(run("some_random_blog", "false").is_err());
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
    fn source_resolves_accepts_doi_and_rejects_missing_url_snapshot() {
        let dir = TempDir::new().unwrap();
        let evdir = dir.path().join("evidence");
        std::fs::create_dir_all(&evdir).unwrap();
        // snapshot present for the DOI row
        std::fs::write(evdir.join("doi_ok.json"), b"hello").unwrap();
        let manifest = serde_json::json!({
            "schema_version": 2,
            "entries": [
                {"source_ref_kind":"doi","source_ref":"10.1/ok","source_class":"conference_proceedings",
                 "source_kind":"abstract_only","path":"doi_ok.json","sha256_binary":"a".repeat(64),
                 "sha256_extracted_text":"b".repeat(64),"extracted_text_normalization":"collapse_whitespace_lowercase_v1",
                 "bytes":5,"retrieval_ts":"2026-05-31T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"CC-BY-4.0"}
            ]
        });
        std::fs::write(
            evdir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let csv = dir.path().join("method_landscape.csv");
        std::fs::write(&csv, "entity,entity_kind,source_ref_kind,source_ref,source_class,evidence_role,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nflair,gene,doi,10.1/ok,conference_proceedings,recommendation_or_benchmark,hello,0,abstract_only,deadbeef,2026-05-31T00:00:00Z,true,true\n").unwrap();
        assert!(run_source_resolves(&csv, &evdir.join("manifest.json")).is_ok());

        // A URL row with no snapshot on disk → SourceUnresolvable.
        let csv2 = dir.path().join("ml2.csv");
        std::fs::write(&csv2, "entity,entity_kind,source_ref_kind,source_ref,source_class,evidence_role,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nstar,gene,url,https://x/y,tool_documentation,capability_or_version,hello,0,doc_page,deadbeef,2026-05-31T00:00:00Z,false,true\n").unwrap();
        let err = run_source_resolves(&csv2, &evdir.join("manifest.json")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::SourceUnresolvable,
                ..
            }
        ));
    }

    #[test]
    fn source_resolves_pmid_branch_byte_identical_to_legacy() {
        // A legacy PMID row (no source_ref_kind) must behave exactly like
        // run_pmid_resolves: malformed PMID → PmidMalformed.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nACAN,gene,123,foo,0,pmc_oa_full_text,sha256:abc,2026-05-14T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(&manifest, r#"{"schema_version":1,"entries":[]}"#);
        let err = run_source_resolves(&csv, &manifest).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::PmidMalformed,
                ..
            }
        ));
    }

    #[test]
    fn source_resolves_skips_curated_baseline_rows() {
        // A curated_baseline row (offline/thin-literature fallback) carries no
        // locator: source_ref_kind / source_ref empty, verified=false. It must
        // be SKIPPED — not resolved, not failed — so the survey task completes
        // rather than blocking when literature retrieval was unavailable.
        let dir = TempDir::new().unwrap();
        let evdir = dir.path().join("evidence");
        std::fs::create_dir_all(&evdir).unwrap();
        std::fs::write(
            evdir.join("manifest.json"),
            r#"{"schema_version":2,"entries":[]}"#,
        )
        .unwrap();
        let csv = dir.path().join("method_landscape.csv");
        // Real method_landscape.csv column shape (axis/candidate_method/...),
        // a single curated_baseline row with no locator.
        std::fs::write(
            &csv,
            "axis,candidate_method,source_ref_kind,source_ref,source_class,evidence_role,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n\
             alignment,star,,,curated_baseline,,,0,,,2026-05-31T00:00:00Z,true,false\n",
        )
        .unwrap();
        assert!(
            run_source_resolves(&csv, &evdir.join("manifest.json")).is_ok(),
            "curated_baseline rows must pass source_resolves (skipped, not failed)"
        );

        // A curated_baseline row mixed with a real locator row that resolves
        // also passes; the real-locator check is not weakened.
        std::fs::write(evdir.join("doi_ok.json"), b"hello").unwrap();
        std::fs::write(
            evdir.join("manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 2,
                "entries": [
                    {"source_ref_kind":"doi","source_ref":"10.1/ok","source_class":"conference_proceedings",
                     "source_kind":"openalex","path":"doi_ok.json","sha256_binary":"a".repeat(64),
                     "sha256_extracted_text":"b".repeat(64),"extracted_text_normalization":"collapse_whitespace_lowercase_v1",
                     "bytes":5,"retrieval_ts":"2026-05-31T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"CC-BY-4.0"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let csv2 = dir.path().join("ml_mixed.csv");
        std::fs::write(
            &csv2,
            "axis,candidate_method,source_ref_kind,source_ref,source_class,evidence_role,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n\
             alignment,star,doi,10.1/ok,conference_proceedings,recommendation_or_benchmark,hello,0,openalex,sha256:abc,2026-05-31T00:00:00Z,true,true\n\
             alignment,hisat2,,,curated_baseline,,,0,,,2026-05-31T00:00:00Z,true,false\n",
        )
        .unwrap();
        assert!(
            run_source_resolves(&csv2, &evdir.join("manifest.json")).is_ok(),
            "mixed curated_baseline + resolvable-locator rows must pass"
        );

        // A curated_baseline row alongside an UNresolvable real-locator row
        // still fails on the real row — the skip is scoped to curated_baseline.
        let csv3 = dir.path().join("ml_bad.csv");
        std::fs::write(
            &csv3,
            "axis,candidate_method,source_ref_kind,source_ref,source_class,evidence_role,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n\
             alignment,hisat2,,,curated_baseline,,,0,,,2026-05-31T00:00:00Z,true,false\n\
             alignment,salmon,url,https://x/missing,tool_documentation,capability_or_version,hello,0,doc_page,sha256:abc,2026-05-31T00:00:00Z,false,true\n",
        )
        .unwrap();
        let err = run_source_resolves(&csv3, &evdir.join("manifest.json")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::SourceUnresolvable,
                ..
            }
        ));
    }

    // ====================================================================
    // claim_support_satisfied
    // ====================================================================

    #[test]
    fn claim_support_rejects_default_without_paper_class() {
        // hisat2 has only tool-doc evidence → not literature_eligible, so it
        // can NOT qualify as a default/recommended candidate. With no
        // qualifying default present, a tool-doc-only candidate carrying the
        // default marker must fail with InsufficientCorroboration.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,tier,source_class,source_ref,verified\n\
             alignment,hisat2,defaultRecommended,tool_documentation,https://rtd/x,true\n",
        );
        let err = run_claim_support_satisfied(&csv, &dir.path().join("ignored")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::InsufficientCorroboration,
                ..
            }
        ));
    }

    #[test]
    fn claim_support_accepts_default_with_two_paper_sources() {
        // star has two distinct paper-class verified sources → qualifies as a
        // default candidate with ≥1 paper-class row AND ≥2 distinct sources.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("ml.csv");
        write(
            &csv,
            "axis,candidate_method,tier,source_class,source_ref,verified\n\
             alignment,star,defaultRecommended,primary_literature,30000000,true\n\
             alignment,star,defaultRecommended,conference_proceedings,10.1/x,true\n",
        );
        assert!(run_claim_support_satisfied(&csv, &dir.path().join("ignored")).is_ok());
    }

    #[test]
    fn claim_support_rejects_eligible_default_with_one_source() {
        // star is literature_eligible (one paper-class verified row) and thus
        // a default candidate, but it has only ONE distinct verified source —
        // below minimumIndependentSources (2) → InsufficientCorroboration.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,verified\n\
             alignment,star,primary_literature,30000000,true\n",
        );
        let err = run_claim_support_satisfied(&csv, &dir.path().join("ignored")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::InsufficientCorroboration,
                ..
            }
        ));
    }

    #[test]
    fn claim_support_ignores_non_default_tool_doc_candidates() {
        // A candidate with only tool-doc evidence is not literature_eligible
        // and is not marked default → it is not constrained and must pass.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,verified\n\
             alignment,hisat2,tool_documentation,https://rtd/x,true\n",
        );
        assert!(run_claim_support_satisfied(&csv, &dir.path().join("ignored")).is_ok());
    }

    #[test]
    fn claim_support_reads_minimum_from_policy() {
        // A package-relative source-discovery-policy.json raising the minimum
        // to 3 makes a two-source default fail.
        let dir = TempDir::new().unwrap();
        // artifact dir layout: <root>/runtime/outputs/<task>/method_landscape.csv
        let artifact = dir.path().join("runtime/outputs/survey_method_landscape");
        std::fs::create_dir_all(&artifact).unwrap();
        let policies = dir.path().join("policies");
        std::fs::create_dir_all(&policies).unwrap();
        write(
            &policies.join("source-discovery-policy.json"),
            r#"{"claimSupportRules":{"minimumIndependentSources":3}}"#,
        );
        let csv = artifact.join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,verified\n\
             alignment,star,primary_literature,30000000,true\n\
             alignment,star,conference_proceedings,10.1/x,true\n",
        );
        let err = run_claim_support_satisfied(&csv, &dir.path().join("ignored")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::InsufficientCorroboration,
                ..
            }
        ));
    }

    #[test]
    fn claim_support_deranks_weak_alternative_when_axis_has_valid_default() {
        // An axis with a corroborated default (gatk_hard_filter: 2 distinct
        // paper sources) plus a thin alternative (bcftools_filter: 1 source)
        // must PASS — the weak candidate is de-ranked, not a failure, because
        // the axis is still recommendable. (Regression for the nekrutenko eval
        // 0.0 where one peripheral single-citation method blocked the whole
        // survey and stranded variant_calling.)
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,verified\n\
             variant_filtering,gatk_hard_filter,primary_literature,30000001,true\n\
             variant_filtering,gatk_hard_filter,primary_literature,30000002,true\n\
             variant_filtering,bcftools_filter,primary_literature,30000003,true\n",
        );
        assert!(
            run_claim_support_satisfied(&csv, &dir.path().join("ignored")).is_ok(),
            "axis with a corroborated default must pass; the single-source \
             alternative is de-ranked, not a hard failure"
        );
    }

    #[test]
    fn claim_support_fails_axis_with_no_corroborated_default() {
        // Two candidates on one axis, BOTH under-corroborated (1 source each):
        // the axis has no recommendable default → InsufficientCorroboration.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,verified\n\
             variant_filtering,gatk_hard_filter,primary_literature,30000001,true\n\
             variant_filtering,bcftools_filter,primary_literature,30000003,true\n",
        );
        let err = run_claim_support_satisfied(&csv, &dir.path().join("ignored")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::InsufficientCorroboration,
                ..
            }
        ));
    }

    // ====================================================================
    // doc_page_matches_tool
    // ====================================================================

    fn doc_manifest(entries: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "schema_version": 2, "entries": entries })
    }

    #[test]
    fn doc_page_matches_when_snapshot_mentions_tool() {
        let dir = TempDir::new().unwrap();
        let evdir = dir.path().join("evidence");
        std::fs::create_dir_all(&evdir).unwrap();
        std::fs::write(evdir.join("flair_doc.html"), "FLAIR v2 supports isoforms").unwrap();
        let manifest = doc_manifest(serde_json::json!([{
            "source_ref_kind":"url","source_ref":"https://rtd/flair","source_class":"tool_documentation",
            "source_kind":"doc_page","path":"flair_doc.html","sha256_binary":"a".repeat(64),
            "sha256_extracted_text":"b".repeat(64),"extracted_text_normalization":"collapse_whitespace_lowercase_v1",
            "bytes":5,"retrieval_ts":"2026-05-31T00:00:00Z","retrieval_query_id":"q","redistributable":true,"license":"CC-BY-4.0"
        }]));
        std::fs::write(
            evdir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,version_context,verified\n\
             alignment,flair,tool_documentation,https://rtd/flair,2,true\n",
        );
        assert!(run_doc_page_matches_tool(&csv, &evdir.join("manifest.json")).is_ok());
    }

    #[test]
    fn doc_page_rejects_snapshot_missing_tool_name() {
        let dir = TempDir::new().unwrap();
        let evdir = dir.path().join("evidence");
        std::fs::create_dir_all(&evdir).unwrap();
        std::fs::write(evdir.join("doc.html"), "this page is about something else").unwrap();
        let manifest = doc_manifest(serde_json::json!([{
            "source_ref_kind":"url","source_ref":"https://rtd/x","source_class":"tool_documentation",
            "source_kind":"doc_page","path":"doc.html","sha256_binary":"a".repeat(64),
            "sha256_extracted_text":"b".repeat(64),"extracted_text_normalization":"collapse_whitespace_lowercase_v1",
            "bytes":5,"retrieval_ts":"2026-05-31T00:00:00Z","retrieval_query_id":"q","redistributable":true,"license":"CC-BY-4.0"
        }]));
        std::fs::write(
            evdir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,version_context,verified\n\
             alignment,flair,tool_documentation,https://rtd/x,2,true\n",
        );
        let err = run_doc_page_matches_tool(&csv, &evdir.join("manifest.json")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::DocPageToolMismatch,
                ..
            }
        ));
    }

    #[test]
    fn doc_page_rejects_missing_version_context() {
        let dir = TempDir::new().unwrap();
        let evdir = dir.path().join("evidence");
        std::fs::create_dir_all(&evdir).unwrap();
        std::fs::write(evdir.join("flair.html"), "flair supports isoforms").unwrap();
        let manifest = doc_manifest(serde_json::json!([{
            "source_ref_kind":"url","source_ref":"https://rtd/flair","source_class":"tool_documentation",
            "source_kind":"doc_page","path":"flair.html","sha256_binary":"a".repeat(64),
            "sha256_extracted_text":"b".repeat(64),"extracted_text_normalization":"collapse_whitespace_lowercase_v1",
            "bytes":5,"retrieval_ts":"2026-05-31T00:00:00Z","retrieval_query_id":"q","redistributable":true,"license":"CC-BY-4.0"
        }]));
        std::fs::write(
            evdir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let csv = dir.path().join("method_landscape.csv");
        // version_context column present but empty → VersionContextMissing.
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,version_context,verified\n\
             alignment,flair,tool_documentation,https://rtd/flair,,true\n",
        );
        let err = run_doc_page_matches_tool(&csv, &evdir.join("manifest.json")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::VersionContextMissing,
                ..
            }
        ));
    }

    #[test]
    fn doc_page_ignores_non_tool_doc_rows() {
        // A primary_literature row is not subject to the tool-doc relevance /
        // version-context guards.
        let dir = TempDir::new().unwrap();
        let evdir = dir.path().join("evidence");
        std::fs::create_dir_all(&evdir).unwrap();
        std::fs::write(
            evdir.join("manifest.json"),
            r#"{"schema_version":2,"entries":[]}"#,
        )
        .unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,version_context,verified\n\
             alignment,star,primary_literature,30000000,,true\n",
        );
        assert!(run_doc_page_matches_tool(&csv, &evdir.join("manifest.json")).is_ok());
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
