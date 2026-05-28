# R-side clustering stage figures: umap_clusters, cluster_size_bar.
# Mirrors the Python lib/plotting/stages/clustering.py contract — same
# input file conventions, same figure_id catalog.

# Stage modules expect runtime/plotting_r/core.R to be sourced before this
# file. The dispatcher in core.R::swfc_generate handles that automatically;
# stand-alone usage should `source("runtime/plotting_r/core.R")` first.
if (!exists("swfc_register_figure")) {
  stop("source runtime/plotting_r/core.R before this stage module")
}

.cluster_iter_runs <- function(ctx) {
  runs <- ctx$manifest$compartments
  if (is.null(runs) || length(runs) == 0) runs <- ctx$manifest$runs
  if (is.null(runs) || length(runs) == 0) runs <- list(list(id = "run"))
  runs
}

.cluster_load_labels <- function(ctx, run) {
  run_id <- run$id %||% "run"
  for (p in c(
    file.path(ctx$outputs_dir, run_id, "cluster_labels.tsv"),
    file.path(ctx$outputs_dir, "cluster_labels.tsv")
  )) {
    if (file.exists(p)) {
      df <- utils::read.delim(p, stringsAsFactors = FALSE, check.names = FALSE)
      cols <- tolower(colnames(df))
      cluster_col <- which(cols == "cluster")
      if (length(cluster_col) == 0) next
      x_col <- which(cols == "x"); y_col <- which(cols == "y")
      x <- if (length(x_col)) df[[x_col]] else rep(0.0, nrow(df))
      y <- if (length(y_col)) df[[y_col]] else rep(0.0, nrow(df))
      return(list(cluster = as.character(df[[cluster_col]]), x = x, y = y))
    }
  }
  NULL
}

.cluster_load_sizes <- function(ctx, run) {
  run_id <- run$id %||% "run"
  if (!is.null(run$clusters) && is.list(run$clusters)) {
    out <- sapply(run$clusters, function(c) {
      n <- c$n_cells %||% c$n_members %||% 0
      stats::setNames(as.integer(n), as.character(c$id))
    })
    if (length(out) > 0) return(unlist(out))
  }
  for (p in c(
    file.path(ctx$outputs_dir, run_id, "cluster_sizes.json"),
    file.path(ctx$outputs_dir, "cluster_sizes.json")
  )) {
    if (file.exists(p)) {
      data <- jsonlite::fromJSON(p, simplifyVector = TRUE)
      if (is.list(data) || is.numeric(data)) return(data)
    }
  }
  NULL
}

swfc_register_figure("clustering", "umap_clusters", function(ctx) {
  runs <- .cluster_iter_runs(ctx)
  chosen <- NULL
  for (run in runs) {
    loaded <- .cluster_load_labels(ctx, run)
    if (is.null(loaded)) next
    if (length(loaded$cluster) > 0 &&
        (any(loaded$x != 0) || any(loaded$y != 0))) {
      chosen <- list(id = run$id %||% "run", loaded = loaded)
      break
    }
  }
  if (is.null(chosen)) stop("no cluster_labels.tsv with x/y columns")
  uniq <- sort(unique(chosen$loaded$cluster))
  pal <- swfc_palette(length(uniq), name = paste0("clustering.", chosen$id))
  df <- data.frame(
    x = chosen$loaded$x, y = chosen$loaded$y,
    cluster = factor(chosen$loaded$cluster, levels = uniq)
  )
  ggplot2::ggplot(df, ggplot2::aes(.data$x, .data$y, color = .data$cluster)) +
    ggplot2::geom_point(size = 0.3, alpha = 0.6, stroke = 0) +
    ggplot2::scale_color_manual(values = stats::setNames(pal, uniq)) +
    ggplot2::labs(
      title = sprintf("Clusters — %s (n = %s cells)",
                      chosen$id, format(nrow(df), big.mark = ",")),
      x = "UMAP 1", y = "UMAP 2"
    ) +
    ggplot2::coord_fixed() +
    ggplot2::guides(color = ggplot2::guide_legend(
      override.aes = list(size = 2, alpha = 1)
    ))
})

swfc_register_figure("clustering", "cluster_size_bar", function(ctx) {
  runs <- .cluster_iter_runs(ctx)
  for (run in runs) {
    sizes <- .cluster_load_sizes(ctx, run)
    if (!is.null(sizes) && length(sizes) > 0) {
      names_v <- as.character(names(sizes) %||% seq_along(sizes))
      values <- as.numeric(sizes)
      return(swfc_bar(
        names = names_v, values = values,
        title = sprintf("Cluster sizes — %s", run$id %||% "run"),
        ylabel = "n members", xlabel = "cluster"
      ))
    }
  }
  stop("no cluster_sizes.json or cluster_labels for any run")
})
