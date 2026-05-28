use crate::classify::ClassificationResult;
use crate::dag::DAG;
use crate::ids::TaskId;
use crate::intake_facts::IntakeFacts;
use crate::ro_crate;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::instrument;

/// Executor-focused brief written into every emitted package. The
/// brief is targeted at the execution agent, not the contributor-
/// oriented host CLAUDE.md (39 KB).
const AGENT_EXECUTOR_BRIEF: &str = include_str!("../../templates/AGENT-EXECUTOR.md");

mod amendment;
mod bagit;
mod copy_libs;
mod ecaa;
mod policies;
use amendment::{
    emit_amendment_lineage_policy, patch_ro_crate_with_amendment, patch_ro_crate_with_branch,
    read_parent_link,
};
use bagit::write_bagit_manifest;
use copy_libs::{copy_plotting_library, copy_r_plotting_library};
use policies::{
    emit_compute_profile_policy, emit_container_spec, emit_gpu_capability_policy,
    emit_intake_facts, emit_memory_discipline_policy, emit_per_atom_runtime_prereqs,
    emit_runtime_prereqs, write_policy,
};

/// EmitConfig data.
pub struct EmitConfig<'a> {
    /// Output dir.
    pub output_dir: &'a Path,
    /// Dag.
    pub dag: &'a DAG,
    /// Classification.
    pub classification: &'a ClassificationResult,
    /// Policies dir.
    pub policies_dir: &'a Path,
    /// Policy allowlist.
    pub policy_allowlist: Option<&'a [String]>,
    /// Filename (within `policies_dir`) of a validation
    /// contract JSON to copy into the emitted `policies/` alongside the
    /// regular downstream policies. Agent-generated validators read it
    /// and enforce its required assertions; the harness cross-checks
    /// the validator's report against the contract to re-block on any
    /// Miss. `None` keeps legacy behavior (no contract).
    pub validation_contract_ref: Option<&'a str>,
    /// Optional free-text claim boundary from the taxonomy — surfaced verbatim
    /// into PROMPT.md so the agent sees it as a standing rule.
    pub claim_boundary: Option<&'a str>,
    /// Root of the compute-profiles config (defaults to
    /// `<repo>/config/compute-profiles`). When present and the profiles.yaml
    /// exists, the emitter writes `policies/compute-resource-policy.json`.
    pub compute_profiles_dir: Option<&'a Path>,
    /// Session-level facts from classification + structured capture.
    /// When provided, the emitter writes `policies/intake-facts.json`
    /// for the harness sizing layer.
    pub intake_facts: Option<&'a IntakeFacts>,
    /// Path to the parent package when this emission is an amendment.
    /// Causes the emitter to write `policies/amendment-lineage.json`,
    /// add `prov:wasDerivedFrom` on the emitted root Dataset, and
    /// attach an `UpdateAction` entity to the RO-Crate graph.
    /// Soft-fails (emits without lineage) when the parent metadata is
    /// missing.
    pub amend_from: Option<&'a Path>,
    /// Paired with `amend_from`. Carries the stage whose method was
    /// swapped, the downstream task ids the DAG slice invalidator
    /// produced, and an optional user-supplied reason string.
    pub amend_context: Option<&'a AmendContext>,
    /// Container image tag from the taxonomy's `preferred_container`
    /// field. When set, the emitter writes `policies/container.json`
    /// with `{ "image": "<tag>" }` so the agent scripts can branch on
    /// it at invocation time. `None` writes `{ "image": null }`
    /// (host-env dispatch).
    pub preferred_container: Option<&'a str>,
    /// Derived-image warm-up: aggregated runtime prereqs the harness
    /// pre-flight reads to derive a content-addressed image before
    /// iteration 1. The caller (CLI intake / composer)
    /// computes the union via
    /// `crate::runtime_prereqs::aggregate_*` and passes it here. The
    /// emitter ALWAYS writes `policies/runtime-prereqs.json` — `None`
    /// (or an empty manifest) writes a v1 schema sentinel that the
    /// harness reads as "skip derived-image build". Always emitting
    /// keeps downstream consumers + the BagIt manifest stable across
    /// packages.
    pub runtime_prereqs: Option<&'a crate::runtime_prereqs::RuntimePrereqs>,
    /// Per-atom runtime prereqs the harness pre-flight reads to
    /// build per-atom isolated derived
    /// images when `ECAA_PER_TASK_IMAGES=1`. Each entry maps
    /// `atom_id -> RuntimePrereqs` and is written to
    /// `policies/atom-prereqs/<atom_id>.json` for the buildable
    /// entries only — unbuildable manifests (empty deltas) are skipped
    /// so the package stays lean. `None` (default) writes no
    /// per-atom files, which preserves the byte-baseline for callers
    /// that haven't opted in (back-compat with legacy CLI / chat /
    /// test fixtures).
    pub per_atom_runtime_prereqs:
        Option<&'a std::collections::BTreeMap<String, crate::runtime_prereqs::RuntimePrereqs>>,
}

/// Structured amendment metadata captured at the moment `emit_package`
/// is re-invoked after `amend_stage_method`. Flows through the
/// session's `SessionState::Amending` fields plus an optional reason.
/// Written into `policies/amendment-lineage.json` and surfaced in the
/// RO-Crate `UpdateAction` entity so downstream tooling can walk the
/// amendment chain and explain *why* this package diverged from its
/// parent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct AmendContext {
    /// Optional free-text reason — typically the SME's one-line
    /// justification for the method swap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Stage whose method was swapped. Matches a DAG task id.
    pub amended_stage: String,
    /// Downstream task ids the invalidator marked Pending.
    pub invalidated_tasks: Vec<String>,
}

/// Walk every task in
/// `dag` that carries a `ContainerSpec` and refuse emission if any
/// container has an empty / all-zero-sentinel digest. The harness's
/// reproducibility contract demands a sha256 digest pin on every
/// container reference; unpinned images mean tag drift silently
/// changes the analysis.
///
/// Surface is `Result<(), EmitError>` so callers can promote the
/// typed error onto a `BlockerKind` or wrap it through anyhow.
///
/// `ECAA_CONTAINER_VERIFY=1` (already documented elsewhere) is the
/// opt-in handle for re-resolving the digest against the upstream
/// registry; today that's handled inside `verify-reproducibility`.
/// The empty-digest guard here fires unconditionally because an
/// unpinned digest is always a defect regardless of the verifier
/// setting.
pub fn validate_container_digests_pinned(
    dag: &crate::dag::DAG,
) -> std::result::Result<(), crate::backend_emitters::EmitError> {
    /// The all-zeros sentinel some upstream resolvers return when a
    /// registry roundtrip fails. Treated the same as an empty string.
    const ZERO_DIGEST: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";
    for (task_id, task) in &dag.tasks {
        if let Some(c) = &task.container {
            let d = c.digest.trim();
            if d.is_empty() || d == ZERO_DIGEST {
                return Err(crate::backend_emitters::EmitError::ImageDigestUnresolved {
                    task: task_id.to_string(),
                    image: c.image.clone(),
                    tag: c.tag.clone(),
                });
            }
        }
    }
    Ok(())
}

