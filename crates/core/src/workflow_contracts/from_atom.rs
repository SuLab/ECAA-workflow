//! `AtomDefinition → TaskNode` conversion.
//!
//! This module is the thin adapter that lets every existing atom
//! materialize as a typed `TaskNode` without reauthoring config
//! YAML. `AtomDefinition` is the authoring surface; rich-port
//! `inputs:` / `outputs:` declarations on atoms augment but do not
//! replace this converter.
//!
//! Field preservation contract:
//!
//! - id, version → `TaskNode::id`, `TaskNode::version`
//! - description → `TaskNode::intent`
//! - edam_data, edam_format → input/output `PortContract` synthesized
//! - role → `TaskNode::attributes["role"]` + `discovery_kind` for
//!   `Discovery` atoms
//! - assignee → `Implementation::ManualProtocol` for `Sme` atoms;
//!   `Implementation::ContainerCommand` (or `Unimplemented`) for
//!   `Agent` atoms
//! - depends_on → preserved on `TaskNode::attributes["depends_on"]`
//!   so the composer's dependency-resolution pass can read it
//!   uniformly across atom-derived and freshly-authored TaskNodes
//! - excludes → `TaskNode::attributes["excludes"]`
//! - attributes → merged into `TaskNode::attributes`
//! - joint_with → `TaskNode::attributes["joint_with"]`
//! - method_choice → `TaskNode::attributes["method_choice"]`
//! - resource_profile → `TaskNode::attributes["resource_profile"]`
//! - preferred_container → `Implementation::ContainerCommand.image`
//!   (when assignee is `Agent`) + `attributes["preferred_container"]`
//!   for the lowering pass
//! - claim_boundary → `TaskNode::attributes["claim_boundary"]`
//! - condition → `TaskNode::attributes["condition"]`
//! - iterate → `TaskNode::attributes["iterate"]`
//! - required_figures → `TaskNode::attributes["required_figures"]`
//! - plot_stage_id → `TaskNode::attributes["plot_stage_id"]`
//! - expected_artifacts → `TaskNode::attributes["expected_artifacts"]`
//! - required_artifacts → `TaskNode::attributes["required_artifacts"]`
//! - validators → `TaskNode.validators` (typed; populated as
//!   `ValidatorRef { id, version: None, parameters: None }` per
//!   atom-declared obligation id; threaded through to
//!   `RequiredArtifact.validation_obligations` by the v4 lowering pass)
//! - runtime_packages → `TaskNode::attributes["runtime_packages"]`
//!
//! The attributes-bag strategy is intentional: it is shape-preserving
//! and reversible. Stable fields are promoted to typed first-class
//! members of `TaskNode` as the lowering pass matures; the bag
//! carries the remainder so the composer's dependency-resolution and
//! lowering passes can read all fields uniformly.

use std::collections::BTreeMap;

use crate::atom::{AtomAssignee, AtomDefinition, AtomRole, ContainerSpec};

use super::implementation::{Implementation, OciImageRef};
use super::port::PortContract;
use super::task_node::{Provenance, SemVer, TaskNode};

impl TaskNode {
    /// Convert an `AtomDefinition` into a `TaskNode` while
    /// preserving every authoring-surface field. Reversible via
    /// the attributes bag for fields that don't yet have typed
    /// homes on `TaskNode`.
    pub fn from_atom(atom: &AtomDefinition) -> Self {
        let inputs = synthesize_inputs(atom);
        let outputs = synthesize_outputs(atom);
        let implementation = synthesize_implementation(atom);
        let attributes = preserve_attributes(atom);

        Self {
            id: atom.id.clone(),
            human_name: atom.id.clone(),
            machine_name: atom.id.clone(),
            status: super::lifecycle::NodeStatus::Active,
            intent: atom.description.clone(),
            inputs,
            outputs,
            preconditions: Vec::new(),
            postconditions: Vec::new(),
            assumptions: Vec::new(),
            implementation,
            // Populated from atom-declared `validators:` YAML field.
            // The v4 lowering pass propagates these to
            // `RequiredArtifact.validation_obligations` so the harness
            // hook fires after task completion.
            validators: atom
                .validators
                .iter()
                .map(|id| super::evidence::ValidatorRef {
                    id: id.clone(),
                    version: None,
                    parameters: None,
                })
                .collect(),
            evidence: super::evidence::EvidenceSet::default(),
            risk: super::evidence::RiskClass::default(),
            provenance: Provenance {
                source: Some(format!("config/stage-atoms/{}.yaml", atom.id)),
                ..Provenance::default()
            },
            version: SemVer::parse_or_default(&atom.version),
            // Atoms default to `Contracted`; the lifecycle state is
            // promoted when verification evidence is attached.
            lifecycle_state: super::lifecycle::LifecycleState::Contracted,
            trust_level: super::lifecycle::TrustLevel::Unverified,
            deprecation: None,
            attributes,
        }
    }
}

