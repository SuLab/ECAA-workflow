//! intake-mutation tools.
//!
//! `set_intake_field`, `set_intake_method`, `append_intake_prose`. All
//! three write into `Session.intake_methods` / `Session.intake_prose`
//! and route through `rebuild_dag` so the LLM sees the updated DAG in
//! the next turn.

use super::classification::load_classifier;
use super::{rebuild_dag, state_delta, validate_discover_stage};
use crate::errors::{ToolError, ToolResult};
use crate::session::Session;
use ecaa_workflow_core::classify::{
    classify_project_class, load_project_class_keywords, ClassificationResult,
};
use ecaa_workflow_core::taxonomy::StageTaxonomy;
use std::path::Path;

/// Route intake prose to a ProjectClass after each
/// `append_intake_prose`. The config file is optional — if it's
/// missing we fall back to Bioinformatics (conservative default).
///
/// Once a session has committed to a non-bio class (clinical_trial or
/// time_series_forecast), subsequent re-classifies never downgrade it
/// back to Bioinformatics on the basis of accumulated prose alone.
/// Bioinformatics is the default-fallback bucket of the keyword
/// classifier (no positive bio keywords are configured), so a later
/// "Bioinformatics" verdict is almost always "no non-bio keywords hit
/// this turn" — typically because the SME is elaborating context, not
/// truly switching domains. Forward transitions (Bio → non-bio, or
/// non-bio → other non-bio) still apply.
fn reclassify_project_class(session: &mut Session, prose: &str, config_dir: &Path) {
    use ecaa_workflow_core::project_class::ProjectClass;
    let path = config_dir.join("project-class-keywords.yaml");
    if let Ok(cfg) = load_project_class_keywords(&path) {
        let new_class = classify_project_class(prose, &cfg);
        match (session.project_class, new_class) {
            (ProjectClass::Bioinformatics, _) => session.project_class = new_class,
            (_, ProjectClass::Bioinformatics) => {
                tracing::debug!(
                    session_id = %session.id,
                    prior = ?session.project_class,
                    "reclassify_project_class: skipping Bioinformatics downgrade"
                );
            }
            _ => session.project_class = new_class,
        }
    }
    // Missing config file → stay on the existing class (defaults to
    // Bioinformatics for fresh sessions); lets offline / mock-backend
    // tests skip the keyword file.
}

pub(super) fn set_intake_field(
    session: &mut Session,
    stage: &str,
    field: &str,
    value: &serde_json::Value,
    config_dir: &Path,
) -> ToolResult {
    // Stage id validation previously keyed off `taxonomy.stages[*].id`.
    // With the legacy YAML loader removed, `taxonomy.stages` is empty on
    // new sessions, so validation now keys off the composed DAG's task list.
    if let Some(dag) = &session.dag {
        let stage_known = dag.tasks.contains_key(stage);
        if !stage_known {
            let alternatives: Vec<String> = dag.tasks.keys().map(|id| id.to_string()).collect();
            return ToolResult::err(ToolError::ValidationFailure {
                reason: format!("unknown stage '{}'", stage),
                valid_alternatives: alternatives,
                hint: "Pick a stage id from the composed DAG.".into(),
            });
        }
    } else if session.taxonomy.is_none() {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "no taxonomy loaded".into(),
            hint: "Call append_intake_prose first so the taxonomy can be classified and loaded."
                .into(),
        });
    }

    // Anthropic's strict tool schema rejects union-typed inputs (oneOf,
    // anyOf, multi-`type` arrays), so the schema declares `value` as a
    // string and asks the LLM to JSON-encode anything richer. Recover
    // the structured form here when the string parses as JSON; pass
    // through unchanged for plain strings (organism names, accessions).
    let parsed = decode_field_value(value);

    session
        .intake_methods
        .set(stage, None, Some((field.to_string(), parsed.clone())));

    session.record_decision(
        ecaa_workflow_core::decision_log::DecisionType::SetIntakeField {
            stage: stage.to_string(),
            field: field.to_string(),
            value: parsed,
        },
        ecaa_workflow_core::decision_log::DecisionActor::Llm,
        None,
    );

    if let Err(e) = rebuild_dag(session, config_dir) {
        return ToolResult::err(e);
    }
    ToolResult::ok(state_delta(session, stage))
}

/// Sub-archetype small-task scoping handler. Sets `session.excluded_atoms`
/// to the (deduped, non-empty-stripped) atom-id list and triggers a
/// rebuild so the next read sees the pruned DAG. Records the change as
/// a `SetIntakeField`-shaped decision so the audit trail is uniform.
pub(super) fn set_intake_excluded_atoms(
    session: &mut Session,
    atom_ids: &[String],
    config_dir: &Path,
) -> ToolResult {
    // Protected-atom guard. These are universal terminals every
    // archetype's structural contract relies on (raw_qc is the
    // input-QC sentinel; reporting + final_reporting are required
    // by every YAML archetype; generic_summary is the Tier-B
    // fallback deliverable). Letting the LLM exclude them produced
    // 6 corpus failures (#1, #3, #6, #8, #9, #10) where the eval
    // rejected packages missing one of these atoms. Belt-and-
    // suspenders alongside the prompt_role.txt PROTECTED ATOMS
    // guidance — refuse the call before it mutates session state
    // so a slipped LLM turn doesn't cascade into a bad emit.
    const PROTECTED: &[&str] = &["raw_qc", "reporting", "final_reporting", "generic_summary"];
    let mut requested_protected: Vec<&str> = Vec::new();
    for id in atom_ids {
        let t = id.trim();
        if let Some(p) = PROTECTED.iter().find(|p| **p == t) {
            requested_protected.push(*p);
        }
    }
    if !requested_protected.is_empty() {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: format!(
                "atom_ids contains protected terminal atom(s): {:?} — these are required by every archetype and cannot be excluded",
                requested_protected
            ),
            hint: "Drop the protected ids from the exclusion list. raw_qc/reporting/final_reporting/generic_summary are universal terminals; even 'minimum-fill' DAGs must keep them. Exclude only upstream pipeline stages (sequence_trimming, alignment, quantification, qc_preprocessing, normalisation) and the literature atoms.".into(),
        });
    }
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let normalized: Vec<String> = atom_ids
        .iter()
        .filter_map(|id| {
            let trimmed = id.trim();
            if trimmed.is_empty() {
                return None;
            }
            if seen.insert(trimmed.to_string()) {
                Some(trimmed.to_string())
            } else {
                None
            }
        })
        .collect();
    session.excluded_atoms = normalized.clone();
    session.record_decision(
        ecaa_workflow_core::decision_log::DecisionType::SetIntakeField {
            stage: "_session".to_string(),
            field: "excluded_atoms".to_string(),
            value: serde_json::Value::Array(
                normalized
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
        },
        ecaa_workflow_core::decision_log::DecisionActor::Llm,
        None,
    );
    if let Err(e) = rebuild_dag(session, config_dir) {
        return ToolResult::err(e);
    }
    ToolResult::ok(serde_json::json!({
        "outcome": "excluded_atoms_set",
        "count": normalized.len(),
        "atom_ids": normalized,
    }))
}

