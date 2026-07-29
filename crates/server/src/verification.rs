//! Server-side glue around the core claim-verification + finalize modules.
//!
//! The reusable per-task completion orchestration (verify FROM SOURCE →
//! coverage → signed verdict sink) lives in
//! [`ecaa_workflow_core::finalize`] so the harness (which links only against
//! `core`) shares one implementation. This module keeps only the server-only
//! parts: the session-state transitions (`block_on_mismatch`, the recall-gap
//! block), telemetry, the `spawn_blocking` wrapping, and the ablation toggle.
//! [`VerifyOutcome`] / [`TaskVerification`] / [`verify_task_with_context`] /
//! [`coverage_should_block`] / [`assert_default_policy_present`] are
//! re-exported from core so existing server call sites
//! (`chat_routes/tasks/result.rs`, `lib.rs`) keep compiling unchanged.

use ecaa_workflow_core::claim_verifier::ClaimVerificationReport;

pub use ecaa_workflow_core::finalize::{
    assert_default_policy_present, coverage_should_block, finalize_task, finalize_task_verdicts,
    verify_task_with_context, TaskFinalizeOutcome, TaskVerification, VerifyOutcome,
};

/// Transition the session to `Blocked { ValidationFailed }` when a freshly
/// computed verification report contains ≥1 claim mismatch. Shared by the
/// manual `POST /verify` endpoint and the on-completion re-verify hook so
/// both drive identical state transitions and identical blocker payloads.
///
/// No-op when the report has no mismatch. The `block_from_harness` call is
/// best-effort/idempotent: a session that is already `Blocked` (or no longer
/// in an execution state) returns `Err`, which is the benign double-fire
/// case — the earlier blocker stays surfaced.
/// Site 2 of the two-site benchmark toggle (Aim 3A). The live L2 block on
/// claim Mismatch is the headline guardrail; under
/// `ECAA_ABLATE_CLAIM_CONSISTENCY` the ablated arm (B') runs WITHOUT it, so
/// the A-vs-B' contrast attributes the blocker's marginal contribution
/// rather than reducing to an at-rest artifact difference. The recompute and
/// the signed-sink persist still run (the sink carries the Task-1 ablation
/// marker); only the dispatch-gating block is suppressed.
pub(crate) fn block_enforced_under_current_env() -> bool {
    !ecaa_workflow_core::ablation::AblationFlagExt::is_active(
        ecaa_workflow_core::ablation::AblationFlag::ClaimConsistency,
    )
}

pub(crate) async fn block_on_mismatch(
    app: &crate::chat_routes::ChatAppState,
    session_id: uuid::Uuid,
    task_id: &str,
    report: &ClaimVerificationReport,
) {
    if !report.has_mismatch() {
        return;
    }
    let first_mismatch = report
        .verdicts
        .iter()
        .find(|v| {
            matches!(
                &v.status,
                ecaa_workflow_core::claim_verifier::ClaimStatus::Mismatch { .. }
            )
        })
        .map(|v| v.claim.entity.clone())
        .unwrap_or_else(|| "unknown".into());
    let detail = format!(
        "{} claim mismatch(es) detected on completion of task {} (first: {})",
        report.n_mismatch, task_id, first_mismatch
    );
    let kind = ecaa_workflow_core::blocker::BlockerKind::ValidationFailed {
        check: format!("claim_verification:{}", task_id),
        message: detail.clone(),
        cause: None,
    };
    if let Err(e) = app
        .conversation
        .block_from_harness(session_id, task_id.to_string(), detail, kind)
        .await
    {
        // Soft-fail: the session most likely isn't in an execution state
        // anymore (already Blocked), which is the idempotent case.
        tracing::debug!(
            ?session_id,
            %task_id,
            error = %e,
            "block_on_mismatch: block_from_harness no-op"
        );
    }
}

