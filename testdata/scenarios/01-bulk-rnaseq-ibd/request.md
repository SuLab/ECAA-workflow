# Public IBD Bulk Transcriptomics Treatment-Response Meta-Analysis Package Request

Use the following local files as the source materials for this package request:

- Project overview: testdata/scenarios/01-bulk-rnaseq-ibd/overview.md
- Study inventory (TSV): testdata/scenarios/01-bulk-rnaseq-ibd/studies.tsv

Create a fully autonomous-ready internal research package for a public human bulk RNA-seq and microarray meta-analysis of treatment response in inflammatory bowel disease (IBD), using the datasets summarized in those local files.

Primary objective: identify a reproducible, cross-study baseline mucosal transcriptional signature that separates responders from non-responders to anti-TNF (infliximab) and anti-α4β7-integrin (vedolizumab) therapy in ulcerative colitis and Crohn's disease, stratified by drug class and anatomical segment.

Context from the source files:

- Five public GEO cohorts are staged: GSE16879 (Arijs 2009 *Gut*, IFX in UC/CD), GSE73661 (Arijs 2018 *Gut*, VDZ in UC), GSE75214 (Vanhove 2018), GSE38713 (Planell 2013), and GSE57945 (Haberman 2014 *JCI*, RISK pediatric CD RNA-seq).
- Assay heterogeneity (HG-U133 Plus 2.0, HuGene 1.0 ST, Illumina RNA-seq) mandates platform-aware normalization and a cross-platform meta-analytic backbone rather than naive gene-level concatenation.
- Outcome labels are non-uniform across cohorts (endoscopic Mayo, histologic healing, composite clinical score). The package must record the response-label ontology it applies and block if a cohort cannot be mapped into the canonical responder / non-responder schema.
- Anti-TNF and anti-integrin responses are distinct biologies and MUST NOT be pooled into a single primary contrast — drug-class–stratified meta-analysis is mandatory.

Data availability and scope:

- All five cohorts are open-access on NCBI GEO. No controlled-access, PHI, or dbGaP-gated data is used.
- The package may refine or exclude individual cohorts at runtime if sample-level metadata cannot be harmonized to the responder/non-responder schema or if QC fails a prespecified floor.
- Treat this as a human colon and ileal mucosal biopsy project focused on biologic-therapy response prediction.
- Publication scope: internal only.
- Governance: public released research data only; no PHI, no controlled-access data, no secrets.

Required package behavior:

- The package must be suitable for downstream autonomous execution after generation.
- Explicit control decisions required for: (a) cross-platform normalization strategy (ComBat-Seq for counts vs ComBat for log-intensities vs per-platform fit-and-meta), (b) drug-class stratification policy, (c) disease-segment stratification policy, (d) inclusion/exclusion rules per cohort, (e) heterogeneity thresholds (I² cap) above which the pooled estimate is withheld, and (f) fail-closed stop conditions when response labels cannot be harmonized.
- The consolidated signature MUST be reported with leave-one-cohort-out sensitivity.
- Conservative claim boundaries: descriptive differential expression, pathway-level hypotheses, and heterogeneity diagnostics only. No diagnostic, prognostic, or clinical-utility claims unless validated on a cohort excluded from signature discovery under a prespecified AUROC floor.
- If runtime dataset refinement or reweighting occurs, provenance and ranking logic must be logged explicitly.
- Literature grounding: at minimum the overview references (Arijs 2009, Arijs 2018, Planell 2013, Vanhove 2018, Haberman 2014, Love 2014, Robinson 2010, Law 2014, Smillie 2019, Martin 2019) must appear in the package bibliography.

## Extracted Overview Preview

```
Project goal: reproducible cross-study transcriptional signature of biologic-therapy response in IBD.
Strategy: harmonized per-study DE then random-effects meta-analysis; drug-class stratified.
Key heterogeneity sources: platform, disease phenotype, outcome definition, drug class, biopsy site.
Core methodological question: single pan-responder signature vs drug-class+segment-specific contrasts.
Analysis prompts: per-study DE, cross-platform harmonization, meta-analysis, stratified sub-meta,
pathway enrichment, scRNA-seq deconvolution calibration, decision-curve utility analysis.
Claim boundaries: descriptive DE + pathway hypotheses only; no diagnostic claims without held-out validation.
```

## Extracted Study Inventory Preview

```
GSE16879 Arijs 2009 Gut IFX UC+CD 61 patients responder labels at 4-6 weeks post first infusion
GSE73661 Arijs 2018 Gut VDZ UC ~40 patients mucosal healing W6/W12/W52; has IFX comparator arm
GSE75214 Vanhove 2018 J Crohns Colitis UC+CD 97 patients inflammation severity only
GSE38713 Planell 2013 Gut UC 43 patients active vs remission (no treatment outcome)
GSE57945 Haberman 2014 JCI pediatric CD ileal ~359 patients treatment-naive RISK cohort, partial 1-year response subset
```