/// SME-confirmed modality override. The LLM calls this after the user
/// disambiguates a `tie_candidates` quick-reply or explicitly names a
/// modality the classifier under-ranked. Refuses unknown modality ids
/// (checked against the on-disk `config/modalities/` registry) and
/// refuses when no classification has been seeded yet (call
/// `append_intake_prose` first). On success, rewrites
/// `session.classification.modality`, clears `archetype_id` and the
/// archetype snapshot so the composer reseeds, drops `taxonomy` so the
/// next `rebuild_dag` reloads it for the new modality, records a
/// `SetIntakeField`-shaped decision, and rebuilds the DAG.
pub(super) fn set_intake_modality(
    session: &mut Session,
    modality_id: &str,
    config_dir: &Path,
) -> ToolResult {
    let trimmed = modality_id.trim();
    if trimmed.is_empty() {
        return ToolResult::err(ToolError::ValidationFailure {
            reason: "modality_id is empty".into(),
            valid_alternatives: vec![],
            hint: "Pass a known modality id (see config/modalities/*.yaml).".into(),
        });
    }

    // Validate against the on-disk modality registry. Routed through
    // the process-wide `load_cached` so repeated tool dispatches in a
    // chat session pay the YAML-parse + schema-validate cost once.
    let modalities_dir = config_dir.join("modalities");
    let registry =
        match ecaa_workflow_core::modality_registry::ModalityRegistry::load_cached(&modalities_dir)
        {
            Ok(r) => r,
            Err(e) => {
                return ToolResult::err(ToolError::InternalError {
                    reason: format!("loading modality registry: {}", e),
                });
            }
        };
    if registry.get(trimmed).is_none() {
        let alternatives: Vec<String> = registry.iter().map(|(id, _)| id.clone()).collect();
        return ToolResult::err(ToolError::ValidationFailure {
            reason: format!("unknown modality '{}'", trimmed),
            valid_alternatives: alternatives,
            hint: "Pick a modality id from config/modalities/*.yaml; the SME's quick-reply \
                   selection should already match one of these."
                .into(),
        });
    }

    if session.classification.is_none() {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "no classification yet — set_intake_modality is an override, not a primer"
                .into(),
            hint: "Call append_intake_prose first so the classifier produces an initial \
                   ClassificationResult; set_intake_modality then rewrites the modality \
                   field after the SME confirms a tie."
                .into(),
        });
    }

    // Rewrite the classification + clear derived snapshots so the
    // composer reseeds against the SME-confirmed modality on the next
    // rebuild. `taxonomy` is dropped + rebuilt from the archetype
    // catalog under the new modality, so the per-modality metadata
    // doesn't carry the prior (wrong) modality forward.
    if let Some(c) = session.classification.as_mut() {
        c.modality = trimmed.to_string();
        c.archetype_id = None;
    }
    session.archetype_snapshot = None;
    session.taxonomy = None;

    // Reload the classifier so the rebuild_dag taxonomy path resolves
    // against a fresh `Classifier::load` (which validates the modality
    // registry is non-empty and current). Discarding the loaded
    // classifier here is intentional — `rebuild_dag` doesn't take it;
    // we're just paying the load cost to surface any registry-level
    // error (missing manifest, drift) before we attempt the rebuild.
    if let Err(e) = super::classification::load_classifier(config_dir) {
        return ToolResult::err(e);
    }

    // Re-derive the lightweight `StageTaxonomy` metadata from the
    // archetype catalog under the new modality. rebuild_dag's
    // precondition is `session.taxonomy.is_some()`; without this the
    // override would always trip "no taxonomy loaded".
    match build_taxonomy_metadata_for_modality(trimmed, session.project_class, config_dir) {
        Ok(tax) => {
            if let Some(c) = session.classification.as_mut() {
                c.domain = tax.domain.clone();
                c.workflow_description = tax.description.clone();
            }
            session.taxonomy = Some(tax);
        }
        Err(e) => {
            return ToolResult::err(ToolError::InternalError {
                reason: format!(
                    "archetype metadata load failed for modality={}, project_class={:?}: {}",
                    trimmed, session.project_class, e
                ),
            });
        }
    }

    session.record_decision(
        ecaa_workflow_core::decision_log::DecisionType::SetIntakeField {
            stage: "_session".to_string(),
            field: "modality".to_string(),
            value: serde_json::Value::String(trimmed.to_string()),
        },
        ecaa_workflow_core::decision_log::DecisionActor::Llm,
        None,
    );

    if let Err(e) = rebuild_dag(session, config_dir) {
        return ToolResult::err(e);
    }
    ToolResult::ok(serde_json::json!({
        "outcome": "modality_overridden",
        "modality": trimmed,
        "state": session.state,
    }))
}

/// Best-effort recovery of a structured value the LLM may have JSON-encoded
/// into a string to fit Anthropic's strict tool-schema rules. Trims, then
/// only attempts a parse when the trimmed text starts with a JSON
/// container (`[`/`{`), boolean (`t`/`f`), null, or a digit/sign — a
/// plain identifier like `GSE12345` stays a string.
fn decode_field_value(value: &serde_json::Value) -> serde_json::Value {
    let serde_json::Value::String(s) = value else {
        return value.clone();
    };
    let trimmed = s.trim();
    let likely_json = trimmed.starts_with('[')
        || trimmed.starts_with('{')
        || trimmed == "true"
        || trimmed == "false"
        || trimmed == "null"
        || trimmed
            .chars()
            .next()
            .is_some_and(|c| c == '-' || c.is_ascii_digit());
    if !likely_json {
        return value.clone();
    }
    serde_json::from_str::<serde_json::Value>(trimmed).unwrap_or_else(|_| value.clone())
}

