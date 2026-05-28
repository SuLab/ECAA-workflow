//! Closed-enum slot manifest for slot-filling over base archetypes.
//!
//! A slot manifest declares ONE closed-enum slot (e.g., `integrator` ∈
//! {diablo, mofa, snf, generic}; `protocol` ∈ {multiome_arc, share_seq,
//! perturb_seq, generic}). Each slot value carries (a) keyword tokens
//! the classifier scans for to pick the slot, and (b) extra atoms that
//! get appended to the base archetype's atom list when the slot is
//! chosen. Base archetype's atoms are always included; the slot only
//! ADDS atoms.

use crate::archetype::ArchetypeAtomRef;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
/// SlotManifest data.
pub struct SlotManifest {
    /// Slot name, e.g. "integrator", "protocol".
    pub slot_name: String,
    /// Currently only `closed_enum` supported. Future: `open_text`.
    pub slot_kind: String,
    /// Fallback value when no value's keywords match.
    pub default: String,
    /// Values.
    pub values: Vec<SlotValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
/// SlotValue data.
pub struct SlotValue {
    /// Slot value id, must appear in archetype id (e.g. `diablo` for
    /// `cross_omics_rnaseq_proteomics_diablo` family).
    pub id: String,
    /// Tokens the classifier scans for (case-insensitive, substring).
    /// Empty for the `default` value — it wins when nothing else does.
    pub keywords: Vec<String>,
    /// Atoms appended to the base archetype when this slot is chosen.
    /// Ordering preserved; depends_on must reference base atoms or
    /// earlier slot-fill atoms.
    pub extra_atoms: Vec<ArchetypeAtomRef>,
}

use crate::classify::normalize_for_match;

/// Pick the slot value whose `keywords` match the prose. First match
/// wins (declaration order in YAML). Returns the slot manifest's
/// `default` when nothing matches.
///
/// The matcher accepts THREE forms in priority order:
/// 1. **Direct id match** — when the caller has already canonicalized
///    the slot value (e.g., `rebuild_dag` injects
///    `goal.modifiers["integrator"] = "snf"` and the planner passes
///    that value as `prose`), the slot id wins immediately. Without
///    this branch, "snf" wouldn't substring-match "snf integration"
///    or "similarity network fusion" and the resolver would return
///    `default`.
/// 2. **Keyword substring match** — normalize prose + each keyword
///    and check `normalized_prose.contains(needle)`. Original
///    intake-text resolution path.
/// 3. **Default** — when neither matches, return the manifest's
///    declared default value.
pub fn resolve_slot_value(manifest: &SlotManifest, prose: &str) -> String {
    let normalized = normalize_for_match(prose);
    // Priority 1: direct id match for caller-canonicalized inputs.
    for v in &manifest.values {
        let id_norm = normalize_for_match(&v.id);
        if !id_norm.is_empty() && normalized == id_norm {
            return v.id.clone();
        }
    }
    // Priority 2: keyword substring scan.
    for v in &manifest.values {
        for kw in &v.keywords {
            let needle = normalize_for_match(kw);
            if !needle.is_empty() && normalized.contains(&needle) {
                return v.id.clone();
            }
        }
    }
    // Priority 3: manifest default.
    manifest.default.clone()
}

/// Expand a base archetype's atoms by appending the chosen slot
/// value's extra_atoms. Deduplication is the caller's responsibility
/// — slots should not introduce alias clashes with base atoms.
pub fn expand_atoms(
    base_atoms: &[ArchetypeAtomRef],
    manifest: &SlotManifest,
    slot_value_id: &str,
) -> Vec<ArchetypeAtomRef> {
    let mut out: Vec<ArchetypeAtomRef> = base_atoms.to_vec();
    if let Some(v) = manifest.values.iter().find(|v| v.id == slot_value_id) {
        out.extend(v.extra_atoms.iter().cloned());
    }
    out
}
