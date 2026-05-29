// Consolidated integration-test binary: groups several former top-level
// tests/*.rs files into one target to cut link time. Each module is a
// verbatim relocation; #[test] behavior is unchanged.
mod atom_count_baseline;
mod atom_registry_overlay;
mod atom_role_consumers;
mod atom_role_speculative_variants;
mod atom_safety_integration;
mod integrators_atom_loads;
mod live_configs;
mod method_choice_self_consistency;
