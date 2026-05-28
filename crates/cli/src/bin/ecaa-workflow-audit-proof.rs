//! Standalone re-verifier for emitted packages.
//! Usage: ecaa-workflow-audit-proof <package-root> [--strict]
//!
//! Exit codes: 0 = all Pass/Warn/Unverified; 1 = at least one Fail
//! (only when --strict). Without --strict always exits 0 (warn-only).

use ecaa_workflow_core::audit_proof::{run_audit_proof, InvariantStatus};
use ecaa_workflow_core::wrroc_validator::{NoopWrrocValidator, WrrocValidator};
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root: PathBuf = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: ecaa-workflow-audit-proof <root> [--strict]"))?
        .into();
    let strict = args.any(|a| a == "--strict");
    let validator: Box<dyn WrrocValidator> = Box::new(NoopWrrocValidator);
    let report = run_audit_proof(&root, validator.as_ref())?;
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
