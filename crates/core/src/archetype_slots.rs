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
    // Priority 2: keyword substring scan, skipping matches that fall
    // inside a negated list of this slot's own values (e.g. "No DIABLO /
    // MOFA / SNF requested" must NOT select an integrator).
    let vocab = slot_vocabulary(manifest);
    for v in &manifest.values {
        for kw in &v.keywords {
            let needle = normalize_for_match(kw);
            if needle.is_empty() {
                continue;
            }
            if let Some(pos) = normalized.find(&needle) {
                if !is_list_negated(&normalized[..pos], &vocab) {
                    return v.id.clone();
                }
            }
        }
    }
    // Priority 3: manifest default.
    manifest.default.clone()
}

/// Normalized word tokens that belong to THIS slot's own vocabulary —
/// the union of every value's keyword words plus every value id. Used to
/// decide whether a negation cue governs a *list of slot values* (e.g.
/// "no diablo / mofa / snf") rather than some unrelated phrase.
fn slot_vocabulary(manifest: &SlotManifest) -> std::collections::BTreeSet<String> {
    let mut set = std::collections::BTreeSet::new();
    for v in &manifest.values {
        for w in normalize_for_match(&v.id).split_whitespace() {
            set.insert(w.to_string());
        }
        for kw in &v.keywords {
            for w in normalize_for_match(kw).split_whitespace() {
                set.insert(w.to_string());
            }
        }
    }
    set
}

/// Build a negation-scope vocabulary from a flat list of phrase tokens
/// (each split into normalized words). For reuse by other naive
/// keyword-scan sites that share the same negation hazard as the slot
/// resolver (e.g. the integrator-token scans in classify / dispatch).
pub fn vocabulary_from_tokens<'a>(
    tokens: impl IntoIterator<Item = &'a str>,
) -> std::collections::BTreeSet<String> {
    let mut set = std::collections::BTreeSet::new();
    for t in tokens {
        for w in normalize_for_match(t).split_whitespace() {
            set.insert(w.to_string());
        }
    }
    set
}

/// True when the slot-value keyword that ends `preceding` is governed by
/// a negation cue reached through nothing but other slot-vocabulary words
/// or list separators. This catches "no diablo / mofa / snf" (negation
/// reaches every integrator token through `/` separators) without
/// false-positiving on "no clean class labels … MOFA" (the word "labels"
/// is not slot vocabulary, so the scan stops before reaching "no").
pub fn is_list_negated(preceding: &str, vocab: &std::collections::BTreeSet<String>) -> bool {
    const NEGATIONS: &[&str] = &[
        "no",
        "not",
        "without",
        "excluding",
        "exclude",
        "neither",
        "none",
        "skip",
        "avoid",
    ];
    const SEPARATORS: &[&str] = &["and", "or", "nor", "plus", "the", "any"];
    let is_sep = |t: &str| SEPARATORS.contains(&t) || !t.chars().any(|c| c.is_alphanumeric());
    // Closest-first, bounded window so a far-away "no" never reaches.
    for t in preceding.split_whitespace().rev().take(8) {
        if NEGATIONS.contains(&t) {
            return true;
        }
        if vocab.contains(t) || is_sep(t) {
            continue;
        }
        // A content word outside the slot vocabulary closes the scope.
        return false;
    }
    false
}

#[cfg(test)]
mod negation_tests {
    use super::*;

    fn integrator_manifest() -> SlotManifest {
        let mk = |id: &str, kws: &[&str]| SlotValue {
            id: id.into(),
            keywords: kws.iter().map(|s| s.to_string()).collect(),
            extra_atoms: vec![],
        };
        SlotManifest {
            slot_name: "integrator".into(),
            slot_kind: "closed_enum".into(),
            default: "generic".into(),
            values: vec![
                mk(
                    "diablo",
                    &["diablo", "spls-da", "sparse pls-da", "mixomics"],
                ),
                mk("mofa", &["mofa", "factor analysis", "multi-omics factor"]),
                mk("snf", &["snf integration", "similarity network fusion"]),
                mk("generic", &[]),
            ],
        }
    }

    #[test]
    fn negated_integrator_list_falls_to_default() {
        let m = integrator_manifest();
        // "No DIABLO / MOFA / SNF requested" must NOT select an integrator.
        let p = "we want each modality run independently then a thematic \
                 comparison. no diablo / mofa / snf requested.";
        assert_eq!(resolve_slot_value(&m, p), "generic");
    }

    #[test]
    fn positive_integrator_still_selected() {
        let m = integrator_manifest();
        assert_eq!(
            resolve_slot_value(&m, "integrate the two omics with diablo (sparse pls-da)."),
            "diablo"
        );
    }

    #[test]
    fn unrelated_negation_does_not_suppress_mofa() {
        let m = integrator_manifest();
        // "no clean class labels" must NOT suppress a later MOFA mention:
        // "labels" is not slot vocabulary, so the negation scope closes
        // before reaching MOFA.
        let p = "200 patients with no clean class labels, so an unsupervised \
                 multi-omics factor analysis (mofa-style) integrator.";
        assert_eq!(resolve_slot_value(&m, p), "mofa");
    }

    #[test]
    fn without_negation_form() {
        let m = integrator_manifest();
        assert_eq!(
            resolve_slot_value(&m, "per-branch outputs without diablo or mofa or snf."),
            "generic"
        );
    }
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
