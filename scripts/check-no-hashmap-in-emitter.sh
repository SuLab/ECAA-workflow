#!/usr/bin/env bash
# Fails if `HashMap` is used under the emit path:
#   crates/core/src/emitter/ and crates/core/src/backend_emitters/.
# Invariant (CLAUDE.md): emitted packages must be byte-reproducible, so
# the emit path uses BTreeMap (ordered) rather than HashMap (random order).
# The lowering pass under backend_emitters/ feeds the same emitted bytes,
# so it is held to the same determinism gate as emitter/.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

dirs=(crates/core/src/emitter crates/core/src/backend_emitters)

hits=$(grep -rnw 'HashMap' --include='*.rs' "${dirs[@]}" 2>/dev/null \
       | grep -vE ':[0-9]+:\s*//' || true)

if [ -n "$hits" ]; then
  echo "ERROR: HashMap used in the emit path (use BTreeMap for determinism):" >&2
  echo "$hits" >&2
  exit 1
fi
echo "OK: no HashMap in ${dirs[*]}"
