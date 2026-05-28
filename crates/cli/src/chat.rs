use anyhow::Result;
use colored::Colorize;
use rustyline::DefaultEditor;
use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::builder::{
    build_dag_from_composition, build_dag_from_workflow_dag, IntakeMethods, IntakeResolution,
};
use ecaa_workflow_core::classify::Classifier;
use ecaa_workflow_core::composer::compose_with_version_and_modalities_full;
use ecaa_workflow_core::dag::{dag_to_dot, Task, TaskKind, TaskState, DAG};
use ecaa_workflow_core::emitter::{emit_package, EmitConfig};
use ecaa_workflow_core::goal_spec::GoalSpec;
use ecaa_workflow_core::taxonomy::StageTaxonomy;
use std::collections::BTreeMap;
use std::path::Path;

/// Run the interactive intake chat loop.
pub(crate) fn run_chat(config_dir: &str, output: &str) -> Result<()> {
    let config_path = Path::new(config_dir);
    let keywords_path = config_path.join("modality-keywords.yaml");
    let policies_dir = config_path.join("downstream-policy");

    let classifier = Classifier::load(&keywords_path)
        .map_err(|e| anyhow::anyhow!("Cannot load classifier config: {}", e))?;

    println!("{}", "Scripps Workflow Compiler".bold().cyan());
    println!(
        "{}",
        "Describe your workflow. Commands: /dag /ready /resolve /show /quit".dimmed()
    );
    println!();

    let mut rl = DefaultEditor::new()?;
    let mut conversation = String::new();
    let mut current_dag: Option<DAG> = None;
    let mut current_taxonomy: Option<StageTaxonomy> = None;
    // Composed atoms surface so /ready can build per-atom runtime prereqs
    // (`policies/atom-prereqs/<atom_id>.json`) at emit time. Populated
    // by `process_classification` from `output.composition.atoms`; stays
    // None until the first successful classification.
    let mut current_atoms: Option<Vec<ecaa_workflow_core::atom::AtomDefinition>> = None;
    let mut intake_methods: IntakeMethods = IntakeMethods::new();
    let mut awaiting_confirm = false;
    let mut pending_classification: Option<ecaa_workflow_core::classify::ClassificationResult> =
        None;
    // Track last classified modality so we only re-print the banner
    // on modality change, not on every line of added prose (debounce).
    let mut last_modality: Option<String> = None;

    loop {
        let readline = rl.readline("> ");
        let line = match readline {
            Ok(l) => l.trim().to_string(),
            Err(_) => break,
        };
        if line.is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(&line);

        // Commands
        match line.as_str() {
            "/quit" | "/exit" => break,
            "/dag" => {
                if let Some(ref dag) = current_dag {
                    println!("{}", dag_to_dot(dag));
                    print_dag_summary(dag);
                } else {
                    println!(
                        "{}",
                        "No workflow defined yet. Describe your workflow first.".yellow()
                    );
                }
                continue;
            }
            "/ready" | "/emit" => {
                if let Some(ref dag) = current_dag {
                    let clf = build_classification_result(
                        &conversation,
                        &classifier,
                        current_taxonomy.as_ref(),
                        &intake_methods,
                    );
                    let out = Path::new(output);
                    let compute_profiles_dir =
                        policies_dir.parent().map(|p| p.join("compute-profiles"));
                    let compute_profiles_opt =
                        compute_profiles_dir.as_deref().filter(|p| p.exists());
                    let intake_facts =
                        ecaa_workflow_core::intake_facts::IntakeFacts::from_classification(&clf);
                    let runtime_prereqs = current_taxonomy.as_ref().map(|t| {
                        ecaa_workflow_core::runtime_prereqs::aggregate_taxonomy(t, &[])
                    });
                    // Per-atom runtime-prereqs map. The harness reads
                    // `policies/atom-prereqs/<atom_id>.json` per task
                    // under SWFC_PER_TASK_IMAGES (default on); a missing
                    // map causes silent fallback to host mode. Sibling
                    // wiring: `run_intake` in `crates/cli/src/main.rs`
                    // (lines :462-487 and :736-757).
                    let per_atom_prereqs: Option<
                        std::collections::BTreeMap<
                            String,
                            ecaa_workflow_core::runtime_prereqs::RuntimePrereqs,
                        >,
                    > = current_atoms.as_ref().map(|atoms| {
                        atoms
                            .iter()
                            .map(|a| (a.id.clone(), a.runtime_packages.clone()))
                            .collect()
                    });
                    emit_package(&EmitConfig {
                        output_dir: out,
                        dag,
                        classification: &clf,
                        policies_dir: &policies_dir,
                        policy_allowlist: current_taxonomy
                            .as_ref()
                            .and_then(|t| t.policies.as_deref()),
                        claim_boundary: current_taxonomy
                            .as_ref()
                            .and_then(|t| t.claim_boundary.as_deref()),
                        compute_profiles_dir: compute_profiles_opt,
                        intake_facts: Some(&intake_facts),
                        amend_from: None,
                        amend_context: None,
                        validation_contract_ref: None,
                        preferred_container: current_taxonomy
                            .as_ref()
                            .and_then(|t| t.preferred_container.as_deref()),
                        runtime_prereqs: runtime_prereqs.as_ref(),
                        // Smoke contract: every chat-driven emission populates this
                        // map from the composer's matched atoms; absence here means
                        // current_atoms is None (no successful classification yet)
                        // — emit is unreachable in that state because current_dag
                        // is also None and the /ready handler returns above.
                        per_atom_runtime_prereqs: per_atom_prereqs.as_ref(),
                    })?;
                    println!(
                        "\n{} Package emitted → {}",
                        "✓".green().bold(),
                        output.cyan()
                    );
                    println!(
                        "  Run: ecaa-workflow-harness --package {} --agent claude",
                        output
                    );
                } else {
                    println!("{}", "No workflow defined yet.".yellow());
                }
                continue;
            }
            _ => {}
        }

        // /show <task_id> — dump the resolved state of a single task
        if let Some(task_id) = line.strip_prefix("/show ") {
            let task_id = task_id.trim();
            if let Some(ref dag) = current_dag {
                if let Some(task) = dag.tasks.get(task_id) {
                    print_task_detail(task_id, task);
                } else {
                    println!("{}", format!("No task '{}'", task_id).yellow());
                }
            } else {
                println!("{}", "No workflow defined yet.".yellow());
            }
            continue;
        }

        // /resolve <stage> [key=value...] [-- <prose method>]
        //
        // Accepted forms:
        // /resolve alignment STAR
        // /resolve batch_correction scVI conditioned on dataset_id
        // /resolve batch_correction batch_correction_required=true -- scVI...
        // /resolve preprocessing include_introns=true flavor=cellranger -- Cell Ranger 7.x...
        //
        // Values parsed from key=value pairs are kept as JSON scalars (true/false/number/string).
        if let Some(rest) = line.strip_prefix("/resolve ") {
            if awaiting_confirm {
                println!(
                    "{}",
                    "Cannot /resolve yet — classification is pending confirmation. Type 'yes' to accept the classification above, or keep describing your workflow to refine it.".yellow()
                );
                continue;
            }
            let parts: Vec<&str> = rest.splitn(2, ' ').collect();
            if parts.len() != 2 {
                println!(
                    "{}",
                    "Usage: /resolve <stage_id> <method prose> [key=value ...]".yellow()
                );
                continue;
            }
            let stage = parts[0].trim().to_string();
            let rest = parts[1].trim().to_string();
            let resolution = parse_resolution_body(&rest);
            let method_display = if resolution.method.is_empty() {
                "<fields only>".into()
            } else {
                resolution.method.clone()
            };
            intake_methods.insert(stage.clone(), resolution);

            println!(
                "  {} discover_{} — resolved ({}, SME)",
                "✓".green(),
                stage,
                truncate_echo(&method_display, 72)
            );

            // Phase B4 — the legacy `/resolve` handler called
            // `build_dag_from_taxonomy` to rebuild the DAG with SME-
            // supplied method resolutions. With the legacy entry point
            // removed, /resolve now only records the resolution on
            // `intake_methods`; the next intake-prose line triggers a
            // fresh v4 composition. The deterministic CLI chat REPL
            // is a developer surface (the production SME UX is the
            // web chat); the simplification is acceptable here.
            let _ = (&stage, &current_taxonomy);
            continue;
        }

        // Bare confirmation tokens are reserved — ignore them outside a confirmation
        // context instead of letting them pollute the intake conversation and trigger
        // a duplicate classification pass.
        let lower = line.to_lowercase();
        if !awaiting_confirm && matches!(lower.as_str(), "yes" | "y" | "no" | "n") {
            println!(
                "{}",
                "(nothing awaiting confirmation — describe your workflow)".dimmed()
            );
            continue;
        }

        // Confirmation response
        if awaiting_confirm {
            if line.to_lowercase() == "yes" || line.to_lowercase() == "y" {
                awaiting_confirm = false;
                if let Some(clf) = pending_classification.take() {
                    last_modality = Some(clf.modality.clone());
                    process_classification(
                        &clf,
                        &intake_methods,
                        config_path,
                        &mut current_dag,
                        &mut current_taxonomy,
                        &mut current_atoms,
                    )?;
                }
            } else {
                // SME continued describing instead of typing "yes". Append and
                // re-classify against the expanded conversation — if the added
                // prose pushes confidence past the 0.5 gate, auto-exit confirm
                // mode without requiring explicit acknowledgment. Otherwise
                // update the pending snapshot and reprint the prompt so the
                // SME sees the revised confidence rather than a silent loop.
                conversation.push_str(&format!("\n{}", line));
                let new_clf = classifier.classify(&conversation);
                if new_clf.confidence
                    >= ecaa_workflow_core::classify_gates::CONFIDENCE_GATE_MEDIUM
                    && !new_clf.modality.is_empty()
                    && new_clf.modality != "generic_omics"
                {
                    awaiting_confirm = false;
                    pending_classification = None;
                    last_modality = Some(new_clf.modality.clone());
                    process_classification(
                        &new_clf,
                        &intake_methods,
                        config_path,
                        &mut current_dag,
                        &mut current_taxonomy,
                        &mut current_atoms,
                    )?;
                } else {
                    println!(
                        "\n  {} {} [confidence: {} — {:.0}% keyword match]",
                        "Classified:".dimmed(),
                        new_clf.modality.cyan(),
                        new_clf.confidence_label.yellow(),
                        new_clf.confidence * 100.0
                    );
                    println!(
                        "  {} Type 'yes' to proceed, or keep describing your workflow.",
                        "Is this correct?".yellow()
                    );
                    pending_classification = Some(new_clf);
                }
            }
            continue;
        }

        // Accumulate text and classify
        conversation.push_str(&format!("\n{}", line));
        let clf_result = classifier.classify(&conversation);

        // Debounce: if the modality hasn't changed since the last line and we
        // already have a DAG, skip the noisy classification banner. The SME can
        // keep typing prose without being spammed; they'll see the next update
        // on modality change or when they explicitly call /dag, /resolve, /emit.
        let modality_changed = last_modality.as_deref() != Some(clf_result.modality.as_str());
        if !modality_changed && current_dag.is_some() {
            continue;
        }

        // Confidence gating: < 0.5 requires SME confirmation
        if clf_result.confidence < ecaa_workflow_core::classify_gates::CONFIDENCE_GATE_MEDIUM
            && !clf_result.modality.is_empty()
            && clf_result.modality != "generic_omics"
        {
            println!(
                "\n  {} {} [confidence: {} — {:.0}% keyword match]",
                "Classified:".dimmed(),
                clf_result.modality.cyan(),
                clf_result.confidence_label.yellow(),
                clf_result.confidence * 100.0
            );
            println!(
                "  {} Type 'yes' to proceed, or describe your workflow further.",
                "Is this correct?".yellow()
            );
            awaiting_confirm = true;
            pending_classification = Some(clf_result);
            continue;
        }

        last_modality = Some(clf_result.modality.clone());
        process_classification(
            &clf_result,
            &intake_methods,
            config_path,
            &mut current_dag,
            &mut current_taxonomy,
            &mut current_atoms,
        )?;
    }

    Ok(())
}

