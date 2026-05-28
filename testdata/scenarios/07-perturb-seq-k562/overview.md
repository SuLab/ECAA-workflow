# Genome-Scale Perturb-seq Reanalysis: Causal Gene Modules from CRISPRi in K562

## Project goal
Re-derive the information-rich genotype-phenotype landscape from the Replogle et al. 2022 *Cell* genome-scale Perturb-seq dataset and identify co-regulated gene modules whose expression fingerprints recur across related perturbations. The deliverable is a ranked table of CRISPRi target genes grouped into transcriptional-response modules with perturbation-weighted module assignments and an interpretable low-dimensional embedding.

## Strategy
Ingest the three Replogle 2022 sub-experiments — K562 genome-wide (`K562_gwps`, 9,866 expressed genes knocked down, day 8), K562 essentialome (`K562_essential`, 2,057 common-essential genes, day 6), and RPE1 essentialome (`RPE1_essential`, day 7) — from SRA BioProject PRJNA831566 and the companion figshare processed h5ad (figshare 20029387) and the `gwps.wi.mit.edu` portal. Preprocess with Scanpy / CellRanger metrics, compute a per-guide pseudo-bulk transcriptome ("energy distance" or "mean-centered log-fold change from NTC controls"), cluster perturbations by their transcriptomic fingerprints (hierarchical clustering on pseudo-bulk z-scores; alternatively UMAP + Leiden on the 2.5 M+ single cells after Mixscape regression-out of unperturbed cells), and recover gene modules. Benchmark against the Replogle-2022 published perturbation clustering and against CellOracle (Kamimoto 2023) gene-regulatory-network predictions.

## Key challenges
- **Scale:** the genome-wide K562 experiment contains >2 million single cells; naive clustering pipelines will exhaust RAM on single machines. Use on-disk backed h5ad (anndata/loompy) and out-of-core computation.
- **Knockdown verification:** median per-guide knockdown is 86% (K562) / 92% (RPE1) in the published data, but per-target knockdown varies. The package must quality-filter guides whose knockdown is below a prespecified floor (e.g. 50% relative to NTC) before downstream analysis.
- **Essentiality masking:** for essential-gene knockdowns, affected cells drop out, shortening the observation window and biasing the pseudo-bulk toward surviving cells. The package must flag essential targets explicitly.
- **Multiple guides per target:** Replogle used multiple sgRNAs per target; the package must decide whether to analyze at the guide level (more noise, more granularity) or target level (guide-pooled pseudo-bulk).
- **Batch and plate effects:** the experiment was carried out across multiple 10x lanes and days; a per-lane correction (harmonypy / scVI) may be required before embedding.
- **Dataset identification:** no single GEO accession exists; canonical citation is **SRA PRJNA831566** plus the figshare processed data. The package must handle both access paths explicitly.

## Work completed so far
Replogle 2022 established the reference methodology for genome-scale CRISPRi + single-cell transcriptomics and released all data openly. Several follow-up analyses have used this dataset as a reference for gene-regulatory-network inference (CellOracle, SCENIC+, GRNBoost, Dictys), for causal-module discovery (GSFA, Pertpy, scPerturb), and as a negative control for perturbation benchmarking (scPerturb, Peidli 2024 *Nat Methods*). The `scPerturb` harmonized release is the preferred structured access path for downstream projects.

## Core methodological question
Whether target-level transcriptomic-fingerprint clustering at K562 genome scale recovers the Replogle-2022 published CORUM / BioGRID complex-aware modules at adjusted Rand ≥ 0.6, and whether the recovered modules are stable under leave-one-guide-out resampling. If either criterion fails, the package must stop and flag either a knockdown filter issue or a batch-correction mismatch.

## Potential analysis prompts
1. Download the K562_gwps processed h5ad from figshare / gwps.wi.mit.edu; download matched metadata from PRJNA831566 if re-processing from raw is required.
2. QC per-cell (UMI, gene, mito) and per-guide (knockdown efficiency vs NTC).
3. Mixscape classification of perturbed vs escaping cells for weak guides.
4. Per-target pseudo-bulk contrast vs NTC (log-fold change, energy distance).
5. Hierarchical clustering of target transcriptomic fingerprints into modules.
6. UMAP embedding of single cells; Leiden clustering; module-to-cell-state mapping.
7. Benchmark against Replogle 2022 published modules / CORUM complexes.
8. Gene-regulatory-network inference with CellOracle or SCENIC+.
9. Sensitivity: leave-one-guide-out stability of module assignments.

## Conservative claim boundaries
Descriptive transcriptomic-response module assignment and gene-regulatory-network hypothesis generation only. No claims about essentiality, therapeutic target relevance, or in vivo phenotype. In silico perturbation predictions are hypothesis-generating and must be labeled as such.

## References
- Replogle JM, Saunders RA, Pogson AN, et al. 2022. Mapping information-rich genotype-phenotype landscapes with genome-scale Perturb-seq. *Cell* 185:2559–2575.e28. DOI 10.1016/j.cell.2022.05.013. PMID 35688146.
- Papalexi E, Mimitou EP, Butler AW, et al. 2021. Characterizing the molecular regulation of inhibitory immune checkpoints with multimodal single-cell screens (Mixscape). *Nat Genet* 53:322–331. DOI 10.1038/s41588-021-00778-2.
- Peidli S, Green TD, Shen C, et al. 2024. scPerturb: harmonized single-cell perturbation data. *Nat Methods* 21:531–540. DOI 10.1038/s41592-023-02144-y.
- Kamimoto K, Stringa B, Hoffmann CM, et al. 2023. Dissecting cell identity via network inference and in silico gene perturbation (CellOracle). *Nature* 614:742–751. DOI 10.1038/s41586-022-05688-9.
- Zhou Y, Luo K, Liang L, Chen M, He X. 2023. A new Bayesian factor analysis method improves detection of genes and biological processes affected by perturbations in single-cell CRISPR screening (GSFA). *Nat Methods* 20:1693–1703. DOI 10.1038/s41592-023-02017-4.
- Li W, Xu H, Xiao T, et al. 2014. MAGeCK enables robust identification of essential genes from genome-scale CRISPR/Cas9 knockout screens. *Genome Biol* 15:554. DOI 10.1186/s13059-014-0554-4.
- Zheng GX, Terry JM, Belgrader P, et al. 2017. Massively parallel digital transcriptional profiling of single cells. *Nat Commun* 8:14049. DOI 10.1038/ncomms14049.