pub(super) fn set_intake_method(
    session: &mut Session,
    stage: &str,
    method_prose: &str,
    config_dir: &Path,
) -> ToolResult {
    if method_prose.trim().is_empty() {
        return ToolResult::err(ToolError::empty_string(
            "method_prose",
            "Provide the SME's method description verbatim.",
        ));
    }
    if session.taxonomy.is_none() {
        return ToolResult::err(ToolError::no_taxonomy());
    }
    // Intake-method stage normalization. The LLM
    // sometimes calls set_intake_method with a `discover_<stage>`
    // prefix matching the on-disk task id (e.g. `discover_aligner`)
    // when the SME's prose names the bare verb (`aligner`). Strip
    // the prefix so callers don't need to know whether the
    // discovery was already disambiguated. validate_discover_stage
    // below still rejects unknown stems, so a mistyped stage name
    // surfaces as ToolError::ValidationFailure with the alternatives
    // list — the SME-facing error stays informative.
    let stage = stage.strip_prefix("discover_").unwrap_or(stage);

    // Server-side SME-signal gate. `set_intake_method`
    // is only permitted to pin a method on a stage when the SME has
    // explicitly named that method via a UI affordance (quick-reply chip
    // or structured intake form). The UI posts to
    // `/api/chat/session/:id/intake-method/:stage_id/sme-named` to flip
    // the flag before the next LLM turn so the dispatch can land. Without
    // this gate the LLM can auto-pin methodological choices the SME never
    // approved, in violation of prompt_role.txt's tool-method-neutrality
    // rule. The check fires BEFORE the decision-log write so a refused
    // call leaves zero trace in `runtime/decisions.jsonl`.
    let signaled = session
        .sme_method_signals
        .named
        .get(stage)
        .copied()
        .unwrap_or(false);
    if !signaled {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: format!(
                "the SME has not yet named a method for the step `{}`",
                stage
            ),
            hint:
                "Ask the SME explicitly which method they want for this step. The system records \
                 the SME's named choice through a UI affordance; only call `set_intake_method` \
                 after that signal lands."
                    .into(),
        });
    }

    // Validate against the built DAG rather than the taxonomy stage list.
    // `set_intake_method` only lands when `builder.rs::resolve_intake_methods`
    // finds a matching `discover_<stage>` task (otherwise it silently skips),
    // so anything the taxonomy defines as an execute-task id would pass
    // taxonomy validation but vanish on write. `validate_discover_stage`
    // captures the detection + alternatives-list logic.
    if let Err(e) = validate_discover_stage(session, stage) {
        return ToolResult::err(e);
    }

    session
        .intake_methods
        .set(stage, Some(method_prose.to_string()), None);

    session.record_decision(
        ecaa_workflow_core::decision_log::DecisionType::SetIntakeMethod {
            stage: stage.to_string(),
            method_prose: method_prose.to_string(),
        },
        ecaa_workflow_core::decision_log::DecisionActor::Llm,
        None,
    );

    if let Err(e) = rebuild_dag(session, config_dir) {
        return ToolResult::err(e);
    }
    ToolResult::ok(state_delta(session, stage))
}

pub(crate) fn append_intake_prose(
    session: &mut Session,
    prose: &str,
    config_dir: &Path,
) -> ToolResult {
    if let Err(result) = validate_prose_input(prose) {
        return result;
    }

    if should_replace_intake_scope(prose) {
        session.intake_prose.clear();
        session.classification = None;
        session.taxonomy = None;
        #[allow(deprecated)] // deliberate cache-reset for non-workflow_dag state change
        session.invalidate_dag();
        session.archetype_snapshot = None;
    }

    if !session.intake_prose.is_empty() {
        session.intake_prose.push(' ');
    }
    session.intake_prose.push_str(prose);

    // Path-hint extraction (e2e #13). Scan the prose for filesystem-
    // shaped tokens that resolve under ECAA_INPUT_ROOTS and stash
    // them on the session so the LLM (via get_session_state) and the
    // UI (via SessionStateSnapshot) can offer to register them. When
    // `ECAA_AUTO_REGISTER_PROSE_PATHS=1` is set we promote each hint
    // straight onto `session.inputs` without waiting for SME approval
    // — useful in non-interactive fixture runs where no SME loop
    // exists.
    extract_and_apply_path_hints(session, prose);

    // If the SME is responding to a pending disambiguation prompt by
    // pasting / typing the chip id (or it appears unambiguously in
    // the prose), clear the latch so we don't keep surfacing the
    // same prompt. The helper is a no-op when no pair is pending or
    // when the prose doesn't contain a known chip id.
    maybe_clear_pending_disambiguation_from_prose(session, prose, config_dir);

    let clf = match load_classifier(config_dir) {
        Ok(c) => c,
        Err(e) => return ToolResult::err(e),
    };
    let prior_modality = session.classification.as_ref().map(|c| c.modality.clone());
    let mut new_clf = clf.classify(&session.intake_prose);

    // Route to a ProjectClass alongside modality classification.
    // Keyword-only; conservative on ties.
    reclassify_project_class(session, &session.intake_prose.clone(), config_dir);

    // Populate session taxonomy metadata from the matched archetype
    // rather than loading a YAML taxonomy from disk. The legacy
    // `config/stage-taxonomies/<modality>.yaml` files have been removed;
    // the archetype catalog is now the source of truth. We construct a
    // lightweight `StageTaxonomy` shaped enough for the emit +
    // session-state paths that still read it (id, domain, description,
    // policies, claim_boundary, project_class).
    if let Err(result) = apply_taxonomy_metadata(session, &mut new_clf, config_dir) {
        return result;
    }
    session.classification = Some(new_clf.clone());
    clear_incompatible_archetype_snapshot(session, &new_clf);

    // Calibrated learning-to-defer: when the classifier is in a known
    // tie window, set `pending_disambiguation` so the next
    // `propose_quick_replies` call emits a targeted SME prompt instead
    // of guessing.
    maybe_set_pending_disambiguation(session, &new_clf, config_dir);

    // When the classifier populated `goal` (from
    // `modality-keywords.yaml::goal_patterns`) and the archetype catalog
    // has a clear winner, snapshot the matched archetype onto the session.
    // Two effects:
    // - Future `rebuild_dag` calls can route through
    // `build_dag_from_composition` (live archetype fast-path)
    // instead of the legacy taxonomy build, exercising the
    // composer in production. Today's rebuild still falls
    // through to `build_dag_from_taxonomy`; the snapshot
    // just makes the composer path's preconditions visible.
    // - In-flight sessions with `archetype_snapshot` validate the
    // archetype path's parity with the legacy build.
    //
    // Soft-skips on every error path: a missing `config/archetypes/`
    // directory, a tie within 5%, or an empty match list all leave
    // `archetype_snapshot = None` and the legacy path keeps working.
    // Sessions that already pinned a snapshot stay pinned; we only
    // populate when None to keep amendment paths byte-stable.
    if session.archetype_snapshot.is_none() {
        maybe_pin_archetype_snapshot(session, &new_clf, config_dir);
    }

    // `StateTrigger::AppendProse` (Greeting → Intake)
    // fires from the dispatcher's post-handler hook. The handler no
    // longer calls `try_transition` directly — see
    // `tools/mod.rs::append_intake_prose_post_ok`.

    // Rebuild DAG so the LLM can see what it produced. A rebuild
    // failure here means the loaded taxonomy + accumulated methods
    // can't form a valid DAG — surface that to the LLM rather than
    // silently advancing the session toward a `propose_summary_confirmation`
    // → `emit_package` chain that will trip the "no DAG built"
    // precondition turns later.
    if let Err(e) = rebuild_dag(session, config_dir) {
        tracing::error!(
            session_id = %session.id,
            err = ?e,
            "append_intake_prose: rebuild_dag failed"
        );
        return ToolResult::err(e);
    }

    let modality_changed = prior_modality.as_deref() != Some(new_clf.modality.as_str());

    session.record_decision(
        ecaa_workflow_core::decision_log::DecisionType::AppendIntakeProse {
            fragment: prose.to_string(),
            classified_modality: new_clf.modality.clone(),
            modality_changed,
        },
        ecaa_workflow_core::decision_log::DecisionActor::Llm,
        None,
    );

    // Materialize the
    // typed `WorkflowIntent` from the SME's prose + classification
    // result. The persistence rail (`Session::workflow_intent`) is
    // already in place; this is where it gets populated. Subsequent
    // amendments overwrite the same field so the v4 planner always
    // reads the latest intent.
    materialize_workflow_intent(session, &new_clf, prose);

    ToolResult::ok(serde_json::json!({
        "modality": new_clf.modality,
        "modality_changed": modality_changed,
        "confidence": new_clf.confidence,
        "confidence_label": new_clf.confidence_label,
        "organisms": new_clf.organisms,
        "data_sources": new_clf.data_sources,
    }))
}

