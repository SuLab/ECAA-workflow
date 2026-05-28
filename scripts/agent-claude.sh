#!/usr/bin/env bash
# agent-claude.sh — Invoke Claude Code as a harness execution agent.
# Called by scripps-workflow-harness as: agent-claude.sh <package_dir>
#
# Each invocation picks up ONE ready task, executes it, and writes
# its state transition to runtime/outputs/<task_id>/state.patch.json.
# The harness merges patches into WORKFLOW.json at iteration end.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "$0")" && pwd)"
# Shared defaults. Each value is `: "${VAR:=default}"`, so any env override
# in the calling shell wins.
source "$SCRIPT_DIR/agent-claude-common.sh"

# Security remediation validate
# ECAA_CHAT_SESSION_ID is a syntactically-correct UUID before any code
# interpolates it into a docker label, cache path, or per-session log
# location. `validate_uuid` is defined in agent-claude-common.sh.
# Empty session id is tolerated; the cache + label code paths gate on
# `-n` already. Only validate when set.
if [ -n "${ECAA_CHAT_SESSION_ID:-}" ]; then
    validate_uuid "$ECAA_CHAT_SESSION_ID"
fi

# Validate ECAA_TASK_ID before it lands in any per-task path or label.
# Empty value is tolerated by the path-composition code below (the
# harness pre-flights this in `run_iteration`), but any non-empty value
# MUST satisfy the safe-id shape.
if [ -n "${ECAA_TASK_ID:-}" ]; then
    validate_task_id "$ECAA_TASK_ID"
fi

PACKAGE="$(realpath "$1")"

# Agent debug tracing: ECAA_AGENT_DEBUG=1 redirects all
# stderr to runtime/outputs/<task_id>/agent-trace.log AND turns on
# bash xtrace, so a silent `set -e` exit between BLAS bootstrap and
# docker run leaves a forensic trail. Off by default; tracing adds noise
# to the harness's stderr_tail otherwise.
if [ "${ECAA_AGENT_DEBUG:-0}" = "1" ] && [ -n "${ECAA_TASK_ID:-}" ]; then
  __AGENT_TRACE_DIR="$PACKAGE/runtime/outputs/$ECAA_TASK_ID"
  mkdir -p "$__AGENT_TRACE_DIR" 2>/dev/null || true
  export PS4='+ [${BASH_SOURCE##*/}:${LINENO}] '
  exec 2> >(tee -a "$__AGENT_TRACE_DIR/agent-trace.log" >&2)
  AGENT_TRACE_LOG="$__AGENT_TRACE_DIR/agent-trace.log"
  set -x
fi

# `run_no_xtrace` (01, C-12) is defined in agent-claude-common.sh.

# BLAS / OpenMP / numerical-library thread-budget exports + R BLAS
# probe. Sourced (not exec'd) so the env-var exports + LD_PRELOAD
# adjustments persist into this shell. See the bootstrap script for the
# full rationale; the short version: the harness sets bare BLAS env
# vars in the envelope, but `Sys.setenv()` inside R is too late and
# stock Debian/Ubuntu R links against single-threaded libRblas.so so
# even correct env vars are inert without an LD_PRELOAD interpose.
__BOOTSTRAP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
. "$__BOOTSTRAP_DIR/_agent-blas-bootstrap.sh"

# Memory-discipline policy surface. When the emitter wrote
# `policies/memory-discipline.json`, append a short block to the
# prompt so the agent consults BPCells / on-disk libraries before
# materializing a large dense matrix. Best-effort — silently skipped
# when jq is unavailable or the policy file is absent, which preserves
# byte-identical output for packages that don't carry the policy.
MEMORY_DISCIPLINE_BLOCK=""
if [ -f "$PACKAGE/policies/memory-discipline.json" ] && command -v jq >/dev/null 2>&1; then
  MAX_GB="$(jq -r '.max_dense_matrix_gb // empty' "$PACKAGE/policies/memory-discipline.json" 2>/dev/null || true)"
  COHORT_K="$(jq -r '.large_cohort_cell_threshold_k // empty' "$PACKAGE/policies/memory-discipline.json" 2>/dev/null || true)"
  if [ -n "$MAX_GB" ] && [ -n "$COHORT_K" ]; then
    MEMORY_DISCIPLINE_BLOCK="

## Memory discipline (REQUIRED when stage handles > ${COHORT_K}k cells or artifacts > ${MAX_GB} GB dense)
Read policies/memory-discipline.json at the start of the task. If your stage touches a cohort above the cell-count threshold OR would materialize a dense matrix above the GB threshold, use an on-disk library from the policy's on_disk_library_hints (BPCells / DelayedArray / HDF5Array for R; anndata backed='r' / zarr / h5py for Python) instead of a dense in-memory matrix. For Seurat v5 scRNA-seq, that means BPCells::write_matrix_dir + open_matrix_dir as the assay backing for SCTransform v2. If the stage would normally end with a global merge (e.g. merging every compartment into an 'all_cells' object), verify the merge is actually consumed downstream before materializing it — redundant global merges have caused production OOMs in prior runs."
  fi
fi

# Shared task-execution body: patch-merge envelope, blocker_kind
# vocabulary, discovery-stage block-by-default rule, iterate-until /
# figures / progress contracts. Single source of truth at
# scripts/agent-prompts/task-execution.md so the local / aws / slurm
# wrappers cannot drift on the WAL-guarded state.patch.json contract.
TASK_EXECUTION_BODY="$(load_task_execution_prompt "$SCRIPT_DIR/agent-prompts/task-execution.md")"

PROMPT="$(cat "$PACKAGE/PROMPT.md")${MEMORY_DISCIPLINE_BLOCK}

## Package location
All paths are relative to: $PACKAGE

${TASK_EXECUTION_BODY}"

# Bundle D — harness agent cost ingestion.
#
# Run Claude Code CLI with --output-format=json so the final line of
# stdout is a structured blob containing token usage + cost. Tee stdout
# so the SME still sees the live progress, then parse the last JSON
# line and write `runtime/outputs/<task_id>/agent-usage.json`. The
# harness reads that file when the agent exits and forwards the usage
# to the server's session-metrics path.
#
# When `jq` is unavailable or the CLI output isn't JSON-parseable, the
# parse step is a silent no-op — agent instrumentation is best-effort.
# The harness handles a missing sidecar gracefully.

# Persist the agent's stdout/stderr alongside the per-task outputs so
# a failed dispatch (NonZeroExit before the script reaches the
# CLAUDE_EXIT capture) leaves a forensic trail. Otherwise a
# `set -euo pipefail` crash inside the docker pipeline would surface
# only as exit 1 with empty stderr_tail. The fallback `mktemp` path
# (no ECAA_TASK_ID) preserves standalone invocations.
if [ -n "${ECAA_TASK_ID:-}" ]; then
  OUT_LOG_DIR="$PACKAGE/runtime/outputs/$ECAA_TASK_ID"
  mkdir -p "$OUT_LOG_DIR" 2>/dev/null || true
  OUT_LOG="$OUT_LOG_DIR/agent-claude.log"
  OUT_LOG_PERSISTENT=1
else
  OUT_LOG="$(mktemp -t agent-claude.XXXXXX.log)"
  OUT_LOG_PERSISTENT=0
fi

# Default 200 MB cap. A runaway claude
# CLI (e.g. infinite tool-loop trace dump) used to fill the agent's
# scratch disk and blow up the harness when the package crate ran out
# of room for runtime/outputs. The cap is a per-log soft limit; when
# the file size crosses it during the run, a background watcher
# rotates the file in place (mv → log.old; truncate the live log).
ECAA_AGENT_LOG_MAX_BYTES="${ECAA_AGENT_LOG_MAX_BYTES:-209715200}"

# Per-task turn budget for the executor agent. On overshoot the agent
# is expected to write status=blocked to result.json; this wrapper
# also enforces a hard cap by post-checking num_turns from agent-usage.json.
MAX_TURNS_PER_TASK="${MAX_TURNS_PER_TASK:-40}"

LOG_WATCHER_PID=""
if [ -n "${ECAA_TASK_ID:-}" ] && [ "$ECAA_AGENT_LOG_MAX_BYTES" -gt 0 ]; then
  ( while :; do
      sleep 30
      if [ -f "$OUT_LOG" ]; then
        SIZE=$(stat -c%s "$OUT_LOG" 2>/dev/null || echo 0)
        if [ "$SIZE" -gt "$ECAA_AGENT_LOG_MAX_BYTES" ]; then
          mv "$OUT_LOG" "${OUT_LOG}.rotated" 2>/dev/null || true
          : > "$OUT_LOG"
          echo "[agent-log] rotated $OUT_LOG at $SIZE bytes (cap $ECAA_AGENT_LOG_MAX_BYTES)" >&2
        fi
      fi
    done ) &
  LOG_WATCHER_PID=$!
fi

# Heartbeat touch loop. The harness seeds the file at pre-mark time;
# this loop keeps it fresh while the agent is genuinely running so a
# hang at a single syscall is distinguishable from a legitimately
# long-running task. ECAA_TASK_ID is set by the harness hardware
# envelope; absent = skip the loop.
HEARTBEAT_PID=""
if [ -n "${ECAA_TASK_ID:-}" ]; then
  HEARTBEAT_FILE="$PACKAGE/runtime/outputs/$ECAA_TASK_ID/.heartbeat"
  mkdir -p "$(dirname "$HEARTBEAT_FILE")" 2>/dev/null || true
  ( while :; do
      date -u +%Y-%m-%dT%H:%M:%SZ > "$HEARTBEAT_FILE" 2>/dev/null || exit 0
      sleep "$ECAA_HEARTBEAT_INTERVAL_SECS"
    done ) &
  HEARTBEAT_PID=$!
fi

# Forward-declared so cleanup_heartbeat can reap it. The loop itself is
# started later, after $AGENT_CLAUDE_DIR is resolved.
CRED_REFRESH_PID=""
DOCKER_SECRET_ENV_FILE=""

