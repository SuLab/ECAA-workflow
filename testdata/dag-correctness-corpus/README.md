# Blinded DAG-correctness corpus

This directory holds a blinded corpus for testing the **ecaa-workflow DAG
composer**. Each scenario in `MANIFEST.yaml` pairs:

1. a **blinded SME-style prompt** scrubbed of every identifying fingerprint
   (author, title, journal, accession, year, cohort, tool-version pins), and
2. a **machine-checkable expected DAG** the harness diffs against the
   compiler's emitted `WORKFLOW.json`.

The corpus is the load-bearing oracle for "does the deterministic composer
recover the correct workflow shape from the analysis shape alone — without
recognising the source study?"

## Why blinded

The LLM-mediated intake (`crates/conversation`) is a UX shim around a
deterministic compiler: it classifies the SME's natural-language request, picks
an archetype, slot-fills it, and the compiler emits a workflow DAG. If a prompt
could be identified as a specific known study, the test would instead measure
"does the model remember what those authors did." The right test is "given only
the SME-readable analysis shape, does the deterministic composer pick the right
archetype and emit the right atoms?" — so every prompt is scrubbed to the level
a working biologist would use in chat (modality, platform, organism, reference
genome, rounded sample size, standard deliverables) and no further.

## Tier A vs. Tier B

- **Tier A — covered archetypes.** ≥2 scenarios per archetype. The composer is
  expected to recover the right archetype, atom set, and structural shape.
  `expected_modality` is filled, and `forbidden_atoms` catches misroutes
  between related archetypes (bulk ATAC vs. scATAC, MOFA vs. DIABLO vs. SNF).
- **Tier B — flexibility tests.** Scenarios whose modality/method is outside
  the archetype catalog (Hi-C TADs, CyTOF, Mendelian randomization, 10x scATAC,
  snmC-seq2, Slide-seq, CODEX, survival, microbiome strain-phylogenetics,
  cryo-EM). `expected_modality: null`. The composer must fall back to
  `generic_omics` plus a `propose_hypothesized_node` for the missing capability,
  or refuse with an explicit blocker — and must NOT silently misroute to the
  nearest superficially-similar archetype.

## Running it

`run_corpus.py` drives the **deterministic compiler path** (`ecaa-workflow
intake`) — no LLM, no server — and diffs each emitted DAG against `expected_dag`:

```bash
cargo build -p ecaa-workflow-cli --bin ecaa-workflow
python3 testdata/dag-correctness-corpus/run_corpus.py            # all 76
python3 testdata/dag-correctness-corpus/run_corpus.py --filter tier:A
python3 testdata/dag-correctness-corpus/run_corpus.py --filter bulk_rnaseq_de
python3 testdata/dag-correctness-corpus/run_corpus.py --strict-structure
```

A scenario PASSES when every `required_atoms` entry is present in the emitted
task set and no `forbidden_atoms` entry is present. By default task-count bounds
and `structural_constraints` surface as warnings; `--strict-structure` promotes
them to failures and additionally checks the emitted graph for missing
dependency targets, cycles, and isolated nodes.

> **Tier-B note:** the `propose_hypothesized_node` fallback is an
> LLM-conversation-layer feature that the deterministic `intake` path does not
> run, so Tier-B proposal-capability assertions are not exercised by this
> offline driver (atom coverage is still checked). Use the conversation/server
> harness to test the full Tier-B contract.

## Files

| File | Purpose |
|---|---|
| `MANIFEST.yaml` | The corpus: blinded prompts + expected DAGs. |
| `run_corpus.py` | Offline driver: runs `ecaa-workflow intake` per scenario and diffs. |
| `_harness_lib.py` | Reusable `corpus_load_validator` + `evaluate_dag` + structural-constraint helpers. |

## Adding a scenario

1. Pick a real, verifiable published analysis. Confirm it resolves; never
   fabricate.
2. Write the `blinded_prompt` as an SME would chat the planner. Strip author /
   lab / cohort / institute names, the year, accession IDs, and any
   uniquely-identifying tool-version pin. Round sample counts.
3. Fill `expected_dag.required_atoms` against the atom IDs in
   `config/stage-atoms/`, seeded by the archetype shape in
   `config/archetypes/<archetype>.yaml`.
4. Fill `forbidden_atoms` with the atoms that catch the most likely misroute.
5. Set tight-but-charitable `min_task_count` / `max_task_count` bounds
   (~±2 of the archetype's baseline; wider for per-sample fan-out).
6. Write `structural_constraints` as plain-English invariants the harness can
   implement.
7. Use a stable kebab-case `id`, unique within `MANIFEST.yaml`.

Duplicate-id check:

```bash
yq '.scenarios[].id' MANIFEST.yaml | sort | uniq -d   # non-empty = bug
```
