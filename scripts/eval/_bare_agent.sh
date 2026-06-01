#!/usr/bin/env bash
# Bare "Claude Code direct" runner for the eval harness.
#
# Runs Claude Code headless inside the SAME bio-min container as the ECAA arm,
# on a raw benchmark instruction, with the same toolchain + subscription
# credentials — but with NO ecaa task scaffolding (no compiled DAG, no appended
# task-execution contract, no state.patch.json/result.json expectation, no
# transient-retry machinery). This is the fair "direct" counterfactual to the
# ECAA arm: identical execution environment, differing only in scaffolding.
#
# It deliberately does NOT use scripts/agent-claude.sh, whose retry/output
# machinery is coupled to the ecaa per-task contract (it retries whenever no
# state.patch.json is written and routes output through a per-task log) and is
# wrong for a bare prompt.
#
# Usage: _bare_agent.sh <workdir>
#   Reads <workdir>/PROMPT.md as the prompt; mounts <workdir> rw as the cwd.
#   Prints claude's `--output-format=json` result envelope to stdout.
set -euo pipefail

WORKDIR="$(realpath "$1")"
PROMPT="$(cat "$WORKDIR/PROMPT.md")"
IMAGE="${ECAA_DEFAULT_CONTAINER_IMAGE:-bio-min:local}"
RUNTIME="${ECAA_CONTAINER_RUNTIME:-docker}"
MODEL="${ECAA_EVAL_BARE_MODEL:-claude-sonnet-4-6}"

# Per-run claude HOME seeded with the host's subscription credentials. Mounted
# rw because claude must write its OAuth refresh token + history. Kept separate
# from the host $HOME/.claude so the operator's own session isn't clobbered.
BARE_HOME="${ECAA_EVAL_BARE_HOME:-$HOME/.ecaa-workflow/eval-bare-home}"
mkdir -p "$BARE_HOME/.claude"
if [ -f "$HOME/.claude/.credentials.json" ]; then
  install -m 600 "$HOME/.claude/.credentials.json" "$BARE_HOME/.claude/.credentials.json"
fi
[ -f "$HOME/.claude.json" ] && cp -f "$HOME/.claude.json" "$BARE_HOME/.claude.json" 2>/dev/null || true

# Claude Code install mounted into the container (the bio image does not bundle
# it). Installed once into a cache dir; the node_modules tree is mounted ro.
CC_DIR="${ECAA_EVAL_BARE_CLAUDE_DIR:-$HOME/.ecaa-workflow/eval-bare-claude}"
if [ ! -e "$CC_DIR/node_modules/.bin/claude" ]; then
  mkdir -p "$CC_DIR"
  echo "_bare_agent.sh: installing @anthropic-ai/claude-code into $CC_DIR (one-time)..." >&2
  npm install --prefix "$CC_DIR" --silent --no-audit --no-fund \
    "@anthropic-ai/claude-code${ECAA_AGENT_CLAUDE_VERSION:+@$ECAA_AGENT_CLAUDE_VERSION}" >/dev/null 2>&1 || true
fi

# Clean container run (mirrors the validated minimal invocation). The container
# runs as the host uid so files written to the rw-mounted workdir are owned by
# the operator. claude writes trace.md/answer.txt (or whatever the prompt asks)
# into the cwd-mounted workdir; its JSON result envelope goes to stdout.
exec "$RUNTIME" run --rm \
  --user "$(id -u):$(id -g)" \
  -v "$WORKDIR":"$WORKDIR":rw \
  -v "$BARE_HOME":"$HOME":rw \
  -v "$CC_DIR/node_modules":/opt/claude-code/node_modules:ro \
  -e PATH="/opt/claude-code/node_modules/.bin:/opt/conda/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
  -w "$WORKDIR" \
  -e "HOME=$HOME" \
  "$IMAGE" \
  claude --dangerously-skip-permissions --output-format=json --model "$MODEL" -p "$PROMPT"
