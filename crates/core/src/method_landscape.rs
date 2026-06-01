//! Reference loader that turns a runtime `method_landscape.csv` into the
//! per-axis [`CandidateMetadata`] the [`crate::composite_score`] rubric
//! consumes. The execution agent mirrors this logic at runtime; this module
//! is the source of truth that tests pin against.
//!
//! # CSV shape
//!
//! Every method-landscape row carries the merged-foundation locator columns
//! (`entity`, `entity_kind`, `evidence_quote`, `evidence_quote_offset`,
//! `source_kind`, `source_hash`, `retrieval_ts`, `redistributable`,
//! `verified`). This workstream's loader additionally reads `axis`,
//! `candidate_method`, `source_class`, `tier`, and `verified`.
//!
//! # Eligibility
//!
//! A candidate is `literature_eligible` iff it has ≥1 VERIFIED row whose
//! `source_class` is a paper class — `primary_literature` or
//! `conference_proceedings`. `high_quality_evidence_count` counts exactly
//! those rows. Tool-documentation-only candidates are therefore NOT
//! literature-eligible.

use std::collections::{BTreeMap, BTreeSet};

use crate::composite_score::{CandidateMetadata, Confidence};

/// Source classes that count as paper-class evidence for the
/// `literature_eligible` predicate and `high_quality_evidence_count`.
const PAPER_SOURCE_CLASSES: [&str; 2] = ["primary_literature", "conference_proceedings"];

/// Per-axis map: `axis -> [(candidate_method, metadata)]`. The inner vec is
/// sorted by candidate name (the accumulator is a [`BTreeMap`]) so the output
/// is deterministic.
pub type CandidateMetadataByAxis = BTreeMap<String, Vec<(String, CandidateMetadata)>>;

/// Accumulator for one `(axis, candidate)` cell while scanning rows.
#[derive(Default)]
struct CandidateAcc {
    supporting: usize,
    high_quality: usize,
    contradictory: usize,
}

impl CandidateAcc {
    fn into_metadata(self) -> CandidateMetadata {
        CandidateMetadata {
            blocking_gate_failures: 0,
            confidence: if self.high_quality >= 1 {
                Confidence::High
            } else {
                Confidence::Moderate
            },
            supporting_evidence_count: self.supporting,
            high_quality_evidence_count: self.high_quality,
            contradictory_evidence_count: self.contradictory,
            freshness_acceptable: true,
            // Eligible iff ≥1 paper-class verified source.
            literature_eligible: self.high_quality >= 1,
        }
    }
}

/// Fold a parsed method-landscape CSV into per-`(axis, candidate)`
/// accumulators. Shared by both the plain and the curated-aware loaders.
fn accumulate(csv: &str) -> anyhow::Result<BTreeMap<(String, String), CandidateAcc>> {
    let mut rdr = csv::Reader::from_reader(csv.as_bytes());
    let headers = rdr.headers()?.clone();
    let idx = |name: &str| headers.iter().position(|h| h == name);
    let i_axis = idx("axis");
    let i_cand = idx("candidate_method");
    let i_class = idx("source_class");
    let i_verified = idx("verified");

    let mut acc: BTreeMap<(String, String), CandidateAcc> = BTreeMap::new();
    for rec in rdr.records() {
        let rec = rec?;
        let get = |i: Option<usize>| i.and_then(|i| rec.get(i)).unwrap_or("").to_string();
        let axis = get(i_axis);
        let cand = get(i_cand);
        let class = get(i_class);
        let verified = get(i_verified) == "true";
        if axis.is_empty() || cand.is_empty() {
            continue;
        }
        let e = acc.entry((axis, cand)).or_default();
        if verified {
            e.supporting += 1;
            if PAPER_SOURCE_CLASSES.contains(&class.as_str()) {
                e.high_quality += 1;
            }
        }
    }
    Ok(acc)
}

/// Parse a method-landscape CSV string into `axis -> [(candidate, metadata)]`.
///
/// This is the reference implementation of the per-candidate evidence rollup
/// the agent applies before ranking with [`crate::composite_score`]. It does
/// no ranking and no curated-pool awareness — see
/// [`load_candidate_metadata_from_str_with_curated`] for the variant that
/// marks non-curated candidates `tentative`.
pub fn load_candidate_metadata_from_str(csv: &str) -> anyhow::Result<CandidateMetadataByAxis> {
    let acc = accumulate(csv)?;
    let mut out: CandidateMetadataByAxis = BTreeMap::new();
    for ((axis, cand), a) in acc {
        out.entry(axis).or_default().push((cand, a.into_metadata()));
    }
    Ok(out)
}

