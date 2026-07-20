//! Deposit-readiness attestation + gate — the three verification layers that
//! make RO-Crate validation and re-execution verification happen automatically
//! for every exported package instead of relying on an operator to remember a
//! separate `replay`.
//!
//! * **Layer 1 (always-on, blocking).** `export` self-validates the deposit it
//!   just sealed — recorded-verdict re-verify (RO-Crate / audit-proof /
//!   claim-verification) + BagIt manifest checksum integrity — and refuses to
//!   emit a deposit that cannot validate itself. Cheap + deterministic; the
//!   same defense-in-depth pattern as `validate_container_digests_pinned` at the
//!   top of `emit_package`.
//! * **Layer 2 (profile-gated, attested).** A `re-executable` deposit — whose
//!   entire contract is replayability — additionally has its re-execution
//!   verdict stamped into the attestation (driven by the CLI export handler,
//!   which owns the container-running orchestration). `not_verified` is recorded
//!   honestly when the check is skipped.
//! * **Layer 3 (downstream gate).** [`check_deposit_readiness`] reads the
//!   attestation and refuses a package that never self-validated or whose
//!   validation failed — the enforcement point wired into the `deposit-check`
//!   CLI subcommand + `make deposit-check`, run before a deposit is trusted.
//!
//! Layer 1 also rolls up per-task domain-correctness validation
//! (`domain_validation`, via [`scan_domain_validation`] — RCA I-10): a
//! `validate_*` companion task's own `result.json` may self-report
//! `validation_passed: false`, which is a SEPARATE axis from computational
//! completion (whether the stage ran and produced output at all). The
//! headline `deposit_ready` bool folds `ro_crate` + `bagit` +
//! `domain_validation` + `reexecution` so a run can be computationally
//! complete while `deposit_ready` reads `false`.
//!
//! The attestation is written to `DEPOSIT-READINESS.json` at the deposit root
//! and is intentionally OFF the BagIt manifest (it carries a wall-clock
//! `verified_at` + a verdict computed at export time), mirroring
//! `audit-proof-report.json`.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::clock::Clock;
use crate::replay::report::{ReplayVerdict, ReverifyResult};
use crate::replay::reverify::reverify;

/// Root-level attestation filename. Manifest-EXCLUDED (see `emitter::bagit`).
pub const DEPOSIT_READINESS_FILE: &str = "DEPOSIT-READINESS.json";

/// Pass/fail outcome of a deterministic self-validation check.
///
/// `Pass` is the derived default so an attestation written before a new
/// `CheckStatus`-typed field existed (e.g. `domain_validation`, RCA I-10)
/// deserializes as "nothing recorded, nothing failed" rather than erroring —
/// absence never fabricates a failure.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    #[default]
    Pass,
    Fail,
}

/// Re-execution verdict recorded in the attestation. `NotVerified` = the check
/// was not run (a non-`re-executable` profile, or an explicit `--no-reexec-check`
/// opt-out); it is recorded honestly rather than silently omitted so the
/// downstream gate can distinguish "not checked" from "checked and reproduced".
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReexecStatus {
    Pass,
    Partial,
    NotVerified,
    Fail,
}

/// The one deposit profile whose entire contract is replayability. A package
/// emitted under this profile that was never re-executed (`NotVerified`) must
/// not read as `deposit_ready` — it is claiming a property it never
/// demonstrated. Other profiles (`full`, `minimal`) do not claim
/// re-executability, so a `NotVerified` re-execution is admitted for them
/// (the honest offline outcome is recorded, not blocked).
pub const REEXECUTABLE_PROFILE: &str = "re-executable";

/// `true` when `profile` claims a re-executability contract, so an
/// un-run (`NotVerified`) re-execution must block `deposit_ready`.
pub fn profile_claims_reexecutability(profile: &str) -> bool {
    profile == REEXECUTABLE_PROFILE
}

/// Map a replay verdict onto the attestation's re-execution status.
pub fn reexec_status_from_verdict(v: &ReplayVerdict) -> ReexecStatus {
    match v {
        ReplayVerdict::Pass => ReexecStatus::Pass,
        ReplayVerdict::Partial => ReexecStatus::Partial,
        ReplayVerdict::Fail => ReexecStatus::Fail,
    }
}

