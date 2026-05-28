//! `TaskNode`, `WorkflowTemplate`, `WorkflowDag` — the central IR
//! per design §1 + §9 + §11.
//!
//! `TaskNode` is the typed peer of today's `AtomDefinition`. The
//! atom→TaskNode conversion (`TaskNode::from_atom`) preserves all
//! authoring-surface fields so existing atoms materialize as typed
//! nodes; the strict-superset relationship documented in
//! `atom.rs` remains the contract.
//!
//! `WorkflowTemplate` is the *logical* template — conditionals,
//! scatter/gather, and bounded iteration are explicit. It is lowered
//! through the existing builder/harness mechanisms (`condition`,
//! per-sample expansion, `iterate`) into the concrete
//! `crates/core/src/dag.rs::DAG` shape consumed by the harness.
//!
//! `WorkflowDag` is the typed-edge form of the lowered DAG —
//! lowering+expansion happens but the proof-carrying edges and the
//! assumption ledger remain attached. The lowering pass
//! (`backend_emitters/`) converts this to the on-disk `DAG` plus
//! sidecars.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

use super::edge::EdgeContract;
use super::evidence::{AssumptionLedger, EvidenceSet, RiskClass, ValidatorRef};
use super::implementation::Implementation;
use super::lifecycle::{Deprecation, LifecycleState, NodeStatus, PromotionAuthority, TrustLevel};
use super::port::{Constraint, PortContract};

/// Stable id for a `TaskNode`. Mirrors `AtomDefinition.id` for
/// migrated atoms; new IDs use the convention `<verb>_<noun>` plus
/// optional `_v<N>` version disambiguation.
pub type TaskNodeId = String;

/// Semantic version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct SemVer {
    /// Major.
    pub major: u32,
    /// Minor.
    pub minor: u32,
    /// Patch.
    pub patch: u32,
    /// Optional pre-release tag (`-alpha.1`, `-rc.2`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pre_release: Option<String>,
}

impl SemVer {
    /// Parse `"1.2.3"` or `"1.2.3-rc.1"`. Permissive — falls back
    /// to `0.0.0` rather than panicking; a strict validator can run
    /// at migration time.
    pub fn parse_or_default(s: &str) -> Self {
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p.to_string())),
            None => (s, None),
        };
        let mut parts = core.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        let major = parts.next().unwrap_or(0);
        let minor = parts.next().unwrap_or(0);
        let patch = parts.next().unwrap_or(0);
        Self {
            major,
            minor,
            patch,
            pre_release: pre,
        }
    }

    /// Render as `"1.2.3"` or `"1.2.3-rc.1"`.
    pub fn render(&self) -> String {
        match &self.pre_release {
            Some(pre) => format!("{}.{}.{}-{}", self.major, self.minor, self.patch, pre),
            None => format!("{}.{}.{}", self.major, self.minor, self.patch),
        }
    }
}

impl Default for SemVer {
    fn default() -> Self {
        Self {
            major: 0,
            minor: 1,
            patch: 0,
            pre_release: None,
        }
    }
}

/// Provenance metadata carried inline on a `TaskNode`. Records
/// authorship + source registry; future extensions will add full
/// RO-Crate / WRROC / PROV-O capture.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct Provenance {
    /// Author / source (`config/stage-atoms/...yaml`,
    /// `dockstore:scripps/dna-seq`, `proposed:llm-<id>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source: Option<String>,
    /// Optional human author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub author: Option<String>,
    /// Optional ISO 8601 created-at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub created_at: Option<String>,
    /// Promotion history (one entry per lifecycle transition).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub promotion_history: Vec<PromotionAuthority>,
}

