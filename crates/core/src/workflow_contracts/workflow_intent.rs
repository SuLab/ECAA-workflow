//! `WorkflowIntent` — strict superset of today's `GoalSpec` +
//! `IntakeFacts`. Design §6.
//!
//! The type shape coexists with `GoalSpec` and `IntakeFacts` so
//! existing composer APIs continue to work; conversion helpers
//! bridge between the typed intent and the legacy shapes.

use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::str::FromStr;
use ts_rs::TS;

use super::data_product::{DataProductContract, PrivacyClass};
use super::evidence::RiskClass;
use crate::population_coverage::CohortDescriptor;

/// V4 typed bioinformatics modality enum.
///
/// Used as the key for `OntologyScopeMatrix` lookups (D2 / F22)
/// and emitted as the typed substrate event payload. Today's
/// classifier code paths still operate on `Option<String>`
/// modality identifiers (per `config/modality-keywords.yaml`);
/// this enum is the canonical typed form. v3 P11 / v4 P6 widen
/// the keyword classifier so `BioinformaticsModality::from_str`
/// is the single source of truth.
///
/// The variant set mirrors `config/_modality-ontology-coverage.schema.json`
/// exactly. Adding a modality requires (1) a new variant here,
/// (2) a row in `config/modality-ontology-coverage.yaml`, (3) a
/// schema enum bump, and (4) widening keyword-classification config.
///
/// R2-N20 — `ModalityId(String)` was introduced
/// alongside this enum as a registry-validated newtype. The enum
/// remains the closed coarse-category type used by the ontology
/// scope matrix; `ModalityId` is the open-world identifier
/// consumed by the per-modality YAML registry under
/// `config/modalities/<id>.yaml`. `From<BioinformaticsModality>
/// for ModalityId` and `ModalityId::as_coarse_category()` bridge
/// the two; see `ModalityId` below.
#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Ord,
    PartialOrd,
    Hash,
    TS,
    Default,
    schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum BioinformaticsModality {
    /// BulkRnaseq variant.
    BulkRnaseq,
    /// SingleCellRnaseq variant.
    SingleCellRnaseq,
    /// VariantCalling variant.
    VariantCalling,
    /// ChipSeq variant.
    ChipSeq,
    /// Metagenomics variant.
    Metagenomics,
    /// Proteomics variant.
    Proteomics,
    /// Metabolomics variant.
    Metabolomics,
    /// SpatialOmics variant.
    SpatialOmics,
    /// Phylogenetics variant.
    Phylogenetics,
    /// StructuralBiology variant.
    StructuralBiology,
    /// MultiOmics variant.
    MultiOmics,
    #[default]
    /// GenericOmics variant.
    GenericOmics,
}

impl BioinformaticsModality {
    /// Canonical lowercase wire identifier (matches the YAML enum
    /// constant in `config/_modality-ontology-coverage.schema.json`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BulkRnaseq => "bulk_rnaseq",
            Self::SingleCellRnaseq => "single_cell_rnaseq",
            Self::VariantCalling => "variant_calling",
            Self::ChipSeq => "chip_seq",
            Self::Metagenomics => "metagenomics",
            Self::Proteomics => "proteomics",
            Self::Metabolomics => "metabolomics",
            Self::SpatialOmics => "spatial_omics",
            Self::Phylogenetics => "phylogenetics",
            Self::StructuralBiology => "structural_biology",
            Self::MultiOmics => "multi_omics",
            Self::GenericOmics => "generic_omics",
        }
    }
}

impl FromStr for BioinformaticsModality {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "bulk_rnaseq" => Ok(Self::BulkRnaseq),
            "single_cell_rnaseq" => Ok(Self::SingleCellRnaseq),
            "variant_calling" => Ok(Self::VariantCalling),
            "chip_seq" => Ok(Self::ChipSeq),
            "metagenomics" => Ok(Self::Metagenomics),
            "proteomics" => Ok(Self::Proteomics),
            "metabolomics" => Ok(Self::Metabolomics),
            "spatial_omics" => Ok(Self::SpatialOmics),
            "phylogenetics" => Ok(Self::Phylogenetics),
            "structural_biology" => Ok(Self::StructuralBiology),
            "multi_omics" => Ok(Self::MultiOmics),
            "generic_omics" => Ok(Self::GenericOmics),
            other => Err(format!("unknown bioinformatics modality: {}", other)),
        }
    }
}

