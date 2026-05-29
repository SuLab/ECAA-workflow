//! Catalog-completeness audit.
//!
//! Asserts that every `renderer_module` in the PlotAffordanceRegistry resolves
//! to a real Python module under `lib/plotting/`.
//!
//! Module-path convention: `runtime.plotting.stages.X` maps to
//! `lib/plotting/stages/X.py` relative to the repo root.
//! The test resolves `runtime.` → `lib/` and replaces `.` with `/`.
//!
//! Run:
//! ```bash
//! cargo test -p ecaa-workflow-core --test plot_affordance_catalog_audit
//! ```

use ecaa_workflow_core::plot_affordance::registry::YamlPlotAffordanceRegistry;
use ecaa_workflow_core::plot_affordance::PlotAffordanceRegistry;
use std::path::Path;

#[test]
fn every_registered_renderer_module_resolves() {
    let repo_root = Path::new("../../");
    let plot_dir = repo_root.join("config").join("plot-affordances");

    let plot_reg = YamlPlotAffordanceRegistry::from_dir(&plot_dir).unwrap_or_else(|e| {
        panic!(
            "failed to load plot affordance registry from {}: {e}",
            plot_dir.display()
        )
    });

    let mut missing: Vec<String> = vec![];
    for (semantic_type, reg) in plot_reg.iter() {
        // Resolve module path: `runtime.plotting.stages.X` →
        // strip `runtime.` prefix → `plotting/stages/X`
        // then prepend `lib/` → `lib/plotting/stages/X.py`
        let module_path = &reg.renderer_module;
        let relative: String = if let Some(stripped) = module_path.strip_prefix("runtime.") {
            // `runtime.plotting.stages.X` → `plotting/stages/X`
            stripped.replace('.', "/")
        } else {
            // Unexpected prefix — convert dots to slashes and try.
            module_path.replace('.', "/")
        };
        let on_disk = repo_root.join("lib").join(format!("{relative}.py"));
        if !on_disk.exists() {
            missing.push(format!(
                "{} → {} (looking for {})",
                semantic_type,
                module_path,
                on_disk.display()
            ));
        }
    }
    assert!(
        missing.is_empty(),
        "plot affordance registry references missing Python modules ({} total):\n  - {}",
        missing.len(),
        missing.join("\n  - ")
    );
}
