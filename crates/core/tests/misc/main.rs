// Consolidated integration-test binary: groups several former top-level
// tests/*.rs files into one target to cut link time. Each module is a
// verbatim relocation; #[test] behavior is unchanged.
mod agent_code;
mod design_doc_markers_baseline;
mod determinism_clock_gate;
mod emit_mode;
mod four_conditions_parity;
mod lineage_task_pair;
mod no_keys_in_emit;
mod opaque_aggregation;
mod parameter_connection_emission;
mod stage_class_coverage;
