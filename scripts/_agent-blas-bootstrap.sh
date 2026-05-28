#!/usr/bin/env bash
# Safe-shell guard: no-op when sourced (flags inherit from caller); enables
# strict mode when executed directly so smoke-testing this file in isolation
# surfaces unset-var / pipe-failure regressions instead of silently swallowing
# them. The [[ source == "$0" ]] test is false under `source` / `.`.
[[ "${BASH_SOURCE[0]}" == "${0}" ]] && set -euo pipefail
# _agent-blas-bootstrap.sh — sourced by agent-claude*.sh.
#
# Two responsibilities:
#
#  1. Defensive re-export of BLAS / OpenMP / numerical-library
#  thread-budget env vars from ECAA_HW_RECOMMENDED_THREADS. The
#  harness hardware envelope (crates/harness/.../hardware_envelope.rs
#  apply_blas_thread_envelope) already sets these as bare env vars,
#  but we re-export defensively so that:
#  - A stale harness binary that only sets ECAA_HW_ENV_OVERRIDES
#  (the JSON blob) still produces correct BLAS threading.
#  - Standalone invocations (debugging) get the same treatment.
#  The defensive form preserves any pre-existing value, so an
#  operator who manually exported OMP_NUM_THREADS=4 wins.
#
#  2. R BLAS probe + LD_PRELOAD libopenblas.so when reference netlib
#  BLAS is detected. Stock R on Debian/Ubuntu links against
#  single-threaded libRblas.so; setting OMP_NUM_THREADS=N has no
#  effect because reference BLAS doesn't read those vars. The fix
#  that works without rebuilding R or sudo is to LD_PRELOAD
#  libopenblas.so so symbol-lookup-order interposes OpenBLAS's
#  dgemm_/dsyrk_/dgesvd_ ahead of libRblas's. The probe runs once
#  per package and caches in runtime/.blas-probe.json (separate
#  from env_capability.json, which the harness rewrites at startup).
#
# Required by caller (set before sourcing): PACKAGE — absolute path to
# the package root.
#
# Output: stderr lines beginning with "[agent-blas]" so the harness log
# captures the BLAS configuration. No stdout. Idempotent across
# repeated invocations.

# ---- 1. Defensive thread-budget exports ------------------------------

__agent_positive_int() {
  [[ "${1:-}" =~ ^[0-9]+$ ]] && [ "$1" -gt 0 ]
}

__agent_thread_budget() {
  if __agent_positive_int "${ECAA_HW_NPROC_HINT:-}"; then
    printf '%s\n' "$ECAA_HW_NPROC_HINT"
  elif __agent_positive_int "${ECAA_HW_RECOMMENDED_THREADS:-}"; then
    printf '%s\n' "$ECAA_HW_RECOMMENDED_THREADS"
  fi
}

__agent_apply_thread_budget() {
  local __agent_blas_n
  __agent_blas_n="$(__agent_thread_budget)"
  [ -n "$__agent_blas_n" ] || return 0
  for __agent_blas_var in \
      OMP_NUM_THREADS OPENBLAS_NUM_THREADS GOTO_NUM_THREADS \
      MKL_NUM_THREADS BLIS_NUM_THREADS VECLIB_MAXIMUM_THREADS \
      NUMEXPR_NUM_THREADS NUMEXPR_MAX_THREADS TBB_NUM_THREADS \
      RAYON_NUM_THREADS NUMBA_NUM_THREADS JULIA_NUM_THREADS \
      POLARS_MAX_THREADS; do
    # `${!var:-}` reads var-by-name. Only export when unset/empty so
    # operator-supplied overrides win. ECAA_HW_NPROC_HINT is the
    # remediation / scheduler floor and intentionally overrides the
    # already-rendered ECAA_HW_RECOMMENDED_THREADS values.
    if __agent_positive_int "${ECAA_HW_NPROC_HINT:-}" \
      || [ -z "${!__agent_blas_var:-}" ]; then
      export "$__agent_blas_var=$__agent_blas_n"
    fi
  done
  unset __agent_blas_n __agent_blas_var
}

__agent_apply_thread_budget

# ---- 2. Build per-runtime ENV-pair list for container forwarding ----
#
# `docker run` and `apptainer/singularity exec` do NOT inherit env vars
# from the parent shell — each var must be passed explicitly via -e
# (docker/podman) or --env (apptainer/singularity). Build a single
# "KEY=VAL" list the caller can fan out into the right flag shape.
#
# Includes BLAS thread vars + the ECAA_HW_* envelope so the agent
# inside the container sees the same view the host-path agent does.
# Kept as a function because caller scripts may create scratch/cache
# env vars after sourcing this file; they call it again immediately
# before building docker/apptainer args.
#
# IMPORTANT: keep this list in sync with the ECAA_* vars stamped by
# crates/harness/src/*.rs::stamp_* functions. The RCA after 13f7e034
# found that adding two identity vars left three other classes of
# stamped vars (network policy, provisioning policy, literature scope)
# still unforwarded into docker containers — silent behavioral
# degradation. A test that asserts forward-list >= stamp-set is in
# crates/harness/tests/env_forward_completeness.rs (TODO).

