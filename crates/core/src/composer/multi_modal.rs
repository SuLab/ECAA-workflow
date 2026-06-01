//! Multi-modality helpers shared by the composer dispatch layer.

use std::collections::BTreeSet;

/// Deduplicate a modality slice while preserving the SME-supplied
/// ordering (used by the multi-modality dispatcher in `mod.rs` to drop
/// duplicates before single- vs multi-modality routing).
pub(super) fn unique_modalities<'a>(target_modalities: &'a [&'a str]) -> Vec<&'a str> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for modality in target_modalities {
        if seen.insert(*modality) {
            out.push(*modality);
        }
    }
    out
}
