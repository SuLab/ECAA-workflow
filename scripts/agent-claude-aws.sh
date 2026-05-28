#!/usr/bin/env bash
# agent-claude-aws.sh — Claude Code agent with AWS-aware MCP server access.
#
# Companion to scripts/agent-claude.sh
# that adds the awslabs.cloudwatch-mcp-server MCP so the agent can
# query live CPU / memory / throughput metrics for the task's EC2
# instance during a long-running compute step.
#
# Operator-facing template that works once the AMI + IAM are
# provisioned per docs/remote-compute-operator-reference.md. The real
# SWFC_AWS_INSTANCE_ID + SWFC_AWS_COMMAND_ID wrappers are handed
# off via SSM RunCommand.
#
# IAM permissions required on the caller credentials:
#  - cloudwatch:GetMetricData
#  - cloudwatch:ListMetrics
#  - (optional) logs:GetLogEvents for in-task log queries
# Scope these to the session-tagged EC2 instances via a condition on
# the Name tag: { "Condition": { "StringEquals": { "ec2:ResourceTag/scripps-session-id": "<session-id>" } } }
# See docs/remote-compute-operator-reference.md#environment-variables
# for the full list of SWFC_AWS_* env vars this script reads.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "$0")" && pwd)"
# Shared defaults. Each value is `: "${VAR:=default}"`, so any env override
# in the calling shell wins.
source "$SCRIPT_DIR/agent-claude-common.sh"

# Security remediation validate
# SWFC_CHAT_SESSION_ID before it interpolates into per-session paths
# and labels. `validate_uuid` lives in agent-claude-common.sh.
if [ -n "${SWFC_CHAT_SESSION_ID:-}" ]; then
    validate_uuid "$SWFC_CHAT_SESSION_ID"
fi

# Validate SWFC_TASK_ID before any per-task path or docker label
# interpolation. Same shape rule the local agent and the Rust
# `_id_validator::is_safe_id` enforce on the harness side.
if [ -n "${SWFC_TASK_ID:-}" ]; then
    validate_task_id "$SWFC_TASK_ID"
fi

# SWFC_AWS_REGION and SWFC_AWS_PROFILE interpolate into a JSON
# heredoc — a hostile value containing `","x":"y"` would inject
# arbitrary keys into the MCP config and could pivot to RCE via a
# forged mcpServers entry. Refuse anything outside the standard AWS
# identifier shape before the heredoc fires. Region:
# `^[a-z]{2}-[a-z]+-[0-9]$` (e.g. `us-west-2`); profile:
# `^[A-Za-z0-9_.-]+$` (same shape AWS CLI enforces in
# `~/.aws/config`).
validate_aws_region() {
    local val="$1"
    if ! [[ "$val" =~ ^[a-z]{2}-[a-z]+-[0-9]+$ ]]; then
        echo "agent-claude-aws.sh: refusing unsafe SWFC_AWS_REGION='$val'" >&2
        exit 1
    fi
}
validate_aws_profile() {
    local val="$1"
    if ! [[ "$val" =~ ^[A-Za-z0-9_.-]+$ ]] || [ "${#val}" -gt 128 ]; then
        echo "agent-claude-aws.sh: refusing unsafe SWFC_AWS_PROFILE='$val'" >&2
        exit 1
    fi
}
if [ -n "${SWFC_AWS_REGION:-}" ]; then
    validate_aws_region "$SWFC_AWS_REGION"
fi
if [ -n "${SWFC_AWS_PROFILE:-}" ]; then
    validate_aws_profile "$SWFC_AWS_PROFILE"
fi
if [ -n "${GITHUB_TOKEN:-}" ] && ! [[ "$GITHUB_TOKEN" =~ ^[A-Za-z0-9_.-]+$ ]]; then
    echo "agent-claude-aws.sh: refusing unsafe GITHUB_TOKEN" >&2
    exit 1
fi

PACKAGE="$(realpath "$1")"

# `run_no_xtrace` (01, C-12) is defined in agent-claude-common.sh.

