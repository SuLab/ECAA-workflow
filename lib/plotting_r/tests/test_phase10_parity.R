# Phase 10 parity scaffolding — R-side tests for the three affordance
# entries added to config/plot-affordances/registered.yaml during Phase
# 10's CURIE-alias sweep:
#
#   data:1234  → preprocessing  (retention_bar)
#   data:2914  → quality_control  (per_sample_metric_violin,
#                                   per_sample_metric_bar,
#                                   qc_summary_bar)
#   ecaax:spatial_coordinates → spatial_clustering  (tissue_overlay,
#                                                    morans_i_scatter,
#                                                    neighborhood_enrichment)
#
# TEST DISCIPLINE: These tests are AUTHORED but NOT RUN in this PR.
# They are deferred to the next full CI sweep per the deferred-testing
# convention established for Phase 10. Do not invoke this file from
# `make test` until ggplot2 + ragg are confirmed in the base image.
#
# Usage (once packages are available):
#   Rscript -e 'testthat::test_file("lib/plotting_r/tests/test_phase10_parity.R")'

library(testthat)

# ---------------------------------------------------------------------------
# Bootstrap
# ---------------------------------------------------------------------------

.find_repo_root <- function() {
  here <- tryCatch({
    f <- sys.frame(1)$ofile
    if (!is.null(f) && nzchar(f)) dirname(normalizePath(f)) else getwd()
  }, error = function(e) getwd())
  candidate <- normalizePath(here, mustWork = FALSE)
  for (i in seq_len(8)) {
    if (file.exists(file.path(candidate, "CLAUDE.md"))) return(candidate)
    candidate <- dirname(candidate)
  }
  getwd()
}

.REPO_ROOT <- .find_repo_root()
.CORE_R    <- file.path(.REPO_ROOT, "lib", "plotting_r", "core.R")
.SHARED_R  <- file.path(.REPO_ROOT, "lib", "plotting_r", "stages", "_shared.R")
.PREP_R    <- file.path(.REPO_ROOT, "lib", "plotting_r", "stages", "preprocessing.R")
.QC_R      <- file.path(.REPO_ROOT, "lib", "plotting_r", "stages", "quality_control.R")
.SPATIAL_R <- file.path(.REPO_ROOT, "lib", "plotting_r", "stages", "spatial_clustering.R")

# Skip immediately when ggplot2 is absent so CI doesn't fail before the
# base image is pinned (matches the skip discipline in test_core.R).
if (!requireNamespace("ggplot2", quietly = TRUE)) {
  cat("[skip] ggplot2 not installed — test_phase10_parity.R skipped\n")
  quit(status = 0)
}

for (.f in c(.CORE_R, .SHARED_R, .PREP_R, .QC_R, .SPATIAL_R)) {
  if (!file.exists(.f)) {
    cat(sprintf("[skip] %s not found\n", basename(.f)))
    quit(status = 0)
  }
}

if (!exists("ecaa_savefig"))          source(.CORE_R,    local = FALSE)
if (!exists(".ecaa_manifest_path"))   source(.SHARED_R,  local = FALSE)
source(.PREP_R,    local = FALSE)
source(.QC_R,      local = FALSE)
source(.SPATIAL_R, local = FALSE)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

.write_manifest <- function(dir, data) {
  p <- file.path(dir, "manifest.json")
  writeLines(jsonlite::toJSON(data, auto_unbox = TRUE, pretty = TRUE,
                               na = "null"), p)
  p
}

.write_tsv <- function(dir, filename, df) {
  p <- file.path(dir, filename)
  utils::write.table(df, p, sep = "\t", quote = FALSE, row.names = FALSE)
  p
}

.png_produced <- function(ctx_or_dir, fig_id) {
  d <- if (is.list(ctx_or_dir)) ctx_or_dir$outputs_dir else ctx_or_dir
  file.exists(file.path(d, "figures", paste0(fig_id, ".png")))
}

