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

pub mod assemble;
pub mod full_table;

pub use assemble::{CONTEXTUALIZE_STAGE_ID, assemble_report_data};
pub use full_table::{
    FULL_TABLE_END, FULL_TABLE_START, inject_full_tables, significant_entities_section,
};
pub use report_data::{
    ArtifactStats, DirectionSplit, DistBin, EntityRow, GroupCount, LitFinding, LiteratureRollup,
    LiteratureStatus, NonReplication, PolicyColumnSynonyms, ReportData, ResultArtifactSummary,
    SPILL_THRESHOLD, join_literature, load_policy_column_synonyms, should_spill, summarize_artifact,
    write_supplementary,
};
pub use result_schema::{Comparator, ResultSchema, Significance};
