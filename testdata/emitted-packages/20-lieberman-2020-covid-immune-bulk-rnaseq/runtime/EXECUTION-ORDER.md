# Execution order

Tasks in dependency (execution) order. Real outputs live under `runtime/outputs/<task_id>/` — the folder names are the task ids (unchanged).

00  review_prior_work  runtime/outputs/review_prior_work/
01  validate_review_prior_work  runtime/outputs/validate_review_prior_work/
02  data_acquisition  runtime/outputs/data_acquisition/
03  validate_data_acquisition  runtime/outputs/validate_data_acquisition/
04  raw_qc  runtime/outputs/raw_qc/
05  validate_raw_qc  runtime/outputs/validate_raw_qc/
06  survey_method_landscape  runtime/outputs/survey_method_landscape/
07  discover_sequence_trimming  runtime/outputs/discover_sequence_trimming/
08  discover_quantification  runtime/outputs/discover_quantification/
09  discover_pathway_enrichment  runtime/outputs/discover_pathway_enrichment/
10  discover_normalisation  runtime/outputs/discover_normalisation/
11  discover_differential_expression  runtime/outputs/discover_differential_expression/
12  discover_alignment  runtime/outputs/discover_alignment/
13  sequence_trimming  runtime/outputs/sequence_trimming/
14  validate_sequence_trimming  runtime/outputs/validate_sequence_trimming/
15  alignment  runtime/outputs/alignment/
16  validate_alignment  runtime/outputs/validate_alignment/
17  quantification  runtime/outputs/quantification/
18  validate_quantification  runtime/outputs/validate_quantification/
19  qc_preprocessing  runtime/outputs/qc_preprocessing/
20  validate_qc_preprocessing  runtime/outputs/validate_qc_preprocessing/
21  normalisation  runtime/outputs/normalisation/
22  validate_normalisation  runtime/outputs/validate_normalisation/
23  differential_expression  runtime/outputs/differential_expression/
24  validate_differential_expression  runtime/outputs/validate_differential_expression/
25  pathway_enrichment  runtime/outputs/pathway_enrichment/
26  validate_pathway_enrichment  runtime/outputs/validate_pathway_enrichment/
27  contextualize_findings_with_literature  runtime/outputs/contextualize_findings_with_literature/
28  validate_contextualize_findings_with_literature  runtime/outputs/validate_contextualize_findings_with_literature/
29  reporting  runtime/outputs/reporting/
30  validate_reporting  runtime/outputs/validate_reporting/
31  final_reporting  runtime/outputs/final_reporting/
32  validate_final_reporting  runtime/outputs/validate_final_reporting/
