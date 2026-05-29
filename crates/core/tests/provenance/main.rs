// Consolidated integration-test binary: groups several former top-level
// tests/*.rs files into one target to cut link time. Each module is a
// verbatim relocation; #[test] behavior is unchanged.
mod audit_proof_invariants;
mod audit_proof_loader;
mod audit_proof_orchestrator;
mod audit_proof_report_types;
mod audit_writer_tamper;
mod auditability_corpus_smoke;
mod claim_extractor_excludes;
mod claim_verifier_pvalue_tolerance;
mod provenance_tiers;
mod prov_o_corpus;
mod reexecution_classifier;
mod replay_provenance;
mod wrroc_v05_fixtures;
