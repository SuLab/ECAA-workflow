#!/usr/bin/env bash
# QA campaign env: load the repo .env verbatim (live LLM key, composer,
# validate-on-emit=full, git, bio-min image), then redirect package/session
# roots onto LOCAL disk so emit + per-task git commits don't race on the
# sshfs mount. Everything that defines product behavior comes from .env.
set -a
# shellcheck disable=SC1091
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/.env"
set +a

export PATH="$HOME/.local/bin:$PATH"
export ECAA_HARNESS_BIN_PATH="/home/a/scripps/ecaa-workflow/target/release/ecaa-workflow-harness"

QA_ROOT="${QA_ROOT:-$HOME/.ecaa-workflow/qa-20260531}"
export ECAA_PACKAGE_ROOT="$QA_ROOT/packages"
export ECAA_CHAT_SESSIONS_DIR="$QA_ROOT/sessions"
mkdir -p "$ECAA_PACKAGE_ROOT" "$ECAA_CHAT_SESSIONS_DIR"

export OMP_NUM_THREADS="${OMP_NUM_THREADS:-1}"
export OPENBLAS_NUM_THREADS="${OPENBLAS_NUM_THREADS:-1}"
export MPLBACKEND=Agg
