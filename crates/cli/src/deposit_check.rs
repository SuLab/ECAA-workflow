//! `ecaa-workflow deposit-check <DIR>` — the deposit gate (Layer 3).
//!
//! Reads a package's `DEPOSIT-READINESS.json` attestation and refuses (non-zero
//! exit) any package that was not produced by a self-validating export, or whose
//! RO-Crate / BagIt self-validation failed, or whose re-execution FAILED. Run
//! this before trusting a package as deposit-grade (e.g. before copying a
//! deposit into a paper/archive location).
//!
//! Exit code:
//! - `0` when the attestation is present and passing (`--strict` additionally
//!   requires re-execution to have been verified — `partial`/`pass`).
//! - non-zero when the attestation is missing, a check failed, re-execution
//!   FAILED, or (`--strict`) re-execution was `not_verified`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use ecaa_workflow_core::deposit_readiness::check_deposit_readiness;

#[derive(clap::Args, Debug)]
pub(crate) struct DepositCheckArgs {
    /// Path to the deposit/package directory to gate.
    #[arg()]
    package: PathBuf,
    /// Also refuse a package whose re-execution was never verified
    /// (`reexecution: not_verified`). Without `--strict`, a not-verified
    /// re-execution is allowed but surfaced as a warning.
    #[arg(long)]
    strict: bool,
}

pub(crate) fn run(args: DepositCheckArgs) -> Result<()> {
    let dr = check_deposit_readiness(&args.package, args.strict)
        .with_context(|| format!("deposit-check refused {}", args.package.display()))?;

    println!(
        "deposit-check: PASS  package={}\n  profile={}  deposit_ready={}  ro_crate={:?}  bagit={:?}  domain_validation={:?}  reexecution={:?}",
        args.package.display(),
        dr.profile,
        dr.deposit_ready,
        dr.ro_crate,
        dr.bagit,
        dr.domain_validation,
        dr.reexecution,
    );
    if let Some(detail) = &dr.detail {
        println!("  detail: {detail}");
    }
    // DR-8: surface residual non-portability as a non-fatal advisory. These are
    // deliberately not gated (a re-executable deposit may need some absolute
    // host paths to replay), but an operator relocating/archiving the deposit
    // should know it carries host-specific state.
    if !dr.portability_warnings.is_empty() {
        eprintln!(
            "  warning: deposit is not fully portable ({} residual host-path / session-id reference(s)); \
             re-execution may depend on these absolute paths. First few:",
            dr.portability_warnings.len()
        );
        for w in dr.portability_warnings.iter().take(5) {
            eprintln!("    - {w}");
        }
    }
    // Surface a non-fatal warning when re-execution was not verified (allowed
    // only because --strict was not given).
    if matches!(
        dr.reexecution,
        ecaa_workflow_core::deposit_readiness::ReexecStatus::NotVerified
    ) {
        eprintln!(
            "  warning: re-execution was NOT verified for this deposit; \
             run `ecaa-workflow replay {} --tier execute` or re-export without --no-reexec-check",
            args.package.display()
        );
    }
    Ok(())
}