/// Reject empty prose and prose carrying markup / control characters /
/// internal tool-name tokens. Returns `Err(ToolResult)` carrying the
/// caller's error response when the input must be refused.
fn validate_prose_input(prose: &str) -> Result<(), ToolResult> {
    if prose.trim().is_empty() {
        return Err(ToolResult::err(ToolError::ValidationFailure {
            reason: "prose is empty".into(),
            valid_alternatives: vec![],
            hint: "Pass non-empty SME prose.".into(),
        }));
    }

    // Input-side prompt-injection sanitizer (grant v19 §C.0.1, E2
    // follow-up). `sanitize_for_session_prose` strips XML/HTML-like
    // tags, ASCII control characters, and whole-word internal
    // tool-name tokens. When any of those are present the sanitized
    // output differs from the input — that's our signal to refuse.
    // Legitimate SME prose (gene symbols, accessions, plain English)
    // round-trips unchanged. See
    // `tests/tool_boundary_adversarial.rs::all_75_adversarial_cases_refused`
    // for the five `redirection_injection` / `recursive` cases this
    // gate covers.
    let sanitized = crate::sme_text::sanitize_for_session_prose(prose);
    if sanitized != prose {
        return Err(ToolResult::err(ToolError::ValidationFailure {
            reason: "prose contains markup, control characters, or internal tool-name tokens"
                .into(),
            valid_alternatives: vec![],
            hint: "Provide plain prose describing the analysis. Don't include XML/HTML \
                   tags, ANSI escape sequences, control characters, or internal system \
                   identifiers."
                .into(),
        }));
    }
    Ok(())
}

/// Populate session taxonomy metadata from the matched archetype.
/// On a first-classify failure returns `Err(ToolResult)` carrying a
/// structured tool error; on a follow-up classify drift logs and keeps
/// the prior taxonomy.
fn apply_taxonomy_metadata(
    session: &mut Session,
    new_clf: &mut ClassificationResult,
    config_dir: &Path,
) -> Result<(), ToolResult> {
    match build_taxonomy_metadata_for_modality(&new_clf.modality, session.project_class, config_dir)
    {
        Ok(tax) => {
            new_clf.domain = tax.domain.clone();
            new_clf.workflow_description = tax.description.clone();
            session.taxonomy = Some(tax);
            Ok(())
        }
        Err(e) => {
            // Same two cases as the legacy loader: surface a structured
            // error on first classify, log + keep prior taxonomy on a
            // follow-up classify drift.
            if session.taxonomy.is_none() {
                tracing::error!(
                    session_id = %session.id,
                    modality = %new_clf.modality,
                    project_class = ?session.project_class,
                    err = %e,
                    "append_intake_prose: archetype metadata load failed; aborting tool"
                );
                Err(ToolResult::err(ToolError::InternalError {
                    reason: format!(
                        "archetype metadata load failed for modality={}, project_class={:?}: {}",
                        new_clf.modality, session.project_class, e
                    ),
                }))
            } else {
                tracing::warn!(
                    session_id = %session.id,
                    err = %e,
                    "append_intake_prose: archetype metadata reload failed; keeping prior taxonomy"
                );
                Ok(())
            }
        }
    }
}

/// Calibrated learning-to-defer: when the classifier is in a known tie
/// window, set `pending_disambiguation` so the next
/// `propose_quick_replies` call emits a targeted SME prompt instead of
/// guessing.
fn maybe_set_pending_disambiguation(
    session: &mut Session,
    new_clf: &ClassificationResult,
    config_dir: &Path,
) {
    let disambig_path = config_dir.join("classifier-disambiguation.yaml");
    if !disambig_path.exists() {
        return;
    }
    let reg = match ecaa_workflow_core::disambiguation::DisambiguationRegistry::load(&disambig_path)
    {
        Ok(reg) => reg,
        Err(e) => {
            tracing::warn!(
                session_id = %session.id,
                err = %e,
                "append_intake_prose: disambiguation registry load failed; skipping"
            );
            return;
        }
    };
    let mut candidates: Vec<&str> = new_clf
        .additional_modalities
        .iter()
        .map(|m| m.modality.as_str())
        .collect();
    // The primary classified modality is also a
    // candidate for `tied_modalities` triggers.
    candidates.push(new_clf.modality.as_str());
    let max_conf = new_clf.confidence;
    if let Some(pair) = reg.match_pair(
        None,
        &candidates,
        &new_clf.modality,
        &session.intake_prose,
        max_conf,
    ) {
        session.pending_disambiguation = Some(pair.id.clone());
        tracing::warn!(
            session_id = %session.id,
            pair = %pair.id,
            "disambiguation_triggered",
        );
    }
}

