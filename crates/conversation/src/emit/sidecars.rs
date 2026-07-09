//! Grant v19 §Authentication of Key Resources — emit the five
//! `runtime/*.json` sidecars cited as live disclosure surfaces:
//! `claim-verification.json`, `determinism-shim.json`,
//! `reexecution.json`, `security-policy.json`, `model-policy.json`.
//!
//! Each sidecar follows the same shape: build a serializable struct →
//! write it to `output_dir/runtime/<name>.json` via `tokio::fs::write`
//! → register it as a `CreativeWork` in `ro_crate.rs::patch_ro_crate_metadata`
//! (presence-gated, like the semantic-sidecar loop).
//!
//! Ablation pairing:
//! - **D1 (claim-verification)** — suppressed under
//!   `ECAA_ABLATE_CLAIM_CONSISTENCY` per Subsystem B4.
//! - **D2 (determinism-shim)** — always written; the
//!   `ablation_engaged` field records `ECAA_ABLATE_REEXECUTION_CLASS`
//!   state per Subsystem B6.
//! - **D5 (reexecution)** — ALWAYS written (uniform presence). Under
//!   `ECAA_ABLATE_REEXECUTION_CLASS` and on a first emit (no parent package)
//!   it is present-but-empty (`per_artifact: []`); with a parent it carries
//!   the classified re-execution buckets. Invariant 4 reads this file as its
//!   Q sub-graph source (empty → `Unverified`). See
//!   [`write_reexecution_sidecar`] for the full contract.
//! - **D3 (security-policy)** + **D4 (model-policy)** — always
//!   written; not ablation-gated (security + model-version disclosure
//!   are load-bearing regardless of arm).
//! - **D5 (typed-blocker)** — suppressed under
//!   `ECAA_ABLATE_TYPED_BLOCKERS` per Subsystem B4. The SSE
//!   broadcaster always emits typed blockers regardless of the flag;
//!   only the emit-time sidecar is ablation-gated.

use crate::session::Session;
use anyhow::{Context, Result};
use ecaa_workflow_core::ablation::{AblationFlag, AblationFlagExt};
use std::path::Path;

/// D1 — write `runtime/claim-verification.json`.
///
/// At emit time the session has not yet produced any narrative claims
/// (verification is computed per-task at runtime by
/// `ecaa_workflow_core::finalize::verify_task_with_context`). We emit an
/// empty-but-valid stub so the grant's §Authentication of Key Resources
/// claim "this surface ships at emit" holds. Post-execution, the host-side
/// `core::finalize::finalize_task` refreshes this file in place with the
/// concrete recomputed verdicts (best-effort, aggregated across finalized
/// tasks via `claim_sink::refresh_plaintext_sidecar`), so a standalone
/// harness run no longer leaves it at `n_checked: 0`. This plaintext is the
/// operator/UI-visible view; the HMAC-signed sink under
/// `runtime/verification-reports/` is the trust surface the audit-proof
/// loader prefers.
///
/// Under `ECAA_ABLATE_CLAIM_CONSISTENCY` (Arm B′ ablation — Aim 3A) this
/// emit-time stub records `ablation_engaged: true` with an empty `verdicts`
/// array. This is ONE of THREE coordinated suppression sites for the same
/// flag: (1) this emit-time stub, (2) the populated host-signed sink under
/// `runtime/verification-reports/` (`core::claim_sink::build_sink_doc`,
/// suppressed with an explicit `ablated: true`), and (3) the live L2 block
/// at task completion (`server::verification::block_enforced_under_current_env`).
/// Together they make the A-vs-B′ contrast measure enforcement PRESENCE, not
/// a status-enum flip on a perpetually-empty file.
pub(super) async fn write_claim_verification(output_dir: &Path) -> Result<()> {
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    let path = runtime.join("claim-verification.json");

    let ablated = AblationFlag::ClaimConsistency.is_active();
    let body = if ablated {
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": "1",
            "n_checked": 0,
            "n_verified": 0,
            "n_unverifiable": 0,
            "n_mismatch": 0,
            "n_suspicious": 0,
            "verdicts": [],
            "ablation_engaged": true,
            "ablation_note": "ECAA_ABLATE_CLAIM_CONSISTENCY=1 — emit-time stub intentionally empty; the populated signed sink and the live L2 block are also suppressed (two-site toggle, Aim 3A)",
        }))
    } else {
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": "1",
            "n_checked": 0,
            "n_verified": 0,
            "n_unverifiable": 0,
            "n_mismatch": 0,
            "n_suspicious": 0,
            "verdicts": [],
        }))
    }
    .context("serializing claim-verification.json")?;

    tokio::fs::write(&path, body)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// D2 — write `runtime/determinism-shim.json`.
