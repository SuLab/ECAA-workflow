# shellcheck shell=bash
# Shared defaults for the three agent wrappers (local / aws / slurm).
# Sourced near the top of each wrapper. Env vars override every value.
#
# Safe-shell guard: no-op when sourced (flags inherit from caller); enables
# strict mode when executed directly. The [[ source == "$0" ]] test is false
# under `source` / `.` (BASH_SOURCE[0] is the file path, $0 is the parent
# shell name); true only when bash is invoked with this script as argv[1].
[[ "${BASH_SOURCE[0]}" == "${0}" ]] && set -euo pipefail

# --- Memory ---

# Percent of container memory budget reserved as soft high-water mark.
# Above this, the systemd-run / docker cgroup pressures jobs into reclaim
# before OOM-killing. 85% gives ~15% headroom for OS + system daemons.
: "${SWFC_AGENT_MEMORY_HIGH_WATER_PCT:=85}"

# --- Docker tmpfs sizes ---

# /tmp tmpfs size. 1g covers typical intermediate-file usage; raise for
# stages with large in-memory pivots.
: "${SWFC_DOCKER_TMPFS_TMP_SIZE:=1g}"

# /var/tmp tmpfs size. Smaller default for fewer-but-larger temps.
: "${SWFC_DOCKER_TMPFS_VARTMP_SIZE:=512m}"

# --- Docker security ---

# Fork-bomb fence. 2048 PIDs is generous for parallel batch jobs without
# blocking common multi-threaded workloads (STAR, salmon).
: "${SWFC_DOCKER_PIDS_LIMIT:=2048}"

# --- Heartbeat ---

# Heartbeat touch interval. UI tails progress.log every 2 s but the agent
# only touches its heartbeat every 30 s — covers transient I/O stalls
# without false stall-detection signals.
: "${SWFC_HEARTBEAT_INTERVAL_SECS:=30}"

# --- Credential refresh ---

# Cycle for the credential-rotation copy loop.
: "${SWFC_AGENT_CRED_REFRESH_SECS:=15}"

# Grace period before clobbering a freshly-rotated credential file.
: "${SWFC_AGENT_CRED_ROTATION_GRACE_SECS:=2}"

# --- Shared helpers ---
# Single source of truth for the agent-wrapper helpers; the three
# wrappers (local / aws / slurm) inherit via `source`.

# Security remediation validate
# SWFC_CHAT_SESSION_ID is a syntactically-correct UUID before any code
# interpolates it into a docker label, cache path, or per-session log
# location. A malformed value (e.g. shell metacharacters, path
# traversal) would otherwise reach `--label swfc-session=$ID`,
# `CACHE_DIR=$CACHE_BASE/$ID`, or the agent-usage JSON body. Exit 98
# is reserved for this specific failure so the harness's stderr_tail
# surfaces a stable, greppable signal.
validate_uuid() {
    local v="$1"
    if [[ ! "$v" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]]; then
        echo "FATAL: SWFC_CHAT_SESSION_ID is not a valid UUIDv4: $v" >&2
        exit 98
    fi
}

