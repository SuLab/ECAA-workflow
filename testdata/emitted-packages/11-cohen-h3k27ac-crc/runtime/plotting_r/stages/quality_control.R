# R-side quality_control stage figures: per_sample_metric_violin,
# per_sample_metric_bar, qc_summary_bar.
# Mirrors lib/plotting/stages/quality_control.py contract.
#
# Plan reference: Phase 10 affordance catalog expansion;
# semantic_type data:2914 → renderer_module
# runtime.plotting.stages.quality_control.
#
# Expected inputs (any subset works; missing inputs produce a typed
# error recorded by ecaa_generate, not a crash):
#
#   outputs_dir/qc_metrics.tsv[.gz]  — long-form: sample, metric, value
#   manifest.per_sample_metrics      — {sample_id: {metric: scalar}}
#   outputs_dir/summary_stats.json   — {key: numeric_value, ...}

if (!exists("ecaa_register_figure")) {
  stop("source runtime/plotting_r/core.R before this stage module")
}

# ---------------------------------------------------------------------------
# Internal data loaders — mirrors Python _load_long_metrics /
# _manifest_per_sample helpers.
# ---------------------------------------------------------------------------

# Parse qc_metrics.{tsv,tsv.gz} → list(metric = list(sample = values)).
# Returns NULL when the file is missing or the header is unrecognised.
.qc_load_long_metrics <- function(outputs_dir) {
  for (nm in c("qc_metrics.tsv.gz", "qc_metrics.tsv")) {
    p <- file.path(outputs_dir, nm)
    if (!file.exists(p)) next
    df <- tryCatch(
      utils::read.delim(p, stringsAsFactors = FALSE, check.names = FALSE,
                        comment.char = ""),
      error = function(e) NULL
    )
    if (is.null(df) || nrow(df) == 0L) next
    cols_lc <- tolower(colnames(df))
    i_sample <- which(cols_lc == "sample")
    i_metric <- which(cols_lc == "metric")
    i_value  <- which(cols_lc == "value")
    if (!length(i_sample) || !length(i_metric) || !length(i_value)) return(NULL)
    out <- list()
    for (row_i in seq_len(nrow(df))) {
      val <- suppressWarnings(as.numeric(df[[i_value[1]]][row_i]))
      if (is.na(val)) next
      metric <- as.character(df[[i_metric[1]]][row_i])
      sample <- as.character(df[[i_sample[1]]][row_i])
      if (!is.list(out[[metric]])) out[[metric]] <- list()
      if (!is.numeric(out[[metric]][[sample]])) {
        out[[metric]][[sample]] <- val
      } else {
        out[[metric]][[sample]] <- c(out[[metric]][[sample]], val)
      }
    }
    return(if (length(out) > 0L) out else NULL)
  }
  NULL
}

# Pull per-sample scalar metrics out of manifest.per_sample_metrics.
# Returns list(samples = character, metrics = list(name = numeric_vec))
# or NULL when the manifest block is absent.
.qc_manifest_per_sample <- function(ctx) {
  per_sample <- ctx$manifest$per_sample_metrics
  if (!is.list(per_sample) || length(per_sample) == 0L) return(NULL)
  samples <- sort(names(per_sample))
  # Collect all numeric metric names across samples.
  metric_names <- character(0)
  for (s in samples) {
    m <- per_sample[[s]]
    if (is.list(m)) {
      metric_names <- union(metric_names,
        names(Filter(function(v) is.numeric(v) || is.integer(v), m)))
    }
  }
  if (length(metric_names) == 0L) return(NULL)
  metric_names <- sort(metric_names)
  metrics <- lapply(stats::setNames(metric_names, metric_names), function(mn) {
    sapply(samples, function(s) {
      v <- per_sample[[s]][[mn]]
      if (!is.null(v) && (is.numeric(v) || is.integer(v))) as.numeric(v)
      else NA_real_
    })
  })
  list(samples = samples, metrics = metrics)
}

# ---------------------------------------------------------------------------
# per_sample_metric_violin
# Violin (with embedded boxplot) of per-sample QC metrics across samples.
# Falls back to a grouped bar when only scalar per-sample data is available
# (no long-form qc_metrics.tsv distribution), matching the Python fallback.
# ---------------------------------------------------------------------------

