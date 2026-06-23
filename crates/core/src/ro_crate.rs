use crate::classify::ClassificationResult;
use crate::clock::Clock;
use crate::dag::{TaskKind, TaskState, DAG};
use anyhow::Result;
use serde_json::{json, Value};

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

/// Build a first-class `CreativeWork` profile entity for a normative
/// `conformsTo` IRI from [`ecaa_workflow_types::consts::REQUIRED_PROFILE_IRIS`].
///
/// The `version` is the IRI's trailing path segment with any leading `v`
/// stripped (`…/1.1` → `1.1`, `…/v0.2` → `0.2`); the `name` is a stable
/// human-readable label keyed off the IRI so the descriptor's `conformsTo`
/// references resolve to a named, versioned node instead of dangling. No value
/// is fabricated — `version` derives solely from the IRI the spec already pins.
fn profile_entity(iri: &str) -> Value {
    let last = iri.rsplit('/').next().unwrap_or(iri);
    let version = last.strip_prefix('v').unwrap_or(last);
    let name = match iri {
        "https://w3id.org/ro/crate/1.1" => "RO-Crate",
        "https://w3id.org/workflowhub/workflow-ro-crate/1.0" => "Workflow RO-Crate Profile",
        "https://w3id.org/ro/wfrun/process/0.5" => "Process Run Crate Profile",
        "https://w3id.org/ro/wfrun/workflow/0.5" => "Workflow Run Crate Profile",
        "https://w3id.org/ro/wfrun/provenance/0.5" => "Provenance Run Crate Profile",
        "https://w3id.org/ecaa/v0.2" => "ECAA Conformance Profile",
        // Unknown IRI (should never happen given the closed const set): fall
        // back to the IRI itself as the name rather than inventing a label.
        other => other,
    };
    json!({
        "@id": iri,
        "@type": "CreativeWork",
        "name": name,
        "version": version,
    })
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
    let topo_order = crate::dag::topo_order_ids(dag);

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
                // The root Dataset declares the full normative profile set
                // (§4.3) — base RO-Crate 1.1, WorkflowHub workflow-ro-crate
                // 1.0, the three WRROC v0.5 Tier-3 profiles, and ECAA v0.2 —
                // mirroring the metadata descriptor's `conformsTo` so an
                // RO-Crate validator that profiles off `./` (the Workflow Run
                // Crate validators do) sees the same declared profiles as one
                // reading the descriptor. Each IRI is also emitted as a
                // first-class `CreativeWork` profile entity below so the
                // reference resolves rather than dangling.
                "conformsTo": ecaa_workflow_types::consts::REQUIRED_PROFILE_IRIS
                    .iter()
                    .map(|iri| json!({"@id": iri}))
                    .collect::<Vec<_>>(),
                "hasPart": [
                    {"@id": "README.md"},
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
            "@id": "README.md",
            "@type": "File",
            "name": "Package README — human landing page",
            "description": "Human-readable entry point: what was asked, where the answer lands, a map of the package, and the re-run command.",
            "encodingFormat": "text/markdown",
            "about": {"@id": "./"}
        }),
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

    // Profile entities. Each `conformsTo` IRI declared on `ro-crate-metadata.json`
    // and on the root `./` Dataset is emitted as a first-class `CreativeWork`
    // so the reference resolves to a named, versioned entity rather than a bare
    // `{@id}` dangling ref. Name + version are parsed deterministically from the
    // IRI's trailing version segment (`…/1.1`, `…/0.5`, `…/v0.2`); no value is
    // invented beyond what the IRI itself encodes.
    for iri in ecaa_workflow_types::consts::REQUIRED_PROFILE_IRIS {
        graph.push(profile_entity(iri));
    }

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
        let task = &dag.tasks[id];
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
/// Re-inject the at-rest audit-proof `report`'s projected `InvariantVerdict`
/// nodes into the on-disk descriptor at `root`, so the descriptor's embedded
/// verdicts EQUAL the authoritative `runtime/audit-proof-report.json`.
///
/// Runs POST-EXECUTION, AFTER finalize regenerates the report. The emit-time
/// embedded verdicts were computed before execution / the signed sink / table
/// registration, so they drift from the recomputed report; this idempotent
/// re-injection (replace-by-`@id`) reconciles them. No-op `Ok(())` when the
/// descriptor is absent or the report carries no verdicts. The caller re-seals
/// the BagIt manifest afterward (the descriptor is a manifested file).
pub fn reinject_audit_proof_verdicts(
    root: &std::path::Path,
    report: &Value,
) -> Result<()> {
    let descriptor = root.join("ro-crate-metadata.json");
    let Ok(bytes) = std::fs::read(&descriptor) else {
        return Ok(());
    };
    let mut doc: Value = serde_json::from_slice(&bytes)?;
    inject_audit_proof_verdict_nodes(&mut doc, report)?;
    let serialized = serde_json::to_vec_pretty(&doc)?;
    crate::fs_helpers::atomic_write_bytes_sync(&descriptor, &serialized)?;
    Ok(())
}

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