/// A typed task node — the central IR shape. Maps 1:1 to today's
/// atom-but-richer; later phases consume the typed fields without
/// having to migrate atoms one-by-one.
///
/// Note: derives `PartialEq` but not `Eq` because `PortContract` does
/// not implement `Eq` (the v4 P6 `LocalExtensionMaturity::GraduationCandidate`
/// variant carries an `f32 success_rate`). Callers needing `Eq`
/// semantics compare on `id` instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct TaskNode {
    /// Stable id within a `WorkflowDag` / registry.
    pub id: TaskNodeId,

    /// Human-friendly name (UI / logs).
    pub human_name: String,

    /// Machine name — short kebab-cased identifier the agent reads
    /// (`align_reads`, `quantify_features`).
    pub machine_name: String,

    /// Status (`Active` / `Disabled` / `Quarantined`).
    #[serde(default)]
    pub status: NodeStatus,

    /// Free-form description of intent (one line).
    pub intent: String,

    /// Typed input ports.
    pub inputs: Vec<PortContract>,

    /// Typed output ports.
    pub outputs: Vec<PortContract>,

    /// Preconditions on inputs (CEL or schema). Threaded into the
    /// compatibility engine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preconditions: Vec<Constraint>,

    /// Postconditions on outputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub postconditions: Vec<Constraint>,

    /// Assumption ledger references this node depends on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<super::evidence::AssumptionRef>,

    /// What this node actually does (container / composite /
    /// generated / manual / etc.).
    #[serde(default)]
    pub implementation: Implementation,

    /// Validators required to pass before this node is treated as
    /// having executed correctly. Cooperates with the existing
    /// claim_extractor/claim_verifier.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validators: Vec<ValidatorRef>,

    /// Validation/promotion evidence accumulated over time.
    #[serde(default, skip_serializing_if = "is_empty_evidence")]
    pub evidence: EvidenceSet,

    /// Risk class (drives planner scoring + policy gates).
    #[serde(default)]
    pub risk: RiskClass,

    /// Provenance metadata.
    #[serde(default, skip_serializing_if = "is_empty_provenance")]
    pub provenance: Provenance,

    /// Semver version (mirrors `AtomDefinition.version`).
    #[serde(default)]
    pub version: SemVer,

    /// Lifecycle state — drives the promotion gate.
    #[serde(default)]
    pub lifecycle_state: LifecycleState,

    /// Trust level — separate axis from lifecycle.
    #[serde(default)]
    pub trust_level: TrustLevel,

    /// Optional deprecation notice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub deprecation: Option<Deprecation>,

    /// Free-form attribute map mirroring today's
    /// `AtomDefinition.attributes`. Stable for backwards
    /// compatibility; entries that graduate to typed facets are
    /// retired here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(type = "Record<string, unknown>")]
    pub attributes: BTreeMap<String, serde_json::Value>,
}

fn is_empty_evidence(e: &EvidenceSet) -> bool {
    e.passed_validators.is_empty()
        && e.benchmarks.is_empty()
        && e.citations.is_empty()
        && e.notes.is_none()
}

fn is_empty_provenance(p: &Provenance) -> bool {
    p.source.is_none()
        && p.author.is_none()
        && p.created_at.is_none()
        && p.promotion_history.is_empty()
}

impl TaskNode {
    /// Construct a minimum-viable `TaskNode` — id, name, intent.
    /// Tests + scaffolding helpers use this; production paths use
    /// the `from_atom` converter.
    pub fn skeleton(id: impl Into<String>, intent: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            human_name: id.clone(),
            machine_name: id.clone(),
            id,
            status: NodeStatus::default(),
            intent: intent.into(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            preconditions: Vec::new(),
            postconditions: Vec::new(),
            assumptions: Vec::new(),
            implementation: Implementation::default(),
            validators: Vec::new(),
            evidence: EvidenceSet::default(),
            risk: RiskClass::default(),
            provenance: Provenance::default(),
            version: SemVer::default(),
            lifecycle_state: LifecycleState::default(),
            trust_level: TrustLevel::default(),
            deprecation: None,
            attributes: BTreeMap::new(),
        }
    }

