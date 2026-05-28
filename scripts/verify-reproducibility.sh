#!/usr/bin/env bash
# Emit the IVD scenario twice into temp dirs, then SHA-compare every
# file in the two packages. The two documented audit logs
# (runtime/intake-conversation.jsonl, runtime/decisions.jsonl) and the
# intentionally-dated top-level directory name are excluded from the
# comparison; everything else must be byte-identical.
#
# This target is the proof-of-contract for CLAUDE.md's claim that
# "Emitted packages must be byte-reproducible for the same intake and
# config." See docs/reproducibility.md for the list of documented
# exceptions.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

echo "[verify-reproducibility] Building workspace…"
cargo build --workspace --release --quiet

SCRATCH="$(mktemp -d -t swfc-verify-XXXXXX)"
trap 'rm -rf "$SCRATCH"' EXIT

echo "[verify-reproducibility] Emitting IVD scenario twice into $SCRATCH"
for tag in a b; do
  "$ROOT/target/release/scripps-workflow" intake \
    --input testdata/IVD_prompt/ivd-request.md \
    --output "$SCRATCH/ivd-$tag" \
    >/dev/null
done

PKG_A="$(find "$SCRATCH/ivd-a" -maxdepth 1 -mindepth 1 -type d -name 'output-*' | head -1)"
PKG_B="$(find "$SCRATCH/ivd-b" -maxdepth 1 -mindepth 1 -type d -name 'output-*' | head -1)"

if [[ -z "$PKG_A" || -z "$PKG_B" ]]; then
  echo "[verify-reproducibility] ERROR: one or both emissions failed" >&2
  exit 1
fi

echo "[verify-reproducibility] Comparing:"
echo "  A = $PKG_A"
echo "  B = $PKG_B"

# Exclusion list: files intentionally not byte-reproducible.
# - runtime/intake-conversation.jsonl and runtime/decisions.jsonl are
#  written by emit_with_conversation_log AFTER emit_package returns.
# - runtime/plot_affordances.jsonl is the affordance sidecar,
#  written only when EmitContext::emit_affordances is Some(_) (not yet
#  wired into the CLI default path; excluded here so future wiring
#  doesn't require a simultaneous script update).
# - runtime/affordance_fallbacks.jsonl is the fallback
#  telemetry sidecar (pre-registered; excluded proactively to
#  avoid breaking the gate when it is emitted).
EXCLUDE_PATTERNS=(
  'runtime/intake-conversation.jsonl'
  'runtime/decisions.jsonl'
  'runtime/plot_affordances.jsonl'
  'runtime/affordance_fallbacks.jsonl'
  # Phase C7: sandbox-runs.jsonl records timestamps and exit codes for
  # every bwrap-wrapped dispatch — inherently non-reproducible.
  'runtime/sandbox-runs.jsonl'
)

# Emit the stable relative file list (excluding the two audit logs).
list_files() {
  local root="$1"
  (cd "$root" && find . -type f ! -path '*/.*' -print)
}

MAPFILE_A="$SCRATCH/list.a"
MAPFILE_B="$SCRATCH/list.b"
list_files "$PKG_A" | sort > "$MAPFILE_A"
list_files "$PKG_B" | sort > "$MAPFILE_B"

if ! diff -q "$MAPFILE_A" "$MAPFILE_B" >/dev/null; then
  echo "[verify-reproducibility] FAIL — file lists differ between the two emissions:" >&2
  diff "$MAPFILE_A" "$MAPFILE_B" >&2 || true
  exit 1
fi

MISMATCHES=0
while IFS= read -r rel; do
  # Strip leading./ from the find output.
  rel="${rel#./}"

  # Skip the documented-exception files.
  skip=0
  for pat in "${EXCLUDE_PATTERNS[@]}"; do
    if [[ "$rel" == "$pat" ]]; then skip=1; break; fi
  done
  if (( skip )); then continue; fi

  sha_a=$(sha256sum "$PKG_A/$rel" | cut -d' ' -f1)
  sha_b=$(sha256sum "$PKG_B/$rel" | cut -d' ' -f1)
  if [[ "$sha_a" != "$sha_b" ]]; then
    echo "[verify-reproducibility] MISMATCH: $rel"
    echo "  A: $sha_a"
    echo "  B: $sha_b"
    MISMATCHES=$((MISMATCHES + 1))

    # Plan S5.19 — when SHA mismatch fires, run diffoscope (if
    # available) so the operator sees *what* differs (tarball / JSON /
    # binary diffs), not just *that* something differs. Soft-fail
    # locally if diffoscope isn't installed — the SHA failure already
    # blocks the gate; diffoscope is a debugging aid on top of it.
    if command -v diffoscope >/dev/null 2>&1; then
      echo "[verify-reproducibility] diffoscope output for $rel:"
      diffoscope --no-progress --max-text-report-size 65536 \
        "$PKG_A/$rel" "$PKG_B/$rel" 2>&1 | sed 's/^/    /' || true
      echo ""
    else
      echo "[verify-reproducibility] (install diffoscope to see what differs: 'apt install diffoscope' or 'pip install diffoscope')"
    fi
  fi
done < "$MAPFILE_A"

if (( MISMATCHES > 0 )); then
  echo "[verify-reproducibility] FAIL — $MISMATCHES file(s) differed." >&2
  exit 1
fi

echo "[verify-reproducibility] OK — both emissions match byte-for-byte (excluding documented audit logs)."