#[instrument(
    skip(config),
    fields(
        output_dir = %config.output_dir.display(),
        task_count = config.dag.tasks.len()
    )
)]
/// Emit package.
pub fn emit_package(config: &EmitConfig) -> Result<()> {
    // Fail closed before any IO if a task's
    // container reference isn't digest-pinned. `anyhow!` wraps the
    // typed `EmitError::ImageDigestUnresolved` Display message so the
    // CLI / chat surface sees an actionable diagnostic that names
    // the offending task + image:tag.
    if let Err(e) = validate_container_digests_pinned(config.dag) {
        return Err(anyhow!("{}", e));
    }

    // Derive a deterministic package_run_id from a SHA-256 of the intake
    // tuple (workflow_id, modality, edam_topic, edam_operation, lineage
    // parent path). This keeps the id stable across two recompositions of
    // the same intake, satisfying the tier-8-3 compose-twice byte-
    // reproducibility gate, while still uniquely distinguishing every
    // distinct (workflow, classification) pair — the same guarantee the
    // grant's N7 claim requires.
    //
    // A random UUID (new_v4) was used before this change. It produced a
    // different id on every emit, so two back-to-back `intake` calls for
    // the same prose differed in WORKFLOW.json::meta.run_id and
    // ro-crate-metadata.json::additionalProperty[package_run_id], causing
    // the tier-8-3 byte-diff check to always exceed its 100-byte ceiling.
    //
    // Implementation: the 64-hex SHA-256 is formatted as a UUID string by
    // taking the first 32 hex characters (16 bytes) and grouping them as
    // 8-4-4-4-12. This is not a standards-compliant UUID v5 (which uses a
    // SHA-1 namespace hash), but it is collision-resistant for our purposes
    // and keeps the field's string shape unchanged from the consumer's POV.
    let lineage_str = config
        .amend_from
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let hash_input = format!(
        "{}|{}|{}|{}|{}",
        config.dag.workflow_id,
        config.classification.modality,
        config.classification.edam_topic,
        config.classification.edam_operation,
        lineage_str,
    );
    let digest = crate::hash_utils::sha256_hex(hash_input.as_bytes());
    // Format the first 32 hex chars (128 bits) as a UUID string.
    let package_run_id = format!(
        "{}-{}-{}-{}-{}",
        &digest[..8],
        &digest[8..12],
        &digest[12..16],
        &digest[16..20],
        &digest[20..32],
    );

    let dir = config.output_dir;
    std::fs::create_dir_all(dir.join("runtime/outputs")).context("creating runtime/outputs")?;

    // Token-reduction tactic #1: emit runtime/outputs/<task_id>/task-spec.json
    // for every task so the executor agent can read a 1-3 KB focused slice
    // instead of the full 40+ KB WORKFLOW.json on its first turn.
    for (task_id, task) in &config.dag.tasks {
        let task_output_dir = dir.join("runtime/outputs").join(task_id.as_str());
        std::fs::create_dir_all(&task_output_dir)
            .with_context(|| format!("creating task output dir {}", task_id))?;
        let task_spec = serde_json::json!({
            "task_id": task_id,
            "kind": task.kind,
            "state": task.state,
            "depends_on": task.depends_on,
            "assignee": task.assignee,
            "description": task.description,
            "resource_class": task.resource_class,
            "safety": task.safety,
            "container": task.container,
            "source_atom_id": task.source_atom_id,
            "spec": task.spec,
        });
        let spec_bytes =
            serde_json::to_vec_pretty(&task_spec).context("serializing task-spec.json")?;
        std::fs::write(task_output_dir.join("task-spec.json"), &spec_bytes)
            .with_context(|| format!("writing task-spec.json for {}", task_id))?;
    }

    // Pre-create LOG.jsonl (empty) so it always exists before the agent runs
    let log_path = dir.join("runtime/LOG.jsonl");
    if !log_path.exists() {
        std::fs::write(&log_path, "").context("creating runtime/LOG.jsonl")?;
    }

    // Serialize DAG with the stable run_id injected into a `meta` top-level
    // key. The `DAG` struct carries `run_id` directly (for back-compat
    // deserialization), so we patch a temporary JSON value to also write
    // `meta.run_id` so harness consumers that haven't adopted the new field
    // yet can still discover the id via the conventional meta block.
    // `meta.modality_stratum` is injected from the embedded strata registry
    // so the scorer and eval harness can partition results by analysis family
    // without re-loading the modality manifest.
    let modality_stratum = crate::strata::modality_stratum(&config.classification.modality);
    let mut dag_json = serde_json::to_value(config.dag).context("serializing DAG to JSON value")?;
    if let Some(obj) = dag_json.as_object_mut() {
        obj.insert(
            "run_id".to_string(),
            serde_json::Value::String(package_run_id.clone()),
        );
        let meta = obj.entry("meta").or_insert(serde_json::json!({}));
        if let Some(meta_obj) = meta.as_object_mut() {
            meta_obj.insert(
                "run_id".to_string(),
                serde_json::Value::String(package_run_id.clone()),
            );
            if let Some(stratum) = &modality_stratum {
                meta_obj.insert(
                    "modality_stratum".to_string(),
                    serde_json::Value::String(stratum.clone()),
                );
            }
            // Also write the fine-grained modality id so eval harnesses
            // (e.g. testdata/paper-recreation-blinded/run_corpus.py)
            // can verify the emitted package matches the expected
            // modality without parsing the package-directory name or
            // re-reading CONTEXT.md. Without this, corpus scenarios
            // see only the high-level stratum ('epigenomics',
            // 'transcriptomics') and flag a baseline modality_stratum
            // drift warning.
            meta_obj.insert(
                "modality_id".to_string(),
                serde_json::Value::String(config.classification.modality.clone()),
            );
        }
    }
    // Atomic-write the three top-level package surfaces. WORKFLOW.json
    // is the DAG-truth file the harness mutates via patch-merge — a
    // torn write here would leave a session unrecoverable. CONTEXT.md
    // and PROMPT.md feed the agent prompt context on every dispatch.
    let workflow_payload = serde_json::to_string_pretty(&dag_json).context("serializing DAG")?;
    crate::fs_helpers::atomic_write_bytes_sync(
        &dir.join("WORKFLOW.json"),
        workflow_payload.as_bytes(),
    )
    .context("writing WORKFLOW.json")?;

    let context_payload = render_context(config.dag, config.classification);
    crate::fs_helpers::atomic_write_bytes_sync(&dir.join("CONTEXT.md"), context_payload.as_bytes())
        .context("writing CONTEXT.md")?;

    let prompt_payload = render_prompt(config.dag, config.classification, config.claim_boundary);
    crate::fs_helpers::atomic_write_bytes_sync(&dir.join("PROMPT.md"), prompt_payload.as_bytes())
        .context("writing PROMPT.md")?;

    // Token-reduction tactic #3: ship an executor-focused brief into every
    // package; the executor agent reads this instead of the contributor-
    // oriented CLAUDE.md on the host.
    crate::fs_helpers::atomic_write_bytes_sync(
        &dir.join("AGENT-EXECUTOR.md"),
        AGENT_EXECUTOR_BRIEF.as_bytes(),
    )
    .context("writing AGENT-EXECUTOR.md")?;

    copy_policies(config.policies_dir, dir, config.policy_allowlist).context("copying policies")?;
    if let Some(contract_name) = config.validation_contract_ref {
        copy_validation_contract(config.policies_dir, dir, contract_name)
            .context("copying validation contract")?;
    }
    copy_plotting_library(dir).context("copying plotting library")?;
    copy_r_plotting_library(dir).context("copying R plotting library")?;
    emit_compute_profile_policy(dir, config.compute_profiles_dir)
        .context("emitting compute-resource-policy")?;
    emit_gpu_capability_policy(dir, config.compute_profiles_dir)
        .context("emitting gpu-capability-policy")?;
    if let Some(facts) = config.intake_facts {
        emit_intake_facts(dir, facts).context("emitting intake-facts")?;
    }

    // Always emit policies/container.json. A null image preserves the
    // host-env path for bio sessions that don't set preferred_container.
    emit_container_spec(dir, config.preferred_container).context("emitting container spec")?;

    // Derived-image warm-up: always emit
    // policies/runtime-prereqs.json. Empty-but-valid manifest when the
    // caller didn't aggregate one (legacy callers, taxonomies with no
    // declared baseline). The harness pre-flight reads is_buildable()
    // and short-circuits on empty.
    emit_runtime_prereqs(dir, config.runtime_prereqs)
        .context("emitting runtime-prereqs manifest")?;

    // Emit one policies/atom-prereqs/<atom_id>.json per buildable
    // atom so the
    // harness pre-flight can build per-atom isolated derived images
    // when ECAA_PER_TASK_IMAGES=1. Additive: when the caller passes
    // `None` no files are written, which preserves the byte-baseline
    // for callers that haven't opted in.
    if let Some(map) = config.per_atom_runtime_prereqs {
        emit_per_atom_runtime_prereqs(dir, map, config.runtime_prereqs)
            .context("emitting per-atom runtime-prereqs")?;
    }

    // Always emit policies/memory-discipline.json. Carries thresholds +
    // on-disk library hints the agent consults before materializing a
    // large dense matrix (scRNA-seq normalization, cohort integration,
    // bulk variant matrices). Lives behind an explicit policy file so
    // taxonomies can eventually override the thresholds per-stage; the
    // host-wide memory cap (ECAA_AGENT_MEMORY_CAP_GB in the agent
    // scripts) is the defensive backstop when the agent ignores the
    // policy (e.g. normalization stages running R with unbounded RSS).
    emit_memory_discipline_policy(dir).context("emitting memory-discipline policy")?;

    // When this emit derives from a prior package, extract the
    // parent's identifiers before the RO-Crate is rendered so we can
    // patch the graph in one pass. The helper soft-fails and logs
    // rather than aborting — a missing parent metadata file must never
    // block the amendment emission itself. Both branches AND amendments
    // flow through here; the patch dispatch below picks the right
    // helper (amendments add a full UpdateAction, branches add only
    // wasDerivedFrom + parent dataset entry).
    let parent_link = config.amend_from.and_then(read_parent_link);
    // `FrozenClock` derived from the intake hash so every BagIt-manifest
    // artifact (amendment-lineage.json, ro-crate-metadata.json::dateCreated)
    // is byte-identical across re-emissions of the same intake.
    let emit_clock = frozen_clock_from_intake(&config.classification.intake_text);

    if let Some(ctx) = config.amend_context {
        emit_amendment_lineage_policy(dir, ctx, parent_link.as_ref(), &emit_clock)
            .context("emitting amendment-lineage policy")?;
    }

    let mut ro_crate_meta =
        ro_crate::build_metadata(config.dag, config.classification, &emit_clock);
    match (config.amend_context, &parent_link) {
        (Some(ctx), Some(link)) => {
            patch_ro_crate_with_amendment(&mut ro_crate_meta, ctx, link);
        }
        (None, Some(link)) => {
            // Branch lineage: parent link without amend context.
            // Records `prov:wasDerivedFrom` so the provenance graph
            // captures the branch edge, but no UpdateAction (a branch
            // is not a method amendment).
            patch_ro_crate_with_branch(&mut ro_crate_meta, link);
        }
        _ => {}
    }
    let ro_crate_payload =
        serde_json::to_string_pretty(&ro_crate_meta).context("serializing RO-Crate metadata")?;
    crate::fs_helpers::atomic_write_bytes_sync(
        &dir.join("ro-crate-metadata.json"),
        ro_crate_payload.as_bytes(),
    )
    .context("writing ro-crate-metadata.json")?;

    // Defense-in-depth scrub of any
    // existing `runtime/outputs/**/agent-trace.log` for known
    // secret patterns. On a fresh package this is a no-op (the
    // outputs subtree is empty at the moment emit_package runs).
    // On amendment / re-emit into the same `dir` (where prior agent
    // runs already populated `runtime/outputs/`), this catches trace
    // logs written by older builds that pre-dated the xtrace
    // suppression in `scripts/agent-claude*.sh`. Soft-fail: scrub
    // errors are surfaced via `.context(...)` but a missing
    // `runtime/outputs` returns Ok(0) cleanly.
    crate::provenance_tiers::scrub_agent_trace_logs(dir)
        .context("scrubbing agent-trace.log files for secrets")?;

    // BagIt 1.0-style manifest. Walks every file committed to the
    // package's deterministic surface and writes <sha512>
    // <relative-path> per line. ECAA runtime sidecars are intentionally
    // written after this call: the conversation emit path may overwrite
    // them with richer session logs after core emit_package returns, so
    // hashing them here would create a stale manifest on live emits.
    //
    // Contract: every reproducibility-bearing file in the emitted
    // package is captured in manifest-sha512.txt; consumers
    // (verify-reproducibility, downstream FAIR consumers) can compare
    // the manifest to detect drift without re-hashing every file.
    write_bagit_manifest(dir, &emit_clock).context("writing BagIt manifest")?;

    ecaa::write_emit_time_sidecars(dir, config.dag, config.classification, &emit_clock)
        .context("emitting ECAA sidecars")?;
    ecaa::write_audit_proof_report(dir).context("emitting ECAA audit-proof report")?;
    ecaa::write_validation_summary(dir).context("emitting ECAA validation summary")?;

    Ok(())
}

