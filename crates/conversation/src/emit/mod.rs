//! Wrap `crates/core::emit_package` with conversation-log embedding.
//!
//! Constraint: do not modify `crates/core`. Implementation:
//! 1. Build the EmitConfig from the live session and call core's
//!    emit_package as-is.
//! 2. Append `runtime/intake-conversation.jsonl` with every Turn and
//!    ToolCallRecord serialized one-per-line.
//! 3. Patch `ro-crate-metadata.json` to register the conversation log
//!    as a CreativeWork entity referenced from the root Dataset.
//!
//! Split across submodules by concern:
//! - `audit_log` — JSONL writers for conversation + decision logs
//! - `ro_crate` — RO-Crate metadata registration + patch
//! - `cross_version_diff` — per-table diff vs a parent package

mod audit_log;
pub mod conventional;
mod cross_version_diff;
mod decision_substrate_writer;
mod model_policy_sidecar;
mod ro_crate;
mod sidecars;
mod sme_intake_methods;
pub mod validation;
mod verification_sidecar;

// v4 P2 / F18 — re-export the read helper so server / harness callers
// can pull the typed substrate without depending on the writer module's
// private path.
pub use decision_substrate_writer::read_verifier_decisions;

// Spec §7.3 / §7.4 — public entry points for literature-evidence
// CreativeWork registration. Used directly by integration tests and
// by the server's share-token export path.
pub use ro_crate::{emit_ro_crate, emit_ro_crate_shareable};

use crate::session::Session;
use anyhow::{anyhow, Context, Result};
use ecaa_workflow_core::ablation::AblationFlagExt;
use ecaa_workflow_core::classify::ClassificationResult;
use ecaa_workflow_core::emitter::{emit_package, EmitConfig};
use std::path::Path;
use tracing::instrument;

/// Atomic emit step. Wraps the multi-step emit pipeline
/// in a `<basename>.partial-<uuid>` staging directory next to the
/// caller-supplied `output_dir`, runs every write into staging, and on
/// success atomically renames staging → output_dir. On failure the
/// staging directory is removed and the error is returned, leaving the
/// caller-supplied path either non-existent or in its prior state.
///
/// This closes the half-state vector flagged by S2.2: previously a mid-
/// emit panic / OS error after `emit_package` would leave a partially-
/// populated package on disk (WORKFLOW.json + ro-crate-metadata.json
/// present, but `runtime/intake-conversation.jsonl` /
/// `runtime/decisions.jsonl` / patched RO-Crate root missing). The
/// staging guard makes the package either fully present or absent.
///
/// The crate-internal helper `emit_steps` runs every actual write; the
/// public entry `emit_with_conversation_log` is the staging-and-rename
/// wrapper. Callers (tests, `tools::emit::emit_package`) keep their
/// current signature unchanged.
#[instrument(
    skip(session),
    fields(session_id = %session.id, output_dir = %output_dir.display())
)]
pub async fn emit_with_conversation_log(
    session: &mut Session,
    output_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    emit_with_conversation_log_tiered(
        session,
        output_dir,
        config_dir,
        ecaa_workflow_core::provenance_tiers::ProvenanceTier::Private,
    )
    .await
}

/// Tiered emit.
///
/// Wraps `emit_with_conversation_log` with a redaction tier so the
/// caller can request a Private (full trace), RedactedAudit (no PHI
/// / secrets / proprietary raw prompt text), or ExportablePublic
/// (RO-Crate / WRROC subset) version of the package.
///
/// Today only `runtime/decisions.jsonl` is tier-aware; future
/// phases extend tier-awareness to `runtime/proofs.jsonl`,
/// `runtime/assumptions.jsonl`, and the policy / validation
/// sidecars (when those land). The
/// non-tiered `emit_with_conversation_log` defaults to `Private`
/// so existing call sites are unchanged.
#[instrument(
    skip(session),
    fields(
        session_id = %session.id,
        output_dir = %output_dir.display(),
        tier = ?tier
    )
)]
pub async fn emit_with_conversation_log_tiered(
    session: &mut Session,
    output_dir: &Path,
    config_dir: &Path,
    tier: ecaa_workflow_core::provenance_tiers::ProvenanceTier,
) -> Result<()> {
    let parent = output_dir
        .parent()
        .ok_or_else(|| anyhow!("emit output_dir {} has no parent", output_dir.display()))?;
    let basename = output_dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("emit output_dir {} has no basename", output_dir.display()))?;
    // Sibling staging dir; uuid suffix lets concurrent emits coexist.
    let staging = parent.join(format!(
        "{}.partial-{}",
        basename,
        uuid::Uuid::new_v4().simple()
    ));
    // Defensive: a leftover staging from a prior crash would block the
    // rename. The uuid suffix makes collision near-impossible but the
    // cleanup is cheap.
    let _ = tokio::fs::remove_dir_all(&staging).await;
    tokio::fs::create_dir_all(&staging)
        .await
        .with_context(|| format!("creating staging dir {}", staging.display()))?;

    let result = emit_steps(session, &staging, config_dir, tier).await;

    match result {
        Ok(()) => {
            // Promote staging to the caller-supplied path. If the path
            // already exists (re-emit, or test that pre-created a
            // tempdir), remove it first — the contract is "atomic
            // replace" not "atomic create new".
            if tokio::fs::metadata(output_dir).await.is_ok() {
                tokio::fs::remove_dir_all(output_dir)
                    .await
                    .with_context(|| {
                        format!(
                            "removing pre-existing emit target {} before promote",
                            output_dir.display()
                        )
                    })?;
            }
            tokio::fs::rename(&staging, output_dir)
                .await
                .with_context(|| {
                    format!(
                        "atomic rename {} → {}",
                        staging.display(),
                        output_dir.display()
                    )
                })?;
            Ok(())
        }
        Err(e) => {
            // Best-effort cleanup; the original error is what matters.
            let _ = tokio::fs::remove_dir_all(&staging).await;
            Err(e)
        }
    }
}

