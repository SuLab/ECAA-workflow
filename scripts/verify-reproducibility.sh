#!/usr/bin/env bash
# Emit several public testdata scenarios twice each into temp dirs, then
# SHA-compare every file in each pair of packages. The files that are
# intentionally NOT byte-reproducible (the documented audit logs + the
# runtime sidecars written after the BagIt manifest) are excluded from
# the comparison; everything else must be byte-identical.
#
# Emitting MULTIPLE distinct modalities (not just one bulk-RNA-seq
# scenario) widens the reproducibility surface so a determinism leak that
# only manifests on one composer path (M18) cannot slip through.
#
# This script is the proof-of-contract for CLAUDE.md's claim that
# "Emitted packages must be byte-reproducible for the same intake and
# config." The exclusion list below is kept in sync with the
# authoritative BagIt manifest exclusion list in
# `crates/core/src/emitter/bagit.rs` (the set of paths that are
# deliberately kept out of the reproducibility surface).
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

# Locate (or build) the `ecaa-workflow` CLI. Honor an already-built
# binary to avoid a needless compile on disk-tight hosts: prefer an
# explicit override, then the debug/release build outputs, then a
# globally installed copy. Build the CLI crate (debug only — never a
# workspace/release build) when none is present.
CLI=""
if [[ -n "${ECAA_CLI_BIN:-}" && -x "${ECAA_CLI_BIN}" ]]; then
  CLI="${ECAA_CLI_BIN}"
elif [[ -x "$ROOT/target/debug/ecaa-workflow" ]]; then
  CLI="$ROOT/target/debug/ecaa-workflow"
elif [[ -x "$ROOT/target/release/ecaa-workflow" ]]; then
  CLI="$ROOT/target/release/ecaa-workflow"
elif command -v ecaa-workflow >/dev/null 2>&1; then
  CLI="$(command -v ecaa-workflow)"
else
  echo "[verify-reproducibility] Building ecaa-workflow CLI (debug)…"
  cargo build -p ecaa-workflow-cli --bin ecaa-workflow --quiet
  CLI="$ROOT/target/debug/ecaa-workflow"
fi
echo "[verify-reproducibility] Using CLI: $CLI"

# Real, committed testdata intake scenarios spanning several distinct
# modalities. `intake` reads each file, classifies it, composes the DAG
# and emits the package directly into the --output directory (there is no
# `output-*` subdirectory). Each scenario is emitted twice and the two
# emissions are SHA-compared; ALL must be byte-identical (M18).
SCENARIOS=(
  "testdata/scenarios/01-bulk-rnaseq-ibd/request.md"
  "testdata/scenarios/02-spatial-dlpfc/request.md"
  "testdata/scenarios/03-wgs-giab-benchmark/request.md"
)
for s in "${SCENARIOS[@]}"; do
  if [[ ! -f "$ROOT/$s" ]]; then
    echo "[verify-reproducibility] ERROR: intake input not found: $s" >&2
    exit 1
  fi
done

SCRATCH="$(mktemp -d -t ecaa-verify-XXXXXX)"
trap 'rm -rf "$SCRATCH"' EXIT

# Exclusion list — kept in sync with `crates/core/src/emitter/bagit.rs`.
# These paths are deliberately NOT part of the byte-reproducibility
# surface:
#  - checksum-seal metadata written after the payload walk;
#  - the documented session audit logs (intake-conversation.jsonl,
#    decisions.jsonl[.mac]) written by the conversation emit path AFTER
#    core emit_package returns;
#  - the runtime ECAA sidecars that core emits after the BagIt manifest
#    and the conversation path may overwrite with richer records;
#  - agent-written artifacts under runtime/outputs and runtime logs;
#  - the affordance sidecars and the conformance-mode package.ttl.
EXCLUDE_PATTERNS=(
  'bagit.txt'
  'bag-info.txt'
  'tagmanifest-sha512.txt'
  'seal-info.json'
  'seal-tagmanifest-sha512.txt'
  'runtime/intake-conversation.jsonl'
  'runtime/decisions.jsonl'
  'runtime/decisions.jsonl.mac'
  'runtime/coverage-statement.json'
  'runtime/catalog-coverage-statement.json'
  'runtime/proofs.jsonl'
  'runtime/claim-verification.json'
  'runtime/verifier-decisions.jsonl'
  'runtime/assumptions.jsonl'
  'runtime/validation-reports.jsonl'
  'runtime/determinism-shim.json'
  'runtime/security-policy.json'
  'runtime/audit-proof-report.json'
  'runtime/validation-summary.json'
  'runtime/policy-decisions.jsonl'
  'runtime/plot_affordances.jsonl'
  'runtime/affordance_fallbacks.jsonl'
  'runtime/sandbox-runs.jsonl'
  'package.ttl'
)
# Path-prefix exclusions (whole subtrees): agent-written outputs + logs +
# per-task verification sidecars.
EXCLUDE_PREFIXES=(
  'runtime/outputs/'
  'runtime/LOG.jsonl'
  'runtime/verification-reports/'
)

