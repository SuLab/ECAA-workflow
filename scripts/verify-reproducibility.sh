#!/usr/bin/env bash
# Emit a public testdata scenario twice into temp dirs, then SHA-compare
# every file in the two packages. The files that are intentionally NOT
# byte-reproducible (the documented audit logs + the runtime sidecars
# written after the BagIt manifest) are excluded from the comparison;
# everything else must be byte-identical.
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

# A real, committed testdata intake scenario. `intake` reads this file,
# classifies it, composes the DAG and emits the package directly into
# the --output directory (there is no `output-*` subdirectory).
INPUT="testdata/scenarios/01-bulk-rnaseq-ibd/request.md"
if [[ ! -f "$INPUT" ]]; then
  echo "[verify-reproducibility] ERROR: intake input not found: $INPUT" >&2
  exit 1
fi

SCRATCH="$(mktemp -d -t ecaa-verify-XXXXXX)"
trap 'rm -rf "$SCRATCH"' EXIT

echo "[verify-reproducibility] Emitting scenario twice into $SCRATCH"
echo "  Input: $INPUT"
for tag in a b; do
  "$CLI" intake \
    --input "$INPUT" \
    --output "$SCRATCH/pkg-$tag" \
    >/dev/null
done

# `emit_package` writes the package (WORKFLOW.json, ro-crate-metadata.json,
# the BagIt manifest, runtime/, policies/, …) directly into the --output
# directory, so the package root IS that directory.
PKG_A="$SCRATCH/pkg-a"
PKG_B="$SCRATCH/pkg-b"

if [[ ! -f "$PKG_A/ro-crate-metadata.json" || ! -f "$PKG_B/ro-crate-metadata.json" ]]; then
  echo "[verify-reproducibility] ERROR: one or both emissions failed (no ro-crate-metadata.json)" >&2
  exit 1
fi

echo "[verify-reproducibility] Comparing:"
echo "  A = $PKG_A"
echo "  B = $PKG_B"

# Exclusion list — kept in sync with `crates/core/src/emitter/bagit.rs`.
# These paths are deliberately NOT part of the byte-reproducibility
# surface:
#  - BagIt tag files (covered by tagmanifest-sha512.txt, written after
#    the payload walk so hashing them here would be self-referential);
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
  'runtime/intake-conversation.jsonl'
  'runtime/decisions.jsonl'
  'runtime/decisions.jsonl.mac'
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

MAPFILE_A="$SCRATCH/list.a"
MAPFILE_B="$SCRATCH/list.b"
list_files "$PKG_A" | sort > "$MAPFILE_A"
list_files "$PKG_B" | sort > "$MAPFILE_B"

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
filtered_list "$MAPFILE_A" > "$SCRATCH/list.a.filtered"
filtered_list "$MAPFILE_B" > "$SCRATCH/list.b.filtered"

if ! diff -q "$SCRATCH/list.a.filtered" "$SCRATCH/list.b.filtered" >/dev/null; then
  echo "[verify-reproducibility] FAIL — file lists differ between the two emissions:" >&2
  diff "$SCRATCH/list.a.filtered" "$SCRATCH/list.b.filtered" >&2 || true
  exit 1
fi

MISMATCHES=0
COMPARED=0
while IFS= read -r rel; do
  COMPARED=$((COMPARED + 1))
  sha_a=$(sha256sum "$PKG_A/$rel" | cut -d' ' -f1)
  sha_b=$(sha256sum "$PKG_B/$rel" | cut -d' ' -f1)
  if [[ "$sha_a" != "$sha_b" ]]; then
    echo "[verify-reproducibility] MISMATCH: $rel"
    echo "  A: $sha_a"
    echo "  B: $sha_b"
    MISMATCHES=$((MISMATCHES + 1))

    # When a SHA mismatch fires, run diffoscope (if available) so the
    # operator sees *what* differs (JSON / binary diffs), not just *that*
    # something differs. Soft-fail locally if diffoscope isn't installed —
    # the SHA failure already blocks the gate; diffoscope is a debugging
    # aid on top of it.
    if command -v diffoscope >/dev/null 2>&1; then
      echo "[verify-reproducibility] diffoscope output for $rel:"
      diffoscope --no-progress --max-text-report-size 65536 \
        "$PKG_A/$rel" "$PKG_B/$rel" 2>&1 | sed 's/^/    /' || true
      echo ""
    else
      echo "[verify-reproducibility] (install diffoscope to see what differs: 'apt install diffoscope' or 'pip install diffoscope')"
    fi
  fi
done < "$SCRATCH/list.a.filtered"

if (( MISMATCHES > 0 )); then
  echo "[verify-reproducibility] FAIL — $MISMATCHES file(s) differed." >&2
  exit 1
fi

echo "[verify-reproducibility] OK — reproducible: $COMPARED file(s) byte-identical across both emissions (excluding the documented non-reproducible sidecars)."
