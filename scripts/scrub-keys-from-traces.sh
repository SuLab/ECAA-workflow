#!/usr/bin/env bash
# Scrub-keys-from-traces.sh — redact secret patterns from emitted package traces.
#
# Walks every emitted package under $SWFC_PACKAGE_ROOT (default
# ~/.scripps-workflow/packages) and rewrites `agent-trace.log` files
# to redact known secret patterns:
#
# - Anthropic API keys (sk-ant-api<N>-...)
# - HuggingFace tokens (hf_...)
# - GitHub PAT classic (ghp_...)
# - GitHub PAT fine-grained (github_pat_...)
# - AWS access-key id (AKIA...)
#
# This is the operator-side remediation for trace artifacts that
# pre-date the xtrace suppression patch in scripts/agent-claude*.sh.
# Idempotent. Safe to run repeatedly. Use after suspicion of leakage
# or as part of routine offline cleanup.
#
# Usage:
#  bash scripts/scrub-keys-from-traces.sh
#  SWFC_PACKAGE_ROOT=/custom/path bash scripts/scrub-keys-from-traces.sh

set -euo pipefail

ROOT="${SWFC_PACKAGE_ROOT:-$HOME/.scripps-workflow/packages}"
if [ ! -d "$ROOT" ]; then
    echo "no package root at $ROOT — nothing to scrub"
    exit 0
fi

count=0
# `-print0` + read -d '' tolerates filenames with spaces / newlines.
while IFS= read -r -d '' f; do
    # sed -i with a `.bak` sidecar gives portable atomic-ish in-place
    # rewrite across GNU sed and BSD sed. Remove the sidecar after a
    # successful rewrite.
    sed -E -i.bak \
        -e 's/sk-ant-api[0-9]+-[A-Za-z0-9_-]{20,}/REDACTED/g' \
        -e 's/hf_[A-Za-z0-9]{20,}/REDACTED/g' \
        -e 's/ghp_[A-Za-z0-9]{20,}/REDACTED/g' \
        -e 's/github_pat_[A-Za-z0-9_]{20,}/REDACTED/g' \
        -e 's/AKIA[0-9A-Z]{16}/REDACTED/g' \
        "$f"
    rm -f "$f.bak"
    count=$((count + 1))
done < <(find "$ROOT" -type f -name 'agent-trace.log' -print0)

echo "scrubbed $count trace files under $ROOT"
