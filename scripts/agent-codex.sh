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
# container run with the essential mounts, the headless Codex invocation, the
# per-session CLI install (mirrors the Claude path), and ChatGPT/API-key auth.
# The following agent-claude.sh features are INTENTIONALLY DEFERRED and can be
# ported here as the Codex path hardens (each is independent of the contract):
#   - per-task derived images (ECAA_PER_TASK_IMAGES), GPU passthrough,
#     bubblewrap host-sandbox path, per-class budget caps + turn-budget
#     enforcement, the retry/transient-error reconciliation loop, and the
#     cost/telemetry parse of the model's JSON (the metrics layer degrades
#     gracefully on an unrecognised shape — see SessionMetrics "unknown model").
# Auth (handled below, no operator pre-step beyond a host `codex login`):
#   - ECAA_OPENAI_API_KEY → OPENAI_API_KEY (rotation-free headless), OR
#   - a ChatGPT login dir (ECAA_CODEX_AUTH_DIR, default ~/.codex): auth.json +
#     config.toml are COPIED into the per-task agent HOME so the container has
#     writable, refreshable credentials without racing the host's dir.
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

# ── Codex CLI delivery — mirrors agent-claude.sh's per-session @anthropic-ai/
#    claude-code install. `@openai/codex`'s `bin/codex.js` spawns a
#    statically-linked musl native binary (@openai/codex-<platform>), so a
#    host-side `npm install` of the linux-x64 package runs unchanged inside the
#    linux-x64 analysis image. Install into a cache dir, bind-mount its
#    node_modules to /opt/codex/node_modules:ro, and put .bin on the container
#    PATH (bio-min already carries the node runtime the Claude path uses).
#    ECAA_CODEX_BIN can point at an explicit host install's node_modules parent
#    to skip the install; ECAA_AGENT_CODEX_DISABLE=1 falls back to an in-image
#    codex.
CODEX_BIN_ARGS=()
CODEX_PATH_PREFIX=""
if [ "${ECAA_AGENT_CODEX_DISABLE:-0}" != "1" ]; then
    if [ -n "${ECAA_CODEX_INSTALL_DIR:-}" ]; then
        CODEX_INSTALL_DIR="$ECAA_CODEX_INSTALL_DIR"
    elif [ -n "${ECAA_SESSION_CACHE_DIR:-}" ]; then
        CODEX_INSTALL_DIR="$ECAA_SESSION_CACHE_DIR/codex-cli"
    else
        CODEX_INSTALL_DIR="${ECAA_AGENT_CACHE_DIR:-$HOME/.ecaa-workflow/agent-cache}/standalone-$(basename "$PACKAGE")/codex-cli"
    fi
    CODEX_PKG_JSON="$CODEX_INSTALL_DIR/node_modules/@openai/codex/package.json"
    CODEX_VERSION="${ECAA_AGENT_CODEX_VERSION:-latest}"
    __codex_installed=""
    if [ -f "$CODEX_PKG_JSON" ] && [ "${ECAA_AGENT_CODEX_FORCE_REINSTALL:-0}" != "1" ]; then
        __codex_installed="$(jq -r .version "$CODEX_PKG_JSON" 2>/dev/null || echo "")"
    fi
    if [ -z "$__codex_installed" ] && command -v npm >/dev/null 2>&1; then
        mkdir -p "$CODEX_INSTALL_DIR" 2>/dev/null || true
        echo "agent-codex.sh: installing @openai/codex@$CODEX_VERSION into $CODEX_INSTALL_DIR (one-time)..." >&2
        npm install --prefix "$CODEX_INSTALL_DIR" --silent --no-audit --no-fund "@openai/codex@$CODEX_VERSION" >/dev/null 2>&1 || true
        __codex_installed="$(jq -r .version "$CODEX_PKG_JSON" 2>/dev/null || echo "")"
    fi
    if [ -n "$__codex_installed" ]; then
        CODEX_BIN_ARGS+=(-v "$CODEX_INSTALL_DIR/node_modules":/opt/codex/node_modules:ro)
        CODEX_PATH_PREFIX="/opt/codex/node_modules/.bin:"
        echo "agent-codex.sh: using codex $__codex_installed from mounted install." >&2
    else
        echo "agent-codex.sh: codex install unavailable; falling back to the image's bundled codex (if any)." >&2
    fi
fi

# ── Codex auth. ECAA_OPENAI_API_KEY (if set) is the rotation-free headless
#    path. Otherwise use a ChatGPT login dir (ECAA_CODEX_AUTH_DIR, default
#    ~/.codex): COPY auth.json + config.toml into the per-task agent HOME's
#    .codex so the container reads writable credentials it can refresh in place
#    (the ChatGPT token rotates) WITHOUT racing or mutating the host's dir.
CODEX_AUTH_ARGS=()
if [ -n "${ECAA_OPENAI_API_KEY:-}" ]; then
    CODEX_AUTH_ARGS+=(-e "OPENAI_API_KEY=$ECAA_OPENAI_API_KEY")
fi
__codex_auth_src="${ECAA_CODEX_AUTH_DIR:-$HOME/.codex}"
if [ -z "${ECAA_OPENAI_API_KEY:-}" ] && [ -f "$__codex_auth_src/auth.json" ]; then
    mkdir -p "$AGENT_HOME_DIR/.codex" 2>/dev/null || true
    cp "$__codex_auth_src/auth.json" "$AGENT_HOME_DIR/.codex/auth.json" 2>/dev/null || true
    [ -f "$__codex_auth_src/config.toml" ] && cp "$__codex_auth_src/config.toml" "$AGENT_HOME_DIR/.codex/config.toml" 2>/dev/null || true
    chmod 600 "$AGENT_HOME_DIR/.codex/auth.json" "$AGENT_HOME_DIR/.codex/config.toml" 2>/dev/null || true
    # $HOME maps to AGENT_HOME_DIR in the container, so ~/.codex resolves to
    # this writable copy — no extra mount needed.
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
    --user "$(id -u):$(id -g)" \
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
    -e "PATH=${CODEX_PATH_PREFIX}/opt/conda/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
    -e "ECAA_TASK_ID=${ECAA_TASK_ID:-}" \
    -e "ECAA_PACKAGE_ROOT=${ECAA_PACKAGE_ROOT:-$PACKAGE}" \
    "$CONTAINER_IMAGE" \
    codex exec --yolo --skip-git-repo-check "${CODEX_MODEL_ARGS[@]}" "$PROMPT" \
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
