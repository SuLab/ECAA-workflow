//! Slot-fill algorithm + the SME intake context that
//! drives it.
//!
//! The composer's primary entry point — `compose_with_intake` — lives
//! here alongside the slot-fill walker (`apply_slot_fill`), the single-
//! slot resolver (`resolve_slot`), the SME-supplied intake context
//! (`IntakeContext`), the slot-binding records the composer attaches to
//! every kept `ComposedAtom` (`SlotBinding` + `SlotSource`), and the
//! literature-atom opt-in list (`LITERATURE_OPT_IN_ATOM_IDS`).
//!
//! Public surface is preserved verbatim — `composer::IntakeContext`,
//! `composer::compose_with_intake`, `composer::SlotBinding`,
//! `composer::SlotSource`, and `composer::LITERATURE_OPT_IN_ATOM_IDS`
//! continue to resolve from this submodule via `pub use`.

use super::{compose, ComposedAtom, CompositionError, CompositionResult};
use crate::archetype_registry::ArchetypeRegistry;
use crate::atom_registry::AtomRegistry;
use crate::edam::is_subtype_of;
use crate::goal_spec::GoalSpec;
use crate::intake_port_mapper::PortMappingRegistry;
use std::collections::{BTreeMap, BTreeSet};

/// One slot-fill binding recorded by `compose_with_intake`. The
/// composer emits these so the builder + the SME confirmation card
/// can show *where* each atom's input came from (upstream atom vs
/// intake field), and so the composer's caller can verify slot-fill
/// coverage independently of re-running the algorithm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotBinding {
    /// The slot name on the consuming atom. Today's slots are the
    /// atom's primary `edam_data` input (named `"primary_input"`)
    /// plus each entry in `depends_on` (named after the dep id).
    /// A future extension will recognise a structured `consumes:`
    /// block on the atom.
    pub slot: String,
    /// The expected EDAM data class IRI for this slot — the value
    /// the slot-fill check matched against via `is_subtype_of`.
    pub expected: String,
    /// Where the slot's value came from.
    pub source: SlotSource,
}

/// What populated a slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotSource {
    /// The slot was filled by another composed atom's output.
    /// Carries the upstream atom's `stage_id`.
    UpstreamAtom(String),
    /// The slot was filled by an SME-supplied intake field. Carries
    /// the intake field name (matching the
    /// `PortMappingRegistry::get` key).
    IntakeField(String),
}

/// Intake context the slot-fill algorithm reads.
///
/// Carries the SME-supplied intake fields (set membership only —
/// the field's value is irrelevant to slot-fill, only presence) plus
/// the `PortMappingRegistry` that maps each field to its EDAM data
/// class. The composer caller (initially the conversation crate's
/// LLM tool layer; later the builder's archetype-path branch) builds
/// this from the live session and passes it to `compose_with_intake`.
///
/// The legacy `compose()` entry point uses an empty context so
/// existing callers keep working without rewiring; new callers that
/// want slot-fill validation use `compose_with_intake` directly.
#[derive(Debug, Clone, Default)]
pub struct IntakeContext<'a> {
    /// Intake field names the SME has supplied a value for. The
    /// field's value isn't consulted — slot-fill is a typing check,
    /// not a value check.
    pub supplied_fields: BTreeSet<String>,
    /// Port-mapping registry resolving each intake field to its
    /// EDAM data class. When `None`, slot-fill is skipped entirely
    /// (legacy `compose()` behavior).
    pub port_mappings: Option<&'a PortMappingRegistry>,
    /// Phase G of the literature-atom plan — opt-in for the
    /// `review_prior_work` + `contextualize_findings_with_literature`
    /// atom family. When `false` (the default), `compose_with_intake`
    /// filters those atoms out of the result before slot-fill so the
    /// emitted DAG is byte-identical to a pre-literature DAG. Set
    /// from `IntakeFacts::literature_review_requested`.
    pub literature_review_requested: bool,
}

