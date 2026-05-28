# Public-Data Test Scenarios for the doc_generator Compiler

These ten scenarios exist to stress-test the compiler across orthogonal bioinformatics modalities using only publicly deposited datasets and citation-verified papers. Each scenario mirrors the shape of `testdata/IVD_prompt/`: a `request.md` (the prompt ingested by the compiler), an `overview.md` (project narrative, in place of the IVD docx), and a `studies.tsv` (dataset inventory, in place of the IVD xlsx).

Every scenario was authored to:

- cover a distinct primary modality taxonomy (bulk RNA-seq, single-cell+spatial, variant calling, GWAS, metagenomics, proteomics, CRISPR perturbation, clinical/EHR, long-read, epigenomics);
- cite the canonical paper plus ≥ 3 canonical method references;
- point at a verified public accession (GEO / ENA / SRA / PDC / GDC / PhysioNet / spatialLIBD / figshare / AWS Open Data);
- enforce conservative claim boundaries consistent with `prompts/29-autonomy-and-execution-safety.md` and the IVD pattern;
- exercise at least one control decision and one fail-closed stop condition unique to that modality.

## Index

| # | Directory | Modality taxonomy | Anchor dataset | Primary paper | Unique compiler stressor |
|---|---|---|---|---|---|
| 01 | `01-bulk-rnaseq-ibd/` | `bulk_rnaseq` | GSE16879, GSE73661, GSE75214, GSE38713, GSE57945 | Arijs 2009 *Gut*; Arijs 2018 *Gut*; Haberman 2014 *JCI* | cross-platform meta (array + RNA-seq); drug-class–stratified contrast; LOCO sensitivity |
| 02 | `02-spatial-dlpfc/` | `single_cell_rnaseq` + spatial | spatialLIBD `HumanPilot10x` (Maynard 2021); LieberInstitute `spatialDLPFC`; SEA-AD | Maynard 2021 *Nat Neurosci*; Gabitto 2024 *Nat Neurosci* | non-GEO access (Bioconductor + Globus); ARI floor vs manual layer ground truth |
| 03 | `03-wgs-giab-benchmark/` | `variant_calling` | GIAB NA12878 / Ashkenazi trio v4.2.1; 1000G NYGC 30x | Zook 2019 *Nat Biotechnol*; Krusche 2019 *Nat Biotechnol*; Byrska-Bishop 2022 *Cell* | reference-build pinning; truth-set version pinning; stratification BED audit; trio Mendelian check |
| 04 | `04-gwas-scz-coloc/` | GWAS + eQTL colocalization | PGC3 SCZ wave3 public; GTEx v8 cis-eQTL | Trubetskoy 2022 *Nature*; GTEx 2020 *Science*; Giambartolomei 2014 *PLoS Genet* | hybrid access tier (public vs DAC-restricted); LD panel pinning; tissue prioritization rule |
| 05 | `05-metagenomics-crc/` | `metagenomics` | 5 ENA cohorts (ERP005534, ERP008729, PRJEB10878, PRJEB12449, PRJEB27928) | Wirbel 2019 *Nat Med*; Pasolli 2017 *Nat Methods* | leave-one-cohort-out evaluation; CLR transform; raw-fastq vs curatedMetagenomicData control decision |
| 06 | `06-proteomics-cptac-brca/` | `proteomics` | PDC000120 (proteome) + PDC000121 (phosphoproteome) + GDC CPTAC-3 | Krug 2020 *Cell*; Mertins 2016 *Nature* | TMT cross-plex IRS normalization; phosphosite localization filter; PAM50 proteomic re-derivation |
| 07 | `07-perturb-seq-k562/` | `single_cell_rnaseq` + perturbation screening | SRA PRJNA831566 + figshare 20029387 + gwps.wi.mit.edu | Replogle 2022 *Cell*; Papalexi 2021 *Nat Genet*; Peidli 2024 *Nat Methods* | no single GEO accession; out-of-core (>1.9 M cells); knockdown-efficiency floor per guide |
| 08 | `08-ehr-sepsis-mimic/` | clinical prediction + EHR | MIMIC-IV v3.1; eICU-CRD v2.0 | Johnson 2023 *Sci Data*; Pollard 2018 *Sci Data*; Singer 2016 *JAMA*; van de Water 2024 *ICLR* (YAIB) | PhysioNet credentialed access (human-in-the-loop); `mimic-code` `sepsis3` label derivation; leakage audit |
| 09 | `09-longread-rna-sgnex/` | long-read transcriptomics | ENA PRJEB44348 (SG-NEx core 7 cell lines × 5 protocols) | Chen 2025 *Nat Methods*; Li 2018 *Bioinformatics* (minimap2) | per-protocol separate reporting; spike-in (sequin/SIRV) truth not GENCODE; DTU vs DTE vs DGE distinction |
| 10 | `10-methylation-aging-clock/` | methylation / epigenomics | GSE40279 (Hannum 2013 training); GSE87571 (calibration); GSE87648 (disease context) | Horvath 2013 *Genome Biol*; Hannum 2013 *Mol Cell*; Levine 2018 *Aging*; Lu 2019 *Aging* | clock MAE floor on held-out cohort; preprocessing-pipeline control decision; cell-composition adjustment |

