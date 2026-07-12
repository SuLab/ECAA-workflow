//! `ecaa-workflow export --package <DIR> --out <FILE.zip>` — emit a clean,
//! deposit-ready `.zip` (or `--dir`) of a completed package.
//!
//! Thin CLI wrapper over the core exporter. The core
//! [`export_depositable_package_with_profile`] copies the A+B audit/review/
//! deposit + re-execution surface, re-seals BagIt + RO-Crate, strips `.git`, and
//! then SELF-VALIDATES the sealed deposit (Layer 1: RO-Crate + BagIt integrity),
//! stamping `DEPOSIT-READINESS.json`. This handler adds Layer 2: for the
//! `re-executable` profile it runs an agent-free re-execution of the deposit's
//! compute (`replay --tier execute`) and folds the verdict into the attestation,
//! unless `--no-reexec-check` is given (recorded honestly as `not_verified`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ecaa_workflow_core::clock::WallClock;
use ecaa_workflow_core::deposit_readiness::{
    reexec_status_from_verdict, update_deposit_readiness_reexecution, ReexecStatus,
};
use ecaa_workflow_core::emitter::{
    export_depositable_package_with_profile, zip_dir, DepositProfile,
};
use ecaa_workflow_core::replay::{run_replay, PackageTrust, ReplayOptions, Tier};

#[derive(clap::Args, Debug)]
pub(crate) struct ExportArgs {
    /// Completed package directory to export (its tier A+B files are
    /// copied; caches, logs, `.git`, and regenerable manifests are dropped).
    #[arg(long)]
    package: PathBuf,
    /// Destination `.zip` path for the depositable package. Mutually exclusive
    /// with `--dir`; exactly one must be given.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Destination DIRECTORY for the depositable package (written in place, not
    /// zipped). Use this when the deposit must preserve symlinks — e.g. a
    /// `re-executable` profile that ships a per-run conda env, whose relative
    /// symlinks a `.zip` round-trip would drop. Mutually exclusive with `--out`.
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Deposit profile: `full` (everything A+B), `re-executable` (drops the
    /// policy-doc catalog + redundant artifacts, keeps the re-execution tier;
    /// still replays), or `minimal` (also drops the re-execution tier —
    /// audit/review-complete only).
    #[arg(long, default_value = "full")]
    profile: String,
    /// Skip the automatic re-execution verification for a `re-executable`
    /// deposit (Layer 2). RO-Crate + BagIt self-validation (Layer 1) always runs
    /// regardless. When skipped, the attestation records `reexecution:
    /// not_verified`, and the downstream `deposit-check --strict` gate will
    /// refuse the package until re-execution is verified.
    #[arg(long)]
    no_reexec_check: bool,
    /// Scratch directory for the Layer-2 re-execution staging (re-executable
    /// profile only). A fresh tempdir is used when omitted.
    #[arg(long)]
    reexec_scratch_dir: Option<PathBuf>,
}

pub(crate) fn run(args: ExportArgs) -> Result<()> {
    let profile: DepositProfile = args
        .profile
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;

    match (args.out.as_ref(), args.dir.as_ref()) {
        (Some(_), Some(_)) | (None, None) => {
            anyhow::bail!("exactly one of --out <FILE.zip> or --dir <DIR> must be given");
        }
        // Directory export: write the deposit tree in place, preserving
        // symlinks (a `.zip` round-trip via `zip_dir` would drop them, breaking
        // a shipped conda env).
        (None, Some(dir)) => {
            let report = export_depositable_package_with_profile(&args.package, dir, profile)
                .with_context(|| format!("exporting package {}", args.package.display()))?;
            println!(
                "export[{}]: {} kept / {} dropped → {}",
                profile,
                report.kept,
                report.dropped,
                dir.display()
            );
            maybe_verify_reexecution(dir, profile, args.no_reexec_check, &args.reexec_scratch_dir)?;
            Ok(())
        }
        // Zip export: build the tree in a scratch tempdir, then zip into `--out`.
        (Some(out), None) => {
            let staging = tempfile::tempdir().context("creating export staging tempdir")?;
            let export_root = staging.path().join("export");
            let report = export_depositable_package_with_profile(&args.package, &export_root, profile)
                .with_context(|| format!("exporting package {}", args.package.display()))?;
            // Layer 2 runs on the sealed tree BEFORE zipping so the attestation
            // the `.zip` carries reflects the re-execution verdict.
            maybe_verify_reexecution(
                &export_root,
                profile,
                args.no_reexec_check,
                &args.reexec_scratch_dir,
            )?;
            if let Some(parent) = out.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("creating output parent {}", parent.display()))?;
                }
            }
            let mut out_file = std::fs::File::create(out)
                .with_context(|| format!("creating {}", out.display()))?;
            zip_dir(&export_root, &mut out_file)
                .with_context(|| format!("zipping export into {}", out.display()))?;
            println!(
                "export[{}]: {} kept / {} dropped → {}",
                profile,
                report.kept,
                report.dropped,
                out.display()
            );
            Ok(())
        }
    }
}