cleanup_heartbeat() {
  if [ -n "$HEARTBEAT_PID" ]; then
    kill "$HEARTBEAT_PID" 2>/dev/null || true
    wait "$HEARTBEAT_PID" 2>/dev/null || true
  fi
  if [ -n "$CRED_REFRESH_PID" ]; then
    kill "$CRED_REFRESH_PID" 2>/dev/null || true
    wait "$CRED_REFRESH_PID" 2>/dev/null || true
  fi
  if [ -n "$LOG_WATCHER_PID" ]; then
    kill "$LOG_WATCHER_PID" 2>/dev/null || true
    wait "$LOG_WATCHER_PID" 2>/dev/null || true
  fi
  if [ "${OUT_LOG_PERSISTENT:-0}" != "1" ]; then
    rm -f "$OUT_LOG"
  fi
  if [ -n "${DOCKER_SECRET_ENV_FILE:-}" ]; then
    rm -f "$DOCKER_SECRET_ENV_FILE" 2>/dev/null || true
  fi
}
trap cleanup_heartbeat EXIT

# Per-task scratch mount. Per-task ephemeral working
# area distinct from the package's persistent outputs dir. Default
# location: `$PACKAGE/runtime/scratch/<task_id>/`. Lifecycle: created
# here at agent-launch time; harness cleans up on task transition to
# Completed/Failed (retained on Blocked for forensic per S15.7).
# Override via `ECAA_AGENT_SCRATCH_DIR` for site-specific layouts.
# Surfaced to the agent as `ECAA_TASK_SCRATCH_DIR` so tool scripts can
# write large intermediates without polluting the package's outputs.
if [ -n "${ECAA_TASK_ID:-}" ]; then
  SCRATCH_BASE="${ECAA_AGENT_SCRATCH_DIR:-$PACKAGE/runtime/scratch}"
  SCRATCH_DIR="$SCRATCH_BASE/$ECAA_TASK_ID"
  mkdir -p "$SCRATCH_DIR" 2>/dev/null || true
  export ECAA_TASK_SCRATCH_DIR="$SCRATCH_DIR"
fi

# Per-session cache mount. Persists pip / conda / apt /
# R-libs across task invocations within a session so the second task
# in a session doesn't re-download the same wheels and conda packages.
# Per-session subdir prevents cross-session cache poisoning (D-R13);
# the per-session dir is pruned at session-store TTL (existing 30-day
# `persistence.rs::load_then_prune`). Default location:
# `~/.scripps-workflow/agent-cache/<session_id>/{pip,conda,apt,R-libs}`.
# `ECAA_AGENT_CACHE_DIR` overrides the parent; `ECAA_AGENT_CACHE_DISABLE=1`
# opts out entirely.
if [ "${ECAA_AGENT_CACHE_DISABLE:-0}" != "1" ] && [ -n "${ECAA_CHAT_SESSION_ID:-}" ]; then
  CACHE_BASE="${ECAA_AGENT_CACHE_DIR:-$HOME/.scripps-workflow/agent-cache}"
  # `ECAA_AGENT_CACHE_GLOBAL=1` opts into a cross-session-shared cache.
  # Default behavior (off) is per-session isolation to prevent
  # cross-session cache poisoning (D-R13). The global mode trades
  # isolation for install-cost amortization: a heavy R package like
  # DESeq2 (15-30 min Bioconductor compile from source) installs once
  # and is reused by every later session that picks the same method.
  # Designed for testing campaigns + dev environments; production
  # deployments should leave it off. The per-package R/Python install
  # is atomic via tmp+rename so two concurrent agents installing the
  # same package don't corrupt each other, but a flock guards the
  # CACHE_DIR root itself to serialize the first-touch directory
  # bootstrap.
  if [ "${ECAA_AGENT_CACHE_GLOBAL:-0}" = "1" ]; then
    CACHE_DIR="$CACHE_BASE/global"
  else
    CACHE_DIR="$CACHE_BASE/$ECAA_CHAT_SESSION_ID"
  fi
  mkdir -p "$CACHE_DIR/pip" "$CACHE_DIR/conda" "$CACHE_DIR/apt" "$CACHE_DIR/R-libs" 2>/dev/null || true
  export ECAA_SESSION_CACHE_DIR="$CACHE_DIR"
  # Surface the standard pip / conda / R env vars so tool scripts pick
  # them up without per-tool override. Apt cache is bind-mounted in
  # container mode only (host mode lacks rights to redirect apt).
  export PIP_CACHE_DIR="$CACHE_DIR/pip"
  export CONDA_PKGS_DIRS="$CACHE_DIR/conda"
  export R_LIBS_USER="$CACHE_DIR/R-libs"
fi

# Per-session @anthropic-ai/claude-code install. The container image's
# baked-in claude CLI lags Anthropic's deployed minor version (we've
# seen 2.1.131 in image vs 2.1.132 on host); older minors get silently
# 429'd at /v1/messages with no stderr, looking exactly like a
# credential or rate-limit problem. Install the latest into the
# per-session cache (one ~30s npm install per session, then reused),
# bind-mount it into the container, and prepend its bin dir to the
# container's PATH so `claude` resolves to the upgraded copy. Pin
# via ECAA_AGENT_CLAUDE_VERSION (default `latest`); set
# ECAA_AGENT_CLAUDE_FORCE_REINSTALL=1 to refresh between tasks.
# Falls back to the image's bundled CLI when npm is unavailable on
# the host or when ECAA_AGENT_CLAUDE_DISABLE=1.
CLAUDE_CODE_INSTALL_DIR=""
CLAUDE_CODE_INSTALLED_VERSION=""
if [ "${ECAA_AGENT_CLAUDE_DISABLE:-0}" != "1" ]; then
  if [ -n "${ECAA_SESSION_CACHE_DIR:-}" ]; then
    CLAUDE_CODE_INSTALL_DIR="$ECAA_SESSION_CACHE_DIR/claude-code"
  else
    CLAUDE_CODE_FALLBACK_BASE="${ECAA_AGENT_CACHE_DIR:-$HOME/.scripps-workflow/agent-cache}"
    CLAUDE_CODE_INSTALL_DIR="$CLAUDE_CODE_FALLBACK_BASE/standalone-$(basename "$PACKAGE")/claude-code"
  fi
  CLAUDE_CODE_PKG_JSON="$CLAUDE_CODE_INSTALL_DIR/node_modules/@anthropic-ai/claude-code/package.json"
  CLAUDE_CODE_VERSION="${ECAA_AGENT_CLAUDE_VERSION:-latest}"
  if [ -f "$CLAUDE_CODE_PKG_JSON" ] && [ "${ECAA_AGENT_CLAUDE_FORCE_REINSTALL:-0}" != "1" ]; then
    CLAUDE_CODE_INSTALLED_VERSION="$(jq -r .version "$CLAUDE_CODE_PKG_JSON" 2>/dev/null || echo "")"
  fi
  if [ -z "$CLAUDE_CODE_INSTALLED_VERSION" ]; then
    if command -v npm >/dev/null 2>&1; then
      mkdir -p "$CLAUDE_CODE_INSTALL_DIR" 2>/dev/null || true
      echo "agent-claude.sh: installing @anthropic-ai/claude-code@$CLAUDE_CODE_VERSION into $CLAUDE_CODE_INSTALL_DIR (one-time, ~30s)..." >&2
      npm install --prefix "$CLAUDE_CODE_INSTALL_DIR" --silent --no-audit --no-fund "@anthropic-ai/claude-code@$CLAUDE_CODE_VERSION" >/dev/null 2>&1 || true
      CLAUDE_CODE_INSTALLED_VERSION="$(jq -r .version "$CLAUDE_CODE_PKG_JSON" 2>/dev/null || echo "")"
      if [ -n "$CLAUDE_CODE_INSTALLED_VERSION" ]; then
        echo "agent-claude.sh: installed claude-code $CLAUDE_CODE_INSTALLED_VERSION." >&2
      else
        echo "agent-claude.sh: claude-code install failed; falling back to container's bundled CLI." >&2
        CLAUDE_CODE_INSTALL_DIR=""
      fi
    else
      echo "agent-claude.sh: npm not on PATH; falling back to container's bundled claude-code." >&2
      CLAUDE_CODE_INSTALL_DIR=""
    fi
  fi
fi

