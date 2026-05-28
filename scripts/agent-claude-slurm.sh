#!/usr/bin/env bash
# agent-claude-slurm.sh — Claude Code agent wrapper for SLURM compute nodes.
#
# Invoked on the remote compute node (not the login node) via
# run-task-on-slurm.sh. Mirrors agent-claude-aws.sh in shape but
# drops the AWS-specific CloudWatch MCP — HPC sites don't expose one.
# Singularity/Apptainer containerization is intentionally deferred —
# the agent runtime is expected to be available via `module load` or a
# pre-installed user environment.
#
# ECAA_SLURM_* env vars the harness passes through via
# `#SBATCH --export=` are enumerated in the remote-compute operator
# reference.
#
# Deliberate omission: runtime/outputs/<task>/agent-usage.json sidecar.
# --------------------------------------------------------------------
# agent-claude.sh and agent-claude-aws.sh invoke `claude` with
# --output-format=json and post-process stdout with jq to write an
# agent-usage.json sidecar the harness forwards to the server's
# session-metrics path. That sidecar feeds `agent_cost_usd` in the
# Performance tab.
#
# This script does NOT emit the sidecar by design. Rationale:
#  1. On-prem SLURM is typically subscription-billed (site-local
#  ~/.claude/.credentials.json) or free — per-token dollar
#  attribution carries no operational meaning.
#  2. --output-format=json changes stdout shape and would break any
#  site-local wrappers that parse the agent's plain-text output.
#  3. The CloudWatch-style per-node metrics that justify the sidecar
#  on AWS are not available on HPC nodes; operators inspect via
#  sstat/sacct instead.
#
# If a site explicitly wants the sidecar (api-billed SLURM is the
# only realistic case), add an opt-in path gated on something like
# ECAA_SLURM_EMIT_USAGE=1 that mirrors the jq post-processor block
# from agent-claude-aws.sh. Keep the default off so existing sites
# aren't disturbed.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "$0")" && pwd)"
# Shared defaults. Each value is `: "${VAR:=default}"`, so any env override
# in the calling shell wins.
source "$SCRIPT_DIR/agent-claude-common.sh"

# Security remediation validate
# ECAA_CHAT_SESSION_ID before it interpolates into per-session paths
# and labels. `validate_uuid` lives in agent-claude-common.sh.
if [ -n "${ECAA_CHAT_SESSION_ID:-}" ]; then
    validate_uuid "$ECAA_CHAT_SESSION_ID"
fi

# Validate ECAA_TASK_ID before per-task path/label interpolation.
# Same defense the local + aws wrappers carry.
if [ -n "${ECAA_TASK_ID:-}" ]; then
    validate_task_id "$ECAA_TASK_ID"
fi

PACKAGE="$(realpath "$1")"

# `run_no_xtrace` (01, C-12) is defined in agent-claude-common.sh.

if [[ "${SLURM_CPUS_PER_TASK:-}" =~ ^[0-9]+$ ]] && [ "$SLURM_CPUS_PER_TASK" -gt 0 ]; then
  : "${ECAA_HW_NPROC_HINT:=$SLURM_CPUS_PER_TASK}"
  export ECAA_HW_NPROC_HINT
  export ECAA_HW_VCPUS_AVAILABLE="$ECAA_HW_NPROC_HINT"
  export ECAA_HW_RECOMMENDED_THREADS="$ECAA_HW_NPROC_HINT"
fi
if [[ "${SLURM_MEM_PER_NODE:-}" =~ ^[0-9]+$ ]] && [ "$SLURM_MEM_PER_NODE" -gt 0 ]; then
  export ECAA_HW_MEMORY_GB=$(((SLURM_MEM_PER_NODE + 1023) / 1024))
elif [[ "${SLURM_MEM_PER_CPU:-}" =~ ^[0-9]+$ ]] \
  && [ "$SLURM_MEM_PER_CPU" -gt 0 ] \
  && [[ "${ECAA_HW_VCPUS_AVAILABLE:-}" =~ ^[0-9]+$ ]]; then
  export ECAA_HW_MEMORY_GB=$((((SLURM_MEM_PER_CPU * ECAA_HW_VCPUS_AVAILABLE) + 1023) / 1024))
