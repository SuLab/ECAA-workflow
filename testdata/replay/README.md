# `testdata/replay/` — Himes replay fixture

This directory holds a **replay-machinery test fixture**, not a scientific
deliverable. It exists so `crates/core/tests/provenance/replay_himes.rs` can
exercise `run_replay` (re-verify + re-execute) offline against a real bulk
RNA-seq package.

## What is here

| Path | What it is |
|---|---|
| `himes-parent/` | A **trimmed two-stage slice** of the paper's Himes case-study package (session `57bd1b53-…`, `bulk_rnaseq-20260711T154822`). Only the `data_acquisition` and `differential_expression` outputs — plus the policies and runtime sidecars the two replay tests read — survive. It is **not** a full package. |
| `himes-golden-report.json` | The pinned stable subset of the `ReplayReport` that the verify-tier test compares against (verdict, `reader_matches_writer`, and each check's `recorded`/`fresh`/`diverged`). Regenerate with `ecaa-workflow replay himes-parent --tier verify --json …` after any intentional change to the fixture. |

The `01_deseq2_de.R` script in the fixture resolves its package root from the
`PACKAGE`/`PKG_ROOT` env vars that `replay::script_runner` injects (with a
script-relative fallback), so the execute tier can stage and re-run it from a
scratch directory in the `bio-min` container.

## Which "Himes" is which

Three Himes artifacts live in this repo; they are **different things**:

1. **`testdata/replay/himes-parent/` (this dir)** — the trimmed replay slice of
   the paper's case-study run (`57bd1b53`): 22 tasks in the full run, apeglm LFC
   shrinkage, **72 candidate claims** (25 verified / 47 unverifiable), 4,030
   significant genes. These numbers reconcile with the paper.
2. **`testdata/emitted-packages/09-himes-dex-airway/`** — a 33-task,
   **from-FASTQ, compile-only** composition example with **0 executed claims**.
   It demonstrates DAG emission for a raw-reads entry point; it is neither the
   paper run nor an executed package.
3. **The full case-study package** (≈595 entities, including non-regenerable
   network/literature outputs) is **not committed** to `testdata/` — it is too
   large and carries one-time agentic products. It is the canonical artifact
   behind the paper and is being prepared for a Zenodo deposit; cite that DOI
   for the paper's numbers.

## A note on "reproducible"

The **compute path** is reproducible: the execute tier re-runs
`01_deseq2_de.R` in `bio-min` and reproduces `de_results.tsv`
(byte-identical / semantically equivalent). The **claim set**, by contrast, is
a one-time, non-deterministic product of the agentic narrative-generation step
(see GitHub issue #5) — it is an **inspectable recorded artifact**, not a
regenerable one. Do not describe the claim results as "reproducible"; describe
them as recorded and re-verifiable against the committed tables.
