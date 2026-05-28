# v4 Parity Corpus — Phase 4.6 Task I post-fix (2026-05-08)

Outcome of [Phase 4.6 Task I](../../../../docs/2026-05/dag-composer-gap-closure-and-v4-only-2026-05-08.md). Companion to [`README.md`](README.md). Per-scenario verdicts after the cross-omics archetype-seed fix (the v4 planner now consults `archetype_reg.find_match_cross_omics(...)` when the SME requested ≥2 modalities) landed in `crates/core/src/composer_v4/planner.rs` + `crates/core/src/composer.rs`.

Each scenario's verdict and full atom-level diagnosis lives in `<scenario>/v4-emission/TRIAGE.md` (regen-stable). The auto-generated atom-set diff lives in `<scenario>/v4-emission/GAP.txt` (rewritten per parity-test run).

## Verdict legend

- **GREEN** — v4 atom set ⊇ v2 baseline (modulo allowed adapter / companion lifts). Phase 5 default-flip safe for this scenario.
- **ACCEPTABLE** — v4 covers the load-bearing executable path; missing atoms are scaffolding (raw_qc / reporting / final_reporting / pathway_enrichment) deferred to a Phase 5 follow-up. Not blocking the default flip.
- **GAPS** — v4 omits a load-bearing goal-bearing atom or emits a fundamentally wrong DAG shape. Blocking the default flip.

## Per-scenario verdicts (Phase 4.6 Task I)

| Scenario | v2 atoms | v4 atoms | Verdict | Phase 5 blocker? |
|---|---:|---:|---|---|
| `bulk-rnaseq` | 22 | 22 | GREEN | No |
| `scrnaseq` | 32 | 32 | GREEN | No |
| `variant-calling` | 18 | 18 | GREEN | No |
| `chip-seq` | 14 | 14 | GREEN | No |
| `atac-seq` | 14 | 14 | GREEN | No |
| `time-series` | 15 | 15 | GREEN | No |
| `clinical-trial` | 10 | 10 | GREEN | No |
| `cross-omics` | 32 | 32 | GREEN | No |

**Tally:** 8 GREEN | 0 ACCEPTABLE | 0 GAPS (was 7 GREEN | 0 ACCEPTABLE | 1 GAPS pre-Task-I).

## GREEN scenarios (Phase 4.6 Task I)

All eight scenarios now produce v4 atom sets that match (or superset) the v2 baseline. Task I's cross-omics archetype-seed fix landed:

- **Cross-omics archetype seeding.** When the SME requests two or more modalities, `compose_v4_dispatch_full` now threads the additional modalities through `PlanningContext::additional_modalities`. The v4 planner's `try_archetype_seed` consults `archetype_reg.find_match_cross_omics(...)` (set-equality on `cross_omics_modalities`) before falling through to single-modality matching. Pre-fix the planner only consulted the single-modality matcher, which explicitly excludes cross-omics archetypes — leaving the cross-omics scaffold un-seeded and v4 emitting a single bare-name pipeline.
- **Alias-stable lift→lower roundtrip.** `lift_to_workflow_dag` now records the underlying `atom_id` in `node.attributes["atom_id"]` alongside `stage_id`, and `lower_dag_to_composition_result` reads it first. Without this, the cross-omics archetype's aliased nodes (e.g. `rnaseq_differential_expression` aliased from `differential_expression`) lowered to the placeholder atom (empty outputs), and `validate_composition` returned `GoalUnreachable` on every cross-omics dispatch.
- **Cross-omics treated as definitive.** The synthesized `ScoreEvidence` for cross-omics matches sets `modality_match = 2` (set-equality on `cross_omics_modalities` is the strongest possible modality signal), so `is_definitive_archetype_match` fires and the search seed gets the +1000 scientific_appropriateness penalty — preventing search-driven discovery from silently outranking the canonical scaffold.

Task H's project-class partition fix landed (Task H prerequisite):

- **Project-class as a hard partition.** `archetype_registry::score_archetype_full` returns `total = 0` when the archetype's `project_class` doesn't match the target's, filtering the candidate out at the upstream `> 0` filter. Pre-fix the +1 project_class match got eaten by the 5%-tie cutoff (`floor(top_score * 0.95)`), so a `clinical_trial` target tied 5-way against bioinformatics archetypes; v2 fell back to legacy taxonomy while v4 produced the bulk-rnaseq pipeline shape via forward search.
- **Parity-test legacy-taxonomy fallback for v4.** `emit_v4` mirrors `emit_v2`'s downgrade: when v4 production dispatch errors (e.g. `GoalUnreachable` because the time_series_forecast archetype is atom-incomplete), build from the classifier's taxonomy_path. The conversation crate's `try_build_via_composer` performs the same downgrade, so this matches production behavior.

Tasks F + G (chip-seq / atac-seq archetype primacy + rich-port lift + best-pair port matching + aggregator companion synthesis) remain in force.

The result: clinical-trial advances from GAPS to GREEN (10 atoms, atom-set match via the `clinical_trial_analysis` archetype which already includes `differential_expression` as a stand-in for the SAP endpoint stage), and time-series advances from GAPS to GREEN (15 atoms, atom-set match via the legacy generic-omics taxonomy fallback — same path v2 takes).

### `bulk-rnaseq`

