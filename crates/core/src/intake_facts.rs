//! Session-level facts the emitter writes into
//! `policies/intake-facts.json`. The AWS sizing layer reads these
//! facts to pick a high-water instance shape.

use crate::classify::ClassificationResult;
use crate::project_class::ProjectClass;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
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

/// Narrow keyword set that flips `literature_review_requested` true.
///
/// Detection is intentionally conservative: it fires ONLY when the SME
/// prose explicitly asks for literature grounding (compare-to-prior-work,
/// citations, contextualization). It must NOT fire on incidental mentions
/// of biology that happen to share a stem — every entry is a phrase the
/// SME would only write when they want the analysis grounded in
/// published work. Matched case-insensitively as substrings on the
/// whitespace-collapsed prose. Keeping this set small bounds the blast
/// radius: corpus scenarios that don't mention literature stay unchanged.
const LITERATURE_INTENT_KEYWORDS: &[&str] = &[
    "literature",
    "prior work",
    "references",
    "citations",
    "contextualize",
    "published",
];

/// Construct a minimal supplied-product contract for an SME-declared input
/// stage (called peaks / VCF / BAM). Mirrors `gene_count_matrix`'s shape
/// (`description_only: false`) so `prune_supplied_upstream` and the dispatch
/// seeding treat it like a concrete held artifact, not a description. Only
/// the `semantic_type` IRI is load-bearing for pruning.
fn supplied_product(
    id: &str,
    iri: &str,
    label: &str,
) -> crate::workflow_contracts::data_product::DataProductContract {
    let mut dp = crate::workflow_contracts::data_product::DataProductContract::skeleton(
        id,
        crate::workflow_contracts::semantic_type::SemanticType::edam(iri, label),
    );
    dp.description_only = false;
    dp
}

impl IntakeFacts {
    /// Detect explicit literature-grounding intent in intake prose.
    ///
    /// Returns `true` when the prose contains any of
    /// [`LITERATURE_INTENT_KEYWORDS`] (case-insensitive substring) OR the
    /// phrase "compared to (the )?literature". This is the single source
    /// of truth for whether `literature_review_requested` flips true; the
    /// conversation v4 gate and the emit path both call it. Default false:
    /// plain analysis prose leaves the opt-in atoms gated out.
    pub fn detect_literature_intent(prose: &str) -> bool {
        // Collapse internal whitespace so multi-word phrases match across
        // newlines / runs of spaces, and lower-case for case-insensitive
        // matching.
        let normalized = prose.split_whitespace().collect::<Vec<_>>().join(" ");
        let lower = normalized.to_lowercase();
        if LITERATURE_INTENT_KEYWORDS.iter().any(|k| lower.contains(k)) {
            return true;
        }
        // "compared to literature" / "compared to the literature" — the
        // "literature" stem already covers these, but keep the phrase
        // anchors explicit so the intent is documented even if the stem
        // list ever changes.
        lower.contains("compared to literature") || lower.contains("compared to the literature")
    }

