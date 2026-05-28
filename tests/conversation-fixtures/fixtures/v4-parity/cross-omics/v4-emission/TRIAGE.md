# cross-omics — Phase 4.6 Task I post-fix (2026-05-08)

**Verdict:** GREEN. **Phase 5 readiness:** UNBLOCKED.

v4 emission produces 32 atoms via PRODUCTION DISPATCH — exact atom-set match with the v2 baseline.

## Resolution

Task I closes the cross-omics archetype-seed gap. Three coordinated changes:

1. **Multi-modality intent threading.** `compose_v4_dispatch_full` now reads `target_modalities[1..]` and threads it into `PlanningContext::additional_modalities`. Pre-fix the planner only saw the primary modality (`target_modalities[0]`).

2. **Cross-omics archetype matcher.** `try_archetype_seed` calls `archetype_reg.find_match_cross_omics(...)` before single-modality matching when `additional_modalities` is non-empty. The single-modality matcher (`find_match_with_evidence_modality_kind`) explicitly excludes archetypes carrying `cross_omics_modalities`, so the cross-omics scaffold was previously never seeded.

3. **Alias-stable lift→lower roundtrip.** `lift_to_workflow_dag` records the underlying `atom_id` in `node.attributes["atom_id"]` alongside `stage_id`, and `lower_dag_to_composition_result` reads it first. Without this, aliased nodes (e.g. `rnaseq_differential_expression` aliased from `differential_expression`) lowered to the placeholder atom with empty outputs, triggering `GoalUnreachable` in `validate_composition`.

The synthesized `ScoreEvidence` for cross-omics matches sets `modality_match = 2`, so `is_definitive_archetype_match` fires (set-equality on `cross_omics_modalities` is the strongest possible modality signal). The archetype-primacy bias (+1000 scientific_appropriateness penalty on the search alternative) prevents search-driven discovery from outranking the canonical parallel-pipeline scaffold.

## v2 ↔ v4 atom-set match

All 32 atoms present in v4. Full set:

- RNA-seq branch (9 + 9 validators): `rnaseq_data_acquisition`, `rnaseq_raw_qc`, `rnaseq_sequence_trimming`, `rnaseq_alignment`, `rnaseq_quantification`, `rnaseq_qc_preprocessing`, `rnaseq_normalisation`, `rnaseq_differential_expression`, `rnaseq_pathway_enrichment` + `validate_*` companions.
- Proteomics branch (5 + 5 validators): `proteomics_data_acquisition`, `proteomics_peptide_search`, `proteomics_protein_quantification`, `proteomics_differential_abundance`, `proteomics_pathway_enrichment` + `validate_*` companions.
- Integration: `cross_omics_thematic_comparison` + `validate_cross_omics_thematic_comparison`.
- Terminal: `final_reporting` + `validate_final_reporting`.

## Regression coverage

`crates/core/tests/composer_v4_cross_omics.rs` adds two regressions:

- `cross_omics_includes_per_modality_atoms` — asserts both `rnaseq_alignment` AND `proteomics_peptide_search` (no overlap) appear in the v4 emission, and the bare-name `alignment` shadow doesn't.
- `cross_omics_includes_integration_step` — asserts `cross_omics_thematic_comparison` and `final_reporting` are present.
