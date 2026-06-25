# Execution order

Tasks in dependency (execution) order. Real outputs live under `runtime/outputs/<task_id>/` — the folder names are the task ids (unchanged).

00  review_prior_work  runtime/outputs/review_prior_work/
01  validate_review_prior_work  runtime/outputs/validate_review_prior_work/
02  data_acquisition  runtime/outputs/data_acquisition/
03  validate_data_acquisition  runtime/outputs/validate_data_acquisition/
04  raw_qc  runtime/outputs/raw_qc/
05  validate_raw_qc  runtime/outputs/validate_raw_qc/
06  survey_method_landscape  runtime/outputs/survey_method_landscape/
07  discover_taxonomic_classification  runtime/outputs/discover_taxonomic_classification/
08  discover_sequence_trimming  runtime/outputs/discover_sequence_trimming/
09  sequence_trimming  runtime/outputs/sequence_trimming/
10  validate_sequence_trimming  runtime/outputs/validate_sequence_trimming/
11  taxonomic_classification  runtime/outputs/taxonomic_classification/
12  validate_taxonomic_classification  runtime/outputs/validate_taxonomic_classification/
13  diversity_analysis  runtime/outputs/diversity_analysis/
14  validate_diversity_analysis  runtime/outputs/validate_diversity_analysis/
15  contextualize_findings_with_literature  runtime/outputs/contextualize_findings_with_literature/
16  validate_contextualize_findings_with_literature  runtime/outputs/validate_contextualize_findings_with_literature/
17  reporting  runtime/outputs/reporting/
18  validate_reporting  runtime/outputs/validate_reporting/
19  final_reporting  runtime/outputs/final_reporting/
20  validate_final_reporting  runtime/outputs/validate_final_reporting/
