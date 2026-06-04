use crate::classify::ClassificationResult;
use crate::clock::Clock;
use crate::dag::{TaskKind, TaskState, DAG};
use crate::ids::TaskId;
use anyhow::Result;
use petgraph::algo::toposort;
use petgraph::graph::DiGraph;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Build a dereferenceable EDAM ontology IRI from a CURIE-style local id.
///
/// EDAM ids are carried internally as CURIEs (`topic:3308`,
/// `operation:3223`), but the canonical dereferenceable IRI uses an
/// underscore in the local id (`https://edamontology.org/topic_3308`),
/// which 303-redirects to the term. Emitting the raw colon form
/// (`https://edamontology.org/topic:3308`) yields a 404. Replace every
/// `:` with `_` so the emitted IRI always dereferences.
fn edam_iri(local_id: &str) -> String {
    format!("https://edamontology.org/{}", local_id.replace(':', "_"))
}

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
        // `conformsTo` asserts the full normative profile set — base
        // RO-Crate 1.1, the WorkflowHub workflow-ro-crate 1.0 profile,
        // the WRROC v0.5 Tier-3 profiles (process / workflow /
        // provenance), and the ECAA v0.2 profile — built from the single
        // `REQUIRED_PROFILE_IRIS` source of truth so the descriptor and
        // the spec-conformance post-checks never drift. The Tier-3 entity
        // builders (`parameter_connection_entity`, `p_plan_entity`) wire
        // into `build_metadata` separately; this descriptor declares the
        // intended profile set, not the per-entity emission.
        json!({
            "@id": "ro-crate-metadata.json",
            "@type": "CreativeWork",
            "conformsTo": ecaa_workflow_types::consts::REQUIRED_PROFILE_IRIS
                .iter()
                .map(|iri| json!({"@id": iri}))
                .collect::<Vec<_>>(),
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
                    {"@id": "runtime/ed-cf-self-assessment.json"},
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
            "programmingLanguage": {"@id": "#ecaa-workflow-dag"},
            "applicationSubCategory": {
                "@id": edam_iri(&classification.edam_topic)
            },
            "featureList": {
                "@id": edam_iri(&classification.edam_operation)
            },
            "step": topo_order.iter()
                .map(|id| json!({"@id": format!("#step-{}", id)}))
                .collect::<Vec<_>>(),
            "sdPublisher": {"@id": "#ecaa-workflow"}
        }),
        // Computer language descriptor
        json!({
            "@id": "#ecaa-workflow-dag",
            "@type": "ComputerLanguage",
            "name": "ECAA-workflow DAG",
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
            "@id": "runtime/ed-cf-self-assessment.json",
            "@type": "CreativeWork",
            "name": "ED/CF rubric self-assessment",
            "description": "Deterministic self-location of the system in the Extensibility-Dimension / Counterfactual-Floor design space. Informational — locates, does not validate.",
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
                    "@id": edam_iri(edam)
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

/// `name` of the experimental-archetype maturity stamp written onto the
/// root Dataset's `additionalProperty` array.
pub const ARCHETYPE_MATURITY_PROPERTY: &str = "archetypeMaturity";

/// `value` written when the chosen archetype is experimental
/// (scaffolded / not-production-validated).
pub const ARCHETYPE_MATURITY_EXPERIMENTAL: &str = "experimental";

/// Stamp the root Dataset (`@id == "./"`) of an already-built
/// `ro-crate-metadata.json` graph with an
/// `archetypeMaturity: experimental` `additionalProperty` PropertyValue,
/// recording that the package was planned from an *experimental*
/// (scaffolded / not-production-validated) archetype so a reviewer sees
/// the maturity.
///
/// Matches the `package_run_id` / `additionalProperty` shape in
/// [`build_metadata`]: appends to the existing array (created by the
/// `run_id` path) or inserts a fresh one. Idempotent — re-stamping a
/// graph that already carries the marker is a no-op — and deterministic
/// (no wall-clock), so re-emits of the same intake stay byte-identical.
///
/// Called only when the caller has determined the chosen archetype is
/// experimental (`EmitConfig::experimental_archetype`); production
/// archetypes are never stamped, preserving their byte-baseline.
pub fn stamp_experimental_archetype(metadata: &mut Value) {
    let Some(graph) = metadata.get_mut("@graph").and_then(|g| g.as_array_mut()) else {
        return;
    };
    let Some(root) = graph
        .iter_mut()
        .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("./"))
    else {
        return;
    };
    let Some(root_obj) = root.as_object_mut() else {
        return;
    };

    let stamp = json!({
        "@type": "PropertyValue",
        "name": ARCHETYPE_MATURITY_PROPERTY,
        "value": ARCHETYPE_MATURITY_EXPERIMENTAL
    });

    match root_obj
        .entry("additionalProperty".to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
    {
        Value::Array(props) => {
            let already = props.iter().any(|p| {
                p.get("name").and_then(|v| v.as_str()) == Some(ARCHETYPE_MATURITY_PROPERTY)
            });
            if !already {
                props.push(stamp);
            }
        }
        // `additionalProperty` exists but isn't an array (shouldn't
        // happen given `build_metadata`'s shape); normalize to an array.
        other => {
            *other = Value::Array(vec![stamp]);
        }
    }
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

/// Inject the audit-proof report's projected `InvariantVerdict` nodes
/// (`evaluated_against` edge folded onto each node) into the RO-Crate
/// `@graph` so the audit-proof verdicts are FIRST-CLASS typed triples
/// inside the package — not just the `runtime/audit-proof-report.json`
/// file-reference `CreativeWork` (which is kept).
///
/// The projection is deterministic (node `@id`s derive from the invariant
/// id; the report's wall-clock `evaluated_at` is intentionally dropped), so
/// re-injection keeps `ro-crate-metadata.json` byte-reproducible. Idempotent:
/// nodes whose `@id` already exists in `@graph` are replaced, not duplicated,
/// so a second emit / a conversation-path re-emit converges.
pub fn inject_audit_proof_verdict_nodes(metadata: &mut Value, report: &Value) -> Result<()> {
    let nodes = crate::emitter::ecaa_projection::project_audit_proof_jsonld(report);
    if nodes.is_empty() {
        return Ok(());
    }
    let graph = metadata
        .get_mut("@graph")
        .and_then(|g| g.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("ro-crate-metadata.json missing @graph array"))?;
    for node in nodes {
        let id = node.get("@id").and_then(Value::as_str).map(str::to_string);
        match id.and_then(|id| {
            graph
                .iter()
                .position(|e| e.get("@id").and_then(Value::as_str) == Some(id.as_str()))
        }) {
            Some(pos) => graph[pos] = node,
            None => graph.push(node),
        }
    }
    Ok(())
}

/// Register produced result tables as Evidence (V) `@graph` entities.
///
/// Runs POST-EXECUTION, after the agent has written result tables under
/// `runtime/outputs/<task>/`. At emit time only `required_figures` become
/// `@graph` entities (see [`build_metadata`]); result tables have no
/// declarative obligation and so were never carried as V nodes. Without this
/// pass a *verified* claim whose `supported_by` names a result table — the
/// common case for quantitative DE / significance claims — dangles in
/// `cross_graph_integrity` (Inv 5), because the cited table is not an
/// analytical-output entity. (This is the executed-Pasilla-package defect:
/// `supported_by: differential_expression.tsv` with no matching V node.)
///
/// Scans each `runtime/outputs/<task>/` directory RECURSIVELY (NOT the
/// `figures/` / `view_data/` sub-dirs — those are ImageObjects / render inputs,
/// not V `Table`s) for `*.tsv` / `*.csv` / `*.parquet` files and adds,
/// idempotently, one `["File", "Dataset"]` entity per table at its REAL
/// package-relative `@id` (`runtime/outputs/<task>/…/<file>`, preserving any
/// nesting), linked from the root Dataset's `hasPart`. Walking recursively
/// matters because agents commonly nest result tables a level down (e.g.
/// `runtime/outputs/<task>/tables/de.tsv`); a direct-children-only scan left
/// those unregistered, so a verified claim citing such a table dangled in
/// `cross_graph_integrity` (Inv 5). [`crate::audit_proof::output_source::analytical_outputs`]
/// then surfaces the table as a V `Table` under `runtime/outputs/`, so the
/// claim's `supported_by` (see [`crate::claim_sink::project_verdict_rows`])
/// resolves — by basename, in Inv 3 / Inv 5 — and the two invariants agree on
/// the same C→V link.
///
/// Idempotent: an `@id` already present is left untouched, so re-running after
/// further task completions never duplicates entities. Deterministic order
/// (tasks then files sorted). Returns the number of newly-registered tables;
/// a no-op `Ok(0)` when the package has no `ro-crate-metadata.json`, no
/// `runtime/outputs/`, or every produced table is already registered.
pub fn register_produced_output_tables(package_root: &std::path::Path) -> std::io::Result<usize> {
    let descriptor = package_root.join("ro-crate-metadata.json");
    let Ok(bytes) = std::fs::read(&descriptor) else {
        return Ok(0);
    };
    let Ok(mut doc) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(0);
    };
    let Some(graph) = doc.get_mut("@graph").and_then(Value::as_array_mut) else {
        return Ok(0);
    };

    // Existing @ids — registration is idempotent.
    let existing: std::collections::BTreeSet<String> = graph
        .iter()
        .filter_map(|e| e.get("@id").and_then(Value::as_str).map(String::from))
        .collect();

    // Discover produced tables, deterministically ordered (tasks, then nested
    // files in sorted path order). Each `runtime/outputs/<task>/` is walked
    // RECURSIVELY so tables the agent nested (e.g. `…/<task>/tables/de.tsv`)
    // are registered at their real relative `@id`. The `figures/` and
    // `view_data/` sub-trees are pruned wholesale — they hold ImageObjects /
    // render inputs, not V `Table`s.
    let outputs_root = package_root.join("runtime").join("outputs");
    let mut rels: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&outputs_root) {
        let mut task_dirs: Vec<std::path::PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        task_dirs.sort();
        for task_dir in task_dirs {
            let task = task_dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if task.is_empty() {
                continue;
            }
            collect_output_tables(&task_dir, &format!("runtime/outputs/{task}"), &mut rels);
        }
    }
    rels.sort();

    let mut new_parts: Vec<Value> = Vec::new();
    for rel in &rels {
        if existing.contains(rel) {
            continue;
        }
        let (task, file) = rel
            .strip_prefix("runtime/outputs/")
            .and_then(|r| r.split_once('/'))
            .unwrap_or(("", rel.as_str()));
        let fmt = if rel.ends_with(".csv") {
            "text/csv"
        } else if rel.ends_with(".parquet") {
            "application/vnd.apache.parquet"
        } else {
            "text/tab-separated-values"
        };
        graph.push(json!({
            "@id": rel,
            "@type": ["File", "Dataset"],
            "name": format!("{task} — {file}"),
            "description": format!("Analytical result table produced by stage '{task}'."),
            "encodingFormat": fmt,
            "schema:about": {"@id": format!("#step-{task}")},
        }));
        new_parts.push(json!({"@id": rel}));
    }

    let added = new_parts.len();
    if added == 0 {
        return Ok(0);
    }

    // Link new tables from the root Dataset's hasPart so walkers find them.
    if let Some(root) = graph
        .iter_mut()
        .find(|e| e.get("@id").and_then(Value::as_str) == Some("./"))
    {
        if let Some(obj) = root.as_object_mut() {
            let parts = obj
                .entry("hasPart")
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(arr) = parts.as_array_mut() {
                arr.extend(new_parts);
            }
        }
    }

    // Atomic, crash-safe descriptor write (write-tmp -> fsync -> rename). The
    // descriptor is also touched by the harness / conversation emit writers, so
    // a non-atomic `std::fs::write` here raced a concurrent writer and could
    // leave a half-written or torn `ro-crate-metadata.json`. The shared
    // [`crate::fs_helpers::atomic_write_bytes_sync`] gives durable rename
    // semantics matching every other package-surface write.
    let serialized = serde_json::to_vec_pretty(&doc)?;
    crate::fs_helpers::atomic_write_bytes_sync(&descriptor, &serialized)?;
    Ok(added)
}

/// Recursively collect produced result-table relative paths under `dir`,
/// rooted at the package-relative `rel_prefix` (e.g. `runtime/outputs/<task>`).
/// Prunes the `figures/` and `view_data/` sub-trees (ImageObjects / render
/// inputs, not V `Table`s). Only `*.tsv` / `*.csv` / `*.parquet` files are
/// kept. Paths accumulate into `out` (the caller sorts for determinism).
fn collect_output_tables(dir: &std::path::Path, rel_prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        let rel = format!("{rel_prefix}/{name}");
        if path.is_dir() {
            // Prune the non-table sub-trees wholesale.
            if name == "figures" || name == "view_data" {
                continue;
            }
            collect_output_tables(&path, &rel, out);
        } else if path.is_file()
            && (name.ends_with(".tsv") || name.ends_with(".csv") || name.ends_with(".parquet"))
        {
            out.push(rel);
        }
    }
}

