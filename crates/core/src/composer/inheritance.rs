//! Archetype `compose:` inheritance flattening.
//!
//! Resolves an archetype's `compose:` inheritance graph into a single
//! flat atom list ready for composition. Handles cycle detection, a
//! depth cap (`INHERITANCE_DEPTH_CAP` = 4), and per-ref `id_prefix` +
//! `replace_atoms` rewriting so the inherited DAG stays internally
//! consistent under stage-id rewrites.
//!
//! Public surface re-exported from `composer::*` for downstream test
//! consumption (`crates/core/tests/compose_inheritance.rs`,
//! `composer_cross_omics_n_way.rs`).

use super::CompositionError;
use crate::archetype::{ArchetypeAtomRef, ArchetypeDefinition, ComposeRef};
use crate::archetype_registry::ArchetypeRegistry;

/// Flattened archetype after `compose:` inheritance is
/// resolved. Has the same shape as `ArchetypeDefinition` but with
/// `compose:` replaced by the recursively-flattened atom list and a
/// `lineage` trail recording which archetypes contributed which
/// atoms (surfaced in composer rationale for SME audit).
#[derive(Debug, Clone, PartialEq)]
pub struct FlattenedArchetype {
    /// Id.
    pub id: String,
    /// Atoms.
    pub atoms: Vec<crate::archetype::ArchetypeAtomRef>,
    /// Inheritance trail: ordered list of `(inheriting_archetype_id,
    /// inherited_archetype_id, position, id_prefix)` tuples. Empty
    /// for archetypes with no `compose:` directives. Used by the
    /// rationale log + tests to assert flattening order.
    pub lineage: Vec<InheritanceStep>,
}

/// One step in a flattened archetype's inheritance trail.
#[derive(Debug, Clone, PartialEq)]
pub struct InheritanceStep {
    /// Inheriting archetype id.
    pub inheriting_archetype_id: String,
    /// Inherited archetype id.
    pub inherited_archetype_id: String,
    /// Position.
    pub position: crate::archetype::ComposePosition,
    /// Id prefix.
    pub id_prefix: Option<String>,
}

/// Depth cap for inheritance flattening. Deep chains
/// hurt auditability; the cap forces archetype authors to flatten
/// manually rather than nest more than 4 levels.
pub const INHERITANCE_DEPTH_CAP: usize = 4;

/// Flatten an archetype's `compose:` inheritance graph
/// into a single atom list ready for composition.
///
/// Recursive walk:
/// - For each `ComposeRef` in the archetype's `compose:` field, look
///   up the referenced archetype in the registry, recurse on ITS
///   `compose:` field (cycle-detected, depth-capped), then apply the
///   ref's `id_prefix` and `replace_atoms` to the recursed atoms.
/// - The archetype's own `atoms:` list is interleaved with the
///   inherited ones based on each ref's `position` (`Prefix` =
///   prepend, `Suffix` = append, `ReplaceAtoms` = ignore this
///   archetype's atoms entirely and use only the inherited ones).
///
/// Stage-id prefix rewriting: when a `ComposeRef` declares
/// `id_prefix: "rnaseq_"`, every inherited atom's `alias` (or
/// `atom_id` if no alias) is rewritten as `"rnaseq_<original>"`, AND
/// every `depends_on` reference inside the inherited atoms is
/// rewritten the same way so the inherited DAG stays internally
/// consistent.
///
/// Cycle detection: a `BTreeSet` tracks the active inheritance path;
/// re-entering an archetype id surfaces `InheritanceCycle` with the
/// path as evidence.
///
/// Depth cap: `INHERITANCE_DEPTH_CAP` (4) — deeper chains return
/// `InheritanceDepthExceeded`.
pub fn resolve_inheritance(
    archetype: &ArchetypeDefinition,
    archetype_reg: &ArchetypeRegistry,
) -> Result<FlattenedArchetype, CompositionError> {
    let mut active_path: Vec<String> = Vec::new();
    let mut lineage: Vec<InheritanceStep> = Vec::new();
    let atoms = flatten_recursive(archetype, archetype_reg, &mut active_path, &mut lineage, 0)?;
    Ok(FlattenedArchetype {
        id: archetype.id.clone(),
        atoms,
        lineage,
    })
}

