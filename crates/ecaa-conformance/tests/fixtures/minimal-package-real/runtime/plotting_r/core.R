# Core plotting primitives — R-side counterpart to lib/plotting/core.py.
#
# Every emitted package carries this module under runtime/plotting_r/core.R.
# Stage modules source it instead of using ggplot2/ggsave directly so the
# theme baseline + Cairo vector output + metadata stripping + provenance
# footer are always applied uniformly.
#
# Visual parity with the Python renderer is enforced by reading the same
# `theme.json` that ships at runtime/plotting/theme.json — fonts, palette,
# DPI, and output formats stay synchronized across renderers.
#
# Public API:
#   ecaa_apply_theme()                — sets theme_set(ecaa_theme())
#   ecaa_palette(n)                   — Wong/Glasbey colorblind-safe palette
#   ecaa_savefig(plot, path, ...)     — dual-format PNG + Cairo PDF writer
#   ecaa_register_figure(stage, id)   — decorator-equivalent for stage modules
#   ecaa_generate(stage, outputs_dir, required) — dispatcher matching Python
#
# Determinism note: PDF output goes through cairo_pdf which is byte-stable
# given a pinned libcairo + system font cache. The container-image
# pin in policies/container.json is the source of truth for those versions.

suppressPackageStartupMessages({
  library(ggplot2)
  library(scales)
  library(jsonlite)
  library(grid)
  library(ragg)
})

# F20 — read from sibling lib/plotting/VERSION so the Python and R sides
# can never drift apart silently. A version bump touches one file, both
# halves pick it up. `%||%` is defined further down; we inline a NULL
# check here because this constant runs at file-source time, before the
# helpers below are defined.
.ecaa_read_shared_version <- function() {
  ofile_dir <- if (!is.null(sys.frame(1)$ofile)) {
    dirname(sys.frame(1)$ofile)
  } else {
    ""
  }
  candidates <- c(
    file.path(ofile_dir, "..", "plotting", "VERSION"),
    file.path(getwd(), "..", "plotting", "VERSION"),
    file.path(getwd(), "lib", "plotting", "VERSION")
  )
  for (p in candidates) {
    if (nzchar(p) && file.exists(p)) {
      return(trimws(readLines(p, n = 1, warn = FALSE)))
    }
  }
  # Tests and offline fixtures may run before the package is materialised.
  # Fall back to a constant; the cross-language sync test in
  # `lib/plotting_r/tests/test_core.R` is the gate that catches drift.
  "1.1.0"
}

ECAA_PLOTTING_R_VERSION <- .ecaa_read_shared_version()

# ---------------------------------------------------------------------------
# Theme: read theme.json from the sibling Python plotting dir so both
# renderers share one source of truth.
# ---------------------------------------------------------------------------

`%||%` <- function(a, b) if (is.null(a)) b else a

.ecaa_self_dir <- function() {
  # Resolve the directory containing this core.R file across the three
  # ways it can be loaded: source(), Rscript core.R, and `R -e`. We
  # check the most reliable signals first.
  # 1) source() with `chdir = TRUE` or under a stage module:
  for (frame in rev(sys.frames())) {
    f <- tryCatch(frame$ofile, error = function(e) NULL)
    if (!is.null(f) && nzchar(f)) {
      return(normalizePath(dirname(f), mustWork = FALSE))
    }
  }
  # 2) Rscript invocation: --file=path is on the command line.
  args <- commandArgs(trailingOnly = FALSE)
  arg <- grep("^--file=", args, value = TRUE)
  if (length(arg) > 0) {
    f <- sub("^--file=", "", arg[1])
    return(normalizePath(dirname(f), mustWork = FALSE))
  }
  # 3) Last-resort: assume CWD has lib/plotting_r/ visible.
  here <- file.path(getwd(), "lib", "plotting_r")
  if (dir.exists(here)) return(here)
  getwd()
}

.ecaa_theme_path <- function() {
  # Look first next to the Python plotting library (same package), then
  # next to this file as a fallback for ad-hoc R-only usage.
  here <- .ecaa_self_dir()
  cands <- c(
    file.path(here, "..", "plotting", "theme.json"),
    file.path(here, "theme.json")
  )
  for (p in cands) {
    if (file.exists(p)) return(p)
  }
  NA_character_
}

.ecaa_load_theme <- function() {
  p <- .ecaa_theme_path()
  if (is.na(p)) {
    return(.ecaa_default_theme())
  }
  tryCatch(jsonlite::fromJSON(p, simplifyVector = FALSE),
           error = function(e) .ecaa_default_theme())
}

.ecaa_default_theme <- function() {
  list(
    schema_version = 1,
    fonts = list(family = "sans-serif",
                 stack = list("Arial", "Helvetica", "DejaVu Sans"),
                 body_pt = 8, title_pt = 9, tick_pt = 7,
                 legend_pt = 7, footer_pt = 5),
    axes = list(linewidth = 0.5, spines_top = FALSE, spines_right = FALSE,
                constrained_layout = TRUE),
    palette = list(categorical_8 = "wong", categorical_extended = "glasbey20",
                   sequential = "viridis", diverging = "RdBu_r",
                   sig_up = "#D55E00", sig_down = "#0072B2", non_sig = "#999999"),
    output = list(png_dpi = 300, formats = list("png", "pdf"),
                  pdf_fonttype = 42, svg_fonttype = "none",
                  rasterize_threshold_n = 50000),
    provenance_footer = TRUE
  )
}

THEME <- .ecaa_load_theme()

# ---------------------------------------------------------------------------
# Palette — Wong (Okabe-Ito) for n <= 8, Glasbey20 extension for n <= 20.
# Identical hex values to the Python core.py palettes.
# ---------------------------------------------------------------------------

.WONG_PALETTE <- c(
  "#000000", "#E69F00", "#56B4E9", "#009E73",
  "#F0E442", "#0072B2", "#D55E00", "#CC79A7"
)

.GLASBEY20_PALETTE <- c(.WONG_PALETTE,
  "#999999", "#882E72", "#1965B0", "#7BAFDE",
  "#4EB265", "#CAE0AB", "#F7F056", "#EE8026",
  "#DC050C", "#B17BA6", "#5289C7", "#882027"
)

ecaa_wong_palette <- function() .WONG_PALETTE
ecaa_glasbey20_palette <- function() .GLASBEY20_PALETTE

.ECAA_HIGH_CARD_WARNED <- new.env(parent = emptyenv())

ecaa_palette <- function(n, name = NULL) {
  if (n <= 0) return(character(0))
  base <- if (n <= 8) .WONG_PALETTE else .GLASBEY20_PALETTE
  if (n > 20) {
    key <- if (is.null(name)) "global" else name
    if (!exists(key, envir = .ECAA_HIGH_CARD_WARNED)) {
      assign(key, TRUE, envir = .ECAA_HIGH_CARD_WARNED)
      warning(sprintf("ecaa_palette(%d) exceeds 20 colors; cycling glasbey20. ",
                      n),
              "At this cardinality, encode category by shape or label as well.",
              call. = FALSE)
    }
  }
  rep_len(base, length.out = n)
}

# ---------------------------------------------------------------------------
# ecaa_theme — ggplot2 theme that matches Python rcParams.
# ---------------------------------------------------------------------------

ecaa_theme <- function(theme_obj = THEME) {
  fonts <- theme_obj$fonts
  axes  <- theme_obj$axes
  base_size <- fonts$body_pt
  ggplot2::theme_classic(base_size = base_size,
                         base_family = fonts$stack[[1]] %||% "sans") +
    ggplot2::theme(
      plot.title       = ggplot2::element_text(size = fonts$title_pt, hjust = 0),
      axis.title       = ggplot2::element_text(size = fonts$body_pt),
      axis.text        = ggplot2::element_text(size = fonts$tick_pt),
      legend.text      = ggplot2::element_text(size = fonts$legend_pt),
      legend.title     = ggplot2::element_text(size = fonts$legend_pt),
      legend.key       = ggplot2::element_blank(),
      legend.background = ggplot2::element_blank(),
      axis.line        = ggplot2::element_line(linewidth = axes$linewidth),
      axis.ticks       = ggplot2::element_line(linewidth = axes$linewidth),
      panel.grid       = ggplot2::element_blank(),
      strip.background = ggplot2::element_blank(),
      plot.margin      = grid::unit(c(4, 4, 12, 4), "pt")
    )
}

ecaa_apply_theme <- function(theme_obj = THEME) {
  ggplot2::theme_set(ecaa_theme(theme_obj))
  invisible(NULL)
}

# Apply on source so any caller benefits without remembering the call.
ecaa_apply_theme()

# ---------------------------------------------------------------------------
# Provenance footer + savefig (dual-format with metadata strip).
# ---------------------------------------------------------------------------

.ecaa_provenance_text <- function(stage_id) {
  pkg <- Sys.getenv("ECAA_PACKAGE_ID", "unknown")
  sha <- Sys.getenv("ECAA_GIT_SHA", "unknown")
  sha_short <- if (nchar(sha) > 0 && sha != "unknown") substr(sha, 1, 7) else "unknown"
  sprintf("%s · %s · plotting v%s · git@%s",
          pkg, stage_id, ECAA_PLOTTING_R_VERSION, sha_short)
}

.ecaa_attach_footer <- function(plot, stage_id) {
  if (!isTRUE(THEME$provenance_footer)) return(plot)
  caption <- .ecaa_provenance_text(stage_id)
  plot + ggplot2::labs(caption = caption) +
    ggplot2::theme(plot.caption = ggplot2::element_text(
      size = THEME$fonts$footer_pt, color = "#888888", hjust = 1
    ))
}

ecaa_savefig <- function(plot, path,
                         stage_id = "unknown",
                         dpi = NULL,
                         formats = NULL,
                         width_in = 7.0,
                         height_in = 5.5) {
  if (is.null(dpi))     dpi     <- THEME$output$png_dpi
  if (is.null(formats)) formats <- unlist(THEME$output$formats)

  # Ensure parent dir exists.
  dir.create(dirname(path), showWarnings = FALSE, recursive = TRUE)

  primary_suffix <- tolower(tools::file_ext(path))
  if (nchar(primary_suffix) == 0) primary_suffix <- "png"

  desired <- c(primary_suffix, setdiff(tolower(formats), primary_suffix))
  decorated <- .ecaa_attach_footer(plot, stage_id)

  written <- character()
  for (fmt in desired) {
    out_path <- sub(paste0("\\.[^.]+$"), paste0(".", fmt), path)
    if (!grepl("\\.[^.]+$", path)) out_path <- paste0(path, ".", fmt)
    .ecaa_write_one(decorated, out_path, fmt, dpi, width_in, height_in)
    written <- c(written, out_path)
  }
  written[1]  # primary path for back-compat
}

