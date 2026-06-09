#!/usr/bin/env bash
# agent.sh — backend-selecting dispatcher for the harness execution agent.
#
# Point `ecaa-workflow-harness --agent` at THIS script to make the executor
# LLM swappable via ECAA_AGENT_BACKEND, without the harness knowing or caring
# which model runs inside the wrapper (the harness↔agent contract is
# file-based: the wrapper writes runtime/outputs/<task_id>/result.json +
# state.patch.json + .heartbeat and exits; see agent-claude.sh's header).
#
#   ECAA_AGENT_BACKEND=claude   (default) → scripts/agent-claude.sh
#   ECAA_AGENT_BACKEND=codex              → scripts/agent-codex.sh
#
# Pointing --agent directly at agent-claude.sh / agent-codex.sh still works;
# the dispatcher is purely an env-controlled selector. All args + env pass
# through unchanged (exec, not a subshell), so per-task envelope vars
# (ECAA_TASK_ID, ECAA_PACKAGE_ROOT, …) reach the chosen wrapper intact.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "$0")" && pwd)"
backend="${ECAA_AGENT_BACKEND:-claude}"

case "$backend" in
  claude)
    exec "$SCRIPT_DIR/agent-claude.sh" "$@"
    ;;
  codex)
    exec "$SCRIPT_DIR/agent-codex.sh" "$@"
    ;;
  *)
    echo "agent.sh: unknown ECAA_AGENT_BACKEND='$backend' (expected: claude|codex)" >&2
    exit 2
    ;;
esac
