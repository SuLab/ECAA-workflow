//! Paper D.2 typed-workflow artifact (`runtime/workflow-typed.json`).
//!
//! A pure projection of the proof-carrying IR (`WorkflowDag`) into the
//! arXiv:2603.06394 §D.2 shape. Byte-deterministic: declared-order
//! `Vec`/`BTreeMap`, `estimated_duration` from a static `ResourceClass`
//! lookup (NOT wall-clock). Does NOT touch `WORKFLOW.json` bytes or the
//! `depends_on` readiness gate. See `lower_to_typed_workflow`.

use crate::dag::ResourceClass;
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Inputs the projection needs beyond the `WorkflowDag`. Constructed
/// identically by both emit paths so the projection is never forked.
#[derive(Debug, Clone, Default)]
pub struct TypedWorkflowContext<'a> {
    /// Classification result for W3 metadata. `None` on bare-builder paths.
    pub classification: Option<&'a crate::classify::ClassificationResult>,
    /// Intake facts for W4 parameters. `None` when the SME supplied none.
    pub intake_facts: Option<&'a crate::intake_facts::IntakeFacts>,
    /// M4 registry pin (`AtomRegistry::snapshot_id`). `None` when unpinned.
    pub atom_snapshot_id: Option<String>,
}

/// Static, deterministic per-`ResourceClass` duration estimate (seconds).
/// NOT a calibrated SLA — a coarse band so the D.2 `estimated_duration`
/// field is populated without `SystemTime::now()`. Bands documented here so
/// they are not mistaken for measured runtimes.
fn estimated_duration_secs(rc: &ResourceClass) -> u64 {
    match rc {
        ResourceClass::IoHeavy => 600,      // 10 min — disk-bound
        ResourceClass::CpuHeavy => 1800,    // 30 min — typical analysis
        ResourceClass::MemoryHeavy => 5400, // 90 min — large matrices
        ResourceClass::Gpu => 7200,         // 120 min — training/structure
    }
}

/// Single source of truth for the D.2 projection. Pure, no IO, no clock.
/// Called by BOTH emit paths (conversation full-fidelity `WorkflowDag`;
/// core/builder reconstructed `WorkflowDag` via `dag_to_workflow_dag`).
pub fn lower_to_typed_workflow(dag: &WorkflowDag, ctx: &TypedWorkflowContext) -> TypedWorkflow {
    // Incoming-edge index (reuse the same shape as lower_to_workflow_json).
    let mut incoming: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for edge in &dag.edges {
        incoming
            .entry(edge.to_node.clone())
            .or_default()
            .push(edge.from_node.clone());
    }
    for v in incoming.values_mut() {
        v.sort();
        v.dedup();
    }

    // Steps, sorted by id.
    let mut sorted_nodes: Vec<&TaskNode> = dag.nodes.iter().collect();
    sorted_nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let mut steps: Vec<TypedStep> = Vec::with_capacity(sorted_nodes.len());
    for node in &sorted_nodes {
        let tool_id = node
            .attributes
            .get("atom_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let dependencies = incoming.get(&node.id).cloned().unwrap_or_default();
        let resource_class = node
            .attributes
            .get("resource_profile")
            .and_then(|v| v.get("cpu"))
            .and_then(|v| v.as_str())
            .map(|cpu| match cpu {
                "very_heavy" | "heavy" => ResourceClass::MemoryHeavy,
                _ => ResourceClass::CpuHeavy,
            })
            .unwrap_or_default();
        steps.push(TypedStep {
            step_id: node.id.clone(),
            tool_id,
            parameters: curate_step_parameters(node),
            dependencies,
            estimated_duration: estimated_duration_secs(&resource_class),
        });
    }

    // Edges + parameter_mappings, sorted deterministically.
    let mut sorted_edges: Vec<&_> = dag.edges.iter().collect();
    sorted_edges.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.from_port.cmp(&b.from_port))
            .then_with(|| a.to_node.cmp(&b.to_node))
            .then_with(|| a.to_port.cmp(&b.to_port))
    });
    let mut edges: Vec<TypedEdge> = Vec::with_capacity(sorted_edges.len());
    let mut parameter_mappings: Vec<ParameterMapping> = Vec::with_capacity(sorted_edges.len());
    for e in &sorted_edges {
        edges.push(TypedEdge {
            edge_id: format!("{}.{}->{}.{}", e.from_node, e.from_port, e.to_node, e.to_port),
            source_node_id: e.from_node.clone(),
            target_node_id: e.to_node.clone(),
            source_output: e.from_port.clone(),
            target_input: e.to_port.clone(),
        });
        parameter_mappings.push(ParameterMapping {
            source_step: e.from_node.clone(),
            source_output: e.from_port.clone(),
            target_step: e.to_node.clone(),
            target_input: e.to_port.clone(),
        });
    }
    edges.sort_by(|a, b| a.edge_id.cmp(&b.edge_id));
    parameter_mappings.sort_by(|a, b| {
        a.target_step
            .cmp(&b.target_step)
            .then_with(|| a.target_input.cmp(&b.target_input))
            .then_with(|| a.source_step.cmp(&b.source_step))
    });

    let metadata = build_metadata(dag, &steps, ctx.classification);
    TypedWorkflow {
        workflow_id: dag.id.clone(),
        name: dag.id.clone(),
        atom_registry_snapshot_id: ctx.atom_snapshot_id.clone(),
        steps,
        edges,
        parameter_mappings,
        parameters: build_parameters(ctx.intake_facts),
        validation_rules: build_validation_rules(dag),
        metadata,
    }
}

