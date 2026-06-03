# Empirical-evaluation campaign

Companion prose for `scripts/eval/campaign.toml`. The campaign produces the
deployed-PoC evidence under `docs/eval-results/`.

## Arms

- **ecaa** — natural-language description -> chat-intake -> compiled RO-Crate
  package -> real `ecaa-workflow-harness` + `scripts/agent-claude.sh`.
- **claude-direct** — the same Claude Code agent, bare instruction, no compiler.

(A third **ecaa-ungated** arm exists as offline scaffolding only — see E6. It is
NOT part of the default campaign and runs only under explicit operator gating.)

## Benchmarks

- **nekrutenko** — mtDNA variant calling. Deterministic (Jaccard vs the canonical
  VCF), no LLM judge. Run with the 36-cell PATH-shim fault matrix. Single task,
  so `--trials 10` is needed to reach the 10-pair power floor.
- **biomnibench** — 50 public BiomniBench-DA tasks, Gemini 3.1 Pro judge.

## Power

The bootstrap CI (`seed=1729`) is flagged `underpowered` below 10 paired
observations. The campaign targets >= 10 paired pairs so the flag clears.

## Verification

`python3 -m scripts.eval.verify_campaign <run_dir>` asserts a produced run's
scorecard satisfies this manifest (arms present, seed match, pair floor met,
deterministic vs judged benchmarks). Run it before publishing.
