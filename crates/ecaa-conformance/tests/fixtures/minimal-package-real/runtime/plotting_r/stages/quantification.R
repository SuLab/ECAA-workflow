# R-side quantification stage: peptide_coverage, ridgeline.
# Mirrors lib/plotting/stages/quantification.py.
#
# Plan reference: §S13.6 — proteomics quantification figures.

if (!exists("ecaa_register_figure")) {
  stop("source runtime/plotting_r/core.R before this stage module")
}

ecaa_register_figure("quantification", "peptide_coverage", function(ctx) {
  p <- .ecaa_manifest_path(ctx$manifest, ctx$outputs_dir, "coverage_table")
  if (is.null(p)) stop("manifest.coverage_table required")
  df <- .ecaa_load_tsv(p, list(
    position = c("position", "residue", "aa_pos"),
    coverage = c("coverage", "depth", "n_peptides")
  ))
  if (is.null(df)) stop(sprintf("unparseable coverage table: %s", p))
  ecaa_peptide_coverage_r(df, title = "Peptide coverage")
})

ecaa_register_figure("quantification", "ridgeline", function(ctx) {
  p <- .ecaa_manifest_path(ctx$manifest, ctx$outputs_dir, "intensity_table")
  if (is.null(p)) stop("manifest.intensity_table required")
  df <- .ecaa_load_tsv(p, list(
    group = c("group", "sample", "run", "channel"),
    value = c("value", "intensity", "log_intensity", "abundance")
  ))
  if (is.null(df)) stop(sprintf("unparseable intensity table: %s", p))
  ecaa_ridgeline_r(df, title = "Ion-intensity ridgeline")
})