    /// Closure Phase B.3 — construct a synthetic `discover_<axis>`
    /// companion node. Emitted by
    /// `composer_v4::discover_companion_synthesis` as a post-pass on
    /// The v4 DAG so the SME can pin `set_intake_method(<axis>,...)`
    /// against a node whose id matches the legacy v2 builder's
    /// `discover_<stage>` shape.
    ///
    /// The node:
    /// - Carries `id == machine_name == discover_<axis>`.
    /// - Tags `attributes["role"] = "discovery"`, so
    ///   `taxonomy::derive_role_from_id` (and any role-aware tooling)
    ///   continues to classify it as `AtomRole::Discovery`.
    /// - Records the target operation node id under
    ///   `attributes["discover_companion_of"]`, the discovery axis
    ///   under `attributes["method_axis"]`, and the candidate options
    ///   under `attributes["method_options"]`. UI surfaces can read
    ///   the options list directly without re-parsing the atom
    ///   registry.
    /// - Implementation is `Unimplemented` (the v2 builder's
    ///   discovery wrapper is essentially a placeholder for runtime
    ///   selection; the agent fills it in at dispatch time).
    /// - Lifecycle is `Production` so the v4 scorer's
    ///   `untrusted_node_count` doesn't penalize the synthesized
    ///   companion (same discipline as
    ///   `synthesize_validator_node` in `companion_synthesis`).
    /// - Has no inputs / outputs declared because the discovery
    ///   atom's runtime contract isn't captured in the post-pass; the
    ///   lowering pass that emits `TaskKind::Discovery` reads the
    ///   role attribute, not the ports.
    pub fn synthesize_discover(
        id: &str,
        method_axis: &str,
        options: &[String],
        target_node_id: &str,
    ) -> Self {
        let mut node = Self::skeleton(
            id,
            format!(
                "Discover method for {method_axis} (options: {}). Surface set_intake_method.",
                if options.is_empty() {
                    "runtime-resolved".to_string()
                } else {
                    options.join(", ")
                }
            ),
        );
        node.attributes.insert(
            "role".into(),
            serde_json::to_value(crate::atom::AtomRole::Discovery)
                .unwrap_or(serde_json::Value::Null),
        );
        node.attributes.insert(
            "assignee".into(),
            serde_json::to_value(crate::atom::AtomAssignee::Agent)
                .unwrap_or(serde_json::Value::Null),
        );
        node.attributes.insert(
            "discover_companion_of".into(),
            serde_json::Value::String(target_node_id.to_string()),
        );
        node.attributes.insert(
            "method_axis".into(),
            serde_json::Value::String(method_axis.to_string()),
        );
        node.attributes.insert(
            "method_options".into(),
            serde_json::Value::Array(
                options
                    .iter()
                    .map(|o| serde_json::Value::String(o.clone()))
                    .collect(),
            ),
        );
        // v4 synthesized companions must lower to
        // `DiscoveryKind::BestPractice` so `WORKFLOW.json` carries
        // `kind: {"discovery": "best_practice"}` — not the
        // `custom` fallback that fires when the attribute is absent.
        node.attributes.insert(
            "discovery_kind".into(),
            serde_json::Value::String("best_practice".into()),
        );
        // Synthesized discover companions must gate the scheduler's
        // SME-review filter (`filter_picks_respecting_sme_gate`).
        // Without this precondition the gate is dead code for v4 DAGs
        // and the only SME-blocking mechanism is the agent's informal
        // text prompt — which fails silently if the agent reads the
        // JSON literally instead.
        node.preconditions.push(super::port::Constraint {
            id: "requires_sme_review".into(),
            statement: "SME must review discover companion output before the target stage runs."
                .into(),
            expression: None,
            severity: super::port::ConstraintSeverity::Hard,
        });
        node.lifecycle_state = LifecycleState::Production;
        node
    }
}

/// Logical workflow template — conditionals, scatter/gather, and
/// bounded iteration are explicit. Lowered to `WorkflowDag` (and
/// ultimately `crates/core/src/dag.rs::DAG`) via
/// `WorkflowJsonEmitter::emit`; see `backend_emitters::mod` for the
/// migration note explaining the `BackendEmitter` trait removal.
///
/// Today's archetype YAMLs are lowered into this shape by the
/// composer; today's builder lowers a `WorkflowTemplate` into the
/// expanded `DAG` via the existing `condition` / per-sample /
/// `iterate` mechanisms.
///
/// `Eq` not derived because `IterationDeclaration` carries `f64`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, Default, schemars::JsonSchema)]
#[ts(export)]
pub struct WorkflowTemplate {
    /// Stable id (e.g. `bulk_rnaseq_de_v4`).
    pub id: String,

    /// Human-readable name.
    pub name: String,

    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,

    /// Typed nodes in topological order. Sorted by `TaskNodeId`
    /// for byte-stable serialization.
    pub nodes: Vec<TaskNode>,

    /// Edges (compatibility-proof carrying).
    #[serde(default)]
    pub edges: Vec<EdgeContract>,

    /// Conditional edges — gate node `gate_node_id` runs only when
    /// `expression` (CEL) evaluates true given prior task outputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditionals: Vec<ConditionalEdge>,

    /// Scatter/gather declarations — node `scatter_node_id`
    /// expands per-sample (or per-shard) at lowering time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scatters: Vec<ScatterDeclaration>,

