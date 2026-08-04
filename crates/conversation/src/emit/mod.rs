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
mod cross_version_diff;
mod decision_substrate_writer;
mod model_policy_sidecar;
/// Re-exports `render_ro_crate_preview` + `write_ro_crate_preview` from
/// `ecaa_workflow_core::preview`. The implementation (pure function, no
/// HashMap, no clock/RNG) lives in core so it can be called from
/// `finalize_evidence_registration_with_verifier` as the last step before
/// the BagIt reseal.
pub mod preview;
mod ro_crate;
pub mod sidecars;
mod sme_intake_methods;
pub mod validation;

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
use ecaa_workflow_core::dag::{TaskKind, DAG};
use ecaa_workflow_core::emitter::{emit_package, EmitConfig};
use std::path::Path;
use tracing::instrument;

/// Describe the workflow that will actually execute, after every intake-driven
/// prune and rewire has been applied.
///
/// Archetype descriptions are authored against the archetype's full scaffold.
/// Reusing that prose after a downstream-first input removes upstream stages
/// can make a correct DAG claim it will run steps that are no longer present.
/// The final DAG and its `required_input_stage` stamp are the executable
/// authorities, so the emitted description is a deterministic projection of
/// those structures. This is intentionally modality-neutral: every archetype,
/// including inherited and namespaced multi-omics graphs, follows the same
/// rule.
fn executable_workflow_description(dag: &DAG, fallback: &str) -> String {
    let order = if dag.execution_order.is_empty() {
        ecaa_workflow_core::dag::topo_order_ids(dag)
    } else {
        dag.execution_order.clone()
    };
    let stages: Vec<String> = order
        .iter()
        .filter_map(|task_id| {
            let task = dag.tasks.get(task_id)?;
            matches!(task.kind, TaskKind::Computation | TaskKind::Review)
                .then(|| task_id.as_str().replace('_', " "))
        })
        .collect();
    if stages.is_empty() {
        return fallback.trim().to_string();
    }

    let input_substrate = dag.tasks.values().find_map(|task| {
        task.spec
            .as_ref()?
            .get("required_input_stage")?
            .as_str()
            .filter(|value| !value.trim().is_empty())
    });
    match input_substrate {
        Some(substrate) => format!(
            "Executable workflow for input substrate `{substrate}`: {}.",
            stages.join(" -> ")
        ),
        None => format!("Executable workflow stages: {}.", stages.join(" -> ")),
    }
}

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
    // Recompute readiness at emit. `ensure_dag_cached` overlays the latest
    // `task_states` onto the derived DAG; the fresh build's own
    // `propagate_readiness` ran BEFORE that overlay, so a post-amend /
    // post-branch frontier task whose upstream deps are now Completed would
    // otherwise serialize as Pending — and the server's `has_ready_task`
    // gate (`execution/start.rs`) would find no ready task and skip
    // auto-relaunch. Re-run `propagate_readiness` on the overlaid DAG, the
    // same call the harness makes, so the serialized WORKFLOW.json marks
    // the frontier Ready. Deterministic: `propagate_readiness` is a pure
    // function of the task states, so a fresh emit (empty `task_states`,
    // entry tasks already Ready from the build) is byte-unchanged
    // (idempotent).
    session.ensure_dag_cached();
    if let Some(dag) = session.dag.as_mut() {
        dag.propagate_readiness();
    }

    let dag = session
        .dag
        .as_ref()
        .ok_or_else(|| anyhow!("session has no DAG"))?;
    let taxonomy = session
        .taxonomy
        .as_ref()
        .ok_or_else(|| anyhow!("session has no taxonomy loaded"))?;
    let workflow_description = executable_workflow_description(dag, &taxonomy.description);

    let policies_dir = config_dir.join("downstream-policy");

    let classification: ClassificationResult = match &session.classification {
        Some(c) => ClassificationResult {
            domain: taxonomy.domain.clone(),
            workflow_description: workflow_description.clone(),
            ..c.clone()
        },
        None => ClassificationResult {
            modality: taxonomy.id.clone(),
            taxonomy_path: String::new(),
            domain: taxonomy.domain.clone(),
            workflow_description,
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
    let mut intake_facts =
        ecaa_workflow_core::intake_facts::IntakeFacts::from_classification(&classification);
    // Literature contextualization is unconditional — every emitted DAG
    // carries the review_prior_work + contextualize_findings_with_literature
    // atoms, so policies/intake-facts.json records that the package was
    // grounded against prior work.
    intake_facts.literature_review_included = true;
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
    // composer-driven path). Empty baseline
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

    // Stage-atoms dir for the recall anchor: emit loads the AtomRegistry
    // here to derive the catalog-declared confirmatory-atom-id set that
    // anchors `verifiableEntities.expected`. Recomputed (rather than
    // reusing the `atoms_dir` scoped inside the per-atom-prereqs block
    // above) so it is always in scope/lifetime-valid; a non-existent path
    // is harmless (emit's registry load `.ok()` → no anchoring).
    let stage_atoms_dir = config_dir.join("stage-atoms");

    // Maturity stamp: is the chosen archetype experimental (scaffolded /
    // not-production-validated, e.g. the cross-omics archetypes)? Look it
    // up from the catalog by `archetype_id`. A missing archetype_id or an
    // unreadable catalog yields `false` (no stamp) — the stamp is
    // additive provenance, never a reason to fail or alter a production
    // package's byte-baseline.
    let experimental_archetype = classification
        .archetype_id
        .as_deref()
        .map(|id| {
            let archetype_dir = config_dir.join("archetypes");
            ecaa_workflow_core::archetype_registry::ArchetypeRegistry::load_cached(&archetype_dir)
                .map(|reg| reg.is_archetype_experimental(id))
                .unwrap_or(false)
        })
        .unwrap_or(false);

    // WG4b — lift the live session WorkflowDag's typed edge kinds into a
    // node-pair map so the core-written runtime/proofs.jsonl carries the
    // real EdgeKind. (The conversation path also overwrites proofs.jsonl
    // below from the full-fidelity edges in `write_phase16_sidecars`, which
    // already serializes the real per-port kind; this keeps the core-written
    // version consistent for the window before that overwrite.)
    let edge_kinds_owned = session.workflow_dag.as_ref().map(|wd| {
        ecaa_workflow_core::workflow_contracts::edge::edge_kind_map_from_edges(&wd.edges)
    });

    // M4 — derive the atom-registry snapshot id from the on-disk catalog so
    // the full-fidelity workflow-typed.json overwrite is self-describing.
    // Best-effort: a missing/unreadable catalog yields `None` (matches the
    // core path's behavior).
    let atom_snapshot_id = {
        let atoms_dir = config_dir.join("stage-atoms");
        ecaa_workflow_core::atom_registry::AtomRegistry::load_cached(&atoms_dir)
            .ok()
            .map(|reg| reg.snapshot_id())
    };

    // Surface the SME's stated analysis objective (chat-intake prose) into the
    // emitted PROMPT.md so execution agents act on the SME's goal/constraints,
    // not just the generic archetype description (Option A). Empty prose -> None
    // (section omitted). Owned clone keeps the borrow independent of `session`.
    let sme_objective_owned: String = session.intake_prose.trim().to_string();
    let sme_objective: Option<&str> = if sme_objective_owned.is_empty() {
        None
    } else {
        Some(sme_objective_owned.as_str())
    };

    let cfg = EmitConfig {
        output_dir,
        dag,
        classification: &classification,
        policies_dir: &policies_dir,
        policy_allowlist: taxonomy.policies.as_deref(),
        claim_boundary: taxonomy.claim_boundary.as_deref(),
        objective: sme_objective,
        compute_profiles_dir: compute_profiles_dir.as_deref(),
        intake_facts: Some(&intake_facts),
        amend_from: amend_from_path,
        amend_context: amend_ctx_owned.as_ref(),
        validation_contract_ref: taxonomy.validation_contract_ref.as_deref(),
        preferred_container: taxonomy.preferred_container.as_deref(),
        runtime_prereqs: Some(&runtime_prereqs),
        per_atom_runtime_prereqs: per_atom_prereqs_owned.as_ref(),
        stage_atoms_dir: Some(&stage_atoms_dir),
        experimental_archetype,
        edge_kinds: edge_kinds_owned.as_ref(),
        sme_parameter_overrides: Some(&session.sme_parameter_overrides),
        sme_validation_bounds: Some(&session.sme_validation_bounds),
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

    // Note: there is deliberately NO emit-time per-task verification
    // sidecar writer here. The `GET /task/:task_id/result` handler always
    // recomputes verification FROM SOURCE on the blocking pool — the
    // package tree is rw-mounted into the executing agent's container, so
    // trusting a pre-written sidecar would let an adversarial agent
    // overwrite it with an all-clean report and defeat the
    // anti-hallucination contract (see `chat_routes/tasks/result.rs`).
    // The former `verification_sidecar` writer was write-only-never-read
    // dead weight and has been removed.

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

    // Surface SME-supplied data inputs to the agent. Three artifacts:
    // 1. `runtime/inputs.json` — machine-readable manifest for the
    // data_acquisition stage's discovery layer to consume
    // directly when it picks a method.
    // 2. A `## SME-supplied data inputs` section appended to
    // CONTEXT.md — narrative context for the agent's free-text
    // reasoning (per-input label, kind, file count, total bytes).
    // 3. A `## SME-named data inputs NOT found at emit` section plus
    // `runtime/inputs-unavailable.json` for every prose-named path
    // that could not be reconciled into a registration.
    // (1) + (2) stay no-ops when the session registered nothing AND
    // named nothing in prose, so packages without inputs remain
    // byte-identical to the baseline.
    write_user_inputs_artifacts(session, output_dir, clock)
        .await
        .context("writing SME data-input artifacts")?;

    audit_log::write_conversation_log(session, output_dir).await?;
    // cross-version diff against the parent package, if any.
    // Mutates `session.decisions` so `write_decision_log` below picks
    // up the new CrossVersionDiff record.
    let diff_written = cross_version_diff::write_cross_version_diff(session, output_dir).await?;
    // Longitudinal ED/CF delta against the same parent package (RS2).
    // Best-effort: soft-skips when there is no lineage parent or no parent
    // assessment. Excluded from the byte-diff baseline.
    cross_version_diff::write_ed_cf_delta(session, output_dir).await?;
    // Durable catalog-coverage statement (CC1-4) — written only when the
    // session is not fully covered. Excluded from the byte-diff baseline.
    cross_version_diff::write_coverage_statement(session, output_dir).await?;
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
    // W1 — overwrite the core-written runtime/workflow-typed.json with the
    // full-fidelity typed artifact (real ports from the session's
    // WorkflowDag). SAME projection as the core path — never forked. No-op
    // for sessions without a cached WorkflowDag (the core degraded-port
    // companion then stands).
    audit_log::write_workflow_typed(
        session,
        output_dir,
        Some(&classification),
        Some(&intake_facts),
        atom_snapshot_id.clone(),
    )
    .await?;
    // Grant v19 §Authentication of Key Resources (D1-D4) — emit the
    // four runtime/*.json sidecars cited as live disclosure surfaces.
    // D1 (claim-verification) is suppressed under
    // ECAA_ABLATE_CLAIM_CONSISTENCY; D2's `ablation_engaged` field
    // mirrors ECAA_ABLATE_REEXECUTION_CLASS; D3 + D4 are always
    // written (security + model-version disclosure are load-bearing
    // regardless of arm). The RO-Crate patcher below picks all four
    // up automatically (presence-gated registration loop).
    sidecars::write_claim_verification(output_dir).await?;
    sidecars::write_determinism_shim(session, output_dir, config_dir).await?;
    // D5 — 5-bucket re-execution classification sidecar. ALWAYS written
    // (uniform presence): classified buckets when a parent package exists;
    // present-but-empty on first emit and under ECAA_ABLATE_REEXECUTION_CLASS.
    // Invariant 4 reads this file as its Q sub-graph source (empty →
    // Unverified). Uses the `output_dir` (staging) as the replay side and the
    // session's parent_package_path as the source side.
    sidecars::write_reexecution_sidecar(session, output_dir, config_dir).await?;
    sidecars::write_security_policy(session, output_dir, config_dir).await?;
    sidecars::write_dependency_lock(&runtime_prereqs, output_dir).await?;
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
    // Drain only THIS session's bucket (the compose step entered
    // `enter_session(session.id)` around `plan()`), so a concurrent
    // sibling session's in-flight decisions never leak into this
    // package's `verifier-decisions.jsonl`.
    if let Err(e) = decision_substrate_writer::write_verifier_decisions_for_session(
        &runtime_dir,
        &session.id.to_string(),
    ) {
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
    // M5 — durable goal-branch coverage statement (catalog vs proposal).
    // No-op without a cached WorkflowDag. The RO-Crate patcher below
    // registers it via the presence-gated semantic_sidecars loop.
    audit_log::write_coverage_statement(session, output_dir)
        .await
        .context("writing runtime/coverage-statement.json (M5)")?;
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
        match ecaa_workflow_core::audit_proof::run_audit_proof(
            output_dir,
            &validator,
            &ecaa_workflow_core::clock::WallClock,
        ) {
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
    //
    // T12 — a non-empty return means the reconcile pass folded inside
    // `patch_ro_crate_metadata` found `Divergent` reads: task(s) that read
    // a file no declared producer's output directory covers. Transition
    // each into `BlockerKind::ProvenanceDivergence` immediately, while
    // `session` is still in scope — `patch_ro_crate_metadata` itself only
    // owns the RO-Crate `@graph`, not session state.
    let divergences =
        ro_crate::patch_ro_crate_metadata(output_dir, diff_written, affordance_records, tier)
            .await?;
    apply_provenance_divergence_blockers(session, &divergences);

    // RCA I-2 / I-7 — re-seal the BagIt manifest over the package's TRUE
    // final pre-execution state. Everything above this line — every sidecar
    // writer, `patch_ro_crate_metadata`'s descriptor rewrite, and the
    // `runtime/workflow-typed.json` full-fidelity overwrite — mutates a file
    // AFTER core's own `emit_package` call sealed `manifest-sha512.txt`
    // (this pipeline's very first step), with nothing resealing it since.
    // Left alone, that manifest describes the snapshot core sealed before
    // these patches, not the package actually on disk — the same
    // finalization-order failure as `emitter::regenerate_bagit_manifest`'s
    // post-execution twin, just at emit time instead of after execution.
    // `reseal_emit_manifest` keeps `SealMode::Emit` semantics (no
    // `runtime/outputs/`, nothing has executed yet) and does not touch RO-
    // Crate content-integrity annotations — this is a pre-execution package,
    // which carries none yet, and annotating one here would perturb the
    // byte-reproducibility baseline `emit_package` itself is held to.
    let seal_clock: &dyn ecaa_workflow_core::clock::Clock = &ecaa_workflow_core::clock::WallClock;
    ecaa_workflow_core::emitter::reseal_emit_manifest(output_dir, seal_clock)
        .context("re-sealing BagIt manifest over the final emitted payload")?;

    Ok(())
}

/// T12 — transition every task named in `divergences` to
/// `BlockerKind::ProvenanceDivergence` via the same `HarnessTaskBlocked`
/// trigger the harness-driven blocker path uses
/// (`crate::service::ConversationService::block_from_harness`), applied
/// directly against `session` since `emit_steps` already holds `&mut
/// Session` — going through the store-backed service would require a
/// `SessionId` + `ConversationService`, neither of which `emit_steps` has.
///
/// `session.state` is `Emitting` at this call site (the tool dispatcher's
/// pre-handler hook fires `EmitPackageStart` before `emit_package`'s
/// handler — which is what calls into this pipeline — runs), so
/// `try_transition` accepts `HarnessTaskBlocked` from `Emitting` too (see
/// `session/transitions.rs`). The dispatcher's post-handler `EmitPackageOk`
/// trigger fires unconditionally after this returns; from the now-`Blocked`
/// state that transition is illegal and is logged + swallowed
/// (`emit_package_post_ok`'s existing `warn_illegal_transition` path), so
/// the session correctly lands on `Blocked` rather than `Emitted`.
///
/// No-op when `divergences` is empty (the overwhelmingly common case: no
/// harness has dispatched against this package yet). Best-effort per
/// entry: an illegal transition (e.g. multiple divergences for a session
/// already off the accepted-state list) is logged and does not abort the
/// emit — the package has already been written to disk at this point.
fn apply_provenance_divergence_blockers(
    session: &mut Session,
    divergences: &[ecaa_workflow_core::provenance::DivergenceRecord],
) {
    for d in divergences {
        let detail = match &d.declared_producer {
            Some(producer) => format!(
                "read {} does not live under any declared producer's output directory \
                 (the declared graph names {} as the producer for this input)",
                d.read_path, producer
            ),
            None => format!(
                "read {} does not live under any declared producer's output directory",
                d.read_path
            ),
        };
        let blocker_kind = ecaa_workflow_core::blocker::BlockerKind::ProvenanceDivergence {
            task_id: d.task_id.clone(),
            read_path: d.read_path.clone(),
            declared_producer: d.declared_producer.clone(),
        };
        if let Err(e) = session.try_transition(crate::session::StateTrigger::HarnessTaskBlocked {
            task_id: d.task_id.clone(),
            detail,
            blocker_kind,
        }) {
            tracing::warn!(
                session_id = %session.id,
                task_id = %d.task_id,
                error = ?e,
                "provenance-divergence block: illegal state transition ignored"
            );
        }
    }
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
///
/// Before either artifact is written, every prose-named path still
/// sitting in `session.pending_input_hints` is reconciled — see
/// [`reconcile_prose_input_hints`]. A hint that resolves to a real
/// directory becomes a registration (so it reaches `runtime/inputs.json`
/// and therefore the harness bind-mount); a hint that does not becomes a
/// visible "named but not present" note. Neither outcome is silent.
async fn write_user_inputs_artifacts(
    session: &mut Session,
    output_dir: &Path,
    clock: &dyn ecaa_workflow_core::clock::Clock,
) -> Result<()> {
    let unavailable = reconcile_prose_input_hints(session, clock);
    sync_user_inputs_to_package(&session.inputs, output_dir).await?;
    // Written AFTER the registered-inputs sync: that writer truncates
    // CONTEXT.md at its own `## SME-supplied data inputs` marker, so a
    // note appended before it would be silently eaten on the next call.
    write_unavailable_inputs_note(&unavailable, output_dir).await
}

/// One prose-named data location that could NOT be turned into a
/// registered input at emit time.
///
/// The SME named the path in intake prose, so `intake_path_hints`
/// surfaced it as a `Session::pending_input_hints` entry, but at emit
/// the path was gone (or present and un-inventoriable). Recording it is
/// the difference between an executor agent silently swapping in a
/// public dataset and the package stating, on its face, that the
/// SME-named data was not there.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct UnavailableProseInput {
    /// Verbatim token the SME typed in intake prose.
    raw_mention: String,
    /// Canonical directory the hint resolved to when it was extracted.
    canonical_root: String,
    /// Specific file inside `canonical_root` the SME named, when the
    /// prose pointed at a file rather than at the directory.
    file_relpath: Option<String>,
    /// Why the path could not be registered.
    reason: String,
}

/// Default `ECAA_INPUT_ROOTS` when unset. Mirrors
/// `tools::intake::DEFAULT_INPUT_ROOTS_FOR_HINTS` and the server's
/// `chat_routes::inputs::list::DEFAULT_INPUT_ROOTS` — keep the three in
/// sync (the two conversation-side copies exist because the private
/// intake helper is not reachable from this module).
const DEFAULT_INPUT_ROOTS_FOR_RECONCILE: &str = "/home/${USER}/data";

/// Allowlisted roots a reconciled hint must still resolve under.
///
/// Re-read at emit rather than trusted from the intake-time validation
/// baked into the hint: `pending_input_hints` round-trips through the
/// on-disk session JSON, so a hand-edited or migrated session must not
/// be able to smuggle an out-of-jail path onto `session.inputs`. This is
/// the same allowlist `register_input_path` enforces (RC-17 posture:
/// the jail is the last line, not the only line).
fn reconcile_input_roots(owner_user: &str) -> Vec<std::path::PathBuf> {
    let raw = std::env::var("ECAA_INPUT_ROOTS")
        .unwrap_or_else(|_| DEFAULT_INPUT_ROOTS_FOR_RECONCILE.to_string());
    raw.split(':')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.replace("${USER}", owner_user))
        .map(std::path::PathBuf::from)
        .map(|p| p.canonicalize().unwrap_or(p))
        .collect()
}

/// Walk `root` and build the per-file inventory `runtime/inputs.json`
/// carries. Same shape + caps as the REST `register_input_path`
/// manifest builder.
///
/// Deterministic by construction: entries are keyed by relpath in a
/// `BTreeMap`, so the emitted manifest does not inherit the
/// filesystem's directory-iteration order.
fn build_reconciled_manifest(
    root: &std::path::Path,
    selected_relpaths: Option<&std::collections::BTreeSet<String>>,
) -> Result<Vec<crate::session::state::UserInputFile>, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
    const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024 * 1024;
    const MAX_FILES: usize = 50_000;

    let mut files: std::collections::BTreeMap<String, crate::session::state::UserInputFile> =
        std::collections::BTreeMap::new();
    let mut total_bytes: u64 = 0;
    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("canonicalize {}: {e}", root.display()))?;
    let candidate_paths = if let Some(relpaths) = selected_relpaths {
        let mut paths = Vec::with_capacity(relpaths.len());
        for relpath in relpaths {
            let relative = std::path::Path::new(relpath);
            if relative.is_absolute()
                || relative.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                return Err(format!(
                    "named file `{relpath}` is not a safe relative path"
                ));
            }
            let path = canonical_root.join(relative);
            let canonical = path
                .canonicalize()
                .map_err(|e| format!("canonicalize {}: {e}", path.display()))?;
            if !canonical.starts_with(&canonical_root) {
                return Err(format!(
                    "named file {} resolves outside {}",
                    path.display(),
                    canonical_root.display()
                ));
            }
            if !canonical.is_file() {
                return Err(format!("named path {} is not a file", path.display()));
            }
            paths.push(canonical);
        }
        paths
    } else {
        let mut paths = Vec::new();
        for entry in walkdir::WalkDir::new(&canonical_root).follow_links(false) {
            let entry = entry.map_err(|e| format!("walking {}: {e}", canonical_root.display()))?;
            let path = entry.path();
            // Skip dotfiles (but never the root itself, which may live
            // under a dot-directory like `~/.ecaa-workflow/<dir>`).
            if path != canonical_root
                && path
                    .file_name()
                    .and_then(|n: &std::ffi::OsStr| n.to_str())
                    .map(|n: &str| n.starts_with('.'))
                    .unwrap_or(false)
            {
                continue;
            }
            if entry.file_type().is_file() {
                paths.push(path.to_path_buf());
            }
        }
        paths
    };

    for path in candidate_paths {
        let meta = std::fs::metadata(&path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        let size = meta.len();
        if size > MAX_FILE_BYTES {
            return Err(format!(
                "file {} is {size} bytes, over the 4GiB per-file cap",
                path.display()
            ));
        }
        total_bytes = total_bytes.saturating_add(size);
        if total_bytes > MAX_TOTAL_BYTES {
            return Err("registration would exceed the 32GiB total cap".to_string());
        }
        if files.len() >= MAX_FILES {
            return Err(format!(
                "registration would include more than {MAX_FILES} files"
            ));
        }
        let relpath = path
            .strip_prefix(&canonical_root)
            .map_err(|e| format!("strip_prefix {}: {e}", path.display()))?
            .to_string_lossy()
            .into_owned();
        let mut hasher = Sha256::new();
        let mut f =
            std::fs::File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
        let mut buf = [0u8; 8192];
        loop {
            let n = f
                .read(&mut buf)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        files.insert(
            relpath.clone(),
            crate::session::state::UserInputFile {
                relpath,
                size_bytes: size,
                sha256: hex::encode(hasher.finalize()),
            },
        );
    }
    Ok(files.into_values().collect())
}

/// True when an explicitly registered input already carries the exact
/// bytes named by a prose file hint. Browser uploads have a different
/// root from server-local paths, so root equality alone cannot identify
/// this case. Require size, SHA-256, and either the same relative path
/// or the same basename to avoid treating an unrelated equal-sized file
/// as satisfying the declaration.
fn registered_input_covers_file(
    candidate: &crate::session::state::UserInputFile,
    registered: &[crate::session::state::UserInput],
) -> bool {
    let candidate_name = std::path::Path::new(&candidate.relpath).file_name();
    registered
        .iter()
        .flat_map(|input| &input.files)
        .any(|file| {
            file.size_bytes == candidate.size_bytes
                && file.sha256 == candidate.sha256
                && (file.relpath == candidate.relpath
                    || std::path::Path::new(&file.relpath).file_name() == candidate_name)
        })
}

/// Stable 16-hex-char id for an auto-registered input, derived from the
/// canonical root. Deterministic (no `Uuid::new_v4`) so re-emitting the
/// same session produces a byte-identical `runtime/inputs.json`.
fn deterministic_input_id(canonical_root: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(canonical_root.as_bytes());
    hex::encode(hasher.finalize())[..16].to_string()
}

/// Emit-time reconciliation of prose-named input paths.
///
/// An SME who writes "the counts are in /home/me/data/cohort" and then
/// says "just go ahead" never triggers `register_input_path`, so
/// `session.inputs` stays empty, `sync_user_inputs_to_package` no-ops,
/// `runtime/inputs.json` is never written, and `agent-claude.sh` — which
/// builds its container bind-mounts *only* from that file — mounts
/// nothing. The named directory is then genuinely ENOENT inside the
/// agent container and the acquisition stage quietly substitutes a
/// public dataset. This function closes that gap: at emit, every
/// surviving hint is either promoted to a real registration or recorded
/// as unavailable, and both outcomes leave a `DecisionRecord`.
///
/// Returns the hints that could NOT be registered so the caller can
/// surface them in `CONTEXT.md`. Hints that ARE registered are removed;
/// hints that are not stay pending so the SME can still fix the path and
/// register through the Inputs tab.
///
/// No-op when nothing is pending — sessions that registered their inputs
/// normally (or named no paths at all) are unaffected.
fn reconcile_prose_input_hints(
    session: &mut Session,
    clock: &dyn ecaa_workflow_core::clock::Clock,
) -> Vec<UnavailableProseInput> {
    if session.pending_input_hints.is_empty() {
        return Vec::new();
    }
    let roots = reconcile_input_roots(&session.owner_user);
    let owner_user = session.owner_user.clone();
    let now = clock.now();
    // Name the ingestion node on the assumption record only when the
    // composed DAG actually has one — `affects_nodes` is a reference
    // field in the ECAA cross-graph-integrity invariant.
    let ingestion_nodes: Vec<String> = session
        .current_dag()
        .map(|dag| {
            ["data_acquisition", "data_import"]
                .into_iter()
                .filter(|id| dag.tasks.contains_key(*id))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let already_registered: std::collections::BTreeSet<String> =
        session.inputs.iter().map(|i| i.root_path.clone()).collect();

    let mut unavailable: Vec<UnavailableProseInput> = Vec::new();
    let mut registered: Vec<crate::session::state::UserInput> = Vec::new();
    let mut retained: Vec<crate::intake_path_hints::InputPathHint> = Vec::new();
    let mut grouped_hints: std::collections::BTreeMap<
        String,
        Vec<crate::intake_path_hints::InputPathHint>,
    > = std::collections::BTreeMap::new();
    for hint in std::mem::take(&mut session.pending_input_hints) {
        grouped_hints
            .entry(hint.canonical_root.clone())
            .or_default()
            .push(hint);
    }

    for (canonical_root, hints) in grouped_hints {
        // Already registered through the UI / tool path: all hints for
        // this root are stale. Existing registrations are untouched.
        if already_registered.contains(&canonical_root) {
            continue;
        }
        let root = std::path::PathBuf::from(&canonical_root);
        let inside_jail = roots.iter().any(|allowed| {
            let allowed = allowed.canonicalize().unwrap_or_else(|_| allowed.clone());
            root.starts_with(&allowed)
        });
        if !inside_jail {
            // Out of jail: never register it and never echo the path
            // into the package. Leave the hint pending so the SME's own
            // (jailed) registration path can still handle it.
            tracing::warn!(
                session_id = %session.id,
                "reconcile_prose_input_hints: hint root is outside ECAA_INPUT_ROOTS; not registering"
            );
            retained.extend(hints);
            continue;
        }
        let root_reason = if !root.exists() {
            Some("not present on disk at emit".to_string())
        } else if !root.is_dir() {
            Some("present at emit but not a directory".to_string())
        } else {
            None
        };
        if let Some(reason) = root_reason {
            for hint in hints {
                unavailable.push(UnavailableProseInput {
                    raw_mention: hint.raw_mention.clone(),
                    canonical_root: hint.canonical_root.clone(),
                    file_relpath: hint.file_relpath.clone(),
                    reason: reason.clone(),
                });
                // Keep the hint pending: the SME can still restore the
                // path and register it, and a later re-emit will pick
                // it up.
                retained.push(hint);
            }
            continue;
        }

        // A directory mention authorizes inventory of the directory.
        // File-only mentions authorize only those named files. Resolve
        // file hints independently so one file that disappeared after
        // intake does not suppress other still-valid declarations.
        let mut resolved_files: std::collections::BTreeMap<
            String,
            crate::session::state::UserInputFile,
        > = std::collections::BTreeMap::new();
        if hints.iter().any(|hint| hint.file_relpath.is_none()) {
            match build_reconciled_manifest(&root, None) {
                Ok(files) => {
                    for file in files {
                        resolved_files.insert(file.relpath.clone(), file);
                    }
                }
                Err(err) => {
                    let reason = format!("present at emit but could not be inventoried: {err}");
                    for hint in hints {
                        unavailable.push(UnavailableProseInput {
                            raw_mention: hint.raw_mention.clone(),
                            canonical_root: hint.canonical_root.clone(),
                            file_relpath: hint.file_relpath.clone(),
                            reason: reason.clone(),
                        });
                        retained.push(hint);
                    }
                    continue;
                }
            }
        } else {
            for hint in &hints {
                let Some(relpath) = hint.file_relpath.as_ref() else {
                    continue;
                };
                let selected = std::collections::BTreeSet::from([relpath.clone()]);
                match build_reconciled_manifest(&root, Some(&selected)) {
                    Ok(files) => {
                        for file in files {
                            resolved_files.insert(file.relpath.clone(), file);
                        }
                    }
                    Err(err) => {
                        unavailable.push(UnavailableProseInput {
                            raw_mention: hint.raw_mention.clone(),
                            canonical_root: hint.canonical_root.clone(),
                            file_relpath: hint.file_relpath.clone(),
                            reason: format!(
                                "present at emit but named file could not be inventoried: {err}"
                            ),
                        });
                        retained.push(hint.clone());
                    }
                }
            }
        }

        // An explicit browser upload or path registration may already
        // contain the exact named bytes under a different root. Do not
        // add a broader local-path registration in that case. When only
        // part of a group is covered, register only the uncovered named
        // files.
        let files: Vec<_> = resolved_files
            .into_values()
            .filter(|file| !registered_input_covers_file(file, &session.inputs))
            .collect();
        if files.is_empty() {
            continue;
        }
        let input_id = deterministic_input_id(&canonical_root);
        registered.push(crate::session::state::UserInput {
            input_id,
            label: root
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| canonical_root.clone()),
            kind: crate::session::state::UserInputKind::LocalPath,
            root_path: canonical_root,
            files,
            registered_at: now,
            registered_by: owner_user.clone(),
        });
    }
    session.pending_input_hints = retained;

    // Decisions are recorded after the walk so the loop can hold the
    // `&mut session.pending_input_hints` borrow (same shape as
    // `apply_checkpoint_mode_auto_advances`).
    //
    // `AssumptionRecorded` is the closest fit in the closed
    // `DecisionType` taxonomy: both outcomes are compiler-made
    // inferences about what data the run will actually read, carrying a
    // risk class and an affected-node set. A dedicated
    // `InputPathReconciled` variant would be more precise.
    // `DataSourceDeviation` is the execution-time counterpart: the
    // harness appends it when an agent actually substitutes a source.
    //
    // Ids are content-derived, so an unresolved hint that survives into
    // a second emit must not append a duplicate row.
    let existing_assumption_ids: std::collections::BTreeSet<String> = session
        .decisions
        .iter()
        .filter_map(|r| match &r.decision {
            ecaa_workflow_core::decision_log::DecisionType::AssumptionRecorded { id, .. } => {
                Some(id.clone())
            }
            _ => None,
        })
        .collect();

    for input in &registered {
        let id = format!("a_input_path_{}", input.input_id);
        if existing_assumption_ids.contains(&id) {
            continue;
        }
        session.record_decision(
            ecaa_workflow_core::decision_log::DecisionType::AssumptionRecorded {
                id,
                statement: format!(
                    "The SME named `{root}` as a data location in intake prose but never \
                     registered it. The path was present at emit, so it was auto-registered \
                     as a `local_path` input ({n} file(s)) and written to \
                     `runtime/inputs.json`; the harness binds it into the ingestion \
                     container from there.",
                    root = input.root_path,
                    n = input.files.len(),
                ),
                source: "sme_accepted".to_string(),
                affects_nodes: ingestion_nodes.clone(),
                risk: "low".to_string(),
            },
            ecaa_workflow_core::decision_log::DecisionActor::Sme,
            Some(
                "auto-registered at emit from an intake-prose path hint; no explicit \
                 SME registration click"
                    .to_string(),
            ),
        );
    }
    for entry in &unavailable {
        let id = format!(
            "a_input_path_missing_{}",
            deterministic_input_id(&entry.canonical_root)
        );
        if existing_assumption_ids.contains(&id) {
            continue;
        }
        session.record_decision(
            ecaa_workflow_core::decision_log::DecisionType::AssumptionRecorded {
                id,
                statement: format!(
                    "The SME named `{mention}` (resolved root `{root}`) as a data location \
                     in intake prose. It was {reason}, so no input was registered and \
                     `runtime/inputs.json` does not list it. Stages needing this data will \
                     fall back to public sources; any such substitution is a deviation and \
                     must be stated in the report.",
                    mention = entry.raw_mention,
                    root = entry.canonical_root,
                    reason = entry.reason,
                ),
                source: "sme_accepted".to_string(),
                affects_nodes: ingestion_nodes.clone(),
                risk: "high".to_string(),
            },
            ecaa_workflow_core::decision_log::DecisionActor::Sme,
            Some(
                "SME-named input path was unavailable at emit; recorded so a downstream \
                 substitution cannot be a surprise"
                    .to_string(),
            ),
        );
    }

    if !registered.is_empty() {
        tracing::info!(
            session_id = %session.id,
            n = registered.len(),
            "reconcile_prose_input_hints: auto-registered prose-named input path(s) at emit"
        );
        session.inputs.extend(registered);
    }
    if !unavailable.is_empty() {
        tracing::warn!(
            session_id = %session.id,
            n = unavailable.len(),
            "reconcile_prose_input_hints: SME-named input path(s) unavailable at emit"
        );
    }
    unavailable
}

/// Marker for the CONTEXT.md block written by
/// [`write_unavailable_inputs_note`]. Distinct from the
/// `## SME-supplied data inputs` marker `sync_user_inputs_to_package`
/// truncates at, so the two blocks never eat each other.
const UNAVAILABLE_INPUTS_MARKER: &str = "\n## SME-named data inputs NOT found at emit";

/// Record every prose-named path that could not be registered, in both
/// the agent-facing narrative (`CONTEXT.md`) and a machine-readable
/// sidecar (`runtime/inputs-unavailable.json`) the reporting stage can
/// read back.
///
/// Strict no-op on an empty list, so packages without unresolved hints
/// stay byte-identical to the baseline.
async fn write_unavailable_inputs_note(
    unavailable: &[UnavailableProseInput],
    output_dir: &Path,
) -> Result<()> {
    if unavailable.is_empty() {
        return Ok(());
    }
    let runtime_dir = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime_dir)
        .await
        .with_context(|| format!("creating {}", runtime_dir.display()))?;
    let sidecar = runtime_dir.join("inputs-unavailable.json");
    let body =
        serde_json::to_vec_pretty(unavailable).context("serializing inputs-unavailable.json")?;
    tokio::fs::write(&sidecar, &body)
        .await
        .with_context(|| format!("writing {}", sidecar.display()))?;

    let context_path = output_dir.join("CONTEXT.md");
    if !context_path.exists() {
        return Ok(());
    }
    let mut narrative = String::new();
    narrative.push_str(UNAVAILABLE_INPUTS_MARKER);
    narrative.push_str(
        "\n\nThe SME named the following data location(s) in the project description, but \
         they were NOT usable on this machine when the package was compiled. No input was \
         registered for them: `runtime/inputs.json` does not list them and the harness \
         mounts nothing for them, so they will be ENOENT inside the task container.\n\n",
    );
    for entry in unavailable {
        narrative.push_str(&format!(
            "- SME wrote `{mention}` (resolved root `{root}`{file}) — {reason}\n",
            mention = entry.raw_mention,
            root = entry.canonical_root,
            file = entry
                .file_relpath
                .as_deref()
                .map(|f| format!(", file `{f}`"))
                .unwrap_or_default(),
            reason = entry.reason,
        ));
    }
    narrative.push_str(
        "\nConsequences you MUST honour:\n\n\
         1. Do NOT quietly substitute a stand-in dataset. If a stage cannot proceed without \
         this data and no public source was named by the SME, block the task with a concrete \
         `missing_input` reason instead of completing it against different data.\n\
         2. If the workflow legitimately falls back to another source (an accession named in \
         the project description, a mirror, or a packaged example dataset), that fallback is \
         a DEVIATION from what the SME asked for and MUST be declared: write the \
         `source_deviation` block at the top level of your `result.json` (`requested` = the \
         path listed above, `requested_available: false`, plus `used` / `used_kind` / \
         `used_version` / `reason` / `checksums`). The harness promotes that block into the \
         typed audit trail; do not write `runtime/decisions.jsonl` yourself.\n\
         3. State the substitution in the stage narrative too, so it reaches the final \
         report's methods/limitations text — \"the SME-named local dataset <path> was not \
         available at compile time; <source> was used instead\".\n\
         4. The same list is machine-readable at `runtime/inputs-unavailable.json`.\n\n",
    );

    let mut existing = tokio::fs::read_to_string(&context_path)
        .await
        .with_context(|| format!("reading {}", context_path.display()))?;
    // Idempotent: strip any prior block before re-appending.
    if let Some(idx) = existing.find(UNAVAILABLE_INPUTS_MARKER) {
        existing.truncate(idx);
    }
    existing.push_str(&narrative);
    tokio::fs::write(&context_path, existing.as_bytes())
        .await
        .with_context(|| format!("writing {}", context_path.display()))?;
    Ok(())
}

/// Write `runtime/inputs.json` + refresh the `## SME-supplied data inputs`
/// section of CONTEXT.md from the given inputs. Shared by the emit path and
/// the server's post-emit input-registration sync, so an input registered
/// AFTER a package is emitted reaches both the machine-readable manifest AND
/// the agent-facing CONTEXT.md narrative. Idempotent — the section is rebuilt
/// (prior block stripped) every call.
pub async fn sync_user_inputs_to_package(
    inputs: &[crate::session::state::UserInput],
    output_dir: &Path,
) -> Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    let runtime_dir = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime_dir)
        .await
        .with_context(|| format!("creating {}", runtime_dir.display()))?;

    // Machine-readable manifest.
    let manifest_path = runtime_dir.join("inputs.json");
    let manifest_json = serde_json::to_vec_pretty(inputs).context("serializing inputs.json")?;
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
             ONLY if a registered source is unreadable. `data_acquisition` copies \
             each registered source into a package-internal staged copy under \
             `runtime/outputs/data_acquisition/data/<label>/`. Every stage AFTER \
             `data_acquisition` MUST read these inputs from that package-internal \
             staged path (relative to `$PACKAGE`/`$PKG_ROOT`) — NEVER from the \
             external `Root:` path below — so each stage is self-contained and \
             offline re-execution (replay) can reproduce it.\n\n",
        );
        for input in inputs {
            let total_bytes: u64 = input.files.iter().map(|f| f.size_bytes).sum();
            let kind_label = match input.kind {
                crate::session::state::UserInputKind::LocalPath => "local path",
                crate::session::state::UserInputKind::UploadedFiles => "uploaded files",
            };
            narrative.push_str(&format!(
                "### `{label}` ({kind_label})\n- Root (external; `data_acquisition` ingestion only): `{root}`\n- Staged (read from here in every stage AFTER data_acquisition): `runtime/outputs/data_acquisition/data/{label}/`\n- {n_files} file(s), {bytes} bytes total\n- Manifest: `runtime/inputs.json` (entry `{input_id}`)\n\n",
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

    #[test]
    fn workflow_description_comes_from_final_dag_and_input_substrate() {
        let dag: DAG = serde_json::from_value(serde_json::json!({
            "version": "1",
            "workflow_id": "wf-counts-first",
            "current_task": null,
            "tasks": {
                "data_acquisition": {
                    "kind": "computation",
                    "state": {"status": "pending"},
                    "depends_on": [],
                    "assignee": "agent",
                    "description": "acquire",
                    "spec": {"required_input_stage": "data:3917"}
                },
                "qc_preprocessing": {
                    "kind": "computation",
                    "state": {"status": "pending"},
                    "depends_on": ["data_acquisition"],
                    "assignee": "agent",
                    "description": "matrix QC"
                },
                "discover_normalisation": {
                    "kind": {"discovery": "best_practice"},
                    "state": {"status": "pending"},
                    "depends_on": ["qc_preprocessing"],
                    "assignee": "agent",
                    "description": "choose method"
                },
                "normalisation": {
                    "kind": "computation",
                    "state": {"status": "pending"},
                    "depends_on": ["discover_normalisation"],
                    "assignee": "agent",
                    "description": "normalise"
                },
                "validate_normalisation": {
                    "kind": "validation",
                    "state": {"status": "pending"},
                    "depends_on": ["normalisation"],
                    "assignee": "agent",
                    "description": "validate"
                }
            },
            "execution_order": [
                "data_acquisition",
                "qc_preprocessing",
                "discover_normalisation",
                "normalisation",
                "validate_normalisation"
            ]
        }))
        .expect("minimal DAG");

        let description = executable_workflow_description(
            &dag,
            "Download raw reads, trim, align, and quantify them.",
        );
        assert_eq!(
            description,
            "Executable workflow for input substrate `data:3917`: data acquisition -> qc preprocessing -> normalisation."
        );
        assert!(!description.contains("trim"));
        assert!(!description.contains("discover"));
        assert!(!description.contains("validate"));
    }

    /// DR-8 portability: the `emit_package` decision records the package's own
    /// output directory, which the production emit tool sets to an ABSOLUTE
    /// host path (`/home/…/packages/<session-id>-…`). That path pins the
    /// deposit to one machine AND embeds the raw session id via the directory
    /// name, so the decision-log writer must relativize it to the
    /// package-root-relative "." before writing `decisions.jsonl`.
    #[tokio::test]
    async fn emit_package_decision_output_dir_relativized_for_portability() {
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
        let absolute_output_dir =
            "/home/a/.ecaa-workflow/packages/deadbeef-1111-2222-3333-444455556666-bulk_rnaseq";
        session.record_decision(
            ecaa_workflow_core::decision_log::DecisionType::EmitPackage {
                output_dir: absolute_output_dir.into(),
            },
            ecaa_workflow_core::decision_log::DecisionActor::Llm,
            None,
        );

        let tmp = tempfile::tempdir().unwrap();
        emit_with_conversation_log(&mut session, tmp.path(), &config_dir())
            .await
            .unwrap();

        let body = std::fs::read_to_string(tmp.path().join("runtime/decisions.jsonl")).unwrap();
        assert!(
            !body.contains(absolute_output_dir),
            "absolute emit_package output_dir must be relativized out of decisions.jsonl; got: {body}"
        );
        assert!(
            !body.contains("/home/a/.ecaa-workflow/packages/deadbeef"),
            "no residual host path / session-id from the package dir name may remain; got: {body}"
        );
        assert!(
            body.contains(r#""output_dir":".""#),
            "emit_package output_dir must be the package-root-relative '.'; got: {body}"
        );
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

    /// M2 — when the harness has written `runtime/invocations.jsonl`,
    /// the emitter's RO-Crate patcher must register it as a
    /// File + CreativeWork entity and link it from the root `hasPart`,
    /// exactly like the install-log presence-gated registration.
    #[tokio::test]
    async fn invocations_jsonl_registered_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        // Plant the invocation log as the harness does at the dispatch site.
        std::fs::write(
            runtime.join("invocations.jsonl"),
            "{\"task_id\":\"qc\",\"epoch\":1,\"harness_run_id\":\"run-abc\",\"started_at\":\"2026-06-02T00:00:00Z\",\"port_typed_inputs_satisfied\":true,\"sandbox\":\"none\",\"sandbox_required\":false,\"network_policy\":null}\n",
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
            .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("runtime/invocations.jsonl"))
            .expect("invocations.jsonl entity missing from RO-Crate graph");
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
        assert_eq!(entity["encodingFormat"], "application/jsonl");

        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let parts = root["hasPart"].as_array().unwrap();
        assert!(
            parts.iter().any(|p| p.get("@id").and_then(|v| v.as_str())
                == Some("runtime/invocations.jsonl")),
            "invocations.jsonl missing from root hasPart array"
        );
    }

    /// M2 — a pre-execution first emit (no harness dispatch yet) has no
    /// `runtime/invocations.jsonl`, so the entity must be absent from the
    /// RO-Crate graph (presence-gated).
    #[tokio::test]
    async fn invocations_jsonl_absent_when_file_not_written() {
        let tmp = tempfile::tempdir().unwrap();
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
        assert!(
            !graph.iter().any(|e| e.get("@id").and_then(|v| v.as_str())
                == Some("runtime/invocations.jsonl")),
            "invocations.jsonl entity must not appear when the file does not exist"
        );
    }

    /// §G-B1 — a DE task whose `runtime/invocations.jsonl` record shows it
    /// actually read `quantification`'s raw counts output must have the
    /// RO-Crate's `quantification -> DE` `ParameterConnection` node resolved
    /// as `authoritative` and KEPT, while the declared-but-unread
    /// `normalisation -> DE` one-of sibling is DROPPED from the standard graph
    /// (not merely annotated) and recorded ONLY in the `ecaax:` side channel —
    /// so a generic RO-Crate / WRROC / runcrate consumer sees only the
    /// authoritative raw edge for the count port.
    #[tokio::test]
    async fn de_one_of_edge_resolves_to_the_read_member() {
        use ecaa_workflow_core::workflow_contracts::edge::{
            CompatibilityProof, EdgeContract, EdgeKind,
        };

        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();

        // Declared graph: both one-of count members wired into DE.
        let raw_edge = EdgeContract {
            from_node: "quantification".into(),
            from_port: "count_matrix".into(),
            to_node: "differential_expression".into(),
            to_port: "raw_counts".into(),
            proof: CompatibilityProof::default(),
            kind: EdgeKind::TypedDataFlow,
            chain_of_custody: None,
            mutually_exclusive_group: Some("counts".into()),
        };
        let normalized_edge = EdgeContract {
            from_node: "normalisation".into(),
            from_port: "normalized_counts".into(),
            to_node: "differential_expression".into(),
            to_port: "normalized_counts".into(),
            proof: CompatibilityProof::default(),
            kind: EdgeKind::TypedDataFlow,
            chain_of_custody: None,
            mutually_exclusive_group: Some("counts".into()),
        };
        let proofs_body = format!(
            "{}\n{}\n",
            serde_json::to_string(&raw_edge).unwrap(),
            serde_json::to_string(&normalized_edge).unwrap(),
        );
        std::fs::write(runtime.join("proofs.jsonl"), proofs_body).unwrap();

        // Harness-observed reads: the DE task actually read the RAW
        // matrix (DESeq2 was the method the agent chose at runtime).
        std::fs::write(
            runtime.join("invocations.jsonl"),
            serde_json::json!({
                "task_id": "differential_expression",
                "epoch": 1,
                "harness_run_id": "run-abc",
                "started_at": "2026-06-02T00:00:00Z",
                "port_typed_inputs_satisfied": true,
                "sandbox": "none",
                "sandbox_required": false,
                "network_policy": null,
                "observed_reads": [
                    {
                        "task_id": "differential_expression",
                        "declared_port": "raw_counts",
                        "path": "runtime/outputs/quantification/count_matrix.tsv"
                    }
                ]
            })
            .to_string()
                + "\n",
        )
        .unwrap();

        // Seed a minimal RO-Crate with the two ParameterConnection
        // nodes the compile-time emit would have produced for this DAG.
        let metadata = serde_json::json!({
            "@context": "https://w3id.org/ro/crate/1.1/context",
            "@graph": [
                {"@id": "./", "@type": "Dataset", "hasPart": []},
                ecaa_workflow_core::ro_crate::parameter_connection_entity(
                    "quantification__to__differential_expression",
                    "#step-quantification", "count_matrix",
                    "#step-differential_expression", "raw_counts",
                ),
                ecaa_workflow_core::ro_crate::parameter_connection_entity(
                    "normalisation__to__differential_expression",
                    "#step-normalisation", "normalized_counts",
                    "#step-differential_expression", "normalized_counts",
                ),
            ]
        });
        std::fs::write(
            tmp.path().join("ro-crate-metadata.json"),
            serde_json::to_vec_pretty(&metadata).unwrap(),
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

        let raw_node = graph
            .iter()
            .find(|e| {
                e["@id"] == "#parameter-connection/quantification__to__differential_expression"
            })
            .expect("raw_counts ParameterConnection node present");
        assert_eq!(
            raw_node["ecaax:provenanceStatus"], "authoritative",
            "the read one-of member must be stamped authoritative"
        );

        // §G-B1 — the unread normalized-counts sibling is GONE from the
        // standard graph; a generic consumer never reads it as a data flow.
        assert!(
            graph
                .iter()
                .all(|e| e["@id"]
                    != "#parameter-connection/normalisation__to__differential_expression"),
            "the unread one-of sibling must be dropped from the standard graph"
        );

        // ...and survives ONLY in the ecaax side channel on the root Dataset,
        // which now references a first-class @graph node by `@id` (the
        // RO-Crate/runcrate `@id` fix) rather than inlining a value object.
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let unused = root["ecaax:unusedCandidateEdge"]
            .as_array()
            .expect("unused-candidate side channel recorded on root Dataset");
        assert_eq!(unused.len(), 1);
        let unused0 = graph
            .iter()
            .find(|e| e["@id"] == unused[0]["@id"])
            .expect("unused-candidate reference resolves to a @graph node");
        assert_eq!(unused0["from_node"], "normalisation");
        assert_eq!(unused0["to_node"], "differential_expression");
        assert_eq!(unused0["ecaax:provenanceStatus"], "candidate_unused");
        assert_eq!(unused0["ecaax:supersededByProducer"], "quantification");
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
        // The parent is identified by its content-addressed workflow id
        // (`parent_package_id`), never by an absolute on-disk path — a
        // `$HOME`-rooted path is non-reproducible across machines and
        // would leak into the BagIt manifest. Assert the id is present
        // and that no absolute path leaked into the hashed payload.
        assert!(
            policy["parent"]["parent_package_id"]
                .as_str()
                .map(|s| !s.is_empty())
                .unwrap_or(false),
            "amendment-lineage must carry content-addressed parent_package_id"
        );
        assert!(
            policy["parent"].get("parent_path").is_none(),
            "amendment-lineage must not serialize an absolute parent_path"
        );
        let policy_str = serde_json::to_string(&policy).unwrap();
        assert!(
            !policy_str.contains(parent_tmp.path().to_string_lossy().as_ref()),
            "amendment-lineage leaked the parent's absolute path: {policy_str}"
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

    /// Task 5.2 — recompute readiness at emit. After an amend/param edit,
    /// the completed-upstream overlay lands AFTER the fresh build's
    /// `propagate_readiness`, so a frontier task whose deps are now
    /// Completed would serialize as Pending and the server's
    /// `has_ready_task` gate would skip auto-relaunch. The emit path must
    /// re-run `propagate_readiness` so the frontier serializes as Ready.
    #[tokio::test]
    async fn reemit_marks_frontier_ready_when_upstream_completed() {
        use ecaa_workflow_core::dag::TaskState;

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

        let tmp1 = tempfile::tempdir().unwrap();
        emit_with_conversation_log(&mut session, tmp1.path(), &config_dir())
            .await
            .unwrap();

        // Pick a frontier task: one with at least one dependency and no
        // conditional-execution spec (so `propagate_readiness` promotes it
        // to Ready rather than evaluating a condition).
        let dag = session.current_dag().expect("composed dag");
        let frontier = dag
            .tasks
            .iter()
            .find(|(_, t)| {
                !t.depends_on.is_empty()
                    && t.spec.as_ref().and_then(|s| s.get("condition")).is_none()
            })
            .map(|(k, _)| k.to_string())
            .expect("a non-conditional task with dependencies");

        // Mark every OTHER task Completed so the frontier's deps are all
        // done. This mirrors a post-amend session whose upstream ran.
        let all_ids: Vec<String> = dag.tasks.keys().map(|k| k.to_string()).collect();
        for id in &all_ids {
            if id != &frontier {
                session.set_task_state(
                    id,
                    TaskState::Completed {
                        result: serde_json::json!({}),
                    },
                );
            }
        }
        // Warm the derived cache with the Completed overlay but WITHOUT
        // readiness propagation, isolating the emit-time recompute from the
        // cache-repopulation path. Without the fix the frontier stays
        // Pending here and serializes as "pending"; with the fix the emit
        // path re-runs propagate_readiness and it serializes as "ready".
        session.ensure_dag_cached();

        let tmp2 = tempfile::tempdir().unwrap();
        emit_with_conversation_log(&mut session, tmp2.path(), &config_dir())
            .await
            .unwrap();

        let wf: serde_json::Value =
            serde_json::from_slice(&std::fs::read(tmp2.path().join("WORKFLOW.json")).unwrap())
                .unwrap();
        assert_eq!(
            wf["tasks"][frontier.as_str()]["state"]["status"],
            serde_json::json!("ready"),
            "frontier task with all-Completed upstream must serialize as ready; got {:?}",
            wf["tasks"][frontier.as_str()]["state"]
        );
    }

    /// T12 — a seeded `Divergent` reconcile verdict (surfaced as a
    /// `DivergenceRecord`, the shape `reconcile_ro_crate_edges` returns)
    /// must transition the offending task to
    /// `BlockerKind::ProvenanceDivergence`. `emit_steps` calls this helper
    /// while `session.state == Emitting` (the tool dispatcher's
    /// `EmitPackageStart` pre-handler hook fires before `emit_package`'s
    /// handler — which runs this whole pipeline — executes), so seed that
    /// state directly rather than driving the full tool-dispatch loop.
    #[test]
    fn provenance_divergence_transitions_task_to_typed_blocker() {
        let mut session = Session::new(false);
        session.state = crate::session::SessionState::Emitting;

        let divergences = vec![ecaa_workflow_core::provenance::DivergenceRecord {
            task_id: "differential_expression".into(),
            read_path: "runtime/outputs/data_acquisition/counts.tsv".into(),
            declared_producer: Some("normalisation".into()),
        }];

        apply_provenance_divergence_blockers(&mut session, &divergences);

        match &session.state {
            crate::session::SessionState::Blocked { blocker_kind, .. } => match blocker_kind {
                Some(ecaa_workflow_core::blocker::BlockerKind::ProvenanceDivergence {
                    task_id,
                    read_path,
                    declared_producer,
                }) => {
                    assert_eq!(task_id, "differential_expression");
                    assert_eq!(read_path, "runtime/outputs/data_acquisition/counts.tsv");
                    assert_eq!(declared_producer.as_deref(), Some("normalisation"));
                }
                other => panic!("expected ProvenanceDivergence blocker, got {other:?}"),
            },
            other => panic!("expected session to transition to Blocked, got {other:?}"),
        }
    }

    /// A second divergence for a session already `Blocked` from the first
    /// must APPEND (not overwrite) — mirrors `try_merge_harness_block`'s
    /// existing merge behavior for the harness-driven blocker path.
    #[test]
    fn provenance_divergence_appends_a_second_blocker_entry() {
        let mut session = Session::new(false);
        session.state = crate::session::SessionState::Emitting;

        let divergences = vec![
            ecaa_workflow_core::provenance::DivergenceRecord {
                task_id: "differential_expression".into(),
                read_path: "runtime/outputs/data_acquisition/counts.tsv".into(),
                declared_producer: None,
            },
            ecaa_workflow_core::provenance::DivergenceRecord {
                task_id: "biological_interpretation".into(),
                read_path: "runtime/outputs/differential_expression/raw.tsv".into(),
                declared_producer: None,
            },
        ];

        apply_provenance_divergence_blockers(&mut session, &divergences);

        match &session.state {
            crate::session::SessionState::Blocked { blockers, .. } => {
                assert_eq!(
                    blockers.len(),
                    2,
                    "expected both tasks blocked, got {blockers:?}"
                );
                assert!(blockers
                    .iter()
                    .any(|b| b.task_id == "differential_expression"));
                assert!(blockers
                    .iter()
                    .any(|b| b.task_id == "biological_interpretation"));
            }
            other => panic!("expected session to be Blocked, got {other:?}"),
        }
    }

    /// No divergences → no-op; the session's state is left untouched.
    #[test]
    fn provenance_divergence_no_op_when_empty() {
        let mut session = Session::new(false);
        session.state = crate::session::SessionState::Emitting;

        apply_provenance_divergence_blockers(&mut session, &[]);

        assert!(matches!(
            session.state,
            crate::session::SessionState::Emitting
        ));
    }
}
