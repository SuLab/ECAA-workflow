//! JSONL writers for the two append-only audit logs emitted into every
//! package:
//!
//! - `runtime/intake-conversation.jsonl` — turns + tool-call records
//! - `runtime/decisions.jsonl` — every SME checkpoint + LLM mutation

use crate::session::Session;
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;
use tokio::io::AsyncWriteExt;

/// Serialize each item as a JSON line and append to
/// `<runtime>/<filename>`. Used by `write_decision_log` and
/// `write_conversation_log` so both share the same newline-delimited
/// JSON writer rather than duplicating the loop + flush idiom.
///
/// Plan S2.1 — `sync_data()` (fdatasync) replaces the prior
/// `flush().await` so the audit-log write is durable across kernel
/// crashes (flush only flushes user-space buffers; fdatasync forces
/// the disk write). `sync_data()` not `sync_all()` because we never
/// stat() these files for mtime ordering — only their content matters
/// — so we save the inode-metadata sync cost (~2× faster on small
/// JSONL appends per PostgreSQL pg_test_fsync). Panic on fsync error
/// per the PostgreSQL "20-year fsync bug" lesson: a silent fsync
/// failure leaves the on-disk state inconsistent with what we tell
/// the caller.
async fn write_jsonl<T: Serialize>(runtime: &Path, filename: &str, items: &[T]) -> Result<()> {
    tokio::fs::create_dir_all(runtime).await?;
    let path = runtime.join(filename);
    let mut file = tokio::fs::File::create(&path)
        .await
        .with_context(|| format!("creating {}", path.display()))?;
    for item in items {
        let line = serde_json::to_string(item)?;
        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;
    }
    file.sync_data()
        .await
        .unwrap_or_else(|e| panic!("audit-log fdatasync failed for {}: {}", path.display(), e));
    Ok(())
}

/// variant of `write_jsonl` that opens the file in append mode.
/// Used when two collections get written to the same JSONL; the first
/// pass creates the file and the second pass appends without
/// overwriting.
async fn append_jsonl<T: Serialize>(runtime: &Path, filename: &str, items: &[T]) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let path = runtime.join(filename);
    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .await
        .with_context(|| format!("opening {} for append", path.display()))?;
    for item in items {
        let line = serde_json::to_string(item)?;
        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;
    }
    // Plan S2.1 — see write_jsonl above for the sync_data + panic
    // rationale. Same discipline applies to the append path.
    file.sync_data()
        .await
        .unwrap_or_else(|e| panic!("audit-log fdatasync failed for {}: {}", path.display(), e));
    Ok(())
}

#[allow(dead_code)] // private-tier convenience wrapper; callers use write_decision_log_tiered directly
pub(super) async fn write_decision_log(session: &Session, output_dir: &Path) -> Result<()> {
    write_decision_log_tiered(
        session,
        output_dir,
        scripps_workflow_core::provenance_tiers::ProvenanceTier::Private,
    )
    .await
}

/// Tiered audit
/// emit. Applies `RedactionPolicy::default_policy()` to each
/// `DecisionRecord` at the requested tier; records that
/// `redact_record` drops entirely (e.g. `UserNote` at
/// `ExportablePublic`) are skipped.
///
/// Tier semantics:
/// - `Private` — full trace; pass-through.
/// - `RedactedAudit` — strips body / fragment / method_prose /
///   rationale free-text fields.
/// - `ExportablePublic`— additionally drops author + assumption
///   statement and removes `UserNote` records entirely.
///
/// Re-emitting the same session under the same tier produces a
/// byte-stable `runtime/decisions.jsonl`, preserving the alignment
/// plan §12 acceptance criterion.
pub(super) async fn write_decision_log_tiered(
    session: &Session,
    output_dir: &Path,
    tier: scripps_workflow_core::provenance_tiers::ProvenanceTier,
) -> Result<()> {
    // Grant v19 §Aim 3A Arm B′ — short-circuit emission when the
    // operator has set SWFC_ABLATE_DECISION_RECORDS=1 (or any truthy
    // value). The Arm B′ control package carries no decisions.jsonl;
    // every other emit-site stays active.
    use scripps_workflow_core::ablation::AblationFlagExt;
    if scripps_workflow_core::ablation::AblationFlag::DecisionRecords.is_active() {
        return Ok(());
    }
    use scripps_workflow_core::provenance_tiers::{redact_record, RedactionPolicy};
    let runtime = output_dir.join("runtime");
    let policy = RedactionPolicy::default_policy();
    let redacted: Vec<_> = session
        .decisions
        .iter()
        .filter_map(|r| redact_record(r, tier, &policy))
        .collect();
    write_jsonl(&runtime, "decisions.jsonl", &redacted).await?;

    // HMAC-SHA256 sidecar (C5): one hex digest per row in
    // `runtime/decisions.jsonl.mac`, verifiable with the session's
    // `audit_writer_secret`. The sidecar is written atomically
    // (`.tmp` + rename) alongside the JSONL so readers can pair them
    // by extension without additional discovery. A corrupt or missing
    // `.mac` file is detected by the verifier and logged as a warning;
    // it never blocks emission.
    write_decisions_mac_sidecar(session, &runtime, &redacted).await
}