/// Build a real executor agent entity for a CreateAction from the task's
/// recorded [`crate::container_state::ContainerState`].
///
/// The executor identity recorded at run time is the container that ran the
/// agent: its resolved `image` (`image:tag` / `image@sha256:digest`), the
/// `runtime` that ran it (docker/podman/apptainer), and the `backend`
/// (aws/slurm/local). We surface that as a `SoftwareApplication` agent keyed on
/// a deterministic `@id` derived from the recorded image so two finalizes of the
/// same package produce byte-identical nodes. Only recorded values populate the
/// entity — nothing is invented.
///
/// Returns `None` when the sidecar carries NO executor identity at all (image,
/// runtime, and backend all empty); the caller then omits the `agent` edge
/// rather than attaching a placeholder. (A real `endTime` may still be emitted
/// from `ended_at` in that case.)
fn executor_agent_entity(state: &crate::container_state::ContainerState) -> Option<Value> {
    let image = state.image.trim();
    let runtime = state.runtime.trim();
    let backend = state.backend.trim();
    if image.is_empty() && runtime.is_empty() && backend.is_empty() {
        return None;
    }
    // Deterministic, grapheme-safe local id from the recorded image (its most
    // specific identity); fall back to the runtime/backend when no image was
    // recorded.
    let key = if !image.is_empty() {
        image
    } else if !runtime.is_empty() {
        runtime
    } else {
        backend
    };
    let local: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut agent = json!({
        "@id": format!("#executor/{local}"),
        "@type": "SoftwareApplication",
        "name": if image.is_empty() {
            format!("Execution agent ({})", if runtime.is_empty() { backend } else { runtime })
        } else {
            format!("Execution agent — {image}")
        },
    });
    let obj = agent.as_object_mut().expect("agent is an object literal");
    if !image.is_empty() {
        obj.insert("softwareVersion".to_string(), json!(image));
    }
    if !runtime.is_empty() {
        obj.insert("runtimePlatform".to_string(), json!(runtime));
    }
    if !backend.is_empty() {
        obj.insert(
            "additionalProperty".to_string(),
            json!([{ "@type": "PropertyValue", "name": "backend", "value": backend }]),
        );
    }
    Some(agent)
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

    // Per-task input step ids, read off the `ParameterConnection` edges already
    // in the graph. Each connection's `targetParameter` is `#step-<task>#<port>`
    // and its `sourceParameter` is `#step-<source>#<port>`; the input `@id` we
    // attribute to the produced output's `CreateAction.object` is the bare
    // source step `#step-<source>`. BTreeMap + BTreeSet so the derived input
    // list is sorted and the emitted PROV is deterministic; we never invent
    // inputs — a task with no inbound ParameterConnection yields an empty list.
    let task_inputs = collect_task_inputs(graph);

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

    // Per-task produced-file index, keyed on the bare task token. Used to
    // resolve a downstream `CreateAction.object` (PROV `used`) to the CONCRETE
    // input File `@id`s — the upstream task's produced output tables — rather
    // than only the abstract `#step-<source>` reference. Built solely from the
    // discovered `rels`, so every resolved input is a real registered output
    // entity; never an invented path.
    let mut task_outputs: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for rel in &rels {
        if let Some((task, _)) = rel
            .strip_prefix("runtime/outputs/")
            .and_then(|r| r.split_once('/'))
        {
            task_outputs
                .entry(task.to_string())
                .or_default()
                .push(rel.clone());
        }
    }

    let mut new_parts: Vec<Value> = Vec::new();
    // Retrospective per-output PROV: one WRROC `CreateAction` per produced
    // table, accumulated here and appended AFTER the output nodes so the graph
    // stays in a stable (outputs, then actions; both in sorted `rels` order)
    // shape. Each output's `wasGeneratedBy` points at its action; the action's
    // `result` is the output, `instrument` is the producing step, and `object`
    // (PROV `used`) is the task's input step(s).
    let mut new_actions: Vec<Value> = Vec::new();
    // Executor agent entities referenced by the CreateActions' `agent` edge,
    // de-duplicated by `@id` (many tasks may share one container image →
    // executor). Appended after the actions so the graph stays append-stable.
    let mut new_agents: Vec<Value> = Vec::new();
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
        // Deterministic action id keyed on the full output path (unique per
        // produced table even when one task emits several).
        let action_id = format!("#action/{rel}");
        graph.push(json!({
            "@id": rel,
            "@type": ["File", "Dataset"],
            "name": format!("{task} — {file}"),
            "description": format!("Analytical result table produced by stage '{task}'."),
            "encodingFormat": fmt,
            "schema:about": {"@id": format!("#step-{task}")},
            "wasGeneratedBy": {"@id": action_id.clone()},
        }));
        // Input step ids derived from the task's ParameterConnection edges
        // (sorted; empty when the task has no inbound connection). Each input
        // step is resolved to the CONCRETE input File `@id`s it feeds in — the
        // upstream task's produced output tables — falling back to the bare
        // `#step-<source>` reference when that upstream produced no registered
        // table (so the dependency is never dropped). PROV `used` (`object`)
        // then names real File entities, not only abstract HowToSteps.
        let object: Vec<Value> = task_inputs
            .get(task)
            .map(|ins| {
                ins.iter()
                    .flat_map(|step| {
                        let src_task = step.strip_prefix("#step-").unwrap_or(step);
                        match task_outputs.get(src_task) {
                            Some(files) if !files.is_empty() => {
                                files.iter().map(|f| json!({"@id": f})).collect::<Vec<_>>()
                            }
                            // No registered upstream output File: keep the step
                            // reference rather than inventing a file path.
                            _ => vec![json!({"@id": step})],
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Real recorded executor + endTime from this task's
        // `runtime/outputs/<task>/.container-state.json`, when present. The
        // sidecar records `ended_at` (→ `endTime`) and the executor identity
        // (image / runtime / backend); there is NO recorded start timestamp, so
        // `startTime` is honestly omitted (see the module deferred note). When
        // the sidecar is absent or malformed, agent + endTime are omitted —
        // never fabricated.
        let task_dir = outputs_root.join(task);
        let cstate = crate::container_state::ContainerState::read_from_task_dir(&task_dir)
            .ok()
            .flatten();
        let mut action = json!({
            "@id": action_id,
            "@type": ["CreateAction", "prov:Activity"],
            "name": format!("Production of {file} by stage '{task}'."),
            "instrument": {"@id": format!("#step-{task}")},
            "result": {"@id": rel},
            "object": object,
        });
        if let Some(state) = &cstate {
            if !state.ended_at.is_empty() {
                action["endTime"] = json!(state.ended_at);
            }
            if let Some(agent) = executor_agent_entity(state) {
                let agent_id = agent
                    .get("@id")
                    .and_then(Value::as_str)
                    .expect("executor agent entity carries @id")
                    .to_string();
                action["agent"] = json!({"@id": agent_id});
                let already = graph
                    .iter()
                    .chain(new_agents.iter())
                    .any(|e| e.get("@id").and_then(Value::as_str) == Some(agent_id.as_str()));
                if !already {
                    new_agents.push(agent);
                }
            }
        }
        new_actions.push(action);
        new_parts.push(json!({"@id": rel}));
    }
    graph.extend(new_actions);
    graph.extend(new_agents);

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

/// Register the agent-written narrative report documents
/// (`runtime/outputs/<task>/*.md` — `report.md`, `final_report.md`,
/// `summary.md`, the literature protocols/verification reports) as
/// `CreativeWork` File entities in the RO-Crate `@graph`, linked from the
/// root Dataset's `hasPart`. Without this the human-readable answer is on
/// disk but absent from the provenance graph, so a curator or RO-Crate tool
/// following `ro-crate-metadata.json` never reaches it.
///
/// `mainEntity` (the workflow) is deliberately left unchanged so the package
/// stays a valid Workflow-Run-Crate. Idempotent: an `@id` already present is
/// skipped, so re-running after further task completions never duplicates.
/// Deterministic order (tasks then files sorted). Returns the count of
/// newly-registered reports; a no-op `Ok(0)` when there is no descriptor, no
/// `runtime/outputs/`, or every report is already registered.
pub fn register_report_documents(package_root: &std::path::Path) -> std::io::Result<usize> {
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
    let existing: std::collections::BTreeSet<String> = graph
        .iter()
        .filter_map(|e| e.get("@id").and_then(Value::as_str).map(String::from))
        .collect();

    // Discover top-level `*.md` reports per task dir, deterministically ordered.
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
            if let Ok(files) = std::fs::read_dir(&task_dir) {
                let mut names: Vec<String> = files
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.is_file()
                            && p.extension().and_then(|s| s.to_str()) == Some("md")
                    })
                    .filter_map(|p| {
                        p.file_name().and_then(|s| s.to_str()).map(String::from)
                    })
                    .collect();
                names.sort();
                for name in names {
                    rels.push(format!("runtime/outputs/{task}/{name}"));
                }
            }
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
        graph.push(json!({
            "@id": rel,
            "@type": ["File", "CreativeWork"],
            "name": format!("{task} — {file}"),
            "description": format!("Narrative report produced by stage '{task}'."),
            "encodingFormat": "text/markdown",
            "schema:about": {"@id": format!("#step-{task}")},
        }));
        new_parts.push(json!({"@id": rel}));
    }
    let added = new_parts.len();
    if added == 0 {
        return Ok(0);
    }

    // Link from the root Dataset's `hasPart` — the canonical RO-Crate 1.1
    // composition edge: these reports ARE files in the crate, so a walker
    // following `hasPart` reaches the human narrative. (`mainEntity` stays the
    // workflow; we deliberately do NOT use `mentions`, which would imply the
    // crate merely references an external work rather than containing it.)
    if let Some(root) = graph
        .iter_mut()
        .find(|e| e.get("@id").and_then(Value::as_str) == Some("./"))
    {
        if let Some(obj) = root.as_object_mut() {
            let slot = obj
                .entry("hasPart")
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(arr) = slot.as_array_mut() {
                for part in &new_parts {
                    let pid = part.get("@id");
                    if !arr.iter().any(|e| e.get("@id") == pid) {
                        arr.push(part.clone());
                    }
                }
            }
        }
    }

    let serialized = serde_json::to_vec_pretty(&doc)?;
    crate::fs_helpers::atomic_write_bytes_sync(&descriptor, &serialized)?;
    Ok(added)
}

/// Register the re-executability + accountability sidecar files that already
/// exist on disk as first-class `@graph` File/CreativeWork entities, linked
/// from the root Dataset's `hasPart`. Idempotent; presence-gated. Called from
/// finalize BEFORE the BagIt re-seal. Never fabricates: only registers files
/// that actually exist on disk.
///
/// Sidecars covered:
/// - `runtime/dependency-lock.json` — pinned R/Python/conda package versions
/// - `policies/runtime-prereqs.json` — base image + system/language packages
/// - `policies/container.json` — container image reference
/// - `runtime/reexecution.json` — per-artifact reproduction buckets
/// - `runtime/cost-ledger.jsonl` — per-step resource/cost accounting
///
/// Returns the count of newly-added entities (0 on a second/idempotent call).
pub fn register_reexecutability_sidecars(
    package_root: &std::path::Path,
) -> std::io::Result<usize> {
    // (rel_path, name, description, encodingFormat)
    const SIDECARS: &[(&str, &str, &str, &str)] = &[
        (
            "runtime/dependency-lock.json",
            "Dependency lock — requested R/Python/conda package versions",
            "Re-executability signal: the pinned dependency set for the run.",
            "application/json",
        ),
        (
            "policies/runtime-prereqs.json",
            "Runtime prerequisites — base image, system + language packages",
            "Re-executability signal: the runtime environment requirements.",
            "application/json",
        ),
        (
            "policies/container.json",
            "Container specification — execution image reference",
            "Re-executability signal: the container image used for execution.",
            "application/json",
        ),
        (
            "runtime/reexecution.json",
            "Re-execution equivalence report — per-artifact reproduction buckets",
            "Provenance: how re-executed outputs compared to the recorded run.",
            "application/json",
        ),
        (
            "runtime/cost-ledger.jsonl",
            "Cost ledger — per-step resource/cost accounting",
            "Accountability: recorded compute/cost ledger for the run.",
            "application/jsonl",
        ),
    ];

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
    let existing: std::collections::BTreeSet<String> = graph
        .iter()
        .filter_map(|e| e.get("@id").and_then(Value::as_str).map(String::from))
        .collect();

    let mut new_parts: Vec<Value> = Vec::new();
    for (rel, name, desc, fmt) in SIDECARS {
        if existing.contains(*rel) {
            continue;
        }
        if !package_root.join(rel).exists() {
            continue;
        }
        graph.push(json!({
            "@id": rel,
            "@type": ["File", "CreativeWork"],
            "name": name,
            "description": desc,
            "encodingFormat": fmt,
            "about": {"@id": "./"},
        }));
        new_parts.push(json!({"@id": rel}));
    }
    let added = new_parts.len();
    if added == 0 {
        return Ok(0);
    }

    // Link from the root Dataset's `hasPart` — the canonical RO-Crate 1.1
    // composition edge.
    if let Some(root) = graph
        .iter_mut()
        .find(|e| e.get("@id").and_then(Value::as_str) == Some("./"))
    {
        if let Some(obj) = root.as_object_mut() {
            let slot = obj
                .entry("hasPart")
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(arr) = slot.as_array_mut() {
                for part in &new_parts {
                    let pid = part.get("@id");
                    if !arr.iter().any(|e| e.get("@id") == pid) {
                        arr.push(part.clone());
                    }
                }
            }
        }
    }

    let serialized = serde_json::to_vec_pretty(&doc)?;
    crate::fs_helpers::atomic_write_bytes_sync(&descriptor, &serialized)?;
    Ok(added)
}

/// Render a human-readable `SNAPSHOTS.md` at the package root from the
/// per-task literature `evidence/manifest.json` files. Each row maps an
/// opaque content-addressed snapshot hash to its source (PMID / kind / role /
/// step), so a reviewer can audit the literature grounding without opening
/// every snapshot blob under `runtime/outputs/<step>/evidence/snapshots/`.
///
/// Deterministic (sorted rows). No-op `Ok(())` when no evidence manifests
/// exist, so non-literature packages emit nothing.
pub fn render_snapshots_md(package_root: &std::path::Path) -> std::io::Result<()> {
    let outputs_root = package_root.join("runtime").join("outputs");
    // (step, source, kind, class, role, snapshot_hash)
    let mut rows: Vec<(String, String, String, String, String, String)> = Vec::new();
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
            let manifest = task_dir.join("evidence").join("manifest.json");
            let Ok(bytes) = std::fs::read(&manifest) else {
                continue;
            };
            let Ok(doc) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            let Some(entries) = doc.get("entries").and_then(Value::as_array) else {
                continue;
            };
            for e in entries {
                let path = e.get("path").and_then(Value::as_str).unwrap_or("");
                let hash = path.rsplit('/').next().unwrap_or(path).to_string();
                let kind = e.get("source_kind").and_then(Value::as_str).unwrap_or("");
                let sref = e.get("source_ref").and_then(Value::as_str).unwrap_or("");
                let sref_kind = e
                    .get("source_ref_kind")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let class = e.get("source_class").and_then(Value::as_str).unwrap_or("");
                let role = e
                    .get("evidence_role")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let source = if sref.is_empty() {
                    "—".to_string()
                } else if sref_kind.is_empty() {
                    sref.to_string()
                } else {
                    format!("{sref_kind}:{sref}")
                };
                rows.push((
                    task.clone(),
                    source,
                    kind.to_string(),
                    class.to_string(),
                    role.to_string(),
                    hash,
                ));
            }
        }
    }
    if rows.is_empty() {
        return Ok(());
    }
    rows.sort();
    let mut md = String::from(
        "# Literature evidence snapshots\n\n\
Human-readable index of the content-addressed literature snapshots stored under \
`runtime/outputs/<step>/evidence/snapshots/`. Each row maps an opaque snapshot hash to \
its source so a reviewer can audit the literature grounding without opening every blob. \
Verify any snapshot with `sha256sum`.\n\n\
| Step | Source | Kind | Class | Role | Snapshot (sha256) |\n\
|---|---|---|---|---|---|\n",
    );
    for (task, source, kind, class, role, hash) in &rows {
        let short = if hash.len() > 16 {
            format!("{}…", &hash[..16])
        } else {
            hash.clone()
        };
        md.push_str(&format!(
            "| {task} | {source} | {kind} | {class} | {role} | `{short}` |\n"
        ));
    }
    crate::fs_helpers::atomic_write_bytes_sync(&package_root.join("SNAPSHOTS.md"), md.as_bytes())?;
    Ok(())
}

