# v4 parity corpus

Phase 4 of [`docs/2026-05/dag-composer-gap-closure-and-v4-only-2026-05-08.md`](../../../../docs/2026-05/dag-composer-gap-closure-and-v4-only-2026-05-08.md).

Cross-version parity baseline across the eight canonical scenarios. The corpus exists to gate Phase 5 (the v4 default flip) — Phase 5 cannot land until every scenario in this directory is GREEN.

## Scenarios

| Scenario | Modality | Project class | Notes |
|---|---|---|---|
| `bulk-rnaseq` | `bulk_rnaseq` | bioinformatics | Mirrors the IVD load-bearing scenario |
| `scrnaseq` | `single_cell_rnaseq` | bioinformatics | scRNA-seq clustering + cell-type annotation |
| `variant-calling` | `variant_calling` | bioinformatics | Germline short-variant calling vs GIAB |
| `chip-seq` | `chip_seq` | bioinformatics | TF-binding peak calling |
| `atac-seq` | `atac_seq` | bioinformatics | Chromatin accessibility |
| `cross-omics` | `bulk_rnaseq + proteomics` | bioinformatics | Cross-omics shared-signal analysis |
| `time-series` | `generic_omics` | time_series_forecast | SARIMA forecast |
| `clinical-trial` | `generic_omics` | clinical_trial | Phase III endpoint analysis |

Request prose lives at [`testdata/v4-parity/<scenario>/request.txt`](../../../../testdata/v4-parity/).

## Layout

```
v4-parity/
├── README.md                 (this file)
├── PARITY_STATUS.md          Phase 4.5 Task E — central executive summary of per-scenario verdicts + Phase 5 readiness
└── <scenario>/
    ├── v2-baseline.json      Committed v2-archetype WORKFLOW.json (or legacy-taxonomy build when v2 dispatch ties)
    └── v4-emission/
        ├── WORKFLOW.json     Regenerated v4 WORKFLOW.json (when v4 emits a DAG)
        ├── TRIAGE.md         Operator-authored triage verdict (ACCEPTABLE / GAPS / GREEN). Regen-stable — `make v4-parity-emit` does not touch this.
        └── GAP.txt           Present iff v4 emission required the direct-path fallback OR the parity test recorded an atom-set diff. Regen rewrites GAP.txt for direct-path-fallback scenarios on every cycle; the parity test self-appends the atom-set diff for any scenario whose v4 ⊋ v2.
```

## Workflow

### Regenerate

```
make v4-parity-emit
```

This runs the `regenerate_baselines` ignored test, which:

1. Reads each `request.txt` and runs the modality + project-class classifier.
2. Composes at `composer_version=2` via `compose_with_version_and_modalities_full`. On `TieRequiresSmeDecision` / `CompositionInfeasible`, falls back to the legacy taxonomy build (`build_dag`) — the same downgrade path the conversation crate's `try_build_via_composer` uses today.
3. Composes at `composer_version=4` via the same dispatch entry point. On failure, retries with a hand-crafted `PlanningContext` that wires `modality`, `project_class`, and `available_data` (since `compose_v4_dispatch_full` does not currently thread these). When the planner returns a DAG that fails `validate_dag` (cycles), records the cycle in `GAP.txt` and writes the unvalidated DAG to `WORKFLOW.json` so the parity check can compare atom sets anyway.
4. Writes both files to disk under `tests/conversation-fixtures/fixtures/v4-parity/`.

The Makefile target re-runs every regen from scratch (clears stale `GAP.txt` / `WORKFLOW.json` per scenario before re-emitting).

### Parity check

```
cargo test -p ecaa-workflow-core --test composer_v4_parity_corpus -- --ignored v4_parity_corpus_matches_v2_baseline
```

Reads `v2-baseline.json` and `v4-emission/WORKFLOW.json` for each scenario, computes `v2_atoms = WORKFLOW.json::tasks.keys()` and `v4_atoms = …`, and asserts:

- `only_in_v2 == ∅` — every v2 atom must appear in v4.
- `only_in_v4 ⊆ {x | x.starts_with("adapter_") || x.contains("_adapter")}` — v4-only extras must be adapter atoms (lossless lift).

Four terminal states per scenario:

