# Workflow Context

**Modality:** atac_seq
**Domain:** computational biology
**Description:** ATAC-seq accessible-region calling. Standard pipeline: raw QC with
insert-size distribution check, optional Tn5-aware trimming, alignment
with soft-clipping, MACS3 BAMPE peak calling (--shift -75 --extsize 150),
TSS-enrichment validation, and HOMER/MEME motif enrichment on
accessible-region centers. Mirrors `config/modalities/atac-seq.yaml` + `config/archetypes/`.

**EDAM topic:** topic:3179
**EDAM operation:** operation:3222
**Confidence:** high (100%)

## Organisms
- Mus musculus (taxon:10090)

## Methods mentioned in SME prose

_Keyword-scraped from intake text; see SME discovery decisions above for authoritative values._

- alignment: Bowtie2
- peak_calling: MACS2

## Data sources
- GSE111586 (NCBI GEO Series)

## SME intake text

We want to recreate the single-cell ATAC-seq mouse tissue atlas from Cusanovich et al. 2018 Cell, GEO accession GSE111586. The dataset is ~80,000 single-cell combinatorial indexing ATAC-seq (sci-ATAC-seq) profiles from 13 mouse tissues (brain, cerebellum, heart, kidney, liver, lung, spleen, small intestine, large intestine, bone marrow, thymus, skin, and testes) from adult C57BL/6 mice. The atlas is used to map cis-regulatory element accessibility at single-cell resolution across the mouse body.

Process sci-ATAC-seq reads using the pipeline described in Cusanovich et al.: demultiplex by cell barcode (25-bp combinatorial indices), trim adapters with Trimmomatic, align with Bowtie2 v2.x to mm10, remove PCR duplicates. Filter cells with fewer than 1,000 unique fragments or a TSS enrichment score below 4. Expected cell yield: approximately 61,000 high-quality cells after QC filtering.

Call peaks using MACS2 on the aggregate per-tissue pseudo-bulk, then build a master peak set by merging all tissue peak calls (max gap 500 bp). The expected master peak set is approximately 436,206 accessible sites. Build a cells-by-peaks binary accessibility matrix.

For dimensionality reduction, apply LSI (latent semantic indexing) on the TF-IDF transformed binary matrix (top 30 LSI components, removing the first component which captures read depth). Run UMAP on the top LSI components (components 2-30). Apply Louvain clustering to identify cell populations.

The atlas should recover tissue-of-origin clustering: cells from the same tissue cluster together before any tissue label is provided. Compute silhouette score for tissue-of-origin on the UMAP; expect mean silhouette > 0.3. Identify tissue-specific accessible peaks using one-vs-rest logistic regression; the top 1,000 tissue-specific peaks should show significant tissue specificity (AUC > 0.85 for the tissue-of-origin classifier).

For cis-regulatory annotation, overlap master peaks with ENCODE SCREEN registry of candidate cis-regulatory elements (cCREs) for mm10. Classify peaks as: proximal promoter (within 2 kb of TSS), distal enhancer (>2 kb from TSS, overlapping ENCODE enhancer), or other. Report fraction of peaks in each class per tissue type.

For TF motif analysis, run ChromVAR on the binary accessibility matrix to compute per-cell TF activity scores for JASPAR 2020 motif set (mouse). Expected tissue-specific TF activities: Nkx2-5 in heart, Gata1 in bone marrow erythroid compartment, Hnf4a in liver.

Acceptance: at least 55,000 high-quality cells after QC; master peak set >400,000 peaks; tissue-of-origin silhouette >0.3; tissue-specific TF motifs (Nkx2-5/heart, Gata1/bone marrow, Hnf4a/liver) with ChromVAR z-score > 2 in expected tissues.

