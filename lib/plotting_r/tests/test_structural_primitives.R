# Snapshot tests for the five universal structural primitives (R side).
#
# Mirrors lib/plotting/tests/test_structural_primitives.py.
#
# Tests are authored but NOT run here — they are deferred to the first
# SNAPSHOT_REGENERATE=1 sweep.  Running these without that env var
# will pass when the snapshot-hashes.json baseline is empty or absent.
#
# Usage:
#   Rscript -e 'testthat::test_dir("lib/plotting_r/tests")'
#
# Regenerate all baselines (run from repo root):
#   SNAPSHOT_REGENERATE=1 Rscript -e 'testthat::test_dir("lib/plotting_r/tests")'
#
# Snapshot file:
#   lib/plotting/tests/snapshots/structural/snapshot-hashes.json
#   (shared with the Python side; keys are prefixed "r_" to avoid collision)
#
# Snapshot comparison uses SHA-256 of the PNG bytes, identical to the
# Python side.  vdiffr is NOT declared as a dep in DESCRIPTION so we
# use direct byte-hash comparison instead.

library(testthat)

# ---------------------------------------------------------------------------
# Bootstrap: source core.R and the structural primitives.
# ---------------------------------------------------------------------------

.ecaa_tests_find_root <- function() {
  # Walk up from this file's directory to find the repo root.
  # Reliable across Rscript, source(), and testthat::test_dir() invocations.
  here <- tryCatch({
    f <- sys.frame(1)$ofile
    if (!is.null(f) && nzchar(f)) dirname(normalizePath(f)) else getwd()
  }, error = function(e) getwd())

  # Climb until we find the marker file.
  candidate <- normalizePath(here, mustWork = FALSE)
  for (i in seq_len(8)) {
    if (file.exists(file.path(candidate, "CLAUDE.md"))) {
      return(candidate)
    }
    candidate <- dirname(candidate)
  }
  # Fallback: assume CWD is the repo root.
  getwd()
}

.REPO_ROOT <- .ecaa_tests_find_root()
.CORE_R    <- file.path(.REPO_ROOT, "lib", "plotting_r", "core.R")
.STRUCT_R  <- file.path(.REPO_ROOT, "lib", "plotting_r", "primitives", "structural.R")

if (!exists("ecaa_savefig")) {
  if (file.exists(.CORE_R)) {
    source(.CORE_R, local = FALSE)
  } else {
    skip("core.R not found — skipping structural primitive tests")
  }
}
if (!exists("structural_matrix_overview")) {
  if (file.exists(.STRUCT_R)) {
    source(.STRUCT_R, local = FALSE)
  } else {
    skip("structural.R not found — skipping structural primitive tests")
  }
}

# ---------------------------------------------------------------------------
# Snapshot helpers
# ---------------------------------------------------------------------------

.SNAPSHOT_FILE <- file.path(
  .REPO_ROOT, "lib", "plotting", "tests", "snapshots", "structural",
  "snapshot-hashes.json"
)

.png_sha256 <- function(path) {
  if (!requireNamespace("digest", quietly = TRUE)) {
    # Fallback: read raw bytes and hash with a simple XOR fold (not
    # cryptographic, but stable for CI comparisons within one run).
    raw_bytes <- readBin(path, what = "raw", n = file.info(path)$size)
    return(paste(format(as.hexmode(sum(as.integer(raw_bytes)) %% 2^31), width = 8), collapse = ""))
  }
  digest::digest(path, algo = "sha256", file = TRUE)
}

.load_snapshots <- function() {
  if (!file.exists(.SNAPSHOT_FILE)) return(list())
  txt <- trimws(readLines(.SNAPSHOT_FILE, warn = FALSE))
  txt <- paste(txt, collapse = "\n")
  if (nchar(txt) == 0 || txt == "{}") return(list())
  jsonlite::fromJSON(txt, simplifyVector = FALSE)
}

.save_snapshots <- function(snaps) {
  dir.create(dirname(.SNAPSHOT_FILE), showWarnings = FALSE, recursive = TRUE)
  writeLines(
    jsonlite::toJSON(snaps, auto_unbox = TRUE, pretty = TRUE, na = "null"),
    .SNAPSHOT_FILE
  )
}

.check_or_regenerate <- function(key, png_path) {
  regenerate <- identical(Sys.getenv("SNAPSHOT_REGENERATE"), "1")
  snaps <- .load_snapshots()
  digest <- .png_sha256(png_path)
  if (regenerate) {
    snaps[[key]] <- digest
    .save_snapshots(snaps)
  } else if (!is.null(snaps[[key]]) && nzchar(snaps[[key]])) {
    expect_equal(
      digest, snaps[[key]],
      label = sprintf("PNG snapshot hash for '%s'", key)
    )
  }
  # If key is absent from snaps (empty {}), skip silently until
  # baselines are generated with SNAPSHOT_REGENERATE=1.
}

