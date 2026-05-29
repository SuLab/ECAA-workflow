//! Port contracts per design §1.
//!
//! A `PortContract` is the typed input/output of a `TaskNode`. The
//! shape mirrors design §1 + §12 — semantic type, physical format,
//! biological/statistical/privacy/cardinality facets, plus an
//! extensible `facets` map for future additions without a schema
//! revision.
//!
//! Today's atoms encode port-level info via the `edam_data` /
//! `edam_format` pair plus `attributes` map; `TaskNode::from_atom`
//! synthesizes one input and one output `PortContract` from those
//! fields so existing atoms materialize as typed nodes.
//!
//! Rich biological facets (genome build, coordinate system, normalization
//! state, etc.) are present as optional fields; atoms synthesized via
//! `TaskNode::from_atom` populate only the EDAM pair and leave richer
//! facets to be authored incrementally.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

use super::evidence::ValidatorRef;
use super::semantic_type::{OntologyTermRef, SemanticType};

/// Cardinality of a port — how many values arrive/depart. Mirrors the
/// existing `taxonomy::StageCardinality` taxonomy without depending
/// on it (the workflow_contracts module is a peer of the legacy
/// taxonomy module, not a consumer).
#[derive(
    Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Cardinality {
    /// Exactly one value.
    #[default]
    One,
    /// Zero or one (optional input/output).
    Optional,
    /// Many — fan-out semantics for scatter/gather.
    Many,
    /// Iterate-until — bounded loop in the logical template; lowers
    /// to the existing 4-template-task scaffold per CLAUDE.md S10.10.
    IterateUntil {
        /// Hard ceiling so the planner can bound search.
        max_iterations: u32,
    },
}

/// Reference to a JSON Schema validating the on-disk shape of an
/// artifact. Used for structural validation in addition to the
/// semantic type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct JsonSchemaRef {
    /// Path or URI of the schema.
    pub uri: String,
    /// Optional version pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
}

/// Reference to a physical file format. Distinct from the semantic
/// type so two semantically-equivalent ports can match across
/// formats with an adapter (e.g. BAM ↔ CRAM).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct FormatRef {
    /// EDAM format IRI (e.g. `format:2572` for BAM) or `ecaax:<slug>`
    /// for in-house formats.
    pub iri: String,
    /// Human label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub label: Option<String>,
    /// File extension (no leading dot). Used for adapter routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub extension: Option<String>,
}

/// Free-form facet value. Aliased to `serde_json::Value` (same
/// convention as `AtomDefinition.attributes`) so authoring YAML
/// round-trips cleanly without untagged-enum ambiguity. Used for
/// extensibility: new facets can be added without schema bumps.
/// Recommended convention is to graduate stable facets to typed
/// first-class fields once they prove load-bearing.
pub type FacetValue = serde_json::Value;

/// A typed constraint attached to a port (precondition or
/// postcondition). CEL or schema constraints are recorded here and
/// evaluated by the compatibility engine and harness at dispatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct Constraint {
    /// Stable id within the constraint set (so proofs can reference
    /// "constraint X satisfied").
    pub id: String,
    /// Human-readable statement.
    pub statement: String,
    /// CEL or schema expression. The composer doesn't compile this;
    /// the harness/agent does at dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expression: Option<String>,
    /// Severity bucket. `Hard` constraints reject incompatible
    /// edges; `Soft` ones produce a warning + assumption-ledger
    /// entry.
    #[serde(default)]
    pub severity: ConstraintSeverity,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// ConstraintSeverity discriminant.
pub enum ConstraintSeverity {
    #[default]
    /// Hard variant.
    Hard,
    /// Soft variant.
    Soft,
    /// Warn variant.
    Warn,
}

/// Privacy class — propagates along edges. Set on the consuming
/// port's input contract; the compatibility engine refuses an edge
/// where the producer's privacy class is "wider" than the
/// consumer's.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum PortPrivacyClass {
    #[default]
    /// Public variant.
    Public,
    /// Internal variant.
    Internal,
    /// Sensitive variant.
    Sensitive,
    /// Phi variant.
    Phi,
    /// Restricted variant.
    Restricted,
}

/// A typed port — input or output of a `TaskNode`. Design §1 + §12.
///
/// All facet-bearing fields are optional because atoms synthesized from
/// `AtomDefinition.edam_data` / `edam_format` provide only the EDAM
/// pair; richer ports supply genome build, coordinate system, and other
/// biological facets inline.
///
/// Note: derives `PartialEq` but not `Eq` because `SemanticType` does
/// not implement `Eq` (`LocalExtensionMaturity::GraduationCandidate`
/// carries an `f32 success_rate`). Callers needing `Eq` semantics
/// compare on `name` or `semantic_type.stable_id()` instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, Default, schemars::JsonSchema)]
#[ts(export)]
pub struct PortContract {
    /// Port name. Stable within the `TaskNode` (used as the edge
    /// endpoint identifier).
    pub name: String,

    /// Semantic type (open-world: ontology term / local extension /
    /// opaque).
    pub semantic_type: SemanticType,

    /// Physical file format. `None` for opaque blobs or in-memory
    /// values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub physical_format: Option<FormatRef>,