/// Re-run claim verification for a completed task FROM SOURCE and, on
/// mismatch, transition the session to `Blocked { ValidationFailed }`.
/// Shared by the manual `POST /verify` endpoint's completion hook so the
/// agent-writable verification sidecar is never trusted: the report is
/// always recomputed against the package's narrative + result tables.
///
/// Best-effort: returns the recomputed [`VerifyOutcome`] (`Verified` whether
/// or not it found a mismatch), or `None` when the session/package is gone or
/// the blocking-pool task panicked. The blocking work runs on
/// `spawn_blocking` so the regex + bounded-fs walk never ties up an async
/// worker, mirroring the GET handler's live-verify path.
pub async fn reverify_and_block_on_mismatch(
    app: &crate::chat_routes::ChatAppState,
    session_id: uuid::Uuid,
    task_id: &str,
) -> Option<VerifyOutcome> {
    let session = app.conversation.get_session(session_id).await?;
    let root = session.emitted_package_path.clone()?;
    let config_dir = crate::chat_routes::config_dir_or_default();
    let project_class = session.project_class;
    let decisions = session.decisions.clone();
    let is_confirmatory = session.mode.is_confirmatory();
    let secret = session.audit_writer_secret;
    let root_c = root.clone();
    let task_c = task_id.to_string();

    // Package-wide graph registration, audit generation, and manifest re-seal
    // run once in the harness end-of-run finalizer. Keeping them out of this
    // request prevents task-completion retries from repeating whole-package
    // scans while preserving the synchronous mismatch and recall gates.
    let finalized = tokio::task::spawn_blocking(move || {
        finalize_task_verdicts(
            &root_c,
            &task_c,
            &config_dir,
            project_class,
            &decisions,
            is_confirmatory,
            Some(&secret),
        )
    })
    .await
    .ok()?
    .ok()?;
    let TaskFinalizeOutcome { outcome, coverage } = finalized;

    match &outcome {
        VerifyOutcome::Verified(v) => {
            // Hallucination-proxy telemetry: accumulate claims-checked +
            // mismatches into the session metrics so `claim_mismatch_rate`
            // stays observable on the completion path, not just the manual
            // POST /verify path. Best-effort.
            app.conversation
                .metrics()
                .record_claim_verification(
                    session_id,
                    v.report.n_checked as u64,
                    v.report.n_mismatch as u64,
                )
                .await;
            // Site 2 (Aim 3A): the live L2 block on Mismatch is the headline
            // guardrail. The recompute + signed-sink persist (in finalize_task)
            // run on both arms; the BLOCK is the toggle — the ablated arm (B')
            // observes the Mismatch but does not gate dispatch, so the contrast
            // measures the blocker's marginal contribution rather than an
            // at-rest delta.
            if block_enforced_under_current_env() {
                block_on_mismatch(app, session_id, task_id, &v.report).await;
            }

            // Block on any Required recall gap (absent or unverifiable),
            // reusing `BlockerKind::ValidationFailed` (no new blocker variant).
            // Additive to the existing Mismatch block above. This is part of
            // the same claim-consistency enforcement surface, so Site 2 gates
            // it too: the ablated arm (B') skips the recall-gap block alongside
            // the Mismatch block.
            if let Some(cov) = coverage.as_ref() {
                if coverage_should_block(cov) {
                    let detail = format!(
                        "recall gap on task {}: {} required claim(s) absent, {} unverifiable",
                        task_id, cov.required_absent, cov.required_unverifiable
                    );
                    // Advisory / warn-only mode (default OFF). When
                    // ECAA_HARNESS_CONTRACT_ADVISORY is truthy this
                    // domain-correctness gate becomes a non-blocking
                    // diagnostic: the recall gap is already persisted into the
                    // signed verdict sink + audit-proof report above, so the
                    // task is LEFT completed and the Blocked/ValidationFailed
                    // transition is suppressed. Read via the typed Config
                    // loaded once at startup (not a per-call std::env::var,
                    // which races across the multi-threaded test binary).
                    // Scoped to the coverage gate ONLY — the Mismatch block
                    // above keeps using `block_enforced_under_current_env` so
                    // the ECAA_ABLATE_CLAIM_CONSISTENCY contrast is unchanged.
                    if app.config.harness_contract_advisory {
                        tracing::warn!(
                            target: "contract-advisory",
                            ?session_id,
                            %task_id,
                            "[contract-advisory] {detail} (advisory, not blocking)"
                        );
                    } else if block_enforced_under_current_env() {
                        let kind = ecaa_workflow_core::blocker::BlockerKind::ValidationFailed {
                            check: format!("claim_coverage:{}", task_id),
                            message: detail.clone(),
                            cause: None,
                        };
                        if let Err(e) = app
                            .conversation
                            .block_from_harness(session_id, task_id.to_string(), detail, kind)
                            .await
                        {
                            tracing::debug!(
                                ?session_id,
                                %task_id,
                                error = %e,
                                "coverage block no-op (already blocked)"
                            );
                        }
                    }
                }
            }
        }
        VerifyOutcome::Disabled => {}
        VerifyOutcome::Unavailable { reason } => {
            // A configuration defect on the completion path is just as loud
            // as on the GET path: log it so a CWD/ECAA_CONFIG_DIR
            // misconfiguration that silently disables verification fleet-wide
            // is visible. The load helper already logged at error level too.
            tracing::error!(
                target: "verification",
                ?session_id,
                %task_id,
                %reason,
                "on-completion re-verify: interpretation policy unavailable — verification not run"
            );
        }
    }
    Some(outcome)
}

