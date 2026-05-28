//! Session-level facts the emitter writes into
//! `policies/intake-facts.json`. The AWS sizing layer reads these
//! facts to pick a high-water instance shape.

use crate::classify::ClassificationResult;
use crate::project_class::ProjectClass;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
/// IntakeFacts data.
pub struct IntakeFacts {
    /// Modality.
    pub modality: String,
    /// Defaults to `Bioinformatics` so sessions persisted before this
    /// field existed load unchanged.
    #[serde(default)]
    pub project_class: ProjectClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Organism taxon id.
    pub organism_taxon_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Organism name.
    pub organism_name: Option<String>,
    /// Methods sourced from `ClassificationResult::methods_specified`.
    pub methods: Vec<String>,
    /// Populated from structured capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sample_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Coverage depth.
    pub coverage_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Cell count.
    pub cell_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Database size gb.
    pub database_size_gb: Option<u32>,
    /// Pinned data accessions. One entry per upstream
    /// dataset (GEO/SRA/ENA/dbGaP/etc.) the SME committed to at
    /// intake. Resolved once + frozen so re-emissions of the same
    /// intake reference identical bytes; enables FAIR re-runnability
    /// (RO-Crate `hasPart` SHA-256 entries cross-reference these).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned_accessions: Vec<PinnedAccession>,
    /// Pinned reference bundles (assembly + annotation).
    /// One entry per reference distribution committed at intake. The
    /// fields together form the reproducibility-bearing key — assembly
    /// (e.g. `GRCh38.p14`), source release tag (e.g. `Ensembl 115`),
    /// and the SHA-256 of the FASTA + GTF tarball.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned_reference_bundles: Vec<PinnedReferenceBundle>,
    /// Phase G of the literature-atom plan — SME opt-in for the
    /// `review_prior_work` + `contextualize_findings_with_literature`
    /// atom family. Default false; flipping to true causes the v4
    /// composer to include the optional literature atoms in supported
    /// archetypes (bulk_rnaseq_de, chip_seq_peaks, variant_calling).
    /// Set via the existing `set_intake_field` mutation tool.
    #[serde(default)]
    pub literature_review_requested: bool,
    /// Sub-archetype small-task exclusion list — mirrors
    /// `Session.excluded_atoms`. Defaults to empty; not surfaced in the
    /// emitted policies/intake-facts.json when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_atoms: Vec<String>,
}

/// One upstream accession with provenance hash.
///
/// The four fields capture the reproducibility-bearing identity:
/// where the data lives (`repo`), what to ask for (`accession`),
/// when the bytes were anchored (`version_or_date_accessed`), and a
/// content hash so re-fetched bytes can be byte-compared. Per Round-4
/// §22.12, this is the right primitive for our scale — DataLad / DVC
/// / lakeFS are out-of-scope for the compiler.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct PinnedAccession {
    /// Repository identifier — `geo`, `sra`, `ena`, `dbgap`,
    /// `proteomexchange`, `metabolights`, `zenodo`, `figshare`,
    /// `bioproject`, etc. Lower-case slug; the validator accepts an
    /// open enumeration (new repos can be added without a code change).
    pub repo: String,
    /// Accession id as known to the repo (e.g. `GSE123456`,
    /// `SRX9876543`, `ERP123456`, `phs001234.v1.p1`,
    /// `PXD019987`, `MTBLS321`, `10.5281/zenodo.7654321`).
    pub accession: String,
    /// Either a version tag from the upstream repo (e.g. `v1.p1` for
    /// dbGaP) or an ISO-8601 date the bytes were anchored on.
    pub version_or_date_accessed: String,
    /// Content hash for byte-equality across re-fetches. Format:
    /// `sha256:<hex>` or `md5:<hex>`. None when the upstream repo
    /// doesn't publish a stable manifest hash; in that case the
    /// SHA-256 is computed locally on first download and pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub content_hash: Option<String>,
}

