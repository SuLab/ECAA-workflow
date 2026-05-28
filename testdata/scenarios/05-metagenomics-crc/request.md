# Public Colorectal Cancer Stool Metagenomics Meta-Analysis Package Request

Use the following local files as the source materials for this package request:

- Project overview: testdata/scenarios/05-metagenomics-crc/overview.md
- Cohort inventory (TSV): testdata/scenarios/05-metagenomics-crc/studies.tsv

Create a fully autonomous-ready internal research package for a public human shotgun metagenomics meta-analysis of colorectal cancer (CRC) stool microbiome, re-deriving and extending the five-cohort LOCO meta-analysis of Wirbel et al. 2019 *Nat Med*.

Primary objective: derive a generalizable cross-cohort species-level and functional-module signature that discriminates CRC from healthy adults, evaluated under strict leave-one-cohort-out (LOCO) cross-validation and benchmarked against the Wirbel-2019 published 29-species signature.

Context from the source files:

- Five primary cohorts are staged with verified ENA accessions: ERP005534 (France, Zeller 2014), ERP008729 (Austria, Feng 2015), PRJEB10878 (China, Yu 2017), PRJEB12449 (USA, Vogtmann 2016), PRJEB27928 (Germany, Wirbel 2019 new cohort). Total 386 CRC + 392 controls.
- Two independent validation cohorts (Italy, Japan) are optional secondary additions.
- The package may consume either raw fastq from ENA (full version-pinned re-profiling) or pre-profiled abundance tables from `curatedMetagenomicData` (faster, but freezes the profiling tool version). This is an explicit control decision.
- LOCO evaluation is mandatory — in-cohort AUROC is not a valid primary endpoint for a generalization claim.
- Compositional data MUST be CLR-transformed (Aitchison geometry) before any parametric test.

Data availability and scope:

- All five primary cohorts and both validation cohorts are open-access on ENA or in `curatedMetagenomicData`.
- No controlled-access, PHI, or identifiable genotype data is used.
- Treat this as a human adult stool shotgun-metagenomics project. Amplicon 16S data is out of scope for the primary analysis but may be used only for external sanity comparison.
- Publication scope: internal only.
- Governance: public released research data only; no PHI, no controlled-access data, no secrets.

Required package behavior:

- The package must be suitable for downstream autonomous execution after generation.
- Explicit control decisions required for: (a) raw-fastq re-profiling vs `curatedMetagenomicData` fast-path, (b) profiling tool version pin (MetaPhlAn4, HUMAnN3), (c) adenoma inclusion policy in the primary contrast, (d) LOCO evaluation strategy, (e) LASSO hyperparameter search / stability selection approach, (f) feature-selection thresholds (CLR abundance and prevalence filters), and (g) fail-closed stop conditions when LOCO AUROC falls below a prespecified floor or Jaccard against Wirbel 2019 29-species list falls below 0.5.
- The package MUST report per-cohort AUROC and per-cohort feature stability, not only an aggregated metric.
- Conservative claim boundaries: descriptive cross-cohort microbial association and classifier discrimination only. No clinical-utility, screening-deployment, causal, or mechanistic claims. Abundance shifts are correlative, not causal.
- If runtime refinement occurs (e.g. dropping cohorts that fail QC), provenance and ranking logic must be logged explicitly.
- Literature grounding: at minimum Wirbel 2019, Zeller 2014, Feng 2015, Yu 2017, Vogtmann 2016, Pasolli 2017 (curatedMetagenomicData), Blanco-Miguez 2023 (MetaPhlAn4), Beghini 2021 (HUMAnN3/bioBakery3), and Wirbel 2021 (SIAMCAT) must appear in the package bibliography.

## Extracted Overview Preview

```
Project goal: cross-cohort CRC fecal microbiome signature with LOCO AUROC evaluation.
Five primary cohorts: FR/AT/CN/US/DE, 386 CRC + 392 controls.
Optional validation cohorts: IT, JP.
Profiling: MetaPhlAn4 species + HUMAnN3 KO/pathway; CLR transformation mandatory.
Modeling: SIAMCAT LASSO logistic with LOCO CV and feature-stability selection.
Methodological question: does LOCO AUROC >= 0.75 and Jaccard >= 0.5 against Wirbel 2019 hold under the pinned pipeline.
Claim boundaries: descriptive association and classifier discrimination only; no clinical or causal claims.
```

## Extracted Cohort Inventory Preview

```
FR Zeller 2014 Mol Syst Biol ERP005534 53 CRC + 61 ctrl
AT Feng 2015 Nat Commun ERP008729 46 CRC + 63 ctrl
CN Yu 2017 Gut PRJEB10878 74 CRC + 54 ctrl
US Vogtmann 2016 PLoS ONE PRJEB12449 52 CRC + 52 ctrl
DE Wirbel 2019 Nat Med PRJEB27928 60 CRC + 60 ctrl (new cohort)
IT Thomas 2019 Nat Med — optional validation
JP Yachida 2019 Nat Med — optional validation
curatedMetagenomicData Bioconductor ExperimentHub — profiled fast-path
```
