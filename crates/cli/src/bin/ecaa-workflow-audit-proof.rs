//! Standalone re-verifier for emitted packages.
//! Usage: ecaa-workflow-audit-proof <package-root> [--strict] [--secret <HEX>] [-o <PATH>]
//!
//! Exit codes: 0 = all Pass/Warn/Unverified; 1 = at least one Fail
//! (only when --strict). Without --strict always exits 0 (warn-only).
//!
//! `--secret <HEX>` (or env `ECAA_AUDIT_SECRET`) supplies the 64-hex-char
//! per-session HMAC secret persisted in `session.audit_writer_secret`.
//! When present, the signed verdict sink at
//! `runtime/verification-reports/claim-verification.signed.json` is read and
//! HMAC-verified, de-vacuifying Invariants 1/4/5 (a reviewer can then
//! demonstrate the non-vacuous result on an executed package). Absent the
//! secret, the legacy stub-only path is used.
//!
//! `-o, --output <PATH>` writes the report JSON to that file (pretty,
//! deterministic serde key order). The report is also always printed to
//! stdout for backward compatibility.
//!
//! WRROC validator selection: when `ECAA_CONFORMANCE_MODE` is truthy the
//! runcrate-backed `PythonRuncrateWrrocValidator` (harness) is injected so
//! Invariant 6 (substrate-validity) reflects a real conformance check;
//! otherwise the `NoopWrrocValidator` is used (Invariant 6 → Unverified).

use ecaa_workflow_core::audit_proof::{
    run_audit_proof, run_audit_proof_with_verifier, AuditProofReport, InvariantStatus,
};
use ecaa_workflow_core::audit_writer::AuditWriter;
use ecaa_workflow_core::clock::WallClock;
use ecaa_workflow_core::wrroc_validator::{NoopWrrocValidator, WrrocValidator};
use std::path::{Path, PathBuf};

/// Truthy parse of `ECAA_CONFORMANCE_MODE` (matches the conformance-mode
/// switch used by the emit-time validator).
fn conformance_mode() -> bool {
    matches!(
        std::env::var("ECAA_CONFORMANCE_MODE")
            .as_deref()
            .unwrap_or("0"),
        "1" | "true" | "yes" | "on"
    )
}

/// Decode the 64-hex-char per-session secret into a 32-byte key and build an
/// [`AuditWriter`] that can verify the signed verdict sink.
fn writer_from_hex(secret_hex: &str) -> anyhow::Result<AuditWriter> {
    let bytes = hex::decode(secret_hex.trim())
        .map_err(|e| anyhow::anyhow!("--secret is not valid hex: {e}"))?;
    let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!(
            "--secret must decode to exactly 32 bytes (64 hex chars), got {} byte(s)",
            bytes.len()
        )
    })?;
    Ok(AuditWriter::with_secret(key))
}

/// Build the audit-proof report for `root`. When `secret` is `Some`, the
/// signed verdict sink is read + HMAC-verified (de-vacuifying Inv 1/4/5);
/// when `output` is `Some`, the report JSON is written to that path. The
/// report is returned so `main` (and tests) can act on the verdicts.
///
/// Kept free of process exit / stdout so it is unit-testable in-process.
fn run_report(
    root: &Path,
    secret: Option<&str>,
    output: Option<&Path>,
) -> anyhow::Result<AuditProofReport> {
    let validator: Box<dyn WrrocValidator> = if conformance_mode() {
        Box::new(ecaa_workflow_harness::wrroc_validator_impl::PythonRuncrateWrrocValidator)
    } else {
        Box::new(NoopWrrocValidator)
    };
    let report = match secret {
        Some(hex) => {
            let writer = writer_from_hex(hex)?;
            run_audit_proof_with_verifier(root, validator.as_ref(), &WallClock, Some(&writer))?
        }
        None => run_audit_proof(root, validator.as_ref(), &WallClock)?,
    };
    if let Some(path) = output {
        let json = serde_json::to_string_pretty(&report)?;
        std::fs::write(path, format!("{json}\n"))
            .map_err(|e| anyhow::anyhow!("write report to {}: {e}", path.display()))?;
    }
    Ok(report)
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut root: Option<PathBuf> = None;
    let mut strict = false;
    let mut secret: Option<String> = None;
    let mut output: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--strict" => strict = true,
            "--secret" => {
                secret =
                    Some(args.next().ok_or_else(|| {
                        anyhow::anyhow!("--secret requires a hex value argument")
                    })?);
            }
            "-o" | "--output" => {
                output = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("-o/--output requires a path argument"))?
                        .into(),
                );
            }
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            other if other.starts_with('-') => {
                anyhow::bail!("unknown flag: {other}");
            }
            positional => {
                if root.is_some() {
                    anyhow::bail!("unexpected extra argument: {positional}");
                }
                root = Some(positional.into());
            }
        }
    }

    let root = root.ok_or_else(|| {
        anyhow::anyhow!(
            "usage: ecaa-workflow-audit-proof <root> [--strict] [--secret <HEX>] [-o <PATH>]"
        )
    })?;

    // --secret falls back to ECAA_AUDIT_SECRET when not given on the CLI.
    let secret = secret.or_else(|| std::env::var("ECAA_AUDIT_SECRET").ok());

    let report = run_report(&root, secret.as_deref(), output.as_deref())?;
    let json = serde_json::to_string_pretty(&report)?;
    println!("{json}");

    if strict
        && report
            .verdicts
            .iter()
            .any(|v| v.status == InvariantStatus::Fail)
    {
        std::process::exit(1);
    }
    Ok(())
}

