#!/usr/bin/env Rscript
# DESeq2 differential expression: dexamethasone treatment effect (~ cell + dex)
# Tool: DESeq2 (Bioconductor, NB-GLM with Wald test + apeglm LFC shrinkage)
# Input: runtime/outputs/data_acquisition/data/himes-inputs/counts.tsv
#        runtime/outputs/data_acquisition/data/himes-inputs/samples.csv
# Output: runtime/outputs/differential_expression/de_results.tsv, de_summary.json, manifest.json
#
# Command: conda run -n ecaa-bioc Rscript runtime/outputs/differential_expression/scripts/01_deseq2_de.R

# Resolve the package root from the injected replay env vars (script_runner
# sets both PACKAGE and PKG_ROOT to the staged scratch root); fall back to a
# path relative to this script's location for a manual, non-replay run.
PKG <- Sys.getenv("PACKAGE",
  unset = Sys.getenv("PKG_ROOT",
    unset = normalizePath(file.path(
      dirname(tryCatch(normalizePath(sys.frame(1)$ofile), error = function(e) getwd())),
      "../../../.."
    ))
  )
)
OUT_DIR <- file.path(PKG, "runtime/outputs/differential_expression")
COUNTS_TSV <- file.path(PKG, "runtime/outputs/data_acquisition/data/himes-inputs/counts.tsv")
SAMPLES_CSV <- file.path(PKG, "runtime/outputs/data_acquisition/data/himes-inputs/samples.csv")

suppressPackageStartupMessages({
  library(DESeq2)
  library(jsonlite)
})

t_start <- proc.time()["elapsed"]

cat("[step 1] Loading count matrix and sample metadata\n")
counts_raw <- read.table(COUNTS_TSV, header=TRUE, sep="\t", row.names=1, check.names=FALSE)
samples <- read.csv(SAMPLES_CSV, stringsAsFactors=FALSE)
rownames(samples) <- samples$sample

# Align columns to samples
counts_raw <- counts_raw[, samples$sample, drop=FALSE]
cat(sprintf("  %d genes x %d samples loaded\n", nrow(counts_raw), ncol(counts_raw)))

# Make factors with reference levels
samples$cell <- factor(samples$cell)
samples$dex <- factor(samples$dex, levels = c("untrt", "trt"))  # untrt is reference

cat("[step 2] Building DESeqDataSet with design ~ cell + dex\n")
dds <- DESeqDataSetFromMatrix(
  countData = as.matrix(counts_raw),
  colData   = samples,
  design    = ~ cell + dex
)

# Pre-filter: keep genes with rowSum >= 10
keep <- rowSums(counts(dds)) >= 10
dds <- dds[keep, ]
cat(sprintf("  %d genes retained after low-count filter (rowSum >= 10)\n", sum(keep)))

cat("[step 3] Running DESeq2 (NB-GLM, Wald test)\n")
# Use recommended thread count
nthreads <- as.integer(Sys.getenv("ECAA_HW_RECOMMENDED_THREADS", unset="4"))
if (requireNamespace("BiocParallel", quietly=TRUE)) {
  BiocParallel::register(BiocParallel::MulticoreParam(workers=max(1L, nthreads - 1L)))
}
dds <- DESeq(dds)

cat("[step 4] Extracting results for contrast: dex trt vs untrt (ref=untrt)\n")
res <- results(dds, contrast=c("dex", "trt", "untrt"), alpha=0.05)
cat(sprintf("  Tested %d genes; %d with padj < 0.05\n",
            sum(!is.na(res$padj)), sum(res$padj < 0.05, na.rm=TRUE)))

cat("[step 5] Applying apeglm LFC shrinkage\n")
# apeglm shrinkage on the dex trt coefficient
res_shr <- lfcShrink(dds, coef="dex_trt_vs_untrt", type="apeglm", quiet=TRUE)

cat("[step 6] Writing de_results.tsv\n")
# apeglm returns baseMean, log2FoldChange (shrunken), lfcSE, svalue, FSR
# Merge shrunken LFC/SE with Wald stat and p-values from the unshrunken result
df_base <- as.data.frame(res)           # has: baseMean, log2FoldChange, lfcSE, stat, pvalue, padj
df_shr  <- as.data.frame(res_shr)      # has: baseMean, log2FoldChange (shrunken), lfcSE
df <- df_base
df$log2FoldChange <- df_shr[rownames(df), "log2FoldChange"]
df$lfcSE          <- df_shr[rownames(df), "lfcSE"]
df$gene <- rownames(df)
df <- df[, c("gene","baseMean","log2FoldChange","lfcSE","stat","pvalue","padj")]
# Sort by padj then abs(log2FoldChange)
df <- df[order(is.na(df$padj), df$padj, -abs(df$log2FoldChange)), ]
write.table(df, file.path(OUT_DIR, "de_results.tsv"),
            sep="\t", quote=FALSE, row.names=FALSE)

n_sig05 <- sum(df$padj < 0.05, na.rm=TRUE)
n_sig01 <- sum(df$padj < 0.01, na.rm=TRUE)
n_up    <- sum(df$padj < 0.05 & df$log2FoldChange > 0, na.rm=TRUE)
n_down  <- sum(df$padj < 0.05 & df$log2FoldChange < 0, na.rm=TRUE)
cat(sprintf("  DE results written: %d genes tested, %d sig (padj<0.05), %d up, %d down\n",
            nrow(df), n_sig05, n_up, n_down))

cat("[step 7] Writing de_summary.json\n")
de_summary <- list(
  task_id       = "differential_expression",
  method        = "deseq2",
  design_formula = "~ cell + dex",
  contrast      = "dex: trt vs untrt",
  reference_level = "untrt",
  n_genes_tested  = nrow(df),
  n_sig_padj05   = n_sig05,
  n_sig_padj01   = n_sig01,
  n_up_padj05    = n_up,
  n_down_padj05  = n_down,
  lfc_shrinkage  = "apeglm",
  size_factors   = as.list(sizeFactors(dds))
)
write_json(de_summary, file.path(OUT_DIR, "de_summary.json"), auto_unbox=TRUE, pretty=TRUE)

cat("[step 8] Writing manifest.json\n")
manifest <- list(
  task_id = "differential_expression",
  comparisons = list(
    list(
      id         = "dex_trt_vs_untrt",
      table_path = "de_results.tsv",
      reference  = "untrt",
      treatment  = "trt",
      n_tested   = nrow(df),
      n_sig_padj05 = n_sig05
    )
  ),
  design_formula = "~ cell + dex",
  primary_variable = "dex",
  method = "deseq2",
  artifacts = c("de_results.tsv", "de_summary.json", "manifest.json", "result.json")
)
write_json(manifest, file.path(OUT_DIR, "manifest.json"), auto_unbox=TRUE, pretty=TRUE)

cat("[step 9] Writing env.lock\n")
si <- sessionInfo()
writeLines(capture.output(print(si)), file.path(OUT_DIR, "env.lock"))

t_elapsed <- proc.time()["elapsed"] - t_start
cat(sprintf("Done in %.1f seconds\n", t_elapsed))
cat(sprintf("ELAPSED_SECONDS=%.1f\n", t_elapsed))
cat(sprintf("N_GENES_TESTED=%d\n", nrow(df)))
cat(sprintf("N_SIG_PADJ05=%d\n", n_sig05))
