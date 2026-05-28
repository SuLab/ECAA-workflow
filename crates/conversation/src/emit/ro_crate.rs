//! RO-Crate metadata patching: register new CreativeWork entities, link
//! them from the root Dataset, and annotate with `schema:isBasedOn` for
//! cross-version diffs. Every operation is idempotent so re-emission
//! over an existing package produces the same metadata.
//!
//! v3 P5 F16 — before the patcher returns, every registered
//! JSONL sidecar is scanned for PHI patterns via
//! `provenance_tiers::detect_phi_leak`. Non-empty leaks at a
//! non-`Private` tier abort the emit with an error citing the
//! offending line + JSON pointer.
//!
//! Literature atoms (spec §7.3 / §7.4) — each `evidence/manifest.json`
//! under `runtime/outputs/<stage_id>/` is detected and produces
//! `CreativeWork` + `ScholarlyArticle` nodes in the `@graph`. CSVs gain
//! `prov:wasDerivedFrom` edges. Redistributable=false entries are omitted
//! from the shareable export but their metadata nodes are preserved with
//! `swfc:contentOmittedFromExport: true`.

use anyhow::{anyhow, bail, Context, Result};
use ecaa_workflow_core::ablation::AblationFlagExt;
use ecaa_workflow_core::backend_emitters::workflow_json::PlotAffordanceRecord;
use ecaa_workflow_core::provenance_tiers::{detect_phi_leak, ProvenanceTier};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use tracing::instrument;