/// Synthesize input ports from `edam_data` for atoms that don't
/// declare rich `inputs:`. `Aggregator` atoms get an empty input
/// vector by design (per CLAUDE.md "discover_*/validate_*/aggregator
/// are self-describing"). `Discovery` and `Validation` atoms get
/// one synthesized input port mirroring their upstream source.
fn synthesize_inputs(atom: &AtomDefinition) -> Vec<PortContract> {
    if matches!(atom.role.default_behavior_class(), AtomRole::Aggregator) {
        // Aggregator inputs are resolved at fan-in; no static port
        // shape to synthesize.
        return Vec::new();
    }
    match atom.edam_data.as_deref() {
        Some(iri) => vec![PortContract::from_edam("input", Some(iri), None)],
        None => Vec::new(),
    }
}

/// Synthesize output ports from `edam_data` + `edam_format` for
/// atoms that don't declare rich `outputs:`.
fn synthesize_outputs(atom: &AtomDefinition) -> Vec<PortContract> {
    let edam_data = atom.edam_data.as_deref();
    let edam_format = atom.edam_format.as_deref();
    if edam_data.is_none() && edam_format.is_none() {
        return Vec::new();
    }
    vec![PortContract::from_edam("output", edam_data, edam_format)]
}

/// Synthesize the implementation. SME-assignee atoms become
/// `ManualProtocol`; Agent atoms with a `preferred_container`
/// become `ContainerCommand`; everything else becomes
/// `Unimplemented` and is filled in by the harness/agent.
fn synthesize_implementation(atom: &AtomDefinition) -> Implementation {
    if matches!(atom.assignee, AtomAssignee::Sme) {
        return Implementation::ManualProtocol {
            sop_ref: format!("sme:{}", atom.id),
        };
    }
    if let Some(container) = &atom.preferred_container {
        return Implementation::ContainerCommand {
            image: oci_from_container_spec(container),
            // Command template is supplied by the agent per
            // existing `agent-claude.sh` flow; the IR carries an
            // empty template so the harness keeps dispatching the
            // same way it does today.
            command_template: Vec::new(),
        };
    }
    Implementation::Unimplemented
}

fn oci_from_container_spec(c: &ContainerSpec) -> OciImageRef {
    OciImageRef {
        image: c.image.clone(),
        tag: c.tag.clone(),
        digest: c.digest.clone(),
        arch: if c.arch.is_empty() {
            vec!["amd64".into()]
        } else {
            c.arch.clone()
        },
        gpu: c.gpu_required,
    }
}

