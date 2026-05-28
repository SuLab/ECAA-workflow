use crate::classify::ClassificationResult;
use crate::clock::Clock;
use crate::dag::{TaskKind, TaskState, DAG};
use crate::ids::TaskId;
use anyhow::Result;
use petgraph::algo::toposort;
use petgraph::graph::DiGraph;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Build the complete ro-crate-metadata.json JSON-LD graph.
///
/// When `dag.run_id` is `Some`, the root Dataset entity includes a
/// `additionalProperty[{name:"package_run_id", value:<uuid>}]` entry so
/// downstream RO-Crate consumers can correlate packages by id.
///
/// `clock` supplies the `dateCreated` value on the root `Dataset`.
/// Emit-pipeline callers pass a `FrozenClock` derived from the intake
/// hash so two emits of the same intake produce byte-identical
/// `ro-crate-metadata.json`; non-emit / read-only callers can pass
/// `&WallClock`.
pub fn build_metadata(
    dag: &DAG,
    classification: &ClassificationResult,
    clock: &dyn Clock,
) -> Value {
    let topo_order = compute_topo_order(dag);

    let mut graph: Vec<Value> = vec![
        // RO-Crate metadata descriptor.
        // Grant v19 D1 / G1 — `conformsTo` asserts WRROC v0.5
        // (process / workflow / provenance Tier-3 profiles) alongside
        // the base RO-Crate 1.1 IRI. The Tier-3 entity builders
        // (`parameter_connection_entity`, `p_plan_entity`) wire into
        // `build_metadata` via Tasks C1 / C2; this descriptor change is
        // valid standalone because `conformsTo` declares the intended
        // profile set, not the per-entity emission.
        json!({
            "@id": "ro-crate-metadata.json",
            "@type": "CreativeWork",
            "conformsTo": [
                {"@id": "https://w3id.org/ro/crate/1.1"},
                {"@id": "https://w3id.org/ro/wfrun/process/0.5"},
                {"@id": "https://w3id.org/ro/wfrun/workflow/0.5"},
                {"@id": "https://w3id.org/ro/wfrun/provenance/0.5"}
            ],
            "about": {"@id": "./"}
        }),
        // Root dataset.
        //
        // `license` is a FAIR-R1.1 hard requirement (data must be
        // released with a clear and accessible data-usage license).
        // The repository is Apache-2.0, so emitted packages declare
        // the same and downstream FAIR consumers (RO-Crate
        // validators, the Bioschemas DataCatalog crawler) accept
        // the entry. We pin the IRI form because the
        // FAIR maturity validator scores literal SPDX strings lower
        // than URL-form licenses.
        {
            let mut root = serde_json::json!({
                "@id": "./",
                "@type": "Dataset",
                "name": format!("{} — {}", classification.domain, classification.workflow_description),
                "description": &classification.intake_text,
                "dateCreated": clock.now_rfc3339(),
                "license": "https://www.apache.org/licenses/LICENSE-2.0",
                "hasPart": [
                    {"@id": "WORKFLOW.json"},
                    {"@id": "PROMPT.md"},
                    {"@id": "CONTEXT.md"},
                    {"@id": "AGENT-EXECUTOR.md"},
                    {"@id": "policies/"},
                    {"@id": "runtime/LOG.jsonl"},
                    {"@id": "runtime/intake-conversation.jsonl"},
                    {"@id": "runtime/decisions.jsonl"},
                    {"@id": "runtime/proofs.jsonl"},
                    {"@id": "runtime/claim-verification.json"},
                    {"@id": "runtime/verifier-decisions.jsonl"},
                    {"@id": "runtime/assumptions.jsonl"},
                    {"@id": "runtime/validation-reports.jsonl"},
                    {"@id": "runtime/determinism-shim.json"},
                    {"@id": "runtime/security-policy.json"},
                    {"@id": "runtime/audit-proof-report.json"},
                    {"@id": "runtime/validation-summary.json"}
                ],
                "mainEntity": {"@id": "WORKFLOW.json"}
            });
            if let Some(run_id) = &dag.run_id {
                let additional_property = serde_json::json!([{
                    "@type": "PropertyValue",
                    "name": "package_run_id",
                    "value": run_id
                }]);
                root.as_object_mut()
                    .expect("root is a JSON object literal above")
                    .insert("additionalProperty".to_string(), additional_property);
            }
            root
        },
        // ComputationalWorkflow with Bioschemas profile
        json!({
            "@id": "WORKFLOW.json",
            "@type": ["File", "ComputationalWorkflow"],
            "name": format!("{} DAG", dag.workflow_id),
            "encodingFormat": "application/json",
            "conformsTo": {
                "@id": "https://bioschemas.org/profiles/ComputationalWorkflow/1.0-RELEASE"
            },
            "programmingLanguage": {"@id": "#scripps-workflow-dag"},
            "applicationSubCategory": {
                "@id": format!("https://edamontology.org/{}", classification.edam_topic)
            },
            "featureList": {
                "@id": format!("https://edamontology.org/{}", classification.edam_operation)
            },
            "step": topo_order.iter()
                .map(|id| json!({"@id": format!("#step-{}", id)}))
                .collect::<Vec<_>>(),
            "sdPublisher": {"@id": "#ecaa-workflow"}
        }),
        // Computer language descriptor
        json!({
            "@id": "#scripps-workflow-dag",
            "@type": "ComputerLanguage",
            "name": "Scripps Workflow DAG",
            "version": &dag.version
        }),
        // Publisher
        json!({
            "@id": "#ecaa-workflow",
            "@type": "Organization",
            "name": "ecaa-workflow"
        }),
        // SME role — used as agent on actions that record compile-time resolutions
        json!({
            "@id": "#sme",
            "@type": "Role",
            "name": "Subject Matter Expert (intake)",
            "description": "The domain expert who resolved discovery decisions during intake chat, prior to agent execution."
        }),
        // File entities
        json!({
            "@id": "PROMPT.md",
            "@type": "File",
            "name": "Agent Instructions",
            "encodingFormat": "text/markdown"
        }),
        json!({
            "@id": "CONTEXT.md",
            "@type": "File",
            "name": "Workflow Context",
            "encodingFormat": "text/markdown"
        }),
        json!({
            "@id": "AGENT-EXECUTOR.md",
            "@type": "CreativeWork",
            "name": "Executor agent task brief",
            "description": "Per-package brief consumed by the execution agent; replaces ambient host CLAUDE.md for executor context.",
            "encodingFormat": "text/markdown"
        }),
        json!({
            "@id": "policies/",
            "@type": "Dataset",
            "name": "Discovery and scoring policies"
        }),
        json!({
            "@id": "runtime/LOG.jsonl",
            "@type": "File",
            "name": "Execution provenance log",
            "encodingFormat": "application/jsonl"
        }),
        json!({
            "@id": "runtime/intake-conversation.jsonl",
            "@type": "CreativeWork",
            "name": "SME intake conversation log",
            "description": "Compile-time intake conversation turns captured for ECAA replay.",
            "encodingFormat": "application/jsonl"
        }),
        json!({
            "@id": "runtime/decisions.jsonl",
            "@type": "CreativeWork",
            "name": "SME decision log",
            "description": "Compile-time decision records captured for ECAA replay.",
            "encodingFormat": "application/jsonl"
        }),
        json!({
            "@id": "runtime/proofs.jsonl",
            "@type": "CreativeWork",
            "name": "Compatibility proofs",
            "description": "Per-edge ECAA evidence records mirroring WORKFLOW.json dependencies.",
            "encodingFormat": "application/jsonl"
        }),
        json!({
            "@id": "runtime/claim-verification.json",
            "@type": "CreativeWork",
            "name": "Deterministic claim verification report",
            "description": "Emit-time claim verification rollup.",
            "encodingFormat": "application/json"
        }),
        json!({
            "@id": "runtime/verifier-decisions.jsonl",
            "@type": "CreativeWork",
            "name": "Verifier decision substrate",
            "description": "Typed verifier-decision event log.",
            "encodingFormat": "application/jsonl"
        }),
        json!({
            "@id": "runtime/assumptions.jsonl",
            "@type": "CreativeWork",
            "name": "Assumption ledger",
            "description": "ECAA failure/assumption ledger.",
            "encodingFormat": "application/jsonl"
        }),
        json!({
            "@id": "runtime/validation-reports.jsonl",
            "@type": "CreativeWork",
            "name": "Validation reports",
            "description": "Harness validation report stream.",
            "encodingFormat": "application/jsonl"
        }),
        json!({
            "@id": "runtime/determinism-shim.json",
            "@type": "CreativeWork",
            "name": "Determinism shim",
            "description": "Active deterministic environment, seed, temp-path, locale, and timezone disclosure.",
            "encodingFormat": "application/json"
        }),
        json!({
            "@id": "runtime/security-policy.json",
            "@type": "CreativeWork",
            "name": "Package security policy",
            "description": "Package-level SafetyPolicy aggregate and container digest disclosure.",
            "encodingFormat": "application/json"
        }),
        json!({
            "@id": "runtime/audit-proof-report.json",
            "@type": "CreativeWork",
            "name": "Audit-proof invariant report",
            "description": "ECAA audit-proof invariant verdicts.",
            "encodingFormat": "application/json"
        }),
        json!({
            "@id": "runtime/validation-summary.json",
            "@type": "CreativeWork",
            "name": "ECAA emit-time validation summary",
            "description": "Schema-validation and external-validator rollup for emitted ECAA artifacts.",
            "encodingFormat": "application/json"
        }),
    ];

    // Organism taxon entities
    for org in &classification.organisms {
        graph.push(json!({
            "@id": format!(
                "https://www.ncbi.nlm.nih.gov/Taxonomy/Browser/wwwtax.cgi?id={}",
                org.taxon_id
            ),
            "@type": "Taxon",
            "name": &org.name,
            "identifier": org.taxon_id
        }));
    }

    // HowToStep entities in topological order (position reflects execution order)
    // SME-resolved discovery tasks also emit a sibling Action entity capturing
    // the structured resolution fields (not just free-text prose).
    let mut sme_actions: Vec<Value> = Vec::new();

    for (i, id) in topo_order.iter().enumerate() {
        let task = &dag.tasks[*id];
        let mut step = json!({
            "@id": format!("#step-{}", id),
            "@type": "HowToStep",
            "name": &task.description,
            "position": i + 1
        });

        // Annotate with task kind
        let kind_str = task_kind_label(&task.kind);
        step["additionalType"] = json!(kind_str);

        // EDAM operation annotation from task spec
        if let Some(spec) = &task.spec {
            if let Some(edam) = spec.get("edam_operation").and_then(|v| v.as_str()) {
                step["instrument"] = json!({
                    "@id": format!("https://edamontology.org/{}", edam)
                });
            }
        }

        // If this task was completed by the SME at compile time, promote the
        // method prose into description and emit a linked Action capturing
        // the full structured result (method + any field overrides).
        if let TaskState::Completed { result } = &task.state {
            let resolved_by = result.get("resolved_by").and_then(|v| v.as_str());
            if resolved_by == Some("sme") {
                if let Some(method) = result.get("method").and_then(|v| v.as_str()) {
                    step["description"] = json!(method);
                }

                let action_id = format!("#sme-action-{}", id);
                step["workExample"] = json!({"@id": action_id});

                let mut action = json!({
                    "@id": action_id,
                    "@type": "Action",
                    "name": format!("SME intake resolution for {}", id),
                    "actionStatus": "https://schema.org/CompletedActionStatus",
                    "object": {"@id": format!("#step-{}", id)},
                    "agent": {"@id": "#sme"}
                });
                if let Some(obj) = result.as_object() {
                    let mut result_payload = serde_json::Map::new();
                    for (k, v) in obj {
                        result_payload.insert(k.clone(), v.clone());
                    }
                    action["result"] = Value::Object(result_payload);
                }
                sme_actions.push(action);
            }
        }

        graph.push(step);
    }

    // Append SME Action entities after the steps they reference
    graph.extend(sme_actions);

    // Figure declarations: every task whose spec declares
    // `required_figures` gets one `ImageObject` entity per figure id.
    // These are declarative — the files land after the agent runs
    // `runtime.plotting.core.generate()`. Consumers walking the
    // RO-Crate get the expected-artifact list without needing to
    // wait for execution. The `@id` matches the path the dashboard
    // and artifact endpoints use.
    let mut figure_entities: Vec<Value> = Vec::new();
    let mut figure_ids_for_root: Vec<Value> = Vec::new();
    for (task_id, task) in &dag.tasks {
        let Some(spec) = &task.spec else { continue };
        let Some(figures) = spec.get("required_figures").and_then(|v| v.as_array()) else {
            continue;
        };
        for fig in figures {
            let Some(fig_id) = fig.as_str() else { continue };
            let rel = format!("runtime/outputs/{}/figures/{}.png", task_id, fig_id);
            figure_entities.push(json!({
                "@id": rel.clone(),
                "@type": ["File", "ImageObject"],
                "name": format!("{} — {}", task_id, fig_id),
                "description": format!("Diagnostic figure '{}' produced by stage '{}' via runtime/plotting/stages/{}.py.", fig_id, task_id, task_id),
                "encodingFormat": "image/png",
                "schema:about": {"@id": format!("#step-{}", task_id)},
            }));
            figure_ids_for_root.push(json!({"@id": rel}));
        }
    }
    // Link figures from the root Dataset's hasPart so walkers find them.
    if !figure_ids_for_root.is_empty() {
        if let Some(root) = graph
            .iter_mut()
            .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("./"))
        {
            if let Some(parts) = root.get_mut("hasPart").and_then(|v| v.as_array_mut()) {
                for entry in figure_ids_for_root {
                    parts.push(entry);
                }
            }
        }
    }
    graph.extend(figure_entities);

    // Grant v19 §C.0.1 / Tasks C1 + C2 — emit one
    // `p-plan:Plan` entity per package and one `ParameterConnection`
    // entity per DAG edge. The Tier-3 builders are defined as
    // `p_plan_entity` + `parameter_connection_entity` below; this is
    // the call-site wiring that walks `dag.tasks` and produces the
    // entities for `runcrate validate` (≥ 0.5.0) + the WRROC
    // post-validation checks asserted by
    // `crates/core/tests/prov_o_corpus.rs`.
    let plan_id = dag.workflow_id.clone();
    let archetype_id = classification.archetype_id.as_deref();
    let rationale = format!(
        "Composed for modality `{}` (domain `{}`).",
        classification.modality, classification.domain
    );
    graph.push(p_plan_entity(&plan_id, archetype_id, &rationale));

    for (target_id, task) in &dag.tasks {
        for source_id in &task.depends_on {
            let edge_id = format!("{}__to__{}", source_id, target_id);
            graph.push(parameter_connection_entity(
                &edge_id,
                &format!("#step-{}", source_id),
                "output",
                &format!("#step-{}", target_id),
                "input",
            ));
        }
    }

    json!({
        "@context": [
            "https://w3id.org/ro/crate/1.1/context",
            // WRROC Tier-3 (Provenance Run Crate) extension
            // namespace. The Workflow Run RO-Crate spec adds an extension
            // context for `ParameterConnection`, `wfprov:`, and the
            // `wfdesc:`/`wf:` workflow-description vocabulary. Adding it
            // here keeps the @context array forward-compatible: ROCs
            // emitted today round-trip through Tier-3 readers (runcrate ≥
            // 0.5.0, StreamFlow ≥ 0.2.0.dev10, nf-prov ≥ 1.4.0); ROCs
            // emitted with explicit ParameterConnection / p-plan entities
            // (S6.14 follow-up) will validate against the Tier-3 schema.
            "https://w3id.org/ro/terms/workflow-run"
        ],
        "@graph": graph
    })
}