impl<'a> IntakeContext<'a> {
    /// Empty context — `compose_with_intake` will skip the slot-fill
    /// pass. Used by the legacy `compose()` entry point.
    pub fn empty() -> Self {
        Self {
            supplied_fields: BTreeSet::new(),
            port_mappings: None,
            literature_review_requested: false,
        }
    }

    /// Build a context from an intake field iterator + a port-mapping
    /// registry reference. Convenience for the LLM tool layer.
    pub fn new(
        supplied: impl IntoIterator<Item = String>,
        mappings: &'a PortMappingRegistry,
    ) -> Self {
        Self {
            supplied_fields: supplied.into_iter().collect(),
            port_mappings: Some(mappings),
            literature_review_requested: false,
        }
    }
}

/// Phase G of the literature-atom plan — atom ids that get filtered
/// out of `compose_with_intake` when `intake.literature_review_requested`
/// is false. Centralized here so the gate is a single edit point.
pub const LITERATURE_OPT_IN_ATOM_IDS: &[&str] = &[
    "review_prior_work",
    "contextualize_findings_with_literature",
];

/// Slot-fill-aware composition entry point.
///
/// Wraps `compose` and runs the slot-fill algorithm immediately after
/// archetype matching but before the exclusion check. For each
/// composed atom, walk its input slots (the atom's primary `edam_data`
/// plus each `depends_on` id) and verify each is filled either by an
/// upstream composed atom's output (EDAM-subtype matched via
/// `is_subtype_of`) or by an SME-supplied intake field (looked up via
/// `PortMappingRegistry`). Required atoms with unfilled slots return
/// `CompositionError::UnfilledRequiredSlot`; optional atoms with
/// unfilled slots are silently dropped from the composition with
/// their stage ids pruned from any surviving atom's `depends_on`.
///
/// When `intake.port_mappings.is_none()` the slot-fill pass is
/// skipped entirely — `compose_with_intake` becomes a thin wrapper
/// over `compose`. This is the path the legacy `compose()` entry
/// point takes.
pub fn compose_with_intake(
    goal: &GoalSpec,
    project_class: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
    intake: &IntakeContext<'_>,
) -> Result<CompositionResult, CompositionError> {
    let mut result = compose(goal, project_class, atom_reg, archetype_reg)?;
    // Phase G — opt-in literature atom family. When the SME hasn't
    // requested literature review, drop the atoms before slot-fill so
    // the emitted DAG is byte-identical to a pre-literature DAG and
    // existing scenarios are not regressed. Their stage_ids are also
    // pruned from surviving atoms' depends_on edges.
    if !intake.literature_review_requested {
        let dropped: BTreeSet<String> = result
            .atoms
            .iter()
            .filter(|c| LITERATURE_OPT_IN_ATOM_IDS.contains(&c.atom.id.as_str()))
            .map(|c| c.stage_id.to_string())
            .collect();
        if !dropped.is_empty() {
            result
                .atoms
                .retain(|c| !LITERATURE_OPT_IN_ATOM_IDS.contains(&c.atom.id.as_str()));
            for c in result.atoms.iter_mut() {
                c.depends_on.retain(|d| !dropped.contains(d));
            }
        }
    }
    if intake.port_mappings.is_some() {
        apply_slot_fill(&mut result, intake)?;
    }
    Ok(result)
}

