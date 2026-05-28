# Auditability Corpus — 23 × 30 = 690 Claim Stubs

This directory contains the **claim-auditability corpus** for PAR-26-040 grant
§Aim 3B. The corpus exercises the `claim_extractor` + `claim_verifier` pipeline
described in `docs/` and is the primary artifact for the claim-auditability
pre-registration. Studies 1-13 ship the original 13×30 baseline; studies 14-19
add the Tier-1 modality-breadth expansion (2026-05-16: proteomics LFQ,
methylation WGBS, spatial-transcriptomics Visium, metagenomics HMP2, bulk-RNA
COVID Calu-3, single-cell ATAC); studies 20-23 add the B-01..B-04 anchors for
the §C.0.1 paper-recreation benchmark (Lieberman COVID immune, 10x PBMC 10k,
ENCODE H3K4me3, Tyanova MaxQuant).

## Layout

```
tests/auditability-corpus/
  01-encode-ctcf-k562/          claims.yaml   (30 claims)
  02-fang-seqc2-hcc1395/        claims.yaml   (30 claims)
  03-recount3-gtex-liver/       claims.yaml   (30 claims)
  04-pbmc-3k-10x/               claims.yaml   (30 claims)
  05-depmap-ccle-rnaseq/        claims.yaml   (30 claims)
  06-tcga-brca-rnaseq/          claims.yaml   (30 claims)
  07-geo-gse68849-chipseq/      claims.yaml   (30 claims)
  08-encode-atac-k562/          claims.yaml   (30 claims)
  09-geo-gse117872-scrnaseq/    claims.yaml   (30 claims)
  10-geo-gse112879-metagenomics/claims.yaml   (30 claims)
  11-proteomicsdb-human/        claims.yaml   (30 claims)
  12-geo-gse63525-hic/          claims.yaml   (30 claims)
  13-giab-ashkenazi-trio/       claims.yaml   (30 claims)
  14-bouwmeester-lfq/           claims.yaml   (30 claims)
  15-loyfer-wgbs/               claims.yaml   (30 claims)
  16-maynard-visium/            claims.yaml   (30 claims)
  17-lloyd-price-hmp2/          claims.yaml   (30 claims)
  18-blanco-melo-calu3/         claims.yaml   (30 claims)
  19-cusanovich-sci-atac/       claims.yaml   (30 claims)
  20-lieberman-covid/           claims.yaml   (30 claims)
  21-10x-pbmc10k/               claims.yaml   (30 claims)
  22-encode-h3k4me3/            claims.yaml   (30 claims)
  23-tyanova-maxquant/          claims.yaml   (30 claims)
                                              ─────────────
                                              690 claims total
```

Each `claims.yaml` has `schema_version: "0.1"` and a `claims:` list of 30
entries. Every entry carries:

| Field | Description |
|---|---|
| `id` | Unique claim identifier (`<study_id>-claim-NNN`). |
| `text` | Natural-language claim text (HCI lead fills this). |
| `contract` | Verification contract type (default: `numeric_table_lookup`). |
| `source_table` | Path to the result table inside the emitted package (e.g. `results/tables/de.csv`). |
| `expected_value` | Numeric or categorical value the verifier checks (HCI lead fills). |
| `tolerance` | Acceptable absolute deviation for numeric claims (HCI lead fills). |

## HCI Lead Fill Protocol

All fields marked `PLACEHOLDER` or `null` must be filled by the HCI lead
before the corpus is used in a pre-registered evaluation run.

**Per-claim checklist:**

