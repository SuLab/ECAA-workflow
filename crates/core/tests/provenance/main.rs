// Consolidated integration-test binary: groups several former top-level
// tests/*.rs files into one target to cut link time. Each module is a
// verbatim relocation; #[test] behavior is unchanged.
mod agent_brief_claims_contract;
mod fair_completeness_e2e;
mod audit_proof_invariants;
mod audit_proof_loader;
mod audit_proof_orchestrator;
mod audit_proof_report_types;
mod audit_proof_with_verifier;
mod audit_writer_tamper;
mod auditability_corpus_smoke;
mod claim_extractor_excludes;
mod claim_verifier_pvalue_tolerance;
mod execution_consistency;
mod prov_o_corpus;
mod provenance_tiers;
mod recall_coverage_invariant;
mod recall_end_to_end;
mod reexecution_classifier;
mod replay_himes;
mod replay_provenance;
mod ro_crate_date_consistency;
mod signed_sink_invariants;
mod signed_sink_loader;
mod wrroc_v05_fixtures;
