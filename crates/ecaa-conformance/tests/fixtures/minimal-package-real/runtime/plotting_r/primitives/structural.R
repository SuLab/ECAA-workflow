# Universal structural primitives — R parity to
# lib/plotting/primitives/structural.py.
#
# Every primitive sources lib/plotting_r/core.R for theme, palette,
# and swfc_savefig so visual style is byte-stable given the same
# pinned ggplot2 + Cairo + ragg versions.
#
# Provenance kind strings match GenericPrimitive::figure_id() in
# crates/core/src/plot_affordance/primitive.rs:
#   __structural_matrix_overview
#   __structural_distribution
#   __structural_categorical_summary
#   __structural_pairs
#   __structural_scalar_card
#
# Each function signature mirrors the Python counterpart:
#   primitive_name(data, ..., png_path, pdf_path, title, theme_path)
#
# NOTE: GGally and gridExtra are not declared as deps in DESCRIPTION.
# The pairs() primitive therefore uses a base ggplot2 facet approach
# via a long-format melt + facet_grid, which requires no additional
# packages.
#
# swfc_savefig() writes both PNG and PDF when THEME$output$formats
# contains both entries (the default).  The primary path passed is the
# PNG path; the companion PDF is written alongside with the same stem
# (matching the Python discipline in savefig(formats=["png","pdf"])).
#
# Guard: source core.R before this file.

if (!exists("swfc_savefig")) {
  stop(paste0(
    "source runtime/plotting_r/core.R before sourcing this file.\n",
    "e.g.  source('lib/plotting_r/core.R')\n",
    "      source('lib/plotting_r/primitives/structural.R')"
  ))
}


# ---------------------------------------------------------------------------
# matrix_overview — PhysicalShape::Numeric2D
# ---------------------------------------------------------------------------

#' Heatmap of any 2D numeric matrix.
#'
#' Rasterizes the tile layer when matrix size > 50 000 cells (controlled by
#' THEME$output$rasterize_threshold_n) to keep PDF file size bounded.
#'
#' @param matrix  Numeric matrix (rows x cols).
#' @param png_path  Character path for the PNG output.
#' @param pdf_path  Character path for the PDF output (written alongside PNG).
#' @param title     Figure title.
#' @param theme_path  Theme identifier string (forwarded; THEME is read
#'                    from the live module-level object loaded by core.R).
structural_matrix_overview <- function(
    matrix,
    png_path,
    pdf_path,
    title      = "",
    theme_path = "theme.json"
) {
  if (!is.matrix(matrix) && !is.array(matrix)) {
    matrix <- as.matrix(matrix)
  }
  if (length(dim(matrix)) != 2) {
    stop(sprintf(
      "structural_matrix_overview expects a 2D matrix; got %d dimensions",
      length(dim(matrix))
    ))
  }
  n_rows <- nrow(matrix)
  n_cols <- ncol(matrix)
  thresh <- THEME$output$rasterize_threshold_n %||% 50000L

  # Convert matrix to long-format data frame for ggplot2 geom_tile.
  df <- data.frame(
    row   = rep(seq_len(n_rows), times = n_cols),
    col   = rep(seq_len(n_cols), each  = n_rows),
    value = as.vector(matrix),
    stringsAsFactors = FALSE
  )

  rasterize <- (n_rows * n_cols) > thresh

  p <- ggplot2::ggplot(df, ggplot2::aes(x = .data$col, y = .data$row, fill = .data$value)) +
    ggplot2::geom_tile(raster = rasterize) +
    ggplot2::scale_fill_viridis_c(name = "value") +
    ggplot2::scale_y_reverse() +
    ggplot2::labs(title = title, x = "column", y = "row") +
    ggplot2::theme(aspect.ratio = n_rows / n_cols)

  p <- .swfc_attach_footer(p, "__structural_matrix_overview")
  swfc_savefig(p, png_path, stage_id = "__structural_matrix_overview")
  invisible(NULL)
}


# ---------------------------------------------------------------------------
# distribution — PhysicalShape::Numeric1D
# ---------------------------------------------------------------------------

