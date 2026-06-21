//! Deterministic conformance repair: re-register produced output tables and
//! re-seal the BagIt manifests.
//!
//! # Scope and honest limitation
//!
//! This executor fixes exactly two deterministic kinds of drift:
//!
//! 1. **Table-registration drift** — agent-produced tables under
//!    `runtime/outputs/<task>/` that are present on disk but not yet recorded
//!    as `Table` entities in `ro-crate-metadata.json::@graph`. It re-runs
//!    [`crate::ro_crate::register_produced_output_tables`], which is idempotent
//!    and never invents inputs.
//! 2. **Manifest drift** — once the descriptor graph changes, the BagIt payload
//!    and tag manifests must be re-sealed to cover the at-rest evidence surface.
//!    It re-runs [`crate::emitter::regenerate_bagit_manifest`] in re-seal mode.
//!
//! It does **NOT** repair WRROC / substrate conformance. Offline, the WRROC
//! validator is a Noop, so `substrate_validity` stays `Unverified` regardless of
//! what this executor does. A real conformance defect against the WRROC profile
//! is not something a deterministic metadata re-seal can close, so those cases
//! are intentionally left to route to human review rather than being silently
//! marked `Applied`. This executor's `Applied` outcome asserts only that table
//! registration and manifest sealing were brought back into agreement with the
//! on-disk outputs — not that the package is WRROC-valid.

use std::path::Path;

use super::super::executor::{Executor, RepairOutcome};
use super::super::failure::{Failure, RepairClass};
use super::super::runner::TaskRunner;

/// Deterministic [`Executor`] for [`RepairClass::ConformanceFix`]: re-registers
/// produced output tables and re-seals the BagIt manifests.
///
/// See the module docs for the precise scope and its honest limitation re. WRROC
/// substrate validity (which this executor does not and cannot establish).
pub struct ConformanceFix;

impl Executor for ConformanceFix {
    fn class(&self) -> RepairClass {
        RepairClass::ConformanceFix
    }

    fn repair(
        &self,
        _f: &Failure,
        pkg: &Path,
        _config_dir: &Path,
        _runner: &dyn TaskRunner,
    ) -> RepairOutcome {
        // Step 1: re-register any produced output tables that are on disk but
        // missing from the descriptor graph. Idempotent.
        let n = match crate::ro_crate::register_produced_output_tables(pkg) {
            Ok(n) => n,
            Err(e) => {
                return RepairOutcome::Unrepairable(format!(
                    "re-registering produced output tables failed: {e}"
                ));
            }
        };

        // Step 2: re-seal the BagIt manifests so they cover the (possibly
        // changed) descriptor + the at-rest outputs surface.
        if let Err(e) =
            crate::emitter::regenerate_bagit_manifest(pkg, &crate::clock::WallClock)
        {
            return RepairOutcome::Unrepairable(format!(
                "re-sealing BagIt manifest failed: {e:#}"
            ));
        }

        RepairOutcome::Applied {
            deterministic: true,
            note: format!("re-registered {n} tables + re-sealed manifests"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::repair_loop::failure::FailureSource;
    use crate::repair_loop::runner::{RepairDirective, TaskRunner};

    /// Test runner that never gets invoked by a deterministic executor.
    struct UnusedRunner;
    impl TaskRunner for UnusedRunner {
        fn rerun(&self, _pkg: &Path, _directive: &RepairDirective) -> anyhow::Result<()> {
            panic!("deterministic ConformanceFix must not invoke the TaskRunner");
        }
    }

    /// Build a minimal package: a descriptor with a `@graph` array, a BagIt
    /// payload manifest, and one produced output table.
    fn build_minimal_package(root: &Path) -> Vec<u8> {
        // Minimal RO-Crate descriptor with the @graph array the registrar reads.
        let descriptor = serde_json::json!({
            "@context": "https://w3id.org/ro/crate/1.1/context",
            "@graph": [
                {
                    "@id": "ro-crate-metadata.json",
                    "@type": "CreativeWork",
                    "about": { "@id": "./" }
                },
                { "@id": "./", "@type": "Dataset" }
            ]
        });
        fs::write(
            root.join("ro-crate-metadata.json"),
            serde_json::to_vec_pretty(&descriptor).expect("serialize descriptor"),
        )
        .expect("write descriptor");

        // A pre-existing (possibly stale) payload manifest.
        fs::write(
            root.join("manifest-sha512.txt"),
            b"0000000000000000000000000000000000000000000000000000000000000000\
0000000000000000000000000000000000000000000000000000000000000000  data/placeholder\n",
        )
        .expect("write manifest");

        // One produced output table under runtime/outputs/<task>/.
        let de_dir = root.join("runtime").join("outputs").join("de");
        fs::create_dir_all(&de_dir).expect("create de outputs dir");
        let de_bytes = b"gene\tlog2fc\tpadj\nCRISPLD2\t-1.23\t0.0004\n".to_vec();
        fs::write(de_dir.join("de.tsv"), &de_bytes).expect("write de.tsv");

        de_bytes
    }

    #[test]
    fn repair_is_well_formed_and_freezes_table_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pkg = dir.path();
        let de_bytes = build_minimal_package(pkg);
        let de_path = pkg.join("runtime").join("outputs").join("de").join("de.tsv");

        let f = Failure::new(
            FailureSource::InvariantFailure("table_registration".to_string()),
            RepairClass::ConformanceFix,
            "de",
            "runtime/outputs/de/de.tsv",
            "produced table not registered in descriptor",
        );

        let exec = ConformanceFix;
        // Must not panic regardless of how much scaffolding the registrar /
        // re-sealer require on this minimal package.
        let outcome = exec.repair(&f, pkg, pkg, &UnusedRunner);

        // The outcome must be a well-formed variant: either a deterministic
        // Applied (registration + re-seal succeeded) or a non-panicking
        // Unrepairable with a non-empty reason. NeedsAgent is never correct for
        // a deterministic metadata repair.
        match &outcome {
            RepairOutcome::Applied { deterministic, note } => {
                assert!(
                    *deterministic,
                    "ConformanceFix Applied must be deterministic, got {outcome:?}"
                );
                assert!(
                    note.contains("re-sealed manifests"),
                    "Applied note must describe the re-seal, got {note:?}"
                );
            }
            RepairOutcome::Unrepairable(msg) => {
                assert!(
                    !msg.trim().is_empty(),
                    "Unrepairable must carry a non-empty reason"
                );
            }
            RepairOutcome::NeedsAgent(d) => {
                panic!("deterministic ConformanceFix must not request an agent: {d:?}");
            }
        }

        // Frozen-table invariant: the produced result bytes must never be
        // touched by a metadata-only repair.
        let after = fs::read(&de_path).expect("read de.tsv after repair");
        assert_eq!(
            after, de_bytes,
            "ConformanceFix must not mutate frozen result-table bytes"
        );
    }

    #[test]
    fn class_is_conformance_fix() {
        assert_eq!(
            ConformanceFix.class(),
            RepairClass::ConformanceFix,
            "ConformanceFix must report its own RepairClass"
        );
    }
}
