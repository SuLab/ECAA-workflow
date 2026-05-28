# Workflow Context

**Modality:** variant_calling
**Domain:** computational biology
**Description:** Germline short-variant (SNV / indel) calling from WGS or WES. Standard
pipeline: raw QC, trim, align, call variants, filter, annotate. Somatic
/ tumor-normal variants are out of scope; a separate archetype will
cover that workload. Mirrors today's
`config/modalities/variant-calling.yaml` + `config/archetypes/`.

**EDAM topic:** topic:3673
**EDAM operation:** operation:3227
**Confidence:** medium (67%)

## Data sources
- PRJNA694014 (NCBI BioProject)

## SME intake text

We have a SARS-CoV-2 lineage-assignment task on the Tegally et al. 2021 Nature dataset of 341 South African genomes. Raw reads are on NCBI SRA under BioProject PRJNA694014; consensus genomes are on GISAID with EPI_ISL_* accessions listed in Tegally Suppl Table 2. The objective is to confirm that pangolin assigns these genomes to lineage B.1.351 (the Beta variant of concern) and recovers the defining spike-protein substitutions of B.1.351 at the expected positions.

The pipeline is the standard pangolin workflow described in O'Toole et al. 2021 Virus Evolution. Genomes are aligned with minimap2 against the Wuhan-Hu-1 reference NC_045512.2, trimmed to the coding region (positions 265 to 29,674), and 5' and 3' UTRs are masked as N. Genomes with greater than 5% ambiguous bases in the coding region are excluded from lineage assignment. The retained genomes are assigned a lineage by the pangoLEARN decision-tree model, with heuristic SNP-override rules layered on top for known variants of concern (specifically B.1.1.7 and B.1.351).

The acceptance criteria are: at least 320 of the 341 genomes pass the 95% completeness QC; at least 90% of the QC-passing genomes are called B.1.351 by pangolin; and the defining spike-protein changes of B.1.351 -- the seven nonsynonymous substitutions D80A, D215G, K417N, E484K, N501Y, A701V, L18F plus the delta-242-244 three-residue NTD deletion (LAL) -- are recovered in at least 90% of the B.1.351-called genomes. Per the published cross-validation, pangolin's recall should be at least 0.94 and F1 at least 0.95 across designation releases.

The pangolin tool version and the pangoLEARN model release date must both be recorded in the run manifest -- these are the two reproducibility anchors. Pin pangolin v2.3.2 with pangoLEARN release 10 May 2021. Reference genome is NC_045512.2 (GenBank canonical ID).

One additional output: recreate Tegally Fig 3d, the spike-trimer 3D structure with the eight defining changes (D80A, D215G, K417N, E484K, N501Y, A701V, L18F, delta-242-244) mapped onto the surface coloured by protein domain (NTD / RBD / S2). PDB 6VXX is the canonical pre-fusion closed trimer structure to use here (Walls et al. 2020, deposited Feb 2020, released March 2020); 6VYB is the matched open state and should not be substituted.

No claims about transmission dynamics or epidemiology -- purely lineage-assignment correctness on a static genome set.