.ecaa_write_one <- function(plot, out, fmt, dpi, width_in, height_in) {
  fmt <- tolower(fmt)
  if (fmt == "png") {
    # ragg::agg_png produces deterministic PNG with stripped metadata.
    ragg::agg_png(out, width = width_in, height = height_in,
                  units = "in", res = dpi, background = "white")
    on.exit(grDevices::dev.off(), add = TRUE)
    print(plot)
  } else if (fmt == "pdf") {
    grDevices::cairo_pdf(out, width = width_in, height = height_in,
                         onefile = FALSE, family = THEME$fonts$stack[[1]] %||% "sans")
    on.exit(grDevices::dev.off(), add = TRUE)
    print(plot)
  } else if (fmt == "svg") {
    grDevices::svg(out, width = width_in, height = height_in,
                   family = THEME$fonts$stack[[1]] %||% "sans")
    on.exit(grDevices::dev.off(), add = TRUE)
    print(plot)
  } else {
    stop(sprintf("unsupported format '%s'", fmt))
  }
}

# ---------------------------------------------------------------------------
# Registry + dispatcher: matches the Python @register_figure pattern.
# ---------------------------------------------------------------------------

.ECAA_FIGURE_REGISTRY <- new.env(parent = emptyenv())

ecaa_register_figure <- function(stage_id, figure_id, fn) {
  bucket <- get0(stage_id, envir = .ECAA_FIGURE_REGISTRY)
  if (is.null(bucket)) {
    bucket <- new.env(parent = emptyenv())
    assign(stage_id, bucket, envir = .ECAA_FIGURE_REGISTRY)
  }
  assign(figure_id, fn, envir = bucket)
  invisible(fn)
}

ecaa_lookup_figure <- function(stage_id, figure_id) {
  bucket <- get0(stage_id, envir = .ECAA_FIGURE_REGISTRY)
  if (is.null(bucket)) return(NULL)
  get0(figure_id, envir = bucket)
}

ecaa_known_figures <- function(stage_id) {
  bucket <- get0(stage_id, envir = .ECAA_FIGURE_REGISTRY)
  if (is.null(bucket)) return(character(0))
  ls(envir = bucket)
}

.ecaa_load_manifest <- function(outputs_dir) {
  p <- file.path(outputs_dir, "manifest.json")
  if (!file.exists(p)) return(list())
  tryCatch(jsonlite::fromJSON(p, simplifyVector = FALSE),
           error = function(e) list())
}

.ecaa_seed <- function(stage_id, figure_id) {
  # Mirror the Python deterministic seed: SHA-256 of "stage|fig", first 8 bytes,
  # masked to 31 bits. Use digest if available; fall back to a stable hash of
  # the concatenated string.
  if (requireNamespace("digest", quietly = TRUE)) {
    h <- digest::digest(paste(stage_id, figure_id, sep = "|"),
                        algo = "sha256", serialize = FALSE)
    as.integer(strtoi(substr(h, 1, 8), 16L) %% 2147483647L)
  } else {
    abs(sum(utf8ToInt(paste(stage_id, figure_id)))) %% 2147483647L
  }
}

ecaa_generate <- function(stage_id, outputs_dir,
                          required = NULL,
                          figures_dir = NULL,
                          write_manifest = TRUE,
                          stage_module = NULL) {
  if (is.null(figures_dir)) figures_dir <- file.path(outputs_dir, "figures")
  dir.create(figures_dir, showWarnings = FALSE, recursive = TRUE)

  if (!is.null(stage_module) && file.exists(stage_module)) {
    source(stage_module, local = FALSE)
  }

  manifest <- .ecaa_load_manifest(outputs_dir)
  known <- ecaa_known_figures(stage_id)
  if (is.null(required)) required <- known

  written <- list()
  formats <- list()
  skipped <- list()
  errors  <- list()

  for (fig_id in required) {
    fn <- ecaa_lookup_figure(stage_id, fig_id)
    if (is.null(fn)) {
      skipped[[fig_id]] <- "not registered in stage module"
      next
    }
    set.seed(.ecaa_seed(stage_id, fig_id))
    target <- file.path(figures_dir, paste0(fig_id, ".png"))
    res <- tryCatch({
      ctx <- list(stage_id = stage_id, figure_id = fig_id,
                  outputs_dir = outputs_dir, manifest = manifest)
      plot_obj <- fn(ctx)
      if (inherits(plot_obj, "ggplot")) {
        primary <- ecaa_savefig(plot_obj, target, stage_id = stage_id)
        list(primary = primary, error = NULL)
      } else {
        list(primary = NULL, error = "figure function did not return a ggplot")
      }
    }, error = function(e) list(primary = NULL, error = conditionMessage(e)))

    if (!is.null(res$primary) && file.exists(res$primary)) {
      written[[fig_id]] <- res$primary
      siblings <- character()
      for (fmt in unlist(THEME$output$formats)) {
        sib <- sub("\\.[^.]+$", paste0(".", tolower(fmt)), res$primary)
        if (file.exists(sib)) siblings <- c(siblings, sib)
      }
      if (length(siblings) > 0) formats[[fig_id]] <- siblings
    } else if (!is.null(res$error)) {
      errors[[fig_id]] <- res$error
    } else {
      errors[[fig_id]] <- "figure function returned without writing"
    }
  }

  manifest_out <- list(
    stage_id = stage_id,
    written  = lapply(written, identity),
    formats  = lapply(formats, identity),
    skipped  = skipped,
    errors   = errors
  )
  if (write_manifest) {
    writeLines(jsonlite::toJSON(manifest_out, auto_unbox = TRUE,
                                pretty = TRUE, na = "null"),
               file.path(figures_dir, "manifest.json"))
  }
  invisible(manifest_out)
}

# ---------------------------------------------------------------------------
# Built-in helpers — bar/scatter/violin/volcano/heatmap with theme baked in.
# Stage modules call these instead of ggplot+ggsave directly.
# ---------------------------------------------------------------------------

ecaa_bar <- function(names, values,
                     ci_lo = NULL, ci_hi = NULL,
                     title = "", ylabel = "", xlabel = "",
                     horizontal = NULL,
                     error_label = "95% CI") {
  n <- length(names)
  if (is.null(horizontal)) horizontal <- n > 12
  pal <- ecaa_palette(n, name = "bar")
  df <- data.frame(name = factor(names, levels = names),
                   value = values,
                   color = pal,
                   stringsAsFactors = FALSE)
  if (!is.null(ci_lo) && !is.null(ci_hi)) {
    df$lo <- ci_lo
    df$hi <- ci_hi
  }
  p <- ggplot2::ggplot(df, ggplot2::aes(x = .data$name, y = .data$value, fill = .data$name)) +
    ggplot2::geom_col(width = 0.75, color = "#333333", linewidth = 0.2) +
    ggplot2::scale_fill_manual(values = stats::setNames(pal, names), guide = "none") +
    ggplot2::labs(title = title, x = xlabel, y = ylabel)
  if (!is.null(ci_lo) && !is.null(ci_hi)) {
    p <- p + ggplot2::geom_errorbar(
      ggplot2::aes(ymin = .data$lo, ymax = .data$hi),
      width = 0.2, color = "#333333", linewidth = 0.4
    )
    p <- p + ggplot2::annotate("text", x = Inf, y = -Inf,
                               label = sprintf("error bars: %s", error_label),
                               hjust = 1.05, vjust = -0.6,
                               size = THEME$fonts$footer_pt / .pt,
                               color = "#666666")
  }
  if (horizontal) p <- p + ggplot2::coord_flip()
  else p <- p + ggplot2::theme(axis.text.x = ggplot2::element_text(angle = 45, hjust = 1))
  p
}

ecaa_scatter <- function(x, y, color = NULL,
                         title = "", xlabel = "", ylabel = "",
                         point_size = 1.0) {
  df <- data.frame(x = x, y = y)
  rasterize <- length(x) > (THEME$output$rasterize_threshold_n %||% 50000)
  if (!is.null(color)) df$color <- color
  p <- ggplot2::ggplot(df, ggplot2::aes(x = .data$x, y = .data$y))
  if (!is.null(color) && is.numeric(color)) {
    p <- p + ggplot2::geom_point(ggplot2::aes(color = .data$color),
                                 size = point_size, alpha = 0.7,
                                 stroke = 0) +
      ggplot2::scale_color_viridis_c()
  } else {
    p <- p + ggplot2::geom_point(size = point_size, alpha = 0.7,
                                 color = .WONG_PALETTE[6], stroke = 0)
  }
  p + ggplot2::labs(title = title, x = xlabel, y = ylabel)
}

ecaa_volcano <- function(log_fc, neg_log10_p, labels = NULL,
                         fc_threshold = 1.0, p_threshold = 1.3,
                         title = "", label_top_n = 10) {
  df <- data.frame(log_fc = log_fc, neg_log_p = neg_log10_p)
  if (!is.null(labels)) df$label <- labels
  sig_up   <- THEME$palette$sig_up
  sig_down <- THEME$palette$sig_down
  non_sig  <- THEME$palette$non_sig
  df$direction <- ifelse(
    abs(df$log_fc) >= fc_threshold & df$neg_log_p >= p_threshold,
    ifelse(df$log_fc > 0, "up", "down"),
    "ns"
  )
  n_up   <- sum(df$direction == "up")
  n_down <- sum(df$direction == "down")
  n_total <- nrow(df)
  pal_v <- c(up = sig_up, down = sig_down, ns = non_sig)
  p <- ggplot2::ggplot(df, ggplot2::aes(x = .data$log_fc, y = .data$neg_log_p,
                                        color = .data$direction)) +
    ggplot2::geom_point(size = 0.8, alpha = 0.75, stroke = 0) +
    ggplot2::scale_color_manual(values = pal_v, guide = "none") +
    ggplot2::geom_hline(yintercept = p_threshold, linetype = "dashed",
                        color = "#444444", linewidth = 0.3) +
    ggplot2::geom_vline(xintercept = c(-fc_threshold, fc_threshold),
                        linetype = "dashed", color = "#444444", linewidth = 0.3) +
    ggplot2::labs(
      title = sprintf("%s\nn = %d features", title, n_total),
      x = expression(log[2] ~ "fold change"),
      y = expression(-log[10] ~ italic(p))
    ) +
    ggplot2::annotate("text", x = Inf, y = Inf, label = sprintf("↑ %d", n_up),
                      hjust = 1.05, vjust = 1.5, color = sig_up, fontface = "bold") +
    ggplot2::annotate("text", x = -Inf, y = Inf, label = sprintf("↓ %d", n_down),
                      hjust = -0.05, vjust = 1.5, color = sig_down, fontface = "bold")

  if (!is.null(labels) && label_top_n > 0 &&
      requireNamespace("ggrepel", quietly = TRUE)) {
    df$score <- abs(df$log_fc) * df$neg_log_p
    top_df <- df[order(-df$score), ][seq_len(min(label_top_n, nrow(df))), , drop = FALSE]
    sig_top <- top_df[top_df$direction != "ns", , drop = FALSE]
    if (nrow(sig_top) > 0) {
      p <- p + ggrepel::geom_text_repel(
        data = sig_top,
        ggplot2::aes(label = .data$label),
        size = THEME$fonts$legend_pt / .pt,
        color = "#222222",
        segment.color = "#888888", segment.size = 0.2,
        max.overlaps = Inf, seed = 1
      )
    }
  }
  p
}

