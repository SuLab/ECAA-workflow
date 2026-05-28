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
- Homo sapiens (taxon:9606)

## Methods mentioned in SME prose

_Keyword-scraped from intake text; see SME discovery decisions above for authoritative values._

- alignment: Bowtie2
- peak_calling: MACS2

## Data sources
- GSE74912 (NCBI GEO Series)
- GSE74310 (NCBI GEO Series)

## SME intake text

We want to recreate the Corces et al. 2016 Nat Genet Fast-ATAC hematopoiesis atlas. The data is on GEO GSE74912 (bulk ATAC-seq + RNA-seq) and GSE74310 (single-cell ATAC subset). The cohort is 9 healthy donors and 12 AML patients across 13 FACS-sorted populations: HSC, MPP, LMPP, CMP, GMP, MEP, CLP (7 stem/progenitor) plus erythroblast, B, CD4 T, CD8 T, NK, monocyte (6 differentiated). 3-4 adult donors per population yield 49 transcriptomes and 77 regulomes; AML samples add ~11 additional ATAC-seq libraries.

Process Fast-ATAC reads with cutadapt for adapter trimming, Bowtie2 v2.x with --very-sensitive -X 2000 against hg19, Picard MarkDuplicates with duplicates removed, MAPQ >= 30, mitochondrial reads removed, ENCODE blacklist subtracted, and Tn5 read shift +4/-5. Call peaks with MACS2 narrow --nomodel --shift -75 --extsize 150 -p 0.01 --call-summits.

Build a master peak union by merging peaks present in >= 2 of 3+ donors per population. The expected union is ~590,650 accessible peaks. Compute per-replicate Pearson R within and across donors; headline numbers to reproduce: technical R = 0.98 within HSCs, biological R = 0.97; mean technical R = 0.94 across all populations.

For enhancer cytometry, use CIBERSORT trained on cell-type-specific regulatory elements; leave-one-out cross-validation should classify at least 11 of 13 populations (HSC vs MPP is the expected difficult pair). Apply on bulk CD34+ HSPC samples and compare to flow cytometry: target R^2 = 0.95.

For TF motif analysis, cluster JASPAR motifs into a non-redundant set of 46 hematopoiesis TF motifs (Fig 4a). Correlate per-cell-type motif accessibility with TF expression from RNA-seq. Reproduce GATA1 <-> GATA motif R = 0.73 at p = 1e-18 and PAX5 <-> PAX motif R = 0.88 at p = 1e-230. Run GATA footprint analysis on MEPs vs CLPs and confirm no GATA footprint in CLPs.

Overlap accessibility with GWAS lead SNPs and recover per-trait cell-type enrichment: mean corpuscular volume -> erythroblast, rheumatoid arthritis -> lymphoid, Alzheimer's -> microglia/monocyte.

For the AML arm, profile pre-leukemic mutation burden in HSC, T, blast cells across 39 AML patients (driver-mutation panel from Suppl Table 5). Project AML cell types onto the first 4 PCs of normal hematopoiesis and reproduce: LSCs near GMP/LMPP; blasts spread along GMP-monocyte axis.

Reference genome is hg19. No mechanistic claims -- descriptive regulome atlas + AML evolution trajectory.