__aws_remote_vcpus="$(nproc 2>/dev/null || true)"
if [[ "$__aws_remote_vcpus" =~ ^[0-9]+$ ]] && [ "$__aws_remote_vcpus" -gt 0 ]; then
  : "${SWFC_HW_NPROC_HINT:=$__aws_remote_vcpus}"
  export SWFC_HW_NPROC_HINT
  export SWFC_HW_VCPUS_AVAILABLE="$SWFC_HW_NPROC_HINT"
  export SWFC_HW_RECOMMENDED_THREADS="$SWFC_HW_NPROC_HINT"
fi
unset __aws_remote_vcpus
__aws_mem_kb="$(awk '/^MemTotal:/ {print $2; exit}' /proc/meminfo 2>/dev/null || true)"
if [[ "$__aws_mem_kb" =~ ^[0-9]+$ ]] && [ "$__aws_mem_kb" -gt 0 ]; then
  export SWFC_HW_MEMORY_GB=$(((__aws_mem_kb + 1048575) / 1048576))
fi
unset __aws_mem_kb

# BLAS / OpenMP / numerical-library thread-budget exports + R BLAS
# probe. See scripts/_agent-blas-bootstrap.sh for rationale.
__BOOTSTRAP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
. "$__BOOTSTRAP_DIR/_agent-blas-bootstrap.sh"

# The MCP config file the claude-code CLI consumes. Written to a
# tempfile each invocation so the instance id + session id can vary
# per task without the operator hand-editing config.
MCP_CONFIG="$(mktemp -t scripps-mcp-XXXXXX.json)"
# (EXIT trap installed below alongside OUT_LOG cleanup so it captures both.)

# Build the MCP config JSON via `jq -n` instead of string-interpolated
# heredocs. The prior heredoc shape (`"AWS_REGION": "$SWFC_AWS_REGION",`)
# trusted upstream validation to never produce a value containing `","x":"y"`,
# which would inject arbitrary keys (or worse, a forged `command`) into
# the MCP config. `jq -n --arg <name> "<value>"` JSON-encodes every value
# unconditionally so a hostile region/profile/token only ever lands inside
# a string literal.
#
# Construction strategy: start with the always-on CloudWatch server,
# then fold in each opt-in server with `+` so the absence of any flag
# produces a literally identical JSON shape to before.
#
# Requires `jq` (already a hard requirement elsewhere in this script).
if ! command -v jq >/dev/null 2>&1; then
  echo "agent-claude-aws.sh: jq is required to build the MCP config; aborting" >&2
  exit 1
fi

__AWS_REGION_ARG="${SWFC_AWS_REGION:-}"
__AWS_PROFILE_ARG="${SWFC_AWS_PROFILE:-default}"
__GITHUB_TOKEN_ARG="${GITHUB_TOKEN:-}"

# Cloudwatch server env: include AWS_REGION only when the env var is set,
# matching the prior heredoc behavior (omitted key, not empty string).
__MCP_JSON="$(jq -n \
  --arg region "$__AWS_REGION_ARG" \
  --arg profile "$__AWS_PROFILE_ARG" \
  '{
    mcpServers: {
      "awslabs.cloudwatch-mcp-server": {
        command: "uvx",
        args: ["awslabs.cloudwatch-mcp-server@latest"],
        env: (
          { "AWS_PROFILE": $profile }
          + (if $region == "" then {} else { "AWS_REGION": $region } end)
        )
      }
    }
  }')"

if [[ "${SWFC_MCP_BIO:-0}" = "1" ]]; then
  __MCP_JSON="$(printf '%s' "$__MCP_JSON" | jq '
    .mcpServers += {
      "bio-mcp": {
        command: "uvx",
        args: ["bio-mcp@latest"]
      }
    }')"
fi

if [[ "${SWFC_MCP_GITHUB:-0}" = "1" ]]; then
  __MCP_JSON="$(printf '%s' "$__MCP_JSON" | jq \
    --arg token "$__GITHUB_TOKEN_ARG" \
    '.mcpServers += {
      "github": {
        command: "uvx",
        args: ["mcp-server-github@latest"],
        env: { "GITHUB_PERSONAL_ACCESS_TOKEN": $token }
      }
    }')"
fi