/// Derive a deterministic `FrozenClock` from the session's intake text
/// so any emit-pipeline artifact that lands in the BagIt manifest
/// (today: `amendment-lineage.json::created_at`; planned:
/// `ro-crate-metadata.json::dateCreated` and the ChainOfCustody
/// timestamps on emitted edge proofs) gets a timestamp identical
/// across two emits of the same intake. The hash is folded through
/// `deterministic_emit_time`, which yields a 2026..2076 RFC-3339
/// value that's schema-indistinguishable from a wall-clock timestamp.
fn frozen_clock_from_intake(intake_text: &str) -> crate::clock::FrozenClock {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(intake_text.as_bytes());
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&digest);
    crate::clock::FrozenClock {
        at: crate::clock::deterministic_emit_time(&hash),
    }
}

/// Copy the taxonomy's validation contract into the emitted
/// `policies/validation-contract.json`. Keeps the filename predictable
/// so agent-generated validators + the harness always look at the same
/// path regardless of which taxonomy-specific contract was chosen.
/// The contract is validated against its sidecar `.schema.json` before
/// being copied.
fn copy_validation_contract(src_dir: &Path, package_dir: &Path, filename: &str) -> Result<()> {
    let src = src_dir.join(filename);
    if !src.exists() {
        return Ok(());
    }
    crate::policy_schema::load_and_validate(&src)
        .with_context(|| format!("validating validation contract '{}'", filename))?;
    let dest_dir = package_dir.join("policies");
    std::fs::create_dir_all(&dest_dir).context("creating policies dir")?;
    // Predictable name so validators + harness key off a single path.
    let dest = dest_dir.join("validation-contract.json");
    std::fs::copy(&src, &dest)
        .with_context(|| format!("copying validation contract {}", src.display()))?;
    Ok(())
}

fn copy_policies(src_dir: &Path, package_dir: &Path, allowlist: Option<&[String]>) -> Result<()> {
    let dest = package_dir.join("policies");
    std::fs::create_dir_all(&dest).context("creating policies dir")?;

    if !src_dir.exists() {
        return Ok(());
    }

    // Validate every policy that's about to be copied. Malformed JSON, a
    // missing required top-level key, or drift from the sidecar schema
    // surfaces as an emission-time error rather than a runtime agent
    // crash. Validation is gated on sidecar presence — policies without
    // A.schema.json sidecar pass through unchanged (transitional-rollout
    // contract; see crate::policy_schema::load_and_validate).
    match allowlist {
        Some(files) => {
            for filename in files {
                let src = src_dir.join(filename);
                if src.exists() {
                    crate::policy_schema::load_and_validate(&src)
                        .with_context(|| format!("validating policy '{}'", filename))?;
                    std::fs::copy(&src, dest.join(filename))
                        .with_context(|| format!("copying policy '{}'", filename))?;
                }
            }
        }
        None => {
            for entry in std::fs::read_dir(src_dir).context("reading policies dir")? {
                let entry = entry.context("reading policies dir entry")?;
                let path = entry.path();
                let name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n,
                    None => continue,
                };
                // Skip schema sidecars and `_`-prefixed support files
                // (shared vocab, skeleton schema) — they are loader
                // inputs, not policies the runtime agent consumes.
                if !name.ends_with(".json")
                    || name.ends_with(".schema.json")
                    || name.starts_with('_')
                {
                    continue;
                }
                crate::policy_schema::load_and_validate(&path)
                    .with_context(|| format!("validating policy '{}'", name))?;
                std::fs::copy(&path, dest.join(name))
                    .with_context(|| format!("copying policy '{}'", name))?;
            }
        }
    }
    Ok(())
}

