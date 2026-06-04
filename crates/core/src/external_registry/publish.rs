//! Publish path (F5): an atom's PortContract IS the cross-provider data
//! contract. `atom_to_published_descriptor` emits a deterministic JSON
//! descriptor whose snapshot shape is byte-identical to what
//! `ExternalRegistryStore::load_from_dir` consumes — so one node's
//! published catalog is directly importable by another (round-trip
//! symmetry: publish output == import input).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::atom::{AtomDefinition, SafetyPolicy};
use crate::external_registry::RegistrySnapshot;
use crate::workflow_contracts::port::PortContract;

/// MCP-shaped published tool descriptor. The typed input/output
/// `PortContract`s are the inter-tool data contract; `SafetyPolicy` is
/// the safety contract; `depends_on` is the inter-tool dependency
/// declaration the paper's MCP extension proposes. Deterministic
/// (BTreeMap-ordered fields via the atom, no timestamps).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct PublishedToolDescriptor {
    /// Tool id (== atom id).
    pub id: String,
    /// Semver string.
    pub version: String,
    /// Human description.
    pub description: String,
    /// EDAM operation IRI.
    pub edam_operation: String,
    /// Typed input ports (the data contract).
    pub inputs: Vec<PortContract>,
    /// Typed output ports (the data contract).
    pub outputs: Vec<PortContract>,
    /// Inter-tool dependency declaration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Safety contract.
    pub safety: SafetyPolicy,
}

/// Project an atom into a deterministic published descriptor. The
/// `PortContract`s ARE the cross-provider data contract. When the atom
/// declares only the legacy `edam_data`/`edam_format` pair, synthesize
/// the same coarse ports `TaskNode::from_atom` would so import is symmetric.
pub fn atom_to_published_descriptor(atom: &AtomDefinition) -> PublishedToolDescriptor {
    let outputs = if !atom.outputs.is_empty() {
        atom.outputs.clone()
    } else if atom.edam_data.is_some() || atom.edam_format.is_some() {
        vec![PortContract::from_edam(
            "output",
            atom.edam_data.as_deref(),
            atom.edam_format.as_deref(),
        )]
    } else {
        Vec::new()
    };
    let inputs = if !atom.inputs.is_empty() {
        atom.inputs.clone()
    } else if let Some(data) = atom.edam_data.as_deref() {
        vec![PortContract::from_edam("input", Some(data), None)]
    } else {
        Vec::new()
    };
    PublishedToolDescriptor {
        id: atom.id.clone(),
        version: atom.version.clone(),
        description: atom.description.clone(),
        edam_operation: atom.edam_operation.clone(),
        inputs,
        outputs,
        depends_on: atom.depends_on.clone(),
        safety: atom.safety.clone(),
    }
}

impl PublishedToolDescriptor {
    /// Re-wrap as a `local_cwl` snapshot so a published catalog is
    /// directly importable by another node (round-trip symmetry). The
    /// metadata mirrors the shape `LocalCwlImporter::import` reads:
    /// `id`/`label`/`inputs`/`outputs` with EDAM `type`/`format`.
    pub fn into_local_cwl_snapshot(&self) -> RegistrySnapshot {
        let port_json = |p: &PortContract| {
            let mut o = serde_json::Map::new();
            o.insert("id".into(), serde_json::Value::String(p.name.clone()));
            if let crate::workflow_contracts::semantic_type::SemanticType::OntologyTerm {
                iri,
                ..
            } = &p.semantic_type
            {
                // Importer parses `edam:data_N` / `edam:format_N`; emit
                // the inverse of its `data:`/`format:` rewrite.
                let t = iri
                    .replace("data:", "edam:data_")
                    .replace("format:", "edam:format_");
                o.insert("type".into(), serde_json::Value::String(t));
            }
            if let Some(f) = &p.physical_format {
                let fr = f
                    .iri
                    .replace("format:", "edam:format_")
                    .replace("data:", "edam:data_");
                o.insert("format".into(), serde_json::Value::String(fr));
            }
            serde_json::Value::Object(o)
        };
        let metadata = serde_json::json!({
            "id": self.id,
            "label": self.description,
            "inputs": self.inputs.iter().map(port_json).collect::<Vec<_>>(),
            "outputs": self.outputs.iter().map(port_json).collect::<Vec<_>>(),
        });
        RegistrySnapshot {
            // Clock-free snapshot id: deterministic from the tool id+version.
            snapshot_id: format!("published-{}-{}", self.id, self.version),
            registry: "local_cwl".into(),
            id: self.id.clone(),
            metadata,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::AtomDefinition;

    #[test]
    fn descriptor_is_deterministic() {
        let atom = AtomDefinition::test_default("align_reads");
        let a = atom_to_published_descriptor(&atom);
        let b = atom_to_published_descriptor(&atom);
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    #[test]
    fn descriptor_preserves_safety_and_ports() {
        let atom = AtomDefinition::test_default("align_reads");
        let d = atom_to_published_descriptor(&atom);
        assert_eq!(d.id, "align_reads");
        assert_eq!(d.version, atom.version);
        assert_eq!(d.safety, atom.safety);
        // Output port carries the EDAM data contract.
        assert!(!d.outputs.is_empty() || atom.edam_data.is_none());
    }

    #[test]
    fn publish_then_import_reconstructs_composable_atom() {
        use crate::external_registry::{
            ExternalRegistryRef, ExternalRegistryStore, ImporterRegistry,
        };
        let atom = AtomDefinition::test_default("align_reads");
        let descriptor = atom_to_published_descriptor(&atom);
        // Re-wrap the descriptor as a snapshot (publish output == import
        // input). The local_cwl importer reads `id`/`label`/`inputs`/`outputs`.
        let snapshot = descriptor.into_local_cwl_snapshot();
        let mut store = ExternalRegistryStore::new();
        store.insert(snapshot);
        let importers = ImporterRegistry::with_builtin();
        let refs = [ExternalRegistryRef {
            registry: "local_cwl".into(),
            id: "align_reads".into(),
            version: None,
            url: None,
        }];
        let (atoms, errors) = store.resolve(&refs, &importers);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(atoms.len(), 1);
        // The reconstructed atom passes the same overlay gate as a local atom.
        let base = crate::atom_registry::AtomRegistry::default();
        base.with_external_overlay(atoms, crate::external_registry::RegistryTier::Community)
            .expect("reconstructed atom is schema+safety valid");
    }
}
