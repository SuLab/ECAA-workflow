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
- Mus musculus (taxon:10090)

## Methods mentioned in SME prose

_Keyword-scraped from intake text; see SME discovery decisions above for authoritative values._

- alignment: STAR
- differential_expression: DESeq2
- differential_expression: edgeR

## Data sources
- GSE102827 (NCBI GEO Series)

## SME intake text

We want to recreate the Hrvatin et al. 2018 Nat Neurosci visual-cortex stimulus time-course atlas. The data is on GEO under GSE102827: ~47,000 cells from mouse V1 across three timepoints -- 0 hours (dark-housed control), 1 hour, and 4 hours after light exposure following 7 days of dark housing. The protocol is inDrops (Klein 2015 / Zilionis 2017) -- different barcode geometry from 10x and Drop-seq. Reference is mm10.

Process FASTQs with the inDrops pipeline (custom barcode parser -> STAR -> UMI deduplication) or start from the published DGE matrices. Filter cells with < 600 detected genes. Normalize by scaling each cell to 10,000 transcripts and log-transforming. PCA on the top ~2,000 highly-variable genes; Louvain clustering at a resolution that yields 30 cell types.

Annotate using canonical mouse-cortex markers per the Tasic 2016 Nat Neurosci reference. The published 30-cluster partition has 6 excitatory neuron subtypes (Slc17a7+: L2/3, L4, three L5 sub-classes ExcL5_1/_2/_3, and L6 -- L6b cells co-cluster with L6), interneuron subclasses (Pvalb, Sst, Vip, Lamp5/Ndnf), and the non-neuronal compartment (astrocytes, OPCs, oligodendrocytes, microglia, endothelial, pericytes, smooth muscle).

Per-cell-type x timepoint cell counts must be reported. Any cell-type x timepoint with fewer than 50 cells should auto-block DE rather than report unstable estimates.

For stimulus-responsive DE, run Monocle2's per-gene differential test on the single-cell counts (the paper's Methods specifies Monocle2 with a negative-binomial model and animal as a covariate) with animal as a blocking factor and timepoint as the contrast. A pseudobulk edgeR / DESeq2 cross-check is welcome but not required. Take the union across cell types of FDR < 0.05 + >= 2-fold hits and reproduce 611 stimulus-responsive genes.

Cluster these 611 genes by 0h -> 1h -> 4h trajectory shape. Validate that immediate-early genes -- Fos, Junb, Arc, Egr1, Fosb -- fall in the early-response (1h-peak) cluster and that excitatory neurons show larger IEG fold-changes than inhibitory.

Acceptance: 30 cell types recovered with ARI >= 0.80 vs published labels; >= 500 of the 611 stim-responsive genes recovered; IEG-temporal ordering preserved.

No mechanistic claims -- descriptive trajectory clusters only.

