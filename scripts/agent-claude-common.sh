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
: "${ECAA_AGENT_MEMORY_HIGH_WATER_PCT:=85}"

# --- Docker tmpfs sizes ---

# /tmp tmpfs size. 1g covers typical intermediate-file usage; raise for
# stages with large in-memory pivots.
: "${ECAA_DOCKER_TMPFS_TMP_SIZE:=1g}"

# /var/tmp tmpfs size. Smaller default for fewer-but-larger temps.
: "${ECAA_DOCKER_TMPFS_VARTMP_SIZE:=512m}"

# --- Docker security ---

# Fork-bomb fence. 2048 PIDs is generous for parallel batch jobs without
# blocking common multi-threaded workloads (STAR, salmon).
: "${ECAA_DOCKER_PIDS_LIMIT:=2048}"

# --- Heartbeat ---

# Heartbeat touch interval. UI tails progress.log every 2 s but the agent
# only touches its heartbeat every 30 s — covers transient I/O stalls
# without false stall-detection signals.
: "${ECAA_HEARTBEAT_INTERVAL_SECS:=30}"

# --- Credential refresh ---

# Cycle for the credential-rotation copy loop.
: "${ECAA_AGENT_CRED_REFRESH_SECS:=15}"

# Grace period before clobbering a freshly-rotated credential file.
: "${ECAA_AGENT_CRED_ROTATION_GRACE_SECS:=2}"

# --- Shared helpers ---
# Single source of truth for the agent-wrapper helpers; the three
# wrappers (local / aws / slurm) inherit via `source`.

# Security remediation validate
# ECAA_CHAT_SESSION_ID is a syntactically-correct UUID before any code
# interpolates it into a docker label, cache path, or per-session log
# location. A malformed value (e.g. shell metacharacters, path
# traversal) would otherwise reach `--label ecaa-session=$ID`,
# `CACHE_DIR=$CACHE_BASE/$ID`, or the agent-usage JSON body. Exit 98
# is reserved for this specific failure so the harness's stderr_tail
# surfaces a stable, greppable signal.
validate_uuid() {
    local v="$1"
    if [[ ! "$v" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]]; then
        echo "FATAL: ECAA_CHAT_SESSION_ID is not a valid UUIDv4: $v" >&2
        exit 98
    fi
}

