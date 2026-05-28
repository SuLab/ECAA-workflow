# bulk-rnaseq — Phase 4.6 Task G post-fix (2026-05-08)

**Verdict:** GREEN (was ACCEPTABLE). **Phase 5 readiness:** unblocked for this scenario.

v4 emission produced via PRODUCTION DISPATCH (22 atoms — atom-set match with the v2 baseline).

## Resolution

Phase 4.6 Task G's archetype primacy rule (+ rich-port lift + best-pair port matching + aggregator companion synthesis in `crates/core/src/composer_v4/`) made the `bulk_rnaseq_de` archetype seed surface as primary. Modality-orthogonal pollution (peptide_search, protein_quantification, diversity_analysis, long_read_benchmarking, batch_correction, clustering, dimensionality_reduction, integration) is excluded because the canonical `bulk_rnaseq_de` archetype declares only the bulk-rnaseq atoms.

Core executable path: `data_acquisition → sequence_trimming → alignment → quantification → qc_preprocessing → normalisation → differential_expression → pathway_enrichment → reporting → final_reporting`, plus `raw_qc` and all `validate_*` companions.

The IBD DESeq2 deliverable is reachable; the parity test confirms v4 atom set ⊇ v2 baseline.
