//! V4 modality-scoped ontology resolution per v4 design §2.6.
//!
//! `(BioinformaticsModality × OntologyPrefix) → ScopeCheck`.
//!
//! Loaded into `PlanningContext` at compose time; consumed by the
//! compatibility engine on parent-term proposals and by the registry
//! importer on modality-conflict detection. v4 P2 will wrap each
//! `check()` call with substrate emission of `OntologyScopeChecked`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use ts_rs::TS;

use crate::workflow_contracts::workflow_intent::BioinformaticsModality;

/// The full coverage matrix loaded from
/// `config/modality-ontology-coverage.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct OntologyScopeMatrix {
    /// Semver-shaped matrix version (asserted by schema).
    pub version: String,
    /// One row per `BioinformaticsModality`.
    pub coverage: Vec<ModalityCoverage>,
}

/// One modality's ontology coverage envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ModalityCoverage {
    /// Modality.
    pub modality: BioinformaticsModality,
    /// Primary ontologies.
    pub primary_ontologies: OntologySet,
    /// Secondary ontologies.
    pub secondary_ontologies: OntologySet,
    /// Forbidden ontologies.
    pub forbidden_ontologies: OntologySet,
}

/// An ontology set per matrix cell — either the literal token `union`
/// (meaning "union across every modality's set") or an explicit set
/// of ontology prefixes (e.g. `["GO", "SO", "EDAM"]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(untagged)]
pub enum OntologySet {
    /// Literal `union` token — expanded at lookup time.
    Union(UnionToken),
    /// Explicit set of ontology prefixes. `BTreeSet` for byte-stable
    /// serialization.
    Explicit(BTreeSet<String>),
}

/// Single-variant marker matching the literal YAML string `"union"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum UnionToken {
    /// Union variant.
    Union,
}

/// Result of a `(modality × ontology_prefix)` check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeCheck {
    /// Prefix is in the modality's primary set (or `union` token
    /// covers it).
    InPrimary,
    /// Prefix is in the modality's secondary set.
    InSecondary,
    /// Prefix is explicitly forbidden for this modality.
    Forbidden,
    /// Prefix is not in any set for this modality.
    OutOfScope,
}

impl OntologyScopeMatrix {
    /// Read + parse + validate the canonical matrix YAML.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Arc<Self>, OntologyScopeError> {
        let p = path.as_ref();
        if !p.exists() {
            return Err(OntologyScopeError::NotFound(p.display().to_string()));
        }
        let raw = std::fs::read_to_string(p).map_err(|e| OntologyScopeError::Io {
            path: p.display().to_string(),
            source: e,
        })?;
        let parsed: OntologyScopeMatrix = serde_yml::from_str(&raw)?;
        parsed.validate()?;
        Ok(Arc::new(parsed))
    }

    /// Internal: semver-version pattern + no duplicate modalities.
    fn validate(&self) -> Result<(), OntologyScopeError> {
        let version_re = regex::Regex::new(r"^\d+\.\d+\.\d+$").expect("static regex compiles");
        if !version_re.is_match(&self.version) {
            return Err(OntologyScopeError::InvalidVersion {
                got: self.version.clone(),
            });
        }
        let mut seen = BTreeSet::new();
        for c in &self.coverage {
            let key = format!("{:?}", c.modality);
            if !seen.insert(key.clone()) {
                return Err(OntologyScopeError::DuplicateModality { modality: key });
            }
        }
        Ok(())
    }

    /// Check a candidate ontology prefix against a modality's scope.
    ///
    /// Precedence: `Forbidden` > `InPrimary` > `InSecondary` >
    /// `OutOfScope`. The `union` token always returns `InPrimary` /
    /// `InSecondary` according to which set it appears in.
    pub fn check(&self, modality: &BioinformaticsModality, ontology_prefix: &str) -> ScopeCheck {
        let coverage = match self.coverage.iter().find(|c| &c.modality == modality) {
            Some(c) => c,
            None => return ScopeCheck::OutOfScope,
        };
        let in_set = |set: &OntologySet| -> bool {
            match set {
                OntologySet::Union(_) => true,
                OntologySet::Explicit(s) => s.contains(ontology_prefix),
            }
        };
        if in_set(&coverage.forbidden_ontologies) {
            return ScopeCheck::Forbidden;
        }
        if in_set(&coverage.primary_ontologies) {
            return ScopeCheck::InPrimary;
        }
        if in_set(&coverage.secondary_ontologies) {
            return ScopeCheck::InSecondary;
        }
        ScopeCheck::OutOfScope
    }

    /// Resolve an IRI like `http://purl.obolibrary.org/obo/GO_0008150`
    /// to its prefix. Also handles compact IRIs (`GO:0008150`,
    /// `EDAM:data_1383`). Returns `None` for unrecognised shapes.
    pub fn prefix_of_iri(iri: &str) -> Option<String> {
        if let Some(rest) = iri.strip_prefix("http://purl.obolibrary.org/obo/") {
            return rest.split('_').next().map(|s| s.to_string());
        }
        if let Some((prefix, _)) = iri.split_once(':') {
            if !prefix.is_empty()
                && prefix
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase())
                && prefix
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                return Some(prefix.to_string());
            }
        }
        None
    }
}