    /// JSON Schema validating the on-disk shape (design §1
    /// `schema`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub structural_schema: Option<JsonSchemaRef>,

    /// Additional ontology hits beyond `semantic_type` (design §1
    /// `ontology_terms`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ontology_terms: Vec<OntologyTermRef>,

    /// Modality bucket (`bulk_rnaseq`, `single_cell_rnaseq`, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub modality: Option<String>,

    /// Organism (e.g. `Homo sapiens`, `Mus musculus`). Composer
    /// blocks edges where producer/consumer organisms diverge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub organism: Option<String>,

    /// Genome build (e.g. `GRCh38.p14`, `GRCm39`). Mismatches are
    /// the canonical source of "two BAM files but incompatible"
    /// false matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub genome_build: Option<String>,

    /// Annotation version (e.g. `Ensembl 110`, `GENCODE 44`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub annotation_version: Option<String>,

    /// Coordinate system (e.g. `0-based half-open`, `1-based
    /// closed`). VCF vs BED vs GFF clash here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub coordinate_system: Option<String>,

    /// Units (e.g. `TPM`, `CPM`, `raw counts`, `log2 fold change`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub units: Option<String>,

    /// Normalization state (e.g. `raw`, `tpm`, `quantile`,
    /// `vsn`). The first source of "matrix shape correct, values
    /// wrong" failures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub normalization_state: Option<String>,

    /// Statistical state (e.g. `unprocessed`, `de_tested`,
    /// `multiple_testing_corrected`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub statistical_state: Option<String>,

    /// Privacy class.
    #[serde(default)]
    pub privacy_class: PortPrivacyClass,

    /// Cardinality (one / optional / many / iterate-until).
    #[serde(default)]
    pub cardinality: Cardinality,

    /// Validators that must pass on data flowing through this port.
    /// References to validator implementations live in the
    /// `EvidenceSet`/`ValidatorRef` registry and are evaluated by
    /// the harness verify endpoint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validators: Vec<ValidatorRef>,

    /// Constraints (preconditions/postconditions) on values flowing
    /// through this port, evaluated as part of the compatibility proof.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<Constraint>,

    /// Extensible facet map. Stable facets graduate to typed
    /// fields above; this map carries the long tail without
    /// requiring schema bumps for niche cases.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(type = "Record<string, unknown>")]
    pub facets: BTreeMap<String, FacetValue>,
}

impl PortContract {
    /// Construct an EDAM-derived port from today's atom-shape EDAM
    /// fields. Used by `TaskNode::from_atom`.
    pub fn from_edam(
        name: impl Into<String>,
        edam_data: Option<&str>,
        edam_format: Option<&str>,
    ) -> Self {
        let semantic_type = match edam_data {
            Some(iri) => SemanticType::edam(iri, ""),
            None => SemanticType::opaque("not specified"),
        };
        let physical_format = edam_format.map(|iri| FormatRef {
            iri: iri.to_string(),
            label: None,
            extension: None,
        });
        Self {
            name: name.into(),
            semantic_type,
            physical_format,
            ..Self::default()
        }
    }

    /// Build a minimal `PortContract` from a name + a `SemanticType`.
    /// Used by Tier F property-test generators
    /// (`crates/eval-adapters/src/property/port.rs`); production paths
    /// use `from_edam` or hand-author the full struct.
    pub fn with_semantic_type(name: impl Into<String>, semantic_type: SemanticType) -> Self {
        Self {
            name: name.into(),
            semantic_type,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_default() {
        let p = PortContract::default();
        let json = serde_json::to_string(&p).unwrap();
        let back: PortContract = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn from_edam_synthesizes_typed_port() {
        let p = PortContract::from_edam("counts", Some("data:3917"), Some("format:3475"));
        assert_eq!(p.name, "counts");
        assert!(matches!(
            p.semantic_type,
            SemanticType::OntologyTerm { ref iri, .. } if iri == "data:3917"
        ));
        assert!(p.physical_format.is_some());
    }

    #[test]
    fn from_edam_handles_missing_data() {
        let p = PortContract::from_edam("counts", None, None);
        assert!(matches!(p.semantic_type, SemanticType::Opaque { .. }));
        assert!(p.physical_format.is_none());
    }

    #[test]
    fn round_trip_with_facets() {
        let mut facets = BTreeMap::new();
        facets.insert("strandedness".into(), serde_json::json!("rf"));
        facets.insert("paired_end".into(), serde_json::json!(true));
        facets.insert("read_length".into(), serde_json::json!(150));

        let p = PortContract {
            name: "fastq_in".into(),
            semantic_type: SemanticType::edam("data:2044", "Sequence"),
            physical_format: Some(FormatRef {
                iri: "format:1930".into(),
                label: Some("FASTQ".into()),
                extension: Some("fastq.gz".into()),
            }),
            modality: Some("bulk_rnaseq".into()),
            facets,
            ..Default::default()
        };
        let yaml = serde_yml::to_string(&p).unwrap();
        let back: PortContract = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn cardinality_iterate_until_round_trips() {
        let c = Cardinality::IterateUntil { max_iterations: 8 };
        let json = serde_json::to_string(&c).unwrap();
        let back: Cardinality = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }
}