/// R2-N20 — open-world modality identifier newtype.
///
/// `BioinformaticsModality` is a closed enum of 12 coarse categories;
/// it matches the `config/_modality-ontology-coverage.schema.json`
/// enum exactly because the ontology-scope matrix is intentionally
/// Coarse (per forbidden/primary/secondary ontologies vary
/// by modality CLASS, not by per-protocol modality id).
///
/// The per-modality YAML registry under `config/modalities/<id>.yaml`
/// Is open-world — 19 keyword-routable modalities at A.S5,
/// growing as new bench protocols come online (cut_tag, chip_exo,
/// ribo_seq, immunopeptidomics, hi_chip, starr_seq, single_cell_vdj,
/// crispr_screen_scrnaseq, methylation, spatial_transcriptomics,
/// long_read_rnaseq, etc.). Forcing every new YAML to land a code-side
/// enum variant blocks operators from adding modalities without a
/// Rust release.
///
/// `ModalityId` is the registry-validated identifier consumed by
/// the per-modality registry. Validation is runtime — `ModalityId`
/// itself is just a wrapped `String`; the registry asserts at load
/// time that each id is reachable from a YAML manifest. The coarse
/// category lookup (for ontology-scope checks) goes through
/// `as_coarse_category()`, which delegates to the YAML manifest's
/// `coarse_category:` field.
///
/// Migration policy (deliberately partial in this commit): the closed
/// `BioinformaticsModality` enum stays put as the source of truth for
/// `OntologyScopeMatrix` keys; `From<BioinformaticsModality> for
/// ModalityId` lifts the enum into the newtype where call sites need
/// to talk to the open registry. Wholesale migration of the 6
/// existing call sites is deferred to a follow-up so the schema enum
/// and the ontology-scope keyspace stay in lockstep until the
/// registry-load validation lands.
#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Ord,
    PartialOrd,
    Hash,
    TS,
    schemars::JsonSchema,
)]
#[ts(export)]
#[serde(transparent)]
pub struct ModalityId(pub String);

impl ModalityId {
    /// Wrap any string as a `ModalityId`. Validation that the id
    /// resolves through `config/modalities/<id>.yaml` happens at
    /// registry load time, not construction time — `ModalityId`
    /// is intentionally permissive at the type level so callers can
    /// thread a YAML-sourced id through without parse-error noise.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the underlying id (matches the YAML manifest stem).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ModalityId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for ModalityId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<BioinformaticsModality> for ModalityId {
    /// Lift a coarse-category enum into the open-world newtype.
    /// Returns the canonical `snake_case` wire id matching the YAML
    /// enum constant in `_modality-ontology-coverage.schema.json`.
    fn from(m: BioinformaticsModality) -> Self {
        Self(m.as_str().to_string())
    }
}

impl std::fmt::Display for ModalityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// SME-stated goal + structured intake. Persisted in the session;
/// re-emitted into `runtime/workflow_intent.json` so the composer is
/// byte-stable on re-runs.
///
/// `schema_version` is `semver::Version`; `Version` doesn't implement
/// `Default`, so we provide a hand-written `Default` impl below that
/// produces `current_workflow_intent_version()` for the version
/// field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct WorkflowIntent {
    /// Stable id (typically `session_id` or a stable hash of the
    /// goal + inputs).
    ///
    /// Note: derives `PartialEq` but not `Eq` because `available_data:
    /// Vec<DataProductContract>` references `SemanticType`, which no
    /// longer implements `Eq` (v4 P6 `LocalExtensionMaturity::GraduationCandidate`
    /// carries an `f32 success_rate`). Set-style dedup compares on `id`.
    pub id: String,

    /// Schema version.
    ///
    /// Stored as `semver::Version`; the `schema_version_serde`
    /// adapter accepts both legacy bare-`u64` values and canonical
    /// SemVer strings on read and writes the canonical SemVer string.
    /// Older on-disk artifacts deserialize transparently as
    /// `<n>.0.0`.
    #[serde(
        default = "default_schema_version",
        with = "crate::migration::schema_version_serde"
    )]
    #[ts(type = "string")]
    #[schemars(with = "String")]
    pub schema_version: Version,

    /// Free-text SME goal statement (the LLM's first turn input,
    /// already cleaned of greeting/conversational noise).
    pub goal: String,

    /// What modality this is about (mirrors today's
    /// `ClassificationResult.modality`). `None` when not yet
    /// classified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub modality: Option<String>,

    /// Project class (`research`, `clinical_trial`, `time_series`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project_class: Option<String>,

    /// Available data — typed contracts. A future dataset profiler
    /// populates these; today an empty vec is accepted for
    /// description-only intake.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_data: Vec<DataProductContract>,

    /// Desired outputs. Each entry names a typed deliverable the
    /// SME wants (e.g. "ranked DE table", "QC report").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub desired_outputs: Vec<DesiredOutput>,

    /// Hard/soft constraints (e.g. "no network", "container
    /// only", "prefer reproducible methods").
    #[serde(default)]
    pub constraints: ConstraintsBlock,

    /// Free-text uncertainties — values the SME flagged as
    /// guessed/inferred. Downstream phases may produce assumptions
    /// from these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncertainties: Vec<UncertaintyEntry>,

    /// Privacy/regulatory flags.
    #[serde(default, skip_serializing_if = "is_empty_privacy")]
    pub privacy: PrivacyBlock,

    /// Preferred execution constraints (backend, region, budget).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub execution_preferences: Vec<ExecutionPreference>,

    /// User-facing explanation preferences (rendered in the LLM's
    /// reply style and in UI cards).
    #[serde(default)]
    pub explanation_style: UserExplanationStyle,

    /// Free-form structured-capture map. Mirrors
    /// `IntakeFacts`-flavored ad-hoc fields; future work retires this
    /// once typed fields cover everything.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(type = "Record<string, unknown>")]
    pub legacy_intake_facts: BTreeMap<String, serde_json::Value>,

    /// SME-declared sample cohort. The intake form sets this for
    /// clinical projects so the composer's population-coverage gate
    /// can compare the cohort against the workflow's validated set.
    /// `None` for non-clinical or unspecified sessions (the gate
    /// then short-circuits to "no check").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sample_cohort: Option<CohortDescriptor>,
}

