# Himes Airway Smooth Muscle RNA-seq Differential Expression

Bulk RNA-seq differential expression analysis of human airway smooth muscle
cells treated with the glucocorticoid dexamethasone (Himes et al. 2014; the
Bioconductor airway dataset). A counts matrix is already prepared (63,677
Ensembl gene IDs x 8 samples).

Goal: identify genes differentially expressed in response to dexamethasone
(treated vs untreated) using DESeq2, with standard QC + visualisation
(sample-distance heatmap, PCA, MA plot, volcano, top-DE-gene heatmap, pathway
enrichment).

Data (in your current working directory):
- counts.tsv  -- gene_id + per-sample count columns (8 samples)
- samples.csv -- sample, cell, dex metadata

No raw FASTQs -- start from the counts matrix. Bulk RNA-seq, treated (dex) vs
untreated contrast. The 8 libraries come from FOUR independent donor airway
smooth muscle cell lines (the `cell` column), each assayed both
dexamethasone-treated and untreated -- a paired/blocked design. Control for the
donor cell-line covariate so the estimated dexamethasone effect is not
confounded by donor-to-donor baseline differences (see the `cell` column).

Also save your full differential-expression results table as `de_results.tsv`
(at minimum: gene_id, log2FoldChange, padj) so the result is inspectable.

Write your analytical narrative (decisions, intermediate findings,
interpretation) to trace.md and a short structured answer to answer.txt.

## Evaluation integrity
Base every result solely on the data files provided here. Do NOT search for,
fetch, or rely on the original source publication for this dataset, its figures
or tables, or any external answer key; derive all findings from your own
analysis of the provided data. You may consult tool/library documentation, but
not look up this task's answers.

IMPORTANT: write trace.md, answer.txt, and de_results.tsv directly in your
current working directory using exactly those relative filenames. Do not use an
absolute path such as /app and do not create subdirectories for them.