/// Preserve all the atom-shape fields that don't yet have typed
/// homes on `TaskNode`. The attributes bag is read by the
/// composer's dependency-resolution and lowering passes.
fn preserve_attributes(atom: &AtomDefinition) -> BTreeMap<String, serde_json::Value> {
    let mut a: BTreeMap<String, serde_json::Value> = BTreeMap::new();

    // Original AtomDefinition attributes are merged first so
    // synthesized keys can override (none currently do, but the
    // contract is "synthesized > authored" for namespacing safety).
    for (k, v) in &atom.attributes {
        a.insert(k.clone(), v.clone());
    }

    a.insert(
        "role".into(),
        serde_json::to_value(atom.role).unwrap_or(serde_json::Value::Null),
    );

    if let Some(kind) = &atom.discovery_kind {
        a.insert(
            "discovery_kind".into(),
            serde_json::Value::String(kind.clone()),
        );
    }
    a.insert(
        "assignee".into(),
        serde_json::to_value(atom.assignee).unwrap_or(serde_json::Value::Null),
    );

    if !atom.depends_on.is_empty() {
        a.insert(
            "depends_on".into(),
            serde_json::to_value(&atom.depends_on).unwrap_or(serde_json::Value::Null),
        );
    }
    if !atom.excludes.is_empty() {
        a.insert(
            "excludes".into(),
            serde_json::to_value(&atom.excludes).unwrap_or(serde_json::Value::Null),
        );
    }
    if !atom.joint_with.is_empty() {
        a.insert(
            "joint_with".into(),
            serde_json::to_value(&atom.joint_with).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(method_choice) = &atom.method_choice {
        a.insert(
            "method_choice".into(),
            serde_json::to_value(method_choice).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(rp) = &atom.resource_profile {
        a.insert(
            "resource_profile".into(),
            serde_json::to_value(rp).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(c) = &atom.preferred_container {
        a.insert(
            "preferred_container".into(),
            serde_json::to_value(c).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(cb) = &atom.claim_boundary {
        a.insert(
            "claim_boundary".into(),
            serde_json::Value::String(cb.clone()),
        );
    }
    if let Some(cond) = &atom.condition {
        a.insert("condition".into(), serde_json::Value::String(cond.clone()));
    }
    if let Some(it) = &atom.iterate {
        a.insert(
            "iterate".into(),
            serde_json::to_value(it).unwrap_or(serde_json::Value::Null),
        );
    }
    if !atom.required_figures.is_empty() {
        a.insert(
            "required_figures".into(),
            serde_json::to_value(&atom.required_figures).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(p) = &atom.plot_stage_id {
        a.insert("plot_stage_id".into(), serde_json::Value::String(p.clone()));
    }
    if !atom.expected_artifacts.is_empty() {
        a.insert(
            "expected_artifacts".into(),
            serde_json::to_value(&atom.expected_artifacts).unwrap_or(serde_json::Value::Null),
        );
    }
    if !atom.required_artifacts.is_empty() {
        a.insert(
            "required_artifacts".into(),
            serde_json::to_value(&atom.required_artifacts).unwrap_or(serde_json::Value::Null),
        );
    }
    // Runtime packages always serialize, but we only stash when
    // the atom declares anything non-default — mirrors the
    // skip_serializing_if predicate on AtomDefinition itself.
    let rp = &atom.runtime_packages;
    let has_rp = !rp.system_packages.is_empty()
        || !rp.language_packages.is_empty()
        || !rp.system_check.is_empty()
        || rp.base_image.is_some()
        || rp.modality.is_some();
    if has_rp {
        a.insert(
            "runtime_packages".into(),
            serde_json::to_value(rp).unwrap_or(serde_json::Value::Null),
        );
    }

    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::{
        AtomAssignee, AtomDefinition, AtomRole, ContainerSource, ContainerSpec, IterateConvergence,
        IterateConvergenceOp, IterateMaxAction, IterateSpec, JointlyWithConstraint,
        MethodChoiceRef, NetworkPolicy, ResourceProfile,
    };
    use crate::runtime_prereqs::RuntimePrereqs;
    use crate::workflow_contracts::implementation::Implementation;
    use crate::workflow_contracts::lifecycle::{LifecycleState, NodeStatus, TrustLevel};
    use crate::workflow_contracts::semantic_type::SemanticType;

    fn minimal_atom(id: &str) -> AtomDefinition {
        AtomDefinition {
            id: id.into(),
            version: "1.0.0".into(),
            role: AtomRole::Operation,
            discovery_kind: None,
            description: format!("Atom {id}"),
            edam_operation: "operation:0292".into(),
            edam_data: Some("data:0863".into()),
            edam_format: Some("format:2572".into()),
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
            runtime_packages: RuntimePrereqs::default(),
            safety: crate::atom::SafetyPolicy::default(),
        }
    }

    #[test]
    fn from_atom_preserves_id_version_intent() {
        let atom = minimal_atom("align_reads");
        let node = TaskNode::from_atom(&atom);
        assert_eq!(node.id, "align_reads");
        assert_eq!(node.machine_name, "align_reads");
        assert_eq!(node.intent, "Atom align_reads");
        assert_eq!(node.version.major, 1);
        assert_eq!(node.version.minor, 0);
        assert_eq!(node.version.patch, 0);
    }

    #[test]
    fn from_atom_synthesizes_typed_ports_from_edam() {
        let atom = minimal_atom("align_reads");
        let node = TaskNode::from_atom(&atom);
        assert_eq!(node.inputs.len(), 1);
        assert_eq!(node.outputs.len(), 1);
        assert_eq!(node.inputs[0].name, "input");
        assert_eq!(node.outputs[0].name, "output");
        assert!(matches!(
            node.outputs[0].semantic_type,
            SemanticType::OntologyTerm { ref iri, .. } if iri == "data:0863"
        ));
        assert!(node.outputs[0].physical_format.is_some());
    }

    #[test]
    fn from_atom_aggregator_has_no_static_inputs() {
        let mut atom = minimal_atom("aggregate_counts");
        atom.role = AtomRole::Aggregator;
        atom.edam_data = None;
        let node = TaskNode::from_atom(&atom);
        assert!(node.inputs.is_empty());
    }

    #[test]
    fn from_atom_sme_assignee_becomes_manual_protocol() {
        let mut atom = minimal_atom("sme_review");
        atom.assignee = AtomAssignee::Sme;
        let node = TaskNode::from_atom(&atom);
        assert!(matches!(
            node.implementation,
            Implementation::ManualProtocol { ref sop_ref } if sop_ref == "sme:sme_review"
        ));
    }

    #[test]
    #[allow(deprecated)]
    fn from_atom_with_container_becomes_container_command() {
        let mut atom = minimal_atom("align_reads");
        atom.preferred_container = Some(ContainerSpec {
            image: "ghcr.io/scripps/bio-base".into(),
            tag: "v0.4.0".into(),
            digest: "sha256:abc".into(),
            arch: vec!["amd64".into()],
            gpu_required: false,
            network: Some(NetworkPolicy::Bridge),
            source: ContainerSource::Image,
        });
        let node = TaskNode::from_atom(&atom);
        match node.implementation {
            Implementation::ContainerCommand { image, .. } => {
                assert_eq!(image.image, "ghcr.io/scripps/bio-base");
                assert_eq!(image.tag, "v0.4.0");
                assert_eq!(image.digest, "sha256:abc");
            }
            other => panic!("expected ContainerCommand, got {other:?}"),
        }
    }

    #[test]
    fn from_atom_without_container_is_unimplemented() {
        let atom = minimal_atom("align_reads");
        let node = TaskNode::from_atom(&atom);
        assert!(matches!(node.implementation, Implementation::Unimplemented));
    }

    #[test]
    fn from_atom_preserves_full_field_set() {
        let mut atom = minimal_atom("everything");
        atom.discovery_kind = Some("method".into());
        atom.role = AtomRole::Discovery;
        atom.depends_on = vec!["upstream_a".into(), "upstream_b".into()];
        atom.excludes = vec!["competing_atom".into()];
        atom.attributes
            .insert("speed".into(), serde_json::json!("fast"));
        atom.joint_with = vec![JointlyWithConstraint {
            lhs: "src_a".into(),
            rhs: "src_b".into(),
        }];
        atom.method_choice = Some(MethodChoiceRef {
            deferred_to: "discover_method".into(),
        });
        atom.resource_profile = Some(ResourceProfile {
            cpu: Some("heavy".into()),
            memory: Some("large".into()),
            gpu: false,
            runtime_class: Some("hours".into()),
        });
        atom.claim_boundary = Some("Internal QC only".into());
        atom.condition = Some("upstream.required == true".into());
        atom.iterate = Some(IterateSpec {
            max_iterations: 10,
            min_iterations: 2,
            convergence: IterateConvergence {
                metric_source: "result.silhouette".into(),
                operator: IterateConvergenceOp::Gt,
                threshold: 0.7,
                consecutive_iterations: 2,
            },
            on_max_iterations: IterateMaxAction::Block,
            best_selector: None,
        });
        atom.required_figures = vec!["fig1".into()];
        atom.plot_stage_id = Some("plotting.normalization".into());
        atom.expected_artifacts = vec!["out.tsv".into()];

        let node = TaskNode::from_atom(&atom);

        // All preserved keys present.
        let a = &node.attributes;
        assert!(a.contains_key("role"));
        assert!(a.contains_key("discovery_kind"));
        assert!(a.contains_key("assignee"));
        assert!(a.contains_key("depends_on"));
        assert!(a.contains_key("excludes"));
        assert!(a.contains_key("joint_with"));
        assert!(a.contains_key("method_choice"));
        assert!(a.contains_key("resource_profile"));
        assert!(a.contains_key("claim_boundary"));
        assert!(a.contains_key("condition"));
        assert!(a.contains_key("iterate"));
        assert!(a.contains_key("required_figures"));
        assert!(a.contains_key("plot_stage_id"));
        assert!(a.contains_key("expected_artifacts"));
        // Author-supplied attribute is preserved too.
        assert_eq!(a.get("speed").unwrap(), &serde_json::json!("fast"));
    }

    #[test]
    fn from_atom_lifecycle_defaults_to_contracted() {
        let atom = minimal_atom("any");
        let node = TaskNode::from_atom(&atom);
        assert!(matches!(node.lifecycle_state, LifecycleState::Contracted));
        assert!(matches!(node.trust_level, TrustLevel::Unverified));
        assert!(matches!(node.status, NodeStatus::Active));
    }

    #[test]
    fn from_atom_records_provenance_source() {
        let atom = minimal_atom("align_reads");
        let node = TaskNode::from_atom(&atom);
        assert_eq!(
            node.provenance.source.as_deref(),
            Some("config/stage-atoms/align_reads.yaml")
        );
    }
}
