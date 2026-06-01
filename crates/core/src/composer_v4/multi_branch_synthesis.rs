//! First-class multi-branch DAG synthesis for the v4 planner.
//!
//! When a request resolves to >=2 modalities and no registered
//! cross-omics archetype matches, `plan()` delegates here. Each modality
//! is planned through the FULL single-modality planner (a recursive,
//! guarded `plan()` call), its node ids are namespace-prefixed, its
//! per-branch final report is stripped, and the branches are joined at a
//! `multi_modal_thematic_comparison` -> `final_reporting` pair (the
//! existing `reporting`/`final_reporting` atoms reused via alias, exactly
//! as the cross-omics archetypes do). This module holds only pure
//! assembly; the planner-private scoring/classification stays in
//! `plan()`. Determinism: snapshot -> append -> re-sort, identical
//! discipline to discover/survey synthesis.

use std::collections::BTreeSet;

/// Hard cap on branch count so a pathological modality list can't fan
/// the planner out unboundedly. Truncation is logged, never silent.
const MAX_MODALITY_BRANCHES: usize = 8;

/// Normalize a modality string into a safe, deterministic stage-id
/// prefix ending in `_`. Lowercases, maps non-alphanumerics to `_`,
/// collapses repeats, trims, and dedupes collisions with a numeric
/// suffix (`bulk_rnaseq_`, then `bulk_rnaseq2_`).
fn modality_prefix(modality: &str, used: &mut BTreeSet<String>) -> String {
    let mut base: String = modality
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    while base.contains("__") {
        base = base.replace("__", "_");
    }
    base = base.trim_matches('_').to_string();
    if base.is_empty() {
        base = "modality".to_string();
    }
    let mut candidate = format!("{base}_");
    let mut i = 2;
    while used.contains(&candidate) {
        candidate = format!("{base}{i}_");
        i += 1;
    }
    used.insert(candidate.clone());
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modality_prefix_is_deterministic_and_dedupes() {
        let mut used = BTreeSet::new();
        assert_eq!(modality_prefix("bulk_rnaseq", &mut used), "bulk_rnaseq_");
        assert_eq!(modality_prefix("chip-seq", &mut used), "chip_seq_");
        assert_eq!(modality_prefix("bulk_rnaseq", &mut used), "bulk_rnaseq2_");
        let mut u2 = BTreeSet::new();
        assert_eq!(modality_prefix("...", &mut u2), "modality_");
    }
}