/// The `DEPOSIT-READINESS.json` payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositReadiness {
    pub schema_version: String,
    /// Deposit profile the attestation was produced for (`full` /
    /// `re-executable` / `minimal`).
    pub profile: String,
    /// Headline signal: `true` iff `ro_crate`, `bagit`, and
    /// `domain_validation` all `Pass` AND `reexecution != Fail` (RCA I-10).
    /// Computed once at attestation-write time by [`compute_deposit_ready`]
    /// and re-derived whenever a component field changes (e.g. the Layer-2
    /// re-execution update). Deliberately SEPARATE from computational
    /// completion — a package whose tasks all ran to completion can still
    /// have `deposit_ready = false` when a required per-task domain check
    /// failed. `#[serde(default)]` so an attestation written before this
    /// field existed deserializes as `false` (conservative: an unscanned
    /// legacy attestation is not claimed ready) rather than erroring.
    #[serde(default)]
    pub deposit_ready: bool,
    /// RO-Crate / recorded-verdict self-validation outcome.
    pub ro_crate: CheckStatus,
    /// BagIt manifest checksum-integrity outcome.
    pub bagit: CheckStatus,
    /// Per-task domain-correctness validation rollup (RCA I-10): `Fail` when
    /// any `validate_*` task's own `result.json` recorded
    /// `validation_passed: false`. See [`scan_domain_validation`]. Separate
    /// from `ro_crate`/`bagit` (structural integrity) and from computational
    /// completion (whether tasks ran at all) — this is the domain-science
    /// signal. `#[serde(default)]` for attestations predating this field.
    #[serde(default)]
    pub domain_validation: CheckStatus,
    /// Re-execution verdict (`not_verified` when the check was not run).
    pub reexecution: ReexecStatus,
    /// Human-readable failure/notes detail (empty on a clean all-pass).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Recorded execution-container image the deposit replays against, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    /// RFC-3339 wall-clock instant the attestation was produced.
    pub verified_at: String,
}

/// Fold the deposit-readiness component signals into the headline
/// `deposit_ready` bool (RCA I-10, DR-1). Mirrors the non-strict branch of
/// [`check_deposit_readiness`] and, like it, is now PROFILE-AWARE: a hard
/// `Fail` on any of the three `CheckStatus` axes — or a `Fail` re-execution
/// — always blocks; and for the `re-executable` profile a `NotVerified`
/// re-execution ALSO blocks (a deposit whose entire contract is replayability
/// must actually have been re-executed). `Partial` — the honest offline
/// outcome — always passes; and `NotVerified` stays admitted for profiles
/// that do not claim re-executability (`full`/`minimal`), where it is a
/// `--strict`-only concern owned by the CLI gate.
pub fn compute_deposit_ready(
    profile: &str,
    ro_crate: CheckStatus,
    bagit: CheckStatus,
    domain_validation: CheckStatus,
    reexecution: ReexecStatus,
) -> bool {
    let reexec_blocks = reexecution == ReexecStatus::Fail
        || (profile_claims_reexecutability(profile)
            && reexecution == ReexecStatus::NotVerified);
    ro_crate == CheckStatus::Pass
        && bagit == CheckStatus::Pass
        && domain_validation == CheckStatus::Pass
        && !reexec_blocks
}

/// One RO-Crate embedded content hash (written by
/// [`crate::ro_crate::register_content_integrity`]) that disagrees with the
/// sealed payload's actual bytes (RCA I-2): the descriptor claims `recorded`
/// for `path`, but the file currently on disk hashes to `actual`.
#[derive(Debug, Clone)]
pub struct RoCrateHashMismatch {
    pub path: String,
    pub recorded: String,
    pub actual: String,
}

/// Post-seal integrity recheck (RCA I-2): recompute the SHA-512 of every
/// payload file the RO-Crate `@graph` declares a content hash for
/// (`ecaa_workflow_core::ro_crate::recorded_content_hashes`) and compare
/// against the value actually recorded in `ro-crate-metadata.json`.
///
/// A non-empty result means the descriptor was sealed (or last had its
/// content-integrity annotations refreshed) BEFORE a later mutation to that
/// file — the finalization-order failure this check exists to catch. A
/// package with no embedded content hashes yet (a fresh, pre-execution
/// emit, which never calls `register_content_integrity`) returns an empty
/// `Vec` — there is nothing to recheck, not a failure.
pub fn recheck_ro_crate_content_hashes(package_root: &Path) -> Result<Vec<RoCrateHashMismatch>> {
    let recorded = crate::ro_crate::recorded_content_hashes(package_root);
    if recorded.is_empty() {
        return Ok(Vec::new());
    }
    let fresh = crate::emitter::bagit::payload_hashes(
        package_root,
        crate::emitter::bagit::SealMode::Reseal,
    )
    .context("recomputing payload hashes for the post-seal RO-Crate recheck")?;
    let mut mismatches = Vec::new();
    for (path, recorded_hex) in recorded {
        let actual_hex = fresh
            .get(&path)
            .map(|(hex, _)| hex.clone())
            .unwrap_or_else(|| "<absent from sealed payload>".to_string());
        if actual_hex != recorded_hex {
            mismatches.push(RoCrateHashMismatch {
                path,
                recorded: recorded_hex,
                actual: actual_hex,
            });
        }
    }
    Ok(mismatches)
}

