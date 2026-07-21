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
    /// DAG fact: the emitted workflow includes the `review_prior_work` +
    /// `contextualize_findings_with_literature` atom family. The v4 composer
    /// adds literature contextualization unconditionally, so every emit records
    /// this `true` — it is a property of the emitted DAG, NOT captured SME
    /// intent. The `#[serde(alias)]` keeps packages emitted under the former
    /// `literature_review_requested` name deserializable.
    #[serde(default, alias = "literature_review_requested")]
    pub literature_review_included: bool,
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
    /// - **pre-computed DE results** (`data:3134`) — DE-capable modalities
    ///   (`bulk_rnaseq`, `single_cell_rnaseq`, `long_read_rnaseq`,
    ///   `spatial_transcriptomics`, `proteomics`) carry a
    ///   `differential_expression` producer. `data:3134` is the DE node's
    ///   OUTPUT-PORT type (the prune-match target), NOT the archetype goal type
    ///   `data:0951`.
    /// - **protein-abundance matrix** (`data:2976`) — proteomics-family
    ///   modalities (`proteomics`, `immunopeptidomics`). NOTE: the
    ///   `protein_quantification` producer port is a `LocalExtension`
    ///   (`ecaax:protein_abundance_matrix`, parent `data:2976`), and
    ///   `prune_supplied_upstream` matches producer ports by ontology-term IRI
    ///   equality only — so this seed is detected + declared available but does
    ///   NOT currently prune the search→quantify chain (documented at the
    ///   detection site).
    /// - **methylation beta matrix** (`data:3917`) — only the `methylation`
    ///   modality. The `methylation_de` archetype reuses the GENERIC
    ///   `quantification` atom for per-CpG extraction, whose OUTPUT port is
    ///   `data:3917` ("Count matrix"); the supplied beta matrix therefore
    ///   carries `data:3917` (NOT the archetype goal type `data:0951`) and
    ///   reuses the counts seed.
    /// - **taxonomy table** (`data:3028`) — only the `metagenomics` modality.
    ///   `data:3028` ("Taxonomy") is the `taxonomic_classification` node's
    ///   OUTPUT-PORT type (the prune-match target).
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
            // ribo_seq's `quantification` atom emits a footprint count matrix
            // (data:3917) exactly like the other RNA-counts modalities, so a
            // supplied-counts declaration must seed the counts-first entry.
            "ribo_seq",
        ];
        const COUNTS_NOUNS: &[&str] = &["counts matrix", "count matrix", "counts"];
        if RNA_COUNTS_MODALITIES.contains(&modality) && bound(COUNTS_NOUNS) {
            return Some(
                crate::workflow_contracts::data_product::DataProductContract::gene_count_matrix(),
            );
        }

        // Called peaks: gate on peak-calling modalities + a peak NOUN.
        // hi_chip carries a `peak_calling` atom (alongside its contact/loop
        // atoms), so a supplied-peaks declaration seeds the peaks entry.
        const PEAK_MODALITIES: &[&str] = &["chip_seq", "atac_seq", "cut_tag", "chip_exo", "hi_chip"];
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

        // Pre-computed DIFFERENTIAL-EXPRESSION RESULTS table: gate on
        // DE-capable modalities + a DE-results NOUN. This is the supplied
        // product for the BiomniBench da-15-8 shape — proteomics XLSX inputs
        // PLUS a pre-computed differential-expression results TSV (no FASTQ).
        // Without this category the supplied DE table is invisible to
        // input-stage pruning, so the lifted raw-read chain
        // (rnaseq_raw_qc → alignment → quantification → differential_expression,
        // or the proteomics search→quantify→DE chain) is never dropped and
        // blocks on NoUpstreamSequencingSubstrate / the differential_expression
        // validation contract.
        //
        // INPUT-TYPE RECOGNITION ONLY — method-neutral. Recognizing that the
        // SME already holds a DE results table prescribes no DE method, model,
        // or significance threshold; it only declares the product is available.
        //
        // The `differential_expression` atom produces `data:3134` ("Gene
        // expression data") on its `de_results` output port (VERIFIED:
        // config/stage-atoms/differential_expression.yaml outputs.de_results.iri).
        // `input_stage_prune::prune_supplied_upstream` matches a supplied
        // product against NODE OUTPUT PORT types, so the supplied DE table is
        // typed `data:3134` (the node output), NOT the archetype GOAL type
        // `data:0951` ("Statistical estimate score") — `data:0951` is the
        // bulk_rnaseq_de archetype's `goal_data`, never a lifted node port, so
        // seeding it would no-op the prune.
        const DE_MODALITIES: &[&str] = &[
            "bulk_rnaseq",
            "single_cell_rnaseq",
            "long_read_rnaseq",
            "spatial_transcriptomics",
            "proteomics",
        ];
        const DE_RESULTS_NOUNS: &[&str] = &[
            "differential expression results",
            "differential expression table",
            "differential expression result",
            "de results table",
            "de_results",
            "de results",
            "limma results",
            "limma output",
            "deseq2 results",
            "edger results",
            "differential abundance results",
            "differential abundance table",
        ];
        if DE_MODALITIES.contains(&modality) && bound(DE_RESULTS_NOUNS) {
            return Some(supplied_product(
                "intake_de_results_0",
                "data:3134",
                "Gene expression data",
            ));
        }

        // Proteomics protein-abundance matrix: gate on proteomics-family
        // modalities + an abundance-matrix NOUN. This is the supplied product
        // for a proteomics intake whose SME already holds the quantified
        // protein × sample matrix (ProteinGroups.txt / a MaxLFQ / directLFQ
        // intensity table) and wants only downstream DE + enrichment — no raw
        // spectra, no peptide search.
        //
        // INPUT-TYPE RECOGNITION ONLY — method-neutral. Recognizing that the
        // SME holds an abundance matrix prescribes no quantification strategy
        // (LFQ / TMT / SILAC / iBAQ) or DE method.
        //
        // IRI CHOICE (VERIFIED): the `protein_quantification` atom's
        // `protein_abundance` OUTPUT PORT is a LOCAL EXTENSION
        // (`ecaax:protein_abundance_matrix`, proposed parent `data:2976`)
        // — see config/stage-atoms/protein_quantification.yaml outputs. We seed
        // the proposed-parent ontology IRI `data:2976` ("Mass spectrometry
        // spectra"/protein-abundance family) because that is the closest EDAM
        // backbone term and matches `protein_quantification`'s top-level
        // `edam_data`. NOTE: `input_stage_prune::prune_supplied_upstream`
        // matches producer OUTPUT ports by ONTOLOGY-TERM IRI EQUALITY only
        // (`type_iri` returns `None` for a `LocalExtension`), so this seed does
        // NOT currently prune the proteomics search→quantify chain — the
        // producer port is a local extension, not `data:2976`. Detection is
        // still correct + useful (it declares the product available and stamps
        // the modifier); the prune becomes effective only once the producing
        // port carries an ontology-term `data:2976` (or the prune learns
        // local-extension parent subsumption). This mirrors how the other
        // categories seed via `supplied_product`, and is called out here so the
        // limitation is not mistaken for a bug.
        const PROTEOMICS_MODALITIES: &[&str] = &["proteomics", "immunopeptidomics"];
        const PROTEIN_ABUNDANCE_NOUNS: &[&str] = &[
            "protein abundance matrix",
            "intensity matrix",
            "proteingroups",
            "quantified proteins",
            "abundance matrix",
        ];
        if PROTEOMICS_MODALITIES.contains(&modality) && bound(PROTEIN_ABUNDANCE_NOUNS) {
            return Some(supplied_product(
                "intake_protein_abundance_0",
                "data:2976",
                "Protein abundance matrix",
            ));
        }

        // Methylation beta-value matrix: gate on the `methylation` modality + a
        // beta-matrix NOUN. This is the supplied product for a methylation
        // intake whose SME already holds the per-CpG / per-probe beta matrix
        // (minfi / array-derived) and wants only DMR calling + downstream work
        // — no raw bisulfite/EM-seq reads, no alignment.
        //
        // INPUT-TYPE RECOGNITION ONLY — method-neutral. No extraction tool
        // (Bismark / bwa-meth / minfi) or DMR method (methylKit / dmrseq) is
        // prescribed.
        //
        // IRI CHOICE (VERIFIED + CORRECTED): the candidate `data:0951` was
        // WRONG — that is the `methylation_de` archetype's GOAL type (the DMR
        // statistical-estimate table), never a lifted node OUTPUT port. The
        // `methylation_de` archetype reuses the GENERIC `quantification` atom
        // for per-CpG methylation extraction, whose `count_matrix` OUTPUT PORT
        // is ontology-term `data:3917` ("Count matrix") — see
        // config/stage-atoms/quantification.yaml outputs and
        // config/archetypes/methylation_de.yaml (quantification stage). So the
        // supplied beta matrix must be typed `data:3917` to match the
        // `quantification` producer node for `input_stage_prune`. `data:3917`
        // coincides with the RNA-counts IRI, so this reuses the existing
        // dispatch `Some("data:3917")` seeding arm — no new dispatch arm needed.
        const METHYLATION_BETA_NOUNS: &[&str] = &[
            "beta values",
            "beta matrix",
            "methylation matrix",
            "methylation levels",
        ];
        if modality == "methylation" && bound(METHYLATION_BETA_NOUNS) {
            return Some(
                crate::workflow_contracts::data_product::DataProductContract::gene_count_matrix(),
            );
        }

        // Metagenomics taxonomy table: gate on the `metagenomics` modality + a
        // taxonomy NOUN. This is the supplied product for a metagenomics intake
        // whose SME already holds the taxonomic-profile / OTU / ASV table
        // (Kraken2 / MetaPhlAn / QIIME2 output) and wants only diversity +
        // group-comparison work — no raw reads, no classification.
        //
        // INPUT-TYPE RECOGNITION ONLY — method-neutral. No classifier
        // (Kraken2 / MetaPhlAn / QIIME2) or reference DB is prescribed.
        //
        // IRI CHOICE (VERIFIED): the `taxonomic_classification` atom's
        // `taxonomic_assignments` OUTPUT PORT is ontology-term `data:3028`
        // ("Taxonomy") — see config/stage-atoms/taxonomic_classification.yaml
        // outputs. `data:3028` has exactly one producer in the
        // `metagenomics_taxonomic` archetype (`diversity_analysis` CONSUMES it,
        // it does not re-produce it), so `prune_supplied_upstream` cleanly drops
        // raw_qc → sequence_trimming → taxonomic_classification and rewires
        // `diversity_analysis` onto the staging anchor. Candidate `data:3028`
        // CONFIRMED correct.
        const TAXONOMY_NOUNS: &[&str] = &[
            "taxonomy table",
            "taxonomic profile",
            "taxonomic abundance",
            "otu table",
            "asv table",
        ];
        if modality == "metagenomics" && bound(TAXONOMY_NOUNS) {
            return Some(supplied_product(
                "intake_taxonomy_table_0",
                "data:3028",
                "Taxonomy",
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
            literature_review_included: false,
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
            literature_review_included: false,
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
            literature_review_included: false,
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
    fn detect_input_data_stage_recognises_supplied_de_results() {
        // D4 (BiomniBench da-15-8): the SME holds a PRE-COMPUTED
        // differential-expression results table (no FASTQ). Each phrase pairs a
        // DE-results NOUN with a possession marker, so the supplied DE product
        // is detected and the upstream raw-read / quantify chain can be pruned.
        // INPUT-TYPE RECOGNITION ONLY — no DE method is prescribed.
        use crate::workflow_contracts::semantic_type::SemanticType;
        for (prose, modality) in [
            (
                "We already have the differential expression results table; \
                 just run pathway enrichment and report.",
                "proteomics",
            ),
            (
                "No raw FASTQs — start from the provided de_results table.",
                "bulk_rnaseq",
            ),
            (
                "differential abundance results already prepared from the proteomics run",
                "proteomics",
            ),
            (
                "limma results provided; downstream enrichment only",
                "bulk_rnaseq",
            ),
        ] {
            let p = IntakeFacts::detect_input_data_stage(prose, Some(modality))
                .unwrap_or_else(|| panic!("expected a DE-results input stage for: {prose:?}"));
            match &p.semantic_type {
                // `data:3134` is the `differential_expression` node OUTPUT-PORT
                // type (the prune-match target), NOT the goal type `data:0951`.
                SemanticType::OntologyTerm { iri, .. } => assert_eq!(iri, "data:3134"),
                other => panic!("expected DE-results ontology term, got {other:?}"),
            }
        }
        // A FASTQ-pipeline description that mentions "a differential expression
        // test" as a STEP must NOT seed supplied DE results (recount3-airway
        // regression — the marker must bind a DE-results NOUN, not the verb).
        for (prose, modality) in [
            (
                "bulk RNA-seq FASTQs, four donors; FastQC and adapter trimming, \
                 splice-aware alignment, gene-level counts, DESeq2-style normalization, \
                 and a differential expression test",
                "bulk_rnaseq",
            ),
            (
                "run differential expression on the provided FASTQ reads",
                "bulk_rnaseq",
            ),
        ] {
            assert!(
                IntakeFacts::detect_input_data_stage(prose, Some(modality)).is_none(),
                "a DE STEP (not a held DE table) must NOT seed supplied DE results: {prose:?}"
            );
        }
    }

    #[test]
    fn detect_input_data_stage_recognises_supplied_protein_abundance() {
        // Proteomics intake where the SME already holds the quantified
        // protein × sample abundance matrix (ProteinGroups / intensity table)
        // and wants only downstream DE + enrichment. Each phrase pairs an
        // abundance-matrix NOUN with a possession marker. INPUT-TYPE
        // RECOGNITION ONLY — no quantification/DE method prescribed.
        use crate::workflow_contracts::semantic_type::SemanticType;
        for (prose, modality) in [
            (
                "We already have the protein abundance matrix; just run DE.",
                "proteomics",
            ),
            (
                "No raw spectra — start from the provided intensity matrix.",
                "proteomics",
            ),
            (
                "proteingroups table already prepared, downstream only",
                "proteomics",
            ),
            (
                "quantified proteins provided; run differential abundance and enrichment",
                "immunopeptidomics",
            ),
        ] {
            let p = IntakeFacts::detect_input_data_stage(prose, Some(modality))
                .unwrap_or_else(|| panic!("expected a protein-abundance input stage for: {prose:?}"));
            match &p.semantic_type {
                // `data:2976` is the proposed-parent ontology term of the
                // `protein_quantification` LocalExtension output port.
                SemanticType::OntologyTerm { iri, .. } => assert_eq!(iri, "data:2976"),
                other => panic!("expected protein-abundance ontology term, got {other:?}"),
            }
        }
        // A proteomics search→quantify pipeline description must NOT seed a
        // supplied abundance matrix (the marker must bind an abundance-matrix
        // NOUN, not the produce-it verb).
        for prose in [
            "DDA LC-MS/MS: search peptides with FragPipe, quantify proteins with MaxLFQ, test DE",
            "acquire raw spectra and quantify protein abundance across conditions",
        ] {
            assert!(
                IntakeFacts::detect_input_data_stage(prose, Some("proteomics")).is_none(),
                "a quantify STEP must NOT seed a supplied abundance matrix: {prose:?}"
            );
        }
        // Modality gating: an abundance-matrix phrase on a non-proteomics
        // modality must NOT seed the proteomics product.
        assert!(
            IntakeFacts::detect_input_data_stage(
                "abundance matrix already prepared",
                Some("bulk_rnaseq"),
            )
            .is_none(),
            "abundance-matrix noun on bulk_rnaseq must not seed proteomics product"
        );
    }

    #[test]
    fn detect_input_data_stage_recognises_supplied_methylation_beta() {
        // Methylation intake where the SME already holds the per-CpG / per-probe
        // beta matrix and wants only DMR calling + downstream work. Each phrase
        // pairs a beta-matrix NOUN with a possession marker. The supplied
        // product carries `data:3917` — the generic `quantification` atom's
        // OUTPUT port that `methylation_de` reuses — NOT the archetype goal
        // `data:0951`.
        use crate::workflow_contracts::semantic_type::SemanticType;
        for prose in [
            "We already have the beta values matrix; just call DMRs.",
            "No raw reads — start from the provided methylation matrix.",
            "beta matrix already prepared, downstream analysis only",
            "methylation levels provided per probe; run DMR calling",
        ] {
            let p = IntakeFacts::detect_input_data_stage(prose, Some("methylation"))
                .unwrap_or_else(|| panic!("expected a methylation-beta input stage for: {prose:?}"));
            match &p.semantic_type {
                SemanticType::OntologyTerm { iri, .. } => assert_eq!(iri, "data:3917"),
                other => panic!("expected methylation-beta ontology term, got {other:?}"),
            }
        }
        // A bisulfite-pipeline description that mentions extracting methylation
        // levels as a STEP must NOT seed a supplied beta matrix.
        for prose in [
            "WGBS: align with Bismark, extract per-CpG methylation levels, call DMRs with dmrseq",
        ] {
            assert!(
                IntakeFacts::detect_input_data_stage(prose, Some("methylation")).is_none(),
                "a methylation-extraction STEP must NOT seed a supplied beta matrix: {prose:?}"
            );
        }
        // Modality gating: a beta-matrix phrase on a non-methylation modality
        // must NOT seed the methylation product.
        assert!(
            IntakeFacts::detect_input_data_stage(
                "beta matrix already prepared",
                Some("bulk_rnaseq"),
            )
            .is_none(),
            "beta-matrix noun on bulk_rnaseq must not seed methylation product"
        );
    }

    #[test]
    fn detect_input_data_stage_recognises_supplied_taxonomy_table() {
        // Metagenomics intake where the SME already holds the taxonomic-profile
        // / OTU / ASV table and wants only diversity + group comparison. Each
        // phrase pairs a taxonomy NOUN with a possession marker. The supplied
        // product carries `data:3028` ("Taxonomy") — the
        // `taxonomic_classification` node OUTPUT-PORT type (the prune target).
        use crate::workflow_contracts::semantic_type::SemanticType;
        for prose in [
            "We already have the taxonomy table; just run diversity analysis.",
            "No raw reads — start from the provided taxonomic profile.",
            "otu table already prepared, downstream diversity only",
            "asv table provided; compute alpha and beta diversity",
            "taxonomic abundance table already prepared",
        ] {
            let p = IntakeFacts::detect_input_data_stage(prose, Some("metagenomics"))
                .unwrap_or_else(|| panic!("expected a taxonomy-table input stage for: {prose:?}"));
            match &p.semantic_type {
                SemanticType::OntologyTerm { iri, .. } => assert_eq!(iri, "data:3028"),
                other => panic!("expected taxonomy ontology term, got {other:?}"),
            }
        }
        // A metagenomics classification-pipeline description must NOT seed a
        // supplied taxonomy table (marker must bind a taxonomy NOUN, not the
        // classify verb).
        for prose in [
            "shotgun metagenomics: QC, trim, classify reads with Kraken2, then diversity",
        ] {
            assert!(
                IntakeFacts::detect_input_data_stage(prose, Some("metagenomics")).is_none(),
                "a classify STEP must NOT seed a supplied taxonomy table: {prose:?}"
            );
        }
        // Modality gating: a taxonomy phrase on a non-metagenomics modality
        // must NOT seed the taxonomy product.
        assert!(
            IntakeFacts::detect_input_data_stage(
                "taxonomy table already prepared",
                Some("bulk_rnaseq"),
            )
            .is_none(),
            "taxonomy noun on bulk_rnaseq must not seed metagenomics product"
        );
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