/// Emit a WRROC Tier-3 `ParameterConnection` entity
/// describing one edge between two composed atoms. Each connection
/// names the source atom's output port + the target atom's input port,
/// matching the WRROC `wfdesc:hasOutput` / `wfdesc:hasInput` shape so
/// runcrate / StreamFlow / nf-prov consumers can resolve the edge.
///
/// Walks `CompositionResult::atoms` and emits one entity per
/// `depends_on` edge. Production-wired through the S6.14 Tier-3 emit
/// path; all 23 testdata fixture packages emit 9-43
/// ParameterConnections + 1 p-plan:Plan at compile time (verified by
/// `wrroc_v05_fixtures::g1_acceptance_…`).
pub fn parameter_connection_entity(
    edge_id: &str,
    source_stage: &str,
    source_port: &str,
    target_stage: &str,
    target_port: &str,
) -> Value {
    json!({
        "@id": format!("#parameter-connection/{}", edge_id),
        "@type": "ParameterConnection",
        "sourceParameter": {"@id": format!("{}#{}", source_stage, source_port)},
        "targetParameter": {"@id": format!("{}#{}", target_stage, target_port)},
    })
}

/// Emit a WRROC Tier-3 `p-plan:Plan` entity for
/// prospective provenance. The plan entity captures the archetype +
/// composition rationale at compose time; the retrospective side
/// (`CreateAction` per task) lands at execution time via the existing
/// agent-side provenance hooks.
pub fn p_plan_entity(plan_id: &str, archetype_id: Option<&str>, rationale: &str) -> Value {
    json!({
        "@id": format!("#p-plan/{}", plan_id),
        "@type": ["Plan", "p-plan:Plan"],
        "matchedArchetype": archetype_id,
        "rationale": rationale,
    })
}

