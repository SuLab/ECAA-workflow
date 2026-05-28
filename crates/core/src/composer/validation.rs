//! Six-item formal validation pass.
//!
//! Runs over a `CompositionResult` independently of which composer
//! path produced it (archetype fast-path, backward-chain fallback, or
//! v4 proof-carrying planner). The checks are:
//!
//! 1. Exclusion consistency — no atom's `excludes` set intersects with
//!    the composed atom set.
//! 2. Acyclicity — Kahn topological sort over `depends_on`.
//! 3. Goal reachability — at least one atom's `edam_data`/`edam_format`
//!    or its output ports' semantic+physical type matches the goal.
//! 4. Input satisfiability — every `depends_on` resolves within the
//!    composition or to an intake-supplied input.
//! 5. Attribute resolution — `method_choice.deferred_to` references a
//!    Discovery atom in the composition.
//! 6. Gate well-formedness — `excludes:` entries reference real atoms.
//!
//! 7. Multi-modal `joint_with` constraints (lhs and rhs must share
//!    the same `source_atom` attribute).

use super::{ComposedAtom, CompositionError, CompositionResult};
use crate::atom::AtomRole;
use crate::atom_registry::AtomRegistry;
use crate::edam::is_subtype_of;
use crate::goal_spec::GoalSpec;
use std::collections::{BTreeMap, BTreeSet};

