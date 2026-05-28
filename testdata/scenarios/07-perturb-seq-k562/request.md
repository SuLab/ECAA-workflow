# Public Genome-Scale Perturb-seq Reanalysis Package Request

Use the following local files as the source materials for this package request:

- Project overview: testdata/scenarios/07-perturb-seq-k562/overview.md
- Sub-experiment inventory (TSV): testdata/scenarios/07-perturb-seq-k562/studies.tsv

Create a fully autonomous-ready internal research package for a public human genome-scale Perturb-seq reanalysis using the Replogle et al. 2022 *Cell* K562 and RPE1 CRISPRi datasets.

Primary objective: re-derive target-level transcriptomic-response modules from the Replogle 2022 genome-wide K562 and essentialome RPE1 Perturb-seq experiments; recover CORUM / BioGRID complex-aware modules; and produce a ranked table of CRISPRi targets grouped into transcriptional-response modules with sensitivity to guide-level resampling.

Context from the source files:

- The dataset has **no single GEO accession**. Raw data is on SRA BioProject PRJNA831566; processed h5ad data is on the figshare article 20029387 and via the `gwps.wi.mit.edu` portal. The package must handle both access paths.
- The genome-wide K562 experiment contains >1.9 M single cells; out-of-core / backed AnnData handling is mandatory.
- Median per-guide knockdown is reported at 86% (K562) and 92% (RPE1). Per-target knockdown varies and the package MUST enforce a prespecified knockdown floor (e.g. 50% relative to NTC) before downstream analysis.
- Essential gene knockdowns cause cell dropout, shortening the observation window and biasing the pseudo-bulk. These must be flagged explicitly.
- The preferred structured access path for downstream reanalysis is the `scPerturb` harmonized release (Peidli 2024 *Nat Methods*).

Data availability and scope:

- All data are open-access research data (SRA + figshare + `gwps.wi.mit.edu` + scPerturb Zenodo).
- No controlled-access, PHI, or identifiable genotype data is used.
- Treat this as a human K562 and RPE1 CRISPRi Perturb-seq project. Other perturbation modalities (CRISPR-KO, chemical, TF overexpression) are out of scope for the primary analysis.
- Publication scope: internal only.
- Governance: public released research data only; no PHI, no controlled-access data, no secrets.

Required package behavior:

- The package must be suitable for downstream autonomous execution after generation.
- Explicit control decisions required for: (a) data access path (raw fastq re-alignment via CellRanger vs figshare processed h5ad vs scPerturb harmonized release), (b) per-cell QC thresholds, (c) knockdown-efficiency floor per guide, (d) guide- vs target-level aggregation, (e) pseudo-bulk contrast metric (log-fold change vs energy distance), (f) batch-correction strategy (scVI, Harmony, or per-lane regression-out), (g) Mixscape classification of escaping cells, (h) clustering resolution for module recovery, and (i) fail-closed stop conditions when module recovery falls below a prespecified Adjusted Rand Index floor against Replogle 2022 published modules.
- Essential gene targets MUST be flagged and handled separately from non-essential targets.
- Conservative claim boundaries: descriptive transcriptomic-response modules and GRN hypothesis generation only. No claims about therapeutic target relevance, essentiality generalization, or in vivo phenotype.
- If runtime refinement occurs (e.g. excluding low-quality guides), provenance and ranking logic must be logged explicitly.
- Literature grounding: at minimum Replogle 2022, Papalexi 2021 (Mixscape), Peidli 2024 (scPerturb), Kamimoto 2023 (CellOracle), and Zheng 2017 (CellRanger/Chromium) must appear in the package bibliography.

## Extracted Overview Preview

```
Project goal: target-level transcriptomic-response modules from Replogle 2022 genome-scale Perturb-seq.
Sub-experiments: K562_gwps (9866 genes, day 8, ~1.97M cells),
K562_essential (2057 essentials, day 6, ~0.6M cells),
RPE1_essential (~2000 essentials, day 7, ~0.3M cells).
Access: SRA PRJNA831566 raw; figshare 20029387 processed h5ad; gwps.wi.mit.edu portal; scPerturb Zenodo.
Methodological question: target-level module recovery vs Replogle 2022 CORUM complexes at ARI >= 0.6.
Claim boundaries: descriptive modules + GRN hypotheses only; no therapeutic or essentiality claims.
```

## Extracted Sub-Experiment Inventory Preview

```
K562_gwps K562 CRISPRi 9866 genes day 8 ~1.97M cells PRJNA831566 + figshare 20029387
K562_essential K562 CRISPRi 2057 essentials day 6 ~0.6M cells
RPE1_essential RPE1 CRISPRi ~2000 essentials day 7 ~0.3M cells
scPerturb harmonized release Peidli 2024 Nat Methods preferred structured access path
```