/// W3 — deterministic metadata from compiler state. NOT calibrated.
fn build_metadata(
    dag: &WorkflowDag,
    steps: &[TypedStep],
    classification: Option<&crate::classify::ClassificationResult>,
) -> WorkflowMetadata {
    let complexity = match steps.len() {
        0..=4 => "simple",
        5..=15 => "moderate",
        _ => "complex",
    }
    .to_string();

    let mut categories: Vec<String> = Vec::new();
    let mut tags: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut use_cases: Vec<String> = Vec::new();
    if let Some(cls) = classification {
        if !cls.modality.is_empty() {
            tags.insert(cls.modality.clone());
        }
        if let Some(stratum) = crate::strata::modality_stratum(&cls.modality) {
            categories.push(stratum);
        }
        if let Some(goal) = &cls.goal {
            for k in goal.modifiers.keys() {
                use_cases.push(k.clone());
            }
        }
    }
    // Distinct role labels via node role attribute.
    for n in &dag.nodes {
        if let Some(role) = n.attributes.get("role").and_then(|v| v.as_str()) {
            tags.insert(role.to_string());
        }
    }
    use_cases.sort();
    use_cases.dedup();
    WorkflowMetadata {
        complexity,
        tags: tags.into_iter().collect(), // BTreeSet → already sorted+unique
        categories,
        use_cases,
    }
}

