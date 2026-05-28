# R-side spatial_clustering stage: tissue_overlay, morans_i_scatter,
# neighborhood_enrichment. Mirrors lib/plotting/stages/spatial_clustering.py.
#
# Plan reference: §S13.6 — spatial-transcriptomics figures. Tissue
# image underlay is best-effort (PNG/JPG read via the `png`/`jpeg`
# packages when available); when absent we fall through to the
# image-less overlay shape, matching the Python soft-fallback.

if (!exists("swfc_register_figure")) {
  stop("source runtime/plotting_r/core.R before this stage module")
}

.spatial_load_image <- function(ctx) {
  p <- .swfc_manifest_path(ctx$manifest, ctx$outputs_dir, "image")
  if (is.null(p)) return(NULL)
  ext <- tolower(tools::file_ext(p))
  img <- tryCatch({
    if (ext == "png" && requireNamespace("png", quietly = TRUE)) {
      png::readPNG(p)
    } else if (ext %in% c("jpg", "jpeg") && requireNamespace("jpeg", quietly = TRUE)) {
      jpeg::readJPEG(p)
    } else {
      NULL
    }
  }, error = function(e) NULL)
  img
}

swfc_register_figure("spatial_clustering", "tissue_overlay", function(ctx) {
  p <- .swfc_manifest_path(ctx$manifest, ctx$outputs_dir, "coords_table")
  if (is.null(p)) stop("manifest.coords_table required")
  df <- .swfc_load_tsv(p, list(
    x = c("x", "x_pixel", "imagecol"),
    y = c("y", "y_pixel", "imagerow"),
    value = c("value", "cluster", "domain", "expression")
  ))
  if (is.null(df)) stop(sprintf("unparseable coords table: %s", p))
  image <- .spatial_load_image(ctx)
  swfc_tissue_overlay_r(df, image = image, title = "Tissue overlay")
})

swfc_register_figure("spatial_clustering", "morans_i_scatter", function(ctx) {
  p <- .swfc_manifest_path(ctx$manifest, ctx$outputs_dir, "morans_i_table")
  if (is.null(p)) stop("manifest.morans_i_table required")
  df <- .swfc_load_tsv(p, list(
    gene = c("gene", "feature", "symbol"),
    morans_i = c("morans_i", "i", "morans"),
    p_value = c("p_value", "pvalue", "p", "morans_p")
  ))
  if (is.null(df)) stop(sprintf("unparseable morans_i table: %s", p))
  swfc_morans_i_scatter_r(df, title = "Moran's I")
})

swfc_register_figure("spatial_clustering", "neighborhood_enrichment",
                     function(ctx) {
  p <- .swfc_manifest_path(ctx$manifest, ctx$outputs_dir, "neighborhood_table")
  if (is.null(p)) stop("manifest.neighborhood_table required")
  df <- .swfc_load_tsv(p, list(
    source = c("source", "source_type", "from"),
    target = c("target", "target_type", "to"),
    score = c("score", "z", "enrichment")
  ))
  if (is.null(df)) stop(sprintf("unparseable neighborhood table: %s", p))
  swfc_neighborhood_enrichment_r(df, title = "Neighborhood enrichment")
})
