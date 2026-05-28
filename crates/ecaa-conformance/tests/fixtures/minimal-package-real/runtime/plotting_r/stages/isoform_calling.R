# R-side isoform_calling stage: isoform_structure, sashimi.
# Mirrors lib/plotting/stages/isoform_calling.py.
#
# Plan reference: §S13.6 — long-read RNA-seq isoform figures. The
# isoform table carries comma-list cells the generic .swfc_load_tsv
# helper can't handle, so the loader here parses them inline (matches
# the Python `_load_isoforms` function).

if (!exists("swfc_register_figure")) {
  stop("source runtime/plotting_r/core.R before this stage module")
}

.iso_parse_int_list <- function(s) {
  if (is.null(s) || is.na(s) || nchar(s) == 0L) return(integer(0))
  parts <- strsplit(as.character(s), ",", fixed = TRUE)[[1]]
  parts <- trimws(parts)
  parts <- parts[nchar(parts) > 0L]
  suppressWarnings(as.integer(parts))
}

.iso_load_isoforms <- function(path) {
  df <- tryCatch(
    utils::read.delim(path, stringsAsFactors = FALSE, check.names = FALSE),
    error = function(e) NULL
  )
  if (is.null(df) || nrow(df) == 0L) return(NULL)
  cols <- tolower(colnames(df))
  t_col <- which(cols %in% c("transcript", "transcript_id", "tx"))
  s_col <- which(cols %in% c("exon_starts", "starts", "blockstarts"))
  e_col <- which(cols %in% c("exon_ends", "ends", "blockends"))
  sd_col <- which(cols %in% c("strand"))
  if (length(t_col) == 0L || length(s_col) == 0L || length(e_col) == 0L) return(NULL)
  rows <- list()
  for (i in seq_len(nrow(df))) {
    starts <- .iso_parse_int_list(df[[s_col[1]]][i])
    ends <- .iso_parse_int_list(df[[e_col[1]]][i])
    if (length(starts) == 0L || length(ends) == 0L) next
    n <- min(length(starts), length(ends))
    rows[[length(rows) + 1L]] <- data.frame(
      transcript = rep(df[[t_col[1]]][i], n),
      exon_starts = starts[seq_len(n)],
      exon_ends = ends[seq_len(n)],
      strand = if (length(sd_col)) df[[sd_col[1]]][i] else "+",
      stringsAsFactors = FALSE
    )
  }
  if (length(rows) == 0L) return(NULL)
  do.call(rbind, rows)
}

swfc_register_figure("isoform_calling", "isoform_structure", function(ctx) {
  p <- .swfc_manifest_path(ctx$manifest, ctx$outputs_dir, "isoform_table")
  if (is.null(p)) stop("manifest.isoform_table required")
  df <- .iso_load_isoforms(p)
  if (is.null(df)) stop(sprintf("unparseable isoform table: %s", p))
  swfc_isoform_structure_r(df, title = "Isoform structure",
                            strand_col = "strand")
})

swfc_register_figure("isoform_calling", "sashimi", function(ctx) {
  p <- .swfc_manifest_path(ctx$manifest, ctx$outputs_dir, "junction_table")
  if (is.null(p)) stop("manifest.junction_table required")
  df <- .swfc_load_tsv(p, list(
    junction = c("junction", "intron", "id"),
    count = c("count", "n_reads", "support")
  ))
  if (is.null(df)) stop(sprintf("unparseable junction table: %s", p))
  swfc_sashimi_r(df, title = "Splice-junction sashimi")
})