# Policy open-log. If the package contains a `policies/` dir,
# fingerprint which policy files are referenced by the
# agent's tool execution. The tail of OUT_LOG is scanned post-run for
# any `policies/<name>.json` strings; matches are appended to
# `$PACKAGE/runtime/policy-opens.jsonl` so the orphan-policy sweep can
# disposition them. Best-effort; missing `jq` / grep simply skips.
log_policy_opens() {
  local pol_dir="$PACKAGE/policies"
  [ -d "$pol_dir" ] || return 0
  local out_log="$1"
  [ -f "$out_log" ] || return 0
  local ts
  ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  mkdir -p "$PACKAGE/runtime"
  local sink="$PACKAGE/runtime/policy-opens.jsonl"
  for pol in "$pol_dir"/*.json; do
    [ -e "$pol" ] || continue
    local name
    name="$(basename "$pol")"
    if grep -qF "policies/$name" "$out_log" 2>/dev/null; then
      echo "{\"ts\":\"$ts\",\"policy\":\"$name\"}" >> "$sink"
    fi
  done
}

# Per-task container resolution. WORKFLOW.json is the
# reproducibility-bearing source of truth: every Task carries an
# Optional `container: { image, tag, digest,... }` populated by the
# composer at emit time (S15.2). Per-task lookup wins; falls back to
# the package-level `policies/container.json::image` when unset
# (legacy path); falls through to host-env when both are absent.
# Requires `docker` (bio + clinical-trial containers ship multi-arch
# images). SLURM sites can set
# ECAA_SLURM_CONTAINER_RUNTIME=singularity|apptainer|podman at the
# sbatch-prologue layer; for this harness entry the host path uses
# docker.
CONTAINER_IMAGE=""

# Per-task lookup against WORKFLOW.json::tasks.<task_id>.container.
# Combines image + tag (falling back to digest when present, since a
# digest pin is strictly more reproducible than a tag).
WORKFLOW_JSON="$PACKAGE/WORKFLOW.json"
if [ -n "${ECAA_TASK_ID:-}" ] \
   && [ -f "$WORKFLOW_JSON" ] \
   && command -v jq >/dev/null 2>&1; then
  TASK_CONTAINER="$(jq -r --arg tid "$ECAA_TASK_ID" \
    '.tasks[$tid].container // empty | tojson' "$WORKFLOW_JSON" 2>/dev/null || true)"
  if [ -n "$TASK_CONTAINER" ] && [ "$TASK_CONTAINER" != "null" ]; then
    TC_IMAGE="$(printf '%s' "$TASK_CONTAINER" | jq -r '.image // empty')"
    TC_TAG="$(printf '%s' "$TASK_CONTAINER" | jq -r '.tag // empty')"
    TC_DIGEST="$(printf '%s' "$TASK_CONTAINER" | jq -r '.digest // empty')"
    # `gpu_required` flips on the GPU passthrough flag
    # for this task's docker / apptainer invocation. Stored as
    # "true"/"false" so downstream conditionals stay shell-friendly.
    TC_GPU_REQUIRED="$(printf '%s' "$TASK_CONTAINER" | jq -r '.gpu_required // false')"
    if [ -n "$TC_IMAGE" ]; then
      if [ -n "$TC_DIGEST" ]; then
        CONTAINER_IMAGE="${TC_IMAGE}@${TC_DIGEST}"
      elif [ -n "$TC_TAG" ]; then
        CONTAINER_IMAGE="${TC_IMAGE}:${TC_TAG}"
      else
        CONTAINER_IMAGE="$TC_IMAGE"
      fi
    fi
    # Exposed for the post-task SBOM emit step. The
    # `oci:<image>@<digest>` form is what `syft scan` accepts; the
    # downstream SBOM step assembles it from these two pieces.
    export TASK_CONTAINER_IMAGE="$TC_IMAGE"
    export TASK_CONTAINER_DIGEST="$TC_DIGEST"
    export TASK_CONTAINER_GPU_REQUIRED="$TC_GPU_REQUIRED"
  fi

  # Per-task GPU target (count + kind +
  # mig_profile). When the atom's resource_profile or the executor
  # overrides declared a GPU, the harness writes it onto the task
  # spec; we read it here so docker `--gpus device=…` and apptainer
  # `--nv` get the right device id. `mig_profile` selects a slice
  # name like `3g.40gb` (S5.49) for an H100; falls back to
  # `--gpus all` when unset.
  TASK_GPU_KIND="$(jq -r --arg tid "$ECAA_TASK_ID" \
    '.tasks[$tid].resource_profile.gpu.kind // empty' "$WORKFLOW_JSON" 2>/dev/null || true)"
  TASK_GPU_COUNT="$(jq -r --arg tid "$ECAA_TASK_ID" \
    '.tasks[$tid].resource_profile.gpu.count // 0' "$WORKFLOW_JSON" 2>/dev/null || true)"
  TASK_GPU_MIG_PROFILE="$(jq -r --arg tid "$ECAA_TASK_ID" \
    '.tasks[$tid].resource_profile.gpu.mig_profile // empty' "$WORKFLOW_JSON" 2>/dev/null || true)"
  export TASK_GPU_KIND
  export TASK_GPU_COUNT
  export TASK_GPU_MIG_PROFILE
fi

# Legacy fallback when per-task lookup didn't yield an image. Pre-S15.2
# packages have no container field on tasks; agent honors the package
# default. The fallback is preserved for compatibility.
if [ -z "$CONTAINER_IMAGE" ]; then
  CONTAINER_POLICY="$PACKAGE/policies/container.json"
  if [ -f "$CONTAINER_POLICY" ] && command -v jq >/dev/null 2>&1; then
    CONTAINER_IMAGE="$(jq -r '.image // empty' "$CONTAINER_POLICY" 2>/dev/null || true)"
  fi
fi

# Operator-level default. When per-task lookup AND the package policy
# both came back empty, fall back to ECAA_DEFAULT_CONTAINER_IMAGE if
# The operator pinned one in their.env. This makes container mode the
# default for hosts that have a baseline agent image installed, without
# requiring every taxonomy to declare `preferred_container`. Per-task
# and package-level pins still win — this is the lowest-priority source.
if [ -z "$CONTAINER_IMAGE" ] && [ -n "${ECAA_DEFAULT_CONTAINER_IMAGE:-}" ]; then
  CONTAINER_IMAGE="$ECAA_DEFAULT_CONTAINER_IMAGE"
fi

# Agent billing mode: subscription (default) vs api.
#
# SUBSCRIPTION: the machine has a logged-in `~/.claude/.credentials.json`
# (Max/Pro plan). The CLI falls back to subscription auth and bills
# against the plan's quotas instead of the API. Per-run cost: $0
# (subscription-flat). Default.
#
# NOTE: the chat-side API key lives at ECAA_ANTHROPIC_API_KEY — the
# SWFC-prefixed name doesn't collide with Claude Code's ANTHROPIC_API_KEY
# scan, so no subprocess env surgery is needed for the modern path. The
# Unset below handles legacy.env files that still set ANTHROPIC_API_KEY
# as the chat-side key (pre-rename).
#
# API mode: set ECAA_AGENT_BILLING=api to bill the API per-token. In
# api mode we forward the available key (prefer ECAA_ANTHROPIC_API_KEY,
# fall back to ANTHROPIC_API_KEY) into ANTHROPIC_API_KEY so the CLI
# picks it up.
if [ "${ECAA_AGENT_BILLING:-subscription}" = "subscription" ]; then
  unset ANTHROPIC_API_KEY
elif [ "${ECAA_AGENT_BILLING:-}" = "api" ]; then
  if [ -z "${ANTHROPIC_API_KEY:-}" ] && [ -n "${ECAA_ANTHROPIC_API_KEY:-}" ]; then
    # Suppress xtrace around the secret expansion
    # so the literal key never lands in agent-trace.log under
    # ECAA_AGENT_DEBUG=1.
    { set +x; } 2>/dev/null
    export ANTHROPIC_API_KEY="$ECAA_ANTHROPIC_API_KEY"
    { set -x; } 2>/dev/null
  fi
fi
# Chat-side SWFC var isn't read by Claude Code — leave it in the env.

# Regression assertion. If the xtrace suppression above regresses
# and the key lands in the trace log verbatim, fail fast rather
# than emit a package containing the literal key. Best-effort:
# only checks when ECAA_AGENT_DEBUG=1 produced a trace log AND the
# active key has the recognizable `sk-` prefix.
if [ "${ECAA_AGENT_DEBUG:-0}" = "1" ] \
   && [ -n "${AGENT_TRACE_LOG:-}" ] \
   && [ -f "${AGENT_TRACE_LOG}" ] \
   && grep -qE 'ANTHROPIC_API_KEY=sk-[A-Za-z0-9_-]{8}' "$AGENT_TRACE_LOG" 2>/dev/null; then
    echo "FATAL: agent-trace.log contains literal API key — aborting" >&2
    exit 99
fi

# Model tiering (§R-5 — on by default; opt out with ECAA_AGENT_MODEL_TIER=0).
# Routes the high-cognitive-load buckets to Opus and the deterministic /
# code-execution buckets to Sonnet:
#
#   Opus (--model claude-opus-4-7):
#     - discover_*  — method-selection over multiple candidate algorithms;
#                     the choice constrains every downstream task, so the
#                     stronger model picks better
#     - validate_*  — independent integrity check of the upstream task's
#                     outputs (must catch Sonnet's mistakes, so stays on
#                     a stronger model — this is the QC safety net)
#     - kind == discovery (legacy alias)
#
#   Sonnet (--model claude-sonnet-4-6):
#     - everything else — compute atoms that execute the chosen method
#       (DESeq2, sctransform, harmony, etc.) and produce structured
#       artifacts. These are well-bounded code-execution tasks where the
#       method is already locked in; Sonnet 4.6 handles them at parity
#       with Opus 4.7 in production runs while costing ~3× less.
#
# Quality safety nets that justify the tier split:
#   1. validate_<task> runs AFTER <task> on Opus, with its own integrity
#      check spec. If a Sonnet-run compute task emits malformed outputs,
#      the validate task blocks the DAG with a concrete reason — Sonnet
#      cannot silently corrupt the workflow.
#   2. claim_extractor + claim_verifier (when interpretation-policy
#      declares a verifiableEntities block) run on the narrative and
#      cross-check claims against the result tables. Any LLM-introduced
#      hallucination is surfaced as a ClaimVerificationReport block.
#   3. ECAA_AGENT_MODEL_TIER=0 reverts to all-Opus for users who want
#      maximum quality regardless of cost.
#
# Reads ECAA_TASK_ID directly (the harness sets it before each agent
# invocation) instead of peeking at WORKFLOW.json's first-ready task —
# the peek-vs-actual heuristic could pick the wrong task when multiple
# tasks are concurrently ready (e.g. validate_qc running parallel to
# normalisation; the peek picks normalisation → validate gets Sonnet
# instead of Opus). ECAA_TASK_ID is authoritative.
MODEL_FLAG_ARGS=()
# Per-task hard budget cap. claude CLI's `--max-budget-usd` (only
# works with --print mode, which we use via --output-format=json)
# terminates the session when the running cost crosses the limit.
# Hard cap at dispatch time, not soft post-hoc enforcement — the
# alternative only blocks the task after the agent has already
# spent the tokens.
#
# Calibrated against measured per-task-class averages:
#   validate_*   $1.90 avg (Opus) → $0.75 cap (Sonnet)
#   discover_*   $1.44 avg (Opus) → $1.50 cap (slight margin)
#   data_acq     $1.04 avg        → $1.20 cap
#   reporting    $0.95 avg        → $1.50 cap
#   analytical   $1.30 avg        → $1.75 cap
#
# Per-class envs override the defaults; ECAA_AGENT_BUDGET_USD
# overrides all classes. Set to 0 to disable the cap entirely.
BUDGET_FLAG_ARGS=()
if [ "${ECAA_AGENT_MODEL_TIER:-1}" = "1" ] && [ -n "${ECAA_TASK_ID:-}" ]; then
  case "$ECAA_TASK_ID" in
    validate_*)
      # validate_* tasks write a deterministic schema/checksum check
      # script. Sonnet handles this shape reliably and is 5x cheaper
      # than Opus. Tighter budget than discover_* because validators
      # have a stable codegen shape.
      MODEL_FLAG_ARGS+=(--model claude-sonnet-4-6)
      _BUDGET="${ECAA_AGENT_BUDGET_USD_VALIDATE:-0.75}"
      ;;
    discover_*)
      # discover_* tasks score methods via env-capability + spec
      # preferences. Opus's deeper analytical reasoning pays off
      # here when the candidate pool has subtle trade-offs.
      MODEL_FLAG_ARGS+=(--model claude-opus-4-7)
      _BUDGET="${ECAA_AGENT_BUDGET_USD_DISCOVER:-1.50}"
      ;;
    data_acquisition|data_import)
      MODEL_FLAG_ARGS+=(--model claude-sonnet-4-6)
      _BUDGET="${ECAA_AGENT_BUDGET_USD_DATA_ACQ:-1.20}"
      ;;
    *)
      # Pull the task kind only when needed — `kind: discovery` is a
      # legacy spelling used by a handful of archetype atoms before the
      # discover_/validate_ prefix convention was adopted. Cheap jq read
      # gated on jq being available and WORKFLOW.json being present.
      TID_KIND=""
      if command -v jq >/dev/null 2>&1 && [ -f "$PACKAGE/WORKFLOW.json" ]; then
        TID_KIND="$(jq -r --arg tid "$ECAA_TASK_ID" '
          .tasks[$tid].kind | if type == "object" then (keys[0]) else . end
        ' "$PACKAGE/WORKFLOW.json" 2>/dev/null)"
      fi
      if [ "$TID_KIND" = "discovery" ]; then
        MODEL_FLAG_ARGS+=(--model claude-opus-4-7)
        _BUDGET="${ECAA_AGENT_BUDGET_USD_DISCOVER:-1.50}"
      else
        MODEL_FLAG_ARGS+=(--model claude-sonnet-4-6)
        _BUDGET="${ECAA_AGENT_BUDGET_USD_ANALYTICAL:-1.75}"
      fi
      ;;
  esac
  # Global override beats per-class.
  _BUDGET="${ECAA_AGENT_BUDGET_USD:-$_BUDGET}"
  # `0` opts out; any positive number is the dollar ceiling. claude
  # CLI accepts fractional dollars (e.g. 0.75).
  if [ -n "$_BUDGET" ] && [ "$_BUDGET" != "0" ]; then
    BUDGET_FLAG_ARGS+=(--max-budget-usd "$_BUDGET")
  fi
fi

# Host-path memory cap. When ECAA_AGENT_MEMORY_CAP_GB is set, wrap the
# host-path CLI invocation in a systemd-run --user --scope so the
# claude subprocess tree runs in a dedicated cgroup with
# MemoryMax=<N>G. On memory breach the kernel reaps ONLY that cgroup —
# the harness + Playwright + dev servers on the same host stay alive,
# instead of the global OOM killer picking a random victim. For the
# docker path we hand the same cap to `docker run --memory=<N>g`.
# No-op when the var is unset (default).
#
# Pair MemoryMax with MemoryHigh = 0.85 * MemoryMax so
# the kernel throttles the cgroup BEFORE OOM-kill. The agent gets a
# soft pressure window to checkpoint its state instead of getting
# SIGKILL'd at the hard cap (PostgreSQL effective_cache_size
# pattern; Round-4 §10). Only applies on the systemd-run path
# (MemoryHigh is a cgroup-v2 knob); prlimit fallback retains hard
# OOM. Docker --memory-reservation= mirrors MemoryHigh for the
# docker path.
AGENT_CMD_PREFIX=()
DOCKER_MEMORY_ARGS=()
AGENT_MEMORY_LIMIT_GB=""
if [ -n "${ECAA_AGENT_MEMORY_CAP_GB:-}" ]; then
  if ! [[ "$ECAA_AGENT_MEMORY_CAP_GB" =~ ^[0-9]+$ ]]; then
    echo "agent-claude.sh: ECAA_AGENT_MEMORY_CAP_GB must be a positive integer (got '$ECAA_AGENT_MEMORY_CAP_GB'); ignoring." >&2
  elif command -v systemd-run >/dev/null 2>&1; then
    AGENT_MEMORY_LIMIT_GB="$ECAA_AGENT_MEMORY_CAP_GB"
    # --user requires an active user systemd instance; fall back to
    # prlimit below when that probe fails. The probe is cheap (~10 ms)
    # and catches WSL / rootless / minimal container hosts.
    if systemd-run --user --scope --quiet -p "MemoryMax=100M" /bin/true >/dev/null 2>&1; then
      # MemoryHigh = floor(0.85 * MemoryMax_MB).
      # Use integer arithmetic on MB so we don't get float
      # precision drift. e.g. 16 GiB → 13926 MiB.
      MEM_MAX_MB=$((ECAA_AGENT_MEMORY_CAP_GB * 1024))
      MEM_HIGH_MB=$((MEM_MAX_MB * ECAA_AGENT_MEMORY_HIGH_WATER_PCT / 100))
      AGENT_CMD_PREFIX=(systemd-run --user --scope --quiet \
        -p "MemoryMax=${ECAA_AGENT_MEMORY_CAP_GB}G" \
        -p "MemoryHigh=${MEM_HIGH_MB}M")
    elif command -v prlimit >/dev/null 2>&1; then
      CAP_BYTES=$((ECAA_AGENT_MEMORY_CAP_GB * 1024 * 1024 * 1024))
      AGENT_CMD_PREFIX=(prlimit "--as=$CAP_BYTES")
    fi
  elif command -v prlimit >/dev/null 2>&1; then
    AGENT_MEMORY_LIMIT_GB="$ECAA_AGENT_MEMORY_CAP_GB"
    CAP_BYTES=$((ECAA_AGENT_MEMORY_CAP_GB * 1024 * 1024 * 1024))
    AGENT_CMD_PREFIX=(prlimit "--as=$CAP_BYTES")
  else
    AGENT_MEMORY_LIMIT_GB="$ECAA_AGENT_MEMORY_CAP_GB"
  fi
elif [[ "${ECAA_HW_MEMORY_GB:-}" =~ ^[0-9]+$ ]] && [ "$ECAA_HW_MEMORY_GB" -gt 0 ]; then
  # Dynamic local sizing provides a per-agent memory slice as
  # ECAA_HW_MEMORY_GB. In container mode, make that an actual cgroup
  # limit; host mode remains advisory unless ECAA_AGENT_MEMORY_CAP_GB
  # is explicitly set above.
  AGENT_MEMORY_LIMIT_GB="$ECAA_HW_MEMORY_GB"
fi
if [ -n "$AGENT_MEMORY_LIMIT_GB" ]; then
  # Pair --memory with --memory-reservation (docker's
  # MemoryHigh equivalent) at 85% so OOM-kill is the absolute last
  # resort. apptainer doesn't expose --memory-reservation; the
  # systemd-run wrapper around the apptainer call handles that.
  DOCKER_MEMORY_RESERVATION_MB=$((AGENT_MEMORY_LIMIT_GB * 1024 * 85 / 100))
  DOCKER_MEMORY_ARGS=(
    "--memory=${AGENT_MEMORY_LIMIT_GB}g"
    "--memory-reservation=${DOCKER_MEMORY_RESERVATION_MB}m"
  )
fi

# Token-reduction tactic #3: load the per-package executor brief to
# pass via --append-system-prompt. Best-effort: if the file is absent
# (older package or non-emission test path), the flag is omitted so the
# agent falls back to ambient context.
EXECUTOR_BRIEF_ARGS=()
if [ -f "$PACKAGE/AGENT-EXECUTOR.md" ] && claude --help 2>&1 | grep -q "append-system-prompt"; then
  EXECUTOR_BRIEF_ARGS=(--append-system-prompt "@$PACKAGE/AGENT-EXECUTOR.md")
fi

# Capture the agent invocation start time in RFC 3339 format so the
# agent-code.json sidecar has an accurate started_at timestamp. Must be
# set BEFORE the docker / npx invocation so it reflects the real start.
AGENT_STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Run Claude Code while preserving wrapper cleanup on nonzero CLI exits.
# A transient API/socket failure can otherwise leave the task failed even
# though a retry would be safe and cheap compared with re-emitting the whole
# workflow. The retry is intentionally narrow: no state.patch.json written,
# terminal JSON classified as a transport error, and bounded attempts.
run_claude_with_retries() {
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

  : > "$OUT_LOG"
  while :; do
    if [ "$attempt" -gt 1 ] && [ -n "$task_dir" ]; then
      printf '[agent-retry] retrying transient Claude transport error (attempt %s/%s)\n' \
        "$attempt" "$max_attempts" >> "$task_dir/progress.log" 2>/dev/null || true
    fi

    set +e
    "$@" 2>&1 | tee -a "$OUT_LOG"
    exit_code="${PIPESTATUS[0]}"
    set -e

    if [ "$exit_code" = "0" ]; then
      if claude_terminal_result_transient_error "$OUT_LOG" \
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

    if claude_terminal_result_transient_error "$OUT_LOG" \
       && [ "$attempt" -lt "$max_attempts" ]; then
      if [ -n "$task_dir" ]; then
        printf '[agent-retry] transient Claude transport error after attempt %s/%s; retrying\n' \
          "$attempt" "$max_attempts" >> "$task_dir/progress.log" 2>/dev/null || true
      fi
      attempt=$((attempt + 1))
      sleep 5
      continue
    fi

    return "$exit_code"
  done
}

if [ -n "$CONTAINER_IMAGE" ] && command -v docker >/dev/null 2>&1; then
  # Container execution path. Container supplies the language runtime
  # + analysis tools; Claude Code CLI is still the executor
  # (`RUN npm install -g @anthropic-ai/claude-code` in the container's
  # Dockerfile). Mount the package read-write; mount ~/.claude
  # read-only so the CLI finds the subscription credentials.
  # ANTHROPIC_API_KEY is intentionally NOT forwarded in subscription
  # mode so the container's CLI picks subscription auth via the
  # mounted creds. In api mode we pass the key through.
  #
  # Critical: `docker run` does NOT inherit env from the parent shell.
  # Without explicit -e flags the BLAS thread vars + ECAA_HW_* envelope
  # are dropped at the container boundary, leaving Rscript inside the
  # container running single-threaded BLAS. The bootstrap built
  # _AGENT_ENV_FORWARD_PAIRS for exactly this; fan it into -e flags.
  # LD_PRELOAD is intentionally NOT forwarded — the host libopenblas.so
  # path won't exist inside the container, and a properly-built bio
  # container ships with a parallel BLAS by default.
  docker pull "$CONTAINER_IMAGE" >/dev/null 2>&1 || true
  __agent_build_env_forward_pairs
  DOCKER_ENV_ARGS=()
  for __agent_kv in "${_AGENT_ENV_FORWARD_PAIRS[@]}"; do
    DOCKER_ENV_ARGS+=(-e "$__agent_kv")
  done
  unset __agent_kv
  DOCKER_SECRET_ENV_ARGS=()
  if [ "${ECAA_AGENT_BILLING:-subscription}" = "api" ] \
     && [ -n "${ANTHROPIC_API_KEY:-}" ]; then
    # Suppress xtrace while writing the env-file so the literal key
    # never lands in agent-trace.log or the docker process argv.
    { set +x; } 2>/dev/null
    DOCKER_SECRET_ENV_FILE="$(mktemp -t agent-docker-env.XXXXXX)"
    chmod 600 "$DOCKER_SECRET_ENV_FILE" 2>/dev/null || true
    printf 'ANTHROPIC_API_KEY=%s\n' "$ANTHROPIC_API_KEY" > "$DOCKER_SECRET_ENV_FILE"
    DOCKER_SECRET_ENV_ARGS+=(--env-file "$DOCKER_SECRET_ENV_FILE")
    { set -x; } 2>/dev/null
  fi

  # GPU passthrough. When the per-task `gpu_required`
  # flag is on AND a non-zero GPU count is declared, add the docker
  # `--gpus` flag. MIG profile (S5.49: `3g.40gb` etc.) routes to
  # `--gpus device=<profile>`; otherwise `--gpus all` (the default
  # used by all single-GPU bio workloads).
  DOCKER_GPU_ARGS=()
  if [ "${TASK_CONTAINER_GPU_REQUIRED:-false}" = "true" ] \
     && [ "${TASK_GPU_COUNT:-0}" != "0" ]; then
    if [ -n "${TASK_GPU_MIG_PROFILE:-}" ]; then
      DOCKER_GPU_ARGS+=("--gpus" "device=${TASK_GPU_MIG_PROFILE}")
    else
      DOCKER_GPU_ARGS+=("--gpus" "all")
    fi
  fi

  DOCKER_CPU_ARGS=()
  __agent_container_cpus="${ECAA_HW_NPROC_HINT:-${ECAA_HW_VCPUS_AVAILABLE:-}}"
  if [[ "$__agent_container_cpus" =~ ^[0-9]+$ ]] && [ "$__agent_container_cpus" -gt 0 ]; then
    DOCKER_CPU_ARGS+=(--cpus "$__agent_container_cpus")
  fi
  unset __agent_container_cpus

  # Label the container with task + session ids so the
  # container-aware orphan reaper can probe `docker ps --filter
  # label=swfc-task=<id>` and reap a hung container without touching
  # the host. Empty values are tolerated (legacy package-only path).
  DOCKER_LABEL_ARGS=()
  if [ -n "${ECAA_TASK_ID:-}" ]; then
    DOCKER_LABEL_ARGS+=("--label" "swfc-task=${ECAA_TASK_ID}")
  fi
  if [ -n "${ECAA_CHAT_SESSION_ID:-}" ]; then
    DOCKER_LABEL_ARGS+=("--label" "swfc-session=${ECAA_CHAT_SESSION_ID}")
  fi

  # Drop to the host UID/GID inside the container so:
  # (1) `claude --dangerously-skip-permissions` doesn't trip the
  #  "running-as-root" guard the Claude CLI added in 2.1.x,
  # (2) files written into the bind-mounted package volume keep host
  #  ownership (no root-owned outputs the host can't clean up),
  # (3) the mounted ~/.claude/.credentials.json is readable as a
  #  normal user (root inside container would also read it via the
  #  RO bind, but the credentials lookup honors $HOME, not uid 0).
  DOCKER_USER_ARGS=(--user "$(id -u):$(id -g)")

  # Per directive language packages (R / Python / conda)
  # install at task time inside the container, not at image build
  # time. Mount the per-session cache dir so installs from one task
  # are visible to the next. R writes to `R_LIBS_USER`; Python `pip
  # install --user` writes under `PYTHONUSERBASE`; pip metadata cache
  # lives at `PIP_CACHE_DIR`. Python's site.py automatically adds
  # `$PYTHONUSERBASE/lib/pythonX.Y/site-packages` to sys.path.
  DOCKER_CACHE_ARGS=()
  if [ -n "${ECAA_SESSION_CACHE_DIR:-}" ]; then
    mkdir -p "$ECAA_SESSION_CACHE_DIR/python" 2>/dev/null || true
    DOCKER_CACHE_ARGS+=(
      -v "$ECAA_SESSION_CACHE_DIR":"$ECAA_SESSION_CACHE_DIR":rw
      -e "ECAA_SESSION_CACHE_DIR=$ECAA_SESSION_CACHE_DIR"
      -e "R_LIBS_USER=$ECAA_SESSION_CACHE_DIR/R-libs"
      -e "PIP_CACHE_DIR=$ECAA_SESSION_CACHE_DIR/pip"
      -e "CONDA_PKGS_DIRS=$ECAA_SESSION_CACHE_DIR/conda"
      -e "PYTHONUSERBASE=$ECAA_SESSION_CACHE_DIR/python"
      # Default every `pip install` to user-mode so packages land in
      # PYTHONUSERBASE (the mounted cache). Bypasses PEP 668's
      # externally-managed marker on Python 3.11+ Debian/Ubuntu bases
      # without the operator having to remember `--user`.
      -e "PIP_USER=1"
      -e "PIP_BREAK_SYSTEM_PACKAGES=1"
    )
  fi

  DOCKER_SCRATCH_ARGS=()
  if [ -n "${ECAA_TASK_SCRATCH_DIR:-}" ]; then
    mkdir -p "$ECAA_TASK_SCRATCH_DIR" 2>/dev/null || true
    case "$ECAA_TASK_SCRATCH_DIR" in
      "$PACKAGE"/*) ;;
      *) DOCKER_SCRATCH_ARGS+=(-v "$ECAA_TASK_SCRATCH_DIR":"$ECAA_TASK_SCRATCH_DIR":rw) ;;
    esac
    DOCKER_SCRATCH_ARGS+=(-e "ECAA_TASK_SCRATCH_DIR=$ECAA_TASK_SCRATCH_DIR")
  fi

  # Bind-mount SME-registered local_path inputs into the container so
  # the agent can read SME-cited files. runtime/inputs.json lists every
  # registered input with `kind=local_path` and `root_path=<absolute
  # host path>`. Without this loop the agent sees the manifest but the
  # cited files resolve to ENOENT inside the container, forcing the
  # task to block with `missing_input` even after the SME has
  # registered the directory. read-only since the agent must not
  # mutate SME-owned source data.
  DOCKER_INPUT_BIND_ARGS=()
  if [ -f "$PACKAGE/runtime/inputs.json" ] && command -v jq >/dev/null 2>&1; then
    while IFS= read -r __root_path; do
      [ -z "$__root_path" ] && continue
      [ ! -e "$__root_path" ] && continue
      case "$__root_path" in
        "$PACKAGE"/*) continue ;;  # already covered by the package bind
      esac
      DOCKER_INPUT_BIND_ARGS+=(-v "$__root_path":"$__root_path":ro)
    done < <(jq -r '.[]? | select(.kind == "local_path") | .root_path // empty' "$PACKAGE/runtime/inputs.json" 2>/dev/null)
  fi

  # Mount the per-session claude-code install (if successfully prepared
  # above) and prepend its bin dir to the container's PATH so `claude`
  # resolves to the upgraded copy instead of the image's bundled binary.
  # The.bin/claude symlink is relative, so the entire node_modules
  # tree must be mounted, not just the bin dir. Read-only is fine —
  # claude doesn't write into its own install tree at runtime.
  if [ -n "$CLAUDE_CODE_INSTALL_DIR" ] && [ -n "$CLAUDE_CODE_INSTALLED_VERSION" ]; then
    DOCKER_CACHE_ARGS+=(
      -v "$CLAUDE_CODE_INSTALL_DIR/node_modules":/opt/claude-code/node_modules:ro
      -e "PATH=/opt/claude-code/node_modules/.bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
    )
  fi

  # Per-session isolated agent HOME. claude needs RW access to BOTH
  # `$HOME/.claude/` (subscription credentials, history, file-history)
  # AND `$HOME/.claude.json` (the main config file claude bootstraps
  # from on startup). Mounting the host's `$HOME/.claude` directly:
  #  - races with the operator's own Claude Code session on the host
  #  (both stomp on.credentials.json refresh + history.jsonl), and
  #  - races between concurrent agent containers (ECAA_HARNESS_CONCURRENCY > 1).
  # Read-only mounts cause claude to silently hang because the CLI
  # cannot write its OAuth refresh token / history.
  # Solution: maintain a per-session copy of the host's claude state in
  # the per-session cache dir. First task in the session seeds it from
  # `$HOME`; later tasks reuse the same copy so claude's history and
  # token-refresh state persist across tasks within the session.
  AGENT_HOME_DIR=""
  AGENT_CLAUDE_DIR=""
  AGENT_CLAUDE_JSON=""
  if [ -n "${ECAA_SESSION_CACHE_DIR:-}" ]; then
    AGENT_HOME_DIR="$ECAA_SESSION_CACHE_DIR/agent-claude-home"
  else
    # Fallback for harness invocations without a session id (`make
    # ivd-execute` etc.). Use a per-package directory under the agent
    # cache so multiple ad-hoc runs don't clobber each other.
    AGENT_FALLBACK_BASE="${ECAA_AGENT_CACHE_DIR:-$HOME/.scripps-workflow/agent-cache}"
    AGENT_HOME_DIR="$AGENT_FALLBACK_BASE/standalone-$(basename "$PACKAGE")"
  fi
  AGENT_CLAUDE_DIR="$AGENT_HOME_DIR/.claude"
  AGENT_CLAUDE_JSON="$AGENT_HOME_DIR/.claude.json"
  mkdir -p "$AGENT_HOME_DIR" "$AGENT_CLAUDE_DIR" 2>/dev/null || true
  # Repair docker's bind-mount auto-create gotcha. When a `-v src:dst`
  # source path doesn't exist, the docker daemon (running as root)
  # creates it AS A ROOT-OWNED DIRECTORY before the container starts.
  # That's a one-way trip for `.claude.json`: the next agent invocation
  # sees a root-owned directory at the JSON path, the user-owned `cp`
  # below silently no-ops (the `2>/dev/null || true` swallows EISDIR /
  # EACCES), the bind-mount surfaces a directory at $HOME/.claude.json
  # inside the container, and claude dies silently the moment it tries
  # to read its config (EISDIR with no useful error, exit 1 in ~30s).
  # Same hazard for AGENT_CLAUDE_DIR if a prior root-owned mount-create
  # left it un-writable. Detect + repair by recreating both as the
  # current user before the conditional cp runs.
  # Security remediation removed the
  # `sudo rm -rf` fallback. A root-owned bind-mount leftover means
  # the operator must intervene manually (per
  # docs/remote-compute-operator-reference.md); silently elevating
  # to sudo from an agent script opens a privilege-escalation seam.
  if [ -e "$AGENT_CLAUDE_JSON" ] && [ ! -f "$AGENT_CLAUDE_JSON" ]; then
    if ! rm -rf "$AGENT_CLAUDE_JSON" 2>/dev/null; then
      echo "ERROR: cannot remove non-file $AGENT_CLAUDE_JSON (likely root-owned bind-mount leftover); run \`sudo rm -rf '$AGENT_CLAUDE_JSON'\` manually and retry" >&2
    fi
  fi
  if [ -d "$AGENT_CLAUDE_DIR" ] && [ ! -w "$AGENT_CLAUDE_DIR" ]; then
    if ! rm -rf "$AGENT_CLAUDE_DIR" 2>/dev/null; then
      echo "ERROR: cannot remove unwritable $AGENT_CLAUDE_DIR (likely root-owned bind-mount leftover); run \`sudo rm -rf '$AGENT_CLAUDE_DIR'\` manually and retry" >&2
    fi
    mkdir -p "$AGENT_CLAUDE_DIR" 2>/dev/null || true
  fi
  # Seed credentials + main config on first encounter so claude can
  # authenticate. The original logic was "cp -n on first encounter
  # only" so a per-session OAuth refresh wouldn't be clobbered by the
  # host's stale snapshot. That's the right rule when the host file
  # is older than the per-session copy. But when the operator switches
  # accounts (`claude /login` → new account → fresh OAuth token written
  # to ~/.claude/.credentials.json), the original rule pinned the
  # per-session copy to the OLD account's token forever and the agent
  # silently exits 1 against subscription. Fix: only skip the seed when
  # the per-session copy is FRESHER than the host copy. When the host
  # is newer (mtime comparison), reseed — the operator just changed
  # accounts and the per-session cache must follow.
  if [ -f "$HOME/.claude/.credentials.json" ]; then
    if [ ! -f "$AGENT_CLAUDE_DIR/.credentials.json" ] \
       || [ "$HOME/.claude/.credentials.json" -nt "$AGENT_CLAUDE_DIR/.credentials.json" ]; then
      cp "$HOME/.claude/.credentials.json" "$AGENT_CLAUDE_DIR/.credentials.json" 2>/dev/null || true
      chmod 600 "$AGENT_CLAUDE_DIR/.credentials.json" 2>/dev/null || true
    fi
  fi
  if [ ! -f "$AGENT_CLAUDE_JSON" ] \
     || { [ -f "$HOME/.claude.json" ] && [ "$HOME/.claude.json" -nt "$AGENT_CLAUDE_JSON" ]; }; then
    if [ -f "$HOME/.claude.json" ]; then
      cp "$HOME/.claude.json" "$AGENT_CLAUDE_JSON" 2>/dev/null || true
    else
      # Required for the bind-mount source to resolve as a file rather
      # than a directory. claude self-heals an empty config on launch.
      echo '{}' > "$AGENT_CLAUDE_JSON" 2>/dev/null || true
    fi
    chmod 600 "$AGENT_CLAUDE_JSON" 2>/dev/null || true
  fi

  # Background credential refresh loop. The initial seed above only
  # runs at task start; without this loop a long-running agent never
  # picks up an operator account switch (`claude /login`) until the
  # next dispatch. Polls every ECAA_AGENT_CRED_REFRESH_SECS seconds
  # (default 15) and re-copies host's.credentials.json into the
  # per-session mount when the host is newer (mtime-gated, so we
  # don't clobber claude's in-container OAuth refresh — the bind-
  # mounted file's mtime advances on every rotation, leaving host
  # -nt per-session false until the operator actually logs into a
  # different account). Set ECAA_AGENT_CRED_REFRESH_SECS=0 to disable
  # the loop entirely; values <5 or non-numeric fall back to default.
  __cred_refresh_secs="${ECAA_AGENT_CRED_REFRESH_SECS:-15}"
  if [[ "$__cred_refresh_secs" == "0" ]]; then
    :  # explicit opt-out — leave loop unstarted
  elif ! [[ "$__cred_refresh_secs" =~ ^[0-9]+$ ]] || [ "$__cred_refresh_secs" -lt 5 ]; then
    echo "agent-claude.sh: ECAA_AGENT_CRED_REFRESH_SECS='$__cred_refresh_secs' invalid (need 0 or integer >=5); using default 15" >&2
    __cred_refresh_secs=15
  fi
  if [ "$__cred_refresh_secs" != "0" ] && [ -f "$HOME/.claude/.credentials.json" ]; then
    # When two agents in the same session run
    # concurrently (ECAA_HARNESS_CONCURRENCY>1), their refresh loops
    # would race on the per-session credentials file. flock the loop
    # body so only one loop actively writes; the loser skips silently.
    __cred_refresh_lock="$AGENT_CLAUDE_DIR/.cred-refresh.lock"
    ( while :; do
        if [ "$HOME/.claude/.credentials.json" -nt "$AGENT_CLAUDE_DIR/.credentials.json" ]; then
          # Detect mid-refresh by claude inside the
          # container. If the per-session file's mtime advanced
          # within the last 2 seconds, that's claude itself
          # rotating its OAuth token — clobbering would invalidate
          # the in-flight rotation. Skip this cycle; we'll retry
          # next poll.
          __cred_now=$(date +%s 2>/dev/null || echo 0)
          __cred_target_mtime=$(stat -c %Y "$AGENT_CLAUDE_DIR/.credentials.json" 2>/dev/null || echo 0)
          if [ $((__cred_now - __cred_target_mtime)) -ge 2 ]; then
            ( flock -n 9 || exit 0
              cp "$HOME/.claude/.credentials.json" "$AGENT_CLAUDE_DIR/.credentials.json" 2>/dev/null || true
              chmod 600 "$AGENT_CLAUDE_DIR/.credentials.json" 2>/dev/null || true
            ) 9>"$__cred_refresh_lock"
          fi
        fi
        sleep "$__cred_refresh_secs"
      done ) &
    CRED_REFRESH_PID=$!
  fi

  # Docker isolation hardening. The agent writes outputs into
  # $PACKAGE and $AGENT_HOME_DIR (which are bound RW above);
  # everything else is read-only. Tmpfs covers /tmp and
  # /var/tmp so installers and toolchains can stage files. All caps
  # dropped — the agent doesn't need to chown, chmod, or bind low
  # ports; if a future workflow needs CHOWN/DAC_OVERRIDE/FOWNER, add
  # them here with --cap-add= and document the syscall trigger.
  # `--security-opt no-new-privileges` blocks suid-escalation inside
  # the container. `--pids-limit` fences fork-bombs.
  if run_claude_with_retries docker run --rm \
    --read-only \
    --tmpfs "/tmp:rw,size=$ECAA_DOCKER_TMPFS_TMP_SIZE,mode=1777" \
    --tmpfs "/var/tmp:rw,size=$ECAA_DOCKER_TMPFS_VARTMP_SIZE,mode=1777" \
    --security-opt no-new-privileges \
    --cap-drop=ALL \
    --pids-limit "$ECAA_DOCKER_PIDS_LIMIT" \
    "${DOCKER_USER_ARGS[@]}" \
    "${DOCKER_MEMORY_ARGS[@]}" \
    "${DOCKER_CPU_ARGS[@]}" \
    "${DOCKER_GPU_ARGS[@]}" \
    "${DOCKER_LABEL_ARGS[@]}" \
    -v "$PACKAGE":"$PACKAGE":rw \
    -v "$AGENT_HOME_DIR":"$HOME":rw \
    "${DOCKER_CACHE_ARGS[@]}" \
    "${DOCKER_SCRATCH_ARGS[@]}" \
    "${DOCKER_INPUT_BIND_ARGS[@]}" \
    -w "$PACKAGE" \
    -e "HOME=$HOME" \
    "${DOCKER_ENV_ARGS[@]}" \
    "${DOCKER_SECRET_ENV_ARGS[@]}" \
    "$CONTAINER_IMAGE" \
    claude --dangerously-skip-permissions --output-format=json "${MODEL_FLAG_ARGS[@]}" "${BUDGET_FLAG_ARGS[@]}" "${EXECUTOR_BRIEF_ARGS[@]}" -p "$PROMPT"; then
    CLAUDE_EXIT=0
  else
    CLAUDE_EXIT=$?
  fi

  # Write a.container-state.json sidecar so the
  # SLURM-side container-aware orphan reaper (heartbeat probe) and
  # the AWS-side equivalent can confirm container exit code without
  # re-running `docker inspect` on a possibly-already-reaped container.
  # Best-effort; missing ECAA_TASK_ID = legacy path with no per-task
  # output dir to write into.
  if [ -n "${ECAA_TASK_ID:-}" ]; then
    CONTAINER_STATE_DIR="$PACKAGE/runtime/outputs/$ECAA_TASK_ID"
    mkdir -p "$CONTAINER_STATE_DIR" 2>/dev/null || true
    cat > "$CONTAINER_STATE_DIR/.container-state.json" 2>/dev/null <<EOF || true
{
  "exit_code": $CLAUDE_EXIT,
  "image": "${CONTAINER_IMAGE:-}",
  "runtime": "docker",
  "session_id": "${ECAA_CHAT_SESSION_ID:-}",
  "task_id": "${ECAA_TASK_ID}",
  "ended_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
  fi
else
  # Host-env path (existing bio behavior; preserves byte-identical
  # output for sessions that don't declare a container).
  #
  # When ECAA_LOCAL_SANDBOX=bubblewrap, layer
  # bubblewrap on top of the cgroup wrapper so filesystem + network
  # containment apply without container overhead. Anthropic Claude
  # Code uses the same bwrap pattern internally (Round-4 §22.10);
  # we adopt it here for the host-mode agent. Default: off.
  #
  # The wrapper composes as
  #  AGENT_CMD_PREFIX (cgroup throttle)
  #  Bwrap … (filesystem/network sandbox)
  #  npx claude-code -p …
  # so MemoryHigh / MemoryMax still apply to the bwrap-spawned tree.
  #
  # bind list: read-only system paths (so the agent can read libs +
  # binaries) + RW the package working dir + a tmpfs scratch for
  # /tmp + procfs + devfs + share ~/.claude RO so the subscription
  # creds resolve. --unshare-net is gated on the ECAA_CONTAINER_NETWORK_DEFAULT
  # env: when set to `none` we drop networking; otherwise the agent
  # keeps the host bridge so it can reach Anthropic + GitHub.
  BWRAP_PREFIX=()
  if [ "${ECAA_LOCAL_SANDBOX:-off}" = "bubblewrap" ]; then
    if command -v bwrap >/dev/null 2>&1; then
      # Tighter bwrap profile:
      #  --unshare-pid     — fresh pid namespace (agent can't see host PIDs)
      #  --unshare-user    — separate user namespace (uid mapping); requires
      #                      userns.allow=1 in /proc/sys/user; warn on hosts
      #                      where it's disabled and fall back to no-sandbox.
      #  --unshare-ipc     — sysv IPC isolation
      #  --unshare-uts     — hostname/domainname isolation
      #  --unshare-cgroup  — fresh cgroup namespace
      #  --new-session     — detach from controlling tty (sigint-safe)
      #  --die-with-parent — child dies when bwrap parent dies (no zombies)
      #  --cap-drop ALL    — drop all caps (defense even if userns gave any)
      #
      # Bind narrowing prevents the agent from overwriting server-trusted
      # sidecars (decisions.jsonl, verifier-decisions.jsonl,
      # validation-reports/, cross-version-diff.json, atom-prereqs/) and
      # restricts /etc to resolver + TLS-cert dirs:
      #   $PACKAGE                          -> RO (was RW)
      #   $PACKAGE/runtime/outputs/$TASK_ID -> RW (only the agent's own
      #                                        task scratch)
      #   /etc                              -> TLS-cert + resolver only
      #
      # Without ECAA_TASK_ID set, fall back to the legacy package-wide
      # bind so standalone debug runs still work; harness-launched
      # agents always have it set.
      BWRAP_PREFIX=(
        bwrap
        --unshare-pid
        --unshare-user
        --unshare-ipc
        --unshare-uts
        --unshare-cgroup
        --new-session
        --die-with-parent
        --cap-drop ALL
        --ro-bind /usr /usr
        --ro-bind /lib /lib
        --ro-bind /lib64 /lib64
        --ro-bind /etc/ssl /etc/ssl
        --ro-bind /etc/resolv.conf /etc/resolv.conf
        --ro-bind "$HOME/.claude" "$HOME/.claude"
        --proc /proc
        --dev /dev
        --tmpfs /tmp
        --setenv HOME "$HOME"
        --chdir "$PACKAGE"
      )
      # ca-certificates lives under a different path on some distros;
      # bind whichever is present. /etc/pki is the Fedora/RHEL layout.
      if [ -d /etc/ca-certificates ]; then
        BWRAP_PREFIX+=(--ro-bind /etc/ca-certificates /etc/ca-certificates)
      fi
      if [ -d /etc/pki ]; then
        BWRAP_PREFIX+=(--ro-bind /etc/pki /etc/pki)
      fi
      # nsswitch.conf isn't present on all systems; bind only if it is
      # so DNS/host resolution honors host policy when the file exists.
      if [ -f /etc/nsswitch.conf ]; then
        BWRAP_PREFIX+=(--ro-bind /etc/nsswitch.conf /etc/nsswitch.conf)
      fi
      # Package narrowing: RO whole tree, RW only the per-task scratch.
      # Harness-launched agents always have ECAA_TASK_ID set; the
      # legacy fallback keeps standalone debug runs working.
      if [ -n "${ECAA_TASK_ID:-}" ]; then
        # mkdir -p was already issued earlier (OUT_LOG_DIR / HEARTBEAT)
        # but repeat here for safety -- bwrap binds error out on a
        # missing path.
        mkdir -p "$PACKAGE/runtime/outputs/$ECAA_TASK_ID" 2>/dev/null || true
        BWRAP_PREFIX+=(
          --ro-bind "$PACKAGE" "$PACKAGE"
          --bind "$PACKAGE/runtime/outputs/$ECAA_TASK_ID" "$PACKAGE/runtime/outputs/$ECAA_TASK_ID"
        )
      else
        # Legacy fallback for ECAA_TASK_ID-less invocations.
        BWRAP_PREFIX+=(--bind "$PACKAGE" "$PACKAGE")
      fi
      # Bind-mount the per-task scratch dir RW into
      # the sandbox so tools can write large intermediates outside
      # the tmpfs `/tmp`. With the C5 narrowing above, $PACKAGE is
      # bound RO whether the scratch lives inside or outside the
      # package root -- so we now always emit an explicit RW bind.
      if [ -n "${ECAA_TASK_SCRATCH_DIR:-}" ]; then
        mkdir -p "$ECAA_TASK_SCRATCH_DIR" 2>/dev/null || true
        BWRAP_PREFIX+=(--bind "$ECAA_TASK_SCRATCH_DIR" "$ECAA_TASK_SCRATCH_DIR")
        BWRAP_PREFIX+=(--setenv ECAA_TASK_SCRATCH_DIR "$ECAA_TASK_SCRATCH_DIR")
      fi
      # Bind-mount the per-session cache dir RW so
      # pip / conda / R-libs caches persist across task invocations
      # within the session. The env-var exports above are inherited
      # by the bwrap-spawned process tree; bwrap needs the explicit
      # --setenv for the path so PIP_CACHE_DIR / CONDA_PKGS_DIRS /
      # R_LIBS_USER resolve identically inside the sandbox.
      if [ -n "${ECAA_SESSION_CACHE_DIR:-}" ]; then
        BWRAP_PREFIX+=(
          --bind "$ECAA_SESSION_CACHE_DIR" "$ECAA_SESSION_CACHE_DIR"
          --setenv ECAA_SESSION_CACHE_DIR "$ECAA_SESSION_CACHE_DIR"
          --setenv PIP_CACHE_DIR "$ECAA_SESSION_CACHE_DIR/pip"
          --setenv CONDA_PKGS_DIRS "$ECAA_SESSION_CACHE_DIR/conda"
          --setenv R_LIBS_USER "$ECAA_SESSION_CACHE_DIR/R-libs"
        )
      fi
      if [ "${ECAA_CONTAINER_NETWORK_DEFAULT:-bridge}" = "none" ]; then
        BWRAP_PREFIX+=(--unshare-net)
      fi
    else
      echo "agent-claude.sh: ECAA_LOCAL_SANDBOX=bubblewrap requested but bwrap is not on PATH; falling back to unsandboxed host path." >&2
    fi
  fi
  if run_claude_with_retries "${AGENT_CMD_PREFIX[@]}" "${BWRAP_PREFIX[@]}" npx @anthropic-ai/claude-code \
    --dangerously-skip-permissions \
    --output-format=json \
    "${MODEL_FLAG_ARGS[@]}" \
    "${BUDGET_FLAG_ARGS[@]}" \
    "${EXECUTOR_BRIEF_ARGS[@]}" \
    -p "$PROMPT"; then
    CLAUDE_EXIT=0
  else
    CLAUDE_EXIT=$?
  fi
fi

# Find the task id the agent just completed/advanced. Harness launches set
# ECAA_TASK_ID, so prefer the exact task directory; the mtime fallback only
# exists for legacy standalone invocations without an envelope.
if [ -n "${ECAA_TASK_ID:-}" ]; then
  TASK_DIR="$PACKAGE/runtime/outputs/$ECAA_TASK_ID/"
else
  TASK_DIR="$(ls -dt "$PACKAGE/runtime/outputs/"*/ 2>/dev/null | head -1 || true)"