    /// Detect a declared INPUT data stage in the SME prose — a processed data
    /// product the SME already holds, so input-stage-aware composition can prune
    /// the upstream chain that would otherwise produce it. Returns the available
    /// [`DataProductContract`], or `None` for the default raw (FASTQ) input.
    ///
    /// Detection is MODALITY-GATED (M7). A supplied data product is only
    /// honored when the requested modality actually has a producer for that
    /// product in its archetype — otherwise `prune_supplied_upstream` finds no
    /// producer and the seed is dead weight (or, worse, mis-seeds `available_data`
    /// with a type the pipeline never makes). Concretely:
    ///
    /// - **counts matrix** (`data:3917`) — only RNA-counts modalities
    ///   (`bulk_rnaseq`, `single_cell_rnaseq`, `long_read_rnaseq`,
    ///   `spatial_transcriptomics`) carry a `quantification`/counts producer.
    ///   A counts phrase on ChIP/ATAC/variant prose must NOT seed counts.
    /// - **called peaks** (`data:1255`) — only peak-calling modalities
    ///   (`chip_seq`, `atac_seq`, `cut_tag`, `chip_exo`).
    /// - **called variants / VCF** (`data:3498`) — only `variant_calling`/`gwas`.
    /// - **alignments / BAM** (`data:0863`) — modality-independent (every
    ///   read-based pipeline has an `alignment` producer), so gated only on a
    ///   known modality being present.
    ///
    /// `modality == None` (no classified modality in scope) fails SAFE to the
    /// raw seed: no stage is detected. Each family requires a possession-marker
    /// ("already" / "provided" / "prepared" / "start from" / "no raw") to
    /// co-occur with the product NOUN — a bare standalone verb ("already
    /// quantified") is too loose and is intentionally NOT a signal.
    pub fn detect_input_data_stage(
        prose: &str,
        modality: Option<&str>,
    ) -> Option<crate::workflow_contracts::data_product::DataProductContract> {
        // Without a classified modality we cannot prove a producer exists for
        // any supplied product — fail safe to the raw (FASTQ) seed.
        let modality = modality?;
        let lower = prose
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();

        // Possession markers — the SME declares they already HOLD the product
        // (vs. describing it as a pipeline STEP they want produced). A signal
        // fires only when a possession marker AND the product noun BOTH appear,
        // so pipeline-OUTPUT prose ("…alignment, gene-level counts, DESeq2…")
        // is never mistaken for a supplied input.
        const POSSESSION_MARKERS: &[&str] = &[
            "already prepared",
            "already have",
            "already processed",
            "provided",
            "prepared",
            "start from",
            "starting from",
            "no raw",
        ];
        // Clause segmentation for marker↔noun BINDING. A supplied-product signal
        // requires a possession marker AND the product noun to occur in the SAME
        // clause — not merely somewhere in the prose. Without this binding, a
        // variant-calling intake whose GOAL sentence names the OUTPUT ("…writing
        // one VCF per sample…") and whose INPUT sentence states a RAW product is
        // "provided" ("The input FASTQ files … are provided") cross-matches: the
        // unbound marker seeds a supplied data:3498 product, which
        // `composer::dispatch` turns into an available product and
        // `input_stage_prune::prune_supplied_upstream` then uses to DELETE the
        // entire variant-PRODUCING chain (raw_qc → align → variant_calling) and
        // rewire `variant_filtering` onto the ingest anchor with a type-violating
        // `data_acquisition.variants` (data:3498) edge. The marker must bind to
        // the noun, not merely co-occur in the document.
        // Segment on sentence terminators AND intra-sentence separators (comma,
        // colon, em-/en-dash) so a single run-on sentence does not bind a
        // possession marker to a downstream output noun across a clause boundary:
        // "using the provided FASTQ reads, call variants into one VCF per sample"
        // must NOT seed a supplied VCF — "provided" binds the raw input clause,
        // "VCF" sits in the produced-output clause. The hyphen-minus '-' is
        // deliberately EXCLUDED (it joins compound words: gene-level, RNA-seq,
        // splice-aware, long-read) — only the true dash characters split. A
        // narrower binding window can only REDUCE seeding (the safe direction for
        // an over-pruning bug): a genuinely-supplied product phrased with a comma
        // between marker and noun simply falls back to running the full pipeline.
        let clauses: Vec<&str> = lower
            .split(|c: char| {
                matches!(
                    c,
                    '.' | ';' | '!' | '?' | '\n' | ',' | ':' | '\u{2014}' | '\u{2013}'
                )
            })
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        // True iff some single clause contains BOTH a possession marker AND one
        // of `nouns`. Replaces the prior document-wide co-occurrence check.
        let bound = |nouns: &[&str]| -> bool {
            clauses.iter().any(|cl| {
                POSSESSION_MARKERS.iter().any(|m| cl.contains(m))
                    && nouns.iter().any(|n| cl.contains(n))
            })
        };

        // RNA-counts: gate on RNA-counts modalities + require a counts NOUN
        // ("counts matrix" / "count matrix" / "counts") to co-occur with a
        // possession marker. Bare standalone verbs ("already quantified",
        // "already counted") are deliberately dropped — they read as a step,
        // not a held artifact.
        const RNA_COUNTS_MODALITIES: &[&str] = &[
            "bulk_rnaseq",
            "single_cell_rnaseq",
            "long_read_rnaseq",
            "spatial_transcriptomics",
        ];
        const COUNTS_NOUNS: &[&str] = &["counts matrix", "count matrix", "counts"];
        if RNA_COUNTS_MODALITIES.contains(&modality) && bound(COUNTS_NOUNS) {
            return Some(
                crate::workflow_contracts::data_product::DataProductContract::gene_count_matrix(),
            );
        }

        // Called peaks: gate on peak-calling modalities + a peak NOUN.
        const PEAK_MODALITIES: &[&str] = &["chip_seq", "atac_seq", "cut_tag", "chip_exo"];
        const PEAK_NOUNS: &[&str] =
            &["called peaks", "peak calls", "narrowpeak", "peak set", "peaks"];
        if PEAK_MODALITIES.contains(&modality) && bound(PEAK_NOUNS) {
            return Some(supplied_product(
                "intake_called_peaks_0",
                "data:1255",
                "Called peaks",
            ));
        }

        // Called variants / VCF: gate on variant modalities + a variant NOUN.
        const VARIANT_MODALITIES: &[&str] = &["variant_calling", "gwas"];
        const VARIANT_NOUNS: &[&str] =
            &["vcf", "called variants", "variant calls", "variant set"];
        if VARIANT_MODALITIES.contains(&modality) && bound(VARIANT_NOUNS) {
            return Some(supplied_product(
                "intake_called_variants_0",
                "data:3498",
                "Sequence variant",
            ));
        }

        // Alignments / BAM: modality-independent (read-based pipelines all have
        // an `alignment` producer) but still require a known modality + a BAM
        // NOUN + a possession marker.
        const BAM_NOUNS: &[&str] =
            &["bam file", "bam files", "aligned reads", "alignments", "bam"];
        if bound(BAM_NOUNS) {
            return Some(supplied_product(
                "intake_alignment_0",
                "data:0863",
                "Sequence alignment",
            ));
        }

        None
    }

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
    fn detect_input_data_stage_recognises_supplied_counts() {
        use crate::workflow_contracts::semantic_type::SemanticType;
        // Counts supplied directly (pasilla-style) on an RNA-counts modality →
        // counts product detected. Each phrase pairs a counts NOUN with a
        // possession marker.
        for prose in [
            "Counts matrix already prepared (14,599 genes x 7 samples). No raw FASTQs — start from counts matrix.",
            "differential expression starting from a counts matrix",
            "we already have the counts matrix prepared, run DE",
        ] {
            let p = IntakeFacts::detect_input_data_stage(prose, Some("bulk_rnaseq"))
                .unwrap_or_else(|| panic!("expected a counts input stage for: {prose:?}"));
            match &p.semantic_type {
                SemanticType::OntologyTerm { iri, .. } => assert_eq!(iri, "data:3917"),
                other => panic!("expected counts ontology term, got {other:?}"),
            }
        }
        // Bare standalone verbs ("already quantified" / "already counted") are
        // NO LONGER signals — they read as a pipeline step, not a held artifact.
        for prose in ["We already quantified; run DE.", "samples already counted"] {
            assert!(
                IntakeFacts::detect_input_data_stage(prose, Some("bulk_rnaseq")).is_none(),
                "bare quantify/count verb must NOT seed counts: {prose:?}"
            );
        }
        // FASTQ input prose → no stage (default raw). Critically, a full
        // FASTQ-pipeline description that mentions producing "gene-level counts"
        // as a STEP must NOT trigger pruning (recount3-airway regression).
        for prose in [
            "bulk RNA-seq FASTQ files, align to GRCh38 and quantify with salmon",
            "call variants from whole-genome sequencing reads",
            "bulk RNA-seq FASTQs, four donors; FastQC and adapter trimming, \
             splice-aware alignment, gene-level counts, DESeq2-style normalization, \
             and a differential expression test",
        ] {
            assert!(
                IntakeFacts::detect_input_data_stage(prose, Some("bulk_rnaseq")).is_none(),
                "expected NO input stage (default raw) for: {prose:?}"
            );
        }
    }

