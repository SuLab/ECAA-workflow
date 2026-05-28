//! WRROC v0.5 conformance validator trait + report types.
//!
//! `crates/core` is the deterministic, I/O-free compiler. Shelling out
//! to `python3` (the original `runcrate validate` wrapper at
//! `ecaa-workflow-core::wrroc_validator::validate_packages`) violates
//! the "no I/O outside the emitter" invariant asserted by CLAUDE.md, so
//! the subprocess impl now lives in
//! `crates/harness/src/wrroc_validator_impl.rs` under the
//! `PythonRuncrateWrrocValidator` adapter. Core retains only the trait,
//! the wire-shape report types (`ValidationReport`, `PackageResult`,
//! `ValidationSummary`), and a `NoopWrrocValidator` impl that returns
//! an all-OK report — useful for offline/CI without Python.
//!
//! The two trait impls each cover one runtime:
//! - `NoopWrrocValidator` (core, this module): every package validates
//!   trivially. Use when WRROC conformance is out of scope for the run
//!   (smoke tests, offline replay, fixture authoring).
//! - `PythonRuncrateWrrocValidator` (harness): shells `python3
//!   scripts/wrroc-validate.py` which wraps `runcrate validate` ≥0.5.0
//!   plus four post-validation checks (RO-Crate 1.1 descriptor +
//!   3 WRROC profile IRIs in conformsTo, ≥1 ParameterConnection, ≥1
//!   p-plan:Plan).

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// ValidationReport data.
pub struct ValidationReport {
    /// Validated.
    pub validated: Vec<PackageResult>,
    /// Summary.
    pub summary: ValidationSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// PackageResult data.
pub struct PackageResult {
    /// Path.
    pub path: String,
    /// Ok.
    pub ok: bool,
    #[serde(default)]
    /// Errors.
    pub errors: Vec<String>,
    #[serde(default)]
    /// Profiles.
    pub profiles: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
/// ValidationSummary data.
pub struct ValidationSummary {
    /// Total.
    pub total: usize,
    /// Passed.
    pub passed: usize,
    /// Failed.
    pub failed: usize,
}

/// Abstracts the WRROC v0.5 conformance check so `crates/core` doesn't
/// have to know whether validation is happening via `runcrate` (the
/// harness adapter) or short-circuited (the noop adapter used by
/// offline tests and CI runs that don't have the validator deps).
///
/// Callers in `crates/server`, `crates/harness`, and the WRROC
/// integration test (`crates/core/tests/wrroc_v05_fixtures.rs`)
/// receive `&dyn WrrocValidator`; production binaries inject the
/// Python adapter; integration tests can substitute the noop impl
/// when the Python toolchain isn't available on the runner.
pub trait WrrocValidator {
    /// Run the validation on one or more package directories and
    /// return the parsed report regardless of per-package failures —
    /// callers inspect `report.summary.failed` to decide pass/fail.
    fn validate_packages(&self, packages: &[&Path]) -> anyhow::Result<ValidationReport>;
}

/// Trivial validator: returns an all-OK report. Provided in core so
/// downstream crates that don't link `harness` (test helpers, the
/// `crates/server` chat surface in offline mode) still satisfy the
/// trait without dragging in a Python subprocess.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopWrrocValidator;

impl WrrocValidator for NoopWrrocValidator {
    fn validate_packages(&self, packages: &[&Path]) -> anyhow::Result<ValidationReport> {
        let validated: Vec<PackageResult> = packages
            .iter()
            .map(|p| PackageResult {
                path: p.display().to_string(),
                ok: true,
                errors: Vec::new(),
                profiles: Vec::new(),
            })
            .collect();
        let total = validated.len();
        Ok(ValidationReport {
            summary: ValidationSummary {
                total,
                passed: total,
                failed: 0,
            },
            validated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn noop_validator_reports_all_ok() {
        let p1 = PathBuf::from("/tmp/pkg-a");
        let p2 = PathBuf::from("/tmp/pkg-b");
        let report = NoopWrrocValidator
            .validate_packages(&[p1.as_path(), p2.as_path()])
            .unwrap();
        assert_eq!(report.summary.total, 2);
        assert_eq!(report.summary.passed, 2);
        assert_eq!(report.summary.failed, 0);
        assert!(report.validated.iter().all(|r| r.ok));
    }

    #[test]
    fn noop_validator_empty_inputs_yields_empty_report() {
        let report = NoopWrrocValidator.validate_packages(&[]).unwrap();
        assert_eq!(report.summary.total, 0);
        assert_eq!(report.summary.passed, 0);
        assert!(report.validated.is_empty());
    }
}
