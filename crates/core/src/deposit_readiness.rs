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
//! `domain_validation` + `provenance_divergence` + `substrate_validity` +
//! `reexecution` so a run can be computationally complete while
//! `deposit_ready` reads `false`.
//!
//! Layer 1 also runs the §G-B2 observed-read provenance-divergence backstop
//! (`provenance_divergence`, via [`scan_provenance_divergence`]): a genuine
//! (allowance-uncovered) observed-read divergence recorded in the package's
//! own durable records — a task `Blocked{ProvenanceDivergence}` in
//! `WORKFLOW.json` and/or a non-empty `ecaax:provenanceDivergence` array on
//! the RO-Crate root — blocks the deposit boundary regardless of which
//! execution path minted it. This is the path-independent enforcement of the
//! invariant the per-path finalize blocking (harness `end_of_run_finalize`)
//! also applies.
//!
//! Layer 1 also READS (never re-runs) the audit-proof `substrate_validity`
//! verdict (`substrate_validity`, via [`scan_substrate_validity`]) — the
//! runcrate/WRROC-substrate axis of the audit-proof invariant suite
//! (Invariant 6, `crate::audit_proof::invariants::substrate_validity`) already
//! recorded in `runtime/audit-proof-report.json`. A concrete recorded `fail`
//! blocks the deposit; `unverified`/`warn`, or an absent/unparseable report —
//! the honest outcome for an offline deposit that never ran the external
//! `runcrate` validator — stay non-blocking, mirroring how `reexecution:
//! NotVerified` is admitted outside the `re-executable` profile.
//!
//! Layer 1 also runs the DR-8 portability scan (`portability_warnings`, via
//! [`scan_portability`]): a WARN-ONLY advisory listing residual absolute host
//! paths (`/home/…`) and bare session-id occurrences found in the sealed
//! deposit OUTSIDE the one declared identity field ([`DECLARED_IDENTITY_FIELD`]).
//! It is deliberately NOT folded into `deposit_ready`: a `re-executable`
//! deposit legitimately needs some absolute host paths to replay (captured
//! conda `env.lock` prefixes, agent-authored `scripts/*`, the resolved BLAS
//! `.so` path) — scrubbing those would break re-execution. So the scan
//! surfaces the non-portability honestly without blocking a deposit whose
//! replay genuinely depends on those paths. See [`scan_portability`] for the
//! bounded set of residuals that cannot be safely relativized and why.
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
    /// Headline signal: `true` iff `ro_crate`, `bagit`,
    /// `domain_validation`, `provenance_divergence`, and `substrate_validity`
    /// all `Pass` AND `reexecution != Fail` (RCA I-10, §G-B2).
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
    /// Genuine observed-read provenance-divergence backstop (§G-B2): `Fail`
    /// when the package records a genuine (allowance-uncovered) observed-read
    /// divergence — a task read an input no declared producer emits. Detected
    /// PATH-INDEPENDENTLY (see [`scan_provenance_divergence`]) from the
    /// package's own durable records — a task `Blocked{ProvenanceDivergence}`
    /// in `WORKFLOW.json` and/or a non-empty `ecaax:provenanceDivergence`
    /// array on the RO-Crate root — so a divergence blocks the deposit
    /// regardless of which execution path minted it. `#[serde(default)]` →
    /// `Pass` for attestations predating this field (absence never fabricates
    /// a divergence; a clean package is unaffected).
    #[serde(default)]
    pub provenance_divergence: CheckStatus,
    /// Substrate-validity axis (the runcrate/WRROC-substrate audit-proof
    /// invariant, Invariant 6): `Fail` when the ALREADY-RECORDED
    /// `substrate_validity` verdict in `runtime/audit-proof-report.json` is a
    /// concrete `fail` — see [`scan_substrate_validity`]. READ-ONLY: writing
    /// this attestation never forces a fresh `runcrate` run. `unverified` /
    /// `warn` (or an absent/unparseable report — the honest outcome for an
    /// offline deposit that never ran the external runcrate validator) are
    /// NON-blocking, mirroring how `reexecution: NotVerified` is admitted
    /// outside the `re-executable` profile. `#[serde(default)]` → `Pass` for
    /// attestations predating this field (absence never fabricates a
    /// substrate failure).
    #[serde(default)]
    pub substrate_validity: CheckStatus,
    /// DR-8 portability advisory: residual absolute host paths (`/home/…`) and
    /// bare session-id occurrences found in the sealed deposit OUTSIDE the one
    /// declared identity field ([`DECLARED_IDENTITY_FIELD`]). WARN-ONLY —
    /// surfaced for operator visibility but DELIBERATELY NOT folded into
    /// `deposit_ready` (see [`scan_portability`]): a `re-executable` deposit
    /// may legitimately need some absolute host paths to replay, so a residual
    /// path must not block an otherwise-valid deposit. Empty ⇒ fully portable.
    /// `#[serde(default)]` so an attestation predating this field deserializes
    /// as clean (absence never fabricates a warning).
    #[serde(default)]
    pub portability_warnings: Vec<String>,
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
    /// `required_failures` array, in scan order. Also carries the
    /// source-owned reporting-correctness validator's REQUIRED failures
    /// (RP-2/RP-4/RP-5) under the synthetic `reporting_invariants` task id
    /// (see [`crate::reporting_invariants`]).
    pub required_failures: Vec<String>,
    /// Advisory (non-blocking) reporting-correctness warnings
    /// (RP-1/RP-3/RP-9). Surfaced for operator visibility but never folded
    /// into [`Self::passed`] — they must not block a scientifically-correct
    /// deposit. `#[serde(default)]` so an older serialized summary
    /// deserializes cleanly.
    #[serde(default)]
    pub reporting_warnings: Vec<String>,
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

    // Fold in the source-owned reporting-correctness checklist (RP-8): it
    // RECOMPUTES values from the package's own runtime outputs rather than
    // trusting the agent-authored per-run report/validator scripts, so a
    // report that transcribes a wrong upstream number is still caught.
    // REQUIRED failures (RP-2/RP-4/RP-5) block deposit-readiness under the
    // synthetic `reporting_invariants` task id; WARN findings
    // (RP-1/RP-3/RP-9) are surfaced separately and never block.
    let ri = crate::reporting_invariants::check_reporting_invariants(package_root);
    let ri_required = ri.required_failures();
    if !ri_required.is_empty() {
        let task_id = "reporting_invariants".to_string();
        summary.checked_tasks.push(task_id.clone());
        summary.failed_tasks.push(task_id.clone());
        for f in ri_required {
            summary.required_failures.push(format!("{task_id}: {f}"));
        }
    } else if !ri.checked.is_empty() {
        // Ran, nothing REQUIRED failed — record it as a passing check.
        summary.checked_tasks.push("reporting_invariants".to_string());
    }
    summary.reporting_warnings = ri.warnings();

    summary
}

