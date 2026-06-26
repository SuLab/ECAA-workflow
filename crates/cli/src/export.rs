//! `ecaa-workflow export --package <DIR> --out <FILE.zip>` — emit a clean,
//! deposit-ready `.zip` of a completed package.
//!
//! Thin CLI wrapper over the core exporter: it runs
//! [`export_depositable_package`] (copy the A+B audit/review/deposit +
//! re-execution surface into a tempdir, re-seal BagIt + RO-Crate, strip
//! `.git`) and then [`zip_dir`] that clean tree into `--out`. The tempdir is
//! removed when its `TempDir` guard drops at the end of `run`, so the only
//! durable output is the requested `.zip`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use ecaa_workflow_core::emitter::{
    export_depositable_package_with_profile, zip_dir, DepositProfile,
};

#[derive(clap::Args, Debug)]
pub(crate) struct ExportArgs {
    /// Completed package directory to export (its tier A+B files are
    /// copied; caches, logs, `.git`, and regenerable manifests are dropped).
    #[arg(long)]
    package: PathBuf,
    /// Destination `.zip` path for the depositable package.
    #[arg(long)]
    out: PathBuf,
    /// Deposit profile: `full` (everything A+B), `re-executable` (drops the
    /// policy-doc catalog + redundant artifacts, keeps the re-execution tier;
    /// still replays), or `minimal` (also drops the re-execution tier —
    /// audit/review-complete only).
    #[arg(long, default_value = "full")]
    profile: String,
}

pub(crate) fn run(args: ExportArgs) -> Result<()> {
    // Export the A+B surface into a scratch tempdir; it is deleted when
    // `staging` drops at end of scope.
    let profile: DepositProfile = args
        .profile
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;
    let staging = tempfile::tempdir().context("creating export staging tempdir")?;
    let export_root = staging.path().join("export");
    let report = export_depositable_package_with_profile(&args.package, &export_root, profile)
        .with_context(|| format!("exporting package {}", args.package.display()))?;

    // Zip the clean tree into `--out`. Parent dirs are created so a caller
    // can target a fresh path.
    if let Some(parent) = args.out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating output parent {}", parent.display()))?;
        }
    }
    let mut out_file = std::fs::File::create(&args.out)
        .with_context(|| format!("creating {}", args.out.display()))?;
    zip_dir(&export_root, &mut out_file)
        .with_context(|| format!("zipping export into {}", args.out.display()))?;

    println!(
        "export[{}]: {} kept / {} dropped → {}",
        profile,
        report.kept,
        report.dropped,
        args.out.display()
    );
    Ok(())
}
