//! Canonical Rust binding of the ECAA v0.1 typed object model.
//!
//! Small dependency a second Rust-language ECAA implementation can import
//! to get the canonical types without pulling in the awa-workflow compiler.
//! See `docs/ecaa-spec/v0.1.md` for the full normative specification.
//!
//! # v0.1 scope
//!
//! Ships the closed types that downstream consumers need to bind to:
//!
//! - [`InvariantId`], [`InvariantStatus`], [`InvariantVerdict`],
//!   [`AuditProofReport`] — A sub-graph wire shape.
//! - [`ReexecutionBucket`] — Q sub-graph `RerunOutcome.class` enum.
//! - [`BlockerKind`] (47 variants, `#[non_exhaustive]`) + cascade
//!   (`ValidationFailureCause`, `LiteratureClaimFailureKind`,
//!   `NetworkPolicy`, `SandboxRequirement`, `ToolErrorEnvelope`,
//!   `ExcludedPath`, `SandboxRefusalRecord`, `StallSignalWire`,
//!   `StallAction`, `BlockerContext`, `BlockerEntry`).
//! - [`AblationFlag`] (6 variants) + [`all_flags`] — the SWFC_ABLATE_*
//!   contract enum. The runtime `is_active()` check is in
//!   `scripps-workflow-core::ablation::AblationFlagExt` (kept there to
//!   avoid coupling this crate to env-var access).
//! - [`consts`] — canonical const arrays for cross-doc consistency.

pub mod ablation;
pub mod atom;
pub mod blocker;
pub mod consts;
pub mod error_envelope;
pub mod invariants;
pub mod reexecution;

pub use ablation::{all_flags, AblationFlag};
pub use atom::{NetworkPolicy, SandboxRequirement};
pub use blocker::{
    BlockerContext, BlockerEntry, BlockerKind, ExcludedPath, LiteratureClaimFailureKind,
    SandboxRefusalRecord, StallAction, StallSignalWire, ValidationFailureCause,
};
pub use error_envelope::ToolErrorEnvelope;
pub use invariants::{AuditProofReport, InvariantId, InvariantStatus, InvariantVerdict};
pub use reexecution::ReexecutionBucket;