__agent_env_forward_keys=(
    OMP_NUM_THREADS OPENBLAS_NUM_THREADS GOTO_NUM_THREADS \
    MKL_NUM_THREADS BLIS_NUM_THREADS VECLIB_MAXIMUM_THREADS \
    NUMEXPR_NUM_THREADS NUMEXPR_MAX_THREADS TBB_NUM_THREADS \
    RAYON_NUM_THREADS NUMBA_NUM_THREADS JULIA_NUM_THREADS \
    POLARS_MAX_THREADS \
    OMP_THREAD_LIMIT OMP_DYNAMIC OMP_PROC_BIND OMP_PLACES \
    MKL_DYNAMIC MKL_DOMAIN_NUM_THREADS GOMP_CPU_AFFINITY \
    OPENBLAS_VERBOSE OPENBLAS_MAIN_FREE \
    CUDA_VISIBLE_DEVICES CUDA_DEVICE_ORDER \
    NVIDIA_VISIBLE_DEVICES NVIDIA_DRIVER_CAPABILITIES \
    R_MAX_NUM_DLLS MALLOC_ARENA_MAX \
    ECAA_HW_VCPUS_AVAILABLE ECAA_HW_MEMORY_GB ECAA_HW_GPU \
    ECAA_HW_RECOMMENDED_THREADS ECAA_HW_NPROC_HINT \
    ECAA_HW_TOOL_THREAD_CURVES \
    ECAA_HW_ENV_OVERRIDES ECAA_HW_GPU_CAPABILITY_REF \
    ECAA_HW_INTAKE_FACTS ECAA_HW_CONCURRENT_PEERS_BY_CLASS \
    ECAA_HW_TASK_RESOURCE_CLASS ECAA_HW_DYNAMIC_ALLOCATION \
    ECAA_HW_OVERHEAD_VCPUS ECAA_HW_OVERHEAD_MEMORY_GB ECAA_HW_OVERHEAD_PCT \
    ECAA_AGENT_MEMORY_CAP_GB ECAA_AGENT_WALLCLOCK_SECS \
    ECAA_HARNESS_CONCURRENCY ECAA_EXECUTOR_MODE \
    ECAA_PILOT_ENABLED ECAA_PILOT_TASKS ECAA_PILOT_MULTIPLIER \
    ECAA_PILOT_INSTANCE ECAA_PILOT_INTERVAL_SECS \
    ECAA_HARNESS_RUN_ID ECAA_DISPATCH_EPOCH \
    ECAA_TASK_NETWORK ECAA_PROVISIONING_POLICY \
    ECAA_LIT_SOURCE_SCOPE ECAA_LIT_EVIDENCE_MAX_MB \
    ECAA_LIT_NCBI_API_KEY ECAA_LIT_INSTITUTIONAL_ACCESS \
    ECAA_TASK_ID ECAA_TASK_SCRATCH_DIR ECAA_SESSION_CACHE_DIR \
    ECAA_AGENT_CACHE_DIR ECAA_AGENT_CACHE_DISABLE ECAA_AGENT_SCRATCH_DIR \
    PIP_CACHE_DIR CONDA_PKGS_DIRS R_LIBS_USER PYTHONUSERBASE \
    PIP_USER PIP_BREAK_SYSTEM_PACKAGES \
    TASK_CONTAINER_GPU_REQUIRED TASK_GPU_KIND TASK_GPU_COUNT TASK_GPU_MIG_PROFILE
)

