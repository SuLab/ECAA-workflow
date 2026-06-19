// Consolidated integration-test binary: groups several former top-level
// tests/*.rs files into one target to cut link time. Each module is a
// verbatim relocation; #[test] behavior is unchanged.
mod assumption_policy_loader;
mod assumption_policy_schema;
mod authority_provenance;
mod blocker_variant_count;
mod boundary_actionability_coverage;
mod claim_entity_coverage;
mod compatibility_proof_evidence;
mod config_parse;
mod cost_arithmetic;
mod default_deny_safety;
mod interpretation_policy_expected_schema;
mod ontology_scope_loader;
mod ontology_scope_schema;
mod policy_rule_id_registry;
mod policy_shared_vocab;
mod policy_validation;
mod population_coverage_gate;
mod repair_auto_application;
mod schema_version_backward_compat;
mod scoring_eligibility_and_gates;
mod scoring_weight_renormalization;