fi

# BLAS / OpenMP / numerical-library thread-budget exports + R BLAS
# probe. See scripts/_agent-blas-bootstrap.sh for rationale. The probe
# runs on the SLURM compute node, not the login node — Rscript may be
# behind `module load r/...` so we let the bootstrap discover what's
# actually on PATH at the agent's invocation site.
__BOOTSTRAP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
. "$__BOOTSTRAP_DIR/_agent-blas-bootstrap.sh"

# Memory-discipline policy surface (mirror of agent-claude.sh). SLURM
# nodes enforce their own memory cap via `#SBATCH --mem=…`, but the
# policy still applies — even on a 256 GB node a 500k-cell global
# dense merge will OOM.
MEMORY_DISCIPLINE_BLOCK=""
if [ -f "$PACKAGE/policies/memory-discipline.json" ] && command -v jq >/dev/null 2>&1; then
  MAX_GB="$(jq -r '.max_dense_matrix_gb // empty' "$PACKAGE/policies/memory-discipline.json" 2>/dev/null || true)"
  COHORT_K="$(jq -r '.large_cohort_cell_threshold_k // empty' "$PACKAGE/policies/memory-discipline.json" 2>/dev/null || true)"
  if [ -n "$MAX_GB" ] && [ -n "$COHORT_K" ]; then
    MEMORY_DISCIPLINE_BLOCK="

## Memory discipline (REQUIRED when stage handles > ${COHORT_K}k cells or artifacts > ${MAX_GB} GB dense)
Read policies/memory-discipline.json at the start of the task. If your stage touches a cohort above the cell-count threshold OR would materialize a dense matrix above the GB threshold, use an on-disk library from the policy's on_disk_library_hints instead of a dense in-memory matrix. For Seurat v5 scRNA-seq, that means BPCells::write_matrix_dir + open_matrix_dir as the assay backing for SCTransform v2. If the stage would normally end with a global merge (e.g. merging every compartment into an 'all_cells' object), verify the merge is actually consumed downstream before materializing it — redundant global merges have caused production OOMs in prior runs."
  fi
fi

# Shared task-execution body: patch-merge envelope, blocker_kind
# vocabulary, discovery-stage block-by-default rule, iterate-until /
# figures / progress contracts. Single source of truth at
# scripts/agent-prompts/task-execution.md so this wrapper cannot drift
# from the local + aws WAL-guarded state.patch.json contract — the
# previous inline body instructed the agent to "Update WORKFLOW.json
# directly" which races with the harness's patch-merge step.
TASK_EXECUTION_BODY="$(load_task_execution_prompt "$SCRIPT_DIR/agent-prompts/task-execution.md")"

PROMPT="$(cat "$PACKAGE/PROMPT.md")${MEMORY_DISCIPLINE_BLOCK}

## Package location
All paths are relative to: $PACKAGE