fi
if [ -n "$TASK_DIR" ] && command -v jq >/dev/null 2>&1; then
  # Extract usage from the last valid JSON line of stdout. Claude Code
  # CLI's --output-format=json emits a single terminal result object of
  # the shape:
  #  { "type": "result", "usage": {...aggregate tokens...},
  #  "total_cost_usd": <number>,
  #  "modelUsage": { "<model-id>[<ctx>]": { inputTokens, outputTokens,
  #  cacheReadInputTokens, cacheCreationInputTokens,
  #  CostUSD,... },... } }
  # The result object has NO top-level `.model` — the model identity
  # lives on each `modelUsage` key. The original implementation tried
  # `(.model // "claude-sonnet-4-6")`, which silently wrote the literal
  # Sonnet 4.6 string into every sidecar regardless of what actually
  # ran. That misprices Opus 4.7 runs at Sonnet rates downstream.
  #
  # New extraction: pick the model entry with the highest costUSD as
  # the primary (drops tiny internal Haiku side-calls the CLI makes),
  # strip the `[1m]`/`[200k]` context-variant suffix so the Rust
  # `resolve_model_api_id` matches cleanly, and emit per-primary-model
  # tokens. If `modelUsage` is missing (older CLI version), the sidecar
  # is intentionally not written — the harness treats an absent file
  # the same as "no instrumentation", which is preferable to a silently
  # wrong cost attribution.
  LAST_JSON="$(grep -E '^\{' "$OUT_LOG" | tail -1 || true)"
  if [ -n "$LAST_JSON" ]; then
    echo "$LAST_JSON" | agent_usage_json_from_claude_result \
      > "${TASK_DIR}.agent-usage.json.tmp" 2>/dev/null || true
    # Atomic publish via tmp+rename so the harness, which polls
    # this file, never reads a half-flushed jq stream. Empty / failed jq
    # output is delivered as a missing file (matches the prior contract
    # that "no instrumentation" is preferable to a zero-byte parse target).
    if [ -s "${TASK_DIR}.agent-usage.json.tmp" ]; then
      mv "${TASK_DIR}.agent-usage.json.tmp" "${TASK_DIR}agent-usage.json"
    else
      rm -f "${TASK_DIR}.agent-usage.json.tmp"
    fi
  fi
