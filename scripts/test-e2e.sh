#!/usr/bin/env bash
# test-e2e.sh — End-to-end smoke test for the ECAA-workflow compiler.
# Tests the compile-time path only — no live agent or server required.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

PASS=0
FAIL=0
OUT_DIR="$(mktemp -d /tmp/scripps-e2e-XXXXXX)"
trap 'rm -rf "$OUT_DIR"' EXIT

# shellcheck source=lib/test-helpers.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/test-helpers.sh"

# ── Step 1: Build ─────────────────────────────────────────────────────────────

echo ""
echo "▸ Step 1: cargo build --workspace"
if cargo build --workspace -q 2>&1; then
  ok "workspace builds"
else
  fail "workspace build failed"
  exit 1
fi

# ── Step 2: Unit tests ────────────────────────────────────────────────────────

echo ""
echo "▸ Step 2: cargo test -p ecaa-workflow-core"
TEST_OUT=$(cargo test -p ecaa-workflow-core 2>&1)
PASS_COUNT=$(echo "$TEST_OUT" | grep -oP 'test result: ok\. \K[0-9]+' | head -1)
if echo "$TEST_OUT" | grep -q "FAILED"; then
  fail "core tests FAILED"
  echo "$TEST_OUT" | grep "FAILED"
else
  ok "core tests: ${PASS_COUNT:-?} passed"
fi

# ── Step 3: CLI build command ─────────────────────────────────────────────────

echo ""
echo "▸ Step 3: CLI emit via bulk_rnaseq_de archetype"
ARCHETYPE="config/archetypes/bulk_rnaseq_de.yaml"

if [[ ! -f "$ARCHETYPE" ]]; then
  fail "archetype not found: $ARCHETYPE"
  exit 1
fi

if cargo run -q -p ecaa-workflow-cli --bin ecaa-workflow -- build \
    --archetype "$ARCHETYPE" \
    --output "$OUT_DIR" 2>&1; then
  ok "CLI build succeeded"
else
  fail "CLI build failed"
  exit 1
fi

# ── Step 4: Required files ────────────────────────────────────────────────────

echo ""
echo "▸ Step 4: Required output files"
assert_file "WORKFLOW.json"
assert_file "ro-crate-metadata.json"
assert_file "PROMPT.md"
assert_file "CONTEXT.md"
assert_file "runtime/LOG.jsonl"

# ── Step 5: WORKFLOW.json structure ──────────────────────────────────────────

echo ""
echo "▸ Step 5: WORKFLOW.json structure"
assert_json_key "WORKFLOW.json" "version"
assert_json_key "WORKFLOW.json" "workflow_id"
assert_json_key "WORKFLOW.json" "tasks"
assert_json_array_nonempty "WORKFLOW.json" "tasks"

# Compiler emission leaves tasks pending; the harness derives initial
# readiness from dependency structure. Assert the emitted DAG is structurally
# dispatchable instead of pinning a stale state convention.
DAG_SHAPE=$(python3 - "$OUT_DIR/WORKFLOW.json" <<'PY' 2>/dev/null || true
import json, sys
wf = json.load(open(sys.argv[1]))
tasks = wf.get("tasks", {}) or {}
failures = []
if not tasks:
    failures.append("zero tasks")
valid_states = {"pending", "ready", "running", "completed", "failed", "blocked"}
for tid, task in tasks.items():
    state = ((task or {}).get("state") or {}).get("status")
    if state not in valid_states:
        failures.append(f"{tid}: invalid state {state!r}")
deps = {tid: set((task or {}).get("depends_on") or []) for tid, task in tasks.items()}
children = {tid: set() for tid in tasks}
for child, parents in deps.items():
    for parent in parents:
        if parent not in tasks:
            failures.append(f"{child}: missing dependency {parent}")
        else:
            children[parent].add(child)
isolated = sorted(tid for tid in tasks if len(tasks) > 1 and not deps[tid] and not children[tid])
if isolated:
    failures.append(f"isolated nodes {isolated}")
roots = [tid for tid, parents in deps.items() if not parents]
if not roots:
    failures.append("no root tasks")
indeg = {tid: len(parents) for tid, parents in deps.items()}
queue = [tid for tid, degree in indeg.items() if degree == 0]
visited = 0
while queue:
    node = queue.pop()
    visited += 1
    for child in children.get(node, set()):
        indeg[child] -= 1
        if indeg[child] == 0:
            queue.append(child)
if visited != len(tasks):
    failures.append("cycle detected")
if failures:
    print("; ".join(failures))
    sys.exit(1)
edge_count = sum(len(parents) for parents in deps.values())
print(f"{len(tasks)} tasks, {len(roots)} root task(s), {edge_count} edge(s)")
PY
)

if [[ "$DAG_SHAPE" == *"tasks"* ]]; then
  ok "WORKFLOW.json DAG is structurally dispatchable ($DAG_SHAPE)"
else
  fail "WORKFLOW.json DAG structure invalid${DAG_SHAPE:+: $DAG_SHAPE}"
fi

# ── Step 6: RO-Crate metadata ─────────────────────────────────────────────────

echo ""
echo "▸ Step 6: RO-Crate metadata"
assert_json_ld "ro-crate-metadata.json"

# Check for ComputationalWorkflow entity
HAS_WF=$(python3 -c "
import json
d=json.load(open('$OUT_DIR/ro-crate-metadata.json'))
types=[str(e.get('@type','')) for e in d.get('@graph',[])]
has='ComputationalWorkflow' in ' '.join(types)
print(1 if has else 0)
" 2>/dev/null || echo 0)

if [[ "$HAS_WF" == "1" ]]; then
  ok "ro-crate-metadata.json has ComputationalWorkflow entity"
else
  fail "ro-crate-metadata.json missing ComputationalWorkflow"
fi

# Check for HowToStep entities
STEP_COUNT=$(python3 -c "
import json
d=json.load(open('$OUT_DIR/ro-crate-metadata.json'))
n=[e for e in d.get('@graph',[]) if 'HowToStep' in str(e.get('@type',''))]
print(len(n))
" 2>/dev/null || echo 0)

if [[ "$STEP_COUNT" -gt 0 ]]; then
  ok "ro-crate-metadata.json has $STEP_COUNT HowToStep entities"
else
  fail "ro-crate-metadata.json missing HowToStep entities"
fi

# ── Step 7: PROMPT.md and CONTEXT.md non-empty ───────────────────────────────

echo ""
echo "▸ Step 7: Document content"
for f in PROMPT.md CONTEXT.md; do
  LINES=$(wc -l < "$OUT_DIR/$f" 2>/dev/null || echo 0)
  if [[ "$LINES" -gt 2 ]]; then
    ok "$f has content ($LINES lines)"
  else
    fail "$f is empty or too short"
  fi
done

# ── Summary ───────────────────────────────────────────────────────────────────

echo ""
echo "──────────────────────────────────────────────"
TOTAL=$((PASS + FAIL))
echo "Results: $PASS/$TOTAL passed"
if [[ "$FAIL" -eq 0 ]]; then
  echo "All checks passed."
  exit 0
else
  echo "$FAIL check(s) FAILED."
  exit 1
fi
