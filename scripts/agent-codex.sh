#!/usr/bin/env bash
# agent-codex.sh — Invoke the Codex CLI as a harness execution agent.
# Called by ecaa-workflow-harness as: agent-codex.sh <package_dir>
# (usually via scripts/agent.sh with ECAA_AGENT_BACKEND=codex).
#
# This is the Codex sibling of agent-claude.sh. It honours the SAME
# harness↔agent contract: pick up the ONE ready task named by ECAA_TASK_ID,
# run it inside the analysis container, and let the agent write its outputs +
# runtime/outputs/<task_id>/{result.json,state.patch.json} + .heartbeat. The
# harness reads those files (NOT the LLM's stdout) to decide the task outcome,
# so the choice of LLM is invisible to the harness.
#
# SCAFFOLDING SCOPE. This wrapper implements the load-bearing core: the
# file-contract, heartbeat liveness, prompt assembly, a security-hardened
# container run with the essential mounts, and the headless Codex invocation.
# The following agent-claude.sh features are INTENTIONALLY DEFERRED and can be
# ported here as the Codex path hardens (each is independent of the contract):
#   - per-task derived images (ECAA_PER_TASK_IMAGES), GPU passthrough,
#     bubblewrap host-sandbox path, per-class budget caps + turn-budget
#     enforcement, the retry/transient-error reconciliation loop, and the
#     cost/telemetry parse of the model's JSON (the metrics layer degrades
#     gracefully on an unrecognised shape — see SessionMetrics "unknown model").
# Operator prerequisites (NOT script-side):
#   - the `codex` CLI must be runnable inside $CONTAINER_IMAGE. Either bake it
#     into the image, or set ECAA_CODEX_BIN to a host codex binary that is
#     ABI-compatible with the container; it is bind-mounted to /usr/local/bin.
#   - Codex auth: set ECAA_OPENAI_API_KEY (threaded as OPENAI_API_KEY) and/or
#     mount a ChatGPT-login ~/.codex via ECAA_CODEX_AUTH_DIR.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "$0")" && pwd)"
# Reuse the shared helpers + tunable defaults (validate_task_id,
# load_task_execution_prompt, heartbeat interval, tmpfs/pids limits, …).
source "$SCRIPT_DIR/agent-claude-common.sh"

if [ -n "${ECAA_CHAT_SESSION_ID:-}" ]; then
    validate_uuid "$ECAA_CHAT_SESSION_ID"
fi
if [ -n "${ECAA_TASK_ID:-}" ]; then
    validate_task_id "$ECAA_TASK_ID"
fi

if [ "$#" -lt 1 ]; then
    echo "usage: agent-codex.sh <package_dir>" >&2
    exit 2
fi
PACKAGE="$(realpath "$1")"

# ── Heartbeat liveness (mirrors agent-claude.sh): prove the task is alive so
#    the harness stall-monitor doesn't false-block a long compute step. The
#    writer tolerates transient write failures and exits only when this parent
#    process is gone.
HEARTBEAT_PID=""
if [ -n "${ECAA_TASK_ID:-}" ]; then
    HEARTBEAT_FILE="$PACKAGE/runtime/outputs/$ECAA_TASK_ID/.heartbeat"
    mkdir -p "$(dirname "$HEARTBEAT_FILE")" 2>/dev/null || true
    __hb_parent=$$
    ( while :; do
        date -u +%Y-%m-%dT%H:%M:%SZ > "$HEARTBEAT_FILE" 2>/dev/null || true
        kill -0 "$__hb_parent" 2>/dev/null || exit 0
        sleep "$ECAA_HEARTBEAT_INTERVAL_SECS"
      done ) &
    HEARTBEAT_PID=$!
fi
cleanup() {
    [ -n "$HEARTBEAT_PID" ] && kill "$HEARTBEAT_PID" 2>/dev/null || true
}
trap cleanup EXIT

# ── Prompt assembly: the package PROMPT.md (SME intent + standing rules) plus
#    the shared task-execution contract (how to write result.json/state.patch).
#    This is the SAME contract the Claude path uses, so the agent's file
#    outputs are backend-independent.
TASK_EXECUTION_BODY="$(load_task_execution_prompt "$SCRIPT_DIR/agent-prompts/task-execution.md")"
PROMPT="$(cat "$PACKAGE/PROMPT.md")

## Package location
All paths are relative to: $PACKAGE

${TASK_EXECUTION_BODY}"

# ── Container image (same resolution fallback the Claude path's default arm
#    uses; per-task derived images are deferred — see SCAFFOLDING SCOPE).
CONTAINER_IMAGE="${ECAA_DEFAULT_CONTAINER_IMAGE:-bio-min:local}"