ecaa_violin <- function(data_list,
                        title = "", ylabel = "", x_label = "group",
                        show_points = NULL,
                        show_significance = TRUE) {
  df <- do.call(rbind, lapply(names(data_list), function(g) {
    v <- as.numeric(data_list[[g]])
    if (length(v) == 0) return(NULL)
    data.frame(group = g, value = v, stringsAsFactors = FALSE)
  }))
  if (is.null(df) || nrow(df) == 0) {
    return(ggplot2::ggplot() + ggplot2::labs(title = title, x = x_label, y = ylabel))
  }
  df$group <- factor(df$group, levels = names(data_list))
  pal <- ecaa_palette(length(levels(df$group)), name = "violin")

  # Annotate group counts on the x-axis labels.
  counts <- tapply(df$value, df$group, length)
  labels <- sprintf("%s\n(n=%d)", names(counts), counts)

  if (is.null(show_points)) show_points <- max(counts) <= 200

  p <- ggplot2::ggplot(df, ggplot2::aes(x = .data$group, y = .data$value, fill = .data$group)) +
    ggplot2::geom_violin(scale = "width", trim = FALSE,
                         color = "#333333", linewidth = 0.3, alpha = 0.7) +
    ggplot2::scale_fill_manual(values = pal, guide = "none") +
    ggplot2::scale_x_discrete(labels = labels) +
    ggplot2::geom_boxplot(width = 0.1, fill = "white", outlier.shape = NA,
                          color = "#333333", linewidth = 0.3) +
    ggplot2::labs(title = title, x = x_label, y = ylabel) +
    ggplot2::theme(axis.text.x = ggplot2::element_text(angle = 45, hjust = 1))
  if (show_points) {
    p <- p + ggplot2::geom_jitter(width = 0.12, height = 0,
                                  size = 0.6, alpha = 0.6,
                                  color = "#333333", stroke = 0)
  }
  if (show_significance && length(levels(df$group)) <= 5) {
    sig <- .ecaa_pairwise_significance(df)
    if (length(sig) > 0) {
      ymax <- max(df$value, na.rm = TRUE)
      yrange <- diff(range(df$value, na.rm = TRUE))
      step <- yrange * 0.08
      cur <- ymax + step
      for (s in sig) {
        p <- p + ggplot2::annotate(
          "segment",
          x = s$i, xend = s$j,
          y = cur + step * 0.3, yend = cur + step * 0.3,
          color = "#444444", linewidth = 0.3
        ) + ggplot2::annotate(
          "text", x = (s$i + s$j) / 2, y = cur + step * 0.4,
          label = s$marker, color = "#222222",
          size = THEME$fonts$legend_pt / .pt
        )
        cur <- cur + step
      }
    }
  }
  p
}

.ecaa_significance_marker <- function(p) {
  if (!is.finite(p)) return("ns")
  if (p < 0.001) return("***")
  if (p < 0.01)  return("**")
  if (p < 0.05)  return("*")
  "ns"
}

.ecaa_pairwise_significance <- function(df) {
  groups <- levels(df$group)
  if (length(groups) > 5) return(list())
  vals <- split(df$value, df$group)
  vals <- vals[sapply(vals, length) >= 3]
  if (length(vals) < 2) return(list())
  pairs <- list()
  ng <- length(vals)
  n_pairs <- ng * (ng - 1) / 2
  for (ii in seq_len(ng - 1)) {
    for (jj in seq(ii + 1, ng)) {
      i <- vals[[ii]]; j <- vals[[jj]]
      tryCatch({
        pv <- stats::wilcox.test(i, j, exact = FALSE)$p.value
        pv_corr <- min(1.0, pv * n_pairs)
        pairs[[length(pairs) + 1]] <- list(
          i = match(names(vals)[ii], groups),
          j = match(names(vals)[jj], groups),
          p = pv_corr, marker = .ecaa_significance_marker(pv_corr)
        )
      }, error = function(e) NULL)
    }
  }
  pairs
}

ecaa_heatmap <- function(matrix, row_labels, col_labels,
                         title = "", cbar_label = NULL,
                         center = 0,
                         z_score_rows = FALSE,
                         cluster_rows = NULL,
                         cluster_cols = NULL) {
  matrix <- as.matrix(matrix)
  if (is.null(cluster_rows)) cluster_rows <- nrow(matrix) >= 2 && nrow(matrix) <= 200
  if (is.null(cluster_cols)) cluster_cols <- ncol(matrix) >= 2 && ncol(matrix) <= 200

  if (z_score_rows && nrow(matrix) > 0) {
    rs <- apply(matrix, 1, function(r) {
      sd_r <- stats::sd(r, na.rm = TRUE)
      if (is.na(sd_r) || sd_r == 0) r - mean(r, na.rm = TRUE)
      else (r - mean(r, na.rm = TRUE)) / sd_r
    })
    matrix <- t(rs)
  }

  row_order <- seq_len(nrow(matrix))
  col_order <- seq_len(ncol(matrix))
  if (cluster_rows && nrow(matrix) >= 2) {
    row_order <- stats::hclust(stats::dist(matrix))$order
  }
  if (cluster_cols && ncol(matrix) >= 2) {
    col_order <- stats::hclust(stats::dist(t(matrix)))$order
  }
  ordered <- matrix[row_order, col_order, drop = FALSE]
  ord_rows <- row_labels[row_order]
  ord_cols <- col_labels[col_order]

  long <- data.frame(
    row = factor(rep(ord_rows, ncol(ordered)), levels = ord_rows),
    col = factor(rep(ord_cols, each = nrow(ordered)), levels = ord_cols),
    value = as.vector(ordered)
  )
  vmax <- max(abs(ordered), na.rm = TRUE)
  if (!is.finite(vmax) || vmax == 0) vmax <- 1
  p <- ggplot2::ggplot(long, ggplot2::aes(x = .data$col, y = .data$row,
                                          fill = .data$value)) +
    ggplot2::geom_tile()
  if (!is.null(center)) {
    p <- p + ggplot2::scale_fill_gradient2(
      low = "#2166AC", mid = "#F7F7F7", high = "#B2182B",
      midpoint = center, limits = c(-vmax, vmax),
      name = if (!is.null(cbar_label)) cbar_label
             else if (z_score_rows) "row Z-score" else NULL
    )
  } else {
    p <- p + ggplot2::scale_fill_viridis_c(
      name = cbar_label %||% NULL
    )
  }
  p +
    ggplot2::labs(title = title, x = NULL, y = NULL) +
    ggplot2::scale_y_discrete(limits = rev(levels(long$row))) +
    ggplot2::theme(
      axis.text.x = ggplot2::element_text(angle = 45, hjust = 1)
    )
}

# ---------------------------------------------------------------------------
# Phase F (plan §S12.2) — variant + GWAS R parity stubs.
#
# Each stub takes a data.frame, applies the ecaa theme via theme_set, and
# returns a ggplot object. Callers pass the result to ecaa_savefig() to
# get the dual-format PNG + PDF write with the provenance footer.
#
# These are skeleton implementations — the headline visuals work for
# smoke testing and snapshot baselining, but the polish (greedy label
# placement, LD bin legends, credible-set shading, log-rank annotation
# parity) lands in a follow-up. See TODO comments on each function.
# ---------------------------------------------------------------------------

.ecaa_chrom_order <- function(chrom) {
  # Numeric-first ordering (1..22, X, Y, MT, then any leftover scaffolds).
  s <- as.character(chrom)
  num <- suppressWarnings(as.integer(s))
  rank <- ifelse(!is.na(num), num,
          ifelse(toupper(s) == "X", 23L,
          ifelse(toupper(s) == "Y", 24L,
          ifelse(toupper(s) %in% c("MT","M"), 25L, 1000L))))
  unique(s[order(rank, s)])
}

ecaa_manhattan_r <- function(df,
                             title = "",
                             sig_threshold = -log10(5e-8),
                             suggestive_threshold = -log10(1e-5),
                             label_top_n = 5,
                             chrom_order = NULL) {
  # TODO(plan §S12.2): expand to match Python parity
  # (greedy collision-avoidance labels, full chrom_offset virtualization,
  # rasterize layer when n > rasterize_threshold).
  ecaa_apply_theme()
  if (is.null(df$neg_log10_p)) df$neg_log10_p <- -log10(pmax(df$pvalue, 1e-300))
  chrom_levels <- if (is.null(chrom_order)) .ecaa_chrom_order(df$chrom)
                  else as.character(chrom_order)
  df$chrom <- factor(as.character(df$chrom), levels = chrom_levels)

  # Virtualize positions per chromosome onto a continuous coordinate.
  df <- df[order(df$chrom, df$pos), , drop = FALSE]
  cumlen <- 0
  df$x <- 0
  midpoints <- numeric(length(chrom_levels))
  for (i in seq_along(chrom_levels)) {
    mask <- df$chrom == chrom_levels[i]
    if (!any(mask)) next
    df$x[mask] <- cumlen + df$pos[mask]
    midpoints[i] <- cumlen + (max(df$pos[mask]) + min(df$pos[mask])) / 2
    cumlen <- cumlen + max(df$pos[mask]) + 1
  }
  pal <- THEME$palette
  alt_colors <- c(pal$non_sig %||% "#999999", "#444444")
  df$color <- alt_colors[(as.integer(df$chrom) %% 2L) + 1L]

  ggplot2::ggplot(df, ggplot2::aes(x = .data$x, y = .data$neg_log10_p)) +
    ggplot2::geom_point(ggplot2::aes(color = .data$color),
                        size = 0.6, alpha = 0.75, stroke = 0) +
    ggplot2::scale_color_identity() +
    ggplot2::geom_hline(yintercept = suggestive_threshold,
                        linetype = "dashed", color = "#444444", linewidth = 0.3) +
    ggplot2::geom_hline(yintercept = sig_threshold,
                        color = pal$sig_up %||% "#D55E00", linewidth = 0.4) +
    ggplot2::scale_x_continuous(breaks = midpoints, labels = chrom_levels) +
    ggplot2::labs(title = sprintf("%s\nn = %d variants", title, nrow(df)),
                  x = "chromosome",
                  y = expression(-log[10] ~ italic(p)))
}