/// Reason-string marker prefix a genuine observed-read divergence writes into
/// a re-blocked task's `BlockedRecord.reason`
/// (`crates/harness/src/end_of_run_finalize.rs::provenance_divergence_reason`
/// and the conversation emit path's `apply_provenance_divergence_blockers`).
/// Matched here so the deposit gate detects a divergence-blocked task
/// independently of the harness crate (core does not depend on harness). Kept
/// byte-identical to the harness/emit marker.
const PROVENANCE_DIVERGENCE_MARKER: &str = "[provenance_divergence]";

/// RO-Crate root-Dataset key recording genuine (allowance-uncovered)
/// observed-read divergences
/// (`crate::ro_crate::reconcile_ro_crate_edges_with_allowances`). Sanctioned
/// reads land under `ecaax:provenanceReadAllowance` instead, so a non-empty
/// value of THIS key is a genuine divergence by construction.
const RO_CRATE_DIVERGENCE_KEY: &str = "ecaax:provenanceDivergence";

/// Path-independent genuine observed-read divergence signal (§G-B2).
///
/// Populated from the package's own durable records — see
/// [`scan_provenance_divergence`]. A non-empty [`Self::divergences`] means a
/// task read an input no declared producer emits (and the read was not covered
/// by a sanctioned read-allowance), which must block the deposit boundary
/// regardless of which execution path (standalone/CLI or session/web-UI)
/// minted it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProvenanceDivergenceSummary {
    /// Human-readable `"<task_id>: <detail>"` for each genuine divergence
    /// found, de-duplicated and sorted. Empty ⇒ clean.
    pub divergences: Vec<String>,
}

impl ProvenanceDivergenceSummary {
    /// `true` iff no genuine observed-read divergence is recorded. Vacuously
    /// `true` for a package that recorded none — absence is not a divergence.
    pub fn is_clean(&self) -> bool {
        self.divergences.is_empty()
    }
}

/// Scan the package (a sealed deposit dir or an emitted package root) for a
/// GENUINE observed-read provenance divergence (§G-B2), from two independent,
/// path-independent durable records — EITHER present ⇒ a divergence:
///
/// 1. **WORKFLOW.json** — a task in `Blocked{ProvenanceDivergence}` state (its
///    `BlockedRecord.reason` carries the [`PROVENANCE_DIVERGENCE_MARKER`]).
///    This is what the harness / conversation emit path flip a task to on a
///    genuine divergence.
/// 2. **ro-crate-metadata.json** — a non-empty [`RO_CRATE_DIVERGENCE_KEY`]
///    array on the root Dataset (`@id == "./"`). The reconciler records only
///    genuine (allowance-uncovered) divergences under this key.
///
/// Both sources already exclude allowance-sanctioned reads, so any hit is a
/// genuine divergence. Best-effort reads: a missing/unparseable file
/// contributes nothing (absence is never a divergence). Deterministic:
/// results are de-duplicated and sorted.
pub fn scan_provenance_divergence(package_root: &Path) -> ProvenanceDivergenceSummary {
    let mut found: Vec<String> = Vec::new();

    // (1) WORKFLOW.json — a task Blocked with the provenance-divergence marker.
    if let Ok(raw) = std::fs::read_to_string(package_root.join("WORKFLOW.json")) {
        if let Ok(wf) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(tasks) = wf.get("tasks").and_then(|t| t.as_object()) {
                for (task_id, task) in tasks {
                    let state = &task["state"];
                    if state.get("status").and_then(|s| s.as_str()) != Some("blocked") {
                        continue;
                    }
                    let reason = state
                        .get("record")
                        .and_then(|r| r.get("reason"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    if reason.starts_with(PROVENANCE_DIVERGENCE_MARKER) {
                        found.push(format!("{task_id}: blocked on observed-read divergence"));
                    }
                }
            }
        }
    }

    // (2) ro-crate-metadata.json — a non-empty divergence array on the root.
    if let Ok(raw) = std::fs::read_to_string(package_root.join("ro-crate-metadata.json")) {
        if let Ok(md) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(graph) = md.get("@graph").and_then(|g| g.as_array()) {
                if let Some(root) = graph
                    .iter()
                    .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("./"))
                {
                    if let Some(arr) = root.get(RO_CRATE_DIVERGENCE_KEY).and_then(|v| v.as_array()) {
                        for d in arr {
                            let task_id = d.get("task_id").and_then(|v| v.as_str()).unwrap_or("?");
                            let read_path =
                                d.get("read_path").and_then(|v| v.as_str()).unwrap_or("?");
                            found.push(format!("{task_id}: undeclared read {read_path}"));
                        }
                    }
                }
            }
        }
    }

    found.sort();
    found.dedup();
    ProvenanceDivergenceSummary { divergences: found }
}

/// Path-independent substrate-validity signal, mirroring
/// [`ProvenanceDivergenceSummary`]: the audit-proof `substrate_validity`
/// invariant verdict (the runcrate/WRROC-substrate axis, recorded by
/// `crate::audit_proof::invariants::substrate_validity::check_substrate_validity`)
/// read back from `runtime/audit-proof-report.json` — see
/// [`scan_substrate_validity`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubstrateValiditySummary {
    /// `true` only for a concrete recorded `fail`. Everything else (`pass`,
    /// `warn`, `unverified`, a missing report, or an unparseable one) leaves
    /// this `false` — an offline deposit legitimately never runs the external
    /// runcrate validator (it records `unverified`), and that honest absence
    /// must never read as a failure.
    pub failed: bool,
    /// Human-readable detail from the recorded verdict, when present.
    pub detail: Option<String>,
}

/// Read the already-recorded `substrate_validity` invariant verdict (the
/// runcrate/WRROC-substrate axis, Invariant 6) back from
/// `runtime/audit-proof-report.json`. NEVER forces a fresh runcrate run —
/// only reads what a prior `emit_package` / `reseal_audit_report` already
/// recorded, mirroring [`scan_provenance_divergence`]'s path-independent,
/// best-effort read: a missing file, an unparseable report, or a report that
/// carries no `substrate_validity` verdict all contribute nothing (absence is
/// never a failure). Only a concrete recorded `InvariantStatus::Fail` sets
/// [`SubstrateValiditySummary::failed`] — `Warn`/`Unverified` (including the
/// no-op-validator outcome recorded when `runcrate` never ran) stay clean.
pub fn scan_substrate_validity(package_root: &Path) -> SubstrateValiditySummary {
    let path = package_root.join("runtime").join("audit-proof-report.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return SubstrateValiditySummary::default();
    };
    let Ok(report) = serde_json::from_str::<crate::audit_proof::AuditProofReport>(&raw) else {
        return SubstrateValiditySummary::default();
    };
    let Some(verdict) = report
        .verdicts
        .iter()
        .find(|v| v.id == crate::audit_proof::InvariantId::SubstrateValidity)
    else {
        return SubstrateValiditySummary::default();
    };
    SubstrateValiditySummary {
        failed: verdict.status == crate::audit_proof::InvariantStatus::Fail,
        detail: verdict.detail.clone(),
    }
}