# ── Per-task scratch + agent HOME (writable; the container is --read-only).
AGENT_HOME_DIR="${ECAA_AGENT_HOME_DIR:-$PACKAGE/runtime/agent-home}"
mkdir -p "$AGENT_HOME_DIR" 2>/dev/null || true
SCRATCH_ARGS=()
if [ -n "${ECAA_TASK_ID:-}" ]; then
    SCRATCH_BASE="${ECAA_AGENT_SCRATCH_DIR:-$PACKAGE/runtime/scratch}"
    SCRATCH_DIR="$SCRATCH_BASE/$ECAA_TASK_ID"
    mkdir -p "$SCRATCH_DIR" 2>/dev/null || true
    SCRATCH_ARGS+=(-v "$SCRATCH_DIR":"$SCRATCH_DIR":rw -e "ECAA_TASK_SCRATCH_DIR=$SCRATCH_DIR")
fi

# ── Codex CLI delivery: bind-mount a host codex binary unless the image
#    already carries one. ECAA_CODEX_BIN may name an explicit path; otherwise
#    fall back to the host's `codex` on PATH. When neither resolves we assume
#    the image provides codex and mount nothing.
CODEX_BIN_ARGS=()
__codex_bin="${ECAA_CODEX_BIN:-$(command -v codex 2>/dev/null || true)}"
if [ -n "$__codex_bin" ] && [ -x "$__codex_bin" ]; then
    CODEX_BIN_ARGS+=(-v "$__codex_bin":/usr/local/bin/codex:ro)
fi

# ── Codex auth: API key (preferred for headless) + optional ChatGPT-login dir.
CODEX_AUTH_ARGS=()
if [ -n "${ECAA_OPENAI_API_KEY:-}" ]; then
    CODEX_AUTH_ARGS+=(-e "OPENAI_API_KEY=$ECAA_OPENAI_API_KEY")
fi
if [ -n "${ECAA_CODEX_AUTH_DIR:-}" ] && [ -d "$ECAA_CODEX_AUTH_DIR" ]; then
    CODEX_AUTH_ARGS+=(-v "$ECAA_CODEX_AUTH_DIR":"$HOME/.codex":ro)
fi

# ── Codex invocation. `codex exec` is the NON-INTERACTIVE subcommand (bare
#    `codex --yolo` launches the TUI and would hang the harness). `--yolo`
#    (= --dangerously-bypass-approvals-and-sandbox) runs fully autonomously,
#    matching `claude --dangerously-skip-permissions`. The model is
#    env-selectable; an explicit ECAA_AGENT_MODEL_OVERRIDE (set by the eval for
#    arm-fairness) wins over the codex-specific default.
CODEX_MODEL="${ECAA_AGENT_MODEL_OVERRIDE:-${ECAA_AGENT_CODEX_MODEL:-}}"
CODEX_MODEL_ARGS=()
[ -n "$CODEX_MODEL" ] && CODEX_MODEL_ARGS+=(--model "$CODEX_MODEL")

# Capture the run for logs/telemetry (best-effort; the contract is the files
# the agent writes, not this stream).
CODEX_OUT_LOG=""
if [ -n "${ECAA_TASK_ID:-}" ]; then
    CODEX_OUT_LOG="$PACKAGE/runtime/outputs/$ECAA_TASK_ID/agent-codex.log"
fi

set +e
docker run --rm \
    --read-only \
    --tmpfs "/tmp:rw,size=$ECAA_DOCKER_TMPFS_TMP_SIZE,mode=1777" \
    --tmpfs "/var/tmp:rw,size=$ECAA_DOCKER_TMPFS_VARTMP_SIZE,mode=1777" \
    --security-opt no-new-privileges \
    --cap-drop=ALL \
    --pids-limit "$ECAA_DOCKER_PIDS_LIMIT" \
    -v "$PACKAGE":"$PACKAGE":rw \
    -v "$AGENT_HOME_DIR":"$HOME":rw \
    -v "$SCRIPT_DIR/ecaa-install":/usr/local/bin/ecaa-install:ro \
    -v "$SCRIPT_DIR/agent_literature_fetch.py":/opt/ecaa/agent_literature_fetch.py:ro \
    "${CODEX_BIN_ARGS[@]}" \
    "${CODEX_AUTH_ARGS[@]}" \
    "${SCRATCH_ARGS[@]}" \
    -w "$PACKAGE" \
    -e "HOME=$HOME" \
    -e "ECAA_TASK_ID=${ECAA_TASK_ID:-}" \
    -e "ECAA_PACKAGE_ROOT=${ECAA_PACKAGE_ROOT:-$PACKAGE}" \
    "$CONTAINER_IMAGE" \
    codex exec --yolo "${CODEX_MODEL_ARGS[@]}" "$PROMPT" \
    > >(if [ -n "$CODEX_OUT_LOG" ]; then tee "$CODEX_OUT_LOG"; else cat; fi) 2>&1
CODEX_EXIT=$?
set -e

# The harness reconciles outcome from result.json / state.patch.json that the
# agent wrote; the exit code is recorded but not authoritative. Surface a
# non-zero codex exit for the harness's stderr_tail diagnostics.
if [ "$CODEX_EXIT" -ne 0 ]; then
    echo "agent-codex.sh: codex exec exited $CODEX_EXIT (task ${ECAA_TASK_ID:-<none>})" >&2
fi
exit "$CODEX_EXIT"
