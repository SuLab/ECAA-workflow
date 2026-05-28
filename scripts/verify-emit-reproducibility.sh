#!/usr/bin/env bash
set -euo pipefail

# Verifies that `make ivd` produces byte-identical output.
# Excludes the two CLAUDE.md-exempt audit-trail files
# (intake-conversation.jsonl, decisions.jsonl).
#
# Usage:
#   ./scripts/verify-emit-reproducibility.sh capture <label>
#       Runs `make ivd` and writes SHA256 manifest to /tmp/ivd-<label>.sha256
#   ./scripts/verify-emit-reproducibility.sh diff <label-a> <label-b>
#       Diffs two captured manifests; exits 0 iff identical.

mode="${1:-}"
case "$mode" in
  capture)
    label="${2:?label required}"
    out="/tmp/ivd-${label}.sha256"
    rm -rf runtime/
    make ivd
    find runtime -type f \
      ! -name 'intake-conversation.jsonl' \
      ! -name 'decisions.jsonl' \
      | sort | xargs sha256sum > "$out"
    echo "Wrote $out ($(wc -l < "$out") files)"
    ;;
  diff)
    a="${2:?label-a required}"
    b="${3:?label-b required}"
    diff "/tmp/ivd-${a}.sha256" "/tmp/ivd-${b}.sha256"
    echo "Manifests are byte-identical."
    ;;
  *)
    echo "Usage: $0 capture <label>  |  $0 diff <a> <b>"
    exit 64
    ;;
esac
