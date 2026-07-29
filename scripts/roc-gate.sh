#!/usr/bin/env bash
# roc-gate.sh — execution-aware strict roc-validator regression gate.
#
# WHAT IT DOES
# ============
# 1. Plan crates (emitted by build-fixture-packages.sh from wrroc-fixtures):
#    asserts ro-crate-1.1 PASSES. The plan crate correctly claims only the
#    3-profile plan set {ro-crate-1.1, workflow-ro-crate/1.0, ecaa/v0.2};
#    roc-validator only recognises ro-crate-1.1 of those, so ONLY that profile
#    is tested — the plan crate does NOT spuriously fail because it (correctly)
#    does not claim the WRROC run-crate profiles.
#
# 2. A fully-scripted executed crate produced by the offline driver
#    (fresh_executed_crate.rs via ECAA_FRESH_EXECUTED_CRATE_OUT):
#    asserts ALL FOUR profiles pass:
#      ro-crate-1.1, process-run-crate-0.5,
#      workflow-run-crate-0.5, provenance-run-crate-0.5.
#    This proves the emitter CAN produce a fully-conformant executed crate.
#
# 3. "Gate bites" proof: remove each executed action's recorded instrument,
#    assert roc-validate-strict.py exits non-zero, then discard the temp copy.
#
# HONEST RESIDUAL
# ===============
# Real packages (e.g. testdata/replay/himes-parent) may contain script-less
# tasks (e.g. data_acquisition records a download with no executor script).
# Such tasks produce no `instrument` on their CreateAction, so the real package
# does NOT fully pass provenance-run-crate-0.5. This gate's executed-crate
# conformance proof therefore uses the offline driver (fully scripted) and
# documents the residual: real packages conform to the extent their producing
# tasks record scripts.
#
# BUILD PROTOCOL
# ==============
# The gate must run on a freshly-built binary. The caller is responsible for
# running `cargo build --bin ecaa-workflow` before invoking this script.
# PATH must have the target/debug directory prepended so the freshly-built
# ecaa-workflow binary is used for fixture emission.
#
# Usage:
#   PATH="$PWD/target/debug:$PATH" bash scripts/roc-gate.sh
#
# Exit code: 0 iff all checks pass; non-zero on any failure.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
VENV_PY="$REPO_ROOT/.venv-validator/bin/python"
VALIDATOR="$SCRIPT_DIR/roc-validate-strict.py"

if [[ ! -x "$VENV_PY" ]]; then
    echo "[roc-gate] ERROR: validator venv not found at $VENV_PY" >&2
    echo "  Run: python3 -m venv .venv-validator && .venv-validator/bin/pip install -r requirements-validator.txt" >&2
    exit 1
fi

# ── Scratch space ────────────────────────────────────────────────────────────
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

PLAN_OUT="$TMP_DIR/plan-packages"
EXEC_OUT="$TMP_DIR/executed-crate"

echo "======================================================================"
echo "[roc-gate] T8 strict roc-validator regression gate"
echo "======================================================================"
echo ""

# ── Step 1: Build fresh plan crates ─────────────────────────────────────────
echo "[roc-gate] Step 1: emit plan crates from wrroc-fixtures"
echo "  (build-fixture-packages.sh → plan crate with 3-profile conformsTo)"

if ! command -v ecaa-workflow >/dev/null 2>&1; then
    echo "[roc-gate] ERROR: ecaa-workflow not on PATH — run:" >&2
    echo "  cargo build --bin ecaa-workflow && PATH=\"\$PWD/target/debug:\$PATH\" bash scripts/roc-gate.sh" >&2
    exit 1
fi

bash "$SCRIPT_DIR/build-fixture-packages.sh" \
    "$REPO_ROOT/testdata/wrroc-fixtures" \
    "$PLAN_OUT" 2>&1 | tee "$TMP_DIR/plan-build.log" | grep -E "^(Building|Done|FAILED)"

# Validate ONE representative plan crate (faster; they are all structurally
# identical — same emitter, same profile set).
PLAN_SAMPLE="$(ls -d "$PLAN_OUT"/*/  | head -1)"
echo ""
echo "[roc-gate] Plan crate sample: $PLAN_SAMPLE"
echo "  Expectation: ro-crate-1.1 PASSES; WRROC run profiles NOT tested"
echo "  (plan crate claims only {ro-crate-1.1, workflow-ro-crate/1.0, ecaa/v0.2})"
echo ""