/// Six-item formal validation. Runs over a
/// `CompositionResult` independently of which path produced it.
pub(super) fn validate_composition(
    result: &CompositionResult,
    atom_reg: &AtomRegistry,
) -> Result<(), CompositionError> {
    let composed_ids: BTreeSet<&str> = result.atoms.iter().map(|c| c.stage_id.as_str()).collect();

    // 1. Exclusion consistency.
    for c in &result.atoms {
        for excl in &c.atom.excludes {
            if composed_ids.contains(excl.as_str()) {
                let archetype_id = result
                    .matched_archetype
                    .clone()
                    .unwrap_or_else(|| "<backward-chain>".to_string());
                return Err(CompositionError::ExclusionConflict {
                    archetype_id,
                    atom_a: c.stage_id.to_string(),
                    atom_b: excl.clone(),
                });
            }
        }
    }

    // 2. Acyclicity.
    if let Some(cycle) = detect_cycle(&result.atoms, &composed_ids) {
        return Err(CompositionError::CycleDetected { cycle });
    }

    // 3. Goal reachability.
    //
    // Wildcard goal: empty edam_data OR the legacy `data:9999`
    // placeholder (defense-in-depth for sessions persisted before
    // the data:9999 placeholder was retired). Treat both as "no
    // constraint" — modality archetype default already produced
    // the right atom set, and the v4 composer must not crash
    // `GoalUnreachable` on a wildcard goal.
    if result.goal.edam_data.is_empty() || result.goal.edam_data == "data:9999" {
        return Ok(());
    }

    // An atom satisfies the goal when EITHER:
    // (a) its legacy top-level `edam_data` / `edam_format` matches
    // the goal — the v2 archetype-path convention where the
    // atom's "primary tag" is its driving data type. Many of
    // today's atoms encode the *input* type at the top level
    // and the *output* format at the top-level format (see
    // `variant_calling.yaml` / `quantification.yaml`), so this
    // branch matches by convention rather than by output port.
    // (b) any of the atom's `outputs[*].semantic_type` IRIs match
    // the goal IRI directly (or via curated subtype edges) AND
    // the matching port's `physical_format.iri` matches the
    // goal format (when set). This is the v4 port-typed
    // convention: the goal is what an SME asks for as output,
    // so the goal-reachability check verifies that some atom's
    // OUTPUT port produces it.
    //
    // Branch (b) handles the chip-seq + atac-seq peak-calling case
    // where the v4-aligned goal `data:1255 / format:3003` (Feature
    // record / BED) doesn't match `peak_calling`'s top-level
    // `edam_data: data:0863` (BAM, the input type). The output port
    // (`data:1255 / format:3003`) is the right thing to match.
    use crate::workflow_contracts::semantic_type::SemanticType;
    let goal_data = result.goal.edam_data.as_str();
    let goal_format = result.goal.edam_format.as_deref();
    let output_port_matches_goal = |c: &ComposedAtom| -> bool {
        c.atom.outputs.iter().any(|p| {
            let data_ok = match &p.semantic_type {
                SemanticType::OntologyTerm { iri, .. } => {
                    iri == goal_data || is_subtype_of(iri, goal_data)
                }
                SemanticType::LocalExtension {
                    proposed_parent_terms,
                    ..
                } => proposed_parent_terms
                    .iter()
                    .any(|parent| parent == goal_data || is_subtype_of(parent, goal_data)),
                SemanticType::Opaque { .. } => false,
                // Union output ports match when any member IRI matches the goal.
                SemanticType::Union { members } => members.iter().any(|m| match m {
                    SemanticType::OntologyTerm { iri, .. } => {
                        iri == goal_data || is_subtype_of(iri, goal_data)
                    }
                    SemanticType::LocalExtension {
                        proposed_parent_terms,
                        ..
                    } => proposed_parent_terms
                        .iter()
                        .any(|parent| parent == goal_data || is_subtype_of(parent, goal_data)),
                    _ => false,
                }),
            };
            let format_ok = match (
                goal_format,
                p.physical_format.as_ref().map(|f| f.iri.as_str()),
            ) {
                (None, _) => true,
                (Some(want), Some(got)) => want == got,
                (Some(_), None) => false,
            };
            data_ok && format_ok
        })
    };
    let any_reaches = result.atoms.iter().any(|c| {
        let data_ok = c
            .atom
            .edam_data
            .as_deref()
            .map(|d| d == goal_data || is_subtype_of(d, goal_data))
            .unwrap_or(false);
        let format_ok = match (goal_format, c.atom.edam_format.as_deref()) {
            (None, _) => true,
            (Some(want), Some(got)) => want == got,
            (Some(_), None) => false,
        };
        (data_ok && format_ok) || output_port_matches_goal(c)
    });
    if !any_reaches {
        return Err(CompositionError::GoalUnreachable {
            goal: format_goal(&result.goal),
        });
    }

    // 4. Input satisfiability.
    for c in &result.atoms {
        for dep in &c.depends_on {
            if composed_ids.contains(dep.as_str()) {
                continue;
            }
            if result.atoms.iter().any(|x| x.atom.id == *dep) {
                continue;
            }
            if atom_reg.get(dep).is_some() {
                return Err(CompositionError::InputUnsatisfied {
                    atom: c.stage_id.to_string(),
                    missing: dep.clone(),
                });
            }
        }
    }

    // 5. Attribute resolution.
    for c in &result.atoms {
        if let Some(mc) = &c.atom.method_choice {
            let target = result
                .atoms
                .iter()
                .find(|x| x.stage_id == mc.deferred_to || x.atom.id == mc.deferred_to);
            let resolved = target
                .map(|t| matches!(t.atom.role, AtomRole::Discovery))
                .unwrap_or(false);
            if !resolved {
                return Err(CompositionError::MethodChoiceUnresolved {
                    atom: c.stage_id.to_string(),
                    deferred_to: mc.deferred_to.clone(),
                });
            }
        }
    }

    // 6. Gate well-formedness.
    for c in &result.atoms {
        for excl in &c.atom.excludes {
            if atom_reg.get(excl).is_none() {
                return Err(CompositionError::MalformedExclusion {
                    atom: c.stage_id.to_string(),
                    excluded: excl.clone(),
                });
            }
        }
    }

    // 7. multi-modal joint-source constraints. For
    // Each atom that declared `joint_with: [{lhs, rhs},...]`, the
    // composed producers of `lhs` and `rhs` must share the same
    // `attributes.source_atom` value. Missing attributes on either
    // side are treated as None — diverging None from a concrete
    // value is a mismatch (the constraint is "joint", which
    // requires both to declare a source).
    for c in &result.atoms {
        for joint in &c.atom.joint_with {
            let lhs_source = result
                .atoms
                .iter()
                .find(|x| x.atom.id == joint.lhs)
                .and_then(|x| x.atom.attributes.get("source_atom"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let rhs_source = result
                .atoms
                .iter()
                .find(|x| x.atom.id == joint.rhs)
                .and_then(|x| x.atom.attributes.get("source_atom"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if lhs_source != rhs_source || lhs_source.is_none() {
                return Err(CompositionError::JointSourceMismatch {
                    atom: c.stage_id.to_string(),
                    lhs: joint.lhs.clone(),
                    rhs: joint.rhs.clone(),
                    lhs_source,
                    rhs_source,
                });
            }
        }
    }

    Ok(())
}

/// Kahn's algorithm + cycle reconstruction.
pub(super) fn detect_cycle(
    atoms: &[ComposedAtom],
    composed_ids: &BTreeSet<&str>,
) -> Option<Vec<String>> {
    let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut indegree: BTreeMap<String, usize> = BTreeMap::new();
    for c in atoms {
        deps.entry(c.stage_id.to_string()).or_default();
        indegree.entry(c.stage_id.to_string()).or_insert(0);
    }
    for c in atoms {
        for d in &c.depends_on {
            let resolved = if composed_ids.contains(d.as_str()) {
                Some(d.clone())
            } else {
                atoms
                    .iter()
                    .find(|x| x.atom.id == *d)
                    .map(|x| x.stage_id.to_string())
            };
            if let Some(stage_id) = resolved {
                deps.get_mut(&stage_id)
                    .unwrap()
                    .push(c.stage_id.to_string());
                *indegree.entry(c.stage_id.to_string()).or_insert(0) += 1;
            }
        }
    }
    let mut queue: Vec<String> = indegree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(k, _)| k.clone())
        .collect();
    queue.sort();
    let mut popped: usize = 0;
    while let Some(node) = queue.pop() {
        popped += 1;
        if let Some(out) = deps.get(&node) {
            for next in out {
                let e = indegree.entry(next.clone()).or_insert(0);
                *e = e.saturating_sub(1);
                if *e == 0 {
                    queue.push(next.clone());
                    queue.sort();
                }
            }
        }
    }
    if popped == atoms.len() {
        None
    } else {
        let mut cycle: Vec<String> = indegree
            .iter()
            .filter(|(_, &d)| d > 0)
            .map(|(k, _)| k.clone())
            .collect();
        cycle.sort();
        Some(cycle)
    }
}

fn format_goal(goal: &GoalSpec) -> String {
    match &goal.edam_format {
        Some(f) => format!("{} ({})", goal.edam_data, f),
        None => goal.edam_data.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::ResourceEstimate;

    fn wildcard_result(edam_data: String) -> CompositionResult {
        CompositionResult {
            matched_archetype: Some("test_archetype".into()),
            match_score: 0,
            atoms: Vec::new(),
            goal: GoalSpec {
                edam_data,
                edam_format: None,
                modifiers: BTreeMap::new(),
                source_prose: None,
                confidence: 0.5,
            },
            rationale: String::new(),
            atom_rationales: BTreeMap::new(),
            resource_estimate: ResourceEstimate::default(),
        }
    }

    #[test]
    fn wildcard_data_9999_does_not_trigger_goal_unreachable() {
        let result = wildcard_result("data:9999".into());
        let atom_reg = AtomRegistry::default();
        let outcome = validate_composition(&result, &atom_reg);
        assert!(
            outcome.is_ok(),
            "data:9999 must be wildcard, got {:?}",
            outcome
        );
    }

    #[test]
    fn empty_edam_data_does_not_trigger_goal_unreachable() {
        let result = wildcard_result(String::new());
        let atom_reg = AtomRegistry::default();
        let outcome = validate_composition(&result, &atom_reg);
        assert!(
            outcome.is_ok(),
            "empty edam_data must be wildcard, got {:?}",
            outcome
        );
    }
}