/// Post-execution finalize: register agent-produced result tables as V `@graph`
/// entities ([`register_produced_output_tables`]) and then re-seal the BagIt
/// payload manifest ([`crate::emitter::regenerate_bagit_manifest`]).
///
/// `ro-crate-metadata.json` is a manifested file, so registering tables (which
/// mutates the descriptor) would otherwise leave `manifest-sha512.txt` stale.
/// The re-seal runs UNCONDITIONALLY — even when no new table is registered —
/// because by post-exec time the descriptor and `WORKFLOW.json` have already
/// drifted from the emit-time seal (the conversation emit path patches the
/// descriptor; the harness rewrites task states), so the package needs
/// reconciling regardless. Idempotent and deterministic: the verify-finalize
/// fires once per task, and repeated invocation converges to identical manifest
/// bytes (registration is idempotent; the re-seal walk is sorted). Returns the
/// count of newly-registered tables.
///
/// Call AFTER the agent's tables are on disk and BEFORE regenerating the
/// at-rest `audit-proof-report.json`, so `cross_graph_integrity` (Inv 5) sees
/// the freshly-registered Evidence node and the report records the resolved
/// C→V link rather than a stale dangling `Fail`.
pub fn finalize_evidence_registration(
    root: &std::path::Path,
    clock: &dyn crate::clock::Clock,
) -> std::io::Result<usize> {
    finalize_evidence_registration_with_verifier(root, clock, None)
}