if ! "$VENV_PY" "$VALIDATOR" "$PLAN_SAMPLE"; then
    echo "[roc-gate] FAIL: plan crate did not pass ro-crate-1.1" >&2
    exit 1
fi
echo "[roc-gate] Step 1 PASS: plan crate passes ro-crate-1.1"
echo ""

# ── Step 2: Produce a fully-scripted executed crate via the offline driver ───
echo "[roc-gate] Step 2: produce fully-scripted executed crate (offline driver)"
echo "  (cargo test fresh_executed_crate_satisfies_provenance_shape with"
echo "   ECAA_FRESH_EXECUTED_CRATE_OUT=$EXEC_OUT)"

mkdir -p "$EXEC_OUT"
ECAA_FRESH_EXECUTED_CRATE_OUT="$EXEC_OUT" \
    cargo test \
    -p ecaa-workflow-core \
    fresh_executed_crate_satisfies_provenance_shape \
    -- --nocapture 2>&1 | tee "$TMP_DIR/executed-build.log" | grep -E "^(test .* ok|dumped |error:|.*FAILED.*)"

echo ""
echo "[roc-gate] Executed crate output: $EXEC_OUT"
echo "  Expectation: ALL FOUR profiles PASS"
echo "  (ro-crate-1.1, process-run-crate-0.5, workflow-run-crate-0.5,"
echo "   provenance-run-crate-0.5)"
echo ""
echo "  HONEST RESIDUAL: real packages (e.g. himes-parent) contain script-less"
echo "  tasks (data_acquisition records a download, not a compute step) and"
echo "  therefore DO NOT fully pass provenance-run-crate-0.5. This gate uses"
echo "  the offline driver (fully scripted) to prove the emitter CAN produce"
echo "  a conformant executed crate. Real packages conform to the extent their"
echo "  producing tasks record scripts."
echo ""

if ! "$VENV_PY" "$VALIDATOR" "$EXEC_OUT"; then
    echo "[roc-gate] FAIL: executed crate did not pass all four profiles" >&2
    exit 1
fi
echo "[roc-gate] Step 2 PASS: fully-scripted executed crate passes all four profiles"
echo ""

# ── Step 3: Prove the gate bites ─────────────────────────────────────────────
echo "[roc-gate] Step 3: prove the gate bites (inject a break → expect FAIL)"
echo "  Mutation: remove instrument from executed CreateActions in a temp copy"

BROKEN_OUT="$TMP_DIR/broken-executed-crate"
cp -r "$EXEC_OUT" "$BROKEN_OUT"

"$VENV_PY" - "$BROKEN_OUT" <<'EOF'
import json, sys
from pathlib import Path

broken_dir = Path(sys.argv[1])
meta = broken_dir / "ro-crate-metadata.json"
with meta.open() as f:
    doc = json.load(f)
graph = doc["@graph"]
# Remove the `instrument` field from every CreateAction. This breaks the
# provenance-run-crate-0.5 "Tool inverse instrument" REQUIRED shape: every
# tool in the workflow's hasPart must be the instrument of a real CreateAction.
# roc-validator enforces this at REQUIRED severity.
removed = 0
for entity in graph:
    t = entity.get("@type", "")
    is_create_action = (t == "CreateAction") or (isinstance(t, list) and "CreateAction" in t)
    if is_create_action and "instrument" in entity:
        del entity["instrument"]
        removed += 1
doc["@graph"] = graph
with meta.open("w") as f:
    json.dump(doc, f, indent=2)
print(f"  Mutated: removed 'instrument' from {removed} CreateAction(s)")
print(f"  Expected failure: provenance-run-crate-0.5 'Tool inverse instrument' REQUIRED check")
EOF

set +e
"$VENV_PY" "$VALIDATOR" "$BROKEN_OUT"
BREAK_EXIT=$?
set -e

if [[ $BREAK_EXIT -eq 0 ]]; then
    echo "[roc-gate] FAIL: gate did NOT bite on broken crate — check logic" >&2
    exit 1
fi
echo "[roc-gate] Step 3 PASS: gate bites — broken crate exited $BREAK_EXIT (non-zero, expected)"
echo ""

# ── All steps passed ─────────────────────────────────────────────────────────
echo "======================================================================"
echo "[roc-gate] ALL STEPS PASSED"
echo "  Step 1: plan crate ro-crate-1.1 PASS (WRROC run profiles correctly skipped)"
echo "  Step 2: executed crate all-four profiles PASS"
echo "  Step 3: gate bites on deliberate break (exit $BREAK_EXIT)"
echo "======================================================================"
exit 0
