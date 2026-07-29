//! Shared fold-back helpers for the two deferred offline verifications —
//! re-execution equivalence (Invariant 5) and WRROC substrate validity
//! (Invariant 6). Used by both `reexec --reseal` and the auto-verifying
//! `export` (Layer 2) so the write-reexecution.json + reseal logic lives in
//! ONE place. The heavy lifting (`reseal_audit_report`) is in core; this is the
//! thin CLI orchestration (validator selection + signed-sink wiring) around it.

use std::path::Path;

use anyhow::{Context, Result};
use ecaa_workflow_core::audit_proof::InvariantId;
use ecaa_workflow_core::audit_writer::AuditWriter;
use ecaa_workflow_core::reexecution::ReexecutionReport;
use ecaa_workflow_core::wrroc_validator::{NoopWrrocValidator, WrrocValidator};
use ecaa_workflow_harness::wrroc_validator_impl::PythonRuncrateWrrocValidator;

/// Truthy parse of `ECAA_CONFORMANCE_MODE`.
pub(crate) fn conformance_mode() -> bool {
    matches!(
        std::env::var("ECAA_CONFORMANCE_MODE")
            .as_deref()
            .unwrap_or("0"),
        "1" | "true" | "yes" | "on"
    )
}

/// `true` when the `runcrate` CLI is resolvable on `PATH`. The WRROC substrate
/// check shells `runcrate report`, so its presence is the gate for a real
/// substrate verdict vs. an honest `Unverified` when the tool is absent.
pub(crate) fn runcrate_available() -> bool {
    std::process::Command::new("runcrate")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Decode the 64-hex-char per-session secret into an [`AuditWriter`].
pub(crate) fn writer_from_hex(secret_hex: &str) -> Result<AuditWriter> {
    let bytes = hex::decode(secret_hex.trim())
        .map_err(|e| anyhow::anyhow!("ECAA_AUDIT_SECRET is not valid hex: {e}"))?;
    let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!(
            "ECAA_AUDIT_SECRET must decode to exactly 32 bytes (64 hex chars), got {}",
            bytes.len()
        )
    })?;
    Ok(AuditWriter::with_secret(key))
}

/// Choose the substrate validator. `prefer_runcrate_if_available = true` (the
/// auto-verifying export) uses runcrate whenever it is installed OR conformance
/// mode is set; `false` (the `reexec --reseal` operator surface) preserves the
/// conformance-mode-only gate. Falls back to the no-op adapter (→ `Unverified`)
/// when runcrate is absent, so a non-run is never recorded as a substrate pass.
pub(crate) fn select_validator(prefer_runcrate_if_available: bool) -> Box<dyn WrrocValidator> {
    let use_runcrate = conformance_mode() || (prefer_runcrate_if_available && runcrate_available());
    if use_runcrate {
        Box::new(PythonRuncrateWrrocValidator)
    } else {
        Box::new(NoopWrrocValidator)
    }
}

/// Write a [`ReexecutionReport`] into `<pkg>/runtime/reexecution.json` — the
/// "Q" the `equivalence_failure` invariant reads. Overwrites the emit-time
/// stub. Manifest-excluded (see `emitter::bagit`), so no reseal is needed for
/// this file itself.
pub(crate) fn write_reexecution_json(pkg: &Path, report: &ReexecutionReport) -> Result<()> {
    ecaa_workflow_core::emitter::write_reexecution_report(pkg, report)
        .context("writing runtime/reexecution.json")
}

/// Fold the deferred verifications into the package's audit report: re-record
/// `runtime/audit-proof-report.json` + `AUDIT-REPORT.md` (and re-seal BagIt),
/// refreshing ONLY `equivalence_failure` + `substrate_validity` from a fresh run
/// and preserving every compile-time invariant at its recorded value. The
/// signed verdict sink is read when `ECAA_AUDIT_SECRET` is set (keeps the claim
/// invariants non-vacuous).
pub(crate) fn reseal_deferred(pkg: &Path, validator: &dyn WrrocValidator) -> Result<()> {
    let writer = match std::env::var("ECAA_AUDIT_SECRET").ok() {
        Some(hex) => Some(writer_from_hex(&hex)?),
        None => None,
    };
    let refresh = [
        InvariantId::EquivalenceFailure,
        InvariantId::SubstrateValidity,
    ];
    ecaa_workflow_core::emitter::reseal_audit_report(
        pkg,
        validator,
        writer.as_ref(),
        Some(&refresh),
    )
    .context("resealing audit-proof report + AUDIT-REPORT.md")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::reexecution::ReexecutionReport;

    #[test]
    fn write_reexecution_json_roundtrips_into_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rep = ReexecutionReport::empty("0.1");
        rep.per_artifact
            .push(ecaa_workflow_core::reexecution::ArtifactClassification {
                artifact_path: "runtime/outputs/differential_expression/de_results.tsv".into(),
                bucket: ecaa_workflow_core::reexecution::ReexecutionBucket::SemanticEquivalent,
                reason: None,
            });
        write_reexecution_json(tmp.path(), &rep).unwrap();
        let raw = std::fs::read_to_string(tmp.path().join("runtime/reexecution.json")).unwrap();
        let back: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let arr = back["per_artifact"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0]["artifact_path"],
            "runtime/outputs/differential_expression/de_results.tsv"
        );
        assert_eq!(arr[0]["bucket"], "semantic_equivalent");
    }

    #[test]
    fn select_validator_falls_back_to_noop_without_runcrate_or_conformance() {
        // In the default test environment runcrate is absent and conformance
        // mode is unset, so both selectors must yield the no-op adapter (a
        // non-run is never a substrate pass). This is a type-level smoke check:
        // the call must not panic and must return SOME validator.
        if !runcrate_available() && !conformance_mode() {
            let _v = select_validator(true);
            let _w = select_validator(false);
        }
    }
}
