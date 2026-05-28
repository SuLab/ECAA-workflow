//! Backward-chain composer fallback.
//!
//! When no archetype matches the goal, plan from the goal's
//! `(edam_data, edam_format)` tuple downward: find producer atoms,
//! recurse on each producer's `depends_on`, build the chain bottom-up.
//! BETSY-style pruning (Hoffman et al. 2017) — when multiple
//! candidates produce the same data class, prefer the one with the
//! shorter dependency chain; tie-break alphabetical by atom id.
//!
//! Recursion depth cap: 10 layers. Every taxonomy in the catalog has
//! chain depth 4–6, so 10 is comfortable headroom while still
//! preventing pathological cycles in malformed registries.

use super::{
    aggregate_resources, resolve_task_container, AtomSelectionRationale, ComposedAtom,
    CompositionError, CompositionResult, SelectionReason,
};
use crate::atom::AtomDefinition;
use crate::atom_registry::AtomRegistry;
use crate::edam::is_subtype_of;
use crate::goal_spec::GoalSpec;
use std::collections::{BTreeMap, BTreeSet};

/// Backward-chain composer fallback.
pub(super) fn backward_chain_compose(
    goal: &GoalSpec,
    atom_reg: &AtomRegistry,
    project_class: &str,
) -> Result<CompositionResult, CompositionError> {
    const MAX_DEPTH: usize = 10;

    // Find producer atoms whose output exactly matches the goal's
    // (edam_data, edam_format) tuple.
    let goal_producers: Vec<&AtomDefinition> = atom_reg
        .find_producers(&goal.edam_data, goal.edam_format.as_deref())
        .collect();

    if goal_producers.is_empty() {
        // No EDAM-exact match — fall back to subtype matching: any
        // producer whose edam_data is a subtype of the goal's
        // edam_data counts.
        let subtype_producers: Vec<&AtomDefinition> = atom_reg
            .iter()
            .map(|(_, a)| a)
            .filter(|a| {
                a.edam_data
                    .as_deref()
                    .map(|d| is_subtype_of(d, &goal.edam_data))
                    .unwrap_or(false)
            })
            .collect();
        if subtype_producers.is_empty() {
            return Err(CompositionError::NoArchetypeMatch {
                target_data: goal.edam_data.clone(),
                target_format: goal.edam_format.clone(),
                target_class: project_class.to_string(),
            });
        }
        return assemble_chain(&subtype_producers, atom_reg, goal, MAX_DEPTH);
    }

    assemble_chain(&goal_producers, atom_reg, goal, MAX_DEPTH)
}

/// Pick the shortest-chain producer (BETSY pruning) and assemble
/// the bottom-up chain.
fn assemble_chain(
    candidates: &[&AtomDefinition],
    atom_reg: &AtomRegistry,
    goal: &GoalSpec,
    max_depth: usize,
) -> Result<CompositionResult, CompositionError> {
    // Score every candidate by transitive chain length; tie-break
    // alphabetical by atom id.
    let mut scored: Vec<(usize, &AtomDefinition)> = candidates
        .iter()
        .map(|a| {
            (
                estimate_chain_length(a, atom_reg, max_depth, &mut BTreeSet::new()),
                *a,
            )
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.id.cmp(&b.1.id)));
    let top = scored[0].1;

    // Walk dependencies bottom-up. Each visited atom enters the
    // composition exactly once; the chain order is topological.
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut order: Vec<String> = Vec::new();
    walk_dependencies(top, atom_reg, &mut visited, &mut order, max_depth, 0)?;

    let composed: Vec<ComposedAtom> = order
        .iter()
        .map(|id| {
            let atom = atom_reg
                .get(id)
                .expect("walked atom id missing from registry");
            // Backward-chain has no archetype context, so
            // resolution falls through to atom > host (None).
            let container = resolve_task_container(atom, None, None);
            ComposedAtom {
                stage_id: id.clone().into(),
                atom: atom.clone(),
                depends_on: atom.depends_on.clone(),
                required: true,
                bindings: Vec::new(),
                container,
            }
        })
        .collect();

    let resource_estimate = aggregate_resources(&composed);
    // Backward-chain rationales. Each atom carries
    // the goal type it produced (or contributed to) plus its step
    // index in the chain.
    let atom_rationales: BTreeMap<String, AtomSelectionRationale> = composed
        .iter()
        .enumerate()
        .map(|(step, c)| {
            (
                c.stage_id.to_string(),
                AtomSelectionRationale {
                    stage_id: c.stage_id.clone(),
                    atom_id: c.atom.id.as_str().into(),
                    reason: SelectionReason::BackwardChainGoalProducer {
                        target_edam: goal.edam_data.clone(),
                        step: step as u32,
                    },
                    score: 0,
                    explanation: format!(
                        "Backward-chain step {} pulled in {} to produce {} for the goal.",
                        step, c.atom.id, goal.edam_data
                    ),
                },
            )
        })
        .collect();
    Ok(CompositionResult {
        matched_archetype: None,
        match_score: 0,
        atoms: composed,
        goal: goal.clone(),
        rationale: format!(
            "Backward-chain composed from goal data={} via producer {}",
            goal.edam_data, top.id
        ),
        atom_rationales,
        resource_estimate,
    })
}

/// Estimate transitive dependency count for BETSY pruning. Uses a
/// visited-set so cycles in the registry don't blow the stack.
fn estimate_chain_length(
    atom: &AtomDefinition,
    atom_reg: &AtomRegistry,
    max_depth: usize,
    visited: &mut BTreeSet<String>,
) -> usize {
    if max_depth == 0 || !visited.insert(atom.id.clone()) {
        return 0;
    }
    let mut count = 1;
    for dep_id in &atom.depends_on {
        if let Some(dep) = atom_reg.get(dep_id) {
            count += estimate_chain_length(dep, atom_reg, max_depth - 1, visited);
        }
    }
    count
}

/// Recursively walk an atom's `depends_on` chain. Visits each atom
/// once, in topological order. Returns `CycleDetected` when the
/// recursion exceeds `max_depth`.
fn walk_dependencies(
    atom: &AtomDefinition,
    atom_reg: &AtomRegistry,
    visited: &mut BTreeSet<String>,
    order: &mut Vec<String>,
    max_depth: usize,
    depth: usize,
) -> Result<(), CompositionError> {
    if depth > max_depth {
        return Err(CompositionError::CycleDetected {
            cycle: vec![atom.id.clone()],
        });
    }
    if visited.contains(&atom.id) {
        return Ok(());
    }
    visited.insert(atom.id.clone());
    for dep_id in &atom.depends_on {
        if let Some(dep) = atom_reg.get(dep_id) {
            walk_dependencies(dep, atom_reg, visited, order, max_depth, depth + 1)?;
        }
        // Unknown deps are intake-supplied inputs; the input-
        // satisfiability validator handles them.
    }
    order.push(atom.id.clone());
    Ok(())
}