/// As [`finalize_evidence_registration`], plus — when a `verifier` is supplied —
/// a C-subgraph BACK-FILL: read the HMAC-signed verdict sink
/// (`runtime/verification-reports/claim-verification.signed.json`), project its
/// verdicts into spec `Claim` `@graph` nodes (+ `supported_by` edges to the V
/// table entities) via [`inject_claim_verdict_nodes`], and inject them into the
/// descriptor's `@graph` idempotently — BEFORE the manifest re-seal, so the
/// re-seal covers the updated descriptor.
///
/// Why: post-exec the `@graph` carries ZERO `Claim` (C) nodes even when the
/// signed sink has verified claims, because the at-emit C-subgraph projection
/// reads the EMPTY agent-writable plaintext `claim-verification.json` and the
/// `@graph` is never re-projected post-exec. The verdicts that matter live only
/// in the signed sink. Without a `verifier` (the 2-arg back-compat path) the
/// back-fill is skipped — the sink's HMAC cannot be verified without the
/// session secret, so projecting it would be unsound.
pub fn finalize_evidence_registration_with_verifier(
    root: &std::path::Path,
    clock: &dyn crate::clock::Clock,
    verifier: Option<&crate::audit_writer::AuditWriter>,
) -> std::io::Result<usize> {
    let added = register_produced_output_tables(root)?;
    if let Some(verifier) = verifier {
        if let Err(e) = backfill_claim_subgraph(root, verifier) {
            // Best-effort: a back-fill failure must not abort the
            // register + re-seal (the manifest must still reconcile).
            tracing::warn!(
                target: "ecaa::ro_crate",
                error = %e,
                "C-subgraph back-fill from signed sink failed"
            );
        }
    }
    crate::emitter::regenerate_bagit_manifest(root, clock).map_err(std::io::Error::other)?;
    Ok(added)
}