/// Loader errors.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum OntologyScopeError {
    #[error("file not found: {0}")]
    /// NotFound variant.
    NotFound(String),
    #[error("io error reading {path}: {source}")]
    /// Io variant.
    Io {
        /// Path.
        path: String,
        #[source]
        /// Source.
        source: std::io::Error,
    },
    #[error("yaml parse: {0}")]
    /// Parse variant.
    Parse(#[from] serde_yml::Error),
    #[error("invalid version: {got}")]
    /// Variant.
    /// Field value.
    InvalidVersion { got: String },
    #[error("duplicate modality: {modality}")]
    /// Variant.
    /// Field value.
    DuplicateModality { modality: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_in_primary() {
        let mut primary = BTreeSet::new();
        primary.insert("GO".to_string());
        primary.insert("SO".to_string());
        let matrix = OntologyScopeMatrix {
            version: "1.0.0".into(),
            coverage: vec![ModalityCoverage {
                modality: BioinformaticsModality::BulkRnaseq,
                primary_ontologies: OntologySet::Explicit(primary),
                secondary_ontologies: OntologySet::Explicit(BTreeSet::new()),
                forbidden_ontologies: OntologySet::Explicit(BTreeSet::new()),
            }],
        };
        assert_eq!(
            matrix.check(&BioinformaticsModality::BulkRnaseq, "GO"),
            ScopeCheck::InPrimary
        );
    }

    #[test]
    fn check_forbidden_beats_primary() {
        let mut primary = BTreeSet::new();
        primary.insert("PR".to_string());
        let mut forbidden = BTreeSet::new();
        forbidden.insert("PR".to_string());
        let matrix = OntologyScopeMatrix {
            version: "1.0.0".into(),
            coverage: vec![ModalityCoverage {
                modality: BioinformaticsModality::SingleCellRnaseq,
                primary_ontologies: OntologySet::Explicit(primary),
                secondary_ontologies: OntologySet::Explicit(BTreeSet::new()),
                forbidden_ontologies: OntologySet::Explicit(forbidden),
            }],
        };
        assert_eq!(
            matrix.check(&BioinformaticsModality::SingleCellRnaseq, "PR"),
            ScopeCheck::Forbidden
        );
    }

    #[test]
    fn check_out_of_scope_when_modality_missing() {
        let matrix = OntologyScopeMatrix {
            version: "1.0.0".into(),
            coverage: vec![],
        };
        assert_eq!(
            matrix.check(&BioinformaticsModality::BulkRnaseq, "GO"),
            ScopeCheck::OutOfScope
        );
    }

    #[test]
    fn check_union_token_returns_in_primary() {
        let matrix = OntologyScopeMatrix {
            version: "1.0.0".into(),
            coverage: vec![ModalityCoverage {
                modality: BioinformaticsModality::MultiOmics,
                primary_ontologies: OntologySet::Union(UnionToken::Union),
                secondary_ontologies: OntologySet::Union(UnionToken::Union),
                forbidden_ontologies: OntologySet::Explicit(BTreeSet::new()),
            }],
        };
        assert_eq!(
            matrix.check(&BioinformaticsModality::MultiOmics, "GO"),
            ScopeCheck::InPrimary
        );
        assert_eq!(
            matrix.check(&BioinformaticsModality::MultiOmics, "PR"),
            ScopeCheck::InPrimary
        );
    }

    #[test]
    fn validate_rejects_bad_version() {
        let matrix = OntologyScopeMatrix {
            version: "v1".into(),
            coverage: vec![],
        };
        assert!(matches!(
            matrix.validate(),
            Err(OntologyScopeError::InvalidVersion { .. })
        ));
    }

    #[test]
    fn validate_rejects_duplicate_modality() {
        let matrix = OntologyScopeMatrix {
            version: "1.0.0".into(),
            coverage: vec![
                ModalityCoverage {
                    modality: BioinformaticsModality::BulkRnaseq,
                    primary_ontologies: OntologySet::Explicit(BTreeSet::new()),
                    secondary_ontologies: OntologySet::Explicit(BTreeSet::new()),
                    forbidden_ontologies: OntologySet::Explicit(BTreeSet::new()),
                },
                ModalityCoverage {
                    modality: BioinformaticsModality::BulkRnaseq,
                    primary_ontologies: OntologySet::Explicit(BTreeSet::new()),
                    secondary_ontologies: OntologySet::Explicit(BTreeSet::new()),
                    forbidden_ontologies: OntologySet::Explicit(BTreeSet::new()),
                },
            ],
        };
        assert!(matches!(
            matrix.validate(),
            Err(OntologyScopeError::DuplicateModality { .. })
        ));
    }
}