fn default_schema_version() -> Version {
    crate::migration::current_workflow_intent_version()
}

impl Default for WorkflowIntent {
    fn default() -> Self {
        Self {
            id: String::default(),
            schema_version: default_schema_version(),
            goal: String::default(),
            modality: None,
            project_class: None,
            available_data: Vec::new(),
            desired_outputs: Vec::new(),
            constraints: ConstraintsBlock::default(),
            uncertainties: Vec::new(),
            privacy: PrivacyBlock::default(),
            execution_preferences: Vec::new(),
            explanation_style: UserExplanationStyle::default(),
            legacy_intake_facts: BTreeMap::new(),
            sample_cohort: None,
        }
    }
}

/// One desired output as the SME stated it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct DesiredOutput {
    /// Free-text label (`"differential expression table"`).
    pub label: String,
    /// Optional EDAM data IRI typing the deliverable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub edam_data: Option<String>,
    /// Optional EDAM format IRI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub edam_format: Option<String>,
    /// True when the output must be human-readable (figure, report)
    /// rather than machine-readable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub human_readable: bool,
}

/// Hard/soft constraints block.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct ConstraintsBlock {
    /// Preferred backend (`local`, `aws`, `slurm`). Mirrors
    /// `ECAA_EXECUTOR_MODE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preferred_backend: Option<String>,
    /// True when network access must be denied for executor tasks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_network: bool,
    /// True when only container-pinned tasks are allowed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub container_only: bool,
    /// True when reproducibility is hard-required (composer
    /// penalizes non-deterministic atoms).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub prefer_reproducible: bool,
    /// Free-text other constraints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub free_text: Vec<String>,
}

/// One uncertainty (SME-stated or LLM-inferred).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct UncertaintyEntry {
    /// Field or topic the uncertainty applies to.
    pub topic: String,
    /// Free-text statement.
    pub statement: String,
    /// Best-effort risk classification.
    #[serde(default)]
    pub risk: RiskClass,
}

/// Privacy/regulatory flags block.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct PrivacyBlock {
    /// Default privacy class for un-tagged data products.
    #[serde(default, skip_serializing_if = "is_default_privacy")]
    pub default_class: PrivacyClass,
    /// True when HIPAA constraints apply.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hipaa: bool,
    /// True when 21 CFR Part 11 constraints apply.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cfr_part11: bool,
    /// Free-text other regulatory contexts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regulatory_context: Vec<String>,
}

fn is_empty_privacy(p: &PrivacyBlock) -> bool {
    matches!(p.default_class, PrivacyClass::Public)
        && !p.hipaa
        && !p.cfr_part11
        && p.regulatory_context.is_empty()
}

fn is_default_privacy(p: &PrivacyClass) -> bool {
    matches!(p, PrivacyClass::Public)
}

/// One execution preference (backend, region, budget).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ExecutionPreference {
    /// Key.
    pub key: String,
    /// Value.
    pub value: String,
}

