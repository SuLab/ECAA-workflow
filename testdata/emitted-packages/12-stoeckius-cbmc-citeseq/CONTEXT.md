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
- Mus musculus (taxon:10090)

## Data sources
- GSE100866 (NCBI GEO Series)

## SME intake text

We want to recreate the original CITE-seq proof-of-principle from Stoeckius et al. 2017 Nat Methods. The data is on GEO under GSE100866 and includes three experiments: a Drop-seq species-mixing assay (HeLa human + 4T1 mouse), the CBMC profiling experiment (~8,005 cord blood cells with a panel of 13 antibodies and ~4% mouse 3T3 fibroblast spike-in for ADT background calibration, on 10X Single Cell 3' v2), and a CD8a sorted-pool comparison between flow cytometry and CITE-seq.

Process the species-mixing data with Drop-seq tools v1.12 against a hybrid hg19+mm10 reference (load 400 cells/uL) and recreate the 97.2% agreement between transcriptomic and ADT species classification at the single-cell level (Fig 1d).

For the CBMC arm, process with Cell Ranger v1.2 against hg19 (NOT GRCh38) with default parameters. Total droplets = 8,617 (8,005 human + 579 mouse + 33 mixed after species classification). Cluster the cells unsupervised on RNA; the mouse 3T3 cells should appear as a distinct cluster representing ~4% of total cells. Use the 3T3 cluster's per-antibody ADT counts to set the signal-vs-background threshold per antibody. Three antibody-oligo conjugates are expected to fall below background and should be excluded -- report which three.

Subtract per-antibody background, then CLR-normalize ADT across cells. Recreate Fig 3a (cluster tSNE), Fig 3b (per-cluster ADT enrichment matrix recovering CD3e in T, CD4/CD8a in T subsets, CD19 in B, CD56/CD16 in NK, CD11c/CD14 in monocytes/DC, CD34 in the rare ~2% precursor cluster), and Fig 3c (CD4 vs CD8a, CD16 vs CD56, CD14 vs CD4 bi-axial gating plots).

Subdivide the NK cluster on CD56 ADT level into CD56-bright vs CD56-dim; verify CD16 ADT is higher in CD56-dim; compare 11 prior NK-subset DE genes (including GZMB, GZMK, PRF1) and reproduce the published direction in 10 of 11.

For the CD8a sorted-pool experiment, sort CBMCs on a Sony SH800 cell sorter into 4 pools by CD8a fluorescence (+++, ++, +, +/-), load each pool at 150 cells/uL on Drop-seq, then reanalyze each by both methods; recreate the linear quantitative agreement (Fig 2c-f).

No mechanistic claims -- descriptive multimodal QC only.

