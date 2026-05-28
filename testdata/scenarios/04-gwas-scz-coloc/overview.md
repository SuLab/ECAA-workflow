# PGC3 Schizophrenia GWAS Locus Prioritization via eQTL Colocalization

## Project goal
Prioritize candidate causal genes and tissues at schizophrenia-associated loci by combining the PGC3 schizophrenia GWAS summary statistics (Trubetskoy 2022 *Nature*) with GTEx v8 cis-eQTL summary statistics (GTEx Consortium 2020 *Science*) via Bayesian colocalization (`coloc`, Giambartolomei 2014; and `coloc-SuSiE`, Wallace 2021). The deliverable is a ranked gene-tissue table with posterior probabilities of a shared causal variant (PP.H4), together with LD-score heritability partitioning (LDSC) and MAGMA gene-level p-values.

## Strategy
Use the PGC3 schizophrenia wave-3 **public** summary statistics file (the DAC-restricted file is out of scope for this package). Define the universe of independent GWS loci from the Trubetskoy paper's 287 index SNPs. For each locus, extract a ±500 kb window from the GWAS and from each GTEx v8 tissue's cis-eQTL summary statistics, align allele coding and MAF, and run `coloc.abf` and then `coloc.susie` when the locus contains multiple independent signals. Report PP.H4 ≥ 0.75 as "suggestive colocalization" and PP.H4 ≥ 0.9 as "strong colocalization." In parallel, compute MAGMA gene-level p-values with the 1000 Genomes EUR reference LD panel, and compute heritability enrichment across GTEx tissue-specific gene sets using stratified LDSC.

## Key challenges
- **Access tier:** the restricted PGC3 wave-3 file requires DAC approval and is explicitly out of scope here. The package must consume only the public wave-3 file and must fail closed if asked to use the restricted tier.
- **LD reference:** the correct LD panel for European-ancestry PGC3 is 1000 Genomes Phase 3 EUR (~503 samples) on the PGC3 variant space; a mismatched LD panel silently corrupts colocalization posteriors.
- **Locus definition:** whether to define loci by Trubetskoy's index SNPs or by independent clumping (`--clump-p1 5e-8 --clump-r2 0.1 --clump-kb 500`) changes the number of tests; the package must pin one definition and log it.
- **Tissue selection:** GTEx v8 spans 49 tissues; running coloc on every tissue at every locus is computationally wasteful and inflates multiple testing without biological justification. The package must document a prespecified tissue-prioritization rule (e.g. all 13 brain tissues as primary, non-brain as secondary).
- **Assumption violations:** `coloc.abf` assumes a single causal variant per locus; when two or more independent signals exist, use `coloc.susie` instead.

## Work completed so far
The Trubetskoy 2022 paper itself reports 120 prioritized genes (including 106 protein-coding) under a transcript-wide association (TWAS) / SMR / eQTL colocalization triangulation. Several downstream re-analyses (PsychENCODE, OpenTargets Genetics) have wrapped PGC3 + GTEx colocalization into interactive tools. A self-contained, rerunnable end-to-end package that ingests only the two public summary statistics files, pins LD references, and emits a reproducible ranked gene-tissue table with explicit fail-closed gates is still absent from most internal pipelines.

## Core methodological question
Whether `coloc.susie` materially changes the prioritized-gene list at multi-signal loci versus single-causal `coloc.abf`, and whether the top-ranked (PP.H4 ≥ 0.9) gene-tissue pairs recapitulate the published Trubetskoy 120-gene list within a prespecified Jaccard threshold. If they do not, the package must stop and flag either a parameter drift or an upstream data-version mismatch.

## Potential analysis prompts
1. Download and checksum PGC3 wave-3 public summary statistics and GTEx v8 cis-eQTL allpairs.
2. Harmonize allele coding, filter to HWE and MAF thresholds, lift over if needed.
3. Define locus windows around Trubetskoy index SNPs.
4. Run `coloc.abf` for each locus × tissue and tabulate PP.H0–H4.
5. Re-run with `coloc.susie` at loci flagged as multi-signal.
6. Compute MAGMA gene-level p-values.
7. Compute S-LDSC tissue-specific heritability enrichment.
8. Integrate into a ranked gene-tissue table with sensitivity analyses.
9. Benchmark against Trubetskoy 2022 published gene list.

## Conservative claim boundaries
Descriptive gene-tissue prioritization under a Bayesian colocalization framework only. No causal biological claims. No clinical-utility, diagnostic, or therapeutic claims. Any downstream interpretation into synaptic biology or neurodevelopmental pathways is hypothesis-generating and must be labeled as such.

## References
- Trubetskoy V, Pardiñas AF, Qi T, et al. 2022. Mapping genomic loci implicates genes and synaptic biology in schizophrenia. *Nature* 604:502–508. DOI 10.1038/s41586-022-04434-5. PMID 35396580.
- GTEx Consortium. 2020. The GTEx Consortium atlas of genetic regulatory effects across human tissues. *Science* 369:1318–1330. DOI 10.1126/science.aaz1776.
- Giambartolomei C, Vukcevic D, Schadt EE, et al. 2014. Bayesian test for colocalisation between pairs of genetic association studies using summary statistics. *PLoS Genet* 10:e1004383. DOI 10.1371/journal.pgen.1004383.
- Wallace C. 2021. A more accurate method for colocalisation analysis allowing for multiple causal variants. *PLoS Genet* 17:e1009440. DOI 10.1371/journal.pgen.1009440.
- Wang G, Sarkar A, Carbonetto P, Stephens M. 2020. A simple new approach to variable selection in regression, with application to genetic fine mapping (SuSiE). *J R Stat Soc B* 82:1273–1300.
- de Leeuw CA, Mooij JM, Heskes T, Posthuma D. 2015. MAGMA: generalized gene-set analysis of GWAS data. *PLoS Comput Biol* 11:e1004219.
- Bulik-Sullivan BK, Loh PR, Finucane HK, et al. 2015. LD Score regression distinguishes confounding from polygenicity. *Nat Genet* 47:291–295.
- Chang CC, Chow CC, Tellier LC, Vattikuti S, Purcell SM, Lee JJ. 2015. Second-generation PLINK. *GigaScience* 4:7.
- Mbatchou J, Barnard L, Backman J, et al. 2021. Computationally efficient whole-genome regression for quantitative and binary traits (REGENIE). *Nat Genet* 53:1097–1103.
