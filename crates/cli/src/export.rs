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
            Ok(())
        }
        // Zip export: build the tree in a scratch tempdir, then zip into `--out`.
        (Some(out), None) => {
            let staging = tempfile::tempdir().context("creating export staging tempdir")?;
            let export_root = staging.path().join("export");
            let report = export_depositable_package_with_profile(&args.package, &export_root, profile)
                .with_context(|| format!("exporting package {}", args.package.display()))?;
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
