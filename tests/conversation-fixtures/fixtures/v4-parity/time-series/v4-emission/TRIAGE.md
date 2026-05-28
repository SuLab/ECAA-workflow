# time-series — Phase 4.6 Task H post-fix (2026-05-08)

**Verdict:** GREEN. **Phase 5 readiness:** UNBLOCKED for this scenario.

v4 emission now produced via LEGACY-TAXONOMY FALLBACK after archetype
dispatch correctly identifies `time_series_forecast` but its incomplete
atom set can't satisfy the goal. v2 takes the same fallback path; both
emit the legacy generic-omics shape (15 tasks).

## Resolution

Phase 4.6 Task H made `archetype_registry::score_archetype_full` treat
`project_class` as a hard partition rather than a tie-breaker. With the
filter, the time_series_forecast archetype is the only candidate for a
`time_series_forecast` project-class target — bulk-rnaseq, long-read,
and metagenomics archetypes (all `project_class: bioinformatics`) are
filtered out before scoring.

The dispatcher then attempts to build from the time_series_forecast
archetype. The archetype's atoms (`data_import` / `qc_preprocessing` /
`reporting` / `final_reporting`) don't produce `data:0951 / format:3475`
— the archetype's YAML caveat acknowledges that "dedicated
exploratory-decomposition, feature-engineering, model-fitting, and
forecasting atoms don't exist yet". `validate_composition` correctly
returns `GoalUnreachable`.

Both v2 (via `emit_v2`) and v4 (via Phase 4.6 Task H's `emit_v4`
legacy-taxonomy fallback) then build from
`config/stage-taxonomies/generic-omics.yaml`, producing the
`discover_*`-driven generic-omics shape (15 tasks). This mirrors the
conversation crate's `try_build_via_composer` downgrade behavior: when
the composer returns Err, fall back to the legacy taxonomy build.

## Atom set parity

15 v2 atoms = 15 v4 atoms (atom-set match):

- `discover_data_acquisition` / `discover_preprocessing` /
  `discover_analysis_method` / `discover_reporting`
- `data_acquisition` / `quality_control` / `preprocessing` /
  `primary_analysis` / `results_reporting` / `results_review`
- `validate_preprocessing` / `validate_quality_control` /
  `validate_primary_analysis` / `validate_analysis` /
  `validate_results_reporting`

## Followups

- The time_series_forecast archetype is functional but atom-incomplete.
  Phase 5+ should add SARIMA / state-space / neural forecasting atoms
  that produce `data:0951` so the archetype path can drive the
  composition end-to-end without legacy fallback.
- The matcher's project_class partition is the load-bearing fix; the
  legacy fallback is a staging behavior that mirrors v2 today.