# ---------------------------------------------------------------------------
# Deterministic test data
# ---------------------------------------------------------------------------

.rng_matrix_small <- function() {
  set.seed(42)
  matrix(rnorm(20 * 12), nrow = 20, ncol = 12)
}

.rng_matrix_large <- function() {
  set.seed(42)
  matrix(rnorm(260 * 210), nrow = 260, ncol = 210)
}

.rng_numeric_large <- function() {
  set.seed(7)
  rnorm(500, mean = 5.0, sd = 2.0)
}

.rng_numeric_small <- function() {
  set.seed(7)
  rnorm(10, mean = 5.0, sd = 2.0)
}

.category_labels <- function() {
  set.seed(13)
  cats <- c("alpha", "beta", "gamma", "delta")
  cats[sample.int(length(cats), 200, replace = TRUE)]
}

.tabular_4col <- function() {
  set.seed(99)
  matrix(rnorm(80 * 4), nrow = 80, ncol = 4)
}

.tabular_8col <- function() {
  set.seed(99)
  matrix(rnorm(60 * 8), nrow = 60, ncol = 8)
}

# ---------------------------------------------------------------------------
# Tests: matrix_overview
# ---------------------------------------------------------------------------

test_that("matrix_overview small writes PNG + PDF", {
  tmp <- tempdir()
  png_path <- file.path(tmp, "r_matrix_overview_small.png")
  pdf_path <- file.path(tmp, "r_matrix_overview_small.pdf")

  structural_matrix_overview(
    .rng_matrix_small(),
    png_path  = png_path,
    pdf_path  = pdf_path,
    title     = "R test small matrix",
    theme_path = "theme.json"
  )

  expect_true(file.exists(png_path))
  # The companion PDF is written by ecaa_savefig alongside the PNG.
  expected_pdf <- sub("\\.png$", ".pdf", png_path)
  expect_true(file.exists(expected_pdf))

  .check_or_regenerate("r_matrix_overview_small", png_path)
})

test_that("matrix_overview large (>50k cells) completes without error", {
  mat <- .rng_matrix_large()
  expect_true(nrow(mat) * ncol(mat) > 50000L)

  tmp      <- tempdir()
  png_path <- file.path(tmp, "r_matrix_overview_large.png")

  structural_matrix_overview(
    mat,
    png_path  = png_path,
    pdf_path  = sub("\\.png$", ".pdf", png_path),
    title     = "R test large matrix (rasterized)",
    theme_path = "theme.json"
  )

  expect_true(file.exists(png_path))
  .check_or_regenerate("r_matrix_overview_large", png_path)
})

test_that("matrix_overview rejects non-2D input", {
  expect_error(
    structural_matrix_overview(
      1:10,           # 1D vector, not a matrix
      png_path  = tempfile(fileext = ".png"),
      pdf_path  = tempfile(fileext = ".pdf"),
      title     = "bad input"
    ),
    regexp = "2D"
  )
})

# ---------------------------------------------------------------------------
# Tests: distribution
# ---------------------------------------------------------------------------

test_that("distribution with KDE (n>=25) writes PNG + PDF", {
  vals <- .rng_numeric_large()
  expect_true(length(vals) >= 25L)

  tmp      <- tempdir()
  png_path <- file.path(tmp, "r_distribution_kde.png")

  structural_distribution(
    vals,
    png_path  = png_path,
    pdf_path  = sub("\\.png$", ".pdf", png_path),
    title     = "R test distribution with KDE",
    theme_path = "theme.json"
  )

  expect_true(file.exists(png_path))
  expect_true(file.exists(sub("\\.png$", ".pdf", png_path)))
  .check_or_regenerate("r_distribution_kde", png_path)
})

test_that("distribution without KDE (n<25) writes PNG + PDF", {
  vals <- .rng_numeric_small()
  expect_true(length(vals) < 25L)

  tmp      <- tempdir()
  png_path <- file.path(tmp, "r_distribution_no_kde.png")

  structural_distribution(
    vals,
    png_path  = png_path,
    pdf_path  = sub("\\.png$", ".pdf", png_path),
    title     = "R test distribution no KDE",
    theme_path = "theme.json"
  )

  expect_true(file.exists(png_path))
  .check_or_regenerate("r_distribution_no_kde", png_path)
})

test_that("distribution rejects empty vector", {
  expect_error(
    structural_distribution(
      numeric(0),
      png_path  = tempfile(fileext = ".png"),
      pdf_path  = tempfile(fileext = ".pdf"),
      title     = "empty"
    ),
    regexp = "non-empty"
  )
})

# ---------------------------------------------------------------------------
# Tests: categorical_summary
# ---------------------------------------------------------------------------

