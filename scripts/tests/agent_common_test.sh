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

CALLER_UID="$(id -u)"
CALLER_GID="$(id -g)"
if [ "$CALLER_UID" -ne 0 ]; then
    test "$(resolve_container_user_identity "" "$PACKAGE" "$CACHE")" \
        = "$CALLER_UID:$CALLER_GID"
fi

ECAA_AGENT_CONTAINER_UID=12345
ECAA_AGENT_CONTAINER_GID=12346
test "$(resolve_container_user_identity "" "$PACKAGE" "$CACHE" 0 0)" \
    = "12345:12346"
unset ECAA_AGENT_CONTAINER_UID ECAA_AGENT_CONTAINER_GID

ECAA_AGENT_CONTAINER_UID=0
ECAA_AGENT_CONTAINER_GID=12346
if resolve_container_user_identity "" "$PACKAGE" "$CACHE" 0 0 >/dev/null 2>&1; then
    echo "root container identity was accepted" >&2
    exit 1
fi
unset ECAA_AGENT_CONTAINER_UID ECAA_AGENT_CONTAINER_GID

WRITABLE="$TEST_ROOT/container-writable"
mkdir -p "$WRITABLE"
prepare_container_writable_path "$WRITABLE" "" "$CALLER_UID" "$CALLER_GID"
test "$(stat -c %u "$WRITABLE")" = "$CALLER_UID"
test "$(stat -c %g "$WRITABLE")" = "$CALLER_GID"

ECAA_AGENT_SCRATCH_DIR="$TEST_ROOT/external-scratch"
ECAA_CHAT_SESSION_ID="11111111-2222-4333-8444-555555555555"
test "$(resolve_task_scratch_dir "$PACKAGE" task)" \
    = "$ECAA_AGENT_SCRATCH_DIR/$ECAA_CHAT_SESSION_ID/task"
unset ECAA_AGENT_SCRATCH_DIR ECAA_CHAT_SESSION_ID
test "$(resolve_task_scratch_dir "$PACKAGE" task)" = "$PACKAGE/runtime/scratch/task"

printf '%s\n' "agent common helper tests passed"