///
/// Captures `TZ`/`LANG`/`LC_ALL`/`PYTHONHASHSEED`/`SOURCE_DATE_EPOCH` env
/// var presence, redacted-by-name secrets, seed policy, temp-path
/// strategy, locale, and timezone at emit time. The `ablation_engaged`
/// field mirrors `ECAA_ABLATE_REEXECUTION_CLASS` (Subsystem B6 — Arm B′).
///
/// Always written — the env capture itself records whether the
/// re-execution-class ablation is engaged, so reviewers see both arms
/// in the same payload shape.
pub(super) async fn write_determinism_shim(
    session: &Session,
    output_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    let path = runtime.join("determinism-shim.json");

    let mut payload = ecaa_workflow_core::determinism_shim::serialize_active_settings();

    // Project each composed atom's declared non-determinism into per-artifact
    // acknowledgments (`non_deterministic_artifacts`). The re-execution
    // comparator and the audit-proof `equivalence_failure` invariant BOTH read
    // this list: an out-of-band divergence on a declared column is
    // `acknowledged_non_determinism`, while any UNdeclared divergence FAILS — so
    // a package self-declares exactly which artifact/column jitter is expected
    // (e.g. adaptive-shrinkage LFC), rather than a blanket no-seed mask. Atom
    // declarations name a bare output basename; expand to the task's full
    // `runtime/outputs/<task_id>/<file>` path. Registry-load failure is
    // non-fatal (warn + no acks), preserving the "always emits" contract.
    let atoms_dir = config_dir.join("stage-atoms");
    match ecaa_workflow_core::atom_registry::AtomRegistry::load_from_dir(&atoms_dir) {
        Ok(registry) => {
            let acks = project_non_det_acks(session, &registry);
            if !acks.is_empty() {
                payload.set_non_deterministic_artifacts(acks);
            }
        }
        Err(e) => tracing::warn!(
            "write_determinism_shim: AtomRegistry load from {} failed: {} \
             (continuing emit with no non-determinism acknowledgments)",
            atoms_dir.display(),
            e
        ),
    }

    let body = serde_json::to_vec_pretty(&payload).context("serializing determinism-shim.json")?;

    tokio::fs::write(&path, body)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Expand every composed atom's declared `non_determinism` into shim
/// [`NonDetAck`]s keyed by the task's full package-relative artifact path.
///
/// Each DAG node id IS the atom id AND the task output-dir name, so an atom
/// declaration `{ artifact: "de_results.tsv", columns: [...] }` on the
/// `differential_expression` node becomes an ack for
/// `runtime/outputs/differential_expression/de_results.tsv`. A missing DAG
/// (legacy emit) or a node absent from the registry yields fewer acks, never a
/// panic. Pure + unit-testable.
fn project_non_det_acks(
    session: &Session,
    registry: &ecaa_workflow_core::atom_registry::AtomRegistry,
) -> Vec<ecaa_workflow_core::determinism_shim::NonDetAck> {
    let Some(dag) = session.workflow_dag.as_ref() else {
        return Vec::new();
    };
    let ids: Vec<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
    acks_for_task_ids(&ids, registry)
}

/// The registry-lookup core of [`project_non_det_acks`], taking bare task ids
/// (= atom ids = output-dir names) so it is unit-testable without a `Session`.
fn acks_for_task_ids(
    task_ids: &[&str],
    registry: &ecaa_workflow_core::atom_registry::AtomRegistry,
) -> Vec<ecaa_workflow_core::determinism_shim::NonDetAck> {
    let mut acks = Vec::new();
    for id in task_ids {
        let Some(atom) = registry.get(id) else {
            continue;
        };
        for decl in &atom.non_determinism {
            acks.push(ecaa_workflow_core::determinism_shim::NonDetAck {
                artifact: format!("runtime/outputs/{}/{}", id, decl.artifact),
                columns: decl.columns.clone(),
                kind: decl.kind.clone(),
                reason: decl.reason.clone(),
            });
        }
    }
    acks
}

#[cfg(test)]
mod nondet_projection_tests {
    use super::acks_for_task_ids;

    fn registry() -> ecaa_workflow_core::atom_registry::AtomRegistry {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/stage-atoms");
        ecaa_workflow_core::atom_registry::AtomRegistry::load_from_dir(&dir)
            .expect("load stage-atoms registry")
    }

    /// The `differential_expression` atom declares its shrunken effect-size
    /// columns (`log2FoldChange` + `lfcSE`) as adaptive-shrinkage
    /// non-determinism; the projection must expand that to the task's FULL
    /// package-relative artifact path, preserving all declared columns.
    #[test]
    fn projects_de_atom_shrinkage_ack_to_full_path() {
        let reg = registry();
        let acks = acks_for_task_ids(&["differential_expression"], &reg);
        let de = acks
            .iter()
            .find(|a| a.artifact == "runtime/outputs/differential_expression/de_results.tsv")
            .expect("DE de_results.tsv ack projected to full path");
        assert_eq!(
            de.columns.as_deref(),
            Some(&["log2FoldChange".to_string(), "lfcSE".to_string()][..])
        );
        assert_eq!(
            de.kind,
            ecaa_workflow_core::determinism_shim::NonDetKind::AdaptiveShrinkage
        );
    }

    /// An unknown task id contributes no acks (non-fatal, never panics).
    #[test]
    fn unknown_task_id_yields_no_acks() {
        let reg = registry();
        assert!(acks_for_task_ids(&["not_a_real_atom_xyz"], &reg).is_empty());
    }
}

/// D3 — write `runtime/security-policy.json`.
///
/// Aggregates the per-atom `SafetyPolicy` 5-tuple
/// (`SafetyLevel` × `NetworkPolicy` × `CodeExecution` × `SandboxRequirement`
/// × `ProvisioningPolicy`) across every atom used by the package, plus
/// container image SHA-256 digests and an optional vulnerability-scan
/// summary. Always written.
///
/// Loads the [`AtomRegistry`] from `config_dir/stage-atoms` once, walks
/// the session's DAG to resolve the atoms in use, and aggregates their
/// SafetyPolicy 5-tuples plus the two-tier container digests. Registry
/// load failure is non-fatal (warn + zero atom policies) so the sidecar
/// still emits a minimal-but-valid manifest — preserving the
/// "always emits" contract (mirrors the per-atom-prereqs block in
/// `emit::mod`).
///
/// [`AtomRegistry`]: ecaa_workflow_core::atom_registry::AtomRegistry
pub async fn write_security_policy(
    session: &Session,
    output_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    let path = runtime.join("security-policy.json");

    let atoms_dir = config_dir.join("stage-atoms");
    let atoms = match ecaa_workflow_core::atom_registry::AtomRegistry::load_from_dir(&atoms_dir) {
        Ok(registry) => session.atoms_in_use(&registry),
        Err(e) => {
            tracing::warn!(
                "write_security_policy: AtomRegistry load from {} failed: {} \
                 (continuing emit with zero atom policies)",
                atoms_dir.display(),
                e
            );
            Vec::new()
        }
    };
    let atom_refs: Vec<&ecaa_workflow_core::atom::AtomDefinition> = atoms.iter().collect();
    let digests = session.container_image_digests(&atoms);
    let payload = ecaa_workflow_core::atom_safety::aggregate_for_package(&atom_refs, digests);
    let body = serde_json::to_vec_pretty(&payload).context("serializing security-policy.json")?;

    tokio::fs::write(&path, body)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// D5 (requested side) — write `runtime/dependency-lock.json` from the
/// aggregated package prereqs. Offline + byte-reproducible: the resolved
/// column is filled at runtime by the install-proxy fold (OPERATOR-GATED).
/// Always written (empty columns when no language packages declared).
pub async fn write_dependency_lock(
    prereqs: &ecaa_workflow_core::runtime_prereqs::RuntimePrereqs,
    output_dir: &Path,
) -> Result<()> {
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    let path = runtime.join("dependency-lock.json");
    let lock = ecaa_workflow_core::dependency_lock::RequestedLock::from_prereqs(prereqs);
    let body = serde_json::to_vec_pretty(&lock).context("serializing dependency-lock.json")?;
    tokio::fs::write(&path, body)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// D4 — write `runtime/model-policy.json`.
///
/// Records the active Anthropic model (Sonnet 5 default; Opus 4.8 on
/// careful-mode / Blocked / low-confidence), API version, SHA-256 of
/// the fully-assembled system prompt, tool-schema version
/// ([`crate::tool_schemas::SCHEMA_VERSION`]), tool count
/// ([`crate::tools::Tool::COUNT`]), provider id, and (when applicable)
/// the escalation reason. Mid-evaluation model-version changes therefore
/// surface in the package diff. Always written.
pub(super) async fn write_model_policy(session: &Session, output_dir: &Path) -> Result<()> {
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    let path = runtime.join("model-policy.json");

    let payload = super::model_policy_sidecar::build_for_session(session);
    let body = serde_json::to_vec_pretty(&payload).context("serializing model-policy.json")?;

    tokio::fs::write(&path, body)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// D5 — write `runtime/reexecution.json`.
///
/// Classifies every `results/tables/*.{csv,tsv}` artifact from a prior emit
/// (the "parent package") against the corresponding file in the current emit
/// directory ("replay"), assigning each artifact to one of five buckets per
/// PAR-26-040 §Aim 3A primary endpoint:
/// `byte_identical` / `semantic_equivalent` / `acknowledged_non_determinism`
/// / `unavailable` / `failed`.
///
/// **Ablation contract (`ECAA_ABLATE_REEXECUTION_CLASS`):** when the flag is
/// active, the file is written with an empty `per_artifact` list and an
/// `ablation_engaged: true` field rather than being skipped. This ensures
/// downstream tooling always finds `runtime/reexecution.json`; the absence of
/// content (not the absence of the file) is the Arm B′ signal. The load-bearing
/// content suppression lives here — `determinism_shim.rs` records the bool flip
/// for historical-session readers but does not suppress any content itself.
///
/// **Uniform presence (first emit):** `reexecution.json` is ALWAYS written. On
/// a first emit — when the session carries no parent package path (neither
/// `session.lineage.parent_emitted_package_path` nor
/// `session.pending_amendment.parent_package_path` is set) — the sidecar is
/// written present-but-empty (`per_artifact: []`). A present-but-empty file
/// means "no re-execution performed"; Invariant 4 (`equivalence_failure`) reads
/// this file as its Q sub-graph source and maps the empty case to `Unverified`.
/// This gives the invariant a defined, always-present source rather than
/// branching on file absence.
pub(super) async fn write_reexecution_sidecar(
    session: &Session,
    output_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    let path = runtime.join("reexecution.json");

    // Helper: write a present-but-empty report. Used for both the ablation
    // path and the first-emit (no-parent) path so the file is uniformly
    // present with a stable, deterministic shape.
    let write_empty = |ablation_engaged: bool| -> Result<Vec<u8>> {
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": "0.1",
            "bucket_counts": {},
            "per_artifact": [],
            "ablation_engaged": ablation_engaged,
        }))
        .context("serializing empty reexecution.json")
    };

    // Ablation engaged: write an empty-but-present sidecar. The file
    // presence preserves downstream tooling assumptions; the empty
    // per_artifact list is the Arm B′ suppression signal.
    if AblationFlag::ReexecutionClass.is_active() {
        let body = write_empty(true)?;
        tokio::fs::write(&path, body)
            .await
            .with_context(|| format!("writing {}", path.display()))?;
        return Ok(());
    }

    // Resolve the parent package path. First-emit → no parent → write the
    // present-but-empty report (uniform presence; Inv 4 reads empty → Unverified).
    let parent_path: std::path::PathBuf = match (
        session
            .lineage
            .as_ref()
            .and_then(|l| l.parent_emitted_package_path.clone()),
        session
            .pending_amendment
            .as_ref()
            .map(|a| a.parent_package_path.clone()),
    ) {
        (Some(p), _) => p,
        (None, Some(p)) => p,
        (None, None) => {
            // No parent to replay against — present-but-empty (not absent).
            let body = write_empty(false)?;
            tokio::fs::write(&path, body)
                .await
                .with_context(|| format!("writing {}", path.display()))?;
            return Ok(());
        }
    };

    if !parent_path.exists() {
        // Parent path recorded but directory missing; write present-but-empty.
        let body = write_empty(false)?;
        tokio::fs::write(&path, body)
            .await
            .with_context(|| format!("writing {}", path.display()))?;
        return Ok(());
    }

    // Resolve per-modality semantic-equivalence bounds from the
    // classified modality. Load is warn-and-continue (missing dir →
    // fallback-only provider), so a config-dir typo degrades to the
    // historical ±5% band rather than blocking emit.
    let modality = session
        .classification
        .as_ref()
        .map(|c| c.modality.clone())
        .unwrap_or_default();
    let bounds = ecaa_workflow_core::reexecution_bounds::ModalityBoundsProvider::from_dir(
        &config_dir.join("reexecution-bounds"),
    )
    .bounds_for(&modality);

    // Run the classifier synchronously inside a spawn_blocking to avoid
    // blocking the async executor — all file reads in core are blocking.
    let output_dir_owned = output_dir.to_path_buf();
    let report = tokio::task::spawn_blocking(move || {
        ecaa_workflow_core::reexecution::classify_reexecution(
            &parent_path,
            &output_dir_owned,
            None,
            bounds,
        )
    })
    .await
    .context("reexecution classifier task panicked")?
    .context("reexecution::classify_reexecution")?;

    let body =
        serde_json::to_vec_pretty(&report).context("serializing runtime/reexecution.json")?;
    tokio::fs::write(&path, body)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// D5 — write `runtime/typed-blocker.json`.
///
/// Emits an empty-but-valid sentinel at emit time so the grant's
/// §Authentication of Key Resources claim "this surface ships at emit"
/// holds. The runtime SSE broadcaster (`broadcaster.rs`) overwrites
/// this with per-blocker typed payloads as tasks fail; the emit-time
/// sentinel ensures the file is always present for downstream consumers
/// that probe for it before any task has run.
///
/// Suppressed entirely under `ECAA_ABLATE_TYPED_BLOCKERS` per the
/// Arm B′ ablation contract (Subsystem B4 — Grant v19 §Aim 3A). When
/// ablated, the file is absent from the package; the SSE broadcaster
/// always emits typed blockers regardless of this flag (the ablation
/// moves to emit-only, not to the live runtime path).
pub(super) async fn write_typed_blocker(output_dir: &Path) -> Result<()> {
    if AblationFlag::TypedBlockers.is_active() {
        return Ok(());
    }
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    let path = runtime.join("typed-blocker.json");

    let body = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": "1",
        "blockers": [],
    }))
    .context("serializing typed-blocker.json")?;

    tokio::fs::write(&path, body)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