fn print_help() {
    println!(
        "ecaa-workflow-audit-proof — re-verify a package's six audit-proof invariants\n\
         \n\
         USAGE:\n    \
         ecaa-workflow-audit-proof <root> [--strict] [--secret <HEX>] [-o <PATH>]\n\
         \n\
         ARGS:\n    \
         <root>    Package root directory to audit\n\
         \n\
         OPTIONS:\n    \
         --strict              Exit 1 if any invariant is Fail (default: warn-only, exit 0)\n    \
         --secret <HEX>        64-hex-char per-session HMAC secret; reads + verifies the\n                          \
         signed verdict sink (env: ECAA_AUDIT_SECRET)\n    \
         -o, --output <PATH>   Write the report JSON to PATH (also printed to stdout)\n    \
         -h, --help            Print this help"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::claim_contract::ClaimContract;
    use ecaa_workflow_core::claim_extractor::Claim;
    use ecaa_workflow_core::claim_sink::persist_signed_verdicts;
    use ecaa_workflow_core::claim_verifier::{
        ClaimStatus, ClaimStrength, ClaimVerdict, ClaimVerificationReport,
    };

    /// Stage a temp package carrying a signed verdict sink written with a
    /// known secret. Mirrors `crates/core/tests/provenance/audit_proof_with_verifier.rs`.
    /// Returns (tempdir, hex secret).
    fn staged_package_with_signed_sink() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let writer = AuditWriter::for_session();
        let secret_hex = hex::encode(writer.secret());
        let claim = Claim {
            entity: "TP53".into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: Some("results/tables/de.csv".into()),
            excerpt: String::new(),
            contract: ClaimContract::NumericTableLookup,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let rep = ClaimVerificationReport {
            n_checked: 1,
            n_verified: 1,
            n_mismatch: 0,
            n_unverifiable: 0,
            n_pending: 0,
            n_suspicious: 0,
            verdicts: vec![ClaimVerdict {
                claim,
                status: ClaimStatus::Verified,
                strength: ClaimStrength::default(),
                audit: None,
            }],
            runtime_decision_log_path: None,
        };
        persist_signed_verdicts(dir.path(), "diff_expr", &rep, None, &writer).unwrap();
        (dir, secret_hex)
    }

    #[test]
    fn run_report_with_secret_reads_signed_sink_non_vacuous() {
        let (dir, secret_hex) = staged_package_with_signed_sink();
        let report = run_report(dir.path(), Some(&secret_hex), None).unwrap();
        let cc = report
            .verdicts
            .iter()
            .find(|v| v.id == ecaa_workflow_core::audit_proof::InvariantId::ClaimCompleteness)
            .unwrap();
        assert!(
            cc.n_inspected > 0,
            "claim_completeness must inspect the signed verdict sink (got n_inspected={}, status={:?})",
            cc.n_inspected,
            cc.status
        );
        assert_eq!(cc.status, InvariantStatus::Pass);
    }

    #[test]
    fn run_report_without_secret_is_vacuous_stub() {
        // No verifier ⇒ signed sink ignored ⇒ claim_completeness is Unverified
        // over an empty set (the legacy stub path; backward compatible).
        let (dir, _secret_hex) = staged_package_with_signed_sink();
        let report = run_report(dir.path(), None, None).unwrap();
        let cc = report
            .verdicts
            .iter()
            .find(|v| v.id == ecaa_workflow_core::audit_proof::InvariantId::ClaimCompleteness)
            .unwrap();
        assert_eq!(cc.n_inspected, 0);
        assert_eq!(cc.status, InvariantStatus::Unverified);
    }

    #[test]
    fn run_report_writes_output_file_that_round_trips() {
        let (dir, secret_hex) = staged_package_with_signed_sink();
        let out = dir.path().join("audit-report.json");
        let report = run_report(dir.path(), Some(&secret_hex), Some(&out)).unwrap();
        assert!(out.exists(), "output file not written");
        let on_disk: AuditProofReport =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        // The file parses back to the same logical report.
        assert_eq!(
            serde_json::to_value(&on_disk).unwrap(),
            serde_json::to_value(&report).unwrap()
        );
    }

    #[test]
    fn writer_from_hex_rejects_wrong_length() {
        assert!(writer_from_hex("deadbeef").is_err());
        assert!(writer_from_hex("zz").is_err());
        // 64 hex chars = 32 bytes is accepted.
        assert!(writer_from_hex(&"ab".repeat(32)).is_ok());
    }
}
