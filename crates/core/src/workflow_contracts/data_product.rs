//! Data product contracts per design §12.
//!
//! A `DataProductContract` is a typed description of a *concrete*
//! input/artifact — what the SME has, not what an atom expects.
//! The composer matches data products against atom input ports via
//! the compatibility engine. The dataset profiler produces these
//! from FASTQ/BAM/VCF/matrix files; `IntakeFacts` fields are
//! stitched into typed contracts via `intake_port_mapper`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use super::port::{Cardinality, JsonSchemaRef};
use super::semantic_type::SemanticType;

/// Physical representation — where the artifact lives and how to
/// fetch it. URLs and digests carry pinning info; the harness
/// resolves these at dispatch time.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct PhysicalRepresentation {
    /// File URI (`file://`, `s3://`, `drs://`). DRS resolution lives
    /// in the deferred GA4GH branch; today we accept the URI but
    /// the harness only handles `file://` + `s3://` per existing
    /// support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub uri: Option<String>,
    /// Optional content digest (sha256, etc.) for byte-pinning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub digest: Option<String>,
    /// File size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub size_bytes: Option<u64>,
    /// MIME type or rough file kind (`application/x-bam`, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub media_type: Option<String>,
}

/// Biological context — design §12 facets that drive scientific
/// compatibility.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct BiologicalContext {
    /// Organism (NCBI taxon id or scientific name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub organism: Option<String>,
    /// Genome build (`GRCh38.p14`, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub genome_build: Option<String>,
    /// Annotation release (`Ensembl 110`, `GENCODE v44`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub annotation_version: Option<String>,
    /// Coordinate system (`0-based`, `1-based`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub coordinate_system: Option<String>,
    /// Feature space (`gene`, `transcript`, `peak`, `cell`,
    /// `metabolite`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub feature_space: Option<String>,
    /// Tissue / sample type. Free-form string; UBERON/CL term
    /// references are accepted when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tissue: Option<String>,
}

/// Assay context — design §12. Currently absent from the codebase;
/// today's `IntakeFacts` captures these as free-form strings. This
/// is the typed home.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct AssayContext {
    /// Sequencing/assay platform (`Illumina NovaSeq`, `10x
    /// Chromium`, `Visium`, `Olink Explore`, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub platform: Option<String>,
    /// Library construction protocol (`TruSeq`, `Smart-seq3`,
    /// `cell hashing`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub library_construction: Option<String>,
    /// Kit identifier (vendor + part number).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kit: Option<String>,
    /// True for paired-end reads.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paired_end: bool,
    /// Read length in bp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub read_length: Option<u32>,
    /// Strandedness (`unstranded`, `fr-firststrand`, `fr-secondstrand`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub strandedness: Option<String>,
    /// True when reads carry UMIs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_umi: bool,
    /// True when reads carry cell barcodes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_cell_barcode: bool,
}

/// Statistical state — design §12.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct StatisticalState {
    /// True when values are raw (counts, intensities).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_raw: bool,
    /// Normalization method (`tpm`, `vsn`, `cpm`, `quantile`,
    /// `none`). `None` when not yet normalized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub normalization: Option<String>,
    /// Transformation (`log2`, `log10`, `arcsinh`, `none`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub transformation: Option<String>,
    /// True when batch-corrected.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub batch_corrected: bool,
    /// Free-form uncertainty model (`gaussian`, `negbinom`, `none`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub uncertainty_model: Option<String>,
}

/// Identifier system — design §12. A data product can carry
/// multiple identifier systems with declared mapping authority and
/// lossiness when one IS lifts to another (e.g. gene symbol →
/// Ensembl).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct IdentifierSystem {
    /// Identifier authority (`ensembl`, `ncbi_gene`, `gene_symbol`,
    /// `refseq`, `ucsc_known_gene`).
    pub authority: String,
    /// Optional release pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub release: Option<String>,
    /// Mapping authority for cross-system lookups (`hgnc`,
    /// `mygene`, `biomart`). `None` when no mapping is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mapping_authority: Option<String>,
    /// True when conversions to other systems lose information
    /// (e.g. gene symbol → Ensembl is lossy).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lossy_conversion: bool,
}

