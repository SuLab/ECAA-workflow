//! Comprehensive-reporting-contract types shared between the atom
//! registry (`AtomDefinition.result_schema`), the deterministic
//! report-data assembler, and the reporting invariants validator.
//!
//! `result_schema` declares, per terminal analytical atom, how its
//! primary result artifact is read. All modality-specific meaning
//! (which column is the entity, which is significance, which is the
//! signed effect) enters the system only through these declarations —
//! never hardcoded downstream.

pub mod report_data;
pub mod result_schema;

pub use report_data::{
    ArtifactStats, DirectionSplit, DistBin, EntityRow, LitFinding, LiteratureRollup,
    LiteratureStatus, NonReplication, ReportData, ResultArtifactSummary, summarize_artifact,
};
pub use result_schema::{Comparator, ResultSchema, Significance};
