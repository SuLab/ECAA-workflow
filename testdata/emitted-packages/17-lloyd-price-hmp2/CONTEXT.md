# Workflow Context

**Modality:** metagenomics
**Domain:** computational biology
**Description:** Shotgun metagenomics or 16S amplicon taxonomic profiling. Standard
pipeline: raw QC, trim, classify reads against a reference database
(Kraken2 / MetaPhlAn / QIIME2-DADA2), then alpha + beta diversity
with group-comparison statistics. Mirrors today's
`config/modalities/metagenomics.yaml` + `config/archetypes/`.

**EDAM topic:** topic:3174
**EDAM operation:** operation:3460
**Confidence:** high (100%)

## Organisms
- Homo sapiens (taxon:9606)

## Methods mentioned in SME prose

_Keyword-scraped from intake text; see SME discovery decisions above for authoritative values._

- taxonomic_profiling: MetaPhlAn

## Data sources
- PRJNA398089 (NCBI BioProject)

## SME intake text

We want to recreate the multi-omic inflammatory bowel disease (IBD) gut microbiome analysis from Lloyd-Price et al. 2019 Nature (the IBDMDB / HMP2 study), SRA BioProject PRJNA398089. The study longitudinally profiled the gut microbiome and host transcriptome in 132 subjects (CD, UC, and healthy controls) over ~1 year, with stool samples collected every two weeks and colon biopsies at colonoscopy. We focus on the shotgun metagenomics + metatranscriptomics arms.

The metagenomics pipeline follows the HMP2 protocol. Process raw paired-end Illumina reads with KneadData v0.7+ for host (hg38) read removal and quality trimming. Run MetaPhlAn4 for taxonomic profiling (species-level relative abundances). Run HUMAnN3 for functional profiling (UniRef90 gene family abundances and MetaCyc pathway coverage). Normalize functional profiles by total sum scaling (TSS) for relative abundance and by reads-per-kilobase (RPK) for gene family counts.

For taxonomic analysis, reproduce the dysbiosis index: compute per-sample Bray-Curtis dissimilarity from MetaPhlAn species profiles. Active IBD flare samples (fecal calprotectin > 200 ug/g) should show higher Bray-Curtis distance from the healthy-cohort centroid than remission samples (p < 0.01 by Wilcoxon). Key dysbiosis species: Bacteroides fragilis, Ruminococcus gnavus enriched in IBD vs healthy; Roseburia intestinalis, Akkermansia muciniphila depleted. At least 4 of these 4 expected species must show the correct direction of effect at FDR < 0.1.

For longitudinal stability analysis, compute intra-individual vs inter-individual Bray-Curtis distances. Intra-individual should be lower than inter-individual for healthy subjects (p < 0.001). Model dysbiosis episodes using a hidden Markov model with 2 states (stable/dysbiotic) per subject time series; CD and UC subjects should show more state transitions per year than healthy (p < 0.05).

Run ANCOM-BC2 for differential abundance testing between disease groups at the species level. Run Maaslin2 for multivariable association of species abundance with antibiotic exposure, host genetics (IBD risk alleles from the metadata), and inflammation markers.

Acceptance: MetaPhlAn4 profiling of at least 800/1,595 stool samples; dysbiosis species directional effects reproduced for 4/4 listed species at FDR < 0.1; intra-individual stability lower than inter-individual in healthy cohort (p < 0.001).