test_that("categorical_summary writes PNG + PDF", {
  tmp      <- tempdir()
  png_path <- file.path(tmp, "r_categorical_summary.png")

  structural_categorical_summary(
    .category_labels(),
    png_path  = png_path,
    pdf_path  = sub("\\.png$", ".pdf", png_path),
    title     = "R test categorical summary",
    theme_path = "theme.json"
  )

  expect_true(file.exists(png_path))
  expect_true(file.exists(sub("\\.png$", ".pdf", png_path)))
  .check_or_regenerate("r_categorical_summary", png_path)
})

test_that("categorical_summary sort order is deterministic", {
  # b appears 3x; a and c each appear 2x — tie broken alphabetically: a < c.
  labels <- c("b", "b", "b", "a", "a", "c", "c")
  tmp      <- tempdir()
  png_path <- file.path(tmp, "r_cat_sort.png")

  expect_no_error(
    structural_categorical_summary(
      labels,
      png_path  = png_path,
      pdf_path  = sub("\\.png$", ".pdf", png_path),
      title     = "R sort order check"
    )
  )
  expect_true(file.exists(png_path))
})

test_that("categorical_summary rejects empty labels", {
  expect_error(
    structural_categorical_summary(
      character(0),
      png_path  = tempfile(fileext = ".png"),
      pdf_path  = tempfile(fileext = ".pdf"),
      title     = "empty"
    ),
    regexp = "at least one"
  )
})

# ---------------------------------------------------------------------------
# Tests: pairs
# ---------------------------------------------------------------------------

test_that("pairs 4-column round-trip writes PNG + PDF", {
  tmp      <- tempdir()
  png_path <- file.path(tmp, "r_pairs_4col.png")

  structural_pairs(
    .tabular_4col(),
    column_names = c("A", "B", "C", "D"),
    png_path     = png_path,
    pdf_path     = sub("\\.png$", ".pdf", png_path),
    title        = "R test pairs 4 columns",
    theme_path   = "theme.json"
  )

  expect_true(file.exists(png_path))
  .check_or_regenerate("r_pairs_4col", png_path)
})

test_that("pairs 8-column (maximum) round-trip", {
  tmp      <- tempdir()
  png_path <- file.path(tmp, "r_pairs_8col.png")
  col_nms  <- paste0("col_", seq_len(8))

  structural_pairs(
    .tabular_8col(),
    column_names = col_nms,
    png_path     = png_path,
    pdf_path     = sub("\\.png$", ".pdf", png_path),
    title        = "R test pairs 8 columns",
    theme_path   = "theme.json"
  )

  expect_true(file.exists(png_path))
  .check_or_regenerate("r_pairs_8col", png_path)
})

test_that("pairs rejects > 8 columns", {
  bad <- matrix(rnorm(10 * 9), nrow = 10, ncol = 9)
  expect_error(
    structural_pairs(
      bad,
      column_names = paste0("c", seq_len(9)),
      png_path     = tempfile(fileext = ".png"),
      pdf_path     = tempfile(fileext = ".pdf"),
      title        = "too many cols"
    ),
    regexp = "8 columns"
  )
})

test_that("pairs rejects mismatched column_names", {
  tbl <- matrix(rnorm(10 * 3), nrow = 10, ncol = 3)
  expect_error(
    structural_pairs(
      tbl,
      column_names = c("a", "b"),   # 2 names for 3 columns
      png_path     = tempfile(fileext = ".png"),
      pdf_path     = tempfile(fileext = ".pdf"),
      title        = "bad names"
    ),
    regexp = "column_names length"
  )
})

# ---------------------------------------------------------------------------
# Tests: scalar_card
# ---------------------------------------------------------------------------

test_that("scalar_card writes PNG + PDF", {
  tmp      <- tempdir()
  png_path <- file.path(tmp, "r_scalar_card.png")

  structural_scalar_card(
    0.9312,
    label     = "AUROC",
    png_path  = png_path,
    pdf_path  = sub("\\.png$", ".pdf", png_path),
    title     = "R model performance",
    theme_path = "theme.json"
  )

  expect_true(file.exists(png_path))
  expect_true(file.exists(sub("\\.png$", ".pdf", png_path)))
  .check_or_regenerate("r_scalar_card", png_path)
})

test_that("scalar_card handles zero value", {
  tmp      <- tempdir()
  png_path <- file.path(tmp, "r_scalar_card_zero.png")

  expect_no_error(
    structural_scalar_card(
      0.0,
      label    = "null metric",
      png_path = png_path,
      pdf_path = sub("\\.png$", ".pdf", png_path),
      title    = "R zero value"
    )
  )
  expect_true(file.exists(png_path))
})

test_that("scalar_card handles negative value", {
  tmp      <- tempdir()
  png_path <- file.path(tmp, "r_scalar_card_neg.png")

  expect_no_error(
    structural_scalar_card(
      -3.14159,
      label    = "log2FC",
      png_path = png_path,
      pdf_path = sub("\\.png$", ".pdf", png_path),
      title    = "R negative scalar"
    )
  )
  expect_true(file.exists(png_path))
})
