// Consolidated integration-test binary: groups several former top-level
// tests/*.rs files into one target to cut link time. Each module is a
// verbatim relocation; #[test] behavior is unchanged.
mod backward_search_finds_path;
mod backward_search_rescues_flex_mr;
mod composer_v4_archetype_seed;
mod composer_v4_assumption_ledger;
mod composer_v4_atom_selection;
mod composer_v4_backward_search;
mod composer_v4_bare_modality;
mod composer_v4_cross_omics;
mod composer_v4_determinism;
mod composer_v4_discover_companion;
mod composer_v4_dispatch_intent;
mod composer_v4_forward_search;
mod composer_v4_generic_omics;
mod composer_v4_meet_in_middle;
mod composer_v4_modality_goal_mismatch;
mod composer_v4_no_cycles;
mod composer_v4_parity_corpus;
mod composer_v4_peak_calling_reachability;
mod composer_v4_project_class_archetype;
mod composer_v4_reporting_consumer_synthesis;
mod composer_v4_scrnaseq_completeness;
mod composer_v4_time_series_forecast_archetype;
mod composer_v4_validate_companions;
mod v3_alignment_status_baseline;
mod v4_archetype_collision_fallback;
mod v4_task_spec_required_figures;
