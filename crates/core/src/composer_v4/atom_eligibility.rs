//! Atom-eligibility filter for v4 search.
//!
//! The v4 forward / backward search walks the entire `AtomRegistry`
//! looking for ports that type-unify with reachable producers /
//! desired-output goals. That's correct *typed* selection but it pulls
//! in atoms that can't actually execute under the current registry:
//!
//! 1. **Unresolvable `method_choice`** — atoms like
//!    `differential_transcript_usage` declare
//!    `method_choice.deferred_to: discover_dtu_method`, but the
//!    referenced discovery atom doesn't exist in the registry. Including
//!    such an atom in a composition surfaces `MethodChoiceUnresolved`
//!    from `validate_composition`. The fix is to filter at search time
//!    so the assembly pass never gets to consider them.
//! 2. **Wide-spectrum agentic / benchmark atoms** — `bio_mystery_query`,
//!    `compbio_query`, `sciagent_solution`, `hle_bio_query`,
//!    `lab_bench_query`, `biomni_eval1_query`, and the clinical-trial
//!    `endpoint_analysis` produce `data:0951`-parented score / endpoint
//!    outputs that type-unify with bulk-rnaseq DE / time-series goals.
//!    They're modality-agnostic and should not appear in modality-routed
//!    scenarios — those are handled by the per-modality archetype
//!    pathway, not by the open-world search.
//!
//! The filter runs in both `forward_search` and `backward_search` so the
//! `meet_in_the_middle` assembly only sees eligible atoms. Determinism
//! is preserved because the filter is a pure predicate over the atom +
//! registry + intent (no I/O, no global state, no randomness).

use crate::atom::AtomDefinition;
use crate::atom_registry::AtomRegistry;
use crate::workflow_contracts::workflow_intent::WorkflowIntent;

/// Atoms that are wide-spectrum agentic / benchmark eval atoms not
/// scoped to any one modality. The list is hand-curated against
/// `config/stage-atoms/*.yaml` as of; new atoms with the
/// same shape (LLM eval scoring, benchmark wrappers) should be added
/// here when authored. A typed marker (`AtomRole::Agentic` or similar)
/// is the eventual cleanup; until then the explicit list keeps the
/// filter surface small and reviewable.
///
/// All entries below output `data:0951`-parented eval-score local
/// extensions that backward-search would otherwise match against any
/// `data:0951` goal — exactly the leak surfaced by the parity corpus.
pub(crate) const AGENTIC_BENCH_ATOM_IDS: &[&str] = &[
    "bio_mystery_query",
    "biomni_eval1_query",
    "compbio_query",
    "endpoint_analysis",
    "hle_bio_query",
    "lab_bench_query",
    "sciagent_solution",
];

/// Predicate: should the v4 search consider this atom as a candidate
/// producer / consumer? Returns `false` when:
///
/// 1. The atom declares `method_choice.deferred_to: <id>` and `<id>` is
///    not present in the registry. Including such an atom surfaces
///    `MethodChoiceUnresolved` from `validate_composition` post-search.
/// 2. The intent declares a modality AND the atom is in the agentic /
///    benchmark allowlist. The archetype seed handles modality-routed
///    selection separately; the open-world search shouldn't pull in
///    agentic atoms when the SME's intent is bulk-rnaseq DE etc.
///
/// Determinism: pure function of `(atom, atom_reg, intent)`. No global
/// state, no I/O, no random tie-break.
pub(crate) fn is_v4_eligible(
    atom: &AtomDefinition,
    atom_reg: &AtomRegistry,
    intent: &WorkflowIntent,
) -> bool {
    // Filter 1 — unresolvable method_choice. Universal (modality-
    // independent) because validate_composition rejects compositions
    // with unresolvable method_choice regardless of intent.
    if let Some(mc) = &atom.method_choice {
        if atom_reg.get(&mc.deferred_to).is_none() {
            return false;
        }
    }
    // Filter 2 — agentic / benchmark atoms. Modality-gated: if the
    // intent doesn't specify a modality, the search is open-world and
    // agentic atoms are legitimate candidates. When modality is set,
    // the archetype pathway handles modality-routed selection; the
    // search shouldn't compete with a hand-authored benchmark atom.
    if intent.modality.is_some() && AGENTIC_BENCH_ATOM_IDS.contains(&atom.id.as_str()) {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::{AtomAssignee, AtomRole, MethodChoiceRef};
    use std::collections::BTreeMap;

    fn minimal_atom(id: &str) -> AtomDefinition {
        AtomDefinition {
            id: id.into(),
            version: "1.0.0".into(),
            role: AtomRole::Operation,
            discovery_kind: None,
            description: "test atom".into(),
            edam_operation: "operation:0004".into(),
            edam_data: None,
            edam_format: None,
            assignee: AtomAssignee::Agent,
            depends_on: vec![],
            excludes: vec![],
            attributes: BTreeMap::new(),
            joint_with: vec![],
            inputs: vec![],
            outputs: vec![],
            method_choice: None,
            resource_profile: None,
            preferred_container: None,
            claim_boundary: None,
            iterate: None,
            condition: None,
            required_figures: vec![],
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: vec![],
            required_artifacts: vec![],
            validators: vec![],
            runtime_packages: Default::default(),
            safety: Default::default(),
        }
    }

    #[test]
    fn atom_with_no_method_choice_passes() {
        let atom_reg = AtomRegistry::default();
        let intent = WorkflowIntent::default();
        assert!(is_v4_eligible(&minimal_atom("clean"), &atom_reg, &intent));
    }

    #[test]
    fn atom_with_unresolvable_method_choice_filtered() {
        let atom_reg = AtomRegistry::default(); // empty — no discovery atom
        let intent = WorkflowIntent::default();
        let mut atom = minimal_atom("uses_missing");
        atom.method_choice = Some(MethodChoiceRef {
            deferred_to: "discover_missing_method".into(),
        });
        assert!(!is_v4_eligible(&atom, &atom_reg, &intent));
    }

    #[test]
    fn agentic_atom_filtered_when_modality_set() {
        let atom_reg = AtomRegistry::default();
        let intent = WorkflowIntent {
            modality: Some("bulk_rnaseq".into()),
            ..Default::default()
        };
        let atom = minimal_atom("bio_mystery_query");
        assert!(!is_v4_eligible(&atom, &atom_reg, &intent));
    }

    #[test]
    fn agentic_atom_allowed_when_modality_unset() {
        let atom_reg = AtomRegistry::default();
        let intent = WorkflowIntent {
            modality: None,
            ..Default::default()
        };
        let atom = minimal_atom("bio_mystery_query");
        assert!(is_v4_eligible(&atom, &atom_reg, &intent));
    }

    #[test]
    fn non_agentic_atom_passes_with_modality_set() {
        let atom_reg = AtomRegistry::default();
        let intent = WorkflowIntent {
            modality: Some("bulk_rnaseq".into()),
            ..Default::default()
        };
        let atom = minimal_atom("differential_expression");
        assert!(is_v4_eligible(&atom, &atom_reg, &intent));
    }
}
