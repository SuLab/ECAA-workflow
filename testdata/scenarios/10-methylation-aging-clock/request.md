# Public DNA Methylation Epigenetic Aging Clock Package Request

Use the following local files as the source materials for this package request:

- Project overview: testdata/scenarios/10-methylation-aging-clock/overview.md
- Dataset inventory (TSV): testdata/scenarios/10-methylation-aging-clock/studies.tsv

Create a fully autonomous-ready internal research package for a public human DNA methylation epigenetic aging clock reanalysis, re-deriving the Horvath 2013 and Hannum 2013 clocks on Illumina HumanMethylation450 array data and running an age-EWAS on an independent whole-blood calibration cohort.

Primary objective: predict epigenetic age from raw IDAT files of GSE40279 (Hannum 2013 training cohort) and GSE87571 (Johansson whole-blood calibration cohort), compare predicted epigenetic age against reported chronological age with MAE and Pearson r, and run an age-EWAS on GSE87571 with cell-composition adjustment, ranking the top CpGs against the Horvath, Hannum, and Levine PhenoAge clock CpG lists.

Context from the source files:

- GSE40279 is the canonical Hannum-clock training dataset (656 whole-blood adults, ages 19–101).
- GSE87571 is an independent whole-blood 450k cohort (729 adults, ages 14–94) that is widely used as an age-EWAS benchmark.
- Preprocessing pipeline (SeSAMe vs minfi vs ENmix vs ChAMP) materially affects clock MAE by 0.5–2 years; the package MUST pin one primary pipeline and report cross-pipeline MAE as sanity check.
- Cell-composition adjustment (IDOL / EpiDISH) is mandatory for the EWAS.
- `methylKit` is a bisulfite-sequencing package and MUST NOT be used as the primary array analysis tool; use `minfi` / `SeSAMe` / `ENmix` / `ChAMP` instead.
- Horvath clock CpG coverage on EPIC vs 450k differs; missing-CpG imputation must be audited.

Data availability and scope:

- GSE40279, GSE87571, and GSE87648 are open-access on NCBI GEO.
- No controlled-access, PHI, or dbGaP-gated data is used.
- Treat this as a human whole-blood 450k methylation age-clock reanalysis project. Tissue-specific clocks, cord-blood clocks, non-human clocks, and bisulfite-sequencing clocks are out of scope.
- Publication scope: internal only.
- Governance: public released research data only; no PHI, no controlled-access data, no secrets.

Required package behavior:

- The package must be suitable for downstream autonomous execution after generation.
- Explicit control decisions required for: (a) preprocessing pipeline (SeSAMe default, minfi cross-validation), (b) normalization method (funnorm vs noob vs BMIQ), (c) detection-p-value filtering threshold, (d) clock set (Horvath 2013 + Hannum 2013 primary; Levine 2018 PhenoAge optional; Lu 2019 GrimAge optional), (e) cell-composition deconvolution method (IDOL vs EpiDISH), (f) EWAS statistical model (robust linear with cell-composition covariates), (g) DMR tool (DMRcate default, bumphunter sensitivity), and (h) fail-closed stop conditions when predicted-age MAE on the held-out calibration cohort exceeds a prespecified floor (e.g. 4 years for Horvath on GSE87571), or when the Horvath clock CpG coverage is below a prespecified completeness threshold.
- The package MUST report per-sample predicted epigenetic age with the reported chronological age, MAE, and Pearson r, AND it MUST run a cross-pipeline sanity check between SeSAMe and minfi.
- Conservative claim boundaries: descriptive epigenetic age prediction and age-EWAS association only. No claims about biological age, healthspan, mortality risk, lifespan prediction, or intervention efficacy. "Age acceleration" interpretations are correlative, not causal, and are hypothesis-generating.
- If runtime refinement occurs (e.g. excluding failing samples), provenance and ranking logic must be logged explicitly.
- Literature grounding: at minimum Horvath 2013, Hannum 2013, Levine 2018 (PhenoAge), Lu 2019 (GrimAge), Aryee 2014 (minfi), Zhou 2018 (SeSAMe), Xu 2016 (ENmix), Tian 2017 (ChAMP), Peters 2015 (DMRcate), Jaffe 2012 (bumphunter), Teschendorff 2017 (EpiDISH), and Koestler 2016 (IDOL) must appear in the package bibliography.

## Extracted Overview Preview

```
Project goal: re-derive Horvath 2013 + Hannum 2013 epigenetic aging clocks on 450k IDAT data;
run age-EWAS on independent whole-blood cohort.
Training: GSE40279 (Hannum 2013 whole blood, 656 samples, ages 19-101).
Calibration: GSE87571 (Johansson whole blood, 729 samples, ages 14-94).
Preprocessing: SeSAMe primary, minfi cross-validation; pipeline choice materially affects MAE.
Cell composition: IDOL or EpiDISH — mandatory for EWAS.
Methodological question: Horvath MAE on GSE87571 <= 4 years with SeSAMe default preprocessing.
Claim boundaries: descriptive prediction and EWAS association only; no biological-age or mortality claims.
```

## Extracted Dataset Inventory Preview

```
GSE40279 Hannum 2013 Mol Cell whole blood n=656 ages 19-101 — training cohort
GSE87571 Johansson 2013 whole blood n=729 ages 14-94 — calibration cohort
GSE87648 Ventham 2016 Nat Commun IBD whole blood n=381 — disease-context extension (optional)
Horvath 2013 online calculator (dnamage.genetics.ucla.edu) — output cross-check only
TCGA methylation (GDC) — optional disease-context extension; out of scope primary
```
