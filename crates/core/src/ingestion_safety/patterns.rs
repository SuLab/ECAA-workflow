//! v3 P11 — injection-pattern catalog.
//!
//! Closes design ingestion-time prompt-injection detection
//! pipeline. The catalog ships starter rules for the five highest-
//! leverage attack vectors (instruction injection, fake EDAM term,
//! dependency-confusion typos, hidden HTML comment, credentials).
//! Sites can extend via `config/ingestion-safety/injection-patterns.yaml`.
//!
//! The catalog is loaded once at importer construction time. Each
//! pattern is a regex; runtime matching is performed by
//! `super::detectors::scan_metadata`.
//!
//! No serialization of compiled regexes — patterns travel as strings
//! and recompile per-importer-load, so the catalog stays byte-stable
//! across runs.
//!
//! See also: `_injection-patterns.schema.json` sidecar in
//! `config/ingestion-safety/`.
//!
//! v4 P6 note: the catalog is referenced by the local-extension
//! graduation pathway only insofar as imported entries that route
//! through `LocalCwlImporter::import` are scanned for injection
//! patterns; the graduation pathway itself does not consult the
//! catalog.

use serde::{Deserialize, Serialize};
use std::path::Path;
use ts_rs::TS;

/// The deserialized YAML envelope. One per `config/ingestion-safety/
/// injection-patterns.yaml` file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct InjectionPatternCatalog {
    /// Semver-shaped catalog version (asserted by schema).
    pub version: String,
    /// Ordered list of patterns. Order is preserved across load/save
    /// for determinism in `IngestionSafetyReport` ordering.
    pub patterns: Vec<InjectionPattern>,
}

/// One pattern entry. The `pattern` field is a raw regex string;
/// callers compile it on demand. Storing strings (not compiled
/// regexes) keeps the catalog itself trivially Clone + serializable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct InjectionPattern {
    /// Stable id (kebab-case). Surfaces in
    /// `InjectionDetection.pattern_id` and
    /// `runtime/ingestion-safety.jsonl`.
    pub id: String,
    /// Coarse category for UI dispatch.
    pub category: PatternCategory,
    /// Regex string compiled at scan time.
    pub pattern: String,
    /// Severity bucket. Drives the `overall_action` precedence.
    pub severity: PatternSeverity,
    /// Default action when the pattern fires. Sites can override
    /// per-pattern via a future YAML extension (today's catalog has
    /// one action per pattern).
    pub default_action: DetectionAction,
}

/// Coarse category for UI dispatch. Not load-bearing for the
/// `IngestionSafetyReport.overall_action` projection.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum PatternCategory {
    /// InstructionInjection variant.
    InstructionInjection,
    /// FakeOntologyTerm variant.
    FakeOntologyTerm,
    /// DependencyConfusion variant.
    DependencyConfusion,
    /// HiddenHtmlComment variant.
    HiddenHtmlComment,
    /// Credential variant.
    Credential,
    /// UrlExfiltration variant.
    UrlExfiltration,
}

/// Severity bucket. Drives the `overall_action` precedence in
/// `IngestionSafetyReport`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum PatternSeverity {
    /// Low variant.
    Low,
    /// Medium variant.
    Medium,
    /// High variant.
    High,
    /// Critical variant.
    Critical,
}

/// What to do when a pattern fires.
///
/// Precedence (higher value wins): `Refuse` (2) > `Quarantine` (1) >
/// `Annotate` (0). Used by `IngestionSafetyReport::overall_action`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum DetectionAction {
    /// Quarantine the imported node (lower trust level, keep in
    /// `LifecycleState::Contracted`).
    Quarantine,
    /// Refuse import entirely. Hard error from the importer.
    Refuse,
    /// Annotate the imported node's attributes; trust unchanged.
    Annotate,
}

impl DetectionAction {
    /// Precedence weight. Used to compute the `overall_action` of a
    /// multi-detection report.
    pub fn precedence(self) -> u8 {
        match self {
            DetectionAction::Refuse => 2,
            DetectionAction::Quarantine => 1,
            DetectionAction::Annotate => 0,
        }
    }
}

/// Errors that may surface during catalog loading.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum InjectionPatternError {
    #[error("catalog file not found: {0}")]
    /// NotFound variant.
    NotFound(String),
    #[error("catalog read error at {path}: {source}")]
    /// Io variant.
    Io {
        /// Path.
        path: String,
        #[source]
        /// Source.
        source: std::io::Error,
    },
    #[error("catalog YAML parse error: {0}")]
    /// Parse variant.
    Parse(#[from] serde_yml::Error),
    #[error("catalog invalid version: {got}")]
    /// Variant.
    /// Field value.
    InvalidVersion { got: String },
    #[error("catalog duplicate pattern id: {id}")]
    /// Variant.
    /// Field value.
    DuplicateId { id: String },
    #[error("catalog pattern {id} has invalid regex: {source}")]
    /// InvalidRegex variant.
    InvalidRegex {
        /// Id.
        id: String,
        #[source]
        /// Source.
        source: regex::Error,
    },
}

