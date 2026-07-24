#!/usr/bin/env bash
# Hermetic test for the ensemble runtime-delivery contract in agent-claude.sh:
# the per-cell persona system-prompt and model tier are read from the emitted
# WORKFLOW.json (keyed by ECAA_TASK_ID) via the exact jq expressions the wrapper
# uses. No live `claude`/docker — this asserts the WORKFLOW.json → jq → value
# path (the load-bearing extraction), not the full agent invocation.
set -euo pipefail

command -v jq >/dev/null 2>&1 || { echo "SKIP: jq not on PATH"; exit 0; }

PKG="$(mktemp -d)"
trap 'rm -rf "$PKG"' EXIT

PERSONA='You are a skeptical reviewer arguing the null. Anchor every claim to a result-table row or a cited PMID.'
cat > "$PKG/WORKFLOW.json" <<JSON
{
  "tasks": {
    "biological_interpretation__m_deseq2__lens_skeptical": {
      "spec": {
        "ensemble_variant": {
          "axis": "interpretive",
          "method_variant": "deseq2",
          "lens": "skeptical",
          "model_tier": "opus",
          "persona_system_prompt": "$PERSONA"
        }
      }
    },
    "raw_qc": { "spec": { "atom_id": "raw_qc" } }
  }
}
JSON

fail() { echo "FAIL: $1"; exit 1; }

# --- ensemble interpretation cell: persona + tier resolve ---
ECAA_TASK_ID="biological_interpretation__m_deseq2__lens_skeptical"
persona="$(jq -r --arg tid "$ECAA_TASK_ID" '.tasks[$tid].spec.ensemble_variant.persona_system_prompt // empty' "$PKG/WORKFLOW.json")"
[ "$persona" = "$PERSONA" ] || fail "persona_system_prompt not extracted (got: '$persona')"

tier="$(jq -r --arg tid "$ECAA_TASK_ID" '.tasks[$tid].spec.ensemble_variant.model_tier // empty' "$PKG/WORKFLOW.json")"
[ "$tier" = "opus" ] || fail "model_tier not extracted (got: '$tier')"
if [ "$tier" = "opus" ]; then model="claude-opus-4-8"; else model="claude-sonnet-4-6"; fi
[ "$model" = "claude-opus-4-8" ] || fail "opus tier did not map to claude-opus-4-8 (got: '$model')"

# --- non-ensemble task: both empty (wrapper falls back to defaults) ---
ECAA_TASK_ID="raw_qc"
persona="$(jq -r --arg tid "$ECAA_TASK_ID" '.tasks[$tid].spec.ensemble_variant.persona_system_prompt // empty' "$PKG/WORKFLOW.json")"
[ -z "$persona" ] || fail "non-ensemble task must yield empty persona (got: '$persona')"
tier="$(jq -r --arg tid "$ECAA_TASK_ID" '.tasks[$tid].spec.ensemble_variant.model_tier // empty' "$PKG/WORKFLOW.json")"
[ -z "$tier" ] || fail "non-ensemble task must yield empty model_tier (got: '$tier')"

echo "PASS: ensemble runtime-delivery jq contract (persona + model_tier extraction, non-ensemble fallback)"
