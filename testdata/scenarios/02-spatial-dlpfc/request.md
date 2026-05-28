# Public DLPFC Spatial Transcriptomics Atlas Reconstruction Package Request

Use the following local files as the source materials for this package request:

- Project overview: testdata/scenarios/02-spatial-dlpfc/overview.md
- Study inventory (TSV): testdata/scenarios/02-spatial-dlpfc/studies.tsv

Create a fully autonomous-ready internal research package for a public human spatial transcriptomics reanalysis of the dorsolateral prefrontal cortex (DLPFC) using the datasets summarized in those local files.

Primary objective: reconstruct cortical layer organization (L1–L6, WM) from 10x Visium spatial transcriptomics data at spot-level resolution, and overlay a snRNA-seq-derived cell-type composition via probabilistic deconvolution, benchmarked against manually-annotated layer ground truth.

Context from the source files:

- The Maynard 2021 *Nat Neurosci* DLPFC pilot (12 sections, 3 donors) is the anchor dataset and has manually-annotated layer labels that serve as the gold standard for every published spatial clustering method in the field.
- There is no single GEO accession for the Maynard pilot. The canonical public distribution is through the `spatialLIBD` Bioconductor ExperimentHub package and the `jhpce#HumanPilot10x` Globus endpoint. The package must handle this non-GEO access pattern explicitly.
- The expanded LieberInstitute `spatialDLPFC` cohort (30 Visium sections + matched snRNA-seq) and the Allen Institute SEA-AD atlas (2.78M nuclei from DLPFC and MTG) are candidate references for the cell2location signature. The package may elect to use either or both and must log the reference choice.
- Prior work has clustered spots with both graph-based (Leiden) and spatial-prior (BayesSpace, STAGATE, SpaGCN) methods. The consolidated package must produce a transparent head-to-head comparison against the Maynard ground-truth labels using Adjusted Rand Index (ARI), not only self-consistency.

Data availability and scope:

- All datasets are open-access research data. No controlled-access, PHI, or dbGaP-gated data is used.
- `spatialLIBD` supplies both raw and processed objects for the pilot cohort; the package is free to start from either tier.
- SEA-AD snRNA-seq is open but subject to the Allen Institute Data Use Agreement — the package must record that DUA acknowledgment.
- Treat this as a human DLPFC neurotypical spatial transcriptomics project. No pediatric samples. Any Alzheimer's disease extension must stay at the descriptive composition-shift level.
- Publication scope: internal only.
- Governance: public released research data only; no PHI, no controlled-access data, no secrets.

Required package behavior:

- The package must be suitable for downstream autonomous execution after generation.
- Explicit control decisions required for: (a) spatial clustering method choice (BayesSpace vs Leiden vs STAGATE vs SpaGCN), (b) number of clusters (k), (c) snRNA-seq reference source (SEA-AD vs LieberInstitute DLPFC snRNA-seq), (d) spot-level QC thresholds, (e) deconvolution method (cell2location as primary, STdeconvolve as reference-free sanity check), (f) cross-section transfer evaluation strategy, and (g) fail-closed stop conditions when ARI against Maynard ground truth falls below a prespecified floor.
- Layer label assignment MUST be audited against manually-annotated Maynard ground truth. If ARI is below threshold, the package must stop and refuse to emit layer-level claims.
- Conservative claim boundaries: descriptive laminar architecture and per-layer cell-type abundance only. No disease-state or phenotype claims. Any comparison to SEA-AD AD samples is descriptive composition shift only.
- If runtime dataset refinement occurs (e.g. excluding sections failing QC), provenance and ranking logic must be logged explicitly.
- Literature grounding: at minimum Maynard 2021, BayesSpace (Zhao 2021), cell2location (Kleshchevnikov 2022), STdeconvolve (Miller 2022), spatialLIBD (Pardo 2022), and SEA-AD (Gabitto 2024) must appear in the package bibliography.

## Extracted Overview Preview

```
Project goal: reconstruct DLPFC cortical layers from Visium + snRNA-seq deconvolution.
Anchor: Maynard 2021 Nat Neurosci 12-section pilot with manual layer ground truth.
Distribution: spatialLIBD Bioconductor + Globus (no GEO accession).
Reference: SEA-AD or LieberInstitute spatialDLPFC snRNA-seq for deconvolution signature.
Challenges: shallow spot coverage, curvilinear anatomy, reference region mismatch, donor/section batch.
Methodological question: do spatial-prior clusterings beat Leiden on ARI vs ground truth, and does
cell2location recover expected laminar cell-type stoichiometry within prespecified Wasserstein distance?
Analysis prompts: SpaceRanger QC, BayesSpace, cell2location, STdeconvolve, layer markers, cross-donor transfer.
Claim boundaries: descriptive only; no disease-state claims.
```

## Extracted Study Inventory Preview

```
Maynard 2021 Nat Neurosci HumanPilot10x 3 donors 12 Visium sections spatialLIBD + Globus (no GEO)
LieberInstitute spatialDLPFC 10 donors 30 Visium + matched snRNA-seq
SEA-AD Gabitto 2024 Nat Neurosci 84 donors 2.78M nuclei DLPFC+MTG AD+ctrl
10x Genomics tutorial Visium brain slides — pipeline validation only
```