    #[test]
    fn detect_input_data_stage_variant_calling_goal_is_not_supplied_variants() {
        // Regression (composer prune bug): the Nekrutenko mtDNA intake. The GOAL
        // sentence names the pipeline OUTPUT ("…writing one VCF per sample…") and a
        // SEPARATE sentence states the RAW input is provided ("The input FASTQ files
        // and the chrM reference are provided…"). The possession marker ("provided")
        // binds to FASTQ, NOT to the VCF output — so NO supplied-variant product may
        // be detected. A false positive here makes the dispatcher seed a synthetic
        // data:3498 product, which `input_stage_prune::prune_supplied_upstream` uses
        // to delete the entire raw_qc→align→variant_calling chain and emit a
        // type-violating `data_acquisition.variants → variant_filtering` edge.
        let prose = "Perform per-sample germline variant calling on four paired-end \
            Illumina mitochondrial (chrM) sequencing samples: align reads with bwa, \
            sort and index with samtools, then run variant calling with lofreq to \
            detect the full spectrum of short variants (SNVs and indels) in each \
            sample — including low-frequency heteroplasmic variants, not only \
            fixed/homoplasmic sites — writing one VCF per sample, and finally build \
            a collapsed per-variant table across samples. The input FASTQ files and \
            the chrM reference are provided in the inputs/ directory of this analysis; \
            use those exact files as the data source — do not synthesize, simulate, \
            or download substitute reads or references.";
        assert!(
            IntakeFacts::detect_input_data_stage(prose, Some("variant_calling")).is_none(),
            "a variant-CALLING goal with FASTQ provided must NOT be read as supplied variants"
        );
    }

