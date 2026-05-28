//! Generic multi-modal DAG synthesis.
//!
//! Used by `compose_with_version_and_modalities` when the SME requests
//! ≥2 modalities AND no cross-omics archetype's
//! `cross_omics_modalities` set-equals the request. The fallback path
//! composes the best single-modality archetype for each requested
//! modality, prefixes every branch's stage ids, joins them through a
//! shared `multi_modal_thematic_comparison` atom and a project-level
//! `multi_modal_final_reporting` atom, and runs §S7.4 validation.

use super::validation::validate_composition;
use super::{
    aggregate_resources, apply_atom_ref_overrides, inheritance::resolve_inheritance,
    resolve_task_container, AtomSelectionRationale, ComposedAtom, CompositionError,
    CompositionResult, SelectionReason,
};
use crate::archetype::ArchetypeAtomRef;
use crate::archetype_registry::ArchetypeRegistry;
use crate::atom::AtomDefinition;
use crate::atom_registry::AtomRegistry;
use crate::goal_spec::GoalSpec;
use crate::ids::StageId;
use std::collections::{BTreeMap, BTreeSet};

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

/// Compose a generic multi-branch DAG over the requested modalities.
/// Each branch is built from the best single-modality archetype, with
/// stage ids prefixed by a per-modality namespace; the branches are
/// joined through shared comparison + final-reporting atoms.
pub(super) fn synthesize_generic_multi_modal_composition(
    goal: &GoalSpec,
    project_class: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
    modalities: &[&str],
) -> Result<CompositionResult, CompositionError> {
    let target_kind = goal.modifiers.get("kind").map(|s| s.as_str());
    let mut composed: Vec<ComposedAtom> = Vec::new();
    let mut atom_rationales: BTreeMap<String, AtomSelectionRationale> = BTreeMap::new();
    let mut branch_terminals: Vec<String> = Vec::new();
    let mut branch_summaries: Vec<String> = Vec::new();
    let mut branch_scores: Vec<u32> = Vec::new();
    let mut used_prefixes = BTreeSet::new();

    for modality in modalities {
        let matches = archetype_reg.find_match_with_modality_and_kind(
            &goal.edam_data,
            goal.edam_format.as_deref(),
            project_class,
            Some(modality),
            target_kind,
        );
        let Some((archetype, score)) = matches
            .iter()
            .find(|(a, _)| a.modality_hint.as_deref() == Some(*modality))
            .copied()
        else {
            return Err(CompositionError::NoArchetypeMatch {
                target_data: goal.edam_data.clone(),
                target_format: goal.edam_format.clone(),
                target_class: format!("{project_class}/{modality}"),
            });
        };

        let prefix = modality_prefix(modality, &mut used_prefixes);
        let flat = resolve_inheritance(archetype, archetype_reg)?;
        let mut stage_rewrite: BTreeMap<String, String> = BTreeMap::new();
        for aref in &flat.atoms {
            let original_stage_id = aref.alias.as_deref().unwrap_or(aref.atom_id.as_str());
            stage_rewrite.insert(
                original_stage_id.to_string(),
                format!("{prefix}{original_stage_id}"),
            );
        }

        let mut branch_terminal: Option<String> = None;
        for aref in &flat.atoms {
            let original_stage_id = aref.alias.as_deref().unwrap_or(aref.atom_id.as_str());
            let stage_id = stage_rewrite
                .get(original_stage_id)
                .cloned()
                .unwrap_or_else(|| format!("{prefix}{original_stage_id}"));
            let atom = atom_reg.get(aref.atom_id.as_str()).ok_or_else(|| {
                CompositionError::UnknownAtom {
                    archetype_id: archetype.id.clone(),
                    atom_id: aref.atom_id.clone(),
                }
            })?;
            let depends_on = generic_branch_depends_on(atom, aref, &stage_rewrite);
            let container = resolve_task_container(atom, None, None);
            let composed_atom = ComposedAtom {
                stage_id: stage_id.clone().into(),
                atom: apply_atom_ref_overrides(atom, aref),
                depends_on,
                required: aref.required,
                bindings: Vec::new(),
                container,
            };
            atom_rationales.insert(
                stage_id.clone(),
                AtomSelectionRationale {
                    stage_id: StageId::from(stage_id.as_str()),
                    atom_id: atom.id.as_str().into(),
                    reason: SelectionReason::ArchetypeRequired,
                    score,
                    explanation: format!(
                        "Generic multi-branch composition selected archetype {} for modality {}.",
                        archetype.id, modality
                    ),
                },
            );
            branch_terminal = Some(stage_id);
            composed.push(composed_atom);
        }

        if let Some(terminal) = branch_terminal {
            branch_terminals.push(terminal);
        }
        branch_summaries.push(format!("{modality}: {}", archetype.id));
        branch_scores.push(score);
    }

    append_generic_join_atom(
        atom_reg,
        &mut composed,
        &mut atom_rationales,
        "reporting",
        "multi_modal_thematic_comparison",
        branch_terminals.clone(),
        "Generic multi-branch composition inserted a shared comparison report.",
    )?;
    append_generic_join_atom(
        atom_reg,
        &mut composed,
        &mut atom_rationales,
        "final_reporting",
        "multi_modal_final_reporting",
        vec!["multi_modal_thematic_comparison".to_string()],
        "Generic multi-branch composition inserted the project-level final report.",
    )?;

    let resource_estimate = aggregate_resources(&composed);
    let match_score = branch_scores.into_iter().min().unwrap_or(0);
    let result = CompositionResult {
        matched_archetype: Some("cross_omics_generic_multi_modal".to_string()),
        match_score,
        atoms: composed,
        goal: goal.clone(),
        rationale: format!(
            "Composed a generic multi-branch DAG across requested modalities: {}.",
            branch_summaries.join(", ")
        ),
        atom_rationales,
        resource_estimate,
    };
    validate_composition(&result, atom_reg)?;
    Ok(result)
}

fn generic_branch_depends_on(
    atom: &AtomDefinition,
    aref: &ArchetypeAtomRef,
    stage_rewrite: &BTreeMap<String, String>,
) -> Vec<String> {
    let deps = if aref.depends_on.is_empty() {
        &atom.depends_on
    } else {
        &aref.depends_on
    };
    deps.iter()
        .map(|dep| {
            stage_rewrite
                .get(dep)
                .cloned()
                .unwrap_or_else(|| dep.clone())
        })
        .collect()
}

fn append_generic_join_atom(
    atom_reg: &AtomRegistry,
    composed: &mut Vec<ComposedAtom>,
    atom_rationales: &mut BTreeMap<String, AtomSelectionRationale>,
    atom_id: &str,
    stage_id: &str,
    depends_on: Vec<String>,
    explanation: &str,
) -> Result<(), CompositionError> {
    let atom = atom_reg
        .get(atom_id)
        .ok_or_else(|| CompositionError::UnknownAtom {
            archetype_id: "cross_omics_generic_multi_modal".to_string(),
            atom_id: atom_id.into(),
        })?;
    let container = resolve_task_container(atom, None, None);
    composed.push(ComposedAtom {
        stage_id: stage_id.into(),
        atom: atom.clone(),
        depends_on,
        required: true,
        bindings: Vec::new(),
        container,
    });
    atom_rationales.insert(
        stage_id.to_string(),
        AtomSelectionRationale {
            stage_id: StageId::from(stage_id),
            atom_id: atom.id.as_str().into(),
            reason: SelectionReason::ArchetypeRequired,
            score: 0,
            explanation: explanation.to_string(),
        },
    );
    Ok(())
}

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