fi

# Per-task agent-code.json sidecar (M1.2). Written BEFORE the
# turn-budget block so over-budget tasks still capture their code.
#
# Fields:
#   prompt         — full prompt string passed to the CLI via -p.
#   response_text  — EMPTY (reserved). Claude Code's --output-format=json
#                    terminal blob does not separate narrative from executed
#                    code; setting to "" is honest rather than fabricated.
#   executed_code  — heuristic extract of the agent log. We look
#                    for the last shebang-headed block or triple-backtick
#                    code fence in the log. May be empty when extraction
#                    fails (e.g. pure interactive tool invocations with no
#                    code block).
#   language       — inferred from shebang or interpreter hint in
#                    executed_code. One of "Python", "R", "Bash", "unknown".
#   started_at     — captured before the CLI invocation (AGENT_STARTED_AT).
#   completed_at   — captured now (after CLI exit).
#
# Atomically published via tmp+rename. Missing jq = skip silently.
if [ -n "${ECAA_TASK_ID:-}" ] && command -v jq >/dev/null 2>&1; then
  _AGENT_CODE_COMPLETED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  _AGENT_CODE_OUT_DIR="$PACKAGE/runtime/outputs/$ECAA_TASK_ID"
  mkdir -p "$_AGENT_CODE_OUT_DIR" 2>/dev/null || true

  # Extract the last non-empty code-fence block (```lang … ```) or
  # shebang-headed block from the agent log. Best-effort regex; falls
  # back to empty string. The sed captures everything between the last
  # opening fence and its matching closing fence; shebang path grabs
  # from the last '#!' line to the next blank-line-terminated block.
  _EXECUTED_CODE=""
  if [ -f "$OUT_LOG" ]; then
    # Try code-fence first (handles Python, R, bash inside ``` blocks).
    _EXECUTED_CODE="$(awk '
      /^```/ {
        if (in_block) { in_block=0; found=1 }
        else { in_block=1; buf="" }
        next
      }
      in_block { buf = buf $0 "\n" }
      END { if (found || in_block) printf "%s", buf }
    ' "$OUT_LOG" 2>/dev/null | tail -c 65536 || true)"

    # Fallback: look for a shebang line. Grab from the last shebang to the
    # nearest following blank line (or end of file). Caps at 64 KB.
    if [ -z "$_EXECUTED_CODE" ]; then
      _EXECUTED_CODE="$(grep -n '^\s*#!' "$OUT_LOG" 2>/dev/null \
        | tail -1 \
        | cut -d: -f1 \
        | xargs -I{} sh -c 'tail -n +{} "$1" | head -200' -- "$OUT_LOG" 2>/dev/null \
        | head -c 65536 || true)"
    fi
  fi

  # Language detection from first line of executed code.
  _LANGUAGE="unknown"
  _FIRST_LINE="$(printf '%s' "$_EXECUTED_CODE" | head -1 2>/dev/null || true)"
  case "$_FIRST_LINE" in
    *python*) _LANGUAGE="Python" ;; # matches python + python3
    *Rscript*|*"#!/usr/bin/env R"*|*"#!/usr/bin/R"*) _LANGUAGE="R" ;;
    *bash*|*zsh*|*sh*) _LANGUAGE="Bash" ;; # bash/zsh first; bare sh would also match here
  esac
  # Fallback: check for R-specific syntax in the extracted block.
  if [ "$_LANGUAGE" = "unknown" ] && printf '%s' "$_EXECUTED_CODE" | grep -qE 'library\(|<-|%>%|dplyr::|ggplot\(' 2>/dev/null; then
    _LANGUAGE="R"
  fi
  if [ "$_LANGUAGE" = "unknown" ] && printf '%s' "$_EXECUTED_CODE" | grep -qE 'import pandas|import numpy|from sklearn|def [a-z_]+\(' 2>/dev/null; then
    _LANGUAGE="Python"
  fi

  # Build the JSON atomically. jq -Rs reads the whole prompt as a
  # single string (handles embedded newlines, quotes, backslashes).
  # --argjson for the other fields avoids double-encoding issues.
  _AC_TMP="${_AGENT_CODE_OUT_DIR}/agent-code.json.tmp"
  {
    printf '%s' "$PROMPT" | jq -Rs \
      --arg rt ""  \
      --arg ec "$_EXECUTED_CODE" \
      --arg lang "$_LANGUAGE" \
      --arg sa "${AGENT_STARTED_AT:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}" \
      --arg ca "$_AGENT_CODE_COMPLETED_AT" \
      '{
        prompt: .,
        response_text: $rt,
        executed_code: $ec,
        language: $lang,
        started_at: $sa,
        completed_at: $ca
      }' > "$_AC_TMP" 2>/dev/null
  } || true
  if [ -s "$_AC_TMP" ]; then
    mv "$_AC_TMP" "${_AGENT_CODE_OUT_DIR}/agent-code.json"
  else
    rm -f "$_AC_TMP"
  fi
fi

# Turn-budget enforcement (BlockerKind::TurnBudgetExceeded).
# Reads num_turns from agent-usage.json; if it exceeds MAX_TURNS_PER_TASK
# and the agent didn't already self-block, rewrite result.json and
# state.patch.json so the harness applies the budget block.
enforce_turn_budget_limit "$PACKAGE" "${ECAA_TASK_ID:-}" "$MAX_TURNS_PER_TASK"

# Log which policies were referenced this turn.
log_policy_opens "$OUT_LOG"

# Per-task SBOM emission. When the task ran in
# container-mode (preferred_container resolved to a non-empty digest)
# and `syft` is on PATH, scan the resolved image and write the SPDX
# JSON SBOM to `runtime/sboms/<task_id>.spdx.json`. The RO-Crate
# emitter (S6.14) registers this as a `CreativeWork` `hasPart` of the
# task's `CreateAction` — the retrospective P-PLAN side. Skipped
# automatically for host-mode tasks (no container = no image to
# fingerprint). `ECAA_SBOM_EMIT=0` opts out site-wide.
if [ "${ECAA_SBOM_EMIT:-1}" = "1" ] \
   && [ -n "${ECAA_TASK_ID:-}" ] \
   && [ -n "${TASK_CONTAINER_DIGEST:-}" ] \
   && command -v syft >/dev/null 2>&1; then
  SBOM_DIR="$PACKAGE/runtime/sboms"
  mkdir -p "$SBOM_DIR" 2>/dev/null || true
  syft scan "oci:${TASK_CONTAINER_IMAGE:-}@${TASK_CONTAINER_DIGEST}" \
    -o spdx-json="$SBOM_DIR/$ECAA_TASK_ID.spdx.json" 2>/dev/null \
    || echo "agent-claude.sh: syft scan failed for $ECAA_TASK_ID — SBOM not emitted (non-fatal)" >&2
fi

# Reconcile contradictory Claude Code outcomes only after all forensic
# sidecars have been written. A nonzero CLI status remains nonzero unless
# the terminal JSON says success and the task produced a parseable patch.
_ORIGINAL_CLAUDE_EXIT="$CLAUDE_EXIT"
CLAUDE_EXIT="$(normalize_claude_exit_status "$CLAUDE_EXIT" "$OUT_LOG" "$PACKAGE" "${ECAA_TASK_ID:-}")"
if [ "$_ORIGINAL_CLAUDE_EXIT" != "$CLAUDE_EXIT" ]; then
  echo "[agent-exit] normalized Claude Code exit $_ORIGINAL_CLAUDE_EXIT to $CLAUDE_EXIT after successful terminal result and state.patch.json" >&2
fi

exit "$CLAUDE_EXIT"
