# DNA Methylation Epigenetic Aging Clock Reconstruction and EWAS

## Project goal
Re-derive the Horvath 2013 and Hannum 2013 epigenetic aging clocks on public Illumina HumanMethylation450 (and EPIC) array data, apply them to held-out cohorts as calibration, and run an epigenome-wide association study (EWAS) of chronological age on an independent whole-blood dataset. The deliverable is a per-sample predicted-age-vs-reported-age scatter with Pearson r, median absolute error (MAE), and a ranked list of EWAS-significant age-associated CpG sites.

## Strategy
Use `GSE40279` (Hannum 2013, 656 whole-blood adults ages 19–101) as the canonical training cohort, and `GSE87571` (Johansson, 729 whole-blood adults ages 14–94) as the independent calibration cohort. Preprocess raw IDAT files with SeSAMe (Zhou 2018 NAR) as the primary pipeline, and `minfi` (Aryee 2014) as a cross-validation pipeline. Apply Horvath 2013 and Hannum 2013 clock coefficients to the beta-value matrices, compute predicted epigenetic age, and measure MAE against reported chronological age. Run an EWAS on GSE87571 against chronological age using a robust linear model with cell-composition adjustment (`IDOL` or `EpiDISH` reference-based deconvolution) and compare the top hits against the Horvath CpG list.

## Key challenges
- **Preprocessing drift:** the same raw IDATs run through SeSAMe, minfi-default, minfi-Noob, and ENmix produce materially different beta matrices. Clock MAE can shift by 0.5–2 years depending on preprocessing. The package must pin a single primary pipeline and report cross-pipeline MAE as a sanity check.
- **Normalization method:** BMIQ / SWAN / funnorm / noob all address Type I vs Type II probe bias differently; the package must pin one and document the choice.
- **Cell composition confounding:** whole-blood DNA methylation reflects leukocyte subset composition, which itself changes with age. Cell-composition adjustment with IDOL (Koestler 2016) or EpiDISH (Teschendorff 2017) is mandatory in the EWAS.
- **Horvath clock probe coverage:** the 353 Horvath clock CpGs are not all present on every array version (EPIC vs 450k); missing-CpG imputation must be audited.
- **Array technology mix:** 450k and EPIC arrays overlap on ~450k probes but have different backgrounds and Type-I/II mixtures. Pooling arrays requires explicit probe-intersection logic.
- **Overfit vs generalization:** the Horvath clock was trained on 39 tissue types and is supposed to be generalizable; the Hannum clock was trained on whole blood only. Applying Hannum to a non-blood tissue is a known failure mode.
- **methylKit scope note:** `methylKit` is a bisulfite-sequencing package, not a 450k/EPIC array tool. The primary pipeline must rely on `minfi` / `SeSAMe` / `ENmix` / `ChAMP` for array data.

## Work completed so far
The Horvath 2013 and Hannum 2013 clocks are the two most-cited epigenetic age predictors, and the Levine 2018 PhenoAge and Lu 2019 GrimAge clocks are the most-cited second-generation "health-weighted" extensions. The Horvath clock coefficients and the Hannum clock coefficients are published in full in the supplementary tables of each paper; dnamage.genetics.ucla.edu provides an online calculator. A rerunnable, fail-closed package that ingests raw IDAT files, applies both clocks, runs an EWAS on a held-out calibration cohort, and reports predicted-age vs reported-age discrepancy is absent from most internal pipelines.

## Core methodological question
Whether Horvath 2013 clock prediction MAE on the held-out GSE87571 cohort is ≤ 4 years with SeSAMe preprocessing, and whether the top 100 EWAS hits on GSE87571 overlap the Horvath clock CpGs at Jaccard ≥ 0.1 (the Horvath set is a learned sparse basis, so higher overlap is not expected). If MAE is above the floor, the package must stop and flag either preprocessing drift or cohort-of-origin mismatch.