/// Privacy class for data products — separate from the port-side
/// classification because data carries its own privacy regardless
/// of the consuming port's declared class.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyClass {
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

/// Per-artifact QC metric contract. Populated by the dataset
/// profiler and consumed by the `QcGate` adapter family. Atoms
/// declare quality requirements here; the harness enforces them
/// at dispatch time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct QualityMetricContract {
    /// Stable metric id (`mapping_rate`, `q30_pct`, `mt_pct`).
    pub id: String,
    /// Threshold expression (CEL). E.g. `value >= 0.85`.
    pub threshold: String,
    /// `Hard` rejects, `Soft` warns + assumption-ledgers.
    #[serde(default)]
    pub severity: super::port::ConstraintSeverity,
    /// Human description of the rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
}

/// A typed description of a *concrete* artifact the SME presents to
/// the composer. Design §12.
///
/// Note: derives `PartialEq` but not `Eq` because `SemanticType` does
/// not implement `Eq` (the v4 P6 `LocalExtensionMaturity::GraduationCandidate`
/// variant carries an `f32 success_rate`). Callers needing `Eq`
/// semantics compare on `id` or `semantic_type.stable_id()` instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct DataProductContract {
    /// Stable id within the planning context (e.g. `intake_fastq_0`).
    pub id: String,

    /// Open-world semantic type.
    pub semantic_type: SemanticType,

    /// Physical representation (URI, digest, size, media type).
    #[serde(default)]
    pub physical: PhysicalRepresentation,

    /// Biological facets.
    #[serde(default)]
    pub biological: BiologicalContext,

    /// Assay facets (sequencing/library/UMI/etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub assay: Option<AssayContext>,

    /// Statistical state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub statistical: Option<StatisticalState>,

    /// JSON Schema validating the on-disk shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub structural_schema: Option<JsonSchemaRef>,

    /// Cardinality (single / optional / many).
    #[serde(default)]
    pub cardinality: Cardinality,

    /// Identifier systems carried by this artifact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifiers: Vec<IdentifierSystem>,

    /// Privacy class.
    #[serde(default)]
    pub privacy: PrivacyClass,

    /// QC contracts. Populated by the dataset profiler; checked by
    /// QcGate adapters at dispatch time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quality: Vec<QualityMetricContract>,

    /// True when the contract was synthesized from a textual
    /// description rather than file inspection. Lower trust;
    /// the planner penalizes paths whose inputs are
    /// description-only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub description_only: bool,
}

impl DataProductContract {
    /// Construct a minimum-viable contract with just an id and
    /// semantic type. Tests + early-phase migration helpers use this.
    pub fn skeleton(id: impl Into<String>, semantic_type: SemanticType) -> Self {
        Self {
            id: id.into(),
            semantic_type,
            physical: PhysicalRepresentation::default(),
            biological: BiologicalContext::default(),
            assay: None,
            statistical: None,
            structural_schema: None,
            cardinality: Cardinality::default(),
            identifiers: Vec::new(),
            privacy: PrivacyClass::default(),
            quality: Vec::new(),
            description_only: true,
        }
    }

