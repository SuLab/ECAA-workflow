# Workflow Context

**Modality:** methylation
**Domain:** computational biology
**Description:** DNA methylation differential analysis covering WGBS, RRBS, EM-seq, and
methylation arrays (EPIC / 450K). Standard pipeline: raw QC, bisulfite
/ enzymatic-converted-read alignment, per-CpG methylation extraction,
DMR (differentially methylated region) calling, annotation.

The methylation modality is keyword-routed (config/modality-keywords.yaml
entry id=methylation). Goal triple is data:0951 (statistical estimate
score, since DMRs are reported with effect-size + adjusted p-value) /
format:3475 (tabular). The same atom inventory (data_acquisition →
raw_qc → ... → differential_expression → reporting) handles
methylation chemistry because the per-stage atom is a discovery wrapper
in v4 — the agent picks Bismark / bwa-meth / minfi at the alignment
stage and methylKit / dmrseq / minfi at the DE stage at runtime.

**EDAM topic:** topic:3940
**EDAM operation:** operation:3204
**Confidence:** high (100%)

## Organisms
- Homo sapiens (taxon:9606)

## Data sources
- GSE186458 (NCBI GEO Series)

## SME intake text

We want to recreate the whole-genome bisulfite sequencing (WGBS) human cell-type methylation atlas from Loyfer et al. 2023 Nature, GEO accession GSE186458. The atlas profiles 39 human cell types at single-CpG resolution to build a reference methylation landscape for deconvolution of liquid biopsy (cell-free DNA methylation) data. The cohort includes sorted primary human cell types from blood, solid tissues, and immune compartments, with 2-4 biological replicates per cell type.

Process WGBS reads using Trim Galore for adapter and quality trimming, then align with Bismark against the human GRCh38 reference genome (UCSC hg38 with ENCODE blacklist v2 excluded). Deduplicate with Bismark deduplicate_bismark. Extract CpG methylation calls with bismark2bedGraph + coverage2cytosine; require at least 5x coverage per CpG for inclusion in analysis. Compute global CpG methylation rate per sample; expect 65-85% genome-wide CpG methylation for somatic cells.

For cell-type-specific differentially methylated regions (DMRs), use DSS (v2.x) with a sliding window approach: window size 500 bp, smoothing span 500 bp. Call DMRs at delta-methylation > 0.3 and FDR < 0.01 vs all other cell types. The atlas should yield cell-type-specific hypomethylated blocks (tissue-specific enhancers) for each of the 39 cell types.

Build the methylation reference atlas as a matrix of CpG beta-values (rows = CpGs, columns = cell types). Select the top 1,000 most cell-type-discriminating CpG loci per cell type (lowest within-group variance, highest between-group variance by ANOVA F-statistic). Validate atlas specificity by leave-one-replicate-out deconvolution: each held-out replicate should deconvolve with >85% correct cell-type assignment using NNLS.

For liquid biopsy application, apply the atlas to plasma cell-free DNA methylation profiles from healthy donors and cancer patients. Deconvolute cfDNA methylation using the atlas reference to infer cell-type-of-origin contributions. Healthy plasma should show >80% hematopoietic contribution (granulocyte + monocyte + lymphocyte). Cancer patient plasma should show elevated contributions from tumor-of-origin cell types vs matched healthy controls.

Acceptance: at least 35/39 cell types with >200 cell-type-specific DMRs; atlas deconvolution leave-one-out accuracy >85%; cfDNA healthy donor hematopoietic fraction >80%.

