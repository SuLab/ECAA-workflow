# Workflow Context

**Modality:** bulk_rnaseq
**Domain:** computational biology
**Description:** Bulk RNA-seq differential expression analysis. Standard pipeline: raw QC,
trim, align, quantify, normalise, test for DE between conditions, then
pathway enrichment on the ranked DE result. Mirrors today's
`config/modalities/rnaseq-de.yaml` + `config/archetypes/`.

**EDAM topic:** topic:3308
**EDAM operation:** operation:3223
**Confidence:** high (100%)

## Organisms
- Homo sapiens (taxon:9606)

## Methods mentioned in SME prose

_Keyword-scraped from intake text; see SME discovery decisions above for authoritative values._

- alignment: STAR
- quantification: featureCounts
- differential_expression: DESeq2

## Data sources
- GSE147507 (NCBI GEO Series)

## SME intake text

We want to recreate the bulk RNA-seq transcriptomic response to SARS-CoV-2 infection from Blanco-Melo et al. 2020 Cell, GEO accession GSE147507. We focus on Series 7: Calu3 lung adenocarcinoma cells infected with SARS-CoV-2 at MOI 2 for 24 hours, compared to mock-infected controls. The dataset has two biological replicates per condition (SARS-CoV-2 vs mock), giving 4 samples total from Series 7. This is the primary cell-line series from the paper because Calu3 shows the strongest interferon response among all cell types profiled.

Process paired-end RNA-seq reads (150 bp Illumina) with STAR v2.7+ using the GRCh38 reference genome with Gencode v38 annotation. Mark duplicates with Picard MarkDuplicates (do not remove). Count reads with featureCounts at the gene level (stranded mode based on library prep; confirm with Infer Experiment from RSeQC). Normalize with DESeq2 median-of-ratios.

Run differential expression with DESeq2 comparing SARS-CoV-2 vs mock at 24 hours. The headline finding from the paper is robust type I and type III interferon response coupled with attenuated pro-inflammatory cytokine response. Specific acceptance criteria: IFITM1, IFITM2, IFITM3, ISG15, MX1, OAS1, OAS2, IFIT1, IFIT2, IFIT3, IRF7, STAT1 must all be significantly upregulated (log2FC > 1, adjusted p < 0.05). TNF, IL1B, IL6 must not be among the top 50 most significantly upregulated genes in Calu3 (the attenuated inflammatory response relative to other virus-infected models is the key biological finding). Total DEG count should exceed 3,000 at FDR < 0.05.

Run gene ontology (GO) enrichment analysis on the upregulated DEGs using clusterProfiler; the top enriched GO Biological Process terms must include type I interferon signaling (GO:0060337 or parent), innate immune response (GO:0045087), and antiviral defense (GO:0051607). Run GSEA with the Hallmark interferon alpha and interferon gamma gene sets from MSigDB v7; both should be significant at FDR < 0.05 with NES > 1.5.

For viral transcript quantification, include the SARS-CoV-2 genome (NCBI NC_045512.2) concatenated to the human reference and quantify viral reads as a fraction of total aligned reads. Expect >5% viral reads in SARS-CoV-2 infected replicates and <0.01% in mock.

Acceptance: all 12 interferon-stimulated genes upregulated at specified thresholds; TNF/IL1B/IL6 not in top 50 upregulated; DEG count >3,000 at FDR<0.05; interferon Hallmark sets significant at FDR<0.05.

