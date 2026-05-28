#!/usr/bin/env bash
# provision_r_bioconductor.sh — idempotent provisioner for the R +
# Bioconductor + Seurat v5 stack the IVD (and most human scRNA-seq)
# analyses depend on.
#
# Runs AS the host user — writes to ~/R/x86_64-pc-linux-gnu-library/<ver>.
# No sudo required for the R install itself; system-library gaps that
# only sudo apt-get can close are reported at the end with a copy-paste
# install command rather than failing silently.
#
# Usage:
#  Scripts/helpers/provision_r_bioconductor.sh # install all
#  Scripts/helpers/provision_r_bioconductor.sh --verify # probe only
#  Scripts/helpers/provision_r_bioconductor.sh --minimal # Seurat only, skip CellChat
#
# Exit codes:
#  0 — all requested R packages install + load successfully
#  2 — R binary missing (host lacks R entirely; install R first)
#  3 — at least one required R package failed to install; stderr lists
#  the missing system dev libs that would unblock it
#  4 — package loaded, but the probe that the harness uses
#  (library(Seurat) / library(CellChat)) still returns false
#
# Called by:
#  - Standalone on a fresh executor host before the first IVD run
#  - Optionally from scripts/agent-claude.sh as a preflight when the
#  task needs R+Seurat and env_capability.r_seurat == false
#
# Intentionally tries to install packages even when some system libs
# are missing; R's install.packages skips individual broken targets
# rather than aborting, so we still get partial progress and a precise
# list of what's missing at the end.
set -u
MODE="${1:-full}"

# ── preflight ──────────────────────────────────────────────────────
if ! command -v Rscript >/dev/null 2>&1; then
  echo "ERROR: Rscript not found in PATH. Install R first (apt-get install r-base r-base-dev)." >&2
  exit 2
fi

R_VER="$(Rscript --version 2>&1 | grep -oE '[0-9]+\.[0-9]+' | head -1)"
USER_LIB="$HOME/R/x86_64-pc-linux-gnu-library/$R_VER"
mkdir -p "$USER_LIB"
export R_LIBS_USER="$USER_LIB"

echo "R version:         $(Rscript --version 2>&1 | head -1)"
echo "User library:      $USER_LIB"
echo "Mode:              $MODE"

# ── package manifest ───────────────────────────────────────────────
# Format: "<pkg>|<source>[|required|optional]" where source is
# cran, bioc, or github:<org>/<repo>. Default required; explicit
# `optional` means a failure on that row does not fail the script
# (SCTransform works without glmGamPoi's fast backend; FindMarkers
# works without presto; only Seurat + SeuratObject are load-bearing
# for the env_capability.r_seurat probe).
CORE=(
  "BiocManager|cran|required"
  "Matrix|cran|required"
  "Rcpp|cran|required"
  "RcppArmadillo|cran|required"
  "RcppEigen|cran|required"
  "SeuratObject|cran|required"
  "Seurat|cran|required"
  "sctransform|cran|required"
  "presto|github:immunogenomics/presto|optional"
  "glmGamPoi|bioc|optional"
  # Memory-discipline path (policies/memory-discipline.json). BPCells
  # is the on-disk matrix backing Seurat v5 uses for large-cohort
  # SCTransform / IntegrateLayers without materializing a dense
  # cell×gene matrix. `optional` because small-cohort analyses still
  # load fine in RAM; the memory-discipline policy steers the agent
  # to BPCells only when the cohort crosses large_cohort_cell_threshold_k.
  "BPCells|github:bnprks/BPCells/r|optional"
  "HDF5Array|bioc|optional"
  "DelayedArray|bioc|optional"
)
OPTIONAL=(
  "remotes|cran|required"
  "CellChat|github:jinworks/CellChat|optional"
  "NMF|cran|optional"
  "ComplexHeatmap|bioc|optional"
  "DESeq2|bioc|optional"
)

MANIFEST=("${CORE[@]}")
if [[ "$MODE" != "--minimal" ]]; then
  MANIFEST+=("${OPTIONAL[@]}")
fi

if [[ "$MODE" == "--verify" ]]; then
  MANIFEST=("Seurat|cran" "sctransform|cran")
fi

# ── verify-only fast path ──────────────────────────────────────────
if [[ "$MODE" == "--verify" ]]; then
  for entry in "${MANIFEST[@]}"; do
    pkg="${entry%%|*}"
    if Rscript -e "suppressPackageStartupMessages(library('$pkg'))" >/dev/null 2>&1; then
      echo "  ✓ $pkg (loadable)"
    else
      echo "  ✗ $pkg (not loadable)"
      exit 4
    fi
  done
  exit 0
fi

