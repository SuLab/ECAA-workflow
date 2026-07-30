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

    let payload = ecaa_workflow_core::determinism_shim::serialize_active_settings();

    // Project each composed atom's STATIC `non_determinism` declaration into the
    // `declared_non_determinism` block — NOT into `non_deterministic_artifacts`.
    //
    // The distinction is the honesty contract. `non_deterministic_artifacts` is
    // the authoritative MASK that the re-execution comparator
    // (`core::reexecution::classify_reexecution`) and the audit-proof
    // `equivalence_failure` invariant both read through `determinism_shim::ack_for`:
    // a divergence covered by an entry there is downgraded to
    // `acknowledged_non_determinism` instead of failing. Emit runs BEFORE any task
    // executes, so at this point nothing is known about what the agent will
    // actually do — projecting the atom's static declaration straight into the mask
    // asserts "this artifact IS non-deterministic" on no evidence, and silently
    // exempts the artifact from equivalence checking even when the executed script
    // never used the declared mechanism. So the projection lands in a sibling block
    // that is explicitly a DECLARATION pending run confirmation, and the mask stays
    // empty until a post-run reconciliation confirms each declaration against the
    // stage's recorded run evidence.
    //
    // Atom declarations name a bare output basename; expand to the task's full
    // `runtime/outputs/<task_id>/<file>` path. Registry-load failure is non-fatal
    // (warn + no declarations), preserving the "always emits" contract.
    let atoms_dir = config_dir.join("stage-atoms");
    let declared = match ecaa_workflow_core::atom_registry::AtomRegistry::load_from_dir(&atoms_dir)
    {
        Ok(registry) => DeclaredNonDeterminism::projected(project_non_det_acks(session, &registry)),
        Err(e) => {
            tracing::warn!(
                "write_determinism_shim: AtomRegistry load from {} failed: {} \
                 (continuing emit with no non-determinism declarations)",
                atoms_dir.display(),
                e
            );
            DeclaredNonDeterminism::projected(Vec::new())
        }
    };

    let body = serde_json::to_vec_pretty(&attach_declared_non_determinism(&payload, declared)?)
        .context("serializing determinism-shim.json")?;

    tokio::fs::write(&path, body)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Reconciliation status of the whole `declared_non_determinism` block.
///
/// `DeclaredPendingRunConfirmation` is the only value this crate writes: emit
/// happens before execution, so every declaration is unconfirmed. The finalize
/// path flips the block to `Reconciled` once it has checked each declaration
/// against run evidence (see [`DeclaredAckStatus`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum DeclaredBlockStatus {
    /// No task has run yet; nothing here has been confirmed.
    DeclaredPendingRunConfirmation,
}

/// Per-declaration reconciliation status.
///
/// Emit writes `Declared`. A post-run reconciliation rewrites each entry to
/// `Confirmed` (the run really did use the declared mechanism — the declaration
/// is then also promoted into `non_deterministic_artifacts`, earning the mask)
/// or `Refuted` (the run did not — the declaration stays here as a record of a
/// static claim the execution contradicted, and NO mask is granted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum DeclaredAckStatus {
    /// Projected from the atom's static YAML; no run evidence consulted.
    Declared,
}

/// One atom-declared non-determinism source, carried with the provenance a
/// post-run reconciliation needs to confirm or drop it.
///
/// Field order is the serialization order; every field is derived from the atom
/// registry + DAG, so the block is byte-stable across emits.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct DeclaredAck {
    /// The task whose output dir the declaration was expanded against. This is
    /// also where the reconciler finds the run evidence.
    task_id: String,
    /// Package-relative artifact path the declaration applies to.
    artifact: String,
    /// Columns the declaration scopes to; absent = whole artifact.
    #[serde(skip_serializing_if = "Option::is_none")]
    columns: Option<Vec<String>>,
    /// Declared class of non-determinism.
    kind: ecaa_workflow_core::determinism_shim::NonDetKind,
    /// Human-readable justification from the atom YAML (never parsed).
    reason: String,
    /// Always `Declared` at emit.
    status: DeclaredAckStatus,
    /// The package-relative file a reconciler must read to decide whether the
    /// run actually exhibited `kind`. Recorded explicitly so the contract is
    /// legible in the deposit rather than implied by finalize-side code.
    confirmation_evidence: String,
}

