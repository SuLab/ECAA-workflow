# Two-arm eval campaign — operator runbook

This run is **operator-gated** (AWS/cost + live API authority). This document and
`scripts/eval/campaign.toml` + `scripts/eval/verify_campaign.py` are the reviewable,
committed spec; the **run is never in CI** and is launched only by an operator.

## Arms

- `ecaa` — compiler -> typed package -> harness + agent.
- `claude-direct` — same agent, bare instruction (the `bare-codex` arm runs the
  same path with `ECAA_AGENT_BACKEND=codex`).

## Fairness invariants (enforced in code; do NOT override for a scored run)

- `ECAA_EVAL_NARRATIVE_AUGMENT=0` (default) — both arms feed the judge the SAME
  raw narrative; no ECAA-only structured-claims augmentation.
- Relaunch budget hard-pinned to 0; `ECAA_EVAL_ALLOW_RELAUNCH` MUST stay unset
  for scored runs (it is a diagnostic-only opt-in). Per-row `relaunch_count` is
  recorded in the scorecard.
- Both arms share one wall-clock ceiling via `ECAA_EVAL_HARNESS_TIMEOUT`.
- `ECAA_EVAL_MODEL` pins the same model to both arms; a `claude-*` id on the
  codex backend is rejected by `eval_model()`.
- Recipe lock: report the active policy when reading the delta. The Issue-4
  landing dropped the ECAA-only method lock, so `Nekrutenko.locked_methods`
  returns `[]` for both arms (free-vs-free) and the scorecard `method_lock` meta
  records `asymmetric: false`.

## Manifest invariants (committed spec)

`scripts/eval/campaign.toml` is the reviewable campaign spec. It pins:

- `seed = 1729` — mirrors `scorecard._BOOTSTRAP_SEED` so a re-rendered CI is
  reproducible.
- `min_paired_pairs = 10` — the paired-observation floor (`_MIN_POWER_PAIRS`).
  Below it, `paired_delta_summary` flags `underpowered`.
- `arms = ["ecaa", "claude-direct"]` — both arms are required.
- `[run_env]` — `lift_budget_caps = true` plus `[run_env.budget_env]` carrying
  the per-stage Opus budgets the operator exports before launch.

The offline tests `test_campaign_manifest.py`, `test_campaign_manifest_ready.py`,
and `test_datasets_lock_frozen.py` assert these invariants (run via
`make eval-tests`).

## Run steps

1. Pin datasets: confirm `scripts/eval/datasets.lock` carries only frozen 40-hex
   SHAs (the offline test `test_datasets_lock_frozen.py` asserts this). Do NOT
   edit the pinned SHAs to rerun; a new pin is a deliberate revision.
2. Lift budget caps (from `campaign.toml [run_env.budget_env]`):
   ```
   export ECAA_AGENT_BUDGET_USD_DISCOVER=3.00
   export ECAA_AGENT_BUDGET_USD_VALIDATE=1.25
   export ECAA_AGENT_BUDGET_USD_DATA_ACQ=2.00
   export ECAA_AGENT_BUDGET_USD_ANALYTICAL=3.00
   ```
3. Launch each benchmark (n>=10 paired):
   ```
   ECAA_EVAL_LIVE=1 GEMINI_API_KEY=... ECAA_ANTHROPIC_API_KEY=... \
     python -m scripts.eval.eval_runner nekrutenko \
       --arms ecaa,claude-direct --trials 10
   ECAA_EVAL_LIVE=1 GEMINI_API_KEY=... ECAA_ANTHROPIC_API_KEY=... \
     python -m scripts.eval.eval_runner biomnibench \
       --arms ecaa,claude-direct --trials 3
   ```
4. Verify before publishing:
   ```
   python -m scripts.eval.verify_campaign <run_dir>
   ```
   This REJECTS fake/single-arm/empty/degenerate scorecards and unfrozen
   provenance datasets-locks. A non-zero exit means the evidence is not
   publishable.

## Value-prose gate

The value-prose gate: no value claim ("ECAA beats bare on benchmark X") may be
written into the paper,
README, or any tracked doc until a committed **non-fake two-arm** scorecard
exists and `verify_campaign.py` exits 0 on its run dir. A single-arm,
empty, or degenerate (constant-`overall`) scorecard does NOT satisfy this gate,
nor does one whose provenance datasets-lock carries a floating ref.