/// When the classifier populated `goal` and the archetype catalog has a
/// clear winner, snapshot the matched archetype onto the session.
/// Soft-skips on every error path so the legacy build keeps working.
/// Caller guarantees `session.archetype_snapshot.is_none()`.
fn maybe_pin_archetype_snapshot(
    session: &mut Session,
    new_clf: &ClassificationResult,
    config_dir: &Path,
) {
    let Some(goal) = new_clf.goal.as_ref() else {
        return;
    };
    let archetype_dir = config_dir.join("archetypes");
    let Ok(reg) =
        ecaa_workflow_core::archetype_registry::ArchetypeRegistry::load_cached(&archetype_dir)
    else {
        return;
    };
    let project_class_str = match session.project_class {
        ecaa_workflow_core::project_class::ProjectClass::Bioinformatics => "bioinformatics",
        ecaa_workflow_core::project_class::ProjectClass::ClinicalTrial => "clinical_trial",
        ecaa_workflow_core::project_class::ProjectClass::TimeSeriesForecast => {
            "time_series_forecast"
        }
    };
    // Pass classifier modality (modality_hint scorer; +2)
    // AND goal modifier `kind` (goal_kind_hint scorer; +2).
    // Two disambiguators stacked resolve both DE-shaped ties
    // (bulk_rnaseq_de vs long_read_rnaseq vs
    // metagenomics_taxonomic, modality breaks) and proteomics
    // DDA-vs-DIA ties (kind breaks).
    let target_kind = goal.modifiers.get("kind").map(|s| s.as_str());

    let cross_omics_winner =
        select_cross_omics_winner(new_clf, goal, &reg, project_class_str, target_kind);

    if let Some(winner) = cross_omics_winner {
        session.archetype_snapshot = Some(winner);
    } else if new_clf.additional_modalities.is_empty() {
        let matches = reg.find_match_with_modality_and_kind(
            &goal.edam_data,
            goal.edam_format.as_deref(),
            project_class_str,
            Some(new_clf.modality.as_str()),
            target_kind,
        );
        if let Some((winner, top_score)) = matches.first() {
            // 5%-tie-window per [DEC Q2.4] / S6.10. If a
            // runner-up is within 5%, we don't auto-snapshot
            // — the SME-facing tie-breaking card surfaces
            // via composer `CompositionError::
            // TieRequiresSmeDecision` when the composer is
            // invoked. Fast-path stays None so the legacy
            // build runs.
            let tie_threshold = (*top_score as f32 * 0.95).floor() as u32;
            let close_count = matches.iter().filter(|(_, s)| *s >= tie_threshold).count();
            if close_count == 1 {
                session.archetype_snapshot = Some((*winner).clone());
            }
        }
    }
}

/// When the classifier surfaced cross-omics intent
/// (additional_modalities non-empty), try `find_match_cross_omics`
/// first so the snapshot pins the cross-omics archetype rather than a
/// single-modality archetype that ignores half the SME's request.
/// Set-equality on `cross_omics_modalities` is the gate. When no exact
/// cross-omics archetype exists, leave the snapshot empty so the
/// composer can synthesize a generic multi-branch DAG instead.
fn select_cross_omics_winner(
    new_clf: &ClassificationResult,
    goal: &ecaa_workflow_core::goal_spec::GoalSpec,
    reg: &ecaa_workflow_core::archetype_registry::ArchetypeRegistry,
    project_class_str: &str,
    target_kind: Option<&str>,
) -> Option<ecaa_workflow_core::archetype::ArchetypeDefinition> {
    if new_clf.additional_modalities.is_empty() {
        return None;
    }
    let mut requested: Vec<&str> = vec![new_clf.modality.as_str()];
    requested.extend(
        new_clf
            .additional_modalities
            .iter()
            .map(|m| m.modality.as_str()),
    );
    let requested_set: std::collections::BTreeSet<&str> = requested.iter().copied().collect();
    let exact_by_modalities = reg
        .iter()
        .map(|(_, archetype)| archetype)
        .filter(|archetype| {
            archetype.project_class == project_class_str
                && !archetype.cross_omics_modalities.is_empty()
        })
        .filter(|archetype| {
            let have: std::collections::BTreeSet<&str> = archetype
                .cross_omics_modalities
                .iter()
                .map(String::as_str)
                .collect();
            have == requested_set
        })
        .min_by(|a, b| a.id.cmp(&b.id))
        .cloned();
    exact_by_modalities.or_else(|| {
        reg.find_match_cross_omics(
            &goal.edam_data,
            goal.edam_format.as_deref(),
            project_class_str,
            &requested,
            target_kind,
            ecaa_workflow_core::classify::is_n_way_intent(&new_clf.intake_text),
            &new_clf.intake_text,
        )
        .into_iter()
        .next()
        .map(|(arch, _score)| arch.clone())
    })
}

/// Populate `Session::workflow_intent` from the latest
/// classification result. The intent's `goal` is the cumulative
/// intake prose; `modality` / `project_class` mirror the
/// classifier's output; `available_data` populates from the
/// classifier's accession list (if any). The `legacy_intake_facts`
/// map carries `IntakeFacts`-flavored fields the typed schema
/// hasn't graduated yet; entries are retired as typed fields take over.
///
/// Idempotency: this function overwrites `session.workflow_intent`
/// every call. Subsequent `append_intake_prose` calls re-derive
/// from the cumulative state, so a session that gets a wrong
/// classification on turn 2 and a corrected one on turn 3 ends
/// with the corrected intent — there's no stale data persisted.
fn materialize_workflow_intent(
    session: &mut Session,
    classification: &ClassificationResult,
    _latest_prose_fragment: &str,
) {
    use ecaa_workflow_core::workflow_contracts::workflow_intent::WorkflowIntent;

    let mut legacy = std::collections::BTreeMap::new();
    if !classification.organisms.is_empty() {
        legacy.insert(
            "organisms".to_string(),
            serde_json::json!(classification.organisms),
        );
    }
    if !classification.data_sources.is_empty() {
        legacy.insert(
            "data_sources".to_string(),
            serde_json::json!(classification.data_sources),
        );
    }
    if let Some(g) = classification.goal.as_ref() {
        legacy.insert(
            "goal_edam_data".to_string(),
            serde_json::json!(g.edam_data.clone()),
        );
        if let Some(f) = &g.edam_format {
            legacy.insert("goal_edam_format".to_string(), serde_json::json!(f.clone()));
        }
    }

    let project_class = match session.project_class {
        ecaa_workflow_core::project_class::ProjectClass::Bioinformatics => {
            Some("research".to_string())
        }
        ecaa_workflow_core::project_class::ProjectClass::ClinicalTrial => {
            Some("clinical_trial".to_string())
        }
        ecaa_workflow_core::project_class::ProjectClass::TimeSeriesForecast => {
            Some("time_series_forecast".to_string())
        }
    };

    let intent = WorkflowIntent {
        id: session.id.to_string(),
        schema_version: ecaa_workflow_core::migration::current_workflow_intent_version(),
        goal: session.intake_prose.clone(),
        modality: Some(classification.modality.clone()),
        project_class,
        available_data: Vec::new(),
        desired_outputs: Vec::new(),
        constraints: Default::default(),
        uncertainties: Vec::new(),
        privacy: Default::default(),
        execution_preferences: Vec::new(),
        explanation_style: Default::default(),
        legacy_intake_facts: legacy,
        // Populated by the intake form for clinical projects;
        // default `None` until that form lands.
        sample_cohort: None,
    };

    // Schema validation: the IR's `Default` shape is already
    // round-trip-stable through serde, so a structurally valid
    // intent is one we can serialize. Failure surfaces as a
    // tracing warning rather than a tool error so the existing
    // intake path doesn't regress.
    match serde_json::to_value(&intent) {
        Ok(_) => {
            session.workflow_intent = Some(intent);
            // After materializing the intent, route any `LocalExtension`
            // semantic types appearing on `available_data` through the
            // cross-session aggregator so their usage counters update +
            // graduation checks fire. Today's intake path doesn't mint
            // LocalExtensions on `available_data` (it's empty until the
            // dataset profiler lands), but the routing is in place so it
            // activates the moment that source materializes one. The
            // helper is intentionally lenient on missing config / IO
            // errors: aggregator-side failures emit tracing warnings and
            // leave the session intact.
            route_local_extensions_through_aggregator(session);
        }
        Err(e) => {
            tracing::warn!(
                session_id = %session.id,
                err = ?e,
                "materialize_workflow_intent: serialization failed; leaving session.workflow_intent untouched"
            );
        }
    }
}