/// The `declared_non_determinism` block appended to `determinism-shim.json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct DeclaredNonDeterminism {
    status: DeclaredBlockStatus,
    /// What the reader must not conclude from this block. Static prose, so the
    /// sidecar stays byte-stable.
    note: &'static str,
    declarations: Vec<DeclaredAck>,
}

impl DeclaredNonDeterminism {
    /// Wrap emit-time projected acks as unconfirmed declarations, sorted +
    /// deduplicated so the block is byte-stable regardless of atom visit order
    /// (mirrors `DeterminismShimSidecar::set_non_deterministic_artifacts`).
    fn projected(mut acks: Vec<ecaa_workflow_core::determinism_shim::NonDetAck>) -> Self {
        acks.sort();
        acks.dedup();
        let declarations = acks
            .into_iter()
            .map(|a| {
                let task_id = task_id_of(&a.artifact);
                DeclaredAck {
                    confirmation_evidence: format!("runtime/outputs/{task_id}/result.json"),
                    task_id,
                    artifact: a.artifact,
                    columns: a.columns,
                    kind: a.kind,
                    reason: a.reason,
                    status: DeclaredAckStatus::Declared,
                }
            })
            .collect();
        Self {
            status: DeclaredBlockStatus::DeclaredPendingRunConfirmation,
            note: "Projected from static atom declarations at emit, BEFORE any task ran. \
                   These are claims about what a stage MIGHT do, not observations. They grant \
                   no equivalence-check exemption: only entries a post-run reconciliation \
                   confirms against the stage's recorded run evidence are promoted into \
                   `non_deterministic_artifacts`, which is the mask the re-execution \
                   comparator and the audit-proof equivalence-failure invariant read.",
            declarations,
        }
    }
}

/// Recover the task id from a projected artifact path
/// (`runtime/outputs/<task_id>/<file>`). Falls back to the whole path when the
/// shape does not match, so a hand-authored declaration never panics.
fn task_id_of(artifact: &str) -> String {
    artifact
        .strip_prefix("runtime/outputs/")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or(artifact)
        .to_string()
}

