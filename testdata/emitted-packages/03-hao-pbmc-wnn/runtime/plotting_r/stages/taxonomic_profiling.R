# R-side taxonomic_profiling stage: taxonomic_stacked_bar,
# diversity_violin. Mirrors lib/plotting/stages/taxonomic_profiling.py.
#
# Plan reference: §S13.6 — metagenomics taxonomic figures.

if (!exists("ecaa_register_figure")) {
  stop("source runtime/plotting_r/core.R before this stage module")
}

ecaa_register_figure("taxonomic_profiling", "taxonomic_stacked_bar",
                     function(ctx) {
  p <- .ecaa_manifest_path(ctx$manifest, ctx$outputs_dir, "abundance_table")
  if (is.null(p)) stop("manifest.abundance_table required")
  df <- .ecaa_load_tsv(p, list(
    sample = c("sample", "sample_id", "subject"),
    taxon = c("taxon", "species", "genus", "family"),
    abundance = c("abundance", "count", "rel_abundance", "fraction")
  ))
  if (is.null(df)) stop(sprintf("unparseable abundance table: %s", p))
  ecaa_taxonomic_stacked_bar_r(df, title = "Taxonomic composition")
})

ecaa_register_figure("taxonomic_profiling", "diversity_violin", function(ctx) {
  p <- .ecaa_manifest_path(ctx$manifest, ctx$outputs_dir, "diversity_table")
  if (is.null(p)) stop("manifest.diversity_table required")
  df <- .ecaa_load_tsv(p, list(
    group = c("group", "cohort", "condition"),
    value = c("value", "shannon", "simpson", "diversity")
  ))
  if (is.null(df)) stop(sprintf("unparseable diversity table: %s", p))
  ecaa_diversity_violin_r(df, title = "Alpha diversity")
})
