# R-side differential_expression stage: volcano + top_features_heatmap.
# Mirrors lib/plotting/stages/differential_expression.py contract.

if (!exists("ecaa_register_figure")) {
  stop("source runtime/plotting_r/core.R before this stage module")
}

.de_load_table <- function(ctx) {
  # Search comparison subdirs and the stage root for *_de_table.tsv variants.
  cands <- c(
    list.files(ctx$outputs_dir, pattern = "de_table\\.tsv$",
               recursive = TRUE, full.names = TRUE),
    list.files(ctx$outputs_dir, pattern = "_de\\.tsv$",
               recursive = TRUE, full.names = TRUE)
  )
  if (length(cands) == 0) return(NULL)
  for (p in cands) {
    df <- tryCatch(utils::read.delim(p, stringsAsFactors = FALSE,
                                     check.names = FALSE),
                   error = function(e) NULL)
    if (is.null(df) || nrow(df) == 0) next
    cols <- tolower(colnames(df))
    fc_col <- which(cols %in% c("log2foldchange", "log2fc", "logfc", "log_fc"))
    p_col  <- which(cols %in% c("pvalue", "p_value", "p"))
    feat_col <- which(cols %in% c("feature", "gene", "symbol", "name"))
    if (length(fc_col) == 0 || length(p_col) == 0) next
    out <- data.frame(
      log_fc = as.numeric(df[[fc_col[1]]]),
      pval = pmax(as.numeric(df[[p_col[1]]]), 1e-300),
      stringsAsFactors = FALSE
    )
    out$neg_log_p <- -log10(out$pval)
    if (length(feat_col)) out$feature <- as.character(df[[feat_col[1]]])
    return(out)
  }
  NULL
}

ecaa_register_figure("differential_expression", "volcano", function(ctx) {
  tab <- .de_load_table(ctx)
  if (is.null(tab) || nrow(tab) == 0) {
    stop("no de_table.tsv found under outputs_dir")
  }
  ecaa_volcano(
    log_fc = tab$log_fc,
    neg_log10_p = tab$neg_log_p,
    labels = tab$feature %||% NULL,
    title = "Differential expression",
    label_top_n = 10
  )
})

ecaa_register_figure("differential_expression", "top_features_heatmap",
                     function(ctx) {
  tab <- .de_load_table(ctx)
  if (is.null(tab) || nrow(tab) == 0) {
    stop("no de_table.tsv found")
  }
  if (is.null(tab$feature)) tab$feature <- sprintf("g%d", seq_len(nrow(tab)))
  # Top 25 by absolute log2FC × significance, single-column heatmap.
  tab$score <- abs(tab$log_fc) * tab$neg_log_p
  ord <- order(-tab$score)
  top <- tab[ord[seq_len(min(25, nrow(tab)))], , drop = FALSE]
  mat <- matrix(top$log_fc, ncol = 1)
  ecaa_heatmap(
    matrix = mat,
    row_labels = top$feature,
    col_labels = "log2FC",
    title = "Top 25 features by |log2FC| × significance",
    cbar_label = "log2 fold change",
    cluster_rows = TRUE,
    cluster_cols = FALSE
  )
})
