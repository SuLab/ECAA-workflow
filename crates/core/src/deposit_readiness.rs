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
//! `validate_*` companion task's own self-report may record a failing
//! verdict, which is a SEPARATE axis from computational completion (whether
//! the stage ran and produced output at all). The rollup reads three
//! independent evidence surfaces — each `validate_*` task's self-report
//! verdict (in whichever of the observed FILENAMES and KEY SPELLINGS the agent
//! wrote; see [`VALIDATE_SELF_REPORT_FILES`] and [`task_validation_verdict`]),
//! the package's contract-obligation records in
//! `runtime/validation-reports.jsonl` (see [`scan_contract_obligations`]), and
//! the source-owned reporting-correctness checklist — and reports
//! `CheckStatus::Unverified`, never `Pass`, when NONE of them had anything to
//! inspect. The headline `deposit_ready` bool folds `ro_crate` + `bagit` +
//! `domain_validation` + `provenance_divergence` + `substrate_validity` +
//! `repair_status` + `reexecution` so a run can be computationally complete
//! while `deposit_ready` reads `false`.
//!
//! A gate that cannot read its own evidence is worse than no gate: it attests
//! `pass` over a validator that self-reported FAIL. Every widening in
//! [`task_validation_verdict`] exists because a REAL package on disk spelled
//! its verdict a way this module could not see — the spellings are surveyed,
//! never guessed, and the numeric check-count path
//! (`FAILED_COUNT_KEYS`) is the spelling-proof floor underneath them.
//!
//! Layer 1 also READS the repair loop's own terminal verdict
//! (`repair_status`, via [`scan_repair_status`]) from
//! `runtime/repair-status.json`. A package whose own iterative repair loop
//! concluded `RepairVerdict::Failing` — too many unresolved failures remain —
//! is not deposit-ready by its own account, so a recorded `failing` BLOCKS.
//! `mostly_passing` / `fully_passing` are surfaced but never block.
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
///
/// `Unverified` is the honest third outcome for an axis that INSPECTED
/// NOTHING: it must never be reported as `Pass` (that is the vacuous-gate
/// bug — a check that examined zero evidence claiming a clean bill of
/// health), and it must never be reported as `Fail` (nothing was found
/// wrong). It is surfaced in the attestation and, like
/// `ReexecStatus::NotVerified` outside the `re-executable` profile, does not
/// block `deposit_ready`. Produced by the `domain_validation` axis (nothing
/// recorded a verdict) and by the `repair_status` axis (no repair loop ever
/// ran over this package); `ro_crate` / `bagit` / `provenance_divergence` /
/// `substrate_validity` are always able to reach a concrete verdict.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    #[default]
    Pass,
    Fail,
    /// The axis ran but had no evidence to inspect — neither a pass nor a
    /// failure. Non-blocking, but never silently rendered as `Pass`.
    Unverified,
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
    /// `domain_validation`, `provenance_divergence`, `substrate_validity`, and
    /// `repair_status` all non-`Fail` (with `ro_crate`/`bagit` required to be
    /// an outright `Pass`) AND `reexecution != Fail` (RCA I-10, §G-B2).
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
    /// Flat-layout SHA-512 checksum-seal outcome. The Rust field name remains
    /// for source compatibility; new attestations serialize it as
    /// `checksum_seal`, while `bagit` is accepted only as a legacy alias.
    #[serde(rename = "checksum_seal", alias = "bagit")]
    pub bagit: CheckStatus,
    /// Per-task domain-correctness validation rollup (RCA I-10): `Fail` when
    /// any `validate_*` task's own `result.json` recorded a failing verdict or
    /// the package recorded a `failed:` contract obligation; `Unverified` when
    /// NOTHING reached a verdict (non-blocking, but never silently reported as
    /// `Pass`). An obligation the harness could not run (`errored:`) or has no
    /// checker for (`unimplemented:`) reaches no verdict and never flips this
    /// to `Fail` — see [`DomainValidationSummary::unverified_obligations`] and
    /// [`scan_domain_validation`]. Separate
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
    /// The repair loop's own terminal verdict, READ (never re-run) from
    /// `runtime/repair-status.json` — see [`scan_repair_status`]. `Fail` when
    /// the recorded verdict is `RepairVerdict::Failing`; `Pass` for
    /// `mostly_passing` / `fully_passing`; `Unverified` when no repair status
    /// was ever written (the common case — the iterative repair loop is
    /// operator-triggered, not part of every run) or the record is
    /// unparseable. A concrete `failing` BLOCKS `deposit_ready`: a package
    /// whose OWN repair loop concluded that too many failures remain
    /// unresolved is not deposit-ready by its own account, and the deposit
    /// boundary must not be the one consumer that reads that record as
    /// nothing. `#[serde(default)]` → `Pass` for attestations predating this
    /// field (absence never fabricates a repair failure).
    #[serde(default)]
    pub repair_status: CheckStatus,
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
/// [`check_deposit_readiness`] and, like it, is PROFILE-AWARE: a hard
/// `Fail` on any of the three `CheckStatus` axes — or a `Fail` re-execution
/// — always blocks; and for the `re-executable` profile a `NotVerified`
/// re-execution ALSO blocks (a deposit whose entire contract is replayability
/// must actually have been re-executed). `Partial` — the honest offline
/// outcome — always passes; and `NotVerified` stays admitted for profiles
/// that do not claim re-executability (`full`/`minimal`), where it is a
/// `--strict`-only concern owned by the CLI gate.
///
/// `domain_validation: Unverified` (the axis inspected nothing) is
/// NON-blocking, matching how a `NotVerified` re-execution is admitted for a
/// profile that does not claim the property: absence of evidence is recorded
/// honestly rather than converted into a failure. `ro_crate` and `bagit`
/// never produce `Unverified`, so their `== Pass` test is unchanged.
///
/// This function folds the FIVE arguments it is given. The axes that are
/// scanned from the package tree rather than passed in —
/// `provenance_divergence`, `substrate_validity`, and `repair_status` — are
/// folded by the two call sites ([`write_deposit_readiness`] and
/// [`update_deposit_readiness_reexecution`]) as `&& axis != CheckStatus::Fail`,
/// the same shape used for every non-argument axis since the §G-B2 backstop
/// landed. The signature is deliberately left alone so out-of-crate callers
/// that pass the five headline signals keep compiling.
pub fn compute_deposit_ready(
    profile: &str,
    ro_crate: CheckStatus,
    bagit: CheckStatus,
    domain_validation: CheckStatus,
    reexecution: ReexecStatus,
) -> bool {
    let reexec_blocks = reexecution == ReexecStatus::Fail
        || (profile_claims_reexecutability(profile) && reexecution == ReexecStatus::NotVerified);
    ro_crate == CheckStatus::Pass
        && bagit == CheckStatus::Pass
        && domain_validation != CheckStatus::Fail
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
/// self-report in any of [`VALIDATE_SELF_REPORT_FILES`] under
/// `runtime/outputs/validate_<stage>/` (a pass/fail verdict in one of the
/// surveyed spellings and/or numeric check counts, plus on failure a
/// `required_failures` / `failed_checks` array naming the failed assertions) —
/// this is a DIFFERENT signal than whether the stage ran at all
/// (computational completion): the deposited `611cf5ee` package had
/// `validate_differential_expression/result.json` recording
/// `validation_passed: false` while every top-level summary layer
/// (`DEPOSIT-READINESS.json`, RO-Crate, BagIt) read as passing, because
/// nothing rolled the per-task self-report up into the deposit-level
/// attestation. The `eda58089` deposit was the same bug one layer down: the
/// rollup EXISTED but could not read the spelling
/// (`verdict: "FAIL 130/135 checks (5 failed)"`) the validator had used.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DomainValidationSummary {
    /// Task ids that recorded a RECOGNIZED verdict, in scan order:
    /// `validate_*` tasks whose self-reports yielded a verdict via
    /// `validate_task_verdict`, then the synthetic
    /// [`CONTRACT_OBLIGATIONS_TASK_ID`] when the package carries contract
    /// obligation records, then the synthetic `reporting_invariants` id when
    /// that checklist ran. EMPTY means the axis inspected nothing — see
    /// [`DomainValidationSummary::status`].
    pub checked_tasks: Vec<String>,
    /// The subset of [`Self::checked_tasks`] whose recorded verdict was a
    /// failure, in scan order.
    pub failed_tasks: Vec<String>,
    /// `"<task_id>: <assertion_id>"` for every named failure a failed task's
    /// self-reports carry (the sorted, de-duplicated union over
    /// `REQUIRED_FAILURE_KEYS` across those files — see
    /// `named_required_failures`), in task scan order. Also carries the
    /// source-owned reporting-correctness validator's REQUIRED failures
    /// (RP-2/RP-4/RP-5) under the synthetic `reporting_invariants` task id
    /// (see [`crate::reporting_invariants`]) and the contract-obligation
    /// failures under the synthetic [`CONTRACT_OBLIGATIONS_TASK_ID`] (see
    /// [`scan_contract_obligations`]).
    pub required_failures: Vec<String>,
    /// Advisory (non-blocking) reporting-correctness warnings
    /// (RP-1/RP-3/RP-9). Surfaced for operator visibility but never folded
    /// into [`Self::passed`] — they must not block a scientifically-correct
    /// deposit. `#[serde(default)]` so an older serialized summary
    /// deserializes cleanly.
    #[serde(default)]
    pub reporting_warnings: Vec<String>,
    /// `"<task_id>.<obligation_id> (<outcome>)"` for every contract obligation
    /// that reached NO verdict — the harness recorded it `errored:` (the
    /// checker could not run: missing input, parse error) or `unimplemented:`
    /// (no checker is registered for it). Sorted + de-duplicated.
    ///
    /// Neither a pass nor a failure, so these are surfaced for operator
    /// visibility and NEVER folded into [`Self::failed_tasks`] /
    /// [`Self::required_failures`]. The harness itself does not treat this
    /// class as a failure: `ValidatorOutcome::Errored` is documented as a
    /// soft-skip and `ValidationReportSummary::has_failures()` matches only
    /// `Failed`, so the task stays `Completed`. An obligation that could not
    /// reach a verdict over the package's own artifacts is a gap in the
    /// validator suite (or in this package's optional inputs), not a defect in
    /// its science — the same rationale that already exempts
    /// `unimplemented:`. `#[serde(default)]` so an older serialized summary
    /// deserializes cleanly.
    #[serde(default)]
    pub unverified_obligations: Vec<String>,
}

impl DomainValidationSummary {
    /// `true` iff no `validate_*` task self-reported a domain-correctness
    /// failure. Vacuously `true` when no task recorded a self-report at all
    /// — absence of a check is not itself a failure. Use [`Self::status`]
    /// when the caller needs to distinguish "nothing failed" from "nothing
    /// was inspected".
    pub fn passed(&self) -> bool {
        self.failed_tasks.is_empty()
    }

    /// The three-way attestation axis for this rollup.
    ///
    /// * `Fail` — at least one recorded verdict was a failure.
    /// * `Unverified` — NOTHING was inspected: no `validate_*` task recorded
    ///   a verdict [`task_validation_verdict`] recognizes, the package
    ///   carries no contract-obligation records that reached a CONCRETE
    ///   verdict, and the reporting-correctness checklist found none of its
    ///   inputs. Reporting this as `Pass` is the vacuous-gate bug this axis
    ///   exists to avoid. A package whose obligation records are ENTIRELY
    ///   [`Self::unverified_obligations`] (every one `errored:` /
    ///   `unimplemented:`) lands here rather than on `Pass`, because
    ///   [`scan_domain_validation`] only counts the obligation axis as
    ///   inspected once some obligation reached a pass or a failure.
    /// * `Pass` — at least one check ran to a concrete verdict and none
    ///   failed. Obligations that reached no verdict do not detract from a
    ///   sibling obligation's genuine pass.
    pub fn status(&self) -> CheckStatus {
        if !self.failed_tasks.is_empty() {
            CheckStatus::Fail
        } else if self.checked_tasks.is_empty() {
            CheckStatus::Unverified
        } else {
            CheckStatus::Pass
        }
    }
}

/// Synthetic task id the contract-obligation rollup
/// ([`scan_contract_obligations`]) is surfaced under, so a failing obligation
/// is attributable in [`DomainValidationSummary::failed_tasks`] without
/// colliding with a real `validate_*` task id.
pub const CONTRACT_OBLIGATIONS_TASK_ID: &str = "contract_obligations";

/// Package-relative path of the ECAA E-subgraph execution-validation sidecar
/// the harness appends one record to per checked obligation.
const VALIDATION_REPORTS_SIDECAR: &str = "validation-reports.jsonl";

/// Per-task report basenames a `validate_*` task writes its own
/// domain-correctness self-report into, in read order.
///
/// Agent-authored validators do not converge on one FILENAME any more than
/// they converge on one key spelling. A survey of the on-disk package corpus
/// found `result.json` (85 files), `validation_report.json` (65) and
/// `validation_results.json` — PLURAL, a different file from the singular one
/// — all carrying a top-level verdict, and 12 `validate_*` tasks whose
/// `result.json` carries NO verdict while its sibling `validation_report.json`
/// does. Reading only `result.json` therefore skipped tasks that had in fact
/// reported. All three are read and their verdicts folded FAIL-DOMINANTLY (see
/// `validate_task_verdict`).
pub const VALIDATE_SELF_REPORT_FILES: [&str; 3] = [
    "result.json",
    "validation_report.json",
    "validation_results.json",
];