// ── Literature-evidence structs (mirror crates/harness/src/literature_validators.rs) ──

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct EvidenceManifest {
    pub entries: Vec<EvidenceEntry>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct EvidenceEntry {
    pub pmid: String,
    pub source_kind: String,
    pub path: String,
    pub sha256_binary: String,
    pub sha256_extracted_text: String,
    pub bytes: u64,
    pub retrieval_ts: String,
    pub redistributable: bool,
    pub license: String,
}

/// Normalise a CC-style license string to a full URI when recognisable.
/// Unrecognised strings are returned verbatim.
fn normalise_license(license: &str) -> String {
    match license {
        "CC-BY-4.0" => "https://creativecommons.org/licenses/by/4.0/".into(),
        "CC-BY-NC-4.0" => "https://creativecommons.org/licenses/by-nc/4.0/".into(),
        "CC0-1.0" => "https://creativecommons.org/publicdomain/zero/1.0/".into(),
        other => other.to_string(),
    }
}

/// Detect literature output directories under `package_root/runtime/outputs/*/`
/// and register evidence entries as `CreativeWork`/`ScholarlyArticle` nodes.
/// CSVs (`prior_claims_matrix.csv`, `claims_evidence_matrix.csv`) get
/// `prov:wasDerivedFrom` edges to the set of cited evidence `@id`s.
///
/// When `shareable` is `true`, entries with `redistributable: false` have
/// their content omitted from the shareable bundle and carry
/// `"swfc:contentOmittedFromExport": true` in the metadata node.
///
/// All operations are idempotent: existing nodes with the same `@id` are
/// left unchanged.
fn register_literature_evidence(
    graph: &mut Vec<serde_json::Value>,
    package_root: &Path,
    shareable: bool,
) -> Result<()> {
    let outputs_dir = package_root.join("runtime/outputs");
    if !outputs_dir.exists() {
        return Ok(());
    }

    let read_dir = std::fs::read_dir(&outputs_dir)
        .with_context(|| format!("reading {}", outputs_dir.display()))?;

    for entry in read_dir.flatten() {
        let stage_dir = entry.path();
        if !stage_dir.is_dir() {
            continue;
        }
        let manifest_path = stage_dir.join("evidence/manifest.json");
        if !manifest_path.exists() {
            continue;
        }

        // Load the evidence manifest.
        let manifest_bytes = std::fs::read(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let manifest: EvidenceManifest = serde_json::from_slice(&manifest_bytes)
            .with_context(|| format!("parsing {}", manifest_path.display()))?;

        // Relative path from package root for the stage dir.
        let stage_rel = stage_dir
            .strip_prefix(package_root)
            .unwrap_or(&stage_dir)
            .to_string_lossy()
            .replace('\\', "/");

        // Build CreativeWork nodes for each evidence entry.
        let mut evidence_ids: Vec<serde_json::Value> = Vec::new();
        for ev in &manifest.entries {
            let evidence_rel_path = format!("{}/evidence/{}", stage_rel, ev.path);

            // Idempotent: skip if already present.
            let already = graph
                .iter()
                .any(|e| e.get("@id").and_then(|v| v.as_str()) == Some(&evidence_rel_path));
            if already {
                evidence_ids.push(serde_json::json!({ "@id": evidence_rel_path }));
                continue;
            }

            let mut node = serde_json::json!({
                "@id": evidence_rel_path,
                "@type": ["CreativeWork", "ScholarlyArticle"],
                "identifier": format!("PMID:{}", ev.pmid),
                "sameAs": format!("https://pubmed.ncbi.nlm.nih.gov/{}/", ev.pmid),
                "license": normalise_license(&ev.license),
                "contentSize": ev.bytes,
                "hasDigest": format!("sha256:{}", ev.sha256_binary),
                "swfc:hasExtractedTextDigest": format!("sha256:{}", ev.sha256_extracted_text),
                "swfc:sourceKind": ev.source_kind,
                "swfc:redistributable": ev.redistributable,
                "swfc:retrievalTimestamp": ev.retrieval_ts,
            });

            if shareable && !ev.redistributable {
                node.as_object_mut().unwrap().insert(
                    "swfc:contentOmittedFromExport".into(),
                    serde_json::Value::Bool(true),
                );
            }

            graph.push(node);
            evidence_ids.push(serde_json::json!({ "@id": evidence_rel_path }));
        }

        // Register the CSV artifact(s) with prov:wasDerivedFrom.
        let csv_names = ["prior_claims_matrix.csv", "claims_evidence_matrix.csv"];
        for csv_name in csv_names {
            let csv_abs = stage_dir.join(csv_name);
            if !csv_abs.exists() {
                continue;
            }
            let csv_rel = format!("{}/{}", stage_rel, csv_name);
            let already = graph
                .iter()
                .any(|e| e.get("@id").and_then(|v| v.as_str()) == Some(&csv_rel));
            if already {
                continue;
            }
            // Grant v19 §Aim 3A Arm B′ — when
            // ECAA_ABLATE_AMENDMENT_PROVENANCE is truthy, omit the
            // prov:wasDerivedFrom edges so the Arm B′ control package
            // carries no amendment-lineage graph. The CSV node itself
            // is still registered (so the file is still discoverable).
            let mut entry = serde_json::json!({
                "@id": csv_rel,
                "@type": ["File", "CreativeWork", "Dataset"],
                "encodingFormat": "text/csv",
            });
            if !ecaa_workflow_core::ablation::AblationFlag::AmendmentProvenance.is_active() {
                entry
                    .as_object_mut()
                    .expect("constructed as object literal above")
                    .insert(
                        "prov:wasDerivedFrom".to_string(),
                        serde_json::Value::Array(evidence_ids.clone()),
                    );
            }
            graph.push(entry);
        }
    }
    Ok(())
}

// ── Public sync entry points for tests and server callers ─────────────────

/// Emit literature-evidence CreativeWork nodes into a package's
/// `ro-crate-metadata.json`. Returns the updated JSON-LD value.
///
/// Spec §7.3 / §7.4. Called from `patch_ro_crate_metadata` for every emit;
/// exposed as a public synchronous function so integration tests can drive
/// it without spinning up a full `emit_with_conversation_log` pipeline.
#[instrument(fields(package_root = %package_root.display()))]
pub fn emit_ro_crate(package_root: &Path) -> Result<serde_json::Value> {
    emit_ro_crate_inner(package_root, false)
}

/// Shareable-mode variant: redistributable=false content is omitted from
/// the bundle; metadata nodes are preserved with
/// `"swfc:contentOmittedFromExport": true`.
///
/// Spec §7.4.
#[instrument(fields(package_root = %package_root.display()))]
pub fn emit_ro_crate_shareable(package_root: &Path) -> Result<serde_json::Value> {
    emit_ro_crate_inner(package_root, true)
}

fn emit_ro_crate_inner(package_root: &Path, shareable: bool) -> Result<serde_json::Value> {
    let path = package_root.join("ro-crate-metadata.json");
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&bytes).context("parsing ro-crate-metadata.json")?;

    let graph = metadata
        .get_mut("@graph")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| anyhow!("ro-crate-metadata.json missing @graph"))?;

    register_literature_evidence(graph, package_root, shareable)
        .context("registering literature evidence")?;

    Ok(metadata)
}

