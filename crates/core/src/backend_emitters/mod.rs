//! Backend emitters.
//!
//! Per ADR 0029, the only emitter that ships is the
//! `WORKFLOW.json` lowering pass for the custom harness. Historical
//! note: this module used to expose a `BackendEmitter` trait so the
//! IR was full-shape regardless of backend; R2-N21 deleted
//! that single-impl trait — `WorkflowJsonEmitter` is the sole emitter
//! and exposes `emit`/`compile` as inherent methods. The day an
//! external emitter (CWL / WDL / Nextflow / Snakemake / Galaxy) ships,
//! reintroduce the trait OR a `BackendKind` enum + dispatch shim; the
//! `BackendCapabilityReport` plumbing (this module's `CompileError`
//! type + `EmitContext::authorized_losses`) is preserved for that
//! future.
//!
//! The lowering is deterministic, pure (no IO), and round-trippable
//! for `Task`-bearing fields. Sidecar fields (compatibility
//! proofs, assumptions, ranked alternatives) are emitted into
//! adjacent files (`runtime/proofs.jsonl`,
//! `runtime/assumptions.jsonl`) and the harness ignores them at
//! execution time.
//!
//! `WorkflowJsonEmitter::compile` is the
//! semantic-preservation entry point. It returns
//! `(BackendArtifact, BackendCapabilityReport)` so callers can
//! enforce the F17 contract: an emit is allowed only when every
//! `UnsupportedConstraint` the backend reports is authorized by a
//! matching `ConstraintLossAck` on `EmitContext::authorized_losses`.
//! Today's `WorkflowJsonEmitter` reports zero losses (the custom
//! harness consumes the full IR shape, so its capability report is
//! unconditionally empty); the error path activates the day any
//! external emitter ships.

pub mod capability_report;
pub mod workflow_json;

pub use capability_report::{
    loss_tag, BackendCapabilityReport, ConstraintLossAck, UnsupportedConstraint,
};
pub use workflow_json::{
    dag_to_workflow_dag, lower_to_workflow_json, workflow_dag_from_artifact, BackendArtifact,
    EmitContext, PlotAffordanceRecord, WorkflowJsonEmitter,
};

/// Generic backend emit error. Only `WORKFLOW.json` ships today,
/// but the error surface is shaped for the future-emitter case.
#[derive(Debug, Clone, thiserror::Error)]
pub enum EmitError {
    #[error("missing implementation for node {node_id}")]
    /// Variant.
    /// Field value.
    MissingImplementation { node_id: String },
    #[error("unsupported feature for backend {backend}: {feature}")]
    /// Variant.
    /// Field value.
    /// Field value.
    UnsupportedFeature { backend: String, feature: String },
    #[error("io error: {0}")]
    /// Io variant.
    Io(String),
    /// A Task carries a
    /// `ContainerSpec` whose `digest` is empty (or the all-zero
    /// sentinel). Emission MUST fail closed: an unpinned image means
    /// the harness will pull `image:tag` at dispatch time, and tag
    /// drift becomes a silent reproducibility hole.
    #[error("task {task} carries container {image}:{tag} with no resolved sha256 digest; run the digest resolver (or pre-pin in the atom YAML) before emit.")]
    ImageDigestUnresolved {
        /// Task.
        task: String,
        /// Image.
        image: String,
        /// Tag.
        tag: String,
    },
}

/// Typed errors from `BackendEmitter::compile`.
/// `SemanticLossNotAuthorized` is the F17 contract violation: the
/// backend reported losses the SME has not authorized via
/// `EmitContext::authorized_losses`.
///
/// `Emit` wraps the per-step `EmitError` so callers can route both
/// classes through a single Result on the compile pipeline.
#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    /// Backend reported one or more `UnsupportedConstraint`s and the
    /// caller did not pre-authorize them via `authorized_losses`.
    #[error("backend {backend} cannot preserve {n_losses} semantic constraint(s) without explicit authorization")]
    SemanticLossNotAuthorized {
        /// Backend.
        backend: String,
        /// Report.
        report: BackendCapabilityReport,
        /// N losses.
        n_losses: usize,
    },
    /// Underlying per-step `EmitError` (missing implementation,
    /// unsupported feature, io).
    #[error("emit error: {0}")]
    Emit(#[from] EmitError),
}

// R2-N21 — the `BackendEmitter` trait has been deleted.
// `WorkflowJsonEmitter` is the sole production emitter; its `emit`
// and `compile` methods are inherent on the concrete type (see
// `backend_emitters::workflow_json`). The day a second backend
// (CWL / WDL / Nextflow / Snakemake / Galaxy) ships, reintroduce a
// dispatch shim — either the trait or a `BackendKind` enum +
// match-based free function — alongside the new concrete emitter.