/// Read the signed verdict sink and inject its projected `Claim` nodes into the
/// descriptor `@graph`. No-op (Ok) when the sink is absent, empty, tampered, or
/// carries no verdicts, and when the descriptor is missing.
fn backfill_claim_subgraph(
    root: &std::path::Path,
    verifier: &crate::audit_writer::AuditWriter,
) -> anyhow::Result<()> {
    let pkg =
        crate::audit_proof::loader::LoadedPackage::from_root_with_verifier(root, Some(verifier))?;
    // `claims` is the unioned signed-sink doc (`{verdicts:[…]}`) when the sink
    // verified; None / tampered ⇒ nothing trustworthy to project.
    let Some(claims) = pkg.claims.as_ref() else {
        return Ok(());
    };
    let nodes = crate::emitter::ecaa_projection::project_claim_jsonld(claims);
    if nodes.is_empty() {
        return Ok(());
    }
    let descriptor = root.join("ro-crate-metadata.json");
    let Ok(bytes) = std::fs::read(&descriptor) else {
        return Ok(());
    };
    let mut doc: Value = serde_json::from_slice(&bytes)?;
    inject_claim_verdict_nodes(&mut doc, nodes)?;
    let serialized = serde_json::to_vec_pretty(&doc)?;
    crate::fs_helpers::atomic_write_bytes_sync(&descriptor, &serialized)?;
    Ok(())
}