fn process_classification(
    clf: &ecaa_workflow_core::classify::ClassificationResult,
    _intake_methods: &IntakeMethods,
    config_path: &Path,
    current_dag: &mut Option<DAG>,
    current_taxonomy: &mut Option<StageTaxonomy>,
    current_atoms: &mut Option<Vec<ecaa_workflow_core::atom::AtomDefinition>>,
) -> Result<()> {
    // Phase B4 — compose through the v4 path.
    let atoms = AtomRegistry::load_from_dir(&config_path.join("stage-atoms"))?;
    let archetypes = ArchetypeRegistry::load_from_dir(&config_path.join("archetypes"))?;
    let goal = clf.goal.clone().unwrap_or_else(|| GoalSpec {
        edam_data: "data:9999".into(),
        edam_format: None,
        modifiers: Default::default(),
        source_prose: Some("CLI chat bare-modality fallback".into()),
        confidence: 0.0,
    });
    let modalities: Vec<&str> = std::iter::once(clf.modality.as_str())
        .chain(
            clf.additional_modalities
                .iter()
                .map(|m| m.modality.as_str()),
        )
        .collect();
    let output = compose_with_version_and_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        4,
        &modalities,
        None,
        // R1/R2 closure — deterministic CLI `chat` shell; no
        // conversation crate involvement, so the opaque sink is None.
        None,
        None,
    )
    .map_err(|e| anyhow::anyhow!("v4 composer dispatch failed: {:?}", e))?;
    let dag = if let Some(workflow_dag) = output.workflow_dag.as_ref() {
        build_dag_from_workflow_dag(workflow_dag, "workflow")?
    } else {
        build_dag_from_composition(&output.composition, "workflow", &BTreeMap::new(), &[])?
    };

    let archetype_id = output
        .composition
        .matched_archetype
        .clone()
        .unwrap_or_else(|| clf.modality.clone());
    let archetype_obj = archetypes
        .iter()
        .find(|(id, _)| id.as_str() == archetype_id.as_str());
    let taxonomy = StageTaxonomy {
        id: archetype_id.clone(),
        domain: "computational biology".into(),
        description: archetype_obj
            .map(|(_, a)| a.description.clone())
            .unwrap_or_else(|| clf.workflow_description.clone()),
        ..Default::default()
    };

    let (_completed, ready, blocked, pending) = dag.progress();
    let confidence_str = format!("[confidence: {}]", clf.confidence_label);
    let confidence_colored = match clf.confidence_label.as_str() {
        "high" => confidence_str.green(),
        "medium" => confidence_str.yellow(),
        _ => confidence_str.red(),
    };

    println!(
        "\n  {} {} {}",
        "Classified:".dimmed(),
        clf.modality.cyan().bold(),
        confidence_colored
    );
    if !clf.edam_topic.is_empty() {
        println!(
            "  {} · {}",
            clf.edam_topic.dimmed(),
            clf.edam_operation.dimmed()
        );
    }

    println!(
        "\n  Draft workflow ({} tasks: {} ready, {} pending, {} blocked, {} review)",
        dag.tasks.len().to_string().bold(),
        ready.to_string().blue(),
        pending.to_string().white(),
        blocked.to_string().red(),
        dag.tasks
            .values()
            .filter(|t| t.kind == TaskKind::Review)
            .count()
    );

    // List unresolved discovery tasks
    let unresolved: Vec<_> = dag
        .tasks
        .iter()
        .filter(|(_, t)| {
            matches!(t.kind, TaskKind::Discovery(_))
                && !matches!(t.state, TaskState::Completed { .. })
        })
        .collect();

    if !unresolved.is_empty() {
        println!(
            "\n  {} discovery task(s) — specify methods or let agent decide:",
            unresolved.len().to_string().yellow()
        );
        for (id, task) in &unresolved {
            println!("  · {} — {}", id.as_str().cyan(), task.description.dimmed());
        }
        println!();
    }

    // Capture the composer's matched atoms so /ready can build the
    // per-atom runtime-prereqs map for emit. Mirrors `run_intake`'s
    // `composed_atoms` extraction in `crates/cli/src/main.rs:447-468`.
    let composed_atoms: Vec<ecaa_workflow_core::atom::AtomDefinition> = output
        .composition
        .atoms
        .iter()
        .map(|ca| ca.atom.clone())
        .collect();

    print_state_delta(&None, &dag);
    *current_dag = Some(dag);
    *current_taxonomy = Some(taxonomy);
    *current_atoms = Some(composed_atoms);
    Ok(())
}