fn flatten_recursive(
    archetype: &ArchetypeDefinition,
    archetype_reg: &ArchetypeRegistry,
    active_path: &mut Vec<String>,
    lineage: &mut Vec<InheritanceStep>,
    depth: usize,
) -> Result<Vec<ArchetypeAtomRef>, CompositionError> {
    if depth > INHERITANCE_DEPTH_CAP {
        return Err(CompositionError::InheritanceDepthExceeded {
            archetype_id: archetype.id.clone(),
            depth,
            cap: INHERITANCE_DEPTH_CAP,
        });
    }
    if active_path.iter().any(|id| id == &archetype.id) {
        let mut path = active_path.clone();
        path.push(archetype.id.clone());
        return Err(CompositionError::InheritanceCycle { path });
    }
    active_path.push(archetype.id.clone());

    // Walk this archetype's own atom list once for the "own" slot.
    let own_atoms: Vec<ArchetypeAtomRef> = archetype.atoms.clone();

    // Build prefix/suffix lists from inheritance refs, plus a flag
    // for replace_atoms-mode refs that override own_atoms entirely.
    let mut prefixed: Vec<ArchetypeAtomRef> = Vec::new();
    let mut suffixed: Vec<ArchetypeAtomRef> = Vec::new();
    let mut replace_mode: Option<Vec<ArchetypeAtomRef>> = None;

    for cref in &archetype.compose {
        let inherited = archetype_reg.get(&cref.archetype_id).ok_or_else(|| {
            CompositionError::UnknownInheritedArchetype {
                archetype_id: cref.archetype_id.clone(),
                referenced_from: archetype.id.clone(),
            }
        })?;

        // Validate replace_atoms targets reference atom_ids that
        // actually exist in the inherited archetype.
        for target_id in cref.replace_atoms.keys() {
            let exists = inherited
                .atoms
                .iter()
                .any(|a| a.atom_id.as_str() == target_id.as_str());
            if !exists {
                return Err(CompositionError::UnknownReplaceTarget {
                    archetype_id: archetype.id.clone(),
                    inherited_archetype_id: cref.archetype_id.clone(),
                    target_atom_id: target_id.as_str().into(),
                });
            }
        }

        // Recurse into the inherited archetype to handle nested
        // compose: directives.
        let recursed =
            flatten_recursive(inherited, archetype_reg, active_path, lineage, depth + 1)?;

        // Apply replace_atoms substitution + id_prefix rewriting.
        let rewritten = rewrite_inherited_atoms(&recursed, cref);

        lineage.push(InheritanceStep {
            inheriting_archetype_id: archetype.id.clone(),
            inherited_archetype_id: cref.archetype_id.clone(),
            position: cref.position,
            id_prefix: cref.id_prefix.clone(),
        });

        match cref.position {
            crate::archetype::ComposePosition::Prefix => prefixed.extend(rewritten),
            crate::archetype::ComposePosition::Suffix => suffixed.extend(rewritten),
            crate::archetype::ComposePosition::ReplaceAtoms => {
                replace_mode = Some(rewritten);
            }
        }
    }

    active_path.pop();

    // Assemble: prefix + (replace_mode OR own) + suffix.
    let middle = replace_mode.unwrap_or(own_atoms);
    let mut out = Vec::with_capacity(prefixed.len() + middle.len() + suffixed.len());
    out.extend(prefixed);
    out.extend(middle);
    out.extend(suffixed);
    Ok(out)
}

/// Apply `id_prefix` + `replace_atoms` to a list of
/// inherited atom refs.
///
/// `replace_atoms` substitution happens first (operates on
/// `atom_id`); `id_prefix` rewriting happens second (operates on
/// `alias` and `depends_on`).
fn rewrite_inherited_atoms(atoms: &[ArchetypeAtomRef], cref: &ComposeRef) -> Vec<ArchetypeAtomRef> {
    // Build an alias-rewrite map: original-id → prefixed-id. Used
    // for both this atom's own alias and downstream depends_on
    // references.
    let prefixed_id = |original: &str| -> String {
        match &cref.id_prefix {
            Some(p) => format!("{p}{original}"),
            None => original.to_string(),
        }
    };

    atoms
        .iter()
        .map(|a| {
            let atom_id: crate::ids::AtomId = cref
                .replace_atoms
                .get(a.atom_id.as_str())
                .map(|s| s.as_str().into())
                .unwrap_or_else(|| a.atom_id.clone());
            // Stage id (alias > atom_id) drives both this atom's own
            // identifier and what depends_on references look like.
            let original_stage_id = a.alias.clone().unwrap_or_else(|| a.atom_id.to_string());
            let new_stage_id = prefixed_id(&original_stage_id);
            // Always set an alias when we're prefixing OR substituting,
            // so the composed DAG carries the rewritten id.
            let alias = if cref.id_prefix.is_some() || a.alias.is_some() {
                Some(new_stage_id.clone())
            } else if atom_id != a.atom_id {
                // replace_atoms changed atom_id but no prefix — still
                // need an alias to keep the original stage-id stable
                // for downstream depends_on references, which are
                // the original stage-ids.
                Some(original_stage_id)
            } else {
                None
            };
            let depends_on: Vec<String> = a.depends_on.iter().map(|d| prefixed_id(d)).collect();
            ArchetypeAtomRef {
                atom_id,
                alias,
                depends_on,
                required: a.required,
                required_figures: a.required_figures.clone(),
                plot_stage_id: a.plot_stage_id.clone(),
                figure_exempt: a.figure_exempt.clone(),
                expected_artifacts: a.expected_artifacts.clone(),
                required_artifacts: a.required_artifacts.clone(),
            }
        })
        .collect()
}
