#!/usr/bin/env bash
# explicit_lock_test.sh — Deterministic, docker-stubbed unit test for
# scripts/lib/explicit_lock.sh::capture_explicit_lock (the exec-time
# per-task EXPLICIT conda lock capture invoked from agent-claude.sh).
#
# No live API calls and no real docker/conda invocation: a fake `docker` is
# prepended to PATH so `capture_explicit_lock` runs entirely against canned,
# in-process output. Run directly:
#   scripts/tests/explicit_lock_test.sh
# (bats is not assumed to be installed; this is plain bash + the shared
# scripts/lib/test-helpers.sh assertion helpers.)

set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$TEST_DIR/.." && pwd)"

PASS=0
FAIL=0

# shellcheck source=../lib/test-helpers.sh
source "$SCRIPTS_DIR/lib/test-helpers.sh"
# shellcheck source=../lib/explicit_lock.sh
source "$SCRIPTS_DIR/lib/explicit_lock.sh"

WORK="$(mktemp_scripps explicit-lock)"
cleanup_trap "rm -rf '$WORK'"

# ── Fake docker ──────────────────────────────────────────────────────────
# Only implements `docker run ... conda list ... --explicit --md5`; ignores
# the exact args and just prints canned output selected via
# FAKE_DOCKER_MODE:
#   explicit (default) — print a canned, valid `@EXPLICIT` lock
#   bad                — print output missing the `@EXPLICIT` marker
#   empty              — print nothing
#   fail               — exit 1 with no output
FAKE_BIN="$WORK/bin"
mkdir -p "$FAKE_BIN"
cat > "$FAKE_BIN/docker" <<'DOCKER_STUB'
#!/usr/bin/env bash
mode="${FAKE_DOCKER_MODE:-explicit}"
case "$mode" in
  explicit)
    cat <<'EOF'
# This file may be used to create an environment using:
# $ conda create --name <env> --file <this file>
@EXPLICIT
https://conda.anaconda.org/conda-forge/linux-64/ca-certificates-2024.2.2-hbcca054_0.conda#8d652ea2ac6f2f3d3e290e12ec8e1ac0
EOF
    ;;
  bad)
    echo "no explicit marker in this output"
    ;;
  empty)
    : ;;
  fail)
    exit 1 ;;
esac
DOCKER_STUB
chmod +x "$FAKE_BIN/docker"
PATH="$FAKE_BIN:$PATH"
export PATH

IMAGE="sha256:0000000000000000000000000000000000000000000000000000000000ab"

echo "Step 1: exactly one env dir + valid docker output -> lock captured"
ENVS_ONE="$WORK/envs-one"
mkdir -p "$ENVS_ONE/my-env"
OUT_ONE="$WORK/out-one"
FAKE_DOCKER_MODE=explicit capture_explicit_lock "task-one" "$ENVS_ONE" "$IMAGE" "$OUT_ONE"
if [[ -s "$OUT_ONE/env.explicit.lock" ]] && grep -q '@EXPLICIT' "$OUT_ONE/env.explicit.lock"; then
  ok "single env + valid docker output -> lock file written with @EXPLICIT"
else
  fail "single env + valid docker output -> expected lock file was not written"
fi

echo ""
echo "Step 2: zero env dirs -> no capture"
ENVS_ZERO="$WORK/envs-zero"
mkdir -p "$ENVS_ZERO"
OUT_ZERO="$WORK/out-zero"
FAKE_DOCKER_MODE=explicit capture_explicit_lock "task-zero" "$ENVS_ZERO" "$IMAGE" "$OUT_ZERO"
if [[ ! -e "$OUT_ZERO/env.explicit.lock" ]]; then
  ok "zero env dirs -> no lock file written"
else
  fail "zero env dirs -> unexpected lock file written"
fi

echo ""
echo "Step 3: two env dirs (ambiguous) -> no capture"
ENVS_TWO="$WORK/envs-two"
mkdir -p "$ENVS_TWO/env-a" "$ENVS_TWO/env-b"
OUT_TWO="$WORK/out-two"
FAKE_DOCKER_MODE=explicit capture_explicit_lock "task-two" "$ENVS_TWO" "$IMAGE" "$OUT_TWO"
if [[ ! -e "$OUT_TWO/env.explicit.lock" ]]; then
  ok "two env dirs -> no lock file written (ambiguous, skipped)"
else
  fail "two env dirs -> unexpected lock file written"
fi

echo ""
echo "Step 4: one env dir but docker output has no @EXPLICIT marker -> dropped"
ENVS_BAD="$WORK/envs-bad"
mkdir -p "$ENVS_BAD/my-env"
OUT_BAD="$WORK/out-bad"
FAKE_DOCKER_MODE=bad capture_explicit_lock "task-bad" "$ENVS_BAD" "$IMAGE" "$OUT_BAD"
if [[ ! -e "$OUT_BAD/env.explicit.lock" ]]; then
  ok "non-@EXPLICIT docker output -> capture dropped"
else
  fail "non-@EXPLICIT docker output -> lock file should have been dropped"
fi

echo ""
echo "Step 5: one env dir but docker prints nothing -> dropped"
ENVS_EMPTY="$WORK/envs-empty-out"
mkdir -p "$ENVS_EMPTY/my-env"
OUT_EMPTY="$WORK/out-empty"
FAKE_DOCKER_MODE=empty capture_explicit_lock "task-empty" "$ENVS_EMPTY" "$IMAGE" "$OUT_EMPTY"
if [[ ! -e "$OUT_EMPTY/env.explicit.lock" ]]; then
  ok "empty docker output -> capture dropped"
else
  fail "empty docker output -> lock file should have been dropped"
fi

echo ""
echo "Step 6: one env dir but docker run fails (exit 1) -> dropped, no crash"
ENVS_FAIL="$WORK/envs-fail-run"
mkdir -p "$ENVS_FAIL/my-env"
OUT_FAIL="$WORK/out-fail"
FAKE_DOCKER_MODE=fail capture_explicit_lock "task-fail" "$ENVS_FAIL" "$IMAGE" "$OUT_FAIL"
if [[ ! -e "$OUT_FAIL/env.explicit.lock" ]]; then
  ok "failed docker run -> capture dropped, never crashes caller"
else
  fail "failed docker run -> lock file should have been dropped"
fi

echo ""
echo "Step 7: empty task_id / image -> no-op guard (never invokes docker)"
OUT_NOID="$WORK/out-noid"
capture_explicit_lock "" "$ENVS_ONE" "$IMAGE" "$OUT_NOID"
if [[ ! -e "$OUT_NOID/env.explicit.lock" ]]; then
  ok "empty task_id -> no-op guard"
else
  fail "empty task_id -> unexpected lock file written"
fi

OUT_NOIMG="$WORK/out-noimg"
capture_explicit_lock "task-noimg" "$ENVS_ONE" "" "$OUT_NOIMG"
if [[ ! -e "$OUT_NOIMG/env.explicit.lock" ]]; then
  ok "empty image -> no-op guard"
else
  fail "empty image -> unexpected lock file written"
fi

echo ""
echo "======================================================"
TOTAL=$((PASS + FAIL))
echo "Explicit Lock Capture Test Results: $PASS/$TOTAL passed"
if [[ "$FAIL" -eq 0 ]]; then
  echo "All explicit-lock capture checks passed."
  exit 0
else
  echo "$FAIL check(s) FAILED."
  exit 1
fi