/// A candidate paired with its rolled-up evidence metadata and a `tentative`
/// flag. `tentative == true` means the candidate was surfaced from literature
/// but is NOT in the axis's curated pool — it is selectable/shown but must
/// route through the proposal/promotion pipeline before it can execute.
#[derive(Debug, Clone)]
pub struct CuratedCandidate {
    /// Method name (the `candidate_method` column value).
    pub method: String,
    /// Rolled-up per-candidate evidence metadata.
    pub metadata: CandidateMetadata,
    /// `true` when the candidate is not in the axis's curated pool.
    pub tentative: bool,
}

/// Curated-aware variant of [`load_candidate_metadata_from_str`].
///
/// `curated_by_axis` maps each axis to its curated candidate pool. A candidate
/// whose method name is NOT in its axis's curated set is marked
/// `tentative = true`; a curated candidate is `tentative = false`. Axes absent
/// from `curated_by_axis` are treated as having an empty curated pool, so every
/// candidate on such an axis is `tentative`.
pub fn load_candidate_metadata_from_str_with_curated(
    csv: &str,
    curated_by_axis: &BTreeMap<String, BTreeSet<String>>,
) -> anyhow::Result<BTreeMap<String, Vec<CuratedCandidate>>> {
    let acc = accumulate(csv)?;
    let empty: BTreeSet<String> = BTreeSet::new();
    let mut out: BTreeMap<String, Vec<CuratedCandidate>> = BTreeMap::new();
    for ((axis, cand), a) in acc {
        let curated = curated_by_axis.get(&axis).unwrap_or(&empty);
        let tentative = !curated.contains(&cand);
        out.entry(axis).or_default().push(CuratedCandidate {
            method: cand,
            metadata: a.into_metadata(),
            tentative,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_CANDIDATE_CSV: &str = "axis,candidate_method,source_ref_kind,source_ref,source_class,evidence_role,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n\
        alignment,star,pmid,30000000,primary_literature,recommendation_or_benchmark,q,0,pmc_oa_full_text,h,2026-01-01T00:00:00Z,true,true\n\
        alignment,hisat2,url,https://rtd/x,tool_documentation,capability_or_version,q,0,doc_page,h,2026-01-01T00:00:00Z,false,true\n";

    #[test]
    fn eligibility_requires_paper_class_evidence() {
        let per_axis = load_candidate_metadata_from_str(TWO_CANDIDATE_CSV).unwrap();
        let aln = per_axis.get("alignment").unwrap();
        let star = aln.iter().find(|(c, _)| c == "star").unwrap();
        let hisat = aln.iter().find(|(c, _)| c == "hisat2").unwrap();
        assert!(
            star.1.literature_eligible,
            "paper-class verified row → eligible"
        );
        assert!(
            !hisat.1.literature_eligible,
            "tool-doc-only → not literature_eligible"
        );
        assert_eq!(star.1.high_quality_evidence_count, 1);
    }

    #[test]
    fn unverified_paper_row_is_not_high_quality() {
        // A paper-class row that did NOT pass substring verification must not
        // count toward eligibility.
        let csv = "axis,candidate_method,source_class,verified\n\
            alignment,star,primary_literature,false\n";
        let per_axis = load_candidate_metadata_from_str(csv).unwrap();
        let star = per_axis
            .get("alignment")
            .unwrap()
            .iter()
            .find(|(c, _)| c == "star")
            .unwrap();
        assert!(!star.1.literature_eligible);
        assert_eq!(star.1.high_quality_evidence_count, 0);
        assert_eq!(star.1.supporting_evidence_count, 0);
    }

    #[test]
    fn curated_aware_loader_marks_non_curated_tentative() {
        let mut curated: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        curated.insert(
            "alignment".to_string(),
            // Only `star` is in the curated pool for the alignment axis.
            ["star".to_string()].into_iter().collect(),
        );
        let per_axis =
            load_candidate_metadata_from_str_with_curated(TWO_CANDIDATE_CSV, &curated).unwrap();
        let aln = per_axis.get("alignment").unwrap();
        let star = aln.iter().find(|c| c.method == "star").unwrap();
        let hisat = aln.iter().find(|c| c.method == "hisat2").unwrap();
        assert!(!star.tentative, "curated candidate → tentative=false");
        assert!(
            hisat.tentative,
            "non-curated literature candidate → tentative=true"
        );
    }

    #[test]
    fn axis_without_curated_pool_marks_everything_tentative() {
        let curated: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let per_axis =
            load_candidate_metadata_from_str_with_curated(TWO_CANDIDATE_CSV, &curated).unwrap();
        for cand in per_axis.get("alignment").unwrap() {
            assert!(
                cand.tentative,
                "no curated pool for axis → {} must be tentative",
                cand.method
            );
        }
    }
}
