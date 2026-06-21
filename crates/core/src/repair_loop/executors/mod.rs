//! Per-class [`crate::repair_loop::executor::Executor`] implementations.
//!
//! One submodule per repair family:
//! - [`narrative`]: mechanical prose-to-table corrections.
//! - [`conformance`]: deterministic structural / manifest fixes.
//! - [`equivalence`]: reproduce-and-compare re-execution (agentic).
//! - [`agentic`]: citation / coverage / analysis / evidence re-runs (agentic).
//!
//! The deterministic families ([`narrative`], [`conformance`]) are authored by
//! sibling modules; their executor structs are re-exported here via glob so the
//! registry can wire every class from a single import point.

pub mod agentic;
pub mod conformance;
pub mod equivalence;
pub mod narrative;

pub use agentic::{AnalysisRerun, CitationFix, CoverageGap, EvidenceCompletion};
pub use conformance::*;
pub use equivalence::EquivalenceRerun;
pub use narrative::*;
