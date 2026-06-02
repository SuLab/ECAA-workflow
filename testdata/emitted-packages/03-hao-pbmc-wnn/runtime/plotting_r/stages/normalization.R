# R-side normalization stage: mean_variance + hvg_count_bar + sample_pca.
# Mirrors lib/plotting/stages/normalization.py — registers the SAME three
# figure_ids so the validator's figures-present check is renderer-agnostic
# whether the agent normalized in R (DESeq2 vst / edgeR TMM / scran) or in
# Python (pydeseq2 / scanpy). Without this module, an R-backed normalization
# task renders zero figures because core.R finds no registered stage.

if (!exists("ecaa_register_figure")) {
  stop("source runtime/plotting_r/core.R before this stage module")
}

# Locate the first file matching any of `patterns` (case-insensitive,
# recursive) under outputs_dir. Filename-robust: agents emit either
# `mean_variance.tsv` or `mean_variance_data.tsv`, `normalized_counts.tsv`
# or `vst_matrix.tsv`, etc. — match the family, not one exact name.
.norm_find_file <- function(outputs_dir, patterns) {
  for (pat in patterns) {
    hits <- list.files(outputs_dir, pattern = pat, recursive = TRUE,
                       full.names = TRUE, ignore.case = TRUE)
    if (length(hits)) return(hits[[1]])
  }
  NULL
}

# Returns list(mean, variance, log_scale). DESeq2/pydeseq2 emit columns
# named log2_mean / log2_variance (already log-transformed); scran/edgeR
# emit raw mean / variance. We match either family and remember whether the
# values are already on a log scale so the figure doesn't double-log them
# (log1p of an already-log2 negative is NaN).
.norm_load_mean_variance <- function(ctx) {
  p <- .norm_find_file(ctx$outputs_dir, c("mean[_-]?variance.*\\.tsv(\\.gz)?$"))
  if (is.null(p)) return(NULL)
  df <- tryCatch(utils::read.delim(p, stringsAsFactors = FALSE,
                                   check.names = FALSE),
                 error = function(e) NULL)
  if (is.null(df) || nrow(df) == 0L) return(NULL)
  cl <- tolower(colnames(df))
  mean_aliases <- c("mean", "means", "mu", "basemean", "base_mean",
                    "log2_mean", "log_mean", "logmean", "gene_mean",
                    "mean_log2")
  var_aliases <- c("variance", "var", "dispersion", "disp",
                   "log2_variance", "log_variance", "gene_var",
                   "variance_log2", "var_log2")
  mi <- which(cl %in% mean_aliases)
  vi <- which(cl %in% var_aliases)
  if (!length(mi) || !length(vi)) return(NULL)
  mean <- suppressWarnings(as.numeric(df[[mi[1]]]))
  variance <- suppressWarnings(as.numeric(df[[vi[1]]]))
  log_scale <- grepl("log", cl[mi[1]]) || grepl("log", cl[vi[1]])
  ok <- is.finite(mean) & is.finite(variance)
  if (!log_scale) ok <- ok & mean >= 0 & variance >= 0
  if (!any(ok)) return(NULL)
  list(mean = mean[ok], variance = variance[ok], log_scale = log_scale)
}

# HVG counts: prefer manifest.runs[].n_hvg; fall back to counting
# hvg_list.tsv rows, then a top-level manifest.n_hvg scalar.
.norm_n_hvg <- function(ctx) {
  m <- ctx$manifest
  runs <- m$runs %||% m$compartments
  names <- character(0); values <- numeric(0)
  if (is.list(runs) && length(runs)) {
    for (r in runs) {
      n <- r$n_hvg %||% r$n_highly_variable_features
      if (!is.null(n) && is.numeric(n)) {
        names <- c(names, as.character(r$id %||% "run"))
        values <- c(values, as.numeric(n))
      }
    }
  }
  if (!length(names)) {
    p <- .norm_find_file(ctx$outputs_dir, c("hvg[_-]?list.*\\.tsv(\\.gz)?$"))
    if (!is.null(p)) {
      df <- tryCatch(utils::read.delim(p, stringsAsFactors = FALSE,
                                       check.names = FALSE),
                     error = function(e) NULL)
      if (!is.null(df) && nrow(df) > 0L) {
        names <- "run"; values <- nrow(df)
      }
    }
  }
  if (!length(names)) {
    n <- m$n_hvg %||% m$n_highly_variable_features %||% m$hvg_count
    if (!is.null(n) && is.numeric(n)) { names <- "run"; values <- as.numeric(n) }
  }
  if (!length(names)) return(NULL)
  list(names = names, values = values)
}

