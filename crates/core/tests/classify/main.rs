// Consolidated integration-test binary: groups several former top-level
// tests/*.rs files into one target to cut link time. Each module is a
// verbatim relocation; #[test] behavior is unchanged.
mod classifier_cross_omics_n_way;
mod classifier_tie_surfacing;
mod classify_cross_omics_no_spurious_bulk;
mod classify_integrator_kind_modifier;
mod classify_no_data_9999_placeholder;
mod disambiguation_loads;
mod modality_bounds;
mod modality_classifier_coverage;
mod modality_manifest_synthetic_modality;
mod modality_test_coverage;
mod project_class_classifier_prespecified;
mod project_class_registry_synthetic_class;
mod tri_omics_3way_matcher;
