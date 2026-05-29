//! Curated EDAM subtype edges.
//!
//! EDAM subtype hierarchy: parent operation id → list of more-specific
//! child operation ids actually referenced by our 12 taxonomies. Used
//! by `find_producers` (S6.11 slot-fill) for "is this atom's
//! `produces` a subtype of the goal's `consumes`?" checks. Curated
//! edges only — every edge below is referenced by ≥ 1 stage in the
//! quarterly audit.
//!
//! # Why a curated table
//!
//! Per ADR 0004, we deliberately stop short of bundling an OWL
//! reasoner. Our hierarchy is shallow (typical chain depth 2–3) and
//! the audit-validated edges are stable across releases — refreshed
//! quarterly when we submit upstream extensions to EDAM proper. Bump
//! `EDAM_TABLE_VERSION` when refreshed; consumers can use it for
//! cache-key invalidation.
//!
//! # `ecaax:` namespace
//!
//! Atoms whose operation has no upstream EDAM coverage live under the
//! `ecaax:` namespace (per `_atom.schema.json` regex + `docs/edam-
//! extensions.md`). The map below intentionally tracks the
//! `operation:NNNN → ["ecaax:slug",...]` edges so the composer's
//! "is-a" lookup folds in our extensions transparently.

use std::collections::BTreeMap;

/// EDAM ontology version + audit timestamp this table was curated
/// against. Bump when refreshing from a new EDAM release; consumers
/// (e.g. composer cache keys) can invalidate against this string.
pub const EDAM_TABLE_VERSION: &str = "1.25-20251112T1620Z";

/// Curated subtype hierarchy for the EDAM operations referenced by
/// today's 12 taxonomies + 4 shared fragments. Maps each parent
/// operation id to the more-specific children we use.
///
/// Determinism: `BTreeMap` so iteration order is stable across runs.
/// Composer's downstream output is byte-reproducible for the same
/// goal.
pub fn edam_subtype_edges() -> BTreeMap<String, Vec<String>> {
    let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();

    // ── operation:0004 (Operation, root) ──────────────────────────────
    m.insert(
        "operation:0004".into(),
        vec![
            "operation:0292".into(),
            "operation:0337".into(),
            "operation:0361".into(),
            "operation:0362".into(),
            "operation:2238".into(),
            "operation:2422".into(),
            "operation:2424".into(),
            "operation:2428".into(),
            "operation:2436".into(),
            "operation:2945".into(),
            "operation:3180".into(),
            "operation:3192".into(),
            "operation:3196".into(),
            "operation:3198".into(),
            "operation:3218".into(),
            "operation:3222".into(),
            "operation:3223".into(),
            "operation:3225".into(),
            "operation:3227".into(),
            "operation:3229".into(),
            "operation:3232".into(),
            "operation:3432".into(),
            "operation:3435".into(),
            "operation:3460".into(),
            "operation:3563".into(),
            "operation:3630".into(),
            "operation:3672".into(),
            "operation:3715".into(),
            "operation:3767".into(),
            "operation:3800".into(),
            "operation:3935".into(),
        ],
    );

    // ── operation:0292 (Sequence alignment) ───────────────────────────
    m.insert("operation:0292".into(), vec!["operation:3198".into()]);

    // ── operation:0361 (Sequence annotation) — extended with our
    // cell-level analogs (cell typing / deconvolution). EDAM
    // 0361 is sequence-level; we treat single-cell cell-type
    // work as a subtype since the structural pattern is
    // identical (annotate observations).
    m.insert(
        "operation:0361".into(),
        vec![
            "operation:0362".into(),
            "operation:3672".into(),
            "ecaax:cell_type_annotation".into(),
            "ecaax:cell_type_deconvolution".into(),
        ],
    );

    // ── operation:2945 (Analysis) ─────────────────────────────────────
    m.insert(
        "operation:2945".into(),
        vec![
            "operation:2238".into(),
            "operation:2436".into(),
            "operation:3222".into(),
            "operation:3223".into(),
            "operation:3227".into(),
            "operation:3460".into(),
            "operation:3563".into(),
            "operation:3630".into(),
            "operation:3767".into(),
            "operation:3800".into(),
        ],
    );

    // ── operation:3192 (Sequence trimming, used as preprocessing
    // parent in current YAMLs) ─────────────────────────────────────
    m.insert(
        "operation:3192".into(),
        vec![
            "ecaax:host_decontamination".into(),
            "ecaax:base_quality_recalibration".into(),
        ],
    );

    // ── operation:3218 (Sequencing quality control) ───────────────────
    m.insert(
        "operation:3218".into(),
        vec!["ecaax:scrnaseq_cell_qc".into()],
    );

    // ── operation:3223 (Differential expression analysis) ─────────────
    m.insert(
        "operation:3223".into(),
        vec![
            "ecaax:differential_transcript_usage".into(),
            "ecaax:spatially_variable_genes".into(),
            "ecaax:isoform_caller_concordance".into(),
            "ecaax:cross_platform_dtu_comparison".into(),
            "ecaax:results_benchmarking".into(),
        ],
    );

    // ── operation:3225 (Variant classification) ───────────────────────
    m.insert(
        "operation:3225".into(),
        vec!["ecaax:variant_filtering".into()],
    );

    // ── operation:3229 (Imputation; used as coloc parent in YAMLs) ────
    m.insert(
        "operation:3229".into(),
        vec!["ecaax:gwas_colocalization".into()],
    );

    // ── operation:3232 (eQTL analysis) ────────────────────────────────
    m.insert(
        "operation:3232".into(),
        vec![
            "ecaax:gwas_locus_windowing".into(),
            "ecaax:tissue_prioritization".into(),
        ],
    );

    // ── operation:3196 (Genotyping, used as harmonization parent) ─────
    m.insert(
        "operation:3196".into(),
        vec!["ecaax:gwas_summary_harmonization".into()],
    );

    // ── operation:3432 (Clustering) ───────────────────────────────────
    m.insert(
        "operation:3432".into(),
        vec!["ecaax:spatial_clustering".into()],
    );

    // ── operation:3435 (Standardisation and normalisation) ────────────
    m.insert(
        "operation:3435".into(),
        vec![
            "ecaax:scrnaseq_batch_correction".into(),
            "ecaax:trajectory_inference".into(),
        ],
    );

    // ── operation:2424 (Comparison) — used as validate_* umbrella
    // today; expose operation:2428 (Validation) as the canonical
    // alias for future renames.
    m.insert("operation:2424".into(), vec!["operation:2428".into()]);

    // ── operation:3563 (RNA-Seq read count analysis) ──────────────────
    m.insert(
        "operation:3563".into(),
        vec!["ecaax:long_read_isoform_calling".into()],
    );

    // ── operation:3715 (Metabolomics; used as MS conversion parent) ───
    m.insert(
        "operation:3715".into(),
        vec!["ecaax:ms_raw_conversion".into()],
    );

    // ── operation:2238 (Statistical calculation) ──────────────────────
    m.insert(
        "operation:2238".into(),
        vec!["ecaax:diversity_analysis".into()],
    );

    m
}

