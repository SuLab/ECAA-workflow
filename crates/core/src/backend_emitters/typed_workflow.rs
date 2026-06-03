//! Paper D.2 typed-workflow artifact (`runtime/workflow-typed.json`).
//!
//! A pure projection of the proof-carrying IR (`WorkflowDag`) into the
//! arXiv:2603.06394 §D.2 shape. Byte-deterministic: declared-order
//! `Vec`/`BTreeMap`, `estimated_duration` from a static `ResourceClass`
//! lookup (NOT wall-clock). Does NOT touch `WORKFLOW.json` bytes or the
//! `depends_on` readiness gate. See `lower_to_typed_workflow`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Paper §D.2 top-level workflow object. Byte-deterministic projection
/// of `WorkflowDag`. The authoritative on-disk graph stays `WORKFLOW.json`;
/// this is an additive, machine-checkable companion at `runtime/workflow-typed.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[non_exhaustive]
pub struct TypedWorkflow {
    /// Stable workflow id (mirrors `WorkflowDag.id` / `DAG.workflow_id`).
    pub workflow_id: String,
    /// Human-facing name. Today equals `workflow_id`; reserved for a
    /// distinct display label.
    pub name: String,
    /// M4 — self-describing registry pin: the atom-registry snapshot id the
    /// composer planned against (`AtomRegistry::snapshot_id`). `None` when
    /// the producing path did not pin a snapshot (legacy / unpinned
    /// sessions). Never fabricated — a missing pin stays `None` so the
    /// artifact remains byte-reproducible across hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub atom_registry_snapshot_id: Option<String>,
    /// One per `TaskNode`, sorted by `step_id`.
    pub steps: Vec<TypedStep>,
    /// One per `EdgeContract`, sorted by `edge_id`.
    pub edges: Vec<TypedEdge>,
    /// Port-level wiring projected from edges, sorted by `(target_step, target_input)`.
    pub parameter_mappings: Vec<ParameterMapping>,
    /// Top-level typed parameters (W4 — intake facts only), sorted by `name`.
    pub parameters: Vec<WorkflowParameter>,
    /// Top-level validation rules (W4 — port/node constraints), sorted by `rule_id`.
    pub validation_rules: Vec<ValidationRule>,
    /// Deterministic metadata (W3).
    pub metadata: WorkflowMetadata,
}

/// Paper §D.2 step. `tool_id` is the source atom; `parameters` is a curated
/// subset of `TaskNode.attributes` (method axes / required figures), NEVER a
/// synthesized method choice (method-neutrality).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[non_exhaustive]
pub struct TypedStep {
    /// `TaskNode.id`.
    pub step_id: String,
    /// `TaskNode.attributes["atom_id"]` (the key the lowering reads).
    /// `None` when the node carries no atom id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tool_id: Option<String>,
    /// Curated step parameters (sorted-key map). Empty for steps with no
    /// method/figure attributes.
    #[ts(type = "Record<string, unknown>")]
    pub parameters: BTreeMap<String, serde_json::Value>,
    /// Incoming `from_node`s, sorted + deduped. Equals the lowered
    /// `Task.depends_on` for this step (W6 asserts equality).
    pub dependencies: Vec<String>,
    /// Static `ResourceClass`-derived estimate in seconds. Deterministic
    /// lookup, NOT wall-clock.
    pub estimated_duration: u64,
}

/// Paper §D.2 edge with full port-level endpoints (the data
/// `lower_to_workflow_json` collapses into `depends_on`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[non_exhaustive]
pub struct TypedEdge {
    /// Deterministic id: `"{from_node}.{from_port}->{to_node}.{to_port}"`.
    pub edge_id: String,
    /// `EdgeContract.from_node`.
    pub source_node_id: String,
    /// `EdgeContract.to_node`.
    pub target_node_id: String,
    /// `EdgeContract.from_port` — the producer port name preserved.
    pub source_output: String,
    /// `EdgeContract.to_port` — the consumer port name preserved.
    pub target_input: String,
}

/// Port wiring projected from an edge. Mirrors the edge endpoints but keyed
/// for consumption ("what feeds this step's input").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[non_exhaustive]
pub struct ParameterMapping {
    /// Producing step id.
    pub source_step: String,
    /// Producing output port.
    pub source_output: String,
    /// Consuming step id.
    pub target_step: String,
    /// Consuming input port.
    pub target_input: String,
}

/// Top-level typed parameter (W4). Sourced ONLY from intake facts the SME
/// supplied — organism/genome/sample counts — never a synthesized method.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[non_exhaustive]
pub struct WorkflowParameter {
    /// Parameter name (e.g. `organism`, `sample_count`, `cell_count`).
    pub name: String,
    /// JSON-schema-ish type tag (`string` / `integer`).
    #[serde(rename = "type")]
    pub r#type: String,
    /// The intake value (canonicalized; no timestamps).
    #[ts(type = "unknown")]
    pub value: serde_json::Value,
    /// Provenance of the value. Always `"intake"` for now.
    pub source: String,
}

/// Top-level validation rule (W4). Strictly richer than the paper: preserves
/// the opaque CEL `expression` and the Hard/Soft/Warn `severity`. The
/// compiler does NOT compile the expression — downstream consumers must not
/// assume it is pre-validated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[non_exhaustive]
pub struct ValidationRule {
    /// Composite stable id: `"{target_step}:{constraint_id}"`.
    pub rule_id: String,
    /// Step this rule constrains.
    pub target_step: String,
    /// Opaque CEL/schema expression (uncompiled). `None` when the
    /// constraint declared no expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expression: Option<String>,
    /// `"hard"` / `"soft"` / `"warn"`.
    pub severity: String,
}

/// Paper §D.2 metadata (W3). All fields deterministic from compiler state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[non_exhaustive]
pub struct WorkflowMetadata {
    /// Deterministic bucket from step count: `<5 simple / 5..=15 moderate /
    /// >15 complex`. NOT a calibrated metric — purely a size band.
    pub complexity: String,
    /// Sorted+deduped tag set (modality, project class, distinct TaskKind labels).
    pub tags: Vec<String>,
    /// `[modality_stratum]` when known, else empty.
    pub categories: Vec<String>,
    /// Intake goal-pattern modifiers (sorted keys), e.g. `per-sample`,
    /// `with_pathway_enrichment`. Empty when no goal modifiers were captured.
    pub use_cases: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_workflow_serializes_d2_top_level_keys() {
        let wf = TypedWorkflow {
            workflow_id: "wf_x".into(),
            name: "wf_x".into(),
            atom_registry_snapshot_id: None,
            steps: vec![],
            edges: vec![],
            parameter_mappings: vec![],
            parameters: vec![],
            validation_rules: vec![],
            metadata: WorkflowMetadata::default(),
        };
        let v = serde_json::to_value(&wf).unwrap();
        for key in [
            "workflow_id",
            "name",
            "steps",
            "edges",
            "parameter_mappings",
            "parameters",
            "validation_rules",
            "metadata",
        ] {
            assert!(v.get(key).is_some(), "missing D.2 key {key}");
        }
    }
}