/// Keys whose BOOLEAN value is a task's domain-correctness verdict, in
/// contract order. `validation_passed` is the canonical spelling the
/// deposit rollup was originally written against; `passed` / `pass` are the
/// empirically-observed tail. Selection is by `.as_bool()`, so a report that
/// spells `passed` as a check COUNT (`"passed": 42`) is not mis-read here — it
/// is read by the numeric path ([`PASSED_COUNT_KEYS`]) instead.
const VERDICT_BOOL_KEYS: [&str; 5] = [
    "validation_passed",
    "overall_pass",
    "all_pass",
    "passed",
    "pass",
];

/// Keys whose STRING value is a task's domain-correctness verdict.
///
/// Agent-authored `validate_*` self-reports do NOT converge on one spelling. A
/// survey of the on-disk package corpus found, by descending frequency:
/// `overall` (32), `overall_status` (13), `overall_validation_status` (12),
/// `validation_overall` (11), `verdict` (6), `validation_outcome` (6),
/// `validation_result` (5), `validation_status` (5), `outcome` (3) — plus the
/// booleans above, with `validation_passed` a MINORITY spelling. Reading only
/// a subset of these made the whole axis vacuous on real packages: on one
/// real 8-`validate_*`-task deposit, ZERO tasks used a recognized key, so the
/// gate attested `domain_validation: pass` over a validator whose own report
/// said `FAIL 130/135 checks (5 failed)`.
///
/// Order: the first five entries are the original contract order, preserved;
/// the last four are the later-surveyed tail. Since the fold is FAIL-DOMINANT
/// (see [`task_validation_verdict`]) the order no longer decides the verdict —
/// it only fixes a deterministic scan sequence.
///
/// `status` is deliberately EXCLUDED. It is the TASK-LIFECYCLE field
/// (`"completed"` on 85 of 90 occurrences) and is legitimately independent of
/// the domain verdict: a `validate_*` task that crashed reached NO domain
/// verdict, which is `Unverified`, not `Fail`. The 5 corpus files that do put a
/// verdict under `status` are each redundantly covered — 4 by their numeric
/// check counts or a sibling report, and the 1 `status: "failed"` by its own
/// `verdict` string AND `checks_failed: 5`.
const VERDICT_STRING_KEYS: [&str; 9] = [
    "validation_result",
    "overall",
    "overall_validation_status",
    "validation_status",
    "validation_outcome",
    "overall_status",
    "validation_overall",
    "verdict",
    "outcome",
];

/// Keys whose NUMERIC value counts a task's FAILED checks. Surveyed spellings,
/// by descending on-disk frequency: `n_fail` (85), `checks_failed` (15),
/// `n_failed` (13), `failed` (9), `n_checks_fail` (3), `n_checks_failed` (3),
/// `n_required_fail` (4), `checks_fail` (2), `n_blocking_fail` (1),
/// `failed_checks` (1, when written as a count rather than an array).
/// (`failed_checks_count` was NOT found on disk and is not invented here.)
///
/// Deliberately EXCLUDED, because they are not failed REQUIRED checks:
/// `n_advisory_fail` (advisory by name), `n_warn` / `n_warnings` /
/// `checks_warn` / `warnings` / `warned` (warnings never block — the same
/// policy [`DomainValidationSummary::reporting_warnings`] applies),
/// `errors` / `n_errors` (a checker that ERRORED reached no verdict — the same
/// policy [`ObligationClass::Unverified`] applies to `errored:`), and
/// `n_skip` / `n_skipped` (a gated-out check is not a failed one).
const FAILED_COUNT_KEYS: [&str; 10] = [
    "n_fail",
    "n_failed",
    "checks_failed",
    "checks_fail",
    "n_checks_fail",
    "n_checks_failed",
    "failed",
    "failed_checks",
    "n_required_fail",
    "n_blocking_fail",
];

/// Keys whose NUMERIC value counts a task's PASSED checks, surveyed the same
/// way. Used ONLY to distinguish "zero failures because checks ran and all
/// passed" from "zero failures because nothing ran" — a zero failure count on
/// its own is not evidence of a pass.
const PASSED_COUNT_KEYS: [&str; 8] = [
    "n_pass",
    "n_passed",
    "checks_passed",
    "checks_pass",
    "n_checks_pass",
    "n_checks_passed",
    "passed",
    "passed_checks",
];

/// Keys carrying an array of named required-check failures on a failing
/// self-report. Every one present is unioned (not just the first), so a report
/// that leaves `required_failures` empty while naming its failures under
/// `failed_checks` still reaches the attestation detail.
const REQUIRED_FAILURE_KEYS: [&str; 3] = ["required_failures", "failed_checks", "failures"];

/// Fields, in order, an OBJECT-shaped entry in a [`REQUIRED_FAILURE_KEYS`]
/// array names itself with. Real validators write both shapes: a bare string
/// (`["contract.assertion_a"]`) and an object — the observed object shape is
/// `{id, category, description, passed, expected, observed}`. Without this, a
/// failing report whose `failed_checks` holds objects yielded an EMPTY name
/// list and the `Fail` axis reached the attestation with no explanation of
/// what failed.
const FAILURE_OBJECT_ID_KEYS: [&str; 5] = ["id", "check", "assertion", "name", "description"];

/// Map ONE whitespace-delimited token onto a pass/fail bool. Case-insensitive
/// and tolerant of edge punctuation (`PASS:` / `(FAIL)`), but never of interior
/// punctuation — `n/a` must stay unrecognized, so `/` is not stripped.
fn verdict_token(token: &str) -> Option<bool> {
    match token
        .trim_matches(|c: char| c.is_ascii_punctuation() && c != '/')
        .to_ascii_lowercase()
        .as_str()
    {
        "pass" | "passed" => Some(true),
        "fail" | "failed" | "error" | "errored" => Some(false),
        _ => None,
    }
}

/// Map a verdict STRING onto a pass/fail bool. `None` for a value that is not
/// a recognized verdict (so a key like `overall` carrying unrelated prose
/// yields no verdict rather than fabricating one).
///
/// Two accepted forms:
///
/// 1. the whole trimmed value is a bare verdict token (`"PASS"`, `"failed"`) —
///    the original contract;
/// 2. a SUMMARY string whose FIRST whitespace-delimited token is a verdict
///    token: `"FAIL 130/135 checks (5 failed)"` → `Some(false)`,
///    `"PASS 42/42 checks"` → `Some(true)`.
///
/// Form 2 exists because requiring the WHOLE string to be a bare token is the
/// second half of the real defect this module had: `validate_reporting`
/// recorded `verdict: "FAIL 130/135 checks (5 failed)"` and the gate resolved
/// it to `None` — no verdict — even though it starts with FAIL.
///
/// Anchoring on the FIRST token is what keeps this from over-matching: a value
/// that merely MENTIONS a verdict word later on (`"validation was
/// inconclusive; 2 checks fail to apply"`) still yields `None`, and so do the
/// real non-verdict values under these keys (`"n/a"`, `"ok"`). Compound
/// verdicts that are not whitespace-delimited (the observed
/// `"PASS-WITH-WARN"`) are deliberately NOT split further — hyphen-splitting
/// would read `"error-free"` as a failure. Those are covered by the numeric
/// path instead ([`numeric_check_verdict`]), which is exactly why the numeric
/// path exists.
fn verdict_from_str(raw: &str) -> Option<bool> {
    let trimmed = raw.trim();
    if let Some(v) = verdict_token(trimmed) {
        return Some(v);
    }
    verdict_token(trimmed.split_whitespace().next()?)
}

/// Read a non-negative integer check count out of `v[key]`.
///
/// `serde_json` yields `None` from `as_u64()` for a `Bool`, so a boolean
/// verdict spelling can never be mis-read as a count. Floats are accepted
/// (some validators write `0.0`) and clamped at zero.
fn json_count(v: &serde_json::Value, key: &str) -> Option<u64> {
    let n = v.get(key)?;
    n.as_u64()
        .or_else(|| n.as_f64().map(|f| if f > 0.0 { f as u64 } else { 0 }))
}

/// The GREATEST count recorded under any of `keys`, or `None` when the document
/// records none of them. Max (not first-present) so a document carrying two
/// spellings of the failure count cannot hide a positive one behind a zero.
fn max_count(v: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    keys.iter().filter_map(|k| json_count(v, *k)).max()
}

/// A task's verdict as derived from its NUMERIC check counts alone.
///
/// * a failure count > 0 → `Some(false)`;
/// * a failure count == 0 alongside a positive PASSED count → `Some(true)`;
/// * anything else (no counts, or zero failures with nothing recorded as
///   having passed) → `None`, so the string/bool keys still get their say.
///
/// This is the spelling-proof floor under the verdict keys, and
/// [`task_validation_verdict`] gives a positive failure count precedence over
/// every string: a number cannot be phrased three ways. `checks_failed: 5` has
/// exactly one reading, whereas the accompanying prose has been observed as
/// `FAIL`, `fail`, `"FAIL 130/135 checks (5 failed)"`, `PASS-WITH-WARN`, and
/// under nine different key names — every one of which is a chance for the
/// gate to see nothing. A count also cannot be defeated by a NEW spelling
/// nobody has surveyed yet, which is the failure mode that made this axis
/// vacuous in the first place.
///
/// The asymmetry is deliberate: a positive failure count is authoritative over
/// everything, but a ZERO failure count never overrides an explicit failing
/// bool/string. Numeric evidence is used to STRENGTHEN the gate, never to
/// weaken it.
fn numeric_check_verdict(v: &serde_json::Value) -> Option<bool> {
    match max_count(v, &FAILED_COUNT_KEYS)? {
        0 => match max_count(v, &PASSED_COUNT_KEYS) {
            Some(p) if p > 0 => Some(true),
            _ => None,
        },
        _ => Some(false),
    }
}

/// Extract a `validate_*` task's self-reported domain-correctness verdict from
/// ONE parsed self-report document (see [`VALIDATE_SELF_REPORT_FILES`] for the
/// files that carry one).
///
/// Returns `Some(true)` / `Some(false)` for a recognized verdict and `None`
/// when the document records no recognized verdict at all — the latter is a
/// task that must be SKIPPED (most `validate_*` companions are pure
/// artifact-presence checks and record no domain verdict), never a failure. A
/// recognized key whose value is not a recognized verdict token is likewise
/// skipped, so unrelated prose under a generic key (`"outcome": "ok"`) cannot
/// fabricate a verdict.
///
/// FAIL-DOMINANT across the three evidence classes it reads (numeric check
/// counts, boolean keys, string keys): any one of them recording a failure
/// makes the document's verdict a failure. A report that contradicts itself
/// (`validation_passed: true` beside `checks_failed: 5`) is a defective report,
/// and the safe reading of a defective validator report at a deposit boundary
/// is the failing one.
pub fn task_validation_verdict(v: &serde_json::Value) -> Option<bool> {
    let mut saw_pass = false;
    // Numeric counts first — see `numeric_check_verdict` for why a positive
    // failure count outranks any string.
    match numeric_check_verdict(v) {
        Some(false) => return Some(false),
        Some(true) => saw_pass = true,
        None => {}
    }
    for key in VERDICT_BOOL_KEYS {
        if let Some(b) = v.get(key).and_then(|x| x.as_bool()) {
            if !b {
                return Some(false);
            }
            saw_pass = true;
        }
    }
    for key in VERDICT_STRING_KEYS {
        if let Some(verdict) = v
            .get(key)
            .and_then(|x| x.as_str())
            .and_then(verdict_from_str)
        {
            if !verdict {
                return Some(false);
            }
            saw_pass = true;
        }
    }
    saw_pass.then_some(true)
}

