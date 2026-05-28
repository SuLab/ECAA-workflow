# Scenario Meta: Compiler Stressors and Research Corrections

Companion to `README.md`. This file records (a) the unique compiler stressor each scenario is designed to exercise and (b) the citation / accession corrections that were folded in during the research-verification pass, so future authors of additional scenarios can see both the design intent and the failure modes that have already been caught.

## 1. Unique compiler stressors per scenario

Each scenario is deliberately built around at least one orthogonal compiler stressor that no other scenario exercises in the same way. Together they span the dimensions along which the compiler's intake/build/chat phases have historically been brittle.

| # | Scenario | Primary stressor | Secondary stressor | Compiler module(s) stretched |
|---|---|---|---|---|
| 01 | `01-bulk-rnaseq-ibd` | Cross-platform meta-analysis combining microarray (HG-U133 Plus 2.0, HuGene 1.0 ST) with Illumina bulk RNA-seq in a single contrast | Drug-class–stratified sub-meta (anti-TNF vs anti-integrin) must not be silently pooled | `17-modality-bulk-rnaseq`, `30-metadata-and-ontology-normalization` (treatment label ontology), `33-change-impact-and-rerun-control` |
| 02 | `02-spatial-dlpfc` | Non-GEO data access: anchor dataset is distributed via Bioconductor `spatialLIBD` + Globus endpoint `jhpce#HumanPilot10x`, not a GSE. The compiler's `data_source_patterns` regex list will NOT match anything in the request and must fall through gracefully | ARI floor vs manually-annotated layer ground truth | `18-modality-single-cell-rnaseq`, `19-modality-spatial-transcriptomics`, `28-validator-contracts` |
| 03 | `03-wgs-giab-benchmark` | Version-pinning triple constraint: truth-set version (GIAB v4.2.1) + reference build (GRCh38 no-alt) + stratification BED version must all match, and a mismatch must trigger a fail-closed gate | Trio Mendelian consistency as a second-tier sanity check | `21-modality-wgs-wes-variant-calling`, `33-change-impact-and-rerun-control`, `28-validator-contracts` |
| 04 | `04-gwas-scz-coloc` | Hybrid access tier: a public PGC3 wave-3 file AND a DAC-restricted full file exist under the same citation — the compiler must route only to the public tier and reject the restricted tier | LD panel pinning for coloc priors | `13-human-data-governance`, `22-modality-gwas`, `29-autonomy-and-execution-safety` |
| 05 | `05-metagenomics-crc` | Leave-one-cohort-out evaluation is mandatory — in-cohort AUROC is not a valid primary endpoint for a generalization claim, and the compiler must enforce this as a structural constraint | Raw-fastq re-profiling vs `curatedMetagenomicData` fast-path as an explicit control decision that freezes tool version | `25-modality-microbiome`, `41-project-class-biomarker-clinical-utility`, `28-validator-contracts` |
| 06 | `06-proteomics-cptac-brca` | TMT cross-plex IRS normalization is required before any cross-tumor comparison; skipping IRS silently corrupts downstream statistics | Phosphosite localization probability filter (≥ 0.75) | `23-modality-proteomics`, `30-metadata-and-ontology-normalization`, `33-change-impact-and-rerun-control` (search-engine version pin) |
| 07 | `07-perturb-seq-k562` | No single GEO accession — data is split across SRA BioProject PRJNA831566 + figshare article 20029387 + `gwps.wi.mit.edu` portal + scPerturb Zenodo. Compiler must handle a four-way access surface | Out-of-core handling of >1.9 M single cells via backed h5ad | `18-modality-single-cell-rnaseq`, `35-modality-crispr-and-perturbation-screening`, `49-manifest-and-session-schema-contract` |
| 08 | `08-ehr-sepsis-mimic` | Credentialed-access human-in-the-loop gate: PhysioNet DUA + CITI training must be acknowledged before any download, and the compiler must not attempt to autonomously download the data | Label-derivation enforcement: ICD-only sepsis labels are forbidden; the `mimic-code` `sepsis3` derived table is the only canonical label source. Leakage audit against the derivation window is mandatory | `13-human-data-governance`, `27-modality-clinical-prediction-biomarkers`, `29-autonomy-and-execution-safety`, `65-subfield-ehr-real-world-evidence`, `50-confirmatory-and-prespecification-controls` |
| 09 | `09-longread-rna-sgnex` | Per-protocol separate reporting: direct RNA, direct cDNA, PCR cDNA, Illumina short-read, and PacBio IsoSeq must NOT be pooled in the primary analysis because their error profiles differ by orders of magnitude | Spike-in (sequin + SIRV) truth, NOT GENCODE, is the precision/recall reference. DTU vs DTE vs DGE distinction must be preserved | `36-modality-long-read-genomics-transcriptomics`, `28-validator-contracts`, `33-change-impact-and-rerun-control` (GENCODE release pin) |
| 10 | `10-methylation-aging-clock` | Preprocessing pipeline pin: SeSAMe / minfi / ENmix / ChAMP produce materially different beta matrices and clock MAE can shift by 0.5–2 years depending on the pipeline choice. Primary pipeline must be pinned AND a cross-pipeline sanity check must be reported | Cell-composition adjustment (IDOL / EpiDISH) is mandatory in the EWAS, not optional | `34-modality-methylation-epigenomics`, `33-change-impact-and-rerun-control`, `28-validator-contracts` |