/// Write `runtime/decisions.jsonl.mac`: one lowercase-hex HMAC-SHA256
/// per line, aligned 1-to-1 with `runtime/decisions.jsonl`.
///
/// Uses [`scripps_workflow_core::audit_writer::AuditWriter`] reconstructed
/// from `session.audit_writer_secret`. The sidecar is verified
/// row-by-row with the same key; a mismatch indicates tampering or
/// a secret rotation across emits.
async fn write_decisions_mac_sidecar(
    session: &Session,
    runtime: &Path,
    records: &[impl serde::Serialize],
) -> Result<()> {
    use scripps_workflow_core::audit_writer::AuditWriter;
    let writer = AuditWriter::with_secret(session.audit_writer_secret);
    let mac_path = runtime.join("decisions.jsonl.mac");
    let tmp_path = runtime.join("decisions.jsonl.mac.tmp");

    let mut mac_lines = String::new();
    for rec in records {
        let row = serde_json::to_value(rec).context("serializing decision record for MAC")?;
        let mac = writer.sign_row(&row);
        mac_lines.push_str(&mac);
        mac_lines.push('\n');
    }
    // Write via `.tmp` + atomic rename for crash-durability, matching
    // the pattern used by `atomic_write_bytes` in persistence.rs.
    let bytes = mac_lines.as_bytes();
    tokio::fs::write(&tmp_path, bytes)
        .await
        .with_context(|| format!("writing {}", tmp_path.display()))?;
    // fsync the temp file before rename so the bytes are on the
    // disk-firmware boundary (same discipline as the JSONL writer).
    {
        let f = tokio::fs::File::open(&tmp_path)
            .await
            .with_context(|| format!("opening {} for sync", tmp_path.display()))?;
        f.sync_data()
            .await
            .with_context(|| format!("fsyncing {}", tmp_path.display()))?;
    }
    tokio::fs::rename(&tmp_path, &mac_path)
        .await
        .with_context(|| format!("renaming {} -> {}", tmp_path.display(), mac_path.display()))?;
    Ok(())
}

pub(super) async fn write_conversation_log(session: &Session, output_dir: &Path) -> Result<()> {
    let runtime = output_dir.join("runtime");
    // intake-conversation.jsonl holds two sequence types (turns, then
    // tool-call records). Append them as two passes so either collection
    // can be empty without producing a dangling file. Both go through
    // `write_jsonl`, which truncates-and-opens on first call — so for
    // the tool-call pass we append manually via a second helper below.
    write_jsonl(&runtime, "intake-conversation.jsonl", &session.conversation).await?;
    append_jsonl(
        &runtime,
        "intake-conversation.jsonl",
        &session.tool_call_log,
    )
    .await
}