/// Map each workflow task to the sorted set of input step `@id`s feeding it,
/// read off the `ParameterConnection` entities already in the `@graph`.
///
/// A connection carries `sourceParameter {@id: "#step-<source>#<port>"}` and
/// `targetParameter {@id: "#step-<target>#<port>"}`. The target's bare task
/// token keys the map; the input `@id` recorded is the bare source step
/// (`#step-<source>`, the port fragment dropped). BTreeMap + BTreeSet keep the
/// derived per-task input list sorted so the emitted `CreateAction.object`
/// arrays are deterministic. Connections whose endpoints are not `#step-…`
/// references are ignored — inputs are never invented.
fn collect_task_inputs(
    graph: &[Value],
) -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
    let mut inputs: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    // Split `#step-<task>#<port>` (or `#step-<task>`) into the step `@id`
    // (`#step-<task>`) and the bare task token (`<task>`).
    let step_ref = |raw: &str| -> Option<(String, String)> {
        let body = raw.strip_prefix("#step-")?;
        let task = body.split('#').next().unwrap_or(body);
        if task.is_empty() {
            return None;
        }
        Some((format!("#step-{task}"), task.to_string()))
    };
    for node in graph {
        let is_connection = match node.get("@type") {
            Some(Value::String(s)) => s == "ParameterConnection",
            Some(Value::Array(a)) => a.iter().any(|t| t.as_str() == Some("ParameterConnection")),
            _ => false,
        };
        if !is_connection {
            continue;
        }
        let source = node
            .get("sourceParameter")
            .and_then(|p| p.get("@id"))
            .and_then(Value::as_str);
        let target = node
            .get("targetParameter")
            .and_then(|p| p.get("@id"))
            .and_then(Value::as_str);
        if let (Some(src), Some(tgt)) = (source, target) {
            if let (Some((src_step, _)), Some((_, tgt_task))) = (step_ref(src), step_ref(tgt)) {
                inputs.entry(tgt_task).or_default().insert(src_step);
            }
        }
    }
    inputs
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

