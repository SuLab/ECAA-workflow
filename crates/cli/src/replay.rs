//! `ecaa-workflow replay <package>` — agent-free re-verify + re-execute.
//!
//! Re-verifies a downloaded ECAA package's recorded verdicts (offline) and
//! optionally re-executes its deterministic compute in a tiered
//! container/host environment to confirm result tables reproduce.
//!
//! Exit code:
//! - `0` when verdict is `Pass`.
//! - `0` when verdict is `Partial` (unless `--strict` is given).
//! - non-zero when verdict is `Fail`, or when verdict is `Partial` with
//!   `--strict`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use ecaa_workflow_core::replay::{run_replay, ReplayOptions, ReplayVerdict, Tier};

#[derive(clap::Args, Debug)]
pub(crate) struct ReplayArgs {
    /// Path to the ECAA package directory to replay.
    #[arg()]
    package: PathBuf,
    /// Which stage(s) to run: `verify` (re-verify only), `execute`
    /// (re-execute only), or `all` (both). Defaults to `all`.
    #[arg(long, value_enum, default_value = "all")]
    tier: TierArg,
    /// Scratch directory for re-execution staging. A fresh tempdir is
    /// created when omitted. The caller owns cleanup of any supplied dir.
    #[arg(long)]
    scratch_dir: Option<PathBuf>,
    /// Path to a `ModalityBoundsProvider` directory
    /// (`config/reexecution-bounds/`) used to resolve per-modality
    /// tolerances. The historical ±5% relative band is used when omitted.
    #[arg(long)]
    bounds: Option<PathBuf>,
    /// Allow rebuilding the container image from a Dockerfile when the
    /// recorded digest is unavailable.
    #[arg(long)]
    allow_rebuild: bool,
    /// Write the full `ReplayReport` as JSON to this path.
    #[arg(long)]
    json: Option<PathBuf>,
    /// Treat a `Partial` verdict as a failure (exit non-zero).
    #[arg(long)]
    strict: bool,
}

/// Clap-parseable mirror of `Tier` (clap's ValueEnum requires a local type).
#[derive(clap::ValueEnum, Clone, Debug)]
enum TierArg {
    /// Run only the deterministic verifiers (re-verify stage).
    Verify,
    /// Run only the compute re-execution stage.
    Execute,
    /// Run both re-verify and re-execute stages.
    All,
}

impl From<TierArg> for Tier {
    fn from(t: TierArg) -> Tier {
        match t {
            TierArg::Verify  => Tier::Verify,
            TierArg::Execute => Tier::Execute,
            TierArg::All     => Tier::All,
        }
    }
}

pub(crate) fn run(args: ReplayArgs) -> Result<()> {
    let opts = ReplayOptions {
        tier: Tier::from(args.tier),
        scratch_dir: args.scratch_dir,
        bounds: args.bounds,
        allow_rebuild: args.allow_rebuild,
        reader_version: env!("CARGO_PKG_VERSION").to_string(),
    };

    let report = run_replay(&args.package, &opts)
        .with_context(|| format!("replay failed for package {}", args.package.display()))?;

    // ── Human summary ────────────────────────────────────────────────────────
    let verdict_str = match report.verdict {
        ReplayVerdict::Pass    => "PASS",
        ReplayVerdict::Partial => "PARTIAL",
        ReplayVerdict::Fail    => "FAIL",
    };
    println!("replay: {verdict_str}  package={}", args.package.display());

    if let Some(rv) = &report.reverify {
        let n_checks = rv.checks.len();
        let n_diverged = rv.checks.iter().filter(|c| c.diverged).count();
        println!(
            "  re-verify: {n_checks} check(s), {n_diverged} diverged  \
             (reader_matches_writer={})",
            rv.reader_matches_writer
        );
    }

    if let Some(re) = &report.reexecute {
        let n_artifacts = re.report.per_artifact.len();
        let counts: Vec<String> = re
            .report
            .bucket_counts
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        println!(
            "  re-execute: env_tier={} {n_artifacts} artifact(s) [{}]{}",
            re.env_tier,
            counts.join(" "),
            if re.unprovisionable { "  (unprovisionable)" } else { "" },
        );
    }

    if !report.skipped.is_empty() {
        println!("  skipped {} stage(s):", report.skipped.len());
        for s in &report.skipped {
            println!("    {} — {}", s.task, s.reason);
        }
    }

    // ── Optional JSON output ─────────────────────────────────────────────────
    if let Some(json_path) = &args.json {
        if let Some(parent) = json_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating parent dir {}", parent.display()))?;
            }
        }
        let body = serde_json::to_vec_pretty(&report)
            .context("serializing ReplayReport to JSON")?;
        std::fs::write(json_path, body)
            .with_context(|| format!("writing {}", json_path.display()))?;
        println!("  report written → {}", json_path.display());
    }

    // ── Verdict → exit code (mirrors reexec.rs: Err(anyhow!) for non-zero) ──
    match report.verdict {
        ReplayVerdict::Pass => Ok(()),
        ReplayVerdict::Partial if !args.strict => Ok(()),
        ReplayVerdict::Partial => Err(anyhow::anyhow!(
            "replay: PARTIAL verdict (re-run with --strict suppressed to allow partial)"
        )),
        ReplayVerdict::Fail => Err(anyhow::anyhow!("replay: FAIL verdict")),
    }
}
