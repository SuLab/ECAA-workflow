# Workflow Context

**Modality:** single_cell_rnaseq
**Domain:** computational biology
**Description:** scRNA-seq clustering + differential expression. Standard pipeline:
preprocess, cell-level QC, normalise, optional batch correction +
integration sweep, dimensionality reduction, cluster, annotate cell
types, test cluster-vs-rest or cross-condition DE, then pathway
enrichment. Mirrors today's `config/modalities/single-cell.yaml` + `config/archetypes/`.

**EDAM topic:** topic:3308
**EDAM operation:** operation:3432
**Confidence:** high (100%)

## Organisms
- Homo sapiens (taxon:9606)

## Methods mentioned in SME prose

_Keyword-scraped from intake text; see SME discovery decisions above for authoritative values._

- preprocessing: Cell Ranger
- clustering: Seurat

## Data sources
- GSE164378 (NCBI GEO Series)

## SME intake text

We want to recreate the Hao et al. 2021 Cell multimodal PBMC reference atlas. The dataset is GEO GSE164378: 161,764 cells from 10X 3' CITE-seq with 228 TotalSeq-A antibodies plus 49,147 cells from ECCITE-seq on 10X 5' with 54 antibodies, totalling 210,911 cells after QC and Cell Hashing-based doublet removal. The cohort is 8 healthy volunteers from an HIV vaccine trial sampled at 3 timepoints (day 0, day 3, day 7) post-VSV-vectored HIV vaccine = 24 samples.

Use Seurat v4 in R as the implementation. Normalize RNA with SCTransform v2 and ADT with centered-log-ratio (CLR) across-cells. Anchor-based integration on the RNA modality across the 24 samples first to remove sample-of-origin clustering. Then 50 PCs on RNA, a separate PCA on ADT, and FindMultiModalNeighbors() for the WNN graph at k=20 -- modality weights are unsupervised, computed per cell with bandwidth-kernel softmax, and sum to 1.

Cluster with Leiden at a resolution that yields 57 fine-grained Level 3 clusters. Annotate hierarchically: 8 Level 1 broad categories, 30 Level 2 categories, 57 Level 3 fine-grained subsets.

Headline acceptance: recover 30 Level 2 and 57 Level 3 clusters with ARI >= 0.80 vs the published labels. Reproduce the panel minimization analysis (Fig 4C): single ADT marker enriches 45 of 57 clusters at 10-fold; 3-marker panel enriches 55 of 57.

For the CD8 memory T-cell heterogeneity finding, identify the bimodal CD49a / CD103 populations and confirm CD8 CD103+ cells express high surface integrin beta-7 (ITGB7) while CD8 CD49a+ do not.

For the TCR repertoire arm (5' ECCITE-seq), assemble TCR alpha/beta chains and identify expanded clones (cells sharing exact CDR3-alpha + CDR3-beta). Reproduce 16,060 distinct clones overall and 31 clones with >= 10 cells after excluding MAIT and invariant NKT cells.

As a cross-modality demo, also run the WNN code on the published 11,351-cell PBMC Multiome ATAC+RNA dataset and reproduce: RNA-only 984 incorrect CD8/CD4 edges, ATAC-only 373, WNN 322.

Reference is GRCh38 with the cellranger pre-built reference. No mechanistic claims -- descriptive multimodal cell-state atlas only.

