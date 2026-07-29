//! `ecaa-workflow export --package <DIR> --out <FILE.zip>` — emit a clean,
//! deposit-ready `.zip` (or `--dir`) of a completed package.
//!
//! Thin CLI wrapper over the core exporter. The core
//! [`export_depositable_package_with_profile`] copies the A+B audit/review/
//! deposit + re-execution surface, re-seals the SHA-512 manifest + RO-Crate,
//! strips `.git`, and then SELF-VALIDATES the sealed deposit (Layer 1: RO-Crate
//! + checksum integrity),
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
use ecaa_workflow_core::intake_facts::IntakeFacts;
use ecaa_workflow_core::reexecution_bounds::{ModalityBounds, ModalityBoundsProvider};
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
    /// deposit (Layer 2). RO-Crate + checksum self-validation (Layer 1) always runs
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
            maybe_verify_reexecution(
                dir,
                &args.package,
                profile,
                args.no_reexec_check,
                &args.reexec_scratch_dir,
            )?;
            Ok(())
        }
        // Zip export: build the tree in a scratch tempdir, then zip into `--out`.
        (Some(out), None) => {
            let staging = tempfile::tempdir().context("creating export staging tempdir")?;
            let export_root = staging.path().join("export");
            let report =
                export_depositable_package_with_profile(&args.package, &export_root, profile)
                    .with_context(|| format!("exporting package {}", args.package.display()))?;
            // Layer 2 runs on the sealed tree BEFORE zipping so the attestation
            // the `.zip` carries reflects the re-execution verdict.
            maybe_verify_reexecution(
                &export_root,
                &args.package,
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

/// The modality the package declares for ITSELF, read from
/// `policies/intake-facts.json` — the record the emitter writes for every
/// package and that every profile except `minimal` retains in the deposit. The
/// deposit tree is consulted first (it is what gets re-executed) and the source
/// package second, so a profile that pruned the record still resolves. `None`
/// when neither tree carries a usable one; the caller then keeps the generic
/// band rather than guessing a modality.
fn declared_modality(dst: &Path, src: &Path) -> Option<String> {
    for root in [dst, src] {
        let Ok(raw) = std::fs::read_to_string(root.join("policies/intake-facts.json")) else {
            continue;
        };
        if let Ok(facts) = serde_json::from_str::<IntakeFacts>(&raw) {
            if !facts.modality.is_empty() {
                return Some(facts.modality);
            }
        }
    }
    None
}

/// `config/reexecution-bounds/` — the per-modality tolerance registry the
/// comparator resolves its numeric band from. Routed through the typed `Config`
/// (`ECAA_CONFIG_DIR`, default `./config`), which is the single sanctioned
/// env-var read site.
fn reexecution_bounds_dir() -> PathBuf {
    ecaa_workflow_core::config::Config::from_env()
        .map(|c| c.config_dir)
        .unwrap_or_else(|_| PathBuf::from("./config"))
        .join("reexecution-bounds")
}

/// Audit label for the numeric band a deposit's declared modality resolves to,
/// so `DEPOSIT-READINESS.json` states the tolerance the equivalence verdict was
/// reached under instead of leaving a reader to assume one.
///
/// A modality with a `config/reexecution-bounds/<modality>.yaml` gets that
/// file's band. Everything else — unconfigured modality, absent registry,
/// package that declares no modality — is labelled as the generic fallback, so
/// the deposit never implies a tighter band than the one that was in force.
fn describe_applied_bounds(bounds_dir: &Path, modality: Option<&str>) -> String {
    let render = |label: &str, b: ModalityBounds| {
        format!(
            "{label} (rel {:.3}, abs {:.6})",
            b.relative_tolerance, b.absolute_tolerance
        )
    };
    match modality {
        Some(m) if bounds_dir.join(format!("{m}.yaml")).is_file() => render(
            m,
            ModalityBoundsProvider::from_dir(bounds_dir).bounds_for(m),
        ),
        Some(m) => render(
            &format!("generic (no per-modality bounds declared for {m})"),
            ModalityBounds::default(),
        ),
        None => render(
            "generic (package declares no modality)",
            ModalityBounds::default(),
        ),
    }
}

/// Layer 2: for a `re-executable` deposit, re-execute its deterministic compute
/// (agent-free) and fold the verdict into `DEPOSIT-READINESS.json`. A skip
/// (`--no-reexec-check`, or a non-`re-executable` profile) leaves the Layer-1
/// `reexecution: not_verified` stamp untouched.
///
/// `src` is the package the deposit was exported FROM; it is consulted only as a
/// fallback source for the deposit's own modality record.
fn maybe_verify_reexecution(
    dst: &Path,
    src: &Path,
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

    // Per-modality semantic-equivalence bounds. Passing `None` here applied the
    // GENERIC ±5% placeholder to every deposit regardless of what its modality
    // declares, so a `bulk_rnaseq` adjusted p-value moving 0.049 → 0.051 (3.9%
    // relative) was still stamped `semantic_equivalent` — the gene crossed FDR
    // 0.05 and the significant set changed. `config/reexecution-bounds/` exists
    // precisely to tighten that (bulk_rnaseq: rel 0.02, abs 0.001).
    //
    // `ReplayOptions::bounds` names the registry DIRECTORY; the replay resolves
    // the modality key itself from the package it is re-executing. It must
    // resolve it from the same `policies/intake-facts.json` record read here for
    // the recorded label and the band actually in force to agree.
    let bounds_dir = reexecution_bounds_dir();
    let modality = declared_modality(dst, src);
    let applied_bounds = describe_applied_bounds(&bounds_dir, modality.as_deref());

    println!("  reexec-check: re-executing deposit compute to verify reproducibility…");
    println!("  reexec-check: equivalence bounds {applied_bounds}");
    let opts = ReplayOptions {
        tier: Tier::Execute,
        scratch_dir: scratch_dir.clone(),
        bounds: Some(bounds_dir),
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

    // Fold the re-execution result back into the package's audit surface so the
    // `equivalence_failure` invariant (Invariant 5) reflects the actual verdict
    // instead of "Q absent", and — when `runcrate` is installed — the WRROC
    // `substrate_validity` invariant (Invariant 6) is verified too. Without this
    // the audit report and DEPOSIT-READINESS.json would disagree (the report
    // would still read the empty emit-time reexecution.json stub). Only when the
    // comparator actually produced rows (env provisioned + ran); an empty stub
    // would not clear "Q absent" anyway.
    if let Some(re) = report.reexecute.as_ref() {
        if !re.report.per_artifact.is_empty() {
            crate::audit_fold::write_reexecution_json(dst, &re.report)
                .context("persisting re-execution result into runtime/reexecution.json")?;
            let validator = crate::audit_fold::select_validator(true);
            crate::audit_fold::reseal_deferred(dst, validator.as_ref())
                .context("folding re-execution + substrate verdicts into the audit report")?;
            let substrate = if crate::audit_fold::runcrate_available()
                || crate::audit_fold::conformance_mode()
            {
                "verified (runcrate)"
            } else {
                "unverified (runcrate absent)"
            };
            println!(
                "  reexec-check: folded into audit report — equivalence_failure refreshed, substrate_validity {substrate}"
            );
        }
    }

    // The band rides along in the recorded detail: a `reproduced` verdict is only
    // interpretable against the tolerance that produced it.
    let detail = format!(
        "{}; bounds={applied_bounds}",
        summarize_reexecution(&report)
    );
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
        parts.push(format!(
            "{} stage(s) skipped (not offline-reproducible)",
            report.skipped.len()
        ));
    }
    if parts.is_empty() {
        "no re-execution artifacts".to_string()
    } else {
        parts.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repo's real registry: `bulk_rnaseq.yaml` declares rel 0.02 / abs
    /// 0.001, `variant_calling.yaml` rel 0.01; every other modality is
    /// unconfigured and must fall through to the generic band.
    fn registry() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/reexecution-bounds")
    }

    fn write_facts(root: &Path, modality: &str) {
        std::fs::create_dir_all(root.join("policies")).unwrap();
        std::fs::write(
            root.join("policies/intake-facts.json"),
            format!("{{\"modality\":\"{modality}\",\"methods\":[]}}"),
        )
        .unwrap();
    }

    /// The modality comes from the package's OWN record, never from a
    /// hardcoded default or the directory name — a deposit tree is written to an
    /// arbitrary `--out`/`--dir` destination.
    #[test]
    fn modality_is_read_from_the_package_record() {
        let dst = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        assert_eq!(
            declared_modality(dst.path(), src.path()),
            None,
            "no record on either side must not invent a modality"
        );

        // A profile that pruned the record from the deposit still resolves via
        // the package it was exported from.
        write_facts(src.path(), "variant_calling");
        assert_eq!(
            declared_modality(dst.path(), src.path()).as_deref(),
            Some("variant_calling")
        );

        // The deposit's own record wins: it is the tree being re-executed.
        write_facts(dst.path(), "bulk_rnaseq");
        assert_eq!(
            declared_modality(dst.path(), src.path()).as_deref(),
            Some("bulk_rnaseq")
        );
    }

    /// A modality with a declared bounds file gets THAT band, and the deposit
    /// records it — not the generic ±5% placeholder that let a padj cross FDR
    /// 0.05 while still being stamped `semantic_equivalent`.
    #[test]
    fn configured_modality_gets_its_declared_band_and_records_it() {
        let label = describe_applied_bounds(&registry(), Some("bulk_rnaseq"));
        assert!(
            label.contains("bulk_rnaseq"),
            "the band must name the modality it came from: {label}"
        );
        assert!(
            label.contains("rel 0.020") && label.contains("abs 0.001000"),
            "bulk_rnaseq.yaml declares rel 0.02 / abs 0.001: {label}"
        );
        assert!(
            !label.contains("rel 0.050"),
            "the generic placeholder must not be reported for a configured \
             modality: {label}"
        );
    }

    /// No per-modality file → the generic band, labelled as such. Falling back
    /// is correct; claiming a modality-specific tolerance would not be.
    #[test]
    fn unconfigured_modality_falls_back_to_generic_and_says_so() {
        for label in [
            describe_applied_bounds(&registry(), Some("single_cell_rnaseq")),
            describe_applied_bounds(&registry(), None),
            describe_applied_bounds(Path::new("/nonexistent/registry"), Some("bulk_rnaseq")),
        ] {
            assert!(
                label.contains("generic"),
                "an unresolved modality must be labelled generic: {label}"
            );
            assert!(
                label.contains("rel 0.050") && label.contains("abs 0.000000"),
                "the generic band is the ±5% placeholder: {label}"
            );
        }
    }
}