## Potential analysis prompts
1. Download raw IDAT files for GSE40279 and GSE87571 from GEO.
2. Preprocess with SeSAMe (primary) and minfi (cross-validation).
3. Apply Horvath 2013 and Hannum 2013 clock coefficients.
4. Compute MAE and Pearson r between predicted epigenetic age and reported chronological age.
5. Run EWAS on GSE87571 with chronological age as the predictor, with cell-composition adjustment via IDOL / EpiDISH.
6. DMR calling with DMRcate and bumphunter as sensitivity analyses.
7. Compare top EWAS hits against Horvath clock CpGs, Hannum clock CpGs, and PhenoAge (Levine 2018) CpGs.
8. Run the Lu 2019 GrimAge predictor as an optional extension if the PC-based clock coefficients are accessible.

## Conservative claim boundaries
Descriptive epigenetic age prediction and EWAS association only. No claims about biological age, healthspan, mortality, or intervention efficacy. Any interpretation of "age acceleration" is correlative, not causal, and is hypothesis-generating.

## References
- Horvath S. 2013. DNA methylation age of human tissues and cell types. *Genome Biol* 14:R115. DOI 10.1186/gb-2013-14-10-r115. PMID 24138928.
- Hannum G, Guinney J, Zhao L, et al. 2013. Genome-wide methylation profiles reveal quantitative views of human aging rates. *Mol Cell* 49:359–367. DOI 10.1016/j.molcel.2012.10.016. PMID 23177740.
- Levine ME, Lu AT, Quach A, et al. 2018. An epigenetic biomarker of aging for lifespan and healthspan (PhenoAge). *Aging (Albany NY)* 10:573–591. DOI 10.18632/aging.101414.
- Lu AT, Quach A, Wilson JG, et al. 2019. DNA methylation GrimAge strongly predicts lifespan and healthspan. *Aging (Albany NY)* 11:303–327. DOI 10.18632/aging.101684.
- Aryee MJ, Jaffe AE, Corrada-Bravo H, et al. 2014. Minfi: a flexible and comprehensive Bioconductor package for the analysis of Infinium DNA methylation microarrays. *Bioinformatics* 30:1363–1369. DOI 10.1093/bioinformatics/btu049.
- Zhou W, Triche TJ, Laird PW, Shen H. 2018. SeSAMe: reducing artifactual detection of DNA methylation by Infinium BeadChips in genomic deletions. *NAR* 46:e123. DOI 10.1093/nar/gky691.
- Xu Z, Niu L, Li L, Taylor JA. 2016. ENmix: a novel background correction method for Illumina HumanMethylation450 BeadChip. *NAR* 44:e20. DOI 10.1093/nar/gkv907.
- Tian Y, Morris TJ, Webster AP, et al. 2017. ChAMP: updated methylation analysis pipeline for Illumina BeadChips. *Bioinformatics* 33:3982–3984. DOI 10.1093/bioinformatics/btx513.
- Peters TJ, Buckley MJ, Statham AL, et al. 2015. De novo identification of differentially methylated regions in the human genome (DMRcate). *Epigenetics & Chromatin* 8:6. DOI 10.1186/1756-8935-8-6.
- Jaffe AE, Murakami P, Lee H, et al. 2012. Bump hunting to identify differentially methylated regions in epigenetic epidemiology studies (bumphunter). *Int J Epidemiol* 41:200–209. DOI 10.1093/ije/dyr238.
- Teschendorff AE, Breeze CE, Zheng SC, Beck S. 2017. A comparison of reference-based algorithms for correcting cell-type heterogeneity in Epigenome-Wide Association Studies (EpiDISH). *BMC Bioinformatics* 18:105.
- Koestler DC, Jones MJ, Usset J, et al. 2016. Improving cell mixture deconvolution by identifying optimal DNA methylation libraries (IDOL). *BMC Bioinformatics* 17:120.