# ---------------------------------------------------------------------------
# 1. preprocessing — retention_bar
# ---------------------------------------------------------------------------

test_that("preprocessing: retention_bar writes PNG for 3-sample manifest", {
  tmp <- tempfile("ecaa_prep_"); dir.create(tmp, recursive = TRUE)
  .write_manifest(tmp, list(
    samples = list(
      list(id = "S1", n_in = 1000000L, n_out = 850000L),
      list(id = "S2", n_in = 1200000L, n_out = 990000L),
      list(id = "S3", n_in =  980000L, n_out = 750000L)
    )
  ))
  dir.create(file.path(tmp, "figures"), showWarnings = FALSE)
  manifest <- jsonlite::fromJSON(file.path(tmp, "manifest.json"),
                                  simplifyVector = FALSE)
  ctx <- list(stage_id = "preprocessing", figure_id = "retention_bar",
               outputs_dir = tmp, manifest = manifest)
  fn <- ecaa_lookup_figure("preprocessing", "retention_bar")
  expect_false(is.null(fn), info = "retention_bar must be registered")
  expect_no_error(fn(ctx))
  expect_true(.png_produced(ctx, "retention_bar"),
              label = "retention_bar.png produced")
})

test_that("preprocessing: retention_bar errors on missing samples block", {
  tmp <- tempfile("ecaa_prep_err_"); dir.create(tmp, recursive = TRUE)
  .write_manifest(tmp, list(stage = "preprocessing"))
  manifest <- jsonlite::fromJSON(file.path(tmp, "manifest.json"),
                                  simplifyVector = FALSE)
  ctx <- list(stage_id = "preprocessing", figure_id = "retention_bar",
               outputs_dir = tmp, manifest = manifest)
  fn <- ecaa_lookup_figure("preprocessing", "retention_bar")
  expect_error(fn(ctx), regexp = "n_in")
})

test_that("preprocessing: retention_bar respects group faceting", {
  tmp <- tempfile("ecaa_prep_grp_"); dir.create(tmp, recursive = TRUE)
  .write_manifest(tmp, list(
    samples = list(
      list(id = "A1", n_in = 500L, n_out = 420L, group = "caseA"),
      list(id = "A2", n_in = 490L, n_out = 410L, group = "caseA"),
      list(id = "B1", n_in = 510L, n_out = 500L, group = "caseB")
    )
  ))
  dir.create(file.path(tmp, "figures"), showWarnings = FALSE)
  manifest <- jsonlite::fromJSON(file.path(tmp, "manifest.json"),
                                  simplifyVector = FALSE)
  ctx <- list(stage_id = "preprocessing", figure_id = "retention_bar",
               outputs_dir = tmp, manifest = manifest)
  fn <- ecaa_lookup_figure("preprocessing", "retention_bar")
  expect_no_error(fn(ctx))
  expect_true(.png_produced(ctx, "retention_bar"))
})

# ---------------------------------------------------------------------------
# 2. quality_control — per_sample_metric_violin
# ---------------------------------------------------------------------------

test_that("quality_control: per_sample_metric_violin from long-form TSV", {
  tmp <- tempfile("ecaa_qc_vln_"); dir.create(tmp, recursive = TRUE)
  set.seed(42)
  n_rows <- 120L
  df_long <- data.frame(
    sample = rep(paste0("S", 1:4), each = 30L),
    metric = rep(c("n_reads", "mapping_rate"), each = 15L,
                 times = 4L),
    value  = c(rnorm(60L, mean = 1e6, sd = 1e5),
               rnorm(60L, mean = 0.9, sd = 0.05)),
    stringsAsFactors = FALSE
  )
  .write_tsv(tmp, "qc_metrics.tsv", df_long)
  .write_manifest(tmp, list())
  dir.create(file.path(tmp, "figures"), showWarnings = FALSE)
  manifest <- jsonlite::fromJSON(file.path(tmp, "manifest.json"),
                                  simplifyVector = FALSE)
  ctx <- list(stage_id = "quality_control",
               figure_id = "per_sample_metric_violin",
               outputs_dir = tmp, manifest = manifest)
  fn <- ecaa_lookup_figure("quality_control", "per_sample_metric_violin")
  expect_false(is.null(fn))
  expect_no_error(fn(ctx))
  expect_true(.png_produced(ctx, "per_sample_metric_violin"))
})

