// Consolidated integration-test binary: groups several former top-level
// tests/*.rs files into one target to cut link time. Each module is a
// verbatim relocation; #[test] behavior is unchanged.
mod figure_obligation;
mod no_pending_affordance_migration;
mod per_atom_plot_smoke;
mod plot_affordance_catalog_audit;
mod plot_affordance_emit_integration;
mod plot_affordance_generated;
mod plot_affordance_ir;
mod plot_affordance_provenance;
mod plot_affordance_selector;
mod plot_affordance_telemetry;
mod plotting_version_sync;