ecaa_qq_r <- function(df,
                      title = "",
                      ci_level = 0.95,
                      annotate_lambda_gc = TRUE) {
  # TODO(plan §S12.2): expand to match Python parity
  # (Beta theoretical CI band, λ_GC computation parity, observed clipping note).
  ecaa_apply_theme()
  if (is.null(df$pvalue) && is.null(df$neg_log10_p)) {
    stop("ecaa_qq_r requires a 'pvalue' or 'neg_log10_p' column")
  }
  if (is.null(df$pvalue)) {
    observed <- 10 ^ -df$neg_log10_p
  } else {
    observed <- df$pvalue
  }
  observed <- pmax(observed, 1e-300)
  observed <- sort(observed)
  n <- length(observed)
  expected <- (seq_len(n) - 0.5) / n
  qdf <- data.frame(expected_log = -log10(expected),
                    observed_log = -log10(observed))

  pal <- THEME$palette
  p <- ggplot2::ggplot(qdf, ggplot2::aes(x = .data$expected_log,
                                         y = .data$observed_log)) +
    ggplot2::geom_abline(slope = 1, intercept = 0,
                         linetype = "dashed", color = "#444444",
                         linewidth = 0.3) +
    ggplot2::geom_point(size = 0.6,
                        color = pal$sig_up %||% "#D55E00", alpha = 0.75) +
    ggplot2::labs(title = sprintf("%s\nn = %d", title, n),
                  x = expression(expected ~ -log[10] ~ italic(p)),
                  y = expression(observed ~ -log[10] ~ italic(p)))
  if (annotate_lambda_gc) {
    chi_sq <- stats::qchisq(observed, df = 1, lower.tail = FALSE)
    lam <- stats::median(chi_sq) / stats::qchisq(0.5, df = 1)
    p <- p + ggplot2::annotate("text", x = -Inf, y = Inf,
                               label = sprintf("lambda[GC] == %.3f", lam),
                               parse = TRUE, hjust = -0.1, vjust = 1.5,
                               color = "#222222")
  }
  p
}

ecaa_miami_r <- function(top_df, bottom_df,
                         title = "",
                         top_label = "top",
                         bottom_label = "bottom",
                         sig_threshold = -log10(5e-8)) {
  # TODO(plan §S12.2): expand to match Python parity
  # (mirrored y-axis with shared chromosome virtualization across both panels;
  # currently uses two stacked manhattan calls with patchwork-style hint).
  ecaa_apply_theme()
  if (is.null(top_df$neg_log10_p))
    top_df$neg_log10_p <- -log10(pmax(top_df$pvalue, 1e-300))
  if (is.null(bottom_df$neg_log10_p))
    bottom_df$neg_log10_p <- -log10(pmax(bottom_df$pvalue, 1e-300))
  top_df$panel <- top_label
  bottom_df$panel <- bottom_label
  bottom_df$neg_log10_p <- -bottom_df$neg_log10_p  # mirror via inverted axis
  combined <- rbind(top_df[, c("chrom", "pos", "neg_log10_p", "panel")],
                    bottom_df[, c("chrom", "pos", "neg_log10_p", "panel")])
  pal <- THEME$palette
  ggplot2::ggplot(combined, ggplot2::aes(x = .data$pos,
                                         y = .data$neg_log10_p)) +
    ggplot2::geom_point(size = 0.5, alpha = 0.75,
                        color = pal$non_sig %||% "#999999", stroke = 0) +
    ggplot2::geom_hline(yintercept = c(sig_threshold, -sig_threshold),
                        color = pal$sig_up %||% "#D55E00", linewidth = 0.4) +
    ggplot2::facet_wrap(~ .data$chrom, scales = "free_x", nrow = 1) +
    ggplot2::labs(title = title,
                  x = "position (bp)",
                  y = expression(-log[10] ~ italic(p)))
}

ecaa_locus_zoom_r <- function(df,
                              title = "",
                              lead_index = NULL) {
  # TODO(plan §S12.2): expand to match Python parity
  # (5-color LD bin scale matched to Python core, gene-track strip below,
  # diamond marker for lead variant, optional recombination-rate axis).
  ecaa_apply_theme()
  if (is.null(df$neg_log10_p))
    df$neg_log10_p <- -log10(pmax(df$pvalue, 1e-300))
  if (is.null(lead_index)) lead_index <- which.max(df$neg_log10_p)
  pal <- THEME$palette
  if (!is.null(df$ld)) {
    bins <- c(0, 0.2, 0.4, 0.6, 0.8, 1.001)
    bin_colors <- c("#0072B2", "#56B4E9", "#009E73", "#E69F00", "#D55E00")
    df$ld_bin <- cut(pmax(pmin(df$ld, 1), 0),
                     breaks = bins, include.lowest = TRUE,
                     labels = c("r²<0.2", "0.2-0.4", "0.4-0.6", "0.6-0.8", "r²≥0.8"))
    p <- ggplot2::ggplot(df, ggplot2::aes(x = .data$pos, y = .data$neg_log10_p,
                                          color = .data$ld_bin)) +
      ggplot2::geom_point(size = 1.0, alpha = 0.85, stroke = 0) +
      ggplot2::scale_color_manual(values = bin_colors, name = "LD r²")
  } else {
    p <- ggplot2::ggplot(df, ggplot2::aes(x = .data$pos, y = .data$neg_log10_p)) +
      ggplot2::geom_point(size = 0.8, alpha = 0.75,
                          color = pal$non_sig %||% "#999999", stroke = 0)
  }
  if (length(lead_index) == 1 && !is.na(lead_index) &&
      lead_index >= 1 && lead_index <= nrow(df)) {
    p <- p + ggplot2::geom_point(
      data = df[lead_index, , drop = FALSE],
      ggplot2::aes(x = .data$pos, y = .data$neg_log10_p),
      shape = 23, size = 4, fill = "#882E72", color = "white",
      stroke = 0.6, inherit.aes = FALSE
    )
  }
  p + ggplot2::labs(title = title, x = "position (bp)",
                    y = expression(-log[10] ~ italic(p)))
}

ecaa_credible_set_track_r <- function(df,
                                      title = "",
                                      pp_threshold = 0.95) {
  # TODO(plan §S12.2): expand to match Python parity
  # (rectangular credible-set shade band over convex hull of CS positions;
  # currently colors stems but skips the ggplot2 annotate("rect") band).
  ecaa_apply_theme()
  if (is.null(df$credible_set)) {
    ord <- order(-df$posterior)
    cumulative <- cumsum(df$posterior[ord])
    cutoff <- which(cumulative >= pp_threshold)[1]
    if (is.na(cutoff)) cutoff <- length(df$posterior)
    df$credible_set <- FALSE
    df$credible_set[ord[seq_len(cutoff)]] <- TRUE
  }
  pal <- THEME$palette
  df$color <- ifelse(df$credible_set, pal$sig_up %||% "#D55E00", "#999999")
  ggplot2::ggplot(df, ggplot2::aes(x = .data$pos, y = .data$posterior)) +
    ggplot2::geom_segment(ggplot2::aes(xend = .data$pos, yend = 0,
                                       color = .data$color), linewidth = 0.4) +
    ggplot2::geom_point(ggplot2::aes(color = .data$color),
                        size = 1.2, stroke = 0) +
    ggplot2::scale_color_identity() +
    ggplot2::labs(title = sprintf("%s\n%d / %d in %d%% credible set",
                                  title, sum(df$credible_set),
                                  nrow(df), as.integer(pp_threshold * 100)),
                  x = "position (bp)",
                  y = "posterior probability")
}

ecaa_coloc_pp_panel_r <- function(df, title = "") {
  # TODO(plan §S12.2): expand to match Python parity
  # (faceted 5-panel layout with H4 in sig-up color and 0.5 reference line;
  # currently uses tidyr::pivot_longer + facet_wrap without the H4 highlight).
  ecaa_apply_theme()
  required_cols <- c("region", "pp_h0", "pp_h1", "pp_h2", "pp_h3", "pp_h4")
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_coloc_pp_panel_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  long <- do.call(rbind, lapply(c("pp_h0", "pp_h1", "pp_h2", "pp_h3", "pp_h4"),
    function(col) data.frame(region = df$region, panel = col,
                             value = df[[col]],
                             stringsAsFactors = FALSE)))
  long$panel <- factor(long$panel,
                       levels = c("pp_h0", "pp_h1", "pp_h2", "pp_h3", "pp_h4"))
  pal <- THEME$palette
  panel_colors <- c(pal$non_sig %||% "#999999", "#bbbbbb", "#999999",
                    "#666666", pal$sig_up %||% "#D55E00")
  ggplot2::ggplot(long, ggplot2::aes(x = .data$value, y = .data$region,
                                     fill = .data$panel)) +
    ggplot2::geom_col(color = "#333333", linewidth = 0.2) +
    ggplot2::scale_fill_manual(values = stats::setNames(panel_colors,
        c("pp_h0","pp_h1","pp_h2","pp_h3","pp_h4")), guide = "none") +
    ggplot2::geom_vline(xintercept = 0.5, linetype = "dashed",
                        color = "#444444", linewidth = 0.3) +
    ggplot2::facet_wrap(~ .data$panel, nrow = 1, scales = "free_x") +
    ggplot2::scale_x_continuous(limits = c(0, 1)) +
    ggplot2::labs(title = sprintf("%s\n%d regions", title, nrow(df)),
                  x = "PP", y = NULL)
}

