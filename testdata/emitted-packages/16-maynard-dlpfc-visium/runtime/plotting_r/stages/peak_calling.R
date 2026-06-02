# R-side peak_calling stage: profile_pileup, coverage_track,
# peak_saturation. Mirrors lib/plotting/stages/peak_calling.py.
#
# Plan reference: §S13.6 — R parity for chip-seq + atac-seq peak
# calling figures. Each figure resolves a manifest table path and
# delegates to the matching `ecaa_*_r` primitive in core.R.

if (!exists("ecaa_register_figure")) {
  stop("source runtime/plotting_r/core.R before this stage module")
}

ecaa_register_figure("peak_calling", "profile_pileup", function(ctx) {
  p <- .ecaa_manifest_path(ctx$manifest, ctx$outputs_dir, "profile_table")
  if (is.null(p)) stop("manifest.profile_table required")
  df <- .ecaa_load_tsv(p, list(
    position = c("position", "distance", "offset"),
    signal = c("signal", "coverage", "rpkm", "fold_enrichment"),
    `group?` = c("group", "antibody", "sample", "condition")
  ))
  if (is.null(df)) stop(sprintf("unparseable profile table: %s", p))
  ecaa_profile_pileup_r(df, title = "Profile pileup",
                        group_col = if ("group" %in% colnames(df)) "group" else NULL)
})

ecaa_register_figure("peak_calling", "coverage_track", function(ctx) {
  p <- .ecaa_manifest_path(ctx$manifest, ctx$outputs_dir, "coverage_table")
  if (is.null(p)) stop("manifest.coverage_table required")
  df <- .ecaa_load_tsv(p, list(
    chrom = c("chrom", "chromosome", "#chrom"),
    pos = c("pos", "position"),
    depth = c("depth", "coverage", "n_reads")
  ))
  if (is.null(df)) stop(sprintf("unparseable coverage table: %s", p))
  ecaa_coverage_track_r(df, title = "Coverage track")
})

ecaa_register_figure("peak_calling", "peak_saturation", function(ctx) {
  p <- .ecaa_manifest_path(ctx$manifest, ctx$outputs_dir, "saturation_table")
  if (is.null(p)) stop("manifest.saturation_table required")
  df <- .ecaa_load_tsv(p, list(
    depth = c("depth", "subsampled_reads", "n_reads"),
    peaks_called = c("peaks_called", "n_peaks", "peaks"),
    `group?` = c("group", "sample", "replicate")
  ))
  if (is.null(df)) stop(sprintf("unparseable saturation table: %s", p))
  ecaa_peak_saturation_r(df, title = "Peak saturation",
                         group_col = if ("group" %in% colnames(df)) "group" else NULL)
})