#[cfg(test)]
mod signed_sink_wiring_tests {
    use ecaa_workflow_core::audit_writer::AuditWriter;
    use ecaa_workflow_core::claim_contract::ClaimContract;
    use ecaa_workflow_core::claim_extractor::Claim;
    use ecaa_workflow_core::claim_sink::{persist_signed_verdicts, SIGNED_SINK_REL};
    use ecaa_workflow_core::claim_verifier::{
        ClaimStatus, ClaimStrength, ClaimVerdict, ClaimVerificationReport,
    };

    #[test]
    fn persisted_sink_is_verifiable_with_session_secret() {
        let dir = tempfile::tempdir().unwrap();
        // Simulate the per-session secret the server holds.
        let secret = [7u8; 32];
        let writer = AuditWriter::with_secret(secret);
        let c = Claim {
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
                claim: c,
                status: ClaimStatus::Verified,
                strength: ClaimStrength::default(),
                audit: None,
            }],
            runtime_decision_log_path: None,
        };

        persist_signed_verdicts(dir.path(), "diff_expr", &rep, None, &writer).unwrap();

        // A reader reconstructing the writer from the same secret verifies it.
        let reader = AuditWriter::with_secret(secret);
        let line = std::fs::read_to_string(dir.path().join(SIGNED_SINK_REL)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert!(reader.verify_row(&parsed).is_ok());
    }
}

#[cfg(test)]
mod recall_wiring_tests {
    use super::coverage_should_block;
    use ecaa_workflow_core::claim_contract::ClaimContract;
    use ecaa_workflow_core::claim_extractor::Claim;
    use ecaa_workflow_core::claim_verifier::{ClaimStatus, ClaimStrength, ClaimVerdict};
    use ecaa_workflow_core::coverage::{reconcile_coverage, EntityCoverage};
    use ecaa_workflow_core::expected_claim::{ExpectedClaim, ExpectedClaimManifest, Requirement};

    #[test]
    fn required_absent_yields_blocking_coverage() {
        let manifest = ExpectedClaimManifest {
            schema_version: "1".into(),
            entries: vec![ExpectedClaim {
                entity: "differential_expression".into(),
                contrast: None,
                expected_output_table: Some("differential_expression".into()),
                requirement: Requirement::Required,
                edam_data: None,
            }],
        };
        let cov = reconcile_coverage(&manifest, &[]);
        assert_eq!(cov.required_absent, 1);
        assert!(
            coverage_should_block(&cov),
            "Required-absent must drive the ValidationFailed block"
        );
    }

