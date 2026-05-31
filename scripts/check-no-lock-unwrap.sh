#!/usr/bin/env bash
# Fails on production `.lock().unwrap()` in the RC-21-converted dirs
# (git_routes + executor/aws). SCOPED on purpose: the repo has ~124
# pre-existing prod `.lock().unwrap()` sites elsewhere, so a blanket gate
# would fail immediately — full coverage is a tracked follow-up that
# extends `dirs` as each module is converted. Annotate any deliberate
# exception with a trailing `// lock-unwrap-allow:<reason>`.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

dirs=(crates/server/src/git_routes crates/harness/src/executor/aws)

hits=$(grep -rn '\.lock()\.unwrap()' --include='*.rs' "${dirs[@]}" 2>/dev/null \
       | grep -v 'lock-unwrap-allow' || true)

if [ -n "$hits" ]; then
  echo "ERROR: bare .lock().unwrap() in an RC-21-hardened dir (use poison-recovery):" >&2
  echo "$hits" >&2
  exit 1
fi
echo "OK: no bare .lock().unwrap() in ${dirs[*]}"