# Normalized expression matrix: features in the first column, samples in
# the remaining columns. Read without row.names to tolerate duplicate
# feature ids, then coerce the sample columns to a numeric matrix.
.norm_load_matrix <- function(ctx) {
  p <- .norm_find_file(ctx$outputs_dir, c(
    "vst[_-]?matrix.*\\.tsv(\\.gz)?$",
    "normali[sz]ed[_-]?counts.*\\.tsv(\\.gz)?$",
    "logcpm[_-]?matrix.*\\.tsv(\\.gz)?$",
    "log2?[_-]?cpm.*\\.tsv(\\.gz)?$"
  ))
  if (is.null(p)) return(NULL)
  df <- tryCatch(utils::read.delim(p, stringsAsFactors = FALSE,
                                   check.names = FALSE),
                 error = function(e) NULL)
  if (is.null(df) || ncol(df) < 3L || nrow(df) < 2L) return(NULL)
  mat <- as.matrix(df[, -1, drop = FALSE])
  mode(mat) <- "numeric"
  colnames(mat) <- colnames(df)[-1]
  mat <- mat[stats::complete.cases(mat), , drop = FALSE]
  if (nrow(mat) < 2L) return(NULL)
  mat
}

# Best-effort sample->condition lookup from the data_acquisition samples
# table, so the PCA can color by biological group. Falls back to the
# sample id when no metadata is discoverable.
.norm_sample_labels <- function(ctx, sample_ids) {
  labels <- stats::setNames(sample_ids, sample_ids)
  # 1) manifest condition map (DESeq2/pydeseq2 emit downstream_handoff.condition_map)
  cmap <- ctx$manifest$condition_map %||%
    (if (is.list(ctx$manifest$downstream_handoff))
       ctx$manifest$downstream_handoff$condition_map else NULL)
  if (is.list(cmap) && length(cmap)) {
    hit <- FALSE
    for (s in sample_ids) {
      v <- cmap[[s]]
      if (!is.null(v)) { labels[s] <- as.character(v); hit <- TRUE }
    }
    if (hit) return(unname(labels[sample_ids]))
  }
  # 2) fall back to a samples table under data_acquisition
  od <- normalizePath(ctx$outputs_dir, mustWork = FALSE)
  parts <- strsplit(od, .Platform$file.sep, fixed = TRUE)[[1]]
  i <- match("outputs", parts)
  if (is.na(i)) return(unname(labels[sample_ids]))
  # Splitting an absolute path yields a leading "" element, which
  # file.path() turns back into the leading "/", so the absolute prefix
  # is preserved without special-casing.
  root <- do.call(file.path, as.list(parts[seq_len(i)]))
  da <- file.path(root, "data_acquisition")
  if (!dir.exists(da)) return(unname(labels[sample_ids]))
  cands <- list.files(da, pattern = "samples?(_metadata)?\\.tsv$",
                      recursive = TRUE, full.names = TRUE, ignore.case = TRUE)
  for (p in cands) {
    df <- tryCatch(utils::read.delim(p, stringsAsFactors = FALSE,
                                     check.names = FALSE),
                   error = function(e) NULL)
    if (is.null(df) || nrow(df) == 0L) next
    cl <- tolower(colnames(df))
    sidx <- which(cl %in% c("sample", "sample_id", "id", "name"))
    lidx <- which(cl %in% c("condition", "group", "treatment", "label"))
    if (length(sidx) && length(lidx)) {
      m <- stats::setNames(as.character(df[[lidx[1]]]),
                           as.character(df[[sidx[1]]]))
      for (s in sample_ids) if (!is.na(m[s])) labels[s] <- m[s]
      break
    }
  }
  unname(labels[sample_ids])
}