if [[ "${SWFC_MCP_AWS_AGENT_REGISTRY:-0}" = "1" ]]; then
  __MCP_JSON="$(printf '%s' "$__MCP_JSON" | jq \
    --arg region "$__AWS_REGION_ARG" \
    --arg profile "$__AWS_PROFILE_ARG" \
    '.mcpServers += {
      "aws-agent-registry": {
        command: "uvx",
        args: ["awslabs.agent-registry-mcp-server@latest"],
        env: (
          { "AWS_PROFILE": $profile }
          + (if $region == "" then {} else { "AWS_REGION": $region } end)
        )
      }
    }')"
fi

printf '%s\n' "$__MCP_JSON" > "$MCP_CONFIG"
unset __AWS_REGION_ARG __AWS_PROFILE_ARG __GITHUB_TOKEN_ARG __MCP_JSON

# Memory-discipline policy surface (mirror of agent-claude.sh). See
# that script for the full rationale. On AWS the cap is typically
# relaxed (big-mem instances), but the guidance still applies because
# even a 256 GB instance can OOM on a 500k-cell dense merge.
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
# scripts/agent-prompts/task-execution.md.
TASK_EXECUTION_BODY="$(load_task_execution_prompt "$SCRIPT_DIR/agent-prompts/task-execution.md")"

PROMPT="$(cat "$PACKAGE/PROMPT.md")${MEMORY_DISCIPLINE_BLOCK}

## Package location
All paths are relative to: $PACKAGE

## Remote execution context
This task runs on AWS EC2. Use the awslabs.cloudwatch-mcp-server
MCP to query instance metrics when you need to understand whether a
stage is I/O-, CPU-, or memory-bound. Metric queries should scope
by the InstanceId dimension = \"${SWFC_AWS_INSTANCE_ID:-<unset>}\".

${TASK_EXECUTION_BODY}"

# Per-task container resolution. WORKFLOW.json's
# tasks.<task_id>.container is the reproducibility-bearing source of
# truth (S15.2). Falls back to policies/container.json::image (legacy
# package-level default), then host-env (no container declared).
# ANTHROPIC_API_KEY + AWS credentials flow through as env.
CONTAINER_IMAGE=""
WORKFLOW_JSON="$PACKAGE/WORKFLOW.json"
if [ -n "${SWFC_TASK_ID:-}" ] \
   && [ -f "$WORKFLOW_JSON" ] \
   && command -v jq >/dev/null 2>&1; then
  TASK_CONTAINER="$(jq -r --arg tid "$SWFC_TASK_ID" \
    '.tasks[$tid].container // empty | tojson' "$WORKFLOW_JSON" 2>/dev/null || true)"
  if [ -n "$TASK_CONTAINER" ] && [ "$TASK_CONTAINER" != "null" ]; then
    TC_IMAGE="$(printf '%s' "$TASK_CONTAINER" | jq -r '.image // empty')"
    TC_TAG="$(printf '%s' "$TASK_CONTAINER" | jq -r '.tag // empty')"
    TC_DIGEST="$(printf '%s' "$TASK_CONTAINER" | jq -r '.digest // empty')"
    # `gpu_required` flips on the GPU passthrough flag
    # for AWS GPU instances (g6/g6e/p4d/p5/p5e). Non-GPU instance
    # types ignore `--gpus` cleanly so guarding here keeps the
    # agent-script flow uniform across instance classes.
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
  # Single-source from WORKFLOW.json so docker `--gpus device=…`
  # honors the same atom-level resource_profile.gpu the SLURM /
  # local paths consume.
  TASK_GPU_KIND="$(jq -r --arg tid "$SWFC_TASK_ID" \
    '.tasks[$tid].resource_profile.gpu.kind // empty' "$WORKFLOW_JSON" 2>/dev/null || true)"
  TASK_GPU_COUNT="$(jq -r --arg tid "$SWFC_TASK_ID" \
    '.tasks[$tid].resource_profile.gpu.count // 0' "$WORKFLOW_JSON" 2>/dev/null || true)"
  TASK_GPU_MIG_PROFILE="$(jq -r --arg tid "$SWFC_TASK_ID" \
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