    #[test]
    fn all_addressed_does_not_block() {
        let manifest = ExpectedClaimManifest {
            schema_version: "1".into(),
            entries: vec![ExpectedClaim {
                entity: "differential_expression".into(),
                contrast: None,
                expected_output_table: Some("differential_expression".into()),
                requirement: Requirement::Required,
                edam_data: None,
            }],
        };
        let verdict = ClaimVerdict {
            claim: Claim {
                entity: "differential_expression".into(),
                direction: None,
                effect_size: None,
                pvalue: None,
                source_table: Some("differential_expression".into()),
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
            },
            status: ClaimStatus::Verified,
            strength: ClaimStrength::Exploratory,
            audit: None,
        };
        let cov = reconcile_coverage(&manifest, &[verdict]);
        assert_eq!(cov.required_addressed, 1);
        assert!(!coverage_should_block(&cov));
        let _ = EntityCoverage::Addressed; // touch the import
    }
}

#[cfg(test)]
mod recall_gate_end_to_end_tests {
    //! F5 floor — LIVE-GATE end-to-end coverage. The function-boundary tests
    //! in `coverage.rs` / this file call `reconcile_coverage` directly; these
    //! drive the real server verify+persist path
    //! (`reverify_and_block_on_mismatch` → `verify_task_with_context`) over a
    //! real package on disk whose per-package interpretation policy carries a
    //! NON-EMPTY `verifiableEntities.expected` (one Required entry) and whose
    //! Completed task wrote NO narrative and a `result.json` with NO `claims[]`.
    //!
    //! Before the fix, `verify_task_with_context` short-circuited to
    //! `Disabled` (no narrative + zero structured claims) BEFORE coverage ran;
    //! the `Disabled` arm in `reverify_and_block_on_mismatch` is a no-op, so no
    //! signed sink was written, no recall-gap block fired, and the at-rest
    //! audit-proof loader fell back to the emit-time stub → Inv 1 Pass. This is
    //! the exact CLEAN-PASS hole F5 claimed was eliminated.
    use super::*;
    use crate::chat_routes::test_support::{config_dir, seed_session_with_completed_task};
    use ecaa_workflow_core::audit_proof::{
        run_audit_proof_with_verifier, InvariantId, InvariantStatus,
    };
    use ecaa_workflow_core::audit_writer::AuditWriter;
    use ecaa_workflow_core::expected_claim::{
        inject_manifest_into_policy, ExpectedClaim, ExpectedClaimManifest, Requirement,
    };
    use ecaa_workflow_core::project_class::ProjectClass;
    use std::fs;
    use std::path::Path;