/// Route every `SemanticType::LocalExtension` minted on
/// `session.workflow_intent.available_data` through the cross-session
/// aggregator. Updates per-iri usage counters + flips the variant's
/// `maturity` field to `GraduationCandidate` when thresholds cross.
///
/// Lenient on missing config / IO errors: the aggregator is best-effort
/// telemetry; failures must not regress the intake path. Errors emit
/// tracing warnings and leave the session intact.
///
/// `succeeded=true` is passed when the session reached `Emitted`; this
/// path runs from `append_intake_prose` so the outcome isn't known yet.
/// We record an optimistic `true` — the harness-side emit pipeline will
/// correct on `Blocked` exits by re-recording the same iri with
/// `succeeded=false` when it transitions there.
pub(super) fn route_local_extensions_through_aggregator(session: &mut Session) {
    use ecaa_workflow_core::local_extension_graduation::GraduationConfig;
    use ecaa_workflow_core::workflow_contracts::semantic_type::{
        LocalExtensionMaturity, SemanticType,
    };

    let Some(intent) = session.workflow_intent.as_mut() else {
        return;
    };
    if intent.available_data.is_empty() {
        return;
    }

    let sessions_dir = sessions_dir_for_aggregator();
    let aggregator =
        crate::session::cross_session_aggregator::CrossSessionAggregator::new(sessions_dir.clone());
    // Best-effort thresholds load. Falls back to
    // GraduationThresholds::default() when no on-disk YAML is present.
    let thresholds = GraduationConfig::try_load_default(config_dir_for_graduation())
        .ok()
        .flatten()
        .map(|c| c.graduation)
        .unwrap_or_default();

    // Modality's primary ontologies. For now we project the classifier's
    // modality string verbatim into a single-element vec; when the
    // ontology-scope matrix is wired through here we'll use its
    // `primary_ontologies_for(modality)` accessor.
    let modality_primary_ontologies: Vec<String> = intent
        .modality
        .as_ref()
        .map(|m| vec![m.clone()])
        .unwrap_or_default();
    let modality_str = intent.modality.clone().unwrap_or_default();
    let session_id_str = session.id.to_string();

    for dp in intent.available_data.iter_mut() {
        if let SemanticType::LocalExtension {
            namespace,
            id,
            definition,
            proposed_parent_terms,
            maturity,
        } = &mut dp.semantic_type
        {
            let iri = format!("{namespace}:{id}");
            let label = if definition.is_empty() {
                id.clone()
            } else {
                definition.clone()
            };
            if let Err(e) = aggregator.record_usage(
                &iri,
                &label,
                definition,
                proposed_parent_terms,
                &modality_str,
                &session_id_str,
                true,
            ) {
                tracing::warn!(
                    session_id = %session.id,
                    iri = %iri,
                    err = ?e,
                    "route_local_extensions_through_aggregator: record_usage failed"
                );
                continue;
            }
            if let Some(candidacy) =
                aggregator.check_graduation(&iri, &thresholds, &modality_primary_ontologies)
            {
                *maturity = LocalExtensionMaturity::GraduationCandidate {
                    usage_count: candidacy.usage_count,
                    unique_sessions: candidacy.unique_sessions,
                    success_rate: candidacy.success_rate,
                    graduation_target_ontology: candidacy.graduation_target_ontology,
                    proposed_at: ecaa_workflow_core::time_helpers::now_rfc3339(),
                };
            }
        }
    }
}

/// Resolve the sessions directory the aggregator writes its registry
/// JSONL into. Honors `ECAA_CHAT_SESSIONS_DIR` so tests can point at a
/// tmpdir; falls back to `$HOME/.ecaa-workflow/sessions`; ultimate
/// fallback is `./.scripps-sessions` so the unit-test path doesn't
/// panic on a HOME-less environment.
fn sessions_dir_for_aggregator() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("ECAA_CHAT_SESSIONS_DIR") {
        return std::path::PathBuf::from(d);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(home).join(".ecaa-workflow/sessions");
    }
    std::path::PathBuf::from("./.scripps-sessions")
}

