# variant-calling — Phase 4.6 Task G post-fix (2026-05-08)

**Verdict:** GREEN (was ACCEPTABLE). **Phase 5 readiness:** unblocked for this scenario.

v4 emission produced via PRODUCTION DISPATCH (18 atoms — atom-set match with the v2 baseline).

## Resolution

Phase 4.6 Task G's archetype primacy rule (+ rich-port lift + best-pair port matching + aggregator companion synthesis) made the `variant_calling_germline` archetype seed surface as primary. The atom set is exactly the v2 baseline: cleanest v4 emission of the corpus.

Core executable path: `data_acquisition → sequence_trimming → alignment → variant_calling → variant_annotation / variant_filtering`, plus `raw_qc`, `reporting`, `final_reporting`, and all `validate_*` companions.

The GIAB benchmark deliverable is reachable; the parity test confirms v4 atom set ⊇ v2 baseline.