ecaa_forest_r <- function(df,
                          title = "",
                          null_value = 0.0,
                          xlabel = "effect size (95% CI)") {
  # TODO(plan §S12.2): expand to match Python parity
  # (weight-proportional marker sizing, "n / N CI excludes null" footer
  # annotation, sig-up vs sig-down color split based on CI exclusion side).
  ecaa_apply_theme()
  required_cols <- c("label", "effect", "ci_lo", "ci_hi")
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_forest_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  pal <- THEME$palette
  df$sig_pos <- df$ci_lo > null_value & df$effect >= null_value
  df$sig_neg <- df$ci_hi < null_value & df$effect <= null_value
  df$color <- ifelse(df$sig_pos, pal$sig_up %||% "#D55E00",
              ifelse(df$sig_neg, pal$sig_down %||% "#0072B2",
                     pal$non_sig %||% "#999999"))
  df$label <- factor(df$label, levels = rev(df$label))
  if (is.null(df$weight)) df$weight <- 1
  wmax <- max(df$weight, na.rm = TRUE)
  df$marker_size <- 2 + 4 * (df$weight / (if (wmax > 0) wmax else 1))

  ggplot2::ggplot(df, ggplot2::aes(x = .data$effect, y = .data$label,
                                   color = .data$color)) +
    ggplot2::geom_errorbarh(ggplot2::aes(xmin = .data$ci_lo,
                                         xmax = .data$ci_hi),
                            height = 0.2, linewidth = 0.5, alpha = 0.85) +
    ggplot2::geom_point(ggplot2::aes(size = .data$marker_size),
                        stroke = 0.4) +
    ggplot2::scale_color_identity() +
    ggplot2::scale_size_identity() +
    ggplot2::geom_vline(xintercept = null_value, linetype = "dashed",
                        color = "#444444", linewidth = 0.4) +
    ggplot2::labs(title = sprintf("%s\nn = %d studies", title, nrow(df)),
                  x = xlabel, y = NULL)
}

# ---------------------------------------------------------------------------
# Phase G (plan §S12.4-S12.6): clinical statistical figures.
# Kaplan-Meier, CONSORT, cumulative-incidence, spaghetti, adverse-event bar.
# Stubs match the Python primitive signatures from lib/plotting/core.py.
# Each is a ggplot2-based first cut; full visual parity (number-at-risk
# tables, Aalen-Johansen estimator parity, severity color-ramp matching)
# is tracked under TODO(plan §S12.6).
# ---------------------------------------------------------------------------

