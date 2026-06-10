# Himes Airway Smooth Muscle RNA-seq Differential Expression

Bulk RNA-seq differential expression analysis of human airway smooth muscle
(ASM) cells treated with the glucocorticoid dexamethasone, from Himes et al.
2014 (PMC4057123, GEO GSE52778; the Bioconductor `airway` dataset). Counts
matrix already prepared (63,677 Ensembl gene IDs x 8 samples).

Goal: identify genes differentially expressed in response to dexamethasone
(treated vs untreated) in airway smooth muscle, using DESeq2, with standard
QC + visualisation (sample-distance heatmap, PCA, MA plot, volcano, top-DE-gene
heatmap, pathway enrichment).

Data: counts.tsv (gene_id + per-sample count columns) and samples.csv (sample,
cell, dex metadata) at testdata/scenarios/atoms/bulk-rnaseq-himes/. No raw
FASTQs - start from the counts matrix. Bulk RNA-seq, treated (dex) vs untreated
contrast. The 8 libraries come from FOUR independent donor airway smooth muscle
cell lines (the `cell` column in samples.csv), each assayed both dexamethasone-
treated and untreated - a paired/blocked design. Control for the donor cell-line
covariate so the estimated dexamethasone effect is not confounded by
donor-to-donor baseline differences.