    /// Iterative declarations — node `iterate_node_id` runs in a
    /// bounded loop until the convergence metric drops below the
    /// declared threshold for the declared consecutive count.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iterations: Vec<IterationDeclaration>,
}

/// Conditional gate on an edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ConditionalEdge {
    /// Node id whose execution is conditioned.
    pub gate_node_id: String,
    /// CEL expression. Composer doesn't compile; harness/agent does.
    pub expression: String,
    /// Optional human rationale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rationale: Option<String>,
}

/// Scatter/gather declaration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ScatterDeclaration {
    /// Node id that scatters per shard.
    pub scatter_node_id: String,
    /// Shard key — typically `"sample"` or a custom key on
    /// `attributes`.
    pub shard_key: String,
    /// Optional gather node id where shards collapse back into one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub gather_node_id: Option<String>,
}

/// Iterate-until declaration. Mirrors `atom::IterateSpec`.
///
/// `Eq` is intentionally not derived because `threshold: f64` —
/// matches the `atom::IterateConvergence` precedent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct IterationDeclaration {
    /// Iterate node id.
    pub iterate_node_id: String,
    /// Max iterations.
    pub max_iterations: u32,
    /// Min iterations.
    pub min_iterations: u32,
    /// Metric source.
    pub metric_source: String,
    /// Operator.
    pub operator: String,
    /// Threshold.
    pub threshold: f64,
    /// Consecutive iterations.
    pub consecutive_iterations: u32,
}

/// Lowered DAG with proof-carrying edges + ledger. The lowering
/// pass emits this plus the four sidecars
/// (proofs/assumptions/validation-reports/policy-decisions).
///
/// `Eq` not derived (`IterationDeclaration` carries `f64`); use
/// stable JSON snapshots in determinism tests instead of
/// `PartialEq` over the struct.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, Default, schemars::JsonSchema)]
#[ts(export)]
pub struct WorkflowDag {
    /// Stable id (matches the originating template).
    pub id: String,

    /// Lowered nodes (post-expansion: per-sample tasks materialized).
    pub nodes: Vec<TaskNode>,

    /// Proof-carrying edges.
    pub edges: Vec<EdgeContract>,

    /// Assumption ledger.
    #[serde(default)]
    pub assumptions: AssumptionLedger,

    /// Source template id, if any. `None` for ad-hoc compositions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_template: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::super::semantic_type::SemanticType;
    use super::*;

    #[test]
    fn semver_round_trips() {
        let s = SemVer::parse_or_default("1.2.3-rc.1");
        assert_eq!(s.major, 1);
        assert_eq!(s.minor, 2);
        assert_eq!(s.patch, 3);
        assert_eq!(s.pre_release.as_deref(), Some("rc.1"));
        assert_eq!(s.render(), "1.2.3-rc.1");
    }

    #[test]
    fn semver_handles_garbage() {
        let s = SemVer::parse_or_default("not a version");
        assert_eq!(s.major, 0);
        assert_eq!(s.render(), "0.0.0");
    }

    #[test]
    fn task_node_skeleton_round_trips() {
        let n = TaskNode::skeleton("align_reads", "Align reads to reference");
        let json = serde_json::to_string(&n).unwrap();
        let back: TaskNode = serde_json::from_str(&json).unwrap();
        assert_eq!(n, back);
        assert_eq!(n.id, "align_reads");
        assert_eq!(n.human_name, "align_reads");
        assert_eq!(n.machine_name, "align_reads");
    }

    #[test]
    fn task_node_with_typed_ports_round_trips() {
        let mut n = TaskNode::skeleton("align_reads", "Align reads");
        n.inputs.push(PortContract {
            name: "fastq".into(),
            semantic_type: SemanticType::edam("data:2044", "Sequence"),
            ..Default::default()
        });
        n.outputs.push(PortContract {
            name: "bam".into(),
            semantic_type: SemanticType::edam("data:0863", "Sequence alignment"),
            ..Default::default()
        });
        let yaml = serde_yml::to_string(&n).unwrap();
        let back: TaskNode = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(n, back);
    }

    #[test]
    fn workflow_template_default_round_trips() {
        let t = WorkflowTemplate::default();
        let json = serde_json::to_string(&t).unwrap();
        let back: WorkflowTemplate = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn workflow_dag_default_round_trips() {
        let d = WorkflowDag::default();
        let json = serde_json::to_string(&d).unwrap();
        let back: WorkflowDag = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }
}