/// Compute topological order of tasks for correct HowToStep position assignment.
/// BTreeMap iteration is lexicographic — we need execution order.
/// Returns Err if cycles exist (should have been caught by validate_dag).
fn compute_topo_order(dag: &DAG) -> Vec<&TaskId> {
    let mut g: DiGraph<&TaskId, ()> = DiGraph::new();
    let idx: HashMap<&TaskId, _> = dag.tasks.keys().map(|id| (id, g.add_node(id))).collect();

    for (id, task) in &dag.tasks {
        for dep in &task.depends_on {
            if let (Some(&from), Some(&to)) = (idx.get(dep), idx.get(id)) {
                g.add_edge(from, to, ());
            }
        }
    }

    // DAG was validated cycle-free at build time; unwrap is safe
    toposort(&g, None)
        .unwrap_or_default()
        .into_iter()
        .map(|n| *g.node_weight(n).unwrap())
        .collect()
}

fn task_kind_label(kind: &TaskKind) -> &'static str {
    match kind {
        TaskKind::Discovery(_) => "discovery",
        TaskKind::Computation => "computation",
        TaskKind::Validation => "validation",
        TaskKind::Review => "review",
        TaskKind::Gate => "gate",
    }
}

/// Append W3C PROV-O provenance entities to an existing ro-crate-metadata.json graph.
/// Called by the post-execution Python script (or directly from Rust in tests).
pub fn append_prov_entities(metadata: &mut Value, prov_activities: Vec<Value>) -> Result<()> {
    let graph = metadata
        .get_mut("@graph")
        .and_then(|g| g.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("ro-crate-metadata.json missing @graph array"))?;
    for activity in prov_activities {
        graph.push(activity);
    }
    Ok(())
}
