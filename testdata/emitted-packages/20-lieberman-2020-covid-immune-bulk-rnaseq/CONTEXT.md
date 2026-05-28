# Workflow Context

**Modality:** bulk_rnaseq
**Domain:** computational biology
**Description:** Bulk RNA-seq differential expression analysis. Standard pipeline: raw QC,
trim, align, quantify, normalise, test for DE between conditions, then
pathway enrichment on the ranked DE result. Mirrors today's
`config/modalities/rnaseq-de.yaml` + `config/archetypes/`.

**EDAM topic:** topic:3308
**EDAM operation:** operation:3223
**Confidence:** high (100%)

## Organisms
- Homo sapiens (taxon:9606)

## Methods mentioned in SME prose

_Keyword-scraped from intake text; see SME discovery decisions above for authoritative values._

- alignment: STAR
- alignment: HISAT2
- quantification: Salmon
- quantification: featureCounts
- differential_expression: DESeq2
- differential_expression: edgeR

## Data sources
- GSE161731 (NCBI GEO Series)

## SME intake text

We want to recreate the SARS-CoV-2 nasopharyngeal bulk RNA-seq study from Lieberman et al. 2020 PLOS Biology (DOI 10.1371/journal.pbio.3000849), GEO accession GSE161731. The dataset is 154 nasopharyngeal-swab Illumina NovaSeq 6000 polyA-selected RNA-seq libraries: 88 COVID-positive patients plus 66 with other respiratory pathogens or virus-negative controls. Patient metadata covers age, sex, and SARS-CoV-2 viral load. The paper characterizes the in vivo antiviral host transcriptional response and reports a type-I interferon signature that scales with viral load.

Process the FASTQ libraries with a standard bulk RNA-seq pipeline. Run FastQC + MultiQC for per-sample QC, align reads to GRCh38 with STAR (two-pass) or HISAT2 against the GENCODE v44 primary assembly + comprehensive annotation, then quantify gene-level counts with featureCounts (or salmon in mapping-based mode against the GENCODE v44 transcriptome) using strand-specific settings appropriate for the polyA-selected library kit.

For differential expression, fit a generalized linear model in DESeq2 (or edgeR quasi-likelihood) with the contrast COVID-positive vs negative, adjusting for age (continuous) and sex (categorical). Apply Benjamini-Hochberg correction. Volcano plot uses log2FC on the x-axis and -log10 adjusted p-value on the y-axis with thresholds at |log2FC| > 1 and FDR < 0.05.

For enrichment, run GSEA against MSigDB Hallmark (v2023.2.Hs) and the Reactome interferon-signaling gene set. Report Normalized Enrichment Score and FDR per pathway. Also build a custom interferon-stimulated gene (ISG) signature score (mean log2-normalized expression of the 50-gene Mostafavi 2016 ISG core list) and correlate per-sample ISG score against viral load (Spearman ρ).

Final outputs: per-sample QC table; full DE table (log2FC + standard error + Wald statistic + adjusted p-value); volcano plot; PCA colored by COVID status, age, and sex; GSEA enrichment table with NES + FDR per pathway; Hallmark and Reactome bar plots of top enriched pathways; ISG signature × viral-load scatter with Spearman correlation.

Acceptance: differentially expressed gene count at FDR < 0.05 within +/-20% of the paper's reported ~2,400-DEG set; top-50 interferon-response Hallmark/Reactome enrichments overlap >= 70% with the paper's reported pathway list; positive-control sex-linked genes (XIST, RPS4Y1, KDM5D) show |log2FC| > 5 in the expected direction; PCA shows separation of COVID-positive from negative on PC1 or PC2.