## 2. Cross-scenario stressors

Stressors that ≥ 2 scenarios exercise in different ways — these test the shared cross-cutting prompt modules:

| Cross-cutting stressor | Scenarios that exercise it | Prompt module |
|---|---|---|
| Fail-closed version pinning | 03 (GIAB v4.2.1 truth), 06 (search-engine version), 09 (GENCODE release), 10 (preprocessing pipeline) | `33-change-impact-and-rerun-control` |
| Access-tier enforcement (public vs restricted) | 04 (PGC3 public vs DAC), 08 (PhysioNet credentialed) | `13-human-data-governance`, `29-autonomy-and-execution-safety` |
| Prespecified pass/fail floor against a published benchmark | 02 (ARI vs Maynard labels), 05 (Jaccard vs Wirbel 29-species), 06 (Rand index vs Krug subtypes), 07 (ARI vs Replogle modules), 08 (AUROC/Brier floors), 09 (precision/recall vs spike-in truth), 10 (MAE ≤ 4 years) | `28-validator-contracts`, `50-confirmatory-and-prespecification-controls` |
| Multi-source data access (≥ 2 portals for one dataset) | 02 (Bioconductor + Globus + GitHub), 07 (SRA + figshare + gwps portal + scPerturb Zenodo), 05 (ENA + curatedMetagenomicData) | `data_source_patterns` + manifest schema |
| Compositional / transform-required data | 05 (CLR for microbiome abundances), 10 (beta-value vs M-value), 04 (log-odds for GWAS summary stats) | `12-statistics-and-inference` |
| Ontology or label-mapping hazard | 01 (treatment response ontology), 05 (adenoma vs CRC), 06 (PAM50 RNA vs proteomic subtype), 08 (Sepsis-3 derivation) | `30-metadata-and-ontology-normalization` |
| "Do not pool" structural constraint | 01 (drug class), 02 (donor/section), 09 (library protocol), 10 (array version) | `28-validator-contracts` |

## 3. Research corrections folded in during verification

These are the facts that differed from the initial scenario plan and were corrected before writing. They are recorded here so future authors do not re-introduce the same errors.

### Data accession corrections

- **Planell 2013 → GSE38713, not GSE59071.** GSE59071 is a separate 2015 Arijs/Vermeire series from the same group, not the Planell 2013 dataset. (Scenario 01)
- **Maynard 2021 has no single GEO accession.** The primary distribution is the `spatialLIBD` Bioconductor ExperimentHub package + the Globus endpoint `jhpce#HumanPilot10x` + the `LieberInstitute/HumanPilot` GitHub repo. Do NOT invent a GSE ID. (Scenario 02)
- **Replogle 2022 canonical access is SRA BioProject PRJNA831566** plus figshare article 20029387 and `gwps.wi.mit.edu`, not a single GSE. The dataset is split across three sub-experiments (`K562_gwps`, `K562_essential`, `RPE1_essential`) and the compiler must handle all three. (Scenario 07)
- **Wirbel 2019 five primary cohort ENA accessions are verified as:** France ERP005534, Austria ERP008729, China PRJEB10878, USA PRJEB12449, Germany PRJEB27928 (the new cohort introduced in Wirbel 2019). (Scenario 05)
- **Krug 2020 CPTAC-BRCA PDC accessions:** PDC000120 for the proteome and PDC000121 for the phosphoproteome. Sample count is 122 prospective tumors (some summary text also reports 125 patients + 18 adjacent normals — use 122 as the headline). (Scenario 06)

### Citation corrections

