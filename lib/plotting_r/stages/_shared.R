# Shared helpers for R stage modules (Stage 13/14 parity with the
# Python-side `lib/plotting/stages/_shared.py`).
#
# Stage modules read manifest entries to find a TSV path relative to
# `outputs_dir`, then load specific columns by header-name aliases.
# Centralising the boilerplate keeps each stage module under ~100 LOC
# and matches the Python contract: missing tables / unparseable
# headers raise the same family of errors so `core.R::swfc_generate`
# records them per-figure.

if (!exists("swfc_register_figure")) {
  stop("source runtime/plotting_r/core.R before _shared.R")
}

# Resolve a manifest entry whose value is a relative path against
# `outputs_dir`. Returns NULL when any segment is missing or the
# resolved file does not exist on disk.
.swfc_manifest_path <- function(manifest, outputs_dir, ...) {
  keys <- c(...)
  cur <- manifest
  for (k in keys) {
    if (!is.list(cur) || is.null(cur[[k]])) return(NULL)
    cur <- cur[[k]]
  }
  if (!is.character(cur) || length(cur) != 1L) return(NULL)
  p <- normalizePath(file.path(outputs_dir, cur), mustWork = FALSE)
  if (!file.exists(p)) return(NULL)
  p
}

# Read a TSV (gzip-aware via R's connection inference) and return the
# requested columns as a data.frame. `columns` is a named list mapping
# the canonical column name to a vector of accepted header aliases.
# Canonical names ending in `?` mark optional columns — when no alias
# matches, the returned frame omits the column rather than failing.
.swfc_load_tsv <- function(path, columns) {
  df <- tryCatch(
    utils::read.delim(path, stringsAsFactors = FALSE, check.names = FALSE,
                      comment.char = ""),
    error = function(e) NULL
  )
  if (is.null(df) || nrow(df) == 0L) return(NULL)
  header_lc <- tolower(colnames(df))
  out <- list()
  for (canonical in names(columns)) {
    aliases <- tolower(columns[[canonical]])
    idx <- which(header_lc %in% aliases)
    if (length(idx) == 0L) {
      if (endsWith(canonical, "?")) next
      return(NULL)
    }
    name <- sub("\\?$", "", canonical)
    out[[name]] <- df[[idx[1]]]
  }
  as.data.frame(out, stringsAsFactors = FALSE)
}
