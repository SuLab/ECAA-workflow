use super::affordance::{AffordanceProof, PlotAffordance};
use super::primitive::GenericPrimitive;
use super::registry::PlotAffordanceRegistry;

/// Inputs the selector needs from the port being resolved. We do not
/// pull `PortContract` directly here to keep this module independent
/// of the workflow_contracts crate path; the lowering pass adapts.
#[derive(Clone, Debug)]
pub struct PortDescriptor<'a> {
    /// Semantic type iri.
    pub semantic_type_iri: &'a str,
    /// Parent ontology terms declared on the port itself (e.g., from
    /// a `LocalExtension { proposed_parent_terms }`).
    pub declared_parents: &'a [String],
    /// Physical-shape hint used as the structural-fallback key.
    pub physical_shape: PhysicalShape,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// PhysicalShape discriminant.
pub enum PhysicalShape {
    /// Numeric2D variant.
    Numeric2D,
    /// Numeric1D variant.
    Numeric1D,
    /// Categorical1D variant.
    Categorical1D,
    /// Variant.
    /// Field value.
    TabularNumeric { columns: u8 },
    /// Scalar variant.
    Scalar,
    /// Unknown variant.
    Unknown,
}

impl PhysicalShape {
    /// Structural primitive.
    pub fn structural_primitive(self) -> Option<GenericPrimitive> {
        match self {
            Self::Numeric2D => Some(GenericPrimitive::MatrixOverview),
            Self::Numeric1D => Some(GenericPrimitive::Distribution),
            Self::Categorical1D => Some(GenericPrimitive::CategoricalSummary),
            Self::TabularNumeric { columns } if columns > 1 && columns <= 8 => {
                Some(GenericPrimitive::Pairs)
            }
            Self::Scalar => Some(GenericPrimitive::ScalarCard),
            _ => None,
        }
    }
}

/// Deterministic resolution. Given the same `(port, registry)` pair,
/// returns the same `PlotAffordance`. Walked in this order:
///
/// 1. Exact `SemanticType` match → `Registered`.
/// 2. Declared-parents walk (port-side `proposed_parent_terms`),
///    sorted lexically.
/// 3. Registry-side ancestor walk (BFS over `parents_of`),
///    visit-once, sorted lexically per layer.
/// 4. Structural fallback by `PhysicalShape`.
/// 5. `Deferred` with `sme_check_required=true`.
pub fn resolve_affordance<R: PlotAffordanceRegistry>(
    port: &PortDescriptor<'_>,
    registry: &R,
    theme_version: &str,
) -> PlotAffordance {
    // 1. Exact match.
    if let Some(reg) = registry.lookup_exact(port.semantic_type_iri) {
        return PlotAffordance::Registered {
            figure_ids: reg.figure_ids.clone(),
            renderer_module: reg.renderer_module.clone(),
            proof: AffordanceProof {
                source_semantic_type: port.semantic_type_iri.to_string(),
                ontology_walk: vec![],
                registry_snapshot_id: registry.snapshot_id().to_string(),
                theme_version: theme_version.to_string(),
                rationale: "exact SemanticType match in registered catalog".into(),
            },
        };
    }

    // 2. Declared-parents walk.
    let mut declared = port.declared_parents.to_vec();
    declared.sort();
    for parent in &declared {
        if let Some(reg) = registry.lookup_exact(parent) {
            return PlotAffordance::InheritedViaOntology {
                parent_term: parent.clone(),
                figure_ids: reg.figure_ids.clone(),
                renderer_module: reg.renderer_module.clone(),
                proof: AffordanceProof {
                    source_semantic_type: port.semantic_type_iri.to_string(),
                    ontology_walk: vec![parent.clone()],
                    registry_snapshot_id: registry.snapshot_id().to_string(),
                    theme_version: theme_version.to_string(),
                    rationale: format!("inherited renderer from declared parent term {parent}"),
                },
            };
        }
    }

    // 3. Registry-side BFS ancestor walk.
    let mut visited = std::collections::BTreeSet::new();
    let mut frontier: Vec<String> = registry.parents_of(port.semantic_type_iri);
    frontier.sort();
    let mut walk = vec![];
    while !frontier.is_empty() {
        let mut next: Vec<String> = vec![];
        for term in &frontier {
            if !visited.insert(term.clone()) {
                continue;
            }
            walk.push(term.clone());
            if let Some(reg) = registry.lookup_exact(term) {
                return PlotAffordance::InheritedViaOntology {
                    parent_term: term.clone(),
                    figure_ids: reg.figure_ids.clone(),
                    renderer_module: reg.renderer_module.clone(),
                    proof: AffordanceProof {
                        source_semantic_type: port.semantic_type_iri.to_string(),
                        ontology_walk: walk.clone(),
                        registry_snapshot_id: registry.snapshot_id().to_string(),
                        theme_version: theme_version.to_string(),
                        rationale: format!("inherited renderer via registry ancestor walk: {term}"),
                    },
                };
            }
            let mut grandparents = registry.parents_of(term);
            grandparents.sort();
            next.extend(grandparents);
        }
        frontier = next;
    }

    // 4. Structural fallback.
    if let Some(primitive) = port.physical_shape.structural_primitive() {
        return PlotAffordance::StructuralFallback {
            primitive,
            figure_id: primitive.figure_id().to_string(),
            warning: format!(
                "type-specific renderer pending for {}; rendered via {} primitive",
                port.semantic_type_iri,
                primitive.figure_id()
            ),
            proof: AffordanceProof {
                source_semantic_type: port.semantic_type_iri.to_string(),
                ontology_walk: walk,
                registry_snapshot_id: registry.snapshot_id().to_string(),
                theme_version: theme_version.to_string(),
                rationale: format!(
                    "structural primitive selected by physical shape {:?}",
                    port.physical_shape
                ),
            },
        };
    }

    // 5. Deferred.
    PlotAffordance::Deferred {
        data_artifact_relpath: String::new(), // populated by lowering pass
        recommendation: format!(
            "no automatic renderer resolved for semantic type {}",
            port.semantic_type_iri
        ),
        sme_check_required: true,
        proof: AffordanceProof {
            source_semantic_type: port.semantic_type_iri.to_string(),
            ontology_walk: walk,
            registry_snapshot_id: registry.snapshot_id().to_string(),
            theme_version: theme_version.to_string(),
            rationale: "no exact, inherited, or structural resolution available".into(),
        },
    }
}