# Validate ECAA_TASK_ID before it interpolates into per-task paths
# (runtime/outputs/$ECAA_TASK_ID/, scratch dirs, docker labels). Without
# validation a hostile id like `../../etc` or `x;rm -rf /` lands
# directly in `mkdir -p $PACKAGE/runtime/outputs/$ECAA_TASK_ID` and any
# subsequent `cat > $TASK_DIR/...` heredoc. Same `^[A-Za-z0-9_.-]+$`
# shape the Rust `_id_validator::is_safe_id` enforces on the harness
# side. Refuses `..`, `/`, leading `.`, NUL, length > 128. Exit 99
# keeps it greppable in harness stderr_tail (distinct from
# validate_uuid's 98).
validate_task_id() {
    local v="$1"
    if [ -z "$v" ]; then
        echo "FATAL: ECAA_TASK_ID is empty" >&2
        exit 99
    fi
    if [ "${#v}" -gt 128 ]; then
        echo "FATAL: ECAA_TASK_ID exceeds 128 chars: ${v:0:64}..." >&2
        exit 99
    fi
    case "$v" in
        .*|*..*|*/*|*\\*)
            echo "FATAL: ECAA_TASK_ID contains path-traversal chars: $v" >&2
            exit 99
            ;;
    esac
    if ! [[ "$v" =~ ^[A-Za-z0-9_.-]+$ ]]; then
        echo "FATAL: ECAA_TASK_ID outside ^[A-Za-z0-9_.-]+$ shape: $v" >&2
        exit 99
    fi
}

# Run a command with xtrace temporarily disabled.
# Used to hide secret expansions (ANTHROPIC_API_KEY, HF_TOKEN, etc.)
# from agent-trace.log when ECAA_AGENT_DEBUG=1 enables `set -x`. The
# `2>/dev/null` swallows the trace line from `set +x` itself.
run_no_xtrace() {
    { set +x; } 2>/dev/null
    "$@"
    local rc=$?
    { set -x; } 2>/dev/null
    return $rc
}

# Stage helper files at a path that both the wrapper container and the host
# Docker daemon can resolve. In a container-first deployment, paths beside the
# wrapper such as /app/scripts exist only inside the server container. Passing
# one of those paths to `docker run -v` makes the host daemon create an empty
# directory and hides the intended helper in the task container.
stage_dood_helpers() {
    local script_dir="$1"
    local package="$2"
    local task_id="${3:-}"
    local helper_dir

    if [ -n "${ECAA_TASK_SCRATCH_DIR:-}" ]; then
        helper_dir="$ECAA_TASK_SCRATCH_DIR/ecaa-helpers"
    elif [ -n "$task_id" ]; then
        helper_dir="$package/runtime/outputs/$task_id/.ecaa-helpers"
    else
        helper_dir="$package/runtime/.ecaa-helpers"
    fi

    if ! mkdir -p "$helper_dir"; then
        echo "FATAL: cannot create host-visible helper directory: $helper_dir" >&2
        return 1
    fi
    if ! cp -f "$script_dir/ecaa-install" "$helper_dir/ecaa-install"; then
        echo "FATAL: cannot stage ecaa-install in host-visible storage" >&2
        return 1
    fi
    if ! cp -f "$script_dir/agent_literature_fetch.py" "$helper_dir/agent_literature_fetch.py"; then
        echo "FATAL: cannot stage agent_literature_fetch.py in host-visible storage" >&2
        return 1
    fi
    chmod 0755 "$helper_dir/ecaa-install"

    if [ ! -f "$helper_dir/ecaa-install" ] || [ ! -x "$helper_dir/ecaa-install" ]; then
        echo "FATAL: staged ecaa-install is not an executable file" >&2
        return 1
    fi
    if [ ! -f "$helper_dir/agent_literature_fetch.py" ]; then
        echo "FATAL: staged agent_literature_fetch.py is not a file" >&2
        return 1
    fi

    ECAA_INSTALL_MOUNT_SRC="$helper_dir/ecaa-install"
    LIT_FETCH_MOUNT_SRC="$helper_dir/agent_literature_fetch.py"
    export ECAA_INSTALL_MOUNT_SRC LIT_FETCH_MOUNT_SRC
}

# A prior root-run Docker bind can leave the persistent install cache owned by
# root. The task container runs as the invoking uid, so such a cache appears
# mounted and configured but every installation fails with EACCES. Repair the
# bounded cache tree through the same Docker daemon, then require every install
# directory to be writable before launching the task.
ensure_writable_session_cache() {
    local cache_dir="$1"
    local container_image="${2:-}"
    local cache_uid cache_gid needs_repair child

    cache_uid="$(id -u)"
    cache_gid="$(id -g)"
    needs_repair=0

    if ! mkdir -p "$cache_dir" 2>/dev/null; then
        needs_repair=1
    elif [ ! -O "$cache_dir" ] || [ ! -w "$cache_dir" ]; then
        needs_repair=1
    fi
    for child in pip conda conda-envs apt R-libs python helpers; do
        if [ -e "$cache_dir/$child" ] \
           && { [ ! -O "$cache_dir/$child" ] || [ ! -w "$cache_dir/$child" ]; }; then
            needs_repair=1
            break
        fi
    done

    if [ "$needs_repair" = "1" ]; then
        if [ -z "$container_image" ] || ! command -v docker >/dev/null 2>&1; then
            echo "FATAL: session cache is not writable and Docker ownership repair is unavailable: $cache_dir" >&2
            return 1
        fi
        if ! docker run --rm --user 0:0 \
            -v "$cache_dir":"$cache_dir" \
            --entrypoint chown "$container_image" \
            -R "$cache_uid:$cache_gid" "$cache_dir" >/dev/null 2>&1; then
            echo "FATAL: could not repair session cache ownership: $cache_dir" >&2
            return 1
        fi
    fi

    for child in pip conda conda-envs apt R-libs python helpers; do
        if ! mkdir -p "$cache_dir/$child" \
           || [ ! -d "$cache_dir/$child" ] \
           || [ ! -w "$cache_dir/$child" ]; then
            echo "FATAL: session cache directory is not writable: $cache_dir/$child" >&2
            return 1
        fi
    done
}

# External scratch roots are shared across packages and chat sessions. Add the
# validated session id when one is available so common task names such as
# data_acquisition cannot collide with another run's ownership or contents.
# Package-local scratch is already isolated by the package directory.
resolve_task_scratch_dir() {
    local package="$1"
    local task_id="$2"

    if [ -n "${ECAA_AGENT_SCRATCH_DIR:-}" ]; then
        if [ -n "${ECAA_CHAT_SESSION_ID:-}" ]; then
            printf '%s/%s/%s' "$ECAA_AGENT_SCRATCH_DIR" "$ECAA_CHAT_SESSION_ID" "$task_id"
        else
            printf '%s/%s' "$ECAA_AGENT_SCRATCH_DIR" "$task_id"
        fi
    else
        printf '%s/runtime/scratch/%s' "$package" "$task_id"
    fi
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
# Required env at call time: PACKAGE, ECAA_TASK_ID. MAX_TURNS_PER_TASK is
# optional and defaults to 40. Other placeholders in the file (e.g.
# <ECAA_HARNESS_RUN_ID>, <task_id>) are intentionally literal — the agent
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
    body="${body//\$ECAA_TASK_ID/${ECAA_TASK_ID:-}}"
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
          "socket connection was closed unexpectedly|connection reset|ECONNRESET|ETIMEDOUT|fetch failed|network error|timed out|temporarily unavailable|502|503|504|429|session limit|usage limit|hit your session|insufficient_quota";
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

    # The budget/turn cut can land AFTER the agent produced a passing result
    # but BEFORE it wrote the terminal state.patch.json (the harness's
    # completion signal). Don't downgrade genuinely-finished work to Blocked:
    # if result.json self-reports completed, or a validation_report.json
    # passed, synthesize the completed patch the agent didn't get to write.
    # The turn cap is a runaway-safety net, not a quality gate over finished
    # work — same trust level as the completed-state.patch branch above.
    local result_file="$task_dir/result.json"
    local vr_file="$task_dir/validation_report.json"
    local succeeded=""
    if [ -f "$result_file" ]; then
        local rstatus
        rstatus="$(jq -r '.status // ""' "$result_file" 2>/dev/null || echo "")"
        [ "$rstatus" = "completed" ] && succeeded="result.json status=completed"
    fi
    if [ -z "$succeeded" ] && [ -f "$vr_file" ]; then
        local vstatus
        vstatus="$(jq -r '(.overall_result // "") | ascii_upcase' "$vr_file" 2>/dev/null || echo "")"
        [ "$vstatus" = "PASS" ] && succeeded="validation_report.json overall_result=PASS"
    fi
    if [ -n "$succeeded" ]; then
        local epoch_ok="${ECAA_DISPATCH_EPOCH:-}"
        [[ "$epoch_ok" =~ ^[0-9]+$ ]] || epoch_ok=""
        # Only write a result.json when the agent didn't already leave a
        # completed one (e.g. a validate task that wrote only the report) —
        # never clobber the agent's own completed result.
        local cur_rstatus=""
        [ -f "$result_file" ] && cur_rstatus="$(jq -r '.status // ""' "$result_file" 2>/dev/null || echo "")"
        if [ "$cur_rstatus" != "completed" ]; then
            local tmp_r2; tmp_r2="$(mktemp)"
            jq -n --arg tid "$task_id" \
              --arg note "Completed; turn/budget cap (${num_turns}>${max_turns}) reached after a passing ${succeeded} was written." \
              '{task_id:$tid, status:"completed", rationale:$note, claims:[], figures:[]}' > "$tmp_r2"
            mv "$tmp_r2" "$result_file"
        fi
        local tmp_p2; tmp_p2="$(mktemp)"
        jq -n \
          --arg note "Completed despite turn/budget cap (${num_turns}>${max_turns}); ${succeeded}" \
          --arg run_id "${ECAA_HARNESS_RUN_ID:-}" --arg epoch "$epoch_ok" \
          '{from:"running", to:{status:"completed", result:{summary:$note, completed_despite_budget_overrun:true}}}
           | if $run_id != "" then . + {harness_run_id:$run_id} else . end
           | if $epoch != "" then . + {dispatch_epoch:($epoch|tonumber)} else . end' > "$tmp_p2"
        mv "$tmp_p2" "$task_dir/state.patch.json"
        echo "[turn-budget] task $task_id ran $num_turns turns (cap $max_turns) but $succeeded; completing instead of blocking" >&2
        return 0
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

    local epoch="${ECAA_DISPATCH_EPOCH:-}"
    if ! [[ "$epoch" =~ ^[0-9]+$ ]]; then
        epoch=""
    fi

    local tmp_patch
    tmp_patch="$(mktemp)"
    jq -n \
      --arg reason "$reason" \
      --arg run_id "${ECAA_HARNESS_RUN_ID:-}" \
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

# Render-as-Contract: the FIXED, non-LLM figure render step. The compute agent
# (any language) writes the standardized figure-data-contract output TABLES
# under runtime/outputs/<task_id>/ and does NOT render figures itself; this
# function then runs the shipped runtime/plotting render entrypoint over those
# tables. Rendering is fixed + language-uniform, so the agent's compute-language
# choice carries no figure-path incentive.
#
# The render is BEST-EFFORT and never fails the task — the harness figure
# validator is the gate that enforces figure presence. Re-rendering from the
# tables is idempotent and OVERWRITES, which is intended (uniform figure
# provenance regardless of how many times the step runs).
#
# Reads .spec.required_figures (list) + .spec.plot_stage_id from the task's
# task-spec.json. Empty/absent required_figures => no-op. STAGE = plot_stage_id
# when non-null/non-empty, else the task id; FIGS = comma-join of
# required_figures. The render's stdout (the result-manifest JSON) is captured;
# its stderr is tee'd into the task's progress.log.
#
# Args:
#   $1 — PACKAGE (absolute package root)
#   $2 — ECAA_TASK_ID
#   $3 — mode: "container" when the compute ran in container mode (PRIMARY: a
#        second minimal docker run reusing the compute image + the same package
#        bind-mount + the same --user); anything else => host FALLBACK.
#   $4 — CONTAINER_IMAGE (required for the container path; ignored on host)
#   $5 — user spec for `docker run --user` (e.g. "1000:1000"); the SAME value
#        the compute container used.
#   $6 — /tmp tmpfs size for the render container (e.g. "$ECAA_DOCKER_TMPFS_TMP_SIZE").
render_required_figures() {
    local package="$1"
    local task_id="$2"
    local mode="$3"
    local container_image="${4:-}"
    local user_spec="${5:-}"
    local tmpfs_tmp_size="${6:-1g}"

    if [ -z "$task_id" ]; then
        return 0
    fi

    local task_dir="$package/runtime/outputs/$task_id"
    local spec_file="$task_dir/task-spec.json"
    local progress_log="$task_dir/progress.log"
    if [ ! -f "$spec_file" ]; then
        echo "[render] no task-spec.json for $task_id; skipping figure render" >&2
        return 0
    fi

    # Read .spec.required_figures (comma-joined) + .spec.plot_stage_id. Prefer
    # jq; fall back to a small python3 one-liner so a jq-less host still renders.
    local figs="" stage_id=""
    if command -v jq >/dev/null 2>&1; then
        figs="$(jq -r '(.spec.required_figures // []) | map(select(. != null and . != "")) | join(",")' "$spec_file" 2>/dev/null || echo "")"
        stage_id="$(jq -r '.spec.plot_stage_id // ""' "$spec_file" 2>/dev/null || echo "")"
    elif command -v python3 >/dev/null 2>&1; then
        figs="$(python3 -c '
import json, sys
try:
    spec = json.load(open(sys.argv[1])).get("spec") or {}
    figs = [f for f in (spec.get("required_figures") or []) if f]
    sys.stdout.write(",".join(figs))
except Exception:
    pass
' "$spec_file" 2>/dev/null || echo "")"
        stage_id="$(python3 -c '
import json, sys
try:
    spec = json.load(open(sys.argv[1])).get("spec") or {}
    sys.stdout.write(spec.get("plot_stage_id") or "")
except Exception:
    pass
' "$spec_file" 2>/dev/null || echo "")"
    else
        echo "[render] neither jq nor python3 available; skipping figure render for $task_id" >&2
        return 0
    fi

    if [ -z "$figs" ]; then
        echo "[render] no required figures for $task_id; skipping figure render" >&2
        return 0
    fi

    # STAGE = plot_stage_id when set, else the task id.
    local stage="$task_id"
    if [ -n "$stage_id" ]; then
        stage="$stage_id"
    fi

    local rel_outputs="runtime/outputs/$task_id"
    mkdir -p "$task_dir" 2>/dev/null || true
    local render_cache_base="${ECAA_TASK_SCRATCH_DIR:-${TMPDIR:-/tmp}}"
    local render_cache_dir="$render_cache_base/ecaa-render-$task_id"
    local render_home="$render_cache_dir/home"
    local render_xdg_cache="$render_cache_dir/xdg"
    local render_mpl_config="$render_cache_dir/matplotlib"
    mkdir -p "$render_home" "$render_xdg_cache" "$render_mpl_config" 2>/dev/null || true
    local container_render_home="/tmp"
    local container_render_xdg_cache="/tmp/ecaa-render-$task_id-xdg"
    local container_render_mpl_config="/tmp/ecaa-render-$task_id-matplotlib"

    echo "[render] rendering required figures for $task_id (stage=$stage figures=$figs)" >&2

    # PRIMARY: a second minimal container run reusing the compute image + the
    # same package bind-mount + the same --user the compute used, dispatched on
    # the executor's container runtime (docker/podman locally + on AWS;
    # apptainer/singularity on SLURM). Hardened like the compute run where the
    # engine supports it (read-only rootfs, tmpfs /tmp, dropped caps). Falls back
    # to the host interpreter when no runtime/image is available. Captures stdout
    # (the result-manifest JSON); tees stderr into progress.log.
    local render_out=""
    local render_host="( cd \"$package\" && HOME=\"$render_home\" XDG_CACHE_HOME=\"$render_xdg_cache\" MPLCONFIGDIR=\"$render_mpl_config\" PYTHONPATH=\"$package\" python3 -m runtime.plotting render --stage \"$stage\" --outputs \"$rel_outputs\" --required \"$figs\" )"
    case "$mode" in
        container|docker|podman)
            local engine="docker"
            [ "$mode" = "podman" ] && engine="podman"
            if [ -n "$container_image" ] && command -v "$engine" >/dev/null 2>&1; then
                local user_args=()
                [ -n "$user_spec" ] && user_args=(--user "$user_spec")
                render_out="$("$engine" run --rm \
                    --read-only \
                    --tmpfs "/tmp:rw,size=$tmpfs_tmp_size,mode=1777" \
                    --security-opt no-new-privileges \
                    --cap-drop=ALL \
                    "${user_args[@]}" \
                    -v "$package":"$package":rw \
                    -w "$package" \
                    -e "HOME=$container_render_home" \
                    -e "XDG_CACHE_HOME=$container_render_xdg_cache" \
                    -e "MPLCONFIGDIR=$container_render_mpl_config" \
                    "$container_image" \
                    python3 -m runtime.plotting render \
                      --stage "$stage" \
                      --outputs "$rel_outputs" \
                      --required "$figs" \
                    2> >(tee -a "$progress_log" >&2) || true)"
            else
                render_out="$( eval "$render_host" 2> >(tee -a "$progress_log" >&2) || true)"
            fi
            ;;
        apptainer|singularity)
            local engine="$mode"
            if [ -n "$container_image" ] && command -v "$engine" >/dev/null 2>&1; then
                render_out="$("$engine" exec --containall \
                    --bind "$package":"$package" \
                    --pwd "$package" \
                    "docker://$container_image" \
                    env \
                      "HOME=$container_render_home" \
                      "XDG_CACHE_HOME=$container_render_xdg_cache" \
                      "MPLCONFIGDIR=$container_render_mpl_config" \
                    python3 -m runtime.plotting render \
                      --stage "$stage" \
                      --outputs "$rel_outputs" \
                      --required "$figs" \
                    2> >(tee -a "$progress_log" >&2) || true)"
            else
                render_out="$( eval "$render_host" 2> >(tee -a "$progress_log" >&2) || true)"
            fi
            ;;
        *)
            # FALLBACK (host): run the shipped render entrypoint directly against
            # the package on the host interpreter.
            render_out="$( eval "$render_host" 2> >(tee -a "$progress_log" >&2) || true)"
            ;;
    esac

    # Surface the result-manifest JSON (stdout of the render) for forensics; the
    # render writes the figures themselves under $task_dir/figures/.
    if [ -n "$render_out" ]; then
        printf '[render] %s\n' "$render_out" >> "$progress_log" 2>/dev/null || true
    fi
    return 0
}
