//! Compatibility proof engine.
//!
//! Replaces simple boolean producer→consumer reachability with
//! proof-carrying compatibility (design §4). Every composed edge
//! gets a `CompatibilityResult` describing exactly why the edge
//! holds (or doesn't), with adapters / assumptions / validators
//! attached as needed.
//!
//! The engine is sync, deterministic, and side-effect free —
//! `crates/core` rule from CLAUDE.md.
//!
//! Capabilities:
//! - exact + subtype match for `SemanticType::OntologyTerm` via
//!   the curated EDAM subtype hierarchy (`crate::edam`).
//! - local-extension parent-term subsumption.
//! - facet unification for genome build / coordinate system /
//!   annotation version / organism / modality / normalization /
//!   statistical state / privacy class / cardinality.
//! - typed `CompatibilityResult` (Compatible /
//!   CompatibleWithAdapters / Incompatible / Unknown).
//! - `PlanningContext` carrying adapter policy + risk mode.
//! - adapter registry wired into `CompatibleWithAdapters`; the
//!   v4 planner uses the engine during forward/backward search.

pub mod engine;
pub mod facet_unification;
pub mod proof_builder;
pub mod reports;

pub use engine::{
    ClarificationOrValidationNeeded, CompatibilityEngine, CompatibilityResult,
    DeterministicCompatibilityEngine, PlanningContext,
};
pub use facet_unification::{unify_facet, FacetUnification};
pub use reports::{IncompatibilityReason, IncompatibilityReport};