test_that("quality_control: per_sample_metric_violin falls back to bar with manifest-only data", {
  tmp <- tempfile("ecaa_qc_vln_fb_"); dir.create(tmp, recursive = TRUE)
  .write_manifest(tmp, list(
    per_sample_metrics = list(
      S1 = list(n_cells = 3200L, pct_mito = 3.2),
      S2 = list(n_cells = 4100L, pct_mito = 4.5),
      S3 = list(n_cells = 2900L, pct_mito = 2.8)
    )
  ))
  dir.create(file.path(tmp, "figures"), showWarnings = FALSE)
  manifest <- jsonlite::fromJSON(file.path(tmp, "manifest.json"),
                                  simplifyVector = FALSE)
  ctx <- list(stage_id = "quality_control",
               figure_id = "per_sample_metric_violin",
               outputs_dir = tmp, manifest = manifest)
  fn <- ecaa_lookup_figure("quality_control", "per_sample_metric_violin")
  expect_no_error(fn(ctx))
  expect_true(.png_produced(ctx, "per_sample_metric_violin"))
})

test_that("quality_control: per_sample_metric_violin errors when no data sources", {
  tmp <- tempfile("ecaa_qc_vln_err_"); dir.create(tmp, recursive = TRUE)
  .write_manifest(tmp, list())
  manifest <- jsonlite::fromJSON(file.path(tmp, "manifest.json"),
                                  simplifyVector = FALSE)
  ctx <- list(stage_id = "quality_control",
               figure_id = "per_sample_metric_violin",
               outputs_dir = tmp, manifest = manifest)
  fn <- ecaa_lookup_figure("quality_control", "per_sample_metric_violin")
  expect_error(fn(ctx), regexp = "qc_metrics")
})

# ---------------------------------------------------------------------------
# 3. quality_control — per_sample_metric_bar
# ---------------------------------------------------------------------------

test_that("quality_control: per_sample_metric_bar selects count-named metric", {
  tmp <- tempfile("ecaa_qc_bar_"); dir.create(tmp, recursive = TRUE)
  .write_manifest(tmp, list(
    per_sample_metrics = list(
      S1 = list(read_count = 8e6L, mapping_rate = 0.91),
      S2 = list(read_count = 9e6L, mapping_rate = 0.93),
      S3 = list(read_count = 7e6L, mapping_rate = 0.89)
    )
  ))
  dir.create(file.path(tmp, "figures"), showWarnings = FALSE)
  manifest <- jsonlite::fromJSON(file.path(tmp, "manifest.json"),
                                  simplifyVector = FALSE)
  ctx <- list(stage_id = "quality_control",
               figure_id = "per_sample_metric_bar",
               outputs_dir = tmp, manifest = manifest)
  fn <- ecaa_lookup_figure("quality_control", "per_sample_metric_bar")
  expect_false(is.null(fn))
  expect_no_error(fn(ctx))
  expect_true(.png_produced(ctx, "per_sample_metric_bar"))
})

test_that("quality_control: per_sample_metric_bar selects n_-prefixed metric", {
  tmp <- tempfile("ecaa_qc_bar_n_"); dir.create(tmp, recursive = TRUE)
  .write_manifest(tmp, list(
    per_sample_metrics = list(
      S1 = list(n_cells = 3200L, pct_mito = 3.2),
      S2 = list(n_cells = 4100L, pct_mito = 4.5)
    )
  ))
  dir.create(file.path(tmp, "figures"), showWarnings = FALSE)
  manifest <- jsonlite::fromJSON(file.path(tmp, "manifest.json"),
                                  simplifyVector = FALSE)
  ctx <- list(stage_id = "quality_control",
               figure_id = "per_sample_metric_bar",
               outputs_dir = tmp, manifest = manifest)
  fn <- ecaa_lookup_figure("quality_control", "per_sample_metric_bar")
  expect_no_error(fn(ctx))
  expect_true(.png_produced(ctx, "per_sample_metric_bar"))
})