/// Parse every self-report [`VALIDATE_SELF_REPORT_FILES`] file present in one
/// `validate_*` task's output directory, in that fixed order. A missing or
/// unparseable file is skipped individually — one malformed sibling must not
/// hide a verdict the others recorded.
fn validate_task_self_reports(task_dir: &Path) -> Vec<serde_json::Value> {
    VALIDATE_SELF_REPORT_FILES
        .iter()
        .filter_map(|name| std::fs::read_to_string(task_dir.join(name)).ok())
        .filter_map(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .collect()
}

/// Fold one task's self-report documents into a single verdict.
///
/// FAIL-DOMINANT: if the files DISAGREE, the failing one wins. The two are not
/// symmetric pieces of evidence — a validator that recorded a failure anywhere
/// found a real defect, whereas a sibling document reporting a pass only means
/// that document did not record that defect (it may summarize a different
/// check set, or predate the failing write). Letting a passing sibling cancel a
/// recorded failure would reintroduce exactly the vacuity this scan exists to
/// prevent. `None` only when NO document recorded a verdict.
fn validate_task_verdict(reports: &[serde_json::Value]) -> Option<bool> {
    let mut saw_pass = false;
    for report in reports {
        match task_validation_verdict(report) {
            Some(false) => return Some(false),
            Some(true) => saw_pass = true,
            None => {}
        }
    }
    saw_pass.then_some(true)
}

/// Union the named required-check failures across one task's self-report
/// documents, sorted + de-duplicated (the two documents commonly carry the
/// same list). Handles both observed element shapes: a bare string, and an
/// object naming itself under one of [`FAILURE_OBJECT_ID_KEYS`].
fn named_required_failures(reports: &[serde_json::Value]) -> Vec<String> {
    let mut out: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for report in reports {
        for key in REQUIRED_FAILURE_KEYS {
            let Some(arr) = report.get(key).and_then(|x| x.as_array()) else {
                continue;
            };
            for entry in arr {
                if let Some(s) = entry.as_str() {
                    out.insert(s.to_string());
                    continue;
                }
                if let Some(label) = FAILURE_OBJECT_ID_KEYS
                    .iter()
                    .find_map(|k| entry.get(*k).and_then(|x| x.as_str()))
                {
                    out.insert(label.to_string());
                }
            }
        }
    }
    out.into_iter().collect()
}

/// Read + de-duplicate the contract-obligation records in
/// `runtime/validation-reports.jsonl`.
///
/// An obligation declared on more than one of a task's required artifacts is
/// recorded once per artifact, so the same `(task_id, obligation_id, outcome)`
/// triple can repeat — the same de-duplication
/// `emitter::audit_report` applies. A missing/unreadable sidecar yields an
/// empty `Vec`; unparseable lines are skipped individually.
fn contract_obligation_rows(package_root: &Path) -> Vec<serde_json::Value> {
    let path = package_root
        .join("runtime")
        .join(VALIDATION_REPORTS_SIDECAR);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut seen: std::collections::BTreeSet<(String, String, String)> =
        std::collections::BTreeSet::new();
    let mut rows = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let key = (field("task_id"), field("obligation_id"), field("outcome"));
        if seen.insert(key) {
            rows.push(v);
        }
    }
    rows
}

/// How one contract-obligation record classifies for the deposit boundary.
///
/// The harness serializes four `outcome` forms
/// (`crates/harness/src/validators.rs::ValidationReportSummary::to_jsonl`):
/// `passed`, `failed:…`, `errored:…`, `unimplemented:…`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObligationClass {
    /// `failed:` — the obligation RAN and its assertion did not hold. The only
    /// class that blocks the deposit, matching the harness gate exactly
    /// (`ValidationReportSummary::has_failures()` matches only
    /// `ValidatorOutcome::Failed`).
    Failed,
    /// `errored:` — the checker could not run (missing input, parse error) — or
    /// `unimplemented:` — no checker is registered. NO verdict was reached, so
    /// this is neither a pass nor a failure: surfaced in
    /// [`DomainValidationSummary::unverified_obligations`], never blocking.
    Unverified,
    /// `passed`, or any spelling this gate does not recognize: a concrete
    /// verdict that is not a failure.
    Passed,
}

/// Classify one obligation record, paired with its
/// `"<task_id>.<obligation_id> (<outcome>)"` attribution label.
///
/// `None` for a malformed record carrying no `outcome` field at all — it
/// records no verdict, so it neither blocks nor counts as evidence that
/// something was inspected.
///
/// `errored:` is deliberately NOT a required failure. The harness treats
/// `ValidatorOutcome::Errored` as a soft-skip ("validator could not run …
/// treated as soft-skip rather than hard fail") and leaves the task
/// `Completed`; folding it in here made the deposit gate the one consumer that
/// disagreed, blocking a package over, e.g., a missing OPTIONAL independent
/// annotation table while the science it checked was independently sound. The
/// `unimplemented:` rationale — "a gap in the validator suite, not a defect in
/// this package's science" — applies verbatim. A validator that must fail
/// closed on a missing input reports `Failed` instead (as
/// `variant_af_spectrum` does when its measurement input is absent), so the
/// fail-closed choice stays owned by the validator, not re-litigated here.
fn classify_obligation(v: &serde_json::Value) -> Option<(ObligationClass, String)> {
    let outcome = v.get("outcome").and_then(|x| x.as_str())?.trim();
    let lower = outcome.to_ascii_lowercase();
    let class = if lower.starts_with("failed:") || lower == "failed" {
        ObligationClass::Failed
    } else if lower.starts_with("errored:")
        || lower == "errored"
        || lower.starts_with("unimplemented:")
        || lower == "unimplemented"
    {
        ObligationClass::Unverified
    } else {
        ObligationClass::Passed
    };
    let task = v
        .get("task_id")
        .and_then(|x| x.as_str())
        .unwrap_or("<unknown task>");
    let obligation = v
        .get("obligation_id")
        .and_then(|x| x.as_str())
        .unwrap_or("<unknown obligation>");
    Some((class, format!("{task}.{obligation} ({outcome})")))
}

/// `"<task_id>.<obligation_id> (<outcome>)"` when this obligation record is a
/// REQUIRED failure (`failed:` / bare `failed`), else `None`.
fn obligation_required_failure(v: &serde_json::Value) -> Option<String> {
    match classify_obligation(v)? {
        (ObligationClass::Failed, label) => Some(label),
        _ => None,
    }
}

/// Roll the package's contract-obligation records
/// (`runtime/validation-reports.jsonl`) up into the REQUIRED failures they
/// declare, sorted + de-duplicated.
///
/// These are the harness-run `policies/validation-contract.json` obligations —
/// a per-artifact evidence surface that the deposit rollup previously never
/// read at all, so an obligation the package itself recorded as `failed:`
/// could not block the deposit boundary. Returns an empty `Vec` for a package
/// with no sidecar, no records, or only passing / no-verdict (`errored:` /
/// `unimplemented:`) obligations — the no-verdict class is reported separately
/// in [`DomainValidationSummary::unverified_obligations`].
pub fn scan_contract_obligations(package_root: &Path) -> Vec<String> {
    let mut out: Vec<String> = contract_obligation_rows(package_root)
        .iter()
        .filter_map(obligation_required_failure)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Scan every `validate_*` task's self-reports under
/// `runtime/outputs/<task_id>/` — each of [`VALIDATE_SELF_REPORT_FILES`] — for
/// a self-reported domain-correctness verdict (via [`task_validation_verdict`],
/// which accepts every key spelling the agent-authored reports actually use,
/// and folds them fail-dominantly per `validate_task_verdict`), fold in the
/// package's contract-obligation records (`runtime/validation-reports.jsonl`,
/// via [`scan_contract_obligations`]) and the source-owned
/// reporting-correctness checklist, and roll the whole lot into one
/// [`DomainValidationSummary`] (RCA I-10).
///
/// A task whose self-report files are all missing, unparseable, or carry no
/// recognized verdict is silently skipped — not every `validate_*` companion
/// emits a domain-correctness self-report (most are pure artifact-presence
/// checks), and absence must never read as a failure. When NOTHING at all was
/// inspected, [`DomainValidationSummary::status`] reports `Unverified` rather
/// than `Pass`.
///
/// Deterministic: task directories are visited in sorted order and the
/// obligation rollup is sorted + de-duplicated.
pub fn scan_domain_validation(package_root: &Path) -> DomainValidationSummary {
    let mut summary = DomainValidationSummary::default();
    let outputs_dir = package_root.join("runtime").join("outputs");
    if let Ok(entries) = std::fs::read_dir(&outputs_dir) {
        let mut task_ids: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| name.starts_with("validate_"))
            .collect();
        task_ids.sort();
        for task_id in task_ids {
            let reports = validate_task_self_reports(&outputs_dir.join(&task_id));
            let Some(passed) = validate_task_verdict(&reports) else {
                continue;
            };
            summary.checked_tasks.push(task_id.clone());
            if !passed {
                summary.failed_tasks.push(task_id.clone());
                let named = named_required_failures(&reports);
                if named.is_empty() {
                    // A failing self-report that names no assertion still has
                    // to reach the attestation detail — otherwise the axis
                    // reads `Fail` with no explanation of what failed.
                    summary
                        .required_failures
                        .push(format!("{task_id}: recorded a failing validation verdict"));
                } else {
                    for s in named {
                        summary.required_failures.push(format!("{task_id}: {s}"));
                    }
                }
            }
        }
    }

    // Fold in the package's own contract-obligation records: the harness
    // appends one `{task_id, obligation_id, outcome}` per checked obligation to
    // `runtime/validation-reports.jsonl`, and a `failed:` outcome there is a
    // domain-correctness failure the deposit boundary must see.
    //
    // `errored:` / `unimplemented:` reached NO verdict and are collected into
    // `unverified_obligations` instead of the required failures — see
    // `classify_obligation` for why an `errored:` must not block. Only a
    // CONCRETE verdict (a pass or a failure) is evidence that something was
    // inspected, so an obligation set that is ENTIRELY no-verdict does not lift
    // the rollup out of `Unverified` — recording it as `Pass` would be the same
    // vacuity bug the axis exists to avoid.
    let obligation_rows = contract_obligation_rows(package_root);
    let mut fails: Vec<String> = Vec::new();
    let mut unverified: Vec<String> = Vec::new();
    let mut any_concrete_verdict = false;
    for row in &obligation_rows {
        match classify_obligation(row) {
            Some((ObligationClass::Failed, label)) => {
                any_concrete_verdict = true;
                fails.push(label);
            }
            Some((ObligationClass::Unverified, label)) => unverified.push(label),
            Some((ObligationClass::Passed, _)) => any_concrete_verdict = true,
            None => {}
        }
    }
    fails.sort();
    fails.dedup();
    unverified.sort();
    unverified.dedup();
    if any_concrete_verdict {
        summary
            .checked_tasks
            .push(CONTRACT_OBLIGATIONS_TASK_ID.to_string());
    }
    if !fails.is_empty() {
        summary
            .failed_tasks
            .push(CONTRACT_OBLIGATIONS_TASK_ID.to_string());
        for f in fails {
            summary
                .required_failures
                .push(format!("{CONTRACT_OBLIGATIONS_TASK_ID}: {f}"));
        }
    }
    summary.unverified_obligations = unverified;

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
        summary
            .checked_tasks
            .push("reporting_invariants".to_string());
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
                    if let Some(arr) = root.get(RO_CRATE_DIVERGENCE_KEY).and_then(|v| v.as_array())
                    {
                        for d in arr {
                            // Newer crates reference each divergence by `@id` (a
                            // flattened `@graph` node — RO-Crate/runcrate rejects
                            // an inline value object with no `@id`); older crates
                            // inlined the object. Resolve a bare `@id` reference
                            // to its flattened node so task_id/read_path are read
                            // from whichever carries them; fall back to `d` for
                            // the legacy inline shape.
                            let node = match (
                                d.get("read_path").is_some(),
                                d.get("@id").and_then(|v| v.as_str()),
                            ) {
                                (false, Some(id)) => graph
                                    .iter()
                                    .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some(id))
                                    .unwrap_or(d),
                                _ => d,
                            };
                            let task_id =
                                node.get("task_id").and_then(|v| v.as_str()).unwrap_or("?");
                            let read_path = node
                                .get("read_path")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
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

/// Package-relative path of the repair loop's terminal status record
/// (`crate::repair_loop::status::RepairStatus::persist`).
const REPAIR_STATUS_SIDECAR: &str = "repair-status.json";

/// Serialized `crate::repair_loop::status::RepairVerdict` spellings
/// (`#[serde(rename_all = "snake_case")]`). Matched as strings rather than
/// deserialized into `RepairStatus` because the `review` array is unbounded
/// (one real record carries 3814 entries) and a strict typed parse of every
/// `Failure` in it would make an unrelated field drift in that payload silently
/// erase the verdict this axis exists to read.
const REPAIR_VERDICT_FAILING: &str = "failing";
const REPAIR_VERDICT_PASSING: [&str; 2] = ["fully_passing", "mostly_passing"];

/// The repair loop's own terminal verdict, read back from
/// `runtime/repair-status.json` — see [`scan_repair_status`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepairStatusSummary {
    /// The recorded `verdict` string, `None` when no parseable record exists.
    pub verdict: Option<String>,
    /// How many unresolved failures the record routed to human review
    /// (`review.len()`), for the attestation detail.
    pub unresolved: usize,
}

impl RepairStatusSummary {
    /// The attestation axis for this rollup.
    ///
    /// * `Fail` — a recorded `failing` verdict.
    /// * `Pass` — a recorded `fully_passing` / `mostly_passing` verdict.
    /// * `Unverified` — no repair status was written at all (the iterative
    ///   repair loop is operator-triggered, and only 10 of 322 local packages
    ///   carry the record), the record is unparseable, or it carries a verdict
    ///   spelling this gate does not know. Nothing was inspected, so this is
    ///   neither a pass nor a failure and does not block — the same policy the
    ///   `domain_validation` axis applies.
    pub fn status(&self) -> CheckStatus {
        match self.verdict.as_deref() {
            Some(v) if v.eq_ignore_ascii_case(REPAIR_VERDICT_FAILING) => CheckStatus::Fail,
            Some(v)
                if REPAIR_VERDICT_PASSING
                    .iter()
                    .any(|p| v.eq_ignore_ascii_case(p)) =>
            {
                CheckStatus::Pass
            }
            _ => CheckStatus::Unverified,
        }
    }
}

/// Read (never re-run) the repair loop's terminal verdict from
/// `runtime/repair-status.json`.
///
/// This record was written by the package's own iterative repair loop and, until
/// now, was read by the emitter's audit report, the readability pass, the repair
/// loop itself and `export` — but by NOTHING at the deposit boundary. The
/// `eda58089` deposit recorded `verdict: "failing"` with 3814 unresolved review
/// items and still attested `deposit_ready: true`.
///
/// A recorded `failing` BLOCKS the deposit (see
/// [`RepairStatusSummary::status`]). The justification is that `Failing` is not
/// an advisory tick-box and is not the routine outcome: `RepairVerdict` is
/// three-valued, and `from_final` only reaches `Failing` when the unresolved
/// count EXCEEDS the loop's own configured `failing_threshold` — the loop's own
/// statement that the package is out of tolerance. A survey of the local
/// package corpus bears that out: of the 10 packages carrying a repair status,
/// 9 recorded `mostly_passing` (4–20 unresolved items) and exactly 1 recorded
/// `failing` (3814). So blocking on `failing` refuses the one package whose own
/// repair loop disowned it while leaving every healthy package untouched — the
/// discrimination a gate needs. `mostly_passing` is explicitly documented as "a
/// tolerable number of unresolved failures remain" and is therefore surfaced
/// (its unresolved count reaches the attestation detail) but never blocking.
///
/// Best-effort and non-fabricating, mirroring [`scan_substrate_validity`]: a
/// missing file (the overwhelmingly common case — the repair loop is
/// operator-triggered), an unparseable one, or one carrying no `verdict` all
/// yield `Unverified`, which does not block.
pub fn scan_repair_status(package_root: &Path) -> RepairStatusSummary {
    let path = package_root.join("runtime").join(REPAIR_STATUS_SIDECAR);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return RepairStatusSummary::default();
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return RepairStatusSummary::default();
    };
    RepairStatusSummary {
        verdict: doc
            .get("verdict")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string()),
        unresolved: doc
            .get("review")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
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
    "jpg", "jpeg", "gif", "bmp", "ico", "parquet", "feather", "arrow", "h5", "hdf5", "h5ad",
    "loom", "bam", "sam", "cram", "bai", "crai", "npz", "npy", "pyc", "pyo", "whl", "rds", "rdata",
    "rda", "bin", "pkl", "pickle", "db", "sqlite", "woff", "woff2", "ttf", "eot", "mp4", "mov",
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
                b'"' | b'\''
                    | b'`'
                    | b','
                    | b'('
                    | b')'
                    | b'['
                    | b']'
                    | b'{'
                    | b'}'
                    | b'<'
                    | b'>'
                    | b';'
                    | b':'
                    | b'\\'
                    | b'|'
                    | b'*'
                    | b'?'
                    | b'='
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

/// The absolute package-root prefixes this deposit's recorded artifacts embed —
/// read from every task's `determinism-env.json` `pkg_root` field (the
/// authoritative record of where the package lived at execution time). A host
/// path AT or UNDER one of these is a SELF-REFERENCE: it points into the
/// deposit's own tree, which relocates WITH the package on deposit, so it is
/// NOT external-machine pinning the way a conda prefix, a mount, or a resolved
/// `.so` path is. Returned sorted+deduped; empty when no `determinism-env.json`
/// records a `pkg_root`, in which case the scan degrades to reporting every
/// host path (an honest over-report, never a miss).
fn recorded_self_reference_roots(package_root: &Path) -> Vec<String> {
    let mut roots: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(package_root.join("runtime/outputs")) {
        for e in entries.filter_map(|e| e.ok()) {
            let Ok(raw) = std::fs::read_to_string(e.path().join("determinism-env.json")) else {
                continue;
            };
            let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
                continue;
            };
            if let Some(root) = val.get("pkg_root").and_then(|v| v.as_str()) {
                let root = root.trim_end_matches('/');
                if !root.is_empty() && HOST_PATH_ROOTS.iter().any(|r| root.starts_with(r)) {
                    roots.insert(root.to_string());
                }
            }
        }
    }
    roots.into_iter().collect()
}

