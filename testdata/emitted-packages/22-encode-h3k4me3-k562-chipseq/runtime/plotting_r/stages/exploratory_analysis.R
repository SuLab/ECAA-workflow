# R-side exploratory_analysis stage: decomposition_panel,
# acf_pacf_panel. Mirrors lib/plotting/stages/exploratory_analysis.py.
#
# Plan reference: §S14.3 — time-series-forecast exploratory figures.

if (!exists("ecaa_register_figure")) {
  stop("source runtime/plotting_r/core.R before this stage module")
}

ecaa_register_figure("exploratory_analysis", "decomposition_panel",
                     function(ctx) {
  p <- .ecaa_manifest_path(ctx$manifest, ctx$outputs_dir, "series_table")
  if (is.null(p)) stop("manifest.series_table required")
  df <- .ecaa_load_tsv(p, list(
    time = c("time", "date", "timestamp", "t"),
    value = c("value", "y", "observation")
  ))
  if (is.null(df)) stop(sprintf("unparseable series table: %s", p))
  ecaa_decomposition_panel_r(df, title = "Series decomposition")
})

ecaa_register_figure("exploratory_analysis", "acf_pacf_panel", function(ctx) {
  p <- .ecaa_manifest_path(ctx$manifest, ctx$outputs_dir, "series_table")
  if (is.null(p)) stop("manifest.series_table required")
  df <- .ecaa_load_tsv(p, list(
    value = c("value", "y", "observation")
  ))
  if (is.null(df)) stop(sprintf("unparseable series table: %s", p))
  ecaa_acf_pacf_panel_r(df, title = "ACF + PACF")
})
