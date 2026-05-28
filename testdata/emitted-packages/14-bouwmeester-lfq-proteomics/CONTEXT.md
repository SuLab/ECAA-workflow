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
- Escherichia coli (taxon:562)
- Saccharomyces cerevisiae (taxon:4932)

## Methods mentioned in SME prose

_Keyword-scraped from intake text; see SME discovery decisions above for authoritative values._

- differential_expression: limma-voom
- protein_identification: MaxQuant

## Data sources
- PXD028735 (ProteomeXchange)

## SME intake text

We want to recreate the label-free quantification (LFQ) proteomics benchmark from Bouwmeester et al. 2020 Journal of Proteome Research, ProteomeXchange accession PXD028735. The dataset is a three-species peptide mixture benchmark (human, yeast, E. coli) with 13 different instrument configurations (Orbitrap Fusion, Q Exactive HF-X, timsTOF Pro, and others) and multiple gradient lengths. The benchmark provides a ground-truth proteome-level mixture of known ratios for evaluating LFQ accuracy and reproducibility.

The analysis follows the benchmark design: process RAW files through MaxQuant v2.x for feature detection and protein group assembly (LFQ min ratio count 2, match between runs enabled, 1% FDR at both PSM and protein level using reverse decoy). Use the canonical UP000005640 human, UP000002311 yeast, and UP000000625 E. coli UniProt reference proteomes concatenated as the search database.

For LFQ normalization benchmarking, compare three quantification approaches on the same raw data: MaxQuant LFQ, intensity-based absolute quantification (iBAQ), and MaxLFQ. The primary acceptance criterion is accurate recovery of the known species ratios from the mixture design: human:yeast:E. coli = 65:30:5 by protein mass. Each method should recover human proteins at 60-70%, yeast at 25-35%, and E. coli at 3-8% of total protein mass.

Protein-level quantification quality: coefficient of variation (CV) for technical replicates within each instrument platform should be below 20% for >80% of quantified proteins. Missing value rate should be below 15% per sample. Require a minimum of 1,000 quantified human protein groups, 500 yeast protein groups, and 100 E. coli protein groups across all platforms.

Run PCA on the log2-normalized LFQ intensities across all samples; the first principal component should separate instrument platforms (expected R^2 > 0.4 for platform effect). Run differential abundance analysis between the three species proteome fractions using limma with empirical Bayes moderation; volcano plots should show clean separation at fold change > 2 and adjusted p-value < 0.05.

Acceptance: species mixture ratios within +/-5 percentage points of ground truth for at least 8/13 instrument configurations; CV < 20% for >80% of proteins in technical replicates; at least 1,200 quantified human protein groups total.