/// Slot-fill algorithm.
///
/// "Slots" are derived implicitly from the AtomDefinition shape: the
/// atom's primary `edam_data` is the canonical input slot named
/// `"primary_input"`; each entry in `depends_on` is an upstream-keyed
/// slot. A future extension recognises a structured `consumes:`
/// block on the atom — the function signature stays compatible.
///
/// For each atom, walk its slots. A slot is filled by:
///
/// 1. **Upstream producer.** Another composed atom's output
///    (`edam_data`) is the same EDAM type as the slot expects, OR
///    is an EDAM subtype per `is_subtype_of`.
/// 2. **Intake-supplied field.** A `PortMappingRegistry` rule whose
///    `edam_data` is a (possibly subtype-matching) match for the
///    slot's expected type AND whose `intake_field` is in the SME's
///    supplied set.
///
/// Optional atoms whose slots remain unfilled are silently dropped
/// from the composition; the dropped atom's stage_id is pruned from
/// any surviving atom's `depends_on` so the cycle/satisfiability
/// validators don't trip on a now-dangling edge. Required atoms with
/// unfilled slots return `CompositionError::UnfilledRequiredSlot`.
pub(super) fn apply_slot_fill(
    result: &mut CompositionResult,
    intake: &IntakeContext<'_>,
) -> Result<(), CompositionError> {
    // Build a lookup of every composed atom's stage_id → its produced
    // EDAM data class. Used by the upstream-producer check.
    let mut producers: BTreeMap<String, Option<String>> = BTreeMap::new();
    for c in &result.atoms {
        producers.insert(c.stage_id.to_string(), c.atom.edam_data.clone());
    }

    // Two-pass algorithm so cascade skip works: pass 1 records every
    // atom's binding decisions; pass 2 prunes the dropped atoms +
    // their dangling depends_on edges.
    enum AtomDecision {
        Keep(Vec<SlotBinding>),
        Drop,
    }
    let mut decisions: Vec<(String, AtomDecision)> = Vec::with_capacity(result.atoms.len());

    for c in &result.atoms {
        let mut bindings: Vec<SlotBinding> = Vec::new();
        let mut unfilled_required: Option<(String, String)> = None;
        let mut any_unfilled_optional = false;

        // Slot 1: primary input (atom's `edam_data`). Today's
        // AtomDefinition shape conflates input/output: `edam_data`
        // names what the atom operates on (and emits, since v1
        // atoms are typically operation-shaped). The slot-fill check
        // therefore only treats `primary_input` as a load-bearing
        // slot when the atom has no `depends_on` — i.e. it's a leaf
        // in the dependency graph and its primary input must come
        // from intake. Atoms with `depends_on` get their primary
        // input from upstream via the dep-slot fan-out below.
        // Aggregator + Discovery atoms commonly skip edam_data
        // entirely; they're handled by the depends_on slots.
        if c.depends_on.is_empty() {
            if let Some(expected) = c.atom.edam_data.as_deref() {
                match resolve_slot("primary_input", expected, c, &producers, intake) {
                    SlotResolution::FilledUpstream(stage_id) => bindings.push(SlotBinding {
                        slot: "primary_input".into(),
                        expected: expected.into(),
                        source: SlotSource::UpstreamAtom(stage_id),
                    }),
                    SlotResolution::FilledIntake(field) => bindings.push(SlotBinding {
                        slot: "primary_input".into(),
                        expected: expected.into(),
                        source: SlotSource::IntakeField(field),
                    }),
                    SlotResolution::Unfilled => {
                        if c.required {
                            unfilled_required = Some(("primary_input".into(), expected.into()));
                        } else {
                            any_unfilled_optional = true;
                        }
                    }
                }
            }
        }

        // Slot 2..N: each `depends_on` id is a slot whose value comes
        // from that upstream atom's output. The expected type is the
        // upstream atom's `edam_data` if that atom is in the
        // composition; otherwise treat the dep as an intake-keyed
        // slot whose name *is* the field name (the atom registry
        // doesn't know about it, so it's a virtual port).
        if unfilled_required.is_none() {
            for dep in &c.depends_on {
                let expected = producers
                    .get(dep)
                    .and_then(|d| d.clone())
                    .unwrap_or_else(|| dep.clone());
                match resolve_slot(dep, &expected, c, &producers, intake) {
                    SlotResolution::FilledUpstream(stage_id) => bindings.push(SlotBinding {
                        slot: dep.clone(),
                        expected: expected.clone(),
                        source: SlotSource::UpstreamAtom(stage_id),
                    }),
                    SlotResolution::FilledIntake(field) => bindings.push(SlotBinding {
                        slot: dep.clone(),
                        expected: expected.clone(),
                        source: SlotSource::IntakeField(field),
                    }),
                    SlotResolution::Unfilled => {
                        if c.required {
                            unfilled_required = Some((dep.clone(), expected));
                            break;
                        }
                        any_unfilled_optional = true;
                    }
                }
            }
        }

        if let Some((slot, expected)) = unfilled_required {
            return Err(CompositionError::UnfilledRequiredSlot {
                atom: c.stage_id.to_string(),
                slot,
                expected,
            });
        }

        let decision = if any_unfilled_optional {
            AtomDecision::Drop
        } else {
            AtomDecision::Keep(bindings)
        };
        decisions.push((c.stage_id.to_string(), decision));
    }

    // Pass 2: rebuild the atom list, dropping skipped atoms and
    // pruning their stage ids from surviving atoms' depends_on edges.
    let dropped: BTreeSet<String> = decisions
        .iter()
        .filter_map(|(id, d)| {
            if matches!(d, AtomDecision::Drop) {
                Some(id.clone())
            } else {
                None
            }
        })
        .collect();

    let mut new_atoms: Vec<ComposedAtom> = Vec::with_capacity(result.atoms.len());
    for (atom, (_id, decision)) in result.atoms.drain(..).zip(decisions.into_iter()) {
        if let AtomDecision::Keep(bindings) = decision {
            let mut next = atom;
            next.depends_on.retain(|d| !dropped.contains(d));
            next.bindings = bindings;
            new_atoms.push(next);
        }
    }
    result.atoms = new_atoms;
    Ok(())
}

