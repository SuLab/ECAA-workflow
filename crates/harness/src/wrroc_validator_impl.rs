//! Harness-side adapter for the WRROC v0.5 conformance validator.
//!
//! Implements the `scripps_workflow_core::wrroc_validator::WrrocValidator`
//! trait by shelling out to `scripts/wrroc-validate.py`, which wraps
//! `runcrate validate` (≥0.5.0) plus four post-validation checks
//! (RO-Crate 1.1 descriptor + 3 WRROC profile IRIs in `conformsTo`,
//! ≥1 ParameterConnection entity, ≥1 p-plan:Plan entity).
//!
//! Lives in `crates/harness` rather than `crates/core` because shelling
//! to `python3` is I/O and the deterministic compiler is required to
//! stay I/O-free (CLAUDE.md "no I/O outside the emitter"). Adapter
//! pattern per Bernhardt "Functional Core, Imperative Shell".

use anyhow::{Context, Result};
use scripps_workflow_core::wrroc_validator::{ValidationReport, WrrocValidator};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Concrete validator that shells `python3 scripts/wrroc-validate.py`.
///
/// The script's location is resolved by walking up from
/// `CARGO_MANIFEST_DIR` until `scripts/wrroc-validate.py` is found —
/// matches the previous in-core lookup so test invocation contracts
/// stay byte-identical for callers.
#[derive(Debug, Default, Clone, Copy)]
pub struct PythonRuncrateWrrocValidator;

impl WrrocValidator for PythonRuncrateWrrocValidator {
    fn validate_packages(&self, packages: &[&Path]) -> Result<ValidationReport> {
        let script = find_validator_script()?;
        let mut cmd = Command::new("python3");
        cmd.arg(&script);
        for p in packages {
            cmd.arg(p);
        }

        let output = cmd
            .output()
            .with_context(|| format!("invoking {}", script.display()))?;

        let report: ValidationReport =
            serde_json::from_slice(&output.stdout).with_context(|| {
                format!(
                    "parsing wrroc-validate.py JSON output (stderr was: {})",
                    String::from_utf8_lossy(&output.stderr)
                )
            })?;

        Ok(report)
    }
}

fn find_validator_script() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut dir = manifest_dir.as_path();
    loop {
        let candidate = dir.join("scripts/wrroc-validate.py");
        if candidate.exists() {
            return Ok(candidate);
        }
        dir = dir.parent().ok_or_else(|| {
            anyhow::anyhow!(
                "scripts/wrroc-validate.py not found above {}",
                manifest_dir.display()
            )
        })?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    #[ignore = "requires runcrate>=0.5.0 installed via requirements-validator.txt"]
    fn validator_rejects_empty_metadata() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("ro-crate-metadata.json"),
            r#"{"@context": "https://w3id.org/ro/crate/1.1/context", "@graph": []}"#,
        )
        .unwrap();

        let report = PythonRuncrateWrrocValidator
            .validate_packages(&[dir.path()])
            .unwrap();
        assert_eq!(report.summary.failed, 1);
        assert!(!report.validated[0].ok);
        assert!(report.validated[0]
            .errors
            .iter()
            .any(|e| e.contains("ParameterConnection") || e.contains("descriptor")));
    }
}
