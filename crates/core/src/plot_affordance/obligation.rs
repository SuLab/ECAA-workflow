//! Figure-obligation lint for the flexible-plotting upgrade.
//!
//! Every non-adapter atom that declares a `DataProduct` output must resolve
//! to a non-`Deferred` `PlotAffordance` or carry an explicit `figure_exempt`
//! annotation. Violation means the atom needs one of:
//!
//! - `required_figures: [...]` entries that map to registered renderers,
//! - a richer `SemanticType` with ontology parents the registry knows,
//! - a `figure_exempt: { reason: "...", category: "..." }` exemption, or
//! - an extension of the structural-primitive set (`PhysicalShape`).
//!
//! # Adapter detection
//!
//! Adapter detection defers entirely to `crate::adapter_registry::is_adapter_atom`.
//! Do **not** use an `id`-prefix heuristic here — the registry is the source of
//! truth per the addendum.
//!
//! # Port walking order
//!
//! `primary_output_descriptor` walks `AtomDefinition.outputs` first (rich
//! `PortContract` path), falling back to the legacy `edam_data` field only
//! When `outputs.is_empty()`. This mirrors the addendum's instruction:
//!
//! > Port resolution MUST walk `AtomDefinition.outputs: Vec<PortContract>`
//! > first, falling back to `edam_data` only when `outputs.is_empty()`.
//!
//! # `PhysicalShape`
//!
//! All descriptors return `PhysicalShape::Unknown` — rich shape inference is
//! a data-profiler follow-up. `Unknown` maps to no structural primitive, so
//! atoms with no registered/inheritable affordance resolve to `Deferred` and
//! become violations (unless exempted).

use crate::adapter_registry;
use crate::atom::AtomDefinition;
use crate::ids::AtomId;
use crate::plot_affordance::{
    resolve_affordance, PhysicalShape, PlotAffordance, PlotAffordanceRegistry, PortDescriptor,
};
use crate::workflow_contracts::port::PortContract;
use crate::workflow_contracts::semantic_type::SemanticType;

/// A single figure-obligation lint violation.
#[derive(Debug)]
pub struct ObligationViolation {
    /// The atom id that triggered the violation.
    pub atom_id: AtomId,
    /// Human-readable description of why the lint flagged this atom.
    pub reason: String,
}

/// Check a single atom against the figure-obligation lint.
///
/// Returns `Ok(())` when the atom passes (exempt, not a data-product
/// producer, or resolves to a non-`Deferred` affordance). Returns
/// `Err(ObligationViolation)` when the atom produces a `DataProduct`
/// but resolves to `Deferred` and carries no exemption.
pub fn check_atom<R: PlotAffordanceRegistry>(
    atom: &AtomDefinition,
    registry: &R,
    theme_version: &str,
) -> Result<(), ObligationViolation> {
    // Explicit figure_exempt annotation → pass immediately.
    if atom.figure_exempt.is_some() {
        return Ok(());
    }

    // Defer adapter detection to the registry — no prefix heuristic.
    if adapter_registry::is_adapter_atom(&atom.id) {
        return Ok(());
    }

    // Skip atoms that declare no data product output (e.g. Discovery
    // atoms that pick a method but produce no artifact, or pure control
    // atoms with empty edam_data and no outputs).
    if !declares_data_product_output(atom) {
        return Ok(());
    }

    let descriptor = primary_output_descriptor(atom);
    let port_desc = PortDescriptor {
        semantic_type_iri: &descriptor.semantic_type_iri,
        declared_parents: &descriptor.declared_parents,
        physical_shape: descriptor.physical_shape,
    };

    let affordance = resolve_affordance(&port_desc, registry, theme_version);
    let resolved_figure_ids: Vec<String> = match &affordance {
        PlotAffordance::Registered { figure_ids, .. }
        | PlotAffordance::InheritedViaOntology { figure_ids, .. }
        | PlotAffordance::GeneratedSandboxed { figure_ids, .. } => figure_ids.clone(),
        PlotAffordance::StructuralFallback { figure_id, .. } => vec![figure_id.clone()],
        PlotAffordance::Deferred { .. } => {
            return Err(ObligationViolation {
                atom_id: atom.id.as_str().into(),
                reason: format!(
                    "atom {} produces a DataProduct but resolves to Deferred; \
                     declare required_figures, add ontology parents, mark figure_exempt, \
                     or extend the structural primitive set",
                    atom.id
                ),
            });
        }
    };

    // Subset check: every required_figure must be in the resolved renderer's figure_ids.
    let missing: Vec<&str> = atom
        .required_figures
        .iter()
        .filter(|rf| !resolved_figure_ids.iter().any(|f| f == *rf))
        .map(|s| s.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(ObligationViolation {
            atom_id: atom.id.as_str().into(),
            reason: format!(
                "atom {} requires figure(s) [{}] not implemented by resolved renderer \
                 (resolved figure_ids: [{}]); register a renderer that implements these, \
                 or migrate the output IRI to one whose renderer does",
                atom.id,
                missing.join(", "),
                resolved_figure_ids.join(", ")
            ),
        });
    }

    Ok(())
}

