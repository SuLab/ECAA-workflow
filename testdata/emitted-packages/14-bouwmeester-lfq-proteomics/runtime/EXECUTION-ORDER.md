# Execution order

Tasks in dependency (execution) order. Real outputs live under `runtime/outputs/<task_id>/` — the folder names are the task ids (unchanged).

00  review_prior_work  runtime/outputs/review_prior_work/
01  validate_review_prior_work  runtime/outputs/validate_review_prior_work/
02  data_acquisition  runtime/outputs/data_acquisition/
03  validate_data_acquisition  runtime/outputs/validate_data_acquisition/
04  survey_method_landscape  runtime/outputs/survey_method_landscape/
05  discover_protein_quantification  runtime/outputs/discover_protein_quantification/
06  discover_peptide_search  runtime/outputs/discover_peptide_search/
07  discover_pathway_enrichment  runtime/outputs/discover_pathway_enrichment/
08  discover_differential_expression  runtime/outputs/discover_differential_expression/
09  peptide_search  runtime/outputs/peptide_search/
10  validate_peptide_search  runtime/outputs/validate_peptide_search/
11  protein_quantification  runtime/outputs/protein_quantification/
12  validate_protein_quantification  runtime/outputs/validate_protein_quantification/
13  differential_expression  runtime/outputs/differential_expression/
14  validate_differential_expression  runtime/outputs/validate_differential_expression/
15  pathway_enrichment  runtime/outputs/pathway_enrichment/
16  validate_pathway_enrichment  runtime/outputs/validate_pathway_enrichment/
17  contextualize_findings_with_literature  runtime/outputs/contextualize_findings_with_literature/
18  validate_contextualize_findings_with_literature  runtime/outputs/validate_contextualize_findings_with_literature/
19  reporting  runtime/outputs/reporting/
20  validate_reporting  runtime/outputs/validate_reporting/
21  final_reporting  runtime/outputs/final_reporting/
22  validate_final_reporting  runtime/outputs/validate_final_reporting/
