# clinical-trial — Phase 4.6 Task H post-fix (2026-05-08)

**Verdict:** GREEN. **Phase 5 readiness:** UNBLOCKED for this scenario.

v4 emission now produced via PRODUCTION DISPATCH (Phase 4.6 Task H landed).

## Resolution

Phase 4.6 Task H made `archetype_registry::score_archetype_full` treat
`project_class` as a hard partition rather than a tie-breaker. When
the archetype's `project_class` doesn't match the target's, scoring
returns `total = 0` so the candidate is filtered out by the upstream
`> 0` filter.

Pre-fix the matcher saw a 5-way tie (clinical_trial_analysis at score 6
vs four bioinformatics archetypes at score 5; the +1 for project_class
got eaten by the 5%-tie cutoff `floor(6 * 0.95) = 5`), and v2 fell
through to the legacy generic-omics taxonomy build while v4 produced
the bulk-rnaseq pipeline shape via forward search.

Post-fix the matcher returns ONLY `clinical_trial_analysis` for a
`clinical_trial` target, no tie surfaces, and the v2 + v4 dispatch
both commit to the archetype: `data_import → qc_preprocessing →
differential_expression → reporting → final_reporting`, plus
`validate_*` companions for each result-producing atom.

## Atom set parity

10 v2 atoms = 10 v4 atoms (atom-set match):

- `data_import` + `validate_data_import`
- `qc_preprocessing` + `validate_qc_preprocessing`
- `differential_expression` + `validate_differential_expression`
- `reporting` + `validate_reporting`
- `final_reporting` + `validate_final_reporting`

Note: `differential_expression` is a stand-in for the SAP-driven
endpoint-analysis stage today. The clinical_trial_analysis archetype's
YAML caveat acknowledges that dedicated CDISC-mapping,
population-definition, and endpoint-analysis atoms don't exist yet — a
follow-up taxonomy extension can replace the stand-in without changing
the parity contract.
