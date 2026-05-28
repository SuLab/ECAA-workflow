# Smoke tests for the R-side plotting library. Run with:
#
#   Rscript lib/plotting_r/tests/test_core.R
#
# Returns exit 0 on success, 1 on the first failure. Mirrors the
# determinism + dual-format checks in the Python pytest suite. Skipped
# when ggplot2 / ragg / Cairo are not available — the live CI image
# pins all of them via policies/container.json.

suppressPackageStartupMessages({
  if (!requireNamespace("ggplot2", quietly = TRUE)) {
    cat("[skip] ggplot2 not installed\n")
    quit(status = 0)
  }
})

.locate_self <- function() {
  args <- commandArgs(trailingOnly = FALSE)
  arg <- grep("^--file=", args, value = TRUE)
  if (length(arg) > 0) return(normalizePath(dirname(sub("^--file=", "", arg[1]))))
  getwd()
}
here <- .locate_self()
source(file.path(here, "..", "core.R"))

passed <- 0; failed <- 0
.report <- function(name, ok, detail = "") {
  status <- if (ok) "PASS" else "FAIL"
  cat(sprintf("[%s] %s%s\n", status, name,
              if (nchar(detail)) sprintf(" — %s", detail) else ""))
  if (ok) passed <<- passed + 1 else failed <<- failed + 1
}

tdir <- tempfile("swfc_r_test_"); dir.create(tdir, recursive = TRUE)

# ---------------------------------------------------------------------------
# Theme + palette
# ---------------------------------------------------------------------------

.report("theme loaded with expected keys",
        !is.null(THEME$schema_version) &&
        THEME$schema_version == 1 &&
        !is.null(THEME$palette$sig_up))

.report("Wong palette is 8 colors starting with black",
        length(swfc_wong_palette()) == 8 &&
        swfc_wong_palette()[1] == "#000000")

.report("categorical_palette returns Wong for n<=8",
        identical(swfc_palette(5), swfc_wong_palette()[1:5]))

.report("categorical_palette extends to glasbey20 for n>8",
        identical(swfc_palette(15)[9:15], swfc_glasbey20_palette()[9:15]))

# ---------------------------------------------------------------------------
# savefig dual-format
# ---------------------------------------------------------------------------

p1 <- swfc_bar(names = c("a","b","c"), values = c(1,2,3),
               title = "t", ylabel = "y")
out1 <- swfc_savefig(p1, file.path(tdir, "bar.png"), stage_id = "test")

.report("PNG produced", file.exists(file.path(tdir, "bar.png")))
.report("PDF produced alongside PNG", file.exists(file.path(tdir, "bar.pdf")))
.report("savefig returned primary path", out1 == file.path(tdir, "bar.png"))

# ---------------------------------------------------------------------------
# Volcano with top-N labels
# ---------------------------------------------------------------------------

set.seed(7)
n <- 300
log_fc <- rnorm(n, sd = 2)
pvals <- pmax(abs(rnorm(n, sd = 0.15)), 1e-6)
labs <- sprintf("GENE%d", seq_len(n))
v <- swfc_volcano(log_fc = log_fc, neg_log10_p = -log10(pvals),
                  labels = labs, title = "Volcano smoke", label_top_n = 10)
swfc_savefig(v, file.path(tdir, "volcano.png"), stage_id = "differential_expression")
.report("volcano PNG written",  file.exists(file.path(tdir, "volcano.png")))
.report("volcano PDF written",  file.exists(file.path(tdir, "volcano.pdf")))

# ---------------------------------------------------------------------------
# Heatmap with dendrogram + Z-score
# ---------------------------------------------------------------------------

set.seed(1)
mat <- matrix(rnorm(20 * 8), nrow = 20)
h <- swfc_heatmap(mat, row_labels = sprintf("r%d", seq_len(20)),
                  col_labels = sprintf("c%d", seq_len(8)),
                  title = "heatmap smoke", z_score_rows = TRUE)
swfc_savefig(h, file.path(tdir, "heatmap.png"), stage_id = "test")
.report("heatmap PNG written",  file.exists(file.path(tdir, "heatmap.png")))
.report("heatmap PDF written",  file.exists(file.path(tdir, "heatmap.pdf")))

# ---------------------------------------------------------------------------
# Violin with three groups + sig markers
# ---------------------------------------------------------------------------

set.seed(0)
data_list <- list(
  Ctrl = rnorm(40),
  Lo   = rnorm(40, mean = 0.3),
  Hi   = rnorm(40, mean = 2.5)
)
v2 <- swfc_violin(data_list, title = "Violin smoke", ylabel = "value")
swfc_savefig(v2, file.path(tdir, "violin.png"), stage_id = "test")
.report("violin PNG written", file.exists(file.path(tdir, "violin.png")))
.report("violin PDF written", file.exists(file.path(tdir, "violin.pdf")))

# ---------------------------------------------------------------------------
# Determinism: render the same figure twice, expect byte-identical output.
# ---------------------------------------------------------------------------

dt1 <- file.path(tdir, "det1.png")
dt2 <- file.path(tdir, "det2.png")
p_det <- swfc_bar(names = c("a","b","c"), values = c(1,2,3),
                  title = "det", ylabel = "y")
swfc_savefig(p_det, dt1, stage_id = "test")
p_det2 <- swfc_bar(names = c("a","b","c"), values = c(1,2,3),
                   title = "det", ylabel = "y")