/// The single field in which a portable deposit is permitted to carry the
/// session/package identity (§G exemption, DR-8). `WORKFLOW.json`'s
/// `workflow_id` is the canonical, content-stable package/workflow id (the
/// `workflow-<session-uuid>` string the composer/conversation layer mints);
/// the portability scan treats the identity there as EXEMPT and reports every
/// OTHER occurrence of the raw session id as a residual advisory.
pub const DECLARED_IDENTITY_FIELD: &str = "WORKFLOW.json::workflow_id";

/// Absolute filesystem roots that make a path host-specific (non-portable).
/// A path beginning with one of these embeds an operator's home / machine
/// layout into the deposit.
const HOST_PATH_ROOTS: &[&str] = &["/home/", "/Users/", "/root/"];

/// File extensions whose bytes are binary (or otherwise not worth a text
/// portability scan). Skipped by [`scan_portability`] — a `/home/…` reference
/// worth surfacing lives in a TEXT config/lock/script, never inside a binary
/// blob (the copied BLAS `.so` is skipped here; the TEXT `env.lock` /
/// `determinism-env.json` that REFERENCES its absolute path is not).
const PORTABILITY_SKIP_EXTS: &[&str] = &[
    "so", "a", "o", "dylib", "dll", "zip", "gz", "bz2", "xz", "zst", "tar", "tgz", "pdf", "png",
    "jpg", "jpeg", "gif", "bmp", "ico", "parquet", "feather", "arrow", "h5", "hdf5", "h5ad", "loom",
    "bam", "sam", "cram", "bai", "crai", "npz", "npy", "pyc", "pyo", "whl", "rds", "rdata", "rda",
    "bin", "pkl", "pickle", "db", "sqlite", "woff", "woff2", "ttf", "eot", "mp4", "mov",
];

/// Files larger than this are skipped by the portability scan (host-path
/// references live in small config/lock/script files; a multi-MB file is data).
const PORTABILITY_MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Cap on findings of each kind so the attestation stays bounded even for a
/// deposit that leaks the same path across thousands of lines.
const PORTABILITY_FINDINGS_CAP: usize = 100;

/// DR-8 portability advisory. WARN-ONLY — never folded into `deposit_ready`.
///
/// A non-empty summary means the deposit carries host-specific state that
/// makes it non-relocatable AS-IS. Some of that state is LOAD-BEARING for
/// re-execution and cannot be safely scrubbed (see the module docs and the
/// "honest residual" note on [`scan_portability`]); the scan therefore reports
/// it as advisory rather than blocking.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PortabilitySummary {
    /// `"<relpath>: <absolute-host-path>"` for each distinct absolute host path
    /// found in a scanned TEXT artifact, sorted + de-duplicated.
    pub host_paths: Vec<String>,
    /// `"<relpath>: <session-uuid>"` for each scanned TEXT artifact that
    /// carries the raw (hyphenated) session id OUTSIDE the one declared
    /// identity field, sorted + de-duplicated. Empty when the deposit's
    /// `workflow_id` is not a `workflow-<uuid>` identity (e.g. a CLI-built
    /// package), in which case only the host-path axis applies.
    pub session_id_leaks: Vec<String>,
}

impl PortabilitySummary {
    /// `true` iff the deposit carries no residual host path and no bare
    /// session-id leak — i.e. it is fully relocatable.
    pub fn is_clean(&self) -> bool {
        self.host_paths.is_empty() && self.session_id_leaks.is_empty()
    }

    /// Flatten to advisory warning strings for the attestation
    /// (`portability_warnings`). Prefixed by kind so an operator reading the
    /// attestation can tell a host-path residual from a session-id residual.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.host_paths.len() + self.session_id_leaks.len());
        for h in &self.host_paths {
            out.push(format!("absolute host path — {h}"));
        }
        for s in &self.session_id_leaks {
            out.push(format!("bare session id — {s}"));
        }
        out
    }
}

/// Extract the raw (hyphenated) session UUID a deposit's `workflow_id` encodes,
/// when it has the canonical `workflow-<32 hex>` shape. Returns `(hyphenated,
/// declared_workflow_id)` so callers can search for the bare UUID form while
/// exempting the declared `workflow-…` string itself. `None` for any other
/// `workflow_id` shape (CLI-built packages, legacy ids) — the host-path axis
/// still applies, only the session-id axis is skipped.
fn declared_session_uuid(package_root: &Path) -> Option<(String, String)> {
    let raw = std::fs::read_to_string(package_root.join("WORKFLOW.json")).ok()?;
    let wf: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let workflow_id = wf.get("workflow_id")?.as_str()?.to_string();
    let simple = workflow_id.strip_prefix("workflow-")?;
    // Must be exactly a 32-hex-char UUID (hyphens stripped) to reconstruct.
    if simple.len() != 32 || !simple.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let hyphenated = format!(
        "{}-{}-{}-{}-{}",
        &simple[0..8],
        &simple[8..12],
        &simple[12..16],
        &simple[16..20],
        &simple[20..32],
    );
    Some((hyphenated, workflow_id))
}