ecaa_kaplan_meier_r <- function(df,
                                title = "",
                                time_col = "time",
                                event_col = "event",
                                group_col = NULL,
                                show_at_risk_table = TRUE) {
  # TODO(plan §S12.6): expand to match Python parity
  # (per-group number-at-risk table beneath axis matching Python
  # `kaplan_meier`'s grid; censoring tick marks at exact follow-up times;
  # categorical_palette() group colors instead of ggplot2 default).
  ecaa_apply_theme()
  required_cols <- c(time_col, event_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_kaplan_meier_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  # Inline KM estimator: surv = product over event times of (1 - d_i / n_i).
  km_for <- function(time, event) {
    ord <- order(time)
    time <- time[ord]; event <- event[ord]
    ev_t <- sort(unique(time[event == 1]))
    if (length(ev_t) == 0) return(data.frame(t = 0, s = 1))
    surv <- 1; out <- numeric(length(ev_t))
    for (i in seq_along(ev_t)) {
      ti <- ev_t[i]
      n_at_risk <- sum(time >= ti)
      n_events <- sum(time == ti & event == 1)
      if (n_at_risk > 0) surv <- surv * (1 - n_events / n_at_risk)
      out[i] <- surv
    }
    data.frame(t = c(0, ev_t), s = c(1, out))
  }
  if (is.null(group_col)) {
    cdf <- km_for(df[[time_col]], df[[event_col]])
    cdf$group <- "all"
  } else {
    levels <- unique(as.character(df[[group_col]]))
    parts <- lapply(levels, function(lvl) {
      sub <- df[as.character(df[[group_col]]) == lvl, , drop = FALSE]
      part <- km_for(sub[[time_col]], sub[[event_col]])
      part$group <- lvl
      part
    })
    cdf <- do.call(rbind, parts)
  }
  ggplot2::ggplot(cdf, ggplot2::aes(x = .data$t, y = .data$s,
                                    color = .data$group)) +
    ggplot2::geom_step(linewidth = 0.6) +
    ggplot2::scale_y_continuous(limits = c(0, 1.05)) +
    ggplot2::labs(title = sprintf("%s\nn = %d", title, nrow(df)),
                  x = "time", y = "S(t)", color = NULL)
}

ecaa_consort_diagram_r <- function(flow, title = "") {
  # TODO(plan §S12.6): expand to match Python parity
  # (boxes drawn with grid::gpar instead of ggplot2 geom_rect; side-arrow
  # exclusion branches that match Python's right-hand placement; sig_up
  # accent color on exclusion edges).
  ecaa_apply_theme()
  required_keys <- c("enrolled", "randomized", "allocated",
                     "followed_up", "analyzed")
  missing_keys <- setdiff(required_keys, names(flow))
  if (length(missing_keys) > 0) {
    stop(sprintf("ecaa_consort_diagram_r missing keys: %s",
                 paste(missing_keys, collapse = ", ")))
  }
  rows <- data.frame(
    stage = factor(required_keys, levels = rev(required_keys)),
    n = vapply(required_keys, function(k) as.integer(flow[[k]]), integer(1)),
    stringsAsFactors = FALSE
  )
  rows$label <- sprintf("%s\n(n = %d)",
                        tools::toTitleCase(gsub("_", " ", as.character(rows$stage))),
                        rows$n)
  ggplot2::ggplot(rows, ggplot2::aes(x = 0, y = .data$stage)) +
    ggplot2::geom_tile(width = 0.6, height = 0.6,
                       fill = "#ffffff", color = "#333333", linewidth = 0.4) +
    ggplot2::geom_text(ggplot2::aes(label = .data$label),
                       size = 2.8) +
    ggplot2::scale_x_continuous(limits = c(-1, 1)) +
    ggplot2::labs(title = title, x = NULL, y = NULL) +
    ggplot2::theme(axis.text.x = ggplot2::element_blank(),
                   axis.ticks.x = ggplot2::element_blank())
}

ecaa_cumulative_incidence_r <- function(df,
                                        title = "",
                                        time_col = "time",
                                        event_col = "event",
                                        competing_col = NULL,
                                        group_col = NULL) {
  # TODO(plan §S12.6): expand to match Python parity
  # (per-cause × per-group line styles using linestyle ramp identical to
  # Python core; sig_up / sig_down accent color rotation per cause).
  ecaa_apply_theme()
  required_cols <- c(time_col, event_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_cumulative_incidence_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  evt <- as.integer(df[[event_col]])
  if (!is.null(competing_col)) evt <- evt * as.integer(df[[competing_col]])
  df$.evt <- evt
  causes <- sort(unique(evt[evt > 0]))
  if (length(causes) == 0) {
    return(ggplot2::ggplot() +
           ggplot2::annotate("text", x = 0.5, y = 0.5,
                             label = "no events observed") +
           ggplot2::labs(title = title))
  }
  aj_for <- function(time, event, cause) {
    ord <- order(time)
    time <- time[ord]; event <- event[ord]
    ev_t <- sort(unique(time[event > 0]))
    if (length(ev_t) == 0) return(data.frame(t = 0, cif = 0))
    surv <- 1; cif <- 0; out <- numeric(length(ev_t))
    for (i in seq_along(ev_t)) {
      ti <- ev_t[i]
      n_at_risk <- sum(time >= ti)
      n_cause <- sum(time == ti & event == cause)
      n_any <- sum(time == ti & event > 0)
      if (n_at_risk > 0) {
        cif <- cif + surv * (n_cause / n_at_risk)
        surv <- surv * (1 - n_any / n_at_risk)
      }
      out[i] <- cif
    }
    data.frame(t = c(0, ev_t), cif = c(0, out))
  }
  parts <- list()
  for (cause in causes) {
    if (is.null(group_col)) {
      part <- aj_for(df[[time_col]], df$.evt, cause)
      part$cause <- as.character(cause); part$group <- "all"
      parts[[length(parts) + 1L]] <- part
    } else {
      for (lvl in unique(as.character(df[[group_col]]))) {
        sub <- df[as.character(df[[group_col]]) == lvl, , drop = FALSE]
        part <- aj_for(sub[[time_col]], sub$.evt, cause)
        part$cause <- as.character(cause); part$group <- lvl
        parts[[length(parts) + 1L]] <- part
      }
    }
  }
  cdf <- do.call(rbind, parts)
  ggplot2::ggplot(cdf, ggplot2::aes(x = .data$t, y = .data$cif,
                                    color = .data$cause,
                                    linetype = .data$group)) +
    ggplot2::geom_step(linewidth = 0.6) +
    ggplot2::labs(title = sprintf("%s\n%d cause(s), n = %d",
                                  title, length(causes), nrow(df)),
                  x = "time", y = "cumulative incidence")
}

ecaa_spaghetti_r <- function(df,
                             title = "",
                             id_col = "id",
                             time_col = "time",
                             value_col = "value",
                             group_col = NULL,
                             show_mean = TRUE) {
  # TODO(plan §S12.6): expand to match Python parity
  # (per-group mean overlay using a thicker line; categorical_palette()
  # color split when group_col supplied; Python sets per-subject alpha=0.45
  # — match that exactly).
  ecaa_apply_theme()
  required_cols <- c(id_col, time_col, value_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_spaghetti_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  if (!is.null(group_col)) {
    p <- ggplot2::ggplot(df, ggplot2::aes(x = .data[[time_col]],
                                          y = .data[[value_col]],
                                          group = .data[[id_col]],
                                          color = .data[[group_col]]))
  } else {
    p <- ggplot2::ggplot(df, ggplot2::aes(x = .data[[time_col]],
                                          y = .data[[value_col]],
                                          group = .data[[id_col]]))
  }
  p <- p + ggplot2::geom_line(linewidth = 0.2, alpha = 0.45)
  if (show_mean) {
    if (!is.null(group_col)) {
      mean_df <- aggregate(df[[value_col]],
                           by = list(t = df[[time_col]],
                                     g = df[[group_col]]),
                           FUN = mean, na.rm = TRUE)
      names(mean_df) <- c(time_col, group_col, value_col)
      p <- p + ggplot2::geom_line(data = mean_df,
                                  ggplot2::aes(x = .data[[time_col]],
                                               y = .data[[value_col]],
                                               color = .data[[group_col]],
                                               group = .data[[group_col]]),
                                  linewidth = 0.8, alpha = 0.95)
    } else {
      mean_df <- aggregate(df[[value_col]],
                           by = list(t = df[[time_col]]),
                           FUN = mean, na.rm = TRUE)
      names(mean_df) <- c(time_col, value_col)
      p <- p + ggplot2::geom_line(data = mean_df,
                                  ggplot2::aes(x = .data[[time_col]],
                                               y = .data[[value_col]]),
                                  inherit.aes = FALSE,
                                  color = THEME$palette$sig_up %||% "#D55E00",
                                  linewidth = 0.8)
    }
  }
  p + ggplot2::labs(title = sprintf("%s\nn = %d",
                                    title, length(unique(df[[id_col]]))),
                    x = "time", y = value_col)
}

ecaa_adverse_event_bar_r <- function(df,
                                     title = "",
                                     term_col = "term",
                                     count_col = "count",
                                     severity_col = NULL,
                                     top_n = 20,
                                     horizontal = NULL) {
  # TODO(plan §S12.6): expand to match Python parity
  # (greys → sig_up severity-ramp palette identical to Python core; auto
  # horizontal/vertical orientation when n_terms > 10; legend title
  # "severity" when split column present).
  ecaa_apply_theme()
  required_cols <- c(term_col, count_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_adverse_event_bar_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  # Aggregate per term (and severity when present).
  if (is.null(severity_col)) {
    agg <- aggregate(df[[count_col]],
                     by = list(term = df[[term_col]]),
                     FUN = sum, na.rm = TRUE)
    names(agg) <- c(term_col, count_col)
  } else {
    agg <- aggregate(df[[count_col]],
                     by = list(term = df[[term_col]],
                               sev = df[[severity_col]]),
                     FUN = sum, na.rm = TRUE)
    names(agg) <- c(term_col, severity_col, count_col)
  }
  # Top-N by total count.
  totals <- aggregate(agg[[count_col]],
                      by = list(t = agg[[term_col]]),
                      FUN = sum, na.rm = TRUE)
  totals <- totals[order(-totals$x), , drop = FALSE]
  keep <- head(totals$t, top_n)
  agg <- agg[agg[[term_col]] %in% keep, , drop = FALSE]
  agg[[term_col]] <- factor(agg[[term_col]], levels = rev(keep))
  if (is.null(horizontal)) horizontal <- length(keep) > 10

  pal <- THEME$palette
  if (!is.null(severity_col)) {
    p <- ggplot2::ggplot(agg, ggplot2::aes(x = .data[[term_col]],
                                           y = .data[[count_col]],
                                           fill = .data[[severity_col]]))
  } else {
    p <- ggplot2::ggplot(agg, ggplot2::aes(x = .data[[term_col]],
                                           y = .data[[count_col]])) +
         ggplot2::geom_col(fill = pal$sig_up %||% "#D55E00",
                           color = "#333333", linewidth = 0.2)
  }
  if (!is.null(severity_col)) {
    p <- p + ggplot2::geom_col(color = "#333333", linewidth = 0.2)
  }
  if (isTRUE(horizontal)) p <- p + ggplot2::coord_flip()
  p + ggplot2::labs(title = sprintf("%s\n%d term(s)",
                                    title, length(keep)),
                    x = NULL, y = "count")
}

# ---------------------------------------------------------------------------
# Phase H (plan §S13.1-§S13.3) — sequencing/ChIP/ATAC, long-read RNA-seq,
# proteomics R parity stubs.
#
# Skeleton ggplot2 implementations matching Python signatures from
# lib/plotting/core.py. Each gets fleshed out under TODO(plan §S13.6) so
# the R side reaches visual parity (multi-track stacking, sashimi arcs,
# severity color ramps).
# ---------------------------------------------------------------------------

ecaa_profile_pileup_r <- function(df,
                                  title = "",
                                  position_col = "position",
                                  signal_col = "signal",
                                  group_col = NULL) {
  # TODO(plan §S13.6): expand to match Python parity
  # (per-group categorical_palette colors, vertical zero reference line,
  # mean per unique position aggregation matching the Python code path).
  ecaa_apply_theme()
  required_cols <- c(position_col, signal_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_profile_pileup_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  pal <- THEME$palette
  if (!is.null(group_col)) {
    p <- ggplot2::ggplot(df, ggplot2::aes(x = .data[[position_col]],
                                          y = .data[[signal_col]],
                                          color = .data[[group_col]])) +
      ggplot2::stat_summary(fun = mean, geom = "line", linewidth = 0.6)
  } else {
    p <- ggplot2::ggplot(df, ggplot2::aes(x = .data[[position_col]],
                                          y = .data[[signal_col]])) +
      ggplot2::stat_summary(fun = mean, geom = "line",
                            linewidth = 0.6,
                            color = pal$sig_up %||% "#D55E00")
  }
  p + ggplot2::geom_vline(xintercept = 0, linetype = "dashed",
                          color = "#444444", linewidth = 0.3) +
    ggplot2::labs(title = sprintf("%s\nn = %d rows", title, nrow(df)),
                  x = "distance from peak center (bp)",
                  y = "mean signal")
}

ecaa_coverage_track_r <- function(df,
                                  title = "",
                                  chrom_col = "chrom",
                                  pos_col = "pos",
                                  depth_col = "depth",
                                  region = NULL) {
  # TODO(plan §S13.6): expand to match Python parity
  # (region clip + most-populated-chromosome auto-default; fill_between-style
  # shaded depth with line overlay; IGV color convention).
  ecaa_apply_theme()
  required_cols <- c(chrom_col, pos_col, depth_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_coverage_track_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  if (!is.null(region) && length(region) == 3) {
    rchrom <- as.character(region[[1]])
    rstart <- as.integer(region[[2]])
    rend <- as.integer(region[[3]])
    df <- df[as.character(df[[chrom_col]]) == rchrom &
             df[[pos_col]] >= rstart & df[[pos_col]] <= rend, , drop = FALSE]
  } else {
    counts <- table(df[[chrom_col]])
    if (length(counts) > 0) {
      top_chrom <- names(counts)[which.max(counts)]
      df <- df[as.character(df[[chrom_col]]) == top_chrom, , drop = FALSE]
    }
  }
  pal <- THEME$palette
  ggplot2::ggplot(df, ggplot2::aes(x = .data[[pos_col]],
                                   y = .data[[depth_col]])) +
    ggplot2::geom_area(fill = pal$non_sig %||% "#999999", alpha = 0.35) +
    ggplot2::geom_line(color = pal$sig_up %||% "#D55E00", linewidth = 0.3) +
    ggplot2::labs(title = sprintf("%s\nn = %d positions", title, nrow(df)),
                  x = "position", y = "depth")
}

ecaa_peak_saturation_r <- function(df,
                                   title = "",
                                   depth_col = "depth",
                                   peaks_called_col = "peaks_called",
                                   group_col = NULL) {
  # TODO(plan §S13.6): expand to match Python parity
  # (markersize=3 marker spec, per-group categorical_palette).
  ecaa_apply_theme()
  required_cols <- c(depth_col, peaks_called_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_peak_saturation_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  pal <- THEME$palette
  if (!is.null(group_col)) {
    p <- ggplot2::ggplot(df, ggplot2::aes(x = .data[[depth_col]],
                                          y = .data[[peaks_called_col]],
                                          color = .data[[group_col]]))
  } else {
    p <- ggplot2::ggplot(df, ggplot2::aes(x = .data[[depth_col]],
                                          y = .data[[peaks_called_col]])) +
      ggplot2::geom_line(color = pal$sig_up %||% "#D55E00", linewidth = 0.4) +
      ggplot2::geom_point(color = pal$sig_up %||% "#D55E00", size = 1.0)
  }
  if (!is.null(group_col)) {
    p <- p + ggplot2::geom_line(linewidth = 0.4) +
             ggplot2::geom_point(size = 1.0)
  }
  p + ggplot2::labs(title = sprintf("%s\nn = %d subsamples", title, nrow(df)),
                    x = "read depth (subsampled)", y = "peaks called")
}

ecaa_isoform_structure_r <- function(df,
                                     title = "",
                                     transcript_col = "transcript",
                                     exon_starts_col = "exon_starts",
                                     exon_ends_col = "exon_ends",
                                     strand_col = NULL) {
  # TODO(plan §S13.6): expand to match Python parity
  # (packed list-column unpacking; intron line + exon rectangle stack;
  # strand arrow at 3' end of every transcript model).
  ecaa_apply_theme()
  required_cols <- c(transcript_col, exon_starts_col, exon_ends_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_isoform_structure_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  pal <- THEME$palette
  ggplot2::ggplot(df, ggplot2::aes(xmin = .data[[exon_starts_col]],
                                   xmax = .data[[exon_ends_col]],
                                   y = .data[[transcript_col]])) +
    ggplot2::geom_errorbarh(height = 0.45,
                            color = pal$sig_up %||% "#D55E00",
                            linewidth = 0.6) +
    ggplot2::labs(title = sprintf("%s\n%d transcript(s)",
                                  title,
                                  length(unique(df[[transcript_col]]))),
                  x = "genomic position (bp)", y = NULL)
}

ecaa_sashimi_r <- function(df,
                           title = "",
                           junction_col = "junction",
                           count_col = "count") {
  # TODO(plan §S13.6): expand to match Python parity
  # (parametric arc draw matching the Python half-ellipse sweep; arc
  # thickness ~ sqrt(count); count label at apex of each arc).
  ecaa_apply_theme()
  required_cols <- c(junction_col, count_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_sashimi_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  parsed <- do.call(rbind, lapply(df[[junction_col]], function(j) {
    s <- as.character(j)
    parts <- strsplit(s, "-", fixed = TRUE)[[1]]
    if (length(parts) == 2) {
      data.frame(start = suppressWarnings(as.numeric(parts[1])),
                 end = suppressWarnings(as.numeric(parts[2])))
    } else {
      data.frame(start = NA_real_, end = NA_real_)
    }
  }))
  df$.start <- parsed$start
  df$.end <- parsed$end
  valid <- !is.na(df$.start) & !is.na(df$.end)
  pal <- THEME$palette
  ggplot2::ggplot(df[valid, , drop = FALSE],
                  ggplot2::aes(x = .data$.start, xend = .data$.end,
                               y = 0, yend = 0)) +
    ggplot2::geom_curve(ggplot2::aes(linewidth = .data[[count_col]]),
                        curvature = -0.3,
                        color = pal$sig_up %||% "#D55E00", alpha = 0.7) +
    ggplot2::scale_linewidth_continuous(range = c(0.3, 2.0)) +
    ggplot2::labs(title = sprintf("%s\nn = %d junctions",
                                  title, sum(valid)),
                  x = "genomic position (bp)", y = NULL)
}

ecaa_peptide_coverage_r <- function(df,
                                    title = "",
                                    position_col = "position",
                                    coverage_col = "coverage") {
  # TODO(plan §S13.6): expand to match Python parity
  # (step-fill_between equivalent via geom_area with step interpolation;
  # coverage percentage in title).
  ecaa_apply_theme()
  required_cols <- c(position_col, coverage_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_peptide_coverage_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  cov_pct <- if (nrow(df) > 0) {
    100 * sum(df[[coverage_col]] > 0, na.rm = TRUE) / nrow(df)
  } else {
    0
  }
  pal <- THEME$palette
  ggplot2::ggplot(df, ggplot2::aes(x = .data[[position_col]],
                                   y = .data[[coverage_col]])) +
    ggplot2::geom_area(fill = pal$non_sig %||% "#999999", alpha = 0.35) +
    ggplot2::geom_step(color = pal$sig_up %||% "#D55E00", linewidth = 0.4) +
    ggplot2::labs(title = sprintf("%s\n%.1f%% residues covered, n = %d",
                                  title, cov_pct, nrow(df)),
                  x = "residue position", y = "coverage")
}

ecaa_ridgeline_r <- function(df,
                             title = "",
                             group_col = "group",
                             value_col = "value",
                             bandwidth = 0.4,
                             overlap = 0.7) {
  # TODO(plan §S13.6): expand to match Python parity
  # (manual KDE + per-row scaling with categorical_palette; ggridges::geom_density_ridges
  # is the natural import target but we want zero-runtime-dep parity).
  ecaa_apply_theme()
  required_cols <- c(group_col, value_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_ridgeline_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  ggplot2::ggplot(df, ggplot2::aes(x = .data[[value_col]],
                                   y = .data[[group_col]],
                                   fill = .data[[group_col]])) +
    ggplot2::geom_violin(alpha = 0.7, color = "#333333", linewidth = 0.4,
                         scale = "width") +
    ggplot2::guides(fill = "none") +
    ggplot2::labs(title = sprintf("%s\n%d group(s), n = %d",
                                  title,
                                  length(unique(df[[group_col]])),
                                  nrow(df)),
                  x = value_col, y = NULL)
}

# ---------------------------------------------------------------------------
# Phase I (plan §S13.4-§S13.5) — metagenomics + spatial transcriptomics R stubs.
# ---------------------------------------------------------------------------

ecaa_taxonomic_stacked_bar_r <- function(df,
                                         title = "",
                                         sample_col = "sample",
                                         taxon_col = "taxon",
                                         abundance_col = "abundance",
                                         top_n = 12,
                                         horizontal = FALSE) {
  # TODO(plan §S13.6): expand to match Python parity
  # (top-N taxon retention with "Other" rollup; row-sum normalization for
  # relative abundance; categorical_palette() for taxon segments).
  ecaa_apply_theme()
  required_cols <- c(sample_col, taxon_col, abundance_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_taxonomic_stacked_bar_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  agg <- aggregate(df[[abundance_col]],
                   by = list(s = df[[sample_col]],
                             t = df[[taxon_col]]),
                   FUN = sum, na.rm = TRUE)
  names(agg) <- c(sample_col, taxon_col, abundance_col)
  totals <- aggregate(agg[[abundance_col]],
                      by = list(t = agg[[taxon_col]]),
                      FUN = sum, na.rm = TRUE)
  totals <- totals[order(-totals$x), , drop = FALSE]
  keep <- head(totals$t, top_n)
  agg[[taxon_col]] <- ifelse(agg[[taxon_col]] %in% keep,
                             as.character(agg[[taxon_col]]),
                             "Other")
  p <- ggplot2::ggplot(agg, ggplot2::aes(x = .data[[sample_col]],
                                         y = .data[[abundance_col]],
                                         fill = .data[[taxon_col]])) +
    ggplot2::geom_col(position = "fill",
                      color = "#333333", linewidth = 0.2) +
    ggplot2::labs(title = sprintf("%s\n%d sample(s)", title,
                                  length(unique(agg[[sample_col]]))),
                  x = NULL, y = "relative abundance")
  if (isTRUE(horizontal)) p <- p + ggplot2::coord_flip()
  p
}

ecaa_diversity_violin_r <- function(df,
                                    title = "",
                                    group_col = "group",
                                    diversity_col = "diversity") {
  # TODO(plan §S13.6): expand to match Python parity
  # (per-group categorical_palette fill; subtle dashed median/whisker
  # overlay matching Python core).
  ecaa_apply_theme()
  required_cols <- c(group_col, diversity_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_diversity_violin_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  ggplot2::ggplot(df, ggplot2::aes(x = .data[[group_col]],
                                   y = .data[[diversity_col]],
                                   fill = .data[[group_col]])) +
    ggplot2::geom_violin(alpha = 0.7, color = "#333333", linewidth = 0.4,
                         scale = "width") +
    ggplot2::stat_summary(fun = median, geom = "point", size = 1.0,
                          color = "#333333") +
    ggplot2::guides(fill = "none") +
    ggplot2::labs(title = sprintf("%s\n%d group(s), n = %d",
                                  title,
                                  length(unique(df[[group_col]])),
                                  nrow(df)),
                  x = group_col, y = diversity_col)
}

ecaa_tissue_overlay_r <- function(coords_df, image,
                                  title = "",
                                  value_col = "value",
                                  x_col = "x",
                                  y_col = "y",
                                  cmap = "viridis",
                                  point_size = 1.5,
                                  image_alpha = 0.6) {
  # TODO(plan §S13.6): expand to match Python parity
  # (raster image background draw via grid::rasterGrob underlay; flipped
  # y-axis to match imshow convention; viridis colorbar matching Python).
  ecaa_apply_theme()
  required_cols <- c(x_col, y_col, value_col)
  missing_cols <- setdiff(required_cols, names(coords_df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_tissue_overlay_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  ggplot2::ggplot(coords_df, ggplot2::aes(x = .data[[x_col]],
                                          y = .data[[y_col]],
                                          color = .data[[value_col]])) +
    ggplot2::geom_point(size = point_size, alpha = 0.9, stroke = 0.2) +
    ggplot2::scale_y_reverse() +
    ggplot2::labs(title = sprintf("%s\nn = %d spots",
                                  title, nrow(coords_df)),
                  x = "x", y = "y", color = value_col)
}

ecaa_morans_i_scatter_r <- function(df,
                                    title = "",
                                    gene_col = "gene",
                                    morans_i_col = "morans_i",
                                    p_col = "p_value",
                                    sig_threshold = 0.05,
                                    label_top_n = 10) {
  # TODO(plan §S13.6): expand to match Python parity
  # (greedy collision-avoidance label placement matching volcano helper;
  # vertical zero reference; sig_up vs non_sig color split below threshold).
  ecaa_apply_theme()
  required_cols <- c(gene_col, morans_i_col, p_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_morans_i_scatter_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  df$.nlogp <- -log10(pmax(df[[p_col]], 1e-300))
  df$.sig <- df[[p_col]] < sig_threshold
  pal <- THEME$palette
  ggplot2::ggplot(df, ggplot2::aes(x = .data[[morans_i_col]],
                                   y = .data$.nlogp,
                                   color = .data$.sig)) +
    ggplot2::geom_point(size = 0.8, alpha = 0.85, stroke = 0) +
    ggplot2::geom_hline(yintercept = -log10(sig_threshold),
                        linetype = "dashed", color = "#444444",
                        linewidth = 0.3) +
    ggplot2::geom_vline(xintercept = 0, linetype = "dashed",
                        color = "#444444", linewidth = 0.3) +
    ggplot2::scale_color_manual(values = c(`FALSE` = pal$non_sig %||% "#999999",
                                           `TRUE` = pal$sig_up %||% "#D55E00"),
                                guide = "none") +
    ggplot2::labs(title = sprintf("%s\n%d / %d genes p < %.3f",
                                  title, sum(df$.sig), nrow(df),
                                  sig_threshold),
                  x = "Moran's I",
                  y = expression(-log[10] ~ italic(p)))
}

ecaa_neighborhood_enrichment_r <- function(df,
                                           title = "",
                                           source_col = "source",
                                           target_col = "target",
                                           score_col = "score",
                                           cmap = "RdBu_r",
                                           vmax = NULL) {
  # TODO(plan §S13.6): expand to match Python parity
  # (symmetric divergent colormap centered on zero; vmax auto-clip from
  # max(|score|); first-seen label ordering across source ∪ target).
  ecaa_apply_theme()
  required_cols <- c(source_col, target_col, score_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_neighborhood_enrichment_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  if (is.null(vmax)) {
    vmax <- max(abs(df[[score_col]]), na.rm = TRUE)
    if (!is.finite(vmax) || vmax <= 0) vmax <- 1
  }
  labels <- unique(c(as.character(df[[source_col]]),
                     as.character(df[[target_col]])))
  df[[source_col]] <- factor(df[[source_col]], levels = labels)
  df[[target_col]] <- factor(df[[target_col]], levels = labels)
  ggplot2::ggplot(df, ggplot2::aes(x = .data[[target_col]],
                                   y = .data[[source_col]],
                                   fill = .data[[score_col]])) +
    ggplot2::geom_tile() +
    ggplot2::scale_fill_gradient2(low = "#2166AC", mid = "#F7F7F7",
                                  high = "#B2182B", midpoint = 0,
                                  limits = c(-vmax, vmax)) +
    ggplot2::labs(title = sprintf("%s\n%d × %d", title,
                                  length(labels), length(labels)),
                  x = NULL, y = NULL, fill = "z-score") +
    ggplot2::theme(axis.text.x = ggplot2::element_text(angle = 45,
                                                       hjust = 1))
}

ecaa_forecast_ribbon_r <- function(df,
                                   title = "",
                                   time_col = "time",
                                   value_col = "forecast",
                                   lower_col = "lower",
                                   upper_col = "upper",
                                   actual_col = "actual") {
  # TODO(plan §S14.6): expand to match Python parity
  # (sort by time before plotting; sig_up forecast line + sig_down actual
  # overlay; "prediction interval" legend entry on the ribbon; exact
  # alpha=0.30 ribbon shading using non_sig palette color).
  ecaa_apply_theme()
  required_cols <- c(time_col, value_col, lower_col, upper_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_forecast_ribbon_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  pal <- THEME$palette
  p <- ggplot2::ggplot(df, ggplot2::aes(x = .data[[time_col]])) +
    ggplot2::geom_ribbon(ggplot2::aes(ymin = .data[[lower_col]],
                                      ymax = .data[[upper_col]]),
                         fill = pal$non_sig %||% "#999999",
                         alpha = 0.30) +
    ggplot2::geom_line(ggplot2::aes(y = .data[[value_col]]),
                       color = pal$sig_up %||% "#D55E00",
                       linewidth = 0.6)
  if (!is.null(actual_col) && actual_col %in% names(df)) {
    p <- p + ggplot2::geom_line(ggplot2::aes(y = .data[[actual_col]]),
                                color = pal$sig_down %||% "#0072B2",
                                linewidth = 0.4, alpha = 0.85)
  }
  p + ggplot2::labs(title = sprintf("%s\nn = %d forecast points",
                                    title, nrow(df)),
                    x = time_col, y = value_col)
}

ecaa_acf_pacf_panel_r <- function(df,
                                  title = "",
                                  value_col = "value",
                                  max_lag = 40,
                                  ci_level = 0.95) {
  # TODO(plan §S14.6): expand to match Python parity
  # (Durbin-Levinson PACF recursion matching Python's inline implementation;
  # Bartlett ±z/√n CI band shading via geom_ribbon; sig_up vs non_sig
  # color split on lags exceeding the band; paired ACF/PACF facets sharing
  # the lag axis with hspace=0.15 and shared ylim=(-1.05, 1.05)).
  ecaa_apply_theme()
  required_cols <- c(value_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_acf_pacf_panel_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  s <- df[[value_col]]
  n <- length(s)
  max_lag <- min(as.integer(max_lag), max(1, n - 1))
  acf_vals <- as.numeric(stats::acf(s, lag.max = max_lag, plot = FALSE)$acf)
  pacf_vals <- c(1.0,
                 as.numeric(stats::pacf(s, lag.max = max_lag,
                                        plot = FALSE)$acf))
  band <- 1.96 / sqrt(n)
  pal <- THEME$palette
  long <- data.frame(
    lag = rep(seq_len(max_lag + 1) - 1, 2),
    value = c(acf_vals, pacf_vals),
    panel = factor(rep(c("ACF", "PACF"), each = max_lag + 1),
                   levels = c("ACF", "PACF"))
  )
  long$sig <- abs(long$value) > band
  ggplot2::ggplot(long, ggplot2::aes(x = lag, y = value)) +
    ggplot2::geom_hline(yintercept = 0, color = "#444444",
                        linewidth = 0.2) +
    ggplot2::geom_ribbon(ggplot2::aes(ymin = -band, ymax = band),
                         fill = "#cccccc", alpha = 0.5) +
    ggplot2::geom_segment(ggplot2::aes(xend = lag, yend = 0,
                                       color = sig),
                          linewidth = 0.4) +
    ggplot2::geom_point(ggplot2::aes(color = sig), size = 0.8) +
    ggplot2::scale_color_manual(values = c(`FALSE` = pal$non_sig %||% "#999999",
                                           `TRUE` = pal$sig_up %||% "#D55E00"),
                                guide = "none") +
    ggplot2::facet_wrap(~ panel, ncol = 1, strip.position = "left") +
    ggplot2::labs(title = sprintf("%s\nn = %d, max lag = %d",
                                  title, n, max_lag),
                  x = "lag", y = NULL) +
    ggplot2::ylim(-1.05, 1.05)
}

ecaa_decomposition_panel_r <- function(df,
                                       title = "",
                                       time_col = "time",
                                       value_col = "value",
                                       period = NULL) {
  # TODO(plan §S14.6): expand to match Python parity
  # (centered moving-average trend with odd-window padding; per-phase
  # seasonal mean re-centered to zero; residual NaN where trend is NaN
  # so edge effects don't corrupt downstream plotting; observed/trend/
  # seasonal/residual stacked in that exact order using the matching
  # palette colors — sig_up trend, sig_down seasonal, non_sig residual).
  ecaa_apply_theme()
  required_cols <- c(time_col, value_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_decomposition_panel_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  n <- nrow(df)
  if (is.null(period)) {
    period <- max(2L, min(as.integer(n %/% 4L), 365L))
  }
  period <- max(2L, as.integer(period))
  ord <- order(df[[time_col]])
  t <- df[[time_col]][ord]
  y <- df[[value_col]][ord]
  freq <- if (period >= 2L) period else 2L
  ts_obj <- stats::ts(y, frequency = freq)
  if (n >= 2 * freq) {
    dec <- stats::decompose(ts_obj, type = "additive")
    trend <- as.numeric(dec$trend)
    seasonal <- as.numeric(dec$seasonal)
    residual <- as.numeric(dec$random)
  } else {
    trend <- rep(NA_real_, n)
    seasonal <- rep(0, n)
    residual <- rep(NA_real_, n)
  }
  pal <- THEME$palette
  long <- data.frame(
    t = rep(t, 4),
    value = c(y, trend, seasonal, residual),
    panel = factor(rep(c("observed", "trend", "seasonal", "residual"),
                       each = n),
                   levels = c("observed", "trend", "seasonal", "residual"))
  )
  ggplot2::ggplot(long, ggplot2::aes(x = t, y = value, color = panel)) +
    ggplot2::geom_line(linewidth = 0.4) +
    ggplot2::scale_color_manual(values = c(
      observed = "#222222",
      trend = pal$sig_up %||% "#D55E00",
      seasonal = pal$sig_down %||% "#0072B2",
      residual = pal$non_sig %||% "#999999"
    ), guide = "none") +
    ggplot2::facet_wrap(~ panel, ncol = 1, scales = "free_y",
                        strip.position = "left") +
    ggplot2::labs(title = sprintf("%s\nn = %d, period = %d",
                                  title, n, period),
                  x = time_col, y = NULL)
}

ecaa_anomaly_timeline_r <- function(df,
                                    title = "",
                                    time_col = "time",
                                    value_col = "value",
                                    anomaly_col = "is_anomaly") {
  # TODO(plan §S14.6): expand to match Python parity
  # (merge contiguous runs of anomaly=TRUE into single geom_rect spans
  # rather than per-point shading so a long incident reads as one block;
  # legend entry "anomaly · n=<count>" only when any anomalies present;
  # sig_up alpha=0.18 fill on rectangles + alpha=1.0 marker color).
  ecaa_apply_theme()
  required_cols <- c(time_col, value_col, anomaly_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_anomaly_timeline_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  pal <- THEME$palette
  ord <- order(df[[time_col]])
  d <- df[ord, , drop = FALSE]
  d[[anomaly_col]] <- as.logical(d[[anomaly_col]])
  p <- ggplot2::ggplot(d, ggplot2::aes(x = .data[[time_col]],
                                       y = .data[[value_col]])) +
    ggplot2::geom_line(color = "#222222", linewidth = 0.3)
  if (any(d[[anomaly_col]], na.rm = TRUE)) {
    anom <- d[d[[anomaly_col]], , drop = FALSE]
    p <- p + ggplot2::geom_point(data = anom,
                                 color = pal$sig_up %||% "#D55E00",
                                 size = 1.2)
  }
  p + ggplot2::labs(title = sprintf("%s\nn = %d, anomalies = %d",
                                    title, nrow(d),
                                    sum(d[[anomaly_col]], na.rm = TRUE)),
                    x = time_col, y = value_col)
}

ecaa_ma_plot_r <- function(df,
                           title = "",
                           mean_col = "base_mean",
                           lfc_col = "log2FoldChange",
                           padj_col = "padj",
                           padj_threshold = 0.05,
                           fc_threshold = 1.0,
                           label_top_n = 10,
                           gene_col = "gene") {
  # TODO(plan §S14.6): expand to match Python parity
  # (greedy collision-avoidance label placement matching volcano helper;
  # up/down corner counts in sig_up/sig_down bold; horizontal zero +
  # ±fc_threshold guides; matching log10(mean) clip to 1e-9; rasterize
  # at scatter when n > rasterize_threshold_n).
  ecaa_apply_theme()
  required_cols <- c(mean_col, lfc_col, padj_col)
  missing_cols <- setdiff(required_cols, names(df))
  if (length(missing_cols) > 0) {
    stop(sprintf("ecaa_ma_plot_r missing columns: %s",
                 paste(missing_cols, collapse = ", ")))
  }
  pal <- THEME$palette
  df$.log_mean <- log10(pmax(df[[mean_col]], 1e-9))
  df$.sig <- df[[padj_col]] < padj_threshold &
    abs(df[[lfc_col]]) >= fc_threshold
  df$.dir <- ifelse(df$.sig & df[[lfc_col]] > 0, "up",
                    ifelse(df$.sig & df[[lfc_col]] < 0, "down", "ns"))
  ggplot2::ggplot(df, ggplot2::aes(x = .data$.log_mean,
                                   y = .data[[lfc_col]],
                                   color = .data$.dir)) +
    ggplot2::geom_point(size = 0.7, alpha = 0.75, stroke = 0) +
    ggplot2::geom_hline(yintercept = 0, color = "#444444",
                        linewidth = 0.3) +
    ggplot2::geom_hline(yintercept = c(-fc_threshold, fc_threshold),
                        linetype = "dashed", color = "#444444",
                        linewidth = 0.25) +
    ggplot2::scale_color_manual(values = c(
      ns = pal$non_sig %||% "#999999",
      up = pal$sig_up %||% "#D55E00",
      down = pal$sig_down %||% "#0072B2"
    ), guide = "none") +
    ggplot2::labs(title = sprintf("%s\nn = %d features",
                                  title, nrow(df)),
                  x = expression(log[10] ~ "mean expression"),
                  y = expression(log[2] ~ "fold change"))
}

ecaa_dashboard_grid_r <- function(panels,
                                  title = "",
                                  layout = c(2L, 2L)) {
  # TODO(plan §S14.6): expand to match Python parity
  # (route per-panel by `type` to existing ecaa_* primitives; numbered
  # subtitles "(N) <subtitle>" via patchwork::plot_annotation tag_levels;
  # unrenderable / error fallbacks render a placeholder ggplot rather
  # than crash the whole grid; deterministic theme inheritance one level
  # up at the patchwork::wrap_plots level).
  if (!requireNamespace("patchwork", quietly = TRUE)) {
    stop("ecaa_dashboard_grid_r requires the 'patchwork' package")
  }
  rows <- as.integer(layout[1])
  cols <- as.integer(layout[2])
  if (rows < 1L || cols < 1L) {
    stop(sprintf("ecaa_dashboard_grid_r layout must be positive (got %s)",
                 paste(layout, collapse = "×")))
  }
  ecaa_apply_theme()
  blank <- ggplot2::ggplot() + ggplot2::theme_void()
  plots <- vector("list", rows * cols)
  for (i in seq_len(rows * cols)) {
    if (i > length(panels)) {
      plots[[i]] <- blank
      next
    }
    panel <- panels[[i]]
    ptype <- panel$type %||% ""
    sub <- panel$args$subtitle %||% ptype
    plots[[i]] <- blank + ggplot2::labs(
      title = sprintf("(%d) %s", i, sub),
      subtitle = sprintf("(unrenderable: %s)", ptype)
    )
  }
  patchwork::wrap_plots(plots, nrow = rows, ncol = cols) +
    patchwork::plot_annotation(title = title)
}
