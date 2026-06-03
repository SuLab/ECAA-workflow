// Consolidated integration-test binary: groups several former top-level
// tests/*.rs files into one target to cut link time. Each module is a
// verbatim relocation; #[test] behavior is unchanged.
mod atom_contract_lint;
mod atom_count_baseline;
mod atom_registry_overlay;
mod confirmatory_atom_catalog;
mod estimated_duration;
mod atom_role_consumers;
mod atom_role_speculative_variants;
mod atom_safety_integration;
mod integrators_atom_loads;
mod live_configs;
mod method_choice_self_consistency;
mod parameters_field;
mod port_schema_seal;
mod provenance_field;
mod snapshot_id;
mod survey_method_landscape_loads;