## Remote execution context
This task runs on a SLURM cluster compute node. The harness
submitted the job via sbatch on behalf of the SME. No cloud-metrics
MCP is available; inspect /proc/meminfo, /proc/cpuinfo, and the
\`sstat\` command directly if you need runtime insight.

${TASK_EXECUTION_BODY}"

# Per-task container resolution. WORKFLOW.json's
# tasks.<task_id>.container is the source of truth (S15.2); falls back
# to policies/container.json::image (legacy), then host-env.
# HPC sites typically use singularity/apptainer/podman rather than
# docker for security + rootless reasons — `ECAA_SLURM_CONTAINER_RUNTIME`
# selects the runtime (defaults to apptainer when the `apptainer`
# binary is on PATH, then singularity, then podman, then docker).
CONTAINER_IMAGE=""
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
    # Gpu_required wires through to apptainer `--nv`
    # (preferred per Round-2 §3.10) or docker `--gpus all`. The SLURM
    # `--gres=gpu:<type>:<count>` is set by `slurm/sbatch.rs::render`
    # at submit time so the node already has the GPU; the agent
    # script just needs to surface it inside the container.
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
    export TASK_CONTAINER_GPU_REQUIRED="$TC_GPU_REQUIRED"
  fi

  # Per-task GPU target (count + kind + mig_profile).
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
if [ -z "$CONTAINER_IMAGE" ]; then
  CONTAINER_POLICY="$PACKAGE/policies/container.json"
  if [ -f "$CONTAINER_POLICY" ] && command -v jq >/dev/null 2>&1; then
    CONTAINER_IMAGE="$(jq -r '.image // empty' "$CONTAINER_POLICY" 2>/dev/null || true)"
  fi
fi

# Pick a container runtime — explicit override first, then auto-probe.
select_slurm_runtime() {
  if [ -n "${ECAA_SLURM_CONTAINER_RUNTIME:-}" ]; then
    echo "$ECAA_SLURM_CONTAINER_RUNTIME"
    return
  fi
  for rt in apptainer singularity podman docker; do
    if command -v "$rt" >/dev/null 2>&1; then
      echo "$rt"
      return
    fi
  done
  echo ""
}

# Policy-open log mirror. SLURM compute nodes write the same
# `runtime/policy-opens.jsonl` shape so disposition runs can aggregate
# across all backends.
OUT_LOG="$(mktemp -t agent-claude-slurm.XXXXXX.log)"
trap 'rm -f "$OUT_LOG"' EXIT

if [ -n "${ECAA_TASK_ID:-}" ]; then
  SCRATCH_BASE="${ECAA_AGENT_SCRATCH_DIR:-$PACKAGE/runtime/scratch}"
  SCRATCH_DIR="$SCRATCH_BASE/$ECAA_TASK_ID"
  mkdir -p "$SCRATCH_DIR" 2>/dev/null || true
  export ECAA_TASK_SCRATCH_DIR="$SCRATCH_DIR"
fi

if [ "${ECAA_AGENT_CACHE_DISABLE:-0}" != "1" ] && [ -n "${ECAA_CHAT_SESSION_ID:-}" ]; then
  CACHE_BASE="${ECAA_AGENT_CACHE_DIR:-$HOME/.scripps-workflow/agent-cache}"
  CACHE_DIR="$CACHE_BASE/$ECAA_CHAT_SESSION_ID"
  mkdir -p "$CACHE_DIR/pip" "$CACHE_DIR/conda" "$CACHE_DIR/apt" "$CACHE_DIR/R-libs" "$CACHE_DIR/python" 2>/dev/null || true
  export ECAA_SESSION_CACHE_DIR="$CACHE_DIR"
  export PIP_CACHE_DIR="$CACHE_DIR/pip"
  export CONDA_PKGS_DIRS="$CACHE_DIR/conda"
  export R_LIBS_USER="$CACHE_DIR/R-libs"
fi

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
      echo "{\"ts\":\"$ts\",\"policy\":\"$name\",\"backend\":\"slurm\"}" >> "$sink"
    fi
  done
}

# Agent billing: same semantics as scripts/agent-claude.sh.
if [ "${ECAA_AGENT_BILLING:-subscription}" = "subscription" ]; then
  unset ANTHROPIC_API_KEY
elif [ "${ECAA_AGENT_BILLING:-}" = "api" ]; then
  if [ -z "${ANTHROPIC_API_KEY:-}" ] && [ -n "${ECAA_ANTHROPIC_API_KEY:-}" ]; then
    # Suppress xtrace around the secret expansion
    # so the literal key never lands in any active trace log.
    { set +x; } 2>/dev/null
    export ANTHROPIC_API_KEY="$ECAA_ANTHROPIC_API_KEY"
    { set -x; } 2>/dev/null
  fi
fi

# Memory cap — mirror of agent-claude.sh. On SLURM, the sbatch
# --mem=<N>G is the authoritative hard cap (enforced by cgroups
# at the slurmd level). This knob is a per-agent belt-and-braces
# ceiling inside whatever SLURM allocated — useful when the agent
# invocation shares a node with other jobs.
AGENT_CMD_PREFIX=()
CONTAINER_MEM_ARGS=()
AGENT_MEMORY_LIMIT_GB=""
if [ -n "${ECAA_AGENT_MEMORY_CAP_GB:-}" ]; then
  if ! [[ "$ECAA_AGENT_MEMORY_CAP_GB" =~ ^[0-9]+$ ]]; then
    echo "agent-claude-slurm.sh: ECAA_AGENT_MEMORY_CAP_GB must be a positive integer (got '$ECAA_AGENT_MEMORY_CAP_GB'); ignoring." >&2
  else
    AGENT_MEMORY_LIMIT_GB="$ECAA_AGENT_MEMORY_CAP_GB"
    # HPC nodes rarely expose a user systemd instance — prefer prlimit,
    # which works regardless. If somehow systemd --user is alive, take
    # it for the nicer cgroup-scoped kill.
    if command -v systemd-run >/dev/null 2>&1 \
      && systemd-run --user --scope --quiet -p "MemoryMax=100M" /bin/true >/dev/null 2>&1; then
      # Pair MemoryMax with MemoryHigh = 0.85*MemoryMax.
      MEM_MAX_MB=$((ECAA_AGENT_MEMORY_CAP_GB * 1024))
      MEM_HIGH_MB=$((MEM_MAX_MB * ECAA_AGENT_MEMORY_HIGH_WATER_PCT / 100))
      AGENT_CMD_PREFIX=(systemd-run --user --scope --quiet \
        -p "MemoryMax=${ECAA_AGENT_MEMORY_CAP_GB}G" \
        -p "MemoryHigh=${MEM_HIGH_MB}M")
    elif command -v prlimit >/dev/null 2>&1; then
      CAP_BYTES=$((ECAA_AGENT_MEMORY_CAP_GB * 1024 * 1024 * 1024))
      AGENT_CMD_PREFIX=(prlimit "--as=$CAP_BYTES")
    fi
    # Docker path on SLURM compute nodes (rare; most
    # HPC sites use apptainer) gets --memory-reservation paired with
    # --memory. apptainer/singularity don't accept --memory at all
    # (cgroup wrapping is via systemd-run above).
    CONTAINER_RESERVATION_MB=$((ECAA_AGENT_MEMORY_CAP_GB * 1024 * 85 / 100))
    CONTAINER_MEM_ARGS=(
      "--memory=${ECAA_AGENT_MEMORY_CAP_GB}g"
      "--memory-reservation=${CONTAINER_RESERVATION_MB}m"
    )
  fi
elif [[ "${ECAA_HW_MEMORY_GB:-}" =~ ^[0-9]+$ ]] && [ "$ECAA_HW_MEMORY_GB" -gt 0 ]; then
  AGENT_MEMORY_LIMIT_GB="$ECAA_HW_MEMORY_GB"
fi
if [ -n "$AGENT_MEMORY_LIMIT_GB" ] && [ "${#CONTAINER_MEM_ARGS[@]}" -eq 0 ]; then
  CONTAINER_RESERVATION_MB=$((AGENT_MEMORY_LIMIT_GB * 1024 * 85 / 100))
  CONTAINER_MEM_ARGS=(
    "--memory=${AGENT_MEMORY_LIMIT_GB}g"
    "--memory-reservation=${CONTAINER_RESERVATION_MB}m"
  )
fi

# Parse ECAA_CONTAINER_REGISTRY_AUTH=<registry>|<user>|<pass>.
# Sets _AGENT_REG_HOST / _AGENT_REG_USER / _AGENT_REG_PASS when the
# image registry hostname matches the configured registry; leaves
# them empty otherwise. Pipe-delimited so we accept passwords that
# contain colons (common in ECR / docker hub access tokens).
__agent_parse_registry_auth() {
  _AGENT_REG_HOST=""
  _AGENT_REG_USER=""
  _AGENT_REG_PASS=""
  local raw="${ECAA_CONTAINER_REGISTRY_AUTH:-}"
  [ -z "$raw" ] && return 0
  [ -z "$CONTAINER_IMAGE" ] && return 0
  local cfg_host cfg_user cfg_pass
  cfg_host="${raw%%|*}"
  local rest="${raw#*|}"
  cfg_user="${rest%%|*}"
  cfg_pass="${rest#*|}"
  if [ -z "$cfg_host" ] || [ -z "$cfg_user" ] || [ "$cfg_pass" = "$rest" ]; then
    echo "  [agent-slurm] warn: ECAA_CONTAINER_REGISTRY_AUTH must be registry|user|pass; ignoring" >&2
    return 0
  fi
  # Image may carry a `docker://` prefix or a digest/tag; reduce to
  # the hostname for the comparison. Hub-style refs (`ubuntu:latest`)
  # have no slashes — skip auth in that case.
  local img_no_scheme="${CONTAINER_IMAGE#docker://}"
  case "$img_no_scheme" in
    */*)
      local img_host="${img_no_scheme%%/*}"
      if [ "$img_host" = "$cfg_host" ]; then
        _AGENT_REG_HOST="$cfg_host"
        _AGENT_REG_USER="$cfg_user"
        _AGENT_REG_PASS="$cfg_pass"
      fi
      ;;
  esac
}

if [ -n "$CONTAINER_IMAGE" ]; then
  RT="$(select_slurm_runtime)"
  __agent_parse_registry_auth
  case "$RT" in
    apptainer|singularity)
      # HPC norm: apptainer/singularity consume docker:// URIs and
      # bind-mount the package RW. Bind ~/.claude read-only so the
      # subscription creds file is available inside the container.
      # apptainer/singularity do not accept --memory; fall back to
      # prlimit on the outer wrapper when a cap is requested.
      # apptainer/singularity also strip parent env (modulo --env
      # passthrough) so we forward BLAS + ECAA_HW_* explicitly.
      #
      # Pre-pull with registry credentials when ECAA_CONTAINER_REGISTRY_AUTH
      # matches the image hostname. apptainer caches by default
      # ($APPTAINER_CACHEDIR / ~/.apptainer/cache), so the subsequent
      # `exec docker://...` will read from the cache and skip the
      # network round-trip. Suppress xtrace around the password
      # expansion so it never lands in any active trace log.
      if [ -n "${_AGENT_REG_HOST:-}" ]; then
        { set +x; } 2>/dev/null
        if ! "$RT" pull --force \
              --docker-username "$_AGENT_REG_USER" \
              --docker-password "$_AGENT_REG_PASS" \
              "docker://$CONTAINER_IMAGE" >/dev/null 2>&1; then
          echo "  [agent-slurm] warn: apptainer pull with registry auth failed for $_AGENT_REG_HOST; falling back to exec-time pull" >&2
        fi
        { set -x; } 2>/dev/null
      fi
      __agent_build_env_forward_pairs
      APT_ENV_ARGS=()
      for __agent_kv in "${_AGENT_ENV_FORWARD_PAIRS[@]}"; do
        APT_ENV_ARGS+=(--env "$__agent_kv")
      done
      unset __agent_kv
      if [ "${ECAA_AGENT_BILLING:-subscription}" = "api" ] \
         && [ -n "${ANTHROPIC_API_KEY:-}" ]; then
        # Suppress xtrace around apptainer --env
        # composition so the literal key isn't echoed to any trace log.
        { set +x; } 2>/dev/null
        APT_ENV_ARGS+=(--env "ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY")
        { set -x; } 2>/dev/null
      fi
      # Apptainer `--nv` reads host NVIDIA drivers
      # natively (no toolkit install on compute nodes per Round-2
      # §3.10 "Apptainer over Docker"). Only flip on when the atom
      # asked for it AND the SLURM partition allocated a GPU.
      APT_GPU_ARGS=()
      if [ "${TASK_CONTAINER_GPU_REQUIRED:-false}" = "true" ] \
         && [ "${TASK_GPU_COUNT:-0}" != "0" ]; then
        APT_GPU_ARGS+=("--nv")
      fi
      APT_BIND_ARGS=()
      if [ -n "${ECAA_SESSION_CACHE_DIR:-}" ]; then
        APT_BIND_ARGS+=(--bind "$ECAA_SESSION_CACHE_DIR":"$ECAA_SESSION_CACHE_DIR")
        APT_ENV_ARGS+=(
          --env "ECAA_SESSION_CACHE_DIR=$ECAA_SESSION_CACHE_DIR"
          --env "R_LIBS_USER=$ECAA_SESSION_CACHE_DIR/R-libs"
          --env "PIP_CACHE_DIR=$ECAA_SESSION_CACHE_DIR/pip"
          --env "CONDA_PKGS_DIRS=$ECAA_SESSION_CACHE_DIR/conda"
          --env "PYTHONUSERBASE=$ECAA_SESSION_CACHE_DIR/python"
          --env "PIP_USER=1"
          --env "PIP_BREAK_SYSTEM_PACKAGES=1"
        )
      fi
      if [ -n "${ECAA_TASK_SCRATCH_DIR:-}" ]; then
        case "$ECAA_TASK_SCRATCH_DIR" in
          "$PACKAGE"/*) ;;
          *) APT_BIND_ARGS+=(--bind "$ECAA_TASK_SCRATCH_DIR":"$ECAA_TASK_SCRATCH_DIR") ;;
        esac
        APT_ENV_ARGS+=(--env "ECAA_TASK_SCRATCH_DIR=$ECAA_TASK_SCRATCH_DIR")
      fi
      # apptainer containment profile.
      # `--contain` blocks the default $HOME/$TMPDIR auto-bind; we
      # re-bind only the package workdir + ~/.claude RO + any opt-in
      # session/scratch dirs. `--no-mount home,tmp,sys-proc` is an
      # additional belt over the suspenders, and `--writable-tmpfs`
      # gives the agent a writeable in-memory rootfs overlay so
      # toolchains that touch `/usr/local` succeed without inheriting
      # the host filesystem. `--no-privs` matches docker
      # `no-new-privileges`.
      #
      # Network selection precedence: per-task ECAA_TASK_NETWORK
      # (stamped by harness::stamp_safety_network from the atom's
      # safety.network policy) wins; falls back to operator-set
      # ECAA_CONTAINER_NETWORK_DEFAULT; defaults to `bridge` so the
      # PROMPT.md "install at task start" path (pip / BiocManager /
      # conda for SME-pinned or discover-picked methods that aren't
      # in the base image) can reach pypi / Bioconductor / bioconda.
      # Operators who need air-gapped execution should either pin
      # ECAA_CONTAINER_NETWORK_DEFAULT=none in the env or set
      # safety.network: { kind: None } on the relevant atoms — both
      # propagate through ECAA_TASK_NETWORK and override this default.
      # Values:
      #   none   → --net --network=none (isolated)
      #   bridge → --net --network=bridge (cluster network reachable; default)
      #   host   → no --net flag (host network namespace)
      APT_NET_ARGS=()
      __agent_net="${ECAA_TASK_NETWORK:-${ECAA_CONTAINER_NETWORK_DEFAULT:-bridge}}"
      case "$__agent_net" in
        none)
          APT_NET_ARGS+=(--net --network=none)
          ;;
        bridge)
          APT_NET_ARGS+=(--net --network=bridge)
          ;;
        host)
          ;;
        *)
          echo "  [agent-slurm] warn: unrecognized network policy '$__agent_net'; defaulting to bridge" >&2
          APT_NET_ARGS+=(--net --network=bridge)
          ;;
      esac
      unset __agent_net
      "${AGENT_CMD_PREFIX[@]}" "$RT" exec \
        --contain \
        --no-mount home,tmp,sys-proc \
        --writable-tmpfs \
        --no-privs \
        "${APT_NET_ARGS[@]}" \
        "${APT_GPU_ARGS[@]}" \
        --bind "$PACKAGE":"$PACKAGE" \
        "${APT_BIND_ARGS[@]}" \
        --bind "$HOME/.claude":"$HOME/.claude":ro \
        --pwd "$PACKAGE" \
        --env "HOME=$HOME" \
        "${APT_ENV_ARGS[@]}" \
        "docker://$CONTAINER_IMAGE" \
        claude --dangerously-skip-permissions -p "$PROMPT" \
        | tee "$OUT_LOG"
      CLAUDE_EXIT="${PIPESTATUS[0]}"
      # Container-state sidecar. Apptainer doesn't
      # carry a label surface like docker, so the orphan-reaper-side
      # join key is the runtime/outputs/<task_id>/.container-state.json
      # file (squeue tells us the SLURM job is alive; this file tells
      # us the container exited).
      if [ -n "${ECAA_TASK_ID:-}" ]; then
        CONTAINER_STATE_DIR="$PACKAGE/runtime/outputs/$ECAA_TASK_ID"
        mkdir -p "$CONTAINER_STATE_DIR" 2>/dev/null || true
        cat > "$CONTAINER_STATE_DIR/.container-state.json" 2>/dev/null <<EOF || true
{
  "exit_code": $CLAUDE_EXIT,
  "image": "${CONTAINER_IMAGE:-}",
  "runtime": "$RT",
  "session_id": "${ECAA_CHAT_SESSION_ID:-}",
  "task_id": "${ECAA_TASK_ID}",
  "backend": "slurm",
  "ended_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
      fi
      log_policy_opens "$OUT_LOG"
      exit "$CLAUDE_EXIT"
      ;;
    podman|docker)
      # podman/docker also drop parent env at the container boundary.
      # Auth via `docker/podman login --password-stdin` when the
      # configured registry hostname matches the image hostname.
      # Suppress xtrace around the password so it never lands in any
      # active trace log.
      if [ -n "${_AGENT_REG_HOST:-}" ]; then
        { set +x; } 2>/dev/null
        if ! printf '%s' "$_AGENT_REG_PASS" \
              | "$RT" login --username "$_AGENT_REG_USER" --password-stdin \
                "$_AGENT_REG_HOST" >/dev/null 2>&1; then
          echo "  [agent-slurm] warn: $RT login failed for $_AGENT_REG_HOST; continuing with anonymous pull" >&2
        fi
        { set -x; } 2>/dev/null
      fi
      "$RT" pull "$CONTAINER_IMAGE" >/dev/null 2>&1 || true
      __agent_build_env_forward_pairs
      DOCKER_ENV_ARGS=()
      for __agent_kv in "${_AGENT_ENV_FORWARD_PAIRS[@]}"; do
        DOCKER_ENV_ARGS+=(-e "$__agent_kv")
      done
      unset __agent_kv
      if [ "${ECAA_AGENT_BILLING:-subscription}" = "api" ] \
         && [ -n "${ANTHROPIC_API_KEY:-}" ]; then
        # Suppress xtrace around docker/podman -e
        # env-arg composition so the literal key isn't echoed to any
        # active trace log.
        { set +x; } 2>/dev/null
        DOCKER_ENV_ARGS+=(-e "ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY")
        { set -x; } 2>/dev/null
      fi
      # Docker / podman GPU passthrough. MIG profile
      # routes to `--gpus device=<profile>` (S5.49: H100 slices like
      # `3g.40gb`); otherwise `--gpus all`.
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

      DOCKER_CACHE_ARGS=()
      if [ -n "${ECAA_SESSION_CACHE_DIR:-}" ]; then
        DOCKER_CACHE_ARGS+=(
          -v "$ECAA_SESSION_CACHE_DIR":"$ECAA_SESSION_CACHE_DIR":rw
          -e "ECAA_SESSION_CACHE_DIR=$ECAA_SESSION_CACHE_DIR"
          -e "R_LIBS_USER=$ECAA_SESSION_CACHE_DIR/R-libs"
          -e "PIP_CACHE_DIR=$ECAA_SESSION_CACHE_DIR/pip"
          -e "CONDA_PKGS_DIRS=$ECAA_SESSION_CACHE_DIR/conda"
          -e "PYTHONUSERBASE=$ECAA_SESSION_CACHE_DIR/python"
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
      # Same labeling as the local + AWS docker paths.
      DOCKER_LABEL_ARGS=()
      if [ -n "${ECAA_TASK_ID:-}" ]; then
        DOCKER_LABEL_ARGS+=("--label" "swfc-task=${ECAA_TASK_ID}")
      fi
      if [ -n "${ECAA_CHAT_SESSION_ID:-}" ]; then
        DOCKER_LABEL_ARGS+=("--label" "swfc-session=${ECAA_CHAT_SESSION_ID}")
      fi

      # Docker isolation hardening on the SLURM docker/podman
      # path. RW $PACKAGE stays mounted for
      # output writes; tmpfs covers /tmp and /var/tmp on a read-only
      # rootfs. All caps dropped, no-new-privileges blocks suid,
      # --pids-limit caps fork-bombs.
      "$RT" run --rm \
        --read-only \
        --tmpfs "/tmp:rw,size=$ECAA_DOCKER_TMPFS_TMP_SIZE,mode=1777" \
        --tmpfs "/var/tmp:rw,size=$ECAA_DOCKER_TMPFS_VARTMP_SIZE,mode=1777" \
        --security-opt no-new-privileges \
        --cap-drop=ALL \
        --pids-limit "$ECAA_DOCKER_PIDS_LIMIT" \
        "${CONTAINER_MEM_ARGS[@]}" \
        "${DOCKER_CPU_ARGS[@]}" \
        "${DOCKER_GPU_ARGS[@]}" \
        "${DOCKER_LABEL_ARGS[@]}" \
        -v "$PACKAGE":"$PACKAGE":rw \
        "${DOCKER_CACHE_ARGS[@]}" \
        "${DOCKER_SCRATCH_ARGS[@]}" \
        -v "$HOME/.claude":"$HOME/.claude":ro \
        -w "$PACKAGE" \
        -e "HOME=$HOME" \
        "${DOCKER_ENV_ARGS[@]}" \
        "$CONTAINER_IMAGE" \
        claude --dangerously-skip-permissions -p "$PROMPT" \
        | tee "$OUT_LOG"
      CLAUDE_EXIT="${PIPESTATUS[0]}"
      if [ -n "${ECAA_TASK_ID:-}" ]; then
        CONTAINER_STATE_DIR="$PACKAGE/runtime/outputs/$ECAA_TASK_ID"
        mkdir -p "$CONTAINER_STATE_DIR" 2>/dev/null || true
        cat > "$CONTAINER_STATE_DIR/.container-state.json" 2>/dev/null <<EOF || true
{
  "exit_code": $CLAUDE_EXIT,
  "image": "${CONTAINER_IMAGE:-}",
  "runtime": "$RT",
  "session_id": "${ECAA_CHAT_SESSION_ID:-}",
  "task_id": "${ECAA_TASK_ID}",
  "backend": "slurm",
  "ended_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
      fi
      log_policy_opens "$OUT_LOG"
      exit "$CLAUDE_EXIT"
      ;;
    *)
      # No runtime available — fall through to host-env.
      echo "agent-claude-slurm.sh: container declared but no runtime on PATH; falling back to host env" >&2
      ;;
  esac
fi

"${AGENT_CMD_PREFIX[@]}" npx @anthropic-ai/claude-code \
  --dangerously-skip-permissions \
  -p "$PROMPT" \
  | tee "$OUT_LOG"
CLAUDE_EXIT="${PIPESTATUS[0]}"
log_policy_opens "$OUT_LOG"
exit "$CLAUDE_EXIT"
