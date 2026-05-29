// Consolidated integration-test binary: groups several former top-level
// tests/*.rs files into one target to cut link time. Each module is a
// verbatim relocation; #[test] behavior is unchanged.
mod cross_omics_archetype_loads;
mod dag_error_missing_stage;
mod dag_error_orphaned_task;
mod hypothesized_proposal;
mod ids_atomid_roundtrip;
mod ids_stageid_roundtrip;
mod ids_taskid_roundtrip;
mod no_stranded_nodes;
mod slot_expansion_in_planner;
mod slot_manifest_loads;
mod task_spec_extract_present;
mod task_state_is_terminal_complete;
mod workflow_contracts_atom_conversion;
mod workflow_dag_round_trip;
mod workflow_template_fixtures;