/// Render prompt.
pub fn render_prompt(
    dag: &DAG,
    classification: &ClassificationResult,
    claim_boundary: Option<&str>,
) -> String {
    let (completed, ready, blocked, pending) = dag.progress();
    let mut out = format!(
        "# Agent Instructions\n\n\
You are executing one harness-dispatched task from a {domain} workflow defined in WORKFLOW.json.\n\
**Workflow:** {description}\n\n\
## Dispatch Contract\n\
1. Read `ECAA_TASK_ID`. That is the only task you may execute in this invocation.\n\
2. Read WORKFLOW.json only to inspect that task's spec and completed dependency outputs.\n\
3. Write outputs only under `runtime/outputs/$ECAA_TASK_ID/`.\n\
4. Write the state transition only to `runtime/outputs/$ECAA_TASK_ID/state.patch.json`.\n\
5. Include top-level `harness_run_id` and `dispatch_epoch` values copied from `ECAA_HARNESS_RUN_ID` and `ECAA_DISPATCH_EPOCH` in that patch.\n\
6. Do not edit WORKFLOW.json. The harness is the only writer of task state.\n\
7. Do not execute any other ready task. The harness will invoke a new agent for the next dispatch.\n\
8. Append a JSON line to runtime/LOG.jsonl for audit context only.\n\n\
## Current state\n\
- Completed: {completed}\n\
- Ready: {ready}\n\
- Blocked: {blocked}\n\
- Pending: {pending}\n\n\
## Rules\n\
- Execute only the task named by `ECAA_TASK_ID`\n\
- Never skip a task's dependencies\n\
- Never mark, patch, or edit any task other than `ECAA_TASK_ID`\n\
- For discovery tasks, consult the policy file referenced in the task spec\n\
- For blocked tasks, write a clear reason and what you tried\n\
- All decisions go in runtime/LOG.jsonl as one JSON object per line\n\
- DO NOT WRITE TO runtime/decisions.jsonl. That file is owned by the\n\
  conversation/server layer and holds only the typed DecisionRecord\n\
  taxonomy (kinds: confirm | reject | unblock | branch | emit_package |\n\
  amend_stage | rerun_task | select_sensitivity_winner |\n\
  cross_version_diff | post_hoc_deviation | auto_advanced |\n\
  applied_structured_decision | disposition_proposed |\n\
  disposition_applied | disposition_rejected | undone_amendment |\n\
  budget_changed | user_note). Free-form audit entries with `kind` at\n\
  the top level break the typed schema and are filtered out by the\n\
  server's /decisions endpoint. Use runtime/LOG.jsonl for your own\n\
  audit and stage-decision artifacts (decision.json + blocker.json +\n\
  sme-selection.json under runtime/outputs/<task_id>/) for the\n\
  per-stage records.\n\n\
## Discovery procedure: env capability + spec-preferred methods (Phases 2 + 3)\n\
\n\
Before composite-scoring any `discover_*` candidate pool:\n\
\n\
1. **Read `runtime/env_capability.json`** (harness-written at startup). Two sections both inform candidate scoring:\n\
   - `capabilities` carries six coarse-grained signals — `r_seurat`, `r_cellchat`, `pyscenic`, `python_lisi`, `cellranger_version`, `rna_velocity_capable` — for the historical spec-preference flags.\n\
   - `methods` carries per-method availability for every common candidate (DE: deseq2 / edger / limma_voom / mast / dexseq / drimseq; normalisation: scran / sctransform / deseq2_vst / edger_tmm / seurat_lognormalize; pathway: fgsea / clusterprofiler / gsea / enrichr; clustering: leiden / louvain / umap / phate; integration: harmony / bbknn / scvi / mnn_correct / combat; multi-omics: mofa2 / mofa_plus / mixomics_diablo; cell-type: celltypist / singler / sctype / azimuth; peaks: macs2 / chipseeker / diffbind / csaw; spatial: bayesspace / banksy / squidpy_neighbors; coloc: coloc / susie_coloc / hyprcoloc). Each entry is `{{ available: bool, language: \"python\"|\"r\", probe_target: <import-name> }}`.\n\
   For each candidate method, check `methods.<id>.available` (or the legacy `capabilities` flag when the method isn't in the `methods` map yet). Candidates whose required capability is unavailable get tagged `{{ env_capability_skip: true, missing: [cap...] }}` in `decision.json::candidate_pool_full` and are DOWN-RANKED (not excluded — the install-at-task-start path may still install them). This signals which methods will need a `pip` / `BiocManager` / `conda` install before they can run, so the discover step can prefer in-image methods when scoring is close.\n\
\n\
2. **Apply `task.spec.spec_preferred_methods` boosts.** When the stage's task spec carries a non-empty `spec_preferred_methods: {{method_id: rationale}}` map, apply a `+0.30` boost on the `spec_match` composite axis to every candidate whose `method_id` is a key in that map. Record the boost in `decision.json::candidate_pool_full[i].spec_match_applied` + cite the rationale. Spec-preferred candidates that are env-available MUST outrank non-spec candidates of otherwise equal score. Set `decision.json::spec_preference_applied = true` when the final pick was re-ranked by the boost.\n\
\n\
3. **specMatch renormalization when spec_preferred_methods is empty.** If `task.spec.spec_preferred_methods` is absent or empty, the `specMatch` axis has no input source and its 0.30 weight budget would otherwise be wasted. The policy's `compositeScoreWeights.renormalizeWhenAxisMissing` field lists axes that trigger weight redistribution in this case. For each axis in that list whose input source is absent: redistribute its weight proportionally among the remaining axes so the effective weights still sum to 1.0. The formula for the 4-axis case (specMatch missing): `eff_weight_i = policy_weight_i / (sum of remaining policy weights)`. Apply effective weights, compute composite, and record `decision.json::spec_match_renormalized = true` + `spec_match_effective_weights = {{defaultSuitability: <v>, robustness: <v>, adoption: <v>, operationalFit: <v>}}` in the decision JSON so the audit trail documents the redistribution. **Do NOT apply renormalization when `spec_preferred_methods` is non-empty** — in that case the specMatch axis IS scored (some candidates get the boost; others score 0.0), and the weight stays at 0.30.\n\
\n\
2b. **Apply `qualityGatePenalties` and check default eligibility.** After computing the composite score (steps 1–3 above), apply gate penalties and evaluate eligibility before assigning tiers:\n\
\n\
    2b-1. For each candidate, count blocking quality-gate failures and non-blocking quality-gate failures (read from `qualityGateResults` in the candidate metadata, or from any gate evaluation your discovery step performed). Subtract `policy.qualityGatePenalties.blocking` (−1.0) per blocking failure and `policy.qualityGatePenalties.nonBlocking` (−0.25) per non-blocking failure from the composite score. Penalties stack additively. If `qualityGatePenalties` is absent from the policy, skip this sub-step (backward-compat). Record `decision.json::candidate_pool_full[i].quality_gate_penalty_total = <delta>` (the sum of all gate penalties for this candidate, e.g. −1.25 for 1 blocking + 1 non-blocking).\n\
\n\
    2b-2. For each candidate, evaluate every criterion in `policy.defaultEligibilityCriteria` as a boolean predicate:\n\
      - `\"no_blocking_quality_gates\"`: candidate has zero blocking gate failures.\n\
      - `\"confidence_not_low\"`: candidate.confidence != \"low\".\n\
      - `\"has_supporting_evidence\"`: candidate has >= 1 supporting evidence record.\n\
      - `\"has_high_quality_support\"`: candidate has >= 1 evidence record whose class is in `policy.citationMinimum.highQualitySourceTypes` (official source / independent benchmark / primary literature).\n\
      - `\"no_contradictory_claims\"`: candidate has zero evidence records flagged contradicted / mixed / unresolved / retracted (see `policy.contradiction.blockingStatuses`).\n\
      - `\"no_freshness_issues\"`: candidate claim freshness status is in `policy.freshness.acceptableStatuses`.\n\
      - `\"literature_eligibility_confirmed\"`: candidate's `literature_eligible` flag is `true`.\n\
      A candidate fails default-eligibility if it fails ANY criterion. If `defaultEligibilityCriteria` is absent from the policy, treat every candidate as eligible (backward-compat). Record `decision.json::candidate_pool_full[i].passes_default_eligibility_criteria = <bool>` and `failed_criteria = [<list of failed criterion names>]`.\n\
\n\
    2b-3. Tier assignment respects the eligibility flag. The `defaultRecommended` tier requires the candidate to satisfy ALL criteria in `defaultEligibilityCriteria` (i.e. `passes_default_eligibility_criteria == true`). Candidates that fail eligibility may still appear as `tentative` or `alternative` but must NOT be selected as the `defaultRecommended` pick. When the top-composite candidate is ineligible, promote the highest-scoring eligible candidate to `defaultRecommended`; if no eligible candidate exists, block for SME review rather than recommending an ineligible default.\n\
\n\
Together these rules mean: spec-preferred tools that ARE in the env get picked automatically; spec-preferred tools that AREN'T fall through cleanly with a structured `env_capability_skip` rationale rather than silently swapping to a Python analog. Stages where no method preferences were expressed score all candidates on the remaining 4 axes at full budget. Quality-gate failures reduce composite scores and exclude top-scoring candidates from the default-recommended tier when eligibility criteria are unmet.\n\
\n\
## Execution-time method availability — install the libraries the method needs\n\
\n\
The discover-step `env_capability` check above only probes a fixed set of known capabilities (R+Seurat, R+CellChat, pySCENIC, python_lisi, rna_velocity, cellranger). The base image (`bio-min:local` or equivalent) ships a curated baseline — numpy / scipy / pandas / scikit-learn / etc. — but it does NOT include every method any discover-step or SME pinning might choose. **The executor agent owns runtime package installation.** When the selected method (whether picked by the discover step's composite scoring or pinned via `sme-decisions.json::method_substitution`) needs a library the env doesn't have, INSTALL IT.\n\
\n\
The flow at task start:\n\
\n\
1. **Resolve the chosen method** from `sme-decisions.json` (`method_substitution.chosen`), the upstream `discover_*` decision (`runtime/outputs/discover_<stage>/decision.json::chosen`), or — if neither pins one — pick from `attributes.candidate_tools` using the same composite-scoring rationale.\n\
2. **Probe importability** of the package(s) the method requires (`python -c 'import gseapy'`, `Rscript -e 'library(fgsea)'`, etc.). On a clean import, proceed.\n\
3. **On `ModuleNotFoundError` / package-not-found**, install at task start. Default channel per language:\n\
   - **Python wheels:** `pip install <name>` (or `pip install <name>==<version>` when the discover decision pins a version).\n\
   - **R / Bioconductor:** `Rscript -e 'if (!requireNamespace(\"BiocManager\", quietly=TRUE)) install.packages(\"BiocManager\"); BiocManager::install(\"<name>\", update=FALSE, ask=FALSE)'` for Bioconductor packages (fgsea, clusterProfiler, DESeq2, edgeR, limma, …); plain `install.packages(\"<name>\")` for CRAN.\n\
   - **Conda / bioconda:** `conda install -y -c bioconda -c conda-forge <name>` when neither pip nor BiocManager carries it and the base image has conda.\n\
   - Capture the install transcript to `runtime/outputs/<task_id>/scripts/00_install.log` and record `language_packages_installed: [{{name, version, channel}}]` in `result.json` so the package stays auditable.\n\
4. **Re-probe** after install. If the import now succeeds, run the method as-pinned. If the install itself failed (network blocked, package name doesn't exist on the channel, build dependency missing), THEN re-block with `awaiting_structured_decision` and `decision_points_for_sme: [\"switch to <available_alternative>\", \"skip stage\"]`. The install failure goes into the blocker's `evidence` block.\n\
\n\
Treat the install as part of the method's setup, not as an exceptional event. The base image is intentionally minimal; expanding the toolbox at task time is the expected path, not a fallback.\n\
\n\
What you must NOT do: import the chosen library, catch `ModuleNotFoundError`, and emit a `method_note` claiming \"functionally equivalent\" with an in-task reimplementation. The result is no longer reproducible against the named method, and the package's claim that it ran the pinned tool is false. \"Custom prerank GSEA using numpy/scipy\" is not gseapy; \"manual Welch's t-test\" is not DESeq2. If you genuinely cannot install the requested library, RE-BLOCK — do not silently substitute.\n\
\n\
## SME-supplied data inputs (consult BEFORE public-repo discovery)\n\
\n\
When the SME has registered local data (file present at `runtime/inputs.json`, also surfaced under the `## SME-supplied data inputs` section of this CONTEXT.md), the `data_acquisition` stage MUST consume those registrations as its primary input and SHOULD NOT propose public-repository fetchers as the top candidate.\n\
\n\
1. **At the start of `discover_data_acquisition`**, check `runtime/inputs.json`. The schema is `[{{ input_id, label, kind: \"local_path\" | \"uploaded_files\", root_path, files: [{{relpath, size_bytes, sha256}}], registered_at, registered_by }}, ...]`.\n\
2. **If the array is non-empty**, your candidate pool MUST include `sme_supplied_local_path` (when any input has `kind: \"local_path\"`) and/or `sme_supplied_uploaded_files` (for `kind: \"uploaded_files\"`). Score these candidates with a strong spec-preference boost (`+0.40` on `spec_match`) so they outrank generic GEO/SRA fetchers. Auto-pick when the boost yields a clear winner; only block for SME approval when there's a genuine ambiguity (e.g. mixed local + cited-but-unsupplied accessions).\n\
3. **The `data_acquisition` task itself** (the compute task that follows discovery) reads `runtime/inputs.json` and copies / symlinks the SME files into the canonical layout `runtime/outputs/data_acquisition/data/<source_label>/<filename>`. Compute and verify each file's sha256 against the manifest; flag mismatches as a blocker (data drift between registration and execution).\n\
4. **Empty `runtime/inputs.json`** (or the file absent entirely) means the SME relies on public accessions captured in CONTEXT.md prose — the existing public-repo dispatcher path applies unchanged.\n\
\n\
This rule is independent of the env_capability and spec_preferred_methods rules above and runs FIRST: an SME-registered input always takes precedence over any other ranking signal.\n\
\n\
## Empty-completion is NOT permitted (with one carve-out)\n\
\n\
When you apply the available SME decisions and still cannot produce non-empty output (e.g. header-only tables, all-zero counts, every compartment failing a minimum-samples gate, any sentinel like `overall_<stage>_not_run: true`), you MUST re-block the task rather than mark it `completed` with an empty result. Write a new `blocker.json` with narrower `decision_points_for_sme` (for example: 'sample-level age TSV required', 'pick a different threshold', 'alternative grouping variable'), set `task.state.status = \"blocked\"`, and stop. Do not silently advance the DAG past an empty computation.\n\
\n\
The harness + validator enforce this:\n\
- Any completed task whose result carries an `overall_*_not_run: true` key is automatically re-blocked by the harness on the next iteration.\n\
- Any completed compute task whose output tables (listed under `manifest.downstream_handoff` or the stage's canonical layout) contain zero data rows is flagged PASS-WITH-WARN by its validator.\n\
\n\
### Carve-out: SME-acknowledged skip is TERMINAL — DO NOT re-block\n\
\n\
When `runtime/outputs/<task_id>/sme-decisions.json` carries a chosen option from the closed skip-intent vocabulary, the SME has explicitly authorized completing the task with an empty/sentinel result and the harness's silent-completion guard will accept it. Writing a NEW `blocker.json` after this point creates an infinite loop — the agent reblocks, the harness short-circuits the reblock and dispatches you again, you reblock again. Recognized skip-intent option ids:\n\
\n\
- `emit_skip_sentinel_row`, `skip_with_deviation`, `skip_with_documented_deviation` — emit a 1-row skip sentinel CSV and status=`completed`. Result must include `skipped_per_sme: true`, `sme_chosen_option_id: <id>`, and a `claim_boundary_note` documenting downstream effects.\n\
- `mark_task_failed_documented_deviation` — status=`failed`, same markers (`sme_chosen_option_id`, `claim_boundary_note`). Downstream dependents may need their own SME skip decisions.\n\
- `drop_stage_from_workflow`, `apply_workflow_amend_then_resume` — status=`completed` with `dropped_per_sme: true` + `sme_chosen_option_id`. Do NOT emit a follow-up 'drop routing' question; the SME's chosen path IS the routing. The harness will not re-dispatch downstream consumers.\n\
\n\
The validator distinguishes 'agent gave up' (rejected) from 'SME explicitly authorized skip' (accepted) by the presence of `skipped_per_sme` / `dropped_per_sme` + `sme_chosen_option_id` in the result. Without those markers, the empty-completion rule above applies and the harness will re-block.\n\
\n\
## Figures (REQUIRED when the task spec has `required_figures`)\n\
\n\
Every compute task whose `spec.required_figures` is non-empty MUST produce each listed figure under `runtime/outputs/<task_id>/figures/<figure_id>.png` and `<figure_id>.pdf` **before** marking the task `completed`. Use the shared plotting library shipped with this package. If `task.spec.plot_stage_id` is present, pass that value as `stage_id`; otherwise pass the task id:\n\
\n\
```python\n\
import sys\n\
from pathlib import Path\n\
sys.path.insert(0, str(Path.cwd()))  # so `runtime.plotting` resolves\n\
from runtime.plotting.core import generate\n\
mf = generate(\n\
    stage_id=task_spec.get(\"plot_stage_id\", \"<task_id>\"),\n\
    outputs_dir=Path(\"runtime/outputs/<task_id>\"),\n\
    required=<task.spec.required_figures>,\n\
)\n\
# mf.written is a dict of figure_id -> path; include it in task.state.result\n\
```\n\
\n\
The harness treats missing required figure PNG/PDF files and a missing `figures/manifest.json` as a hard completion failure. If input artifacts are absent, block the task with a concrete missing-input reason instead of completing with skipped required figures. Do NOT silently omit figures from the result.\n\
\n\
The library owns determinism (Agg backend, stripped metadata, seeded RNG, theme baseline from `runtime/plotting/theme.json`). Output is dual-format: a 300dpi PNG and a vector PDF for every figure. Do NOT import matplotlib directly — go through `runtime.plotting.core` helpers (`violin`, `bar`, `scatter`, `volcano`, `heatmap`) plus `categorical_palette(n)` for any categorical encoding (Wong/Glasbey colorblind-safe; never `tab10`/`tab20`) so every figure across the package is byte-reproducible.\n\
\n\
**For R-based tasks** (Seurat / DESeq2 / Bioconductor), source the parallel R-side library at `runtime/plotting_r/core.R` and call `swfc_savefig(plot, path, stage_id=...)`. Both renderers consume the same `theme.json`, the same Wong palette, and produce figures at the same figure_id catalog so the validator's `figures_present` check is renderer-agnostic.\n\n\
## Hardware-aware execution\n\
\n\
You run under a harness that passes a per-task hardware envelope in environment variables (prefix `ECAA_HW_`). Never ignore these vars. Parse `ECAA_HW_TOOL_THREAD_CURVES`, `ECAA_HW_ENV_OVERRIDES`, `ECAA_HW_INTAKE_FACTS`, `ECAA_HW_CONCURRENT_PEERS_BY_CLASS` as JSON; the rest are plain scalars.\n\
\n\
- BLAS / OpenMP / NumExpr / etc. thread budgets are ALREADY set as bare env vars on your shell environment by the harness (`OMP_NUM_THREADS`, `OPENBLAS_NUM_THREADS`, `MKL_NUM_THREADS`, `NUMEXPR_NUM_THREADS`, `BLIS_NUM_THREADS`, `TBB_NUM_THREADS`, `RAYON_NUM_THREADS`, `NUMBA_NUM_THREADS`, `JULIA_NUM_THREADS`, `POLARS_MAX_THREADS`, `VECLIB_MAXIMUM_THREADS`, `GOTO_NUM_THREADS`). Numerical libraries read these at .so init time, so DO NOT use `Sys.setenv()` in R or `os.environ[...] = ...` in Python to set them — that runs after BLAS has already loaded and is a no-op. To change BLAS thread count at runtime use `RhpcBLASctl::blas_set_num_threads(N)` in R or `threadpoolctl.threadpool_limits(N)` in Python. The bundled `ECAA_HW_ENV_OVERRIDES` JSON is back-compat metadata; you do not need to parse or re-export it.\n\
- Pass `--threads N` (or the tool-specific equivalent) equal to `min(ECAA_HW_RECOMMENDED_THREADS, ECAA_HW_TOOL_THREAD_CURVES[<your-tool>])`. Never default to 1, never default to `$(nproc)`. If your tool isn't in `tool_thread_curves`, fall back to `ECAA_HW_RECOMMENDED_THREADS`.\n\
- Piped multi-threaded tools (`bwa mem -t X | samtools sort -@ Y`): split `ECAA_HW_RECOMMENDED_THREADS` favoring the CPU-bound stage. Typical split: `X = 0.6 * recommended_threads`, `Y = 0.4 * recommended_threads`.\n\
- Distinguish compression/decompression thread flags (`samtools -@`, `pigz -p`, `bgzip -@`) from compute thread flags (`--threads`). They are separate pools and should not share a budget.\n\
- GPU routing (not recommendation): if the chosen method has an entry in `policies/gpu-capability-policy.json` AND `ECAA_HW_GPU != \"none\"` AND the `requires` binaries are on `PATH` (probe with `which`), invoke the `gpu_impl`. This is routing — the method was selected upstream. On missing binary, fall back to `cpu_impl` with a warning logged to `runtime/task-log.jsonl`.\n\
- Size batch parameters (AlphaFold tile, Parabricks batch, ESMfold max sequence length) to the VRAM implied by `ECAA_HW_GPU` (format `nvidia-<kind>:<count>`; VRAM is implicit from kind).\n\
- Multi-phase tools (DeepVariant `make_examples → call_variants → postprocess_variants`): read `phase_thread_counts` from `policies/compute-resource-policy.json` rather than using a single `recommended_threads` for every phase.\n\
- Respect `ECAA_HW_CONCURRENT_PEERS_BY_CLASS`. When your class's peer count > 1 in that map, reduce your thread budget proportionally — the scheduler has granted you a slice, not the whole box. This field is always `{{cpu_heavy: 1}}` today but will be dynamic once parallel scheduling lands.\n\
\n\
## Auto-detect compute and fan out embarrassingly parallel work\n\
\n\
You are expected to fully utilize the compute granted to you. Detect what's actually available at runtime and fan out independent units of work across all of it — never default to a serial loop when the work is parallelizable.\n\
\n\
### Step 1 — Detect at runtime, don't trust prior assumptions\n\
\n\
Run these probes before any heavy work and log the results to `runtime/outputs/<task_id>/progress.log`:\n\
\n\
- **Total cores**: `nproc --all` (Linux) — fallback `getconf _NPROCESSORS_ONLN` if `nproc` is missing. In Python: `os.cpu_count()`. In R: `parallel::detectCores(logical=TRUE)`.\n\
- **Free memory (MiB)**: `free -m | awk 'NR==2 {{print $7}}'` (the \"available\" column on Linux). In Python: `psutil.virtual_memory().available // (1024*1024)`. In R: read `/proc/meminfo` `MemAvailable`.\n\
- **GPU presence**: `nvidia-smi --query-gpu=name,memory.free --format=csv,noheader 2>/dev/null` — empty output = no GPU. Cross-check against `ECAA_HW_GPU`.\n\
- **Container limits**: if running under cgroups v2, also check `/sys/fs/cgroup/cpu.max` and `/sys/fs/cgroup/memory.max` — these can be tighter than the host nproc.\n\
\n\
### Step 2 — Compute the worker pool\n\
\n\
- **Effective core budget**: `cores = min(detected_cores, ECAA_HW_RECOMMENDED_THREADS or detected_cores)`. The env var is a ceiling, not a target. If unset, use the full detected count.\n\
- **Reserve 1 core** for the orchestrator process: `usable = max(1, cores - 1)`.\n\
- **Inner thread budget per unit**: pick from `ECAA_HW_TOOL_THREAD_CURVES[your-tool]` if your tool is listed; otherwise default to `min(4, usable)` for BLAS-heavy R/Python (SCTransform, DESeq2, Seurat anchor finding) or `1` for pure-Python single-threaded code.\n\
- **Outer worker count**: `outer_workers = max(1, floor(usable / inner_threads_per_unit))`. Total active threads stay bounded: `outer_workers * inner_threads_per_unit ≤ usable`.\n\
- **Memory check**: estimate per-worker memory (e.g. an SCTransform on a 30k-cell Seurat object ≈ 6 GiB). If `outer_workers * per_worker_gib > available_gib`, reduce `outer_workers` until it fits, OR switch to BPCells/DelayedArray on-disk backing per the memory-discipline policy.\n\
\n\
### Step 3 — Fan out\n\
\n\
- **R**: `BiocParallel::bplapply(units, FUN, BPPARAM = MulticoreParam(workers = outer_workers))` on Linux; never `SnowParam` (slower fork-then-PSOCK overhead). The harness sets BLAS env vars to `recommended_threads` for the parent process — that's correct for a single-process Rscript, but oversubscribes when you fan out (each forked worker inherits `OMP_NUM_THREADS=recommended_threads` and the BLAS pool has already been created). To prevent oversubscription, call `RhpcBLASctl::blas_set_num_threads(inner_threads_per_unit)` INSIDE each worker (the FUN argument), AFTER `mclapply` forks. `Sys.setenv()` BEFORE the fan-out does NOT work — BLAS reads its thread count at .so init, which already happened when R started.\n\
- **Python**: `concurrent.futures.ProcessPoolExecutor(max_workers = outer_workers)` for CPU-bound work, or `joblib.Parallel(n_jobs = outer_workers, backend = \"loky\")`. Each child inherits the harness-set env vars; constrain per-worker BLAS at runtime with `threadpoolctl.threadpool_limits(inner_threads_per_unit)` inside the worker function (set, don't override the env vars).\n\
- **Shell**: `parallel -j outer_workers ...` for independent CLI invocations.\n\
- **Determinism**: `bplapply` and `Parallel(...)` preserve input order. If you use `imap_unordered`, sort the collected results by a stable key before writing output — the package must be byte-reproducible across runs.\n\
\n\
### Common embarrassingly-parallel cases in scRNA-seq\n\
\n\
- Per-sample / per-library: SCTransform, Scrublet, QC filtering, Cell Ranger reuse-and-fix\n\
- Per-compartment: integration (NP, AF, CEP independently), per-compartment clustering\n\
- Per-cluster / per-cell-type: DE marker discovery, pseudobulk DESeq2 fit, GSEA per-comparison\n\
- Per-permutation: GSEA / cell-type-proportion permutation tests, sensitivity sweeps\n\
- Per-fold: cross-validation, integration-method comparison\n\
\n\
### When NOT to fan out\n\
\n\
- Sequential dependencies inside a single task (the output of unit N feeds unit N+1)\n\
- One-shot work that fits in a single core-second\n\
- Stage spec explicitly says \"single-process\" or `parallel_processable: false`\n\
- Memory budget can't accommodate even 2 workers (use the on-disk libraries instead)\n\
\n\
### Required logging\n\
\n\
Append exactly one line per fan-out region to `runtime/outputs/<task_id>/progress.log`:\n\
\n\
```\n\
parallelism: detected_cores=<N>, recommended=<R>, usable=<U>, units=<K>, outer_workers=<W>, inner_threads=<T>, available_mem_gib=<M>, gpu=<g-or-none>\n\
```\n\
\n\
This lets the SME confirm the budget was actually used. If `outer_workers=1` despite `units > 1` and `usable > 1`, also log `parallelism_skip_reason=<one of: serial-deps | memory-budget | unit-too-small | spec-forbids>`.\n\
\n\
## Per-task progress reporting (drives the live UI progress bar)\n\
\n\
The server reads `runtime/outputs/<task_id>/progress.log` to render a live progress bar in the SME's web UI. To make that bar determinate (concrete N/M instead of an indeterminate shimmer), append exactly one structured marker line per phase as you advance:\n\
\n\
```\n\
[step <N>/<M>] <one-line description of the current phase>\n\
```\n\
\n\
The server picks the MOST RECENT `[step N/M]` line in the log; emit a new one whenever you transition phases. If your estimate of M changes mid-task (e.g., you discovered a sub-step you hadn't planned for), it's fine to revise — the bar updates honestly.\n\
\n\
### When to emit\n\
\n\
- Plan your task into a small number of distinct phases (typically 3-7) BEFORE you start, and emit `[step 1/<M>] <plan-line>` as one of your first progress.log lines.\n\
- Append a new `[step N/M]` line when you START each phase (not when you finish). The SME's bar reflects \"currently in phase N of M.\"\n\
- For embarrassingly-parallel fan-out, the steps are the phases of the whole task (download → parse → validate), not per-unit. Per-unit progress goes through the parallelism-log channel.\n\
- For a one-phase task that genuinely has nothing to subdivide, emit `[step 1/1]` once and skip the marker thereafter; the bar will sit at 100% but that's accurate.\n\
\n\
### Example for a 5-phase data acquisition task\n\
\n\
```\n\
[step 1/5] resolving 7 GEO accessions to download URLs\n\
[step 2/5] downloading 47 supplementary files in parallel (outer_workers=8)\n\
[step 3/5] parsing matrix files into per-sample 10x mtx triplets\n\
[step 4/5] validating cohort manifest against expected_libraries=47\n\
[step 5/5] writing per_accession_summary.json + matrices_index.json\n\
```\n\
\n\
This pattern lets every multi-phase task display real progress. Without it the bar falls back to expected_artifacts counting if the stage declares them, otherwise indeterminate.\n\
\n\
## Package containment (everything stays in $PACKAGE)\n\
\n\
The package is a self-contained, byte-reproducible artifact. EVERY script you author, every byte you download, every intermediate file you write, and every environment lock must land somewhere under `$PACKAGE/`. Nothing escapes — no `/tmp/`, no `$HOME/Downloads/`, no system-wide pip/conda/R caches the next runner won't have.\n\
\n\
### Required layout under runtime/outputs/<task_id>/\n\
\n\
- `scripts/` — every script you authored for this task. One file per logical step, named verb-first (e.g. `01_fetch_matrices.py`, `02_validate_columns.py`, `01_run_sctransform.R`, `pipeline.sh`). Include a shebang line and a comment block at the top stating: tool versions used, exact command line that invoked it, input artifact paths (relative to `$PACKAGE`), and output artifact paths. Do NOT use `Bash` heredocs that leave no on-disk script — every line of code that ran for this task must be replayable.\n\
- `data/` — raw downloaded inputs (GEO matrix files, supplementary tables, reference indexes) IF this task fetched them. Otherwise reference upstream task outputs by relative path: `../<upstream_task_id>/<artifact>` — never absolute paths, never paths outside `$PACKAGE`.\n\
- `intermediates/` — anything the script writes that isn't the final result but the SME might want to inspect (filtered count matrices, integration anchors, embedding matrices, fold-CV temp objects).\n\
- `<final-artifact-from-spec>` — the headline output named in `task.spec.expected_artifacts`. Lives directly under `runtime/outputs/<task_id>/` so RO-Crate registers it.\n\
- `figures/<figure_id>.png` — required figures per `spec.required_figures`.\n\
- `env.lock` — resolved tool/library versions for reproducibility:\n\
    - R: `sessionInfo() |> capture.output() |> writeLines(\"env.lock\")` OR `renv::snapshot()` if the package uses renv\n\
    - Python: `pip freeze > env.lock` OR `conda env export > env.yml`\n\
    - System tools: append `<tool> --version` lines for non-R/Python binaries you invoked (samtools, bcftools, fastqc, parallel, etc.)\n\
- `progress.log` — per-iteration narrative (start, mid, end-with-decision)\n\
- `parallelism.log` (or appended to `progress.log`) — the structured `parallelism: ...` line per fan-out region.\n\
\n\
### Redirect tool caches into the package\n\
\n\
Set these BEFORE invoking heavy tools:\n\
\n\
```bash\n\
export TMPDIR=\"$PACKAGE/runtime/outputs/<task_id>/tmp\"\n\
export XDG_CACHE_HOME=\"$PACKAGE/runtime/cache\"\n\
export R_LIBS_USER=\"$PACKAGE/runtime/r-libs\"\n\
export PIP_CACHE_DIR=\"$PACKAGE/runtime/cache/pip\"\n\
export HF_HOME=\"$PACKAGE/runtime/cache/huggingface\"\n\
mkdir -p \"$TMPDIR\" \"$XDG_CACHE_HOME\" \"$R_LIBS_USER\" \"$PIP_CACHE_DIR\" \"$HF_HOME\"\n\
```\n\
\n\
`runtime/cache/` is shared across all tasks in the package; per-task `tmp/` is task-scoped.\n\
\n\
### When containment is impossible\n\
\n\
Some tools refuse to write into a relative path (rare — usually length, permissions, or a hard-coded system dir). Document the deviation in `runtime/outputs/<task_id>/result.json` under a `containment_deviations` array with: tool name, absolute path used, reason, and a copy of any critical files mirrored back into `runtime/outputs/<task_id>/external_refs/`. The SME reviews deviations as part of the decision audit.\n\
\n\
### Verification before completing\n\
\n\
Before flipping the task state to `completed`, the agent runs:\n\
\n\
```bash\n\
find runtime/outputs/<task_id>/ -type f | wc -l   # must be > 0\n\
test -f runtime/outputs/<task_id>/env.lock        # required\n\
test -d runtime/outputs/<task_id>/scripts/        # required\n\
ls runtime/outputs/<task_id>/scripts/*.{{py,R,sh,smk}} 2>/dev/null | wc -l  # must be > 0 unless task is pure-discovery\n\
```\n\
\n\
If any check fails, the task is incomplete — re-run the missing step or block with `containment_violation` as the blocker_kind.\n\
\n\
## Local git versioning (snapshot every task)\n\
\n\
The package is a git repo. Every task you complete creates a commit so the SME can diff runs, revert mistakes, and time-travel through the analysis.\n\
\n\
### Bootstrap (first agent invocation only — idempotent)\n\
\n\
```bash\n\
if [ ! -d \"$PACKAGE/.git\" ]; then\n\
  cd \"$PACKAGE\"\n\
  git init -q -b main\n\
  git config user.name \"scripps-workflow-agent\"\n\
  git config user.email \"agent@scripps-workflow.local\"\n\
  cat > .gitignore <<'EOF'\n\
runtime/cache/\n\
runtime/r-libs/\n\
runtime/outputs/*/tmp/\n\
runtime/outputs/*/.heartbeat\n\
*.pyc\n\
__pycache__/\n\
.Rhistory\n\
.ipynb_checkpoints/\n\
EOF\n\
  git add -A\n\
  git commit -q -m \"package: initial emit\"\n\
fi\n\
```\n\
\n\
### Per-task commit (run at the very end of every task, BEFORE setting status to completed)\n\
\n\
```bash\n\
cd \"$PACKAGE\"\n\
git add -A\n\
if ! git diff --cached --quiet; then\n\
  git commit -q -m \"task <task_id>: <one-line summary of what changed>\n\
\n\
method: <method_id_used>\n\
inputs: <upstream task_ids consumed>\n\
outputs: <files written under runtime/outputs/<task_id>/>\n\
parallelism: outer=<W> inner=<T> units=<K>\n\
runtime_seconds: <elapsed>\n\
agent_iteration: <i>\n\
\"\n\
fi\n\
```\n\
\n\
The commit message header is what the SME sees in `git log --oneline`. The body lets `git log --grep` find specific runs. The `task <task_id>:` prefix lets `git log -- runtime/outputs/<task_id>/` show only one task's history.\n\
\n\
### When a task re-runs (agent retry, rerun button, amend)\n\
\n\
Don't delete the old commits. The new run's commit lands on top:\n\
\n\
```bash\n\
git commit -q -m \"task <task_id>: rerun (iteration <N>)\n\
\n\
reason: <why — amend / blocker / failed first attempt>\n\
prior_commit: <sha of the previous task <task_id> commit>\n\
... (other fields)\"\n\
```\n\
\n\
Use `git log -- runtime/outputs/<task_id>/` for full task history; `git diff <prior_sha> HEAD -- runtime/outputs/<task_id>/` for what changed.\n\
\n\
### When a blocker fires\n\
\n\
Commit blocker artifacts before yielding to the SME:\n\
\n\
```bash\n\
git commit -q -m \"task <task_id>: blocked (awaiting_sme_*)\n\
\n\
blocker_kind: <kind>\n\
options_offered: <count>\n\
top_candidate: <name>\n\
\"\n\
```\n\
\n\
This way the SME's selection (when it lands) gets its own commit on top, and the diff cleanly shows what the SME changed. NEVER `git reset --hard` or `git rebase -i` — append commits, never rewrite history.\n",
        domain = classification.domain,
        description = classification.workflow_description,
    );

    // Claim boundary (from taxonomy) — standing instruction the agent must obey
    if let Some(boundary) = claim_boundary {
        if !boundary.trim().is_empty() {
            out.push_str("\n## Claim boundary (non-negotiable)\n\n");
            out.push_str(boundary.trim());
            out.push_str("\n\nThese limits on claims apply to every result, log entry, and blocked-task reason.\n");
        }
    }

    // SME directives — already-resolved discovery decisions carried verbatim
    let decisions = collect_sme_decisions(dag);
    if !decisions.is_empty() {
        out.push_str("\n## SME directives (compile-time resolved)\n\n");
        out.push_str(
            "The following discovery tasks were resolved by the SME during intake. \
                      Honor these choices exactly — do not re-run discovery on them.\n\n",
        );
        for d in &decisions {
            out.push_str(&format!("- **`{}`**", d.task_id));
            if !d.method.is_empty() {
                out.push_str(&format!(": {}", d.method));
            }
            out.push('\n');
            for (k, v) in &d.fields {
                out.push_str(&format!("    - `{}` = `{}`\n", k, v));
            }
        }
    }

    out
}

