# R-side forecasting_inference stage: forecast_ribbon, anomaly_timeline.
# Mirrors lib/plotting/stages/forecasting_inference.py.
#
# Plan reference: §S14.3 — time-series-forecast inference figures.

if (!exists("ecaa_register_figure")) {
  stop("source runtime/plotting_r/core.R before this stage module")
}

ecaa_register_figure("forecasting_inference", "forecast_ribbon", function(ctx) {
  p <- .ecaa_manifest_path(ctx$manifest, ctx$outputs_dir, "forecast_table")
  if (is.null(p)) stop("manifest.forecast_table required")
  df <- .ecaa_load_tsv(p, list(
    time = c("time", "date", "timestamp", "t"),
    forecast = c("forecast", "yhat", "predicted", "value"),
    lower = c("lower", "lcl", "yhat_lower", "low"),
    upper = c("upper", "ucl", "yhat_upper", "high"),
    `actual?` = c("actual", "y", "observed")
  ))
  if (is.null(df)) stop(sprintf("unparseable forecast table: %s", p))
  ecaa_forecast_ribbon_r(df, title = "Forecast",
                         actual_col = if ("actual" %in% colnames(df)) "actual" else NULL)
})

ecaa_register_figure("forecasting_inference", "anomaly_timeline", function(ctx) {
  p <- .ecaa_manifest_path(ctx$manifest, ctx$outputs_dir, "anomaly_table")
  if (is.null(p)) stop("manifest.anomaly_table required")
  df <- .ecaa_load_tsv(p, list(
    time = c("time", "date", "timestamp", "t"),
    value = c("value", "y", "observation"),
    is_anomaly = c("is_anomaly", "anomaly", "flag", "alert")
  ))
  if (is.null(df)) stop(sprintf("unparseable anomaly table: %s", p))
  ecaa_anomaly_timeline_r(df, title = "Anomaly timeline")
})
