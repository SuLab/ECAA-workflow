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
# EXPERIMENTAL backend: gated behind ECAA_AGENT_CODEX_EXPERIMENTAL=1 (refuses
# to run otherwise) so it can't be selected silently in production.
# The following agent-claude.sh features are INTENTIONALLY DEFERRED and can be
# ported here as the Codex path hardens (each is independent of the contract):
#   - per-task derived images (ECAA_PER_TASK_IMAGES), GPU passthrough,
#     bubblewrap host-sandbox path, and the cost/telemetry parse of the model's
#     JSON (the metrics layer degrades gracefully on an unrecognised shape —
#     see SessionMetrics "unknown model").
# Now ported from the Claude path: the memory/CPU resource fences
# (DOCKER_MEMORY_ARGS/DOCKER_CPU_ARGS), the retry/transient-error
# reconciliation loop (run_codex_with_retries), and a per-class budget — but
# codex has NO native --max-budget-usd, so the budget is passed in as
# ECAA_TASK_BUDGET_USD for the task-execution contract to SOFT-enforce only.
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

# ── Experimental opt-in gate. Codex is an EXPERIMENTAL backend (no per-task
#    images, GPU, or cost-telemetry parity with the claude path yet — see
#    docs/known-limitations.md). Require explicit opt-in so it can't be
#    selected silently in production.
if [ "${ECAA_AGENT_CODEX_EXPERIMENTAL:-0}" != "1" ]; then
    echo "agent-codex.sh: codex backend is experimental; set ECAA_AGENT_CODEX_EXPERIMENTAL=1 to use it." >&2
    exit 2
fi

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
# The agent HOME MUST live OUTSIDE the emitted package root. The ChatGPT
# OAuth token is copied into $AGENT_HOME_DIR/.codex/auth.json below; keeping
# it out of $PACKAGE means it is never served by the artifact route
# (GET .../artifacts/*) nor staged by the provenance `git add -A`. Mirrors
# agent-claude.sh's placement under the session / agent cache.
if [ -n "${ECAA_AGENT_HOME_DIR:-}" ]; then
    AGENT_HOME_DIR="$ECAA_AGENT_HOME_DIR"
elif [ -n "${ECAA_SESSION_CACHE_DIR:-}" ]; then
    AGENT_HOME_DIR="$ECAA_SESSION_CACHE_DIR/agent-codex-home"
else
    AGENT_HOME_DIR="${ECAA_AGENT_CACHE_DIR:-$HOME/.ecaa-workflow/agent-cache}/standalone-$(basename "$PACKAGE")/agent-codex-home"
fi
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

# ── Per-task budget translation. Codex has NO native --max-budget-usd flag
#    (the claude path's hard CLI ceiling), so the per-class budget is passed
#    into the container as ECAA_TASK_BUDGET_USD for the task-execution
#    contract to soft-enforce. Same class buckets + calibrated p99 caps as
#    agent-claude.sh; per-class envs override defaults, ECAA_AGENT_BUDGET_USD
#    overrides all classes, and `0` opts out. NOTE: this is advisory only —
#    codex lacks a hard CLI budget cap (see docs/known-limitations.md).
CODEX_BUDGET_ENV_ARGS=()
if [ "${ECAA_AGENT_MODEL_TIER:-1}" = "1" ] && [ -n "${ECAA_TASK_ID:-}" ]; then
  case "$ECAA_TASK_ID" in
    validate_*)
      _BUDGET="${ECAA_AGENT_BUDGET_USD_VALIDATE:-1.25}"
      ;;
    discover_*)
      _BUDGET="${ECAA_AGENT_BUDGET_USD_DISCOVER:-3.00}"
      ;;
    data_acquisition|data_import)
      _BUDGET="${ECAA_AGENT_BUDGET_USD_DATA_ACQ:-2.00}"
      ;;
    *)
      # `kind: discovery` is a legacy spelling used by a handful of archetype
      # atoms before the discover_/validate_ prefix convention. Cheap jq read
      # gated on jq + WORKFLOW.json being present.
      TID_KIND=""
      if command -v jq >/dev/null 2>&1 && [ -f "$PACKAGE/WORKFLOW.json" ]; then
        TID_KIND="$(jq -r --arg tid "$ECAA_TASK_ID" '
          .tasks[$tid].kind | if type == "object" then (keys[0]) else . end
        ' "$PACKAGE/WORKFLOW.json" 2>/dev/null)"
      fi
      if [ "$TID_KIND" = "discovery" ]; then
        _BUDGET="${ECAA_AGENT_BUDGET_USD_DISCOVER:-3.00}"
      else
        _BUDGET="${ECAA_AGENT_BUDGET_USD_ANALYTICAL:-3.00}"
      fi
      ;;
  esac
  # Global override beats per-class.
  _BUDGET="${ECAA_AGENT_BUDGET_USD:-$_BUDGET}"
  # `0` opts out; any positive number is the dollar ceiling for soft enforcement.
  if [ -n "$_BUDGET" ] && [ "$_BUDGET" != "0" ]; then
    CODEX_BUDGET_ENV_ARGS+=(-e "ECAA_TASK_BUDGET_USD=$_BUDGET")
  fi
