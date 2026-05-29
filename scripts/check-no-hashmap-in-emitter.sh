#!/usr/bin/env bash
# Fails if `HashMap` is used under crates/core/src/emitter/.
# Invariant (CLAUDE.md): emitted packages must be byte-reproducible, so
# the emit path uses BTreeMap (ordered) rather than HashMap (random order).
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

hits=$(grep -rnw 'HashMap' --include='*.rs' crates/core/src/emitter 2>/dev/null \
       | grep -vE ':[0-9]+:\s*//' || true)

if [ -n "$hits" ]; then
  echo "ERROR: HashMap used in the emit path (use BTreeMap for determinism):" >&2
  echo "$hits" >&2
  exit 1
fi
echo "OK: no HashMap in crates/core/src/emitter"
