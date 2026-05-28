//! Stage-id → SME-readable label translation.
//!
//! Internal stage identifiers follow a `discover_*` / `validate_*` /
//! `select_*` prefix convention useful for the builder and the agent, but
//! exposing those raw strings to an SME violates the "no internal vocabulary
//! in user-facing text" rule from `prompt_role.txt`. This helper strips the
//! prefix and humanizes the remainder.
//!
//! The TypeScript mirror in `ui/src/lib/stageLabels.ts` must stay
//! behaviorally identical; cross-language equivalence is asserted via the
//! shared examples in the unit tests below (and in the `sme-copy-linter`
//! on the UI side).
//!
//! No taxonomy lookup today — a later revision can accept an optional
//! `&Taxonomy` and prefer an explicit `label:` field when present; for now
//! the stateless prefix-stripping rule covers every stage ID that surfaces
//! in SME-visible text.

/// Translate an internal stage identifier into a plain-English label.
///
/// Examples:
/// - `discover_normalization` → `"Normalization"`
/// - `validate_qc` → `"Qc"` (capitalization is naive on purpose; callers
///   can pass a taxonomy label through when they have one)
/// - `discover_batch_correction` → `"Batch correction"`
/// - `alignment` → `"Alignment"` (no prefix)
/// - `""` → `""`
pub fn stage_id_to_human_label(stage_id: &str) -> String {
    let base = strip_known_prefix(stage_id);
    let spaced = base.replace('_', " ");
    capitalize_first(&spaced)
}

fn strip_known_prefix(stage_id: &str) -> &str {
    for prefix in &["discover_", "validate_", "select_"] {
        if let Some(rest) = stage_id.strip_prefix(prefix) {
            return rest;
        }
    }
    stage_id
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_discover_prefix() {
        assert_eq!(
            stage_id_to_human_label("discover_normalization"),
            "Normalization"
        );
    }

    #[test]
    fn strips_validate_prefix() {
        assert_eq!(stage_id_to_human_label("validate_qc"), "Qc");
    }

    #[test]
    fn strips_select_prefix() {
        assert_eq!(stage_id_to_human_label("select_aligner"), "Aligner");
    }

    #[test]
    fn humanizes_underscore_separated_base() {
        assert_eq!(
            stage_id_to_human_label("discover_batch_correction"),
            "Batch correction"
        );
    }

    #[test]
    fn passes_through_when_no_known_prefix() {
        assert_eq!(stage_id_to_human_label("alignment"), "Alignment");
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(stage_id_to_human_label(""), "");
    }

    #[test]
    fn does_not_strip_unknown_prefix() {
        // Not a `discover_` / `validate_` / `select_` prefix — leave the
        // original identifier intact (including underscore → space for the
        // first letter + capitalization).
        assert_eq!(stage_id_to_human_label("emit_package"), "Emit package");
    }

    #[test]
    fn prefix_only_input_yields_empty_capitalized() {
        assert_eq!(stage_id_to_human_label("discover_"), "");
    }
}