fi

# ── Host-path memory cap. When ECAA_AGENT_MEMORY_CAP_GB is set we hand the
#    cap to `docker run --memory=<N>g`; --memory-reservation (docker's
#    MemoryHigh equivalent) at 85% so OOM-kill is the last resort. Falls back
#    to the dynamic-sizing slice (ECAA_HW_MEMORY_GB) in container mode.
#    Ported from agent-claude.sh (the non-systemd docker arm).
DOCKER_MEMORY_ARGS=()
AGENT_MEMORY_LIMIT_GB=""
if [ -n "${ECAA_AGENT_MEMORY_CAP_GB:-}" ]; then
  if ! [[ "$ECAA_AGENT_MEMORY_CAP_GB" =~ ^[0-9]+$ ]]; then
    echo "agent-codex.sh: ECAA_AGENT_MEMORY_CAP_GB must be a positive integer (got '$ECAA_AGENT_MEMORY_CAP_GB'); ignoring." >&2
  else
    AGENT_MEMORY_LIMIT_GB="$ECAA_AGENT_MEMORY_CAP_GB"
  fi
elif [[ "${ECAA_HW_MEMORY_GB:-}" =~ ^[0-9]+$ ]] && [ "$ECAA_HW_MEMORY_GB" -gt 0 ]; then
  # Dynamic local sizing provides a per-agent memory slice as ECAA_HW_MEMORY_GB.
  # In container mode, make that an actual cgroup limit.
  AGENT_MEMORY_LIMIT_GB="$ECAA_HW_MEMORY_GB"
fi
if [ -n "$AGENT_MEMORY_LIMIT_GB" ]; then
  DOCKER_MEMORY_RESERVATION_MB=$((AGENT_MEMORY_LIMIT_GB * 1024 * 85 / 100))
  DOCKER_MEMORY_ARGS=(
    "--memory=${AGENT_MEMORY_LIMIT_GB}g"
    "--memory-reservation=${DOCKER_MEMORY_RESERVATION_MB}m"
  )
fi

# ── CPU cap. Pin the container to the dynamic-sizing CPU slice when present.
#    Ported from agent-claude.sh.
DOCKER_CPU_ARGS=()
__agent_container_cpus="${ECAA_HW_NPROC_HINT:-${ECAA_HW_VCPUS_AVAILABLE:-}}"
if [[ "$__agent_container_cpus" =~ ^[0-9]+$ ]] && [ "$__agent_container_cpus" -gt 0 ]; then
  DOCKER_CPU_ARGS+=(--cpus "$__agent_container_cpus")
fi
unset __agent_container_cpus

# Capture the run for logs/telemetry (best-effort; the contract is the files
# the agent writes, not this stream).
CODEX_OUT_LOG=""
if [ -n "${ECAA_TASK_ID:-}" ]; then
    CODEX_OUT_LOG="$PACKAGE/runtime/outputs/$ECAA_TASK_ID/agent-codex.log"
fi

