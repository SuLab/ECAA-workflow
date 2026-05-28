# CPTAC Prospective Breast Cancer Proteogenomic Reanalysis

## Project goal
Re-derive the prospective CPTAC breast cancer (Krug 2020 *Cell*) proteomic and phosphoproteomic landscape from open-access Proteomic Data Commons (PDC) spectra, harmonize it with matched CPTAC-3 WES/WGS/RNA-seq from the Genomic Data Commons (GDC), and re-identify PAM50-subtype-discriminating protein and phosphosite features together with their genomic drivers.

## Strategy
Use the Krug 2020 prospective CPTAC-BRCA confirmatory cohort as the primary dataset: 122 treatment-naïve primary tumors profiled by TMT-11 mass spectrometry for the global proteome (PDC000120) and phosphoproteome (PDC000121). Re-process the RAW files with FragPipe (Philosopher / MSFragger) or MaxQuant at pinned versions, apply MSstats for statistical contrast analysis, and cross-reference phosphosite abundances against the per-tumor somatic mutation calls, copy-number alteration profiles, and RNA-seq expression from the GDC CPTAC-3 release. Validate subtype-discriminating proteomic features against the retrospective TCGA-based Mertins 2016 *Nature* cohort as an independent comparison.

## Key challenges
- **Reprocessing from RAW vs processed:** PDC provides both RAW and pre-processed PSM-level identifications. Re-processing pins the search engine version explicitly, but costs thousands of search-engine CPU-hours. The package must expose this as a control decision.
- **TMT batch effects:** 122 tumors do not fit in a single TMT-11 plex, so the cohort is split across ~11 plexes. Plex-median normalization and IRS (internal reference scaling) are mandatory before any cross-plex comparison.
- **Phosphosite localization:** ambiguous phosphosite localization (e.g. peptide with 2 STY residues) must be resolved with a localization-confidence filter such as `ptmProphet` or `MaxQuant Localisation Prob ≥ 0.75`.
- **Protein-RNA discordance:** protein and mRNA abundance are only moderately correlated, especially for short-lived regulatory proteins; the package must report discordance explicitly rather than silently assuming RNA is a proxy.
- **PAM50 reassignment:** the canonical PAM50 classifier is RNA-based; projecting it onto proteome features requires either a proteomic PAM50 equivalent (published in Mertins 2016 and refined in Krug 2020) or explicit RNA-based labels.

## Work completed so far
Krug 2020 reported five proteomic-driven subtypes (LumA-I, LumA-II, LumB, Basal, HER2) and linked phosphosite signaling modules to actionable targeted-therapy hypotheses. Downstream public tools (LinkedOmics, CPTAC portal, cProSite) expose subsets of the data interactively. A rerunnable Rust-or-Nextflow package that ingests RAW MS data, pins the search-engine version, normalizes across TMT plexes, and produces a subtype-discriminating phosphosite + mutation-coupled module table with explicit fail-closed gates is still absent from most internal pipelines.

## Core methodological question
Whether TMT-11 quantification re-processed with FragPipe (MSFragger) under 2024+ defaults recapitulates the Krug 2020 published subtype assignments at ≥ 0.9 Rand index, and whether the top-ranked phosphosite-level features for each subtype overlap the published list at Jaccard ≥ 0.5. If either criterion fails, the package must stop and flag either tool-version drift or a normalization mismatch.

## Potential analysis prompts
1. Download CPTAC BRCA prospective proteome (PDC000120) and phosphoproteome (PDC000121) RAW files or processed tables from PDC.
2. Re-search RAW with FragPipe (MSFragger + Philosopher + IonQuant) at a pinned version; or use Krug-2020-era MaxQuant.
3. Normalize TMT channels per plex; apply IRS for cross-plex scaling.
4. Phosphosite localization filtering (≥ 0.75 localization probability).
5. Differential abundance between PAM50 subtypes with MSstats.
6. Cross-reference somatic mutations and CNA from GDC CPTAC-3 WES/WGS.
7. Kinase-substrate enrichment analysis (KSEA) on phosphosite abundance.
8. Benchmark subtype assignments and signature features against Krug 2020 published tables.

## Conservative claim boundaries
Descriptive cross-subtype proteomic and phosphoproteomic differences and hypothesis-generating kinase-substrate associations only. No clinical-utility, therapeutic-target actionability, or diagnostic claims. Any targeted-therapy implication is hypothesis-generating.

## References
- Krug K, Jaehnig EJ, Satpathy S, et al. 2020. Proteogenomic landscape of breast cancer tumorigenesis and targeted therapy. *Cell* 183:1436–1456.e31. DOI 10.1016/j.cell.2020.10.036. PMID 33212010.
- Mertins P, Mani DR, Ruggles KV, et al. 2016. Proteogenomics connects somatic mutations to signalling in breast cancer. *Nature* 534:55–62. DOI 10.1038/nature18003. PMID 27251275.
- Cox J, Mann M. 2008. MaxQuant enables high peptide identification rates, individualized p.p.b.-range mass accuracies and proteome-wide protein quantification. *Nat Biotechnol* 26:1367–1372. DOI 10.1038/nbt.1511.
- Choi M, Chang CY, Clough T, et al. 2014. MSstats: an R package for statistical analysis of quantitative mass spectrometry-based proteomic experiments. *Bioinformatics* 30:2524–2526. DOI 10.1093/bioinformatics/btu305.
- da Veiga Leprevost F, Haynes SE, Avtonomov DM, et al. 2020. Philosopher: a versatile toolkit for shotgun proteomics data analysis. *Nat Methods* 17:869–870. DOI 10.1038/s41592-020-0912-y.
- Demichev V, Messner CB, Vernardis SI, Lilley KS, Ralser M. 2020. DIA-NN: neural networks and interference correction enable deep proteome coverage in high throughput. *Nat Methods* 17:41–44. DOI 10.1038/s41592-019-0638-x.
- Kong AT, Leprevost FV, Avtonomov DM, et al. 2017. MSFragger: ultrafast and comprehensive peptide identification in mass spectrometry-based proteomics. *Nat Methods* 14:513–520.