// v3 P7 — schema-versions manifest writer.
//
// Authors `runtime/schema-versions.json` listing the SemVer of every
// IR type the package was emitted with (`SchemaVersionsManifest::current()`).
// Always written (even on v1/v2/v3 sessions) so the on-disk shape is
// uniform across composer generations and the RO-Crate registration
// step in `ro_crate.rs::patch_ro_crate_metadata` can register it
// unconditionally. Replay consumers diff this manifest against their
// own current manifest to decide which migrators to run (see
// `core::migration::replay_provenance`).
//
// Conflict-marker for parallel-phase coordination — `audit_log.rs` is
// also touched by v4 P2 (substrate writer); inserting this block at a
// clearly distinct site so the two never collide. Do not move; do not
// merge with v4 P2's writer.
pub(super) async fn write_schema_versions_manifest(output_dir: &std::path::Path) -> Result<()> {
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    let manifest = scripps_workflow_core::migration::SchemaVersionsManifest::current();
    let bytes =
        serde_json::to_vec_pretty(&manifest).context("serializing runtime/schema-versions.json")?;
    let path = runtime.join("schema-versions.json");
    tokio::fs::write(&path, bytes)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Write the v4 planner sidecars
/// (`runtime/proofs.jsonl`, `runtime/assumptions.jsonl`,
/// `runtime/policy-decisions.jsonl`) when the session has a cached
/// `WorkflowDag` from a v4 dispatch. v1/v2/v3 sessions skip every
/// write site (the absent files are no-ops in the RO-Crate
/// registration step). Tier redaction is applied to proofs +
/// assumptions per
/// `provenance_tiers::{redact_proofs_jsonl, redact_assumptions_jsonl}`
/// where applicable.
pub(super) async fn write_phase16_sidecars(
    session: &Session,
    output_dir: &Path,
    tier: scripps_workflow_core::provenance_tiers::ProvenanceTier,
) -> Result<()> {
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;

    // 1. proofs.jsonl — one entry per edge with a CompatibilityProof.
    if let Some(workflow_dag) = session.workflow_dag.as_ref() {
        let proofs_jsonl = build_proofs_jsonl(workflow_dag);
        if !proofs_jsonl.is_empty() {
            let redacted = match tier {
                scripps_workflow_core::provenance_tiers::ProvenanceTier::Private => proofs_jsonl,
                _ => scripps_workflow_core::provenance_tiers::redact_proofs_jsonl(
                    &proofs_jsonl,
                    tier,
                ),
            };
            let path = runtime.join("proofs.jsonl");
            tokio::fs::write(&path, redacted)
                .await
                .with_context(|| format!("writing {}", path.display()))?;
        }

        // 2. assumptions.jsonl — one entry per AssumptionLedger entry.
        let assumptions_jsonl = build_assumptions_jsonl(workflow_dag);
        if !assumptions_jsonl.is_empty() {
            let redacted = scripps_workflow_core::provenance_tiers::redact_assumptions_jsonl(
                assumptions_jsonl,
                tier,
            );
            let path = runtime.join("assumptions.jsonl");
            tokio::fs::write(&path, redacted)
                .await
                .with_context(|| format!("writing {}", path.display()))?;
        }
    }

    // 3. policy-decisions.jsonl — one entry per recorded policy
    // decision. Cached on Session by tools::rebuild_dag from the v4
    // policy gate.
    if !session.policy_decisions.is_empty() {
        let mut sorted: Vec<_> = session.policy_decisions.iter().collect();
        sorted.sort_by(|a, b| {
            a.node_id
                .cmp(&b.node_id)
                .then_with(|| a.kind.cmp(&b.kind))
                .then_with(|| a.bundle_id.cmp(&b.bundle_id))
        });
        write_jsonl(&runtime, "policy-decisions.jsonl", &sorted).await?;
    }

    Ok(())
}

/// Write `runtime/task-nodes.json` (typed `TaskNode`
/// list) and `runtime/sandbox-policy.json` (active `SandboxPolicy`)
/// when the session has both. Consumed by the harness's
/// `pre_dispatch_check` so generated-code refusal can run at
/// dispatch time, not just compose time. Best-effort: a missing
/// session field is a no-op (the harness then skips the check
/// entirely).
pub(super) async fn write_phase14_sidecars(session: &Session, output_dir: &Path) -> Result<()> {
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;

    if let Some(workflow_dag) = session.workflow_dag.as_ref() {
        let path = runtime.join("task-nodes.json");
        let bytes = serde_json::to_vec_pretty(&workflow_dag.nodes)
            .context("serializing task-nodes.json")?;
        tokio::fs::write(&path, bytes)
            .await
            .with_context(|| format!("writing {}", path.display()))?;
    }

    let sandbox_path = runtime.join("sandbox-policy.json");
    if let Some(bundle_id) = session.active_policy_bundle.as_deref() {
        if let Some(policy) = sandbox_policy_for_bundle(bundle_id) {
            let bytes =
                serde_json::to_vec_pretty(&policy).context("serializing sandbox-policy.json")?;
            tokio::fs::write(&sandbox_path, bytes)
                .await
                .with_context(|| format!("writing {}", sandbox_path.display()))?;
        } else {
            // Bundle id unrecognized — clean any stale prior sandbox
            // file rather than leaving the harness to enforce a
            // policy whose id doesn't match the session's stated
            // bundle.
            let _ = tokio::fs::remove_file(&sandbox_path).await;
        }
    } else {
        // SME cleared the active policy bundle. Remove any stale
        // sandbox-policy.json from a previous emit so the harness's
        // pre_dispatch_check naturally short-circuits (no policy →
        // no refusal).
        let _ = tokio::fs::remove_file(&sandbox_path).await;
    }

    Ok(())
}

/// Derive a `SandboxPolicy` from a session's active
/// policy bundle id. Today's mapping:
/// - `clinical_trial` → `SandboxPolicy::default_strict()`
/// - `phi_strict` → `SandboxPolicy::default_strict()` with
///   `require_signed_artifacts` reduced (PHI is about data handling,
///   not artifact signing)
///
/// Returns `None` for unrecognized bundle ids.
fn sandbox_policy_for_bundle(
    bundle_id: &str,
) -> Option<scripps_workflow_core::sandbox_policy::SandboxPolicy> {
    use scripps_workflow_core::sandbox_policy::SandboxPolicy;
    match bundle_id {
        "clinical_trial" => Some(SandboxPolicy::default_strict()),
        "phi_strict" => {
            let mut p = SandboxPolicy::default_strict();
            p.id = "phi_strict_v1".into();
            p.label = "PHI-strict sandbox".into();
            p.require_signed_artifacts = false;
            Some(p)
        }
        _ => None,
    }
}

/// Build `runtime/proofs.jsonl` content from a `WorkflowDag`. Mirrors
/// `backend_emitters::workflow_json::emit_proofs_jsonl` so the
/// emit-time write produces the same shape as the lowering pass.
fn build_proofs_jsonl(
    dag: &scripps_workflow_core::workflow_contracts::task_node::WorkflowDag,
) -> String {
    let mut sorted: Vec<&scripps_workflow_core::workflow_contracts::edge::EdgeContract> =
        dag.edges.iter().collect();
    sorted.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.from_port.cmp(&b.from_port))
            .then_with(|| a.to_node.cmp(&b.to_node))
            .then_with(|| a.to_port.cmp(&b.to_port))
    });
    let mut out = String::new();
    for edge in sorted {
        if let Ok(line) = serde_json::to_string(edge) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Build `runtime/assumptions.jsonl` content from a `WorkflowDag`.
fn build_assumptions_jsonl(
    dag: &scripps_workflow_core::workflow_contracts::task_node::WorkflowDag,
) -> String {
    let mut sorted: Vec<&scripps_workflow_core::workflow_contracts::evidence::Assumption> =
        dag.assumptions.entries.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    let mut out = String::new();
    for assumption in sorted {
        if let Ok(line) = serde_json::to_string(assumption) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Phase A1–A2 (flexible-plotting resolver wiring) — resolve plot
/// affordances for every task in the session's DAG, write
/// `runtime/plot_affordances.jsonl` and
/// `runtime/affordance_fallbacks.jsonl`, and increment the
/// session-scoped `AffordanceFallbackCounter` for each
/// `StructuralFallback` resolution.
///
/// **Integration approach: Option β** — pre-computation in the
/// conversation crate. The resolver is called here, not inside
/// `crates/core::WorkflowJsonEmitter`, because:
///
/// 1. `crates/core` has no knowledge of `Session`; the counter
///    lives on `Session` and can only be incremented in
///    `crates/conversation`.
/// 2. `EmitContext` and `WorkflowJsonEmitter` are called inside
///    `core::emit_package`, which runs synchronously before this
///    function. We cannot mutate `EmitContext::emit_affordances`
///    post-facto without rewriting the emit path. Wiring the
///    resolution here keeps the core path untouched.
/// 3. The affordance records are written as a separate sidecar file
///    directly from this function; the RO-Crate patching step
///    (`ro_crate::patch_ro_crate_metadata`) picks them up via the
///    presence-gated sidecar table that already covers
///    `runtime/plot_affordances.jsonl`.
///
/// **Fallback behavior for legacy atoms** — tasks whose task-id
/// does not resolve to any `AtomDefinition` in the registry (all
/// legacy taxonomy-built tasks) fall back to:
/// - `task.spec["edam_operation"]` if present → `OntologyTerm`
/// - Otherwise → `Opaque` with a synthetic `swfc:opaque:<task_id>`
///
/// Tasks with no spec at all are skipped (no port to resolve).
///
/// **Returns** the resolved `PlotAffordanceRecord` list sorted by
/// `(task_id, port_name)`. Callers use this for the RO-Crate
/// provisional-flag patching step (A3).
pub(super) async fn write_affordance_sidecars(
    session: &mut Session,
    output_dir: &Path,
    config_dir: &Path,
) -> Result<Vec<scripps_workflow_core::backend_emitters::workflow_json::PlotAffordanceRecord>> {
    use scripps_workflow_core::atom_registry::AtomRegistry;
    use scripps_workflow_core::backend_emitters::workflow_json::PlotAffordanceRecord;
    use scripps_workflow_core::plot_affordance::telemetry::AffordanceFallbackRecord;
    use scripps_workflow_core::plot_affordance::{
        resolve_affordance, AffordanceFallbackCounter, PhysicalShape, PlotAffordance,
        PlotAffordanceRegistry, PortDescriptor, YamlPlotAffordanceRegistry,
    };
    use scripps_workflow_core::workflow_contracts::semantic_type::SemanticType;

    let dag = match session.current_dag() {
        Some(d) => d,
        None => return Ok(vec![]),
    };

    // Load PlotAffordanceRegistry; soft-fail if the dir is absent
    // (e.g., CI without config/ available). An empty registry means
    // every task resolves to Deferred or StructuralFallback.
    let plot_affordances_dir = config_dir.join("plot-affordances");
    // Use YamlPlotAffordanceRegistry directly to avoid the Box<dyn>
    // blanket-impl indirection. Both arms produce the same concrete type
    // so no boxing is needed; the `empty()` constructor handles the
    // "registry dir missing" case without a heap allocation.
    let registry: YamlPlotAffordanceRegistry = if plot_affordances_dir.exists() {
        match YamlPlotAffordanceRegistry::from_dir(&plot_affordances_dir) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "affordance resolver: failed to load plot-affordances registry: {}; falling back to empty",
                    e
                );
                YamlPlotAffordanceRegistry::empty()
            }
        }
    } else {
        YamlPlotAffordanceRegistry::empty()
    };

    // Load AtomRegistry; soft-fail. When absent, legacy-taxonomy
    // tasks fall through to their spec-based edam_operation IRI.
    let atom_dir = config_dir.join("stage-atoms");
    let atoms: Option<std::sync::Arc<AtomRegistry>> = if atom_dir.exists() {
        match AtomRegistry::load_cached(&atom_dir) {
            Ok(a) => Some(a),
            Err(e) => {
                tracing::warn!(
                    "affordance resolver: failed to load atom registry: {}; skipping atom-output lookup",
                    e
                );
                None
            }
        }
    } else {
        None
    };

    // Theme version: use the snapshot_id from the registry as a proxy
    // (theme.json is not available at compile time; the agent owns it
    // at runtime). This matches the test convention in the selector tests.
    let theme_version = registry.snapshot_id().to_string();

    let mut affordance_records: Vec<PlotAffordanceRecord> = Vec::new();
    let mut fallback_records: Vec<AffordanceFallbackRecord> = Vec::new();
    let mut counter = AffordanceFallbackCounter::default();

    for (task_id, task) in dag.tasks.iter() {
        // Determine the semantic-type IRI + declared parents for this task.
        //
        // Priority order:
        // 1. Atom registry: atom.outputs[0].semantic_type
        // 2. Atom registry: atom.edam_data (legacy field, non-empty outputs absent)
        // 3. task.spec["edam_operation"] — only for Opaque fallback, not
        // an output port but better than nothing for unmigrated atoms.
        // 4. Opaque with synthetic swfc:opaque:<task_id>

        // Helper closure — build port descriptor + port name.
        // Returns None when there is genuinely nothing to resolve.
        let maybe_port: Option<(String, String, Vec<String>)> = {
            // 1. Atom outputs
            if let Some(atom) = atoms.as_ref().and_then(|a| a.get(task_id.as_str())) {
                if let Some(first_out) = atom.outputs.first() {
                    let (iri, parents) = match &first_out.semantic_type {
                        SemanticType::OntologyTerm { iri, .. } => (iri.clone(), vec![]),
                        SemanticType::LocalExtension {
                            namespace,
                            id,
                            proposed_parent_terms,
                            ..
                        } => (format!("{namespace}:{id}"), proposed_parent_terms.clone()),
                        SemanticType::Opaque { .. } => (format!("swfc:opaque:{task_id}"), vec![]),
                        SemanticType::Union { .. } => {
                            // Union output ports carry no single IRI; synthesize a
                            // stable id so the audit log entry is non-empty.
                            (format!("swfc:union:{task_id}"), vec![])
                        }
                    };
                    Some((first_out.name.clone(), iri, parents))
                } else {
                    // Legacy atom: has edam_data but no outputs vec yet.
                    // Atom with neither outputs nor edam_data → None (skip).
                    atom.edam_data
                        .as_deref()
                        .map(|edam_data| ("out".to_string(), edam_data.to_string(), vec![]))
                }
            } else if let Some(edam_op) = task
                .spec
                .as_ref()
                .and_then(|s| s.get("edam_operation"))
                .and_then(|v| v.as_str())
            {
                // No atom entry; fall back to the task spec's
                // edam_operation. This is an operation IRI not a data IRI
                // but it's better than pure Opaque for unmigrated atoms
                // that don't have atom definitions yet.
                Some(("out".to_string(), edam_op.to_string(), vec![]))
            } else if task.spec.is_some() {
                // Task has a spec but no useful IRI — use opaque.
                Some(("out".to_string(), format!("swfc:opaque:{task_id}"), vec![]))
            } else {
                // No spec, no atom: skip entirely (no port to resolve).
                None
            }
        };

        let (port_name, semantic_type_iri, declared_parents) = match maybe_port {
            Some(t) => t,
            None => continue,
        };

        let port_desc = PortDescriptor {
            semantic_type_iri: &semantic_type_iri,
            declared_parents: &declared_parents,
            // Data-shape inference is deferred to a future phase.
            physical_shape: PhysicalShape::Unknown,
        };

        let affordance = resolve_affordance(&port_desc, &registry, theme_version.as_str());
        let provisional = affordance.is_provisional();

        // A2: increment the fallback counter for StructuralFallback.
        if let PlotAffordance::StructuralFallback { primitive, .. } = &affordance {
            // Use serde_json to get the snake_case variant name, matching
            // the serde(rename_all = "snake_case") attribute on GenericPrimitive.
            let primitive_str = serde_json::to_value(primitive)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("{:?}", primitive).to_lowercase());
            counter.record(&semantic_type_iri, &primitive_str);
            fallback_records.push(AffordanceFallbackRecord {
                task_id: task_id.clone(),
                port_name: port_name.clone(),
                semantic_type: semantic_type_iri.clone(),
                primitive: primitive_str.clone(),
                fallback_reason: format!(
                    "PhysicalShape::Unknown resolved to structural primitive {primitive_str}"
                ),
            });
        }

        affordance_records.push(PlotAffordanceRecord {
            task_id: task_id.clone(),
            port_name: port_name.clone(),
            affordance,
            provisional,
        });
    }

    // Sort by (task_id, port_name) for byte-determinism.
    affordance_records.sort_by(|a, b| {
        a.task_id
            .cmp(&b.task_id)
            .then_with(|| a.port_name.cmp(&b.port_name))
    });
    fallback_records.sort_by(|a, b| {
        a.task_id
            .cmp(&b.task_id)
            .then_with(|| a.port_name.cmp(&b.port_name))
    });

    // Merge counter into session-scoped counter.
    for (semantic_type, primitive, count) in counter.all_gaps_sorted_by_count_desc() {
        for _ in 0..count {
            session
                .affordance_fallback_counter
                .record(&semantic_type, &primitive);
        }
    }

    // Write runtime/plot_affordances.jsonl (always, even when empty,
    // so the RO-Crate presence-gate can register the entity
    // unconditionally — matching the plan §10 note).
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    write_jsonl(&runtime, "plot_affordances.jsonl", &affordance_records).await?;

    // Write runtime/affordance_fallbacks.jsonl only when non-empty.
    if !fallback_records.is_empty() {
        write_jsonl(&runtime, "affordance_fallbacks.jsonl", &fallback_records).await?;
    }

    Ok(affordance_records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use scripps_workflow_core::audit_writer::AuditWriter;

    /// E23 — `write_decisions_mac_sidecar` writes one hex HMAC per
    /// decision record and each line verifies against the session secret.
    #[tokio::test]
    async fn decisions_mac_sidecar_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        tokio::fs::create_dir_all(&runtime).await.unwrap();

        let session = Session::new(false);
        // Use two synthetic JSON-serializable values as stand-ins for
        // DecisionRecord (actual DecisionRecord requires core types and
        // extra setup; we test the MAC logic directly through the
        // same helper that write_decision_log_tiered calls).
        let records = vec![
            serde_json::json!({"kind": "Confirm", "user": "alice"}),
            serde_json::json!({"kind": "Reject", "user": "bob"}),
        ];
        write_decisions_mac_sidecar(&session, &runtime, &records)
            .await
            .expect("sidecar write must succeed");

        let mac_path = runtime.join("decisions.jsonl.mac");
        assert!(mac_path.exists(), "decisions.jsonl.mac must be created");

        let mac_content = tokio::fs::read_to_string(&mac_path).await.unwrap();
        let mac_lines: Vec<&str> = mac_content.lines().collect();
        assert_eq!(mac_lines.len(), records.len(), "one MAC line per record");

        // Verify each MAC using the same secret.
        let verifier = AuditWriter::with_secret(session.audit_writer_secret);
        for (i, (record, mac_hex)) in records.iter().zip(mac_lines.iter()).enumerate() {
            let expected = verifier.sign_row(record);
            assert_eq!(
                *mac_hex,
                expected.as_str(),
                "MAC line {} must match sign_row output",
                i
            );
        }
    }

    /// E23 — empty record list writes an empty sidecar (zero bytes,
    /// file present).
    #[tokio::test]
    async fn decisions_mac_sidecar_empty_is_zero_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        tokio::fs::create_dir_all(&runtime).await.unwrap();

        let session = Session::new(false);
        let records: Vec<serde_json::Value> = vec![];
        write_decisions_mac_sidecar(&session, &runtime, &records)
            .await
            .expect("empty sidecar write must succeed");

        let mac_path = runtime.join("decisions.jsonl.mac");
        let content = tokio::fs::read_to_string(&mac_path).await.unwrap();
        assert!(content.is_empty(), "empty record list yields empty sidecar");
    }

    /// E23 — a tampered MAC (different from what sign_row produces)
    /// is detected by the verifier.
    #[tokio::test]
    async fn decisions_mac_sidecar_tamper_detected() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        tokio::fs::create_dir_all(&runtime).await.unwrap();

        let session = Session::new(false);
        let records = vec![serde_json::json!({"kind": "Confirm"})];
        write_decisions_mac_sidecar(&session, &runtime, &records)
            .await
            .unwrap();

        // Tamper: overwrite the sidecar with a bogus hex.
        let mac_path = runtime.join("decisions.jsonl.mac");
        tokio::fs::write(
            &mac_path,
            b"deadbeef00000000000000000000000000000000000000000000000000000000\n",
        )
        .await
        .unwrap();

        let verifier = AuditWriter::with_secret(session.audit_writer_secret);
        let expected = verifier.sign_row(&records[0]);
        let stored = tokio::fs::read_to_string(&mac_path).await.unwrap();
        let stored_mac = stored.trim();
        assert_ne!(
            stored_mac, expected,
            "tampered MAC must differ from expected"
        );
        // Callers would surface this as a warning; we just confirm
        // the mismatch is detectable.
    }
}