# Return success when agent-codex.log's tail shows a transient transport
# failure (socket/connection/network/5xx) rather than an agent-authored task
# failure. Callers use this to retry the same task; deterministic analysis or
# validation errors must NOT match here. Codex has no structured terminal JSON
# like the claude path, so this greps the raw log tail for transient markers.
codex_log_transient_error() {
  local out_log="$1"
  [ -f "$out_log" ] || return 1
  tail -n 40 "$out_log" 2>/dev/null | grep -Eiq \
    "socket connection was closed unexpectedly|connection reset|ECONNRESET|ETIMEDOUT|fetch failed|network error|timed out|temporarily unavailable|stream error|502 Bad Gateway|503 Service Unavailable|504 Gateway"
}

# Run codex while preserving wrapper cleanup on nonzero exits. A transient
# API/socket failure can otherwise leave the task failed even though a retry
# would be safe and cheap. The retry is narrow: no state.patch.json written,
# the log tail classified as a transport error, and bounded attempts. Mirrors
# run_claude_with_retries in agent-claude.sh.
run_codex_with_retries() {
  local max_attempts="${ECAA_AGENT_TRANSIENT_MAX_ATTEMPTS:-2}"
  if ! [[ "$max_attempts" =~ ^[0-9]+$ ]] || [ "$max_attempts" -lt 1 ]; then
    max_attempts=1
  fi

  local attempt=1
  local exit_code=0
  local task_dir=""
  local patch_path=""
  if [ -n "${ECAA_TASK_ID:-}" ]; then
    task_dir="$PACKAGE/runtime/outputs/$ECAA_TASK_ID"
    patch_path="$task_dir/state.patch.json"
    mkdir -p "$task_dir" 2>/dev/null || true
  fi

  if [ -n "$CODEX_OUT_LOG" ]; then
    : > "$CODEX_OUT_LOG"
  fi
  while :; do
    if [ "$attempt" -gt 1 ] && [ -n "$task_dir" ]; then
      printf '[agent-retry] retrying transient codex transport error (attempt %s/%s)\n' \
        "$attempt" "$max_attempts" >> "$task_dir/progress.log" 2>/dev/null || true
    fi

    set +e
    if [ -n "$CODEX_OUT_LOG" ]; then
      "$@" 2>&1 | tee -a "$CODEX_OUT_LOG"
      exit_code="${PIPESTATUS[0]}"
    else
      "$@" 2>&1
      exit_code=$?
    fi
    set -e

    if [ "$exit_code" = "0" ]; then
      if codex_log_transient_error "$CODEX_OUT_LOG" \
         && [ -n "$patch_path" ] \
         && [ ! -s "$patch_path" ] \
         && [ "$attempt" -lt "$max_attempts" ]; then
        attempt=$((attempt + 1))
        sleep 5
        continue
      fi
      return 0
    fi

    if [ -n "$patch_path" ] && [ -s "$patch_path" ]; then
      return "$exit_code"
    fi

    if codex_log_transient_error "$CODEX_OUT_LOG" \
       && [ "$attempt" -lt "$max_attempts" ]; then
      if [ -n "$task_dir" ]; then
        printf '[agent-retry] transient codex transport error after attempt %s/%s; retrying\n' \
          "$attempt" "$max_attempts" >> "$task_dir/progress.log" 2>/dev/null || true
      fi
      attempt=$((attempt + 1))
      sleep 5
      continue
    fi

    return "$exit_code"
  done
}

set +e
# DooD-safe helper staging (see agent-claude.sh for the full rationale). Under
# the container-first deployment $SCRIPT_DIR is a container-only path, so a
# `-v "$SCRIPT_DIR/ecaa-install":...` bind resolves on the HOST where the path
# is absent — docker then creates an empty dir and the sibling sees no
# ecaa-install. Stage the helpers into a host-shared, identical-path-mounted
# directory and mount from there; on a pure-host harness this path is equally
# valid. Falls back to $SCRIPT_DIR when staging can't be written.
HELPER_MOUNT_DIR="${ECAA_SESSION_CACHE_DIR:-$HOME/.ecaa-workflow/agent-cache}/helpers"
mkdir -p "$HELPER_MOUNT_DIR" 2>/dev/null || true
if cp -f "$SCRIPT_DIR/ecaa-install" "$HELPER_MOUNT_DIR/ecaa-install" 2>/dev/null; then
  chmod 0755 "$HELPER_MOUNT_DIR/ecaa-install" 2>/dev/null || true
  ECAA_INSTALL_MOUNT_SRC="$HELPER_MOUNT_DIR/ecaa-install"