/// Serialize the core shim payload and attach the `declared_non_determinism`
/// block.
///
/// The block lives OUTSIDE [`DeterminismShimSidecar`] on purpose: that struct's
/// `non_deterministic_artifacts` is the load-bearing mask, and a declaration is
/// not a mask. Consumers that deserialize the file back into the core struct
/// ignore the extra key (no `deny_unknown_fields`), so this is additive.
/// Key order is `serde_json::Map`'s (BTreeMap) ordering — byte-stable.
fn attach_declared_non_determinism(
    payload: &ecaa_workflow_core::determinism_shim::DeterminismShimSidecar,
    declared: DeclaredNonDeterminism,
) -> Result<serde_json::Value> {
    let mut value =
        serde_json::to_value(payload).context("serializing determinism-shim.json payload")?;
    let obj = value
        .as_object_mut()
        .context("determinism-shim.json payload is not a JSON object")?;
    obj.insert(
        "declared_non_determinism".to_string(),
        serde_json::to_value(declared).context("serializing declared_non_determinism block")?,
    );
    Ok(value)
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
    use super::{acks_for_task_ids, attach_declared_non_determinism, DeclaredNonDeterminism};

    fn registry() -> ecaa_workflow_core::atom_registry::AtomRegistry {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/stage-atoms");
        ecaa_workflow_core::atom_registry::AtomRegistry::load_from_dir(&dir)
            .expect("load stage-atoms registry")
    }

    /// The `differential_expression` atom declares every supported shrunken
    /// effect-size alias plus `lfcSE` as adaptive-shrinkage non-determinism;
    /// the projection must expand that to the task's FULL package-relative
    /// artifact path while preserving all declared columns.
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
            Some(
                &[
                    "log2FoldChange".to_string(),
                    "log2FC".to_string(),
                    "logFC".to_string(),
                    "lfcSE".to_string(),
                ][..]
            )
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

    /// Emit happens BEFORE execution, so an atom's static `non_determinism`
    /// declaration is a claim about what a stage might do — never an observation.
    /// It must therefore land in `declared_non_determinism` marked `declared`,
    /// and must NOT populate `non_deterministic_artifacts`: that array is the
    /// mask `determinism_shim::ack_for` grants the re-execution comparator and
    /// the audit-proof equivalence-failure invariant, so writing it at emit
    /// exempts an artifact from equivalence checking on zero evidence.
    #[test]
    fn projected_ack_is_marked_declared_not_confirmed() {
        let reg = registry();
        let acks = acks_for_task_ids(&["differential_expression"], &reg);
        assert!(
            !acks.is_empty(),
            "fixture precondition: the DE atom declares non-determinism"
        );

        let shim = ecaa_workflow_core::determinism_shim::serialize_active_settings();
        let value = attach_declared_non_determinism(&shim, DeclaredNonDeterminism::projected(acks))
            .expect("attach declared block");

        // The comparator mask stays empty: nothing has run, nothing is earned.
        let mask = value.get("non_deterministic_artifacts");
        assert!(
            mask.is_none()
                || mask
                    .and_then(|m| m.as_array())
                    .is_some_and(|a| a.is_empty()),
            "emit must not assert non-determinism into the comparator mask; got {mask:?}"
        );

        let block = value
            .get("declared_non_determinism")
            .expect("declared_non_determinism block present");
        assert_eq!(
            block["status"], "declared_pending_run_confirmation",
            "the block must announce that nothing here is confirmed"
        );

        let de = block["declarations"]
            .as_array()
            .expect("declarations array")
            .iter()
            .find(|d| d["artifact"] == "runtime/outputs/differential_expression/de_results.tsv")
            .expect("DE declaration projected to its full package-relative path");
        assert_eq!(de["status"], "declared");
        assert_eq!(de["task_id"], "differential_expression");
        // The reconciler is told, in the deposit itself, what to read.
        assert_eq!(
            de["confirmation_evidence"],
            "runtime/outputs/differential_expression/result.json"
        );
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

/// D5 — write `runtime/dependency-lock.json` from the aggregated package
/// prereqs (the REQUESTED side), then fold in the RESOLVED exact versions
/// recorded in the per-task `runtime/outputs/<task>/{env.lock,env.explicit.lock}`
/// snapshots when they exist. At a fresh emit `runtime/outputs/` is absent, so
/// the fold is a no-op and the requested-only lock stays byte-reproducible (the
/// emit determinism contract); the deposit / finalize re-emit — where the
/// env.lock snapshots exist — folds the real installed versions in so the
/// deposited `dependency-lock.json` is non-empty and reflects what ACTUALLY ran
/// (wiring the otherwise-caller-less `RequestedLock::fold_resolved`). Always
/// written.
///
/// The columns alone are ambiguous: atoms declare no package prereqs, so a fresh
/// emit produces `{"r":[],"python":[],"conda":[]}` — which reads as "this package
/// has no dependencies" when the truth is "nothing has been captured yet", and
/// the per-task `runtime/outputs/<task>/env.explicit.lock` snapshots the agents
/// write are full conda locks. `capture_status` disambiguates the two, so an
/// empty lock can never be mistaken for a dependency-free package. It is derived
/// from the folded lock (see [`capture_status_of`]) rather than from the caller,
/// so a finalize-time backfill that re-runs the fold flips the status by
/// construction.
pub async fn write_dependency_lock(
    prereqs: &ecaa_workflow_core::runtime_prereqs::RuntimePrereqs,
    output_dir: &Path,
) -> Result<()> {
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    let path = runtime.join("dependency-lock.json");
    let mut lock = ecaa_workflow_core::dependency_lock::RequestedLock::from_prereqs(prereqs);
    lock.fold_from_package_outputs(output_dir);
    let body = serde_json::to_vec_pretty(&attach_capture_status(&lock)?)
        .context("serializing dependency-lock.json")?;
    tokio::fs::write(&path, body)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// How much of the package-level lock is real.
///
/// Distinguishes the three states that bare empty columns conflate. Pure +
/// unit-testable.
///
/// - `captured_from_run` — at least one entry carries a `resolved` exact
///   version folded out of a per-task `env.lock` / `env.explicit.lock`. This is
///   what ACTUALLY ran.
/// - `requested_only_not_captured` — the composer requested packages but no run
///   evidence has been folded in yet.
/// - `not_captured` — nothing requested and nothing captured. NOT a claim that
///   the package has no dependencies.
fn capture_status_of(lock: &ecaa_workflow_core::dependency_lock::RequestedLock) -> &'static str {
    let columns = [&lock.r, &lock.python, &lock.conda];
    if columns
        .iter()
        .any(|c| c.iter().any(|e| e.resolved.is_some()))
    {
        "captured_from_run"
    } else if columns.iter().any(|c| !c.is_empty()) {
        "requested_only_not_captured"
    } else {
        "not_captured"
    }
}

/// Serialize the lock and stamp `capture_status` + a reader-facing note.
///
/// `RequestedLock` is core-owned and its column shape is the stable contract, so
/// the status is attached additively — the `r` / `python` / `conda` arrays and
/// `schema_version` are untouched. Key order is `serde_json::Map`'s (BTreeMap)
/// ordering, so the file stays byte-reproducible across emits (unlike
/// `determinism-shim.json`, `dependency-lock.json` is NOT on the byte-diff
/// exclusion allowlist).
fn attach_capture_status(
    lock: &ecaa_workflow_core::dependency_lock::RequestedLock,
) -> Result<serde_json::Value> {
    let status = capture_status_of(lock);
    let mut value = serde_json::to_value(lock).context("serializing dependency-lock payload")?;
    let obj = value
        .as_object_mut()
        .context("dependency-lock payload is not a JSON object")?;
    obj.insert(
        "capture_status".to_string(),
        serde_json::Value::String(status.to_string()),
    );
    if status != "captured_from_run" {
        obj.insert(
            "capture_note".to_string(),
            serde_json::Value::String(
                "Empty or resolved-free columns mean NOT-YET-CAPTURED, not \
                 'this package has no dependencies'. The versions that actually ran are \
                 recorded per task in runtime/outputs/<task_id>/env.explicit.lock (and \
                 env.lock); a finalize-time backfill unions those into these columns and \
                 flips capture_status to captured_from_run."
                    .to_string(),
            ),
        );
    }
    Ok(value)
}

#[cfg(test)]
mod dependency_lock_tests {
    use super::{attach_capture_status, capture_status_of};
    use ecaa_workflow_core::dependency_lock::RequestedLock;
    use ecaa_workflow_core::runtime_prereqs::RuntimePrereqs;

    /// Atoms declare no package prereqs, so the requested side is empty at emit
    /// while the agents' per-task `env.explicit.lock` files are full conda locks.
    /// A bare `{"r":[],"python":[],"conda":[]}` therefore asserts something false
    /// — that the package has no dependencies. The emitted lock must say
    /// `not_captured` instead, and point the reader at where the real versions
    /// live.
    #[test]
    fn empty_prereqs_do_not_emit_a_false_empty_lock() {
        let lock = RequestedLock::from_prereqs(&RuntimePrereqs::new());
        assert_eq!(capture_status_of(&lock), "not_captured");

        let value = attach_capture_status(&lock).expect("attach capture status");
        assert_eq!(value["capture_status"], "not_captured");
        assert!(
            value["capture_note"]
                .as_str()
                .is_some_and(|n| n.contains("env.explicit.lock")),
            "an empty lock must name where the captured versions actually live"
        );
        // The core-owned column shape is untouched.
        assert_eq!(value["schema_version"], "1");
        assert!(value["python"].as_array().is_some_and(|a| a.is_empty()));
    }

    /// A requested-but-unresolved package is distinct from nothing-captured, and
    /// both are distinct from a lock folded out of real run evidence.
    #[test]
    fn capture_status_separates_requested_from_captured() {
        let mut p = RuntimePrereqs::new();
        p.language_packages.python = ["scanpy>=1.10".into()].into();
        let mut lock = RequestedLock::from_prereqs(&p);
        assert_eq!(capture_status_of(&lock), "requested_only_not_captured");

        lock.fold_resolved("python", "scanpy", "1.10.4");
        assert_eq!(capture_status_of(&lock), "captured_from_run");
        let value = attach_capture_status(&lock).expect("attach capture status");
        assert_eq!(value["capture_status"], "captured_from_run");
        assert!(
            value.get("capture_note").is_none(),
            "a captured lock needs no disambiguating note"
        );
    }
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