# Validate SWFC_TASK_ID before it interpolates into per-task paths
# (runtime/outputs/$SWFC_TASK_ID/, scratch dirs, docker labels). Without
# validation a hostile id like `../../etc` or `x;rm -rf /` lands
# directly in `mkdir -p $PACKAGE/runtime/outputs/$SWFC_TASK_ID` and any
# subsequent `cat > $TASK_DIR/...` heredoc. Same `^[A-Za-z0-9_.-]+$`
# shape the Rust `_id_validator::is_safe_id` enforces on the harness
# side. Refuses `..`, `/`, leading `.`, NUL, length > 128. Exit 99
# keeps it greppable in harness stderr_tail (distinct from
# validate_uuid's 98).
validate_task_id() {
    local v="$1"
    if [ -z "$v" ]; then
        echo "FATAL: SWFC_TASK_ID is empty" >&2
        exit 99
    fi
    if [ "${#v}" -gt 128 ]; then
        echo "FATAL: SWFC_TASK_ID exceeds 128 chars: ${v:0:64}..." >&2
        exit 99
    fi
    case "$v" in
        .*|*..*|*/*|*\\*)
            echo "FATAL: SWFC_TASK_ID contains path-traversal chars: $v" >&2
            exit 99
            ;;
    esac
    if ! [[ "$v" =~ ^[A-Za-z0-9_.-]+$ ]]; then
        echo "FATAL: SWFC_TASK_ID outside ^[A-Za-z0-9_.-]+$ shape: $v" >&2
        exit 99
    fi
}

# Run a command with xtrace temporarily disabled.
# Used to hide secret expansions (ANTHROPIC_API_KEY, HF_TOKEN, etc.)
# from agent-trace.log when SWFC_AGENT_DEBUG=1 enables `set -x`. The
# `2>/dev/null` swallows the trace line from `set +x` itself.
run_no_xtrace() {
    { set +x; } 2>/dev/null
    "$@"
    local rc=$?
    { set -x; } 2>/dev/null
    return $rc
}

# Load the shared task-execution prompt body and expand runtime placeholders
# to the caller's current values. Used by all three agent wrappers (local /
# aws / slurm) so the patch-merge envelope, blocker-kind vocabulary,
# discovery-stage block-by-default rule, and iterate-until / figures /
# progress contracts cannot drift between backends — a single change to
# scripts/agent-prompts/task-execution.md propagates to every executor.
# Echoes the rendered body to stdout for command-substitution into
# PROMPT="..." assemblies.
#
# Args:
#   $1 — absolute path to scripts/agent-prompts/task-execution.md
#
# Required env at call time: PACKAGE, SWFC_TASK_ID. MAX_TURNS_PER_TASK is
# optional and defaults to 40. Other placeholders in the file (e.g.
# <SWFC_HARNESS_RUN_ID>, <task_id>) are intentionally literal — the agent
# receives them as-is for runtime substitution.
load_task_execution_prompt() {
    local prompt_path="$1"
    if [ ! -f "$prompt_path" ]; then
        echo "FATAL: shared task-execution prompt missing: $prompt_path" >&2
        exit 97
    fi
    local max_turns="${MAX_TURNS_PER_TASK:-40}"
    if ! [[ "$max_turns" =~ ^[0-9]+$ ]] || [ "$max_turns" -le 0 ]; then
        max_turns=40
    fi
    local soft_turns=$((max_turns * 4 / 5))
    if [ "$soft_turns" -ge "$max_turns" ]; then
        soft_turns=$((max_turns - 1))
    fi
    if [ "$soft_turns" -lt 1 ]; then
        soft_turns=1
    fi
    local body
    body="$(cat "$prompt_path")"
    body="${body//\$PACKAGE/$PACKAGE}"
    body="${body//\$SWFC_TASK_ID/${SWFC_TASK_ID:-}}"
    body="${body//\{\{MAX_TURNS_PER_TASK\}\}/$max_turns}"
    body="${body//\{\{SOFT_TURNS_PER_TASK\}\}/$soft_turns}"
    printf '%s' "$body"
}

# Return success when Claude Code's terminal JSON says the run completed
# successfully. This is intentionally stricter than "last line is JSON":
# callers use it only to reconcile contradictory CLI exit statuses.
claude_terminal_result_succeeded() {
    local out_log="$1"
    if [ ! -f "$out_log" ] || ! command -v jq >/dev/null 2>&1; then
        return 1
    fi
    local last_json
    last_json="$(grep -E '^\{' "$out_log" 2>/dev/null | tail -1 || true)"
    if [ -z "$last_json" ]; then
        return 1
    fi
    printf '%s\n' "$last_json" | jq -e '
      (.type // "") == "result"
      and ((.is_error // false) == false)
      and (((.subtype // "") == "success") or ((.terminal_reason // "") == "completed"))
    ' >/dev/null 2>&1
}

# Return success when Claude Code's terminal JSON is a transient transport
# failure from the CLI/API connection rather than an agent-authored task
# failure. Callers use this to retry the same task once; do not classify
# deterministic analysis or validation errors here.
claude_terminal_result_transient_error() {
    local out_log="$1"
    if [ ! -f "$out_log" ] || ! command -v jq >/dev/null 2>&1; then
        return 1
    fi
    local last_json
    last_json="$(grep -E '^\{' "$out_log" 2>/dev/null | tail -1 || true)"
    if [ -z "$last_json" ]; then
        return 1
    fi
    printf '%s\n' "$last_json" | jq -e '
      (.type // "") == "result"
      and ((.is_error // false) == true)
      and (
        (.result // "") | test(
          "socket connection was closed unexpectedly|connection reset|ECONNRESET|ETIMEDOUT|fetch failed|network error|timed out|temporarily unavailable|502|503|504";
          "i"
        )
      )
    ' >/dev/null 2>&1
}

# Claude Code has emitted a terminal success JSON while returning a non-zero
# process status in some live runs. Normalize only when the agent also wrote
# a parseable state.patch.json for its dispatched task; otherwise keep the
# non-zero status so missing-patch failures stay visible to the harness.
normalize_claude_exit_status() {
    local exit_code="$1"
    local out_log="$2"
    local package="$3"
    local task_id="${4:-}"
    if [ "$exit_code" = "0" ]; then
        printf '0\n'
        return 0
    fi
    if [ -z "$task_id" ]; then
        printf '%s\n' "$exit_code"
        return 0
    fi
    local patch_path="$package/runtime/outputs/$task_id/state.patch.json"
    if [ ! -s "$patch_path" ]; then
        printf '%s\n' "$exit_code"
        return 0
    fi
    if command -v jq >/dev/null 2>&1 && ! jq -e . "$patch_path" >/dev/null 2>&1; then
        printf '%s\n' "$exit_code"
        return 0
    fi
    if claude_terminal_result_succeeded "$out_log"; then
        printf '0\n'
    else
        printf '%s\n' "$exit_code"
    fi
}

# Convert the Claude Code terminal result JSON from stdin into the harness
# agent-usage sidecar shape. The turn count is part of cost discipline: the
# wrapper enforces MAX_TURNS_PER_TASK from this sidecar after the CLI exits.
agent_usage_json_from_claude_result() {
    jq -c '
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
              total_cost_usd: ($r.total_cost_usd // ($top.value.costUSD // 0)),
              num_turns: ($r.num_turns // 0)
            }
        end
    '
}

# Enforce the per-task Claude Code turn cap after agent-usage.json has been
# written. The harness advances tasks from state.patch.json, so budget
# enforcement must update that patch as well as the human-facing result.json.
enforce_turn_budget_limit() {
    local package="$1"
    local task_id="$2"
    local max_turns="$3"

    if [ -z "$task_id" ] || ! command -v jq >/dev/null 2>&1; then
        return 0
    fi
    if ! [[ "$max_turns" =~ ^[0-9]+$ ]] || [ "$max_turns" -le 0 ]; then
        max_turns=40
    fi

    local task_dir="$package/runtime/outputs/$task_id"
    local usage_file="$task_dir/agent-usage.json"
    if [ ! -f "$usage_file" ]; then
        return 0
    fi

    local num_turns
    num_turns="$(jq -r '.num_turns // 0' "$usage_file" 2>/dev/null || echo 0)"
    if ! [ "$num_turns" -gt "$max_turns" ] 2>/dev/null; then
        return 0
    fi

    # If the agent self-reported a completed state (and the harness will
    # accept that), trust the agent's outputs instead of overwriting them
    # with a blocked patch. The turn cap is a safety net against runaway
    # tasks, not a quality gate over successfully-finished work.
    local patch_file="$task_dir/state.patch.json"
    if [ -f "$patch_file" ]; then
        local existing_status
        existing_status="$(jq -r '.to.status // ""' "$patch_file" 2>/dev/null || echo "")"
        if [ "$existing_status" = "completed" ]; then
            echo "[turn-budget] task $task_id ran $num_turns turns (cap $max_turns) but self-reported completed; respecting agent state.patch.json" >&2
            return 0
        fi
    fi

    local reason="Task ran ${num_turns} turns; cap is ${max_turns}. Inspect agent-claude.log."
    mkdir -p "$task_dir" 2>/dev/null || true

    local tmp_result
    tmp_result="$(mktemp)"
    jq -n --arg tid "$task_id" --arg reason "$reason" \
      '{
         task_id: $tid,
         status: "blocked",
         blocker_kind: "TurnBudgetExceeded",
         rationale: $reason,
         claims: [],
         figures: []
       }' > "$tmp_result"
    mv "$tmp_result" "$task_dir/result.json"

    local epoch="${SWFC_DISPATCH_EPOCH:-}"
    if ! [[ "$epoch" =~ ^[0-9]+$ ]]; then
        epoch=""
    fi

    local tmp_patch
    tmp_patch="$(mktemp)"
    jq -n \
      --arg reason "$reason" \
      --arg run_id "${SWFC_HARNESS_RUN_ID:-}" \
      --arg epoch "$epoch" \
      '{
         from: "running",
         to: {
           status: "blocked",
           record: {
             reason: $reason,
             attempts: [
               {
                 method: "turn budget enforcement",
                 result: $reason
               }
             ]
           }
         }
       }
       | if $run_id != "" then . + {harness_run_id: $run_id} else . end
       | if $epoch != "" then . + {dispatch_epoch: ($epoch | tonumber)} else . end' \
      > "$tmp_patch"
    mv "$tmp_patch" "$task_dir/state.patch.json"
    echo "[turn-budget] task $task_id exceeded cap ($num_turns > $max_turns); wrote blocked state.patch.json" >&2
}