# Policy-open log. Mirror of agent-claude.sh's helper. Captures stdout
# to a tempfile, scans for `policies/<name>.json` strings, and appends
# one JSONL record per matched policy to `runtime/policy-opens.jsonl`
# so the orphan-policy sweep can disposition orphans on the AWS path
# the same way as the local path.
OUT_LOG="$(mktemp -t agent-claude-aws.XXXXXX.log)"
trap 'rm -f "$OUT_LOG" "$MCP_CONFIG"' EXIT

if [ -n "${SWFC_TASK_ID:-}" ]; then
  SCRATCH_BASE="${SWFC_AGENT_SCRATCH_DIR:-$PACKAGE/runtime/scratch}"
  SCRATCH_DIR="$SCRATCH_BASE/$SWFC_TASK_ID"
  mkdir -p "$SCRATCH_DIR" 2>/dev/null || true
  export SWFC_TASK_SCRATCH_DIR="$SCRATCH_DIR"
fi

if [ "${SWFC_AGENT_CACHE_DISABLE:-0}" != "1" ] && [ -n "${SWFC_CHAT_SESSION_ID:-}" ]; then
  CACHE_BASE="${SWFC_AGENT_CACHE_DIR:-$HOME/.scripps-workflow/agent-cache}"
  CACHE_DIR="$CACHE_BASE/$SWFC_CHAT_SESSION_ID"
  mkdir -p "$CACHE_DIR/pip" "$CACHE_DIR/conda" "$CACHE_DIR/apt" "$CACHE_DIR/R-libs" "$CACHE_DIR/python" 2>/dev/null || true
  export SWFC_SESSION_CACHE_DIR="$CACHE_DIR"
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
      echo "{\"ts\":\"$ts\",\"policy\":\"$name\",\"backend\":\"aws\"}" >> "$sink"
    fi
  done
}

# Agent billing: same semantics as scripts/agent-claude.sh. Default
# "subscription" uses ~/.claude/.credentials.json mounted into the
# container; "api" mode forwards the key.
if [ "${SWFC_AGENT_BILLING:-subscription}" = "subscription" ]; then
  unset ANTHROPIC_API_KEY
elif [ "${SWFC_AGENT_BILLING:-}" = "api" ]; then
  if [ -z "${ANTHROPIC_API_KEY:-}" ] && [ -n "${SWFC_ANTHROPIC_API_KEY:-}" ]; then
    # Suppress xtrace around the secret expansion
    # so the literal key never lands in any active trace log.
    { set +x; } 2>/dev/null
    export ANTHROPIC_API_KEY="$SWFC_ANTHROPIC_API_KEY"
    { set -x; } 2>/dev/null
  fi
fi

# Memory cap — mirror of agent-claude.sh. On AWS this is usually left
# unset (the big-mem instance is the whole point), but the knob is
# available for pilot-sized / t-shirt-size smaller instances where the
# agent could still OOM the runner.
AGENT_CMD_PREFIX=()
DOCKER_MEMORY_ARGS=()
AGENT_MEMORY_LIMIT_GB=""
if [ -n "${SWFC_AGENT_MEMORY_CAP_GB:-}" ]; then
  if ! [[ "$SWFC_AGENT_MEMORY_CAP_GB" =~ ^[0-9]+$ ]]; then
    echo "agent-claude-aws.sh: SWFC_AGENT_MEMORY_CAP_GB must be a positive integer (got '$SWFC_AGENT_MEMORY_CAP_GB'); ignoring." >&2
  elif command -v systemd-run >/dev/null 2>&1 \
    && systemd-run --user --scope --quiet -p "MemoryMax=100M" /bin/true >/dev/null 2>&1; then
    AGENT_MEMORY_LIMIT_GB="$SWFC_AGENT_MEMORY_CAP_GB"
    # Pair MemoryMax with MemoryHigh = 0.85 * MemoryMax
    # so the cgroup throttles before OOM-kill (PostgreSQL pattern).
    MEM_MAX_MB=$((SWFC_AGENT_MEMORY_CAP_GB * 1024))
    MEM_HIGH_MB=$((MEM_MAX_MB * SWFC_AGENT_MEMORY_HIGH_WATER_PCT / 100))
    AGENT_CMD_PREFIX=(systemd-run --user --scope --quiet \
      -p "MemoryMax=${SWFC_AGENT_MEMORY_CAP_GB}G" \
      -p "MemoryHigh=${MEM_HIGH_MB}M")
  elif command -v prlimit >/dev/null 2>&1; then
    AGENT_MEMORY_LIMIT_GB="$SWFC_AGENT_MEMORY_CAP_GB"
    CAP_BYTES=$((SWFC_AGENT_MEMORY_CAP_GB * 1024 * 1024 * 1024))
    AGENT_CMD_PREFIX=(prlimit "--as=$CAP_BYTES")
  else
    AGENT_MEMORY_LIMIT_GB="$SWFC_AGENT_MEMORY_CAP_GB"
  fi