/// Pretty-print a single task's resolved state for /show.
fn print_task_detail(task_id: &str, task: &Task) {
    let state_str = match &task.state {
        TaskState::Pending => "pending".white(),
        TaskState::Ready => "ready".blue().bold(),
        TaskState::Running { .. } => "running".yellow(),
        TaskState::Completed { .. } => "completed".green(),
        TaskState::Failed { .. } => "failed".red(),
        TaskState::Blocked { .. } => "blocked".red().bold(),
    };
    println!("\n  {} {}", "task".dimmed(), task_id.cyan().bold());
    println!("  {} {}", "state".dimmed(), state_str);
    println!("  {} {:?}", "kind".dimmed(), task.kind);
    println!("  {} {:?}", "assignee".dimmed(), task.assignee);
    if !task.depends_on.is_empty() {
        println!("  {} {}", "depends_on".dimmed(), task.depends_on.join(", "));
    }
    if !task.description.is_empty() {
        println!("  {} {}", "description".dimmed(), task.description);
    }
    if let Some(spec) = &task.spec {
        if let Ok(pretty) = serde_json::to_string_pretty(spec) {
            println!("  {}", "spec:".dimmed());
            for line in pretty.lines() {
                println!("    {}", line);
            }
        }
    }
    if let TaskState::Completed { result } = &task.state {
        if let Ok(pretty) = serde_json::to_string_pretty(result) {
            println!("  {}", "result:".dimmed());
            for line in pretty.lines() {
                println!("    {}", line);
            }
        }
    }
    println!();
}