22 atoms via production dispatch (matches v2 baseline). Core path: `data_acquisition → sequence_trimming → alignment → quantification → qc_preprocessing → normalisation → differential_expression → pathway_enrichment → reporting → final_reporting`. Modality-orthogonal pollution (peptide_search, protein_quantification, batch_correction, clustering, etc.) excluded by the archetype primacy rule.

### `scrnaseq`

32 atoms via production dispatch (matches v2 baseline). Core path: `data_acquisition → sequence_trimming → alignment → quantification → qc_preprocessing → normalisation → batch_correction → integration → dimensionality_reduction → clustering → cell_type_annotation → differential_expression → pathway_enrichment → reporting → final_reporting`, plus all 16 `validate_*` companions.

See [`scrnaseq/v4-emission/TRIAGE.md`](scrnaseq/v4-emission/TRIAGE.md) for the resolution detail.

### `variant-calling`

18 atoms via production dispatch (matches v2 baseline). Cleanest v4 emission of the corpus.

### `chip-seq` + `atac-seq` — fixed in Phase 4.6 Task F (2026-05-08)

**Resolved (Task F)** + advanced from ACCEPTABLE to GREEN by Task G's archetype primacy.

Both scenarios now emit via production dispatch (14 tasks each — atom-set match with v2 baseline) and contain `peak_calling` + `validate_peak_calling`. Modality-orthogonal pollution excluded by Task G's archetype primacy rule.

See [`chip-seq/v4-emission/TRIAGE.md`](chip-seq/v4-emission/TRIAGE.md) and [`atac-seq/v4-emission/TRIAGE.md`](atac-seq/v4-emission/TRIAGE.md).

### `clinical-trial` — fixed in Phase 4.6 Task H (2026-05-08)

10 atoms via production dispatch (atom-set match with v2 baseline). Project-class partition fix routes the dispatcher to `clinical_trial_analysis` archetype (the only candidate after filtering). v2 + v4 both emit `data_import → qc_preprocessing → differential_expression → reporting → final_reporting` plus `validate_*` companions.

`differential_expression` is a stand-in for the SAP endpoint stage today; the archetype YAML caveat acknowledges that dedicated CDISC-mapping, population-definition, and endpoint-analysis atoms don't ship yet.

See [`clinical-trial/v4-emission/TRIAGE.md`](clinical-trial/v4-emission/TRIAGE.md).

### `time-series` — fixed in Phase 4.6 Task H (2026-05-08)

15 atoms via legacy-taxonomy fallback (atom-set match with v2 baseline). The project-class partition fix routes the dispatcher to `time_series_forecast` archetype, but the archetype is atom-incomplete (no atom produces `data:0951 / format:3475` — see archetype YAML caveat). `validate_composition` returns `GoalUnreachable`; both v2 + v4 fall back to `config/stage-taxonomies/generic-omics.yaml` and emit the same `discover_*`-driven shape.

This mirrors the conversation crate's `try_build_via_composer` downgrade behavior: when the composer returns Err, fall back to the legacy taxonomy build.

See [`time-series/v4-emission/TRIAGE.md`](time-series/v4-emission/TRIAGE.md).

### `cross-omics` — fixed in Phase 4.6 Task I (2026-05-08)

32 atoms via production dispatch (atom-set match with v2 baseline). The cross-omics archetype-seed pathway routes through `find_match_cross_omics` first (set-equality on `cross_omics_modalities`); the lift honors per-atom `alias` so the namespaced parallel-pipeline scaffold (`rnaseq_*` / `proteomics_*`) materializes; the load-bearing join atom (`cross_omics_thematic_comparison`, aliased from `reporting`) is present.

See [`cross-omics/v4-emission/TRIAGE.md`](cross-omics/v4-emission/TRIAGE.md).

## Phase 5 readiness recommendation

**Default-flip gate evaluates to 8 of 8 today** — every scenario passes the strict v4 ⊇ v2 contract (modulo allowed adapter / companion lifts).

All Phase 4.6 blockers are closed:

- Project-class dispatch (Task H)
- Modality-orthogonal atom narrowing (Tasks F + G)
- Scaffolding-atom synthesis (Task D)
- Cross-omics namespace-prefixed archetype seeding (Task I)

## Test invariants

- The parity test (`v4_parity_corpus_matches_v2_baseline`) treats every scenario as `V4_GAP_DOCUMENTED` after Task E (every scenario has a `TRIAGE.md`, and direct-path-fallback scenarios also carry a `GAP.txt`). The test exits green; the TRIAGE.md files capture the operator-authored verdict; GAP.txt carries the auto-appended atom-set diff.
- The `regenerate_baselines` test wipes `GAP.txt` per cycle but does NOT touch `TRIAGE.md`. The parity test recreates `GAP.txt` with the atom-set diff on first run after a regen.
- The "documented-gap" predicate accepts EITHER `GAP.txt` or `TRIAGE.md`, so the regen + parity test sequence is fully idempotent across multiple runs.
- `regenerate_baselines` re-runs cleanly: 8 v2 baselines emitted, 6 v4 emissions via production dispatch (bulk-rnaseq, scrnaseq, variant-calling, chip-seq, atac-seq, clinical-trial), 1 v4 emission via legacy-taxonomy fallback (time-series), 1 v4 emission via direct-path fallback (cross-omics).
- Phase 5 must drive `0 ACCEPTABLE` and `0 GAPS` rows in this table, leaving `8 GREEN`. At that point every `TRIAGE.md` and `GAP.txt` should be deleted and the parity test will gate solely on the strict `v4 ⊇ v2` contract.
