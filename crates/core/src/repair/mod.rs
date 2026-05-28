//! V4 repair-strategy registry.
//!
//! When `meet_in_the_middle` returns `MeetResult::Disconnected`
//! / `PartiallyConnected` with one or more unsatisfied gaps, the
//! planner consults the repair registry for each gap. Each registered
//! strategy independently decides whether it can repair the gap and,
//! if so, returns a typed [`proposal::RepairProposal`] carrying the
//! exact [`proposal::DagModification`] it would apply, a risk class
//! that gates auto-application, and a deterministic
//! `ctx_snapshot_hash` so stale proposals are rejected at accept time.
//!
//! F20 invariant: only proposals whose `risk_class <=
//! PlanningContext::auto_attempt_risk_threshold` are auto-applied.
//! `MediumUserGated` + `HighCredentialedReview` proposals emit
//! substrate (`VerifierDecision::RepairProposed`) but never mutate the
//! DAG — accept/reject is the SME's explicit decision.

pub mod proposal;
pub mod registry;
pub mod strategies;
pub mod strategy;

pub use proposal::{
    DagModification, EdgeRef, FacetMismatch, PortRef, PortRole, RegistryQuery, RepairGap,
    RepairProposal, RepairRiskClass,
};
pub use registry::RepairRegistry;
pub use strategy::{GapKind, RepairStrategy};