fn print_dag_summary(dag: &DAG) {
    let (completed, ready, blocked, pending) = dag.progress();
    println!(
        "\n  {} tasks: {} completed, {} ready, {} pending, {} blocked\n",
        dag.tasks.len(),
        completed.to_string().green(),
        ready.to_string().blue(),
        pending.to_string().white(),
        blocked.to_string().red()
    );
    for (id, task) in &dag.tasks {
        let state = match &task.state {
            TaskState::Pending => "pending  ".white(),
            TaskState::Ready => "ready    ".blue().bold(),
            TaskState::Running { .. } => "running  ".yellow(),
            TaskState::Completed { .. } => "completed".green(),
            TaskState::Failed { .. } => "failed   ".red(),
            TaskState::Blocked { .. } => "BLOCKED  ".red().bold(),
        };
        println!("  {} {}", state, id.as_str().cyan());
    }
    println!();
}

fn build_classification_result(
    text: &str,
    classifier: &Classifier,
    taxonomy: Option<&StageTaxonomy>,
    _methods: &IntakeMethods,
) -> ecaa_workflow_core::classify::ClassificationResult {
    let mut result = classifier.classify(text);
    if let Some(t) = taxonomy {
        result.domain = t.domain.clone();
        result.workflow_description = t.description.clone();
    }
    result
}

