# atac-seq — Phase 4.6 Task G post-fix (2026-05-08)

**Verdict:** GREEN (was ACCEPTABLE post-Task F). **Phase 5 readiness:** unblocked for this scenario.

v4 emission produced via PRODUCTION DISPATCH (14 atoms — atom-set match with the v2 baseline).

## Resolution

Same two-phase resolution as chip-seq:

1. **Phase 4.6 Task F (2026-05-08).** Fixed the shared `peak_calling` reachability gap (goal IRI correction `data:0863` → `data:1255`).

2. **Phase 4.6 Task G (2026-05-08).** Archetype primacy rule made the `atac_seq_peaks` archetype seed surface as primary, excluded modality-orthogonal pollution.

Core executable path: `data_acquisition → sequence_trimming → alignment → peak_calling`, plus `raw_qc`, `reporting`, `final_reporting`, and all `validate_*` companions. The MACS2 BAMPE accessible-chromatin deliverable is reachable.