# Emit the stable relative file list (skipping dotfiles).
list_files() {
  local root="$1"
  (cd "$root" && find . -type f ! -path '*/.*' -print)
}

excluded() {
  local rel="$1"
  local pat
  for pat in "${EXCLUDE_PATTERNS[@]}"; do
    if [[ "$rel" == "$pat" ]]; then return 0; fi
  done
  for pat in "${EXCLUDE_PREFIXES[@]}"; do
    if [[ "$rel" == "$pat"* ]]; then return 0; fi
  done
  return 1
}

# Compare the file lists, but only after dropping excluded paths from
# both sides (a runtime sidecar present in one emission but not the
# other must not fail the gate when it's outside the reproducibility
# surface).
filtered_list() {
  local infile="$1"
  local rel
  while IFS= read -r rel; do
    rel="${rel#./}"
    if excluded "$rel"; then continue; fi
    echo "$rel"
  done < "$infile"
}

# Emit a single scenario twice and SHA-compare the two packages.
# $1 = scenario request.md (repo-relative), $2 = unique label for scratch dirs.
# Returns non-zero on any file-list divergence or SHA mismatch.
compare_scenario() {
  local input="$1" label="$2"
  local pkg_a="$SCRATCH/pkg-${label}-a"
  local pkg_b="$SCRATCH/pkg-${label}-b"

  echo "[verify-reproducibility] [$label] emitting twice: $input"
  "$CLI" intake --input "$ROOT/$input" --output "$pkg_a" >/dev/null
  "$CLI" intake --input "$ROOT/$input" --output "$pkg_b" >/dev/null

  # `emit_package` writes the package (WORKFLOW.json, ro-crate-metadata.json,
  # the BagIt manifest, runtime/, policies/, …) directly into --output, so the
  # package root IS that directory.
  if [[ ! -f "$pkg_a/ro-crate-metadata.json" || ! -f "$pkg_b/ro-crate-metadata.json" ]]; then
    echo "[verify-reproducibility] [$label] ERROR: one or both emissions failed (no ro-crate-metadata.json)" >&2
    return 1
  fi

  local list_a="$SCRATCH/list.${label}.a"
  local list_b="$SCRATCH/list.${label}.b"
  list_files "$pkg_a" | sort > "$list_a"
  list_files "$pkg_b" | sort > "$list_b"
  filtered_list "$list_a" > "$list_a.filtered"
  filtered_list "$list_b" > "$list_b.filtered"

  if ! diff -q "$list_a.filtered" "$list_b.filtered" >/dev/null; then
    echo "[verify-reproducibility] [$label] FAIL — file lists differ between the two emissions:" >&2
    diff "$list_a.filtered" "$list_b.filtered" >&2 || true
    return 1
  fi

  local mismatches=0 compared=0 rel sha_a sha_b
  while IFS= read -r rel; do
    compared=$((compared + 1))
    sha_a=$(sha256sum "$pkg_a/$rel" | cut -d' ' -f1)
    sha_b=$(sha256sum "$pkg_b/$rel" | cut -d' ' -f1)
    if [[ "$sha_a" != "$sha_b" ]]; then
      echo "[verify-reproducibility] [$label] MISMATCH: $rel"
      echo "  A: $sha_a"
      echo "  B: $sha_b"
      mismatches=$((mismatches + 1))

      # When a SHA mismatch fires, run diffoscope (if available) so the
      # operator sees *what* differs (JSON / binary diffs), not just *that*
      # something differs. Soft-fail locally if diffoscope isn't installed —
      # the SHA failure already blocks the gate; diffoscope is a debugging
      # aid on top of it.
      if command -v diffoscope >/dev/null 2>&1; then
        echo "[verify-reproducibility] [$label] diffoscope output for $rel:"
        diffoscope --no-progress --max-text-report-size 65536 \
          "$pkg_a/$rel" "$pkg_b/$rel" 2>&1 | sed 's/^/    /' || true
        echo ""
      else
        echo "[verify-reproducibility] (install diffoscope to see what differs: 'apt install diffoscope' or 'pip install diffoscope')"
      fi
    fi
  done < "$list_a.filtered"

  if (( mismatches > 0 )); then
    echo "[verify-reproducibility] [$label] FAIL — $mismatches file(s) differed." >&2
    return 1
  fi

  echo "[verify-reproducibility] [$label] OK — reproducible: $compared file(s) byte-identical across both emissions (excluding the documented non-reproducible sidecars)."
  return 0
}

FAILED=0
i=0
for s in "${SCENARIOS[@]}"; do
  i=$((i + 1))
  if ! compare_scenario "$s" "scenario-${i}"; then
    FAILED=$((FAILED + 1))
  fi
done

if (( FAILED > 0 )); then
  echo "[verify-reproducibility] FAIL — $FAILED of ${#SCENARIOS[@]} scenario(s) were NOT byte-reproducible." >&2
  exit 1
fi

echo "[verify-reproducibility] OK — all ${#SCENARIOS[@]} scenario(s) byte-reproducible across repeated emits."
