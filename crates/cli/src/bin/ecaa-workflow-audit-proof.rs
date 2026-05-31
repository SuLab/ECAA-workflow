//! Standalone re-verifier for emitted packages.
//! Usage: ecaa-workflow-audit-proof <package-root> [--strict]
//!
//! Exit codes: 0 = all Pass/Warn/Unverified; 1 = at least one Fail
//! (only when --strict). Without --strict always exits 0 (warn-only).
//!
//! WRROC validator selection: when `ECAA_CONFORMANCE_MODE` is truthy the
//! runcrate-backed `PythonRuncrateWrrocValidator` (harness) is injected so
//! Invariant 6 (substrate-validity) reflects a real conformance check;
//! otherwise the `NoopWrrocValidator` is used (Invariant 6 → Unverified).

use ecaa_workflow_core::audit_proof::{run_audit_proof, InvariantStatus};
use ecaa_workflow_core::clock::WallClock;
use ecaa_workflow_core::wrroc_validator::{NoopWrrocValidator, WrrocValidator};
use std::path::PathBuf;

/// Truthy parse of `ECAA_CONFORMANCE_MODE` (matches the conformance-mode
/// switch used by the emit-time validator).
fn conformance_mode() -> bool {
    matches!(
        std::env::var("ECAA_CONFORMANCE_MODE")
            .as_deref()
            .unwrap_or("0"),
        "1" | "true" | "yes" | "on"
    )
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root: PathBuf = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: ecaa-workflow-audit-proof <root> [--strict]"))?
        .into();
    let strict = args.any(|a| a == "--strict");
    let validator: Box<dyn WrrocValidator> = if conformance_mode() {
        Box::new(ecaa_workflow_harness::wrroc_validator_impl::PythonRuncrateWrrocValidator)
    } else {
        Box::new(NoopWrrocValidator)
    };
    let report = run_audit_proof(&root, validator.as_ref(), &WallClock)?;
    let json = serde_json::to_string_pretty(&report)?;
    println!("{}", json);
    if strict
        && report
            .verdicts
            .iter()
            .any(|v| v.status == InvariantStatus::Fail)
    {
        std::process::exit(1);
    }
    Ok(())
}
