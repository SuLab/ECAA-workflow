#!/usr/bin/env bash
# run-task-on-slurm.sh — the compute-node entrypoint for a SLURM-scheduled
# scripps task. Invoked from the sbatch script body; see
# crates/harness/src/executor/slurm/sbatch.rs::render_sbatch_script.
#
# Flow:
#  1. Sanity-check that we are actually inside a SLURM allocation
#  ($SLURM_JOB_ID set).
#  2. Source a per-session credentials file if one was staged (so
#  ANTHROPIC_API_KEY never has to go through `#SBATCH --export=`,
#  which is visible in `scontrol show job` on some sites).
#  3. Run the agent wrapper passed as the first positional argument,
#  passing the package root ($SLURM_SUBMIT_DIR or the rsync'd
#  staging copy — we cd'd into it before calling this).
#  4. Propagate the agent's exit code so sbatch's ExitCode reflects
#  what actually happened, not just "the script ran".

set -euo pipefail

AGENT_CMD="${1:?usage: run-task-on-slurm.sh <agent-wrapper> [package-root]}"
PACKAGE="${2:-$PWD}"

if [[ -z "${SLURM_JOB_ID:-}" ]]; then
  echo "[run-task-on-slurm] warning: SLURM_JOB_ID not set — are we actually running under sbatch?" >&2
fi

# Optional: source credentials file staged alongside the package. The
# harness drops this with 0600 perms so `--export=` doesn't have to
# carry secrets.
CREDS_FILE="$PACKAGE/.scripps-creds.env"
if [[ -f "$CREDS_FILE" ]]; then
  # shellcheck disable=SC1090
  source "$CREDS_FILE"
fi

# The sbatch script already did `cd $remote_pkg` before calling this,
# but double-check in case a future caller shell-outs differently.
cd "$PACKAGE"

echo "[run-task-on-slurm] job=$SLURM_JOB_ID node=$(hostname) package=$PACKAGE agent=$AGENT_CMD"
exec "$AGENT_CMD" "$PACKAGE"
