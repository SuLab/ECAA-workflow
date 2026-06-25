# Execution order

Tasks in dependency (execution) order. Real outputs live under `runtime/outputs/<task_id>/` — the folder names are the task ids (unchanged).

00  review_prior_work  runtime/outputs/review_prior_work/
01  validate_review_prior_work  runtime/outputs/validate_review_prior_work/
02  data_import  runtime/outputs/data_import/
03  validate_data_import  runtime/outputs/validate_data_import/
04  time_series_decompose  runtime/outputs/time_series_decompose/
05  validate_time_series_decompose  runtime/outputs/validate_time_series_decompose/
06  time_series_model_fit  runtime/outputs/time_series_model_fit/
07  validate_time_series_model_fit  runtime/outputs/validate_time_series_model_fit/
08  time_series_forecast_evaluate  runtime/outputs/time_series_forecast_evaluate/
09  validate_time_series_forecast_evaluate  runtime/outputs/validate_time_series_forecast_evaluate/
10  contextualize_findings_with_literature  runtime/outputs/contextualize_findings_with_literature/
11  validate_contextualize_findings_with_literature  runtime/outputs/validate_contextualize_findings_with_literature/
12  reporting  runtime/outputs/reporting/
13  validate_reporting  runtime/outputs/validate_reporting/
14  final_reporting  runtime/outputs/final_reporting/
15  validate_final_reporting  runtime/outputs/validate_final_reporting/
