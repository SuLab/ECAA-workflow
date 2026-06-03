# Committed eval evidence

This directory is the **only** tracked home for empirical-evaluation evidence.
Everything else the eval harness writes lands under `runtime/eval-runs/`, which
is gitignored (operator scratch). Files here are the durable, redacted,
provenance-stamped public scorecards an external reader can re-derive.

## What lands here

- `<benchmark>-<date>/scorecard.public.md` + `scorecard.public.json` — the
  cost-redacted public scorecard, stamped with `git_head`, `datasets_lock`,
  `seed`, `arms`, and `trials`. Raw `total_cost_usd` / wall-clock are stripped.
- `schema-burden.json` + `schema-burden.md` — the offline schema-authoring-burden
  analyzer output (`make schema-burden`).
- `CAMPAIGN.md` — the operator-run campaign spec (companion to
  `scripts/eval/campaign.toml`).

## How it is produced

1. (operator-gated) `make eval-full` runs the live campaign into `runtime/eval-runs/`.
2. (code-only) `make eval-publish RUN=<run_dir>` copies the `.public.*` files here.
3. (code-only) `python3 -m scripts.eval.verify_campaign <run_dir>` asserts the
   committed scorecard satisfies `scripts/eval/campaign.toml`.

The live run in step 1 requires `ECAA_EVAL_LIVE=1` + GEMINI/Anthropic keys +
AWS/harness authority and is run by a human operator, never by an assistant.