/// SME-facing explanation style. Renders into LLM tone + UI density.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum UserExplanationStyle {
    /// Domain-friendly: less methods jargon, more interpretation.
    #[default]
    DomainFriendly,
    /// Technical: full method names, statistical terminology.
    Technical,
    /// Minimal: just confirm/proceed without explanation.
    Minimal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_default() {
        let i = WorkflowIntent::default();
        let json = serde_json::to_string(&i).unwrap();
        let back: WorkflowIntent = serde_json::from_str(&json).unwrap();
        assert_eq!(i, back);
    }

    #[test]
    fn bioinformatics_modality_from_str_round_trips_all_variants() {
        for (s, expected) in [
            ("bulk_rnaseq", BioinformaticsModality::BulkRnaseq),
            (
                "single_cell_rnaseq",
                BioinformaticsModality::SingleCellRnaseq,
            ),
            ("variant_calling", BioinformaticsModality::VariantCalling),
            ("chip_seq", BioinformaticsModality::ChipSeq),
            ("metagenomics", BioinformaticsModality::Metagenomics),
            ("proteomics", BioinformaticsModality::Proteomics),
            ("metabolomics", BioinformaticsModality::Metabolomics),
            ("spatial_omics", BioinformaticsModality::SpatialOmics),
            ("phylogenetics", BioinformaticsModality::Phylogenetics),
            (
                "structural_biology",
                BioinformaticsModality::StructuralBiology,
            ),
            ("multi_omics", BioinformaticsModality::MultiOmics),
            ("generic_omics", BioinformaticsModality::GenericOmics),
        ] {
            let parsed: BioinformaticsModality = s.parse().expect("parse");
            assert_eq!(parsed, expected, "from_str({s})");
            let json = serde_json::to_value(expected).unwrap();
            assert_eq!(json, serde_json::Value::String(s.to_string()));
        }
    }

    #[test]
    fn bioinformatics_modality_unknown_returns_err() {
        let r: Result<BioinformaticsModality, _> = "bogus_modality".parse();
        assert!(r.is_err());
    }

    #[test]
    fn modality_id_lifts_from_coarse_enum() {
        // R2-N20 — every closed-enum variant round-trips into the
        // open-world newtype with the same wire id the schema enum
        // accepts.
        let cases: [(BioinformaticsModality, &str); 12] = [
            (BioinformaticsModality::BulkRnaseq, "bulk_rnaseq"),
            (
                BioinformaticsModality::SingleCellRnaseq,
                "single_cell_rnaseq",
            ),
            (BioinformaticsModality::VariantCalling, "variant_calling"),
            (BioinformaticsModality::ChipSeq, "chip_seq"),
            (BioinformaticsModality::Metagenomics, "metagenomics"),
            (BioinformaticsModality::Proteomics, "proteomics"),
            (BioinformaticsModality::Metabolomics, "metabolomics"),
            (BioinformaticsModality::SpatialOmics, "spatial_omics"),
            (BioinformaticsModality::Phylogenetics, "phylogenetics"),
            (
                BioinformaticsModality::StructuralBiology,
                "structural_biology",
            ),
            (BioinformaticsModality::MultiOmics, "multi_omics"),
            (BioinformaticsModality::GenericOmics, "generic_omics"),
        ];
        for (enum_v, wire) in cases {
            let id: ModalityId = enum_v.into();
            assert_eq!(id.as_str(), wire, "{:?} → ModalityId wire id", enum_v);
            assert_eq!(format!("{}", id), wire);
        }
    }

    #[test]
    fn modality_id_accepts_open_world_ids() {
        // R2-N20 — `ModalityId` is permissive at the type level so
        // operators can land a new `config/modalities/<id>.yaml`
        // without a Rust release. Registry-load validation is the
        // gate that asserts a given id is reachable.
        let novel = ModalityId::from("cut_tag");
        assert_eq!(novel.as_str(), "cut_tag");
        let from_string = ModalityId::from(String::from("ribo_seq"));
        assert_eq!(from_string.as_str(), "ribo_seq");
        // Untagged serde: serializes/deserializes as a bare string.
        let json = serde_json::to_string(&novel).unwrap();
        assert_eq!(json, "\"cut_tag\"");
        let back: ModalityId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, novel);
    }

    #[test]
    fn round_trip_full_intent() {
        let i = WorkflowIntent {
            id: "s_abc".into(),
            schema_version: Version::new(1, 0, 0),
            goal: "Identify differentially expressed genes between treated and control".into(),
            modality: Some("bulk_rnaseq".into()),
            project_class: Some("research".into()),
            available_data: vec![],
            desired_outputs: vec![DesiredOutput {
                label: "DE table".into(),
                edam_data: Some("data:3917".into()),
                edam_format: Some("format:3475".into()),
                human_readable: false,
            }],
            constraints: ConstraintsBlock {
                preferred_backend: Some("local".into()),
                no_network: false,
                container_only: true,
                prefer_reproducible: true,
                free_text: vec![],
            },
            uncertainties: vec![UncertaintyEntry {
                topic: "strandedness".into(),
                statement: "Library prep is older; strandedness uncertain".into(),
                risk: RiskClass::Moderate,
            }],
            privacy: PrivacyBlock {
                default_class: PrivacyClass::Internal,
                hipaa: false,
                cfr_part11: false,
                regulatory_context: vec![],
            },
            execution_preferences: vec![],
            explanation_style: UserExplanationStyle::DomainFriendly,
            legacy_intake_facts: BTreeMap::new(),
            sample_cohort: None,
        };
        let yaml = serde_yml::to_string(&i).unwrap();
        let back: WorkflowIntent = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(i, back);
    }
}