// ── /resolve parsing helpers ──────────────────────────────────────────────────

/// Parse the body of a /resolve command into an IntakeResolution.
///
/// Grammar:
/// Body::= (key_eq_value SPACE)* [PROSE]
/// where PROSE is everything after an optional " -- " separator, or the
/// tail of the body starting at the first non-k=v token if no separator
/// is present. k=v pairs are only collected at the head of the body; once
/// a non-k=v token is seen, the remainder is prose and further k=v-looking
/// tokens inside it (e.g. "flavor=seurat_v3" embedded in a method
/// description) are preserved verbatim instead of being hoisted out.
///
/// A valid k=v key must start with a letter or underscore and contain only
/// `[A-Za-z0-9_]`. This excludes accidental matches like "(n=1" or
/// "(n_neighbors=15,".
///
/// Examples:
/// "STAR" → {method: "STAR", fields: {}}
/// "scVI conditioned on dataset_id" → {method: "scVI...", fields: {}}
/// "required=true -- scVI" → {method: "scVI", fields: {required: true}}
/// "required=true flavor=scvi" → {method: "", fields: {required: true, flavor: "scvi"}}
/// "required=true scVI latent" → {method: "scVI latent", fields: {required: true}}
/// "scanpy with flavor=seurat_v3" → {method: "scanpy with flavor=seurat_v3", fields: {}}
fn parse_resolution_body(body: &str) -> IntakeResolution {
    // Split on " -- " first so k=v pairs can precede prose explicitly
    let (kv_part, explicit_prose) = if let Some(idx) = body.find(" -- ") {
        (&body[..idx], Some(body[idx + 4..].trim().to_string()))
    } else {
        (body, None)
    };

    let mut fields = std::collections::BTreeMap::new();
    let mut implicit_prose_start: Option<usize> = None;
    let mut cursor = 0usize;

    loop {
        // Skip leading whitespace
        let tok_rel = kv_part[cursor..].find(|c: char| !c.is_whitespace());
        let Some(rel) = tok_rel else { break };
        let tok_start = cursor + rel;
        let tok_end = kv_part[tok_start..]
            .find(char::is_whitespace)
            .map(|i| tok_start + i)
            .unwrap_or(kv_part.len());
        let token = &kv_part[tok_start..tok_end];

        if let Some((key, raw)) = parse_kv_token(token) {
            fields.insert(key.to_string(), parse_field_value(raw));
            cursor = tok_end;
            continue;
        }

        // First non-k=v token: everything from here on is prose.
        implicit_prose_start = Some(tok_start);
        break;
    }

    let method = match (explicit_prose, implicit_prose_start) {
        (Some(prose), _) => prose,
        (None, Some(start)) => kv_part[start..].trim().to_string(),
        (None, None) => String::new(),
    };

    IntakeResolution { method, fields }
}