/// Scan `content` for absolute host paths, returning each match as
/// `(start, end)` byte spans plus the matched path string. A match begins at a
/// [`HOST_PATH_ROOTS`] prefix and runs until a path-terminating delimiter
/// (whitespace, quotes, backtick, or JSON/markdown structural punctuation).
fn find_host_paths(content: &str) -> Vec<(usize, usize, String)> {
    /// A byte that terminates a filesystem path token in JSON / markdown /
    /// shell contexts. `:` and `=` end key/value framing; brackets/quotes end
    /// string literals. Unix paths in this codebase never contain these.
    fn is_terminator(b: u8) -> bool {
        b.is_ascii_whitespace()
            || matches!(
                b,
                b'"' | b'\'' | b'`' | b',' | b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'<' | b'>'
                    | b';' | b':' | b'\\' | b'|' | b'*' | b'?' | b'='
            )
    }
    // Scan on BYTES (not `str` slices) so a `/home/…` reference embedded in an
    // otherwise-multibyte UTF-8 file never panics on a non-char-boundary index.
    // The roots + path chars are ASCII, and every UTF-8 continuation byte is
    // >= 0x80, so byte matching never splits a codepoint.
    let bytes = content.as_bytes();
    let mut spans: Vec<(usize, usize, String)> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let at_root = HOST_PATH_ROOTS
            .iter()
            .any(|r| bytes[i..].starts_with(r.as_bytes()));
        if !at_root {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        while end < bytes.len() && !is_terminator(bytes[end]) {
            end += 1;
        }
        // Trim a trailing path separator or dot so `"/home/x/",` records
        // `/home/x` rather than `/home/x/`.
        let mut trimmed = end;
        while trimmed > start && matches!(bytes[trimmed - 1], b'/' | b'.') {
            trimmed -= 1;
        }
        if trimmed > start {
            let seg = String::from_utf8_lossy(&bytes[start..trimmed]).into_owned();
            spans.push((start, trimmed, seg));
        }
        i = end.max(start + 1);
    }
    spans
}

