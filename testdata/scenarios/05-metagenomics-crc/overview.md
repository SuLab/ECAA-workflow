# Cross-Cohort Shotgun Metagenomics Meta-Analysis of Colorectal Cancer Stool Microbiome

## Project goal
Re-derive a generalizable, cross-cohort microbial signature of colorectal cancer (CRC) from publicly deposited fecal shotgun metagenomics datasets, and benchmark it against the five-cohort meta-analysis of Wirbel et al. 2019 *Nature Medicine*. The deliverable is a held-out-cohort AUROC table together with a consolidated ranked list of CRC-associated species and functional modules.

## Strategy
Ingest the five primary cohorts from Wirbel 2019 — France (Zeller 2014), Austria (Feng 2015), China (Yu 2017), United States (Vogtmann 2016), and Germany (Wirbel 2019 new cohort) — as raw paired-end Illumina shotgun fastq from ENA, or as pre-profiled relative abundance tables via the `curatedMetagenomicData` Bioconductor package. Profile all samples uniformly with MetaPhlAn4 (species-level relative abundance) and HUMAnN3 (KEGG ortholog and pathway abundance). Fit a leave-one-cohort-out (LOCO) random-effects meta-analysis of log-ratio-transformed abundances and train a LASSO logistic classifier under SIAMCAT defaults for LOCO AUROC, held out on each cohort in turn.

## Key challenges
- **Raw-data recomputation vs `curatedMetagenomicData`:** raw-fastq re-profiling pins MetaPhlAn and HUMAnN versions explicitly but costs hundreds of CPU-hours; using `curatedMetagenomicData` is faster but freezes the profiling tool version (MetaPhlAn3/MetaPhlAn4 as of the package release). The package must expose this as a control decision.
- **Compositional data:** relative abundances are compositional and cannot be analyzed directly with parametric tests — use CLR transformation (Aitchison geometry) as implemented in SIAMCAT.
- **Cohort heterogeneity:** diet, geography, antibiotic exposure, ethnicity, and stool-collection protocol vary across cohorts and can masquerade as disease signal. The LOCO evaluation is essential to guard against overfitting to cohort-specific confounders.
- **Cancer stage / adenoma drift:** some cohorts include adenomas, others only invasive CRC, others mix early and late stage. The package must let the analyst decide whether to include adenomas in the primary contrast or restrict to CRC vs healthy.
- **Contamination and batch:** low-biomass stool samples are more sensitive to sequencing-batch contamination than tissue or blood; decontamination against reagent-control profiles should be applied where controls exist.

## Work completed so far
Wirbel 2019 *Nat Med* reported 29 CRC-enriched species at FDR < 1e-5 under their original five-cohort LOCO meta, and confirmed the findings across two additional validation cohorts (Italian IT1/IT2 and Japanese JP). The SIAMCAT package has since become the canonical machine-learning-on-metagenomics toolbox and has its own dedicated paper (Wirbel 2021 *Genome Biology*). Multiple follow-up meta-analyses have since expanded the species list and added functional-module signatures; however, a rerunnable end-to-end package that pins profiling tool versions and exposes fail-closed gates on cohort-specific confounding does not exist as a shared artifact.

## Core methodological question
Whether a LASSO classifier trained on MetaPhlAn4 species-level relative abundances generalizes to held-out cohorts (LOCO AUROC ≥ 0.75 as a suggestive floor, ≥ 0.80 as strong), and whether the cross-cohort stable feature set substantially overlaps the 29 Wirbel-2019 species (Jaccard ≥ 0.5 as a sanity-check floor). If either criterion fails, the package must stop and refuse to emit a consolidated signature.

## Potential analysis prompts
1. Download raw fastq for the five primary cohorts from ENA.
2. Profile with MetaPhlAn4 and HUMAnN3 at pinned versions; CLR-transform abundances.
3. Per-cohort differential abundance (Wilcoxon; SIAMCAT `check.associations`).
4. LOCO random-effects meta-analysis of log-fold changes.
5. LASSO logistic regression per held-out cohort; report LOCO AUROC.
6. Functional module enrichment with HUMAnN3 KO / MetaCyc pathway output.
7. Cross-cohort stability analysis of selected features.
8. Comparison against `curatedMetagenomicData` profiled tables as a cross-validation.
9. Benchmark consolidated feature set against Wirbel 2019's 29-species list.

## Conservative claim boundaries
Descriptive cross-cohort microbial association and classifier discrimination only. No clinical-utility, screening-deployment, causal, or mechanistic claims. Abundance shifts are correlative, not causal.

## References
- Wirbel J, Pyl PT, Kartal E, et al. 2019. Meta-analysis of fecal metagenomes reveals global microbial signatures that are specific for colorectal cancer. *Nat Med* 25:679–689. DOI 10.1038/s41591-019-0406-6. PMID 30936547.
- Zeller G, Tap J, Voigt AY, et al. 2014. Potential of fecal microbiota for early-stage detection of colorectal cancer. *Mol Syst Biol* 10:766.
- Feng Q, Liang S, Jia H, et al. 2015. Gut microbiome development along the colorectal adenoma-carcinoma sequence. *Nat Commun* 6:6528.
- Yu J, Feng Q, Wong SH, et al. 2017. Metagenomic analysis of faecal microbiome as a tool towards targeted non-invasive biomarkers for colorectal cancer. *Gut* 66:70–78.
- Vogtmann E, Hua X, Zeller G, et al. 2016. Colorectal cancer and the human gut microbiome. *PLoS ONE* 11:e0155362.
- Pasolli E, Schiffer L, Manghi P, et al. 2017. Accessible, curated metagenomic data through ExperimentHub. *Nat Methods* 14:1023–1024. DOI 10.1038/nmeth.4468.
- Blanco-Míguez A, Beghini F, Cumbo F, et al. 2023. Extending and improving metagenomic taxonomic profiling with uncharacterized species using MetaPhlAn 4. *Nat Biotechnol* 41:1633–1644. DOI 10.1038/s41587-023-01688-w.
- Beghini F, McIver LJ, Blanco-Míguez A, et al. 2021. Integrating taxonomic, functional, and strain-level profiling of diverse microbial communities with bioBakery 3. *eLife* 10:e65088. DOI 10.7554/eLife.65088.
- Wirbel J, Zych K, Essex M, et al. 2021. Microbiome meta-analysis and cross-disease comparison enabled by the SIAMCAT machine learning toolbox. *Genome Biol* 22:93. DOI 10.1186/s13059-021-02306-1.