# ── install ────────────────────────────────────────────────────────
TMPDIR="$(mktemp -d)"
SUMMARY="$TMPDIR/summary.tsv"
MISSING_SYS_LIBS=()

run_install() {
  local pkg="$1"
  local source="$2"
  local kind="${3:-required}"
  local probe="suppressPackageStartupMessages(library('$pkg'))"
  if Rscript -e "$probe" >/dev/null 2>&1; then
    printf "  %-20s %s\n" "$pkg" "[already installed]"
    echo -e "$pkg\tok\talready_installed" >> "$SUMMARY"
    return 0
  fi
  printf "  %-20s %s" "$pkg" "installing from $source..."
  case "$source" in
    cran)
      Rscript -e "install.packages('$pkg', repos='https://cloud.r-project.org', Ncpus=max(1, parallel::detectCores()-1), quiet=TRUE)" \
        >"$TMPDIR/$pkg.install.log" 2>&1
      ;;
    bioc)
      Rscript -e "BiocManager::install('$pkg', update=FALSE, ask=FALSE, Ncpus=max(1, parallel::detectCores()-1))" \
        >"$TMPDIR/$pkg.install.log" 2>&1
      ;;
    github:*)
      local repo="${source#github:}"
      # Needs `remotes` — install it inline if absent rather than
      # failing the whole row.
      Rscript -e "if (!requireNamespace('remotes', quietly=TRUE)) install.packages('remotes', repos='https://cloud.r-project.org', quiet=TRUE); remotes::install_github('$repo', upgrade='never', quiet=TRUE)" \
        >"$TMPDIR/$pkg.install.log" 2>&1
      ;;
  esac
  if Rscript -e "$probe" >/dev/null 2>&1; then
    echo " [ok]"
    echo -e "$pkg\tok\tinstalled_from_$source" >> "$SUMMARY"
    return 0
  fi
  if [[ "$kind" == "optional" ]]; then
    echo " [skipped (optional)]"
    echo -e "$pkg\tskipped\toptional_install_failed_see_$pkg.install.log" >> "$SUMMARY"
    return 0
  fi
  echo " [FAILED]"
  echo -e "$pkg\tfail\tsee_$pkg.install.log" >> "$SUMMARY"
  # pull likely-missing system-lib hints out of the log
  grep -oE 'lib[a-z0-9_-]+\.h|lib[a-z0-9_-]+\.so|could not find package|package .* not available|cannot find -l[a-z0-9_-]+' \
    "$TMPDIR/$pkg.install.log" 2>/dev/null | sort -u | head -3 | while read hint; do
    MISSING_SYS_LIBS+=("$hint  (from $pkg)")
    echo "    hint: $hint"
  done
  return 1
}

echo ""
echo "── installing $((${#MANIFEST[@]})) packages ──"
fails=0
for entry in "${MANIFEST[@]}"; do
  # split on | — accept either 2-field (pkg|source, default required)
  # or 3-field (pkg|source|required|optional) entries.
  IFS='|' read -ra fields <<< "$entry"
  pkg="${fields[0]}"
  source="${fields[1]}"
  kind="${fields[2]:-required}"
  if ! run_install "$pkg" "$source" "$kind"; then
    fails=$((fails+1))
  fi
done

# ── report ──────────────────────────────────────────────────────────
echo ""
echo "── summary ──"
column -t -s $'\t' "$SUMMARY" 2>/dev/null || cat "$SUMMARY"

echo ""
echo "── env_capability probe ──"
for pkg in Seurat CellChat; do
  if Rscript -e "suppressPackageStartupMessages(library('$pkg'))" >/dev/null 2>&1; then
    echo "  $pkg: loadable  (harness probe_r_package will return true)"
  else
    echo "  $pkg: NOT loadable  (harness probe_r_package will return false)"
    if [[ "$pkg" == "Seurat" ]]; then fails=$((fails+1)); fi
  fi
done

# ── guidance on system-lib gaps ─────────────────────────────────────
if [[ $fails -gt 0 ]]; then
  echo ""
  echo "── likely missing system dev libs ──"
  echo "Run (with sudo) on Ubuntu/Debian:"
  echo ""
  echo "    sudo apt-get update && sudo apt-get install -y \\"
  echo "        libcurl4-openssl-dev libfontconfig1-dev libharfbuzz-dev \\"
  echo "        libfribidi-dev libfreetype6-dev libgsl-dev libglpk-dev \\"
  echo "        libgit2-dev libhdf5-dev cmake"
  echo ""
  echo "Then re-run: $0"
  echo ""
  echo "Install logs: $TMPDIR/*.install.log"
  exit 3
fi

# keep logs only on failure; clean up on success
rm -rf "$TMPDIR"
echo ""
echo "OK — Seurat v5 stack is provisioned at $USER_LIB"
exit 0