/// DR-8 portability scan over a sealed deposit (or an emitted package root).
///
/// Walks every TEXT artifact (skipping binary blobs by extension, files over
/// [`PORTABILITY_MAX_FILE_BYTES`], and the manifest-excluded
/// `DEPOSIT-READINESS.json` itself) and collects two residual signals:
///
/// 1. **Absolute host paths** (`/home/…`, `/Users/…`, `/root/…`) — anything
///    that pins the deposit to one operator's machine layout.
/// 2. **Bare session id** — the raw (hyphenated) session UUID the deposit's
///    `workflow_id` encodes, found ANYWHERE other than inside an
///    already-reported host path. The declared identity itself
///    ([`DECLARED_IDENTITY_FIELD`], the `workflow-<uuid>` string) is EXEMPT.
///
/// **Honest residual — why this is WARN-only, not a hard gate.** A
/// `re-executable` deposit's replay legitimately depends on absolute host
/// paths that CANNOT be safely scrubbed by the compiler:
/// - captured conda/pip lock files (`runtime/outputs/*/env.lock`) that pin a
///   host-specific environment prefix,
/// - the agent-authored per-task `scripts/*` and `agent-code.json`, which may
///   hardcode an input or output path the run actually used,
/// - the resolved BLAS shared object path (`…/libopenblasp-r0.3.33.so`) baked
///   into a determinism/env capture,
/// - the SME-registered EXTERNAL data root (`runtime/inputs.json::root_path`,
///   surfaced in `CONTEXT.md`), which points at real data OUTSIDE the package.
///
/// Blindly relativizing any of these would break re-execution, so the scan
/// SURFACES them (deposit non-portability is a real, documented property) but
/// never flips `deposit_ready`. Deterministic: findings are de-duplicated and
/// sorted; each axis is capped at [`PORTABILITY_FINDINGS_CAP`].
pub fn scan_portability(package_root: &Path) -> PortabilitySummary {
    let declared = declared_session_uuid(package_root);
    let sid = declared.as_ref().map(|(hyphenated, _)| hyphenated.as_str());

    let mut host_paths: Vec<String> = Vec::new();
    let mut session_id_leaks: Vec<String> = Vec::new();

    // Deterministic depth-first walk; entries within a directory are visited in
    // sorted order so a truncated (capped) finding set is stable.
    let mut stack: Vec<std::path::PathBuf> = vec![package_root.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&cur) else {
            continue;
        };
        let mut entries: Vec<std::path::PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        // Push in reverse so the sorted order is preserved on the LIFO stack.
        for path in entries.into_iter().rev() {
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(package_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            // Skip the attestation itself (written AFTER this scan; a prior
            // copy would otherwise feed its own warnings back in) and binary
            // blobs / oversized data files.
            if rel == DEPOSIT_READINESS_FILE {
                continue;
            }
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if PORTABILITY_SKIP_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
                    continue;
                }
            }
            match std::fs::metadata(&path) {
                Ok(m) if m.len() > PORTABILITY_MAX_FILE_BYTES => continue,
                Ok(_) => {}
                Err(_) => continue,
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue; // non-UTF8 → treat as binary, skip
            };

            let spans = find_host_paths(&content);
            for (_, _, p) in &spans {
                host_paths.push(format!("{rel}: {p}"));
            }

            // Session-id leak: the raw UUID found outside any host-path span.
            // The `workflow-<uuid>` declared identity never matches (it carries
            // the simple, hyphen-free form), so it is exempt by construction.
            if let Some(sid) = sid {
                let leaked_outside_path = content.match_indices(sid).any(|(idx, _)| {
                    !spans.iter().any(|(s, e, _)| idx >= *s && idx < *e)
                });
                if leaked_outside_path {
                    session_id_leaks.push(format!("{rel}: {sid}"));
                }
            }
        }
    }

    host_paths.sort();
    host_paths.dedup();
    host_paths.truncate(PORTABILITY_FINDINGS_CAP);
    session_id_leaks.sort();
    session_id_leaks.dedup();
    session_id_leaks.truncate(PORTABILITY_FINDINGS_CAP);

    PortabilitySummary {
        host_paths,
        session_id_leaks,
    }
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

    // §G-B2 backstop — a genuine observed-read divergence recorded in the
    // package's own durable records must block the deposit boundary regardless
    // of which execution path minted it (the session/web-UI path records the
    // divergence but is the exact path where nothing else fails the deposit).
    let divergence = scan_provenance_divergence(dst);
    let provenance_divergence = if divergence.is_clean() {
        CheckStatus::Pass
    } else {
        CheckStatus::Fail
    };
    let divergence_detail = (!divergence.divergences.is_empty()).then(|| {
        format!(
            "observed-read provenance divergence(s): {}",
            divergence.divergences.join("; ")
        )
    });

    // Substrate-validity axis (Invariant 6) — READ the already-recorded
    // verdict from `runtime/audit-proof-report.json`; never forces a fresh
    // `runcrate` run here. A concrete recorded `fail` blocks the deposit; the
    // honest offline `unverified`/`warn` outcome (or an absent report) stays
    // non-blocking.
    let substrate = scan_substrate_validity(dst);
    let substrate_validity = if substrate.failed {
        CheckStatus::Fail
    } else {
        CheckStatus::Pass
    };
    let substrate_detail = substrate.failed.then(|| {
        format!(
            "substrate-validity failure: {}",
            substrate.detail.as_deref().unwrap_or("no detail recorded")
        )
    });

    // Advisory reporting-correctness warnings (RP-1/RP-3/RP-9): recorded in
    // the attestation for operator visibility but deliberately NOT folded
    // into `deposit_ready` — a warn-only prose finding must never block a
    // scientifically-correct deposit.
    let reporting_warnings_detail = (!domain.reporting_warnings.is_empty()).then(|| {
        format!(
            "reporting-correctness warning(s): {}",
            domain.reporting_warnings.join(", ")
        )
    });

    // DR-8 portability advisory — residual absolute host paths + bare
    // session-id occurrences. WARN-ONLY: surfaced in the attestation but NOT
    // folded into `deposit_ready` (a re-executable deposit's replay may
    // legitimately need some host paths — see `scan_portability`).
    let portability = scan_portability(dst);
    let portability_warnings = portability.warnings();
    let portability_detail = (!portability_warnings.is_empty()).then(|| {
        format!(
            "portability warning(s): {} residual host path(s), {} bare session-id occurrence(s)",
            portability.host_paths.len(),
            portability.session_id_leaks.len()
        )
    });

    let detail = [
        tier1.detail.clone(),
        domain_detail,
        divergence_detail,
        substrate_detail,
        reporting_warnings_detail,
        portability_detail,
        reexec_detail,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let detail = (!detail.is_empty()).then(|| detail.join("; "));

    // `portability_warnings` is intentionally ABSENT from this fold: a residual
    // host path is advisory (a re-executable deposit may need it to replay),
    // so it must never flip `deposit_ready` false.
    let deposit_ready = compute_deposit_ready(
        profile,
        tier1.ro_crate,
        tier1.bagit,
        domain_validation,
        reexecution,
    ) && provenance_divergence == CheckStatus::Pass
        && substrate_validity == CheckStatus::Pass;

    let att = DepositReadiness {
        schema_version: "0.1".to_string(),
        profile: profile.to_string(),
        deposit_ready,
        ro_crate: tier1.ro_crate,
        bagit: tier1.bagit,
        domain_validation,
        provenance_divergence,
        substrate_validity,
        portability_warnings,
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
///
/// Also RE-SCANS `substrate_validity` from `runtime/audit-proof-report.json`
/// before re-deriving `deposit_ready`: the CLI's Layer-2 fold-back
/// (`reseal_audit_report` / `audit_fold::reseal_deferred`) re-records that
/// file's `substrate_validity` verdict from a real `runcrate` run BEFORE
/// calling this function, so the value Layer 1 captured at write time can be
/// stale here. Re-reading (never re-running) closes that window — a genuine
/// substrate `fail` folded in by the reseal step must flip `deposit_ready`
/// false even though Layer 1 saw the pre-reseal (typically `unverified`)
/// verdict.
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
    // Re-scan the substrate-validity axis: a Layer-2 reseal (which may have
    // just run before this call) can have refreshed the on-disk
    // `runtime/audit-proof-report.json` verdict since Layer 1 wrote this
    // attestation.
    let substrate = scan_substrate_validity(dst);
    att.substrate_validity = if substrate.failed {
        CheckStatus::Fail
    } else {
        CheckStatus::Pass
    };
    if substrate.failed {
        let d = format!(
            "substrate-validity failure: {}",
            substrate.detail.as_deref().unwrap_or("no detail recorded")
        );
        att.detail = Some(match att.detail.take() {
            Some(existing) => format!("{existing}; {d}"),
            None => d,
        });
    }
    // Re-derive the headline signal: ro_crate/bagit/domain_validation and the
    // §G-B2 provenance-divergence axis are unchanged by a Layer-2 re-execution
    // update, but the new `reexecution` value can flip `deposit_ready` (e.g. a
    // fresh `Fail`, or — for the `re-executable` profile — a re-execution that
    // never ran), and the freshly re-scanned `substrate_validity` can too. A
    // recorded divergence still hard-blocks readiness.
    att.deposit_ready = compute_deposit_ready(
        &att.profile,
        att.ro_crate,
        att.bagit,
        att.domain_validation,
        att.reexecution,
    ) && att.provenance_divergence == CheckStatus::Pass
        && att.substrate_validity == CheckStatus::Pass;
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
/// self-validation failed, or whose re-execution FAILED, or whose recorded
/// `substrate_validity` verdict is a concrete FAIL. A `NotVerified`
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
    // §G-B2 backstop: refuse a deposit that recorded a genuine observed-read
    // provenance divergence, regardless of `--strict` and regardless of which
    // execution path minted it. A task read an input no declared producer
    // emits — the provenance graph is not sound, so the deposit must not ship.
    if dr.provenance_divergence != CheckStatus::Pass {
        bail!(
            "deposit gate: a genuine observed-read provenance divergence is recorded ({:?}){} — \
             a task read an undeclared input (no declared producer emits it); the observed \
             provenance is unsound, so the deposit is refused. Reconcile the read/declared edge \
             (or record a sanctioned read-allowance) and re-export",
            dr.provenance_divergence,
            dr.detail.as_deref().map(|d| format!(" — {d}")).unwrap_or_default()
        );
    }
    // Substrate-validity axis (Invariant 6): refuse a deposit whose recorded
    // WRROC/runcrate substrate verdict is a concrete FAIL. `unverified`/`warn`
    // (the honest offline outcome — see `scan_substrate_validity`) are not
    // blocked here.
    if dr.substrate_validity != CheckStatus::Pass {
        bail!(
            "deposit gate: recorded substrate-validity verdict did not pass ({:?}){} — \
             the WRROC/runcrate substrate audit (Invariant 6) recorded a FAIL; remediate \
             and re-export (or re-run `ecaa-workflow reexec --reseal`)",
            dr.substrate_validity,
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

    // -----------------------------------------------------------------------
    // §G-B2 — genuine observed-read divergence backstop
    // -----------------------------------------------------------------------

    /// Plant a WORKFLOW.json with `task_id` in `Blocked{ProvenanceDivergence}`
    /// (its reason carries the `[provenance_divergence]` marker) — the shape
    /// the harness / emit path leave on a genuine divergence.
    fn write_workflow_divergence_block(root: &Path, task_id: &str) {
        let wf = serde_json::json!({
            "tasks": {
                task_id: {
                    "state": {
                        "status": "blocked",
                        "record": {
                            "reason": "[provenance_divergence] {\"ProvenanceDivergence\":{\"task_id\":\"differential_expression\",\"read_path\":\"runtime/outputs/data_acquisition/counts.tsv\",\"declared_producer\":null}}",
                            "attempts": []
                        }
                    }
                }
            }
        });
        fs::write(root.join("WORKFLOW.json"), serde_json::to_vec_pretty(&wf).unwrap()).unwrap();
    }

    /// Plant an ro-crate-metadata.json with a non-empty
    /// `ecaax:provenanceDivergence` array on the root Dataset.
    fn write_ro_crate_divergence(root: &Path) {
        let md = serde_json::json!({
            "@graph": [
                {
                    "@id": "./",
                    "@type": "Dataset",
                    "ecaax:provenanceDivergence": [
                        {
                            "task_id": "differential_expression",
                            "read_path": "runtime/outputs/data_acquisition/counts.tsv",
                            "declared_producer": null
                        }
                    ]
                }
            ]
        });
        fs::write(
            root.join("ro-crate-metadata.json"),
            serde_json::to_vec_pretty(&md).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn scan_provenance_divergence_detects_both_sources_and_clean_when_none() {
        // Clean: empty package.
        let clean = tempfile::tempdir().unwrap();
        assert!(scan_provenance_divergence(clean.path()).is_clean());

        // WORKFLOW.json blocked-task source.
        let wf = tempfile::tempdir().unwrap();
        write_workflow_divergence_block(wf.path(), "differential_expression");
        let s = scan_provenance_divergence(wf.path());
        assert!(!s.is_clean(), "a Blocked{{ProvenanceDivergence}} task is a divergence");
        assert_eq!(s.divergences.len(), 1);

        // RO-Crate array source.
        let rc = tempfile::tempdir().unwrap();
        write_ro_crate_divergence(rc.path());
        let s = scan_provenance_divergence(rc.path());
        assert!(!s.is_clean(), "a non-empty ecaax:provenanceDivergence is a divergence");
        assert!(s.divergences[0].contains("data_acquisition/counts.tsv"));

        // A Blocked task for ANOTHER reason must NOT count.
        let other = tempfile::tempdir().unwrap();
        let wf = serde_json::json!({
            "tasks": {"t": {"state": {"status": "blocked",
                "record": {"reason": "[claim_coverage] something else", "attempts": []}}}}
        });
        fs::write(other.path().join("WORKFLOW.json"), wf.to_string()).unwrap();
        assert!(scan_provenance_divergence(other.path()).is_clean());
    }

    /// §G-B2: a recorded genuine observed-read divergence must flip
    /// `deposit_ready` false and make the Layer-3 gate REFUSE — even for a
    /// `full` profile and even non-strict (a divergence is a hard block).
    #[test]
    fn gate_refuses_recorded_provenance_divergence() {
        // Source (1): WORKFLOW.json Blocked{ProvenanceDivergence}.
        let wf = tempfile::tempdir().unwrap();
        write_workflow_divergence_block(wf.path(), "differential_expression");
        write_deposit_readiness(
            wf.path(),
            "full",
            &tier1(CheckStatus::Pass, CheckStatus::Pass, None),
            ReexecStatus::Pass,
            None,
            None,
            &WallClock,
        )
        .unwrap();
        let dr = read_deposit_readiness(wf.path()).unwrap().unwrap();
        assert_eq!(dr.ro_crate, CheckStatus::Pass, "structurally sound");
        assert_eq!(dr.bagit, CheckStatus::Pass, "structurally sound");
        assert_eq!(dr.provenance_divergence, CheckStatus::Fail);
        assert!(!dr.deposit_ready, "a recorded divergence must block deposit-readiness");
        assert!(dr.detail.as_deref().unwrap().contains("divergence"));
        let err = check_deposit_readiness(wf.path(), false)
            .expect_err("Layer-3 gate must refuse a recorded divergence even non-strict");
        assert!(format!("{err:#}").contains("divergence"));

        // Source (2): RO-Crate ecaax:provenanceDivergence array.
        let rc = tempfile::tempdir().unwrap();
        write_ro_crate_divergence(rc.path());
        write_deposit_readiness(
            rc.path(),
            "full",
            &tier1(CheckStatus::Pass, CheckStatus::Pass, None),
            ReexecStatus::Pass,
            None,
            None,
            &WallClock,
        )
        .unwrap();
        let dr = read_deposit_readiness(rc.path()).unwrap().unwrap();
        assert_eq!(dr.provenance_divergence, CheckStatus::Fail);
        assert!(!dr.deposit_ready);
        assert!(check_deposit_readiness(rc.path(), false).is_err());
    }

    /// A clean package (no recorded divergence) passes the §G-B2 backstop:
    /// `provenance_divergence` reads `Pass` and the gate admits it.
    #[test]
    fn clean_package_passes_divergence_backstop() {
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
        assert_eq!(dr.provenance_divergence, CheckStatus::Pass);
        assert!(dr.deposit_ready);
        assert!(check_deposit_readiness(tmp.path(), false).is_ok());
    }

    // -----------------------------------------------------------------------
    // Invariant 6 — substrate-validity (runcrate/WRROC) axis
    // -----------------------------------------------------------------------

    /// Plant `runtime/audit-proof-report.json` with the `substrate_validity`
    /// verdict set to `status`/`detail`, everything else left at the
    /// `AuditProofReport::empty()` default (`Unverified`).
    fn write_audit_proof_substrate(
        root: &Path,
        status: crate::audit_proof::InvariantStatus,
        detail: Option<&str>,
    ) {
        use crate::audit_proof::InvariantId;
        let mut report = crate::audit_proof::AuditProofReport::empty();
        for v in report.verdicts.iter_mut() {
            if v.id == InvariantId::SubstrateValidity {
                v.status = status;
                v.detail = detail.map(str::to_string);
            }
        }
        let runtime = root.join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        fs::write(
            runtime.join("audit-proof-report.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn scan_substrate_validity_reads_recorded_fail_and_ignores_unverified_or_absent() {
        // Absent report ⇒ clean (never fabricate a failure from silence).
        let absent = tempfile::tempdir().unwrap();
        let s = scan_substrate_validity(absent.path());
        assert!(!s.failed);

        // Recorded `unverified` (the honest offline outcome, no `runcrate` run)
        // ⇒ NOT a failure.
        let unverified = tempfile::tempdir().unwrap();
        write_audit_proof_substrate(
            unverified.path(),
            crate::audit_proof::InvariantStatus::Unverified,
            Some("runcrate not installed"),
        );
        assert!(!scan_substrate_validity(unverified.path()).failed);

        // Recorded `warn` (execution-consistency drift downgrade) ⇒ NOT a
        // failure either.
        let warn = tempfile::tempdir().unwrap();
        write_audit_proof_substrate(
            warn.path(),
            crate::audit_proof::InvariantStatus::Warn,
            Some("execution-consistency drift"),
        );
        assert!(!scan_substrate_validity(warn.path()).failed);

        // Recorded concrete `fail` ⇒ IS a failure, detail carried through.
        let failed = tempfile::tempdir().unwrap();
        write_audit_proof_substrate(
            failed.path(),
            crate::audit_proof::InvariantStatus::Fail,
            Some("runcrate report exited nonzero"),
        );
        let s = scan_substrate_validity(failed.path());
        assert!(s.failed);
        assert_eq!(s.detail.as_deref(), Some("runcrate report exited nonzero"));
    }

    /// A concrete recorded `substrate_validity` FAIL must flip
    /// `deposit_ready` false and make the Layer-3 gate refuse — even for a
    /// `full` profile and even non-strict (mirrors the provenance-divergence
    /// backstop's hard-block semantics).
    #[test]
    fn substrate_validity_fail_blocks_deposit_ready_and_gate() {
        let tmp = tempfile::tempdir().unwrap();
        write_audit_proof_substrate(
            tmp.path(),
            crate::audit_proof::InvariantStatus::Fail,
            Some("runcrate report exited nonzero"),
        );
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
        assert_eq!(dr.substrate_validity, CheckStatus::Fail);
        assert!(!dr.deposit_ready, "a recorded substrate FAIL must block deposit-readiness");
        assert!(dr.detail.as_deref().unwrap().contains("substrate"));
        let err = check_deposit_readiness(tmp.path(), false)
            .expect_err("Layer-3 gate must refuse a recorded substrate FAIL even non-strict");
        assert!(format!("{err:#}").contains("substrate"));
    }

    /// An `unverified` substrate-validity verdict (the honest outcome for an
    /// offline deposit that never ran `runcrate`) — or no audit-proof report
    /// at all — must NOT block `deposit_ready` when every other axis passes.
    #[test]
    fn substrate_validity_unverified_or_absent_does_not_block_deposit_ready() {
        // No `runtime/audit-proof-report.json` at all.
        let absent = tempfile::tempdir().unwrap();
        write_deposit_readiness(
            absent.path(),
            "full",
            &tier1(CheckStatus::Pass, CheckStatus::Pass, None),
            ReexecStatus::Pass,
            None,
            None,
            &WallClock,
        )
        .unwrap();
        let dr = read_deposit_readiness(absent.path()).unwrap().unwrap();
        assert_eq!(dr.substrate_validity, CheckStatus::Pass);
        assert!(dr.deposit_ready);
        assert!(check_deposit_readiness(absent.path(), false).is_ok());

        // Recorded `unverified`.
        let unverified = tempfile::tempdir().unwrap();
        write_audit_proof_substrate(
            unverified.path(),
            crate::audit_proof::InvariantStatus::Unverified,
            Some("runcrate not installed"),
        );
        write_deposit_readiness(
            unverified.path(),
            "re-executable",
            &tier1(CheckStatus::Pass, CheckStatus::Pass, None),
            ReexecStatus::Pass,
            None,
            None,
            &WallClock,
        )
        .unwrap();
        let dr = read_deposit_readiness(unverified.path()).unwrap().unwrap();
        assert_eq!(dr.substrate_validity, CheckStatus::Pass);
        assert!(dr.deposit_ready);
        assert!(check_deposit_readiness(unverified.path(), false).is_ok());
    }

    /// Regression for the ordering bug this fix closes: the CLI's Layer-2
    /// fold-back (`reseal_deferred`/`reseal_audit_report`) rewrites
    /// `runtime/audit-proof-report.json`'s `substrate_validity` verdict from a
    /// real `runcrate` run AFTER Layer 1 already wrote the attestation (see
    /// `crates/cli/src/export.rs`: `reseal_deferred` then
    /// `update_deposit_readiness_reexecution`). `update_deposit_readiness_reexecution`
    /// must RE-SCAN the axis rather than trust the stale Layer-1 value, so a
    /// substrate FAIL folded in by the reseal step still flips `deposit_ready`
    /// false.
    #[test]
    fn update_deposit_readiness_reexecution_rescans_substrate_validity_after_reseal() {
        let tmp = tempfile::tempdir().unwrap();
        // Layer 1: no audit-proof report yet (pre-reseal) — attestation
        // records a clean, non-blocking substrate axis.
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
        assert_eq!(
            read_deposit_readiness(tmp.path()).unwrap().unwrap().substrate_validity,
            CheckStatus::Pass
        );

        // Simulate the CLI's `reseal_deferred` step running between Layer 1
        // and Layer 2: a real runcrate run recorded a concrete FAIL.
        write_audit_proof_substrate(
            tmp.path(),
            crate::audit_proof::InvariantStatus::Fail,
            Some("WRROC HowToStep set diverges from proofs.jsonl"),
        );

        // Layer 2: the re-execution verdict itself is clean, but the just-
        // resealed substrate FAIL must still be picked up and block readiness.
        update_deposit_readiness_reexecution(
            tmp.path(),
            ReexecStatus::Pass,
            Some("6 byte_identical".into()),
            None,
            &WallClock,
        )
        .unwrap();
        let dr = read_deposit_readiness(tmp.path()).unwrap().unwrap();
        assert_eq!(dr.substrate_validity, CheckStatus::Fail);
        assert!(
            !dr.deposit_ready,
            "a substrate FAIL folded in by the Layer-2 reseal must still block deposit_ready"
        );
        assert!(dr.detail.as_deref().unwrap().contains("substrate"));
        assert!(check_deposit_readiness(tmp.path(), false).is_err());
    }

    // -----------------------------------------------------------------------
    // DR-8 — portability advisory (WARN-only)
    // -----------------------------------------------------------------------

    /// Session id used by the portability fixtures. Its `workflow_id` form is
    /// the hyphen-free `workflow-<32hex>`; its raw (leaked) form is hyphenated.
    const FIX_WORKFLOW_ID: &str = "workflow-805c50a6f70c4565a8e9ec5dad7dff16";
    const FIX_SESSION_UUID: &str = "805c50a6-f70c-4565-a8e9-ec5dad7dff16";

    fn write_workflow_id(root: &Path) {
        let wf = serde_json::json!({ "workflow_id": FIX_WORKFLOW_ID, "tasks": {} });
        fs::write(root.join("WORKFLOW.json"), serde_json::to_vec_pretty(&wf).unwrap()).unwrap();
    }

    /// The scan flags a residual absolute host path and the raw session id,
    /// while EXEMPTING the declared `workflow_id` (the one declared identity
    /// field) and NOT double-counting a session id that only appears inside a
    /// host path.
    #[test]
    fn scan_portability_flags_host_paths_and_session_id_exempting_workflow_id() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_workflow_id(root);
        fs::create_dir_all(root.join("runtime")).unwrap();
        // (1) An external SME data root — a genuine host path (load-bearing).
        fs::write(
            root.join("runtime/inputs.json"),
            r#"[{"root_path":"/home/a/.ecaa-workflow/himes-inputs"}]"#,
        )
        .unwrap();
        // (2) The raw session id embedded as a bare field (decisions.jsonl).
        fs::write(
            root.join("runtime/decisions.jsonl"),
            format!("{{\"session_id\":\"{FIX_SESSION_UUID}\"}}\n"),
        )
        .unwrap();
        // (3) The session id ONLY inside a host path — must count as a host
        //     path, NOT separately as a session-id leak.
        fs::write(
            root.join("runtime/only-in-path.json"),
            format!("{{\"p\":\"/home/a/.ecaa-workflow/packages/{FIX_SESSION_UUID}-bulk\"}}"),
        )
        .unwrap();

        let s = scan_portability(root);
        assert!(!s.is_clean(), "residual host path + session id are non-portable");

        // Host paths surfaced for inputs.json and only-in-path.json.
        assert!(
            s.host_paths.iter().any(|h| h.starts_with("runtime/inputs.json:")
                && h.contains("/home/a/.ecaa-workflow/himes-inputs")),
            "expected the external SME data root as a host path; got {:?}",
            s.host_paths
        );
        assert!(
            s.host_paths.iter().any(|h| h.starts_with("runtime/only-in-path.json:")),
            "expected the packages dir path as a host path; got {:?}",
            s.host_paths
        );

        // Session-id leak surfaced for decisions.jsonl (bare field) ...
        assert!(
            s.session_id_leaks
                .iter()
                .any(|l| l.starts_with("runtime/decisions.jsonl:") && l.contains(FIX_SESSION_UUID)),
            "expected the bare session id in decisions.jsonl; got {:?}",
            s.session_id_leaks
        );
        // ... but NOT for the file where the id only appears inside a host path.
        assert!(
            !s.session_id_leaks
                .iter()
                .any(|l| l.starts_with("runtime/only-in-path.json:")),
            "a session id that only appears inside a host path must not be double-counted; got {:?}",
            s.session_id_leaks
        );
        // ... and NEVER for the declared identity field (WORKFLOW.json uses the
        // hyphen-free workflow-<32hex> form, so the raw UUID never matches).
        assert!(
            !s.session_id_leaks.iter().any(|l| l.starts_with("WORKFLOW.json:")),
            "the declared workflow_id identity must be exempt; got {:?}",
            s.session_id_leaks
        );
    }

    /// A relocatable package (no host paths, no bare session id) scans clean.
    #[test]
    fn scan_portability_clean_on_portable_package() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_workflow_id(root);
        fs::create_dir_all(root.join("runtime")).unwrap();
        fs::write(
            root.join("runtime/execution-order.json"),
            r#"{"order":["data_acquisition","differential_expression"]}"#,
        )
        .unwrap();
        fs::write(root.join("CONTEXT.md"), "# Context\nRelative path: runtime/outputs/x\n").unwrap();
        let s = scan_portability(root);
        assert!(s.is_clean(), "no residuals expected; got {s:?}");
        assert!(s.warnings().is_empty());
    }

    /// A `workflow_id` that is not a `workflow-<uuid>` identity (e.g. a
    /// CLI-built package) disables only the session-id axis; the host-path
    /// axis still fires.
    #[test]
    fn scan_portability_no_session_axis_for_non_uuid_workflow_id() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wf = serde_json::json!({ "workflow_id": "bulk-rnaseq-demo", "tasks": {} });
        fs::write(root.join("WORKFLOW.json"), serde_json::to_vec_pretty(&wf).unwrap()).unwrap();
        fs::write(root.join("CONTEXT.md"), "root: /home/a/data\n").unwrap();
        let s = scan_portability(root);
        assert!(s.session_id_leaks.is_empty(), "no derivable session id → no session axis");
        assert!(
            s.host_paths.iter().any(|h| h.contains("/home/a/data")),
            "host-path axis still applies; got {:?}",
            s.host_paths
        );
    }

    /// The portability advisory is recorded in the attestation but MUST NOT
    /// flip `deposit_ready` false, and MUST NOT make the Layer-3 gate refuse:
    /// a re-executable deposit may need some host paths to replay.
    #[test]
    fn portability_warn_recorded_without_blocking_deposit_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_workflow_id(root);
        fs::create_dir_all(root.join("runtime")).unwrap();
        // A residual host path (as a captured env.lock / inputs.json would have).
        fs::write(
            root.join("runtime/inputs.json"),
            r#"[{"root_path":"/home/a/.ecaa-workflow/himes-inputs"}]"#,
        )
        .unwrap();

        write_deposit_readiness(
            root,
            REEXECUTABLE_PROFILE,
            &tier1(CheckStatus::Pass, CheckStatus::Pass, None),
            ReexecStatus::Partial,
            None,
            None,
            &WallClock,
        )
        .unwrap();

        let dr = read_deposit_readiness(root).unwrap().unwrap();
        assert!(
            !dr.portability_warnings.is_empty(),
            "residual host path must be surfaced as a portability warning"
        );
        assert!(
            dr.portability_warnings.iter().any(|w| w.contains("/home/a/.ecaa-workflow/himes-inputs")),
            "the residual host path should appear in the warnings; got {:?}",
            dr.portability_warnings
        );
        assert!(
            dr.deposit_ready,
            "a portability WARN alone must NOT flip deposit_ready false"
        );
        assert!(
            dr.detail.as_deref().unwrap_or_default().contains("portability warning"),
            "the human detail should note the portability warning; got {:?}",
            dr.detail
        );
        // Layer-3 gate admits it (portability is advisory, not gated).
        assert!(
            check_deposit_readiness(root, false).is_ok(),
            "portability residuals must not make the deposit gate refuse"
        );
    }

    /// A clean deposit records an empty portability advisory.
    #[test]
    fn portability_warn_clear_on_clean_deposit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_workflow_id(root);
        write_deposit_readiness(
            root,
            "full",
            &tier1(CheckStatus::Pass, CheckStatus::Pass, None),
            ReexecStatus::Pass,
            None,
            None,
            &WallClock,
        )
        .unwrap();
        let dr = read_deposit_readiness(root).unwrap().unwrap();
        assert!(dr.portability_warnings.is_empty(), "clean deposit has no portability warnings");
        assert!(dr.deposit_ready);
    }
}