#' Histogram of any 1D numeric vector with optional KDE overlay.
#'
#' The KDE overlay is added only when length(values) >= 25.
#'
#' @param values    Numeric vector.
#' @param png_path  Character path for the PNG output.
#' @param pdf_path  Character path for the PDF output.
#' @param title     Figure title.
#' @param theme_path  Theme identifier string (forwarded; not used directly).
#' @param bins      Number of histogram bins (default 50).
structural_distribution <- function(
    values,
    png_path,
    pdf_path,
    title      = "",
    theme_path = "theme.json",
    bins       = 50L
) {
  arr <- as.numeric(values)
  arr <- arr[is.finite(arr)]
  if (length(arr) == 0) {
    stop("structural_distribution requires a non-empty input vector")
  }

  df <- data.frame(value = arr, stringsAsFactors = FALSE)

  p <- ggplot2::ggplot(df, ggplot2::aes(x = .data$value)) +
    ggplot2::geom_histogram(
      ggplot2::aes(y = ggplot2::after_stat(density)),
      bins  = bins,
      fill  = .WONG_PALETTE[6],
      color = "#333333",
      linewidth = 0.2,
      alpha = 0.6
    ) +
    ggplot2::labs(title = title, x = "value", y = "density")

  if (length(arr) >= 25L) {
    p <- p + ggplot2::geom_density(
      color     = .WONG_PALETTE[3],
      linewidth = 0.7
    )
  }

  p <- .swfc_attach_footer(p, "__structural_distribution")
  swfc_savefig(p, png_path, stage_id = "__structural_distribution")
  invisible(NULL)
}


# ---------------------------------------------------------------------------
# categorical_summary — PhysicalShape::Categorical1D
# ---------------------------------------------------------------------------

#' Bar plot of per-category counts for a 1D categorical vector.
#'
#' Categories are sorted by descending count; ties broken by ascending
#' string representation (deterministic regardless of insertion order).
#'
#' @param labels    Character (or factor / integer) vector of category labels.
#' @param png_path  Character path for the PNG output.
#' @param pdf_path  Character path for the PDF output.
#' @param title     Figure title.
#' @param theme_path  Theme identifier string (forwarded; not used directly).
structural_categorical_summary <- function(
    labels,
    png_path,
    pdf_path,
    title      = "",
    theme_path = "theme.json"
) {
  labels <- as.character(labels)
  if (length(labels) == 0) {
    stop("structural_categorical_summary requires at least one label")
  }

  counts_tbl <- sort(table(labels), decreasing = TRUE)
  # Deterministic tie-break: sort tied groups by ascending label string.
  freq <- as.integer(counts_tbl)
  nms  <- names(counts_tbl)
  ord  <- order(-freq, nms)
  freq <- freq[ord]
  nms  <- nms[ord]

  df <- data.frame(
    category = factor(nms, levels = nms),
    count    = freq,
    stringsAsFactors = FALSE
  )

  n_cats <- length(nms)
  pal    <- swfc_palette(n_cats, name = "categorical_summary")

  fig_width <- max(6.0, min(16.0, n_cats * 0.6 + 2.0))

  p <- ggplot2::ggplot(df, ggplot2::aes(x = .data$category, y = .data$count,
                                        fill = .data$category)) +
    ggplot2::geom_col(width = 0.75, color = "#333333", linewidth = 0.2) +
    ggplot2::scale_fill_manual(values = stats::setNames(pal, nms), guide = "none") +
    ggplot2::labs(title = title, x = "", y = "count") +
    ggplot2::theme(axis.text.x = ggplot2::element_text(angle = 45, hjust = 1, size = 7))

  p <- .swfc_attach_footer(p, "__structural_categorical_summary")
  swfc_savefig(p, png_path,
               stage_id = "__structural_categorical_summary",
               width_in  = fig_width,
               height_in = 5.5)
  invisible(NULL)
}


# ---------------------------------------------------------------------------
# pairs — PhysicalShape::TabularNumeric { columns: 2..=8 }
# ---------------------------------------------------------------------------