pub(super) enum SlotResolution {
    FilledUpstream(String),
    FilledIntake(String),
    Unfilled,
}

/// Resolve a single slot. First check upstream composed atoms for a
/// subtype-compatible producer; then look in the port-mapping
/// registry for an intake field whose declared `edam_data` is a
/// subtype of the expected type AND is present in the SME's intake
/// supplied-fields set.
pub(super) fn resolve_slot(
    slot_name: &str,
    expected: &str,
    consumer: &ComposedAtom,
    producers: &BTreeMap<String, Option<String>>,
    intake: &IntakeContext<'_>,
) -> SlotResolution {
    // Upstream producer check. When the slot name matches a composed
    // atom id (the depends_on case), prefer that explicit edge; for
    // primary_input, scan every producer for a subtype match.
    if let Some(Some(produced)) = producers.get(slot_name) {
        if produced == expected || is_subtype_of(produced, expected) {
            return SlotResolution::FilledUpstream(slot_name.into());
        }
    }
    for (stage_id, produced) in producers {
        if stage_id.as_str() == consumer.stage_id.as_str() {
            continue;
        }
        if let Some(produced) = produced {
            if produced == expected || is_subtype_of(produced, expected) {
                return SlotResolution::FilledUpstream(stage_id.clone());
            }
        }
    }

    // Intake-supplied check. Walk every port-mapping rule; pick the
    // first one whose edam_data is a (possibly subtype) match for the
    // expected slot type AND whose intake_field is in the SME's
    // supplied set. Iteration is BTreeMap-ordered so multi-hit ties
    // resolve deterministically (alphabetical by intake_field name).
    if let Some(mappings) = intake.port_mappings {
        for (field, rule) in mappings.iter() {
            if !intake.supplied_fields.contains(field) {
                continue;
            }
            if rule.edam_data == expected || is_subtype_of(&rule.edam_data, expected) {
                return SlotResolution::FilledIntake(field.clone());
            }
        }
    }

    SlotResolution::Unfilled
}
