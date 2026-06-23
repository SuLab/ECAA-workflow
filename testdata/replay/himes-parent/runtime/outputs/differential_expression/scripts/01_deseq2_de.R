#!/usr/bin/env Rscript
# Tool: DESeq2 1.50.2
# Command: Rscript scripts/01_deseq2_de.R
# Inputs:
#   - runtime/outputs/data_acquisition/data/himes-inputs/counts.tsv (raw counts, 63677 genes x 8 samples)
#   - runtime/outputs/data_acquisition/data/himes-inputs/samples.csv (sample metadata)
# Outputs:
#   - runtime/outputs/differential_expression/de_results.tsv
#   - runtime/outputs/differential_expression/de_summary.json
#   - runtime/outputs/differential_expression/intermediates/deseq2_dds.rds
#   - runtime/outputs/differential_expression/intermediates/size_factors.tsv
# Design: ~ cell + dex  (paired donor blocking + dexamethasone treatment)
# Contrast: dex trt vs untrt  (normal-prior LFC shrinkage via apeglm fallback to normal)

suppressPackageStartupMessages({
  library(DESeq2)
})

PACKAGE <- Sys.getenv("PACKAGE",
  unset = normalizePath(file.path(
    dirname(tryCatch(normalizePath(sys.frame(1)$ofile), error = function(e) getwd())),
    "../../.."
  ))
)

COUNTS_PATH  <- file.path(PACKAGE, "runtime/outputs/data_acquisition/data/himes-inputs/counts.tsv")
SAMPLES_PATH <- file.path(PACKAGE, "runtime/outputs/data_acquisition/data/himes-inputs/samples.csv")
OUT_DIR      <- file.path(PACKAGE, "runtime/outputs/differential_expression")
INTER_DIR    <- file.path(OUT_DIR, "intermediates")
dir.create(INTER_DIR, showWarnings = FALSE, recursive = TRUE)

progress <- function(msg) {
  cat(format(Sys.time(), "[%H:%M:%S]"), msg, "\n", file = stderr())
  cat(msg, "\n", file = file.path(OUT_DIR, "progress.log"), append = TRUE)
}

progress("[step 2/6] Loading count matrix and sample metadata")

# Load counts
counts_mat <- read.table(COUNTS_PATH, header = TRUE, row.names = 1,
                         sep = "\t", check.names = FALSE, comment.char = "")
counts_mat <- as.matrix(counts_mat)
storage.mode(counts_mat) <- "integer"
cat("Raw counts:", nrow(counts_mat), "genes x", ncol(counts_mat), "samples\n")

# Load sample metadata
samples <- read.csv(SAMPLES_PATH, stringsAsFactors = FALSE)
rownames(samples) <- samples$sample
# Reorder to match count matrix columns
samples <- samples[colnames(counts_mat), , drop = FALSE]
samples$cell <- factor(samples$cell)
samples$dex  <- factor(samples$dex, levels = c("untrt", "trt"))

cat("Sample metadata:\n")
print(samples)

progress("[step 3/6] Building DESeqDataSet and estimating size factors")

# Build DESeqDataSet with design ~ cell + dex
dds <- DESeqDataSetFromMatrix(
  countData = counts_mat,
  colData   = samples,
  design    = ~ cell + dex
)

# Run full DESeq2 pipeline
set.seed(42)
dds <- DESeq(dds, parallel = FALSE)

# Record size factors
sf_df <- data.frame(
  sample      = colnames(dds),
  size_factor = sizeFactors(dds)
)
write.table(sf_df, file.path(INTER_DIR, "size_factors.tsv"),
            sep = "\t", quote = FALSE, row.names = FALSE)
cat("Size factors:\n")
print(sf_df)

# Save dds object
saveRDS(dds, file.path(INTER_DIR, "deseq2_dds.rds"))

progress("[step 4/6] Extracting DE results (dex trt vs untrt) with LFC shrinkage")

# Extract results for contrast: dex trt vs untrt
res_raw <- results(dds, contrast = c("dex", "trt", "untrt"),
                   alpha = 0.05)
cat("Results summary (raw):\n")
summary(res_raw)

# Apply normal-prior LFC shrinkage (as specified by SME)
res_shrunk <- lfcShrink(dds, contrast = c("dex", "trt", "untrt"),
                        type = "normal", res = res_raw)
cat("Results summary (normal-prior LFC shrinkage):\n")
summary(res_shrunk)

