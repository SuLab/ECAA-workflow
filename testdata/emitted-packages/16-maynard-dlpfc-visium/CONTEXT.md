# Workflow Context

**Modality:** spatial_transcriptomics
**Domain:** computational biology
**Description:** Spatially-resolved transcriptomics — Visium, Slide-seq, MERFISH, Xenium,
Stereo-seq. Standard pipeline: acquire platform-tagged data, preprocess,
cell-level QC, normalise, dimensionality-reduce, segment the section into
spatial domains (graph-aware clustering — BANKSY / BayesSpace / GraphST),
discover spatially variable genes per domain (Moran's I / SpatialDE),
annotate domain cell-type composition, then test for per-domain
differential expression. Cell-type deconvolution is bundled into the
`cell_type_annotation` atom.

Spatial-domain segmentation and spatially-variable-gene (SVG) discovery
are first-class atoms (`spatial_domain_segmentation`,
`spatially_variable_genes`); the spatial-clustering method is chosen at
runtime via the auto-synthesized `discover_spatial_clustering_method`
companion.

**EDAM topic:** topic:3308
**EDAM operation:** operation:3432
**Confidence:** high (100%)

## Organisms
- Homo sapiens (taxon:9606)

## SME intake text

We want to recreate the spatialLIBD dorsolateral prefrontal cortex (DLPFC) spatial transcriptomics atlas from Maynard et al. 2021 Nature Neuroscience, available via the spatialLIBD Bioconductor package and Bioconductor ExperimentHub (DOI 10.18129/B9.bioc.spatialLIBD). The dataset is 12 human postmortem DLPFC tissue sections from 3 neurotypical adult donors (4 sections per donor) profiled using the 10x Genomics Visium spatial gene expression platform. The key scientific goal is automated annotation of the six cortical layers (L1-L6) plus white matter (WM) using spatial transcriptomics gene expression, validated against manual histological annotations by a neuropathologist.

Process Visium FASTQ data using Space Ranger v1.3+ with the GRCh38 reference (10x Genomics 2020-A annotation bundle). Space Ranger outputs: filtered_feature_bc_matrix/, spatial/tissue_positions.csv, and the raw H&E image. Require at least 2,000 median UMIs per spot and 1,000 median genes per spot for QC; spots with fewer than 200 genes are excluded.

For layer deconvolution, implement the spatialLIBD annotation workflow in R using the spatialLIBD Bioconductor package. Build a pseudobulk reference using layer-annotated snRNA-seq data from the same DLPFC region (Allen Brain Cell Atlas). Apply nnSVG to identify spatially variable genes (top 2,000 SVGs). Use layer marker gene sets from the spatialLIBD paper (Suppl Table 2) to compute per-spot layer scores. Run BayesSpace (v1.x) for spatially-aware clustering with k=7 (6 layers + WM).

Layer annotation accuracy: evaluate automated annotation against the manual ground-truth labels using Cohen's kappa. Target kappa >= 0.7 for the automated layer calls vs expert labels. Reproduce the spatial correlation of established layer markers: RELN (L1), CUX2 (L2/L3), RORB (L4), PCP4 (L5), SNAP25/KRT17 (L6), MBP (WM) must each show significant spatial autocorrelation (Moran's I > 0.2, p < 0.01).

Run spatial differential expression between adjacent layers using SPARK-X (spatially-aware DE) and identify at least 50 DEGs per layer boundary. Run cell-type deconvolution using Tangram or cell2location with snRNA-seq reference to estimate cell-type fractions per Visium spot.

Acceptance: Cohen's kappa >= 0.70 for 10/12 tissue sections; all 7 canonical layer markers spatially significant; at least 50 DEGs per layer boundary in at least 10/12 sections.