    #[test]
    fn detect_input_data_stage_single_sentence_provided_fastq_is_not_supplied_variants() {
        // Residual of the composer prune bug: a SINGLE run-on sentence (no period
        // between the input clause and the output clause) where the possession
        // marker binds the RAW input and the VCF is a produced OUTPUT. Before the
        // intra-sentence (comma/dash) clause split, "provided" and "vcf"
        // co-occurred in the one clause and falsely seeded a supplied VCF →
        // over-pruned the calling chain. The finer split keeps "provided FASTQ
        // reads" and "one VCF per sample" in separate clauses, so no marker binds
        // the output noun and nothing is seeded (the safe fall-back to raw).
        for prose in [
            "Using the provided FASTQ reads, call variants and write one VCF per sample.",
            "From the provided raw reads — align, call, and emit a VCF of variants per sample.",
        ] {
            assert!(
                IntakeFacts::detect_input_data_stage(prose, Some("variant_calling")).is_none(),
                "a single-sentence 'provided FASTQ … produce VCF' must NOT seed supplied variants: {prose:?}"
            );
        }
    }

    #[test]
    fn detect_input_data_stage_genuinely_supplied_variants_still_detected() {
        // True positive must survive the binding fix: a possession marker and a
        // variant noun in the SAME clause = the SME really holds called variants
        // and wants only downstream work — pruning the calling chain is correct.
        use crate::workflow_contracts::semantic_type::SemanticType;
        for prose in [
            "We already have called variants in a VCF; just filter and annotate.",
            "start from the provided VCF of called variants",
        ] {
            let p = IntakeFacts::detect_input_data_stage(prose, Some("variant_calling"))
                .unwrap_or_else(|| panic!("expected supplied variants for: {prose:?}"));
            match &p.semantic_type {
                SemanticType::OntologyTerm { iri, .. } => assert_eq!(iri, "data:3498"),
                other => panic!("expected variant ontology term, got {other:?}"),
            }
        }
    }

    #[test]
    fn detect_literature_intent_on_explicit_keywords() {
        // Each phrase must flip the flag true.
        for prose in [
            "Please ground the findings in the published literature.",
            "Compare our DE genes to prior work in the field.",
            "Include references and citations for every called variant.",
            "I want the report to contextualize results against published studies.",
            "Add the relevant citations from PubMed.",
            "Compared to the literature, are these peaks novel?",
            "Compared to literature this looks consistent.",
        ] {
            assert!(
                IntakeFacts::detect_literature_intent(prose),
                "expected literature intent to fire on: {prose:?}"
            );
        }
    }

    #[test]
    fn detect_literature_intent_stays_false_on_plain_prose() {
        // Plain analysis prose with no literature-grounding ask.
        for prose in [
            "Call variants in mtDNA across 36 samples and report allele frequencies.",
            "Bulk RNA-seq differential expression between tumor and normal.",
            "Run quality control on some omics data.",
            "",
            "single-cell clustering with leiden and report marker genes",
        ] {
            assert!(
                !IntakeFacts::detect_literature_intent(prose),
                "literature intent must NOT fire on plain prose: {prose:?}"
            );
        }
    }

    #[test]
    fn detect_literature_intent_is_case_insensitive() {
        assert!(IntakeFacts::detect_literature_intent(
            "GROUND THIS IN THE LITERATURE"
        ));
        assert!(IntakeFacts::detect_literature_intent(
            "please add CITATIONS"
        ));
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
