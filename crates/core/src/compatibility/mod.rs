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
//! - run-scoped facet propagation (`facet_propagation`) and a
//!   warn-only coverage measure over the resulting checks
//!   (`facet_coverage`).
//!
//! What a proof establishes: EDAM port-type subsumption, plus a
//! per-facet unification for each facet the two contracts declare.
//! Facets neither side declares unify to `Unknown` — an honest
//! "undecided", not a pass. `facet_coverage` measures how much of that
//! surface is decided.

pub mod engine;
pub mod facet_coverage;
pub mod facet_propagation;
pub mod facet_unification;
pub mod proof_builder;
pub mod reports;

pub use engine::{
    ClarificationOrValidationNeeded, CompatibilityEngine, CompatibilityResult,
    DeterministicCompatibilityEngine, PlanningContext,
};
pub use facet_coverage::{
    facet_coverage, facet_coverage_advisory, facet_coverage_from_proof_rows, facet_coverage_over,
    log_facet_coverage_advisory, terminal_facet_coverage, FacetCoverage, FacetCoverageRow,
    FacetCoverageScope, FACETS, FACET_COVERAGE_ADVISORY_FLOOR,
};
pub use facet_propagation::{
    propagate_run_facets, propagation_rule, terminal_edge_indices, FacetAssignment, FacetConflict,
    FacetOrigin, FacetPropagation, FacetPropagationReport, PortKey, PortSide, RunFacetSeed,
    PROPAGATED_FACETS,
};
pub use facet_unification::{unify_facet, FacetUnification};
pub use reports::{IncompatibilityReason, IncompatibilityReport};