fn render_context(dag: &DAG, classification: &ClassificationResult) -> String {
    let mut out = format!(
        "# Workflow Context\n\n\
**Modality:** {modality}\n\
**Domain:** {domain}\n\
**Description:** {description}\n\
**EDAM topic:** {topic}\n\
**EDAM operation:** {operation}\n\
**Confidence:** {confidence_label} ({confidence:.0}%)\n\n",
        modality = classification.modality,
        domain = classification.domain,
        description = classification.workflow_description,
        topic = classification.edam_topic,
        operation = classification.edam_operation,
        confidence_label = classification.confidence_label,
        confidence = classification.confidence * 100.0,
    );

    if !classification.organisms.is_empty() {
        out.push_str("## Organisms\n");
        for org in &classification.organisms {
            out.push_str(&format!("- {} (taxon:{})\n", org.name, org.taxon_id));
        }
        out.push('\n');
    }

    // SME discovery decisions: walk the DAG for tasks whose state is Completed
    // with resolved_by=sme. This captures /resolve directives (not keyword scrapes)
    // and preserves the full method prose plus any structured fields the SME set.
    let sme_decisions = collect_sme_decisions(dag);
    if !sme_decisions.is_empty() {
        out.push_str("## SME discovery decisions\n\n");
        out.push_str(
            "_Authoritative: these resolutions override any heuristic method scan below._\n\n",
        );
        for decision in &sme_decisions {
            out.push_str(&format!("### `{}`\n", decision.task_id));
            if !decision.method.is_empty() {
                out.push_str(&format!("**Method:** {}\n\n", decision.method));
            }
            if !decision.fields.is_empty() {
                out.push_str("**Structured fields:**\n");
                for (k, v) in &decision.fields {
                    out.push_str(&format!("- `{}` = `{}`\n", k, v));
                }
                out.push('\n');
            }
        }
    }

    if !classification.methods_specified.is_empty() {
        out.push_str("## Methods mentioned in SME prose\n\n");
        out.push_str("_Keyword-scraped from intake text; see SME discovery decisions above for authoritative values._\n\n");
        for m in &classification.methods_specified {
            out.push_str(&format!("- {}: {}\n", m.stage, m.method));
        }
        out.push('\n');
    }

    if !classification.data_sources.is_empty() {
        out.push_str("## Data sources\n");
        for ds in &classification.data_sources {
            let label = if ds.kind.is_empty() {
                String::new()
            } else {
                format!(" ({})", ds.kind)
            };
            let qual = match &ds.qualifier {
                Some(q) => format!(" — qualifiers: {}", q),
                None => String::new(),
            };
            out.push_str(&format!("- {}{}{}\n", ds.accession, label, qual));
            for child in &ds.children {
                out.push_str(&format!("  - {} (sample)\n", child.accession));
            }
        }
        out.push('\n');
    }

    out.push_str("## SME intake text\n\n");
    out.push_str(&classification.intake_text);
    out.push('\n');
    out
}

