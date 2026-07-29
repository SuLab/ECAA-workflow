#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "$0")/../.." && pwd)"
source "$REPO_ROOT/scripts/agent-claude-common.sh"

TEST_ROOT="$(mktemp -d)"
trap 'chmod -R u+w "$TEST_ROOT" 2>/dev/null || true; rm -rf "$TEST_ROOT"' EXIT

PACKAGE="$TEST_ROOT/package"
SCRATCH="$TEST_ROOT/scratch/task"
mkdir -p "$PACKAGE/runtime/outputs/task" "$SCRATCH"

ECAA_TASK_SCRATCH_DIR="$SCRATCH"
stage_dood_helpers "$REPO_ROOT/scripts" "$PACKAGE" task

test "$ECAA_INSTALL_MOUNT_SRC" = "$SCRATCH/ecaa-helpers/ecaa-install"
test "$LIT_FETCH_MOUNT_SRC" = "$SCRATCH/ecaa-helpers/agent_literature_fetch.py"
test -f "$ECAA_INSTALL_MOUNT_SRC"
test -x "$ECAA_INSTALL_MOUNT_SRC"
test -f "$LIT_FETCH_MOUNT_SRC"
cmp "$REPO_ROOT/scripts/ecaa-install" "$ECAA_INSTALL_MOUNT_SRC"
cmp "$REPO_ROOT/scripts/agent_literature_fetch.py" "$LIT_FETCH_MOUNT_SRC"

unset ECAA_TASK_SCRATCH_DIR
stage_dood_helpers "$REPO_ROOT/scripts" "$PACKAGE" task
test "$ECAA_INSTALL_MOUNT_SRC" = "$PACKAGE/runtime/outputs/task/.ecaa-helpers/ecaa-install"
test -x "$ECAA_INSTALL_MOUNT_SRC"

CACHE="$TEST_ROOT/cache"
mkdir -p "$CACHE"
ensure_writable_session_cache "$CACHE" ""
for child in pip conda conda-envs apt R-libs python helpers; do
    test -d "$CACHE/$child"
    test -w "$CACHE/$child"
done

printf '%s\n' "agent common helper tests passed"
