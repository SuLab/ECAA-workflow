# Public Germline Variant Calling GIAB Benchmark Package Request

Use the following local files as the source materials for this package request:

- Project overview: testdata/scenarios/03-wgs-giab-benchmark/overview.md
- Sample inventory (TSV): testdata/scenarios/03-wgs-giab-benchmark/studies.tsv

Create a fully autonomous-ready internal research package for a reproducible benchmarking of germline short-variant calling on human whole-genome sequencing data, using the NIST Genome in a Bottle (GIAB) reference individuals as ground truth.

Primary objective: re-derive precision, recall, and F1 for SNVs and small indels from both GATK4 HaplotypeCaller and DeepVariant on the GIAB HG002 Ashkenazi son 30× Illumina WGS sample (and optionally the HG002/HG003/HG004 trio) against the GIAB v4.2.1 truth VCF inside the v4.2.1 high-confidence BED, stratified by genomic region difficulty.

Context from the source files:

- GIAB is the community-accepted source of germline truth for Illumina short-read variant calling (Zook 2019 *Nat Biotechnol*).
- The benchmarking must follow the Krusche 2019 best-practices (`hap.py` + `vcfeval` engine, normalized VCFs, region-stratified reporting).
- Truth sets are versioned; the package MUST pin v4.2.1 for both truth and high-confidence BED, and MUST reject any stratification BED mismatched to this version or to the reference build.
- Data are distributed via the NIST FTP endpoint `ftp-trace.ncbi.nlm.nih.gov/ReferenceSamples/giab/release/` under `NA12878_HG001/` and `AshkenazimTrio/`. The 1000 Genomes Project NYGC 30× rebase (Byrska-Bishop 2022 *Cell*) is an optional companion dataset for non-benchmarked samples.

Data availability and scope:

- GIAB truth sets and FASTQ/BAM are open-access, fully public, no credentialed access.
- 1000 Genomes NYGC 30× rebase is open-access via IGSR.
- No controlled-access, PHI, or dbGaP-gated data is used.
- Treat this as a human GRCh38 short-read Illumina benchmarking project. Long-read (PacBio HiFi, ONT) evaluation is explicitly out of scope and must not be added without re-issuing this request.
- Publication scope: internal only.
- Governance: public released research data only; no PHI, no controlled-access data, no secrets.

Required package behavior:

- The package must be suitable for downstream autonomous execution after generation.
- Explicit control decisions required for: (a) reference build pinning (GRCh38-no-alt-analysis-set is the default), (b) truth-set version pinning (v4.2.1), (c) caller set (GATK HC + DeepVariant as primary; Strelka2 or Clair3 as optional third arm), (d) read-pair alignment tool (BWA-MEM2 default), (e) duplicate-marking tool, (f) stratification BED provenance, and (g) fail-closed stop conditions when the stratification BED version does not match the truth-set version or when the reference build of the BAM does not match the pinned build.
- Stratification MUST be carried out against the official GIAB stratifications v3.x BEDs matched to the truth-set version.
- Results MUST be reported as SNV and indel precision/recall/F1 separately, with region-stratified tables for segmental duplications, homopolymers, GC content extremes, tandem repeats, and low-mappability regions.
- Conservative claim boundaries: descriptive caller precision/recall on a reference benchmark only. No clinical-performance, diagnostic-suitability, or cross-technology extrapolation claims. Results are explicitly limited to the region strata actually measured.
- If the trio is run, Mendelian consistency of joint-genotyped calls MUST be reported.
- Literature grounding: at minimum Zook 2019, Krusche 2019, Poplin 2018 (GATK4 bioRxiv), Poplin 2018 (DeepVariant *Nat Biotech*), Li 2013 (BWA-MEM), Van der Auwera 2020 (GATK book), and Byrska-Bishop 2022 must appear in the package bibliography.

## Extracted Overview Preview

```
Project goal: reproducible GIAB-truth precision/recall for germline short-variant calling.
Anchor sample: HG002 Ashkenazi son, 30x Illumina NovaSeq, GRCh38.
Ground truth: GIAB v4.2.1 truth VCF + high-confidence BED + stratification BEDs v3.x.
Pipeline: BWA-MEM2 alignment; GATK4 HaplotypeCaller and DeepVariant in parallel;
hap.py + vcfeval for comparison; bcftools norm normalization first.
Stratification: segmental duplications, homopolymers, GC extremes, tandem repeats, low-mappability.
Methodological question: reproduce PrecisionFDA Truth Challenge headline numbers from raw BAM end-to-end.
Claim boundaries: descriptive benchmark only; no clinical or cross-platform claims.
```

## Extracted Sample Inventory Preview

```
HG001 NA12878 CEPH Utah NIST GIAB v4.2.1 GRCh38 30x NovaSeq
HG002 NA24385 Ashkenazi son NIST GIAB v4.2.1 GRCh38 30x NovaSeq (+ 300x available)
HG003 NA24149 Ashkenazi father
HG004 NA24143 Ashkenazi mother
1000G HG00096 NYGC 30x rebase Byrska-Bishop 2022 Cell — no GIAB truth, used for pipeline-validation only
```