1. Replace `text` with the specific scientific claim from the study's published
   results (e.g. "The number of differentially expressed genes at FDR < 0.05
   is 1 234").
2. Set `contract` to the appropriate verification strategy:
   - `numeric_table_lookup` — a number read from a CSV/TSV column.
   - `existence_check` — presence of a file or row.
   - `regex_match` — a pattern the verifier applies to a text column.
   - `boolean_flag` — a true/false fact from a JSON result field.
3. Set `source_table` to the relative path inside the emitted package where
   the claim can be verified.
4. Set `expected_value` to the ground-truth value from the published paper.
5. Set `tolerance` (numeric claims only) to the acceptable deviation (e.g. for
   fold-change claims, 0.01 is usually appropriate).

**Anti-p-hacking discipline:** claim content must be locked (committed) before
any evaluation run against the claims corpus. Changes to `expected_value` or
`tolerance` after observing verifier output require a logged amendment with
rationale. See `docs/eval-prereg/` for the pre-registration process.

## Verification Pipeline

The `claim_verifier` in `crates/core/src/claim_verifier.rs` reads these YAML
files at eval time. The Tier 20.x runners (`tier-20-1`, `tier-20-2`,
`tier-20-3`) exercise the verifier against this corpus and the labeled
adversarial extensions.

Run a dry-pass (no live data required):

```bash
cargo test -p scripps-workflow-core claim_verifier
```

Run the full Tier 20.1 ROC scorer (requires emitted packages):

```bash
SWFC_TIER_20_1_LIVE=1 cargo run --bin tier-20-1 -- \
  --corpus-dir tests/auditability-corpus
```

## Study Descriptions

| Dir | Study | Modality |
|---|---|---|
| `01-encode-ctcf-k562` | ENCODE CTCF ChIP-seq in K562 cells | chip_seq |
| `02-fang-seqc2-hcc1395` | Fang et al. SEQC2 HCC1395 tumor/normal | bulk_rnaseq |
| `03-recount3-gtex-liver` | recount3 GTEx v8 liver tissue | bulk_rnaseq |
| `04-pbmc-3k-10x` | 10x PBMC 3k public dataset | single_cell_rnaseq |
| `05-depmap-ccle-rnaseq` | DepMap CCLE RNA-seq (22Q2 release) | bulk_rnaseq |
| `06-tcga-brca-rnaseq` | TCGA BRCA RNA-seq (GDC harmonized) | bulk_rnaseq |
| `07-geo-gse68849-chipseq` | GSE68849 H3K27ac ChIP-seq (mouse) | chip_seq |
| `08-encode-atac-k562` | ENCODE ATAC-seq in K562 (open chromatin) | atac_seq |
| `09-geo-gse117872-scrnaseq` | GSE117872 single-cell RNA-seq (human) | single_cell_rnaseq |
| `10-geo-gse112879-metagenomics` | GSE112879 gut metagenomics | metagenomics |
| `11-proteomicsdb-human` | ProteomicsDB human tissue proteome | proteomics |
| `12-geo-gse63525-hic` | GSE63525 Hi-C (Rao et al. 2014) | hi_chip |
| `13-giab-ashkenazi-trio` | GIAB Ashkenazi trio (HG002/3/4) | variant_calling |
| `14-bouwmeester-lfq` | Bouwmeester LFQ proteomics drug-target deconvolution | proteomics |
| `15-loyfer-wgbs` | Loyfer WGBS cell-type atlas (GSE186458) | methylation |
| `16-maynard-visium` | Maynard DLPFC Visium (spatialLIBD) | spatial_transcriptomics |
| `17-lloyd-price-hmp2` | Lloyd-Price HMP2 / iHMP IBDMDB metagenomics | metagenomics |
| `18-blanco-melo-calu3` | Blanco-Melo Calu-3 SARS-CoV-2 bulk RNA-seq | bulk_rnaseq |
| `19-cusanovich-sci-atac` | Cusanovich sci-ATAC-seq mouse atlas | single_cell_atac |
| `20-lieberman-covid` | Lieberman 2020 COVID immune bulk RNA-seq (B-01) | bulk_rnaseq |
| `21-10x-pbmc10k` | 10x PBMC 10k v3 single-cell RNA-seq (B-02) | single_cell_rnaseq |
| `22-encode-h3k4me3` | ENCODE H3K4me3 K562 ChIP-seq (B-03) | chip_seq |
| `23-tyanova-maxquant` | Tyanova MaxQuant proteomics protocol (B-04) | proteomics |