/// Split a token into (key, value) if it looks like a valid k=v pair.
/// Valid keys start with a letter or underscore and contain only alphanumerics
/// and underscores — this rejects accidental matches like "(n=1" or "n,=v".
fn parse_kv_token(token: &str) -> Option<(&str, &str)> {
    let eq = token.find('=')?;
    let key = &token[..eq];
    let raw = &token[eq + 1..];
    if key.is_empty() || raw.is_empty() {
        return None;
    }
    let mut chars = key.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some((key, raw))
}

/// Parse a right-hand-side value into a JSON scalar.
/// Tries bool, then i64, then f64, then falls back to unquoted string.
fn parse_field_value(raw: &str) -> serde_json::Value {
    let s = raw.trim_matches('"');
    match s {
        "true" => serde_json::Value::Bool(true),
        "false" => serde_json::Value::Bool(false),
        _ => {
            if let Ok(i) = s.parse::<i64>() {
                serde_json::json!(i)
            } else if let Ok(f) = s.parse::<f64>() {
                serde_json::json!(f)
            } else {
                serde_json::Value::String(s.to_string())
            }
        }
    }
}

// Phase B4 — `warn_on_missing_required_fields` removed. It scanned
// `taxonomy.stages[*].condition` for `discover_<stage>.result.<field>`
// references to warn when an SME `/resolve` omitted a field that gates
// a downstream conditional task. The v4 composer doesn't surface
// taxonomy-level condition expressions through the same path; the
// production SME UX (web chat with `set_intake_field`) handles this
// via the conversation tools instead.

/// Truncate echoed text for the confirmation line so long method prose doesn't
/// visually overflow the terminal.
fn truncate_echo(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", head)
    }
}

