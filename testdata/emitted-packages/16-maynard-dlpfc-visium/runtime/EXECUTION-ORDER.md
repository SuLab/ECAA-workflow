# Execution order

Tasks in dependency (execution) order. Real outputs live under `runtime/outputs/<task_id>/` — the folder names are the task ids (unchanged).

00  review_prior_work  runtime/outputs/review_prior_work/
01  validate_review_prior_work  runtime/outputs/validate_review_prior_work/
02  data_acquisition  runtime/outputs/data_acquisition/
03  validate_data_acquisition  runtime/outputs/validate_data_acquisition/
04  qc_preprocessing  runtime/outputs/qc_preprocessing/
05  validate_qc_preprocessing  runtime/outputs/validate_qc_preprocessing/
06  survey_method_landscape  runtime/outputs/survey_method_landscape/
07  discover_spatial_clustering_method  runtime/outputs/discover_spatial_clustering_method/
08  discover_normalisation  runtime/outputs/discover_normalisation/
09  discover_dimensionality_reduction  runtime/outputs/discover_dimensionality_reduction/
10  discover_differential_expression  runtime/outputs/discover_differential_expression/
11  discover_cell_type_annotation  runtime/outputs/discover_cell_type_annotation/
12  normalisation  runtime/outputs/normalisation/
13  validate_normalisation  runtime/outputs/validate_normalisation/
14  dimensionality_reduction  runtime/outputs/dimensionality_reduction/
15  validate_dimensionality_reduction  runtime/outputs/validate_dimensionality_reduction/
16  spatial_domain_segmentation  runtime/outputs/spatial_domain_segmentation/
17  validate_spatial_domain_segmentation  runtime/outputs/validate_spatial_domain_segmentation/
18  spatially_variable_genes  runtime/outputs/spatially_variable_genes/
19  validate_spatially_variable_genes  runtime/outputs/validate_spatially_variable_genes/
20  cell_type_annotation  runtime/outputs/cell_type_annotation/
21  validate_cell_type_annotation  runtime/outputs/validate_cell_type_annotation/
22  differential_expression  runtime/outputs/differential_expression/
23  validate_differential_expression  runtime/outputs/validate_differential_expression/
24  contextualize_findings_with_literature  runtime/outputs/contextualize_findings_with_literature/
25  validate_contextualize_findings_with_literature  runtime/outputs/validate_contextualize_findings_with_literature/
26  reporting  runtime/outputs/reporting/
27  validate_reporting  runtime/outputs/validate_reporting/
28  final_reporting  runtime/outputs/final_reporting/
29  validate_final_reporting  runtime/outputs/validate_final_reporting/
