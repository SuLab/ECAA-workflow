//! v3 P11 — ingestion-time injection-detection pipeline.
//!
//! Closes design v3 §16.1. Two submodules:
//! - [`patterns`] — the `InjectionPatternCatalog` loaded from
//!   `config/ingestion-safety/injection-patterns.yaml`, plus the
//!   typed `PatternCategory` / `PatternSeverity` / `DetectionAction`
//!   enums.
//! - [`detectors`] — the `scan_metadata` entry point and
//!   `IngestionSafetyReport` projection.
//!
//! Wired into `crate::external_registry::local_cwl::LocalCwlImporter`:
//! before the existing trust-level + ontology-scope logic, the
//! importer flattens the CWL metadata blob into a text dictionary,
//! scans it, and reacts to the `overall_action`:
//! - `Refuse` → return `ExternalImportError::IngestionRefused`.
//! - `Quarantine` → downgrade `trust_level` to `Untrusted`.
//! - `Annotate` → attach the report to attributes; trust unchanged.
//!
//! v4 P6 supersession: the cross-session aggregation half of design
//! v3 §4 (recurring `Unknown` registry-improvement signal) graduates
//! to the LocalExtension graduation pathway in
//! `crate::local_extension_graduation` + the conversation crate's
//! `cross_session_aggregator`. The lexical aggregator surface lives
//! in `registry_improvement` as a thin adapter the legacy `make doctor`
//! output continues to call.

pub mod detectors;
pub mod patterns;

pub use detectors::{
    extract_text_fields, scan_metadata, IngestionSafetyReport, InjectionDetection,
};
pub use patterns::{
    DetectionAction, InjectionPattern, InjectionPatternCatalog, InjectionPatternError,
    PatternCategory, PatternSeverity,
};
