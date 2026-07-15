# explicit_lock.sh — per-task EXPLICIT conda lock capture (install-from-log
# re-execution). Sourced by agent-claude.sh; also sourced directly by
# scripts/tests/explicit_lock_test.sh for deterministic, docker-stubbed
# coverage.
#
# Contract: capture_explicit_lock TASK_ID ENVS_DIR IMAGE OUT_DIR
#  - Best-effort, NEVER fails (always returns 0): every fallible step is
#    `|| true`-guarded so a caller under `set -e` is unaffected.
#  - Only captures when ENVS_DIR contains EXACTLY ONE env subdirectory
#    (unambiguous which env belongs to this task); zero or multiple envs
#    is a silent no-op — no file is written, no error is raised.
#  - Runs `conda list -p <env> --explicit --md5` inside IMAGE via a
#    `timeout 180 docker run --rm -v <env>:<env> <image> ...` (conda's own
#    conda-meta bookkeeping lives in the env dir, so no host conda binary
#    is needed).
#  - Drops an empty or non-`@EXPLICIT` capture so a failed/garbage docker
#    run never masquerades as a valid lock file.
#  - Writes OUT_DIR/env.explicit.lock on success; OUT_DIR is created if
#    missing.

capture_explicit_lock() {
  local task_id="$1" envs_dir="$2" image="$3" out_dir="$4"

  [ -n "$task_id" ] || return 0
  [ -n "$image" ] || return 0
  command -v docker >/dev/null 2>&1 || return 0
  [ -d "$envs_dir" ] || return 0

  local env_n env_p
  env_n=$(find "$envs_dir" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | wc -l)
  if [ "$env_n" = "1" ]; then
    env_p=$(find "$envs_dir" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | head -1)
  elif [ -d "$envs_dir/ecaa-bioc" ]; then
    # ecaa-install materialises R/Bioconductor packages into a shared
    # 'ecaa-bioc' env, so a per-session envs dir routinely holds more than one
    # env (e.g. an 'ecaa-bioc-annot' companion). The old single-env guard then
    # silently captured nothing, leaving the deposit with no installable
    # env.explicit.lock and breaking offline re-execution. Every generated
    # stage that uses conda runs `conda run -n ecaa-bioc` (or
    # `--prefix .../ecaa-bioc`), so the shared 'ecaa-bioc' env is the
    # authoritative one to snapshot when several exist.
    env_p="$envs_dir/ecaa-bioc"
  else
    return 0
  fi

  mkdir -p "$out_dir" 2>/dev/null || true

  timeout 180 docker run --rm -v "$env_p:$env_p" "$image" \
    conda list -p "$env_p" --explicit --md5 \
    > "$out_dir/env.explicit.lock" 2>/dev/null \
    || { rm -f "$out_dir/env.explicit.lock" 2>/dev/null; true; }

  # Drop an empty/failed capture so it never masquerades as a valid lock.
  [ -s "$out_dir/env.explicit.lock" ] \
    && grep -q '@EXPLICIT' "$out_dir/env.explicit.lock" 2>/dev/null \
    || { rm -f "$out_dir/env.explicit.lock" 2>/dev/null; true; }

  return 0
}
