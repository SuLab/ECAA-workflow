#!/usr/bin/env bash
# Build emitted RO-Crate packages from intake fixtures.
# Used to populate testdata/emitted-packages/ for D9 WorkflowHub upload + N5 evidence.
#
# Each fixture lives at <ROOT>/<name>/intake.json with shape
#   { "study_id": "...", "intake_prose": "...", ... }
# We extract `intake_prose` to a temp text file and feed it to
# `scripps-workflow intake --input` (the CLI takes raw intake text, not JSON).
set -euo pipefail
ROOT="${1:-testdata/wrroc-fixtures}"
OUT="${2:-testdata/emitted-packages}"
CONFIG_DIR="${ECAA_CONFIG_DIR:-config}"
mkdir -p "$OUT"
TMP_PROSE_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_PROSE_DIR"' EXIT
for dir in "$ROOT"/*/; do
  name=$(basename "$dir")
  pkg_dir="$OUT/$name"
  fixture="$dir/intake.json"
  if [[ ! -f "$fixture" ]]; then
    echo "FAILED: $name (no intake.json)"
    continue
  fi
  rm -rf "$pkg_dir"
  echo "Building $name..."
  prose_file="$TMP_PROSE_DIR/$name.txt"
  if ! jq -er '.intake_prose' "$fixture" > "$prose_file"; then
    echo "FAILED: $name (intake_prose missing or non-string)"
    continue
  fi
  scripps-workflow intake \
    --input "$prose_file" \
    --output "$pkg_dir" \
    --config "$CONFIG_DIR" || echo "FAILED: $name (skipped)"
done
echo "Done. Emitted packages under $OUT"