/// Project pinned dependencies from `runtime/dependency-lock.json` into one
/// `SoftwareApplication` `@graph` node per package (deterministic `@id` of the
/// form `#dep/<ecosystem>/<name>`), then link all new nodes from the
/// `ComputationalWorkflow` entity's `softwareRequirements` array.
///
/// `softwareVersion` is set to the `resolved` exact version when present, the
/// `requested` range otherwise; omitted when both are absent. Ecosystems are
/// iterated in fixed alphabetical order (conda, python, r) for determinism.
/// Idempotent: skips any `@id` already present in the graph; returns 0 on the
/// second run. No-op (Ok(0)) when the lock file or descriptor is absent/unparseable.
/// Never fabricates a package not in the lock.
pub fn register_software_dependencies(
    package_root: &std::path::Path,
) -> std::io::Result<usize> {
    let lock_path = package_root.join("runtime/dependency-lock.json");
    let Ok(lock_bytes) = std::fs::read(&lock_path) else {
        return Ok(0);
    };
    let Ok(lock) = serde_json::from_slice::<Value>(&lock_bytes) else {
        return Ok(0);
    };
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
    // Collect existing @ids so we can skip duplicates (idempotency).
    let existing: std::collections::BTreeSet<String> = graph
        .iter()
        .filter_map(|e| e.get("@id").and_then(Value::as_str).map(String::from))
        .collect();

    let mut new_nodes: Vec<Value> = Vec::new();
    let mut req_ids: Vec<Value> = Vec::new();
    // Iterate ecosystems in fixed sorted order for deterministic output.
    for eco in ["conda", "python", "r"] {
        let Some(arr) = lock.get(eco).and_then(Value::as_array) else {
            continue;
        };
        for pkg in arr {
            let Some(name) = pkg.get("name").and_then(Value::as_str) else {
                continue;
            };
            let id = format!("#dep/{eco}/{name}");
            if existing.contains(&id) {
                continue;
            }
            // Prefer resolved (runtime exact) over requested (spec range).
            let version = pkg
                .get("resolved")
                .and_then(Value::as_str)
                .or_else(|| pkg.get("requested").and_then(Value::as_str));
            let mut node = json!({
                "@id": id,
                "@type": "SoftwareApplication",
                "name": name,
                "applicationCategory": eco,
            });
            if let Some(v) = version {
                node["softwareVersion"] = json!(v);
            }
            new_nodes.push(node);
            req_ids.push(json!({"@id": id}));
        }
    }

    let added = new_nodes.len();
    if added == 0 {
        return Ok(0);
    }
    for node in new_nodes {
        graph.push(node);
    }

    // Link new nodes from the ComputationalWorkflow entity's softwareRequirements.
    if let Some(wf) = graph.iter_mut().find(|e| {
        matches!(
            e.get("@type"),
            Some(Value::Array(a)) if a.iter().any(|v| v.as_str() == Some("ComputationalWorkflow"))
        )
    }) {
        if let Some(obj) = wf.as_object_mut() {
            let slot = obj
                .entry("softwareRequirements")
                .or_insert_with(|| json!([]));
            if let Some(arr) = slot.as_array_mut() {
                for r in &req_ids {
                    if !arr.iter().any(|e| e.get("@id") == r.get("@id")) {
                        arr.push(r.clone());
                    }
                }
            }
        }
    }

    crate::fs_helpers::atomic_write_bytes_sync(&descriptor, &serde_json::to_vec_pretty(&doc)?)?;
    Ok(added)
}