/// A single SME-resolved discovery decision surfaced in CONTEXT.md and RO-Crate.
pub(crate) struct SmeDecision {
    pub task_id: TaskId,
    pub method: String,
    pub fields: Vec<(String, String)>,
}

/// Walk the DAG and collect all tasks whose completed result was written by
/// the SME at compile time. Used by both CONTEXT.md and the RO-Crate builder.
///
/// Re-routed through `Task::completion_kind()`'s typed
/// `AgentTaskResult::SmeResolved` inspector so the discriminator
/// (`resolved_by == "sme"`) lives in one place. Wire shape unchanged.
pub(crate) fn collect_sme_decisions(dag: &DAG) -> Vec<SmeDecision> {
    use crate::dag::AgentTaskResult;
    let mut out = Vec::new();
    // Stable order: walk BTreeMap (lexicographic) — topological ordering would
    // be nicer but the audience here is humans skimming the doc, not executors.
    for (id, task) in &dag.tasks {
        let Some(AgentTaskResult::SmeResolved { method, fields, .. }) = task.completion_kind()
        else {
            continue;
        };
        let mut rendered_fields: Vec<(String, String)> = Vec::new();
        for (k, v) in fields {
            // Skip the provenance + method keys already captured
            // structurally by the SmeResolved variant itself.
            if matches!(k.as_str(), "resolved_by" | "resolved_at" | "method") {
                continue;
            }
            let rendered = match v {
                serde_json::Value::String(s) => s.clone(),
                _ => v.to_string(),
            };
            rendered_fields.push((k.clone(), rendered));
        }
        out.push(SmeDecision {
            task_id: id.clone(),
            method: method.to_string(),
            fields: rendered_fields,
        });
    }
    out
}

#[cfg(test)]
mod tests;
