# R-side exploratory_analysis stage: decomposition_panel,
# acf_pacf_panel. Mirrors lib/plotting/stages/exploratory_analysis.py.
#
# Plan reference: §S14.3 — time-series-forecast exploratory figures.

if (!exists("swfc_register_figure")) {
  stop("source runtime/plotting_r/core.R before this stage module")
}

swfc_register_figure("exploratory_analysis", "decomposition_panel",
                     function(ctx) {
  p <- .swfc_manifest_path(ctx$manifest, ctx$outputs_dir, "series_table")
  if (is.null(p)) stop("manifest.series_table required")
  df <- .swfc_load_tsv(p, list(
    time = c("time", "date", "timestamp", "t"),
    value = c("value", "y", "observation")
  ))
  if (is.null(df)) stop(sprintf("unparseable series table: %s", p))
  swfc_decomposition_panel_r(df, title = "Series decomposition")
})

swfc_register_figure("exploratory_analysis", "acf_pacf_panel", function(ctx) {
  p <- .swfc_manifest_path(ctx$manifest, ctx$outputs_dir, "series_table")
  if (is.null(p)) stop("manifest.series_table required")
  df <- .swfc_load_tsv(p, list(
    value = c("value", "y", "observation")
  ))
  if (is.null(df)) stop(sprintf("unparseable series table: %s", p))
  swfc_acf_pacf_panel_r(df, title = "ACF + PACF")
})