/// Whether `path` is a self-reference into one of the deposit's own recorded
/// roots (the exact root itself or a child under it).
fn is_self_reference(path: &str, self_roots: &[String]) -> bool {
    self_roots
        .iter()
        .any(|root| path == root || path.starts_with(&format!("{root}/")))
}

/// DR-8 portability scan over a sealed deposit (or an emitted package root).
///
/// Walks every TEXT artifact (skipping binary blobs by extension, files over
/// [`PORTABILITY_MAX_FILE_BYTES`], and the manifest-excluded
/// `DEPOSIT-READINESS.json` itself) and collects two residual signals:
///
/// 1. **External absolute host paths** (`/home/…`, `/Users/…`, `/root/…`) —
///    anything that pins the deposit to one operator's machine layout.
///    Self-references to the deposit's OWN recorded root
///    ([`recorded_self_reference_roots`]) are EXCLUDED: they point into the
///    package's own tree, which relocates with the deposit, so they are not
///    external-machine pinning (the bulk of a real deposit's raw host-path
///    hits — `agent-code.json`, `error.json`, `decisions.jsonl` — are these).
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
    let self_roots = recorded_self_reference_roots(package_root);

    let mut host_paths: Vec<String> = Vec::new();
    let mut session_id_leaks: Vec<String> = Vec::new();

    // Deterministic depth-first walk; entries within a directory are visited in
    // sorted order so a truncated (capped) finding set is stable.
    let mut stack: Vec<std::path::PathBuf> = vec![package_root.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&cur) else {
            continue;
        };
        let mut entries: Vec<std::path::PathBuf> =
            rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
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
                // A self-reference into the deposit's OWN recorded root relocates
                // with the package (see `recorded_self_reference_roots`); only
                // EXTERNAL host paths are genuine portability residuals. `spans`
                // is left intact so the session-id leak check below still treats
                // an excluded path's bytes as a covered host-path span.
                if is_self_reference(p, &self_roots) {
                    continue;
                }
                host_paths.push(format!("{rel}: {p}"));
            }

            // Session-id leak: the raw UUID found outside any host-path span.
            // The `workflow-<uuid>` declared identity never matches (it carries
            // the simple, hyphen-free form), so it is exempt by construction.
            if let Some(sid) = sid {
                let leaked_outside_path = content
                    .match_indices(sid)
                    .any(|(idx, _)| !spans.iter().any(|(s, e, _)| idx >= *s && idx < *e));
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
/// the flat-layout SHA-512 manifest checksums over the freshly sealed deposit.
///
/// * `ro_crate` = `Pass` unless EITHER the re-verify saw a genuine divergence
///   (a recorded verdict that a fresh recomputation contradicts) while the
///   reader version matches the writer — the same "real tamper vs version
///   drift" distinction `replay`'s verdict uses; on a fresh self-export
///   `reader == writer`, so any divergence is real and fails the check — OR
///   the post-seal recheck (RCA I-2) finds an embedded content hash that
///   disagrees with the sealed payload.
/// * `checksum_seal` = `Pass` iff every file listed in
///   `manifest-sha512.txt` is present and its SHA-512 matches.
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
        .context("verifying SHA-512 checksum seal for readiness")?;
    let bagit = if bagit_ok {
        CheckStatus::Pass
    } else {
        CheckStatus::Fail
    };

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
        notes.push("SHA-512 checksum mismatch or missing manifested file".to_string());
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
    let domain_validation = domain.status();
    let domain_detail = (!domain.required_failures.is_empty()).then(|| {
        format!(
            "domain-validation failure(s): {}",
            domain.required_failures.join(", ")
        )
    });
    // An axis that inspected nothing is recorded honestly (and visibly) as
    // UNVERIFIED rather than silently rendered as a clean pass.
    let domain_unverified_detail = (domain_validation == CheckStatus::Unverified).then(|| {
        "domain-validation UNVERIFIED: no validate_* task recorded a recognized verdict, \
         the package carries no contract-obligation records, and no reporting-correctness \
         invariant found its inputs — nothing was inspected on this axis"
            .to_string()
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

    // The repair loop's own terminal verdict — READ from
    // `runtime/repair-status.json`, never re-run. A recorded `failing` blocks:
    // a package whose own repair loop concluded that too many failures remain
    // unresolved is not deposit-ready by its own account. `mostly_passing` is
    // surfaced with its unresolved count but never blocks. See
    // `scan_repair_status`.
    let repair = scan_repair_status(dst);
    let repair_status = repair.status();
    let repair_detail = repair.verdict.as_deref().and_then(|verdict| {
        match repair_status {
            CheckStatus::Fail => Some(format!(
                "repair-loop verdict {verdict}: {} unresolved failure(s) remain",
                repair.unresolved
            )),
            // Surfaced-but-non-blocking: an operator can see the residual
            // review queue without the deposit being refused over it.
            CheckStatus::Pass if repair.unresolved > 0 => Some(format!(
                "repair-loop verdict {verdict} (non-blocking): {} unresolved failure(s) left for review",
                repair.unresolved
            )),
            _ => None,
        }
    });

    // Contract obligations that reached NO verdict (`errored:` /
    // `unimplemented:`): named in the attestation so an operator can see WHICH
    // obligations went unchecked, but deliberately NOT folded into
    // `deposit_ready` — the harness treats them as soft-skips and leaves the
    // task Completed, so the deposit gate must not be the one consumer that
    // converts "could not check" into "failed".
    let unverified_obligations_detail = (!domain.unverified_obligations.is_empty()).then(|| {
        format!(
            "contract obligation(s) UNVERIFIED (no verdict reached; non-blocking): {}",
            domain.unverified_obligations.join(", ")
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
        domain_unverified_detail,
        unverified_obligations_detail,
        divergence_detail,
        substrate_detail,
        repair_detail,
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
    //
    // `repair_status` uses `!= Fail`, not `== Pass`, so the common
    // `Unverified` (no repair loop ever ran) stays non-blocking — the same
    // shape the `domain_validation` axis uses.
    let deposit_ready = compute_deposit_ready(
        profile,
        tier1.ro_crate,
        tier1.bagit,
        domain_validation,
        reexecution,
    ) && provenance_divergence == CheckStatus::Pass
        && substrate_validity == CheckStatus::Pass
        && repair_status != CheckStatus::Fail;

    let att = DepositReadiness {
        schema_version: "0.1".to_string(),
        profile: profile.to_string(),
        deposit_ready,
        ro_crate: tier1.ro_crate,
        bagit: tier1.bagit,
        domain_validation,
        provenance_divergence,
        substrate_validity,
        repair_status,
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
        && att.substrate_validity == CheckStatus::Pass
        // Carried from the Layer-1 attestation rather than re-scanned: a
        // Layer-2 re-execution cannot change what the repair loop concluded.
        && att.repair_status != CheckStatus::Fail;
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
/// `substrate_validity` verdict is a concrete FAIL, or whose recorded
/// `repair_status` is a concrete FAIL. A `NotVerified`
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
            dr.detail
                .as_deref()
                .map(|d| format!(" — {d}"))
                .unwrap_or_default()
        );
    }
    if dr.bagit != CheckStatus::Pass {
        bail!(
            "deposit gate: checksum-seal integrity did not pass ({:?}){}",
            dr.bagit,
            dr.detail
                .as_deref()
                .map(|d| format!(" — {d}"))
                .unwrap_or_default()
        );
    }
    // `Unverified` (the axis inspected nothing) is surfaced in the
    // attestation but does not block: absence of evidence is recorded
    // honestly, not converted into a failure. Only a concrete `Fail` blocks.
    if dr.domain_validation == CheckStatus::Fail {
        bail!(
            "deposit gate: per-task domain-correctness validation did not pass ({:?}){} — \
             a required validate_* check failed even though the run may be computationally \
             complete; remediate and re-export",
            dr.domain_validation,
            dr.detail
                .as_deref()
                .map(|d| format!(" — {d}"))
                .unwrap_or_default()
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
            dr.detail
                .as_deref()
                .map(|d| format!(" — {d}"))
                .unwrap_or_default()
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
            dr.detail
                .as_deref()
                .map(|d| format!(" — {d}"))
                .unwrap_or_default()
        );
    }
    // Repair-loop axis: refuse a deposit whose OWN repair loop recorded
    // `RepairVerdict::Failing` — the loop's statement that the unresolved
    // failure count exceeded its tolerance. `Unverified` (no repair loop ran,
    // the common case) and `Pass` (`fully_passing`/`mostly_passing`) are not
    // blocked here.
    if dr.repair_status == CheckStatus::Fail {
        bail!(
            "deposit gate: the package's own repair loop recorded a FAILING verdict{} — \
             `runtime/repair-status.json` reports more unresolved failures than its \
             threshold tolerates, so the package is not deposit-ready by its own account; \
             resolve the review queue (`ecaa-workflow repair`) and re-export",
            dr.detail
                .as_deref()
                .map(|d| format!(" — {d}"))
                .unwrap_or_default()
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

// ---------------------------------------------------------------------------
// Post-seal revalidation
// ---------------------------------------------------------------------------
//
// The deposit-readiness attestation is a verdict recorded AT EXPORT TIME.
// Everything downstream of that — the deposit gate, a reviewer, an archive —
// re-reads the verdict, never the evidence. Post-seal revalidation closes that
// gap for the subset of assertions that are checkable OFFLINE against the
// sealed tree alone: the file-presence claims the package's own per-task
// reports make about themselves.
//
// A validator script that recorded `artifact_presence.X: PASS
// (runtime/outputs/<task>/X.tsv)` and a deposit whose sealed tree does not
// contain that file are inconsistent regardless of whether the checksums of
// the files that ARE present all match — BagIt integrity cannot catch it,
// because a file absent from both the tree and the manifest is invisible to a
// manifest walk. This is the check that catches it.

/// Package-relative path of the post-seal revalidation report.
pub const POST_SEAL_VALIDATION_FILE: &str = "runtime/post-seal-validation.json";

/// Per-task report files whose contents are scanned for file-presence claims.
const PRESENCE_CLAIM_SOURCES: [&str; 3] =
    ["validation_report.json", "result.json", "manifest.json"];

/// The package-root-anchored prefix a presence claim uses to name a file in
/// the package's own tree. Any token containing this — including an absolute
/// host path recorded by an agent that ran inside the package dir — is
/// re-anchored at the package root from this segment onward.
const PACKAGE_ANCHOR: &str = "runtime/";

/// A single file-presence assertion recovered from a package's own reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceClaim {
    /// Task whose report made the claim.
    pub task_id: String,
    /// Report file the claim was recovered from (one of
    /// [`PRESENCE_CLAIM_SOURCES`]).
    pub source: String,
    /// Package-relative path the report claims is present.
    pub claimed_path: String,
}

/// The post-seal revalidation report, written to
/// [`POST_SEAL_VALIDATION_FILE`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PostSealValidation {
    /// Report schema version.
    pub schema_version: String,
    /// How many distinct file-presence claims were recovered and re-checked.
    pub claims_checked: usize,
    /// The subset of the recovered claims naming a path that is NOT in the
    /// sealed tree AND is not disclosed as dropped by the export, sorted.
    /// Non-empty ⇒ the package asserts the presence of a file it does not
    /// contain and does not admit to lacking.
    pub missing_claims: Vec<PresenceClaim>,
    /// Claims whose path is absent from the sealed tree but which the SAME
    /// report discloses as dropped at export
    /// (`export_reconciliation.unavailable[].dropped_at_export`), sorted.
    ///
    /// Not a defect: the export tier gate deliberately drops `intermediates/`
    /// and `view_data/`, and `emitter::export::reconcile_dropped_file_references`
    /// records each drop on the very report that cites it. A checker that
    /// re-flags a disclosed drop reports the package as dishonest about
    /// precisely the thing it was honest about. Surfaced for visibility,
    /// excluded from [`Self::presence_claims_hold`].
    #[serde(default)]
    pub reconciled_claims: Vec<PresenceClaim>,
    /// REQUIRED failures from the source-owned reporting-correctness
    /// checklist, re-run against the sealed tree.
    pub reporting_required_failures: Vec<String>,
    /// Advisory (never-blocking) reporting-correctness warnings.
    pub reporting_warnings: Vec<String>,
    /// REQUIRED contract-obligation failures recorded in the sealed tree's
    /// `runtime/validation-reports.jsonl` (see [`scan_contract_obligations`]).
    pub contract_obligation_failures: Vec<String>,
    /// `true` iff nothing above found a problem.
    pub passed: bool,
    /// RFC-3339 wall-clock instant the revalidation ran.
    pub checked_at: String,
}

impl PostSealValidation {
    /// `true` iff every recovered presence claim resolves in the sealed tree.
    /// The `--strict` refusal condition, kept separate from [`Self::passed`]
    /// so a reporting-invariant or contract-obligation finding is surfaced
    /// without being converted into a presence failure.
    pub fn presence_claims_hold(&self) -> bool {
        self.missing_claims.is_empty()
    }
}

/// Package-relative paths a report discloses as dropped by the export, read
/// from the `export_reconciliation.unavailable` block
/// `emitter::export::reconcile_dropped_file_references` writes onto every
/// report whose citations the tier gate invalidated.
///
/// Only an entry explicitly flagged `dropped_at_export` counts. A bare
/// `available: false` is not accepted: that would let any producer excuse its
/// own dangling citation by asserting the file is missing, which is the claim
/// under test, not evidence about it.
fn export_dropped_paths(doc: &serde_json::Value) -> std::collections::BTreeSet<String> {
    doc.get("export_reconciliation")
        .and_then(|b| b.get("unavailable"))
        .and_then(|u| u.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter(|e| {
                    e.get("dropped_at_export")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
                })
                .filter_map(|e| e.get("path").and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `true` when `p`'s final segment looks like a filename with an extension.
///
/// The presence-claim extractor only accepts tokens that pass this test.
/// Requiring an extension is what keeps prose out of the claim set: a
/// description string like `"scripts directory contains at least one script"`
/// tokenizes into words, none of which survives here. Directory-presence
/// claims are consequently NOT re-checked — a deliberate under-approximation,
/// because `--strict` refuses the deposit on a missing claim and a false
/// positive there is worse than a missed check.
fn looks_like_filename(p: &str) -> bool {
    let last = p.rsplit('/').next().unwrap_or("");
    match last.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && (1..=8).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// `true` for a JSON key whose value is a path rather than prose.
///
/// Only consulted for TASK-RELATIVE tokens (a bare `sub/dir/file.tsv` with no
/// package anchor); package-anchored tokens are accepted under any key,
/// including free-form `detail` strings, because `runtime/…` inside a
/// package's own report is unambiguous.
fn key_names_a_path(key: Option<&str>) -> bool {
    let Some(k) = key else {
        return false;
    };
    let k = k.to_ascii_lowercase();
    matches!(
        k.as_str(),
        "path" | "file" | "filename" | "artifact" | "artifacts" | "output" | "outputs"
    ) || k.ends_with("_path")
        || k.ends_with("_paths")
        || k.ends_with("_file")
        || k.ends_with("_files")
}

/// Candidate package-relative paths a single token could be naming, most
/// specific first. A claim is SATISFIED when any candidate exists, so an
/// ambiguous relative token cannot be reported missing on the strength of one
/// guess. Empty ⇒ the token is not a presence claim.
fn claim_candidates(task_id: &str, key: Option<&str>, token: &str) -> Vec<String> {
    if token.is_empty() || token.contains("..") || token.contains('*') || token.contains('?') {
        return Vec::new();
    }
    // Package-anchored: re-anchor from the LAST `runtime/` so an absolute host
    // path recorded by the agent (`/home/…/<pkg>/runtime/outputs/…`) collapses
    // to the package-relative form.
    if let Some(idx) = token.rfind(PACKAGE_ANCHOR) {
        let rel = &token[idx..];
        return if looks_like_filename(rel) {
            vec![rel.to_string()]
        } else {
            Vec::new()
        };
    }
    // A non-anchored ABSOLUTE path names something outside the package (a host
    // tool, a reference bundle). Never a claim about the sealed tree.
    if token.starts_with('/') || token.starts_with('~') {
        return Vec::new();
    }
    if !key_names_a_path(key) {
        return Vec::new();
    }
    let rel = token.trim_start_matches("./");
    if !rel.contains('/') || !looks_like_filename(rel) {
        return Vec::new();
    }
    // Resolve against the claiming task's own output dir first, then against
    // the package root — agent reports use both conventions.
    vec![format!("runtime/outputs/{task_id}/{rel}"), rel.to_string()]
}

/// Split a report string into path-shaped tokens.
///
/// Report strings are as often prose-with-a-path (`"path: /…/x.tsv, exists:
/// True"`) as they are a bare path, so the string is tokenized on whitespace
/// and the punctuation that brackets a path in prose, then each token is
/// tested independently.
fn path_tokens(s: &str) -> Vec<&str> {
    s.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`' | '<' | '>' | '|'
            )
    })
    .map(|t| t.trim_end_matches(|c: char| matches!(c, '.' | ':' | '!' | '?')))
    .filter(|t| !t.is_empty())
    .collect()
}

/// `true` when this string RECORDS A FAILURE rather than asserting presence.
/// A check the package itself recorded as failing is not claiming the file is
/// there, so re-checking it would double-report a known finding as a fresh
/// presence violation.
fn records_a_failure(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.starts_with("fail")
        || lower.starts_with("error")
        || lower.starts_with("missing")
        || lower.contains("exists: false")
        || lower.contains("exists=false")
        || lower.contains("does not exist")
        || lower.contains("not found")
}

/// Walk a parsed report document, collecting every file-presence claim as its
/// candidate-path list (see [`claim_candidates`]). A `BTreeSet` both
/// de-duplicates repeated claims and fixes the iteration order, so the report
/// is deterministic.
fn collect_presence_claims(
    task_id: &str,
    key: Option<&str>,
    v: &serde_json::Value,
    out: &mut std::collections::BTreeSet<Vec<String>>,
) {
    match v {
        serde_json::Value::Object(map) => {
            // A check object that recorded a negative outcome is not claiming
            // presence — skip its whole subtree, paths and all.
            for negative in ["passed", "present", "exists", "ok"] {
                if map.get(negative).and_then(serde_json::Value::as_bool) == Some(false) {
                    return;
                }
            }
            for (k, child) in map {
                collect_presence_claims(task_id, Some(k.as_str()), child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_presence_claims(task_id, key, item, out);
            }
        }
        serde_json::Value::String(s) => {
            if records_a_failure(s) {
                return;
            }
            for token in path_tokens(s) {
                let candidates = claim_candidates(task_id, key, token);
                if !candidates.is_empty() {
                    out.insert(candidates);
                }
            }
        }
        _ => {}
    }
}

/// Re-run every OFFLINE-CHECKABLE assertion the sealed package makes about
/// itself, without re-executing anything.
///
/// Three evidence surfaces, all read from `package_root` as it sits on disk:
///
/// 1. **File-presence claims.** Every `runtime/outputs/<task>/` report in
///    [`PRESENCE_CLAIM_SOURCES`] is walked for tokens naming a file in the
///    package tree; each is re-resolved against the sealed tree. This is the
///    class BagIt integrity structurally cannot cover — a manifest walk only
///    sees files that exist.
/// 2. **Reporting-correctness invariants**
///    ([`crate::reporting_invariants::check_reporting_invariants`]),
///    recomputed from the package's own outputs.
/// 3. **Contract obligations** ([`scan_contract_obligations`]).
///
/// Pure scan — writes nothing. Deterministic: claims are de-duplicated and
/// sorted; only `checked_at` comes from `clock`.
pub fn revalidate_post_seal(package_root: &Path, clock: &dyn Clock) -> PostSealValidation {
    let outputs_dir = package_root.join("runtime").join("outputs");
    let mut task_ids: Vec<String> = std::fs::read_dir(&outputs_dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_else(|_| Vec::new());
    task_ids.sort();

    let mut claims_checked = 0usize;
    let mut missing_claims: Vec<PresenceClaim> = Vec::new();
    let mut reconciled_claims: Vec<PresenceClaim> = Vec::new();
    for task_id in &task_ids {
        for source in PRESENCE_CLAIM_SOURCES {
            let report_path = outputs_dir.join(task_id).join(source);
            let Ok(raw) = std::fs::read_to_string(&report_path) else {
                continue;
            };
            let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) else {
                continue;
            };
            // Drops this very report discloses. Scoped to the same document on
            // purpose: a drop admitted by one task's report must not excuse a
            // dangling claim in another's.
            let dropped = export_dropped_paths(&doc);
            let mut claims: std::collections::BTreeSet<Vec<String>> =
                std::collections::BTreeSet::new();
            collect_presence_claims(task_id, None, &doc, &mut claims);
            for candidates in claims {
                claims_checked += 1;
                // A claim holds when ANY of its candidate resolutions exists —
                // an ambiguous relative token must never be reported missing
                // on the strength of one guess.
                if candidates.iter().any(|rel| package_root.join(rel).exists()) {
                    continue;
                }
                let claim = PresenceClaim {
                    task_id: task_id.clone(),
                    source: source.to_string(),
                    // Non-empty by construction: `claim_candidates` returns an
                    // empty vec for a non-claim, and those are never inserted.
                    claimed_path: candidates
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "<unresolvable claim>".to_string()),
                };
                if candidates.iter().any(|rel| dropped.contains(rel)) {
                    reconciled_claims.push(claim);
                } else {
                    missing_claims.push(claim);
                }
            }
        }
    }
    let by_identity = |a: &PresenceClaim, b: &PresenceClaim| {
        (&a.task_id, &a.source, &a.claimed_path).cmp(&(&b.task_id, &b.source, &b.claimed_path))
    };
    missing_claims.sort_by(by_identity);
    reconciled_claims.sort_by(by_identity);

    let ri = crate::reporting_invariants::check_reporting_invariants(package_root);
    let reporting_required_failures = ri.required_failures();
    let reporting_warnings = ri.warnings();
    let contract_obligation_failures = scan_contract_obligations(package_root);

    let passed = missing_claims.is_empty()
        && reporting_required_failures.is_empty()
        && contract_obligation_failures.is_empty();

    PostSealValidation {
        schema_version: "0.1".to_string(),
        claims_checked,
        missing_claims,
        reconciled_claims,
        reporting_required_failures,
        reporting_warnings,
        contract_obligation_failures,
        passed,
        checked_at: clock.now_rfc3339(),
    }
}

/// Run [`revalidate_post_seal`], write the report to
/// [`POST_SEAL_VALIDATION_FILE`], and — under `strict` — refuse the package
/// when any presence claim names a file absent from the sealed tree.
///
/// The report is a MUTABLE META FILE, the same class as
/// `DEPOSIT-READINESS.json`: it carries a wall-clock `checked_at` and a
/// verdict computed after the seal, so it is not a hashed `@graph` payload
/// entity. Writing it does not invalidate the BagIt manifest (a manifest walk
/// verifies the files it lists; an unlisted file is not a mismatch).
///
/// Non-presence findings (reporting invariants, contract obligations) are
/// recorded in the report and reflected in [`PostSealValidation::passed`] but
/// do NOT drive the `strict` refusal — those axes already have their own
/// enforcement point in the deposit gate, and duplicating it here would
/// refuse the same package twice for one finding.
pub fn run_post_seal_revalidation(
    package_root: &Path,
    strict: bool,
    clock: &dyn Clock,
) -> Result<PostSealValidation> {
    let report = revalidate_post_seal(package_root, clock);
    let path = package_root.join(POST_SEAL_VALIDATION_FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let body =
        serde_json::to_vec_pretty(&report).context("serializing post-seal-validation.json")?;
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;

    if strict && !report.presence_claims_hold() {
        let detail = report
            .missing_claims
            .iter()
            .take(10)
            .map(|c| format!("{} ({}) → {}", c.task_id, c.source, c.claimed_path))
            .collect::<Vec<_>>()
            .join("; ");
        bail!(
            "post-seal revalidation: {} of {} file-presence claim(s) name a file absent from \
             the sealed tree — the package asserts artifacts it does not contain (see {}): {detail}",
            report.missing_claims.len(),
            report.claims_checked,
            POST_SEAL_VALIDATION_FILE,
        );
    }
    Ok(report)
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
        assert!(
            read_deposit_readiness(tmp.path())
                .unwrap()
                .unwrap()
                .deposit_ready
        );
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
            !read_deposit_readiness(tmp.path())
                .unwrap()
                .unwrap()
                .deposit_ready,
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
        assert_eq!(
            reexec_status_from_verdict(&ReplayVerdict::Pass),
            ReexecStatus::Pass
        );
        assert_eq!(
            reexec_status_from_verdict(&ReplayVerdict::Partial),
            ReexecStatus::Partial
        );
        assert_eq!(
            reexec_status_from_verdict(&ReplayVerdict::Fail),
            ReexecStatus::Fail
        );
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
        assert_eq!(
            summary.failed_tasks,
            vec!["validate_differential_expression"]
        );
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
        // Nothing FAILED …
        assert!(summary.passed());
        // … but nothing was inspected either, so the attestation axis must not
        // read as a clean pass.
        assert_eq!(summary.status(), CheckStatus::Unverified);
    }

    /// Agent-authored `validate_*` reports do not converge on the
    /// `validation_passed` spelling: the on-disk corpus uses
    /// `validation_result` / `overall_validation_status` / `validation_status`
    /// / `overall` / `validation_outcome` / `overall_pass` / `all_pass`.
    /// Reading only `validation_passed` made the rollup skip every real task.
    #[test]
    fn domain_validation_reads_validation_result_spelling() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_validate_result(
            root,
            "validate_primary_analysis",
            serde_json::json!({"validation_result": "FAIL", "required_failures": ["contract.assertion_a"]}),
        );
        write_validate_result(
            root,
            "validate_secondary_analysis",
            serde_json::json!({"overall_validation_status": "fail"}),
        );
        write_validate_result(
            root,
            "validate_tertiary_analysis",
            serde_json::json!({"overall_pass": false}),
        );

        let summary = scan_domain_validation(root);
        assert_eq!(
            summary.failed_tasks,
            vec![
                "validate_primary_analysis",
                "validate_secondary_analysis",
                "validate_tertiary_analysis"
            ],
            "every observed verdict spelling must be read: {summary:?}"
        );
        assert_eq!(summary.status(), CheckStatus::Fail);
        assert!(summary
            .required_failures
            .iter()
            .any(|f| f == "validate_primary_analysis: contract.assertion_a"));
        // A failing self-report that names no assertion still reaches the
        // detail, so a `Fail` axis is never unexplained.
        assert!(summary
            .required_failures
            .iter()
            .any(|f| f.starts_with("validate_secondary_analysis: ")));
    }

    /// The mirror case: a PASS in a non-`validation_passed` spelling has to
    /// count as an inspected, passing check — otherwise the axis stays
    /// vacuous on a package whose validators all succeeded.
    #[test]
    fn domain_validation_counts_pass_spelling() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_validate_result(
            root,
            "validate_primary_analysis",
            serde_json::json!({"validation_result": "PASS"}),
        );
        write_validate_result(
            root,
            "validate_secondary_analysis",
            serde_json::json!({"validation_status": "pass"}),
        );
        write_validate_result(
            root,
            "validate_tertiary_analysis",
            serde_json::json!({"all_pass": true}),
        );
        // An unrecognized value under a recognized key must fall through as
        // "no verdict recorded" rather than fabricating one.
        write_validate_result(
            root,
            "validate_quaternary_analysis",
            serde_json::json!({"overall": "n/a"}),
        );

        let summary = scan_domain_validation(root);
        assert_eq!(
            summary.checked_tasks,
            vec![
                "validate_primary_analysis",
                "validate_secondary_analysis",
                "validate_tertiary_analysis"
            ]
        );
        assert!(summary.failed_tasks.is_empty());
        assert_eq!(summary.status(), CheckStatus::Pass);
    }

    /// Write one named self-report file into a `validate_*` task's output dir.
    fn write_validate_file(root: &Path, task_id: &str, file: &str, body: serde_json::Value) {
        let dir = root.join("runtime/outputs").join(task_id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(file), body.to_string()).unwrap();
    }

    fn write_repair_status(root: &Path, verdict: &str, unresolved: usize) {
        let dir = root.join("runtime");
        fs::create_dir_all(&dir).unwrap();
        let review: Vec<serde_json::Value> = (0..unresolved)
            .map(|i| serde_json::json!({"failure": {"id": format!("f{i}")}, "why": "unresolved"}))
            .collect();
        fs::write(
            dir.join("repair-status.json"),
            serde_json::json!({"verdict": verdict, "rounds": 1, "review": review}).to_string(),
        )
        .unwrap();
    }

    /// (a) The exact defect: `validate_reporting` recorded its verdict under the
    /// key `verdict` as a SUMMARY string. The old gate missed it twice — the key
    /// was unrecognized, and the value was not a bare token — so a deposit
    /// carrying a validator that self-reported FAIL attested
    /// `domain_validation: pass`.
    ///
    /// Byte-identical to the `eda58089` deposit's real `result.json` fields.
    #[test]
    fn summary_verdict_string_under_verdict_key_is_a_failure() {
        assert_eq!(
            verdict_from_str("FAIL 130/135 checks (5 failed)"),
            Some(false),
            "a summary string whose first token is FAIL is a failing verdict"
        );
        assert_eq!(
            verdict_from_str("PASS 42/42 checks"),
            Some(true),
            "…and the passing mirror must not be dragged along with it"
        );

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_validate_result(
            root,
            "validate_reporting",
            serde_json::json!({
                "task_id": "validate_reporting",
                // The TASK LIFECYCLE state — legitimately independent of the
                // domain verdict, and deliberately not read as one.
                "status": "completed",
                "verdict": "FAIL 130/135 checks (5 failed)",
                "checks_total": 135,
                "checks_passed": 130,
                "checks_failed": 5,
                "failed_checks": [
                    {"id": "depleted_table.row07.found", "category": "pathway_depleted_table",
                     "description": "Depleted pathway 7 found in pathway_results.tsv",
                     "passed": false},
                    {"id": "depleted_table.row09.nes", "category": "pathway_depleted_table",
                     "description": "Depleted pathway 9 NES matches pathway_results.tsv",
                     "passed": false},
                ],
            }),
        );

        let summary = scan_domain_validation(root);
        assert_eq!(
            summary.failed_tasks,
            vec!["validate_reporting"],
            "{summary:?}"
        );
        assert_eq!(summary.status(), CheckStatus::Fail, "{summary:?}");
        // The named failures must reach the detail even though the array holds
        // OBJECTS, not strings — otherwise the Fail axis is unexplained.
        assert_eq!(
            summary.required_failures,
            vec![
                "validate_reporting: depleted_table.row07.found",
                "validate_reporting: depleted_table.row09.nes",
            ],
            "{summary:?}"
        );
    }

    /// (b) `validation_overall` is the DOMINANT on-disk spelling (6 of the 8
    /// `validate_*` tasks on the reference deposit, 11 across the corpus) and
    /// was absent from the recognized key list, so the whole axis read as
    /// vacuous on a package whose validators had all in fact reported.
    #[test]
    fn validation_overall_spelling_is_recognized() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_validate_result(
            root,
            "validate_normalisation",
            serde_json::json!({"validation_overall": "PASS", "n_checks": 79}),
        );
        // The other surveyed tail spellings, same treatment.
        write_validate_result(
            root,
            "validate_pathway_enrichment",
            serde_json::json!({"overall_status": "PASS"}),
        );
        write_validate_result(
            root,
            "validate_reporting",
            serde_json::json!({"outcome": "PASS"}),
        );

        let summary = scan_domain_validation(root);
        assert_eq!(
            summary.checked_tasks,
            vec![
                "validate_normalisation",
                "validate_pathway_enrichment",
                "validate_reporting"
            ],
            "every surveyed spelling must count as an inspected check: {summary:?}"
        );
        assert_eq!(summary.status(), CheckStatus::Pass, "{summary:?}");
    }

    /// (c) The spelling-proof path: a positive failure COUNT is a failing
    /// verdict on its own, with no recognizable verdict string anywhere in the
    /// document. Covers the two real corpus reports whose `validate_reporting`
    /// `result.json` carries `n_fail: 2` / `n_checks_fail: 1` and NO verdict
    /// string at all — both invisible to the old gate.
    #[test]
    fn positive_failure_count_alone_is_a_failure() {
        for key in ["checks_failed", "n_fail", "n_checks_fail", "n_failed"] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            let mut body = serde_json::Map::new();
            body.insert(
                "task_id".to_string(),
                serde_json::json!("validate_reporting"),
            );
            // The task LIFECYCLE state, which must not be read as a verdict.
            body.insert("status".to_string(), serde_json::json!("completed"));
            body.insert(key.to_string(), serde_json::json!(5));
            write_validate_result(root, "validate_reporting", serde_json::Value::Object(body));
            let summary = scan_domain_validation(root);
            assert_eq!(
                summary.failed_tasks,
                vec!["validate_reporting"],
                "a positive `{key}` must fail the axis with no verdict string: {summary:?}"
            );
            assert_eq!(summary.status(), CheckStatus::Fail);
        }
    }

    /// (d) The mirror: zero failures alongside a positive PASSED count is a
    /// pass. Zero failures ALONE is not — a document that recorded nothing as
    /// having run is no evidence of a clean bill of health, so it stays
    /// `Unverified`.
    #[test]
    fn zero_failures_with_passes_is_a_pass_but_zero_alone_is_not() {
        let tmp = tempfile::tempdir().unwrap();
        write_validate_result(
            tmp.path(),
            "validate_normalisation",
            serde_json::json!({"checks_failed": 0, "checks_passed": 79}),
        );
        let summary = scan_domain_validation(tmp.path());
        assert_eq!(summary.checked_tasks, vec!["validate_normalisation"]);
        assert_eq!(summary.status(), CheckStatus::Pass, "{summary:?}");

        // Zero failures and nothing recorded as passing: no verdict.
        let bare = tempfile::tempdir().unwrap();
        write_validate_result(
            bare.path(),
            "validate_normalisation",
            serde_json::json!({"checks_failed": 0}),
        );
        let bare_summary = scan_domain_validation(bare.path());
        assert!(
            bare_summary.checked_tasks.is_empty(),
            "a zero failure count with no passes recorded is not evidence of a pass: \
             {bare_summary:?}"
        );
        assert_eq!(bare_summary.status(), CheckStatus::Unverified);

        // …and it must NOT be allowed to cancel an explicit failing bool.
        let contra = tempfile::tempdir().unwrap();
        write_validate_result(
            contra.path(),
            "validate_normalisation",
            serde_json::json!({"validation_passed": false, "checks_failed": 0, "checks_passed": 9}),
        );
        assert_eq!(
            scan_domain_validation(contra.path()).status(),
            CheckStatus::Fail,
            "numeric evidence may only strengthen the gate, never weaken it"
        );
    }

    /// A positive failure count OUTRANKS a contradicting passing string/bool: a
    /// report that contradicts itself is defective, and the safe reading of a
    /// defective validator report at a deposit boundary is the failing one.
    #[test]
    fn numeric_failure_count_outranks_a_passing_string() {
        let tmp = tempfile::tempdir().unwrap();
        write_validate_result(
            tmp.path(),
            "validate_reporting",
            serde_json::json!({
                "validation_overall": "PASS",
                "validation_passed": true,
                "checks_failed": 5,
                "checks_passed": 130,
            }),
        );
        let summary = scan_domain_validation(tmp.path());
        assert_eq!(
            summary.failed_tasks,
            vec!["validate_reporting"],
            "{summary:?}"
        );
        assert_eq!(summary.status(), CheckStatus::Fail);
    }

    /// (e) A verdict written ONLY into `validation_results.json` (PLURAL — a
    /// different file from `validation_report.json`) must still be seen: that
    /// filename was in none of the sources this module read. Same for the
    /// singular `validation_report.json`, which 12 corpus tasks used as the
    /// ONLY place they recorded a verdict.
    #[test]
    fn verdict_in_a_sibling_report_file_only_is_still_seen() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // `result.json` records the lifecycle state and nothing else.
        write_validate_file(
            root,
            "validate_reporting",
            "result.json",
            serde_json::json!({"task_id": "validate_reporting", "status": "completed"}),
        );
        write_validate_file(
            root,
            "validate_reporting",
            "validation_results.json",
            serde_json::json!({
                "validator": "validate_reporting",
                "verdict": "FAIL 130/135 checks (5 failed)",
                "checks_failed": 5,
            }),
        );
        // A second task that reports only in the SINGULAR filename.
        write_validate_file(
            root,
            "validate_data_acquisition",
            "validation_report.json",
            serde_json::json!({"overall": "PASS", "n_pass": 34, "n_fail": 0}),
        );

        let summary = scan_domain_validation(root);
        assert_eq!(
            summary.checked_tasks,
            vec!["validate_data_acquisition", "validate_reporting"],
            "a verdict in any self-report file must be read: {summary:?}"
        );
        assert_eq!(
            summary.failed_tasks,
            vec!["validate_reporting"],
            "{summary:?}"
        );
    }

    /// (f) When a task's own files DISAGREE, the FAILING one wins.
    #[test]
    fn disagreeing_self_reports_resolve_to_the_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_validate_file(
            root,
            "validate_reporting",
            "result.json",
            serde_json::json!({"validation_overall": "PASS", "n_checks": 135}),
        );
        write_validate_file(
            root,
            "validate_reporting",
            "validation_results.json",
            serde_json::json!({
                "verdict": "FAIL 130/135 checks (5 failed)",
                "failed_checks": [{"id": "depleted_table.row07.found"}],
            }),
        );

        let summary = scan_domain_validation(root);
        assert_eq!(
            summary.failed_tasks,
            vec!["validate_reporting"],
            "{summary:?}"
        );
        assert_eq!(summary.status(), CheckStatus::Fail);
        assert_eq!(
            summary.required_failures,
            vec!["validate_reporting: depleted_table.row07.found"],
            "the failing sibling's named failures must reach the detail: {summary:?}"
        );

        // Symmetric: order of the files must not matter.
        let flipped = tempfile::tempdir().unwrap();
        write_validate_file(
            flipped.path(),
            "validate_reporting",
            "result.json",
            serde_json::json!({"verdict": "FAIL 1/2 checks (1 failed)"}),
        );
        write_validate_file(
            flipped.path(),
            "validate_reporting",
            "validation_results.json",
            serde_json::json!({"verdict": "PASS 2/2 checks"}),
        );
        assert_eq!(
            scan_domain_validation(flipped.path()).status(),
            CheckStatus::Fail,
            "fail-dominance must be order-independent"
        );
    }

    /// (g) Widening must not become over-matching: unrelated prose under a
    /// recognized-but-generic key still yields NO verdict, so it can neither
    /// fabricate a failure nor fabricate a pass.
    #[test]
    fn unrelated_prose_under_a_generic_key_yields_no_verdict() {
        for value in [
            "n/a",
            "ok",
            "unknown",
            "not applicable",
            "validation was inconclusive; 2 checks fail to apply",
            "error-free",
            "no failures observed",
            "",
        ] {
            assert_eq!(
                verdict_from_str(value),
                None,
                "{value:?} must not resolve to a verdict"
            );
        }

        let tmp = tempfile::tempdir().unwrap();
        write_validate_result(
            tmp.path(),
            "validate_normalisation",
            serde_json::json!({"overall": "n/a", "outcome": "ok", "verdict": "error-free"}),
        );
        let summary = scan_domain_validation(tmp.path());
        assert!(
            summary.checked_tasks.is_empty(),
            "prose must not fabricate an inspected check: {summary:?}"
        );
        assert_eq!(summary.status(), CheckStatus::Unverified);
    }

    /// A compound verdict that is not whitespace-delimited (the observed
    /// `PASS-WITH-WARN`) is deliberately NOT split on the hyphen — that would
    /// read `error-free` as a failure. It resolves through the NUMERIC path
    /// instead, which is precisely why the numeric path exists.
    #[test]
    fn hyphenated_compound_verdict_resolves_through_the_numeric_path() {
        assert_eq!(
            verdict_from_str("PASS-WITH-WARN"),
            None,
            "hyphen-splitting is not safe, so the string path abstains"
        );
        let tmp = tempfile::tempdir().unwrap();
        write_validate_result(
            tmp.path(),
            "validate_review_prior_work",
            // The real corpus shape for this verdict.
            serde_json::json!({
                "verdict": "PASS-WITH-WARN",
                "checks_total": 19,
                "checks_pass": 18,
                "checks_warn": 1,
                "checks_fail": 0,
            }),
        );
        let summary = scan_domain_validation(tmp.path());
        assert_eq!(
            summary.checked_tasks,
            vec!["validate_review_prior_work"],
            "the numeric counts must rescue an unparseable verdict string: {summary:?}"
        );
        assert_eq!(summary.status(), CheckStatus::Pass, "{summary:?}");
    }

    /// (h) The repair loop's own terminal verdict reaches the deposit boundary.
    /// `failing` BLOCKS: a package whose own repair loop concluded that more
    /// failures remain unresolved than its threshold tolerates is not
    /// deposit-ready by its own account. Reproduces the `eda58089` deposit,
    /// which recorded `verdict: "failing"` with 3814 unresolved review items
    /// and still attested `deposit_ready: true`.
    #[test]
    fn failing_repair_verdict_blocks_the_deposit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_repair_status(root, "failing", 7);
        let repair = scan_repair_status(root);
        assert_eq!(repair.verdict.as_deref(), Some("failing"));
        assert_eq!(repair.unresolved, 7);
        assert_eq!(repair.status(), CheckStatus::Fail);

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
        assert_eq!(dr.repair_status, CheckStatus::Fail, "{dr:?}");
        assert!(
            !dr.deposit_ready,
            "a failing repair verdict must block deposit-readiness: {dr:?}"
        );
        assert!(
            dr.detail
                .as_deref()
                .unwrap_or_default()
                .contains("7 unresolved"),
            "the unresolved count must be surfaced: {dr:?}"
        );
        let err = check_deposit_readiness(root, false)
            .expect_err("the Layer-3 gate must refuse a failing repair verdict");
        assert!(
            format!("{err:#}").contains("repair loop"),
            "gate error must name the repair loop: {err:#}"
        );
    }

    /// `mostly_passing` is documented as "a tolerable number of unresolved
    /// failures remain" and is the routine outcome (9 of the 10 local packages
    /// carrying a repair status). It is SURFACED with its unresolved count but
    /// must never block. An absent record — the overwhelmingly common case,
    /// since the repair loop is operator-triggered — is `Unverified`, likewise
    /// non-blocking.
    #[test]
    fn mostly_passing_and_absent_repair_status_do_not_block() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_repair_status(root, "mostly_passing", 4);
        assert_eq!(scan_repair_status(root).status(), CheckStatus::Pass);
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
        assert_eq!(dr.repair_status, CheckStatus::Pass, "{dr:?}");
        assert!(dr.deposit_ready, "mostly_passing must not block: {dr:?}");
        assert!(
            dr.detail
                .as_deref()
                .unwrap_or_default()
                .contains("4 unresolved failure(s) left for review"),
            "a tolerated review queue must still be visible: {dr:?}"
        );
        assert!(check_deposit_readiness(root, false).is_ok());

        // No repair status at all → Unverified, non-blocking, no detail noise.
        let none = tempfile::tempdir().unwrap();
        assert_eq!(
            scan_repair_status(none.path()).status(),
            CheckStatus::Unverified
        );
        write_deposit_readiness(
            none.path(),
            "full",
            &tier1(CheckStatus::Pass, CheckStatus::Pass, None),
            ReexecStatus::Partial,
            None,
            None,
            &WallClock,
        )
        .unwrap();
        let dr_none = read_deposit_readiness(none.path()).unwrap().unwrap();
        assert_eq!(
            dr_none.repair_status,
            CheckStatus::Unverified,
            "{dr_none:?}"
        );
        assert!(dr_none.deposit_ready, "{dr_none:?}");
        assert!(check_deposit_readiness(none.path(), false).is_ok());
    }

    /// End-to-end over the reference deposit's real shape: 8 `validate_*` tasks
    /// of which 7 self-reported PASS under `validation_overall` / `verdict` and
    /// one (`validate_reporting`) self-reported
    /// `verdict: "FAIL 130/135 checks (5 failed)"`. The old gate recognized a
    /// verdict on ZERO of the eight and attested `domain_validation: pass` with
    /// `deposit_ready: true`. It must now read all eight and refuse the deposit.
    #[test]
    fn reference_deposit_shape_flips_to_not_deposit_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for (task, checks) in [
            ("validate_contextualize_findings_with_literature", 11),
            ("validate_data_acquisition", 34),
            ("validate_differential_expression", 40),
            ("validate_final_reporting", 81),
            ("validate_normalisation", 79),
            ("validate_pathway_enrichment", 83),
        ] {
            write_validate_result(
                root,
                task,
                serde_json::json!({
                    "task_id": task,
                    "status": "completed",
                    "validation_overall": "PASS",
                    "n_checks": checks,
                    "n_passed": checks,
                    "n_failed": 0,
                }),
            );
        }
        write_validate_result(
            root,
            "validate_review_prior_work",
            serde_json::json!({
                "task_id": "validate_review_prior_work",
                "status": "completed",
                "verdict": "PASS 42/42 checks",
                "checks_total": 42,
                "checks_passed": 42,
                "checks_failed": 0,
            }),
        );
        write_validate_file(
            root,
            "validate_reporting",
            "result.json",
            serde_json::json!({
                "task_id": "validate_reporting",
                "status": "completed",
                "verdict": "FAIL 130/135 checks (5 failed)",
                "checks_total": 135,
                "checks_passed": 130,
                "checks_failed": 5,
                "failed_checks": [{"id": "depleted_table.row07.found"}],
            }),
        );
        write_validate_file(
            root,
            "validate_reporting",
            "validation_results.json",
            serde_json::json!({
                "task_id": "validate_reporting",
                // The validator's own summary state, distinct from the task
                // lifecycle `completed` in result.json.
                "status": "failed",
                "verdict": "FAIL 130/135 checks (5 failed)",
                "checks_failed": 5,
                "checks_passed": 130,
            }),
        );
        write_repair_status(root, "failing", 3814);

        let summary = scan_domain_validation(root);
        let inspected: Vec<&String> = summary
            .checked_tasks
            .iter()
            .filter(|t| t.starts_with("validate_"))
            .collect();
        assert_eq!(
            inspected.len(),
            8,
            "all eight validate_* tasks must be recognized as inspected \
             (the old gate recognized ZERO): {summary:?}"
        );
        assert_eq!(
            summary.failed_tasks,
            vec!["validate_reporting"],
            "{summary:?}"
        );
        assert_eq!(summary.status(), CheckStatus::Fail);

        write_deposit_readiness(
            root,
            "re-executable",
            &tier1(CheckStatus::Pass, CheckStatus::Pass, None),
            ReexecStatus::Partial,
            None,
            None,
            &WallClock,
        )
        .unwrap();
        let dr = read_deposit_readiness(root).unwrap().unwrap();
        assert_eq!(dr.domain_validation, CheckStatus::Fail, "{dr:?}");
        assert_eq!(dr.repair_status, CheckStatus::Fail, "{dr:?}");
        assert!(
            !dr.deposit_ready,
            "the reference deposit shape must NOT read deposit-ready: {dr:?}"
        );
        assert!(check_deposit_readiness(root, false).is_err());
    }

    fn write_validation_reports(root: &Path, lines: &[serde_json::Value]) {
        let dir = root.join("runtime");
        fs::create_dir_all(&dir).unwrap();
        let body = lines
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(dir.join("validation-reports.jsonl"), body).unwrap();
    }

    /// `runtime/validation-reports.jsonl` is the package's own record of the
    /// contract obligations the harness ran. A `failed:` outcome there is a
    /// domain-correctness failure the deposit rollup never read.
    ///
    /// `errored:` is NOT folded in (see `errored_obligation_is_unverified_not_failed`):
    /// only the outcome class the harness itself gates on (`failed:`) blocks.
    #[test]
    fn domain_validation_folds_contract_obligation_failures() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_validation_reports(
            root,
            &[
                serde_json::json!({"task_id":"t1","obligation_id":"ob_ok","outcome":"passed"}),
                serde_json::json!({"task_id":"t2","obligation_id":"ob_bad","outcome":"failed:threshold not met"}),
                // Duplicate row (an obligation declared on two required
                // artifacts) must be folded once.
                serde_json::json!({"task_id":"t2","obligation_id":"ob_bad","outcome":"failed:threshold not met"}),
                // An obligation whose checker COULD NOT RUN reached no verdict.
                // The harness treats it as a soft-skip and leaves the task
                // Completed, so it must not block here either.
                serde_json::json!({"task_id":"t3","obligation_id":"ob_err","outcome":"errored:input table absent"}),
                // A catalog obligation with no checker yet is a validator-suite
                // gap, not a defect in this package — it must not block.
                serde_json::json!({"task_id":"t4","obligation_id":"ob_todo","outcome":"unimplemented:foo_check"}),
            ],
        );

        let failures = scan_contract_obligations(root);
        assert_eq!(
            failures,
            vec!["t2.ob_bad (failed:threshold not met)"],
            "only the `failed:` class is a required failure"
        );

        let summary = scan_domain_validation(root);
        assert_eq!(summary.failed_tasks, vec![CONTRACT_OBLIGATIONS_TASK_ID]);
        assert_eq!(summary.status(), CheckStatus::Fail);
        assert!(summary
            .required_failures
            .iter()
            .all(|f| f.starts_with("contract_obligations: ")));
        // The two no-verdict obligations are surfaced, not folded into failures.
        assert_eq!(
            summary.unverified_obligations,
            vec![
                "t3.ob_err (errored:input table absent)",
                "t4.ob_todo (unimplemented:foo_check)"
            ]
        );

        // An all-passing obligation set is not a failure, but it IS evidence
        // that something was inspected.
        let clean = tempfile::tempdir().unwrap();
        write_validation_reports(
            clean.path(),
            &[serde_json::json!({"task_id":"t1","obligation_id":"ob_ok","outcome":"passed"})],
        );
        let clean_summary = scan_domain_validation(clean.path());
        assert_eq!(clean_summary.status(), CheckStatus::Pass);
        assert_eq!(
            clean_summary.checked_tasks,
            vec![CONTRACT_OBLIGATIONS_TASK_ID]
        );
        assert!(clean_summary.unverified_obligations.is_empty());
    }

    /// An obligation the harness recorded as `errored:` (its checker could not
    /// run) is NOT a required failure — it reached no verdict.
    ///
    /// Reproduces the real fresh-package case: one
    /// `errored:no independent symbol↔Ensembl annotation table in package …`
    /// for `gene_symbol_ensembl_consistent` alongside 14 passing obligations.
    /// That single line used to flip `domain_validation` to `fail` and block the
    /// deposit even though the science it would have checked was independently
    /// verified sound — and even though the harness left the task `Completed`
    /// (`ValidationReportSummary::has_failures()` matches only `Failed`), making
    /// this gate the only consumer that disagreed.
    #[test]
    fn errored_obligation_is_unverified_not_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut rows = vec![serde_json::json!({
            "task_id": "contextualize_findings_with_literature",
            "obligation_id": "gene_symbol_ensembl_consistent",
            "outcome": "errored:no independent symbol↔Ensembl annotation table in package",
        })];
        for i in 0..14 {
            rows.push(serde_json::json!({
                "task_id": "differential_expression",
                "obligation_id": format!("ob_{i:02}"),
                "outcome": "passed",
            }));
        }
        write_validation_reports(root, &rows);

        // Not a required failure on either surface.
        assert!(
            scan_contract_obligations(root).is_empty(),
            "an obligation that could not run must not be a required failure"
        );
        let summary = scan_domain_validation(root);
        assert!(summary.failed_tasks.is_empty(), "{summary:?}");
        assert!(summary.required_failures.is_empty(), "{summary:?}");
        assert!(summary.passed(), "{summary:?}");

        // Surfaced instead, attributably.
        assert_eq!(
            summary.unverified_obligations,
            vec![
                "contextualize_findings_with_literature.gene_symbol_ensembl_consistent \
                 (errored:no independent symbol↔Ensembl annotation table in package)"
            ]
        );
        // 14 concrete passes ⇒ the axis WAS inspected ⇒ Pass, not Unverified.
        assert_eq!(summary.status(), CheckStatus::Pass);

        // …and the deposit is not blocked, end to end through the attestation.
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
        assert_eq!(dr.domain_validation, CheckStatus::Pass, "{dr:?}");
        assert!(
            dr.deposit_ready,
            "an obligation that could not run must not block the deposit: {dr:?}"
        );
        assert!(
            dr.detail
                .as_deref()
                .unwrap_or_default()
                .contains("gene_symbol_ensembl_consistent"),
            "the unchecked obligation must still be NAMED in the attestation: {dr:?}"
        );
        // The Layer-3 gate admits it. `--strict` is not an escalation lever for
        // a no-verdict domain axis (that lever is `ReexecStatus::NotVerified`).
        assert!(check_deposit_readiness(root, false).is_ok());
        assert!(check_deposit_readiness(root, true).is_ok());
    }

    /// `unimplemented:` was already exempt from the required failures — pin that
    /// it stays exempt AND is now NAMED in `unverified_obligations` rather than
    /// vanishing silently.
    #[test]
    fn unimplemented_obligation_stays_non_blocking() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_validation_reports(
            root,
            &[
                serde_json::json!({"task_id":"t1","obligation_id":"ob_ok","outcome":"passed"}),
                serde_json::json!({"task_id":"t2","obligation_id":"ob_todo","outcome":"unimplemented:foo_check"}),
            ],
        );

        assert!(scan_contract_obligations(root).is_empty());
        let summary = scan_domain_validation(root);
        assert!(summary.failed_tasks.is_empty(), "{summary:?}");
        assert!(summary.required_failures.is_empty(), "{summary:?}");
        assert_eq!(summary.status(), CheckStatus::Pass);
        assert_eq!(
            summary.unverified_obligations,
            vec!["t2.ob_todo (unimplemented:foo_check)"]
        );
        assert!(compute_deposit_ready(
            "full",
            CheckStatus::Pass,
            CheckStatus::Pass,
            summary.status(),
            ReexecStatus::Partial
        ));
    }

    /// An obligation set that reached NO verdict at all inspected NOTHING, so
    /// the axis must attest `Unverified` — rendering it `Pass` would be the same
    /// vacuity bug `vacuous_scan_is_not_a_pass` pins for an empty scan. Still
    /// non-blocking on both gate layers.
    #[test]
    fn only_unverified_obligations_yields_unverified_axis() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_validation_reports(
            root,
            &[
                serde_json::json!({"task_id":"t1","obligation_id":"ob_err","outcome":"errored:input absent"}),
                serde_json::json!({"task_id":"t2","obligation_id":"ob_todo","outcome":"unimplemented:bar_check"}),
            ],
        );

        let summary = scan_domain_validation(root);
        assert!(
            summary.checked_tasks.is_empty(),
            "no obligation reached a concrete verdict, so nothing was inspected: {summary:?}"
        );
        assert!(summary.failed_tasks.is_empty(), "{summary:?}");
        assert_eq!(summary.unverified_obligations.len(), 2, "{summary:?}");
        assert_eq!(
            summary.status(),
            CheckStatus::Unverified,
            "an all-no-verdict obligation set must not attest a pass: {summary:?}"
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
        assert_eq!(dr.domain_validation, CheckStatus::Unverified, "{dr:?}");
        assert!(
            dr.deposit_ready,
            "Unverified is surfaced, not blocking: {dr:?}"
        );
        assert!(check_deposit_readiness(root, false).is_ok());
    }

    /// The core of the vacuity bug: a scan that inspected NOTHING must be
    /// attested as `Unverified`, never as `Pass`. `Unverified` stays
    /// non-blocking (absence of evidence is not evidence of a defect) but is
    /// surfaced in the attestation detail.
    #[test]
    fn vacuous_scan_is_not_a_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        assert_eq!(
            scan_domain_validation(root).status(),
            CheckStatus::Unverified
        );

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
        assert_eq!(
            dr.domain_validation,
            CheckStatus::Unverified,
            "an axis that inspected nothing must not attest a pass: {dr:?}"
        );
        assert!(
            dr.deposit_ready,
            "Unverified is surfaced, not blocking: {dr:?}"
        );
        assert!(
            dr.detail
                .as_deref()
                .unwrap_or_default()
                .contains("UNVERIFIED"),
            "the attestation must surface the vacuity: {dr:?}"
        );
        assert!(check_deposit_readiness(root, false).is_ok());
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
        // A single passing obligation record is enough evidence for the axis
        // to reach a concrete verdict; without any, it would (correctly) read
        // `Unverified` — see `vacuous_scan_is_not_a_pass`.
        let runtime = tmp.path().join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        fs::write(
            runtime.join("validation-reports.jsonl"),
            r#"{"task_id":"t1","obligation_id":"ob_ok","outcome":"passed"}"#,
        )
        .unwrap();
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
        fs::write(
            root.join("WORKFLOW.json"),
            serde_json::to_vec_pretty(&wf).unwrap(),
        )
        .unwrap();
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
        assert!(
            !s.is_clean(),
            "a Blocked{{ProvenanceDivergence}} task is a divergence"
        );
        assert_eq!(s.divergences.len(), 1);

        // RO-Crate array source.
        let rc = tempfile::tempdir().unwrap();
        write_ro_crate_divergence(rc.path());
        let s = scan_provenance_divergence(rc.path());
        assert!(
            !s.is_clean(),
            "a non-empty ecaax:provenanceDivergence is a divergence"
        );
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
        assert!(
            !dr.deposit_ready,
            "a recorded divergence must block deposit-readiness"
        );
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
        assert!(
            !dr.deposit_ready,
            "a recorded substrate FAIL must block deposit-readiness"
        );
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
            read_deposit_readiness(tmp.path())
                .unwrap()
                .unwrap()
                .substrate_validity,
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
        fs::write(
            root.join("WORKFLOW.json"),
            serde_json::to_vec_pretty(&wf).unwrap(),
        )
        .unwrap();
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
        assert!(
            !s.is_clean(),
            "residual host path + session id are non-portable"
        );

        // Host paths surfaced for inputs.json and only-in-path.json.
        assert!(
            s.host_paths
                .iter()
                .any(|h| h.starts_with("runtime/inputs.json:")
                    && h.contains("/home/a/.ecaa-workflow/himes-inputs")),
            "expected the external SME data root as a host path; got {:?}",
            s.host_paths
        );
        assert!(
            s.host_paths
                .iter()
                .any(|h| h.starts_with("runtime/only-in-path.json:")),
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
            !s.session_id_leaks
                .iter()
                .any(|l| l.starts_with("WORKFLOW.json:")),
            "the declared workflow_id identity must be exempt; got {:?}",
            s.session_id_leaks
        );
    }

    /// A host path pointing into the deposit's OWN recorded root (read from
    /// `determinism-env.json::pkg_root`) is a self-reference that relocates with
    /// the package — it must NOT be reported as a portability residual, while a
    /// genuine EXTERNAL host path (a conda-prefix `.so`) still surfaces. This is
    /// the himes-deposit regression: 96 of 102 raw host-path hits were the
    /// package's own emit root embedded in `agent-code.json` / `error.json`.
    #[test]
    fn scan_portability_excludes_self_reference_keeps_external() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_workflow_id(root);
        let own_root = "/home/a/.ecaa-workflow/packages/mypkg";
        let de_dir = root.join("runtime/outputs/differential_expression");
        fs::create_dir_all(&de_dir).unwrap();
        fs::write(
            de_dir.join("determinism-env.json"),
            format!("{{\"pkg_root\":\"{own_root}\"}}"),
        )
        .unwrap();
        // Recorded agent code embeds BOTH a self-reference into the package's
        // own root (relocatable) AND an external conda-prefix `.so` (a genuine,
        // load-bearing external dependency that must stay flagged).
        fs::write(
            de_dir.join("agent-code.json"),
            format!(
                "{{\"out\":\"{own_root}/runtime/outputs/differential_expression/view_data/volcano_data.tsv\",\
                  \"blas\":\"/home/a/miniconda3/envs/bioc/lib/libopenblas.so\"}}"
            ),
        )
        .unwrap();

        let s = scan_portability(root);
        assert!(
            !s.host_paths.iter().any(|h| h.contains(own_root)),
            "self-references into the package's own recorded root must be excluded; got {:?}",
            s.host_paths
        );
        assert!(
            s.host_paths
                .iter()
                .any(|h| h.contains("/home/a/miniconda3/envs/bioc/lib/libopenblas.so")),
            "an external conda-prefix path must still be flagged; got {:?}",
            s.host_paths
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
        fs::write(
            root.join("CONTEXT.md"),
            "# Context\nRelative path: runtime/outputs/x\n",
        )
        .unwrap();
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
        fs::write(
            root.join("WORKFLOW.json"),
            serde_json::to_vec_pretty(&wf).unwrap(),
        )
        .unwrap();
        fs::write(root.join("CONTEXT.md"), "root: /home/a/data\n").unwrap();
        let s = scan_portability(root);
        assert!(
            s.session_id_leaks.is_empty(),
            "no derivable session id → no session axis"
        );
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
            dr.portability_warnings
                .iter()
                .any(|w| w.contains("/home/a/.ecaa-workflow/himes-inputs")),
            "the residual host path should appear in the warnings; got {:?}",
            dr.portability_warnings
        );
        assert!(
            dr.deposit_ready,
            "a portability WARN alone must NOT flip deposit_ready false"
        );
        assert!(
            dr.detail
                .as_deref()
                .unwrap_or_default()
                .contains("portability warning"),
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
        assert!(
            dr.portability_warnings.is_empty(),
            "clean deposit has no portability warnings"
        );
        assert!(dr.deposit_ready);
    }
}
