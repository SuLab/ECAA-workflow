#!/usr/bin/env bash
# Fails if `tokio` is referenced in crates/core/src or crates/harness/src.
# Invariant (CLAUDE.md): the compiler core and the sync harness must not
# depend on tokio. tokio belongs to server, conversation, and cli's async
# subcommands only.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# Match `use tokio` or `tokio::` in real code; ignore comment-only lines.
hits=$(grep -rnE '(^|[^[:alnum:]_])tokio(::|[[:space:]]|$)' \
         --include='*.rs' crates/core/src crates/harness/src 2>/dev/null \
       | grep -vE ':[0-9]+:\s*//' || true)

if [ -n "$hits" ]; then
  echo "ERROR: tokio referenced in core/harness (forbidden):" >&2
  echo "$hits" >&2
  exit 1
fi
echo "OK: no tokio in core/harness"