/// Derive the `swfc:affordanceVariant` snake_case tag from a `PlotAffordance`
/// variant. Matches the serde `rename_all = "snake_case"` tag attribute
/// on `PlotAffordance`. Used to stamp figure entities in the RO-Crate graph.
fn affordance_variant_tag(
    affordance: &ecaa_workflow_core::plot_affordance::PlotAffordance,
) -> &'static str {
    use ecaa_workflow_core::plot_affordance::PlotAffordance;
    match affordance {
        PlotAffordance::Registered { .. } => "registered",
        PlotAffordance::InheritedViaOntology { .. } => "inherited_via_ontology",
        PlotAffordance::StructuralFallback { .. } => "structural_fallback",
        PlotAffordance::GeneratedSandboxed { .. } => "generated_sandboxed",
        PlotAffordance::Deferred { .. } => "deferred",
    }
}

/// Idempotently add a `@type: ["File", "CreativeWork"]` entity
/// to the RO-Crate `@graph`. No-op if an entity with the same `@id`
/// already exists. Replaces the four hand-rolled `any(@id == id) + push`
/// blocks around `runtime/*.jsonl` registrations.
fn register_ro_crate_entity(
    graph: &mut Vec<serde_json::Value>,
    id: &str,
    name: &str,
    description: &str,
    encoding_format: &str,
) {
    let already = graph
        .iter()
        .any(|e| e.get("@id").and_then(|v| v.as_str()) == Some(id));
    if already {
        return;
    }
    graph.push(serde_json::json!({
        "@id": id,
        "@type": ["File", "CreativeWork"],
        "name": name,
        "description": description,
        "encodingFormat": encoding_format,
    }));
}

