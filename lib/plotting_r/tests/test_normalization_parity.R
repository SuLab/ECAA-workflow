# R-side parity tests for the normalization stage renderer.
#
# Regression coverage for two bugs found when an R-backed (DESeq2 vst /
# edgeR TMM / scran) normalization task rendered ZERO figures in a live run:
#
#   1. lib/plotting_r/stages/normalization.R was missing entirely, so
#      core.R found no registered figures for the "normalization" stage
#      (Python had normalization.py; the R catalog did not — violating the
#      renderer-parity contract documented in emitter/mod.rs).
#   2. .ecaa_seed() parsed 8 hex digits via strtoi(), which overflows R's
#      signed-int max to NA for ~half of all (stage, figure) hashes, so
#      ecaa_generate()'s set.seed() crashed and killed every figure for the
#      affected stage. The existing tests called fn(ctx) directly and never
#      exercised ecaa_generate(), so the crash went unseen.
#
# TEST DISCIPLINE: authored to run in the R test lane once testthat +
# ggplot2 are present in the image (matches test_phase10_parity.R). The
# fix was validated out-of-band by rendering all three figures through
# ecaa_generate() in the bio-min:local container.

library(testthat)

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
.NORM_R    <- file.path(.REPO_ROOT, "lib", "plotting_r", "stages", "normalization.R")

if (!requireNamespace("ggplot2", quietly = TRUE)) {
  cat("[skip] ggplot2 not installed — test_normalization_parity.R skipped\n")
  quit(status = 0)
}
for (.f in c(.CORE_R, .SHARED_R, .NORM_R)) {
  if (!file.exists(.f)) {
    cat(sprintf("[skip] %s not found\n", basename(.f)))
    quit(status = 0)
  }
}

if (!exists("ecaa_savefig"))        source(.CORE_R,   local = FALSE)
if (!exists(".ecaa_manifest_path")) source(.SHARED_R, local = FALSE)
source(.NORM_R, local = FALSE)

# ---------------------------------------------------------------------------
# 1. Registry coverage — normalization registers the Python catalog's ids
# ---------------------------------------------------------------------------

test_that("normalization registers mean_variance, hvg_count_bar, sample_pca", {
  registered <- ecaa_known_figures("normalization")
  for (fig in c("mean_variance", "hvg_count_bar", "sample_pca")) {
    expect_true(fig %in% registered,
                label = sprintf("normalization::%s registered", fig))
  }
})

# ---------------------------------------------------------------------------
# 2. .ecaa_seed must always yield a valid, set.seed-able integer (Bug #2).
#    The old 8-hex-digit strtoi() returned NA for ~half of inputs.
# ---------------------------------------------------------------------------

test_that(".ecaa_seed never returns NA / always set.seed-able", {
  set.seed(7)
  bad <- 0L
  for (s in c("normalization", "differential_expression", "clustering",
              "quality_control", "pathway_enrichment", "peak_calling")) {
    for (f in c("mean_variance", "hvg_count_bar", "sample_pca", "volcano",
                "umap_clusters", "qc_summary_bar")) {
      for (k in 1:20) {
        seed <- .ecaa_seed(paste0(s, k), paste0(f, k))
        if (is.na(seed) || !is.finite(seed) || seed != as.integer(seed)) {
          bad <- bad + 1L
        } else {
          expect_no_error(set.seed(seed))
        }
      }
    }
  }
  expect_equal(bad, 0L)
})

# ---------------------------------------------------------------------------
# 3. End-to-end via ecaa_generate() — exercises set.seed + dispatch + render
#    (the path the live agent uses; the path the old tests skipped).
# ---------------------------------------------------------------------------

test_that("ecaa_generate renders all three normalization figures", {
  tmp <- tempfile("ecaa_norm_"); dir.create(tmp, recursive = TRUE)
  # mean_variance with the *_data suffix a real DESeq2 task emits.
  mv <- data.frame(feature = paste0("g", 1:120),
                   mean = exp(rnorm(120, 4, 1)),
                   variance = exp(rnorm(120, 5, 1)))
  utils::write.table(mv, file.path(tmp, "mean_variance_data.tsv"),
                     sep = "\t", quote = FALSE, row.names = FALSE)
  utils::write.table(data.frame(feature = paste0("g", 1:33)),
                     file.path(tmp, "hvg_list.tsv"),
                     sep = "\t", quote = FALSE, row.names = FALSE)
  m <- matrix(rnorm(120 * 4, 8, 2), nrow = 120,
              dimnames = list(paste0("g", 1:120),
                              c("ctrl1", "ctrl2", "kd1", "kd2")))
  mdf <- data.frame(gene = rownames(m), m, check.names = FALSE)
  utils::write.table(mdf, file.path(tmp, "normalized_counts.tsv"),
                     sep = "\t", quote = FALSE, row.names = FALSE)
  writeLines(jsonlite::toJSON(list(runs = list(list(id = "all", n_hvg = 33L))),
                              auto_unbox = TRUE),
             file.path(tmp, "manifest.json"))

  expect_no_error(ecaa_generate("normalization", tmp))
  for (fig in c("mean_variance", "hvg_count_bar", "sample_pca")) {
    expect_true(file.exists(file.path(tmp, "figures", paste0(fig, ".png"))),
                label = sprintf("%s.png produced via ecaa_generate", fig))
  }
})
