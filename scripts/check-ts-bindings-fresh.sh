#!/usr/bin/env bash
# Fails if any committed ts-rs binding disagrees with the Rust types it is
# generated from. Run `make types` and commit the result to fix a failure.
#
# Two independent checks, in this order:
#
#   1. CONTENT — snapshot every generated tree, run `make types`, diff. This is
#      git-independent on purpose: it catches a hand-edited binding, a Rust type
#      changed without regenerating, and a type deleted without pruning its
#      `.ts`. On failure the snapshot is restored so the working tree is left
#      exactly as it was found.
#
#   2. COMMITTED — `git status` over the same trees must be clean, so that what
#      is on disk is also what a push would carry. Catches "ran `make types`,
#      forgot to `git add`". Purely advisory of state; never mutates the tree.
#
# Check 1 must come first: if the cleanliness check ran first, any local edit
# would short-circuit the gate and the content invariant would never be tested.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
ROOT="$PWD"

# Every tree `make types` writes. Keep in sync with scripts/regen-ts-bindings.sh.
TREES=(
  ui/src/types
  crates/ecaa-types/bindings
  crates/core/bindings
  crates/conversation/bindings
  crates/server/bindings
  crates/harness/bindings
)

SNAP=$(mktemp -d)
restored=0
restore() {
  [ "$restored" -eq 1 ] && return 0
  restored=1
  for t in "${TREES[@]}"; do
    if [ -d "$SNAP/$(echo "$t" | tr / _)" ]; then
      rm -rf "$ROOT/$t"
      mkdir -p "$(dirname "$ROOT/$t")"
      cp -a "$SNAP/$(echo "$t" | tr / _)" "$ROOT/$t"
    fi
  done
}
cleanup() { rm -rf "$SNAP"; }
trap cleanup EXIT

for t in "${TREES[@]}"; do
  [ -d "$ROOT/$t" ] && cp -a "$ROOT/$t" "$SNAP/$(echo "$t" | tr / _)"
done

# Cargo's compile chatter is noise here; keep it unless the regen fails, but
# always surface the collision WARN/ERROR lines from regen-ts-bindings.sh.
regen_log="$SNAP/.make-types.log"
if ! make types >"$regen_log" 2>&1; then
  echo "ERROR: 'make types' failed:" >&2
  cat "$regen_log" >&2
  restore
  exit 1
fi
grep -E '(WARN|ERROR):' "$regen_log" >&2 || true

drift=0
for t in "${TREES[@]}"; do
  if ! diff -rq "$SNAP/$(echo "$t" | tr / _)" "$ROOT/$t" >/dev/null 2>&1; then
    if [ "$drift" -eq 0 ]; then
      echo "ERROR: generated TypeScript bindings are stale. Run 'make types' and commit:" >&2
    fi
    diff -rq "$SNAP/$(echo "$t" | tr / _)" "$ROOT/$t" 2>&1 | sed "s|$SNAP/$(echo "$t" | tr / _)|<committed>|g; s|$ROOT/$t|<regenerated>|g" >&2
    drift=1
  fi
done

if [ "$drift" -eq 1 ]; then
  restore
  exit 1
fi

# Check 2. Untracked files matter as much as modified ones: a brand-new
# `#[ts(export)]` type produces a file `git diff` alone would never report.
dirty=$(git status --porcelain -- "${TREES[@]}")
if [ -n "$dirty" ]; then
  echo "ERROR: generated bindings match the Rust types but are not committed:" >&2
  echo "$dirty" >&2
  echo "       Commit the regenerated bindings (git add ${TREES[*]})." >&2
  exit 1
fi

echo "OK: generated TypeScript bindings are in sync with the Rust types"