/// Resolve the config dir to load `local-extension-graduation.yaml`
/// from. Honors `ECAA_CONFIG_DIR`; falls back to `./config`.
fn config_dir_for_graduation() -> std::path::PathBuf {
    std::env::var("ECAA_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("config"))
}

fn should_replace_intake_scope(prose: &str) -> bool {
    let normalized = prose.to_lowercase().replace(['-', '_'], " ");
    normalized.contains("start fresh")
        || normalized.contains("separate follow on package")
        || (normalized.contains("this session")
            && (normalized.contains(" only")
                || normalized.contains("no proteomics")
                || normalized.contains("no single cell")
                || normalized.contains("no single nucleus")))
        || (normalized.contains("analysis only")
            && (normalized.contains("no proteomics")
                || normalized.contains("no single cell")
                || normalized.contains("no single nucleus")))
}

fn clear_incompatible_archetype_snapshot(
    session: &mut Session,
    classification: &ClassificationResult,
) {
    let Some(snapshot) = session.archetype_snapshot.as_ref() else {
        return;
    };

    let requested = requested_modalities(classification);
    let matches = if !snapshot.cross_omics_modalities.is_empty() {
        let have: std::collections::BTreeSet<&str> = snapshot
            .cross_omics_modalities
            .iter()
            .map(String::as_str)
            .collect();
        have == requested
    } else {
        requested.len() == 1
            && snapshot.modality_hint.as_deref() == Some(classification.modality.as_str())
    };

    if !matches {
        session.archetype_snapshot = None;
    }
}

fn requested_modalities(classification: &ClassificationResult) -> std::collections::BTreeSet<&str> {
    std::iter::once(classification.modality.as_str())
        .chain(
            classification
                .additional_modalities
                .iter()
                .map(|m| m.modality.as_str()),
        )
        .collect()
}

/// Build a lightweight `StageTaxonomy` metadata holder from the archetype
/// catalog. The legacy `config/stage-taxonomies/<modality>.yaml` files
/// have been removed; the archetype registry is the source of truth for
/// per-modality metadata (description, project_class). Returns an error
/// when no archetype matches.
fn build_taxonomy_metadata_for_modality(
    modality_id: &str,
    project_class: ecaa_workflow_core::project_class::ProjectClass,
    config_dir: &Path,
) -> anyhow::Result<StageTaxonomy> {
    use anyhow::{anyhow, Context};
    use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
    use ecaa_workflow_core::project_class::ProjectClass;
    let archetype_dir = config_dir.join("archetypes");
    let registry = ArchetypeRegistry::load_cached(&archetype_dir).with_context(|| {
        format!(
            "loading archetype registry from {}",
            archetype_dir.display()
        )
    })?;

    // Try the archetype that matches both modality and project class.
    // Fall back to first archetype whose modality_hint matches (single-
    // modality bio cases). Cross-omics archetypes are skipped here —
    // the conversation crate inspects `additional_modalities` separately
    // before composing.
    let class_str = match project_class {
        ProjectClass::Bioinformatics => "bioinformatics",
        ProjectClass::ClinicalTrial => "clinical_trial",
        ProjectClass::TimeSeriesForecast => "time_series_forecast",
    };
    let matched = registry.iter().find(|(_id, a)| {
        a.modality_hint.as_deref() == Some(modality_id)
            && a.project_class == class_str
            && a.cross_omics_modalities.is_empty()
    });
    // Fallback: any archetype matching project_class (covers
    // clinical_trial / time_series_forecast where modality is the
    // class id itself).
    let matched = matched.or_else(|| {
        registry
            .iter()
            .find(|(_id, a)| a.project_class == class_str && a.cross_omics_modalities.is_empty())
    });
    let Some((_id, archetype)) = matched else {
        return Err(anyhow!(
            "no archetype matches modality={} project_class={:?} in {}",
            modality_id,
            project_class,
            archetype_dir.display()
        ));
    };
    let preferred_container_str = archetype
        .preferred_container
        .as_ref()
        .map(|c| c.image.clone());
    Ok(StageTaxonomy {
        id: archetype.id.clone(),
        domain: "computational biology".into(),
        description: archetype.description.clone(),
        policies: None,
        claim_boundary: archetype.claim_boundary.clone(),
        validation_contract_ref: None,
        project_class: Some(project_class),
        preferred_container: preferred_container_str,
        runtime_baseline: archetype.runtime_baseline.clone(),
        // Stages are populated lazily by the composer-driven build path
        // (not the metadata holder). Leaving empty here matches the
        // intent: the v4 composer is the single source of truth for
        // stage shape.
        stages: vec![],
    })
}

/// Inspect freshly-appended prose for a known quick-reply chip id of
/// the pending disambiguation pair. When found, clear the latch by
/// calling `clear_disambiguation_on_selection`. No-op when no pair is
/// pending, the registry can't be loaded, or no chip id matches.
///
/// The match is whole-token (case-insensitive substring on a
/// word-boundary approximation): the chip ids in the calibration
/// table (`diablo`, `mofa`, `mr`, `gwas_coloc`, `chip_seq`,
/// `atac_seq`) are distinctive enough that substring is sufficient
/// for the corpus we calibrate against. A more rigorous tokenizer
/// can replace this without changing the public surface.
fn maybe_clear_pending_disambiguation_from_prose(
    session: &mut Session,
    prose: &str,
    config_dir: &Path,
) {
    let Some(pair_id) = session.pending_disambiguation.clone() else {
        return;
    };
    let disambig_path = config_dir.join("classifier-disambiguation.yaml");
    if !disambig_path.exists() {
        return;
    }
    let Ok(reg) = ecaa_workflow_core::disambiguation::DisambiguationRegistry::load(&disambig_path)
    else {
        return;
    };
    let Some(pair) = reg.pairs.iter().find(|p| p.id == pair_id) else {
        return;
    };
    let prose_lower = prose.to_lowercase();
    let matched_chip_id = pair
        .quick_replies
        .iter()
        .find(|q| prose_lower.contains(&q.id.to_lowercase()))
        .map(|q| q.id.clone());
    if let Some(reply_id) = matched_chip_id {
        clear_disambiguation_on_selection(session, &reply_id, config_dir);
    }
}

/// Clear `session.pending_disambiguation` after the SME has selected
/// a quick-reply chip from a disambiguation prompt. The `reply_id`
/// is the id of the selected quick-reply (e.g. `diablo`, `mr`,
/// `chip_seq`).
///
/// Called from the path that consumes quick-reply payloads — typically
/// the same site that forwards the SME's selection into
/// `append_intake_prose` or `set_intake_field`. Caller is responsible
/// for writing the selection into the appropriate slot
/// (`goal.modifiers[slot_name]` or `classification.modality`) before
/// invoking this helper.
///
/// No-op if `pending_disambiguation` is `None` or if the selected
/// `reply_id` does not match any of the pair's declared quick-reply
/// ids (defensive — the UI should never submit an unlisted id, but
/// this prevents stuck-state corruption).
pub(crate) fn clear_disambiguation_on_selection(
    session: &mut Session,
    reply_id: &str,
    config_dir: &Path,
) {
    let Some(pair_id) = session.pending_disambiguation.clone() else {
        return;
    };
    let disambig_path = config_dir.join("classifier-disambiguation.yaml");
    if !disambig_path.exists() {
        // Fail-safe: clear the latch so the SME isn't stuck if the
        // registry file is removed mid-session.
        session.pending_disambiguation = None;
        return;
    }
    let Ok(reg) = ecaa_workflow_core::disambiguation::DisambiguationRegistry::load(&disambig_path)
    else {
        session.pending_disambiguation = None;
        return;
    };
    let Some(pair) = reg.pairs.iter().find(|p| p.id == pair_id) else {
        // Pair was retired from the registry; clear so we don't loop.
        session.pending_disambiguation = None;
        return;
    };
    if pair.quick_replies.iter().any(|q| q.id == reply_id) {
        tracing::info!(
            session_id = %session.id,
            pair = %pair_id,
            reply_id = %reply_id,
            "disambiguation_cleared",
        );
        session.pending_disambiguation = None;
    }
}

/// Default `ECAA_INPUT_ROOTS` when unset. Matches the server's
/// `register_input_path` constant — keep in sync.
const DEFAULT_INPUT_ROOTS_FOR_HINTS: &str = "/home/${USER}/data";

/// Read the allowlist roots the path-hint extractor will validate
/// against. Mirrors `crates/server/src/chat_routes/inputs/list.rs`'s
/// `allowlisted_roots`; duplicated here so the conversation crate
/// doesn't take a runtime dependency on the server crate (which would
/// flip the dep direction). Both readers consult the same env var.
fn input_roots_for_hints(owner_user: &str) -> Vec<std::path::PathBuf> {
    let raw =
        std::env::var("ECAA_INPUT_ROOTS").unwrap_or_else(|_| DEFAULT_INPUT_ROOTS_FOR_HINTS.into());
    raw.split(':')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.replace("${USER}", owner_user))
        .map(std::path::PathBuf::from)
        .map(|p| p.canonicalize().unwrap_or(p))
        .collect()
}