#' Small-multiples scatter matrix for 2D tabular numeric input (<=8 columns).
#'
#' Diagonal cells show per-column density plots; off-diagonal cells show
#' pairwise scatter plots.  Implemented as a ggplot2 facet_grid over
#' a melted long-format data frame — no GGally or gridExtra dependency.
#'
#' @param table        Numeric matrix or data.frame of shape (n_rows, n_cols).
#'                     n_cols must be between 2 and 8 inclusive.
#' @param column_names Character vector of length n_cols.
#' @param png_path     Character path for the PNG output.
#' @param pdf_path     Character path for the PDF output.
#' @param title        Figure suptitle.
#' @param theme_path   Theme identifier string (forwarded; not used directly).
structural_pairs <- function(
    table,
    column_names,
    png_path,
    pdf_path,
    title      = "",
    theme_path = "theme.json"
) {
  mat <- as.matrix(table)
  if (length(dim(mat)) != 2) {
    stop("structural_pairs expects a 2D matrix or data.frame")
  }
  n_cols <- ncol(mat)
  if (n_cols > 8L) {
    stop(sprintf(
      "structural_pairs accepts at most 8 columns; got %d. ",
      n_cols
    ))
  }
  if (length(column_names) != n_cols) {
    stop(sprintf(
      "column_names length (%d) must match table column count (%d)",
      length(column_names), n_cols
    ))
  }

  colnames(mat) <- column_names
  scatter_color <- .WONG_PALETTE[6]

  # Build each panel as an individual ggplot object and arrange via
  # a grid.  We use base graphics grid.newpage / pushViewport so we
  # need only the standard grid package (already a dep via ggplot2).
  cell_plots <- vector("list", n_cols * n_cols)
  for (i in seq_len(n_cols)) {
    for (j in seq_len(n_cols)) {
      idx <- (i - 1) * n_cols + j
      xi <- mat[, j]
      yi <- mat[, i]
      if (i == j) {
        df_diag <- data.frame(v = xi, stringsAsFactors = FALSE)
        cell_plots[[idx]] <- ggplot2::ggplot(df_diag, ggplot2::aes(x = .data$v)) +
          ggplot2::geom_histogram(bins = 30L, fill = scatter_color,
                                  color = "#333333", linewidth = 0.2, alpha = 0.7) +
          ggplot2::labs(x = if (i == n_cols) column_names[j] else NULL,
                        y = if (j == 1)    column_names[i] else NULL) +
          ggplot2::theme(
            axis.title.x = ggplot2::element_text(size = 7),
            axis.title.y = ggplot2::element_text(size = 7),
            plot.margin  = grid::unit(c(1, 1, 1, 1), "pt")
          )
      } else {
        df_sc <- data.frame(x = xi, y = yi, stringsAsFactors = FALSE)
        cell_plots[[idx]] <- ggplot2::ggplot(df_sc, ggplot2::aes(x = .data$x, y = .data$y)) +
          ggplot2::geom_point(size = 0.8, alpha = 0.5, color = scatter_color, stroke = 0) +
          ggplot2::labs(x = if (i == n_cols) column_names[j] else NULL,
                        y = if (j == 1)    column_names[i] else NULL) +
          ggplot2::theme(
            axis.title.x = ggplot2::element_text(size = 7),
            axis.title.y = ggplot2::element_text(size = 7),
            plot.margin  = grid::unit(c(1, 1, 1, 1), "pt")
          )
      }
    }
  }

  # Combine using patchwork if available; otherwise fall back to grid.
  cell_size <- 2.0
  fig_size  <- cell_size * n_cols

  if (requireNamespace("patchwork", quietly = TRUE)) {
    combined <- Reduce(`+`, cell_plots) +
      patchwork::plot_layout(ncol = n_cols) +
      patchwork::plot_annotation(title = title)
    swfc_savefig(combined, png_path,
                 stage_id  = "__structural_pairs",
                 width_in  = fig_size,
                 height_in = fig_size)
  } else {
    # Fallback: write each cell to a temporary file and compose with grid.
    png_out <- as.character(png_path)
    pdf_out <- sub("\\.[^.]+$", ".pdf", png_out)
    dpi     <- THEME$output$png_dpi %||% 300L

    .swfc_write_pairs_grid(cell_plots, n_cols, title,
                           png_out, pdf_out, fig_size, dpi)
  }
  invisible(NULL)
}