/// W4 — top-level parameters from intake facts ONLY (SME-named). No method
/// synthesis (method-neutrality): we never emit a parameter the SME did not
/// supply. Sorted by name for byte-stability.
fn build_parameters(
    intake_facts: Option<&crate::intake_facts::IntakeFacts>,
) -> Vec<WorkflowParameter> {
    let mut out: Vec<WorkflowParameter> = Vec::new();
    let Some(f) = intake_facts else { return out };
    let mut push_str = |name: &str, val: &Option<String>| {
        if let Some(v) = val {
            out.push(WorkflowParameter {
                name: name.to_string(),
                r#type: "string".into(),
                value: serde_json::Value::String(v.clone()),
                source: "intake".into(),
            });
        }
    };
    push_str("organism", &f.organism_name);
    let mut push_u32 = |name: &str, val: Option<u32>| {
        if let Some(v) = val {
            out.push(WorkflowParameter {
                name: name.to_string(),
                r#type: "integer".into(),
                value: serde_json::json!(v),
                source: "intake".into(),
            });
        }
    };
    push_u32("sample_count", f.sample_count);
    push_u32("coverage_depth", f.coverage_depth);
    push_u32("cell_count", f.cell_count);
    // `f.methods` is SME-named methods — included verbatim, never invented.
    for (i, m) in f.methods.iter().enumerate() {
        out.push(WorkflowParameter {
            name: format!("sme_method_{i}"),
            r#type: "string".into(),
            value: serde_json::Value::String(m.clone()),
            source: "intake".into(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// W4 — validation rules from each node's pre/postcondition Constraints.
/// Preserves the opaque CEL `expression` and Hard/Soft/Warn severity.
/// Sorted by rule_id.
fn build_validation_rules(dag: &WorkflowDag) -> Vec<ValidationRule> {
    use crate::workflow_contracts::port::ConstraintSeverity;
    let sev = |s: &ConstraintSeverity| match s {
        ConstraintSeverity::Hard => "hard",
        ConstraintSeverity::Soft => "soft",
        ConstraintSeverity::Warn => "warn",
    };
    let mut out: Vec<ValidationRule> = Vec::new();
    let mut sorted_nodes: Vec<&TaskNode> = dag.nodes.iter().collect();
    sorted_nodes.sort_by(|a, b| a.id.cmp(&b.id));
    for n in sorted_nodes {
        for c in n.preconditions.iter().chain(n.postconditions.iter()) {
            out.push(ValidationRule {
                rule_id: format!("{}:{}", n.id, c.id),
                target_step: n.id.clone(),
                expression: c.expression.clone(),
                severity: sev(&c.severity).to_string(),
            });
        }
    }
    out.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));
    out
}

/// Curate a step's parameter map from `TaskNode.attributes`. ONLY surfaces
/// figure/method-axis keys the compiler already holds — never synthesizes a
/// method choice (method-neutrality). Sorted by key via `BTreeMap`.
fn curate_step_parameters(node: &TaskNode) -> BTreeMap<String, serde_json::Value> {
    let mut out = BTreeMap::new();
    for key in [
        "required_figures",
        "plot_stage_id",
        "stage_class",
        "spec_preferred_methods",
    ] {
        if let Some(v) = node.attributes.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    out
}

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
    use crate::backend_emitters::workflow_json::lower_to_workflow_json;
    use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
    use crate::workflow_contracts::evidence::AssumptionLedger;
    use crate::workflow_contracts::implementation::{Implementation, OciImageRef};
    use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

    fn align_node() -> TaskNode {
        let mut n = TaskNode::skeleton("align_reads", "Align reads");
        n.implementation = Implementation::ContainerCommand {
            image: OciImageRef {
                image: "ghcr.io/scripps/bio-base".into(),
                tag: "v0.4.0".into(),
                digest: "sha256:abc".into(),
                arch: vec!["amd64".into()],
                gpu: false,
            },
            command_template: vec![],
        };
        n.attributes
            .insert("role".into(), serde_json::Value::String("operation".into()));
        n.attributes.insert(
            "atom_id".into(),
            serde_json::Value::String("align_reads_atom".into()),
        );
        n
    }
    fn quantify_node() -> TaskNode {
        let mut n = TaskNode::skeleton("quantify_features", "Count features");
        n.attributes
            .insert("role".into(), serde_json::Value::String("operation".into()));
        n
    }
    fn simple_dag() -> WorkflowDag {
        WorkflowDag {
            id: "test_dag".into(),
            nodes: vec![align_node(), quantify_node()],
            edges: vec![EdgeContract {
                from_node: "align_reads".into(),
                from_port: "bam".into(),
                to_node: "quantify_features".into(),
                to_port: "bam".into(),
                proof: CompatibilityProof {
                    producer_type: "data:0863".into(),
                    consumer_type: "data:0863".into(),
                    ..Default::default()
                },
                kind: EdgeKind::TypedDataFlow,
                chain_of_custody: None,
            }],
            assumptions: AssumptionLedger::default(),
            source_template: None,
        }
    }

    #[test]
    fn projects_one_step_per_node_sorted() {
        let wf = simple_dag();
        let out = lower_to_typed_workflow(&wf, &TypedWorkflowContext::default());
        assert_eq!(out.steps.len(), 2);
        assert_eq!(out.steps[0].step_id, "align_reads");
        assert_eq!(out.steps[1].step_id, "quantify_features");
        assert_eq!(out.steps[0].tool_id.as_deref(), Some("align_reads_atom"));
    }

    #[test]
    fn edge_preserves_real_ports() {
        let wf = simple_dag();
        let out = lower_to_typed_workflow(&wf, &TypedWorkflowContext::default());
        assert_eq!(out.edges.len(), 1);
        let e = &out.edges[0];
        assert_eq!(e.source_output, "bam");
        assert_eq!(e.target_input, "bam");
        assert_eq!(e.edge_id, "align_reads.bam->quantify_features.bam");
        assert_eq!(e.source_node_id, "align_reads");
        assert_eq!(e.target_node_id, "quantify_features");
    }

    #[test]
    fn dependencies_match_lowered_depends_on() {
        let wf = simple_dag();
        let out = lower_to_typed_workflow(&wf, &TypedWorkflowContext::default());
        let q = out
            .steps
            .iter()
            .find(|s| s.step_id == "quantify_features")
            .unwrap();
        assert_eq!(q.dependencies, vec!["align_reads".to_string()]);
        // Equality with the WORKFLOW.json readiness gate's depends_on.
        let lowered = lower_to_workflow_json(&wf, &Default::default()).unwrap();
        let depends_on: Vec<String> = lowered
            .dag
            .tasks
            .get("quantify_features")
            .unwrap()
            .depends_on
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert_eq!(q.dependencies, depends_on);
    }

    #[test]
    fn parameter_mappings_mirror_edges() {
        let wf = simple_dag();
        let out = lower_to_typed_workflow(&wf, &TypedWorkflowContext::default());
        assert_eq!(out.parameter_mappings.len(), 1);
        let m = &out.parameter_mappings[0];
        assert_eq!(m.source_step, "align_reads");
        assert_eq!(m.source_output, "bam");
        assert_eq!(m.target_step, "quantify_features");
        assert_eq!(m.target_input, "bam");
    }

    #[test]
    fn estimated_duration_is_static_not_clock() {
        // CpuHeavy default → fixed value; two calls identical (no clock).
        let wf = simple_dag();
        let a = lower_to_typed_workflow(&wf, &TypedWorkflowContext::default());
        let b = lower_to_typed_workflow(&wf, &TypedWorkflowContext::default());
        assert_eq!(a.steps[0].estimated_duration, b.steps[0].estimated_duration);
        assert!(a.steps[0].estimated_duration > 0);
    }

    #[test]
    fn lowering_is_byte_stable_across_100_replays() {
        let wf = simple_dag();
        let first =
            serde_json::to_string(&lower_to_typed_workflow(&wf, &TypedWorkflowContext::default()))
                .unwrap();
        for i in 0..100 {
            let json = serde_json::to_string(&lower_to_typed_workflow(
                &wf,
                &TypedWorkflowContext::default(),
            ))
            .unwrap();
            assert_eq!(first, json, "byte-stability lost at iteration {i}");
        }
    }

    use crate::workflow_contracts::port::{Constraint, ConstraintSeverity};

    fn classification_fixture() -> crate::classify::ClassificationResult {
        let mut c = crate::classify::ClassificationResult::default();
        c.modality = "single_cell_rnaseq".into();
        c.edam_topic = "topic:3170".into();
        c
    }

    #[test]
    fn metadata_complexity_buckets() {
        // 2-node dag → simple.
        let wf = simple_dag();
        let cls = classification_fixture();
        let ctx = TypedWorkflowContext {
            classification: Some(&cls),
            ..Default::default()
        };
        let out = lower_to_typed_workflow(&wf, &ctx);
        assert_eq!(out.metadata.complexity, "simple");
        // tags sorted + deduped
        let mut sorted = out.metadata.tags.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(out.metadata.tags, sorted, "tags must be sorted+deduped");
    }

    #[test]
    fn w4_validation_rules_preserve_cel_and_severity() {
        let mut wf = simple_dag();
        wf.nodes[0].preconditions.push(Constraint {
            id: "min_reads".into(),
            statement: "at least 1M reads".into(),
            expression: Some("input.reads >= 1000000".into()),
            severity: ConstraintSeverity::Hard,
        });
        let out = lower_to_typed_workflow(&wf, &TypedWorkflowContext::default());
        let rule = out
            .validation_rules
            .iter()
            .find(|r| r.rule_id == "align_reads:min_reads")
            .expect("expected validation rule for hard precondition");
        assert_eq!(rule.target_step, "align_reads");
        assert_eq!(rule.severity, "hard");
        assert_eq!(rule.expression.as_deref(), Some("input.reads >= 1000000"));
    }

    #[test]
    fn w4_parameters_from_intake_facts_no_method_synthesis() {
        let wf = simple_dag(); // nodes carry NO SME-pinned method
        let mut facts = crate::intake_facts::IntakeFacts::default();
        facts.organism_name = Some("Homo sapiens".into());
        facts.sample_count = Some(12);
        let ctx = TypedWorkflowContext {
            intake_facts: Some(&facts),
            ..Default::default()
        };
        let out = lower_to_typed_workflow(&wf, &ctx);
        assert!(out.parameters.iter().any(|p| p.name == "organism"
            && p.value == serde_json::json!("Homo sapiens")
            && p.source == "intake"));
        assert!(out
            .parameters
            .iter()
            .any(|p| p.name == "sample_count" && p.value == serde_json::json!(12)));
        // method-neutrality: no parameter named after an aligner/method axis.
        assert!(
            !out.parameters
                .iter()
                .any(|p| p.name == "aligner" || p.name == "method"),
            "must not synthesize a method choice"
        );
        // sorted by name (byte-stability).
        let names: Vec<&str> = out.parameters.iter().map(|p| p.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "parameters must be sorted by name");
    }

    #[test]
    fn m4_snapshot_id_threads_when_present() {
        let wf = simple_dag();
        let ctx = TypedWorkflowContext {
            atom_snapshot_id: Some("atoms-v89-20260515T1200Z".into()),
            ..Default::default()
        };
        let out = lower_to_typed_workflow(&wf, &ctx);
        assert_eq!(
            out.atom_registry_snapshot_id.as_deref(),
            Some("atoms-v89-20260515T1200Z")
        );
    }

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