fi
if [ -n "$AGENT_MEMORY_LIMIT_GB" ]; then
  # Docker --memory-reservation mirrors MemoryHigh.
  DOCKER_MEMORY_RESERVATION_MB=$((AGENT_MEMORY_LIMIT_GB * 1024 * 85 / 100))
  DOCKER_MEMORY_ARGS=(
    "--memory=${AGENT_MEMORY_LIMIT_GB}g"
    "--memory-reservation=${DOCKER_MEMORY_RESERVATION_MB}m"
  )
fi

# Capture start time before the CLI invocation for agent-code.json.
AGENT_STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

if [ -n "$CONTAINER_IMAGE" ] && command -v docker >/dev/null 2>&1; then
  # `docker run` does NOT inherit env from the parent shell. Forward
  # the BLAS thread vars + SWFC_HW_* envelope explicitly so Rscript /
  # python inside the container sees the same threading view the host
  # path does. _AGENT_ENV_FORWARD_PAIRS comes from the bootstrap.

  # Registry auth.
  #
  # Case 1: ECR image. The instance profile is expected to carry
  # `ecr:GetAuthorizationToken` + `ecr:BatchGetImage`. We probe via
  # `aws sts get-caller-identity`; success → run `aws ecr
  # get-login-password` and pipe to `docker login`. Failure (no IAM
  # role, no creds) → log and skip; the docker pull may still succeed
  # on a public ECR or fall back to the union image.
  #
  # Case 2: Non-ECR image with SWFC_CONTAINER_REGISTRY_AUTH set as
  # `registry|user|pass`. Login when the registry hostname matches
  # the image hostname.
  __agent_aws_img_no_scheme="${CONTAINER_IMAGE#docker://}"
  case "$__agent_aws_img_no_scheme" in
    */*) __agent_aws_img_host="${__agent_aws_img_no_scheme%%/*}" ;;
    *)   __agent_aws_img_host="" ;;
  esac
  if [[ "$__agent_aws_img_host" =~ ^[0-9]+\.dkr\.ecr\.[a-z0-9-]+\.amazonaws\.com$ ]] \
     && command -v aws >/dev/null 2>&1; then
    __agent_aws_region="${__agent_aws_img_host##*.ecr.}"
    __agent_aws_region="${__agent_aws_region%%.amazonaws.com}"
    if aws sts get-caller-identity >/dev/null 2>&1; then
      { set +x; } 2>/dev/null
      if ! aws ecr get-login-password --region "$__agent_aws_region" 2>/dev/null \
            | docker login --username AWS --password-stdin "$__agent_aws_img_host" >/dev/null 2>&1; then
        echo "  [agent-aws] warn: ECR login failed for $__agent_aws_img_host; continuing with anonymous pull" >&2
      fi
      { set -x; } 2>/dev/null
    else
      echo "  [agent-aws] info: ECR image $__agent_aws_img_host but no AWS credentials (sts get-caller-identity failed); skipping login" >&2
    fi
    unset __agent_aws_region
  elif [ -n "${SWFC_CONTAINER_REGISTRY_AUTH:-}" ] && [ -n "$__agent_aws_img_host" ]; then
    __agent_aws_cfg_host="${SWFC_CONTAINER_REGISTRY_AUTH%%|*}"
    __agent_aws_cfg_rest="${SWFC_CONTAINER_REGISTRY_AUTH#*|}"
    __agent_aws_cfg_user="${__agent_aws_cfg_rest%%|*}"
    __agent_aws_cfg_pass="${__agent_aws_cfg_rest#*|}"
    if [ -n "$__agent_aws_cfg_host" ] && [ -n "$__agent_aws_cfg_user" ] \
       && [ "$__agent_aws_cfg_pass" != "$__agent_aws_cfg_rest" ] \
       && [ "$__agent_aws_cfg_host" = "$__agent_aws_img_host" ]; then
      { set +x; } 2>/dev/null
      if ! printf '%s' "$__agent_aws_cfg_pass" \
            | docker login --username "$__agent_aws_cfg_user" --password-stdin \
              "$__agent_aws_cfg_host" >/dev/null 2>&1; then
        echo "  [agent-aws] warn: docker login failed for $__agent_aws_cfg_host; continuing with anonymous pull" >&2
      fi
      { set -x; } 2>/dev/null
    fi
    unset __agent_aws_cfg_host __agent_aws_cfg_rest __agent_aws_cfg_user __agent_aws_cfg_pass
  fi
  unset __agent_aws_img_no_scheme __agent_aws_img_host

  docker pull "$CONTAINER_IMAGE" >/dev/null 2>&1 || true
  __agent_build_env_forward_pairs
  DOCKER_ENV_ARGS=()
  for __agent_kv in "${_AGENT_ENV_FORWARD_PAIRS[@]}"; do
    DOCKER_ENV_ARGS+=(-e "$__agent_kv")
  done
  unset __agent_kv
  if [ "${SWFC_AGENT_BILLING:-subscription}" = "api" ] \
     && [ -n "${ANTHROPIC_API_KEY:-}" ]; then
    # Suppress xtrace around docker -e env-arg
    # composition so the literal key isn't echoed to any trace log.
    { set +x; } 2>/dev/null
    DOCKER_ENV_ARGS+=(-e "ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY")
    { set -x; } 2>/dev/null
  fi

  # GPU passthrough on AWS GPU instances. The instance
  # type was selected with the right gres advertisement at
  # provisioning time (`AwsExecutor::select_instance_type`); here we
  # turn the flag on so the docker invocation sees the GPU the
  # instance carries. MIG profile maps to a slice device id; without
  # MIG we hand all visible GPUs to the container.
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
  __agent_container_cpus="${SWFC_HW_NPROC_HINT:-${SWFC_HW_VCPUS_AVAILABLE:-}}"
  if [[ "$__agent_container_cpus" =~ ^[0-9]+$ ]] && [ "$__agent_container_cpus" -gt 0 ]; then
    DOCKER_CPU_ARGS+=(--cpus "$__agent_container_cpus")
  fi
  unset __agent_container_cpus

  DOCKER_CACHE_ARGS=()
  if [ -n "${SWFC_SESSION_CACHE_DIR:-}" ]; then
    DOCKER_CACHE_ARGS+=(
      -v "$SWFC_SESSION_CACHE_DIR":"$SWFC_SESSION_CACHE_DIR":rw
      -e "SWFC_SESSION_CACHE_DIR=$SWFC_SESSION_CACHE_DIR"
      -e "R_LIBS_USER=$SWFC_SESSION_CACHE_DIR/R-libs"
      -e "PIP_CACHE_DIR=$SWFC_SESSION_CACHE_DIR/pip"
      -e "CONDA_PKGS_DIRS=$SWFC_SESSION_CACHE_DIR/conda"
      -e "PYTHONUSERBASE=$SWFC_SESSION_CACHE_DIR/python"
      -e "PIP_USER=1"
      -e "PIP_BREAK_SYSTEM_PACKAGES=1"
    )
  fi

  DOCKER_SCRATCH_ARGS=()
  if [ -n "${SWFC_TASK_SCRATCH_DIR:-}" ]; then
    mkdir -p "$SWFC_TASK_SCRATCH_DIR" 2>/dev/null || true
    case "$SWFC_TASK_SCRATCH_DIR" in
      "$PACKAGE"/*) ;;
      *) DOCKER_SCRATCH_ARGS+=(-v "$SWFC_TASK_SCRATCH_DIR":"$SWFC_TASK_SCRATCH_DIR":rw) ;;
    esac
    DOCKER_SCRATCH_ARGS+=(-e "SWFC_TASK_SCRATCH_DIR=$SWFC_TASK_SCRATCH_DIR")
  fi

  # Label the container so the AWS orphan reaper can
  # probe `docker ps --filter label=swfc-task=<id>` against the
  # instance via SSM RunShellScript and tell apart a live container
  # from a hung one (instance still alive but container exited).
  DOCKER_LABEL_ARGS=()
  if [ -n "${SWFC_TASK_ID:-}" ]; then
    DOCKER_LABEL_ARGS+=("--label" "swfc-task=${SWFC_TASK_ID}")
  fi
  if [ -n "${SWFC_CHAT_SESSION_ID:-}" ]; then
    DOCKER_LABEL_ARGS+=("--label" "swfc-session=${SWFC_CHAT_SESSION_ID}")
  fi

  # Docker isolation hardening. The bind-mounted $PACKAGE stays
  # RW so the agent can write outputs; the rootfs is read-only
  # with tmpfs over /tmp and /var/tmp. All caps dropped,
  # no-new-privileges blocks suid escalation, --pids-limit caps
  # fork-bombs.
  #
  # Drop to the host UID/GID inside the container. Without this
  # the AWS agent runs as root in the container with /root
  # inheriting any side effects of an exploited tool.
  docker run --rm \
    --read-only \
    --tmpfs "/tmp:rw,size=$SWFC_DOCKER_TMPFS_TMP_SIZE,mode=1777" \
    --tmpfs "/var/tmp:rw,size=$SWFC_DOCKER_TMPFS_VARTMP_SIZE,mode=1777" \
    --security-opt no-new-privileges \
    --cap-drop=ALL \
    --pids-limit "$SWFC_DOCKER_PIDS_LIMIT" \
    --user "$(id -u):$(id -g)" \
    "${DOCKER_MEMORY_ARGS[@]}" \
    "${DOCKER_CPU_ARGS[@]}" \
    "${DOCKER_GPU_ARGS[@]}" \
    "${DOCKER_LABEL_ARGS[@]}" \
    -v "$PACKAGE":"$PACKAGE":rw \
    "${DOCKER_CACHE_ARGS[@]}" \
    "${DOCKER_SCRATCH_ARGS[@]}" \
    -v "$MCP_CONFIG":"$MCP_CONFIG":ro \
    -v "$HOME/.claude":"$HOME/.claude":ro \
    -w "$PACKAGE" \
    -e "HOME=$HOME" \
    -e AWS_REGION="${SWFC_AWS_REGION:-}" \
    -e AWS_PROFILE="${SWFC_AWS_PROFILE:-default}" \
    -e AWS_DEFAULT_REGION="${SWFC_AWS_REGION:-${AWS_DEFAULT_REGION:-}}" \
    "${DOCKER_ENV_ARGS[@]}" \
    "$CONTAINER_IMAGE" \
    claude --dangerously-skip-permissions --output-format=json --mcp-config "$MCP_CONFIG" -p "$PROMPT" \
    | tee "$OUT_LOG"
  CLAUDE_EXIT="${PIPESTATUS[0]}"

  # Container-state sidecar so the AWS-side reaper
  # picks up the exit code even if the container has been removed by
  # `--rm` before the harness probes via SSM.
  if [ -n "${SWFC_TASK_ID:-}" ]; then
    CONTAINER_STATE_DIR="$PACKAGE/runtime/outputs/$SWFC_TASK_ID"
    mkdir -p "$CONTAINER_STATE_DIR" 2>/dev/null || true
    cat > "$CONTAINER_STATE_DIR/.container-state.json" 2>/dev/null <<EOF || true
{
  "exit_code": $CLAUDE_EXIT,
  "image": "${CONTAINER_IMAGE:-}",
  "runtime": "docker",
  "session_id": "${SWFC_CHAT_SESSION_ID:-}",
  "task_id": "${SWFC_TASK_ID}",
  "backend": "aws",
  "ended_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
  fi
else
  "${AGENT_CMD_PREFIX[@]}" npx @anthropic-ai/claude-code \
    --dangerously-skip-permissions \
    --output-format=json \
    --mcp-config "$MCP_CONFIG" \
    -p "$PROMPT" \
    | tee "$OUT_LOG"
  CLAUDE_EXIT="${PIPESTATUS[0]}"
fi

log_policy_opens "$OUT_LOG"

# Agent-usage sidecar (mirror of scripts/agent-claude.sh). The AWS
# variant previously emitted no sidecar at all, so AWS-executor runs
# had zero `agent_cost_usd` rollup on the Performance tab. With
# --output-format=json above, the CLI's terminal result object carries
# .modelUsage (per-model token + cost breakdown) and.total_cost_usd,
# from which we emit the same shape scripps-workflow-conversation's
# AgentUsageWire expects. See the comment block in scripts/agent-claude.sh
# For the detail on why we read from modelUsage (not top-level.model)
# and strip the `[1m]` context-variant suffix.
TASK_DIR="$(ls -dt "$PACKAGE/runtime/outputs/"*/ 2>/dev/null | head -1 || true)"
if [ -n "$TASK_DIR" ] && command -v jq >/dev/null 2>&1; then
  LAST_JSON="$(grep -E '^\{' "$OUT_LOG" | tail -1 || true)"
  if [ -n "$LAST_JSON" ]; then
    echo "$LAST_JSON" | jq -c '
      . as $r
      | (($r.modelUsage // {}) | to_entries) as $models
      | if ($models | length) == 0 then empty
        else
          ($models | max_by(.value.costUSD // 0)) as $top
          | {
              model: ($top.key | sub("\\[[^\\]]*\\]$"; "")),
              input_tokens: ($top.value.inputTokens // 0),
              output_tokens: ($top.value.outputTokens // 0),
              cache_read_tokens: ($top.value.cacheReadInputTokens // 0),
              cache_creation_tokens: ($top.value.cacheCreationInputTokens // 0),
              total_cost_usd: ($r.total_cost_usd // ($top.value.costUSD // 0))
            }
        end
    ' > "${TASK_DIR}agent-usage.json" 2>/dev/null || true
    [ -s "${TASK_DIR}agent-usage.json" ] || rm -f "${TASK_DIR}agent-usage.json"
  fi
fi

# Per-task agent-code.json sidecar (M1.2 — mirrors agent-claude.sh).
# Placed before exit so AWS-executor runs also capture code artifacts.
if [ -n "${SWFC_TASK_ID:-}" ] && command -v jq >/dev/null 2>&1; then
  _AGENT_CODE_COMPLETED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  _AGENT_CODE_OUT_DIR="$PACKAGE/runtime/outputs/$SWFC_TASK_ID"
  mkdir -p "$_AGENT_CODE_OUT_DIR" 2>/dev/null || true
  _EXECUTED_CODE=""
  if [ -f "$OUT_LOG" ]; then
    _EXECUTED_CODE="$(awk '
      /^```/ {
        if (in_block) { in_block=0; found=1 }
        else { in_block=1; buf="" }
        next
      }
      in_block { buf = buf $0 "\n" }
      END { if (found || in_block) printf "%s", buf }
    ' "$OUT_LOG" 2>/dev/null | tail -c 65536 || true)"
    if [ -z "$_EXECUTED_CODE" ]; then
      _EXECUTED_CODE="$(grep -n '^\s*#!' "$OUT_LOG" 2>/dev/null \
        | tail -1 | cut -d: -f1 \
        | xargs -I{} sh -c 'tail -n +{} "$1" | head -200' -- "$OUT_LOG" 2>/dev/null \
        | head -c 65536 || true)"
    fi
  fi
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
  _AC_TMP="${_AGENT_CODE_OUT_DIR}/agent-code.json.tmp"
  {
    printf '%s' "$PROMPT" | jq -Rs \
      --arg rt "" \
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

exit "$CLAUDE_EXIT"
