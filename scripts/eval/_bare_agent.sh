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

# Seed credentials + main config under a flock, and ONLY when the bare-home
# copy is absent or the host's is strictly newer (mtime-gated). Mirrors
# agent-claude.sh's RC-23 credential handling and is the fix for the OAuth
# rotation race: once claude rotates its refresh token inside BARE_HOME the
# bare-home file's mtime advances past the host's, so unconditionally
# re-seeding (the previous `install -m 600` every run) would clobber a
# freshly-rotated valid token with the host's now-stale one and strand the arm
# on a dead token — manifesting as a 40-60s auth-retry hang then empty output.
# Gating on `-nt` lets the bare home keep its own self-rotating token lineage;
# we re-sync only when the operator deliberately updates the host credentials.
# The flock serializes concurrent bare runs that share one BARE_HOME (each run
# should set ECAA_EVAL_BARE_HOME to its own dir for true parallelism).
__seed_lock="$BARE_HOME/.cred-seed.lock"
(
  flock 9
  if [ -f "$HOME/.claude/.credentials.json" ]; then
    if [ ! -f "$BARE_HOME/.claude/.credentials.json" ] \
       || [ "$HOME/.claude/.credentials.json" -nt "$BARE_HOME/.claude/.credentials.json" ]; then
      install -m 600 "$HOME/.claude/.credentials.json" "$BARE_HOME/.claude/.credentials.json"
    fi
  fi
  if [ -f "$HOME/.claude.json" ]; then
    if [ ! -f "$BARE_HOME/.claude.json" ] \
       || [ "$HOME/.claude.json" -nt "$BARE_HOME/.claude.json" ]; then
      cp -f "$HOME/.claude.json" "$BARE_HOME/.claude.json" 2>/dev/null || true
    fi
  fi
) 9>"$__seed_lock"

# Claude Code install mounted into the container (the bio image does not bundle
# it). Installed once into a cache dir; the node_modules tree is mounted ro.
CC_DIR="${ECAA_EVAL_BARE_CLAUDE_DIR:-$HOME/.ecaa-workflow/eval-bare-claude}"
if [ ! -e "$CC_DIR/node_modules/.bin/claude" ]; then
  mkdir -p "$CC_DIR"
  echo "_bare_agent.sh: installing @anthropic-ai/claude-code into $CC_DIR (one-time)..." >&2
  npm install --prefix "$CC_DIR" --silent --no-audit --no-fund \
    "@anthropic-ai/claude-code${ECAA_AGENT_CLAUDE_VERSION:+@$ECAA_AGENT_CLAUDE_VERSION}" >/dev/null 2>&1 || true
fi

# Eval fault-injection shim (STRICT no-op unless ECAA_EVAL_SHIM_DIR is set, so
# production runs are byte-identical). When set, the eval harness wants the
# fault to cross the container boundary: ro-mount the shim dir, rw-mount the
# per-cell state dir (the shim writes its bypass-detection marker there), PREPEND
# the shim dir to the container PATH so bwa/lofreq resolve to the shim FIRST, and
# forward the EVAL_INJECT_* contract env into the container.
CONTAINER_PATH="/opt/claude-code/node_modules/.bin:/opt/conda/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
DOCKER_SHIM_ARGS=()
if [ -n "${ECAA_EVAL_SHIM_DIR:-}" ]; then
  DOCKER_SHIM_ARGS+=(
    -v "$ECAA_EVAL_SHIM_DIR":"$ECAA_EVAL_SHIM_DIR":ro
    -v "$EVAL_INJECT_STATE":"$EVAL_INJECT_STATE":rw
    -e EVAL_INJECT_PATTERN
    -e EVAL_INJECT_TARGET
    -e EVAL_INJECT_STATE
  )
  CONTAINER_PATH="$ECAA_EVAL_SHIM_DIR:$CONTAINER_PATH"
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
  "${DOCKER_SHIM_ARGS[@]}" \
  -e PATH="$CONTAINER_PATH" \
  -w "$WORKDIR" \
  -e "HOME=$HOME" \
  "$IMAGE" \
  claude --dangerously-skip-permissions --output-format=json --model "$MODEL" -p "$PROMPT"