# ── mean-variance: log-log scatter, the canonical normalization diagnostic
ecaa_register_figure("normalization", "mean_variance", function(ctx) {
  mv <- .norm_load_mean_variance(ctx)
  if (is.null(mv) || length(mv$mean) == 0L) {
    stop("no mean_variance table (mean/variance or log2_mean/log2_variance columns) found under outputs_dir")
  }
  if (isTRUE(mv$log_scale)) {
    ecaa_scatter(x = mv$mean, y = mv$variance, title = "Mean-variance",
                 xlabel = "log2(mean)", ylabel = "log2(variance)",
                 point_size = 0.8)
  } else {
    ecaa_scatter(x = log1p(mv$mean), y = log1p(mv$variance),
                 title = "Mean-variance", xlabel = "log1p(mean)",
                 ylabel = "log1p(variance)", point_size = 0.8)
  }
})

# ── hvg_count_bar: highly-variable feature count per run
ecaa_register_figure("normalization", "hvg_count_bar", function(ctx) {
  h <- .norm_n_hvg(ctx)
  if (is.null(h)) {
    stop("manifest.runs[].n_hvg or hvg_list.tsv required for hvg_count_bar")
  }
  ecaa_bar(
    names = h$names,
    values = h$values,
    title = "Highly-variable features per run",
    ylabel = "n HVG",
    xlabel = "run"
  )
})

# ── sample_pca: PC1/PC2 of samples on the normalized matrix, colored by
# condition. Distinct condition clusters confirm biological signal survives
# library-size correction.
ecaa_register_figure("normalization", "sample_pca", function(ctx) {
  mat <- .norm_load_matrix(ctx)
  if (is.null(mat)) {
    stop("no normalized expression matrix (vst_matrix.tsv / normalized_counts.tsv) found")
  }
  sample_ids <- colnames(mat)
  if (length(sample_ids) < 2L) {
    stop(sprintf("sample_pca requires >=2 samples, got %d", length(sample_ids)))
  }
  X <- t(mat)  # samples x genes
  keep <- apply(X, 2, function(col) stats::var(col) > 0)
  X <- X[, keep, drop = FALSE]
  if (ncol(X) < 2L) stop("sample_pca requires >=2 informative features")
  pca <- stats::prcomp(X, center = TRUE, scale. = FALSE)
  if (ncol(pca$x) < 2L) stop("sample_pca requires rank >= 2")
  var_exp <- (pca$sdev^2) / sum(pca$sdev^2)
  labels <- .norm_sample_labels(ctx, sample_ids)
  df <- data.frame(pc1 = pca$x[, 1], pc2 = pca$x[, 2],
                   sample = sample_ids, condition = labels,
                   stringsAsFactors = FALSE)
  uniq <- unique(labels)
  pal <- ecaa_palette(length(uniq), name = "pca")
  p <- ggplot2::ggplot(df, ggplot2::aes(x = .data$pc1, y = .data$pc2)) +
    ggplot2::geom_hline(yintercept = 0, color = "#cccccc", linewidth = 0.3) +
    ggplot2::geom_vline(xintercept = 0, color = "#cccccc", linewidth = 0.3) +
    ggplot2::geom_point(ggplot2::aes(color = .data$condition),
                        size = 3, stroke = 0.3) +
    ggplot2::geom_text(ggplot2::aes(label = .data$sample),
                       vjust = -0.8, size = 2.8, show.legend = FALSE) +
    ggplot2::scale_color_manual(values = stats::setNames(pal, uniq),
                                name = "condition") +
    ggplot2::labs(
      title = "Sample PCA",
      x = sprintf("PC1 (%.1f%% var)", 100 * var_exp[1]),
      y = sprintf("PC2 (%.1f%% var)", 100 * var_exp[2])
    )
  if (length(uniq) <= 1L) p <- p + ggplot2::theme(legend.position = "none")
  p
})
