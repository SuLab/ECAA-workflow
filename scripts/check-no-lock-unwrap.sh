#!/usr/bin/env bash
# Fails on production `.lock().unwrap()` anywhere in crates/*/src.
# RC-21 invariant: never `lock().unwrap()` — always poison-recover via
# `.lock().unwrap_or_else(|p| p.into_inner())` or a `lock_recover`/
# `git_lock_recover` helper. Test-module sites are exempt via a trailing
# `// lock-unwrap-allow:<reason>` annotation; full-line comments are ignored.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

hits=$(grep -rn '\.lock()\.unwrap()' --include='*.rs' crates/*/src 2>/dev/null \
       | grep -vE ':[0-9]+:[[:space:]]*//' \
       | grep -v 'lock-unwrap-allow' || true)

if [ -n "$hits" ]; then
  echo "ERROR: bare .lock().unwrap() found (use poison-recovery per RC-21):" >&2
  echo "$hits" >&2
  exit 1
fi
echo "OK: no bare .lock().unwrap() in crates/*/src"
