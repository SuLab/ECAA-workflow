# R-side preprocessing stage: retention_bar.
# Mirrors lib/plotting/stages/preprocessing.py contract — same
# manifest input convention (manifest.samples[].{n_in,n_out}),
# same figure_id catalog.
#
# Plan reference: Phase 10 affordance catalog expansion;
# semantic_type data:1234 → renderer_module
# runtime.plotting.stages.preprocessing.
#
# Visual contract: grouped bar showing n_in vs n_out per sample,
# with 45° x-axis labels. Optionally faceted by a group column
# when manifest.samples[].group is present (matching the Python
# side's group-aware variant).

if (!exists("ecaa_register_figure")) {
  stop("source runtime/plotting_r/core.R before this stage module")
}

# Extract (sample_id, n_in, n_out, group?) rows from manifest.
# Returns a data.frame with columns: id, n_in, n_out, group (NA
# when absent). Returns NULL when the manifest lacks a usable
# `samples` block.
.preprocessing_retention_rows <- function(ctx) {
  samples <- ctx$manifest$samples
  if (!is.list(samples) || length(samples) == 0L) return(NULL)
  rows <- lapply(samples, function(s) {
    if (!is.list(s)) return(NULL)
    sid <- as.character(s$id %||% "?")
    n_in  <- s$n_in  %||% s$n_before
    n_out <- s$n_out %||% s$n_after
    if (is.null(n_in) || is.null(n_out)) return(NULL)
    grp <- as.character(s$group %||% NA_character_)
    data.frame(id = sid, n_in = as.integer(n_in),
               n_out = as.integer(n_out), group = grp,
               stringsAsFactors = FALSE)
  })
  rows <- Filter(Negate(is.null), rows)
  if (length(rows) == 0L) return(NULL)
  do.call(rbind, rows)
}

ecaa_register_figure("preprocessing", "retention_bar", function(ctx) {
  df <- .preprocessing_retention_rows(ctx)
  if (is.null(df)) stop("manifest.samples[].n_in/n_out required")

  # Reshape to long-form for grouped bar plotting.
  n_samples <- nrow(df)
  long <- data.frame(
    id      = rep(df$id, 2L),
    count   = c(df$n_in, df$n_out),
    phase   = factor(rep(c("before", "after"), each = n_samples),
                     levels = c("before", "after")),
    group   = rep(df$group, 2L),
    stringsAsFactors = FALSE
  )
  long$id <- factor(long$id, levels = df$id)

  # Two-color palette: light steel blue for "before", steel blue for "after".
  # These mirror the Python matplotlib defaults (lightsteelblue / steelblue).
  phase_pal <- c(before = "#B0C4DE", after  = "#4682B4")

  fig_width  <- max(6.0, 0.4 * n_samples)
  fig_height <- 5.0

  has_group <- !all(is.na(df$group))

  p <- ggplot2::ggplot(long,
         ggplot2::aes(x = .data$id, y = .data$count,
                      fill = .data$phase)) +
    ggplot2::geom_col(position = ggplot2::position_dodge(width = 0.75),
                      width = 0.72, color = "#333333", linewidth = 0.2) +
    ggplot2::scale_fill_manual(values = phase_pal, name = NULL) +
    ggplot2::labs(
      title  = "Retention per sample",
      x      = "sample",
      y      = "count"
    ) +
    ggplot2::theme(
      axis.text.x = ggplot2::element_text(angle = 45, hjust = 1)
    )

  if (has_group) {
    p <- p + ggplot2::facet_wrap(~ .data$group, scales = "free_x", nrow = 1L)
  }

  ecaa_savefig(p, path = file.path(ctx$outputs_dir, "figures",
                                    "retention_bar.png"),
               stage_id = "preprocessing",
               width_in = fig_width, height_in = fig_height)
  p
})