else
  ECAA_INSTALL_MOUNT_SRC="$SCRIPT_DIR/ecaa-install"
fi
if cp -f "$SCRIPT_DIR/agent_literature_fetch.py" "$HELPER_MOUNT_DIR/agent_literature_fetch.py" 2>/dev/null; then
  LIT_FETCH_MOUNT_SRC="$HELPER_MOUNT_DIR/agent_literature_fetch.py"
else
  LIT_FETCH_MOUNT_SRC="$SCRIPT_DIR/agent_literature_fetch.py"
fi
if run_codex_with_retries docker run --rm \
    --user "$(id -u):$(id -g)" \
    --read-only \
    --tmpfs "/tmp:rw,size=$ECAA_DOCKER_TMPFS_TMP_SIZE,mode=1777" \
    --tmpfs "/var/tmp:rw,size=$ECAA_DOCKER_TMPFS_VARTMP_SIZE,mode=1777" \
    --security-opt no-new-privileges \
    --cap-drop=ALL \
    --pids-limit "$ECAA_DOCKER_PIDS_LIMIT" \
    "${DOCKER_MEMORY_ARGS[@]}" \
    "${DOCKER_CPU_ARGS[@]}" \
    -v "$PACKAGE":"$PACKAGE":rw \
    -v "$AGENT_HOME_DIR":"$HOME":rw \
    -v "$ECAA_INSTALL_MOUNT_SRC":/usr/local/bin/ecaa-install:ro \
    -v "$LIT_FETCH_MOUNT_SRC":/opt/ecaa/agent_literature_fetch.py:ro \
    "${CODEX_BIN_ARGS[@]}" \
    "${CODEX_AUTH_ARGS[@]}" \
    "${CODEX_BUDGET_ENV_ARGS[@]}" \
    "${SCRATCH_ARGS[@]}" \
    -w "$PACKAGE" \
    -e "HOME=$HOME" \
    -e "PATH=${CODEX_PATH_PREFIX}/opt/conda/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
    -e "ECAA_TASK_ID=${ECAA_TASK_ID:-}" \
    -e "ECAA_PACKAGE_ROOT=${ECAA_PACKAGE_ROOT:-$PACKAGE}" \
    "$CONTAINER_IMAGE" \
    codex exec --yolo --skip-git-repo-check "${CODEX_MODEL_ARGS[@]}" "$PROMPT"; then
  CODEX_EXIT=0
else
  CODEX_EXIT=$?
fi
set -e

# Render-as-Contract: after a SUCCESSFUL compute (CODEX_EXIT=0), run the FIXED,
# non-LLM figure render over the compute-output tables the agent just wrote.
# Skipped after a failed compute. Best-effort: never fails the task (the harness
# figure validator is the gate). Mirrors the agent-claude.sh container path —
# a second minimal docker run reuses the compute image + the same package
# bind-mount + the same --user.
if [ "$CODEX_EXIT" -eq 0 ] && [ -n "${ECAA_TASK_ID:-}" ] \
   && [ -n "$CONTAINER_IMAGE" ] && command -v docker >/dev/null 2>&1; then
  render_required_figures "$PACKAGE" "$ECAA_TASK_ID" "container" \
    "$CONTAINER_IMAGE" "$(id -u):$(id -g)" "$ECAA_DOCKER_TMPFS_TMP_SIZE"
fi

# The harness reconciles outcome from result.json / state.patch.json that the
# agent wrote; the exit code is recorded but not authoritative. Surface a
# non-zero codex exit for the harness's stderr_tail diagnostics.
if [ "$CODEX_EXIT" -ne 0 ]; then
    echo "agent-codex.sh: codex exec exited $CODEX_EXIT (task ${ECAA_TASK_ID:-<none>})" >&2
fi
exit "$CODEX_EXIT"