/// Report newly-ready and newly-completed tasks after a DAG rebuild.
/// Replaces the older print_dag_delta which only reported newly-added tasks —
/// after a /resolve the interesting signal is tasks that transitioned state.
fn print_state_delta(before: &Option<DAG>, after: &DAG) {
    let Some(before_dag) = before.as_ref() else {
        // First build — nothing to compare against
        return;
    };
    let mut newly_ready = Vec::new();
    let mut newly_completed = Vec::new();
    for (id, after_task) in &after.tasks {
        let before_state = before_dag.tasks.get(id).map(|t| &t.state);
        let is_now_ready = matches!(after_task.state, TaskState::Ready);
        let was_ready = matches!(before_state, Some(TaskState::Ready));
        let is_now_completed = matches!(after_task.state, TaskState::Completed { .. });
        let was_completed = matches!(before_state, Some(TaskState::Completed { .. }));
        if is_now_ready && !was_ready {
            newly_ready.push(id.clone());
        }
        if is_now_completed && !was_completed {
            newly_completed.push(id.clone());
        }
    }
    if !newly_completed.is_empty() {
        println!(
            "  {} +{} completed: {}",
            "Δ".green(),
            newly_completed.len(),
            newly_completed.join(", ").dimmed()
        );
    }
    if !newly_ready.is_empty() {
        println!(
            "  {} +{} ready: {}",
            "Δ".blue(),
            newly_ready.len(),
            newly_ready.join(", ").dimmed()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_method() {
        let r = parse_resolution_body("STAR");
        assert_eq!(r.method, "STAR");
        assert!(r.fields.is_empty());
    }

    #[test]
    fn parse_prose_method() {
        let r = parse_resolution_body("scVI conditioned on dataset_id only");
        assert_eq!(r.method, "scVI conditioned on dataset_id only");
        assert!(r.fields.is_empty());
    }

    #[test]
    fn parse_kv_then_prose() {
        let r = parse_resolution_body(
            "batch_correction_required=true -- scVI conditioned on dataset_id",
        );
        assert_eq!(r.method, "scVI conditioned on dataset_id");
        assert_eq!(
            r.fields.get("batch_correction_required"),
            Some(&serde_json::json!(true))
        );
    }

    #[test]
    fn parse_kv_only() {
        let r = parse_resolution_body("required=false threshold=0.5");
        assert_eq!(r.method, "");
        assert_eq!(r.fields.get("required"), Some(&serde_json::json!(false)));
        assert_eq!(r.fields.get("threshold"), Some(&serde_json::json!(0.5)));
    }

    #[test]
    fn parse_truncate_echo() {
        let long = "a".repeat(100);
        let t = truncate_echo(&long, 20);
        assert_eq!(t.chars().count(), 20);
        assert!(t.ends_with('…'));
    }

    // ── Regression tests: parser must not hoist embedded k=v out of prose ────

    #[test]
    fn parse_parenthesized_kv_stays_in_prose() {
        // "(n_neighbors=15," starts with a paren so it's not a valid identifier
        // key and must NOT be extracted as a structured field.
        let r = parse_resolution_body(
            "Leiden sweep at 0.4 0.8 1.2 on per-dataset KNN graphs (n_neighbors=15, 50 PCs)",
        );
        assert!(r.fields.is_empty(), "fields={:?}", r.fields);
        assert!(r.method.contains("(n_neighbors=15,"));
        assert!(r.method.contains("50 PCs)"));
    }

    #[test]
    fn parse_embedded_kv_after_prose_stays_in_prose() {
        // "flavor=seurat_v3" has a valid identifier key but appears after prose
        // tokens, so it must stay in the method text, not be hoisted to fields.
        let r = parse_resolution_body(
            "scanpy highly_variable_genes with flavor=seurat_v3 and batch_key=dataset_id",
        );
        assert!(r.fields.is_empty(), "fields={:?}", r.fields);
        assert!(r.method.contains("flavor=seurat_v3"));
        assert!(r.method.contains("batch_key=dataset_id"));
    }

    #[test]
    fn parse_leading_kv_then_implicit_prose() {
        // k=v pairs at the head followed by prose without " -- " still split:
        // the first non-k=v token begins the prose tail.
        let r = parse_resolution_body("required=true scVI latent conditioned on dataset_id");
        assert_eq!(r.fields.get("required"), Some(&serde_json::json!(true)));
        assert_eq!(r.method, "scVI latent conditioned on dataset_id");
    }

    #[test]
    fn parse_exclude_n_equals_one_stays_in_prose() {
        // "(n=1" is not a valid identifier; must stay in prose intact.
        let r = parse_resolution_body(
            "exclude Li Z GSE205535 from DE entirely (n=1 per condition is not estimable)",
        );
        assert!(r.fields.is_empty(), "fields={:?}", r.fields);
        assert!(r.method.contains("(n=1"));
    }
}