/// Run `check_atom` over every atom in `atoms` and collect all violations.
pub fn check_all<R: PlotAffordanceRegistry>(
    atoms: &[AtomDefinition],
    registry: &R,
    theme_version: &str,
) -> Vec<ObligationViolation> {
    atoms
        .iter()
        .filter_map(|a| check_atom(a, registry, theme_version).err())
        .collect()
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Lightweight descriptor produced from an atom's output declaration.
/// The iri/parents are the keys the affordance selector uses; shape is
/// always `Unknown` (rich inference is a data-profiler follow-up).
struct PrimaryOutputDescriptor {
    semantic_type_iri: String,
    declared_parents: Vec<String>,
    physical_shape: PhysicalShape,
}

/// Returns `true` when the atom has any indication of a data-product
/// output — either an explicit `outputs` port or a legacy `edam_data`
/// IRI. Atoms with neither (e.g. pure discovery or aggregator atoms
/// with no output spec) are skipped by the lint.
fn declares_data_product_output(atom: &AtomDefinition) -> bool {
    if !atom.outputs.is_empty() {
        return true;
    }
    // Legacy path: edam_data set means the atom emits a typed artifact.
    atom.edam_data.is_some()
}

/// Extract the primary output descriptor. Walks `outputs` first
/// (rich `PortContract` path), falls back to legacy `edam_data` when
/// no explicit ports are declared.
fn primary_output_descriptor(atom: &AtomDefinition) -> PrimaryOutputDescriptor {
    if let Some(port) = atom.outputs.first() {
        return port_descriptor_from_port(port, &atom.id);
    }
    // Legacy fallback for atoms that haven't been migrated to explicit
    // PortContract outputs yet. `edam_data` is a single IRI; there are no
    // declared parent terms in
    // the legacy schema, so `declared_parents` is empty and the selector
    // relies on the registry-side BFS ancestor walk instead.
    PrimaryOutputDescriptor {
        semantic_type_iri: atom.edam_data.clone().unwrap_or_default(),
        declared_parents: vec![],
        physical_shape: PhysicalShape::Unknown,
    }
}

/// Build a `PrimaryOutputDescriptor` from a rich `PortContract`.
///
/// Variant mapping:
/// - `OntologyTerm { iri,.. }` → use `iri` as the lookup key;
///   `declared_parents` is empty (the IRI is already a registered term).
/// - `LocalExtension { namespace, id, proposed_parent_terms,.. }` →
///   key is `format!("{namespace}:{id}")`; `proposed_parent_terms`
///   become the `declared_parents` passed to the selector so ontology
///   inheritance kicks in.
/// - `Opaque {.. }` → synthesize `swfc:opaque:<atom_id>` so the key
///   is deterministic and atom-scoped; no declared parents (there is
///   nothing useful to inherit from an opaque type).
fn port_descriptor_from_port(port: &PortContract, atom_id: &str) -> PrimaryOutputDescriptor {
    match &port.semantic_type {
        SemanticType::OntologyTerm { iri, .. } => PrimaryOutputDescriptor {
            semantic_type_iri: iri.clone(),
            declared_parents: vec![],
            physical_shape: PhysicalShape::Unknown,
        },
        SemanticType::LocalExtension {
            namespace,
            id,
            proposed_parent_terms,
            ..
        } => PrimaryOutputDescriptor {
            // Stable id format matches `SemanticType::stable_id()`:
            // `<namespace>:<id>`.
            semantic_type_iri: format!("{namespace}:{id}"),
            declared_parents: proposed_parent_terms.clone(),
            physical_shape: PhysicalShape::Unknown,
        },
        SemanticType::Opaque { .. } => PrimaryOutputDescriptor {
            // Synthesized atom-scoped key so different opaque atoms
            // don't collide on the registry lookup.
            semantic_type_iri: format!("swfc:opaque:{atom_id}"),
            declared_parents: vec![],
            physical_shape: PhysicalShape::Unknown,
        },
        SemanticType::Union { .. } => PrimaryOutputDescriptor {
            // Union output ports are only used on input sides of atoms;
            // if an output port carries a Union type it is treated as
            // opaque for figure-obligation purposes (no single IRI to
            // look up in the plot-affordance catalog).
            semantic_type_iri: format!("swfc:union:{atom_id}"),
            declared_parents: vec![],
            physical_shape: PhysicalShape::Unknown,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::{AtomAssignee, AtomRole, FigureExempt};
    use crate::plot_affordance::registry::YamlPlotAffordanceRegistry;
    use crate::workflow_contracts::port::PortContract;
    use crate::workflow_contracts::semantic_type::SemanticType;
    use std::collections::BTreeMap;

    fn minimal_operation_atom(id: &str, edam_data: Option<&str>) -> AtomDefinition {
        AtomDefinition {
            id: id.into(),
            version: "1.0.0".into(),
            role: AtomRole::Operation,
            discovery_kind: None,
            description: "test".into(),
            edam_operation: "operation:0004".into(),
            edam_data: edam_data.map(|s| s.into()),
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

    fn empty_registry() -> YamlPlotAffordanceRegistry {
        // Build an empty registry via the in-memory path.
        YamlPlotAffordanceRegistry::empty()
    }

    /// A no-edam-data atom (e.g. pure aggregator) must be skipped by the lint.
    #[test]
    fn atom_without_edam_data_passes() {
        let atom = minimal_operation_atom("my_aggregator", None);
        let registry = empty_registry();
        assert!(check_atom(&atom, &registry, "theme.json").is_ok());
    }

    /// A known adapter id is skipped even with edam_data set.
    #[test]
    fn known_adapter_atom_passes() {
        let mut atom = minimal_operation_atom("bam_sort_coordinate", Some("data:0863"));
        atom.id = "bam_sort_coordinate".into();
        let registry = empty_registry();
        assert!(check_atom(&atom, &registry, "theme.json").is_ok());
    }

    /// An atom with edam_data but no registered affordance → violation.
    #[test]
    fn unregistered_data_product_is_violation() {
        let atom = minimal_operation_atom("my_stage", Some("data:9999"));
        let registry = empty_registry();
        let result = check_atom(&atom, &registry, "theme.json");
        assert!(
            result.is_err(),
            "expected a violation for unregistered type"
        );
        assert!(result.unwrap_err().atom_id.as_str() == "my_stage");
    }

    /// `figure_exempt` suppresses the violation.
    #[test]
    fn figure_exempt_suppresses_violation() {
        let mut atom = minimal_operation_atom("my_stage", Some("data:9999"));
        atom.figure_exempt = Some(FigureExempt {
            reason: "intermediate artifact".into(),
            category: Some("intermediate".into()),
        });
        let registry = empty_registry();
        assert!(check_atom(&atom, &registry, "theme.json").is_ok());
    }

    /// `check_all` collects violations from multiple atoms.
    #[test]
    fn check_all_collects_multiple_violations() {
        let atoms = vec![
            minimal_operation_atom("stage_a", Some("data:1111")),
            minimal_operation_atom("stage_b", None), // no output → pass
            minimal_operation_atom("stage_c", Some("data:2222")),
        ];
        let registry = empty_registry();
        let violations = check_all(&atoms, &registry, "theme.json");
        assert_eq!(
            violations.len(),
            2,
            "expected violations for stage_a and stage_c"
        );
        let ids: Vec<&str> = violations.iter().map(|v| v.atom_id.as_str()).collect();
        assert!(ids.contains(&"stage_a"));
        assert!(ids.contains(&"stage_c"));
    }

    /// An atom with an explicit PortContract output using an `OntologyTerm`
    /// that has no registry entry → violation (walks `outputs` first).
    #[test]
    fn port_contract_output_ontology_term_unregistered_is_violation() {
        let mut atom = minimal_operation_atom("rich_stage", None);
        atom.outputs = vec![PortContract {
            name: "counts".into(),
            semantic_type: SemanticType::OntologyTerm {
                iri: "data:3917".into(),
                label: "Count matrix".into(),
                ontology_version: None,
            },
            ..Default::default()
        }];
        let registry = empty_registry();
        let result = check_atom(&atom, &registry, "theme.json");
        assert!(result.is_err());
    }

    /// An atom with a `LocalExtension` output carrying proposed parent terms
    /// that are also unregistered → violation (parents not found).
    #[test]
    fn port_contract_local_extension_with_unregistered_parent_is_violation() {
        let mut atom = minimal_operation_atom("ext_stage", None);
        atom.outputs = vec![PortContract {
            name: "out".into(),
            semantic_type: SemanticType::LocalExtension {
                namespace: "swfc".into(),
                id: "novel_type".into(),
                proposed_parent_terms: vec!["data:9001".into()],
                definition: "A novel data type".into(),
                maturity: crate::workflow_contracts::semantic_type::LocalExtensionMaturity::Minted,
            },
            ..Default::default()
        }];
        let registry = empty_registry();
        let result = check_atom(&atom, &registry, "theme.json");
        assert!(result.is_err());
    }

    /// An `Opaque`-output atom → violation unless exempt.
    #[test]
    fn opaque_output_atom_is_violation() {
        let mut atom = minimal_operation_atom("opaque_stage", None);
        atom.outputs = vec![PortContract {
            name: "out".into(),
            semantic_type: SemanticType::Opaque {
                description: "profiler failed".into(),
            },
            ..Default::default()
        }];
        let registry = empty_registry();
        let result = check_atom(&atom, &registry, "theme.json");
        assert!(result.is_err());
    }

    /// An atom that declares `required_figures` not present in the
    /// resolved renderer's figure_ids → violation.
    #[test]
    fn required_figures_not_in_resolved_renderer_is_violation() {
        use crate::plot_affordance::registry::PlotAffordanceEntry;
        let registry = YamlPlotAffordanceRegistry::from_entries(vec![PlotAffordanceEntry {
            semantic_type: "data:0951".into(),
            figure_ids: vec!["top_enriched_terms".into()],
            renderer_module: "runtime.plotting.stages.biological_interpretation".into(),
            theme_version: "theme.json".into(),
            parents: vec![],
        }]);

        let mut atom = minimal_operation_atom("my_stage", Some("data:0951"));
        atom.required_figures = vec!["volcano".into(), "top_features_heatmap".into()];

        let result = check_atom(&atom, &registry, "theme.json");
        assert!(
            result.is_err(),
            "expected violation when required_figures ⊄ renderer.figure_ids"
        );
        let err = result.unwrap_err();
        assert!(
            err.reason.contains("required figure") || err.reason.contains("not implemented"),
            "reason should explain the figure_id mismatch, got: {}",
            err.reason
        );
    }

    /// A passing case — required_figures fully covered by the resolved
    /// renderer's figure_ids.
    #[test]
    fn required_figures_covered_passes() {
        use crate::plot_affordance::registry::PlotAffordanceEntry;
        let registry = YamlPlotAffordanceRegistry::from_entries(vec![PlotAffordanceEntry {
            semantic_type: "data:3134".into(),
            figure_ids: vec!["volcano".into(), "top_features_heatmap".into()],
            renderer_module: "runtime.plotting.stages.differential_expression".into(),
            theme_version: "theme.json".into(),
            parents: vec![],
        }]);

        let mut atom = minimal_operation_atom("dx", Some("data:3134"));
        atom.required_figures = vec!["volcano".into()];

        let result = check_atom(&atom, &registry, "theme.json");
        assert!(result.is_ok(), "subset should pass: {:?}", result.err());
    }
}
