# Workflow Context

**Modality:** methylation
**Domain:** computational biology
**Description:** DNA methylation differential analysis covering WGBS, RRBS, EM-seq, and
methylation arrays (EPIC / 450K). Standard pipeline: raw QC, bisulfite
/ enzymatic-converted-read alignment, per-CpG methylation extraction,
DMR (differentially methylated region) calling, annotation.

The methylation modality is keyword-routed (config/modality-keywords.yaml
entry id=methylation). Goal triple is data:0951 (statistical estimate
score, since DMRs are reported with effect-size + adjusted p-value) /
format:3475 (tabular). The same atom inventory (data_acquisition →
raw_qc → ... → differential_expression → reporting) handles
methylation chemistry because the per-stage atom is a discovery wrapper
in v4 — the agent picks Bismark / bwa-meth / minfi at the alignment
stage and methylKit / dmrseq / minfi at the DE stage at runtime.

**EDAM topic:** topic:3940
**EDAM operation:** operation:3204
**Confidence:** high (100%)

## Data sources
- GSE68379 (NCBI GEO Series)
- E-MTAB-3610 (ArrayExpress)

## SME intake text

We want to recreate the Iorio et al. 2016 Cell pharmacogenomic landscape paper end-to-end. The cell-line panel is 1,001 lines molecularly characterized (whole-exome sequencing, copy-number profiling, DNA methylation arrays, transcriptional microarray), with the drug screen run on 990 of these against 265 anti-cancer compounds for 212,774 dose-response curves total (median 878 lines per drug; range 366-935). Accessions: ArrayExpress E-MTAB-3610 for the transcriptional microarray (Affymetrix HG-U219), GEO GSE68379 for the RNA-seq complement, EGA EGAS00001000978 for cell-line WES + CNV. Drug response data lives on the GDSC1000 portal (cancerrxgene.org), not on ArrayExpress.

The patient-tumor side of the analysis is 11,289 tumors from 29 cancer types, drawn from TCGA, ICGC, and 48 contributing sequencing studies. From the patient-tumor data we define Cancer Functional Events (CFEs) in three classes: cancer genes (selection against MutSigCV / OncodriveFM / OncodriveCLUST consensus, expected 470 CGs), recurrent amplified / deleted chromosome segments (ADMIRE, expected ~425 RACSs pan-cancer), and informative methylated CpG-island promoters (multimodal distribution test on 450K arrays, expected 378 iCpGs). The pan-cancer union is ~1,273 CFEs.

Each cell line is then assigned a CFE status vector -- mutated, amplified, deleted, or hypermethylated -- for downstream analyses: (i) cell-line <-> tumor concordance via nearest-neighbor classifier on CFE profiles, expected 71% 1-NN match to the right cancer type; (ii) ANOVA testing CFE status against log IC50 across all (cell line x drug) pairs, with FDR < 25% and effect-size filter; (iii) LOBICO logic-combination models capturing AND / OR / NOT combinations of CFEs as predictors of drug response.

Headline acceptance: 1,273 +/- 5% pan-cancer CFEs identified; ~688 statistically significant CFE-drug interactions of which ~262 are large-effect; ~208 of 265 drugs have a predictive LOBICO model; known clinical interactions (EGFR/Gefitinib in LUAD, BRAF/Vemurafenib in SKCM, BCR-ABL/Imatinib in CML) are all recovered as significant.

Compute will be cluster-scale. The most-efficient entry point is the GDSC portal aggregated tables; raw IDAT and raw BAM re-processing from EGAS00001000978 is the exhaustive stress test.