| State | Trigger | Action |
|---|---|---|
| GREEN | Parity contract holds AND no `GAP.txt` / `TRIAGE.md` | Counted toward Phase 5 readiness |
| V4_GAP_DOCUMENTED | `GAP.txt` OR `TRIAGE.md` exists | Diff is appended to `GAP.txt`; not a test failure |
| SKIPPED | Fixture missing | Operator must run `make v4-parity-emit` |
| REGRESSION | Parity contract broken AND no `GAP.txt` / `TRIAGE.md` | **Test panics** |

Phase 5 (default flip from `composer_version=1` to `composer_version=4`) requires every scenario to be GREEN.

## Phase 4.5 Task E status (2026-05-08)

Per-scenario triage is captured in [`PARITY_STATUS.md`](PARITY_STATUS.md) — central executive summary — and in each `v4-emission/TRIAGE.md`. After Tasks A–D, the corpus is **2 ACCEPTABLE / 6 GAPS** vs the **≥6 of 8** Phase 5 readiness threshold. Phase 5 is **NOT READY**.

## Today's status (Phase 4 baseline, 2026-05-08)

All eight scenarios are V4_GAP_DOCUMENTED. Three distinct gaps surface:

1. **Production-path plumbing gap.** `compose_v4_dispatch_full` constructs a `PlanningContext` via `planning_context_for_goal` that leaves `modality`, `project_class`, and `available_data` empty. As a result:
   - `try_archetype_seed` runs without a modality hint and bails on the 5%-tie window when ≥2 archetypes share the goal's `(edam_data, edam_format, project_class)` triple.
   - `forward_search` starts with an empty frontier and produces no edges.
   - `meet_in_the_middle` returns `Disconnected`.
   - The planner emits `PartialDag { unresolved_gaps: ["no producer atom in registry produces <data:N>"] }`.
   
   Affected: every scenario.

2. **v4 lowering cycle gap.** When the direct-path fallback is taken (PlanningContext seeded), `composer_v4::plan` produces a `WorkflowDag` that lowers to a cyclic `DAG`:
   - `bulk-rnaseq`, `scrnaseq`, `cross-omics`, `time-series`, `clinical-trial` — cycle in `batch_correction`.
   - `variant-calling`, `chip-seq`, `atac-seq` — cycle in `sequence_trimming`.
   
   The cycle survives `validate_dag` rejection only because the corpus generator falls back to the unvalidated `lower_to_workflow_json` artifact.

3. **v4 atom-set divergence.** Even ignoring cycles, the v4 emission diverges from v2 at the atom level:
   - v4 routinely **omits** `validate_*` companion atoms, `pathway_enrichment`, `final_reporting` / `reporting`, and (in cross-omics) the `<modality>_*` namespaced branches that the v2 cross-omics archetype synthesizes.
   - v4 **adds** wide-spectrum atoms that match the goal IRI but are not goal-relevant: `bio_mystery_query`, `compbio_query`, `lab_bench_query`, `sciagent_solution`, `endpoint_analysis`, `differential_transcript_usage`, `isoform_discovery`, `peptide_search`, `protein_quantification`, `spatial_domain_segmentation`, `data_import`, `diversity_analysis`, etc. These are agentic / generic-query atoms whose `outputs` happen to declare the same `data:0951` / `data:0863` / `data:3917` types that the goal pattern matches.

   The full per-scenario diff lives in each `v4-emission/GAP.txt`.

## Phase 5 closure list

Before Phase 5 can flip the default, the following must all be true:

- [ ] `compose_v4_dispatch_full` populates `PlanningContext.intent.modality` from the caller-supplied modality set, `PlanningContext.intent.project_class`, and `PlanningContext.intent.available_data` (from intake / the dataset profiler).
- [ ] `composer_v4::plan` produces a `WorkflowDag` whose lowering passes `validate_dag` for every scenario (no cycles).
- [ ] The v4 atom selection narrows to goal-relevant producers (modality / archetype-aware filter on the agentic / wide-spectrum atom set).
- [ ] The v4 planner synthesizes the `validate_*` companion stages that the v2 archetype path emits via the builder's `validate_*` wrapper convention.
- [ ] `make v4-parity-emit && cargo test -p ecaa-workflow-core --test composer_v4_parity_corpus -- --ignored v4_parity_corpus_matches_v2_baseline` exits with all eight scenarios GREEN and zero documented gaps.