## Compiler coverage rationale

The ten scenarios together exercise the following modality prompts in `prompts/`:

- `17-modality-bulk-rnaseq.md` (scenario 1)
- `18-modality-single-cell-rnaseq.md` (scenarios 2, 7)
- `19-modality-spatial-transcriptomics.md` (scenario 2)
- `21-modality-wgs-wes-variant-calling.md` (scenario 3)
- `22-modality-gwas.md` (scenario 4)
- `23-modality-proteomics.md` (scenario 6)
- `25-modality-microbiome.md` (scenario 5)
- `27-modality-clinical-prediction-biomarkers.md` (scenario 8)
- `34-modality-methylation-epigenomics.md` (scenario 10)
- `35-modality-crispr-and-perturbation-screening.md` (scenario 7)
- `36-modality-long-read-genomics-transcriptomics.md` (scenario 9)

Supporting cross-cutting modules exercised by ≥ 2 scenarios:

- `13-human-data-governance.md` — scenarios 4 (DAC access-tier gate), 6 (CPTAC), 8 (credentialed PhysioNet DUA)
- `15-literature-grounding.md` — all scenarios
- `28-validator-contracts.md` — all scenarios carry prespecified pass/fail floors
- `29-autonomy-and-execution-safety.md` — all scenarios; scenario 8 additionally requires an explicit human-in-the-loop gate
- `30-metadata-and-ontology-normalization.md` — scenarios 1, 5, 6, 8
- `33-change-impact-and-rerun-control.md` — scenarios 3 (truth-set version pin), 6 (search-engine version pin), 9 (GENCODE release pin), 10 (preprocessing pipeline pin)
- `41-project-class-biomarker-clinical-utility.md` — scenarios 1, 5, 8, 10

## How to use

Each `request.md` is a self-contained intake file. Feed it to the compiler with the `intake` subcommand:

```bash
# From the repo root, with the workspace built (`make build`)
ecaa-workflow intake \
  --input testdata/scenarios/01-bulk-rnaseq-ibd/request.md \
  --output /tmp/scenario-01

# Inspect the DAG
ecaa-workflow dag --package /tmp/scenario-01

# Or execute it end-to-end with Claude Code as the agent
ecaa-workflow-harness \
  --package /tmp/scenario-01 \
  --agent ./scripts/agent-claude.sh \
  --max-iterations 30
```

The `overview.md` and `studies.tsv` in each scenario directory are the inline-referenced source material — the compiler may parse them directly or rely on previews inlined in the request. They are also useful when reviewing a scenario by hand before running the compiler against it.

For the built-in IVD scenario end-to-end test (not one of the ten, but the reference real-world test), use `make ivd-execute` from the repo root; see [../../docs/testing.md](../../docs/testing.md) for the full test map.

Intended coverage mapping for the historical testdata campaigns now archived under `testdata/archive/campaigns/`: these scenarios are candidates for being added to `classification.json`, `workflow.json`, and `repeatability.json` to verify that the modality classifier and the stage-taxonomy mapper pick the expected taxonomy for each.

## Governance summary

| Scenario | Governance tier |
|---|---|
| 01, 02, 03, 05, 07, 09, 10 | public released research data only |
| 04 | public PGC3 wave3 tier only; DAC-restricted tier explicitly out of scope |
| 06 | public PDC + GDC CPTAC-3 tier only |
| 08 | PhysioNet credentialed access; DUA + CITI training required; human-in-the-loop download gate |

No scenario uses PHI, identifiable genotype data beyond explicitly consented public tiers, or dbGaP-gated individual-level records.

## Citation hygiene

Every scenario's `overview.md` includes a references section with DOIs. All accessions have been verified as of April 2026 via NCBI/ENA/PDC/PhysioNet/Bioconductor portal pages and the cited primary literature. Where the research pass found a correction relative to the original plan (e.g. Planell 2013 is GSE38713 not GSE59071; Zook 2019 is *Nat Biotechnol* not *Sci Data*; YAIB is ICLR 2024 not NeurIPS; Replogle 2022 canonical access is SRA PRJNA831566 not a single GSE; Maynard 2021 has no GEO accession and is distributed via spatialLIBD + Globus), the scenarios reflect the corrected facts.
