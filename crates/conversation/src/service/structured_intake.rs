//! Deterministic mutations that are specific to the structured-intake path.
//!
//! The regular chat path routes SME-named methods through a UI signal and a
//! subsequent `set_intake_method` tool call. Structured intake has no
//! subsequent LLM turn, so exact method keywords recognized from the SME's
//! submitted prose must be promoted here before the confirmation card is
//! raised.

use crate::session::Session;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::StructuredIntakeValidation;

/// Promote classifier-recognized method names that address discovery stages
/// present in the compiled DAG.
///
/// A method whose stage was pruned by the supplied-product entry point is
/// treated as provenance about an upstream product, not as an instruction to
/// reinsert that stage. Two distinct methods for the same active stage are
/// ambiguous and therefore rejected instead of choosing one by catalog order.
pub(super) fn apply_classifier_named_methods(
    session: &mut Session,
    config_dir: &Path,
) -> anyhow::Result<()> {
    let methods = session
        .classification
        .as_ref()
        .map(|classification| classification.methods_specified.clone())
        .unwrap_or_default();
    if methods.is_empty() {
        return Ok(());
    }

    let active_axes = active_method_axes(session);

    // stage -> normalized method id -> first classifier display string.
    let mut by_stage: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for method in methods {
        let declared_stage = method
            .stage
            .strip_prefix("discover_")
            .unwrap_or(&method.stage)
            .to_string();
        let normalized = ecaa_workflow_core::preferred_methods::normalize_method_id(&method.method);
        if normalized.is_empty() {
            continue;
        }
        let stage = if active_axes.contains_key(&declared_stage) {
            declared_stage
        } else {
            let compatible_axes = active_axes
                .iter()
                .filter(|(_, options)| {
                    options
                        .iter()
                        .any(|option| method_ids_compatible(&normalized, option))
                })
                .map(|(axis, _)| axis.clone())
                .collect::<Vec<_>>();
            match compatible_axes.as_slice() {
                [axis] => axis.clone(),
                [] => {
                    // The named method belongs to a producer removed by the
                    // registered starting product. Retain it in the intake
                    // prose as provenance, but do not reinsert that producer.
                    continue;
                }
                many => {
                    return Err(anyhow::Error::new(StructuredIntakeValidation(format!(
                        "structured intake method `{}` matches multiple active steps: {}; name the target step",
                        method.method,
                        many.join(", ")
                    ))));
                }
            }
        };
        by_stage
            .entry(stage)
            .or_default()
            .entry(normalized)
            .or_insert(method.method);
    }

    for (stage, candidates) in &by_stage {
        if candidates.len() > 1 {
            return Err(anyhow::Error::new(StructuredIntakeValidation(format!(
                "structured intake names multiple methods for `{stage}`: {}; name one method for this step",
                candidates.values().cloned().collect::<Vec<_>>().join(", ")
            ))));
        }
    }

    if by_stage.is_empty() {
        return Ok(());
    }

    for (stage, candidates) in by_stage {
        let method = candidates
            .into_values()
            .next()
            .expect("non-empty method group checked above");
        let rationale = explicit_method_selection_rationale(
            stage.as_str(),
            method.as_str(),
            "structured intake",
        );
        session.sme_method_signals.named.insert(stage.clone(), true);
        session
            .intake_methods
            .set(&stage, Some(method.clone()), None);
        session.record_decision(
            ecaa_workflow_core::decision_log::DecisionType::SetIntakeMethod {
                stage,
                method_prose: method,
            },
            ecaa_workflow_core::decision_log::DecisionActor::Sme,
            Some(rationale),
        );
    }

    crate::tools::rebuild_dag(session, config_dir)
        .map_err(|error| anyhow::anyhow!(error.short_reason()))
}

/// Build a truthful, audit-sufficient rationale for a method the SME selected
/// explicitly. The method name remains verbatim in `method_prose`; this
/// sentence records why the value is authoritative without inventing a
/// scientific justification the SME did not provide.
pub(crate) fn explicit_method_selection_rationale(
    stage: &str,
    method: &str,
    selection_surface: &str,
) -> String {
    format!(
        "The SME explicitly selected `{method}` for the `{stage}` step through {selection_surface}."
    )
}

/// Active method axes and their candidate ids, read from the compiled workflow
/// rather than inferred from a modality or archetype name.
fn active_method_axes(session: &Session) -> BTreeMap<String, BTreeSet<String>> {
    let mut axes = BTreeMap::new();
    if let Some(workflow) = session.workflow_dag.as_ref() {
        for node in &workflow.nodes {
            if !node.id.starts_with("discover_") {
                continue;
            }
            let axis = node
                .attributes
                .get("method_axis")
                .and_then(serde_json::Value::as_str)
                .or_else(|| node.id.strip_prefix("discover_"))
                .unwrap_or(node.id.as_str())
                .to_string();
            let options = node
                .attributes
                .get("method_options")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(ecaa_workflow_core::preferred_methods::normalize_method_id)
                .filter(|option| !option.is_empty())
                .collect::<BTreeSet<_>>();
            axes.entry(axis).or_insert(options);
        }
    }
    if axes.is_empty() {
        if let Some(dag) = session.dag.as_ref() {
            for task_id in dag.tasks.keys() {
                if let Some(axis) = task_id.as_str().strip_prefix("discover_") {
                    axes.entry(axis.to_string()).or_default();
                }
            }
        }
    }
    axes
}

/// Treat a catalog family name as compatible with one of its qualified
/// candidates (for example, a base executable and a workflow-specific
/// subcommand). Separators are explicit so unrelated prefix collisions do not
/// match.
fn method_ids_compatible(requested: &str, candidate: &str) -> bool {
    requested == candidate
        || candidate
            .strip_prefix(requested)
            .is_some_and(|suffix| suffix.starts_with('_'))
        || requested
            .strip_prefix(candidate)
            .is_some_and(|suffix| suffix.starts_with('_'))
}