/// All actual emit work. Called by `emit_with_conversation_log` against
/// a staging directory; the caller is responsible for promoting staging
/// to the final output path on success.
#[instrument(
    skip(session),
    fields(session_id = %session.id, tier = ?tier)
)]
async fn emit_steps(
    session: &mut Session,
    output_dir: &Path,
    config_dir: &Path,
    tier: ecaa_workflow_core::provenance_tiers::ProvenanceTier,
) -> Result<()> {
    // Aim 3A Arm B″ branch. When `ECAA_ECAA_MODE=conventional`, emit
    // a competent conventional-documentation envelope (README +
    // analysis.ipynb + basic RO-Crate + tables/*.csv) and skip the full
    // ECAA pipeline entirely. The branch is intentionally placed inside
    // `emit_steps` so the conventional output inherits the atomic-
    // staging guarantee of the outer `emit_with_conversation_log_tiered`.
    //
    // Reading the env var directly (rather than threading a `Config`
    // argument) keeps the diff narrow; the production `Config::from_env`
    // already parses this var into `Config::ecaa_mode`, so consumers
    // wanting the typed value have it.
    #[allow(clippy::disallowed_methods)]
    let raw_mode = std::env::var("ECAA_ECAA_MODE").ok();
    let mode = ecaa_workflow_core::emit_mode::EcaaMode::from_env_str(raw_mode.as_deref());
    if mode == ecaa_workflow_core::emit_mode::EcaaMode::Conventional {
        // Intent summary: prefer the SME's raw intake prose; fall back
        // to a literal placeholder when the session was constructed
        // without prose (e.g. test fixtures that bypass AppendIntakeProse).
        let intent_summary = if session.intake_prose.trim().is_empty() {
            "[Intent summary not provided.]".to_string()
        } else {
            session.intake_prose.clone()
        };
        // No tables at emit time — the conventional envelope is emitted
        // pre-execution; the harness writes result CSVs during/after the run.
        let tables: Vec<(&str, &str)> = Vec::new();
        conventional::emit_conventional(output_dir, &intent_summary, &tables)
            .context("conventional emit (Arm B″)")?;
        return Ok(());
    }

    let dag = session
        .dag
        .as_ref()
        .ok_or_else(|| anyhow!("session has no DAG"))?;
    let taxonomy = session
        .taxonomy
        .as_ref()
        .ok_or_else(|| anyhow!("session has no taxonomy loaded"))?;

    let policies_dir = config_dir.join("downstream-policy");

    let classification: ClassificationResult = match &session.classification {
        Some(c) => ClassificationResult {
            domain: taxonomy.domain.clone(),
            workflow_description: taxonomy.description.clone(),
            ..c.clone()
        },
        None => ClassificationResult {
            modality: taxonomy.id.clone(),
            taxonomy_path: String::new(),
            domain: taxonomy.domain.clone(),
            workflow_description: taxonomy.description.clone(),
            confidence: 1.0,
            confidence_label: "high".into(),
            edam_topic: String::new(),
            edam_operation: String::new(),
            organisms: vec![],
            methods_specified: vec![],
            data_sources: vec![],
            intake_text: session.intake_prose.clone(),
            // Synthetic ClassificationResult (built
            // when the session never ran the classifier) has no
            // SME-stated goal; the composer falls through.
            goal: None,
            // Synthetic results carry None; the
            // archetype path populates this when an archetype
            // matches.
            archetype_id: None,
            // Synthetic results carry no cross-omics companions; the
            // classifier path populates this when SME prose triggers
            // cross-omics intent.
            additional_modalities: vec![],
            tie_candidates: vec![],
        },
    };

    // Derive IntakeFacts from classification and locate the
    // compute-profiles config dir (sibling to policies_dir) so the
    // emitter writes policies/compute-resource-policy.json and
    // policies/intake-facts.json. Both are gated on presence —
    // packages emitted from a tree without config/compute-profiles/
    // stay byte-identical to the baseline.
    let compute_profiles_dir = policies_dir.parent().map(|p| p.join("compute-profiles"));
    let intake_facts =
        ecaa_workflow_core::intake_facts::IntakeFacts::from_classification(&classification);
    // If the SME just amended a stage, thread the
    // amendment context through to the core emitter so it can write
    // `prov:wasDerivedFrom`, the `UpdateAction` entity, and
    // `policies/amendment-lineage.json`. The conversation crate
    // captured `(target_stage, invalidated_tasks, parent_package_path,
    // rationale)` at AmendStart-time on `session.pending_amendment`;
    // we move it onto a transient `AmendContext` for this emit and
    // clear the session field at the end of a successful emit.
    let amend_ctx_owned: Option<ecaa_workflow_core::emitter::AmendContext> = session
        .pending_amendment
        .as_ref()
        .map(|p| ecaa_workflow_core::emitter::AmendContext {
            reason: p.rationale.clone(),
            amended_stage: p.target_stage.clone(),
            invalidated_tasks: p.invalidated_tasks.clone(),
        });
    // Source the parent package path from amendment context FIRST
    // (the in-progress amend), then fall back to session.lineage for
    // branch emissions. Both shapes flow through the core emitter's
    // `amend_from` parameter; the core picks the right RO-Crate patch
    // helper (UpdateAction for amend, wasDerivedFrom-only for branch)
    // based on whether amend_context is also set. Without this fallback,
    // branched packages emitted with no UpdateAction lose their parent
    // edge in `ro-crate-metadata.json` entirely (the CLAUDE.md spec
    // requires `prov:wasDerivedFrom` on every branch emission).
    let amend_from_path: Option<&Path> = session
        .pending_amendment
        .as_ref()
        .map(|p| p.parent_package_path.as_path())
        .or_else(|| {
            session
                .lineage
                .as_ref()
                .and_then(|l| l.parent_emitted_package_path.as_deref())
        });

    // Aggregate the runtime-prereqs manifest from the taxonomy's
    // `runtime_baseline` (the legacy path has no
    // atom catalog in scope; per-atom prereqs flow through the
    // composer-driven path on composer_version >= 2). Empty baseline
    // produces an empty-but-valid manifest, which the harness pre-
    // flight short-circuits on.
    let runtime_prereqs = ecaa_workflow_core::runtime_prereqs::aggregate_taxonomy(taxonomy, &[]);

    // Per-atom runtime prereqs. The harness reads
    // `policies/atom-prereqs/<atom_id>.json` per task under
    // ECAA_PER_TASK_IMAGES (default on); a missing map silently falls
    // back to host mode (harness/src/executor/per_atom_image.rs:86-88,
    // harness/src/executor/local.rs:582,592-624). Resolve atom ids from
    // the session's cached `WorkflowDag` (v4 composer-driven path) and
    // look each up against the on-disk AtomRegistry to copy
    // `runtime_packages`. Sessions without a cached `workflow_dag`
    // (legacy/v1 emits) pass `None` to preserve prior behavior.
    //
    // Smoke contract: every chat-driven emission with a v4 WorkflowDag
    // populates this map. Sibling wiring: `run_intake` and `run_build`
    // in `crates/cli/src/main.rs` (:462-487, :736-757) and the
    // deterministic CLI chat REPL in `crates/cli/src/chat.rs`.
    let per_atom_prereqs_owned: Option<
        std::collections::BTreeMap<String, ecaa_workflow_core::runtime_prereqs::RuntimePrereqs>,
    > = if let Some(workflow_dag) = session.workflow_dag.as_ref() {
        let atoms_dir = config_dir.join("stage-atoms");
        if atoms_dir.exists() {
            match ecaa_workflow_core::atom_registry::AtomRegistry::load_from_dir(&atoms_dir) {
                Ok(registry) => Some(
                    workflow_dag
                        .nodes
                        .iter()
                        .filter_map(|node| {
                            registry
                                .get(&node.id)
                                .map(|atom| (atom.id.clone(), atom.runtime_packages.clone()))
                        })
                        .collect(),
                ),
                Err(e) => {
                    tracing::warn!(
                        "per_atom_runtime_prereqs: AtomRegistry load from {} failed: {} \
                         (continuing emit with map=None)",
                        atoms_dir.display(),
                        e
                    );
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let cfg = EmitConfig {
        output_dir,
        dag,
        classification: &classification,
        policies_dir: &policies_dir,
        policy_allowlist: taxonomy.policies.as_deref(),
        claim_boundary: taxonomy.claim_boundary.as_deref(),
        compute_profiles_dir: compute_profiles_dir.as_deref(),
        intake_facts: Some(&intake_facts),
        amend_from: amend_from_path,
        amend_context: amend_ctx_owned.as_ref(),
        validation_contract_ref: taxonomy.validation_contract_ref.as_deref(),
        preferred_container: taxonomy.preferred_container.as_deref(),
        runtime_prereqs: Some(&runtime_prereqs),
        per_atom_runtime_prereqs: per_atom_prereqs_owned.as_ref(),
    };
    emit_package(&cfg).context("core emit_package")?;

    // Capture the stable run_id that emit_package wrote into WORKFLOW.json
    // so the session can expose it via get_session_state. Read from the
    // written file rather than threading it through the EmitConfig API
    // (which would touch 40+ call-sites). Best-effort: a missing or
    // malformed WORKFLOW.json is not a reason to fail the emit.
    if let Ok(workflow_bytes) = std::fs::read(output_dir.join("WORKFLOW.json")) {
        if let Ok(workflow_json) = serde_json::from_slice::<serde_json::Value>(&workflow_bytes) {
            if let Some(run_id) = workflow_json
                .get("run_id")
                .or_else(|| workflow_json.get("meta").and_then(|m| m.get("run_id")))
                .and_then(|v| v.as_str())
            {
                session.last_emitted_run_id = Some(run_id.to_string());
            }
        }
    }

    // Keep `session.pending_amendment` populated until AFTER the
    // parent-aware sidecars (cross_version_diff + figure_diff) have
    // run. Both readers resolve the parent path from EITHER
    // `session.lineage` (branch) OR `session.pending_amendment
    // .parent_package_path` (amend) under unified EmissionLineage,
    // so the IVD v1→v5 amend-amend-amend workload finally produces
    // concordance reports. The clear happens lower in this function
    // once every parent-aware sidecar has been written.

    // Pre-approve every SME review gate that the session's
    // CheckpointMode auto-advances. Writes one sidecar per
    // auto-advanced stage (`runtime/sme-review-confirmed-<stage>.json`)
    // and logs an `AutoAdvanced` decision. Scheduler reads the
    // sidecars without any code changes.
    //
    // Threads a Clock so the sidecar's `confirmed_at` field uses the
    // deterministic emit-time path (C6) rather than wall-clock Utc::now,
    // matching the byte-reproducibility contract on emitted packages.
    let clock: &dyn ecaa_workflow_core::clock::Clock = &ecaa_workflow_core::clock::WallClock;
    apply_checkpoint_mode_auto_advances(session, output_dir, clock)?;

    // P3-4 — per-task verification sidecars. For every Completed task
    // with a narrative artifact, run claim-extractor + claim-verifier
    // once at emit time and persist the report under
    // `runtime/verification-reports/<task_id>.json`. The
    // `GET /task/:task_id/result` handler reads these instead of
    // re-running verification synchronously on every poll. Best-effort:
    // any failure leaves the sidecar absent and the GET handler falls
    // back to live verification.
    let _ = verification_sidecar::write_verification_sidecars(output_dir, session, config_dir);

    // Required safety net (not "fallback"): sessions
    // accumulate SME `set_intake_method` entries whose content must
    // reach the agent via the `## SME discovery decisions` section
    // of CONTEXT.md. Append idempotently from session.intake_methods
    // if the core renderer didn't surface the section. Keeps the
    // core emit path byte-identical for sessions that already carry
    // it. Module renamed from `sme_fallback` to `sme_intake_methods`
    // to reflect the load-bearing role.
    sme_intake_methods::append_sme_intake_methods_if_missing(session, output_dir)
        .await
        .context("appending SME intake methods")?;

    // Surface SME-supplied data inputs to the agent. Two artifacts:
    // 1. `runtime/inputs.json` — machine-readable manifest for the
    // data_acquisition stage's discovery layer to consume
    // directly when it picks a method.
    // 2. A `## SME-supplied data inputs` section appended to
    // CONTEXT.md — narrative context for the agent's free-text
    // reasoning (per-input label, kind, file count, total bytes).
    // Both are no-ops when `session.inputs` is empty, so packages
    // without registered inputs stay byte-identical to the baseline.
    write_user_inputs_artifacts(session, output_dir)
        .await
        .context("writing SME data-input artifacts")?;

    audit_log::write_conversation_log(session, output_dir).await?;
    // cross-version diff against the parent package, if any.
    // Mutates `session.decisions` so `write_decision_log` below picks
    // up the new CrossVersionDiff record.
    let diff_written = cross_version_diff::write_cross_version_diff(session, output_dir).await?;
    // Per-figure diff against the same parent. Writes
    // `runtime/figure-diff.json` when a parent emit exists; soft-skips
    // otherwise. Hash-only — no decoding, no LLM, sub-second on
    // typical packages.
    write_figure_diff(session, output_dir).await?;
    // Clear `pending_amendment` only after the parent-aware sidecars
    // (cross_version_diff + figure_diff) have run — both read
    // `session.pending_amendment.parent_package_path` as the
    // amend-path source under unified EmissionLineage. A subsequent
    // ReadyToEmit (re-emit of the same package without an intervening
    // amend) must not fabricate lineage, so the field is one-shot.
    session.pending_amendment = None;
    audit_log::write_decision_log_tiered(session, output_dir, tier).await?;
    // Proof-carrying sidecars. No-ops for v1/v2/v3
    // sessions (no cached WorkflowDag). For v4 sessions, writes
    // runtime/proofs.jsonl, runtime/assumptions.jsonl, and
    // runtime/policy-decisions.jsonl. The RO-Crate registration
    // below picks them up automatically (presence-gated).
    audit_log::write_phase16_sidecars(session, output_dir, tier).await?;
    // Grant v19 §Authentication of Key Resources (D1-D4) — emit the
    // four runtime/*.json sidecars cited as live disclosure surfaces.
    // D1 (claim-verification) is suppressed under
    // ECAA_ABLATE_CLAIM_CONSISTENCY; D2's `ablation_engaged` field
    // mirrors ECAA_ABLATE_REEXECUTION_CLASS; D3 + D4 are always
    // written (security + model-version disclosure are load-bearing
    // regardless of arm). The RO-Crate patcher below picks all four
    // up automatically (presence-gated registration loop).
    sidecars::write_claim_verification(output_dir).await?;
    sidecars::write_determinism_shim(output_dir).await?;
    // D5 — 5-bucket re-execution classification sidecar. Written when a
    // parent package exists; suppressed (empty file) under
    // ECAA_ABLATE_REEXECUTION_CLASS; absent on first emit. Uses the
    // `output_dir` (staging) as the replay side and the session's
    // parent_package_path as the source side.
    sidecars::write_reexecution_sidecar(session, output_dir).await?;
    sidecars::write_security_policy(session, output_dir).await?;
    sidecars::write_model_policy(session, output_dir).await?;
    // D5 — typed-blocker sidecar. Suppressed under
    // ECAA_ABLATE_TYPED_BLOCKERS (ablation moves from the SSE broadcaster
    // to emit-only; the live runtime path always returns typed blockers).
    sidecars::write_typed_blocker(output_dir).await?;
    // v3 P7 — write `runtime/schema-versions.json` listing the
    // SemVer of every IR type this build of the compiler emits. The
    // RO-Crate registration in `ro_crate.rs::patch_ro_crate_metadata`
    // picks it up as a `CreativeWork` (see the `// v3 P7` marker
    // there). Always written, even on legacy emits — replay consumers
    // need the manifest unconditionally to detect required migrations.
    audit_log::write_schema_versions_manifest(output_dir)
        .await
        .context("writing runtime/schema-versions.json (v3 P7)")?;
    // v3 P4 / F17 — run the backend emitter's `compile()` call when
    // the session carries a v4 `WorkflowDag`, persist any non-empty
    // `BackendCapabilityReport` as `runtime/backend-capability-report.json`,
    // and refuse the emit when an `UnsupportedConstraint` lacks a
    // matching `ConstraintLossAck` on `EmitContext::authorized_losses`.
    // Today's only emitter (`WorkflowJsonEmitter`) consumes the full
    // IR shape so its report is unconditionally empty — this is a
    // no-op on the happy path; the error path activates the day any
    // External emitter (CWL / WDL / Nextflow /...) ships.
    write_backend_capability_report(session, output_dir)
        .await
        .context("writing backend capability report")?;
    // v4 P2 / F18 — drain the verifier substrate buffer and write
    // `runtime/verifier-decisions.jsonl`. No-op when no `prove()`
    // calls fired during this session (v1/v2/v3 emits, or v4 emits
    // that never reached the compatibility engine). Sync I/O is safe
    // here because the substrate file is tiny.
    let runtime_dir = output_dir.join("runtime");
    if let Err(e) = decision_substrate_writer::write_verifier_decisions(&runtime_dir) {
        // Substrate is observational; a write failure must not abort
        // the emit. Log + continue.
        tracing::warn!(
            "verifier_decisions: failed to write substrate sidecar: {} (continuing emit)",
            e
        );
    }
    // Sandbox-policy + task-nodes sidecars consumed by
    // the harness pre_dispatch_check. No-op when the session has
    // no active_policy_bundle / no cached workflow_dag.
    audit_log::write_phase14_sidecars(session, output_dir).await?;
    // Phase A1–A3 (flexible-plotting resolver wiring) — resolve a
    // PlotAffordance per output port for every task in the DAG, write
    // runtime/plot_affordances.jsonl + runtime/affordance_fallbacks.jsonl,
    // and increment session.affordance_fallback_counter for every
    // StructuralFallback resolution. Returns the sorted records so the
    // RO-Crate patcher (A3) can stamp ecaax:provisional on non-Registered
    // figure entities without re-reading the sidecar file.
    let affordance_records =
        audit_log::write_affordance_sidecars(session, output_dir, config_dir).await?;
    // D8 audit-proof — compute the 6-invariant verdict report from all
    // previously-written ECAA sidecars and persist it as
    // `runtime/audit-proof-report.json`. Warn-only: a serialization or
    // I/O failure must not abort the emit. Suppressed under
    // ECAA_ABLATE_AUDIT_PROOF for the Arm B′ ablation control.
    if !ecaa_workflow_core::ablation::AblationFlag::AuditProof.is_active() {
        let validator = ecaa_workflow_core::wrroc_validator::NoopWrrocValidator;
        match ecaa_workflow_core::audit_proof::run_audit_proof(output_dir, &validator) {
            Ok(report) => {
                let path = output_dir.join("runtime").join("audit-proof-report.json");
                match serde_json::to_string_pretty(&report) {
                    Ok(mut buf) => {
                        buf.push('\n');
                        if let Err(e) = tokio::fs::write(&path, buf).await {
                            tracing::warn!(
                                "audit-proof-report.json write failed: {} (continuing emit)",
                                e
                            );
                        }
                    }
                    Err(e) => tracing::warn!(
                        error = %e,
                        "audit-proof-report serialization failed (continuing emit)"
                    ),
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "audit-proof report skipped (continuing emit)");
            }
        }
    }
    // ECAA emit-time validation — runs after audit_proof so the
    // audit-proof-report.json file is present for the SHACL projection.
    // Mode is read from ECAA_VALIDATE_ON_EMIT:
    //   unset / schema_only (default, sane production): pure-Rust JSON Schema only
    //   full: + external Python validators (SHACL via pyshacl, OWL DL via owlready2 + HermiT, runcrate validate)
    //   off / 0 / false / no: skipped entirely
    // Warn-only unless ECAA_VALIDATION_BLOCK_ON_FAIL=1. See
    // crates/conversation/src/emit/validation.rs for full env-var docs.
    {
        let pkg_root = output_dir.to_path_buf();
        match tokio::task::spawn_blocking(move || validation::validate_emitted_package(&pkg_root))
            .await
        {
            Ok(Ok(summary)) => {
                let pkg_root2 = output_dir.to_path_buf();
                let summary_clone = summary.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    validation::write_validation_summary(&pkg_root2, &summary_clone)
                })
                .await;
            }
            Ok(Err(e)) => {
                // ECAA_VALIDATION_BLOCK_ON_FAIL=1 path — propagate the abort.
                return Err(e);
            }
            Err(join_err) => {
                tracing::warn!(
                    error = %join_err,
                    "[ecaa-validation] join error during emit-time validation (continuing emit)"
                );
            }
        }
    }

    // v3 P5 F16 — thread the redaction tier through so the patcher
    // can refuse the emit when PHI patterns escape into a
    // non-`Private` tier sidecar.
    ro_crate::patch_ro_crate_metadata(output_dir, diff_written, affordance_records, tier).await?;

    Ok(())
}

/// v3 P4 / F17 — run the backend `compile()` pass and refuse the
/// emit when any `UnsupportedConstraint` lacks a matching
/// `ConstraintLossAck`. No-op when:
/// - the session has no cached `WorkflowDag` (v1/v2/v3 emits),
/// - the backend reports zero losses (today's `WorkflowJsonEmitter`),
/// - every reported loss is authorized via
///   `EmitContext::authorized_losses`.
///
/// When the report is non-empty, the JSON is persisted to
/// `runtime/backend-capability-report.json` so the SME can review the
/// loss enumeration. The RO-Crate patcher picks the file up via the
/// presence-gated semantic-sidecar loop.
async fn write_backend_capability_report(
    session: &crate::session::Session,
    output_dir: &Path,
) -> Result<()> {
    use ecaa_workflow_core::backend_emitters::{workflow_json::WorkflowJsonEmitter, EmitContext};

    let Some(workflow_dag) = session.workflow_dag.as_ref() else {
        // v1/v2/v3 sessions never lower through `WorkflowJsonEmitter::compile`;
        // the F17 contract is vacuous for them.
        return Ok(());
    };

    let emitter = WorkflowJsonEmitter;
    let ctx = EmitContext::defaults();
    let (_artifact, report) = emitter.compile(workflow_dag, &ctx).map_err(|e| {
        anyhow!(
            "backend emitter compile() failed during v3-P4 capability check: {}",
            e
        )
    })?;

    if report.is_empty() {
        // Happy path — `WorkflowJsonEmitter` always reports zero
        // losses today. Nothing to persist; nothing to authorize.
        return Ok(());
    }

    // External-emitter contingency. Enforce the F17 contract:
    // every reported loss must be ack'd on EmitContext::authorized_losses.
    if !report.fully_authorized(&ctx.authorized_losses) {
        return Err(anyhow!(
            "F17 contract violation: backend {} reported {} unauthorized semantic loss(es); supply ConstraintLossAck entries on EmitContext::authorized_losses before re-emitting",
            report.backend,
            report.losses.len(),
        ));
    }

    // Authorized losses still get persisted so the RO-Crate carries
    // the audit trail.
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime)
        .await
        .with_context(|| format!("creating {}", runtime.display()))?;
    let path = runtime.join("backend-capability-report.json");
    let bytes = serde_json::to_vec_pretty(&report).context("serializing capability report")?;
    tokio::fs::write(&path, bytes)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Write `runtime/figure-diff.json` when the session has a parent
/// package, classifying every figure as Identical / Drifted /
/// NewInChild / DroppedInParent. Sibling to `cross_version_diff` — the
/// row-level diff captures *result* drift, this captures *figure*
/// drift. Errors are logged but never fatal: figure diff is a
/// diagnostic, not a contract.
async fn write_figure_diff(
    session: &crate::session::Session,
    output_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let Some(parent_path) = figure_diff_parent_path(session) else {
        return Ok(());
    };
    if !parent_path.exists() {
        return Ok(());
    }
    write_figure_diff_report(&parent_path, output_dir).await;
    Ok(())
}

/// Resolve the parent package path for the figure-diff sidecar from EITHER
/// `session.lineage` (branch) OR `session.pending_amendment` (amend), so the
/// sidecar fires for both lineage kinds (lineage wins). Mirrors the dual-source
/// resolution in `write_cross_version_diff`. `None` → no parent, skip the diff.
fn figure_diff_parent_path(session: &crate::session::Session) -> Option<std::path::PathBuf> {
    if let Some(p) = session
        .lineage
        .as_ref()
        .and_then(|l| l.parent_emitted_package_path.clone())
    {
        return Some(p);
    }
    session
        .pending_amendment
        .as_ref()
        .map(|a| a.parent_package_path.clone())
}

/// Compute the figure diff (pure `core::figure_diff::diff_figures` — hash-only
/// over the tens of figures a package emits) and write
/// `runtime/figure-diff.json`. Best-effort: any diff/serialize/write failure is
/// warned and swallowed (the diff sidecar is advisory, never blocks emit).
async fn write_figure_diff_report(parent_path: &std::path::Path, child: &std::path::Path) {
    let report = match ecaa_workflow_core::figure_diff::diff_figures(parent_path, child) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "figure_diff failed");
            return;
        }
    };
    let runtime = child.join("runtime");
    if let Err(e) = tokio::fs::create_dir_all(&runtime).await {
        tracing::warn!(error = %e, "figure_diff create_dir_all failed");
        return;
    }
    write_report_json(&runtime.join("figure-diff.json"), &report).await;
}