/// One pinned reference bundle.
///
/// Per Round-4 §22.12 the reproducibility-bearing key is `(assembly,
/// release, hash)`. GRCh39 is "indefinitely postponed" so GRCh38.p14
/// stays current; capturing the patch number is non-optional.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct PinnedReferenceBundle {
    /// Genome assembly identifier including patch (e.g.
    /// `GRCh38.p14`, `GRCm39`, `Pf3D7v68`). Patch number is part of
    /// the identity — bumping the patch is a separate amendment.
    pub assembly: String,
    /// Source release tag (e.g. `Ensembl 115`, `GENCODE 47`,
    /// `RefSeq release 224`, `Custom TruSeq Stranded mRNA`). Tells
    /// the SME *and* the FAIR consumer which annotation set was
    /// pinned alongside the assembly.
    pub release: String,
    /// SHA-256 of the FASTA + GTF tarball, format `sha256:<hex>`.
    /// Computed at intake time on the first download; pinned for the
    /// session lifetime. Re-emissions verify against this hash.
    pub content_hash: String,
}

impl IntakeFacts {
    /// Extract a minimal IntakeFacts snapshot from the classifier
    /// output. Scaling fields remain `None`; call
    /// `with_scaling_from_map` or `from_classification_with_scaling`
    /// to hydrate them from structured-capture values.
    pub fn from_classification(c: &ClassificationResult) -> Self {
        let organism = c.organisms.first();
        Self {
            modality: c.modality.clone(),
            project_class: ProjectClass::default(),
            organism_taxon_id: organism.map(|o| o.taxon_id),
            organism_name: organism.map(|o| o.name.clone()),
            methods: c
                .methods_specified
                .iter()
                .map(|m| m.method.clone())
                .collect(),
            sample_count: None,
            coverage_depth: None,
            cell_count: None,
            database_size_gb: None,
            pinned_accessions: Vec::new(),
            pinned_reference_bundles: Vec::new(),
            literature_review_requested: false,
            excluded_atoms: Vec::new(),
        }
    }

    /// Override the project class after construction. Typically called
    /// by the classifier stage once `classify_project_class` has run
    /// over the intake text (see §8.B.3).
    pub fn with_project_class(mut self, class: ProjectClass) -> Self {
        self.project_class = class;
        self
    }

    /// Hydrate the four scaling fields from a
    /// `BTreeMap<String, String>` keyed by the canonical
    /// structured-capture field names (`sample_count`,
    /// `coverage_depth`, `cell_count`, `database_size_gb`).
    ///
    /// Values that don't parse as u32 are silently dropped (the card's
    /// UX hint requires a positive integer; anything malformed stays
    /// None — the high-water resolver treats None as "use the
    /// unscaled base requirement").
    pub fn with_scaling_from_map(
        mut self,
        map: &std::collections::BTreeMap<String, String>,
    ) -> Self {
        fn parse_u32(m: &std::collections::BTreeMap<String, String>, key: &str) -> Option<u32> {
            m.get(key)
                .and_then(|v| v.trim().parse::<u32>().ok())
                .filter(|&n| n > 0)
        }
        if let Some(n) = parse_u32(map, "sample_count") {
            self.sample_count = Some(n);
        }
        if let Some(n) = parse_u32(map, "coverage_depth") {
            self.coverage_depth = Some(n);
        }
        if let Some(n) = parse_u32(map, "cell_count") {
            self.cell_count = Some(n);
        }
        if let Some(n) = parse_u32(map, "database_size_gb") {
            self.database_size_gb = Some(n);
        }
        self
    }

