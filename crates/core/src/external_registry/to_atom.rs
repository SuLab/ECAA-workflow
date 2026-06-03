//! `TaskNode -> AtomDefinition` reverse adapter.
//!
//! The forward adapter is `crate::workflow_contracts::from_atom`
//! (`AtomDefinition::from_atom`). This is its inverse for the federation
//! import path: an importer yields a `TaskNode` in the quarantine band,
//! and this adapter lowers it to an `AtomDefinition` so the SAME schema
//! + safety validators that gate local YAML atoms (run by
//! `AtomRegistry::with_external_overlay`) can vet it before it enters the
//! catalog the composer searches. Lossless enough that the safety
//! validator sees the same fields it would on a YAML atom — proven by
//! the `AtomDefinition -> TaskNode -> AtomDefinition` round-trip test.

use crate::atom::{AtomAssignee, AtomDefinition, AtomRole};
use crate::external_registry::{ExternalImportError, ExternalRegistryRef};
use crate::workflow_contracts::port::PortContract;
use crate::workflow_contracts::semantic_type::SemanticType;
use crate::workflow_contracts::task_node::TaskNode;

/// Pull the EDAM `data:`/`format:` IRI back out of a port. The forward
/// adapter (`from_atom::synthesize_*`) wrote `edam_data` into the port's
/// `semantic_type` as `SemanticType::OntologyTerm` and `edam_format`
/// into `physical_format.iri`; recover both. Non-EDAM (`ecaax:`/opaque)
/// semantic types yield `None` so a non-EDAM external port doesn't
/// masquerade as an EDAM data class.
fn edam_data_of(port: &PortContract) -> Option<String> {
    match &port.semantic_type {
        SemanticType::OntologyTerm { iri, .. }
            if iri.starts_with("data:") || iri.starts_with("format:") =>
        {
            Some(iri.clone())
        }
        _ => None,
    }
}

fn edam_format_of(port: &PortContract) -> Option<String> {
    port.physical_format.as_ref().and_then(|f| {
        if f.iri.starts_with("format:") || f.iri.starts_with("data:") {
            Some(f.iri.clone())
        } else {
            None
        }
    })
}

/// Lower an imported `TaskNode` (quarantine band) into an
/// `AtomDefinition` so the federation overlay can re-run the SAME
/// `_atom.schema.json` + `validate_atom_safety` validators local atoms
/// pass. Inverse of `AtomDefinition::from_atom`. Deterministic
/// (BTreeMap attributes, no timestamps). Returns
/// `ExternalImportError::MissingField` when the node lacks an id.
pub fn imported_node_to_atom(
    node: &TaskNode,
    src: &ExternalRegistryRef,
) -> Result<AtomDefinition, ExternalImportError> {
    if node.id.is_empty() {
        return Err(ExternalImportError::MissingField { field: "id".into() });
    }

    let edam_data = node
        .outputs
        .first()
        .and_then(edam_data_of)
        .or_else(|| node.inputs.first().and_then(edam_data_of));
    let edam_format = node.outputs.first().and_then(edam_format_of);

    // EDAM operation isn't carried on TaskNode; the schema requires a
    // non-empty operation IRI. Recover from attributes when the forward
    // adapter stashed it; otherwise synthesize a deterministic ecaax:
    // extension keyed off the id so the value passes the schema regex
    // `ecaax:[a-z][a-z0-9_]*[a-z0-9]`.
    let edam_operation = node
        .attributes
        .get("edam_operation")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("ecaax:imported_{}", sanitize_ecaax(&node.id)));

    // Record the source ref so provenance survives import. Goes into the
    // `attributes` map, which the schema permits.
    let mut attributes = std::collections::BTreeMap::new();
    attributes.insert(
        "external_registry_ref".to_string(),
        serde_json::to_value(src).unwrap_or(serde_json::Value::Null),
    );

    // Build off `test_default` so new `AtomDefinition` fields added by
    // later phases don't break this adapter, then overwrite every field
    // the import carries. `test_default` seeds placeholder EDAM values we
    // replace below.
    let mut atom = AtomDefinition::test_default(node.id.clone());
    atom.id = node.id.clone();
    atom.version = node.version.render();
    atom.role = AtomRole::Operation;
    atom.confirmatory = false;
    atom.discovery_kind = None;
    atom.description = if node.intent.is_empty() {
        format!("Imported tool {}", node.id)
    } else {
        node.intent.clone()
    };
    atom.edam_operation = edam_operation;
    atom.edam_data = edam_data;
    atom.edam_format = edam_format;
    atom.assignee = AtomAssignee::Agent;
    atom.depends_on = Vec::new();
    atom.excludes = Vec::new();
    atom.attributes = attributes;
    atom.joint_with = Vec::new();
    atom.inputs = node.inputs.clone();
    atom.outputs = node.outputs.clone();
    atom.method_choice = None;
    atom.resource_profile = None;
    atom.preferred_container = None;
    atom.claim_boundary = None;
    atom.iterate = None;
    atom.condition = None;
    atom.required_figures = Vec::new();
    atom.plot_stage_id = None;
    atom.figure_exempt = None;
    atom.expected_artifacts = Vec::new();
    atom.required_artifacts = Vec::new();
    atom.validators = Vec::new();
    atom.runtime_packages = crate::runtime_prereqs::RuntimePrereqs::default();
    atom.parameters = Vec::new();
    atom.provenance = None;
    atom.estimated_duration = None;
    atom.safety = node.safety.clone();
    Ok(atom)
}

