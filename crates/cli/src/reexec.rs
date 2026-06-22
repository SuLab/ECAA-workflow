//! `ecaa-workflow reexec --parent <DIR> --replay <DIR>` — standalone
//! re-execution equivalence classification.
//!
//! Compares the result tables of a `--parent` package against a `--replay`
//! package and writes a [`ReexecutionReport`] to `runtime/reexecution.json`
//! (under `--into` when given, else under `--replay`). Until this
//! subcommand existed, [`classify_reexecution`] was only reachable from the
//! chat emit pipeline; this surface lets an operator run the comparator
//! against any pair of package directories.
//!
//! Exit code:
//! - `0` when `per_artifact` is non-empty AND every artifact landed in a
//!   passing bucket (`byte_identical` / `semantic_equivalent` /
//!   `acknowledged_non_determinism`).
//! - non-zero when any artifact is `failed`/`unavailable`, or when
//!   `per_artifact` is empty (nothing was compared).

use std::path::PathBuf;

use anyhow::{Context, Result};
// `ReexecutionBucket` is re-exported by core (`pub use
// ecaa_workflow_types::ReexecutionBucket`), so the CLI reaches it through
// core rather than taking a direct dependency on the types crate.
use ecaa_workflow_core::reexecution::{classify_reexecution, ReexecutionBucket};
use ecaa_workflow_core::reexecution_bounds::ModalityBounds;

#[derive(clap::Args, Debug)]
pub(crate) struct ReexecArgs {
    /// Parent package directory (the original run; its result tables are
    /// the reference side of the comparison).
    #[arg(long)]
    parent: PathBuf,
    /// Replay package directory (the re-execution to classify against the
    /// parent).
    #[arg(long)]
    replay: PathBuf,
    /// Package directory to write `runtime/reexecution.json` into. When
    /// omitted, the report is written under `--replay`.
    #[arg(long)]
    into: Option<PathBuf>,
    /// Optional path to a `determinism-shim.json` from the parent package.
    /// When omitted, `<parent>/runtime/determinism-shim.json` is used if
    /// present.
    #[arg(long)]
    policy: Option<PathBuf>,
}

pub(crate) fn run(args: ReexecArgs) -> Result<()> {
    // Use the crate's default relative-band bounds (the historical ±5%
    // placeholder), matching what the emit pipeline threads in for an
    // unconfigured modality (see crates/conversation/src/emit/sidecars.rs).
    let bounds = ModalityBounds::default();

    let report = classify_reexecution(
        &args.parent,
        &args.replay,
        args.policy.as_deref(),
        bounds,
    )
    .context("reexecution::classify_reexecution")?;

    // Resolve the destination package dir: --into when given, else --replay.
    let dest_pkg = args.into.as_deref().unwrap_or(&args.replay);
    let runtime_dir = dest_pkg.join("runtime");
    std::fs::create_dir_all(&runtime_dir)
        .with_context(|| format!("creating {}", runtime_dir.display()))?;
    let out_path = runtime_dir.join("reexecution.json");
    let body = serde_json::to_vec_pretty(&report)
        .context("serializing runtime/reexecution.json")?;
    std::fs::write(&out_path, body)
        .with_context(|| format!("writing {}", out_path.display()))?;

    // One-line summary of bucket_counts + artifact total.
    let counts: Vec<String> = report
        .bucket_counts
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    println!(
        "reexec: {} artifact(s) [{}] → {}",
        report.per_artifact.len(),
        counts.join(" "),
        out_path.display()
    );

    // Exit code: 0 only when something was compared AND every artifact is
    // in a passing bucket.
    let all_pass = !report.per_artifact.is_empty()
        && report.per_artifact.iter().all(|ac| {
            matches!(
                ac.bucket,
                ReexecutionBucket::ByteIdentical
                    | ReexecutionBucket::SemanticEquivalent
                    | ReexecutionBucket::AcknowledgedNonDeterminism
            )
        });

    if all_pass {
        Ok(())
    } else if report.per_artifact.is_empty() {
        Err(anyhow::anyhow!(
            "reexec: no artifacts compared (no result tables found under parent)"
        ))
    } else {
        Err(anyhow::anyhow!(
            "reexec: one or more artifacts failed or were unavailable"
        ))
    }
}