# Convert to data frame for output
res_df <- as.data.frame(res_shrunk)
res_df$gene_id    <- rownames(res_df)
res_df$tested     <- !is.na(res_df$padj)

# Add unshrunken LFC for reference
res_unsh          <- as.data.frame(res_raw)
res_df$log2fc_raw <- res_unsh$log2FoldChange

# Rename columns to canonical names for measure_de_effect_size.py compatibility
colnames(res_df)[colnames(res_df) == "log2FoldChange"] <- "log2fc"
colnames(res_df)[colnames(res_df) == "baseMean"]       <- "base_mean"
colnames(res_df)[colnames(res_df) == "lfcSE"]          <- "lfc_se"
colnames(res_df)[colnames(res_df) == "stat"]           <- "stat"
colnames(res_df)[colnames(res_df) == "pvalue"]         <- "pvalue"
colnames(res_df)[colnames(res_df) == "padj"]           <- "padj"

# Reorder columns
res_df <- res_df[, c("gene_id", "base_mean", "log2fc", "lfc_se", "stat",
                     "pvalue", "padj", "log2fc_raw", "tested")]

# Sort by adjusted p-value, then by |log2fc|
res_df <- res_df[order(res_df$padj, -abs(res_df$log2fc), na.last = TRUE), ]

progress("[step 5/6] Writing de_results.tsv")

write.table(res_df, file.path(OUT_DIR, "de_results.tsv"),
            sep = "\t", quote = FALSE, row.names = FALSE)
cat("Wrote de_results.tsv:", nrow(res_df), "genes\n")

# Compute summary statistics
n_tested    <- sum(res_df$tested, na.rm = TRUE)
n_sig_05    <- sum(!is.na(res_df$padj) & res_df$padj < 0.05, na.rm = TRUE)
n_sig_01    <- sum(!is.na(res_df$padj) & res_df$padj < 0.01, na.rm = TRUE)
n_up        <- sum(!is.na(res_df$padj) & res_df$padj < 0.05 & res_df$log2fc > 0, na.rm = TRUE)
n_down      <- sum(!is.na(res_df$padj) & res_df$padj < 0.05 & res_df$log2fc < 0, na.rm = TRUE)
top10_genes <- head(res_df$gene_id[!is.na(res_df$padj)], 10)

summary_obj <- list(
  task_id           = "differential_expression",
  method            = "DESeq2",
  design_formula    = "~ cell + dex",
  contrast          = list("dex", "trt", "untrt"),
  lfc_shrinkage     = "normal-prior",
  n_genes_total     = nrow(res_df),
  n_genes_tested    = n_tested,
  n_sig_padj_0.05   = n_sig_05,
  n_sig_padj_0.01   = n_sig_01,
  n_upregulated     = n_up,
  n_downregulated   = n_down,
  top10_genes_by_padj = top10_genes,
  deseq2_version    = as.character(packageVersion("DESeq2")),
  r_version         = as.character(R.version$major_minor)
)

library(jsonlite)
write_json(summary_obj, file.path(OUT_DIR, "de_summary.json"),
           auto_unbox = TRUE, pretty = TRUE)
cat("Wrote de_summary.json\n")

progress("[step 6/6] Writing manifest.json")

manifest <- list(
  task_id  = "differential_expression",
  outputs  = list(
    de_results   = "de_results.tsv",
    de_summary   = "de_summary.json",
    size_factors = "intermediates/size_factors.tsv",
    dds_rds      = "intermediates/deseq2_dds.rds"
  ),
  downstream_handoff = list(
    de_results_path     = file.path(OUT_DIR, "de_results.tsv"),
    effect_col          = "log2fc",
    padj_col            = "padj",
    gene_id_col         = "gene_id",
    base_mean_col       = "base_mean",
    reference_level     = "untrt",
    treatment_level     = "trt",
    factor_variable     = "dex",
    n_sig_padj_0.05     = n_sig_05
  )
)

write_json(manifest, file.path(OUT_DIR, "manifest.json"),
           auto_unbox = TRUE, pretty = TRUE)

cat("\nDE analysis complete.\n")
cat("Significant (padj < 0.05):", n_sig_05, "\n")
cat("  Up-regulated:", n_up, "\n")
cat("  Down-regulated:", n_down, "\n")

# Session info for env.lock
capture.output(sessionInfo()) |> writeLines(file.path(OUT_DIR, "env.lock"))