#' Internal grid-based layout used when patchwork is unavailable.
#' @keywords internal
.swfc_write_pairs_grid <- function(cell_plots, n_cols, title,
                                   png_out, pdf_out, fig_size, dpi) {
  .write_one <- function(out_path, device_fn) {
    device_fn(out_path, width = fig_size, height = fig_size, units = "in", res = dpi)
    on.exit(grDevices::dev.off(), add = TRUE)
    grid::grid.newpage()
    grid::pushViewport(
      grid::viewport(layout = grid::grid.layout(n_cols, n_cols))
    )
    for (idx in seq_along(cell_plots)) {
      i <- ((idx - 1) %/% n_cols) + 1L
      j <- ((idx - 1) %%  n_cols) + 1L
      grid::pushViewport(grid::viewport(
        layout.pos.row = i, layout.pos.col = j
      ))
      print(cell_plots[[idx]], newpage = FALSE)
      grid::popViewport()
    }
    grid::popViewport()
    if (nzchar(title)) {
      grid::grid.text(title, x = 0.5, y = 0.99,
                      just = c("centre", "top"),
                      gp = grid::gpar(fontsize = 9))
    }
  }

  .write_one(png_out, function(p, ...) ragg::agg_png(p, ..., background = "white"))
  .write_one(pdf_out, function(p, width, height, ...) {
    grDevices::cairo_pdf(p, width = width, height = height, onefile = FALSE,
                         family = THEME$fonts$stack[[1]] %||% "sans")
  })
}


# ---------------------------------------------------------------------------
# scalar_card — PhysicalShape::Scalar
# ---------------------------------------------------------------------------

#' Plain-text display card for a single scalar metric.
#'
#' Renders the numeric value in large type with a descriptive label below,
#' suitable for surfacing aggregate statistics (AUROC, R-squared, p-value).
#'
#' @param value     Scalar numeric value.
#' @param label     Short descriptive label.
#' @param png_path  Character path for the PNG output.
#' @param pdf_path  Character path for the PDF output.
#' @param title     Figure title.
#' @param theme_path  Theme identifier string (forwarded; not used directly).
structural_scalar_card <- function(
    value,
    label,
    png_path,
    pdf_path,
    title      = "",
    theme_path = "theme.json"
) {
  value_str <- formatC(as.numeric(value), digits = 4, format = "g")

  df_val   <- data.frame(x = 0.5, y = 0.60, label = value_str,
                         stringsAsFactors = FALSE)
  df_label <- data.frame(x = 0.5, y = 0.25, label = as.character(label),
                         stringsAsFactors = FALSE)

  p <- ggplot2::ggplot() +
    ggplot2::xlim(0, 1) + ggplot2::ylim(0, 1) +
    ggplot2::geom_text(
      data = df_val,
      ggplot2::aes(x = .data$x, y = .data$y, label = .data$label),
      size  = 36 / .pt,
      color = "#222222"
    ) +
    ggplot2::geom_text(
      data = df_label,
      ggplot2::aes(x = .data$x, y = .data$y, label = .data$label),
      size  = 14 / .pt,
      color = "#444444"
    ) +
    ggplot2::labs(title = title) +
    ggplot2::theme_void() +
    ggplot2::theme(
      plot.title   = ggplot2::element_text(size = THEME$fonts$title_pt %||% 9,
                                           hjust = 0.5),
      plot.margin  = grid::unit(c(8, 8, 24, 8), "pt")
    )

  p <- .swfc_attach_footer(p, "__structural_scalar_card")
  swfc_savefig(p, png_path,
               stage_id  = "__structural_scalar_card",
               width_in  = 4.0,
               height_in = 2.5)
  invisible(NULL)
}
