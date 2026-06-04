//! Per-atom contract lint over the SHIPPED catalog. Wired into `make lint`
//! via scripts/check-atom-contracts.sh (which shells `cargo test`).
//!
//! Three checks per atom: serde round-trip, figure-affordance resolution
//! (delegated to the shared `check_atom` resolver), and non-orphan
//! `depends_on`. The shipped catalog already passes `figure_obligation`,
//! so this gate must report zero violations.

use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::registry::lifecycle::lint_atom_contracts;
use std::path::Path;

#[test]
fn shipped_catalog_passes_atom_contract_lint() {
    // Integration tests run with CWD = crate root (crates/core), so the
    // config dirs live two levels up at the workspace root.
    let reg = AtomRegistry::load_from_dir(Path::new("../../config/stage-atoms"))
        .expect("load shipped catalog");
    let violations = lint_atom_contracts(&reg, Path::new("../../config/plot-affordances"));
    assert!(
        violations.is_empty(),
        "atom contract lint found {} violation(s):\n  - {}",
        violations.len(),
        violations.join("\n  - ")
    );
}