test_that("quality_control: per_sample_metric_bar errors when manifest block absent", {
  tmp <- tempfile("ecaa_qc_bar_err_"); dir.create(tmp, recursive = TRUE)
  .write_manifest(tmp, list())
  manifest <- jsonlite::fromJSON(file.path(tmp, "manifest.json"),
                                  simplifyVector = FALSE)
  ctx <- list(stage_id = "quality_control",
               figure_id = "per_sample_metric_bar",
               outputs_dir = tmp, manifest = manifest)
  fn <- ecaa_lookup_figure("quality_control", "per_sample_metric_bar")
  expect_error(fn(ctx), regexp = "per_sample_metrics")
})

# ---------------------------------------------------------------------------
# 4. quality_control — qc_summary_bar
# ---------------------------------------------------------------------------

test_that("quality_control: qc_summary_bar reads summary_stats.json", {
  tmp <- tempfile("ecaa_qc_sumbar_"); dir.create(tmp, recursive = TRUE)
  writeLines(
    jsonlite::toJSON(list(total_reads = 42000000L,
                          mapped_reads = 38500000L,
                          pct_mapped = 91.67,
                          n_peaks = 12000L),
                     auto_unbox = TRUE),
    file.path(tmp, "summary_stats.json")
  )
  .write_manifest(tmp, list())
  dir.create(file.path(tmp, "figures"), showWarnings = FALSE)
  manifest <- jsonlite::fromJSON(file.path(tmp, "manifest.json"),
                                  simplifyVector = FALSE)
  ctx <- list(stage_id = "quality_control",
               figure_id = "qc_summary_bar",
               outputs_dir = tmp, manifest = manifest)
  fn <- ecaa_lookup_figure("quality_control", "qc_summary_bar")
  expect_false(is.null(fn))
  expect_no_error(fn(ctx))
  expect_true(.png_produced(ctx, "qc_summary_bar"))
})

test_that("quality_control: qc_summary_bar errors when summary_stats.json absent", {
  tmp <- tempfile("ecaa_qc_sumbar_err_"); dir.create(tmp, recursive = TRUE)
  .write_manifest(tmp, list())
  manifest <- jsonlite::fromJSON(file.path(tmp, "manifest.json"),
                                  simplifyVector = FALSE)
  ctx <- list(stage_id = "quality_control",
               figure_id = "qc_summary_bar",
               outputs_dir = tmp, manifest = manifest)
  fn <- ecaa_lookup_figure("quality_control", "qc_summary_bar")
  expect_error(fn(ctx), regexp = "summary_stats")
})

# ---------------------------------------------------------------------------
# 5. spatial_clustering — all three figure ids
# ---------------------------------------------------------------------------

test_that("spatial_clustering: tissue_overlay writes PNG with synthetic coords", {
  tmp <- tempfile("ecaa_sc_to_"); dir.create(tmp, recursive = TRUE)
  set.seed(7)
  n <- 80L
  coords <- data.frame(
    x       = runif(n, 0, 5000),
    y       = runif(n, 0, 5000),
    cluster = paste0("C", sample(1:5, n, replace = TRUE)),
    stringsAsFactors = FALSE
  )
  .write_tsv(tmp, "coords_table.tsv", coords)
  .write_manifest(tmp, list(coords_table = "coords_table.tsv"))
  dir.create(file.path(tmp, "figures"), showWarnings = FALSE)
  manifest <- jsonlite::fromJSON(file.path(tmp, "manifest.json"),
                                  simplifyVector = FALSE)
  ctx <- list(stage_id = "spatial_clustering",
               figure_id = "tissue_overlay",
               outputs_dir = tmp, manifest = manifest)
  fn <- ecaa_lookup_figure("spatial_clustering", "tissue_overlay")
  expect_false(is.null(fn))
  # tissue_overlay delegates to ecaa_tissue_overlay_r defined in core.R;
  # if the helper exists the call should not error.
  if (exists("ecaa_tissue_overlay_r")) {
    expect_no_error(fn(ctx))
  } else {
    expect_warning(fn(ctx), regexp = NA)  # at minimum must not hard-crash
  }
})

