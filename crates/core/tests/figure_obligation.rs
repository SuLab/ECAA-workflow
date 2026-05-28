//! Figure-obligation lint integration test.
//!
//! Asserts that every non-adapter, non-exempt atom in the real atom catalog
//! (`config/stage-atoms/`) resolves to a non-`Deferred` `PlotAffordance`
//! against the real affordance registry (`config/plot-affordances/`).
//!
//! Runs as part of `make test`.

use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::plot_affordance::check_all;
use scripps_workflow_core::plot_affordance::registry::YamlPlotAffordanceRegistry;
use std::path::Path;

#[test]
fn every_data_product_atom_resolves_to_an_affordance() {
    let atom_dir = Path::new("../../config/stage-atoms");
    let plot_dir = Path::new("../../config/plot-affordances");

    let atom_reg = AtomRegistry::load_from_dir(atom_dir).unwrap_or_else(|e| {
        panic!(
            "failed to load atom registry from {}: {e}",
            atom_dir.display()
        )
    });

    let plot_reg = YamlPlotAffordanceRegistry::from_dir(plot_dir).unwrap_or_else(|e| {
        panic!(
            "failed to load plot affordance registry from {}: {e}",
            plot_dir.display()
        )
    });

    // Collect atoms as a Vec<AtomDefinition> for check_all.
    let atoms: Vec<_> = atom_reg.iter().map(|(_, v)| v.clone()).collect();

    let violations = check_all(&atoms, &plot_reg, "theme.json");
    if !violations.is_empty() {
        let summary: Vec<String> = violations.iter().map(|v| v.reason.clone()).collect();
        panic!(
            "figure-obligation lint violations ({} total):\n  - {}",
            violations.len(),
            summary.join("\n  - ")
        );
    }
}
