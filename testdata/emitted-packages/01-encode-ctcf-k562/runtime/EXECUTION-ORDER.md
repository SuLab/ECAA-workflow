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
08  discover_peak_calling  runtime/outputs/discover_peak_calling/
09  discover_peak_annotation  runtime/outputs/discover_peak_annotation/
10  discover_motif_enrichment  runtime/outputs/discover_motif_enrichment/
11  discover_alignment  runtime/outputs/discover_alignment/
12  sequence_trimming  runtime/outputs/sequence_trimming/
13  validate_sequence_trimming  runtime/outputs/validate_sequence_trimming/
14  alignment  runtime/outputs/alignment/
15  validate_alignment  runtime/outputs/validate_alignment/
16  peak_calling  runtime/outputs/peak_calling/
17  validate_peak_calling  runtime/outputs/validate_peak_calling/
18  peak_annotation  runtime/outputs/peak_annotation/
19  validate_peak_annotation  runtime/outputs/validate_peak_annotation/
20  motif_enrichment  runtime/outputs/motif_enrichment/
21  validate_motif_enrichment  runtime/outputs/validate_motif_enrichment/
22  contextualize_findings_with_literature  runtime/outputs/contextualize_findings_with_literature/
23  validate_contextualize_findings_with_literature  runtime/outputs/validate_contextualize_findings_with_literature/
24  reporting  runtime/outputs/reporting/
25  validate_reporting  runtime/outputs/validate_reporting/
26  final_reporting  runtime/outputs/final_reporting/
27  validate_final_reporting  runtime/outputs/validate_final_reporting/