/// Layer 2: for a `re-executable` deposit, re-execute its deterministic compute
/// (agent-free) and fold the verdict into `DEPOSIT-READINESS.json`. A skip
/// (`--no-reexec-check`, or a non-`re-executable` profile) leaves the Layer-1
/// `reexecution: not_verified` stamp untouched.
fn maybe_verify_reexecution(
    dst: &Path,
    profile: DepositProfile,
    no_reexec_check: bool,
    scratch_dir: &Option<PathBuf>,
) -> Result<()> {
    if profile != DepositProfile::ReExecutable {
        return Ok(());
    }
    if no_reexec_check {
        println!(
            "  reexec-check: SKIPPED (--no-reexec-check) → DEPOSIT-READINESS.json reexecution=not_verified"
        );
        return Ok(());
    }

    println!("  reexec-check: re-executing deposit compute to verify reproducibility…");
    let opts = ReplayOptions {
        tier: Tier::Execute,
        scratch_dir: scratch_dir.clone(),
        bounds: None,
        // Rely on the recorded image / image-fallback rather than rebuilding
        // from a Dockerfile inside an export.
        allow_rebuild: false,
        reader_version: ecaa_workflow_types::consts::ECAA_VERSION.to_string(),
        trust: PackageTrust::Trusted,
    };
    let report = run_replay(dst, &opts)
        .with_context(|| format!("re-executing deposit {} for readiness", dst.display()))?;

    let unprovisionable = report
        .reexecute
        .as_ref()
        .map(|r| r.unprovisionable)
        .unwrap_or(false);
    // No execution environment at all → we could not verify; record honestly as
    // not_verified rather than letting the unprovisionable→Partial mapping imply
    // a partial reproduction that never ran.
    let status = if unprovisionable {
        ReexecStatus::NotVerified
    } else {
        reexec_status_from_verdict(&report.verdict)
    };

    let detail = summarize_reexecution(&report);
    update_deposit_readiness_reexecution(dst, status, Some(detail.clone()), None, &WallClock)
        .context("recording re-execution verdict into DEPOSIT-READINESS.json")?;

    println!("  reexec-check: reexecution={status:?} — {detail}");
    Ok(())
}

/// One-line human summary of a replay report's re-execution buckets + skips.
fn summarize_reexecution(report: &ecaa_workflow_core::replay::ReplayReport) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(re) = &report.reexecute {
        let counts: Vec<String> = re
            .report
            .bucket_counts
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        parts.push(format!("env_tier={} [{}]", re.env_tier, counts.join(" ")));
        if re.unprovisionable {
            parts.push("unprovisionable".to_string());
        }
    }
    if !report.skipped.is_empty() {
        parts.push(format!("{} stage(s) skipped (not offline-reproducible)", report.skipped.len()));
    }
    if parts.is_empty() {
        "no re-execution artifacts".to_string()
    } else {
        parts.join("; ")
    }
}
