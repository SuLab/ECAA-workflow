# Public CPTAC Breast Cancer Proteogenomic Reanalysis Package Request

Use the following local files as the source materials for this package request:

- Project overview: testdata/scenarios/06-proteomics-cptac-brca/overview.md
- Dataset inventory (TSV): testdata/scenarios/06-proteomics-cptac-brca/studies.tsv

Create a fully autonomous-ready internal research package for a public human CPTAC prospective breast cancer proteogenomic reanalysis, using the Krug 2020 *Cell* cohort as the primary dataset.

Primary objective: re-derive the CPTAC-BRCA prospective cohort proteomic and phosphoproteomic landscape from open-access RAW and processed files (PDC000120, PDC000121), cross-reference with matched CPTAC-3 WES/WGS/RNA-seq from the Genomic Data Commons, and re-identify PAM50-subtype-discriminating proteomic and phosphoproteomic features with per-subtype kinase-substrate enrichment modules, benchmarked against the Krug 2020 published tables.

Context from the source files:

- The primary cohort is 122 treatment-naive primary tumors profiled by TMT-11 LC-MS/MS. The cohort is split across ~11 TMT plexes; cross-plex IRS normalization is mandatory.
- Proteomic and phosphoproteomic RAW and processed data are open-access on the Proteomic Data Commons (PDC000120 for proteome, PDC000121 for phosphoproteome).
- Matched CPTAC-3 WES/WGS/RNA-seq is on the Genomic Data Commons under the CPTAC-3 program.
- The retrospective TCGA-based Mertins 2016 *Nature* cohort is available as an independent validation comparison through the PDC legacy portal.
- PAM50 subtype labels are canonically RNA-based; protein-based subtype classifiers exist but must be explicitly chosen.
- Phosphosite-level analysis requires a localization-probability filter (≥ 0.75) to avoid ambiguous assignments.

Data availability and scope:

- PDC proteomics RAW and processed files are open-access.
- GDC CPTAC-3 WES/WGS/RNA-seq are open-access.
- No controlled-access, PHI, or dbGaP-gated data is used.
- Treat this as a human breast cancer TMT proteomics + matched genomics project. Plasma proteomics, other cancer types, and DIA-only reprocessing are out of scope for the primary analysis.
- Publication scope: internal only.
- Governance: public released research data only; no PHI, no controlled-access data, no secrets.

Required package behavior:

- The package must be suitable for downstream autonomous execution after generation.
- Explicit control decisions required for: (a) raw-data re-search vs processed-table consumption, (b) search engine choice (FragPipe/MSFragger default, MaxQuant alternative), (c) database version pin (UniProt human canonical + isoform), (d) TMT normalization strategy (median per plex + IRS as canonical), (e) phosphosite localization-probability threshold, (f) statistical-contrast tool (MSstats default), (g) PAM50 subtype label source (RNA-based vs proteomic PAM50 equivalent), and (h) fail-closed stop conditions when re-derived subtype assignments diverge from Krug 2020 published labels beyond a prespecified Rand-index floor.
- Protein-RNA correlations MUST be reported explicitly per gene; the package must not silently substitute RNA expression for protein abundance.
- Conservative claim boundaries: descriptive subtype-discriminating proteomic and phosphoproteomic features and hypothesis-generating kinase-substrate enrichment modules only. No clinical-utility, therapeutic actionability, diagnostic, or survival claims.
- If runtime refinement occurs (e.g. excluding tumors that fail QC), provenance and ranking logic must be logged explicitly.
- Literature grounding: at minimum Krug 2020, Mertins 2016, Cox & Mann 2008 (MaxQuant), Choi 2014 (MSstats), da Veiga Leprevost 2020 (Philosopher/FragPipe), and Kong 2017 (MSFragger) must appear in the package bibliography.

## Extracted Overview Preview

```
Project goal: re-derive Krug 2020 CPTAC-BRCA prospective proteogenomic landscape from raw PDC files.
Cohort: 122 treatment-naive primary tumors; TMT-11 LC-MS/MS across ~11 plexes.
Accessions: PDC000120 (proteome), PDC000121 (phosphoproteome), GDC CPTAC-3 WES/WGS/RNA-seq.
Validation: Mertins 2016 Nature TCGA retrospective cohort.
Pipeline: FragPipe/MSFragger default, MaxQuant alternative; MSstats contrast; IRS cross-plex normalization.
Methodological question: do re-derived subtype assignments match Krug 2020 at Rand >= 0.9 and
phosphosite feature Jaccard >= 0.5.
Claim boundaries: descriptive subtype-discriminating features only; no clinical or therapeutic claims.
```

## Extracted Dataset Inventory Preview

```
PDC000120 CPTAC-BRCA prospective proteome Krug 2020 Cell 122 tumors TMT-11
PDC000121 CPTAC-BRCA prospective phosphoproteome Krug 2020 Cell same 122 tumors
PDC legacy CPTAC-BRCA retrospective TCGA Mertins 2016 Nature 77 tumors
GDC CPTAC-3 BRCA WES/WGS/RNA-seq — matched genomics for the 122 tumors
```
