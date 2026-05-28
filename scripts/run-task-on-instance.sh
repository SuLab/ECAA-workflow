#!/usr/bin/env bash
# Runs on the EC2 instance via SSM RunCommand; the harness's
# AwsExecutor::run_iteration hands this script off as the SSM document
# body.
#
# Contract:
#  1. Pull the package directory from S3 to a local staging path.
#  2. Run the agent against the staging path.
#  3. Atomically rename staging → final output path so a partial run
#  never leaves stale outputs in the canonical location.
#  4. Push the updated package back to S3.
#  5. Append a marker line to /tmp/scripps-task.log on success so the
#  AwsExecutor::is_task_stale SSM-aware check can confirm a task
#  that finished even if the harness crashed before observing it.
#
# Required env vars (passed via SSM `--parameters`):
#  SWFC_S3_PACKAGE_URI — s3://bucket/prefix/<package>
#  SWFC_AGENT_CMD — full agent invocation (e.g. /opt/scripps/bin/agent-claude.sh)
#  SWFC_TASK_ID — task id this run targets, for log + atomic-rename
#  SWFC_S3_OUTPUT_URI — s3://bucket/prefix/<package>/runtime/outputs/<task_id>/

set -euo pipefail

: "${SWFC_S3_PACKAGE_URI:?missing required env var}"
: "${SWFC_AGENT_CMD:?missing required env var}"
: "${SWFC_TASK_ID:?missing required env var}"
: "${SWFC_S3_OUTPUT_URI:?missing required env var}"

STAGING="$(mktemp -d -t scripps-task-XXXXXX)"
FINAL="${STAGING}-final"
LOG="/tmp/scripps-task.log"

cleanup() {
  rm -rf -- "$STAGING" "$FINAL"
}
trap cleanup EXIT

echo "[$(date -u +%FT%TZ)] task ${SWFC_TASK_ID} started; staging=${STAGING}" >> "$LOG"

# 1. Pull the package from S3 to a fresh staging dir.
aws s3 sync "$SWFC_S3_PACKAGE_URI" "$STAGING" --quiet

# 2. Run the agent. Failure here aborts before the rename so the
#  canonical output path stays untouched.
"$SWFC_AGENT_CMD" "$STAGING"

# 3. Atomic rename: move staging into a sibling dir then rename to
#  final so the canonical path either holds the previous run or
#  the new run, never a half-written mix.
mv "$STAGING" "$FINAL"

# 4. Push the updated package back to S3. Use sync --delete so files
#  the agent removed (e.g. an old result_ref) actually disappear.
aws s3 sync "$FINAL" "$SWFC_S3_PACKAGE_URI" --delete --quiet

# 5. Per-task output dir gets its own sync so the AwsExecutor can
#  fetch just the task result without re-pulling the whole package.
if [[ -d "$FINAL/runtime/outputs/$SWFC_TASK_ID" ]]; then
  aws s3 sync \
    "$FINAL/runtime/outputs/$SWFC_TASK_ID" \
    "$SWFC_S3_OUTPUT_URI" \
    --delete --quiet
fi

echo "[$(date -u +%FT%TZ)] task ${SWFC_TASK_ID} completed" >> "$LOG"

# The trap clears $FINAL too — the canonical state lives in S3 from
# this point on.
