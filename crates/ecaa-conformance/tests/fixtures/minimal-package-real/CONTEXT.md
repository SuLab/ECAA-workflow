# Workflow Context

**Modality:** single_cell_rnaseq
**Domain:** computational biology
**Description:** scRNA-seq clustering + differential expression. Standard pipeline:
preprocess, cell-level QC, normalise, optional batch correction +
integration sweep, dimensionality reduction, cluster, annotate cell
types, test cluster-vs-rest or cross-condition DE, then pathway
enrichment. Mirrors today's `config/stage-taxonomies/single-cell.yaml`.

**EDAM topic:** topic:3308
**EDAM operation:** operation:3432
**Confidence:** high (100%)

## Organisms
- Homo sapiens (taxon:9606)

## SME intake text

single cell scRNA-seq from human IVD samples comparing degenerated and healthy single cell scRNA-seq from human IVD samples comparing degenerated and healthy
