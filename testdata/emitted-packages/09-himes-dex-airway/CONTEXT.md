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

## Data sources
- GSE52778 (NCBI GEO Series)

## SME intake text

We have a public bulk RNA-seq dataset on primary human airway smooth muscle cells from Himes et al. 2014 in PLOS ONE, GEO accession GSE52778. The dataset has sixteen SRA runs total (untreated, dex, albuterol, and dex+albuterol arms across four donors), but the PLOS ONE analysis we want to recreate is the dex-vs-untreated contrast: eight paired-end RNA-seq samples -- four anonymous Caucasian male donors, each profiled in two conditions (18-hour 1 uM dexamethasone treatment versus vehicle control). The objective is a paired-donor differential-expression analysis to identify the glucocorticoid-responsive transcriptome of airway smooth muscle.

The design is paired by donor -- donor must be a blocking factor in the differential model, and pooling across donors before fitting is incorrect. Standard QC: FastQC plus the ERCC spike-in dose response as a calibration on the technical noise floor. Reads are 75 bp paired-end Illumina.

The published headline result is 316 differentially expressed genes at Benjamini-Hochberg FDR < 0.05, including well-known glucocorticoid-response genes (DUSP1, KLF15, PER1, FKBP5, TSC22D3) plus the novel candidate the paper highlights, CRISPLD2. We want our DEG count within +/-5% of 316 and the canonical seven upregulated genes present in our top hits with the published direction of effect.

Reference genome is hg19 with the RefSeq annotation. Gene-set enrichment on the GO Biological Process universe should recover the functional categories from the paper (extracellular matrix, vasculature, circulatory system, response to hormone stimulus). No claims beyond descriptive differential expression and pathway enrichment -- this is a clean baseline-fit smoke test.

Compute is single-workstation. No cluster needed.