__agent_build_env_forward_pairs() {
  _AGENT_ENV_FORWARD_PAIRS=()
  local __agent_v
  for __agent_v in "${__agent_env_forward_keys[@]}"; do
    if [ -n "${!__agent_v:-}" ]; then
      _AGENT_ENV_FORWARD_PAIRS+=("$__agent_v=${!__agent_v}")
    fi
  done

  # Stage profiles can promote arbitrary env_overrides_template keys
  # (for example CUDA_VISIBLE_DEVICES or allocator knobs). Forward
  # string/number/bool JSON entries even when the key is not in the
  # fixed allowlist above.
  if [ -n "${ECAA_HW_ENV_OVERRIDES:-}" ] && command -v jq >/dev/null 2>&1; then
    local __agent_override_lines __agent_line __agent_key __agent_val
    __agent_override_lines="$(printf '%s' "$ECAA_HW_ENV_OVERRIDES" \
      | jq -r 'to_entries[] | select(.value | type == "string" or type == "number" or type == "boolean") | "\(.key)=\(.value|tostring)"' 2>/dev/null || true)"
    while IFS= read -r __agent_line; do
      [ -n "$__agent_line" ] || continue
      __agent_key="${__agent_line%%=*}"
      __agent_val="${__agent_line#*=}"
      [[ "$__agent_key" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
      if [ -n "${!__agent_key:-}" ]; then
        _AGENT_ENV_FORWARD_PAIRS+=("$__agent_key=${!__agent_key}")
      else
        _AGENT_ENV_FORWARD_PAIRS+=("$__agent_key=$__agent_val")
      fi
    done <<< "$__agent_override_lines"
  fi
  export _AGENT_ENV_FORWARD_PAIRS
}

__agent_build_env_forward_pairs

# ---- 3. R BLAS probe + LD_PRELOAD libopenblas ------------------------

__agent_blas_probe_r() {
  local pkg="$1"
  [ -n "$pkg" ] || return 0
  command -v jq >/dev/null 2>&1 || return 0
  command -v Rscript >/dev/null 2>&1 || return 0

  local cache="$pkg/runtime/.blas-probe.json"
  mkdir -p "$pkg/runtime" 2>/dev/null || return 0
  [ -f "$cache" ] || echo '{}' > "$cache"

  # Probe R BLAS path once per package.
  local r_blas
  r_blas="$(jq -r '.r_blas_path // empty' "$cache" 2>/dev/null)"
  if [ -z "$r_blas" ] || [ "$r_blas" = "null" ]; then
    r_blas="$(Rscript --vanilla -e 'cat(extSoftVersion()["BLAS"])' 2>/dev/null || true)"
    [ -n "$r_blas" ] || r_blas="unknown"
    local tmp
    tmp="$(mktemp 2>/dev/null)" || return 0
    if jq --arg p "$r_blas" '.r_blas_path = $p' "$cache" > "$tmp" 2>/dev/null; then
      mv "$tmp" "$cache"
    else
      rm -f "$tmp"
    fi
  fi

  # Operator kill-switch.
  if [ "${ECAA_DISABLE_BLAS_PRELOAD:-0}" = "1" ]; then
    echo "[agent-blas] R BLAS = $r_blas (ECAA_DISABLE_BLAS_PRELOAD=1 — skipping)" >&2
    return 0
  fi

  # Detect whether R's BLAS is already a parallel implementation. The
  # path contains a self-identifying name for every parallel BLAS in
  # the wild — invert the check so any unknown shape (including the
  # Debian/Ubuntu alternative path `.../blas/libblas.so.3.x` which is
  # single-threaded netlib reference, and the historical `libRblas.so`
  # bundled with stock R) defaults to preload-needed. Safe-by-default:
  # libopenblas symbols only intercept BLAS calls, so preloading on a
  # process that turns out to use parallel BLAS is a small RSS cost
  # with no functional harm.
  local r_blas_lower
  r_blas_lower="$(printf '%s' "$r_blas" | tr '[:upper:]' '[:lower:]')"
  case "$r_blas_lower" in
    *openblas*|*mkl_rt*|*libmkl*|*blis*|*flexiblas*|*accelerate*|*armpl*)
      echo "[agent-blas] R BLAS = $r_blas (parallel — no LD_PRELOAD needed)" >&2
      return 0
      ;;
    *)
      # Likely single-threaded (libRblas.so or the Debian reference
      # netlib alternative path); fall through to LD_PRELOAD setup.
      ;;
  esac

  # Reference BLAS detected. Resolve a usable libopenblas; cache the
  # result so subsequent invocations skip the ldconfig scan.
  local preload
  preload="$(jq -r '.openblas_preload_path // empty' "$cache" 2>/dev/null)"
  if [ -z "$preload" ] || [ "$preload" = "null" ] || [ ! -e "$preload" ]; then
    preload="$(ldconfig -p 2>/dev/null | awk '/libopenblas\.so/ {print $NF; exit}')"
    if [ -z "$preload" ]; then
      for cand in \
          /usr/lib/x86_64-linux-gnu/libopenblas.so.0 \
          /usr/lib/x86_64-linux-gnu/libopenblas.so \
          /usr/lib64/libopenblas.so.0 \
          /usr/lib64/libopenblas.so \
          /opt/homebrew/opt/openblas/lib/libopenblas.dylib \
          /usr/local/opt/openblas/lib/libopenblas.dylib; do
        if [ -e "$cand" ]; then
          preload="$cand"
          break
        fi
      done
    fi
    if [ -z "$preload" ]; then
      echo "[agent-blas] R linked against reference BLAS ($r_blas) and no libopenblas.so found via ldconfig — Rscript subprocesses will run single-threaded BLAS. Install libopenblas-dev (Debian/Ubuntu) or libopenblas (macOS Homebrew) to enable parallel BLAS." >&2
      return 0
    fi
    local tmp
    tmp="$(mktemp 2>/dev/null)" || return 0
    if jq --arg p "$preload" '.openblas_preload_path = $p' "$cache" > "$tmp" 2>/dev/null; then
      mv "$tmp" "$cache"
    else
      rm -f "$tmp"
    fi
  fi

  if [ -n "${LD_PRELOAD:-}" ]; then
    case ":$LD_PRELOAD:" in
      *":$preload:"*) ;;
      *) export LD_PRELOAD="$preload:$LD_PRELOAD" ;;
    esac
  else
    export LD_PRELOAD="$preload"
  fi
  echo "[agent-blas] R reference BLAS detected ($r_blas); LD_PRELOAD=$preload to interpose libopenblas" >&2
}

__agent_blas_probe_r "${PACKAGE:-}"
unset -f __agent_blas_probe_r 2>/dev/null || true
