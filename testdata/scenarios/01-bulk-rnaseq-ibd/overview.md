# Bulk Transcriptomics Meta-Analysis of Treatment Response in Inflammatory Bowel Disease

## Project goal
Define a reproducible, cross-study transcriptional signature of biologic-therapy response in inflammatory bowel disease (IBD) by re-analyzing public mucosal biopsy microarray and bulk RNA-seq cohorts that span both ulcerative colitis (UC) and Crohn's disease (CD). The deliverable is a consolidated ranked gene list separating clinical responders from non-responders prior to first dose, stratified by drug class (anti-TNF vs anti-α4β7-integrin) and disease segment (colon vs ileum).

## Strategy
Leverage publicly deposited GEO cohorts that each reported a pre-treatment mucosal transcriptome paired with a documented response outcome at a protocol-defined endpoint. Harmonize the cohorts to a common gene-level expression space, fit a within-study responder vs non-responder contrast using DESeq2 (Love 2014), edgeR (Robinson 2010), or limma-voom (Law 2014) as appropriate for the assay, then combine per-study effect sizes under a random-effects meta-analysis. Where array and RNA-seq cohorts are mixed, probe-to-gene remapping uses the latest Ensembl BioMart release and array-specific annotations (`hgu133plus2.db`, `hugene10sttranscriptcluster.db`).

## Key challenges
The IBD treatment-response literature is difficult to synthesize due to substantial heterogeneity:

- **Assay platform:** Affymetrix HG-U133 Plus 2.0, Affymetrix HuGene 1.0 ST, Illumina RNA-seq — not directly comparable.
- **Disease phenotype:** mixed UC, CD-colitis and CD-ileitis cohorts; some studies enroll pediatric treatment-naive patients, others enrol adult refractory patients.
- **Outcome definition:** "response" variously defined as endoscopic mucosal healing, histologic remission, clinical Mayo score reduction, or a composite — without a universal operational definition.
- **Drug class:** infliximab (anti-TNF) and vedolizumab (anti-α4β7) cohorts have distinct biology and must not be pooled for the primary contrast.
- **Biopsy site:** inflamed mucosa, peri-inflamed tissue, and paired uninvolved tissue are often mixed.

## Work completed so far
Prior re-analyses of these cohorts have largely treated each study independently. Meta-analytic syntheses exist (West 2017 *Nat Med*, Verstockt 2020 *JCC*, and the SOURCE / UNIFI biomarker programmes) but none of them are packaged as a rerunnable end-to-end pipeline from raw public data through a harmonized meta-analytic output, nor do they expose a fail-closed evidence contract that can be re-executed when a new cohort appears.

## Core methodological question
Whether a single cross-platform, cross-drug "pan-responder" signature is reliable, or whether treatment response is better modelled as a drug-class–specific and disease-segment–specific contrast with partial overlap. The package must support both conditional paths and report how much of the consolidated signature is driven by any single cohort (leave-one-out sensitivity).

## Potential analysis prompts
1. Per-study responder vs non-responder differential expression at treatment baseline, adjusted for age, sex, and inflammation severity.
2. Cross-platform batch harmonization (ComBat-Seq for counts, ComBat for log-intensities) and gene-level meta-analysis.
3. Random-effects meta-analysis of log fold changes with Cochran's Q and I² heterogeneity.
4. Drug-class stratified meta (anti-TNF vs anti-integrin) and disease-segment stratified meta (colon vs ileum).
5. Pathway enrichment on the consolidated signature (Reactome, KEGG, Hallmark) with FGSEA.
6. Cell-type deconvolution against a CITE-seq-derived or scRNA-seq-derived IBD reference to interpret bulk shifts (e.g. Martin 2019 *Cell* ileal CD atlas, Smillie 2019 *Cell* UC atlas).
7. Decision-curve / utility analysis for a hypothetical baseline-biopsy-based responder classifier.

## Conservative claim boundaries
Descriptive cross-study differential expression only. No causal or mechanistic claims. No clinical-utility or diagnostic-performance claims unless held-out validation on a cohort not used for signature discovery meets a prespecified AUROC floor. Pathway-level interpretations are hypothesis-generating.

## References
- Arijs I et al. 2009. *Gut* 58:1612–1619. Mucosal gene signatures to predict response to infliximab in UC. PMID 19700435. GEO GSE16879.
- Arijs I et al. 2018. *Gut* 67:43–52. Effect of vedolizumab on histological healing and mucosal gene expression in UC. GEO GSE73661.
- Planell N et al. 2013. *Gut* 62:967–976. Transcriptional analysis of the intestinal mucosa of UC in remission. PMID 23135761. GEO GSE38713.
- Vanhove W et al. 2018. *J Crohns Colitis* 12:77–86. Effect of IBD active inflammation on mucosal gene expression. GEO GSE75214.
- Haberman Y et al. 2014. *J Clin Invest* 124:3617–3633. Pediatric Crohn's ileal transcriptome and microbiome signature (RISK cohort). DOI 10.1172/JCI75436. GEO GSE57945.
- Love MI, Huber W, Anders S. 2014. *Genome Biol* 15:550. DESeq2. DOI 10.1186/s13059-014-0550-8.
- Robinson MD, McCarthy DJ, Smyth GK. 2010. *Bioinformatics* 26:139–140. edgeR. DOI 10.1093/bioinformatics/btp616.
- Law CW et al. 2014. *Genome Biol* 15:R29. voom. DOI 10.1186/gb-2014-15-2-r29.
- Smillie CS et al. 2019. *Cell* 178:714–730. Intra- and inter-cellular rewiring of the human colon during UC.
- Martin JC et al. 2019. *Cell* 178:1493–1508. Single-cell analysis of ileal Crohn's disease.