/// Semantic sidecars produced by the v4 planner +
/// validation pipeline. Registered as RO-Crate `CreativeWork` entities
/// when the corresponding files are present in `output_dir`. Each
/// sidecar's presence is independent: the absence of `proofs.jsonl`
/// (e.g., a v1/v2 emit) means we simply skip its registration.
///
/// Phase A3 (flexible-plotting resolver wiring) — `affordance_records`
/// is the pre-resolved affordance table from `write_affordance_sidecars`.
/// Figure entities (`@type` includes `"ImageObject"`) whose owning
/// task resolved to a non-`Registered` affordance are stamped with:
/// - `"swfc:provisional": true`
/// - `"swfc:affordanceVariant": "<snake_case_tag>"`
///
/// Figure entities whose task has NO affordance record at all (e.g.,
/// legacy atoms with no outputs AND no edam_data) are left un-flagged
/// — absence is treated as legacy/Validated by convention.
pub(super) async fn patch_ro_crate_metadata(
    output_dir: &Path,
    diff_tables: Vec<String>,
    affordance_records: Vec<PlotAffordanceRecord>,
    target_tier: ProvenanceTier,
) -> Result<()> {
    // Build a task_id → (provisional, variant_tag) lookup for A3.
    // Only tasks with a non-Registered affordance contribute an entry;
    // tasks with no record are left unflagged (legacy / Validated).
    let mut affordance_by_task: BTreeMap<String, (bool, &'static str)> = BTreeMap::new();
    for rec in &affordance_records {
        if rec.provisional {
            let tag = affordance_variant_tag(&rec.affordance);
            affordance_by_task.insert(rec.task_id.to_string(), (true, tag));
        }
    }
    let path = output_dir.join("ro-crate-metadata.json");
    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&bytes).context("parsing ro-crate-metadata.json")?;

    let graph = metadata
        .get_mut("@graph")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| anyhow!("ro-crate-metadata.json missing @graph"))?;

    // idempotent entity registration goes through
    // `register_ro_crate_entity`, which folds the `.any(@id == id)`
    // guard inside.
    register_ro_crate_entity(
        graph,
        "runtime/intake-conversation.jsonl",
        "SME intake conversation log",
        "Full LLM-mediated intake conversation including tool calls and results, captured at compile time. Replayable for audit.",
        "application/jsonl",
    );

    // Decision log alongside the conversation log. Each SME-visible
    // checkpoint (confirm, reject, unblock, branch) and each LLM-driven
    // mutation (emit, amend, rerun, sensitivity winner) produces one
    // `DecisionRecord` in this file.
    let decisions_id = "runtime/decisions.jsonl";
    register_ro_crate_entity(
        graph,
        decisions_id,
        "SME decision log",
        "Append-only audit trail of every SME checkpoint decision and every LLM-driven mutation on the session — confirm, reject, unblock, branch, emit_package, amend_stage, rerun_task, select_sensitivity_winner. One DecisionRecord per line.",
        "application/jsonl",
    );

    // Semantic sidecars registered as CreativeWork.
    // Each sidecar is registered only when its file actually exists
    // on disk; absence is a no-op (legacy v1/v2 emits skip the
    // entire block). Flexible-plotting added
    // `runtime/plot_affordances.jsonl` to the same list — the
    // existence-gated registration loop covers it without a
    // dedicated branch.
    let semantic_sidecars: &[(&str, &str, &str, &str)] = &[
        (
            "runtime/proofs.jsonl",
            "Compatibility proofs",
            "Per-edge compatibility proofs from the v4 proof-carrying composer. One CompatibilityProof per line, JSONL.",
            "application/jsonl",
        ),
        (
            "runtime/assumptions.jsonl",
            "Assumption ledger",
            "Per-assumption ledger entries from the v4 composer. One Assumption per line, JSONL. Tracks LLM-inferred defaults, lossy adapter assumptions, unresolved ontology mappings, policy exceptions.",
            "application/jsonl",
        ),
        (
            "runtime/validation-reports.jsonl",
            "Validation reports",
            "Per-task validation obligation outcomes from the harness validator runner. One ValidationReport per line, JSONL. Includes contract / golden / metamorphic / biological / statistical / reproducibility / security / resource-bound test results.",
            "application/jsonl",
        ),
        (
            "runtime/policy-decisions.jsonl",
            "Policy decisions",
            "Per-edge and per-node policy decisions recorded by the v4 planner's policy gate (Phase 9). Includes audit-trail / human-signoff / no-privacy-widening / clinical-bundle decisions.",
            "application/jsonl",
        ),
        (
            "runtime/task-nodes.json",
            "Task nodes (proof-carrying IR)",
            "Phase 14 — typed `TaskNode` list lowered from the v4 `WorkflowDag`. Consumed by the harness `pre_dispatch_check` so generated-code refusal can run at dispatch time alongside compose-time policy gating. Absent on v1/v2/v3 sessions and v4 sessions with no active policy bundle.",
            "application/json",
        ),
        (
            "runtime/sandbox-policy.json",
            "Sandbox policy",
            "Phase 14 — `SandboxPolicy` derived from the session's active policy bundle. Carries network/secret/host-fs/memory/timeout/signed-artifact/human-review constraints the harness enforces at dispatch time. Absent when no active_policy_bundle is set on the session.",
            "application/json",
        ),
        (
            "runtime/plot_affordances.jsonl",
            "Plot affordance resolutions",
            "Flexible-plotting Phase 5 — per-task PlotAffordance variant + AffordanceProof for every figure produced by this package. Non-Registered variants are marked provisional. Emitted by the backend emitter when `EmitContext::emit_affordances` is `Some(_)`; absent on default emits until the Phase 5.x resolution call is wired.",
            "application/jsonl",
        ),
        (
            "runtime/verifier-decisions.jsonl",
            "Verifier Decision Substrate",
            "v4 P2 / F18 — typed event log of every verifier decision made during composition: prove() attempts/succeeds/fails, planner alternative ranking, assumption-policy consults, promotion-gate consults, ontology-scope checks, and proposal rejections. One VerifierDecision per JSON line. Excluded from the byte-diff baseline.",
            "application/jsonl",
        ),
        (
            "runtime/backend-capability-report.json",
            "Backend capability report (F17)",
            "v3 P4 / F17 — `BackendCapabilityReport` recording the semantic constraints from the `WorkflowDag` IR that the backend emitter could not preserve in the emitted artifact, plus each loss's authorizing `ConstraintLossAck`. Absent for `WorkflowJsonEmitter` (custom harness consumes the full IR shape, so the report is empty); present the day an external emitter (CWL/WDL/Nextflow/...) lands and reports any constraint losses.",
            "application/json",
        ),
        // v3 P7 — schema-versions manifest. ALWAYS present at emit
        // (the writer in `audit_log::write_schema_versions_manifest`
        // is unconditional). Replay consumers diff this against
        // their own `SchemaVersionsManifest::current()` to detect
        // required migrations via `core::migration::replay_provenance`.
        // Conflict-marker for parallel-phase coordination: this row
        // sits in its own marked block adjacent to v4 P2 (substrate)
        // and v3 P4 (backend-capability-report); do not merge.
        (
            "runtime/schema-versions.json",
            "Schema versions manifest (v3 P7)",
            "v3 P7 — per-package manifest listing the SemVer of every IR type (`WorkflowIntent`, `Session`, `WorkflowDag`, `SessionLineage`, `DispatchRecord`) the package was emitted with. Replay consumers diff this against their own `SchemaVersionsManifest::current()` to detect which migrations they need to run (`core::migration::replay_provenance`).",
            "application/json",
        ),
        // Install log. Written one entry per
        // line by the install-proxy shims (`runtime/install-proxy/*`)
        // whenever an agent install request passes the
        // `provisioning.json` policy check. Absent when no runtime
        // installs occur (sealed atoms, declared_only without
        // declared packages, agents that never installed anything) —
        // the presence gate below skips its registration in that
        // case. Auditors trace which packages were pulled at task
        // time vs vendored at compile time via the `source` field
        // (`agent_runtime` for these entries).
        (
            "runtime/install-log.jsonl",
            "Runtime install log",
            "Per-package install log written by the install-proxy shims (apt/pip/conda/cran/npm/rubygems) every time an agent install request passes the per-task provisioning.json policy. Each line records timestamp, atom_id, package, registry, and source=agent_runtime so auditors can distinguish runtime installs from compile-time-vendored ones.",
            "application/jsonl",
        ),
        // ── Grant v19 §Authentication of Key Resources (D1-D4) ──────
        // Four runtime/*.json sidecars cited as live disclosure
        // surfaces. D1 is suppressed under ECAA_ABLATE_CLAIM_CONSISTENCY;
        // D2 is always written (its `ablation_engaged` field mirrors
        // ECAA_ABLATE_REEXECUTION_CLASS); D3 + D4 are always written.
        // Presence-gated like every other entry above.
        (
            "runtime/claim-verification.json",
            "Deterministic claim verification report",
            "Per-narrative-claim verdict (verified / unverified / contradicted) against extracted result tables. Suppressed under ECAA_ABLATE_CLAIM_CONSISTENCY (Arm B' control).",
            "application/json",
        ),
        (
            "runtime/determinism-shim.json",
            "Determinism shim — active env, seed, temp-path, locale, timezone",
            "Captures the deterministic environment in effect at package emit time. The ablation_engaged field records whether ECAA_ABLATE_REEXECUTION_CLASS was set (Arm B' control).",
            "application/json",
        ),
        (
            "runtime/security-policy.json",
            "Per-atom SafetyPolicy aggregate + container digests + scan summary",
            "Aggregates the SafetyPolicy 5-tuple (SafetyLevel x NetworkPolicy x CodeExecution x SandboxRequirement x ProvisioningPolicy) for every atom in the package, plus container image SHA-256 digests and any vulnerability-scan summary. Always written.",
            "application/json",
        ),
        (
            "runtime/model-policy.json",
            "LLM model pinning + system-prompt hash + tool-schema version",
            "Active Anthropic model, API version, SHA-256 of the assembled system prompt, tool-schema version, tool count (Tool::COUNT), and any escalation reason that selected the model. Mid-evaluation model-version changes therefore surface in the package diff.",
            "application/json",
        ),
        (
            "runtime/audit-proof-report.json",
            "Audit-proof invariant report (D8)",
            "Per-invariant verdicts (claim-completeness, decision-justification, evidence-coverage, equivalence-failure, cross-graph-integrity, substrate-validity). Warn-only at emission time; the verdicts surface in the UI Verifier tab. Suppressed under ECAA_ABLATE_AUDIT_PROOF (Arm B' control).",
            "application/json",
        ),
        // ECAA emit-time validation summary written by
        // `emit::validation::write_validation_summary` after the
        // pure-Rust + optional external validators run. Carries the
        // active validation mode, per-sidecar schema verdicts, skipped
        // harness-runtime sidecars count, and external-validator outcomes
        // when ECAA_VALIDATE_ON_EMIT=full. Always written when validation
        // is not `Disabled`.
        (
            "runtime/validation-summary.json",
            "ECAA emit-time validation summary",
            "Aggregated outcome of the emit-time validator: ValidationMode (off/schema_only/full), per-sidecar JSON Schema verdicts (passed / failed / skipped_pending_harness), and external-validator outcomes (SHACL projection, OWL consistency, runcrate validate) when ECAA_VALIDATE_ON_EMIT=full. Warn-only unless ECAA_VALIDATION_BLOCK_ON_FAIL=1.",
            "application/json",
        ),
    ];
    let mut linked_semantic_ids: Vec<String> = Vec::new();
    for (id, name, description, encoding_format) in semantic_sidecars {
        let exists = tokio::fs::try_exists(output_dir.join(id))
            .await
            .unwrap_or(false);
        if !exists {
            continue;
        }
        register_ro_crate_entity(graph, id, name, description, encoding_format);
        linked_semantic_ids.push((*id).to_string());
    }

    // Phase A3 (flexible-plotting resolver wiring) — stamp
    // `swfc:provisional` + `swfc:affordanceVariant` on every
    // `ImageObject` entity whose owning task resolved to a non-Registered
    // affordance. The owning task_id is parsed out of the figure's `@id`
    // path: `runtime/outputs/<task_id>/figures/<fig_id>.png`.
    //
    // Entities whose task has no affordance record (legacy atoms without
    // outputs or edam_data) are left unflagged — absence is treated as
    // legacy/Validated by convention. This avoids false provisional
    // flags on figures from well-established taxonomy stages whose
    // atoms haven't been migrated to the outputs-bearing atom format yet.
    if !affordance_by_task.is_empty() {
        for entity in graph.iter_mut() {
            // Target only ImageObject entities.
            let is_image_object = entity
                .get("@type")
                .and_then(|v| v.as_array())
                .is_some_and(|arr| arr.iter().any(|x| x.as_str() == Some("ImageObject")));
            if !is_image_object {
                continue;
            }
            // Extract task_id from the @id path.
            // Format: "runtime/outputs/<task_id>/figures/<fig_id>.png"
            let task_id = entity.get("@id").and_then(|v| v.as_str()).and_then(|id| {
                // Strip "runtime/outputs/" prefix and take the next segment.
                id.strip_prefix("runtime/outputs/")
                    .and_then(|rest| rest.split('/').next())
                    .map(str::to_string)
            });
            if let Some(task_id) = task_id {
                if let Some((provisional, variant_tag)) = affordance_by_task.get(&task_id) {
                    if *provisional {
                        if let Some(obj) = entity.as_object_mut() {
                            obj.insert(
                                "swfc:provisional".to_string(),
                                serde_json::Value::Bool(true),
                            );
                            obj.insert(
                                "swfc:affordanceVariant".to_string(),
                                serde_json::Value::String(variant_tag.to_string()),
                            );
                        }
                    }
                }
            }
        }
    }

    // register the cross-version diff files when
    // `write_cross_version_diff` produced them. Always idempotent.
    let diff_json_id = "runtime/cross-version-diff.json";
    let diff_written = !diff_tables.is_empty();
    // read the parent package path out of the diff JSON
    // so we can annotate the root Dataset with `schema:isBasedOn`
    // pointing at the parent package's results/tables/ directory. This
    // is complementary to `prov:wasDerivedFrom` (package root) already
    // written by core::emitter.
    let mut parent_tables_uri: Option<String> = None;
    if diff_written {
        register_ro_crate_entity(
            graph,
            diff_json_id,
            "Cross-version diff report",
            "Per-row robust/concordant/discordant classification comparing this package's results/tables/ to the parent package's. Generated at emit time when lineage.parent_emitted_package_path is set.",
            "application/json",
        );
        for csv_name in &diff_tables {
            let id = format!("runtime/{}", csv_name);
            let name = format!("Cross-version diff: {}", csv_name);
            register_ro_crate_entity(
                graph,
                &id,
                &name,
                "Per-row diff table for offline inspection. Columns: entity, classification, parent/child effect + raw/adj p-values, Pearson contribution.",
                "text/csv",
            );
        }
        // Pull `parent_package` out of the diff report to build the
        // `schema:isBasedOn` URI. Keep the signature minimal — no need
        // to thread lineage through the function. Best-effort: if the
        // JSON can't be read or parsed, just skip the annotation.
        let diff_path = output_dir.join("runtime/cross-version-diff.json");
        if let Ok(bytes) = tokio::fs::read(&diff_path).await {
            if let Ok(report) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if let Some(parent) = report.get("parent_package").and_then(|v| v.as_str()) {
                    let trimmed = parent.trim_end_matches('/');
                    parent_tables_uri = Some(format!("{}/results/tables/", trimmed));
                }
            }
        }
    }

    // Add a hasPart link from the root Dataset.
    if let Some(root) = graph
        .iter_mut()
        .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("./"))
    {
        if let Some(parts) = root.get_mut("hasPart").and_then(|v| v.as_array_mut()) {
            let exists = parts.iter().any(|p| {
                p.get("@id").and_then(|v| v.as_str()) == Some("runtime/intake-conversation.jsonl")
            });
            if !exists {
                parts.push(serde_json::json!({
                    "@id": "runtime/intake-conversation.jsonl"
                }));
            }
            let decisions_linked = parts
                .iter()
                .any(|p| p.get("@id").and_then(|v| v.as_str()) == Some(decisions_id));
            if !decisions_linked {
                parts.push(serde_json::json!({ "@id": decisions_id }));
            }
            // Link any semantic
            // sidecars that were registered above (incl. plot_affordances.jsonl).
            for sid in &linked_semantic_ids {
                let already = parts
                    .iter()
                    .any(|p| p.get("@id").and_then(|v| v.as_str()) == Some(sid.as_str()));
                if !already {
                    parts.push(serde_json::json!({ "@id": sid }));
                }
            }
            if diff_written {
                let diff_linked = parts
                    .iter()
                    .any(|p| p.get("@id").and_then(|v| v.as_str()) == Some(diff_json_id));
                if !diff_linked {
                    parts.push(serde_json::json!({ "@id": diff_json_id }));
                }
                for csv_name in &diff_tables {
                    let id = format!("runtime/{}", csv_name);
                    let linked = parts
                        .iter()
                        .any(|p| p.get("@id").and_then(|v| v.as_str()) == Some(id.as_str()));
                    if !linked {
                        parts.push(serde_json::json!({ "@id": id }));
                    }
                }
            }
        }

        // annotate the root Dataset with
        // `schema:isBasedOn` pointing at the parent package's
        // results/tables/ directory. Idempotent: skip if the same URI
        // is already present; if an existing value has a different
        // @id, keep it and append ours as an array entry.
        if let Some(uri) = parent_tables_uri.as_deref() {
            let new_entry = serde_json::json!({ "@id": uri });
            match root.get("schema:isBasedOn").cloned() {
                None => {
                    root.as_object_mut()
                        .expect("root Dataset is a JSON object")
                        .insert("schema:isBasedOn".to_string(), new_entry);
                }
                Some(serde_json::Value::Object(map)) => {
                    let existing_id = map.get("@id").and_then(|v| v.as_str());
                    if existing_id == Some(uri) {
                        // Already present with the same URI — no-op.
                    } else {
                        // Different existing value — preserve it and
                        // append ours as an array so we don't overwrite.
                        let array = serde_json::Value::Array(vec![
                            serde_json::Value::Object(map),
                            new_entry,
                        ]);
                        root.as_object_mut()
                            .expect("root Dataset is a JSON object")
                            .insert("schema:isBasedOn".to_string(), array);
                    }
                }
                Some(serde_json::Value::Array(mut arr)) => {
                    let exists = arr
                        .iter()
                        .any(|v| v.get("@id").and_then(|s| s.as_str()) == Some(uri));
                    if !exists {
                        arr.push(new_entry);
                        root.as_object_mut()
                            .expect("root Dataset is a JSON object")
                            .insert(
                                "schema:isBasedOn".to_string(),
                                serde_json::Value::Array(arr),
                            );
                    }
                }
                Some(_) => {
                    // Unexpected shape — don't overwrite; log and skip.
                    eprintln!(
                        "warn: ro-crate-metadata.json root Dataset has a non-object/array schema:isBasedOn value; skipping cross-version-diff annotation"
                    );
                }
            }
        }
    }

    // ── Spec §7.3 / §7.4 — literature-atom evidence registration ─────
    //
    // Walk `runtime/outputs/*/evidence/manifest.json`. For each entry,
    // register a `CreativeWork`/`ScholarlyArticle` node. CSVs get
    // `prov:wasDerivedFrom` edges. Errors are non-fatal (missing
    // runtime/outputs is normal for packages without literature atoms).
    if let Err(e) = register_literature_evidence(graph, output_dir, false) {
        tracing::warn!(
            "literature evidence registration skipped: {} (non-fatal)",
            e
        );
    }

    // ── v3 P5 F16 — PHI-scope leak detection ──────────────────────
    //
    // After every JSONL sidecar is registered + linked, scan each one
    // for PHI patterns under the requested redaction tier. A non-empty
    // leak set at any non-`Private` tier is a hard refusal: the
    // ro-crate-metadata.json write is aborted so the package never
    // ships PHI under the wrong privacy class. Deliberately placed
    // here so it sits at a CLEAR, distinct location separate from v4
    // P2's substrate registration, v3 P4's capability-report row, and
    // v3 P7's schema-versions row — see neighboring `// v3 P7` /
    // `// v4 P2` markers earlier in this function.
    //
    // The scan is presence-gated: only existing sidecars are read.
    // `Private` short-circuits in `detect_phi_leak`, so this loop has
    // zero cost for the default tier.
    let scanned_sidecars: &[&str] = &[
        "runtime/intake-conversation.jsonl",
        "runtime/decisions.jsonl",
        "runtime/proofs.jsonl",
        "runtime/assumptions.jsonl",
        "runtime/validation-reports.jsonl",
        "runtime/policy-decisions.jsonl",
        "runtime/plot_affordances.jsonl",
        "runtime/verifier-decisions.jsonl",
        // Install-log entries are
        // (atom_id, package, registry, timestamp, source) records
        // with no SME free-text; PHI patterns shouldn't appear here
        // by construction. The scan is defensive against a future
        // shim that logs more verbosely. `detect_phi_leak`
        // short-circuits on `Private` so this is free for the
        // default tier.
        "runtime/install-log.jsonl",
    ];
    for sid in scanned_sidecars {
        let file_path = output_dir.join(sid);
        let exists = tokio::fs::try_exists(&file_path).await.unwrap_or(false);
        if !exists {
            continue;
        }
        let bytes = tokio::fs::read(&file_path)
            .await
            .with_context(|| format!("v3 P5 F16: reading {}", file_path.display()))?;
        let text = String::from_utf8_lossy(&bytes);
        let leaks = detect_phi_leak(&text, target_tier);
        if !leaks.is_empty() {
            // Build a compact human-readable summary; the structured
            // findings remain in `leaks` for future telemetry.
            let mut summary = format!(
                "v3 P5 F16: PHI-scope leak detected in {} under tier {:?} ({} finding{}):",
                sid,
                target_tier,
                leaks.len(),
                if leaks.len() == 1 { "" } else { "s" },
            );
            for leak in leaks.iter().take(5) {
                summary.push_str(&format!(
                    "\n  line {} field {} pattern {}",
                    leak.line, leak.field, leak.pattern_name
                ));
            }
            if leaks.len() > 5 {
                summary.push_str(&format!("\n  … {} more leaks suppressed", leaks.len() - 5));
            }
            bail!(summary);
        }
    }

    let new_bytes = serde_json::to_vec_pretty(&metadata)?;
    tokio::fs::write(&path, new_bytes).await?;
    Ok(())
}
