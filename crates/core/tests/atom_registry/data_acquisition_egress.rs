//! `data_acquisition` fetches raw experimental data from public
//! repositories (SRA / GEO / ArrayExpress / ENA / PRIDE / Synapse) over
//! the internet. It therefore MUST declare network egress — the
//! historical default (no `safety:` block → Compute / `None{[]}`, full
//! isolation) was a lie that a downstream offline-replay capability
//! check would trust, concluding the stage is hermetic when it is not.
//!
//! The repository host set is broad and mirror/CDN-backed (SRA alone
//! routes through NCBI, AWS, and GCP mirrors), so this atom declares
//! the open `Bridge` policy rather than a fragile curated allowlist —
//! unlike the literature atoms, which DO curate a bounded host set.

use ecaa_workflow_core::atom::{NetworkPolicy, SafetyLevel};
use ecaa_workflow_core::atom_registry::AtomRegistry;
use std::path::PathBuf;

fn stage_atoms_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/stage-atoms")
}

#[test]
fn data_acquisition_declares_network_egress() {
    let reg = AtomRegistry::load_from_dir(&stage_atoms_dir()).expect("registry loads");
    let atom = reg
        .get("data_acquisition")
        .expect("data_acquisition atom must be registered");

    // Fetches from public repositories → must be Network-level (Compute
    // and Safe forbid egress at registry-load lint).
    assert_eq!(
        atom.safety.level,
        SafetyLevel::Network,
        "data_acquisition fetches public-repository data and must declare SafetyLevel::Network"
    );

    // Open web/repository retrieval across a mirror-backed host set →
    // Bridge, not a curated allowlist.
    assert_eq!(
        atom.safety.network,
        NetworkPolicy::Bridge,
        "data_acquisition must declare Bridge egress (open public-repository fetch)"
    );
}