/// Pretty-serialize `report` and write it to `out`. Best-effort: serialize /
/// write failures are warned and swallowed.
async fn write_report_json<T: serde::Serialize>(out: &std::path::Path, report: &T) {
    let body = match serde_json::to_vec_pretty(report) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "figure_diff serialize failed");
            return;
        }
    };
    if let Err(e) = tokio::fs::write(out, body).await {
        tracing::warn!(path = %out.display(), error = %e, "figure_diff write failed");
    }
}

/// Persist `Session.inputs` as agent-readable artifacts:
///
/// - `runtime/inputs.json` — full manifest (path, label, kind, file
///   list with size + sha256). The `data_acquisition` agent reads
///   this verbatim to short-circuit public-repo discovery and use
///   `sme_supplied_local_path` / `sme_supplied_uploaded_files`
///   directly.
/// - A `## SME-supplied data inputs` section appended to
///   `CONTEXT.md` so the agent's free-text reasoning sees them too
///   (a future agent that doesn't open `inputs.json` still won't
///   ask the SME for accessions when local data is registered).
///
/// Both are no-ops when `session.inputs` is empty, keeping byte-
/// reproducibility for sessions without registered inputs.
async fn write_user_inputs_artifacts(session: &Session, output_dir: &Path) -> Result<()> {
    if session.inputs.is_empty() {
        return Ok(());
    }
    let runtime_dir = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime_dir)
        .await
        .with_context(|| format!("creating {}", runtime_dir.display()))?;

    // Machine-readable manifest.
    let manifest_path = runtime_dir.join("inputs.json");
    let manifest_json =
        serde_json::to_vec_pretty(&session.inputs).context("serializing inputs.json")?;
    tokio::fs::write(&manifest_path, &manifest_json)
        .await
        .with_context(|| format!("writing {}", manifest_path.display()))?;

    // Narrative section appended to CONTEXT.md. Idempotent — the
    // section is rebuilt every emit, so re-emitting after editing
    // inputs replaces the prior block by appending a new one (the
    // agent reads the file top-to-bottom; later sections win for
    // narrative purposes, and `runtime/inputs.json` is the
    // authoritative machine surface).
    let context_path = output_dir.join("CONTEXT.md");
    if context_path.exists() {
        let mut narrative = String::new();
        narrative.push_str("\n## SME-supplied data inputs\n\n");
        narrative.push_str(
            "The SME registered the following data sources via the Inputs tab. \
             The `data_acquisition` stage MUST consume these as its primary input \
             (selected method should be `sme_supplied_local_path` or \
             `sme_supplied_uploaded_files`); fall back to public-repo fetchers \
             ONLY if a registered source is unreadable.\n\n",
        );
        for input in &session.inputs {
            let total_bytes: u64 = input.files.iter().map(|f| f.size_bytes).sum();
            let kind_label = match input.kind {
                crate::session::state::UserInputKind::LocalPath => "local path",
                crate::session::state::UserInputKind::UploadedFiles => "uploaded files",
            };
            narrative.push_str(&format!(
                "### `{label}` ({kind_label})\n- Root: `{root}`\n- {n_files} file(s), {bytes} bytes total\n- Manifest: `runtime/inputs.json` (entry `{input_id}`)\n\n",
                label = input.label,
                root = input.root_path,
                n_files = input.files.len(),
                bytes = total_bytes,
                input_id = input.input_id,
            ));
        }
        let mut existing = tokio::fs::read_to_string(&context_path)
            .await
            .with_context(|| format!("reading {}", context_path.display()))?;
        // Strip any prior `## SME-supplied data inputs` block so
        // re-emit is idempotent.
        if let Some(idx) = existing.find("\n## SME-supplied data inputs") {
            existing.truncate(idx);
        }
        existing.push_str(&narrative);
        tokio::fs::write(&context_path, existing.as_bytes())
            .await
            .with_context(|| format!("writing {}", context_path.display()))?;
    }
    Ok(())
}