- **Zook 2019 is *Nature Biotechnology*, not *Scientific Data*.** The 2016 companion "Extensive sequencing of seven human genomes" *Sci Data* 3:160025 is a different paper by the same group. (Scenario 03)
- **YAIB (van de Water 2024) was published at *ICLR 2024*, not NeurIPS.** arXiv:2306.05109. (Scenario 08)
- **GSFA authors are Zhou Y, Luo K, Liang L, Chen M, He X** (Xin He lab, University of Chicago), not "Xie/Zhao" as in one of the initial drafts. (Scenario 07)
- **SG-NEx core benchmark (Chen 2025 *Nat Methods*) uses 7 cell lines**, not 14. The "14 cell lines" figure that appears in some press releases describes the *extended* SG-NEx resource, not the peer-reviewed core benchmark. (Scenario 09)

### Tool-scope corrections

- **`methylKit` is a bisulfite-sequencing package, not a 450k/EPIC array tool.** Primary 450k/EPIC array pipelines must rely on `minfi` / `SeSAMe` / `ENmix` / `ChAMP`. `methylKit` can only be cited when WGBS/RRBS is actually part of the scope. (Scenario 10)
- **`curatedMetagenomicData` freezes the profiling tool version** at the package release time (MetaPhlAn3/MetaPhlAn4 depending on cut). It is faster than raw-fastq re-profiling but is NOT a drop-in replacement when version-pinning is required — this is a real control decision that the compiler must surface. (Scenario 05)
- **Spectronaut / Bruderer 2015 cite was NOT re-verified** in the research pass and is flagged as uncertain. Scenario 06 uses DIA-NN (Demichev 2020) as the primary DIA tool instead and only mentions Spectronaut as an optional alternative. (Scenario 06)

### Access-tier corrections

- **PGC3 wave-3 access is hybrid, not uniformly public.** A public wave-3 file is freely downloadable from the PGC portal; a DAC-restricted file with full sample-overlap metadata requires a PGC Data Access Committee application. Scenario 04 uses only the public tier and explicitly marks the DAC tier out of scope. (Scenario 04)
- **GTEx v8 individual-level genotypes and reads are dbGaP-gated** under `phs000424.v8.p2` and are out of scope for scenario 04. Only the open-access cis-eQTL summary statistics from the GTEx Portal are used. (Scenario 04)
- **MIMIC-IV and eICU-CRD are PhysioNet credentialed-access**, not fully open. CITI training + signed DUA are required. The compiler must NOT attempt autonomous download; a human-in-the-loop acknowledgment gate is mandatory. (Scenario 08)
- **SEA-AD (Allen Institute) dense spatial coverage is middle temporal gyrus (MTG), not DLPFC.** SEA-AD does provide DLPFC snRNA-seq and multi-omics data, but the tightest spatial MERFISH coverage is MTG. Scenario 02 uses SEA-AD only as a snRNA-seq reference for cell2location deconvolution, not as a disease-specific DLPFC spatial atlas. (Scenario 02)

### Label-derivation corrections

- **MIMIC-IV does NOT ship hand-labeled sepsis outcomes.** Sepsis-3 labels must be derived via the official `mimic-code` `sepsis3` table, which implements SOFA ≥ 2 + suspected infection (antibiotics + cultures). Deriving labels from ICD-9/10 codes alone is a known failure mode and scenario 08 explicitly forbids it. (Scenario 08)

## 4. What a future scenario-author should take away

When adding an 11th scenario:

1. Start by naming the **single modality taxonomy** the scenario primarily stretches, then list what else it touches (single-modality scenarios are fine, but the whole point of this testdata is to avoid duplicating a stressor that an existing scenario already covers).
2. **Verify every accession** via NCBI/ENA/PDC/PhysioNet/Bioconductor portal page AND against the primary paper's data availability statement. Do not trust knowledge-cutoff recollection for accession IDs — the corrections in section 3 above are all mistakes that a diligent author would have made without verification.
3. **Name a prespecified pass/fail floor** against a number published in the anchor paper (AUROC, Jaccard, ARI, MAE, precision/recall). A scenario without a prespecified floor does not exercise `28-validator-contracts` or `50-confirmatory-and-prespecification-controls`, and it does not stretch the fail-closed machinery.
4. **Write an access-tier line explicitly.** Even if everything is open-access, say so. If any tier is credentialed or DAC-gated, the package must treat that as a human-in-the-loop gate.
5. **Pin versions wherever drift would silently corrupt results** — reference builds, tool versions, annotation releases, truth-set versions, preprocessing pipelines. The compiler has `33-change-impact-and-rerun-control` for exactly this reason.
6. **Do not pool when the protocol says not to pool.** Drug class, library protocol, array version, batch plex, and donor-of-origin are load-bearing stratifiers for several scenarios and the compiler must enforce them as structural constraints, not as optional stratifications.
