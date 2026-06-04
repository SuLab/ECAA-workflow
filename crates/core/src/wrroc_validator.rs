//! WRROC v0.5 conformance validator trait + report types.
//!
//! `crates/core` is the deterministic, I/O-free compiler. Shelling out
//! to `python3` (the original external WRROC wrapper at
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
//!   scripts/wrroc-validate.py` which wraps `runcrate report` as the
//!   released runcrate parseability check
//!   plus four post-validation checks (RO-Crate 1.1 descriptor +
//!   3 WRROC profile IRIs in conformsTo, ≥1 ParameterConnection, ≥1
//!   p-plan:Plan).

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Three-valued result of a WRROC substrate check.
///
/// The earlier design collapsed "validator did not actually run" into a
/// trivially-passing report, which let `NoopWrrocValidator` masquerade as
/// a genuine conformance pass on the live/CI path. `Unverified` makes the
/// "no real check was performed" case first-class so Invariant 6 can map
/// it to `InvariantStatus::Unverified` instead of `Pass`.
///
/// `#[non_exhaustive]` per the workspace default for new public enums:
/// downstream consumers (a second ECAA implementation linking this crate)
/// must not exhaustively `match`, so adding a future outcome variant
/// (e.g. `Skipped`) stays a minor change. The only in-tree matcher is
/// `check_substrate_validity`, which lives in this crate and exhausts the
/// current set directly.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WrrocOutcome {
    /// runcrate (or an equivalent real validator) ran and reported zero
    /// per-package failures.
    Pass,
    /// runcrate ran and reported one or more failures; the messages are
    /// the per-package error strings (prefixed with the package path).
    Fail(Vec<String>),
    /// No real validation was performed (e.g. `NoopWrrocValidator`, or the
    /// runcrate toolchain is unavailable). The string explains why.
    Unverified(String),
}

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

    /// Three-valued substrate-validity outcome for the audit-proof
    /// Invariant 6. The default implementation derives the outcome from
    /// [`validate_packages`]: zero failures ⇒ `Pass`, ≥1 failure ⇒
    /// `Fail`, a validator error ⇒ `Unverified`. A validator that does
    /// not actually run a conformance check (e.g. the no-op adapter)
    /// overrides this to return `Unverified` so a non-run is never
    /// reported as a pass.
    fn validate_outcome(&self, packages: &[&Path]) -> WrrocOutcome {
        match self.validate_packages(packages) {
            Ok(report) => {
                if report.summary.failed == 0 {
                    WrrocOutcome::Pass
                } else {
                    let msgs: Vec<String> = report
                        .validated
                        .iter()
                        .filter(|p| !p.ok)
                        .flat_map(|p| {
                            let path = p.path.clone();
                            p.errors.iter().map(move |e| format!("{path}: {e}"))
                        })
                        .collect();
                    WrrocOutcome::Fail(msgs)
                }
            }
            Err(e) => WrrocOutcome::Unverified(format!("validator error: {e}")),
        }
    }
}

/// Trivial validator: returns an all-OK [`ValidationReport`] so callers
/// that only inspect `summary.failed` keep working, but reports
/// [`WrrocOutcome::Unverified`] from [`validate_outcome`] — because no
/// real WRROC conformance check was performed. Provided in core so
/// downstream crates that don't link `harness` (test helpers, the
/// `crates/server` chat surface in offline mode) still satisfy the
/// trait without dragging in a Python subprocess.
///
/// Note the asymmetry: `validate_packages` returns `failed: 0` (so the
/// legacy report shape stays valid), while `validate_outcome` returns
/// `Unverified` (so the audit-proof Invariant 6 does NOT record a
/// substrate-validity pass that never actually happened).
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

    fn validate_outcome(&self, _packages: &[&Path]) -> WrrocOutcome {
        WrrocOutcome::Unverified("runcrate not run".to_string())
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

    #[test]
    fn noop_validator_outcome_is_unverified_not_pass() {
        let p = PathBuf::from("/tmp/pkg-a");
        // The legacy report shape still reports zero failures…
        let report = NoopWrrocValidator
            .validate_packages(&[p.as_path()])
            .unwrap();
        assert_eq!(report.summary.failed, 0);
        // …but the three-valued outcome must NOT claim a pass, because no
        // real WRROC conformance check was performed.
        match NoopWrrocValidator.validate_outcome(&[p.as_path()]) {
            WrrocOutcome::Unverified(msg) => assert!(
                msg.contains("runcrate"),
                "expected a 'runcrate not run' explanation, got: {msg}"
            ),
            other => panic!("expected Unverified, got {other:?}"),
        }
    }

    /// A stub that reports failures so the DEFAULT `validate_outcome`
    /// (used by real validators like `PythonRuncrateWrrocValidator`) maps
    /// failures to `Fail` and clean reports to `Pass`.
    struct FailingStub;
    impl WrrocValidator for FailingStub {
        fn validate_packages(&self, packages: &[&Path]) -> anyhow::Result<ValidationReport> {
            let validated: Vec<PackageResult> = packages
                .iter()
                .map(|p| PackageResult {
                    path: p.display().to_string(),
                    ok: false,
                    errors: vec!["missing conformsTo: https://w3id.org/ecaa/v0.1".to_string()],
                    profiles: Vec::new(),
                })
                .collect();
            let total = validated.len();
            Ok(ValidationReport {
                summary: ValidationSummary {
                    total,
                    passed: 0,
                    failed: total,
                },
                validated,
            })
        }
    }

    #[test]
    fn default_outcome_maps_failures_to_fail_with_messages() {
        let p = PathBuf::from("/tmp/pkg-a");
        match FailingStub.validate_outcome(&[p.as_path()]) {
            WrrocOutcome::Fail(msgs) => {
                assert_eq!(msgs.len(), 1);
                assert!(msgs[0].contains("ecaa/v0.1"), "got: {msgs:?}");
                assert!(msgs[0].contains("/tmp/pkg-a"), "path-prefixed: {msgs:?}");
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    /// A stub that reports a clean run so the DEFAULT outcome maps to Pass.
    struct CleanStub;
    impl WrrocValidator for CleanStub {
        fn validate_packages(&self, packages: &[&Path]) -> anyhow::Result<ValidationReport> {
            let total = packages.len();
            Ok(ValidationReport {
                summary: ValidationSummary {
                    total,
                    passed: total,
                    failed: 0,
                },
                validated: packages
                    .iter()
                    .map(|p| PackageResult {
                        path: p.display().to_string(),
                        ok: true,
                        errors: Vec::new(),
                        profiles: Vec::new(),
                    })
                    .collect(),
            })
        }
    }

    #[test]
    fn default_outcome_maps_clean_report_to_pass() {
        let p = PathBuf::from("/tmp/pkg-a");
        assert_eq!(
            CleanStub.validate_outcome(&[p.as_path()]),
            WrrocOutcome::Pass
        );
    }
}
