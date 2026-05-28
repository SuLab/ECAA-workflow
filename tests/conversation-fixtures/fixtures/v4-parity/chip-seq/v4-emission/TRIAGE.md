# chip-seq — Phase 4.6 Task G post-fix (2026-05-08)

**Verdict:** GREEN (was ACCEPTABLE post-Task F). **Phase 5 readiness:** unblocked for this scenario.

v4 emission produced via PRODUCTION DISPATCH (14 atoms — atom-set match with the v2 baseline).

## Resolution

Two-phase resolution:

1. **Phase 4.6 Task F (2026-05-08).** Fixed the `peak_calling` reachability gap by correcting the goal IRI in `config/modality-keywords.yaml` (was `data:0863` BAM input, now `data:1255` Feature-record output) and extending `validate_composition` in `crates/core/src/composer.rs` to also consult atom output port IRIs. This took the scenario from GAPS to ACCEPTABLE (peak_calling reachable, but with modality-orthogonal pollution).

2. **Phase 4.6 Task G (2026-05-08).** Made the archetype seed surface as primary for definitive matches (modality_hint + goal_data + goal_format all set), excluded the modality-orthogonal pollution (clustering, normalisation, peptide_search, etc.), and aligned aggregator companion synthesis with v2's `emit_stage` post-pass. The atom set is now exactly the v2 baseline.

Core executable path: `data_acquisition → sequence_trimming → alignment → peak_calling`, plus `raw_qc`, `reporting`, `final_reporting`, and all `validate_*` companions. The MACS2 narrow-peak deliverable is reachable.
