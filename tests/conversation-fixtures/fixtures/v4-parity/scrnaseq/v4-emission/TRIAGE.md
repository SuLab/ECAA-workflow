# scrnaseq — Phase 4.6 Task G resolution (2026-05-08)

**Verdict:** GREEN (was GAPS). **Phase 5 readiness:** unblocked for this scenario.

v4 emission produced via PRODUCTION DISPATCH (32 atoms — atom-set match with the v2 baseline).

## What changed

Phase 4.6 Task G fix landed in `crates/core/src/composer_v4/planner.rs`:

1. **Definitive archetype seed wins ranking.** When `archetype_reg.find_match` produces a match whose evidence carries `goal_data_exact + goal_format_match + modality_match` all set (the canonical-pipeline signal for the requested goal+modality+format triple), the planner penalizes the search seed's `scientific_appropriateness_penalty` so the archetype seed surfaces as primary.
2. **Rich-port lift.** `lift_to_workflow_dag` now uses the atom's authored `inputs:` / `outputs:` directly instead of the legacy `from_atom`-synthesized `edam_data` placeholders. Pre-fix every archetype-lifted edge carried fake `SemanticTypeMismatch` warnings (e.g. `data:1234` inputs/outputs everywhere because `from_atom` projects only `edam_data`), which flipped `score.required_contract_unsatisfied` to `Reject` and disqualified the archetype seed.
3. **Best-pair port matching.** Multi-port atoms (e.g. `data_acquisition` emits both raw FASTQ AND a cohort manifest; `differential_expression` consumes normalized counts but its archetype `depends_on` lists `cell_type_annotation` for *workflow ordering*, not data flow) now pick the most compatible (output, input) port pair via the compatibility engine. Failed pairs surface a `workflow_ordering_edge` warning that doesn't trip the score-Reject gate.
4. **Aggregator atoms get validate companions.** `companion_synthesis::is_eligible_for_validate_companion` no longer skips `role: aggregator`. v2's `emit_stage` post-pass already emits `validate_<id>` for aggregators (e.g. `validate_integration`); aligning v4 closes the last 1-atom gap.

## Resolution

- v4 atom set ⊇ v2 atom set (33 vs 32 atoms — v4 set-match the v2 baseline; no diffs).
- Regression coverage: `crates/core/tests/composer_v4_scrnaseq_completeness.rs` (5 tests).
- Side-benefits: `bulk-rnaseq`, `variant-calling`, `chip-seq`, `atac-seq` all advanced from ACCEPTABLE to GREEN through the same fix (definitive archetype match now wins; modality-orthogonal pollution gone).

## Remaining out of scope (Phase 5 closure list)

- `cross-omics`: needs namespace-prefixed archetype path (`rnaseq_*` / `proteomics_*`) and a join atom (`cross_omics_thematic_comparison`). Architectural — distinct from this fix.
- `time-series` / `clinical-trial`: project-class != bioinformatics archetype dispatch missing in v4. Architectural — distinct from this fix.