/// Annotate every `File` `@graph` entity whose `@id` resolves to a payload file
/// with `contentSize` (bytes) + `sha512` (hex). Excludes the metadata descriptor
/// (`ro-crate-metadata.json`) and BagIt tag files (cannot self-hash). Finalize-
/// time (off the byte-repro baseline); deterministic given fixed payload bytes.
pub fn register_content_integrity(package_root: &std::path::Path) -> std::io::Result<usize> {
    let hashes = crate::emitter::bagit::payload_hashes(
        package_root, crate::emitter::bagit::SealMode::Reseal)?;
    let descriptor = package_root.join("ro-crate-metadata.json");
    let Ok(bytes) = std::fs::read(&descriptor) else { return Ok(0); };
    let Ok(mut doc) = serde_json::from_slice::<Value>(&bytes) else { return Ok(0); };
    let Some(graph) = doc.get_mut("@graph").and_then(Value::as_array_mut) else { return Ok(0); };
    let mut annotated = 0usize;
    for e in graph.iter_mut() {
        let Some(id) = e.get("@id").and_then(Value::as_str).map(String::from) else { continue };
        if id == "ro-crate-metadata.json" || id.starts_with("manifest-")
            || id == "bagit.txt" || id.starts_with('#') || id == "./" { continue; }
        let is_file = match e.get("@type") {
            Some(Value::String(s)) => s == "File",
            Some(Value::Array(a)) => a.iter().any(|v| v.as_str() == Some("File")),
            _ => false,
        };
        if !is_file { continue; }
        if let Some((hex, size)) = hashes.get(&id) {
            if let Some(obj) = e.as_object_mut() {
                obj.insert("contentSize".into(), json!(size));
                obj.insert("sha512".into(), json!(hex));
                annotated += 1;
            }
        }
    }
    crate::fs_helpers::atomic_write_bytes_sync(&descriptor, &serde_json::to_vec_pretty(&doc)?)?;
    Ok(annotated)
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
    // Surface the human-readable narrative reports in the provenance graph and
    // index the literature snapshots. Both are additive + best-effort: a
    // failure must not abort the register + re-seal (the manifest must still
    // reconcile). Run BEFORE the re-seal so the updated descriptor +
    // SNAPSHOTS.md are hashed into the regenerated manifest.
    if let Err(e) = register_report_documents(root) {
        tracing::warn!(
            target: "ecaa::ro_crate",
            error = %e,
            "narrative-report registration failed"
        );
    }
    if let Err(e) = render_snapshots_md(root) {
        tracing::warn!(
            target: "ecaa::ro_crate",
            error = %e,
            "SNAPSHOTS.md render failed"
        );
    }
    // Register re-executability + accountability sidecars (dependency-lock,
    // runtime-prereqs, container, reexecution, cost-ledger) as first-class
    // @graph entities. Best-effort: a failure must not abort the re-seal.
    // Run BEFORE the re-seal so the updated descriptor is hashed into the
    // regenerated manifest.
    if let Err(e) = register_reexecutability_sidecars(root) {
        tracing::warn!(
            target: "ecaa::ro_crate",
            error = %e,
            "reexecutability sidecar registration failed"
        );
    }
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
    // Enumerate pinned dependencies from runtime/dependency-lock.json as
    // SoftwareApplication @graph entities, linked from the ComputationalWorkflow
    // via softwareRequirements. Run BEFORE register_content_integrity so the new
    // SoftwareApplication nodes (non-File typed) are not hash-annotated; they
    // must exist before the integrity pass so the descriptor written here is the
    // one hashed into the regenerated manifest. Best-effort.
    if let Err(e) = register_software_dependencies(root) {
        tracing::warn!(
            target: "ecaa::ro_crate",
            error = %e,
            "software dependency registration failed"
        );
    }
    // Annotate @graph File entities with contentSize + sha512. Best-effort:
    // a failure must not abort the re-seal.
    if let Err(e) = register_content_integrity(root) {
        tracing::warn!(
            target: "ecaa::ro_crate",
            error = %e,
            "content integrity annotation failed"
        );
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
        tasks.insert(crate::ids::TaskId::from("t1"), task);
        let mut dag = DAG {
            version: "1.0".into(),
            schema_version: crate::dag::current_dag_schema_version(),
            workflow_id: "test".into(),
            current_task: None,
            tasks,
            run_id: None,
            reverse_deps: std::collections::BTreeMap::new(),
            execution_order: Vec::new(),
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

    /// B1: the metadata descriptor AND the root `./` Dataset both declare the
    /// full normative `conformsTo` profile set, and every declared profile IRI
    /// resolves to a first-class `CreativeWork` profile entity (name + version)
    /// in the `@graph` — no bare dangling `{@id}` ref.
    #[test]
    fn root_dataset_conforms_to_and_profile_entities_resolve() {
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
        let graph = metadata["@graph"].as_array().expect("@graph array");

        // Root `./` carries conformsTo equal to the const profile set.
        let root = graph
            .iter()
            .find(|e| e["@id"].as_str() == Some("./"))
            .expect("root ./ Dataset present");
        let declared: Vec<&str> = root["conformsTo"]
            .as_array()
            .expect("root conformsTo is an array")
            .iter()
            .filter_map(|c| c["@id"].as_str())
            .collect();
        for iri in ecaa_workflow_types::consts::REQUIRED_PROFILE_IRIS {
            assert!(
                declared.contains(iri),
                "root ./ conformsTo must declare {iri}; got {declared:?}"
            );
            // Each declared profile resolves to a CreativeWork entity carrying a
            // name + version.
            let entity = graph
                .iter()
                .find(|e| e["@id"].as_str() == Some(iri))
                .unwrap_or_else(|| panic!("profile IRI {iri} must resolve to a @graph entity"));
            assert_eq!(
                entity["@type"].as_str(),
                Some("CreativeWork"),
                "profile entity {iri} must be a CreativeWork"
            );
            assert!(
                entity["name"].as_str().is_some_and(|s| !s.is_empty()),
                "profile entity {iri} must carry a non-empty name"
            );
            assert!(
                entity["version"].as_str().is_some_and(|s| !s.is_empty()),
                "profile entity {iri} must carry a non-empty version"
            );
        }
    }

    /// Guard: README.md must remain a first-class `File` entity in the `@graph`
    /// and must be listed in the root `./` `hasPart` array.  This is a
    /// characterisation/regression test — it must PASS immediately because the
    /// behaviour already exists (ro_crate.rs:195-202, hasPart :119).
    #[test]
    fn readme_is_registered_as_file_and_linked_from_haspart() {
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
        let graph = metadata["@graph"].as_array().expect("@graph array");

        // The README.md File entity must exist in the graph.
        let readme = graph
            .iter()
            .find(|e| e["@id"] == "README.md")
            .expect("README.md File entity must be present in @graph");

        // Its @type must include "File" (may be a string or an array).
        let types: Vec<&str> = match &readme["@type"] {
            serde_json::Value::String(s) => vec![s.as_str()],
            serde_json::Value::Array(a) => {
                a.iter().filter_map(|v| v.as_str()).collect()
            }
            _ => vec![],
        };
        assert!(types.contains(&"File"), "README.md must be typed as File; got {types:?}");

        // The root Dataset's hasPart must reference README.md.
        let root = graph
            .iter()
            .find(|e| e["@id"] == "./")
            .expect("root ./ Dataset must be present");
        let part_ids: Vec<&str> = root["hasPart"]
            .as_array()
            .expect("root hasPart must be an array")
            .iter()
            .filter_map(|p| p["@id"].as_str())
            .collect();
        assert!(
            part_ids.contains(&"README.md"),
            "README.md must be linked from root ./ hasPart; got {part_ids:?}"
        );
    }

    /// B1: `executor_agent_entity` is built only from RECORDED container-state
    /// fields, and returns `None` when no executor identity was recorded
    /// (honest omission rather than a fabricated placeholder).
    #[test]
    fn executor_agent_entity_uses_recorded_fields_only() {
        let state = crate::container_state::ContainerState {
            task_id: "differential_expression".into(),
            exit_code: 0,
            image: "ghcr.io/scripps/scripps-bio-base:1.4.4".into(),
            runtime: "docker".into(),
            session_id: "s".into(),
            backend: "aws".into(),
            ended_at: "2026-05-05T12:34:56Z".into(),
        };
        let agent = executor_agent_entity(&state).expect("recorded executor yields an agent");
        assert_eq!(agent["@type"].as_str(), Some("SoftwareApplication"));
        assert_eq!(
            agent["softwareVersion"].as_str(),
            Some("ghcr.io/scripps/scripps-bio-base:1.4.4"),
            "softwareVersion must be the recorded image verbatim"
        );
        assert_eq!(agent["runtimePlatform"].as_str(), Some("docker"));

        // No recorded identity at all → None (honest omission).
        let empty = crate::container_state::ContainerState {
            task_id: "t".into(),
            exit_code: 0,
            image: String::new(),
            runtime: String::new(),
            session_id: String::new(),
            backend: String::new(),
            ended_at: String::new(),
        };
        assert!(
            executor_agent_entity(&empty).is_none(),
            "no recorded executor identity must yield None, not a placeholder agent"
        );
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

    /// A production-shaped descriptor that registers a figure but NOT the
    /// narrative report, plus report `*.md` files on disk under two task dirs.
    fn write_package_with_reports(root: &std::path::Path) {
        let graph = json!({
            "@context": "https://w3id.org/ro/crate/1.1/context",
            "@graph": [
                {"@id": "ro-crate-metadata.json", "@type": "CreativeWork", "about": {"@id": "./"}},
                {"@id": "./", "@type": "Dataset", "hasPart": []},
            ]
        });
        std::fs::write(
            root.join("ro-crate-metadata.json"),
            serde_json::to_vec_pretty(&graph).unwrap(),
        )
        .unwrap();
        for (task, file) in [
            ("final_reporting", "final_report.md"),
            ("reporting", "report.md"),
            ("differential_expression", "summary.md"),
        ] {
            let dir = root.join(format!("runtime/outputs/{task}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(file), "# narrative\n").unwrap();
            // A non-md sibling must NOT be registered as a report.
            std::fs::write(dir.join("table.tsv"), "a\tb\n").unwrap();
        }
    }

    #[test]
    fn register_report_documents_links_narrative_into_graph_and_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        write_package_with_reports(root);

        let added = register_report_documents(root).unwrap();
        assert_eq!(added, 3, "the three narrative .md reports are registered");

        let doc: Value =
            serde_json::from_slice(&std::fs::read(root.join("ro-crate-metadata.json")).unwrap())
                .unwrap();
        let graph = doc["@graph"].as_array().unwrap();

        // Each report is a File + CreativeWork node, about its producing step,
        // markdown-typed.
        let rep = graph
            .iter()
            .find(|e| e["@id"].as_str() == Some("runtime/outputs/final_reporting/final_report.md"))
            .expect("final_report.md registered");
        let types: Vec<&str> = rep["@type"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(types.contains(&"File") && types.contains(&"CreativeWork"));
        assert_eq!(rep["encodingFormat"].as_str(), Some("text/markdown"));
        assert_eq!(
            rep["schema:about"]["@id"].as_str(),
            Some("#step-final_reporting")
        );

        // The non-md sibling is NOT registered.
        assert!(
            !graph
                .iter()
                .any(|e| e["@id"].as_str() == Some("runtime/outputs/reporting/table.tsv")),
            "non-md siblings must not be registered as reports"
        );

        // Linked from root hasPart (the canonical RO-Crate composition edge).
        let root_node = graph.iter().find(|e| e["@id"].as_str() == Some("./")).unwrap();
        let part_ids: Vec<&str> = root_node["hasPart"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["@id"].as_str())
            .collect();
        assert!(
            part_ids.contains(&"runtime/outputs/reporting/report.md"),
            "root hasPart links the report"
        );
        // `mentions` is deliberately NOT used (reports are part of the crate,
        // not external works it references).
        assert!(
            root_node.get("mentions").is_none(),
            "report registration must not add a `mentions` edge"
        );

        // Idempotent: a second pass adds nothing and does not duplicate.
        assert_eq!(register_report_documents(root).unwrap(), 0);
        let doc2: Value =
            serde_json::from_slice(&std::fs::read(root.join("ro-crate-metadata.json")).unwrap())
                .unwrap();
        let count = doc2["@graph"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["@id"].as_str() == Some("runtime/outputs/reporting/report.md"))
            .count();
        assert_eq!(count, 1, "no duplicate report nodes after re-run");
    }

    #[test]
    fn render_snapshots_md_indexes_literature_evidence() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let ev = root.join("runtime/outputs/review_prior_work/evidence");
        std::fs::create_dir_all(&ev).unwrap();
        let manifest = json!({
            "schema_version": 2,
            "entries": [
                {
                    "source_kind": "pubmed_abstract",
                    "source_ref_kind": "pmid",
                    "source_ref": "24926665",
                    "source_class": "primary_literature",
                    "evidence_role": "recommendation_or_benchmark",
                    "path": "snapshots/84c21d2fd1d32f25aa844203feb19f7b75fe77c39db1fb3c527977a08b10ee17"
                }
            ]
        });
        std::fs::write(ev.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();

        render_snapshots_md(root).unwrap();
        let md = std::fs::read_to_string(root.join("SNAPSHOTS.md")).unwrap();
        assert!(md.contains("# Literature evidence snapshots"));
        assert!(md.contains("review_prior_work"), "step column");
        assert!(md.contains("pmid:24926665"), "source column");
        assert!(md.contains("primary_literature"), "class column");
        // The hash is truncated to a 16-char prefix with an ellipsis.
        assert!(md.contains("`84c21d2fd1d32f25…`"), "truncated sha256");
    }

    #[test]
    fn render_snapshots_md_is_noop_without_evidence() {
        let tmp = tempfile::TempDir::new().unwrap();
        render_snapshots_md(tmp.path()).unwrap();
        assert!(
            !tmp.path().join("SNAPSHOTS.md").exists(),
            "no SNAPSHOTS.md when there is no literature evidence"
        );
    }

    #[test]
    fn content_integrity_injects_contentsize_and_sha512_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("WORKFLOW.json"), b"{\"x\":1}").unwrap();
        std::fs::write(dir.path().join("ro-crate-metadata.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "@context":"https://w3id.org/ro/crate/1.1/context",
                "@graph":[
                  {"@id":"ro-crate-metadata.json","@type":"CreativeWork","about":{"@id":"./"}},
                  {"@id":"./","@type":"Dataset","hasPart":[{"@id":"WORKFLOW.json"}]},
                  {"@id":"WORKFLOW.json","@type":["File","ComputationalWorkflow"],"name":"wf"}
                ]
            })).unwrap()).unwrap();
        let n = register_content_integrity(dir.path()).unwrap();
        assert_eq!(n, 1, "one payload File entity annotated (descriptor excluded)");
        let doc: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.path().join("ro-crate-metadata.json")).unwrap()).unwrap();
        let wf = doc["@graph"].as_array().unwrap().iter()
            .find(|e| e["@id"]=="WORKFLOW.json").unwrap();
        assert!(wf["contentSize"].as_u64().unwrap() >= 1);
        assert_eq!(wf["sha512"].as_str().unwrap().len(), 128);
        // descriptor must NOT carry its own hash (circular)
        let desc = doc["@graph"].as_array().unwrap().iter()
            .find(|e| e["@id"]=="ro-crate-metadata.json").unwrap();
        assert!(desc.get("sha512").is_none());
        // idempotent: second call returns same count, descriptor bytes unchanged
        let bytes_before = std::fs::read(dir.path().join("ro-crate-metadata.json")).unwrap();
        let n2 = register_content_integrity(dir.path()).unwrap();
        assert_eq!(n2, n, "second run returns same annotated count");
        let bytes_after = std::fs::read(dir.path().join("ro-crate-metadata.json")).unwrap();
        assert_eq!(bytes_before, bytes_after, "descriptor is byte-identical after second run");
    }

    #[test]
    fn reexecutability_sidecars_register_into_graph_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        // minimal descriptor with a root Dataset + empty hasPart
        std::fs::write(
            dir.path().join("ro-crate-metadata.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "@context": "https://w3id.org/ro/crate/1.1/context",
                "@graph": [
                    {"@id":"ro-crate-metadata.json","@type":"CreativeWork","about":{"@id":"./"}},
                    {"@id":"./","@type":"Dataset","hasPart":[]}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("runtime")).unwrap();
        std::fs::create_dir_all(dir.path().join("policies")).unwrap();
        std::fs::write(dir.path().join("runtime/dependency-lock.json"), b"{}").unwrap();
        std::fs::write(
            dir.path().join("policies/container.json"),
            b"{\"image\":\"x\"}",
        )
        .unwrap();

        let n = register_reexecutability_sidecars(dir.path()).unwrap();
        assert!(n >= 2, "registered at least dependency-lock + container");

        let doc: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.path().join("ro-crate-metadata.json")).unwrap(),
        )
        .unwrap();
        let graph = doc["@graph"].as_array().unwrap();
        let lock = graph
            .iter()
            .find(|e| e["@id"] == "runtime/dependency-lock.json")
            .unwrap();
        let types: Vec<&str> = lock["@type"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(types.contains(&"File"));
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let parts: Vec<&str> = root["hasPart"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["@id"].as_str())
            .collect();
        assert!(parts.contains(&"runtime/dependency-lock.json"));

        // idempotent
        let n2 = register_reexecutability_sidecars(dir.path()).unwrap();
        assert_eq!(n2, 0, "second run adds nothing");
    }

    #[test]
    fn software_dependencies_registered_from_lock_with_versions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("runtime")).unwrap();
        std::fs::write(
            dir.path().join("runtime/dependency-lock.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": "1",
                "r": [{"name": "DESeq2", "requested": ">=1.40", "resolved": "1.40.2"}],
                "python": [{"name": "scanpy"}],
                "conda": []
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("ro-crate-metadata.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "@context": "https://w3id.org/ro/crate/1.1/context",
                "@graph": [
                    {"@id": "ro-crate-metadata.json", "@type": "CreativeWork", "about": {"@id": "./"}},
                    {"@id": "./", "@type": "Dataset", "hasPart": []},
                    {"@id": "WORKFLOW.json", "@type": ["File", "ComputationalWorkflow"], "name": "wf"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let n = register_software_dependencies(dir.path()).unwrap();
        assert_eq!(n, 2, "DESeq2 + scanpy registered");

        let doc: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.path().join("ro-crate-metadata.json")).unwrap(),
        )
        .unwrap();
        let g = doc["@graph"].as_array().unwrap();

        // DESeq2 entity uses resolved version
        let deseq = g
            .iter()
            .find(|e| e.get("name").and_then(|v| v.as_str()) == Some("DESeq2"))
            .unwrap();
        assert_eq!(deseq["@type"], "SoftwareApplication");
        assert_eq!(deseq["softwareVersion"], "1.40.2", "resolved preferred over requested");
        assert_eq!(deseq["applicationCategory"], "r");

        // scanpy has no requested/resolved — no softwareVersion field
        let scanpy = g
            .iter()
            .find(|e| e.get("name").and_then(|v| v.as_str()) == Some("scanpy"))
            .unwrap();
        assert_eq!(scanpy["@type"], "SoftwareApplication");
        assert!(scanpy.get("softwareVersion").is_none(), "no version when both absent");

        // workflow links them via softwareRequirements
        let wf = g.iter().find(|e| e["@id"] == "WORKFLOW.json").unwrap();
        let reqs: Vec<&str> = wf["softwareRequirements"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["@id"].as_str())
            .collect();
        assert_eq!(reqs.len(), 2);
        assert!(reqs.contains(&"#dep/r/DESeq2"));
        assert!(reqs.contains(&"#dep/python/scanpy"));

        // idempotent: second run adds 0
        assert_eq!(register_software_dependencies(dir.path()).unwrap(), 0);
    }
}
