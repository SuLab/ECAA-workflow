#!/usr/bin/env bash
# test-chat-confirm.sh — Regression test for the awaiting_confirm auto-proceed
# behavior in scripps-workflow chat.
#
# Scenario: an SME starts with a terse, low-keyword intake line that classifies
# below the 0.5 confidence gate (triggering awaiting_confirm mode), then keeps
# describing without typing "yes". The second line is keyword-dense enough to
# push confidence over the gate, so the chat must auto-exit confirm mode, build
# the DAG, and accept subsequent /resolve + /emit commands — no literal "yes"
# required.
#
# Prior to the fix, the REPL got stuck in a "Tell me more about your workflow"
# loop: subsequent prose was appended silently without re-classifying, so the
# SME could never escape confirm mode without typing the exact token "yes".

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

PASS=0
FAIL=0
OUT_DIR="$(mktemp -d /tmp/scripps-chat-confirm-XXXXXX)"
LOG_FILE="$(mktemp /tmp/scripps-chat-confirm-log-XXXXXX.txt)"
SESSION_FILE="$(mktemp /tmp/scripps-chat-confirm-sess-XXXXXX.txt)"

if [[ -n "${KEEP_PACKAGE:-}" ]]; then
  ln -sfn "$OUT_DIR" /tmp/scripps-chat-confirm-latest
  ln -sfn "$LOG_FILE" /tmp/scripps-chat-confirm-latest-log
  echo "Package preserved at /tmp/scripps-chat-confirm-latest"
  echo "Log preserved at     /tmp/scripps-chat-confirm-latest-log"
else
  trap 'rm -rf "$OUT_DIR" "$LOG_FILE" "$SESSION_FILE"' EXIT
fi

# ── Helpers ──────────────────────────────────────────────────────────────────
# Shared assertion helpers live in scripts/lib/test-helpers.sh.
# shellcheck source=lib/test-helpers.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/test-helpers.sh"

# ── Scripted SME session ─────────────────────────────────────────────────────
# Line 1: vague "I want to analyze RNA data" — expected to classify below 0.5
#  confidence and enter awaiting_confirm mode.
# Line 2: keyword-dense description that on its own pushes confidence over the
#  0.5 gate. Under the fix, the REPL must auto-proceed here without any
#  explicit "yes" token.
# Lines 3-4: normal /resolve + /emit flow to prove the DAG was built and is
#  writable. If the REPL were still stuck in awaiting_confirm, /emit
#  would report "No workflow defined yet" and the package dir would
#  remain empty.

cat > "$SESSION_FILE" <<'EOF'
I'm exploring some scRNA data from aging human samples for an initial look.
Specifically, it's a single-cell RNA-seq meta-analysis processed from 10x Chromium Cell Ranger matrices and analyzed with Seurat and scanpy; I also want to do differential expression between conditions for this scRNA project.
/resolve data_acquisition Fetch public scRNA-seq datasets from GEO
/emit
/quit
EOF

echo ""
echo "Chat auto-proceed from low-confidence Test"
echo "=========================================="
echo ""

echo "Step 1: cargo build --workspace"
if cargo build --workspace -q 2>/dev/null; then
  ok "workspace builds"
else
  fail "workspace build failed"
  cargo build --workspace 2>&1 | tail -20
  exit 1
fi

echo ""
echo "Step 2: Run chat with scripted low-then-high confidence session"
if cargo run -q -p scripps-workflow-cli --bin scripps-workflow -- chat --output "$OUT_DIR" \
    < "$SESSION_FILE" > "$LOG_FILE" 2>&1; then
  ok "chat command exited cleanly"
else
  fail "chat command failed (exit non-zero)"
  tail -30 "$LOG_FILE"
  exit 1
fi

echo ""
echo "Step 3: Low-confidence line triggered the confirmation prompt"
if grep -q "confidence: low" "$LOG_FILE" || grep -q "Is this correct?" "$LOG_FILE"; then
  ok "first line hit the low-confidence gate"
else
  fail "first line did not trigger confirmation prompt — test is not exercising the fix"
  echo "  (tune line 1 in the session to be vaguer so it classifies below 0.5)"
  cat "$LOG_FILE"
  exit 1
fi

echo ""
echo "Step 4: Second line auto-proceeded without explicit 'yes'"
# The second line should classify at medium/high confidence. The fix prints
# the Classified: banner via process_classification, which includes
# "Draft workflow (N tasks" for single-cell. The single-cell taxonomy was
# extended to cover lotz M09-M12 (interpretation, trajectory,
# cell-cell communication, final reporting), so N is in the 30s. The
# grep below doesn't pin a specific count.
if grep -q "Draft workflow" "$LOG_FILE"; then
  ok "draft workflow printed — auto-proceeded out of awaiting_confirm"
else
  fail "no Draft workflow banner — chat stuck in awaiting_confirm mode"
  echo ""
  echo "--- log ---"
  cat "$LOG_FILE"
  exit 1
fi

if grep -q "single_cell_rnaseq" "$LOG_FILE"; then
  ok "classified as single_cell_rnaseq after line 2"
else
  fail "did not classify as single_cell_rnaseq"
fi

echo ""
echo "Step 5: /resolve worked (DAG was actually built)"
if grep -q "✓ discover_data_acquisition — resolved" "$LOG_FILE"; then
  ok "/resolve data_acquisition succeeded"
else
  fail "/resolve did not mark the discovery task resolved — DAG was not built"
fi

echo ""
echo "Step 6: /emit produced a package"
if [[ -f "$OUT_DIR/WORKFLOW.json" ]]; then
  ok "WORKFLOW.json emitted"
else
  fail "WORKFLOW.json missing — /emit did not fire"
  cat "$LOG_FILE"
  exit 1
fi

if [[ -f "$OUT_DIR/PROMPT.md" ]]; then
  ok "PROMPT.md emitted"
else
  fail "PROMPT.md missing"
fi

if [[ -f "$OUT_DIR/CONTEXT.md" ]]; then
  ok "CONTEXT.md emitted"
else
  fail "CONTEXT.md missing"
fi

echo ""
echo "Step 7: Confirmation output is informative (updated confidence shown)"
# The fix reprints the banner each time the SME keeps describing from inside
# confirm mode. Even though line 2 auto-proceeds here, the prompt after line 1
# must still have been visible to a human SME.
if grep -q "Is this correct?" "$LOG_FILE"; then
  ok "confirmation prompt was shown after line 1"
else
  fail "confirmation prompt never printed"
fi

echo ""
echo "======================================================"
TOTAL=$((PASS + FAIL))
echo "Chat Confirm Test Results: $PASS/$TOTAL passed"
if [[ "$FAIL" -eq 0 ]]; then
  echo "All chat auto-proceed checks passed."
  exit 0
else
  echo "$FAIL check(s) FAILED."
  echo "Log file: $LOG_FILE"
  exit 1
fi