/// Bail with a detailed message if [`recheck_ro_crate_content_hashes`] finds
/// any mismatch. The hard post-seal gate: a sealed/resealed package must
/// never claim a content hash the sealed payload does not actually carry.
pub fn assert_ro_crate_hashes_match_payload(package_root: &Path) -> Result<()> {
    let mismatches = recheck_ro_crate_content_hashes(package_root)?;
    if mismatches.is_empty() {
        return Ok(());
    }
    let detail = mismatches
        .iter()
        .map(|m| {
            let actual_short = &m.actual[..12.min(m.actual.len())];
            format!(
                "{} (recorded {}…, actual {}…)",
                m.path,
                &m.recorded[..12],
                actual_short
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    bail!(
        "post-seal RO-Crate content-hash recheck failed ({} mismatch(es)): {detail}",
        mismatches.len()
    );
}

/// Aggregated per-task domain-correctness validation signal (RCA I-10).
///
/// A `validate_<stage>` companion task may record its own domain-correctness
/// self-report in `runtime/outputs/validate_<stage>/result.json`
/// (`validation_passed: bool`, and on failure `required_failures: [String]`
/// naming the failed `policies/validation-contract.json` assertion ids) —
/// this is a DIFFERENT signal than whether the stage ran at all
/// (computational completion): the deposited `611cf5ee` package had
/// `validate_differential_expression/result.json` recording
/// `validation_passed: false` while every top-level summary layer
/// (`DEPOSIT-READINESS.json`, RO-Crate, BagIt) read as passing, because
/// nothing rolled the per-task self-report up into the deposit-level
/// attestation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DomainValidationSummary {
    /// `validate_*` task ids that recorded a self-report (a `validation_passed`
    /// key was present), in sorted order.
    pub checked_tasks: Vec<String>,
    /// The subset of `checked_tasks` whose self-report was
    /// `validation_passed: false`, in sorted order.
    pub failed_tasks: Vec<String>,
    /// `"<task_id>: <assertion_id>"` for every entry in a failed task's
    /// `required_failures` array, in scan order.
    pub required_failures: Vec<String>,
}

impl DomainValidationSummary {
    /// `true` iff no `validate_*` task self-reported a domain-correctness
    /// failure. Vacuously `true` when no task recorded a self-report at all
    /// — absence of a check is not itself a failure.
    pub fn passed(&self) -> bool {
        self.failed_tasks.is_empty()
    }
}

/// Scan every `validate_*` task's `runtime/outputs/<task_id>/result.json`
/// under `package_root` for a self-reported `validation_passed` boolean and
/// roll the per-task verdicts into one [`DomainValidationSummary`] (RCA
/// I-10). A `result.json` that is missing, unparseable, or carries no
/// `validation_passed` key is silently skipped — not every `validate_*`
/// companion emits a domain-correctness self-report (most are pure
/// artifact-presence checks), and absence must never read as a failure.
/// Deterministic: task directories are visited in sorted order.
pub fn scan_domain_validation(package_root: &Path) -> DomainValidationSummary {
    let mut summary = DomainValidationSummary::default();
    let outputs_dir = package_root.join("runtime").join("outputs");
    let Ok(entries) = std::fs::read_dir(&outputs_dir) else {
        return summary;
    };
    let mut task_ids: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.starts_with("validate_"))
        .collect();
    task_ids.sort();
    for task_id in task_ids {
        let result_path = outputs_dir.join(&task_id).join("result.json");
        let Ok(raw) = std::fs::read_to_string(&result_path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let Some(passed) = v.get("validation_passed").and_then(|x| x.as_bool()) else {
            continue;
        };
        summary.checked_tasks.push(task_id.clone());
        if !passed {
            summary.failed_tasks.push(task_id.clone());
            if let Some(arr) = v.get("required_failures").and_then(|x| x.as_array()) {
                for f in arr {
                    if let Some(s) = f.as_str() {
                        summary.required_failures.push(format!("{task_id}: {s}"));
                    }
                }
            }
        }
    }
    summary
}

/// Result of the Layer-1 deterministic self-validation over a sealed deposit.
pub struct Tier1Validation {
    pub ro_crate: CheckStatus,
    pub bagit: CheckStatus,
    #[allow(dead_code)]
    pub reverify: ReverifyResult,
    /// Failure explanation (`None` on a clean pass).
    pub detail: Option<String>,
}

impl Tier1Validation {
    /// `true` iff both deterministic checks passed.
    pub fn passed(&self) -> bool {
        self.ro_crate == CheckStatus::Pass && self.bagit == CheckStatus::Pass
    }
}

/// Layer 1: re-verify recorded verdicts against a fresh recomputation and check
/// the BagIt manifest checksums over the freshly-sealed deposit.
///
/// * `ro_crate` = `Pass` unless EITHER the re-verify saw a genuine divergence
///   (a recorded verdict that a fresh recomputation contradicts) while the
///   reader version matches the writer — the same "real tamper vs version
///   drift" distinction `replay`'s verdict uses; on a fresh self-export
///   `reader == writer`, so any divergence is real and fails the check — OR
///   the post-seal recheck (RCA I-2) finds an embedded content hash that
///   disagrees with the sealed payload.
/// * `bagit` = `Pass` iff every file listed in `manifest-sha512.txt` is present
///   and its SHA-512 matches.
pub fn validate_deposit_tier1(dst: &Path, reader_version: &str) -> Result<Tier1Validation> {
    let rv = reverify(dst, reader_version).context("re-verifying deposit for readiness")?;

    let diverged: Vec<&str> = rv
        .checks
        .iter()
        .filter(|c| c.diverged)
        .map(|c| c.check.as_str())
        .collect();
    // A divergence is a real integrity failure only when the reader version
    // matches the writer; under a version mismatch it is drift, not tampering.
    let recorded_verdict_diverged = !diverged.is_empty() && rv.reader_matches_writer;

    let hash_mismatches = recheck_ro_crate_content_hashes(dst)
        .context("post-seal RO-Crate content-hash recheck for readiness")?;

    let ro_crate = if recorded_verdict_diverged || !hash_mismatches.is_empty() {
        CheckStatus::Fail
    } else {
        CheckStatus::Pass
    };

    let bagit_ok = crate::emitter::bagit::verify_manifest(dst)
        .context("verifying BagIt manifest for readiness")?;
    let bagit = if bagit_ok { CheckStatus::Pass } else { CheckStatus::Fail };

    let mut notes: Vec<String> = Vec::new();
    if recorded_verdict_diverged {
        notes.push(format!(
            "recorded-verdict divergence on: {}",
            diverged.join(", ")
        ));
    }
    if !hash_mismatches.is_empty() {
        let paths: Vec<&str> = hash_mismatches.iter().map(|m| m.path.as_str()).collect();
        notes.push(format!(
            "RO-Crate content-hash mismatch on: {}",
            paths.join(", ")
        ));
    }
    if bagit == CheckStatus::Fail {
        notes.push("BagIt manifest checksum mismatch or missing manifested file".to_string());
    }
    let detail = (!notes.is_empty()).then(|| notes.join("; "));

    Ok(Tier1Validation {
        ro_crate,
        bagit,
        reverify: rv,
        detail,
    })
}

/// Write `DEPOSIT-READINESS.json` into the deposit root, folding the Layer-1
/// validation + the per-task domain-validation rollup (RCA I-10, via
/// [`scan_domain_validation`] over `dst`) + the (possibly `NotVerified`)
/// re-execution status into one attestation. `reexec_detail` augments the
/// Layer-1 `detail`.
pub fn write_deposit_readiness(
    dst: &Path,
    profile: &str,
    tier1: &Tier1Validation,
    reexecution: ReexecStatus,
    reexec_detail: Option<String>,
    image_digest: Option<String>,
    clock: &dyn Clock,
) -> Result<()> {
    let domain = scan_domain_validation(dst);
    let domain_validation = if domain.passed() {
        CheckStatus::Pass
    } else {
        CheckStatus::Fail
    };
    let domain_detail = (!domain.required_failures.is_empty()).then(|| {
        format!(
            "domain-validation failure(s): {}",
            domain.required_failures.join(", ")
        )
    });

    let detail = [tier1.detail.clone(), domain_detail, reexec_detail]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let detail = (!detail.is_empty()).then(|| detail.join("; "));

    let deposit_ready = compute_deposit_ready(
        profile,
        tier1.ro_crate,
        tier1.bagit,
        domain_validation,
        reexecution,
    );

    let att = DepositReadiness {
        schema_version: "0.1".to_string(),
        profile: profile.to_string(),
        deposit_ready,
        ro_crate: tier1.ro_crate,
        bagit: tier1.bagit,
        domain_validation,
        reexecution,
        detail,
        image_digest,
        verified_at: clock.now_rfc3339(),
    };
    let body = serde_json::to_vec_pretty(&att).context("serializing DEPOSIT-READINESS.json")?;
    let path = dst.join(DEPOSIT_READINESS_FILE);
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Update only the re-execution fields of an existing attestation (Layer 2,
/// called by the CLI export handler after it runs the re-execution check). Reads
/// the attestation Layer 1 wrote, overwrites `reexecution` + folds in the detail,
/// and rewrites. Bails if no attestation is present (Layer 1 must have run).
pub fn update_deposit_readiness_reexecution(
    dst: &Path,
    reexecution: ReexecStatus,
    reexec_detail: Option<String>,
    image_digest: Option<String>,
    clock: &dyn Clock,
) -> Result<()> {
    let mut att = read_deposit_readiness(dst)?.ok_or_else(|| {
        anyhow::anyhow!(
            "no {DEPOSIT_READINESS_FILE} to update — Layer-1 self-validation must run first"
        )
    })?;
    att.reexecution = reexecution;
    if let Some(d) = reexec_detail {
        att.detail = Some(match att.detail.take() {
            Some(existing) => format!("{existing}; {d}"),
            None => d,
        });
    }
    if image_digest.is_some() {
        att.image_digest = image_digest;
    }
    // Re-derive the headline signal: ro_crate/bagit/domain_validation are
    // unchanged by a Layer-2 re-execution update, but the new `reexecution`
    // value can flip `deposit_ready` (e.g. a fresh `Fail`, or — for the
    // `re-executable` profile — a re-execution that never ran).
    att.deposit_ready = compute_deposit_ready(
        &att.profile,
        att.ro_crate,
        att.bagit,
        att.domain_validation,
        att.reexecution,
    );
    att.verified_at = clock.now_rfc3339();
    let body = serde_json::to_vec_pretty(&att).context("serializing DEPOSIT-READINESS.json")?;
    let path = dst.join(DEPOSIT_READINESS_FILE);
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Read the attestation, returning `None` when absent.
pub fn read_deposit_readiness(pkg: &Path) -> Result<Option<DepositReadiness>> {
    let path = pkg.join(DEPOSIT_READINESS_FILE);
    match std::fs::read_to_string(&path) {
        Ok(raw) => Ok(Some(
            serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Layer 3: the downstream deposit gate. Refuses a package that was not produced
/// by a self-validating export (no attestation), or whose RO-Crate / BagIt
/// self-validation failed, or whose re-execution FAILED. A `NotVerified`
/// re-execution is a hard block (DR-1) when EITHER `strict` is set OR the
/// attestation's `profile` claims re-executability (`re-executable`) — a
/// deposit marketing replayability that was never re-executed must not pass;
/// for other profiles (`full`/`minimal`) a `NotVerified` is admitted outside
/// `--strict` (the caller should surface it as a warning). `Partial` (the
/// expected clean outcome for a package whose analytical tables reproduce
/// while its network-dependent stages cannot run offline) always passes.
pub fn check_deposit_readiness(pkg: &Path, strict: bool) -> Result<DepositReadiness> {
    let dr = read_deposit_readiness(pkg)?.ok_or_else(|| {
        anyhow::anyhow!(
            "no {DEPOSIT_READINESS_FILE}: package was not produced by a self-validating export; \
             refusing to treat it as deposit-grade (re-export it, or run `ecaa-workflow export`)"
        )
    })?;
    if dr.ro_crate != CheckStatus::Pass {
        bail!(
            "deposit gate: RO-Crate self-validation did not pass ({:?}){}",
            dr.ro_crate,
            dr.detail.as_deref().map(|d| format!(" — {d}")).unwrap_or_default()
        );
    }
    if dr.bagit != CheckStatus::Pass {
        bail!(
            "deposit gate: BagIt integrity did not pass ({:?}){}",
            dr.bagit,
            dr.detail.as_deref().map(|d| format!(" — {d}")).unwrap_or_default()
        );
    }
    if dr.domain_validation != CheckStatus::Pass {
        bail!(
            "deposit gate: per-task domain-correctness validation did not pass ({:?}){} — \
             a required validate_* check failed even though the run may be computationally \
             complete; remediate and re-export",
            dr.domain_validation,
            dr.detail.as_deref().map(|d| format!(" — {d}")).unwrap_or_default()
        );
    }
    match dr.reexecution {
        ReexecStatus::Fail => bail!(
            "deposit gate: re-execution verification FAILED{}",
            dr.detail.as_deref().map(|d| format!(" — {d}")).unwrap_or_default()
        ),
        // DR-1: a `re-executable`-profile deposit that was never re-executed
        // is claiming replayability it never demonstrated — refuse it
        // regardless of `--strict`.
        ReexecStatus::NotVerified if profile_claims_reexecutability(&dr.profile) => bail!(
            "deposit gate: re-execution NOT verified for the {REEXECUTABLE_PROFILE} profile \
             — a package claiming re-executability must actually have been re-executed \
             (run `ecaa-workflow replay <dir> --tier execute`, re-export without \
             --no-reexec-check, or downgrade the deposit profile)"
        ),
        ReexecStatus::NotVerified if strict => bail!(
            "deposit gate: re-execution NOT verified and --strict was given \
             (run `ecaa-workflow replay <dir> --tier execute` or re-export without --no-reexec-check)"
        ),
        _ => {}
    }
    Ok(dr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::WallClock;
    use crate::replay::report::VerifierDiff;
    use std::fs;

    fn sha512_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha512};
        let mut h = Sha512::new();
        h.update(bytes);
        format!("{:x}", h.finalize())
    }

    fn tier1(ro: CheckStatus, bagit: CheckStatus, detail: Option<&str>) -> Tier1Validation {
        Tier1Validation {
            ro_crate: ro,
            bagit,
            reverify: ReverifyResult {
                checks: Vec::new(),
                reader_matches_writer: true,
            },
            detail: detail.map(str::to_string),
        }
    }

    #[test]
    fn attestation_write_read_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        write_deposit_readiness(
            tmp.path(),
            "re-executable",
            &tier1(CheckStatus::Pass, CheckStatus::Pass, None),
            ReexecStatus::Partial,
            Some("6 byte_identical, 15 unavailable".into()),
            Some("bio-min:local".into()),
            &WallClock,
        )
        .unwrap();
        let dr = read_deposit_readiness(tmp.path()).unwrap().unwrap();
        assert_eq!(dr.ro_crate, CheckStatus::Pass);
        assert_eq!(dr.bagit, CheckStatus::Pass);
        assert_eq!(dr.reexecution, ReexecStatus::Partial);
        assert_eq!(dr.profile, "re-executable");
        assert!(dr.detail.as_deref().unwrap().contains("byte_identical"));
        assert!(!dr.verified_at.is_empty());
    }

    #[test]
    fn verify_manifest_true_on_match_false_on_tamper_or_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.txt"), b"hello").unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/b.txt"), b"world").unwrap();
        let manifest = format!(
            "{}  a.txt\n{}  sub/b.txt\n",
            sha512_hex(b"hello"),
            sha512_hex(b"world")
        );
        fs::write(root.join("manifest-sha512.txt"), &manifest).unwrap();
        assert!(crate::emitter::bagit::verify_manifest(root).unwrap());

        // Tamper a payload file → checksum mismatch → invalid.
        fs::write(root.join("a.txt"), b"HELLO").unwrap();
        assert!(!crate::emitter::bagit::verify_manifest(root).unwrap());

        // Manifested file missing → invalid.
        fs::write(root.join("a.txt"), b"hello").unwrap();
        fs::remove_file(root.join("sub/b.txt")).unwrap();
        assert!(!crate::emitter::bagit::verify_manifest(root).unwrap());
    }

    #[test]
    fn gate_blocks_missing_attestation() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(check_deposit_readiness(tmp.path(), false).is_err());
    }

    #[test]
    fn gate_blocks_failed_checks_and_reexec_fail() {
        let tmp = tempfile::tempdir().unwrap();
        // ro_crate fail
        write_deposit_readiness(
            tmp.path(),
            "full",
            &tier1(CheckStatus::Fail, CheckStatus::Pass, Some("divergence")),
            ReexecStatus::NotVerified,
            None,
            None,
            &WallClock,
        )
        .unwrap();
        assert!(check_deposit_readiness(tmp.path(), false).is_err());

        // bagit fail
        write_deposit_readiness(
            tmp.path(),
            "full",
            &tier1(CheckStatus::Pass, CheckStatus::Fail, Some("bad manifest")),
            ReexecStatus::NotVerified,
            None,
            None,
            &WallClock,
        )
        .unwrap();
        assert!(check_deposit_readiness(tmp.path(), false).is_err());

        // reexecution fail
        write_deposit_readiness(
            tmp.path(),
            "re-executable",
            &tier1(CheckStatus::Pass, CheckStatus::Pass, None),
            ReexecStatus::Fail,
            Some("de_results.tsv failed".into()),
            None,
            &WallClock,
        )
        .unwrap();
        assert!(check_deposit_readiness(tmp.path(), false).is_err());
    }

    #[test]
    fn gate_allows_pass_and_partial_nonstrict() {
        // Pass + Partial pass the non-strict gate for every profile, including
        // `re-executable`. (NotVerified under `re-executable` is covered by the
        // DR-1 tests below — it now blocks.)
        let tmp = tempfile::tempdir().unwrap();
        for reexec in [ReexecStatus::Pass, ReexecStatus::Partial] {
            write_deposit_readiness(
                tmp.path(),
                "re-executable",
                &tier1(CheckStatus::Pass, CheckStatus::Pass, None),
                reexec,
                None,
                None,
                &WallClock,
            )
            .unwrap();
            assert!(
                check_deposit_readiness(tmp.path(), false).is_ok(),
                "reexec={reexec:?} must pass the non-strict gate"
            );
        }
    }

    /// DR-1: a `NotVerified` re-execution is admitted non-strict for a profile
    /// that does NOT claim re-executability (`full`), but is a hard block for
    /// the `re-executable` profile even without `--strict`; and `--strict`
    /// blocks it for every profile.
    #[test]
    fn gate_notverified_profile_and_strict_matrix() {
        let tmp = tempfile::tempdir().unwrap();

        // full + NotVerified: allowed non-strict, blocked strict, and marked
        // deposit_ready.
        write_deposit_readiness(
            tmp.path(),
            "full",
            &tier1(CheckStatus::Pass, CheckStatus::Pass, None),
            ReexecStatus::NotVerified,
            None,
            None,
            &WallClock,
        )
        .unwrap();
        assert!(read_deposit_readiness(tmp.path()).unwrap().unwrap().deposit_ready);
        assert!(check_deposit_readiness(tmp.path(), false).is_ok());
        assert!(check_deposit_readiness(tmp.path(), true).is_err());

        // re-executable + NotVerified: blocked on BOTH gates (writer flips
        // deposit_ready false; reader refuses even non-strict).
        write_deposit_readiness(
            tmp.path(),
            "re-executable",
            &tier1(CheckStatus::Pass, CheckStatus::Pass, None),
            ReexecStatus::NotVerified,
            None,
            None,
            &WallClock,
        )
        .unwrap();
        assert!(
            !read_deposit_readiness(tmp.path()).unwrap().unwrap().deposit_ready,
            "re-executable + NotVerified must not read as deposit_ready"
        );
        let err = check_deposit_readiness(tmp.path(), false)
            .expect_err("re-executable + NotVerified must be refused even non-strict");
        assert!(format!("{err:#}").contains(REEXECUTABLE_PROFILE));
        assert!(check_deposit_readiness(tmp.path(), true).is_err());
    }

    #[test]
    fn update_reexecution_preserves_tier1_and_overwrites_reexec() {
        let tmp = tempfile::tempdir().unwrap();
        write_deposit_readiness(
            tmp.path(),
            "re-executable",
            &tier1(CheckStatus::Pass, CheckStatus::Pass, Some("tier1 note")),
            ReexecStatus::NotVerified,
            None,
            None,
            &WallClock,
        )
        .unwrap();
        update_deposit_readiness_reexecution(
            tmp.path(),
            ReexecStatus::Partial,
            Some("6 byte_identical".into()),
            Some("bio-min:local".into()),
            &WallClock,
        )
        .unwrap();
        let dr = read_deposit_readiness(tmp.path()).unwrap().unwrap();
        assert_eq!(dr.reexecution, ReexecStatus::Partial);
        assert_eq!(dr.ro_crate, CheckStatus::Pass);
        assert_eq!(dr.image_digest.as_deref(), Some("bio-min:local"));
        let detail = dr.detail.unwrap();
        assert!(detail.contains("tier1 note") && detail.contains("byte_identical"));
    }

    #[test]
    fn reexec_status_maps_from_verdict() {
        assert_eq!(reexec_status_from_verdict(&ReplayVerdict::Pass), ReexecStatus::Pass);
        assert_eq!(reexec_status_from_verdict(&ReplayVerdict::Partial), ReexecStatus::Partial);
        assert_eq!(reexec_status_from_verdict(&ReplayVerdict::Fail), ReexecStatus::Fail);
    }

    #[test]
    fn tier1_validation_passed_helper() {
        assert!(tier1(CheckStatus::Pass, CheckStatus::Pass, None).passed());
        assert!(!tier1(CheckStatus::Fail, CheckStatus::Pass, None).passed());
        assert!(!tier1(CheckStatus::Pass, CheckStatus::Fail, None).passed());
        // silence unused-field warning on the reverify diff type in this module
        let _ = VerifierDiff {
            check: "x".into(),
            recorded: serde_json::Value::Null,
            fresh: serde_json::Value::Null,
            diverged: false,
            note: None,
        };
    }

    fn write_validate_result(root: &Path, task_id: &str, body: serde_json::Value) {
        let dir = root.join("runtime/outputs").join(task_id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("result.json"), body.to_string()).unwrap();
    }

    #[test]
    fn scan_domain_validation_rolls_up_failures_and_skips_unreported() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Passing self-report.
        write_validate_result(
            root,
            "validate_qc",
            serde_json::json!({"validation_passed": true}),
        );
        // Failing self-report with named required-check failures.
        write_validate_result(
            root,
            "validate_differential_expression",
            serde_json::json!({
                "validation_passed": false,
                "checks_failed": 1,
                "required_failures": ["differential_expression.response_matches_stated_outcome"]
            }),
        );
        // A validate_* task with no self-report at all (e.g. a pure
        // artifact-presence validator) must be silently skipped, not treated
        // as a failure.
        write_validate_result(
            root,
            "validate_normalisation",
            serde_json::json!({"outcome": "ok"}),
        );
        // A non-`validate_*` output dir must never be scanned.
        write_validate_result(
            root,
            "differential_expression",
            serde_json::json!({"validation_passed": false}),
        );

        let summary = scan_domain_validation(root);
        assert_eq!(
            summary.checked_tasks,
            vec!["validate_differential_expression", "validate_qc"]
        );
        assert_eq!(summary.failed_tasks, vec!["validate_differential_expression"]);
        assert_eq!(
            summary.required_failures,
            vec!["validate_differential_expression: differential_expression.response_matches_stated_outcome"]
        );
        assert!(!summary.passed());
    }

    #[test]
    fn scan_domain_validation_empty_package_is_vacuously_passed() {
        let tmp = tempfile::tempdir().unwrap();
        let summary = scan_domain_validation(tmp.path());
        assert!(summary.checked_tasks.is_empty());
        assert!(summary.passed());
    }

    /// RCA I-10: a package whose RO-Crate/BagIt self-validation both pass
    /// (structurally sound — "computationally completed") must still read
    /// `deposit_ready: false` once a `validate_*` task self-reports a failed
    /// required domain check, and the Layer-3 gate must refuse it even
    /// without `--strict`.
    #[test]
    fn failed_domain_check_flips_deposit_not_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_validate_result(
            root,
            "validate_differential_expression",
            serde_json::json!({
                "validation_passed": false,
                "required_failures": ["differential_expression.response_matches_stated_outcome"]
            }),
        );
        write_deposit_readiness(
            root,
            "full",
            &tier1(CheckStatus::Pass, CheckStatus::Pass, None),
            ReexecStatus::Partial,
            None,
            None,
            &WallClock,
        )
        .unwrap();

        let dr = read_deposit_readiness(root).unwrap().unwrap();
        assert_eq!(dr.ro_crate, CheckStatus::Pass, "structurally sound");
        assert_eq!(dr.bagit, CheckStatus::Pass, "structurally sound");
        assert_eq!(dr.domain_validation, CheckStatus::Fail);
        assert!(
            !dr.deposit_ready,
            "a required domain-check failure must block deposit-readiness \
             even though the package is otherwise computationally complete"
        );
        assert!(dr
            .detail
            .as_deref()
            .unwrap()
            .contains("response_matches_stated_outcome"));

        let err = check_deposit_readiness(root, false)
            .expect_err("Layer-3 gate must refuse a failed domain check even non-strict");
        assert!(format!("{err:#}").contains("domain-correctness"));
    }

    #[test]
    fn clean_package_with_no_domain_reports_is_deposit_ready() {
        let tmp = tempfile::tempdir().unwrap();
        write_deposit_readiness(
            tmp.path(),
            "full",
            &tier1(CheckStatus::Pass, CheckStatus::Pass, None),
            ReexecStatus::Pass,
            None,
            None,
            &WallClock,
        )
        .unwrap();
        let dr = read_deposit_readiness(tmp.path()).unwrap().unwrap();
        assert_eq!(dr.domain_validation, CheckStatus::Pass);
        assert!(dr.deposit_ready);
        assert!(check_deposit_readiness(tmp.path(), false).is_ok());
    }

    #[test]
    fn compute_deposit_ready_matches_gate_semantics() {
        // A non-re-executable profile admits NotVerified.
        assert!(compute_deposit_ready(
            "full",
            CheckStatus::Pass,
            CheckStatus::Pass,
            CheckStatus::Pass,
            ReexecStatus::NotVerified
        ));
        assert!(!compute_deposit_ready(
            "full",
            CheckStatus::Pass,
            CheckStatus::Pass,
            CheckStatus::Fail,
            ReexecStatus::Pass
        ));
        assert!(!compute_deposit_ready(
            "full",
            CheckStatus::Pass,
            CheckStatus::Pass,
            CheckStatus::Pass,
            ReexecStatus::Fail
        ));
    }

    /// DR-1 at the writer: the `re-executable` profile blocks a `NotVerified`
    /// re-execution while `Partial`/`Pass` still pass; a non-re-executable
    /// profile admits `NotVerified`.
    #[test]
    fn compute_deposit_ready_profile_aware_reexec() {
        // re-executable: NotVerified blocks, Partial/Pass do not.
        assert!(!compute_deposit_ready(
            REEXECUTABLE_PROFILE,
            CheckStatus::Pass,
            CheckStatus::Pass,
            CheckStatus::Pass,
            ReexecStatus::NotVerified
        ));
        assert!(compute_deposit_ready(
            REEXECUTABLE_PROFILE,
            CheckStatus::Pass,
            CheckStatus::Pass,
            CheckStatus::Pass,
            ReexecStatus::Partial
        ));
        assert!(compute_deposit_ready(
            REEXECUTABLE_PROFILE,
            CheckStatus::Pass,
            CheckStatus::Pass,
            CheckStatus::Pass,
            ReexecStatus::Pass
        ));
        // Fail blocks under every profile.
        for profile in [REEXECUTABLE_PROFILE, "full", "minimal"] {
            assert!(!compute_deposit_ready(
                profile,
                CheckStatus::Pass,
                CheckStatus::Pass,
                CheckStatus::Pass,
                ReexecStatus::Fail
            ));
        }
        // Non-re-executable profiles admit NotVerified.
        for profile in ["full", "minimal"] {
            assert!(compute_deposit_ready(
                profile,
                CheckStatus::Pass,
                CheckStatus::Pass,
                CheckStatus::Pass,
                ReexecStatus::NotVerified
            ));
        }
    }
}
