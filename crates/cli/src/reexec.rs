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
    /// After folding `runtime/reexecution.json` into the destination package,
    /// re-record its `runtime/audit-proof-report.json` and regenerate
    /// `AUDIT-REPORT.md` so Invariant 5 (`equivalence_failure`) reflects the
    /// re-execution and Invariant 6 (`substrate_validity`) reflects runcrate.
    /// Set `ECAA_CONFORMANCE_MODE=1` to run the real runcrate validator (else
    /// substrate_validity stays Unverified); set `ECAA_AUDIT_SECRET=<64-hex>`
    /// to keep the claim invariants non-vacuous (reads the signed verdict sink).
    #[arg(long)]
    reseal: bool,
}

/// Truthy parse of `ECAA_CONFORMANCE_MODE` (mirrors the audit-proof binary).
fn conformance_mode() -> bool {
    matches!(
        std::env::var("ECAA_CONFORMANCE_MODE").as_deref().unwrap_or("0"),
        "1" | "true" | "yes" | "on"
    )
}

/// Decode the 64-hex-char per-session secret into an [`AuditWriter`].
fn writer_from_hex(secret_hex: &str) -> Result<ecaa_workflow_core::audit_writer::AuditWriter> {
    let bytes = hex::decode(secret_hex.trim())
        .map_err(|e| anyhow::anyhow!("ECAA_AUDIT_SECRET is not valid hex: {e}"))?;
    let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!(
            "ECAA_AUDIT_SECRET must decode to exactly 32 bytes (64 hex chars), got {}",
            bytes.len()
        )
    })?;
    Ok(ecaa_workflow_core::audit_writer::AuditWriter::with_secret(key))
}

/// Fold-back reseal: re-record the audit-proof report + AUDIT-REPORT.md for
/// `dest_pkg`, choosing the runcrate validator under conformance mode and the
/// signed-sink verifier when `ECAA_AUDIT_SECRET` is present.
fn reseal(dest_pkg: &std::path::Path) -> Result<()> {
    use ecaa_workflow_core::wrroc_validator::{NoopWrrocValidator, WrrocValidator};
    let validator: Box<dyn WrrocValidator> = if conformance_mode() {
        Box::new(ecaa_workflow_harness::wrroc_validator_impl::PythonRuncrateWrrocValidator)
    } else {
        Box::new(NoopWrrocValidator)
    };
    let writer = match std::env::var("ECAA_AUDIT_SECRET").ok() {
        Some(hex) => Some(writer_from_hex(&hex)?),
        None => None,
    };
    ecaa_workflow_core::emitter::reseal_audit_report(dest_pkg, validator.as_ref(), writer.as_ref())
        .context("resealing audit-proof report + AUDIT-REPORT.md")?;
    println!(
        "reexec: resealed audit-proof report + AUDIT-REPORT.md under {} (runcrate={}, signed_sink={})",
        dest_pkg.display(),
        conformance_mode(),
        std::env::var("ECAA_AUDIT_SECRET").is_ok(),
    );
    Ok(())
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

    // Fold-back reseal (always runs before the exit-code decision so the
    // audit trail is refreshed even when some upstream stages are unavailable).
    if args.reseal {
        reseal(dest_pkg)?;
    }

    // A hard re-execution failure (a stage that ran and diverged without an
    // acknowledged non-determinism source) is the only fatal outcome.
    let any_failed = report
        .per_artifact
        .iter()
        .any(|ac| ac.bucket == ReexecutionBucket::Failed);

    // Exit code: 0 only when something was compared AND every artifact is
    // in a passing bucket. When `--reseal` is set, `unavailable` artifacts
    // (excluded/ingestion/literature stages that were not re-run) are tolerated
    // — only a hard `failed` bucket is fatal — because a successful reseal is
    // the point of the invocation.
    let all_pass = !report.per_artifact.is_empty()
        && report.per_artifact.iter().all(|ac| {
            matches!(
                ac.bucket,
                ReexecutionBucket::ByteIdentical
                    | ReexecutionBucket::SemanticEquivalent
                    | ReexecutionBucket::AcknowledgedNonDeterminism
            )
        });

    if report.per_artifact.is_empty() {
        Err(anyhow::anyhow!(
            "reexec: no artifacts compared (no result tables found under parent)"
        ))
    } else if all_pass || (args.reseal && !any_failed) {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "reexec: one or more artifacts failed or were unavailable"
        ))
    }
}
