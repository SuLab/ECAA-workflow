# Workflow Context

**Modality:** proteomics
**Domain:** computational biology
**Description:** Data-dependent acquisition (DDA) LC-MS/MS proteomics — TMT, SILAC, or
label-free MaxLFQ. Standard pipeline: acquire raw spectra, search
peptides (MaxQuant / FragPipe), quantify proteins, test for differential
abundance, then enrichment. Mirrors the DDA branch of today's
`config/modalities/proteomics.yaml` + `config/archetypes/`.

**EDAM topic:** topic:0121
**EDAM operation:** operation:3767
**Confidence:** high (100%)

## Organisms
- Homo sapiens (taxon:9606)

## Methods mentioned in SME prose

_Keyword-scraped from intake text; see SME discovery decisions above for authoritative values._

- protein_identification: MaxQuant

## Data sources
- PXD001656 (ProteomeXchange)

## SME intake text

We want to recreate the canonical MaxQuant tutorial proteome analysis from Tyanova et al. 2016 Nature Protocols (DOI 10.1038/nprot.2016.136), 'The MaxQuant computational platform for mass spectrometry-based shotgun proteomics.' The tutorial dataset is a label-free quantification (LFQ) DDA shotgun proteomics comparison across four human cell lines (HeLa, K562, A549, HEK293), each with three technical replicates, on a Q Exactive HF orbitrap with a 2 h gradient. The raw data is available on the MaxQB / ProteomeXchange repository (PXD001656; the Tyanova 2016 tutorial walks through this exact dataset for the MaxQuant + Perseus workflow). The Tyanova tutorial pairs with MaxQB-deposited identifications as the published ground-truth identification list.

Process the .raw files through MaxQuant 2.x with default DDA parameters: trypsin specificity, 2 missed cleavages, fixed Carbamidomethyl (C), variable Oxidation (M) + Acetyl (Protein N-term), 1% FDR at PSM and protein level using reverse-decoy, 20 ppm first-search MS1 tolerance + 4.5 ppm main-search, match-between-runs (MBR) enabled within the same cell-line group, LFQ minimum ratio count 2. Search against the canonical UP000005640 human UniProt reference proteome with the MaxQuant contaminant FASTA included.

Post-MaxQuant: load the proteinGroups.txt into Perseus (or replicate via Python / R), filter out reverse hits, contaminants, and 'only identified by site' entries, require valid LFQ value in at least 2 of 3 replicates per cell line, impute missing values from a downshifted normal distribution (Perseus defaults: width 0.3, downshift 1.8 SD), log2-transform LFQ intensities.

For differential analysis: pairwise two-sample t-tests (HeLa vs K562, HeLa vs A549, HeLa vs HEK293, etc.) with permutation-based FDR control at 5%, plus an ANOVA across all four cell lines. Volcano plots per pairwise contrast (x = log2 FC, y = -log10 q). PCA on log2-LFQ intensities expected to separate cell lines on PC1 + PC2 (R^2 > 0.6 explained by cell-line factor). Hierarchical clustering of significant proteins (Pearson correlation distance, average linkage).

Final outputs: MaxQuant proteinGroups.txt + summary; filtered + imputed log2-LFQ matrix; per-cell-line valid-value count; PCA scatter colored by cell line; pairwise volcano plots; ANOVA-significant protein heatmap; functional annotation enrichment (Reactome / GO BP) for HeLa-specific vs K562-specific proteins.

Acceptance: >= 4,500 quantified protein groups across the 12 LFQ samples (Tyanova 2016 reports ~5,000-7,000 quantified protein groups for similar HeLa-class samples); each cell line should have >= 80% of protein groups with valid LFQ values in >= 2 of 3 replicates; PCA separates the four cell lines on PC1+PC2; pairwise t-test recovers >= 1,000 significant proteins at permutation FDR 5% for HeLa vs HEK293 (the most divergent pair); the canonical cell-line marker proteins are recovered (HBA1/HBA2/HBB enriched in K562, KRT8/KRT18 enriched in HEK293, surfactant / type-II-pneumocyte markers enriched in A549).

