#!/usr/bin/env bash
# Regenerates ts-rs bindings and fails if ui/src/types/ is out of date.
# Run `make types` and commit the result to fix a failure.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# Refuse to run with pre-existing uncommitted changes under ui/src/types,
# so a failure unambiguously means "bindings are stale", not "you had local edits".
if ! git diff --quiet -- ui/src/types; then
  echo "ERROR: ui/src/types has uncommitted changes; commit or stash before running this gate." >&2
  exit 1
fi

make types >/dev/null

if ! git diff --quiet -- ui/src/types; then
  echo "ERROR: ui/src/types is stale. Run 'make types' and commit:" >&2
  git --no-pager diff --stat -- ui/src/types >&2
  git checkout -- ui/src/types
  exit 1
fi
echo "OK: ui/src/types is in sync with Rust types"
