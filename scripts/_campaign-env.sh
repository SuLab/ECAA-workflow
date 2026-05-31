#!/usr/bin/env bash
# Campaign env shim: load the repo .env verbatim, then redirect package/session
# roots off the sshfs mount onto LOCAL disk (live container execution + per-task
# git commits over sshfs is slow and races on .git/index.lock). Everything else
# (executor=local, bio-min image, billing, composer, git, API key) comes from .env.
set -a
# shellcheck disable=SC1091
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/.env"
set +a

export PATH="$HOME/.local/bin:$PATH"   # static jq for the agent scripts

# Point the server at the freshly-built release harness so /start-execution
# can spawn it (it is not installed to ~/.cargo/bin or on PATH).
# Absolute literal — .env sourcing above may reset REPO_ROOT, so don't depend on it here.
export ECAA_HARNESS_BIN_PATH="/home/a/scripps/ecaa-workflow/target/release/ecaa-workflow-harness"

CAMPAIGN_ROOT="${CAMPAIGN_ROOT:-$HOME/.ecaa-workflow/atom-campaign}"
export ECAA_PACKAGE_ROOT="$CAMPAIGN_ROOT/packages"
export ECAA_CHAT_SESSIONS_DIR="$CAMPAIGN_ROOT/sessions"
mkdir -p "$ECAA_PACKAGE_ROOT" "$ECAA_CHAT_SESSIONS_DIR"

# Keep container math single-threaded + headless (matches fixture agent defaults).
export OMP_NUM_THREADS="${OMP_NUM_THREADS:-1}"
export OPENBLAS_NUM_THREADS="${OPENBLAS_NUM_THREADS:-1}"
export MPLBACKEND=Agg
