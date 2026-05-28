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
//!   `SWFC_ABLATE_CLAIM_CONSISTENCY` per Subsystem B4.
//! - **D2 (determinism-shim)** — always written; the
//!   `ablation_engaged` field records `SWFC_ABLATE_REEXECUTION_CLASS`
//!   state per Subsystem B6.
//! - **D5 (reexecution)** — written as an empty report under
//!   `SWFC_ABLATE_REEXECUTION_CLASS` (file present, `per_artifact` empty)
//!   so downstream tooling always finds the path; skipped entirely when
//!   no parent package exists (first emit). See
//!   [`write_reexecution_sidecar`] for the full ablation contract.
//! - **D3 (security-policy)** + **D4 (model-policy)** — always
//!   written; not ablation-gated (security + model-version disclosure
//!   are load-bearing regardless of arm).
//! - **D5 (typed-blocker)** — suppressed under
//!   `SWFC_ABLATE_TYPED_BLOCKERS` per Subsystem B4. The SSE
//!   broadcaster always emits typed blockers regardless of the flag;
//!   only the emit-time sidecar is ablation-gated.

use crate::session::Session;
use anyhow::{Context, Result};
use scripps_workflow_core::ablation::{AblationFlag, AblationFlagExt};
use std::path::Path;

/// D1 — write `runtime/claim-verification.json`.
///
/// At emit time the session has not yet produced any narrative claims
/// (verification is computed per-task at runtime by
/// `server::verification::verify_task_with_context`). We emit an
/// empty-but-valid stub so the grant's §Authentication of Key Resources
/// claim "this surface ships at emit" holds; the runtime overwrites
/// with concrete verdicts as tasks complete.
///
/// Under `SWFC_ABLATE_CLAIM_CONSISTENCY` (Arm B′ ablation — Grant v19
/// §Aim 3A Subsystem B4) the file is still written but with an empty
/// `verdicts` array and an `ablation_engaged: true` metadata note.
/// Writing the file (rather than omitting it) preserves the runtime
/// `/verify` endpoint's ability to operate: the endpoint runs the live
/// extractor regardless of this flag — the ablation only suppresses the
/// emitted artifact, not runtime verification.
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
            "verdicts": [],
            "ablation_engaged": true,
            "ablation_note": "SWFC_ABLATE_CLAIM_CONSISTENCY=1 — emitted artifact is intentionally empty; runtime /verify endpoint is unaffected",
        }))
    } else {
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": "1",
            "n_checked": 0,
            "n_verified": 0,
            "n_unverifiable": 0,
            "n_mismatch": 0,
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
/// field mirrors `SWFC_ABLATE_REEXECUTION_CLASS` (Subsystem B6 — Arm B′).
///
/// Always written — the env capture itself records whether the
/// re-execution-class ablation is engaged, so reviewers see both arms
/// in the same payload shape.
pub(super) async fn write_determinism_shim(output_dir: &Path) -> Result<()> {
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    let path = runtime.join("determinism-shim.json");

    let payload = scripps_workflow_core::determinism_shim::serialize_active_settings();
    let body = serde_json::to_vec_pretty(&payload).context("serializing determinism-shim.json")?;

    tokio::fs::write(&path, body)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// D3 — write `runtime/security-policy.json`.
///
/// Aggregates the per-atom `SafetyPolicy` 5-tuple
/// (`SafetyLevel` × `NetworkPolicy` × `CodeExecution` × `SandboxRequirement`
/// × `ProvisioningPolicy`) across every atom used by the package, plus
/// container image SHA-256 digests and an optional vulnerability-scan
/// summary. Always written.
///
/// Today the session's `atoms_in_use()` accessor returns an empty `Vec`
/// (the per-atom composer wiring is in progress). The aggregator handles
/// the empty case gracefully — `package_max_safety_level` falls back to
/// the default `SafetyLevel::Compute`. When the atom registry walk
/// closes, this sidecar gains content without touching this call site.
pub(super) async fn write_security_policy(session: &Session, output_dir: &Path) -> Result<()> {
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    let path = runtime.join("security-policy.json");

    let atoms = session.atoms_in_use();
    let atom_refs: Vec<&scripps_workflow_core::atom::AtomDefinition> = atoms.iter().collect();
    let digests = session.container_image_digests();
    let payload = scripps_workflow_core::atom_safety::aggregate_for_package(&atom_refs, digests);
    let body = serde_json::to_vec_pretty(&payload).context("serializing security-policy.json")?;

    tokio::fs::write(&path, body)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// D4 — write `runtime/model-policy.json`.
///
/// Records the active Anthropic model (Sonnet 4.6 default; Opus 4.7 on
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
/// **Ablation contract (`SWFC_ABLATE_REEXECUTION_CLASS`):** when the flag is
/// active, the file is written with an empty `per_artifact` list and an
/// `ablation_engaged: true` field rather than being skipped. This ensures
/// downstream tooling always finds `runtime/reexecution.json`; the absence of
/// content (not the absence of the file) is the Arm B′ signal. The load-bearing
/// content suppression lives here — `determinism_shim.rs` records the bool flip
/// for historical-session readers but does not suppress any content itself.
///
/// **First-emit skip:** when the session carries no parent package path (first
/// emit — neither `session.lineage.parent_emitted_package_path` nor
/// `session.pending_amendment.parent_package_path` is set), the sidecar is
/// not written. Downstream tooling must treat absence of the file as "no
/// parent to replay against".
pub(super) async fn write_reexecution_sidecar(session: &Session, output_dir: &Path) -> Result<()> {
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    let path = runtime.join("reexecution.json");

    // Ablation engaged: write an empty-but-present sidecar. The file
    // presence preserves downstream tooling assumptions; the empty
    // per_artifact list is the Arm B′ suppression signal.
    if AblationFlag::ReexecutionClass.is_active() {
        let body = serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": "0.1",
            "bucket_counts": {},
            "per_artifact": [],
            "ablation_engaged": true,
        }))
        .context("serializing empty reexecution.json (ablation)")?;
        tokio::fs::write(&path, body)
            .await
            .with_context(|| format!("writing {}", path.display()))?;
        return Ok(());
    }

    // Resolve the parent package path. First-emit → no parent → skip.
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
            // No parent to replay against — sidecar intentionally absent.
            return Ok(());
        }
    };

    if !parent_path.exists() {
        // Parent path recorded but directory missing; skip gracefully.
        return Ok(());
    }

    // Run the classifier synchronously inside a spawn_blocking to avoid
    // blocking the async executor — all file reads in core are blocking.
    let output_dir_owned = output_dir.to_path_buf();
    let report = tokio::task::spawn_blocking(move || {
        scripps_workflow_core::reexecution::classify_reexecution(
            &parent_path,
            &output_dir_owned,
            None,
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
/// Suppressed entirely under `SWFC_ABLATE_TYPED_BLOCKERS` per the
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
