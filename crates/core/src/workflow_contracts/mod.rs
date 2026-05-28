//! Canonical IR for the proof-carrying semantic composer.
//!
//! Design: [`docs/dag_composer_design.md`](../../../../docs/dag_composer_design.md).
//!
//! This module defines the canonical IR — `TaskNode`,
//! `PortContract`, `DataProductContract`, `EdgeContract`,
//! `CompatibilityProof`, `AssumptionLedger`, `WorkflowIntent`,
//! `WorkflowTemplate`, `WorkflowDag`, `ComposeOutcome` — wired into
//! composition, emission, promotion, and lowering paths.
//!
//! Compatibility with `crates/core/src/atom.rs::AtomDefinition` is
//! preserved via `TaskNode::from_atom`, which converts every loaded
//! atom to a `TaskNode` so consumers can migrate incrementally without
//! reauthoring config YAML.
//!
//! Determinism contract: every collection field uses `BTreeMap` /
//! `Vec` (sorted). Round-trip through serde is byte-stable.

pub mod assumption_projection;
pub mod chain_of_custody;
pub mod data_product;
pub mod edge;
pub mod evidence;
pub mod from_atom;
pub mod from_intake;
pub mod implementation;
pub mod lifecycle;
pub mod outcome;
pub mod policy_rule_id;
pub mod port;
pub mod promotion_gate;
pub mod refusal_kind;
pub mod semantic_type;
pub mod task_node;
pub mod unblock_path;
pub mod workflow_intent;

pub use chain_of_custody::{AuditorProcedure, ChainOfCustody, ContentCommitment, SuppressionClass};
pub use data_product::{
    AssayContext, BiologicalContext, Cardinality as DataCardinality, DataProductContract,
    IdentifierSystem, JsonSchemaRef, PhysicalRepresentation, PrivacyClass, QualityMetricContract,
    StatisticalState,
};
pub use edge::{CompatibilityProof, EdgeContract, FacetMatch, ProofEvidence};
pub use evidence::{
    AssumptionLedger, AssumptionRef, AssumptionResolution, AssumptionSource, EvidenceSet,
    RiskClass, ValidatorRef,
};
pub use implementation::{Implementation, OciImageRef, RegistryRef, ReviewStatus};
pub use lifecycle::{Deprecation, LifecycleState, NodeStatus, PromotionAuthority, TrustLevel};
pub use outcome::{
    BlockerContext, ComposeOutcome, GapReport, RefusalReport, RefusalValidationError,
    ValidationObligation, ValidationReport,
};
pub use policy_rule_id::{PolicyRuleId, PolicyRuleIdError};
pub use port::{Cardinality as PortCardinality, Constraint, FacetValue, FormatRef, PortContract};
pub use refusal_kind::RefusalKind;
pub use semantic_type::{OntologyTermRef, SemanticType};
pub use task_node::{Provenance, SemVer, TaskNode, TaskNodeId, WorkflowDag, WorkflowTemplate};
pub use unblock_path::{ProjectedOutcome, UnblockPath};
pub use workflow_intent::{
    ConstraintsBlock, DesiredOutput, ExecutionPreference, UncertaintyEntry, UserExplanationStyle,
    WorkflowIntent,
};
