# Public Schizophrenia GWAS Locus Prioritization Package Request

Use the following local files as the source materials for this package request:

- Project overview: testdata/scenarios/04-gwas-scz-coloc/overview.md
- Resource inventory (TSV): testdata/scenarios/04-gwas-scz-coloc/studies.tsv

Create a fully autonomous-ready internal research package for a public human GWAS locus prioritization reanalysis of schizophrenia, using the PGC3 wave-3 public summary statistics (Trubetskoy 2022 *Nature*) and GTEx v8 cis-eQTL summary statistics (GTEx Consortium 2020 *Science*).

Primary objective: produce a ranked gene-tissue table with Bayesian colocalization posteriors (`coloc.abf` and `coloc.susie`) for schizophrenia-associated loci, triangulated with MAGMA gene-level p-values and stratified LD-score regression tissue-enrichment, and benchmarked against the Trubetskoy 2022 published 120 prioritized genes.

Context from the source files:

- The PGC3 summary statistics have a **hybrid access tier**: a public wave-3 file is freely downloadable and a restricted full-sample-overlap file is DAC-gated. This package uses **only the public tier** and MUST fail closed if the restricted file is referenced.
- GTEx v8 cis-eQTL summary statistics are open-access via the GTEx Portal. Individual-level GTEx genotypes and reads are dbGaP-gated (`phs000424.v8.p2`) and are **explicitly out of scope**.
- Locus definition, LD reference panel (1000 Genomes Phase 3 EUR), tissue-prioritization rule, and coloc prior choice (`p1`, `p2`, `p12`) all materially affect the output and must be pinned in the package manifest.
- The Trubetskoy 2022 paper reports 287 GWS loci and 120 prioritized genes under a TWAS/SMR/coloc triangulation; the consolidated package should benchmark its output against that gene list as a prespecified sanity check.

Data availability and scope:

- All data are open-access research summary statistics.
- No controlled-access, PHI, dbGaP-gated genotype data, or secrets are ingested.
- Treat this as a European-ancestry schizophrenia locus prioritization project. Trans-ancestry meta-analysis and fine-mapping is explicitly out of scope.
- Publication scope: internal only.
- Governance: public released summary statistics only; DAC-gated tiers are out of scope and must trigger a fail-closed gate.

Required package behavior:

- The package must be suitable for downstream autonomous execution after generation.
- Explicit control decisions required for: (a) access-tier enforcement (public PGC3 only; reject DAC-restricted and dbGaP tiers), (b) locus-definition rule (Trubetskoy index SNPs vs PLINK clumping), (c) window size (default ±500 kb), (d) LD reference panel (1000G Phase 3 EUR), (e) tissue-prioritization rule (13 brain tissues primary, non-brain secondary), (f) coloc prior choice (defaults `p1 = p2 = 1e-4`, `p12 = 1e-5`), (g) single-signal vs SuSiE-multi-signal threshold, and (h) fail-closed stop conditions when the public/restricted tier boundary is violated or LD panel is mismatched.
- The package MUST re-benchmark its top-ranked gene-tissue pairs against the Trubetskoy 2022 published 120-gene list and report Jaccard and rank-concordance statistics.
- Conservative claim boundaries: descriptive gene-tissue colocalization and pathway hypothesis generation only. No causal, clinical, diagnostic, therapeutic, or genotype-to-phenotype-mechanism claims.
- If runtime refinement occurs (e.g. tissue filtering based on expression), provenance and ranking logic must be logged explicitly.
- Literature grounding: at minimum Trubetskoy 2022, GTEx 2020, Giambartolomei 2014 (coloc), Wallace 2021 (coloc-SuSiE), de Leeuw 2015 (MAGMA), Bulik-Sullivan 2015 (LDSC), Wang 2020 (SuSiE), and Chang 2015 (PLINK2) must appear in the package bibliography.

## Extracted Overview Preview

```
Project goal: PGC3 SCZ + GTEx v8 Bayesian colocalization-driven gene-tissue prioritization.
Summary statistics: PGC3 wave3 public file (DAC-restricted file out of scope); GTEx v8 cis-eQTL allpairs.
LD reference: 1000 Genomes Phase 3 EUR (503 samples).
Tissue prioritization: 13 brain tissues primary, non-brain secondary.
Methods: coloc.abf single-causal primary; coloc.susie at multi-signal loci; MAGMA gene-level; S-LDSC.
Benchmark: compare top PP.H4 >= 0.9 gene-tissue pairs against Trubetskoy 2022 published 120 prioritized genes.
Claim boundaries: descriptive prioritization only; no causal or clinical claims.
```

## Extracted Resource Inventory Preview

```
PGC3 SCZ wave3 public  Trubetskoy 2022 Nature  76755 cases / 243649 controls  287 GWS loci  public
PGC3 SCZ wave3 restricted  DAC-gated  OUT OF SCOPE
GTEx v8 cis-eQTL allpairs  GTEx Consortium 2020 Science  838 donors  49 tissues  public (use 13 brain as primary)
GTEx v8 individual-level  dbGaP phs000424.v8.p2  OUT OF SCOPE
1000G Phase3 EUR LD panel  503 EUR individuals  public — coloc + MAGMA reference
```