    /// Project a `PortContract` into a concrete `DataProductContract`
    /// representing the artifact a producer would emit through that
    /// port. The id is `<atom_id>:<port_name>@<depth>` so the id is
    /// stable across replays of `forward_search` (depth disambiguates
    /// repeat appearances of the same atom in iterative searches).
    /// Helper for the v4 planner: projects a `PortContract` into a
    /// concrete `DataProductContract`.
    pub fn from_port(port: &super::port::PortContract, atom_id: &str, depth: u32) -> Self {
        Self {
            id: format!("{atom_id}:{}@{depth}", port.name),
            semantic_type: port.semantic_type.clone(),
            physical: PhysicalRepresentation::default(),
            biological: BiologicalContext {
                organism: port.organism.clone(),
                genome_build: port.genome_build.clone(),
                annotation_version: port.annotation_version.clone(),
                coordinate_system: port.coordinate_system.clone(),
                feature_space: None,
                tissue: None,
            },
            // Assay context isn't carried on PortContract — leave None.
            assay: None,
            // Normalize the SME-supplied `normalization_state` string
            // before classifying: trim + ASCII-lowercase so `"RAW"`,
            // `" Raw "`, `"raw"` all collapse to the same predicate.
            // A `"raw"` normalisation state describes UNPROCESSED
            // data, so the statistical contract is omitted entirely —
            // a raw artifact has no normalisation / transformation /
            // batch-correction story to assert, and downstream
            // matchers should treat "no statistical contract" as
            // "raw" rather than seeing `StatisticalState { is_raw:
            // true, normalization: Some("raw"), ... }` which would
            // (incorrectly) claim the port has a populated normaliser.
            statistical: port.normalization_state.as_ref().and_then(|n| {
                let canon = n.trim().to_ascii_lowercase();
                if canon == "raw" {
                    None
                } else {
                    Some(StatisticalState {
                        is_raw: false,
                        normalization: Some(canon),
                        transformation: None,
                        batch_corrected: false,
                        uncertainty_model: None,
                    })
                }
            }),
            structural_schema: port.structural_schema.clone(),
            cardinality: port.cardinality.clone(),
            identifiers: Vec::new(),
            privacy: match port.privacy_class {
                super::port::PortPrivacyClass::Public => PrivacyClass::Public,
                super::port::PortPrivacyClass::Internal => PrivacyClass::Internal,
                super::port::PortPrivacyClass::Sensitive => PrivacyClass::Sensitive,
                super::port::PortPrivacyClass::Phi => PrivacyClass::Phi,
                super::port::PortPrivacyClass::Restricted => PrivacyClass::Restricted,
            },
            quality: Vec::new(),
            // Synthesized from a port shape — lower trust than a
            // profiler-derived contract.
            description_only: true,
        }
    }

    /// Hardcoded fixture for the v4 forward-search tests: a paired-end
    /// FASTQ data product (`data:2044` / `format:1930`) suitable for
    /// driving `forward_search` to atoms whose input ports declare
    /// the same semantic type and physical format.
    pub fn sample_paired_fastq() -> Self {
        Self {
            id: "intake_paired_fastq_0".into(),
            semantic_type: SemanticType::OntologyTerm {
                iri: "data:2044".into(),
                label: "Sequence reads".into(),
                ontology_version: Some("EDAM-1.25".into()),
            },
            physical: PhysicalRepresentation {
                uri: None,
                digest: None,
                size_bytes: None,
                media_type: Some("application/x-fastq".into()),
            },
            biological: BiologicalContext {
                organism: Some("Homo sapiens".into()),
                ..Default::default()
            },
            assay: Some(AssayContext {
                platform: Some("Illumina NovaSeq".into()),
                paired_end: true,
                read_length: Some(150),
                ..Default::default()
            }),
            statistical: None,
            structural_schema: None,
            cardinality: Cardinality::One,
            identifiers: Vec::new(),
            privacy: PrivacyClass::Internal,
            quality: Vec::new(),
            description_only: false,
        }
    }

    /// Hardcoded fixture for the v4 backward-search tests: a bulk
    /// RNA-seq differential-expression results table
    /// (`data:0951` / `format:3475`). Mirrors the output port shape
    /// of the `differential_expression` atom — same EDAM data type,
    /// same physical format, same `de_tested` statistical state — so
    /// `backward_search` can unify it with that atom's output port
    /// and recursively decompose into the upstream chain
    /// (normalisation → qc_preprocessing → quantification →
    /// alignment → sequence_trimming → raw_qc → data_acquisition).
    /// Fixture for the v4 backward-search tests.
    pub fn sample_de_table() -> Self {
        Self {
            id: "goal_de_table_0".into(),
            semantic_type: SemanticType::OntologyTerm {
                iri: "data:0951".into(),
                label: "Statistical estimate score".into(),
                ontology_version: Some("EDAM-1.25".into()),
            },
            physical: PhysicalRepresentation {
                uri: None,
                digest: None,
                size_bytes: None,
                media_type: Some("text/tab-separated-values".into()),
            },
            biological: BiologicalContext {
                organism: Some("Homo sapiens".into()),
                feature_space: Some("gene".into()),
                ..Default::default()
            },
            assay: None,
            statistical: Some(StatisticalState {
                is_raw: false,
                normalization: Some("normalized".into()),
                transformation: None,
                batch_corrected: false,
                uncertainty_model: None,
            }),
            structural_schema: None,
            cardinality: Cardinality::One,
            identifiers: Vec::new(),
            privacy: PrivacyClass::Internal,
            quality: Vec::new(),
            description_only: false,
        }
    }