ecaa_register_figure("quality_control", "per_sample_metric_violin",
                     function(ctx) {
  long_form <- .qc_load_long_metrics(ctx$outputs_dir)
  if (!is.null(long_form) && length(long_form) > 0L) {
    # Pick the metric with the most distinct values (widest variation).
    metric_name <- names(long_form)[which.max(
      sapply(long_form, function(m) {
        all_vals <- unlist(m)
        if (is.null(all_vals)) return(0L)
        length(unique(all_vals))
      })
    )]
    samples_per_metric <- long_form[[metric_name]]
    data_list <- lapply(samples_per_metric, as.numeric)
    p <- ecaa_violin(
      data_list,
      title   = sprintf("QC: %s per sample", metric_name),
      ylabel  = metric_name,
      x_label = "sample"
    )
    ecaa_savefig(p, path = file.path(ctx$outputs_dir, "figures",
                                      "per_sample_metric_violin.png"),
                 stage_id = "quality_control")
    return(p)
  }
  # Fallback: scalar manifest data → bar.
  mdata <- .qc_manifest_per_sample(ctx)
  if (is.null(mdata)) {
    stop("no qc_metrics.tsv[.gz] or manifest.per_sample_metrics")
  }
  metric_name <- names(mdata$metrics)[1]
  p <- ecaa_bar(
    names  = mdata$samples,
    values = mdata$metrics[[metric_name]],
    title  = sprintf("QC: %s per sample", metric_name),
    ylabel = metric_name,
    xlabel = "sample"
  )
  ecaa_savefig(p, path = file.path(ctx$outputs_dir, "figures",
                                    "per_sample_metric_violin.png"),
               stage_id = "quality_control")
  p
})

# ---------------------------------------------------------------------------
# per_sample_metric_bar
# Horizontal bar of the cardinal per-sample count metric. Prefers a key
# containing the substring "count" or starting with "n_"; falls back to
# the first key. Matches Python per_sample_metric_bar.
# ---------------------------------------------------------------------------

ecaa_register_figure("quality_control", "per_sample_metric_bar",
                     function(ctx) {
  mdata <- .qc_manifest_per_sample(ctx)
  if (is.null(mdata)) {
    stop("manifest.per_sample_metrics required for per_sample_metric_bar")
  }
  metric_keys <- names(mdata$metrics)
  preferred <- {
    cnt_key <- grep("count",           metric_keys, value = TRUE,
                    ignore.case = TRUE)[1]
    n_key   <- grep("^n_",             metric_keys, value = TRUE,
                    ignore.case = TRUE, perl = TRUE)[1]
    if (!is.na(cnt_key)) cnt_key
    else if (!is.na(n_key)) n_key
    else metric_keys[1]
  }
  p <- ecaa_bar(
    names      = mdata$samples,
    values     = mdata$metrics[[preferred]],
    title      = sprintf("QC: %s per sample", preferred),
    ylabel     = preferred,
    xlabel     = "sample",
    horizontal = TRUE          # horizontal bar for per-sample count
  )
  ecaa_savefig(p, path = file.path(ctx$outputs_dir, "figures",
                                    "per_sample_metric_bar.png"),
               stage_id = "quality_control")
  p
})

# ---------------------------------------------------------------------------
# qc_summary_bar
# Aggregate summary bar from summary_stats.json. Useful when the stage
# didn't emit per-sample distributions but produced aggregate counts
# (e.g. total reads, total variants, total cells). Matches Python
# qc_summary_bar.
# ---------------------------------------------------------------------------

ecaa_register_figure("quality_control", "qc_summary_bar", function(ctx) {
  summary_path <- file.path(ctx$outputs_dir, "summary_stats.json")
  if (!file.exists(summary_path)) stop("summary_stats.json")
  summary_data <- tryCatch(
    jsonlite::fromJSON(summary_path, simplifyVector = TRUE),
    error = function(e) stop(sprintf("summary_stats.json unparseable: %s",
                                      conditionMessage(e)))
  )
  # Keep only scalar (length-1) numerics.
  scalar <- Filter(
    function(v) is.numeric(v) && length(v) == 1L && is.finite(v),
    summary_data
  )
  if (length(scalar) == 0L) stop("summary_stats.json has no scalar metrics")
  nm  <- names(scalar)
  val <- as.numeric(unlist(scalar))
  p <- ecaa_bar(
    names  = nm,
    values = val,
    title  = "QC: aggregate summary",
    ylabel = "value"
  )
  ecaa_savefig(p, path = file.path(ctx$outputs_dir, "figures",
                                    "qc_summary_bar.png"),
               stage_id = "quality_control")
  p
})
