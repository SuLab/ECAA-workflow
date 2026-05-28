#!/usr/bin/env bash
# Regenerate the golden-file corpus that pins the
# byte-level shape of every emitted package. Two tiers:
#
#  1. Per-archetype goldens — `tests/golden-workflows/archetypes/<id>/`
#  one directory per of the 12 canonical archetypes.
#  Contents: `intake.yaml` (deterministic intake fixture) +
#  `WORKFLOW.json` (the package the compiler emits for that intake).
#
#  2. Edge-case goldens — `tests/golden-workflows/edge-cases/<name>/`
#  Five named edge cases: per-sample-fan-out, conditional-trimming,
#  sensitivity-comparison, amendment-re-emit, cross-version-diff.
#
# CI gate (`golden-diff` job in `.github/workflows/rust.yml`) runs the
# emit path against each `intake.yaml` and `git diff`s the result
# against the committed `WORKFLOW.json`. Drift fails CI; this script
# is the only blessed path to bump the goldens.
#
# IMPORTANT: this script only regenerates from a freshly built
# `scripps-workflow` binary. Run after every PR that intentionally
# changes emit output (atom rename, new stage, schema bump, etc.) and
# review the diff carefully before committing.
#
# Usage:
#  Bash scripts/regenerate-goldens.sh # all
#  Bash scripts/regenerate-goldens.sh archetypes # only archetype goldens
#  Bash scripts/regenerate-goldens.sh edge-cases # only edge-case goldens
#  bash scripts/regenerate-goldens.sh single_cell_de # only one archetype
#
# Requires: cargo build --release; jq on PATH.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GOLDEN_DIR="${REPO_ROOT}/tests/golden-workflows"

target="${1:-all}"

# Build a release binary so the goldens are produced by the same
# binary CI consumes. Cargo memoizes so subsequent runs are fast.
echo "[regen] building scripps-workflow (release)…"
cargo build --release --bin scripps-workflow --quiet

CMD="${REPO_ROOT}/target/release/scripps-workflow"

# 12 archetype ids matching `config/archetypes/*.yaml`.
ARCHETYPES=(
  "atac_seq_peaks"
  "bulk_rnaseq_de"
  "chip_seq_peaks"
  "clinical_trial_analysis"
  "gwas_coloc"
  "long_read_rnaseq"
  "metagenomics_taxonomic"
  "proteomics_dda"
  "proteomics_dia"
  "single_cell_de"
  "spatial_transcriptomics"
  "time_series_forecast"
  "variant_calling_germline"
)

EDGE_CASES=(
  "per-sample-fan-out"
  "conditional-trimming"
  "sensitivity-comparison"
  "amendment-re-emit"
  "cross-version-diff"
)

regen_archetype() {
  local arch="$1"
  local out_dir="${GOLDEN_DIR}/archetypes/${arch}"
  local intake="${out_dir}/intake.yaml"

  if [[ ! -f "${intake}" ]]; then
    echo "[regen] SKIP ${arch}: no intake.yaml at ${intake}"
    return 0
  fi

  echo "[regen] archetype: ${arch}"
  # `intake` reads the intake fixture, classifies, builds, and writes
  # `WORKFLOW.json` into the out dir. The compiler emits deterministic
  # output under SOURCE_DATE_EPOCH (S5.20) so the diff against the
  # committed copy is byte-stable. ECAA_COMPOSER=archetypes routes
  # through the composer fast-path so the goldens reflect the
  # post-Phase-4-sunset shape.
  SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1735689600}" \
  ECAA_COMPOSER="${ECAA_COMPOSER:-archetypes}" \
    "${CMD}" intake \
      --input "${intake}" \
      --output "${out_dir}/emit"
  # Stabilize the workflow_id to the archetype name so goldens
  # don't drift on every re-run (the CLI's `uuid_short()` is
  # invocation-non-deterministic by design — sessions need
  # unique ids in production but goldens want byte-stability).
  jq --arg wid "golden-${arch}" '.workflow_id = $wid' \
     "${out_dir}/emit/WORKFLOW.json" > "${out_dir}/WORKFLOW.json"
  rm -rf "${out_dir}/emit"
}

regen_edge_case() {
  local case="$1"
  local out_dir="${GOLDEN_DIR}/edge-cases/${case}"
  local intake="${out_dir}/intake.yaml"

  if [[ ! -f "${intake}" ]]; then
    echo "[regen] SKIP edge-case ${case}: no intake.yaml at ${intake}"
    return 0
  fi

  echo "[regen] edge-case: ${case}"
  SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1735689600}" \
  ECAA_COMPOSER="${ECAA_COMPOSER:-archetypes}" \
    "${CMD}" intake \
      --input "${intake}" \
      --output "${out_dir}/emit"
  jq --arg wid "golden-edge-${case}" '.workflow_id = $wid' \
     "${out_dir}/emit/WORKFLOW.json" > "${out_dir}/WORKFLOW.json"
  rm -rf "${out_dir}/emit"
}

case "${target}" in
  all)
    for a in "${ARCHETYPES[@]}"; do regen_archetype "${a}"; done
    for c in "${EDGE_CASES[@]}"; do regen_edge_case "${c}"; done
    ;;
  archetypes)
    for a in "${ARCHETYPES[@]}"; do regen_archetype "${a}"; done
    ;;
  edge-cases)
    for c in "${EDGE_CASES[@]}"; do regen_edge_case "${c}"; done
    ;;
  *)
    # Single archetype id (or edge-case name).
    if [[ -d "${GOLDEN_DIR}/archetypes/${target}" ]]; then
      regen_archetype "${target}"
    elif [[ -d "${GOLDEN_DIR}/edge-cases/${target}" ]]; then
      regen_edge_case "${target}"
    else
      echo "[regen] ERROR: '${target}' is not all|archetypes|edge-cases or a known archetype/edge-case id"
      exit 1
    fi
    ;;
esac

echo "[regen] done. Run 'git diff tests/golden-workflows/' and commit intentional changes."
