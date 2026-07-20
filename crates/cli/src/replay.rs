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
use ecaa_workflow_core::replay::{run_replay, PackageTrust, ReplayOptions, ReplayVerdict, Tier};

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

/// Declare the re-executable deposit profile in the process environment when
/// the replay tier actually re-executes compute (`execute`/`all`).
///
/// A `--tier execute|all` replay RE-EXECUTES the package's deterministic
/// compute to confirm reproducibility — an unambiguous re-executable deposit
/// context. The value is the canonical `REEXECUTABLE_PROFILE` token the deposit
/// gate shares, and the name is the harness `ENV_DEPOSIT_PROFILE` the sandbox
/// resolver reads, so any execution launched under this process tree that
/// consults the harness filesystem-sandbox resolver
/// (`sandbox_enforcer::resolve_local_sandbox_mode`) defaults bwrap enforcement
/// ON unless the operator opts out with `ECAA_LOCAL_SANDBOX=off`. No-op for
/// `--tier verify`, which runs no compute. Returns `true` when it set the var.
fn declare_reexecutable_profile_if_executing(tier: &Tier) -> bool {
    if matches!(tier, Tier::Execute | Tier::All) {
        std::env::set_var(
            ecaa_workflow_harness::sandbox_enforcer::ENV_DEPOSIT_PROFILE,
            ecaa_workflow_core::deposit_readiness::REEXECUTABLE_PROFILE,
        );
        true
    } else {
        false
    }
}

pub(crate) fn run(args: ReplayArgs) -> Result<()> {
    let tier = Tier::from(args.tier);
    declare_reexecutable_profile_if_executing(&tier);

    let opts = ReplayOptions {
        tier,
        scratch_dir: args.scratch_dir,
        bounds: args.bounds,
        allow_rebuild: args.allow_rebuild,
        reader_version: ecaa_workflow_types::consts::ECAA_VERSION.to_string(),
        // Operator-run CLI replays act on a package the operator controls.
        trust: PackageTrust::Trusted,
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
            "replay: PARTIAL verdict — omit --strict to treat PARTIAL as success"
        )),
        ReplayVerdict::Fail => Err(anyhow::anyhow!("replay: FAIL verdict")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::deposit_readiness::REEXECUTABLE_PROFILE;
    use ecaa_workflow_harness::sandbox_enforcer::{reexecutable_profile_active, ENV_DEPOSIT_PROFILE};

    /// `--tier execute|all` marks the process as a re-executable deposit
    /// re-execution so the harness sandbox resolver defaults bwrap ON; the
    /// value must be the exact token the harness reader (`reexecutable_profile_active`)
    /// and the deposit gate agree on. `--tier verify` runs no compute and must
    /// not set the var.
    #[test]
    fn tier_execute_declares_reexecutable_profile() {
        // Serialized against the other env-mutating cases below because
        // ECAA_DEPOSIT_PROFILE is process-global.
        std::env::remove_var(ENV_DEPOSIT_PROFILE);
        assert!(!reexecutable_profile_active());

        // verify → no-op.
        assert!(!declare_reexecutable_profile_if_executing(&Tier::Verify));
        assert!(std::env::var(ENV_DEPOSIT_PROFILE).is_err());

        // execute → sets the canonical token; the harness reader agrees.
        assert!(declare_reexecutable_profile_if_executing(&Tier::Execute));
        assert_eq!(
            std::env::var(ENV_DEPOSIT_PROFILE).as_deref(),
            Ok(REEXECUTABLE_PROFILE)
        );
        assert!(reexecutable_profile_active());

        std::env::remove_var(ENV_DEPOSIT_PROFILE);

        // all → also sets it.
        assert!(declare_reexecutable_profile_if_executing(&Tier::All));
        assert!(reexecutable_profile_active());

        std::env::remove_var(ENV_DEPOSIT_PROFILE);
    }
}
