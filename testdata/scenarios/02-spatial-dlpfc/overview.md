# Human Dorsolateral Prefrontal Cortex Spatial Transcriptomics Atlas Reconstruction

## Project goal
Reconstruct the laminar organization of human dorsolateral prefrontal cortex (DLPFC) from publicly deposited 10x Genomics Visium spatial transcriptomics data, and project cell-type abundances from a matched single-nucleus RNA-seq reference onto the tissue architecture via probabilistic deconvolution.

## Strategy
Use the Maynard et al. 2021 *Nature Neuroscience* pilot DLPFC Visium dataset (12 sections from 3 neurotypical adult donors) as the anchor, and complement with the LieberInstitute `spatialDLPFC` expanded 30-sample Visium resource and the Seattle Alzheimer's Disease Brain Cell Atlas (SEA-AD) matched snRNA-seq data. The analytic backbone is BayesSpace spatial clustering at spot resolution, cell2location cell-type deconvolution with a snRNA-seq reference signature, and layer-wise marker re-identification compared against the Maynard pilot's gold-standard histologically-annotated layers L1–L6/WM.

## Key challenges
- **Shallow per-spot coverage:** Visium 55 µm spots contain 1–10 cells and are UMI-sparse, so single-spot cell-type calls are noisy and require prior-regularized deconvolution.
- **Anatomical registration:** cortical layers are curvilinear; spot-level clustering must respect spatial neighborhood structure rather than treating spots as independent.
- **Reference mismatch:** snRNA-seq references often come from different brain regions (primary motor cortex, middle temporal gyrus) and may underrepresent DLPFC-specific subtypes.
- **No canonical GEO accession** for the Maynard 2021 pilot — data distribution is via the `spatialLIBD` Bioconductor ExperimentHub package and the `jhpce#HumanPilot10x` Globus endpoint, which the package must handle as a first-class data source pattern.
- **Batch effects** between donors and between the two serial-section replicates within each donor must be distinguishable from laminar biology.

## Work completed so far
The Maynard pilot released a histologically-annotated ground truth for cortical layers on 12 sections, which has since served as the de-facto benchmark for every spatial-clustering method published in the field (BayesSpace, STAGATE, SpaGCN, GraphST, SiGra, PRECAST). What is less well established is the end-to-end re-derivation of the layer labels from raw SpaceRanger outputs together with a snRNA-seq-driven cell2location deconvolution that honours the known excitatory/inhibitory/glial composition of each layer. A rerunnable, fail-closed package for this task does not yet exist in the public domain.

## Core methodological question
Whether spot-level laminar clusters derived with spatial priors (BayesSpace) are more accurate than unsupervised Leiden clustering on gene expression alone, and whether the layer-wise cell-type composition recovered by cell2location matches the expected snRNA-seq-derived stoichiometry to within a prespecified Wasserstein distance. If either criterion fails, the package must stop and refuse to emit layer-level claims.

## Potential analysis prompts
1. Run SpaceRanger (already pre-computed for the pilot) through QC, filter spots by UMI/feature thresholds, and project to log-normalized counts.
2. Spatial clustering with BayesSpace at enhanced resolution; comparison against Leiden and K-means baselines; ARI vs the Maynard ground-truth labels.
3. Build a snRNA-seq reference signature from SEA-AD DLPFC or from a matched Lieber snRNA-seq DLPFC cohort.
4. Cell2location deconvolution at spot level; summarize per-layer cell-type abundance.
5. STdeconvolve as a reference-free sanity check.
6. Layer marker re-identification (e.g. `PCP4`, `CUX2`, `RORB`, `NTNG2`, `TLE4`) and comparison to literature.
7. Cross-donor layer transfer: does layer identity learned on donor A transfer to donors B and C?
8. Sensitivity analysis: leave-one-section-out spatial clustering.

## Conservative claim boundaries
Descriptive laminar structure and per-layer cell-type abundance only. No disease-state claims. No inference about cognitive or psychiatric phenotypes. Any Alzheimer's disease comparison to SEA-AD data must stay at the descriptive composition-shift level.

## References
- Maynard KR, Collado-Torres L, Weber LM, et al. 2021. Transcriptome-scale spatial gene expression in the human dorsolateral prefrontal cortex. *Nature Neuroscience* 24:425–436. DOI 10.1038/s41593-020-00787-0. PMID 33558695.
- Pardo B, Spangler A, Weber LM, et al. 2022. spatialLIBD: an R/Bioconductor package to visualize spatially-resolved transcriptomics data. *BMC Genomics* 23:434. DOI 10.1186/s12864-022-08601-w.
- Zhao E, Stone MR, Ren X, et al. 2021. Spatial transcriptomics at subspot resolution with BayesSpace. *Nat Biotechnol* 39:1375–1384. DOI 10.1038/s41587-021-00935-2.
- Kleshchevnikov V, Shmatko A, Dann E, et al. 2022. Cell2location maps fine-grained cell types in spatial transcriptomics. *Nat Biotechnol* 40:661–671. DOI 10.1038/s41587-021-01139-4.
- Miller BF, Huang F, Atta L, Sahoo A, Fan J. 2022. Reference-free cell type deconvolution of multi-cellular pixel-resolution spatially resolved transcriptomics data. *Nat Commun* 13:2339. DOI 10.1038/s41467-022-30033-z.
- Gabitto MI et al. 2024. Integrated multimodal cell atlas of Alzheimer's disease (SEA-AD). *Nat Neurosci*. PMC11577961.
- 10x Genomics. SpaceRanger software and Visium chemistry documentation.
