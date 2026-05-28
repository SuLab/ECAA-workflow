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

## Methods mentioned in SME prose

_Keyword-scraped from intake text; see SME discovery decisions above for authoritative values._

- clustering: Seurat
- clustering: scanpy

## SME intake text

We want to recreate the 10x Genomics PBMC10k_v3 reference dataset analysis (10x Genomics public dataset 'PBMC 10k from a Healthy Donor, v3 chemistry'), with the analysis pattern grounded in Zheng et al. 2017 Nature Communications (DOI 10.1038/ncomms14049) and the 10x Cell Ranger 3.0 reference workflow. The input is 10,194 single-cell 3' RNA-seq libraries from peripheral blood mononuclear cells of a single healthy donor, generated with 10x 3' v3 chemistry on Illumina NovaSeq.

Process the raw FASTQ libraries with Cell Ranger 7.x (or STARsolo against GENCODE v44 / 10x GRCh38-2020-A reference) to produce a filtered_feature_bc_matrix. Drop empty droplets with emptyDrops (DropletUtils, FDR < 0.001) or rely on the Cell Ranger filtered call. Apply Scanpy / Seurat cell QC: keep cells with 200 <= n_genes_per_cell <= 6,000, percent_mito < 15%, percent_hemoglobin < 5%. Expected post-QC yield: ~10,000 +/- 5% cells.

Normalize with sctransform v2 (or LogNormalize at scale.factor 10000), select 2,000 highly variable genes, run PCA on the scaled HVG matrix (50 PCs), build a kNN graph (k=20) on the top 30 PCs, embed in UMAP (min_dist=0.3, spread=1.0), and cluster with Leiden at resolution 0.5 (or Louvain). Annotate cell types using SingleR with the HumanPrimaryCellAtlasData reference and a marker-gene scoring pass against a curated PBMC panel: CD3D / CD3E (T cells), CD4 (CD4 T), CD8A / CD8B (CD8 T), GNLY / NKG7 (NK), MS4A1 / CD79A (B), CD14 / LYZ (CD14 mono), FCGR3A / MS4A7 (CD16 mono), FCER1A / CST3 (dendritic), PPBP (platelet).

Final outputs: UMAP plot colored by Leiden cluster and by predicted cell-type label; HVG-based feature plots for the marker panel above; cluster-level marker-gene table (Wilcoxon test, top 10 per cluster); cell-type proportion bar plot; QC summary (n_cells_post_QC, median n_genes, median UMIs, percent_mito distribution); cluster-vs-published-label ARI / NMI.

Acceptance: cluster ARI >= 0.90 versus the 10x-published PBMC10k_v3 cluster labels; at least 7 of the 8 major PBMC populations recovered as distinct clusters (CD4 T, CD8 T, NK, B, CD14 mono, CD16 mono, dendritic, platelet); post-QC cell count 10,000 +/- 5%; marker panel scoring shows each annotated cell-type cluster expressing its canonical marker at log-fold-change > 1.5 versus other clusters.