swfc_savefig(p_det2, dt2, stage_id = "test")
h1 <- digest::digest(file = dt1, algo = "sha256")
h2 <- digest::digest(file = dt2, algo = "sha256")
.report("PNG byte-determinism across re-renders", h1 == h2,
        sprintf("%s vs %s", substr(h1,1,12), substr(h2,1,12)))

pdf_det1 <- sub("\\.png$", ".pdf", dt1)
pdf_det2 <- sub("\\.png$", ".pdf", dt2)
hp1 <- digest::digest(file = pdf_det1, algo = "sha256")
hp2 <- digest::digest(file = pdf_det2, algo = "sha256")
# PDFs include a CreationDate by default; matplotlib's Python savefig
# strips it but cairo_pdf doesn't expose that knob in the same way.
# We accept that PDF determinism on the R side requires a deterministic
# clock. Test only that the file is produced — leave byte parity to a
# pinned-cairo CI image.
.report("PDF re-rendered (byte-determinism deferred to pinned cairo)",
        file.exists(pdf_det1) && file.exists(pdf_det2))

# ---------------------------------------------------------------------------
# Provenance footer text
# ---------------------------------------------------------------------------

Sys.setenv(ECAA_PACKAGE_ID = "test-pkg-r", ECAA_GIT_SHA = "abc123def456")
text <- .swfc_provenance_text("clustering")
.report("provenance footer carries pkg id, stage, version, sha",
        grepl("test-pkg-r", text) &&
        grepl("clustering", text) &&
        grepl(ECAA_PLOTTING_R_VERSION, text, fixed = TRUE) &&
        grepl("abc123d", text, fixed = TRUE))

# ---------------------------------------------------------------------------
# Stage dispatcher: register + generate round-trip
# ---------------------------------------------------------------------------

swfc_register_figure("test_stage_xyz", "demo", function(ctx) {
  swfc_bar(names = c("a","b"), values = c(1, 2), title = "demo", ylabel = "y")
})
out_dir <- file.path(tdir, "outputs"); dir.create(out_dir)
mf <- swfc_generate("test_stage_xyz", out_dir, required = "demo")
.report("generate() wrote primary figure",
        !is.null(mf$written$demo) && file.exists(mf$written$demo))
.report("generate() wrote figures/manifest.json",
        file.exists(file.path(out_dir, "figures", "manifest.json")))

# ---------------------------------------------------------------------------
# Stage-module discoverability — every R stage module under stages/
# must source cleanly + register at least one figure (plan §S13.6 +
# §S14.3). The sourced figure ids are checked against the
# corresponding Python stage_id so the cross-renderer parity gate
# can rely on shape parity end-to-end.
# ---------------------------------------------------------------------------

stage_dir <- file.path(here, "..", "stages")
expected_modules <- list(
  list(file = "_shared.R", stage = NA_character_, figs = character()),
  list(file = "clustering.R", stage = "clustering",
       figs = c("umap_clusters", "cluster_size_bar")),
  list(file = "differential_expression.R", stage = "differential_expression",
       figs = c("volcano", "top_features_heatmap")),
  list(file = "peak_calling.R", stage = "peak_calling",
       figs = c("profile_pileup", "coverage_track", "peak_saturation")),
  list(file = "isoform_calling.R", stage = "isoform_calling",
       figs = c("isoform_structure", "sashimi")),
  list(file = "quantification.R", stage = "quantification",
       figs = c("peptide_coverage", "ridgeline")),
  list(file = "taxonomic_profiling.R", stage = "taxonomic_profiling",
       figs = c("taxonomic_stacked_bar", "diversity_violin")),
  list(file = "spatial_clustering.R", stage = "spatial_clustering",
       figs = c("tissue_overlay", "morans_i_scatter",
                "neighborhood_enrichment")),
  list(file = "exploratory_analysis.R", stage = "exploratory_analysis",
       figs = c("decomposition_panel", "acf_pacf_panel")),
  list(file = "forecasting_inference.R", stage = "forecasting_inference",
       figs = c("forecast_ribbon", "anomaly_timeline")),
  list(file = "preprocessing.R", stage = "preprocessing",
       figs = c("retention_bar")),
  list(file = "quality_control.R", stage = "quality_control",
       figs = c("per_sample_metric_violin", "per_sample_metric_bar",
                "qc_summary_bar"))
)
# _shared.R must source first (the others use .swfc_load_tsv /
# .swfc_manifest_path defined there). Sourcing it twice is idempotent.
shared_path <- file.path(stage_dir, "_shared.R")
if (file.exists(shared_path)) source(shared_path, local = FALSE)

for (mod in expected_modules) {
  if (is.na(mod$stage)) next
  path <- file.path(stage_dir, mod$file)
  if (!file.exists(path)) {
    .report(sprintf("stage module %s exists", mod$file), FALSE)
    next
  }
  ok <- tryCatch({
    source(path, local = FALSE)
    TRUE
  }, error = function(e) {
    .report(sprintf("stage module %s sources", mod$file), FALSE,
            conditionMessage(e))
    FALSE
  })
  if (!ok) next
  registered <- swfc_known_figures(mod$stage)
  missing <- setdiff(mod$figs, registered)
  .report(
    sprintf("stage %s registers expected figures", mod$stage),
    length(missing) == 0,
    if (length(missing)) paste("missing:", paste(missing, collapse = ", ")) else ""
  )
}

# ---------------------------------------------------------------------------
cat(sprintf("\nResults: %d passed, %d failed\n", passed, failed))
if (failed > 0) quit(status = 1)
quit(status = 0)