impl InjectionPatternCatalog {
    /// Read + parse + validate the canonical catalog YAML.
    ///
    /// Validation includes: semver version pattern, no duplicate
    /// pattern ids, every regex compiles.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, InjectionPatternError> {
        let p = path.as_ref();
        if !p.exists() {
            return Err(InjectionPatternError::NotFound(p.display().to_string()));
        }
        let raw = std::fs::read_to_string(p).map_err(|e| InjectionPatternError::Io {
            path: p.display().to_string(),
            source: e,
        })?;
        let parsed: InjectionPatternCatalog = serde_yml::from_str(&raw)?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Load the catalog from the conventional location under the
    /// configured config dir. Returns `Ok(None)` when the file
    /// doesn't exist so callers can disable injection scanning by
    /// simply not authoring the catalog file.
    pub fn try_load_default(
        config_dir: impl AsRef<Path>,
    ) -> Result<Option<Self>, InjectionPatternError> {
        let path = config_dir
            .as_ref()
            .join("ingestion-safety")
            .join("injection-patterns.yaml");
        if !path.exists() {
            return Ok(None);
        }
        Self::load_from_path(&path).map(Some)
    }

    /// Internal: validate the loaded catalog.
    fn validate(&self) -> Result<(), InjectionPatternError> {
        let version_re = regex::Regex::new(r"^\d+\.\d+\.\d+$").expect("static regex compiles");
        if !version_re.is_match(&self.version) {
            return Err(InjectionPatternError::InvalidVersion {
                got: self.version.clone(),
            });
        }
        let mut seen = std::collections::BTreeSet::new();
        for p in &self.patterns {
            if !seen.insert(p.id.clone()) {
                return Err(InjectionPatternError::DuplicateId { id: p.id.clone() });
            }
            // Compile each pattern at load time so we fail loudly at
            // boot rather than silently per-scan.
            if let Err(e) = regex::Regex::new(&p.pattern) {
                return Err(InjectionPatternError::InvalidRegex {
                    id: p.id.clone(),
                    source: e,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_orders_actions() {
        assert!(DetectionAction::Refuse.precedence() > DetectionAction::Quarantine.precedence());
        assert!(DetectionAction::Quarantine.precedence() > DetectionAction::Annotate.precedence());
    }

    #[test]
    fn catalog_round_trips_yaml() {
        let yaml = r#"
version: "1.0.0"
patterns:
  - id: ignore-previous-instructions
    category: instruction_injection
    pattern: "(?i)ignore previous"
    severity: high
    default_action: quarantine
"#;
        let parsed: InjectionPatternCatalog = serde_yml::from_str(yaml).unwrap();
        parsed.validate().unwrap();
        assert_eq!(parsed.patterns.len(), 1);
        assert_eq!(parsed.patterns[0].id, "ignore-previous-instructions");
        assert_eq!(
            parsed.patterns[0].category,
            PatternCategory::InstructionInjection
        );
        assert_eq!(parsed.patterns[0].severity, PatternSeverity::High);
        assert_eq!(
            parsed.patterns[0].default_action,
            DetectionAction::Quarantine
        );
    }

    #[test]
    fn validate_rejects_duplicate_ids() {
        let cat = InjectionPatternCatalog {
            version: "1.0.0".into(),
            patterns: vec![
                InjectionPattern {
                    id: "dup".into(),
                    category: PatternCategory::Credential,
                    pattern: ".*".into(),
                    severity: PatternSeverity::Low,
                    default_action: DetectionAction::Annotate,
                },
                InjectionPattern {
                    id: "dup".into(),
                    category: PatternCategory::Credential,
                    pattern: ".*".into(),
                    severity: PatternSeverity::Low,
                    default_action: DetectionAction::Annotate,
                },
            ],
        };
        match cat.validate() {
            Err(InjectionPatternError::DuplicateId { id }) => assert_eq!(id, "dup"),
            other => panic!("expected DuplicateId, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_bad_regex() {
        let cat = InjectionPatternCatalog {
            version: "1.0.0".into(),
            patterns: vec![InjectionPattern {
                id: "broken".into(),
                category: PatternCategory::Credential,
                pattern: "(invalid".into(), // unbalanced paren
                severity: PatternSeverity::Low,
                default_action: DetectionAction::Annotate,
            }],
        };
        match cat.validate() {
            Err(InjectionPatternError::InvalidRegex { id, .. }) => assert_eq!(id, "broken"),
            other => panic!("expected InvalidRegex, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_bad_version() {
        let cat = InjectionPatternCatalog {
            version: "not-semver".into(),
            patterns: vec![],
        };
        match cat.validate() {
            Err(InjectionPatternError::InvalidVersion { got }) => assert_eq!(got, "not-semver"),
            other => panic!("expected InvalidVersion, got {other:?}"),
        }
    }
}
