#!/usr/bin/env bash
# Local helper: run the nekrutenko ECAA-arm smoke with the fix-loop env.
# NOT part of the shipped eval surface — a throwaway operator launcher.
set -uo pipefail
cd /home/a/scripps/ecaa-workflow

# Load .env (keys, executor mode, eval dirs).
set -a; . ./.env; set +a

# --- fix-loop overrides ---
export ECAA_EVAL_LIVE=1
# Dodge the subscription session limit (429) seen in prior runs: bill the agent
# against the API key so an unattended multi-task DAG can complete.
export ECAA_AGENT_BILLING=api
# Clear the survey_method_landscape Phase-13 validation guard-block (its output
# is not needed for VCFs since methods are locked) so downstream variant_calling
# runs. Guard catch is recorded BEFORE the documented-deviation skip.
export ECAA_EVAL_MAX_RELAUNCH=3
# Keep the emitted package + run dir for post-mortem.
export ECAA_EVAL_KEEP_SCRATCH=1
# Isolate from the running dev server's ivd-comprehensive session/package dirs.
export ECAA_PACKAGE_ROOT=/home/a/mounts/wadmin/home/a/eval-packages/nek-jaccard
export ECAA_CHAT_SESSIONS_DIR=/home/a/mounts/wadmin/home/a/eval-packages/nek-jaccard-sessions
mkdir -p "$ECAA_PACKAGE_ROOT" "$ECAA_CHAT_SESSIONS_DIR"

echo "[run] git HEAD: $(git rev-parse --short HEAD)"
echo "[run] harness: $(command -v ecaa-workflow-harness) ($(date -r "$(command -v ecaa-workflow-harness)" +%H:%M 2>/dev/null))"
echo "[run] billing=$ECAA_AGENT_BILLING relaunch=$ECAA_EVAL_MAX_RELAUNCH pkg_root=$ECAA_PACKAGE_ROOT"
echo "[run] starting nekrutenko --smoke --arms ${1:-ecaa} at $(date -u)"

python3 -m scripts.eval.eval_runner nekrutenko --smoke --arms "${1:-ecaa}"
echo "[run] eval_runner EXIT=$? at $(date -u)"