    /// Seed for clinical-trial + time-series intake. The
    /// `data_import` atom's input port is a
    /// `ecaax:dataset_descriptor` (SME-supplied tabular/file pointer),
    /// not FASTQ. Seeding the planner with paired-end FASTQ for these
    /// project classes caused `GoalUnreachable` because the forward
    /// search couldn't bridge `data:2044` (sequence reads) to the
    /// CDISC ADaM tabular shape that drives the clinical / time-series
    /// pipelines. This sample mirrors the `data_import` input port
    /// shape so the planner can unify the seed against it on the very
    /// first forward step.
    pub fn sample_dataset_descriptor() -> Self {
        Self {
            id: "intake_dataset_descriptor_0".into(),
            semantic_type: SemanticType::LocalExtension {
                namespace: "ecaax".into(),
                id: "dataset_descriptor".into(),
                proposed_parent_terms: vec!["data:2531".into()],
                definition: "SME-declared input file descriptor (path, format, label)".into(),
                maturity: super::semantic_type::default_minted(),
            },
            physical: PhysicalRepresentation {
                uri: None,
                digest: None,
                size_bytes: None,
                media_type: Some("text/csv".into()),
            },
            biological: BiologicalContext::default(),
            assay: None,
            statistical: None,
            structural_schema: None,
            cardinality: Cardinality::Many,
            identifiers: Vec::new(),
            privacy: PrivacyClass::Internal,
            quality: Vec::new(),
            description_only: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_skeleton() {
        let dp = DataProductContract::skeleton("input_0", SemanticType::edam("data:1383", ""));
        let json = serde_json::to_string(&dp).unwrap();
        let back: DataProductContract = serde_json::from_str(&json).unwrap();
        assert_eq!(dp, back);
    }

    #[test]
    fn round_trip_full_shape() {
        let dp = DataProductContract {
            id: "tumor_bam".into(),
            semantic_type: SemanticType::edam("data:0863", "Sequence alignment"),
            physical: PhysicalRepresentation {
                uri: Some("s3://bucket/tumor.bam".into()),
                digest: Some("sha256:abc".into()),
                size_bytes: Some(123_456_789),
                media_type: Some("application/x-bam".into()),
            },
            biological: BiologicalContext {
                organism: Some("Homo sapiens".into()),
                genome_build: Some("GRCh38.p14".into()),
                annotation_version: Some("GENCODE v44".into()),
                coordinate_system: Some("0-based".into()),
                feature_space: None,
                tissue: Some("breast tumor".into()),
            },
            assay: Some(AssayContext {
                platform: Some("Illumina NovaSeq".into()),
                paired_end: true,
                read_length: Some(150),
                strandedness: Some("unstranded".into()),
                ..Default::default()
            }),
            statistical: None,
            structural_schema: None,
            cardinality: Cardinality::One,
            identifiers: vec![IdentifierSystem {
                authority: "ensembl".into(),
                release: Some("110".into()),
                mapping_authority: None,
                lossy_conversion: false,
            }],
            privacy: PrivacyClass::Phi,
            quality: vec![QualityMetricContract {
                id: "mapping_rate".into(),
                threshold: "value >= 0.85".into(),
                severity: super::super::port::ConstraintSeverity::Hard,
                description: Some("Mapping rate should be at least 85%".into()),
            }],
            description_only: false,
        };
        let yaml = serde_yml::to_string(&dp).unwrap();
        let back: DataProductContract = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(dp, back);
    }
}