    /// Convenience: classification + scaling map in a single call.
    /// Equivalent to `from_classification(c).with_scaling_from_map(m)`
    /// and kept for ergonomic symmetry with the structured-capture
    /// call sites.
    pub fn from_classification_with_scaling(
        c: &ClassificationResult,
        map: &std::collections::BTreeMap<String, String>,
    ) -> Self {
        Self::from_classification(c).with_scaling_from_map(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{MethodSpec, OrganismInfo};

    fn sample_classification() -> ClassificationResult {
        ClassificationResult {
            modality: "single_cell_rnaseq".into(),
            taxonomy_path: String::new(),
            domain: String::new(),
            workflow_description: String::new(),
            edam_topic: String::new(),
            edam_operation: String::new(),
            confidence: 0.7,
            confidence_label: "high".into(),
            organisms: vec![OrganismInfo {
                name: "Homo sapiens".into(),
                taxon_id: 9606,
            }],
            methods_specified: vec![
                MethodSpec {
                    stage: "alignment".into(),
                    method: "star-2pass".into(),
                },
                MethodSpec {
                    stage: "differential_expression".into(),
                    method: "deseq2".into(),
                },
            ],
            data_sources: vec![],
            intake_text: String::new(),
            goal: None,
            archetype_id: None,
            additional_modalities: vec![],
            tie_candidates: vec![],
        }
    }

    #[test]
    fn from_classification_populates_modality_and_organism() {
        let facts = IntakeFacts::from_classification(&sample_classification());
        assert_eq!(facts.modality, "single_cell_rnaseq");
        assert_eq!(facts.organism_taxon_id, Some(9606));
        assert_eq!(facts.organism_name.as_deref(), Some("Homo sapiens"));
        assert_eq!(facts.methods, vec!["star-2pass", "deseq2"]);
    }

    #[test]
    fn scaling_fields_default_to_none() {
        let facts = IntakeFacts::from_classification(&sample_classification());
        assert!(facts.sample_count.is_none());
        assert!(facts.coverage_depth.is_none());
        assert!(facts.cell_count.is_none());
        assert!(facts.database_size_gb.is_none());
    }

    #[test]
    fn missing_organism_stays_none() {
        let mut c = sample_classification();
        c.organisms.clear();
        let facts = IntakeFacts::from_classification(&c);
        assert!(facts.organism_taxon_id.is_none());
        assert!(facts.organism_name.is_none());
    }

    #[test]
    fn serde_roundtrip_with_scaling_fields_set() {
        let facts = IntakeFacts {
            modality: "bulk_rnaseq".into(),
            project_class: ProjectClass::Bioinformatics,
            organism_taxon_id: Some(10090),
            organism_name: Some("Mus musculus".into()),
            methods: vec!["deseq2".into()],
            sample_count: Some(42),
            coverage_depth: Some(30),
            cell_count: None,
            database_size_gb: Some(12),
            pinned_accessions: Vec::new(),
            pinned_reference_bundles: Vec::new(),
            literature_review_requested: false,
            excluded_atoms: Vec::new(),
        };
        let json = serde_json::to_string(&facts).unwrap();
        let back: IntakeFacts = serde_json::from_str(&json).unwrap();
        assert_eq!(facts, back);
        assert!(
            !json.contains("cell_count"),
            "None fields must not serialize"
        );
    }

    #[test]
    fn project_class_defaults_to_bioinformatics() {
        let facts = IntakeFacts::from_classification(&sample_classification());
        assert_eq!(facts.project_class, ProjectClass::Bioinformatics);
    }

    /// Pinned accessions + reference bundles round-trip
    /// through serde and stay empty by default. Existing on-disk
    /// IntakeFacts JSON without these two fields deserialize cleanly
    /// (additive serde, default = empty Vec).
    #[test]
    fn pinned_accessions_and_reference_bundles_default_empty_and_roundtrip() {
        let facts = IntakeFacts {
            modality: "bulk_rnaseq".into(),
            project_class: ProjectClass::Bioinformatics,
            organism_taxon_id: Some(9606),
            organism_name: Some("Homo sapiens".into()),
            methods: vec![],
            sample_count: None,
            coverage_depth: None,
            cell_count: None,
            database_size_gb: None,
            pinned_accessions: vec![PinnedAccession {
                repo: "geo".into(),
                accession: "GSE123456".into(),
                version_or_date_accessed: "2026-04-15".into(),
                content_hash: Some("sha256:abc123".into()),
            }],
            pinned_reference_bundles: vec![PinnedReferenceBundle {
                assembly: "GRCh38.p14".into(),
                release: "Ensembl 115".into(),
                content_hash: "sha256:def456".into(),
            }],
            literature_review_requested: false,
            excluded_atoms: Vec::new(),
        };
        let json = serde_json::to_string(&facts).unwrap();
        let back: IntakeFacts = serde_json::from_str(&json).unwrap();
        assert_eq!(facts, back);
        assert_eq!(back.pinned_accessions[0].accession, "GSE123456");
        assert_eq!(back.pinned_reference_bundles[0].assembly, "GRCh38.p14");

        // Legacy on-disk IntakeFacts JSON without the new fields
        // deserializes cleanly (additive serde with default).
        let legacy = r#"{"modality":"bulk_rnaseq","methods":[]}"#;
        let parsed: IntakeFacts = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.pinned_accessions.len(), 0);
        assert_eq!(parsed.pinned_reference_bundles.len(), 0);
    }

    #[test]
    fn with_project_class_overrides_default() {
        let facts = IntakeFacts::from_classification(&sample_classification())
            .with_project_class(ProjectClass::ClinicalTrial);
        assert_eq!(facts.project_class, ProjectClass::ClinicalTrial);
    }

    #[test]
    fn project_class_absent_from_json_deserializes_as_bioinformatics() {
        let json = r#"{"modality":"bulk_rnaseq","methods":[]}"#;
        let facts: IntakeFacts = serde_json::from_str(json).unwrap();
        assert_eq!(facts.project_class, ProjectClass::Bioinformatics);
    }

    #[test]
    fn serde_roundtrip_default_only() {
        let facts = IntakeFacts::from_classification(&sample_classification());
        let json = serde_json::to_string(&facts).unwrap();
        let back: IntakeFacts = serde_json::from_str(&json).unwrap();
        assert_eq!(facts, back);
    }

    #[test]
    fn with_scaling_from_map_parses_canonical_keys() {
        use std::collections::BTreeMap;
        let mut map = BTreeMap::new();
        map.insert("sample_count".into(), "42".into());
        map.insert("coverage_depth".into(), "30".into());
        map.insert("cell_count".into(), "5000".into());
        map.insert("database_size_gb".into(), "150".into());
        let facts =
            IntakeFacts::from_classification(&sample_classification()).with_scaling_from_map(&map);
        assert_eq!(facts.sample_count, Some(42));
        assert_eq!(facts.coverage_depth, Some(30));
        assert_eq!(facts.cell_count, Some(5000));
        assert_eq!(facts.database_size_gb, Some(150));
    }

    #[test]
    fn with_scaling_from_map_drops_unparseable() {
        use std::collections::BTreeMap;
        let mut map = BTreeMap::new();
        map.insert("sample_count".into(), "not-a-number".into());
        map.insert("coverage_depth".into(), "0".into()); // zero filtered out
        map.insert("cell_count".into(), "   ".into());
        map.insert("database_size_gb".into(), "12 GB".into()); // trailing unit
        let facts =
            IntakeFacts::from_classification(&sample_classification()).with_scaling_from_map(&map);
        assert!(facts.sample_count.is_none());
        assert!(facts.coverage_depth.is_none());
        assert!(facts.cell_count.is_none());
        assert!(facts.database_size_gb.is_none());
    }

    #[test]
    fn with_scaling_from_map_tolerates_whitespace() {
        use std::collections::BTreeMap;
        let mut map = BTreeMap::new();
        map.insert("sample_count".into(), "  42  ".into());
        let facts =
            IntakeFacts::from_classification(&sample_classification()).with_scaling_from_map(&map);
        assert_eq!(facts.sample_count, Some(42));
    }

    #[test]
    fn with_scaling_from_map_ignores_non_canonical_keys() {
        use std::collections::BTreeMap;
        let mut map = BTreeMap::new();
        map.insert("random_field".into(), "nonsense".into());
        map.insert("sample_count".into(), "10".into());
        let facts =
            IntakeFacts::from_classification(&sample_classification()).with_scaling_from_map(&map);
        assert_eq!(facts.sample_count, Some(10));
        // No new fields leak into IntakeFacts.
        assert!(facts.coverage_depth.is_none());
    }

    #[test]
    fn from_classification_with_scaling_is_equivalent_to_chained_calls() {
        use std::collections::BTreeMap;
        let mut map = BTreeMap::new();
        map.insert("sample_count".into(), "7".into());
        let a = IntakeFacts::from_classification_with_scaling(&sample_classification(), &map);
        let b =
            IntakeFacts::from_classification(&sample_classification()).with_scaling_from_map(&map);
        assert_eq!(a, b);
    }
}