/// Inject projected `Claim` `@graph` nodes into `metadata`'s `@graph`
/// idempotently: a node whose `@id` already exists is REPLACED (so a re-run with
/// updated verdicts converges), not duplicated. Mirrors
/// [`inject_audit_proof_verdict_nodes`].
pub fn inject_claim_verdict_nodes(metadata: &mut Value, nodes: Vec<Value>) -> Result<()> {
    if nodes.is_empty() {
        return Ok(());
    }
    let graph = metadata
        .get_mut("@graph")
        .and_then(|g| g.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("ro-crate-metadata.json missing @graph array"))?;
    for node in nodes {
        let id = node.get("@id").and_then(Value::as_str).map(str::to_string);
        match id.and_then(|id| {
            graph
                .iter()
                .position(|e| e.get("@id").and_then(Value::as_str) == Some(id.as_str()))
        }) {
            Some(pos) => graph[pos] = node,
            None => graph.push(node),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FrozenClock;
    use regex::Regex;

    #[test]
    fn edam_iri_converts_curie_colon_to_underscore() {
        // CURIE-style ids carry a colon internally; the dereferenceable
        // EDAM IRI must use an underscore in the local id.
        assert_eq!(
            edam_iri("topic:3673"),
            "https://edamontology.org/topic_3673"
        );
        assert_eq!(
            edam_iri("operation:3223"),
            "https://edamontology.org/operation_3223"
        );
        // Already-underscore ids pass through unchanged.
        assert_eq!(
            edam_iri("topic_3308"),
            "https://edamontology.org/topic_3308"
        );
    }

    /// Build a minimal one-task DAG whose single task carries an
    /// `edam_operation` spec, so `build_metadata` exercises all three
    /// edamontology.org IRI build sites (applicationSubCategory,
    /// featureList, and the per-step instrument annotation).
    fn one_task_dag() -> DAG {
        let task: crate::dag::Task = serde_json::from_value(json!({
            "kind": "computation",
            "state": {"status": "pending"},
            "depends_on": [],
            "assignee": "agent",
            "description": "test task",
            "spec": {"edam_operation": "operation:3223"}
        }))
        .expect("minimal task deserializes");
        let mut tasks = std::collections::BTreeMap::new();
        tasks.insert(TaskId::from("t1"), task);
        let mut dag = DAG {
            version: "1.0".into(),
            schema_version: crate::dag::current_dag_schema_version(),
            workflow_id: "test".into(),
            current_task: None,
            tasks,
            run_id: None,
            reverse_deps: std::collections::BTreeMap::new(),
        };
        dag.rebuild_reverse_deps();
        dag
    }

    #[test]
    fn emitted_edam_iris_are_dereferenceable_underscore_form() {
        let dag = one_task_dag();
        let classification = ClassificationResult {
            domain: "genomics".into(),
            workflow_description: "test workflow".into(),
            intake_text: "test intake".into(),
            edam_topic: "topic:3673".into(),
            edam_operation: "operation:3223".into(),
            ..Default::default()
        };

        let metadata = build_metadata(&dag, &classification, &FrozenClock::default());

        // Collect every "@id" string that points at edamontology.org,
        // descending the whole @graph so workflow-level and per-step
        // IRIs are both checked.
        let mut edam_ids: Vec<String> = Vec::new();
        collect_edam_ids(&metadata, &mut edam_ids);

        // applicationSubCategory + featureList + instrument => 3 sites.
        assert_eq!(
            edam_ids.len(),
            3,
            "expected 3 edamontology IRIs, got {edam_ids:?}"
        );

        let topic_re = Regex::new(r"^https://edamontology\.org/topic_\d+$").unwrap();
        let op_re = Regex::new(r"^https://edamontology\.org/operation_\d+$").unwrap();
        for id in &edam_ids {
            // No raw colon may survive in the local id.
            let local = id
                .strip_prefix("https://edamontology.org/")
                .expect("edam IRI keeps its base prefix");
            assert!(
                !local.contains(':'),
                "edam IRI local id still contains ':': {id}"
            );
            assert!(
                topic_re.is_match(id) || op_re.is_match(id),
                "edam IRI not in canonical topic_N / operation_N form: {id}"
            );
        }

        // Exactly one topic_ IRI and two operation_ IRIs (featureList +
        // per-step instrument both come from edam_operation).
        assert_eq!(edam_ids.iter().filter(|i| topic_re.is_match(i)).count(), 1);
        assert_eq!(edam_ids.iter().filter(|i| op_re.is_match(i)).count(), 2);
    }

    /// Recursively gather every `@id` value whose string points at
    /// edamontology.org.
    fn collect_edam_ids(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(map) => {
                if let Some(Value::String(id)) = map.get("@id") {
                    if id.starts_with("https://edamontology.org/") {
                        out.push(id.clone());
                    }
                }
                for (_, child) in map {
                    collect_edam_ids(child, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect_edam_ids(item, out);
                }
            }
            _ => {}
        }
    }
}