/// Lowercase + underscore-collapse an id so it satisfies the schema's
/// `ecaax:` regex tail. Deterministic.
fn sanitize_ecaax(id: &str) -> String {
    let mut out: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    // Regex requires trailing [a-z0-9]; trim a trailing underscore.
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() || !out.starts_with(|c: char| c.is_ascii_lowercase()) {
        out = format!("x{out}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::AtomDefinition;
    use crate::external_registry::ExternalRegistryRef;
    use crate::workflow_contracts::task_node::TaskNode;

    #[test]
    fn round_trip_atom_node_atom_is_stable() {
        let atom = AtomDefinition::test_default("align_reads");
        let node = TaskNode::from_atom(&atom);
        let src = ExternalRegistryRef {
            registry: "local_cwl".into(),
            id: "align_reads".into(),
            version: Some("1.0.0".into()),
            url: None,
        };
        let back = imported_node_to_atom(&node, &src).expect("reverse adapter");
        // Load-bearing fields survive the round trip so the safety + schema
        // validators see what a YAML atom would.
        assert_eq!(back.id, atom.id);
        assert_eq!(back.version, atom.version);
        assert_eq!(back.role, atom.role);
        assert_eq!(back.edam_operation, atom.edam_operation);
        assert_eq!(back.edam_data, atom.edam_data);
        assert_eq!(back.edam_format, atom.edam_format);
        assert_eq!(back.safety, atom.safety);
        // The source ref is recorded so provenance survives import.
        assert!(back.attributes.contains_key("external_registry_ref"));
    }

    #[test]
    fn external_only_atom_never_reaches_production_lifecycle() {
        // An imported node stays Contracted/Unverified after reverse
        // adaptation -> the production-execution gate keeps it out of
        // production DAGs.
        use crate::workflow_contracts::lifecycle::LifecycleState;
        use crate::workflow_contracts::port::PortContract;
        let src = ExternalRegistryRef {
            registry: "local_cwl".into(),
            id: "ext_only".into(),
            version: None,
            url: None,
        };
        let mut node = TaskNode::skeleton("ext_only", "imported");
        node.outputs
            .push(PortContract::from_edam("out", Some("data:2978"), None));
        // Importer set quarantine band; the adapter copies nothing that
        // would promote it.
        assert_eq!(node.lifecycle_state, LifecycleState::Contracted);
        let atom = imported_node_to_atom(&node, &src).unwrap();
        // Re-materialize through from_atom -> still Contracted/Unverified.
        let back = TaskNode::from_atom(&atom);
        assert!(!back.lifecycle_state.allows_production_execution());
    }
}
