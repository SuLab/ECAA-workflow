# Execution order

Tasks in dependency (execution) order. Real outputs live under `runtime/outputs/<task_id>/` — the folder names are the task ids (unchanged).

00  review_prior_work  runtime/outputs/review_prior_work/
01  validate_review_prior_work  runtime/outputs/validate_review_prior_work/
02  data_acquisition  runtime/outputs/data_acquisition/
03  validate_data_acquisition  runtime/outputs/validate_data_acquisition/
04  raw_qc  runtime/outputs/raw_qc/
05  validate_raw_qc  runtime/outputs/validate_raw_qc/
06  survey_method_landscape  runtime/outputs/survey_method_landscape/
07  discover_variant_filtering  runtime/outputs/discover_variant_filtering/
08  discover_variant_calling  runtime/outputs/discover_variant_calling/
09  discover_variant_annotation  runtime/outputs/discover_variant_annotation/
10  discover_sequence_trimming  runtime/outputs/discover_sequence_trimming/
11  discover_alignment  runtime/outputs/discover_alignment/
12  sequence_trimming  runtime/outputs/sequence_trimming/
13  validate_sequence_trimming  runtime/outputs/validate_sequence_trimming/
14  alignment  runtime/outputs/alignment/
15  variant_calling  runtime/outputs/variant_calling/
16  variant_filtering  runtime/outputs/variant_filtering/
17  variant_annotation  runtime/outputs/variant_annotation/
18  validate_variant_annotation  runtime/outputs/validate_variant_annotation/
19  contextualize_findings_with_literature  runtime/outputs/contextualize_findings_with_literature/
20  validate_contextualize_findings_with_literature  runtime/outputs/validate_contextualize_findings_with_literature/
21  reporting  runtime/outputs/reporting/
22  validate_reporting  runtime/outputs/validate_reporting/
23  final_reporting  runtime/outputs/final_reporting/
24  validate_final_reporting  runtime/outputs/validate_final_reporting/
25  validate_variant_filtering  runtime/outputs/validate_variant_filtering/
26  validate_variant_calling  runtime/outputs/validate_variant_calling/
27  validate_alignment  runtime/outputs/validate_alignment/
