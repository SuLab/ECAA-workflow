//! Atom-registry
//! self-consistency gate for `method_choice.deferred_to`.
//!
//! Earlier triage removed `method_choice.deferred_to` from
//! `differential_transcript_usage.yaml` and `isoform_discovery.yaml`
//! as a workaround because the targets (`discover_dtu_method` /
//! `discover_isoform_caller`) didn't exist on disk and
//! `AtomRegistry::validate_consistency` rejects pointers to missing or
//! non-Discovery atoms. The proper fix authored the missing discovery
//! atoms; this test guards against future regressions by re-running
//! `validate_consistency` against the real on-disk catalog. A
//! `method_choice.deferred_to` pointing at a non-existent or
//! non-Discovery atom fails this gate.
//!
//! The plan's verbatim API (`ConfigDir::from_path(...).atom_registry()`)
//! is not present in the codebase; we use `AtomRegistry::load_from_dir`
//! directly, mirroring `crates/core/tests/composer_v4_determinism.rs`.
//!
//! Run: `cargo test -p scripps-workflow-core --test method_choice_self_consistency`.

use scripps_workflow_core::atom_registry::AtomRegistry;
use std::path::Path;

#[test]
fn every_method_choice_deferred_to_resolves_to_discovery_atom() {
    let atom_dir = Path::new("../../config/stage-atoms");
    let reg = AtomRegistry::load_from_dir(atom_dir).unwrap_or_else(|e| {
        panic!(
            "failed to load atom registry from {}: {e}",
            atom_dir.display()
        )
    });
    reg.validate_consistency()
        .expect("atom registry self-consistency: every method_choice.deferred_to must resolve to a Discovery atom on disk");
}
