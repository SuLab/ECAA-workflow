# Public SG-NEx Long-Read RNA-Seq Isoform Benchmark Package Request

Use the following local files as the source materials for this package request:

- Project overview: testdata/scenarios/09-longread-rna-sgnex/overview.md
- Cell-line inventory (TSV): testdata/scenarios/09-longread-rna-sgnex/studies.tsv

Create a fully autonomous-ready internal research package for a public Nanopore long-read RNA-seq isoform discovery and differential transcript usage reanalysis, using the Singapore Nanopore Expression Project (SG-NEx, Chen 2025 *Nat Methods*) as the primary benchmark.

Primary objective: benchmark three long-read isoform callers (Bambu, FLAIR, IsoQuant) against the SG-NEx sequin and SIRV spike-in truth sets at per-cell-line, per-protocol resolution; compute cross-cell-line differential transcript usage; and compare Nanopore-based DTU against matched Illumina-based DTU.

Context from the source files:

- The SG-NEx core benchmark is 7 cell lines (A549, HCT116, HepG2, K562, MCF7, HEYA8, H9) profiled by 5 library protocols (Nanopore direct RNA, direct cDNA, PCR cDNA, Illumina short-read cDNA, PacBio IsoSeq), all deposited under ENA BioProject **PRJEB44348** and mirrored on the AWS Open Data registry (`sgnex`).
- Isoform-discovery precision/recall MUST be reported against spike-in truth (sequins, SIRVs), not against GENCODE, because GENCODE is not a gold-standard truth for isoform discovery.
- Protocols have very different error profiles and MUST NOT be pooled in the primary analysis.
- GENCODE release is a pinned parameter; re-running the same package against a different GENCODE release will silently change isoform IDs.
- DTU (differential transcript usage), DTE (differential transcript expression), and DGE (differential gene expression) are distinct hypotheses; the package must make this distinction explicit.

Data availability and scope:

- SG-NEx is open-access on ENA and AWS Open Data. No credentialed access.
- No controlled-access, PHI, or identifiable genotype data is used.
- Treat this as a human cell-line long-read RNA-seq isoform benchmarking project. Tissue samples, patient-derived models, and non-human long-read data are out of scope for the primary benchmark.
- Publication scope: internal only.
- Governance: public released research data only; no PHI, no controlled-access data, no secrets.

Required package behavior:

- The package must be suitable for downstream autonomous execution after generation.
- Explicit control decisions required for: (a) reference build (GRCh38 + pinned GENCODE release), (b) aligner parameters per protocol (minimap2 `-ax splice -uf -k14` for direct-RNA, `-ax splice` for cDNA, `-ax splice:hq` for PacBio), (c) isoform caller set (Bambu + FLAIR + IsoQuant primary; StringTie2 optional), (d) spike-in truth source (SG-NEx sequin/SIRV), (e) DTU tool (DRIMSeq + StageR default), (f) cross-platform comparison (Nanopore DTU vs Illumina DTU via Salmon + DRIMSeq), and (g) fail-closed stop conditions when per-tool precision or recall against spike-in truth falls below prespecified floors (e.g. recall ≥ 0.80, precision ≥ 0.75 for direct-RNA).
- Per-protocol results MUST be reported separately; pooling across protocols is not allowed in the primary analysis.
- Conservative claim boundaries: descriptive per-tool isoform-discovery benchmarks and cross-cell-line DTU calls only. No disease, tissue-specific, or functional claims. Isoform-level annotation extensions are hypothesis-generating.
- If runtime refinement occurs (e.g. excluding low-quality runs), provenance and ranking logic must be logged explicitly.
- Literature grounding: at minimum Chen 2025 (SG-NEx), Li 2018 (minimap2), Chen 2023 (Bambu), Tang 2020 (FLAIR), Prjibelski 2023 (IsoQuant), Kovaka 2019 (StringTie2), and Patro 2017 (Salmon) must appear in the package bibliography.

## Extracted Overview Preview

```
Project goal: benchmark long-read isoform callers on SG-NEx sequin/SIRV truth + cross-cell-line DTU.
Cohort: 7 cell lines x 5 library protocols; ENA PRJEB44348; AWS Open Data sgnex mirror.
Reference: GRCh38 + pinned GENCODE release.
Aligners: minimap2 with protocol-specific presets.
Callers: Bambu (Chen 2023) + FLAIR (Tang 2020) + IsoQuant (Prjibelski 2023).
Truth: SG-NEx sequin/SIRV spike-ins (NOT GENCODE).
Methodological question: per-tool precision >= 0.75 and recall >= 0.80 on direct-RNA vs spike-in truth.
Claim boundaries: descriptive per-tool benchmarks only; no disease or functional claims.
```

## Extracted Cell-Line Inventory Preview

```
A549 HCT116 HepG2 K562 MCF7 HEYA8 H9 — 7 core cell lines
Per-cell-line: direct-RNA + direct-cDNA + PCR-cDNA + Illumina short-read + PacBio IsoSeq
All under ENA PRJEB44348; mirrored on AWS Open Data sgnex
Extended SG-NEx release — out of scope for primary benchmark
```