/// Extract path hints from `prose_chunk` and merge them into
/// `session.pending_input_hints`. Idempotent: a hint already on the
/// session is not duplicated. Best-effort: any extraction error is
/// logged at WARN and swallowed so the broader intake flow can't be
/// derailed by a misconfigured allowlist or canonicalize failure.
fn extract_and_apply_path_hints(session: &mut crate::session::Session, prose_chunk: &str) {
    let roots = input_roots_for_hints(&session.owner_user);
    if roots.is_empty() {
        return;
    }
    let new_hints = crate::intake_path_hints::extract_path_hints(prose_chunk, &roots);
    if new_hints.is_empty() {
        return;
    }
    // Dedup against existing pending_input_hints AND against the
    // already-registered inputs (so a hint disappears once the SME
    // registers the same path via the UI). Comparing
    // canonical_root + file_relpath uniquely identifies a hint.
    let existing_hint_keys: std::collections::BTreeSet<String> = session
        .pending_input_hints
        .iter()
        .map(|h| {
            format!(
                "{}|{}",
                h.canonical_root,
                h.file_relpath.as_deref().unwrap_or("")
            )
        })
        .collect();
    let registered_root_paths: std::collections::BTreeSet<String> =
        session.inputs.iter().map(|i| i.root_path.clone()).collect();
    let mut added = 0usize;
    for hint in new_hints {
        let key = format!(
            "{}|{}",
            hint.canonical_root,
            hint.file_relpath.as_deref().unwrap_or("")
        );
        if existing_hint_keys.contains(&key) {
            continue;
        }
        // Drop hints whose canonical_root already corresponds to a
        // registered input — no point re-suggesting what's done.
        if registered_root_paths.contains(&hint.canonical_root) {
            continue;
        }
        session.pending_input_hints.push(hint);
        added += 1;
    }
    if added > 0 {
        tracing::info!(
            session_id = %session.id,
            added = added,
            total = session.pending_input_hints.len(),
            "extract_and_apply_path_hints: added path hints from intake prose"
        );
    }

    // ECAA_AUTO_REGISTER_PROSE_PATHS=1: promote every newly-added hint
    // straight onto session.inputs without waiting for the SME to
    // click "Register path". Useful for non-interactive fixture runs
    // and for development sessions where the SME shouldn't have to
    // bounce to the Inputs tab. Each hint's canonical_root is walked +
    // sha256-hashed inline (same shape the REST `register_input_path`
    // endpoint produces). Hints whose root fails to walk or hash are
    // left in `pending_input_hints` so the SME can retry via the UI.
    if std::env::var("ECAA_AUTO_REGISTER_PROSE_PATHS")
        .ok()
        .as_deref()
        == Some("1")
    {
        auto_register_pending_hints(session);
    }
}

/// Walk `root_path` and produce the per-file inventory the
/// `register_input_path` REST endpoint emits. Mirrors
/// `crates/server/src/chat_routes/inputs/list.rs::build_manifest`
/// inline (the server-side helper isn't exposed across crate
/// boundaries). Best-effort: walk + hash errors propagate so the
/// caller can leave the hint pending for the SME to retry.
fn build_manifest_for_auto_register(
    root: &std::path::Path,
) -> Result<Vec<crate::session::state::UserInputFile>, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut files = Vec::new();
    let mut total_bytes: u64 = 0;
    let max_file: u64 = 4 * 1024 * 1024 * 1024;
    let max_total: u64 = 32 * 1024 * 1024 * 1024;
    let max_files: usize = 50_000;
    let mut count = 0usize;
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|e| format!("walking {}: {e}", root.display()))?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|n: &std::ffi::OsStr| n.to_str())
            .map(|n: &str| n.starts_with('.'))
            .unwrap_or(false)
            && path != root
        {
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let meta = entry
            .metadata()
            .map_err(|e| format!("stat {}: {e}", path.display()))?;
        let size = meta.len();
        if size > max_file {
            return Err(format!(
                "file {} is {size} bytes, exceeds 4GiB per-file cap for auto-register",
                path.display()
            ));
        }
        total_bytes = total_bytes.saturating_add(size);
        if total_bytes > max_total {
            return Err(
                "total registration size exceeds 32GiB cap for auto-register; \
                 stop at a more specific subdirectory"
                    .to_string(),
            );
        }
        count += 1;
        if count > max_files {
            return Err(
                "registration would include more than 50000 files for auto-register".to_string(),
            );
        }
        let relpath = path
            .strip_prefix(root)
            .map_err(|e| format!("strip_prefix {}: {e}", path.display()))?
            .to_string_lossy()
            .into_owned();
        let mut hasher = Sha256::new();
        let mut f =
            std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
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
        let sha = hex::encode(hasher.finalize());
        files.push(crate::session::state::UserInputFile {
            relpath,
            size_bytes: size,
            sha256: sha,
        });
    }
    Ok(files)
}

/// Promote every entry currently in `session.pending_input_hints`
/// into `session.inputs` after walking + sha256-hashing each root.
/// Removes the hint on success so the LLM (via `get_session_state`)
/// no longer sees it. On hash failure the hint stays put — the SME
/// can recover via the Inputs tab.
fn auto_register_pending_hints(session: &mut crate::session::Session) {
    use chrono::Utc;
    use uuid::Uuid;
    let mut retained: Vec<crate::intake_path_hints::InputPathHint> = Vec::new();
    let already_registered: std::collections::BTreeSet<String> =
        session.inputs.iter().map(|i| i.root_path.clone()).collect();
    let pending = std::mem::take(&mut session.pending_input_hints);
    for hint in pending {
        if already_registered.contains(&hint.canonical_root) {
            // Skip — already registered (idempotent retry).
            continue;
        }
        let root = std::path::PathBuf::from(&hint.canonical_root);
        let files = match build_manifest_for_auto_register(&root) {
            Ok(f) => f,
            Err(err) => {
                tracing::warn!(
                    session_id = %session.id,
                    root = %hint.canonical_root,
                    err = %err,
                    "auto_register_pending_hints: walk/hash failed, leaving hint pending"
                );
                retained.push(hint);
                continue;
            }
        };
        let label = root
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| hint.canonical_root.clone());
        let input_id = Uuid::new_v4().as_simple().to_string()[..16].to_string();
        let registration = crate::session::state::UserInput {
            input_id,
            label,
            kind: crate::session::state::UserInputKind::LocalPath,
            root_path: hint.canonical_root.clone(),
            files,
            registered_at: Utc::now(),
            registered_by: session.owner_user.clone(),
        };
        tracing::info!(
            session_id = %session.id,
            input_id = %registration.input_id,
            root = %registration.root_path,
            n_files = registration.files.len(),
            "auto_register_pending_hints: promoted hint to session.inputs"
        );
        session.inputs.push(registration);
    }
    session.pending_input_hints = retained;
}