/// True iff `child` is a direct or transitive subtype of `parent` per
/// the curated table. Returns `true` for `child == parent` (reflexive
/// closure).
pub fn is_subtype_of(child: &str, parent: &str) -> bool {
    if child == parent {
        return true;
    }
    let table = edam_subtype_edges();
    // BFS over the curated edges. Bounded by table size (~30 nodes
    // today, capped at depth 3 in practice — no risk of blowing the
    // stack).
    let mut frontier: Vec<String> = vec![parent.to_string()];
    let mut visited: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    while let Some(node) = frontier.pop() {
        if !visited.insert(node.clone()) {
            continue;
        }
        if node == child {
            return true;
        }
        if let Some(children) = table.get(&node) {
            for c in children {
                if c == child {
                    return true;
                }
                if !visited.contains(c) {
                    frontier.push(c.clone());
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_constant_is_iso_8601_dash_format() {
        // "1.25-20251112T1620Z" — sanity check that the constant is
        // shaped the way callers will key on. Format is
        // <edam-release>-<YYYYMMDDTHHMMZ>.
        assert!(EDAM_TABLE_VERSION.contains('-'));
        assert!(EDAM_TABLE_VERSION.ends_with('Z'));
    }

    #[test]
    fn root_table_contains_expected_edam_op_count() {
        // The 30+ direct children of operation:0004 in the audit.
        // Drift down from this floor would mean we silently dropped
        // a referenced operation; drift up means we added a new
        // root-level op without rebalancing the tree.
        let table = edam_subtype_edges();
        let root = table.get("operation:0004").expect("root");
        assert!(
            root.len() >= 30,
            "root edge count fell below floor (got {}); audit says ≥ 30",
            root.len()
        );
    }

    #[test]
    fn is_subtype_handles_reflexive_case() {
        assert!(is_subtype_of("operation:0292", "operation:0292"));
    }

    #[test]
    fn is_subtype_handles_one_hop() {
        // Read mapping is-a Sequence alignment.
        assert!(is_subtype_of("operation:3198", "operation:0292"));
    }

    #[test]
    fn is_subtype_handles_ecaax_extensions() {
        // ecaax:scrnaseq_batch_correction is a child of
        // operation:3435 (Standardisation and normalisation).
        assert!(is_subtype_of(
            "ecaax:scrnaseq_batch_correction",
            "operation:3435"
        ));
    }

    #[test]
    fn is_subtype_handles_two_hop_via_root() {
        // operation:3198 (Read mapping) is reachable from
        // operation:0004 via 0292; also direct child of root.
        assert!(is_subtype_of("operation:3198", "operation:0004"));
    }

    #[test]
    fn is_subtype_returns_false_for_unrelated() {
        assert!(!is_subtype_of("operation:0292", "operation:3715"));
    }

    #[test]
    fn iter_is_id_sorted() {
        // Determinism guard: BTreeMap keys are lexicographically
        // sorted. Lock this in.
        let table = edam_subtype_edges();
        let keys: Vec<&String> = table.keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }
}