    /// Build a package tree the live gate reads: copy the REAL shipped
    /// interpretation policy into `<pkg>/policies/` (exactly what the emitter's
    /// `copy_policies` does), then inject a Required `differential_expression`
    /// expected-claim via the REAL `inject_manifest_into_policy` (exactly what
    /// the emitter does after `copy_policies`). The Completed task writes a
    /// `result.json` with NO `claims[]` array and NO narrative file.
    fn scaffold_package_with_required_manifest_and_empty_result(pkg_root: &Path, task_id: &str) {
        // 1. Per-package policy = the real shipped policy, byte-copied.
        let cfg = config_dir();
        let src_policy = cfg
            .join("downstream-policy")
            .join("interpretation-policy.json");
        let policies_dir = pkg_root.join("policies");
        fs::create_dir_all(&policies_dir).unwrap();
        fs::copy(&src_policy, policies_dir.join("interpretation-policy.json"))
            .expect("copy shipped interpretation-policy.json");

        // 2. Inject a NON-EMPTY Required manifest via the real emitter fn.
        let manifest = ExpectedClaimManifest {
            schema_version: "1".into(),
            entries: vec![ExpectedClaim {
                entity: "differential_expression".into(),
                contrast: None,
                expected_output_table: Some("differential_expression".into()),
                requirement: Requirement::Required,
                edam_data: None,
            }],
        };
        inject_manifest_into_policy(pkg_root, &manifest).expect("inject manifest");

        // Sanity: the per-package manifest the live gate reads is non-empty.
        let raw = fs::read_to_string(policies_dir.join("interpretation-policy.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let expected = v["verifiableEntities"]["expected"].as_array().unwrap();
        assert_eq!(expected.len(), 1, "manifest must carry one Required entry");

        // 3. Completed task: a result.json with NO `claims[]` array, NO
        //    narrative (.md/.txt) file. Canonical outputs layout.
        let task_dir = pkg_root.join("runtime").join("outputs").join(task_id);
        fs::create_dir_all(&task_dir).unwrap();
        fs::write(
            task_dir.join("result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "status": "ok",
                "metric": 42
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn empty_claims_plus_required_manifest_blocks_and_fails_inv1_end_to_end() {
        // Pin config + ensure the claim-consistency enforcement is NOT ablated
        // (Site 2 / Site 1 both gate the block + signed-sink content on this).
        let cfg = config_dir();
        std::env::set_var("ECAA_CONFIG_DIR", &cfg);
        std::env::remove_var("ECAA_ABLATE_CLAIM_CONSISTENCY");

        let task_id = "differential_expression";
        let pkg = tempfile::tempdir().unwrap();
        scaffold_package_with_required_manifest_and_empty_result(pkg.path(), task_id);

        // --- Sub-assertion A: the LIVE gate no longer short-circuits to
        //     Disabled. verify_task_with_context must now return Verified
        //     (carrying the empty report) so the Verified arm can run. ---
        let direct = verify_task_with_context(
            pkg.path(),
            task_id,
            &cfg,
            ProjectClass::Bioinformatics,
            &[],
            false,
        );
        assert!(
            matches!(direct, VerifyOutcome::Verified(_)),
            "live gate must NOT return Disabled when the package manifest carries \
             a Required entry and the task produced no claims (got {})",
            direct.label()
        );

        // --- Drive the REAL server verify+persist path. ---
        // Set up the app + a session whose emitted_package_path points at the
        // scaffolded package and whose state accepts a HarnessTaskBlocked.
        let dir = tempfile::tempdir().unwrap();
        let store = ecaa_workflow_conversation::SessionStore::open(dir.path())
            .await
            .unwrap();
        let backend: std::sync::Arc<dyn ecaa_workflow_conversation::LlmBackend> =
            std::sync::Arc::new(ecaa_workflow_conversation::MockLlmBackend::new(vec![]));
        let app = crate::chat_routes::ChatAppState::with_backend(backend, store, cfg.clone());

        let session_id =
            seed_session_with_completed_task(&app, task_id, Some(pkg.path().to_path_buf())).await;
        // The seeded session is in Greeting; block_from_harness only accepts
        // execution-side states (Emitted / ReadyToEmit / Amending / Blocked /
        // Intake / IntakeFollowup). Move it to Emitted so the recall-gap block
        // can transition it to Blocked { ValidationFailed }.
        app.conversation
            .store_handle()
            .update(session_id, |s| {
                s.state = ecaa_workflow_conversation::SessionState::Emitted;
                Ok(())
            })
            .await
            .unwrap();

        // Capture the per-session secret BEFORE the call so we can reconstruct
        // the writer and independently verify the signed sink + re-run audit
        // proof with the same key the server used.
        let secret = app
            .conversation
            .get_session(session_id)
            .await
            .unwrap()
            .audit_writer_secret;

        // THE REAL PATH.
        let outcome = reverify_and_block_on_mismatch(&app, session_id, task_id).await;
        assert!(
            matches!(outcome, Some(VerifyOutcome::Verified(_))),
            "reverify must run the Verified arm (coverage recompute + persist)"
        );

        // --- Sub-assertion B: the signed sink was written carrying the
        //     coverage FAILURE block (required_absent == 1). ---
        let writer = AuditWriter::with_secret(secret);
        let sink_path = pkg
            .path()
            .join("runtime/verification-reports/claim-verification.signed.json");
        assert!(
            sink_path.exists(),
            "signed verdict sink must be written on the recall-gap path"
        );
        let line = fs::read_to_string(&sink_path).unwrap();
        let signed: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        let inner = writer
            .verify_row(&signed)
            .expect("signed sink must verify with the session secret");
        assert_eq!(
            inner["coverage"]["required_absent"],
            serde_json::json!(1),
            "sink coverage block must record the Required recall gap"
        );

        // --- Sub-assertion C: the session is Blocked { ValidationFailed }. ---
        let blocked = app.conversation.get_session(session_id).await.unwrap();
        match &blocked.state {
            ecaa_workflow_conversation::SessionState::Blocked { blocker_kind, .. } => {
                assert!(
                    matches!(
                        blocker_kind,
                        Some(ecaa_workflow_core::blocker::BlockerKind::ValidationFailed { .. })
                    ),
                    "recall gap must surface as BlockerKind::ValidationFailed, got {:?}",
                    blocker_kind
                );
            }
            other => panic!(
                "session must be Blocked after the recall gap, got {:?}",
                other
            ),
        }

        // --- Sub-assertion D (the headline): run_audit_proof_with_verifier →
        //     check_claim_completeness (Inv 1) == Fail. ---
        let validator = ecaa_workflow_core::wrroc_validator::NoopWrrocValidator;
        let clock = ecaa_workflow_core::clock::WallClock;
        let report = run_audit_proof_with_verifier(pkg.path(), &validator, &clock, Some(&writer))
            .expect("audit proof must run");
        let inv1 = report
            .verdicts
            .iter()
            .find(|v| v.id == InvariantId::ClaimCompleteness)
            .expect("claim-completeness verdict present");
        assert_eq!(
            inv1.status,
            InvariantStatus::Fail,
            "Inv 1 (claim-completeness) MUST Fail end-to-end on empty-claims + \
             non-empty Required manifest; detail = {:?}",
            inv1.detail
        );

        std::env::remove_var("ECAA_CONFIG_DIR");
    }

    #[test]
    #[serial_test::serial]
    fn required_manifest_for_other_stage_does_not_verify_empty_non_confirmatory_task() {
        // A package-level manifest can contain Required entries for later
        // confirmatory result stages. Completing an earlier operational task
        // must not trigger a recall gap for those future outputs.
        let cfg = config_dir();
        std::env::set_var("ECAA_CONFIG_DIR", &cfg);
        std::env::remove_var("ECAA_ABLATE_CLAIM_CONSISTENCY");

        let pkg = tempfile::tempdir().unwrap();
        scaffold_package_with_required_manifest_and_empty_result(pkg.path(), "data_acquisition");

        let out = verify_task_with_context(
            pkg.path(),
            "data_acquisition",
            &cfg,
            ProjectClass::Bioinformatics,
            &[],
            false,
        );
        assert!(
            matches!(out, VerifyOutcome::Disabled),
            "manifest entries for differential_expression must not force \
             data_acquisition into claim coverage verification, got {}",
            out.label()
        );
        std::env::remove_var("ECAA_CONFIG_DIR");
    }

    #[test]
    #[serial_test::serial]
    fn no_manifest_still_returns_disabled_for_empty_task() {
        // Guard the narrow scope of the fix: when the package carries NO
        // expected manifest (un-anchored task), an empty task still returns
        // Disabled — the Phase-1 verdict-only shape is preserved and we did
        // not turn every empty task into a Verified/blocking outcome.
        let cfg = config_dir();
        std::env::set_var("ECAA_CONFIG_DIR", &cfg);
        std::env::remove_var("ECAA_ABLATE_CLAIM_CONSISTENCY");

        let task_id = "some_task";
        let pkg = tempfile::tempdir().unwrap();
        // Empty task dir, no policies/interpretation-policy.json at all →
        // compute_task_coverage returns None → recall_gap == false → Disabled.
        fs::create_dir_all(pkg.path().join("runtime").join("outputs").join(task_id)).unwrap();

        let out = verify_task_with_context(
            pkg.path(),
            task_id,
            &cfg,
            ProjectClass::Bioinformatics,
            &[],
            false,
        );
        assert!(
            matches!(out, VerifyOutcome::Disabled),
            "empty task with no package manifest must stay Disabled, got {}",
            out.label()
        );
        std::env::remove_var("ECAA_CONFIG_DIR");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn advisory_on_suppresses_recall_gap_block_but_still_persists_sink() {
        // Broadened ECAA_HARNESS_CONTRACT_ADVISORY: when ON, the claim_coverage
        // recall-gap gate becomes a non-blocking diagnostic. The session must
        // stay completed (NOT Blocked), while the signed verdict sink still
        // records the gap (durable diagnostic trail, no new sidecar needed).
        let cfg = config_dir();
        std::env::set_var("ECAA_CONFIG_DIR", &cfg);
        std::env::remove_var("ECAA_ABLATE_CLAIM_CONSISTENCY");

        let task_id = "differential_expression";
        let pkg = tempfile::tempdir().unwrap();
        scaffold_package_with_required_manifest_and_empty_result(pkg.path(), task_id);

        let dir = tempfile::tempdir().unwrap();
        let store = ecaa_workflow_conversation::SessionStore::open(dir.path())
            .await
            .unwrap();
        let backend: std::sync::Arc<dyn ecaa_workflow_conversation::LlmBackend> =
            std::sync::Arc::new(ecaa_workflow_conversation::MockLlmBackend::new(vec![]));
        let mut app = crate::chat_routes::ChatAppState::with_backend(backend, store, cfg.clone());
        // Flip the typed advisory flag ON (the verify path reads
        // app.config.harness_contract_advisory, loaded once at startup — here
        // we substitute a test Config carrying the flag).
        app.config = std::sync::Arc::new(
            ecaa_workflow_core::config::Config::for_test()
                .config_dir(cfg.clone())
                .harness_contract_advisory(true)
                .build(),
        );

        let session_id =
            seed_session_with_completed_task(&app, task_id, Some(pkg.path().to_path_buf())).await;
        app.conversation
            .store_handle()
            .update(session_id, |s| {
                s.state = ecaa_workflow_conversation::SessionState::Emitted;
                Ok(())
            })
            .await
            .unwrap();

        let secret = app
            .conversation
            .get_session(session_id)
            .await
            .unwrap()
            .audit_writer_secret;

        let outcome = reverify_and_block_on_mismatch(&app, session_id, task_id).await;
        assert!(
            matches!(outcome, Some(VerifyOutcome::Verified(_))),
            "reverify must still run the Verified arm (coverage recompute + persist)"
        );

        // The signed sink still carries the recall-gap (durable diagnostic).
        let writer = AuditWriter::with_secret(secret);
        let sink_path = pkg
            .path()
            .join("runtime/verification-reports/claim-verification.signed.json");
        assert!(
            sink_path.exists(),
            "signed verdict sink must still be written under advisory mode"
        );
        let line = fs::read_to_string(&sink_path).unwrap();
        let signed: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        let inner = writer
            .verify_row(&signed)
            .expect("signed sink must verify with the session secret");
        assert_eq!(
            inner["coverage"]["required_absent"],
            serde_json::json!(1),
            "sink coverage block must still record the Required recall gap under advisory mode"
        );

        // The session must NOT be Blocked — the gate is advisory-only.
        let after = app.conversation.get_session(session_id).await.unwrap();
        assert!(
            !matches!(
                after.state,
                ecaa_workflow_conversation::SessionState::Blocked { .. }
            ),
            "advisory mode must NOT transition the task to Blocked, got {:?}",
            after.state
        );

        std::env::remove_var("ECAA_CONFIG_DIR");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn advisory_off_still_blocks_on_recall_gap_regression() {
        // Regression guard: with the advisory flag OFF (the production /
        // SME default), the claim_coverage recall gap still hard-blocks.
        let cfg = config_dir();
        std::env::set_var("ECAA_CONFIG_DIR", &cfg);
        std::env::remove_var("ECAA_ABLATE_CLAIM_CONSISTENCY");

        let task_id = "differential_expression";
        let pkg = tempfile::tempdir().unwrap();
        scaffold_package_with_required_manifest_and_empty_result(pkg.path(), task_id);

        let dir = tempfile::tempdir().unwrap();
        let store = ecaa_workflow_conversation::SessionStore::open(dir.path())
            .await
            .unwrap();
        let backend: std::sync::Arc<dyn ecaa_workflow_conversation::LlmBackend> =
            std::sync::Arc::new(ecaa_workflow_conversation::MockLlmBackend::new(vec![]));
        // with_backend builds a test Config with harness_contract_advisory =
        // false by default, so no override is needed for the OFF path.
        let app = crate::chat_routes::ChatAppState::with_backend(backend, store, cfg.clone());
        assert!(
            !app.config.harness_contract_advisory,
            "test Config default must keep advisory OFF for the regression arm"
        );

        let session_id =
            seed_session_with_completed_task(&app, task_id, Some(pkg.path().to_path_buf())).await;
        app.conversation
            .store_handle()
            .update(session_id, |s| {
                s.state = ecaa_workflow_conversation::SessionState::Emitted;
                Ok(())
            })
            .await
            .unwrap();

        let outcome = reverify_and_block_on_mismatch(&app, session_id, task_id).await;
        assert!(
            matches!(outcome, Some(VerifyOutcome::Verified(_))),
            "reverify must run the Verified arm"
        );

        let after = app.conversation.get_session(session_id).await.unwrap();
        match &after.state {
            ecaa_workflow_conversation::SessionState::Blocked { blocker_kind, .. } => {
                assert!(
                    matches!(
                        blocker_kind,
                        Some(ecaa_workflow_core::blocker::BlockerKind::ValidationFailed { .. })
                    ),
                    "advisory OFF must still surface BlockerKind::ValidationFailed, got {:?}",
                    blocker_kind
                );
            }
            other => panic!(
                "advisory OFF: session must be Blocked after the recall gap, got {:?}",
                other
            ),
        }

        std::env::remove_var("ECAA_CONFIG_DIR");
    }
}

#[cfg(test)]
mod site2_ablation_tests {
    // Pure-decision helper: does the live /verify enforce a block on Mismatch?
    // Under ECAA_ABLATE_CLAIM_CONSISTENCY (Site 2) it must NOT block, so the
    // ablated arm runs without the L2 guardrail — the contrast measures the
    // blocker's marginal contribution, not a status flip.
    use super::block_enforced_under_current_env;

    #[test]
    #[serial_test::serial]
    fn block_disabled_under_claim_consistency_ablation() {
        std::env::set_var("ECAA_ABLATE_CLAIM_CONSISTENCY", "1");
        assert!(
            !block_enforced_under_current_env(),
            "Site 2: ablated arm must NOT enforce the live block"
        );
        std::env::remove_var("ECAA_ABLATE_CLAIM_CONSISTENCY");
        assert!(
            block_enforced_under_current_env(),
            "un-ablated arm must enforce the live block"
        );
    }
}