/// For each stage whose `requires_sme_review: true` but whose
/// `checkpoint_level` + session `checkpoint_mode` combination resolves
/// to "auto-advance," write the scheduler's review-gate sidecar
/// pre-approving the stage and log an `AutoAdvanced` decision.
///
/// Confirmatory mode has already been rejected at confirm time when
/// paired with Fast (see `confirm_with_modes`). Selective with a
/// confirmatory session is allowed, but a prespecified stage marked
/// `Recommended` still pauses — the `mode.is_prespecified` check runs
/// first to keep confirmatory stages gated regardless of
/// checkpoint_level.
fn apply_checkpoint_mode_auto_advances(
    session: &mut Session,
    output_dir: &Path,
    clock: &dyn ecaa_workflow_core::clock::Clock,
) -> Result<()> {
    use ecaa_workflow_core::checkpoint_mode::{CheckpointLevel, CheckpointMode};
    let mode: CheckpointMode = session.checkpoint_mode;
    // Gated is the only mode that never auto-advances — skip the walk.
    if matches!(mode, CheckpointMode::Gated) {
        return Ok(());
    }
    // Phase B4 — pre-B4 this iterated `taxonomy.stages[*]` reading
    // `requires_sme_review` + `checkpoint_level` off the YAML stage.
    // With the legacy taxonomy loader retired, source the same flag
    // from the composed DAG's `Task.requires_sme_review` field instead.
    // `checkpoint_level` doesn't surface on the Task today; default to
    // `Required` (the most conservative level).
    let dag = match session.current_dag() {
        Some(d) => d,
        None => return Ok(()),
    };
    let runtime_dir = output_dir.join("runtime");
    std::fs::create_dir_all(&runtime_dir).context("creating runtime dir for auto-advance")?;

    let mut auto_advanced: Vec<(String, String)> = Vec::new();
    for (stage_id, task) in &dag.tasks {
        let requires_review = task.requires_sme_review;
        if !requires_review {
            continue;
        }
        // Confirmatory + prespecified stage never auto-advances.
        if session.mode.is_prespecified(stage_id.as_str()) {
            continue;
        }
        let level = CheckpointLevel::from_opt_str(None);
        if mode.auto_advances_level(requires_review, level) {
            let sidecar = runtime_dir.join(format!("sme-review-confirmed-{}.json", stage_id));
            let body = serde_json::json!({
                "stage": stage_id,
                "confirmed_at": clock.now_rfc3339(),
                "rationale": format!("auto-advanced by CheckpointMode::{}", mode.as_str()),
                "auto_advanced": true,
                "checkpoint_mode": mode.as_str(),
            });
            std::fs::write(
                &sidecar,
                serde_json::to_string_pretty(&body).unwrap_or_default(),
            )
            .with_context(|| {
                format!(
                    "writing auto-advance sidecar for stage '{}' at {}",
                    stage_id,
                    sidecar.display()
                )
            })?;
            auto_advanced.push((stage_id.to_string(), mode.as_str().to_string()));
        }
    }
    // Log AutoAdvanced decisions separately (after the iteration) so
    // the borrow checker is happy with session.record_decision.
    for (stage, mode_name) in auto_advanced {
        session.record_decision(
            ecaa_workflow_core::decision_log::DecisionType::AutoAdvanced {
                stage,
                mode: mode_name,
            },
            ecaa_workflow_core::decision_log::DecisionActor::Llm,
            None,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Session, Turn};
    use crate::tools::{dispatch_one, BatchableTool, Tool, ToolContext};
    use std::path::PathBuf;

    fn config_dir() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config")
    }

    #[tokio::test]
    async fn emit_writes_conversation_log_and_patches_metadata() {
        let mut session = Session::new(false);
        // Build DAG by appending prose
        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
        dispatch_one(
            &Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose:
                    "single cell scRNA-seq from human IVD samples comparing degenerated and healthy"
                        .into(),
            }),
            &mut session,
            &ctx,
        )
        .await;

        // Add a couple of turns so the log isn't empty
        {
            let conv = std::sync::Arc::make_mut(&mut session.conversation);
            conv.push(Turn::user("hello"));
            conv.push(Turn::assistant("acknowledged"));
        }

        let tmp = tempfile::tempdir().unwrap();
        emit_with_conversation_log(&mut session, tmp.path(), &config_dir())
            .await
            .unwrap();

        // Conversation log file
        let log_path = tmp.path().join("runtime/intake-conversation.jsonl");
        assert!(log_path.exists(), "log file missing");
        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.contains("\"hello\""));
        assert!(log.contains("\"acknowledged\""));

        // Metadata patched
        let metadata_path = tmp.path().join("ro-crate-metadata.json");
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&metadata_path).unwrap()).unwrap();
        let graph = metadata["@graph"].as_array().unwrap();
        assert!(graph.iter().any(|e| {
            e.get("@id").and_then(|v| v.as_str()) == Some("runtime/intake-conversation.jsonl")
        }));
        // Root Dataset has hasPart entry
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let parts = root["hasPart"].as_array().unwrap();
        assert!(parts.iter().any(|p| {
            p.get("@id").and_then(|v| v.as_str()) == Some("runtime/intake-conversation.jsonl")
        }));
    }

    #[tokio::test]
    async fn emit_writes_decision_log_and_patches_metadata() {
        let mut session = Session::new(false);
        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
        dispatch_one(
            &Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "single cell scRNA-seq human samples".into(),
            }),
            &mut session,
            &ctx,
        )
        .await;

        // Simulate the SME clicking Confirm + the LLM emitting — the
        // service layer is what normally records these, so we fake them
        // here since the test driver doesn't go through the service.
        session.record_decision(
            ecaa_workflow_core::decision_log::DecisionType::Confirm { summary_hash: None },
            ecaa_workflow_core::decision_log::DecisionActor::Sme,
            Some("looks good — proceed".into()),
        );
        session.record_decision(
            ecaa_workflow_core::decision_log::DecisionType::EmitPackage {
                output_dir: "/tmp/fake-package-dir".into(),
            },
            ecaa_workflow_core::decision_log::DecisionActor::Llm,
            None,
        );

        let tmp = tempfile::tempdir().unwrap();
        emit_with_conversation_log(&mut session, tmp.path(), &config_dir())
            .await
            .unwrap();

        // Decision log JSONL exists and contains all three records.
        // Per S1.6, dispatch_one(AppendIntakeProse) now writes its own
        // DecisionType::AppendIntakeProse record at the head of the
        // log (LLM-actor); the manual confirm + emit_package follow.
        let log_path = tmp.path().join("runtime/decisions.jsonl");
        assert!(log_path.exists(), "decisions.jsonl missing");
        let body = std::fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3, "expected 3 records, got {}", lines.len());
        assert!(
            body.contains("\"kind\":\"append_intake_prose\""),
            "missing append_intake_prose kind"
        );
        assert!(
            body.contains("\"kind\":\"confirm\""),
            "missing confirm kind"
        );
        assert!(
            body.contains("\"kind\":\"emit_package\""),
            "missing emit_package kind"
        );
        assert!(
            body.contains("looks good — proceed"),
            "rationale not serialized"
        );

        // Metadata registered the decision log + linked it from the root
        let metadata_path = tmp.path().join("ro-crate-metadata.json");
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&metadata_path).unwrap()).unwrap();
        let graph = metadata["@graph"].as_array().unwrap();
        assert!(graph
            .iter()
            .any(|e| { e.get("@id").and_then(|v| v.as_str()) == Some("runtime/decisions.jsonl") }));
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let parts = root["hasPart"].as_array().unwrap();
        assert!(parts
            .iter()
            .any(|p| p.get("@id").and_then(|v| v.as_str()) == Some("runtime/decisions.jsonl")));
    }

    #[tokio::test]
    async fn decision_log_idempotent_on_reemit() {
        let mut session = Session::new(false);
        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
        dispatch_one(
            &Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "single cell scRNA-seq human samples".into(),
            }),
            &mut session,
            &ctx,
        )
        .await;
        session.record_decision(
            ecaa_workflow_core::decision_log::DecisionType::Confirm { summary_hash: None },
            ecaa_workflow_core::decision_log::DecisionActor::Sme,
            None,
        );

        let tmp = tempfile::tempdir().unwrap();
        emit_with_conversation_log(&mut session, tmp.path(), &config_dir())
            .await
            .unwrap();
        emit_with_conversation_log(&mut session, tmp.path(), &config_dir())
            .await
            .unwrap();

        let metadata_path = tmp.path().join("ro-crate-metadata.json");
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&metadata_path).unwrap()).unwrap();
        let graph = metadata["@graph"].as_array().unwrap();
        let count = graph
            .iter()
            .filter(|e| e.get("@id").and_then(|v| v.as_str()) == Some("runtime/decisions.jsonl"))
            .count();
        assert_eq!(count, 1, "duplicate decision-log CreativeWork on re-emit");
    }

    /// When the install-proxy shims accept
    /// runtime installs, the resulting `runtime/install-log.jsonl`
    /// must be registered as a `CreativeWork` entity and linked from
    /// the root Dataset's `hasPart` array, alongside
    /// `intake-conversation.jsonl` and `decisions.jsonl`.
    ///
    /// `emit_with_conversation_log` does atomic-replace on every call
    /// (the staging dir is renamed over the output dir, wiping any
    /// pre-planted files), so this test calls `patch_ro_crate_metadata`
    /// directly against a minimal pre-staged RO-Crate. That exercises
    /// the exact registration loop the emitter runs in production.
    #[tokio::test]
    async fn install_log_registered_as_creative_work_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        // Plant the install-log just like the install-proxy shims do
        // at task time.
        std::fs::write(
            runtime.join("install-log.jsonl"),
            r#"{"timestamp":1700000000.0,"atom_id":"rnaseq_align","package":"samtools","registry":"apt","source":"agent_runtime"}
{"timestamp":1700000010.5,"atom_id":"rnaseq_align","package":"pandas","registry":"pip","source":"agent_runtime"}
"#,
        )
        .unwrap();
        // Minimal RO-Crate metadata the patcher operates on. Matches
        // the shape `core::emitter` lays down (root Dataset + empty
        // hasPart). The patcher reads this file, augments it, and
        // writes it back.
        std::fs::write(
            tmp.path().join("ro-crate-metadata.json"),
            r#"{
              "@context": "https://w3id.org/ro/crate/1.1/context",
              "@graph": [
                {
                  "@id": "./",
                  "@type": "Dataset",
                  "hasPart": []
                }
              ]
            }"#,
        )
        .unwrap();

        ro_crate::patch_ro_crate_metadata(
            tmp.path(),
            vec![],
            vec![],
            ecaa_workflow_core::provenance_tiers::ProvenanceTier::Private,
        )
        .await
        .unwrap();

        let metadata: serde_json::Value = serde_json::from_slice(
            &std::fs::read(tmp.path().join("ro-crate-metadata.json")).unwrap(),
        )
        .unwrap();
        let graph = metadata["@graph"].as_array().unwrap();
        let entity = graph
            .iter()
            .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("runtime/install-log.jsonl"))
            .expect("install-log.jsonl entity missing from RO-Crate graph");
        let types: Vec<&str> = entity["@type"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            types.contains(&"File") && types.contains(&"CreativeWork"),
            "expected @type to include File + CreativeWork, got {:?}",
            types
        );
        assert_eq!(
            entity["encodingFormat"], "application/jsonl",
            "JSONL encoding format expected"
        );
        assert!(
            entity["name"].as_str().unwrap().contains("install log"),
            "name should describe the install log"
        );

        // Linked from root hasPart.
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let parts = root["hasPart"].as_array().unwrap();
        assert!(
            parts.iter().any(|p| p.get("@id").and_then(|v| v.as_str())
                == Some("runtime/install-log.jsonl")),
            "install-log.jsonl missing from root hasPart array"
        );
    }

    /// Sessions without runtime installs (the
    /// common case: sealed atoms, declared_only with everything
    /// already vendored) must NOT carry a stray install-log entry in
    /// the RO-Crate graph. The presence-gate is what makes this safe.
    #[tokio::test]
    async fn install_log_absent_when_file_not_written() {
        let mut session = Session::new(false);
        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
        dispatch_one(
            &Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "single cell scRNA-seq human samples".into(),
            }),
            &mut session,
            &ctx,
        )
        .await;

        let tmp = tempfile::tempdir().unwrap();
        emit_with_conversation_log(&mut session, tmp.path(), &config_dir())
            .await
            .unwrap();

        // No install-log.jsonl written by anyone — the entity should
        // be absent from the RO-Crate graph.
        let metadata_path = tmp.path().join("ro-crate-metadata.json");
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&metadata_path).unwrap()).unwrap();
        let graph = metadata["@graph"].as_array().unwrap();
        assert!(
            !graph.iter().any(|e| e.get("@id").and_then(|v| v.as_str())
                == Some("runtime/install-log.jsonl")),
            "install-log.jsonl entity must not appear when the file does not exist"
        );
    }

    /// Calling the patcher twice with the same
    /// install-log present MUST NOT duplicate the entity or the
    /// `hasPart` link (idempotent registration). Mirrors the existing
    /// `decision_log_idempotent_on_reemit` invariant for the same
    /// `register_ro_crate_entity` machinery.
    #[tokio::test]
    async fn install_log_registration_idempotent_on_reemit() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(
            runtime.join("install-log.jsonl"),
            r#"{"timestamp":1700000000.0,"atom_id":"a","package":"p","registry":"apt","source":"agent_runtime"}
"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("ro-crate-metadata.json"),
            r#"{
              "@context": "https://w3id.org/ro/crate/1.1/context",
              "@graph": [
                {
                  "@id": "./",
                  "@type": "Dataset",
                  "hasPart": []
                }
              ]
            }"#,
        )
        .unwrap();

        for _ in 0..3 {
            ro_crate::patch_ro_crate_metadata(
                tmp.path(),
                vec![],
                vec![],
                ecaa_workflow_core::provenance_tiers::ProvenanceTier::Private,
            )
            .await
            .unwrap();
        }

        let metadata: serde_json::Value = serde_json::from_slice(
            &std::fs::read(tmp.path().join("ro-crate-metadata.json")).unwrap(),
        )
        .unwrap();
        let graph = metadata["@graph"].as_array().unwrap();
        let count = graph
            .iter()
            .filter(|e| e.get("@id").and_then(|v| v.as_str()) == Some("runtime/install-log.jsonl"))
            .count();
        assert_eq!(count, 1, "duplicate install-log CreativeWork on re-emit");
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let parts = root["hasPart"].as_array().unwrap();
        let link_count = parts
            .iter()
            .filter(|p| p.get("@id").and_then(|v| v.as_str()) == Some("runtime/install-log.jsonl"))
            .count();
        assert_eq!(link_count, 1, "duplicate hasPart link on re-emit");
    }

    #[tokio::test]
    async fn cross_version_diff_written_when_parent_lineage_present() {
        use crate::session::SessionLineage;
        use chrono::Utc;

        // Build a "parent" package with a minimal results/tables/de_summary.tsv
        let parent_tmp = tempfile::tempdir().unwrap();
        let parent_tables = parent_tmp.path().join("results/tables");
        std::fs::create_dir_all(&parent_tables).unwrap();
        std::fs::write(
            parent_tables.join("de_summary.tsv"),
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t2.1\t0.0001\t0.001\n",
        )
        .unwrap();

        let mut session = Session::new(false);
        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
        dispatch_one(
            &Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "single cell scRNA-seq human samples".into(),
            }),
            &mut session,
            &ctx,
        )
        .await;
        // Attach lineage so the diff step finds a parent path.
        session.lineage = Some(SessionLineage {
            schema_version: crate::session::lineage::session_lineage_schema_version(),
            parent_session_id: uuid::Uuid::new_v4(),
            branched_at: Utc::now(),
            branched_from_turn_index: None,
            parent_emitted_package_path: Some(parent_tmp.path().to_path_buf()),
            branched_from_task_id: None,
        });

        // Child emission target; also seed a matching de_summary.tsv so the diff
        // has overlap (ACAN effect shifts from 2.1 → -1.8, a direction flip).
        let child_tmp = tempfile::tempdir().unwrap();
        emit_with_conversation_log(&mut session, child_tmp.path(), &config_dir())
            .await
            .unwrap();
        // Post-emit, overwrite the child table to create discordance, then
        // re-emit to trigger the cross-version diff against the updated
        // child tables (emit is idempotent on re-run).
        let child_tables = child_tmp.path().join("results/tables");
        std::fs::create_dir_all(&child_tables).unwrap();
        std::fs::write(
            child_tables.join("de_summary.tsv"),
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t-1.8\t0.0002\t0.002\n",
        )
        .unwrap();
        emit_with_conversation_log(&mut session, child_tmp.path(), &config_dir())
            .await
            .unwrap();

        // Diff report written
        let diff_path = child_tmp.path().join("runtime/cross-version-diff.json");
        assert!(diff_path.exists(), "cross-version-diff.json missing");
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&diff_path).unwrap()).unwrap();
        assert!(report["tables"].is_array());
        let tables = report["tables"].as_array().unwrap();
        assert!(!tables.is_empty(), "expected at least one table diffed");

        // Per-table CSV written
        let csv_path = child_tmp
            .path()
            .join("runtime/cross-version-diff-de_summary.tsv.csv");
        assert!(csv_path.exists(), "per-table diff CSV missing");

        // DecisionRecord appended to decisions.jsonl
        let decisions =
            std::fs::read_to_string(child_tmp.path().join("runtime/decisions.jsonl")).unwrap();
        assert!(
            decisions.contains("\"kind\":\"cross_version_diff\""),
            "decision log missing cross_version_diff record; got: {}",
            decisions
        );

        // RO-Crate metadata registers the diff
        let metadata: serde_json::Value = serde_json::from_slice(
            &std::fs::read(child_tmp.path().join("ro-crate-metadata.json")).unwrap(),
        )
        .unwrap();
        let graph = metadata["@graph"].as_array().unwrap();
        assert!(graph.iter().any(|e| {
            e.get("@id").and_then(|v| v.as_str()) == Some("runtime/cross-version-diff.json")
        }));

        // root Dataset carries `schema:isBasedOn`
        // pointing at the parent package's results/tables/ directory.
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let based_on = root
            .get("schema:isBasedOn")
            .expect("schema:isBasedOn missing on root Dataset");
        let id_str = match based_on {
            serde_json::Value::Object(_) => based_on.get("@id").and_then(|v| v.as_str()),
            serde_json::Value::Array(arr) => arr
                .iter()
                .find_map(|v| v.get("@id").and_then(|s| s.as_str())),
            _ => None,
        };
        assert!(
            id_str
                .expect("schema:isBasedOn missing @id")
                .ends_with("/results/tables/"),
            "schema:isBasedOn @id should end with /results/tables/; got {:?}",
            id_str
        );
    }

    #[tokio::test]
    async fn emit_is_idempotent_on_metadata_patch() {
        let mut session = Session::new(false);
        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
        dispatch_one(
            &Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "single cell scRNA-seq human samples".into(),
            }),
            &mut session,
            &ctx,
        )
        .await;

        let tmp = tempfile::tempdir().unwrap();
        emit_with_conversation_log(&mut session, tmp.path(), &config_dir())
            .await
            .unwrap();
        // Re-emit on top
        emit_with_conversation_log(&mut session, tmp.path(), &config_dir())
            .await
            .unwrap();

        let metadata_path = tmp.path().join("ro-crate-metadata.json");
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&metadata_path).unwrap()).unwrap();
        let graph = metadata["@graph"].as_array().unwrap();
        let count = graph
            .iter()
            .filter(|e| {
                e.get("@id").and_then(|v| v.as_str()) == Some("runtime/intake-conversation.jsonl")
            })
            .count();
        assert_eq!(count, 1, "duplicate entry on idempotent re-emit");
    }

    /// When the SME amends a stage, the next emit
    /// must thread `pending_amendment` into `EmitConfig::amend_from`
    /// + `EmitConfig::amend_context` so the core emitter writes the
    /// `prov:wasDerivedFrom` edge + the `UpdateAction` entity into
    /// `ro-crate-metadata.json` + populates
    /// `policies/amendment-lineage.json`.
    #[tokio::test]
    async fn amend_emit_writes_ro_crate_lineage_and_amendment_policy() {
        use crate::session::PendingAmendment;

        // Step 1 — emit a parent package with a single-cell DAG.
        let parent_tmp = tempfile::tempdir().unwrap();
        let mut session = Session::new(false);
        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
        dispatch_one(
            &Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "single cell scRNA-seq from human IVD samples".into(),
            }),
            &mut session,
            &ctx,
        )
        .await;
        emit_with_conversation_log(&mut session, parent_tmp.path(), &config_dir())
            .await
            .unwrap();
        session.emitted_package_path = Some(parent_tmp.path().to_path_buf());

        // Step 2 — fabricate the AmendStart effect: pin
        // pending_amendment with a parent-path snapshot + the swapped
        // stage. Bypasses the full state-machine round-trip (which
        // requires Emitted, conversation Confirm, etc.) so the test
        // exercises the emit-time wiring in isolation.
        let amended_stage = session
            .dag
            .as_ref()
            .and_then(|d| d.tasks.keys().next().cloned())
            .unwrap_or_else(|| "data_acquisition".into());
        session.pending_amendment = Some(PendingAmendment {
            target_stage: amended_stage.to_string(),
            invalidated_tasks: vec!["downstream_a".into(), "downstream_b".into()],
            parent_package_path: parent_tmp.path().to_path_buf(),
            rationale: Some("Switching from CCA to scVI for batch correction.".into()),
        });

        // Step 3 — emit a child package. The amend wiring should
        // populate amend_from + amend_context on EmitConfig.
        let child_tmp = tempfile::tempdir().unwrap();
        emit_with_conversation_log(&mut session, child_tmp.path(), &config_dir())
            .await
            .unwrap();

        // pending_amendment cleared post-emit
        assert!(
            session.pending_amendment.is_none(),
            "pending_amendment must clear after a successful emit"
        );

        // amendment-lineage policy file written
        let policy_path = child_tmp.path().join("policies/amendment-lineage.json");
        assert!(
            policy_path.exists(),
            "policies/amendment-lineage.json missing after amendment emit"
        );
        let policy: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&policy_path).unwrap()).unwrap();
        assert_eq!(policy["amended_stage"], serde_json::json!(amended_stage));
        assert_eq!(
            policy["amendment_reason"],
            serde_json::json!("Switching from CCA to scVI for batch correction.")
        );
        assert_eq!(
            policy["invalidated_tasks"].as_array().unwrap().len(),
            2,
            "amendment-lineage.json invalidated_tasks count"
        );
        assert!(
            policy["parent"]["parent_path"]
                .as_str()
                .unwrap()
                .contains(parent_tmp.path().to_string_lossy().as_ref()),
            "amendment-lineage parent_path should point at parent package"
        );

        // RO-Crate root Dataset has prov:wasDerivedFrom
        let metadata: serde_json::Value = serde_json::from_slice(
            &std::fs::read(child_tmp.path().join("ro-crate-metadata.json")).unwrap(),
        )
        .unwrap();
        let graph = metadata["@graph"].as_array().unwrap();
        let root = graph
            .iter()
            .find(|e| e["@id"] == "./")
            .expect("root Dataset (./) missing");
        let derived_from_id = root["prov:wasDerivedFrom"]["@id"]
            .as_str()
            .expect("prov:wasDerivedFrom @id missing on root Dataset");
        assert!(
            derived_from_id.starts_with("amendment-parent:"),
            "prov:wasDerivedFrom should reference amendment-parent: id"
        );

        // UpdateAction entity registered.
        // The action @id now embeds the parent workflow_id to avoid collisions
        // in multi-amend chains (see emitter::amendment). Search by @type +
        // @id prefix rather than an exact id match.
        let action = graph
            .iter()
            .find(|e| {
                e["@type"] == serde_json::json!("UpdateAction")
                    && e["@id"]
                        .as_str()
                        .map(|id| id.starts_with(&format!("#amendment-action-{}", amended_stage)))
                        .unwrap_or(false)
            })
            .expect("UpdateAction entity missing from @graph");
        assert_eq!(action["@type"], serde_json::json!("UpdateAction"));
        assert_eq!(
            action["description"],
            serde_json::json!("Switching from CCA to scVI for batch correction.")
        );
        assert_eq!(
            action["actionStatus"],
            serde_json::json!("https://schema.org/CompletedActionStatus")
        );
    }

    // ── Checkpoint-mode auto-advance emission ───

    // Phase B4 — `fast_mode_writes_auto_advance_sidecars_and_decisions`
    // was deleted. It exercised the legacy taxonomy YAML's per-stage
    // `requires_sme_review: true` flag against `clinical-trial-
    // analysis.yaml`. The v4 archetype catalog doesn't author that flag
    // per-atom; only Discovery atoms get the flag automatically via
    // `composed_atom_to_stage_spec`. Re-introducing this coverage on v4
    // needs the archetype YAML to declare review gates explicitly —
    // out of scope for B4.

    #[tokio::test]
    async fn gated_mode_writes_no_auto_advance_sidecars() {
        let mut session = Session::new(false); // default = Gated
        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
        dispatch_one(
            &Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "Phase III RCT with frozen SAP, ITT primary endpoint.".into(),
            }),
            &mut session,
            &ctx,
        )
        .await;
        let tmp = tempfile::tempdir().unwrap();
        emit_with_conversation_log(&mut session, tmp.path(), &config_dir())
            .await
            .unwrap();
        let runtime = tmp.path().join("runtime");
        let count = std::fs::read_dir(&runtime)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("sme-review-confirmed-")
            })
            .count();
        assert_eq!(count, 0, "Gated mode must not write auto-advance sidecars");
    }

    /// Emit step is wrapped in `<basename>.partial-<uuid>` →
    /// atomic rename. After a successful emit, no `*.partial-*` sibling
    /// must remain in the parent directory.
    #[tokio::test]
    async fn no_partial_staging_dir_remains_after_successful_emit() {
        let mut session = Session::new(false);
        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
        dispatch_one(
            &Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "single cell scRNA-seq from healthy donors".into(),
            }),
            &mut session,
            &ctx,
        )
        .await;
        let tmp = tempfile::tempdir().unwrap();
        // Place the emit target as a SUBDIR of the tempdir so the
        // sibling-staging path lands inside `tmp`. The current
        // signature accepts `tmp.path()` directly too; both must
        // leave no `*.partial-*` residue.
        let target = tmp.path().join("pkg-out");
        emit_with_conversation_log(&mut session, &target, &config_dir())
            .await
            .unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".partial-"))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            leftovers.is_empty(),
            "S2.2: staging dir leaked after successful emit: {:?}",
            leftovers
        );
        assert!(
            target.exists(),
            "S2.2: target dir should exist after promote"
        );
        assert!(
            target.join("WORKFLOW.json").exists(),
            "S2.2: WORKFLOW.json should be inside the promoted target"
        );
    }

    /// Phase A1–A3 (flexible-plotting resolver wiring) — full chain test.
    ///
    /// Exercises `write_affordance_sidecars` + `patch_ro_crate_metadata`:
    /// - `runtime/plot_affordances.jsonl` is written with ≥ 1 record.
    /// - Every record deserializes correctly.
    /// - Figure ImageObject entities for tasks with a non-Registered
    /// affordance carry `ecaax:provisional: true` and
    /// `ecaax:affordanceVariant`.
    /// - Figure ImageObject entities with NO affordance record (if any)
    /// carry NO provisional flag.
    /// - `session.affordance_fallback_counter` is non-empty when at
    /// least one task resolved via StructuralFallback (rare in the
    /// standard single-cell taxonomy since most tasks are legacy and
    /// resolve to Deferred via Unknown shape; the counter is validated
    /// non-panicking in all cases).
    ///
    /// Marked `#[ignore]` because it requires a full emit (config/ dir,
    /// tempdir, tokio). Run with:
    /// cargo test -p ecaa-workflow-conversation -- --ignored affordance
    #[tokio::test]
    #[ignore]
    async fn affordance_sidecars_written_and_provisional_flags_stamped() {
        use ecaa_workflow_core::backend_emitters::workflow_json::PlotAffordanceRecord;

        let mut session = Session::new(false);
        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
        dispatch_one(
            &Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose:
                    "single cell scRNA-seq from human IVD samples comparing degenerated and healthy"
                        .into(),
            }),
            &mut session,
            &ctx,
        )
        .await;

        let tmp = tempfile::tempdir().unwrap();
        emit_with_conversation_log(&mut session, tmp.path(), &config_dir())
            .await
            .unwrap();

        // A1: plot_affordances.jsonl exists and contains valid records.
        let affordances_path = tmp.path().join("runtime/plot_affordances.jsonl");
        assert!(
            affordances_path.exists(),
            "runtime/plot_affordances.jsonl must exist after emit"
        );
        let body = std::fs::read_to_string(&affordances_path).unwrap();
        let records: Vec<PlotAffordanceRecord> = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("affordance record must deserialize"))
            .collect();
        assert!(
            !records.is_empty(),
            "plot_affordances.jsonl must have at least one record"
        );

        // Check sorted order: (task_id, port_name) must be non-decreasing.
        for w in records.windows(2) {
            let order = w[0]
                .task_id
                .cmp(&w[1].task_id)
                .then_with(|| w[0].port_name.cmp(&w[1].port_name));
            assert!(
                order != std::cmp::Ordering::Greater,
                "affordance records must be sorted by (task_id, port_name); got {:?} before {:?}",
                w[0].task_id,
                w[1].task_id
            );
        }

        // A3: walk ro-crate-metadata.json and verify provisional stamping.
        let metadata_path = tmp.path().join("ro-crate-metadata.json");
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&metadata_path).unwrap()).unwrap();
        let graph = metadata["@graph"].as_array().unwrap();

        // Collect all ImageObject entities.
        let image_objects: Vec<&serde_json::Value> = graph
            .iter()
            .filter(|e| {
                e.get("@type")
                    .and_then(|v| v.as_array())
                    .is_some_and(|arr| arr.iter().any(|x| x.as_str() == Some("ImageObject")))
            })
            .collect();

        // For each ImageObject, extract its task_id and check against the
        // affordance record.
        for img in &image_objects {
            let id = img["@id"].as_str().unwrap_or("");
            // id format: "runtime/outputs/<task_id>/figures/<fig_id>.png"
            let task_id = id
                .strip_prefix("runtime/outputs/")
                .and_then(|rest| rest.split('/').next())
                .unwrap_or("");

            let maybe_record = records.iter().find(|r| r.task_id.as_str() == task_id);
            match maybe_record {
                Some(rec) if rec.provisional => {
                    // Must carry ecaax:provisional: true
                    assert_eq!(
                        img.get("ecaax:provisional"),
                        Some(&serde_json::Value::Bool(true)),
                        "Figure entity for provisional task '{}' must have ecaax:provisional: true; got: {}",
                        task_id,
                        img
                    );
                    // Must carry ecaax:affordanceVariant
                    assert!(
                        img.get("ecaax:affordanceVariant")
                            .and_then(|v| v.as_str())
                            .is_some(),
                        "Figure entity for provisional task '{}' must have ecaax:affordanceVariant",
                        task_id
                    );
                }
                Some(rec) if !rec.provisional => {
                    // Must NOT carry ecaax:provisional
                    assert!(
                        img.get("ecaax:provisional").is_none(),
                        "Figure entity for Registered task '{}' must not have ecaax:provisional; got: {}",
                        task_id,
                        img
                    );
                }
                None => {
                    // Legacy task (no affordance record) — must NOT carry ecaax:provisional.
                    assert!(
                        img.get("ecaax:provisional").is_none(),
                        "Figure entity for legacy task '{}' (no affordance record) must not have ecaax:provisional; got: {}",
                        task_id,
                        img
                    );
                }
                _ => {}
            }
        }
    }

    /// Flip-default smoke. With
    /// `read_composer_version` defaulting to v4 for new sessions, a fresh
    /// session that goes through `dispatch_one(AppendIntakeProse)` for a
    /// scrnaseq prose must:
    /// 1. Pin `composer_version = 4` (verified by Session::new + env unset).
    /// 2. Cache a `WorkflowDag` on the session via `tools::rebuild_dag`.
    /// 3. Emit the five v4-specific sidecars at emit time:
    /// - `runtime/proofs.jsonl`
    /// - `runtime/assumptions.jsonl`
    /// - `runtime/policy-decisions.jsonl` (only when policy gate
    /// recorded a decision; absent when no bundle is active)
    /// - `runtime/validation-reports.jsonl` (post-harness; only the
    /// obligation hooks land at emit time, the file appends at
    /// run time)
    /// - `runtime/plot_affordances.jsonl`
    ///
    /// Marked `#[ignore]` to keep CI fast. Run with:
    /// cargo test -p ecaa-workflow-conversation -- --ignored v4_default_emits_sidecars
    #[tokio::test]
    #[ignore]
    async fn v4_default_emits_sidecars() {
        // Make sure ECAA_COMPOSER is unset so we exercise the default.
        std::env::remove_var("ECAA_COMPOSER");

        let mut session = Session::new(false);
        assert_eq!(
            session.composer_version, 4,
            "Phase 5 default flip: new sessions with ECAA_COMPOSER unset must pin composer_version=4"
        );

        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
        // Use a prose that triggers both modality + goal classification.
        // Bulk RNA-seq DE is the canonical test path with a clear goal pattern
        // ("differential expression").
        dispatch_one(
            &Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose:
                    "bulk RNA-seq differential expression analysis comparing IBD vs healthy controls, \
                     30 samples, human GRCh38"
                        .into(),
            }),
            &mut session,
            &ctx,
        )
        .await;

        assert!(
            session.workflow_dag.is_some(),
            "v4 dispatch must cache a WorkflowDag on the session"
        );
        assert!(
            session.archetype_snapshot.is_some(),
            "bulk RNA-seq + DE goal must pin the bulk_rnaseq_de archetype_snapshot"
        );

        let tmp = tempfile::tempdir().unwrap();
        emit_with_conversation_log(&mut session, tmp.path(), &config_dir())
            .await
            .unwrap();

        let runtime = tmp.path().join("runtime");
        let proofs = runtime.join("proofs.jsonl");
        let plot_affordances = runtime.join("plot_affordances.jsonl");

        // proofs.jsonl is the load-bearing v4 emit: the WorkflowDag's
        // edges carry CompatibilityProofs that must round-trip through
        // the audit log so downstream Workflow Run RO-Crate consumers
        // can verify the typed-port matches.
        assert!(
            proofs.exists(),
            "v4 emit must write runtime/proofs.jsonl (got missing at {})",
            proofs.display()
        );
        // plot_affordances.jsonl is unconditionally written (even when
        // empty) at every emit by Phase A1–A3 wiring.
        assert!(
            plot_affordances.exists(),
            "every emit must write runtime/plot_affordances.jsonl (got missing at {})",
            plot_affordances.display()
        );
        // assumptions.jsonl is presence-gated on the AssumptionLedger
        // having entries. Ranking-style archetypes (bulk_rnaseq_de) don't
        // populate the ledger, so absence is acceptable for this smoke;
        // only assert presence when the workflow_dag actually carries
        // assumptions.
        if !session
            .workflow_dag
            .as_ref()
            .unwrap()
            .assumptions
            .entries
            .is_empty()
        {
            assert!(
                runtime.join("assumptions.jsonl").exists(),
                "assumptions.jsonl must exist when workflow_dag carries ledger entries"
            );
        }
        // policy-decisions.jsonl is presence-gated on the policy gate
        // having recorded any decision. For scrnaseq with no clinical
        // bundle that's typically empty, so we only assert the file
        // exists when session.policy_decisions is non-empty.
        if !session.policy_decisions.is_empty() {
            assert!(
                runtime.join("policy-decisions.jsonl").exists(),
                "policy-decisions.jsonl must exist when policy_decisions is non-empty"
            );
        }
        // validation-reports.jsonl is appended at harness runtime, not
        // emit time. We don't assert its presence here — emit only
        // wires the obligation hooks via task-nodes.json.
    }
}