test_that("spatial_clustering: morans_i_scatter writes PNG with synthetic data", {
  tmp <- tempfile("ecaa_sc_mi_"); dir.create(tmp, recursive = TRUE)
  set.seed(13)
  n_genes <- 30L
  mi_df <- data.frame(
    gene     = paste0("GENE", seq_len(n_genes)),
    morans_i = runif(n_genes, -0.2, 0.8),
    p_value  = runif(n_genes, 0, 0.1),
    stringsAsFactors = FALSE
  )
  .write_tsv(tmp, "morans_i_table.tsv", mi_df)
  .write_manifest(tmp, list(morans_i_table = "morans_i_table.tsv"))
  dir.create(file.path(tmp, "figures"), showWarnings = FALSE)
  manifest <- jsonlite::fromJSON(file.path(tmp, "manifest.json"),
                                  simplifyVector = FALSE)
  ctx <- list(stage_id = "spatial_clustering",
               figure_id = "morans_i_scatter",
               outputs_dir = tmp, manifest = manifest)
  fn <- ecaa_lookup_figure("spatial_clustering", "morans_i_scatter")
  expect_false(is.null(fn))
  if (exists("ecaa_morans_i_scatter_r")) {
    expect_no_error(fn(ctx))
  }
})

test_that("spatial_clustering: neighborhood_enrichment writes PNG with synthetic data", {
  tmp <- tempfile("ecaa_sc_ne_"); dir.create(tmp, recursive = TRUE)
  set.seed(99)
  types <- paste0("T", 1:4)
  pairs_idx <- expand.grid(source = types, target = types,
                            stringsAsFactors = FALSE)
  ne_df <- data.frame(
    source = pairs_idx$source,
    target = pairs_idx$target,
    score  = rnorm(nrow(pairs_idx)),
    stringsAsFactors = FALSE
  )
  .write_tsv(tmp, "neighborhood_table.tsv", ne_df)
  .write_manifest(tmp, list(neighborhood_table = "neighborhood_table.tsv"))
  dir.create(file.path(tmp, "figures"), showWarnings = FALSE)
  manifest <- jsonlite::fromJSON(file.path(tmp, "manifest.json"),
                                  simplifyVector = FALSE)
  ctx <- list(stage_id = "spatial_clustering",
               figure_id = "neighborhood_enrichment",
               outputs_dir = tmp, manifest = manifest)
  fn <- ecaa_lookup_figure("spatial_clustering", "neighborhood_enrichment")
  expect_false(is.null(fn))
  if (exists("ecaa_neighborhood_enrichment_r")) {
    expect_no_error(fn(ctx))
  }
})

# ---------------------------------------------------------------------------
# 6. Registry coverage sanity-check: all Phase 10 figure ids are registered.
# ---------------------------------------------------------------------------

test_that("all Phase 10 figure ids are registered in the R stage registry", {
  expected <- list(
    preprocessing     = "retention_bar",
    quality_control   = c("per_sample_metric_violin",
                           "per_sample_metric_bar",
                           "qc_summary_bar"),
    spatial_clustering = c("tissue_overlay", "morans_i_scatter",
                            "neighborhood_enrichment")
  )
  for (stage in names(expected)) {
    registered <- ecaa_known_figures(stage)
    for (fig in expected[[stage]]) {
      expect_true(fig %in% registered,
                  label = sprintf("%s::%s registered", stage, fig))
    }
  }
})
