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

/// The `#ecaa-workflow` publisher Organization entity, recording the compiler
/// revision that authored the crate. On a `-dirty` build (uncommitted tree at
/// build time) it references a flattened `source_tree_dirty` PropertyValue so a
/// reader never mistakes a dirty build for a clean tagged one.
fn publisher_entity(source_commit: &str) -> Value {
    let mut publisher = json!({
        "@id": "#ecaa-workflow",
        "@type": "Organization",
        "name": "ecaa-workflow"
    });
    if source_commit != "unknown" {
        let obj = publisher
            .as_object_mut()
            .expect("publisher is a JSON object literal above");
        obj.insert("softwareVersion".to_string(), json!(source_commit));
        if source_commit.ends_with("-dirty") {
            obj.insert(
                "additionalProperty".to_string(),
                json!([{"@id": "#source-tree-dirty"}]),
            );
        }
    }
    publisher
}

fn source_tree_dirty_entity() -> Value {
    json!({
        "@id": "#source-tree-dirty",
        "@type": "PropertyValue",
        "name": "source_tree_dirty",
        "value": true
    })
}

/// Build the complete ro-crate-metadata.json JSON-LD graph.
///
/// When `dag.run_id` is `Some`, the root Dataset references a flattened
/// `package_run_id` PropertyValue so downstream RO-Crate consumers can
/// correlate packages by id.
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
        //
        // EXECUTION-AWARE `conformsTo`: this is the PRE-EXECUTION PLAN crate
        // (a workflow *definition* with ZERO executed `CreateAction`s), so it
        // declares ONLY the profiles it truthfully satisfies — base
        // RO-Crate 1.1, the WorkflowHub workflow-ro-crate 1.0 profile, and the
        // ECAA v0.2 profile (`PLAN_PROFILE_IRIS`). The three WRROC v0.5 run
        // profiles (process / workflow / provenance) all document *executed*
        // runs and require real run actions, so they are NOT claimed here;
        // the finalize/execution path adds them (`EXECUTED_ADDED_PROFILE_IRIS`)
        // once retrospective per-output `CreateAction`s are registered. We
        // never fabricate a run action to make a run profile pass.
        json!({
            "@id": "ro-crate-metadata.json",
            "@type": "CreativeWork",
            "conformsTo": ecaa_workflow_types::consts::PLAN_PROFILE_IRIS
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
            let emitted_at = clock.now_rfc3339();
            let mut root = serde_json::json!({
                "@id": "./",
                "@type": "Dataset",
                "name": format!("{} — {}", classification.domain, classification.workflow_description),
                "description": &classification.intake_text,
                "dateCreated": emitted_at.clone(),
                "datePublished": emitted_at.clone(),
                "license": "https://www.apache.org/licenses/LICENSE-2.0",
                // The root Dataset mirrors the metadata descriptor's
                // execution-aware `conformsTo`: the PLAN-crate profile subset
                // only (base RO-Crate 1.1, WorkflowHub workflow-ro-crate 1.0,
                // ECAA v0.2), so an RO-Crate validator that profiles off `./`
                // (the Workflow Run Crate validators do) sees the same
                // truthfully-declared profiles as one reading the descriptor.
                // The WRROC run profiles are added to BOTH on finalize. Each
                // IRI is also emitted as a first-class `CreativeWork` profile
                // entity below so the reference resolves rather than dangling.
                "conformsTo": ecaa_workflow_types::consts::PLAN_PROFILE_IRIS
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
                "mainEntity": {"@id": "WORKFLOW.json"},
                // DR-11 — link the deposit-readiness attestation as a related
                // CreativeWork via `mentions` (NOT `hasPart`): it is a mutable
                // meta file materialized by `export`, off the sealed payload
                // manifest, so `mentions` is the honest RO-Crate 1.1 edge.
                "mentions": [{"@id": "DEPOSIT-READINESS.json"}]
            });
            if dag.run_id.is_some() {
                let additional_property = serde_json::json!([{"@id": "#package-run-id"}]);
                root.as_object_mut()
                    .expect("root is a JSON object literal above")
                    .insert("additionalProperty".to_string(), additional_property);
            }
            root
        },
        // ComputationalWorkflow with Bioschemas profile
        json!({
            "@id": "WORKFLOW.json",
            "@type": ["File", "SoftwareSourceCode", "ComputationalWorkflow"],
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
        // Publisher — the ecaa-workflow compiler that authored this crate. The
        // source commit is baked in at build time (build.rs → the
        // `ECAA_SOURCE_COMMIT` compile-time const), so the emitted crate records
        // exactly which compiler revision produced it. The const is fixed per
        // binary, so this stays byte-reproducible across repeated emits.
        publisher_entity(env!("ECAA_SOURCE_COMMIT")),
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
        // DR-11 — the deposit-readiness attestation (`DEPOSIT-READINESS.json`,
        // at the deposit root, materialized by `export`) is represented as a
        // first-class `CreativeWork` audit entity in the graph so a deposit
        // consumer finds it in the provenance record rather than encountering
        // an unreferenced root-level mutable meta file. It is deliberately kept
        // OUT of the BagIt payload manifest (mutable, wall-clock `verified_at`)
        // and out of the root `hasPart` (not a sealed payload member); the
        // root Dataset links it via `mentions` below.
        json!({
            "@id": "DEPOSIT-READINESS.json",
            "@type": "CreativeWork",
            "name": "Deposit-readiness attestation",
            "description": "Self-validation verdict written by `ecaa-workflow export`: RO-Crate re-verify + flat-layout SHA-512 checksum integrity (Layer 1) and, for the re-executable profile, re-execution equivalence (Layer 2). Mutable meta file — excluded from the payload manifest by design.",
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

    if env!("ECAA_SOURCE_COMMIT").ends_with("-dirty") {
        graph.push(source_tree_dirty_entity());
    }
    if let Some(run_id) = &dag.run_id {
        graph.push(json!({
            "@id": "#package-run-id",
            "@type": "PropertyValue",
            "name": "package_run_id",
            "value": run_id
        }));
    }

    // Profile entities. Each `conformsTo` IRI declared on `ro-crate-metadata.json`
    // and on the root `./` Dataset is emitted as a first-class `CreativeWork`
    // so the reference resolves to a named, versioned entity rather than a bare
    // `{@id}` dangling ref. Name + version are parsed deterministically from the
    // IRI's trailing version segment (`…/1.1`, `…/0.5`, `…/v0.2`); no value is
    // invented beyond what the IRI itself encodes. Plan-crate only emits the
    // profiles it actually claims (`PLAN_PROFILE_IRIS`); the finalize/execution
    // path adds the WRROC run-profile entities alongside their `conformsTo`
    // references once real run actions exist.
    for iri in ecaa_workflow_types::consts::PLAN_PROFILE_IRIS {
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
            "https://w3id.org/ro/terms/workflow-run",
            // Inline term-map for extension keys emitted by this crate that
            // are not defined by either upstream context URL.  Without these
            // definitions the JSON-LD compaction step used by roc-validator's
            // "Validation of the compaction format of the file descriptor"
            // REQUIRED check fails.  Map each bare key to its canonical IRI:
            //   - wasGeneratedBy  → PROV-O
            //   - matchedArchetype / rationale → ECAA ns/0.2# vocabulary (same namespace as evaluated_against/verdict/invariant_id)
            //   - evaluated_against / verdict / invariant_id → ECAA ns vocabulary
            {
                "wasGeneratedBy": "http://www.w3.org/ns/prov#wasGeneratedBy",
                "matchedArchetype": "https://w3id.org/ecaa/ns/0.2#matchedArchetype",
                "rationale": "https://w3id.org/ecaa/ns/0.2#rationale",
                "evaluated_against": "https://w3id.org/ecaa/ns/0.2#evaluated_against",
                "verdict": "https://w3id.org/ecaa/ns/0.2#verdict",
                "invariant_id": "https://w3id.org/ecaa/ns/0.2#invariantId"
            }
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
/// Matches the flattened `package_run_id` / `additionalProperty` shape in
/// [`build_metadata`]: appends an `@id`-only reference and emits the
/// PropertyValue as a top-level graph entity. Re-stamping a graph that already
/// carries the marker is a no-op and re-emits remain byte-identical.
///
/// Called only when the caller has determined the chosen archetype is
/// experimental (`EmitConfig::experimental_archetype`); production
/// archetypes are never stamped, preserving their byte-baseline.
pub fn stamp_experimental_archetype(metadata: &mut Value) {
    let Some(graph) = metadata.get_mut("@graph").and_then(|g| g.as_array_mut()) else {
        return;
    };
    let stamp_id = "#archetype-maturity";
    let already = graph
        .iter()
        .any(|entity| entity.get("@id").and_then(Value::as_str) == Some(stamp_id));
    let Some(root_index) = graph
        .iter()
        .position(|e| e.get("@id").and_then(|v| v.as_str()) == Some("./"))
    else {
        return;
    };
    let root = &mut graph[root_index];
    let Some(root_obj) = root.as_object_mut() else {
        return;
    };

    let stamp_ref = json!({"@id": stamp_id});

    match root_obj
        .entry("additionalProperty".to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
    {
        Value::Array(props) => {
            let linked = props
                .iter()
                .any(|p| p.get("@id").and_then(Value::as_str) == Some(stamp_id));
            if !linked {
                props.push(stamp_ref);
            }
        }
        // `additionalProperty` exists but isn't an array (shouldn't
        // happen given `build_metadata`'s shape); normalize to an array.
        other => {
            *other = Value::Array(vec![stamp_ref]);
        }
    }
    if !already {
        graph.push(json!({
            "@id": stamp_id,
            "@type": "PropertyValue",
            "name": ARCHETYPE_MATURITY_PROPERTY,
            "value": ARCHETYPE_MATURITY_EXPERIMENTAL
        }));
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
pub fn reinject_audit_proof_verdicts(root: &std::path::Path, report: &Value) -> Result<()> {
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

/// Design §5.2 C5 — reconcile harness-observed reads (from
/// `runtime/invocations.jsonl`, parsed by the caller into
/// [`crate::provenance::ObservedRead`]s) against the declared per-edge
/// graph (`runtime/proofs.jsonl`'s [`crate::workflow_contracts::edge::EdgeContract`]
/// rows) and stamp the RO-Crate's `ParameterConnection` nodes
/// (`#parameter-connection/<from>__to__<to>`, built by
/// [`parameter_connection_entity`]) with the outcome.
///
/// For a one-of mutually-exclusive group (e.g. the
/// `differential_expression` `raw_counts`/`normalized_counts`
/// candidates, design §5.1 C1/C2), once observed reads resolve which
/// member was actually read (§G-B1), the standard structural graph must
/// show ONLY that authoritative edge for the group:
/// - the member whose producer output the task read is stamped
///   `"ecaax:provenanceStatus": "authoritative"` and KEPT;
/// - every OTHER member of the SAME resolved group is **dropped from the
///   standard graph** — its `ParameterConnection` node is removed (not
///   merely annotated), so a generic RO-Crate / WRROC / runcrate consumer
///   never reads the unread candidate as an authoritative data flow. The
///   dropped candidate is recorded ONLY in the root Dataset's
///   `ecaax:unusedCandidateEdge` side channel (from/to node+port, its
///   group, and `ecaax:supersededByProducer`), so an ecaax-aware consumer
///   still knows it was a declared alternative.
///
/// A one-of group whose authoritative member was NOT resolved this pass
/// (no member read, or the read diverged from both producers) keeps BOTH
/// members, each stamped `"candidate_unused"` — we never fabricate a
/// resolution and never drop a member we cannot rule out. An ordinary
/// (non-grouped), unread edge is left unstamped (silence is not evidence
/// either way).
///
/// Emit-time (pre-execution) behavior is preserved: with no observed reads
/// the function early-returns, so the compiled graph legitimately keeps
/// BOTH one-of members until a run resolves which was read.
///
/// A `Divergent` verdict — a read that matches no declared producer's
/// output for its task — is recorded on the root Dataset's
/// `ecaax:provenanceDivergence` array rather than silently dropped
/// (design §5.2: "a typed provenance violation"), AND returned to the
/// caller as a typed [`crate::provenance::DivergenceRecord`] list (T12) —
/// `crates/conversation/src/emit/ro_crate.rs::patch_ro_crate_metadata`'s
/// caller uses the return value to transition the offending task(s) to
/// `BlockerKind::ProvenanceDivergence`. The RO-Crate stamping above is
/// unconditional regardless of what the caller does with the return value.
///
/// Idempotent: re-running (e.g. a second re-emit after more reads land)
/// re-stamps by `@id`, never duplicates. No-op (empty return) when either
/// input is empty or the graph carries no matching `ParameterConnection`
/// node.
///
/// A task whose atom carries a `read_allowance` (RCA I-1 — e.g.
/// `final_reporting`'s dashboard aggregation, or a synthesized
/// `validate_<id>` companion that inherited its producer's allowance;
/// see `crate::atom::ReadAllowance`) never contributes to
/// `ecaax:provenanceDivergence` — its otherwise-divergent reads are
/// instead recorded as sanctioned, with their rationale, under
/// `ecaax:provenanceReadAllowance` on the root Dataset, so the
/// exception stays visible without being flagged as a violation.
///
/// Finally, every retrospective `CreateAction.object` (PROV `used`) is
/// restated as the union of the task's declared inputs and its observed
/// reads, each entry marked observed / declared-only / allowance-covered
/// — see [`rebuild_create_action_objects`].
pub fn reconcile_ro_crate_edges(
    metadata: &mut Value,
    declared_edges: &[crate::workflow_contracts::edge::EdgeContract],
    observed_reads: &[crate::provenance::ObservedRead],
) -> Vec<crate::provenance::DivergenceRecord> {
    reconcile_ro_crate_edges_with_allowances(
        metadata,
        declared_edges,
        observed_reads,
        &std::collections::BTreeMap::new(),
    )
}

/// Full form of [`reconcile_ro_crate_edges`] taking the per-task
/// declared `read_allowance` facets (keyed by task id, sourced from
/// `runtime/task-nodes.json`'s `TaskNode.attributes["read_allowance"]`
/// — see `crate::workflow_contracts::from_atom::preserve_attributes`).
pub fn reconcile_ro_crate_edges_with_allowances(
    metadata: &mut Value,
    declared_edges: &[crate::workflow_contracts::edge::EdgeContract],
    observed_reads: &[crate::provenance::ObservedRead],
    read_allowances: &std::collections::BTreeMap<String, Vec<crate::atom::ReadAllowance>>,
) -> Vec<crate::provenance::DivergenceRecord> {
    use crate::provenance::{
        classify_reconciled_edges, reconcile, DivergenceRecord, EdgeDisposition, ReconVerdict,
    };
    use std::collections::{BTreeMap, BTreeSet};

    if declared_edges.is_empty() || observed_reads.is_empty() {
        return Vec::new();
    }
    // Retries and re-executions can append the same read record more than
    // once. Reconciliation is about the set of files a task consumed, not the
    // number of times it opened them; duplicate observations must not mint
    // duplicate divergence/allowance nodes or duplicate typed blockers.
    let mut observed_keys: BTreeSet<(String, Option<String>, String)> = BTreeSet::new();
    let unique_observed_reads: Vec<crate::provenance::ObservedRead> = observed_reads
        .iter()
        .filter(|read| {
            observed_keys.insert((
                read.task_id.clone(),
                read.declared_port.clone(),
                read.path.clone(),
            ))
        })
        .cloned()
        .collect();
    let Some(graph) = metadata.get_mut("@graph").and_then(Value::as_array_mut) else {
        return Vec::new();
    };

    // Deterministic order (BTreeSet, not HashMap) so a rebuild of the
    // same on-disk inputs always stamps the graph identically.
    let mut task_ids: BTreeSet<&str> = BTreeSet::new();
    for r in &unique_observed_reads {
        task_ids.insert(r.task_id.as_str());
    }

    let mut divergences: Vec<Value> = Vec::new();
    let mut typed_divergences: Vec<DivergenceRecord> = Vec::new();
    // Sanctioned divergent reads, minted as first-class `@graph` entities
    // each carrying a deterministic `#read-allowance/<task>/<n>` `@id`. A
    // strict RO-Crate / runcrate validator rejects inline value objects with
    // no `@id`, so the root property references these nodes by `@id` rather
    // than inlining the value objects.
    let mut allowance_nodes: Vec<Value> = Vec::new();
    // Stamps to apply to KEPT `ParameterConnection` nodes: (from, to, status).
    let mut stamps: Vec<(String, String, &'static str)> = Vec::new();
    // `@id`s of unread one-of candidate `ParameterConnection`s to DROP from
    // the standard graph once their group's authoritative member is resolved.
    let mut drop_ids: BTreeSet<String> = BTreeSet::new();
    // The dropped candidates, recorded ONLY in the `ecaax:` side channel so an
    // ecaax-aware consumer still knows they were declared alternatives while a
    // generic RO-Crate / PROV / runcrate consumer sees only the authoritative
    // edge (§G-B1).
    let mut unused_candidates: Vec<Value> = Vec::new();
    // For each dropped unread one-of member: `(consuming_task, producer_task,
    // index into `unused_candidates`)`. On the SESSION path the consuming
    // task's retrospective `CreateAction` was registered DURING execution —
    // BEFORE this end-of-run drop — so its `object` (PROV `used`) still lists
    // the unread producer's output. After dropping the `ParameterConnection`
    // we ALSO prune that `used` entry so a generic runcrate/WRROC consumer
    // never reads the unread producer as an authoritative data flow (§G-B1).
    let mut object_prunes: Vec<(String, String, usize)> = Vec::new();
    // Per-task observed-read paths, and the subset of them a declared
    // `read_allowance` sanctions. Both feed the `CreateAction.object` (PROV
    // `used`) rebuild at the END of this pass, which restates every action's
    // `used` list as the union of what the task ACTUALLY read and what the
    // declared graph says it consumes, each entry marked with its provenance
    // status (see [`rebuild_create_action_objects`]).
    let mut observed_by_task: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for r in &unique_observed_reads {
        observed_by_task
            .entry(r.task_id.clone())
            .or_default()
            .insert(r.path.clone());
    }
    let mut allowance_covered: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for task_id in task_ids {
        let edges_for_task: Vec<&crate::workflow_contracts::edge::EdgeContract> = declared_edges
            .iter()
            .filter(|e| e.to_node == task_id)
            .collect();
        if edges_for_task.is_empty() {
            continue;
        }
        let owned_edges: Vec<_> = edges_for_task.iter().map(|e| (*e).clone()).collect();
        let verdicts = reconcile(&owned_edges, &unique_observed_reads, task_id);
        let allowances = read_allowances.get(task_id);

        let mut authoritative: BTreeSet<(String, String)> = BTreeSet::new();
        // Per-task 0-based index for the `#read-allowance/<task>/<n>`
        // fragment ids; the task loop iterates a BTreeSet so it is stable.
        let mut allowance_n: usize = 0;
        // Same, for the `#provenance-divergence/<task>/<n>` fragment ids.
        let mut divergence_n: usize = 0;
        for v in &verdicts {
            match v {
                ReconVerdict::Match { authoritative_edge } => {
                    authoritative.insert(authoritative_edge.clone());
                }
                ReconVerdict::Divergent {
                    read_path,
                    declared_producer,
                } => {
                    if let Some(rationale) = covering_rationale(allowances) {
                        allowance_nodes.push(json!({
                            "@id": format!("#read-allowance/{task_id}/{allowance_n}"),
                            "@type": ["ecaax:ProvenanceReadAllowance", "PropertyValue"],
                            "task_id": task_id,
                            "read_path": read_path,
                            "rationale": rationale,
                        }));
                        allowance_n += 1;
                        // Remember the sanctioned path so the `used` rebuild
                        // can mark this entry `allowanceCovered` rather than a
                        // plain observed read.
                        allowance_covered
                            .entry(task_id.to_string())
                            .or_default()
                            .insert(read_path.clone());
                    } else {
                        divergences.push(json!({
                            "@id": format!("#provenance-divergence/{task_id}/{divergence_n}"),
                            "@type": ["ecaax:ProvenanceDivergence", "PropertyValue"],
                            "task_id": task_id,
                            "read_path": read_path,
                            "declared_producer": declared_producer,
                        }));
                        divergence_n += 1;
                        typed_divergences.push(DivergenceRecord {
                            task_id: task_id.to_string(),
                            read_path: read_path.clone(),
                            declared_producer: declared_producer.clone(),
                        });
                    }
                }
                ReconVerdict::Untracked => {}
            }
        }

        // §G-B1 — once observed reads resolve which one-of member was read,
        // the standard graph must show ONLY that authoritative edge for the
        // group; the unread candidate is DROPPED from the standard
        // `ParameterConnection`s and recorded ONLY in the `ecaax:` side
        // channel. An unresolved group (no member read this pass) keeps both
        // as candidates — we never fabricate a resolution.
        let dispositions = classify_reconciled_edges(&owned_edges, &authoritative);
        for (edge, disposition) in edges_for_task.iter().zip(dispositions) {
            match disposition {
                EdgeDisposition::Authoritative => {
                    stamps.push((
                        edge.from_node.clone(),
                        edge.to_node.clone(),
                        "authoritative",
                    ));
                }
                EdgeDisposition::UnusedCandidate { superseded_by } => {
                    let node_id = parameter_connection_node_id(&edge.from_node, &edge.to_node);
                    drop_ids.insert(node_id.clone());
                    let record_index = unused_candidates.len();
                    unused_candidates.push(json!({
                        "@id": unused_candidate_edge_node_id(&edge.from_node, &edge.to_node),
                        "@type": ["ecaax:UnusedCandidateEdge", "PropertyValue"],
                        "task_id": task_id,
                        "from_node": edge.from_node,
                        "from_port": edge.from_port,
                        "to_node": edge.to_node,
                        "to_port": edge.to_port,
                        "mutually_exclusive_group": edge.mutually_exclusive_group,
                        "ecaax:provenanceStatus": "candidate_unused",
                        "ecaax:supersededByProducer": superseded_by,
                        "ecaax:droppedConnection": node_id,
                    }));
                    object_prunes.push((
                        edge.to_node.clone(),
                        edge.from_node.clone(),
                        record_index,
                    ));
                }
                EdgeDisposition::UnresolvedCandidate => {
                    stamps.push((
                        edge.from_node.clone(),
                        edge.to_node.clone(),
                        "candidate_unused",
                    ));
                }
                EdgeDisposition::Unobserved => {
                    // No evidence either way for an ordinary edge this pass
                    // didn't observe a read for — leave it unstamped.
                }
            }
        }
    }

    // Apply the kept-node stamps, then DROP the unread one-of candidates from
    // the standard graph (order matters only for clarity — the two node sets
    // are disjoint: a dropped `@id` never appears in `stamps`).
    for (from, to, status) in &stamps {
        stamp_parameter_connection_status(graph, from, to, status);
    }
    if !drop_ids.is_empty() {
        graph.retain(|node| {
            node.get("@id")
                .and_then(Value::as_str)
                .map(|id| !drop_ids.contains(id))
                .unwrap_or(true)
        });
    }

    // Having dropped the unread one-of members' `ParameterConnection`s, prune
    // the matching `used` entry from the CONSUMING task's `CreateAction.object`
    // so a generic runcrate/WRROC consumer sees only the authoritative
    // producer's output as a data flow (§G-B1). The producer→output mapping
    // mirrors `register_produced_output_tables`/`collect_task_inputs`: an
    // `object` entry references the unread producer either by its bare
    // `#step-<producer>` step or by a concrete `runtime/outputs/<producer>/…`
    // output file. Gated on a resolved one-of (only `UnusedCandidate` populates
    // `object_prunes`) and idempotent (a re-run finds the entry already gone).
    for (consuming_task, producer_task, record_index) in &object_prunes {
        let pruned =
            prune_unread_producer_from_create_action_objects(graph, consuming_task, producer_task);
        if !pruned.is_empty() {
            if let Some(obj) = unused_candidates[*record_index].as_object_mut() {
                obj.insert("ecaax:prunedUsedObject".to_string(), json!(pruned));
            }
        }
    }

    // Minimal port-alias map: let a reviewer resolve each declared task
    // input port name — including composer-synthesized positional names
    // (`companion_in_N` / `residual_in_N`) and atom-alias names — back to the
    // producer task + port it wires to. The linkage is read straight from the
    // declared `EdgeContract`s already in scope; no new data is plumbed, so
    // edam facets are intentionally omitted. Keyed (task, port) in a BTreeMap
    // so the emitted order is deterministic and any duplicate declaration
    // collapses to the smallest-sorting producer.
    let mut port_alias_by_port: BTreeMap<(String, String), (String, String)> = BTreeMap::new();
    for e in declared_edges {
        let key = (e.to_node.clone(), e.to_port.clone());
        let producer = (e.from_node.clone(), e.from_port.clone());
        match port_alias_by_port.get_mut(&key) {
            Some(existing) => {
                if producer < *existing {
                    *existing = producer;
                }
            }
            None => {
                port_alias_by_port.insert(key, producer);
            }
        }
    }
    let port_alias_nodes: Vec<Value> = port_alias_by_port
        .into_iter()
        .map(|((task, port), (from_node, from_port))| {
            json!({
                "@id": format!("#port-alias/{task}/{port}"),
                "@type": "ecaax:PortAlias",
                "task": task,
                "port": port,
                "from_node": from_node,
                "from_port": from_port,
            })
        })
        .collect();

    // Register the sanctioned-read, unused-candidate, and port-alias side
    // channels as first-class `@graph` entities (each carrying its own `@id`)
    // and reference them from the root Dataset by `@id`. A strict RO-Crate /
    // runcrate validator rejects value objects that carry no `@id`, so these
    // must be flattened nodes, not inline value objects on the root.
    let allowance_refs = upsert_side_channel_nodes(graph, allowance_nodes);
    let unused_refs = upsert_side_channel_nodes(graph, unused_candidates);
    let port_alias_refs = upsert_side_channel_nodes(graph, port_alias_nodes);
    // A genuine (allowance-uncovered) divergence is a side-channel node too: a
    // strict RO-Crate/runcrate validator rejects an inline value object with no
    // `@id`, so flatten each divergence into `@graph` (it already carries a
    // deterministic `#provenance-divergence/<task>/<n>` `@id`) and reference it
    // from the root by `@id`, exactly as the allowance/unused/port-alias
    // channels do. (Previously the divergences were inlined on the root, which
    // failed WRROC parseability → substrate_validity.)
    let divergence_refs = upsert_side_channel_nodes(graph, divergences);

    if let Some(root) = graph
        .iter_mut()
        .find(|e| e.get("@id").and_then(Value::as_str) == Some("./"))
    {
        if let Some(obj) = root.as_object_mut() {
            if !divergence_refs.is_empty() {
                obj.insert(
                    "ecaax:provenanceDivergence".to_string(),
                    Value::Array(divergence_refs),
                );
            }
            if !allowance_refs.is_empty() {
                obj.insert(
                    "ecaax:provenanceReadAllowance".to_string(),
                    Value::Array(allowance_refs),
                );
            }
            if !unused_refs.is_empty() {
                obj.insert(
                    "ecaax:unusedCandidateEdge".to_string(),
                    Value::Array(unused_refs),
                );
            }
            if !port_alias_refs.is_empty() {
                obj.insert(
                    "ecaax:portAliasMap".to_string(),
                    Value::Array(port_alias_refs),
                );
            }
        }
    }

    // Restate every retrospective `CreateAction.object` (PROV `used`) from the
    // reconciled evidence. Runs LAST so it sees the post-drop graph — a one-of
    // candidate this pass pruned can never be re-introduced — and so the
    // per-entry provenance nodes it appends keep a stable tail position across
    // repeated reconciles (the side-channel upserts above replace in place).
    rebuild_create_action_objects(graph, &observed_by_task, &allowance_covered);

    typed_divergences
}

/// The rationale of the first allowance in `allowances` whose scope
/// covers a divergent read. Only one scope exists today
/// (`AnyUpstreamStage`, which covers every divergent read
/// unconditionally), so this is just "first entry, if any" — written
/// as a search so a future narrower scope slots in without changing
/// the caller.
// `find_map` degrades to a plain `map` today because the single
// existing scope always matches — kept as a match-based search
// (rather than clippy's suggested `.map(..).next()`) so a future
// narrower `ReadAllowanceScope` variant that DOESN'T unconditionally
// cover a read slots in as a new match arm without restructuring the
// function.
#[allow(clippy::unnecessary_find_map)]
fn covering_rationale(allowances: Option<&Vec<crate::atom::ReadAllowance>>) -> Option<&str> {
    let allowances = allowances?;
    allowances.iter().find_map(|a| match a.scope {
        crate::atom::ReadAllowanceScope::AnyUpstreamStage => Some(a.rationale.as_str()),
    })
}

/// The `@id` of the task-level `ParameterConnection` node for the edge
/// `from_node -> to_node` (see [`parameter_connection_entity`]'s `@id`
/// scheme). Single source of truth so the stamp path and the §G-B1 drop
/// path address the same node.
fn parameter_connection_node_id(from_node: &str, to_node: &str) -> String {
    format!("#parameter-connection/{from_node}__to__{to_node}")
}

/// The `@id` of the `#unused-candidate-edge/…` side-channel node for the
/// dropped one-of edge `from_node -> to_node` (§G-B1). A deterministic
/// per-edge fragment so the node is a first-class, `@id`-bearing `@graph`
/// entity a strict RO-Crate / runcrate validator accepts, rather than an
/// inline value object with no `@id`.
fn unused_candidate_edge_node_id(from_node: &str, to_node: &str) -> String {
    format!("#unused-candidate-edge/{from_node}__to__{to_node}")
}

/// Register each side-channel `node` (which MUST carry an `@id`) as a
/// first-class `@graph` entity — replacing any existing node with the same
/// `@id` so repeated reconciles stay idempotent — and return an `{"@id": …}`
/// reference per input node, in input order. Callers reference these nodes
/// from a root-Dataset property by `@id` instead of inlining value objects,
/// which a strict RO-Crate / runcrate validator rejects ("no @id in {…}").
fn upsert_side_channel_nodes(graph: &mut Vec<Value>, nodes: Vec<Value>) -> Vec<Value> {
    let mut refs = Vec::with_capacity(nodes.len());
    for node in nodes {
        let Some(id) = node.get("@id").and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        let existing = graph
            .iter()
            .position(|e| e.get("@id").and_then(Value::as_str) == Some(id.as_str()));
        match existing {
            Some(pos) => graph[pos] = node,
            None => graph.push(node),
        }
        refs.push(json!({ "@id": id }));
    }
    refs
}

/// Stamp the `ParameterConnection` node for the task-level edge
/// `from_node -> to_node` (see [`parameter_connection_entity`]'s `@id`
/// scheme) with `"ecaax:provenanceStatus": status`. A no-op when no
/// such node exists in the graph (e.g. a legacy crate emitted before
/// Tier-3 `ParameterConnection` entities existed).
fn stamp_parameter_connection_status(
    graph: &mut [Value],
    from_node: &str,
    to_node: &str,
    status: &str,
) {
    let node_id = parameter_connection_node_id(from_node, to_node);
    if let Some(node) = graph
        .iter_mut()
        .find(|e| e.get("@id").and_then(Value::as_str) == Some(node_id.as_str()))
    {
        if let Some(obj) = node.as_object_mut() {
            obj.insert("ecaax:provenanceStatus".to_string(), json!(status));
        }
    }
}

/// Prune the unread one-of producer's output from the CONSUMING task's
/// retrospective `CreateAction.object` (PROV `used`).
///
/// The consuming task's `CreateAction`s are the ones whose `result` `@id`
/// lives under `runtime/outputs/<consuming_task>/` (the id scheme
/// [`register_produced_output_tables`] mints). Within each, an `object` entry
/// references the unread `producer_task` in exactly the two forms that
/// registrar emits: the bare `#step-<producer_task>` step reference, or a
/// concrete `runtime/outputs/<producer_task>/…` produced-output file. Both are
/// removed; the authoritative producer's output (a disjoint id prefix / a
/// different step) is untouched.
///
/// Returns the removed `@id`s (sorted, deduped) for the `ecaax:` side channel.
/// Idempotent — a re-run over an already-pruned graph matches nothing and
/// returns empty.
fn prune_unread_producer_from_create_action_objects(
    graph: &mut [Value],
    consuming_task: &str,
    producer_task: &str,
) -> Vec<String> {
    let result_prefix = format!("runtime/outputs/{consuming_task}/");
    let producer_step = format!("#step-{producer_task}");
    let producer_output_prefix = format!("runtime/outputs/{producer_task}/");
    let refers_to_producer =
        |id: &str| id == producer_step || id.starts_with(&producer_output_prefix);
    let mut pruned: Vec<String> = Vec::new();
    for node in graph.iter_mut() {
        let is_create_action = match node.get("@type") {
            Some(Value::String(s)) => s == "CreateAction",
            Some(Value::Array(a)) => a.iter().any(|t| t.as_str() == Some("CreateAction")),
            _ => false,
        };
        if !is_create_action {
            continue;
        }
        let belongs = node
            .get("result")
            .and_then(|r| r.get("@id"))
            .and_then(Value::as_str)
            .map(|id| id.starts_with(&result_prefix))
            .unwrap_or(false);
        if !belongs {
            continue;
        }
        let Some(object) = node.get_mut("object").and_then(Value::as_array_mut) else {
            continue;
        };
        object.retain(|entry| {
            let hit = entry
                .get("@id")
                .and_then(Value::as_str)
                .map(refers_to_producer)
                .unwrap_or(false);
            if hit {
                if let Some(id) = entry.get("@id").and_then(Value::as_str) {
                    pruned.push(id.to_string());
                }
            }
            !hit
        });
    }
    pruned.sort();
    pruned.dedup();
    pruned
}

/// `@id` prefix of the per-entry `CreateAction.object` provenance nodes
/// [`rebuild_create_action_objects`] mints. Doubles as the purge key that keeps
/// the rebuild exactly idempotent.
const OBJECT_PROVENANCE_ID_PREFIX: &str = "#object-provenance/";

/// Provenance-status marker for a `used` entry the consuming task WAS observed
/// to read (`runtime/outputs/<task>/reads.jsonl` → `ObservedRead`).
const OBJECT_STATUS_OBSERVED: &str = "ecaax:observed";

/// Marker for a `used` entry the DECLARED graph asserts but no observed read
/// confirms. Honest about the asymmetry rather than passing a compile-time
/// belief off as a recorded data flow.
const OBJECT_STATUS_DECLARED_ONLY: &str = "ecaax:declaredOnly";

/// Marker for an observed read the declared graph does NOT back, sanctioned by
/// the consuming task's declared `read_allowance` (see
/// [`crate::atom::ReadAllowance`] and `ecaax:provenanceReadAllowance`).
const OBJECT_STATUS_ALLOWANCE_COVERED: &str = "ecaax:allowanceCovered";

/// The `@id` of the side-channel node recording one `CreateAction.object`
/// entry's provenance status. Keyed on (consuming task, entry) — every action
/// of a task shares that task's `used` list, so the node is minted once per
/// task rather than once per produced output.
///
/// A step reference (`#step-<producer>`) carries a leading `#`, which may not
/// nest inside a fragment identifier, so it is stripped; produced-output ids
/// are package-relative paths and pass through unchanged.
fn object_provenance_node_id(task: &str, object_id: &str) -> String {
    let slug = object_id.strip_prefix('#').unwrap_or(object_id);
    format!("{OBJECT_PROVENANCE_ID_PREFIX}{task}/{slug}")
}

/// The task a retrospective `CreateAction` belongs to: the `<task>` segment of
/// its `result` `@id` under `runtime/outputs/<task>/…` (the id scheme
/// [`register_produced_output_tables`] mints). `None` for every other action —
/// the run-level `#workflow-run` (whose `result` is an ARRAY, not an object),
/// the compile-time SME actions, and the amendment `UpdateAction` — so none of
/// them is ever mistaken for a stage's production activity.
fn create_action_result_task(node: &Value) -> Option<String> {
    if !is_create_action_entity(node) {
        return None;
    }
    let result = node
        .get("result")
        .and_then(|r| r.get("@id"))
        .and_then(Value::as_str)?;
    let (task, file) = result.strip_prefix("runtime/outputs/")?.split_once('/')?;
    (!task.is_empty() && !file.is_empty()).then(|| task.to_string())
}

/// True when a `used` entry already references `producer` — either by the bare
/// `#step-<producer>` step or by one of its `runtime/outputs/<producer>/…`
/// files. Mirrors `prune_unread_producer_from_create_action_objects`'s
/// `refers_to_producer`: the two must agree on what "this entry is that
/// producer's data flow" means.
fn entry_refers_to_producer(entry: &str, producer: &str) -> bool {
    entry.strip_prefix("#step-") == Some(producer)
        || entry.starts_with(&format!("runtime/outputs/{producer}/"))
}

/// Registered produced-output `@id`s per producer task, read off the graph's
/// retrospective `CreateAction`s. This is the set of REAL entities a consumer's
/// `used` edge may name for that producer; a producer ABSENT here registered no
/// file at all, and only then may the abstract `#step-<producer>` reference
/// stand in for it. Every returned entry is non-empty by construction.
fn registered_producer_outputs(
    graph: &[Value],
) -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
    let mut out: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for node in graph {
        let Some(task) = create_action_result_task(node) else {
            continue;
        };
        let Some(result) = node
            .get("result")
            .and_then(|r| r.get("@id"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        out.entry(task).or_default().insert(result.to_string());
    }
    out
}

/// Append `id` to `used` unless it is already present (order-preserving
/// dedupe — the emitted `used` order must stay stable, so this is a `Vec`
/// rather than a set).
fn push_unique_used(used: &mut Vec<String>, id: &str) {
    if !used.iter().any(|e| e == id) {
        used.push(id.to_string());
    }
}

/// The `used` (`CreateAction.object`) entries each consuming task's actions
/// carry TODAY, first-seen order preserved and de-duplicated across a task's
/// several per-output actions (they all share the task's input list). A task
/// whose action carries no `object` still gets an entry, so the rebuild can
/// populate it from observed reads alone.
fn existing_used_by_task(graph: &[Value]) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut out: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for node in graph {
        let Some(task) = create_action_result_task(node) else {
            continue;
        };
        let entry = out.entry(task).or_default();
        let Some(objects) = node.get("object").and_then(Value::as_array) else {
            continue;
        };
        for o in objects {
            if let Some(id) = o.get("@id").and_then(Value::as_str) {
                push_unique_used(entry, id);
            }
        }
    }
    out
}

/// The reconciled `used` list for ONE consuming task: the union of the declared
/// inputs and the observed reads, in a deterministic order.
///
/// Three passes, each order-preserving so the emitted array is stable:
/// 1. the entries the action already names, with an abstract
///    `#step-<producer>` fallback COLLAPSED to that producer's real registered
///    files — the fallback is honest only while the producer registered nothing;
/// 2. declared producers (off the RECONCILED `ParameterConnection`s) the action
///    does not reference yet, resolved the same way; a bare step reference is
///    added only when it really exists in the graph, so `used` never dangles;
/// 3. observed reads that resolve to a registered entity. A read of an
///    unregistered path is deliberately NOT invented into `used` — it stays
///    visible through `ecaax:provenanceDivergence` / `ecaax:provenanceReadAllowance`
///    instead of minting a dangling reference.
fn union_used_entries(
    existing: &[String],
    declared_steps: Option<&std::collections::BTreeSet<String>>,
    observed: Option<&std::collections::BTreeSet<String>>,
    producer_outputs: &std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    entity_ids: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let mut used: Vec<String> = Vec::new();

    for id in existing {
        if let Some(producer) = id.strip_prefix("#step-") {
            if let Some(files) = producer_outputs.get(producer).filter(|f| !f.is_empty()) {
                for f in files {
                    push_unique_used(&mut used, f);
                }
                continue;
            }
        }
        push_unique_used(&mut used, id);
    }

    for step in declared_steps.into_iter().flatten() {
        let producer = step.strip_prefix("#step-").unwrap_or(step);
        if used.iter().any(|u| entry_refers_to_producer(u, producer)) {
            continue;
        }
        match producer_outputs.get(producer).filter(|f| !f.is_empty()) {
            Some(files) => {
                for f in files {
                    push_unique_used(&mut used, f);
                }
            }
            None => {
                if entity_ids.contains(step) {
                    push_unique_used(&mut used, step);
                }
            }
        }
    }

    for path in observed.into_iter().flatten() {
        if entity_ids.contains(path) {
            push_unique_used(&mut used, path);
        }
    }

    used
}

/// Restate every retrospective `CreateAction.object` (PROV `used`) as the UNION
/// of the task's DECLARED inputs and its OBSERVED reads, marking each entry with
/// its provenance status.
///
/// Without this the `used` array is a projection of the DECLARED graph alone —
/// `collect_task_inputs` mapped each inbound `ParameterConnection` to the
/// producer's registered output files, or to an abstract `#step-<source>` when
/// the producer had registered none — so a file the stage genuinely read never
/// appeared, while a declared-but-unread input appeared indistinguishably from
/// a real data flow. Comparing a run's `reads.jsonl` against its actions then
/// reconciles for no task.
///
/// The rebuild fixes both directions without ever inventing an entity:
/// * an observed read is added ONLY when it resolves to an `@id` already in the
///   `@graph` (never a fabricated File node, never a dangling reference);
/// * a declared input is kept, but marked [`OBJECT_STATUS_DECLARED_ONLY`] so a
///   reviewer can see no read corroborates it;
/// * the abstract `#step-<source>` reference survives ONLY for a producer that
///   registered no file at all.
///
/// The status itself rides a side-channel node per entry
/// (`#object-provenance/<task>/<entry>`, referenced from the action's
/// `ecaax:objectProvenance`) rather than being inlined onto the `object` entry:
/// RO-Crate requires `object` elements to stay bare `{"@id": …}` references, so
/// annotating them in place would break the flattened reference shape a strict
/// runcrate/WRROC consumer expects.
///
/// Deterministic (`BTreeMap`/`BTreeSet` keyed, order-preserving unions) and
/// idempotent: the previous pass's provenance nodes are purged before the fresh
/// set is appended, so a repeated reconcile converges on the same graph content
/// AND the same node order.
fn rebuild_create_action_objects(
    graph: &mut Vec<Value>,
    observed_by_task: &std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    allowance_covered: &std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
) {
    graph.retain(|node| {
        !node
            .get("@id")
            .and_then(Value::as_str)
            .map(|id| id.starts_with(OBJECT_PROVENANCE_ID_PREFIX))
            .unwrap_or(false)
    });

    let entity_ids: std::collections::BTreeSet<String> = graph
        .iter()
        .filter_map(|e| e.get("@id").and_then(Value::as_str).map(String::from))
        .collect();
    let producer_outputs = registered_producer_outputs(graph);
    // Declared producers read off the RECONCILED graph: the unread one-of
    // members' `ParameterConnection`s are already dropped, so a candidate this
    // pass pruned is never resurrected here.
    let declared_steps = collect_task_inputs(graph);
    let existing = existing_used_by_task(graph);

    // task -> [(used @id, provenance status)], in emitted order.
    let mut rebuilt: std::collections::BTreeMap<String, Vec<(String, &'static str)>> =
        std::collections::BTreeMap::new();
    for (task, existing_used) in &existing {
        let observed = observed_by_task.get(task);
        let covered = allowance_covered.get(task);
        let marked = union_used_entries(
            existing_used,
            declared_steps.get(task),
            observed,
            &producer_outputs,
            &entity_ids,
        )
        .into_iter()
        .map(|id| {
            let status = if covered.is_some_and(|c| c.contains(&id)) {
                OBJECT_STATUS_ALLOWANCE_COVERED
            } else if observed.is_some_and(|o| o.contains(&id)) {
                OBJECT_STATUS_OBSERVED
            } else {
                OBJECT_STATUS_DECLARED_ONLY
            };
            (id, status)
        })
        .collect();
        rebuilt.insert(task.clone(), marked);
    }

    for node in graph.iter_mut() {
        let Some(task) = create_action_result_task(node) else {
            continue;
        };
        let Some(entries) = rebuilt.get(&task) else {
            continue;
        };
        let Some(obj) = node.as_object_mut() else {
            continue;
        };
        obj.insert(
            "object".to_string(),
            Value::Array(entries.iter().map(|(id, _)| json!({"@id": id})).collect()),
        );
        if entries.is_empty() {
            obj.remove("ecaax:objectProvenance");
        } else {
            obj.insert(
                "ecaax:objectProvenance".to_string(),
                Value::Array(
                    entries
                        .iter()
                        .map(|(id, _)| json!({"@id": object_provenance_node_id(&task, id)}))
                        .collect(),
                ),
            );
        }
    }

    for (task, entries) in &rebuilt {
        for (id, status) in entries {
            graph.push(json!({
                "@id": object_provenance_node_id(task, id),
                "@type": ["ecaax:ObjectProvenance", "PropertyValue"],
                "task_id": task,
                "object": {"@id": id},
                "ecaax:provenanceStatus": status,
            }));
        }
    }
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
/// Returns the agent and its optional flattened backend `PropertyValue`.
/// Returns `None` when the sidecar carries no executor identity at all (image,
/// runtime, and backend all empty); the caller then omits the `agent` edge
/// rather than attaching a placeholder. A real `endTime` may still be emitted
/// from `ended_at` in that case.
fn executor_agent_entities(
    state: &crate::container_state::ContainerState,
) -> Option<(Value, Option<Value>)> {
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
    let agent_id = format!("#executor/{local}");
    let mut agent = json!({
        "@id": agent_id.clone(),
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
    let backend_entity = if backend.is_empty() {
        None
    } else {
        let backend_id = format!("{agent_id}/backend");
        obj.insert(
            "additionalProperty".to_string(),
            json!([{"@id": backend_id}]),
        );
        Some(json!({
            "@id": backend_id,
            "@type": "PropertyValue",
            "name": "backend",
            "value": backend
        }))
    };
    Some((agent, backend_entity))
}

/// Does the `@graph` carry at least one REAL executed run `CreateAction`?
///
/// "Real run action" = an entity typed `CreateAction` (the retrospective
/// per-output actions [`register_produced_output_tables`] appends, each with a
/// real `instrument`). The compile-time SME resolution entities are typed plain
/// `Action` (not `CreateAction`) and are deliberately NOT counted — they record
/// intake-time decisions, not executed workflow steps. The presence of a real
/// `CreateAction` is the truthful precondition for a crate to claim the WRROC
/// run profiles (process / workflow / provenance run crate).
fn graph_has_run_create_action(graph: &[Value]) -> bool {
    graph.iter().any(|e| {
        let is_create_action = match e.get("@type") {
            Some(Value::String(s)) => s == "CreateAction",
            Some(Value::Array(a)) => a.iter().any(|t| t.as_str() == Some("CreateAction")),
            _ => false,
        };
        is_create_action
            && e.get("instrument")
                .and_then(|i| i.get("@id"))
                .and_then(Value::as_str)
                .is_some()
    })
}

/// Upgrade an executed crate's `conformsTo` from the plan-set to the full
/// executed set, idempotently.
///
/// Adds each [`EXECUTED_ADDED_PROFILE_IRIS`] entry that is not already present
/// to BOTH the metadata descriptor's and the root `./` Dataset's `conformsTo`
/// (mirroring the plan-crate dual declaration), and emits the corresponding
/// first-class `CreativeWork` profile entity for any newly-added IRI so the
/// reference resolves rather than dangling. Called ONLY when the graph already
/// contains a real run `CreateAction` ([`graph_has_run_create_action`]) — so
/// the added run profiles are truthful, never fabricated. Idempotent: a
/// second invocation on an already-upgraded graph is a no-op (the IRIs and
/// profile entities are already present).
fn upgrade_conforms_to_executed(graph: &mut Vec<Value>) {
    let add_iris = ecaa_workflow_types::consts::EXECUTED_ADDED_PROFILE_IRIS;

    // Which executed-add IRIs are missing from the descriptor's conformsTo?
    let mut needs_profile_entity: Vec<&str> = Vec::new();
    for target_id in ["ro-crate-metadata.json", "./"] {
        let Some(entry) = graph
            .iter_mut()
            .find(|e| e.get("@id").and_then(Value::as_str) == Some(target_id))
        else {
            continue;
        };
        let Some(obj) = entry.as_object_mut() else {
            continue;
        };
        let conforms = obj
            .entry("conformsTo")
            .or_insert_with(|| Value::Array(Vec::new()));
        let Some(arr) = conforms.as_array_mut() else {
            continue;
        };
        let present: std::collections::BTreeSet<String> = arr
            .iter()
            .filter_map(|c| c.get("@id").and_then(Value::as_str).map(String::from))
            .collect();
        for iri in add_iris {
            if !present.contains(*iri) {
                arr.push(json!({"@id": iri}));
                // Track once (descriptor pass) for profile-entity emission.
                if target_id == "ro-crate-metadata.json" {
                    needs_profile_entity.push(iri);
                }
            }
        }
    }

    // Emit a resolving profile entity for each newly-claimed IRI not already
    // present as a node, so the `conformsTo` ref does not dangle.
    for iri in needs_profile_entity {
        let already = graph
            .iter()
            .any(|e| e.get("@id").and_then(Value::as_str) == Some(iri));
        if !already {
            graph.push(profile_entity(iri));
        }
    }
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
/// A stage's primary recorded output, in attribution-priority order. Used both
/// to detect that an agent-orchestrated stage actually RAN (so it earns a
/// synthesized executor tool) and to pick the `result` of its production
/// `CreateAction`. Every entry is retained by all deposit profiles.
const PRIMARY_STAGE_OUTPUTS: [&str; 5] = [
    "result.json",
    "decision.json",
    "final_report.md",
    "report.md",
    "manifest.json",
];

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

    // Every EXECUTED stage = each `#step-<task>` HowToStep in the graph (table
    // producers are a subset). Provenance Run Crate couples two MUSTs — every
    // HowToStep names a tool (`must/1_howtostep.ttl` "HowToStep workExample")
    // AND every tool is the `instrument` of a CreateAction (`must/0_tool.ttl`
    // "Tool inverse instrument"). We use this set to (a) give EVERY stage a tool
    // and (b) record a production `CreateAction` for the agent-orchestrated
    // stages (validation / discovery / reporting) that emit no V `Table`, over
    // the real primary output each one recorded — so both MUSTs hold without
    // inventing any artifact.
    let executed_tasks: std::collections::BTreeSet<String> = {
        let is_step = |e: &Value| match e.get("@type") {
            Some(Value::String(s)) => s == "HowToStep",
            Some(Value::Array(a)) => a.iter().any(|t| t.as_str() == Some("HowToStep")),
            _ => false,
        };
        let mut s: std::collections::BTreeSet<String> = graph
            .iter()
            .filter(|e| is_step(e))
            .filter_map(|e| e.get("@id").and_then(Value::as_str))
            .filter_map(|id| id.strip_prefix("#step-").map(str::to_string))
            .collect();
        for rel in &rels {
            if let Some((task, _)) = rel
                .strip_prefix("runtime/outputs/")
                .and_then(|r| r.split_once('/'))
            {
                s.insert(task.to_string());
            }
        }
        s
    };

    // Per-task EXECUTED TOOL entities. A WRROC tool-execution `CreateAction`
    // documents the run of a *tool*: its `instrument` MUST name a
    // SoftwareApplication / SoftwareSourceCode / ComputationalWorkflow
    // (process/workflow/provenance-run-crate "Action instrument"), NOT the
    // abstract `HowToStep`. The concrete tool a step ran is the REAL code it
    // authored under `runtime/outputs/<task>/scripts/` (the executor brief
    // mandates every executed line land there). We register those scripts as
    // `SoftwareSourceCode` File entities and group them under one per-task tool
    // entity `#tool/<task>` (also `SoftwareSourceCode`) whose `hasPart` lists
    // the real scripts. The CreateActions below name this tool via `instrument`,
    // the workflow `hasPart`s it, and each HowToStep's `workExample` points at
    // it — all REAL graph entities, never synthetic placeholders. A task with NO
    // recorded script gets no tool entity (we never invent code that did not
    // run); its output's CreateAction then falls back to the HowToStep
    // `instrument` (honest, though it leaves the optional shape unsatisfied for
    // that single action rather than fabricating a tool).
    //
    // `tool_for_task[task] = Some("#tool/<task>")` when the task has ≥1 real
    // script; the tool + its script File entities are accumulated in
    // `new_tools` / `new_scripts` and appended after the actions.
    let mut tool_for_task: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let mut new_tools: Vec<Value> = Vec::new();
    let mut new_scripts: Vec<Value> = Vec::new();
    {
        // Iterate EVERY executed stage (sorted) for deterministic emission.
        for task in &executed_tasks {
            let tool_id = format!("#tool/{task}");
            if existing.contains(&tool_id) {
                // Already registered by a prior finalize; still record the
                // mapping so this run's CreateActions point at the real tool.
                tool_for_task.insert(task.clone(), tool_id);
                continue;
            }
            let scripts_dir = outputs_root.join(task).join("scripts");
            let mut script_rels: Vec<String> = Vec::new();
            collect_task_scripts(
                &scripts_dir,
                &format!("runtime/outputs/{task}/scripts"),
                &mut script_rels,
            );
            script_rels.sort();
            if script_rels.is_empty() {
                // No recorded script. Only stages that ACTUALLY RAN — produced a
                // registered output table OR a primary recorded output on disk —
                // get a synthesized executor. A PRE-EXECUTION PLAN crate, whose
                // steps have no outputs yet, gets NO tool: it stays a clean
                // workflow *definition* and never claims an executor that has not
                // run (this also keeps plan-emit byte-deterministic).
                let ran = task_outputs.contains_key(task)
                    || PRIMARY_STAGE_OUTPUTS
                        .iter()
                        .any(|f| outputs_root.join(task).join(f).is_file());
                if !ran {
                    continue;
                }
                // This stage ran agent-orchestrated (validation / discovery /
                // reporting). Register a SoftwareApplication EXECUTOR —
                // deliberately NOT SoftwareSourceCode, so NO source artifact is
                // claimed — so the step's `workExample` resolves and the stage's
                // production `CreateAction` (recorded below over its real
                // recorded output) has a resolvable `instrument`. The stage
                // genuinely ran and recorded an output, so this materialises its
                // executor rather than inventing connectivity.
                new_tools.push(json!({
                    "@id": tool_id,
                    "@type": "SoftwareApplication",
                    "name": format!("{task} stage executor"),
                    "description": format!(
                        "Agent-orchestrated executor that ran the '{task}' stage of the \
                         ECAA workflow (no standalone source artifact recorded)."
                    ),
                }));
                tool_for_task.insert(task.clone(), tool_id);
                continue;
            }
            let mut script_refs: Vec<Value> = Vec::new();
            for srel in &script_rels {
                if !existing.contains(srel) {
                    new_scripts.push(json!({
                        "@id": srel,
                        "@type": ["File", "SoftwareSourceCode"],
                        "name": srel.rsplit('/').next().unwrap_or(srel),
                        "description": format!("Executed script authored by stage '{task}'."),
                        "encodingFormat": script_encoding_format(srel),
                    }));
                }
                script_refs.push(json!({"@id": srel}));
            }
            new_tools.push(json!({
                "@id": tool_id,
                "@type": "SoftwareSourceCode",
                "name": format!("{task} tool"),
                "description": format!(
                    "The executable code stage '{task}' ran, grouping its recorded scripts."
                ),
                "hasPart": script_refs,
            }));
            tool_for_task.insert(task.clone(), tool_id);
        }
    }

    let mut new_parts: Vec<Value> = Vec::new();
    // Retrospective per-output PROV: one WRROC `CreateAction` per produced
    // table, accumulated here and appended AFTER the output nodes so the graph
    // stays in a stable (outputs, then actions; both in sorted `rels` order)
    // shape. Each output's `wasGeneratedBy` points at its action; the action's
    // `result` is the output, `instrument` is the producing tool (the REAL
    // per-task code under `scripts/`, falling back to the `HowToStep` only when
    // a task recorded no script), and `object` (PROV `used`) is the task's
    // input step(s).
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
        // `instrument` = the REAL tool the step ran (its recorded code), so the
        // action documents a tool execution per WRROC "Action instrument".
        // Falls back to the abstract `HowToStep` only when the task recorded no
        // script (we never invent a tool that did not run).
        let instrument_id = tool_for_task
            .get(task)
            .cloned()
            .unwrap_or_else(|| format!("#step-{task}"));
        let mut action = json!({
            "@id": action_id,
            "@type": ["CreateAction", "prov:Activity"],
            "name": format!("Production of {file} by stage '{task}'."),
            "instrument": {"@id": instrument_id},
            "result": {"@id": rel},
            "object": object,
        });
        if let Some(state) = &cstate {
            if !state.ended_at.is_empty() {
                action["endTime"] = json!(state.ended_at);
            }
            if let Some((agent, backend_entity)) = executor_agent_entities(state) {
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
                    if let Some(entity) = backend_entity {
                        new_agents.push(entity);
                    }
                }
            }
        }
        new_actions.push(action);
        new_parts.push(json!({"@id": rel}));
    }

    // ── Production CreateActions for the NON-TABLE (agent-orchestrated) stages ─
    //
    // The loop above covers every stage that produced a V `Table`. The
    // validation / discovery / reporting stages produce no table, so their
    // executor tool would have no `instrument`-side action ("Tool inverse
    // instrument" in `must/0_tool.ttl`). Each such stage DID record a real
    // primary output — its `result.json` execution manifest (or, in priority
    // order, `decision.json` / `final_report.md` / `report.md` / `manifest.json`)
    // — every one retained by all deposit profiles. Register that output File +
    // a `CreateAction` (`instrument` = the stage executor, `result` = that real
    // file). Nothing is fabricated: the file exists on disk and the stage
    // produced it. Skip stages that already have a table action.
    for task in &executed_tasks {
        if task_outputs.contains_key(task) {
            continue;
        }
        let Some(tool_id) = tool_for_task.get(task) else {
            continue;
        };
        let task_dir = outputs_root.join(task);
        let Some(file) = PRIMARY_STAGE_OUTPUTS
            .iter()
            .copied()
            .find(|f| task_dir.join(f).is_file())
        else {
            // The stage recorded no primary output to attribute — never invent
            // one; its step keeps no production action (Provenance then flags it
            // honestly rather than fabricating).
            continue;
        };
        let rel = format!("runtime/outputs/{task}/{file}");
        if existing.contains(&rel) {
            continue;
        }
        let action_id = format!("#action/{rel}");
        let fmt = if file.ends_with(".json") {
            "application/json"
        } else {
            "text/markdown"
        };
        graph.push(json!({
            "@id": rel,
            "@type": ["File"],
            "name": format!("{task} — {file}"),
            "description": format!("Primary recorded output of stage '{task}'."),
            "encodingFormat": fmt,
            "schema:about": {"@id": format!("#step-{task}")},
            "wasGeneratedBy": {"@id": action_id.clone()},
        }));
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
                            _ => vec![json!({"@id": step})],
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut action = json!({
            "@id": action_id,
            "@type": ["CreateAction", "prov:Activity"],
            "name": format!("Execution of stage '{task}' producing {file}."),
            "instrument": {"@id": tool_id},
            "result": {"@id": rel},
            "object": object,
        });
        let cstate = crate::container_state::ContainerState::read_from_task_dir(&task_dir)
            .ok()
            .flatten();
        if let Some(state) = &cstate {
            if !state.ended_at.is_empty() {
                action["endTime"] = json!(state.ended_at);
            }
            if let Some((agent, backend_entity)) = executor_agent_entities(state) {
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
                    if let Some(entity) = backend_entity {
                        new_agents.push(entity);
                    }
                }
            }
        }
        new_actions.push(action);
        new_parts.push(json!({"@id": rel}));
    }

    graph.extend(new_actions);
    graph.extend(new_agents);
    graph.extend(new_scripts);
    graph.extend(new_tools);

    // ── Wire the executed tools into the workflow + steps (REAL entities) ────
    //
    // Provenance Run Crate requires, for the executed crate:
    //   * ComputationalWorkflow `hasPart` → orchestrated tools
    //     (`must/0_computational_workflow.ttl` "ComputationalWorkflow hasPart").
    //   * the workflow that links HowToStep via `step` ALSO carries the `HowTo`
    //     `@type` ("ComputationalWorkflow with steps type").
    //   * every HowToStep `workExample` → the tool it runs
    //     (`must/1_howtostep.ttl` "HowToStep workExample").
    // Every wired tool is a real `#tool/<task>` entity that is genuinely the
    // `instrument` of a CreateAction above — a per-output table action for
    // table stages, or the production action over the stage's recorded primary
    // output for agent-orchestrated stages ("Tool inverse instrument" in
    // `must/0_tool.ttl`) — so adding it to `hasPart` introduces no unbacked
    // structure. We map each tool to its stage so the step's `workExample`
    // names the right tool.
    if !tool_for_task.is_empty() {
        // The full set of tool @ids to attach (newly-registered this run plus
        // any registered by a prior finalize, recorded in `tool_for_task`).
        let all_tool_ids: std::collections::BTreeSet<String> =
            tool_for_task.values().cloned().collect();

        if let Some(wf) = graph.iter_mut().find(|e| match e.get("@type") {
            Some(Value::String(s)) => s == "ComputationalWorkflow",
            Some(Value::Array(a)) => a
                .iter()
                .any(|v| v.as_str() == Some("ComputationalWorkflow")),
            _ => false,
        }) {
            if let Some(obj) = wf.as_object_mut() {
                // (a) Add the `HowTo` @type (the workflow links HowToSteps via
                //     `step`). @type is always the 3-element array literal from
                //     `build_metadata`; handle both array and string shapes.
                match obj.get_mut("@type") {
                    Some(Value::Array(types)) => {
                        if !types.iter().any(|t| t.as_str() == Some("HowTo")) {
                            types.push(json!("HowTo"));
                        }
                    }
                    Some(Value::String(s)) => {
                        let existing_t = s.clone();
                        obj.insert("@type".to_string(), json!([existing_t, "HowTo"]));
                    }
                    _ => {}
                }
                // (b) `hasPart` → the executed tools, de-duplicated.
                let parts = obj
                    .entry("hasPart")
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Some(arr) = parts.as_array_mut() {
                    for tid in &all_tool_ids {
                        if !arr
                            .iter()
                            .any(|p| p.get("@id").and_then(Value::as_str) == Some(tid))
                        {
                            arr.push(json!({"@id": tid}));
                        }
                    }
                }
            }
        }

        // (c) Each HowToStep `workExample` → the tool its stage ran. Every
        //     executed stage now has a `#tool/<task>` (script-backed
        //     SoftwareSourceCode, or a SoftwareApplication executor for
        //     agent-orchestrated stages), so every step is wired.
        for entity in graph.iter_mut() {
            let is_step = match entity.get("@type") {
                Some(Value::String(s)) => s == "HowToStep",
                Some(Value::Array(a)) => a.iter().any(|t| t.as_str() == Some("HowToStep")),
                _ => false,
            };
            if !is_step {
                continue;
            }
            let Some(step_id) = entity.get("@id").and_then(Value::as_str) else {
                continue;
            };
            let Some(task) = step_id.strip_prefix("#step-") else {
                continue;
            };
            if let Some(tool_id) = tool_for_task.get(task) {
                if let Some(obj) = entity.as_object_mut() {
                    obj.insert("workExample".to_string(), json!({"@id": tool_id}));
                }
            }
        }
    }

    // ── FormalParameter entities for the ParameterConnection endpoints ───────
    //
    // Each `ParameterConnection` carries `sourceParameter {@id:
    // "#step-<src>#<port>"}` and `targetParameter {@id:
    // "#step-<tgt>#<port>"}`. Provenance Run Crate `must/5_parameterconnection.ttl`
    // requires both to RESOLVE to `FormalParameter` entities — in the plan crate
    // these @ids dangle. Emit one `FormalParameter` per distinct endpoint @id
    // actually referenced by a connection, named for the `<task>` / `<port>` it
    // denotes. These are real declared ports of the executed workflow's steps
    // (the connection edges already in the graph reference them); we materialise
    // the entity the edge points at rather than inventing new connectivity.
    register_parameter_connection_endpoints(graph);

    // EXECUTION-AWARE `conformsTo` upgrade. The plan crate emitted by
    // `build_metadata` declares only the profiles a workflow *definition*
    // truthfully meets (`PLAN_PROFILE_IRIS`). Now that retrospective per-output
    // `CreateAction`s with real `instrument`s are in the graph, the crate
    // honestly documents executed processes / a workflow run / provenance, so
    // it may add the three WRROC v0.5 run profiles (`EXECUTED_ADDED_PROFILE_IRIS`)
    // to its `conformsTo`. Gated on the actual presence of a real run
    // `CreateAction` (not the compile-time SME `Action`s) so the upgrade is
    // truthful and idempotent across re-runs — it claims a run profile only
    // when the graph genuinely carries run provenance.
    let has_run_action = graph_has_run_create_action(graph);
    if has_run_action {
        upgrade_conforms_to_executed(graph);
    }

    let added = new_parts.len();
    if added == 0 {
        // Even with no NEW tables this invocation, a prior finalize may have
        // registered run actions and upgraded the descriptor — persist the
        // (idempotent) upgrade so a re-run on an already-executed crate keeps
        // the executed `conformsTo`. With neither new tables nor any run
        // action, nothing changed and we skip the write.
        if has_run_action {
            let serialized = serde_json::to_vec_pretty(&doc)?;
            crate::fs_helpers::atomic_write_bytes_sync(&descriptor, &serialized)?;
        }
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
                    .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("md"))
                    .filter_map(|p| p.file_name().and_then(|s| s.to_str()).map(String::from))
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
pub fn register_reexecutability_sidecars(package_root: &std::path::Path) -> std::io::Result<usize> {
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
                let role = e.get("evidence_role").and_then(Value::as_str).unwrap_or("");
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

/// Materialise a `bioschemas:FormalParameter` entity for every distinct
/// endpoint `@id` referenced by a `ParameterConnection`'s `sourceParameter` /
/// `targetParameter`, so those references resolve (Provenance Run Crate
/// `must/5_parameterconnection.ttl` requires both endpoints to be
/// `FormalParameter` entities). In the plan crate the endpoint @ids
/// (`#step-<task>#<port>`) dangle — this fills the entity the EXISTING edge
/// already points at; no new connectivity is invented. Idempotent: an endpoint
/// whose @id is already a graph node is skipped. Appends in sorted @id order
/// for deterministic output.
fn register_parameter_connection_endpoints(graph: &mut Vec<Value>) {
    // Collect the endpoint @ids referenced by ParameterConnections.
    let mut endpoints: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for node in graph.iter() {
        let is_connection = match node.get("@type") {
            Some(Value::String(s)) => s == "ParameterConnection",
            Some(Value::Array(a)) => a.iter().any(|t| t.as_str() == Some("ParameterConnection")),
            _ => false,
        };
        if !is_connection {
            continue;
        }
        for key in ["sourceParameter", "targetParameter"] {
            if let Some(id) = node
                .get(key)
                .and_then(|p| p.get("@id"))
                .and_then(Value::as_str)
            {
                endpoints.insert(id.to_string());
            }
        }
    }
    // Existing @ids — skip endpoints already present (idempotency + never shadow
    // a real entity the edge happens to point at).
    let existing: std::collections::BTreeSet<String> = graph
        .iter()
        .filter_map(|e| e.get("@id").and_then(Value::as_str).map(String::from))
        .collect();
    for id in &endpoints {
        if existing.contains(id) {
            continue;
        }
        // Derive a human name from the `#step-<task>#<port>` shape (best effort;
        // unparseable endpoints still get a FormalParameter with the bare @id).
        let (task, port) = id
            .strip_prefix("#step-")
            .and_then(|body| body.split_once('#'))
            .map(|(t, p)| (t.to_string(), p.to_string()))
            .unwrap_or_else(|| (id.clone(), String::new()));
        let name = if port.is_empty() {
            task.clone()
        } else {
            format!("{task} {port}")
        };
        graph.push(json!({
            "@id": id,
            "@type": "FormalParameter",
            // Workflow Run Crate `must/3_formal_parameter.ttl` requires an
            // `additionalType`. These ports carry data artifacts between steps
            // and are not declared with a concrete format, so the honest,
            // non-overclaiming value is EDAM `data_0006` ("Data") — it asserts
            // only that the port is a data parameter, inventing no specific type.
            // RP-10 — `https://` scheme, matching `edam_iri` and every other
            // EDAM reference in ro-crate-metadata.json (no mixed http/https).
            "additionalType": {"@id": edam_iri("data_0006")},
            "name": name,
            "description": format!(
                "Declared {} port of workflow step '{}'.",
                if port.is_empty() { "parameter" } else { &port },
                task
            ),
        }));
    }
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

/// Recursively collect the relative paths of the REAL executed scripts a task
/// authored under `runtime/outputs/<task>/scripts/`. These are the concrete
/// code artifacts that ran the step (`*.R` / `*.py` / `*.sh` / `*.bash` /
/// `*.pl` / `*.jl`), recorded per the executor brief. Paths are package-
/// relative (`runtime/outputs/<task>/scripts/<file>`) and sorted by the caller.
/// Returns an empty vec when the task has no `scripts/` directory.
fn collect_task_scripts(scripts_dir: &std::path::Path, rel_prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(scripts_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        let rel = format!("{rel_prefix}/{name}");
        if path.is_dir() {
            collect_task_scripts(&path, &rel, out);
        } else if path.is_file() {
            let is_script = [".R", ".r", ".py", ".sh", ".bash", ".pl", ".jl"]
                .iter()
                .any(|ext| name.ends_with(ext));
            if is_script {
                out.push(rel);
            }
        }
    }
}

/// Map a script's relative path to a JSON-LD media type for its
/// `programmingLanguage` / `encodingFormat`. Honest, file-extension-derived;
/// unknown extensions fall back to `text/plain`.
fn script_encoding_format(rel: &str) -> &'static str {
    if rel.ends_with(".R") || rel.ends_with(".r") {
        "text/x-r-source"
    } else if rel.ends_with(".py") {
        "text/x-python"
    } else if rel.ends_with(".sh") || rel.ends_with(".bash") {
        "application/x-sh"
    } else if rel.ends_with(".pl") {
        "text/x-perl"
    } else if rel.ends_with(".jl") {
        "text/x-julia"
    } else {
        "text/plain"
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
pub fn register_software_dependencies(package_root: &std::path::Path) -> std::io::Result<usize> {
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
    // @type may be a plain string or an array — handle both.
    if let Some(wf) = graph.iter_mut().find(|e| match e.get("@type") {
        Some(Value::String(s)) => s == "ComputationalWorkflow",
        Some(Value::Array(a)) => a
            .iter()
            .any(|v| v.as_str() == Some("ComputationalWorkflow")),
        _ => false,
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

/// Project actually-used analysis tools from per-task `runtime/outputs/*/env.lock`
/// files into `SoftwareApplication` `@graph` nodes, linked from the
/// `ComputationalWorkflow` entity's `softwareRequirements` array.
///
/// Mirrors [`register_software_dependencies`] in node shape and idempotency contract.
/// Complements it: `register_software_dependencies` reads the *requested* dependency
/// lock (`runtime/dependency-lock.json`) which records tools the orchestrator asked
/// for; this function reads the *recorded* env.lock snapshots which record what was
/// ACTUALLY installed and used per task — including tools the agent installed at
/// runtime that never appeared in the declared dependency lock.
///
/// ## env.lock line shapes (three parseable forms; everything else is skipped)
///
/// 1. **pip pin** `name==version` → eco=`python`
///    e.g. `pydeseq2==0.5.4`, `gseapy==1.3.0`
/// 2. **conda pin** `name: version` (value starts with a digit) → eco=`conda`
///    e.g. `bioconductor-deseq2: 1.50.2`, `r-jsonlite: 2.0.0`
///    Metadata lines like `conda env: deseq2_vst_env` are excluded because
///    their value does not start with a digit.
/// 3. **R sessionInfo "other attached packages"** block → eco=`r`
///    After a `other attached packages:` header, parse `Name_version` tokens
///    (often prefixed by `[N]`) until a blank line or the next section header.
///    e.g. `DESeq2_1.50.2`, `SummarizedExperiment_1.40.0`
///
/// ## Dedup + ordering
/// First occurrence of `(eco, name)` across ALL task env.lock files wins; tasks
/// are processed in sorted order so the output is deterministic. `@id` scheme:
/// `#dep/<eco>/<name>` (same namespace as `register_software_dependencies`).
///
/// ## Idempotency
/// Skips any `@id` already present in the `@graph`, so coexists safely with
/// `register_software_dependencies` and repeated finalize runs.
///
/// Returns the count of newly-added nodes. No-op `Ok(0)` when
/// `runtime/outputs/` is absent, no task dir contains an `env.lock`, or
/// the descriptor is missing/unparseable.
pub fn register_software_from_env_locks(package_root: &std::path::Path) -> std::io::Result<usize> {
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

    // Collect existing @ids for idempotency check.
    let existing: std::collections::BTreeSet<String> = graph
        .iter()
        .filter_map(|e| e.get("@id").and_then(Value::as_str).map(String::from))
        .collect();

    // Scan runtime/outputs/<task>/env.lock files in sorted task order.
    let outputs_root = package_root.join("runtime").join("outputs");
    let mut task_dirs: Vec<std::path::PathBuf> = match std::fs::read_dir(&outputs_root) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        Err(_) => return Ok(0),
    };
    task_dirs.sort();

    // (eco, name) → version — first occurrence across all tasks wins.
    let mut seen: std::collections::BTreeMap<(String, String), String> =
        std::collections::BTreeMap::new();

    for task_dir in &task_dirs {
        let env_lock = task_dir.join("env.lock");
        let Ok(content) = std::fs::read_to_string(&env_lock) else {
            continue;
        };
        parse_env_lock(&content, &mut seen);
    }

    if seen.is_empty() {
        return Ok(0);
    }

    let mut new_nodes: Vec<Value> = Vec::new();
    let mut req_ids: Vec<Value> = Vec::new();

    // Emit in deterministic (eco, name) sorted order.
    for ((eco, name), version) in &seen {
        let id = format!("#dep/{eco}/{name}");
        if existing.contains(&id) {
            continue;
        }
        let mut node = json!({
            "@id": id,
            "@type": "SoftwareApplication",
            "name": name,
            "applicationCategory": eco,
        });
        if !version.is_empty() {
            node["softwareVersion"] = json!(version);
        }
        new_nodes.push(node);
        req_ids.push(json!({"@id": id}));
    }

    let added = new_nodes.len();
    if added == 0 {
        return Ok(0);
    }
    for node in new_nodes {
        graph.push(node);
    }

    // Link new nodes from the ComputationalWorkflow entity's softwareRequirements.
    // @type may be a plain string or an array — handle both (same as register_software_dependencies).
    if let Some(wf) = graph.iter_mut().find(|e| match e.get("@type") {
        Some(Value::String(s)) => s == "ComputationalWorkflow",
        Some(Value::Array(a)) => a
            .iter()
            .any(|v| v.as_str() == Some("ComputationalWorkflow")),
        _ => false,
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

/// Parse a single `env.lock` file content and insert discovered `(eco, name) →
/// version` entries into `seen`. First occurrence wins — callers accumulate
/// across multiple files by passing the same `seen` map.
///
/// Three line shapes are recognised (everything else is silently skipped):
/// 1. pip pin: `name==version`
/// 2. conda pin: `name: version` where version starts with a digit
/// 3. R sessionInfo "other attached packages" block: `Name_version` tokens
fn parse_env_lock(content: &str, seen: &mut std::collections::BTreeMap<(String, String), String>) {
    // Compile regexes once per call. The patterns are simple enough that
    // using regex is not required; we use hand-rolled matching to avoid
    // adding a dependency and to stay allocation-light.
    let mut in_other_attached = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Skip blank lines (also exits the R sessionInfo block).
        if trimmed.is_empty() {
            in_other_attached = false;
            continue;
        }

        // Skip comment lines.
        if trimmed.starts_with('#') {
            continue;
        }

        // Detect R sessionInfo "other attached packages:" header (case-insensitive).
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("other attached packages:") {
            in_other_attached = true;
            // The header line itself may contain package tokens after the colon;
            // fall through so they are also parsed.
            // Extract the part after the colon for parsing.
            if let Some(after) = trimmed
                .find("packages:")
                .map(|i| &trimmed[i + "packages:".len()..])
            {
                parse_r_session_tokens(after, seen);
            }
            continue;
        }

        // Exit the R sessionInfo block on section headers like
        // "loaded via a namespace (and not attached):" or other non-package lines.
        if in_other_attached {
            if lower.contains("loaded via") || lower.contains("namespace") {
                in_other_attached = false;
            } else {
                // Parse all `Name_version` tokens on this line.
                parse_r_session_tokens(trimmed, seen);
                continue;
            }
        }

        // pip pin: name==version (no spaces, version starts with digit).
        if let Some((name_part, ver_part)) = trimmed.split_once("==") {
            let name = name_part.trim();
            let version = ver_part.split_whitespace().next().unwrap_or("").trim();
            // Validate: name is non-empty, name@file:// pip-from-conda artifact is excluded,
            // version starts with a digit.
            if !name.is_empty()
                && !name.contains('@')
                && !name.contains(' ')
                && !name.contains(':')
                && version.starts_with(|c: char| c.is_ascii_digit())
                && is_valid_pkg_name(name)
            {
                seen.entry(("python".to_string(), name.to_string()))
                    .or_insert_with(|| version.to_string());
            }
            continue;
        }

        // conda pin: name: version (value starts with digit, no == present).
        if let Some((name_part, ver_part)) = trimmed.split_once(':') {
            let name = name_part.trim();
            let version = ver_part.split_whitespace().next().unwrap_or("").trim();
            if !name.is_empty()
                && !name.contains(' ')
                && !name.contains('@')
                && version.starts_with(|c: char| c.is_ascii_digit())
                && is_valid_pkg_name(name)
            {
                seen.entry(("conda".to_string(), name.to_string()))
                    .or_insert_with(|| version.to_string());
            }
            // Don't `continue` — no further shape matches a line with a bare `:`.
        }
    }
}

/// Parse R sessionInfo package tokens (`Name_version`) from a text fragment.
/// Tokens match `[A-Za-z][A-Za-z0-9.]+_[0-9][0-9A-Za-z.-]*`.
/// Handles bracket prefixes like `[1]`, `[2]` and surrounding whitespace.
fn parse_r_session_tokens(
    text: &str,
    seen: &mut std::collections::BTreeMap<(String, String), String>,
) {
    // Walk the text, looking for `Name_version` tokens.
    // We split on whitespace and try each token.
    for token in text.split_whitespace() {
        // Strip leading `[N]` bracket annotations.
        let t = if token.starts_with('[') {
            if let Some(end) = token.find(']') {
                &token[end + 1..]
            } else {
                token
            }
        } else {
            token
        };
        // Match `Name_version`: letter, letters/digits/dots, underscore, version starting with digit.
        if let Some(under) = t.rfind('_') {
            let name = &t[..under];
            let version = &t[under + 1..];
            if !name.is_empty()
                && !version.is_empty()
                && name.starts_with(|c: char| c.is_ascii_alphabetic())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
                && version.starts_with(|c: char| c.is_ascii_digit())
                && version
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
            {
                seen.entry(("r".to_string(), name.to_string()))
                    .or_insert_with(|| version.to_string());
            }
        }
    }
}

/// Validate that a package name consists only of allowed characters:
/// alphanumerics, `.`, `_`, `+`, `-` (pip/conda name charset).
fn is_valid_pkg_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '+' || c == '-')
}

/// Annotate every `File` `@graph` entity whose `@id` resolves to a payload file
/// with `contentSize` (bytes) + `sha512` (hex). Excludes the metadata descriptor
/// (`ro-crate-metadata.json`) and BagIt tag files (cannot self-hash). Finalize-
/// time (off the byte-repro baseline); deterministic given fixed payload bytes.
pub fn register_content_integrity(package_root: &std::path::Path) -> std::io::Result<usize> {
    let hashes = crate::emitter::bagit::payload_hashes(
        package_root,
        crate::emitter::bagit::SealMode::Reseal,
    )?;
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
    let mut annotated = 0usize;
    for e in graph.iter_mut() {
        let Some(id) = e.get("@id").and_then(Value::as_str).map(String::from) else {
            continue;
        };
        if id == "ro-crate-metadata.json"
            || id == "ro-crate-preview.html"
            || id.starts_with("manifest-")
            || id == "bagit.txt"
            || id.starts_with('#')
            || id == "./"
        {
            continue;
        }
        let is_file = match e.get("@type") {
            Some(Value::String(s)) => s == "File",
            Some(Value::Array(a)) => a.iter().any(|v| v.as_str() == Some("File")),
            _ => false,
        };
        if !is_file {
            continue;
        }
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

/// Read `ro-crate-metadata.json`'s embedded per-file `sha512` annotations
/// (written by [`register_content_integrity`]) as a `{@id: sha512_hex}` map.
/// Empty when the descriptor is absent, unparseable, or carries no
/// `sha512`-annotated `File` entities yet (a fresh, pre-execution emit that
/// has never run [`register_content_integrity`]).
///
/// Companion read to the writer above — used by the post-seal recheck
/// ([`crate::deposit_readiness::recheck_ro_crate_content_hashes`]) to detect
/// the RCA I-2 finalization-order failure: a descriptor sealed BEFORE a
/// later mutation to the payload it describes.
pub fn recorded_content_hashes(
    package_root: &std::path::Path,
) -> std::collections::BTreeMap<String, String> {
    let descriptor = package_root.join("ro-crate-metadata.json");
    let Ok(bytes) = std::fs::read(&descriptor) else {
        return Default::default();
    };
    let Ok(doc) = serde_json::from_slice::<Value>(&bytes) else {
        return Default::default();
    };
    let Some(graph) = doc.get("@graph").and_then(Value::as_array) else {
        return Default::default();
    };
    graph
        .iter()
        .filter_map(|e| {
            let id = e.get("@id")?.as_str()?.to_string();
            let hex = e.get("sha512")?.as_str()?.to_string();
            Some((id, hex))
        })
        .collect()
}

/// Register `ro-crate-preview.html` as a `["File","CreativeWork"]` `@graph`
/// entity and link it from the root `hasPart`. Idempotent: a second call with
/// the entity already present is a no-op (returns `Ok(0)`). No-op when the
/// descriptor is absent or unparseable.
///
/// NOTE: This function does NOT skip when `ro-crate-preview.html` does not yet
/// exist on disk — the entity is registered first so the descriptor that the
/// preview embeds already includes the preview entity. The file itself is
/// written immediately after by [`render_and_write_preview`].
fn register_preview_entity(package_root: &std::path::Path) -> std::io::Result<usize> {
    const PREVIEW_ID: &str = "ro-crate-preview.html";
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

    // Idempotent guard: already registered → no-op.
    if graph
        .iter()
        .any(|e| e.get("@id").and_then(Value::as_str) == Some(PREVIEW_ID))
    {
        return Ok(0);
    }

    graph.push(json!({
        "@id": PREVIEW_ID,
        "@type": ["File", "CreativeWork"],
        "name": "RO-Crate preview — human-readable rendering",
        "description": "Static HTML rendering of the root Dataset entity. Embeds the \
                        RO-Crate JSON-LD in a typed application/ld+json block per the \
                        RO-Crate 1.1 specification §10. Zero executable JavaScript.",
        "encodingFormat": "text/html",
        "about": {"@id": "./"},
    }));

    // Link from root Dataset `hasPart`.
    if let Some(root) = graph
        .iter_mut()
        .find(|e| e.get("@id").and_then(Value::as_str) == Some("./"))
    {
        if let Some(obj) = root.as_object_mut() {
            let slot = obj
                .entry("hasPart")
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(arr) = slot.as_array_mut() {
                let already = arr
                    .iter()
                    .any(|p| p.get("@id").and_then(Value::as_str) == Some(PREVIEW_ID));
                if !already {
                    arr.push(json!({"@id": PREVIEW_ID}));
                }
            }
        }
    }

    crate::fs_helpers::atomic_write_bytes_sync(&descriptor, &serde_json::to_vec_pretty(&doc)?)?;
    Ok(1)
}

/// Read the current (FINAL) `ro-crate-metadata.json`, render a deterministic
/// HTML preview embedding those exact bytes, and write it to
/// `ro-crate-preview.html`. No-op (returns `Ok(())`) when the descriptor is
/// absent or unparseable.
///
/// Determinism: delegates to [`crate::preview::render_ro_crate_preview`] which
/// is a pure function of the `Value` — no clock, no RNG, no HashMap, no host
/// paths.
fn render_and_write_preview(package_root: &std::path::Path) -> std::io::Result<()> {
    let descriptor = package_root.join("ro-crate-metadata.json");
    let Ok(bytes) = std::fs::read(&descriptor) else {
        return Ok(());
    };
    let Ok(metadata) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(());
    };
    crate::preview::write_ro_crate_preview(package_root, &metadata)
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

/// True when `e` is a `CreateAction` graph entity (`@type` string or array
/// containing `"CreateAction"`).
fn is_create_action_entity(e: &Value) -> bool {
    match e.get("@type") {
        Some(Value::String(s)) => s == "CreateAction",
        Some(Value::Array(a)) => a.iter().any(|t| t.as_str() == Some("CreateAction")),
        _ => false,
    }
}

/// PR-6 — inject ONE top-level workflow-run `CreateAction` whose `instrument`
/// is `WORKFLOW.json`, tying the per-stage CreateActions (each keyed to a
/// per-task tool/step) into a single run-level activity for the Workflow Run
/// Crate profile. Additive + idempotent (guarded on the fixed `#workflow-run`
/// `@id`); a no-op when the graph carries no per-stage CreateActions yet
/// (pre-execution). `endTime`/`agent` are populated ONLY from what the per-stage
/// actions actually recorded — the run's `endTime` is the latest recorded stage
/// `endTime` (ISO-8601 UTC ⇒ lexicographic max) and `agent` is set only when a
/// single agent identity is unambiguous across the run. No `startTime` is
/// emitted: no start timestamp is recorded anywhere (`.container-state.json`
/// carries `ended_at` only), so it is honestly omitted rather than fabricated.
fn register_workflow_run_action(package_root: &std::path::Path) -> std::io::Result<usize> {
    const RUN_ID: &str = "#workflow-run";
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
    if graph
        .iter()
        .any(|e| e.get("@id").and_then(Value::as_str) == Some(RUN_ID))
    {
        return Ok(0);
    }

    let mut end_times: Vec<String> = Vec::new();
    let mut agents: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut results: Vec<Value> = Vec::new();
    let mut seen_results: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut stage_actions = 0usize;
    for e in graph.iter() {
        if !is_create_action_entity(e) {
            continue;
        }
        // A real per-stage activity carries an `instrument`.
        if e.get("instrument").and_then(|i| i.get("@id")).is_none() {
            continue;
        }
        stage_actions += 1;
        if let Some(t) = e.get("endTime").and_then(Value::as_str) {
            if !t.is_empty() {
                end_times.push(t.to_string());
            }
        }
        if let Some(a) = e
            .get("agent")
            .and_then(|a| a.get("@id"))
            .and_then(Value::as_str)
        {
            agents.insert(a.to_string());
        }
        match e.get("result") {
            Some(Value::Object(o)) => {
                if let Some(id) = o.get("@id").and_then(Value::as_str) {
                    if seen_results.insert(id.to_string()) {
                        results.push(json!({ "@id": id }));
                    }
                }
            }
            Some(Value::Array(arr)) => {
                for r in arr {
                    if let Some(id) = r.get("@id").and_then(Value::as_str) {
                        if seen_results.insert(id.to_string()) {
                            results.push(json!({ "@id": id }));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if stage_actions == 0 {
        return Ok(0);
    }

    let mut action = json!({
        "@id": RUN_ID,
        "@type": ["CreateAction", "prov:Activity"],
        "name": "Workflow run — end-to-end execution of the emitted plan.",
        "instrument": {"@id": "WORKFLOW.json"},
    });
    if !results.is_empty() {
        action["result"] = Value::Array(results);
    }
    if let Some(max_end) = end_times.iter().max() {
        action["endTime"] = json!(max_end);
    }
    if agents.len() == 1 {
        action["agent"] = json!({ "@id": agents.into_iter().next().unwrap() });
    }
    graph.push(action);

    let serialized = serde_json::to_vec_pretty(&doc)?;
    crate::fs_helpers::atomic_write_bytes_sync(&descriptor, &serialized)?;
    Ok(1)
}

/// DR-6 — re-stamp the root Dataset's `dateCreated`/`datePublished` with REAL
/// run timestamps once the run has produced recorded `CreateAction.endTime`s,
/// rather than leaving the deterministic emit-skeleton run-epoch (`2026-01-01`
/// in the no-`SOURCE_DATE_EPOCH` case) that reads as a placeholder next to the
/// real per-stage `endTime`s. `datePublished` = latest recorded stage `endTime`
/// (the run's completion); `dateCreated` = earliest recorded stage `endTime`
/// (the earliest real run timestamp available — no start timestamp is
/// recorded). Additive + idempotent (values derive from the graph). No-op
/// pre-execution (no recorded `endTime`s) so the byte-reproducible emit
/// skeleton is untouched.
fn restamp_root_dates_from_run(package_root: &std::path::Path) -> std::io::Result<usize> {
    let descriptor = package_root.join("ro-crate-metadata.json");
    let Ok(bytes) = std::fs::read(&descriptor) else {
        return Ok(0);
    };
    let Ok(mut doc) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(0);
    };
    let Some(graph) = doc.get("@graph").and_then(Value::as_array) else {
        return Ok(0);
    };
    let mut end_times: Vec<String> = Vec::new();
    for e in graph {
        if !is_create_action_entity(e) {
            continue;
        }
        if let Some(t) = e.get("endTime").and_then(Value::as_str) {
            if !t.is_empty() {
                end_times.push(t.to_string());
            }
        }
    }
    let (Some(earliest), Some(latest)) = (
        end_times.iter().min().cloned(),
        end_times.iter().max().cloned(),
    ) else {
        return Ok(0);
    };
    let Some(graph_mut) = doc.get_mut("@graph").and_then(Value::as_array_mut) else {
        return Ok(0);
    };
    let Some(root) = graph_mut
        .iter_mut()
        .find(|e| e.get("@id").and_then(Value::as_str) == Some("./"))
    else {
        return Ok(0);
    };
    let Some(obj) = root.as_object_mut() else {
        return Ok(0);
    };
    obj.insert("dateCreated".into(), json!(earliest));
    obj.insert("datePublished".into(), json!(latest));

    let serialized = serde_json::to_vec_pretty(&doc)?;
    crate::fs_helpers::atomic_write_bytes_sync(&descriptor, &serialized)?;
    Ok(1)
}

/// DR-9 — fold the REAL executed image digests from each task's
/// `.container-state.json` (`image` = `<ref>@sha256:<digest>` resolved at run
/// time) into `policies/container.json` as `oci_digest` entries, so the deposit
/// records the image that ACTUALLY ran — the emit-time `container.json` carries
/// only the DECLARED digest (or none, for host-environment atoms). Additive +
/// idempotent (dedup by digest value); a no-op when no container-state records a
/// resolved `@sha256:` digest (e.g. a host-env run). Kept DISTINCT from the
/// emit-time `content_hash` entries — never coerces or overwrites them.
fn fold_execution_container_digests(package_root: &std::path::Path) -> std::io::Result<usize> {
    let container_json = package_root.join("policies/container.json");
    let Ok(bytes) = std::fs::read(&container_json) else {
        return Ok(0);
    };
    let Ok(mut doc) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(0);
    };
    let Some(obj) = doc.as_object_mut() else {
        return Ok(0);
    };

    let mut executed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(package_root.join("runtime/outputs")) {
        for entry in rd.flatten() {
            let task_dir = entry.path();
            if !task_dir.is_dir() {
                continue;
            }
            if let Ok(Some(state)) =
                crate::container_state::ContainerState::read_from_task_dir(&task_dir)
            {
                // Only a run-time-RESOLVED `<ref>@sha256:<digest>` carries an
                // OCI content digest; a bare `image:tag` does not.
                if let Some((_, digest)) = state.image.trim().split_once("@sha256:") {
                    if !digest.is_empty() {
                        executed.insert(format!("sha256:{digest}"));
                    }
                }
            }
        }
    }
    if executed.is_empty() {
        return Ok(0);
    }

    let digests = obj
        .entry("digests")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(arr) = digests.as_array_mut() else {
        return Ok(0);
    };
    let mut added = 0usize;
    for value in executed {
        let present = arr
            .iter()
            .any(|e| e.get("value").and_then(Value::as_str) == Some(value.as_str()));
        if !present {
            arr.push(json!({ "kind": "oci_digest", "value": value }));
            added += 1;
        }
    }
    if added == 0 {
        return Ok(0);
    }

    let serialized = serde_json::to_vec_pretty(&doc)?;
    crate::fs_helpers::atomic_write_bytes_sync(&container_json, &serialized)?;
    Ok(added)
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
    // DR-9 — fold the real executed OCI image digests into policies/container.json
    // (the emit-time file carries only declared/content-hash digests). Best-effort,
    // additive, idempotent; runs before the reseal so the update is manifested.
    if let Err(e) = fold_execution_container_digests(root) {
        tracing::warn!(
            target: "ecaa::ro_crate",
            error = %e,
            "execution container-digest fold failed"
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
    // Register tools actually installed per-task, read from per-task env.lock files.
    // Runs AFTER register_software_dependencies so lock-file entries take precedence
    // (their @ids are already in the graph; env.lock fills remaining tools). Best-effort.
    if let Err(e) = register_software_from_env_locks(root) {
        tracing::warn!(
            target: "ecaa::ro_crate",
            error = %e,
            "env.lock tool registration failed"
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
    // PR-6 — tie the per-stage CreateActions into one workflow-run action
    // (instrument = WORKFLOW.json). Additive + idempotent; no-op pre-exec.
    // Runs after per-stage actions exist and BEFORE the preview embed + reseal.
    if let Err(e) = register_workflow_run_action(root) {
        tracing::warn!(
            target: "ecaa::ro_crate",
            error = %e,
            "workflow-run action registration failed"
        );
    }
    // DR-6 — re-stamp the root Dataset dates with real run timestamps once
    // CreateAction endTimes exist (replacing the 2026-01-01 emit skeleton).
    // Runs LAST among @graph mutations (after the run action so its endTime is
    // considered) and BEFORE the preview embed + reseal.
    if let Err(e) = restamp_root_dates_from_run(root) {
        tracing::warn!(
            target: "ecaa::ro_crate",
            error = %e,
            "root date re-stamp failed"
        );
    }
    // ── ro-crate-preview.html (LAST step before re-seal) ─────────────────────
    //
    // Ordering rationale (controller override): the preview embeds the
    // RO-Crate JSON-LD in its `<head>` (spec MUST), so it must embed the
    // FINAL metadata — after all @graph mutations (reexec-sidecars,
    // software-deps, content-integrity) have been applied.
    //
    // Step 1: Register `ro-crate-preview.html` as a ["File","CreativeWork"]
    //         entity in the @graph + link from root `hasPart`, then
    //         re-write the descriptor so the preview entity itself is part
    //         of the canonical metadata.
    //
    // Step 2: Read back the freshly-serialized descriptor (now including the
    //         preview entity) and render+write `ro-crate-preview.html` so
    //         the embedded JSON-LD is byte-identical to the descriptor.
    //
    // The preview File entity does NOT carry contentSize/sha512 (same
    // treatment as the descriptor itself — both are integrity-covered by the
    // BagIt manifest at reseal). Best-effort: a failure must not abort the
    // re-seal.
    if let Err(e) = register_preview_entity(root) {
        tracing::warn!(
            target: "ecaa::ro_crate",
            error = %e,
            "ro-crate-preview.html entity registration failed"
        );
    }
    // Step 2: render + write, embedding the FINAL descriptor bytes.
    if let Err(e) = render_and_write_preview(root) {
        tracing::warn!(
            target: "ecaa::ro_crate",
            error = %e,
            "ro-crate-preview.html render/write failed"
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
    fn publisher_dirty_build_property_value_carries_id() {
        let dirty = publisher_entity("abc123def456-dirty");
        let props = dirty
            .get("additionalProperty")
            .and_then(|v| v.as_array())
            .expect("dirty build must carry additionalProperty");
        let pv = props
            .iter()
            .find(|p| p.get("@id").and_then(Value::as_str) == Some("#source-tree-dirty"))
            .expect("source_tree_dirty PropertyValue reference present");
        assert_eq!(
            pv.as_object().map(serde_json::Map::len),
            Some(1),
            "embedded JSON-LD node must be an @id-only reference"
        );
        let entity = source_tree_dirty_entity();
        assert_eq!(entity["@id"], "#source-tree-dirty");
        assert_eq!(entity["name"], "source_tree_dirty");
        assert_eq!(entity["value"], true);
        // A clean build emits no source_tree_dirty property at all.
        let clean = publisher_entity("abc123def456");
        assert!(
            clean.get("additionalProperty").is_none(),
            "a clean build must not emit source_tree_dirty: {clean:?}"
        );
    }

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

    /// B1 (execution-aware): the metadata descriptor AND the root `./` Dataset
    /// of a PLAN crate both declare exactly the plan-set `conformsTo` profiles,
    /// and every declared profile IRI resolves to a first-class `CreativeWork`
    /// profile entity (name + version) in the `@graph` — no bare dangling
    /// `{@id}` ref.
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

        // Root `./` carries conformsTo equal to the PLAN profile set.
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
        for iri in ecaa_workflow_types::consts::PLAN_PROFILE_IRIS {
            assert!(
                declared.contains(iri),
                "root ./ conformsTo must declare plan profile {iri}; got {declared:?}"
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

    /// EXECUTION-AWARE `conformsTo` (T6′): a PRE-EXECUTION PLAN crate has ZERO
    /// real run `CreateAction`s, so it must declare ONLY the truthful plan-set
    /// profiles (base RO-Crate 1.1 + workflow-ro-crate/1.0 + ecaa/v0.2) on BOTH
    /// the metadata descriptor and the root `./` Dataset — and must NOT claim
    /// any of the three WRROC v0.5 run profiles (process / workflow /
    /// provenance), which document executed runs. This is the regression guard
    /// against the rejected Task-6 hack (which fabricated a planned run action
    /// + workflow self-`hasPart` to make `provenance/0.5` "pass").
    #[test]
    fn plan_crate_conforms_to_excludes_run_profiles() {
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

        // The plan crate genuinely has NO real run CreateAction (the precondition
        // for claiming any WRROC run profile).
        assert!(
            !graph_has_run_create_action(graph),
            "a pre-execution plan crate must contain ZERO real run CreateActions"
        );

        let expected: std::collections::BTreeSet<&str> =
            ecaa_workflow_types::consts::PLAN_PROFILE_IRIS
                .iter()
                .copied()
                .collect();

        for target_id in ["ro-crate-metadata.json", "./"] {
            let entry = graph
                .iter()
                .find(|e| e["@id"].as_str() == Some(target_id))
                .unwrap_or_else(|| panic!("{target_id} entry present"));
            let declared: std::collections::BTreeSet<&str> = entry["conformsTo"]
                .as_array()
                .unwrap_or_else(|| panic!("{target_id} conformsTo is an array"))
                .iter()
                .filter_map(|c| c["@id"].as_str())
                .collect();

            // Exactly the truthful plan set — no more, no less.
            assert_eq!(
                declared, expected,
                "{target_id} conformsTo must equal the plan set exactly; got {declared:?}"
            );

            // Specifically: none of the three execution-only WRROC run profiles.
            for run_iri in ecaa_workflow_types::consts::EXECUTED_ADDED_PROFILE_IRIS {
                assert!(
                    !declared.contains(run_iri),
                    "plan crate {target_id} conformsTo must NOT claim run profile {run_iri}"
                );
            }
        }
    }

    /// EXECUTION-AWARE `conformsTo` (T6′, executed half): once a real run
    /// `CreateAction` exists in the graph, the finalize-path upgrade
    /// (`upgrade_conforms_to_executed`) adds the three WRROC v0.5 run profiles
    /// to BOTH the descriptor and root `./`, emits resolving profile entities
    /// for them, and is idempotent (a second upgrade adds nothing). The result
    /// is the full executed `REQUIRED_PROFILE_IRIS` set, claimed truthfully
    /// because the run action is real (not fabricated).
    #[test]
    fn executed_crate_conforms_to_upgrade_is_truthful_and_idempotent() {
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
        let mut graph: Vec<Value> = metadata["@graph"].as_array().expect("@graph array").clone();

        // Inject a REAL run CreateAction (an `instrument`-bearing CreateAction),
        // mirroring what `register_produced_output_tables` appends post-execution.
        graph.push(json!({
            "@id": "#action/runtime/outputs/t1/de.tsv",
            "@type": ["CreateAction", "prov:Activity"],
            "name": "Production of de.tsv by stage 't1'.",
            "instrument": {"@id": "#step-t1"},
            "result": {"@id": "runtime/outputs/t1/de.tsv"}
        }));
        assert!(
            graph_has_run_create_action(&graph),
            "injected CreateAction must be recognized as a real run action"
        );

        upgrade_conforms_to_executed(&mut graph);
        // Idempotent: a second upgrade must not duplicate IRIs or entities.
        upgrade_conforms_to_executed(&mut graph);

        let full: std::collections::BTreeSet<&str> =
            ecaa_workflow_types::consts::REQUIRED_PROFILE_IRIS
                .iter()
                .copied()
                .collect();

        for target_id in ["ro-crate-metadata.json", "./"] {
            let entry = graph
                .iter()
                .find(|e| e["@id"].as_str() == Some(target_id))
                .unwrap_or_else(|| panic!("{target_id} entry present"));
            let arr = entry["conformsTo"]
                .as_array()
                .unwrap_or_else(|| panic!("{target_id} conformsTo is an array"));
            let declared: Vec<&str> = arr.iter().filter_map(|c| c["@id"].as_str()).collect();

            // No duplicate IRIs after two upgrades.
            let unique: std::collections::BTreeSet<&str> = declared.iter().copied().collect();
            assert_eq!(
                declared.len(),
                unique.len(),
                "{target_id} conformsTo must have no duplicate IRIs; got {declared:?}"
            );
            // Equals the full executed set.
            assert_eq!(
                unique, full,
                "{target_id} conformsTo must equal the full executed set; got {declared:?}"
            );
        }

        // Each newly-claimed run profile resolves to exactly one CreativeWork
        // profile entity (no dangling ref, no duplicate).
        for run_iri in ecaa_workflow_types::consts::EXECUTED_ADDED_PROFILE_IRIS {
            let n = graph
                .iter()
                .filter(|e| e["@id"].as_str() == Some(run_iri))
                .count();
            assert_eq!(
                n, 1,
                "run profile {run_iri} must resolve to exactly one entity; found {n}"
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
            serde_json::Value::Array(a) => a.iter().filter_map(|v| v.as_str()).collect(),
            _ => vec![],
        };
        assert!(
            types.contains(&"File"),
            "README.md must be typed as File; got {types:?}"
        );

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
        let (agent, backend_entity) =
            executor_agent_entities(&state).expect("recorded executor yields an agent");
        assert_eq!(agent["@type"].as_str(), Some("SoftwareApplication"));
        assert_eq!(
            agent["softwareVersion"].as_str(),
            Some("ghcr.io/scripps/scripps-bio-base:1.4.4"),
            "softwareVersion must be the recorded image verbatim"
        );
        assert_eq!(agent["runtimePlatform"].as_str(), Some("docker"));
        let backend_ref = &agent["additionalProperty"][0];
        assert_eq!(
            backend_ref.as_object().map(serde_json::Map::len),
            Some(1),
            "backend must be an @id-only reference"
        );
        let backend_entity = backend_entity.expect("recorded backend yields PropertyValue");
        assert_eq!(backend_ref["@id"], backend_entity["@id"]);
        assert_eq!(backend_entity["name"], "backend");
        assert_eq!(backend_entity["value"], "aws");

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
            executor_agent_entities(&empty).is_none(),
            "no recorded executor identity must yield None, not a placeholder agent"
        );
    }

    /// A2: the `@context` array must include an inline term-map that resolves
    /// all extension terms emitted by this crate so that JSON-LD compaction
    /// validates without unknown-term errors (roc-validator check ro-crate-1.1_2.1).
    #[test]
    fn context_defines_extension_terms() {
        let dag = one_task_dag();
        let classification = ClassificationResult {
            domain: "genomics".into(),
            workflow_description: "test workflow".into(),
            intake_text: "test intake".into(),
            edam_topic: "topic:3673".into(),
            edam_operation: "operation:3223".into(),
            ..Default::default()
        };
        let meta = build_metadata(&dag, &classification, &FrozenClock::default());
        let ctx = meta["@context"].as_array().expect("@context is an array");
        let inline = ctx
            .iter()
            .find_map(|v| v.as_object())
            .expect("inline term map present in @context");
        // PROV-O term used by wasGeneratedBy edges on output nodes.
        assert_eq!(
            inline["wasGeneratedBy"],
            "http://www.w3.org/ns/prov#wasGeneratedBy"
        );
        // ECAA ns/0.2# terms used by the p-plan / archetype entity.
        assert_eq!(
            inline["matchedArchetype"],
            "https://w3id.org/ecaa/ns/0.2#matchedArchetype"
        );
        assert_eq!(
            inline["rationale"],
            "https://w3id.org/ecaa/ns/0.2#rationale"
        );
        // ECAA ns terms used by InvariantVerdict nodes in the audit-proof
        // projection (ecaa_projection.rs).
        assert_eq!(
            inline["evaluated_against"],
            "https://w3id.org/ecaa/ns/0.2#evaluated_against"
        );
        assert_eq!(inline["verdict"], "https://w3id.org/ecaa/ns/0.2#verdict");
        assert_eq!(
            inline["invariant_id"],
            "https://w3id.org/ecaa/ns/0.2#invariantId"
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

    /// PR-6 + DR-6 — the finalize passes inject one workflow-run CreateAction
    /// (instrument = WORKFLOW.json) tying the per-stage actions into a run, and
    /// re-stamp the root Dataset dates from the real recorded endTimes (not the
    /// 2026-01-01 emit skeleton). No `startTime` is fabricated.
    #[test]
    fn workflow_run_action_and_date_restamp_from_create_actions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let graph = json!({
            "@context": "https://w3id.org/ro/crate/1.1/context",
            "@graph": [
                {"@id": "ro-crate-metadata.json", "@type": "CreativeWork", "about": {"@id": "./"}},
                {"@id": "./", "@type": "Dataset",
                 "dateCreated": "2026-01-01T00:00:00Z",
                 "datePublished": "2026-01-01T00:00:00Z",
                 "hasPart": []},
                {"@id": "#action/align", "@type": ["CreateAction", "prov:Activity"],
                 "instrument": {"@id": "#tool/align"},
                 "result": {"@id": "runtime/outputs/align/out.bam"},
                 "agent": {"@id": "#agent/claude"},
                 "endTime": "2026-06-21T10:00:00Z"},
                {"@id": "#action/de", "@type": ["CreateAction", "prov:Activity"],
                 "instrument": {"@id": "#tool/de"},
                 "result": {"@id": "runtime/outputs/de/de.tsv"},
                 "agent": {"@id": "#agent/claude"},
                 "endTime": "2026-06-21T12:30:00Z"},
            ]
        });
        std::fs::write(
            root.join("ro-crate-metadata.json"),
            serde_json::to_vec_pretty(&graph).unwrap(),
        )
        .unwrap();

        assert_eq!(register_workflow_run_action(root).unwrap(), 1);
        // Idempotent — a second call adds nothing.
        assert_eq!(register_workflow_run_action(root).unwrap(), 0);
        assert_eq!(restamp_root_dates_from_run(root).unwrap(), 1);

        let doc: Value =
            serde_json::from_slice(&std::fs::read(root.join("ro-crate-metadata.json")).unwrap())
                .unwrap();
        let graph = doc["@graph"].as_array().unwrap();

        let run = graph
            .iter()
            .find(|e| e["@id"] == "#workflow-run")
            .expect("workflow-run action present");
        assert_eq!(run["instrument"]["@id"], "WORKFLOW.json");
        assert_eq!(
            run["endTime"], "2026-06-21T12:30:00Z",
            "run endTime = latest stage endTime"
        );
        assert_eq!(
            run["agent"]["@id"], "#agent/claude",
            "single agent surfaced"
        );
        assert_eq!(
            run["result"].as_array().unwrap().len(),
            2,
            "aggregates both stage results"
        );
        assert!(
            run.get("startTime").is_none(),
            "no start timestamp is recorded, so none is fabricated"
        );

        let root_ds = graph.iter().find(|e| e["@id"] == "./").unwrap();
        assert_eq!(
            root_ds["dateCreated"], "2026-06-21T10:00:00Z",
            "dateCreated re-stamped to earliest real endTime"
        );
        assert_eq!(
            root_ds["datePublished"], "2026-06-21T12:30:00Z",
            "datePublished re-stamped to latest real endTime"
        );
    }

    /// DR-6 — pre-execution (no recorded CreateActions), the re-stamp is a
    /// no-op so the byte-reproducible emit skeleton dates are left intact.
    #[test]
    fn date_restamp_is_noop_without_create_actions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let graph = json!({
            "@graph": [
                {"@id": "./", "@type": "Dataset", "dateCreated": "2026-01-01T00:00:00Z"},
            ]
        });
        std::fs::write(
            root.join("ro-crate-metadata.json"),
            serde_json::to_vec_pretty(&graph).unwrap(),
        )
        .unwrap();
        assert_eq!(restamp_root_dates_from_run(root).unwrap(), 0);
        assert_eq!(register_workflow_run_action(root).unwrap(), 0);
        let doc: Value =
            serde_json::from_slice(&std::fs::read(root.join("ro-crate-metadata.json")).unwrap())
                .unwrap();
        assert_eq!(doc["@graph"][0]["dateCreated"], "2026-01-01T00:00:00Z");
    }

    /// DR-9 — the finalize fold records the run-time-resolved OCI image digest
    /// from a task's `.container-state.json` into `policies/container.json`,
    /// labeled `oci_digest`, WITHOUT touching the emit-time `content_hash` entry.
    #[test]
    fn execution_container_digest_fold_records_resolved_oci_digest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("policies")).unwrap();
        std::fs::write(
            root.join("policies/container.json"),
            serde_json::to_vec_pretty(&json!({
                "image": null,
                "digests": [{"kind": "content_hash", "value": "abcdef0123456789"}]
            }))
            .unwrap(),
        )
        .unwrap();
        let task = root.join("runtime/outputs/align");
        std::fs::create_dir_all(&task).unwrap();
        let executed_digest = "1111111111111111111111111111111111111111111111111111111111111111";
        std::fs::write(
            task.join(".container-state.json"),
            serde_json::to_vec(&json!({
                "task_id": "align",
                "image": format!("biocontainers/star@sha256:{executed_digest}"),
                "ended_at": "2026-06-21T10:00:00Z"
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(fold_execution_container_digests(root).unwrap(), 1);
        // Idempotent — a second fold adds nothing.
        assert_eq!(fold_execution_container_digests(root).unwrap(), 0);

        let doc: Value =
            serde_json::from_slice(&std::fs::read(root.join("policies/container.json")).unwrap())
                .unwrap();
        let digests = doc["digests"].as_array().unwrap();
        assert!(
            digests
                .iter()
                .any(|d| d["kind"] == "content_hash" && d["value"] == "abcdef0123456789"),
            "emit-time content_hash entry must be preserved"
        );
        assert!(
            digests
                .iter()
                .any(|d| d["kind"] == "oci_digest"
                    && d["value"] == format!("sha256:{executed_digest}")),
            "run-time resolved OCI digest must be folded in as oci_digest: {digests:?}"
        );
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
        let types: Vec<&str> = rep["@type"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
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
        let root_node = graph
            .iter()
            .find(|e| e["@id"].as_str() == Some("./"))
            .unwrap();
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
        std::fs::write(
            ev.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

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
        std::fs::write(
            dir.path().join("ro-crate-metadata.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "@context":"https://w3id.org/ro/crate/1.1/context",
                "@graph":[
                  {"@id":"ro-crate-metadata.json","@type":"CreativeWork","about":{"@id":"./"}},
                  {"@id":"./","@type":"Dataset","hasPart":[{"@id":"WORKFLOW.json"}]},
                  {"@id":"WORKFLOW.json","@type":["File","ComputationalWorkflow"],"name":"wf"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let n = register_content_integrity(dir.path()).unwrap();
        assert_eq!(
            n, 1,
            "one payload File entity annotated (descriptor excluded)"
        );
        let doc: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.path().join("ro-crate-metadata.json")).unwrap(),
        )
        .unwrap();
        let wf = doc["@graph"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["@id"] == "WORKFLOW.json")
            .unwrap();
        assert!(wf["contentSize"].as_u64().unwrap() >= 1);
        assert_eq!(wf["sha512"].as_str().unwrap().len(), 128);
        // descriptor must NOT carry its own hash (circular)
        let desc = doc["@graph"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["@id"] == "ro-crate-metadata.json")
            .unwrap();
        assert!(desc.get("sha512").is_none());
        // idempotent: second call returns same count, descriptor bytes unchanged
        let bytes_before = std::fs::read(dir.path().join("ro-crate-metadata.json")).unwrap();
        let n2 = register_content_integrity(dir.path()).unwrap();
        assert_eq!(n2, n, "second run returns same annotated count");
        let bytes_after = std::fs::read(dir.path().join("ro-crate-metadata.json")).unwrap();
        assert_eq!(
            bytes_before, bytes_after,
            "descriptor is byte-identical after second run"
        );
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
        assert_eq!(
            deseq["softwareVersion"], "1.40.2",
            "resolved preferred over requested"
        );
        assert_eq!(deseq["applicationCategory"], "r");

        // scanpy has no requested/resolved — no softwareVersion field
        let scanpy = g
            .iter()
            .find(|e| e.get("name").and_then(|v| v.as_str()) == Some("scanpy"))
            .unwrap();
        assert_eq!(scanpy["@type"], "SoftwareApplication");
        assert!(
            scanpy.get("softwareVersion").is_none(),
            "no version when both absent"
        );

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

    #[test]
    fn software_dependencies_linked_when_workflow_type_is_plain_string() {
        // Regression: @type as a plain string "ComputationalWorkflow" (not an array)
        // must still receive the softwareRequirements link.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("runtime")).unwrap();
        std::fs::write(
            dir.path().join("runtime/dependency-lock.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": "1",
                "r": [{"name": "edgeR", "resolved": "3.44.0"}],
                "python": [],
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
                    {"@id": "WORKFLOW.json", "@type": "ComputationalWorkflow", "name": "wf-string-type"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let n = register_software_dependencies(dir.path()).unwrap();
        assert_eq!(n, 1, "edgeR registered");

        let doc: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.path().join("ro-crate-metadata.json")).unwrap(),
        )
        .unwrap();
        let g = doc["@graph"].as_array().unwrap();

        // The workflow entity must carry softwareRequirements even with string @type
        let wf = g.iter().find(|e| e["@id"] == "WORKFLOW.json").unwrap();
        let reqs: Vec<&str> = wf["softwareRequirements"]
            .as_array()
            .expect("softwareRequirements must be present when @type is a plain string")
            .iter()
            .filter_map(|r| r["@id"].as_str())
            .collect();
        assert!(
            reqs.contains(&"#dep/r/edgeR"),
            "edgeR linked via softwareRequirements"
        );
    }

    /// Task 5: `finalize_evidence_registration_with_verifier` writes
    /// `ro-crate-preview.html` AND registers it in the `@graph` as a
    /// `["File","CreativeWork"]` entity linked from root `hasPart`.
    ///
    /// Verifies the controller ordering: preview registration → render/write →
    /// then BagIt reseal. After finalize, both the file on disk AND the `@graph`
    /// entity must be present.
    #[test]
    fn finalize_writes_preview_html_and_registers_it_in_graph() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Minimal emit-style directory so finalize can run end-to-end.
        // We need: ro-crate-metadata.json, bagit.txt, a payload file,
        // manifest-sha512.txt. The emitter's regenerate_bagit_manifest
        // writes manifest-sha512.txt; we just need the descriptor + payload.
        std::fs::write(root.join("WORKFLOW.json"), b"{\"version\":\"1.0\"}").unwrap();
        let descriptor = serde_json::json!({
            "@context": "https://w3id.org/ro/crate/1.1/context",
            "@graph": [
                {
                    "@id": "ro-crate-metadata.json",
                    "@type": "CreativeWork",
                    "conformsTo": [{"@id": "https://w3id.org/ro/crate/1.1"}],
                    "about": {"@id": "./"}
                },
                {
                    "@id": "./",
                    "@type": "Dataset",
                    "name": "Test package for preview",
                    "description": "Finalize test",
                    "conformsTo": [{"@id": "https://w3id.org/ro/crate/1.1"}],
                    "hasPart": [{"@id": "WORKFLOW.json"}]
                },
                {
                    "@id": "WORKFLOW.json",
                    "@type": ["File", "ComputationalWorkflow"],
                    "name": "Workflow"
                }
            ]
        });
        std::fs::write(
            root.join("ro-crate-metadata.json"),
            serde_json::to_vec_pretty(&descriptor).unwrap(),
        )
        .unwrap();

        // Write the minimal BagIt tag files so regenerate_bagit_manifest has
        // something to work with.
        std::fs::write(
            root.join("bagit.txt"),
            b"BagIt-Version: 1.0\nTag-File-Character-Encoding: UTF-8\n",
        )
        .unwrap();
        std::fs::write(root.join("manifest-sha512.txt"), b"").unwrap();

        let clock = crate::clock::FrozenClock::default();
        finalize_evidence_registration_with_verifier(root, &clock, None).unwrap();

        // 1. `ro-crate-preview.html` must exist on disk.
        let preview_path = root.join("ro-crate-preview.html");
        assert!(
            preview_path.exists(),
            "ro-crate-preview.html must be written by finalize"
        );

        // 2. The preview must be valid HTML with the JSON-LD embed.
        let preview_html = std::fs::read_to_string(&preview_path).unwrap();
        assert!(
            preview_html.starts_with("<!DOCTYPE html>"),
            "valid HTML5 doctype"
        );
        assert!(
            preview_html.contains("<script type=\"application/ld+json\">"),
            "JSON-LD head embed (spec MUST)"
        );
        assert!(
            preview_html.contains("Test package for preview"),
            "root name rendered in body"
        );
        assert!(
            !preview_html.to_lowercase().contains("<script>"),
            "no executable JS in preview"
        );

        // 3. `ro-crate-preview.html` must be registered in the @graph.
        let final_meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("ro-crate-metadata.json")).unwrap())
                .unwrap();
        let graph = final_meta["@graph"].as_array().unwrap();

        let preview_entity = graph
            .iter()
            .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("ro-crate-preview.html"))
            .expect("ro-crate-preview.html entity must be in @graph");
        let types: Vec<&str> = match &preview_entity["@type"] {
            serde_json::Value::Array(a) => a.iter().filter_map(|v| v.as_str()).collect(),
            serde_json::Value::String(s) => vec![s.as_str()],
            _ => vec![],
        };
        assert!(
            types.contains(&"File") && types.contains(&"CreativeWork"),
            "preview entity must be typed [\"File\",\"CreativeWork\"]; got {types:?}"
        );

        // 4. Linked from root `hasPart`.
        let root_entity = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let part_ids: Vec<&str> = root_entity["hasPart"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["@id"].as_str())
            .collect();
        assert!(
            part_ids.contains(&"ro-crate-preview.html"),
            "ro-crate-preview.html must be in root hasPart; got {part_ids:?}"
        );

        // 5. Idempotent: second call keeps the entity exactly once in @graph.
        finalize_evidence_registration_with_verifier(root, &clock, None).unwrap();
        let final_meta2: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("ro-crate-metadata.json")).unwrap())
                .unwrap();
        let graph2 = final_meta2["@graph"].as_array().unwrap();
        let preview_count = graph2
            .iter()
            .filter(|e| e.get("@id").and_then(|v| v.as_str()) == Some("ro-crate-preview.html"))
            .count();
        assert_eq!(
            preview_count, 1,
            "ro-crate-preview.html entity must appear exactly once in @graph (idempotent)"
        );
    }

    // ── env.lock parser unit tests ──────────────────────────────────────────

    /// Helper: build a temp package containing env.lock fixtures and a minimal
    /// ro-crate-metadata.json with a ComputationalWorkflow entity.
    fn write_env_lock_package(
        root: &std::path::Path,
        tasks: &[(&str, &str)], // (task_name, env.lock content)
    ) {
        let descriptor = json!({
            "@context": "https://w3id.org/ro/crate/1.1/context",
            "@graph": [
                {"@id": "ro-crate-metadata.json", "@type": "CreativeWork", "about": {"@id": "./"}},
                {"@id": "./", "@type": "Dataset", "hasPart": []},
                {"@id": "WORKFLOW.json", "@type": ["File", "ComputationalWorkflow"], "name": "wf"}
            ]
        });
        std::fs::write(
            root.join("ro-crate-metadata.json"),
            serde_json::to_vec_pretty(&descriptor).unwrap(),
        )
        .unwrap();
        for (task, content) in tasks {
            let dir = root.join("runtime").join("outputs").join(task);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("env.lock"), content).unwrap();
        }
    }

    /// Mixed env.lock covering all three line shapes plus noise lines.
    const MIXED_ENV_LOCK: &str = r#"
# This file was auto-generated
conda env: deseq2_vst_env
channel: bioconda + conda-forge
Running under: Debian GNU/Linux 12 (bookworm)

bioconductor-deseq2: 1.50.2
r-jsonlite: 2.0.0
numpy: 1.26.4

pydeseq2==0.5.4
gseapy==1.3.0
name @ file:///opt/conda/pkgs/skipped-1.0.0.tar.bz2

other attached packages:
[1] DESeq2_1.50.2
[2] SummarizedExperiment_1.40.0
[3] Biobase_2.70.0

loaded via a namespace (and not attached):
[1] GenomicRanges_1.52.0
"#;

    /// T1: pip, conda, and R sessionInfo tools are all registered with correct
    /// applicationCategory and softwareVersion.
    #[test]
    fn env_lock_registers_all_three_line_shapes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        write_env_lock_package(root, &[("task1", MIXED_ENV_LOCK)]);

        let n = register_software_from_env_locks(root).unwrap();
        assert!(
            n >= 4,
            "at least pydeseq2, gseapy, bioconductor-deseq2, DESeq2 registered; got {n}"
        );

        let doc: Value =
            serde_json::from_slice(&std::fs::read(root.join("ro-crate-metadata.json")).unwrap())
                .unwrap();
        let g = doc["@graph"].as_array().unwrap();

        // pip: pydeseq2
        let pydeseq = g
            .iter()
            .find(|e| e["@id"] == "#dep/python/pydeseq2")
            .expect("pydeseq2 node present");
        assert_eq!(pydeseq["@type"].as_str(), Some("SoftwareApplication"));
        assert_eq!(pydeseq["applicationCategory"].as_str(), Some("python"));
        assert_eq!(pydeseq["softwareVersion"].as_str(), Some("0.5.4"));
        assert_eq!(pydeseq["name"].as_str(), Some("pydeseq2"));

        // pip: gseapy
        let gseapy = g
            .iter()
            .find(|e| e["@id"] == "#dep/python/gseapy")
            .expect("gseapy node present");
        assert_eq!(gseapy["applicationCategory"].as_str(), Some("python"));
        assert_eq!(gseapy["softwareVersion"].as_str(), Some("1.3.0"));

        // conda: bioconductor-deseq2
        let bdeseq = g
            .iter()
            .find(|e| e["@id"] == "#dep/conda/bioconductor-deseq2")
            .expect("bioconductor-deseq2 node present");
        assert_eq!(bdeseq["applicationCategory"].as_str(), Some("conda"));
        assert_eq!(bdeseq["softwareVersion"].as_str(), Some("1.50.2"));

        // R sessionInfo: DESeq2
        let rdeseq = g
            .iter()
            .find(|e| e["@id"] == "#dep/r/DESeq2")
            .expect("DESeq2 node present");
        assert_eq!(rdeseq["applicationCategory"].as_str(), Some("r"));
        assert_eq!(rdeseq["softwareVersion"].as_str(), Some("1.50.2"));
    }

    /// T2: Noise lines (@ file://, #comments, conda env:, channel:, Running under:)
    /// are NOT registered as SoftwareApplication nodes.
    #[test]
    fn env_lock_skips_noise_lines() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        write_env_lock_package(root, &[("task1", MIXED_ENV_LOCK)]);

        register_software_from_env_locks(root).unwrap();
        let doc: Value =
            serde_json::from_slice(&std::fs::read(root.join("ro-crate-metadata.json")).unwrap())
                .unwrap();
        let g = doc["@graph"].as_array().unwrap();

        // pip-from-conda artifact must not appear
        assert!(
            !g.iter().any(|e| e["name"].as_str() == Some("name")),
            "pip `name @ file://` artifact must not be registered"
        );
        // Conda metadata lines must not appear
        assert!(
            !g.iter().any(|e| e["name"].as_str() == Some("conda env")),
            "`conda env:` metadata line must not be registered"
        );
        assert!(
            !g.iter().any(|e| e["name"].as_str() == Some("channel")),
            "`channel:` metadata line must not be registered"
        );
        assert!(
            !g.iter().any(|e| {
                e["name"]
                    .as_str()
                    .map(|s| s.starts_with("Running under"))
                    .unwrap_or(false)
            }),
            "`Running under:` line must not be registered"
        );
        // Loaded-via-namespace tools must not appear (they are in the filtered section)
        assert!(
            !g.iter()
                .any(|e| e["@id"].as_str() == Some("#dep/r/GenomicRanges")),
            "`loaded via a namespace` tools must not be registered"
        );
    }

    /// T3: A tool appearing in two tasks' env.lock files is registered ONCE (dedup).
    #[test]
    fn env_lock_deduplicates_across_tasks() {
        let lock_a = "pydeseq2==0.5.4\ngseapy==1.3.0\n";
        let lock_b = "pydeseq2==0.5.4\nmatplotlib==3.9.0\n"; // pydeseq2 duplicated

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        write_env_lock_package(root, &[("task_a", lock_a), ("task_b", lock_b)]);

        let n = register_software_from_env_locks(root).unwrap();
        assert_eq!(n, 3, "pydeseq2 + gseapy + matplotlib = 3, no duplicate");

        let doc: Value =
            serde_json::from_slice(&std::fs::read(root.join("ro-crate-metadata.json")).unwrap())
                .unwrap();
        let g = doc["@graph"].as_array().unwrap();
        let pydeseq_count = g
            .iter()
            .filter(|e| e["@id"].as_str() == Some("#dep/python/pydeseq2"))
            .count();
        assert_eq!(pydeseq_count, 1, "pydeseq2 registered exactly once");
    }

    /// T4: Each registered @id is appended to the workflow's softwareRequirements.
    #[test]
    fn env_lock_links_into_software_requirements() {
        let lock = "scipy==1.13.0\nnumpy==1.26.4\n";

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        write_env_lock_package(root, &[("task1", lock)]);

        register_software_from_env_locks(root).unwrap();
        let doc: Value =
            serde_json::from_slice(&std::fs::read(root.join("ro-crate-metadata.json")).unwrap())
                .unwrap();
        let g = doc["@graph"].as_array().unwrap();
        let wf = g.iter().find(|e| e["@id"] == "WORKFLOW.json").unwrap();
        let reqs: Vec<&str> = wf["softwareRequirements"]
            .as_array()
            .expect("softwareRequirements present")
            .iter()
            .filter_map(|r| r["@id"].as_str())
            .collect();
        assert!(reqs.contains(&"#dep/python/scipy"), "scipy linked");
        assert!(reqs.contains(&"#dep/python/numpy"), "numpy linked");
    }

    /// T5: Idempotent — a second call returns 0 and adds no duplicate nodes.
    #[test]
    fn env_lock_registration_is_idempotent() {
        let lock = "pydeseq2==0.5.4\n";

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        write_env_lock_package(root, &[("task1", lock)]);

        let n1 = register_software_from_env_locks(root).unwrap();
        assert_eq!(n1, 1);

        let n2 = register_software_from_env_locks(root).unwrap();
        assert_eq!(n2, 0, "second call must return 0 (idempotent)");

        let doc: Value =
            serde_json::from_slice(&std::fs::read(root.join("ro-crate-metadata.json")).unwrap())
                .unwrap();
        let count = doc["@graph"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["@id"].as_str() == Some("#dep/python/pydeseq2"))
            .count();
        assert_eq!(count, 1, "no duplicate pydeseq2 node after second call");
    }

    /// T6: No env.lock dir → Ok(0), no crash.
    #[test]
    fn env_lock_no_dir_returns_zero() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        // Write descriptor only; no runtime/outputs/ at all.
        let descriptor = json!({
            "@context": "https://w3id.org/ro/crate/1.1/context",
            "@graph": [
                {"@id": "ro-crate-metadata.json", "@type": "CreativeWork", "about": {"@id": "./"}},
                {"@id": "./", "@type": "Dataset", "hasPart": []},
                {"@id": "WORKFLOW.json", "@type": ["File", "ComputationalWorkflow"], "name": "wf"}
            ]
        });
        std::fs::write(
            root.join("ro-crate-metadata.json"),
            serde_json::to_vec_pretty(&descriptor).unwrap(),
        )
        .unwrap();

        let n = register_software_from_env_locks(root).unwrap();
        assert_eq!(n, 0, "no env.lock dir → Ok(0)");
    }

    /// T7: parse_env_lock — unit-level coverage of the parser helper.
    #[test]
    fn parse_env_lock_unit_coverage() {
        let content = r#"
# comment line skipped
conda env: my_env
channel: bioconda
Running under: Debian 12

bioconductor-deseq2: 1.50.2
r-jsonlite: 2.0.0
pydeseq2==0.5.4
name @ file:///bad/path==1.0.0

other attached packages:
[1] DESeq2_1.50.2 SummarizedExperiment_1.40.0

loaded via a namespace (and not attached):
[1] GenomicRanges_1.52.0
"#;
        let mut seen = std::collections::BTreeMap::new();
        parse_env_lock(content, &mut seen);

        // Expected registrations
        assert_eq!(
            seen.get(&("conda".into(), "bioconductor-deseq2".into()))
                .map(String::as_str),
            Some("1.50.2")
        );
        assert_eq!(
            seen.get(&("conda".into(), "r-jsonlite".into()))
                .map(String::as_str),
            Some("2.0.0")
        );
        assert_eq!(
            seen.get(&("python".into(), "pydeseq2".into()))
                .map(String::as_str),
            Some("0.5.4")
        );
        assert_eq!(
            seen.get(&("r".into(), "DESeq2".into())).map(String::as_str),
            Some("1.50.2")
        );
        assert_eq!(
            seen.get(&("r".into(), "SummarizedExperiment".into()))
                .map(String::as_str),
            Some("1.40.0")
        );

        // Must NOT be registered
        assert!(seen.get(&("conda".into(), "conda env".into())).is_none());
        assert!(seen.get(&("conda".into(), "channel".into())).is_none());
        assert!(seen.get(&("python".into(), "name".into())).is_none()); // @ file:// artifact
        assert!(seen.get(&("r".into(), "GenomicRanges".into())).is_none()); // loaded-via-namespace
    }

    /// B2: the main workflow entity's `@type` includes `SoftwareSourceCode`
    /// (WRROC "Main Workflow type" REQUIRED check).
    #[test]
    fn main_workflow_type_includes_softwaresourcecode() {
        let dag = one_task_dag();
        let classification = ClassificationResult {
            domain: "genomics".into(),
            workflow_description: "test workflow".into(),
            intake_text: "test intake".into(),
            edam_topic: "topic:3673".into(),
            edam_operation: "operation:3223".into(),
            ..Default::default()
        };
        let meta = build_metadata(&dag, &classification, &FrozenClock::default());
        let wf = meta["@graph"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| {
                e["@type"]
                    .as_array()
                    .map_or(false, |a| a.iter().any(|t| t == "ComputationalWorkflow"))
            })
            .unwrap();
        let types: Vec<&str> = wf["@type"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.as_str())
            .collect();
        assert!(types.contains(&"File"));
        assert!(types.contains(&"SoftwareSourceCode"));
        assert!(types.contains(&"ComputationalWorkflow"));
    }

    // ── Design §5.2 C5 — observed-provenance reconciliation into the
    // RO-Crate graph ──────────────────────────────────────────────────

    fn de_one_of_edges() -> Vec<crate::workflow_contracts::edge::EdgeContract> {
        use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
        vec![
            EdgeContract {
                from_node: "quantification".into(),
                from_port: "count_matrix".into(),
                to_node: "differential_expression".into(),
                to_port: "raw_counts".into(),
                proof: CompatibilityProof::default(),
                kind: EdgeKind::TypedDataFlow,
                chain_of_custody: None,
                mutually_exclusive_group: Some("counts".into()),
            },
            EdgeContract {
                from_node: "normalisation".into(),
                from_port: "normalized_counts".into(),
                to_node: "differential_expression".into(),
                to_port: "normalized_counts".into(),
                proof: CompatibilityProof::default(),
                kind: EdgeKind::TypedDataFlow,
                chain_of_custody: None,
                mutually_exclusive_group: Some("counts".into()),
            },
        ]
    }

    fn graph_with_parameter_connections(
        edges: &[crate::workflow_contracts::edge::EdgeContract],
    ) -> Value {
        let mut graph: Vec<Value> = vec![json!({"@id": "./", "@type": "Dataset", "hasPart": []})];
        for e in edges {
            graph.push(parameter_connection_entity(
                &format!("{}__to__{}", e.from_node, e.to_node),
                &format!("#step-{}", e.from_node),
                &e.from_port,
                &format!("#step-{}", e.to_node),
                &e.to_port,
            ));
        }
        json!({"@graph": graph})
    }

    /// Resolve a root-Dataset side-channel property (an array of
    /// `{"@id": …}` references) to the full `@graph` nodes each reference
    /// points at, preserving order. Panics if any reference lacks an `@id`
    /// or fails to resolve — the exact invariant the RO-Crate/runcrate `@id`
    /// fix must uphold.
    fn resolve_side_channel<'a>(graph: &'a [Value], root: &Value, key: &str) -> Vec<&'a Value> {
        root[key]
            .as_array()
            .unwrap_or_else(|| panic!("root Dataset carries a `{key}` reference array"))
            .iter()
            .map(|r| {
                let id = r["@id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("`{key}` element is an `{{@id}}` reference: {r:?}"));
                assert_eq!(
                    r.as_object().map(|o| o.len()),
                    Some(1),
                    "`{key}` element must be a bare reference (only `@id`), got: {r:?}"
                );
                graph
                    .iter()
                    .find(|e| e["@id"] == id)
                    .unwrap_or_else(|| panic!("`{key}` reference {id} resolves to a @graph node"))
            })
            .collect()
    }

    /// §G-B1 — once observed reads resolve the authoritative one-of member,
    /// the STANDARD graph must show ONLY the authoritative `ParameterConnection`
    /// for the count port; the unread candidate is DROPPED (not annotated) and
    /// recorded ONLY in the `ecaax:unusedCandidateEdge` side channel.
    #[test]
    fn reconcile_drops_unread_one_of_member_from_standard_graph_and_side_channels_it() {
        let edges = de_one_of_edges();
        let mut metadata = graph_with_parameter_connections(&edges);
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: "runtime/outputs/quantification/count_matrix.tsv".into(),
        }];

        reconcile_ro_crate_edges(&mut metadata, &edges, &reads);

        let graph = metadata["@graph"].as_array().unwrap();
        // The authoritative raw-counts edge is KEPT and stamped.
        let raw_node = graph
            .iter()
            .find(|e| {
                e["@id"] == "#parameter-connection/quantification__to__differential_expression"
            })
            .expect("raw_counts ParameterConnection node present");
        assert_eq!(raw_node["ecaax:provenanceStatus"], "authoritative");

        // The unread normalized-counts edge is GONE from the standard graph —
        // a generic RO-Crate/WRROC/runcrate consumer never sees it as a
        // ParameterConnection data flow.
        assert!(
            graph
                .iter()
                .all(|e| e["@id"]
                    != "#parameter-connection/normalisation__to__differential_expression"),
            "the unread one-of candidate must NOT remain as a standard ParameterConnection"
        );
        // No surviving ParameterConnection references the dropped normalisation
        // producer for the count port.
        assert!(
            graph.iter().all(|e| {
                let is_pc = e.get("@type").and_then(Value::as_str) == Some("ParameterConnection");
                !(is_pc
                    && e.get("sourceParameter")
                        .and_then(|s| s.get("@id"))
                        .and_then(Value::as_str)
                        .map(|s| s.starts_with("#step-normalisation"))
                        .unwrap_or(false))
            }),
            "no standard ParameterConnection may still wire the unread normalisation edge"
        );

        // The unread member survives ONLY in the ecaax side channel — now a
        // root reference resolving to a first-class @graph node carrying the
        // fields (so a strict validator sees a proper `@id`-bearing entity).
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let unused = resolve_side_channel(graph, root, "ecaax:unusedCandidateEdge");
        assert_eq!(unused.len(), 1);
        assert!(unused[0]["@id"]
            .as_str()
            .unwrap()
            .starts_with("#unused-candidate-edge/"));
        assert_eq!(unused[0]["from_node"], "normalisation");
        assert_eq!(unused[0]["to_node"], "differential_expression");
        assert_eq!(unused[0]["to_port"], "normalized_counts");
        assert_eq!(unused[0]["mutually_exclusive_group"], "counts");
        assert_eq!(unused[0]["ecaax:provenanceStatus"], "candidate_unused");
        assert_eq!(unused[0]["ecaax:supersededByProducer"], "quantification");
    }

    #[test]
    fn reconcile_is_idempotent_on_repeated_runs() {
        let edges = de_one_of_edges();
        let mut metadata = graph_with_parameter_connections(&edges);
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: "runtime/outputs/quantification/count_matrix.tsv".into(),
        }];

        reconcile_ro_crate_edges(&mut metadata, &edges, &reads);
        reconcile_ro_crate_edges(&mut metadata, &edges, &reads);

        let graph = metadata["@graph"].as_array().unwrap();
        // Exactly one node per @id — no duplication from the second pass.
        let raw_matches = graph
            .iter()
            .filter(|e| {
                e["@id"] == "#parameter-connection/quantification__to__differential_expression"
            })
            .count();
        assert_eq!(raw_matches, 1);
    }

    #[test]
    fn reconcile_records_divergent_read_on_root_dataset() {
        let edges = de_one_of_edges();
        let mut metadata = graph_with_parameter_connections(&edges);
        // A read that matches neither declared producer's output dir.
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: None,
            path: "runtime/outputs/data_acquisition/counts.tsv".into(),
        }];

        reconcile_ro_crate_edges(&mut metadata, &edges, &reads);

        let graph = metadata["@graph"].as_array().unwrap();
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let divergences = root["ecaax:provenanceDivergence"]
            .as_array()
            .expect("divergence array recorded on root Dataset");
        assert_eq!(divergences.len(), 1);
        // The root references the divergence by `@id` (a flattened `@graph`
        // node), NOT an inline value object — a strict runcrate/WRROC validator
        // rejects an inline object with no `@id` (the substrate_validity bug).
        let div_id = divergences[0]["@id"]
            .as_str()
            .expect("divergence referenced by @id");
        assert_eq!(div_id, "#provenance-divergence/differential_expression/0");
        assert!(
            divergences[0].get("read_path").is_none(),
            "divergence must be referenced by @id on the root, not inlined"
        );
        let div_node = graph
            .iter()
            .find(|e| e["@id"] == div_id)
            .expect("divergence node flattened into @graph with its own @id");
        assert_eq!(
            div_node["read_path"],
            "runtime/outputs/data_acquisition/counts.tsv"
        );
        assert!(
            div_node["@type"]
                .as_array()
                .map(|a| a.iter().any(|t| t == "ecaax:ProvenanceDivergence"))
                .unwrap_or(false),
            "flattened divergence node must be typed ecaax:ProvenanceDivergence"
        );

        // §G-B1 — an UNRESOLVED group (the read matched neither producer)
        // must keep BOTH members as candidates; we never fabricate a
        // resolution and never drop a member we cannot rule out.
        let raw_node = graph
            .iter()
            .find(|e| {
                e["@id"] == "#parameter-connection/quantification__to__differential_expression"
            })
            .unwrap();
        assert_eq!(raw_node["ecaax:provenanceStatus"], "candidate_unused");
        let normalized_node = graph
            .iter()
            .find(|e| {
                e["@id"] == "#parameter-connection/normalisation__to__differential_expression"
            })
            .expect("unresolved one-of members are both kept in the standard graph");
        assert_eq!(
            normalized_node["ecaax:provenanceStatus"],
            "candidate_unused"
        );
        // No candidate was dropped, so the side channel is absent.
        assert!(root.get("ecaax:unusedCandidateEdge").is_none());
    }

    #[test]
    fn reconcile_deduplicates_retried_observed_reads() {
        let edges = de_one_of_edges();
        let mut metadata = graph_with_parameter_connections(&edges);
        let read = crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: None,
            path: "runtime/outputs/data_acquisition/counts.tsv".into(),
        };

        let returned =
            reconcile_ro_crate_edges(&mut metadata, &edges, &[read.clone(), read.clone()]);

        assert_eq!(
            returned.len(),
            1,
            "one consumed path must yield one typed divergence even when retry epochs repeat it"
        );
        let graph = metadata["@graph"].as_array().unwrap();
        let root = graph.iter().find(|entry| entry["@id"] == "./").unwrap();
        assert_eq!(
            root["ecaax:provenanceDivergence"].as_array().map(Vec::len),
            Some(1),
            "the RO-Crate side channel must also contain one divergence node"
        );
    }

    /// RCA I-1 (Task 13) — a divergent read on a task carrying a
    /// declared `read_allowance` is sanctioned (recorded under
    /// `ecaax:provenanceReadAllowance` with its rationale), NOT
    /// flagged in `ecaax:provenanceDivergence`. Complements
    /// `reconcile_records_divergent_read_on_root_dataset` (same
    /// divergent-read shape, no allowance) as the positive case.
    #[test]
    fn reconcile_with_allowances_sanctions_a_covered_divergent_read() {
        let edges = de_one_of_edges();
        let mut metadata = graph_with_parameter_connections(&edges);
        // differential_expression has no declared edge from
        // data_acquisition — an ordinary divergent read — but we grant
        // it an any_upstream_stage allowance for this test.
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: None,
            path: "runtime/outputs/data_acquisition/counts.tsv".into(),
        }];
        let mut allowances = std::collections::BTreeMap::new();
        allowances.insert(
            "differential_expression".to_string(),
            vec![crate::atom::ReadAllowance {
                scope: crate::atom::ReadAllowanceScope::AnyUpstreamStage,
                rationale: "test: aggregates any upstream stage".into(),
            }],
        );

        reconcile_ro_crate_edges_with_allowances(&mut metadata, &edges, &reads, &allowances);

        let graph = metadata["@graph"].as_array().unwrap();
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        assert!(
            root.get("ecaax:provenanceDivergence").is_none(),
            "a covered divergent read must not surface as a divergence: {:?}",
            root.get("ecaax:provenanceDivergence")
        );
        // The sanctioned read is a root reference resolving to a first-class
        // @graph node carrying the fields (validator-acceptable `@id`).
        let allowed = resolve_side_channel(graph, root, "ecaax:provenanceReadAllowance");
        assert_eq!(allowed.len(), 1);
        assert_eq!(
            allowed[0]["@id"],
            "#read-allowance/differential_expression/0"
        );
        assert_eq!(allowed[0]["task_id"], "differential_expression");
        assert_eq!(
            allowed[0]["read_path"],
            "runtime/outputs/data_acquisition/counts.tsv"
        );
        assert_eq!(
            allowed[0]["rationale"],
            "test: aggregates any upstream stage"
        );
    }

    /// A task's `read_allowance` covers ONLY that task — a sibling task
    /// with the identical divergent-read shape but no allowance of its
    /// own still surfaces the divergence. Guards against a keying bug
    /// that would apply one task's allowance to every task.
    #[test]
    fn reconcile_with_allowances_does_not_leak_across_tasks() {
        let edges = de_one_of_edges();
        let mut metadata = graph_with_parameter_connections(&edges);
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: None,
            path: "runtime/outputs/data_acquisition/counts.tsv".into(),
        }];
        // Allowance keyed to a DIFFERENT task id.
        let mut allowances = std::collections::BTreeMap::new();
        allowances.insert(
            "final_reporting".to_string(),
            vec![crate::atom::ReadAllowance {
                scope: crate::atom::ReadAllowanceScope::AnyUpstreamStage,
                rationale: "unrelated task's allowance".into(),
            }],
        );

        reconcile_ro_crate_edges_with_allowances(&mut metadata, &edges, &reads, &allowances);

        let graph = metadata["@graph"].as_array().unwrap();
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let divergences = root["ecaax:provenanceDivergence"]
            .as_array()
            .expect("unallowanced task's divergence must still surface");
        assert_eq!(divergences.len(), 1);
        assert!(root.get("ecaax:provenanceReadAllowance").is_none());
    }

    #[test]
    fn reconcile_no_op_when_no_observed_reads() {
        let edges = de_one_of_edges();
        let mut metadata = graph_with_parameter_connections(&edges);
        let before = metadata.clone();

        reconcile_ro_crate_edges(&mut metadata, &edges, &[]);

        assert_eq!(
            metadata, before,
            "empty observed reads must leave the graph untouched"
        );
    }

    /// A `Divergent` verdict must be surfaced to the caller as a typed
    /// `DivergenceRecord`, not just folded into the JSON-LD graph — this is
    /// what lets `crates/conversation/src/emit/ro_crate.rs::patch_ro_crate_metadata`'s
    /// caller transition the offending task to a typed blocker (design §5.2,
    /// T12).
    #[test]
    fn reconcile_returns_divergence_record_for_undeclared_read() {
        let edges = de_one_of_edges();
        let mut metadata = graph_with_parameter_connections(&edges);
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("normalized_counts".into()),
            path: "runtime/outputs/data_acquisition/counts.tsv".into(),
        }];

        let divergences = reconcile_ro_crate_edges(&mut metadata, &edges, &reads);

        assert_eq!(
            divergences,
            vec![crate::provenance::DivergenceRecord {
                task_id: "differential_expression".into(),
                read_path: "runtime/outputs/data_acquisition/counts.tsv".into(),
                declared_producer: Some("normalisation".into()),
            }]
        );
    }

    /// No divergence anywhere → the returned record list is empty (mirrors
    /// `reconcile_marks_read_one_of_member_authoritative_and_sibling_candidate`'s
    /// all-Match fixture).
    #[test]
    fn reconcile_returns_no_divergence_records_when_all_reads_match() {
        let edges = de_one_of_edges();
        let mut metadata = graph_with_parameter_connections(&edges);
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: "runtime/outputs/quantification/count_matrix.tsv".into(),
        }];

        let divergences = reconcile_ro_crate_edges(&mut metadata, &edges, &reads);

        assert!(
            divergences.is_empty(),
            "expected no divergence records, got {divergences:?}"
        );
    }

    /// Build the DE one-of graph AND the consuming task's retrospective
    /// `CreateAction` exactly as the SESSION path leaves it: the server
    /// registers the CreateAction DURING execution — BEFORE the end-of-run
    /// reconcile drop — so its `object` (PROV `used`) already lists BOTH
    /// producers' outputs (the authoritative one AND the unread one-of
    /// sibling). `used_objects` are the `object` `@id`s to seed.
    fn graph_with_de_create_action(
        edges: &[crate::workflow_contracts::edge::EdgeContract],
        used_objects: &[&str],
    ) -> Value {
        let mut metadata = graph_with_parameter_connections(edges);
        let de_result = "runtime/outputs/differential_expression/tables/de_results.tsv";
        let object: Vec<Value> = used_objects.iter().map(|id| json!({"@id": id})).collect();
        metadata["@graph"].as_array_mut().unwrap().push(json!({
            "@id": format!("#action/{de_result}"),
            "@type": ["CreateAction", "prov:Activity"],
            "name": "Production of de_results.tsv by stage 'differential_expression'.",
            "instrument": {"@id": "#step-differential_expression"},
            "result": {"@id": de_result},
            "object": object,
        }));
        metadata
    }

    fn de_create_action_object_ids(metadata: &Value) -> Vec<String> {
        metadata["@graph"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| {
                e["@id"] == "#action/runtime/outputs/differential_expression/tables/de_results.tsv"
            })
            .expect("DE CreateAction present")["object"]
            .as_array()
            .expect("CreateAction.object is an array")
            .iter()
            .map(|o| o["@id"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// §G-B1 (session-path gap) — the reconcile drops the unread one-of
    /// member's `ParameterConnection` (existing behavior), AND must ALSO prune
    /// the consuming task's `CreateAction.object` (PROV `used`) so a generic
    /// runcrate/WRROC consumer never reads the unread producer's output as an
    /// authoritative data flow. After reconcile the consuming `object` MUST
    /// `used` ONLY the authoritative producer's output; the unread producer's
    /// output is recorded ONLY in the `ecaax:` side channel.
    #[test]
    fn reconcile_prunes_unread_producer_from_consuming_task_create_action_object() {
        let edges = de_one_of_edges();
        // The session path registered the DE CreateAction pre-drop → BOTH
        // producers' concrete outputs are in `used`.
        let mut metadata = graph_with_de_create_action(
            &edges,
            &[
                "runtime/outputs/quantification/count_matrix.tsv",
                "runtime/outputs/normalisation/normalized_counts.tsv",
            ],
        );
        // Observed reads bind the RAW member (quantification is authoritative).
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: "runtime/outputs/quantification/count_matrix.tsv".into(),
        }];

        reconcile_ro_crate_edges(&mut metadata, &edges, &reads);

        let graph = metadata["@graph"].as_array().unwrap();
        // (a) the unread ParameterConnection is dropped (existing behavior).
        assert!(
            graph
                .iter()
                .all(|e| e["@id"]
                    != "#parameter-connection/normalisation__to__differential_expression"),
            "the unread one-of candidate must NOT remain as a standard ParameterConnection"
        );

        // (b) the consuming CreateAction `object` now `used`s ONLY the
        // authoritative producer's output.
        let objects = de_create_action_object_ids(&metadata);
        assert_eq!(
            objects,
            vec!["runtime/outputs/quantification/count_matrix.tsv".to_string()],
            "the unread producer's output must be pruned from CreateAction.object"
        );

        // (c) the pruned `used` entry is recorded in the ecaax side channel —
        // a root reference resolving to the first-class @graph node.
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let unused = resolve_side_channel(graph, root, "ecaax:unusedCandidateEdge");
        assert_eq!(unused.len(), 1);
        assert_eq!(unused[0]["from_node"], "normalisation");
        assert_eq!(
            unused[0]["ecaax:prunedUsedObject"]
                .as_array()
                .expect("pruned-used-object recorded on the dropped edge"),
            &vec![json!("runtime/outputs/normalisation/normalized_counts.tsv")]
        );
    }

    /// The `object` prune is idempotent: a second reconcile over the same
    /// (already-pruned) graph leaves the authoritative-only `used` array
    /// unchanged and never re-introduces the unread producer.
    #[test]
    fn reconcile_object_prune_is_idempotent() {
        let edges = de_one_of_edges();
        let mut metadata = graph_with_de_create_action(
            &edges,
            &[
                "runtime/outputs/quantification/count_matrix.tsv",
                "runtime/outputs/normalisation/normalized_counts.tsv",
            ],
        );
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: "runtime/outputs/quantification/count_matrix.tsv".into(),
        }];

        reconcile_ro_crate_edges(&mut metadata, &edges, &reads);
        reconcile_ro_crate_edges(&mut metadata, &edges, &reads);

        assert_eq!(
            de_create_action_object_ids(&metadata),
            vec!["runtime/outputs/quantification/count_matrix.tsv".to_string()],
        );
    }

    /// The prune also handles the `#step-<producer>` bare fallback form the
    /// registrar emits when the unread producer registered no output table.
    #[test]
    fn reconcile_prunes_bare_step_used_reference() {
        let edges = de_one_of_edges();
        let mut metadata =
            graph_with_de_create_action(&edges, &["#step-quantification", "#step-normalisation"]);
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: "runtime/outputs/quantification/count_matrix.tsv".into(),
        }];

        reconcile_ro_crate_edges(&mut metadata, &edges, &reads);

        assert_eq!(
            de_create_action_object_ids(&metadata),
            vec!["#step-quantification".to_string()],
            "the unread producer's bare step ref must be pruned from CreateAction.object"
        );
    }

    /// An UNRESOLVED one-of group (the read matched neither declared producer)
    /// must keep BOTH `ParameterConnection`s AND leave the consuming
    /// `CreateAction.object` untouched — we never prune a member we cannot
    /// rule out.
    #[test]
    fn reconcile_unresolved_one_of_keeps_both_create_action_object_entries() {
        let edges = de_one_of_edges();
        let mut metadata = graph_with_de_create_action(
            &edges,
            &[
                "runtime/outputs/quantification/count_matrix.tsv",
                "runtime/outputs/normalisation/normalized_counts.tsv",
            ],
        );
        // A read matching NEITHER producer's output dir → unresolved group.
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: None,
            path: "runtime/outputs/data_acquisition/counts.tsv".into(),
        }];

        reconcile_ro_crate_edges(&mut metadata, &edges, &reads);

        assert_eq!(
            de_create_action_object_ids(&metadata),
            vec![
                "runtime/outputs/quantification/count_matrix.tsv".to_string(),
                "runtime/outputs/normalisation/normalized_counts.tsv".to_string(),
            ],
            "an unresolved one-of must leave both `used` entries in place"
        );
    }

    // ── RO-Crate/runcrate `@id` fix — the sanctioned-read + unused-candidate
    // side channels must be first-class `@graph` entities referenced by `@id`
    // from the root, never inline value objects (which fail a strict
    // validator with "no @id in {…}") ────────────────────────────────────

    /// Both side channels populated in one pass: the read one-of member
    /// resolves the group (normalisation DROPPED → `unusedCandidateEdge`),
    /// while an allowance-covered divergent read lands under
    /// `provenanceReadAllowance`. Every root element is a bare `{@id}`
    /// reference; each `@id` resolves to a `@graph` node carrying the fields
    /// and an `@type` — the invariant the "no @id" fix upholds.
    #[test]
    fn reconcile_side_channels_are_id_referenced_graph_entities() {
        let edges = de_one_of_edges();
        let mut metadata = graph_with_parameter_connections(&edges);
        let reads = vec![
            // Binds the RAW member authoritative → normalisation is dropped.
            crate::provenance::ObservedRead {
                task_id: "differential_expression".into(),
                declared_port: Some("raw_counts".into()),
                path: "runtime/outputs/quantification/count_matrix.tsv".into(),
            },
            // An extra divergent read, covered below by an allowance.
            crate::provenance::ObservedRead {
                task_id: "differential_expression".into(),
                declared_port: None,
                path: "runtime/outputs/data_acquisition/counts.tsv".into(),
            },
        ];
        let mut allowances = std::collections::BTreeMap::new();
        allowances.insert(
            "differential_expression".to_string(),
            vec![crate::atom::ReadAllowance {
                scope: crate::atom::ReadAllowanceScope::AnyUpstreamStage,
                rationale: "test: aggregates any upstream stage".into(),
            }],
        );

        reconcile_ro_crate_edges_with_allowances(&mut metadata, &edges, &reads, &allowances);

        let graph = metadata["@graph"].as_array().unwrap();
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();

        // The raw root arrays hold ONLY references — no inline fields leak.
        for key in ["ecaax:provenanceReadAllowance", "ecaax:unusedCandidateEdge"] {
            for elem in root[key].as_array().unwrap() {
                assert_eq!(
                    elem.as_object().map(|o| o.len()),
                    Some(1),
                    "{key} element must be a bare @id reference, got {elem:?}"
                );
                assert!(elem["@id"].is_string(), "{key} element must carry an @id");
                assert!(
                    elem.get("task_id").is_none(),
                    "{key} reference must not inline the fields: {elem:?}"
                );
            }
        }

        // Each reference resolves to a @graph node carrying the fields + @type.
        let allowed = resolve_side_channel(graph, root, "ecaax:provenanceReadAllowance");
        assert_eq!(allowed.len(), 1);
        let allow_types: Vec<&str> = allowed[0]["@type"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.as_str())
            .collect();
        assert!(
            allow_types.contains(&"ecaax:ProvenanceReadAllowance"),
            "got {allow_types:?}"
        );
        assert_eq!(
            allowed[0]["read_path"],
            "runtime/outputs/data_acquisition/counts.tsv"
        );
        assert_eq!(
            allowed[0]["rationale"],
            "test: aggregates any upstream stage"
        );

        let unused = resolve_side_channel(graph, root, "ecaax:unusedCandidateEdge");
        assert_eq!(unused.len(), 1);
        let unused_types: Vec<&str> = unused[0]["@type"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.as_str())
            .collect();
        assert!(
            unused_types.contains(&"ecaax:UnusedCandidateEdge"),
            "got {unused_types:?}"
        );
        assert_eq!(unused[0]["from_node"], "normalisation");
        assert_eq!(unused[0]["ecaax:supersededByProducer"], "quantification");
    }

    /// Multiple sanctioned reads on ONE task get distinct
    /// `#read-allowance/<task>/<n>` fragment ids (0-based per task), so no two
    /// nodes collide on `@id`.
    #[test]
    fn reconcile_mints_distinct_read_allowance_ids_per_task() {
        let edges = de_one_of_edges();
        let mut metadata = graph_with_parameter_connections(&edges);
        let reads = vec![
            crate::provenance::ObservedRead {
                task_id: "differential_expression".into(),
                declared_port: None,
                path: "runtime/outputs/data_acquisition/counts.tsv".into(),
            },
            crate::provenance::ObservedRead {
                task_id: "differential_expression".into(),
                declared_port: None,
                path: "runtime/outputs/metadata_prep/design.tsv".into(),
            },
        ];
        let mut allowances = std::collections::BTreeMap::new();
        allowances.insert(
            "differential_expression".to_string(),
            vec![crate::atom::ReadAllowance {
                scope: crate::atom::ReadAllowanceScope::AnyUpstreamStage,
                rationale: "test: aggregates any upstream stage".into(),
            }],
        );

        reconcile_ro_crate_edges_with_allowances(&mut metadata, &edges, &reads, &allowances);

        let graph = metadata["@graph"].as_array().unwrap();
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let ids: std::collections::BTreeSet<&str> = root["ecaax:provenanceReadAllowance"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["@id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            std::collections::BTreeSet::from([
                "#read-allowance/differential_expression/0",
                "#read-allowance/differential_expression/1",
            ]),
            "per-task read-allowance ids must be distinct and 0-based"
        );
        // All ids resolve to distinct @graph nodes.
        assert_eq!(
            resolve_side_channel(graph, root, "ecaax:provenanceReadAllowance").len(),
            2
        );
    }

    // ── Fix 2: minimal port-alias map — every declared task input port maps
    // to its producer task+port, including composer-synthesized positional
    // port names (`companion_in_N` / `residual_in_N`) ─────────────────────

    /// A synthesized `companion_in_N` consumer port resolves to a
    /// `#port-alias/<task>/<port>` node naming the producer task + port, and
    /// the root `ecaax:portAliasMap` references it by `@id`.
    #[test]
    fn reconcile_emits_port_alias_for_synthetic_companion_port() {
        use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
        let edges = vec![EdgeContract {
            from_node: "survey_method_landscape".into(),
            from_port: "method_landscape".into(),
            to_node: "discover_markers".into(),
            to_port: "companion_in_1".into(),
            proof: CompatibilityProof::default(),
            kind: EdgeKind::OrderingOnly,
            chain_of_custody: None,
            mutually_exclusive_group: None,
        }];
        let mut metadata = graph_with_parameter_connections(&edges);
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "discover_markers".into(),
            declared_port: Some("companion_in_1".into()),
            path: "runtime/outputs/survey_method_landscape/method_landscape.json".into(),
        }];

        reconcile_ro_crate_edges(&mut metadata, &edges, &reads);

        let graph = metadata["@graph"].as_array().unwrap();
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let aliases = resolve_side_channel(graph, root, "ecaax:portAliasMap");
        assert_eq!(aliases.len(), 1, "one declared edge → one port-alias node");
        let alias = aliases[0];
        assert_eq!(alias["@id"], "#port-alias/discover_markers/companion_in_1");
        assert_eq!(alias["@type"], "ecaax:PortAlias");
        assert_eq!(alias["task"], "discover_markers");
        assert_eq!(alias["port"], "companion_in_1");
        assert_eq!(alias["from_node"], "survey_method_landscape");
        assert_eq!(alias["from_port"], "method_landscape");
    }

    // ── `CreateAction.object` (PROV `used`) rebuilt from OBSERVED reads ∪
    // DECLARED inputs, every entry marked with its provenance status ──────

    fn declared_edge(
        from_node: &str,
        from_port: &str,
        to_node: &str,
        to_port: &str,
    ) -> crate::workflow_contracts::edge::EdgeContract {
        use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
        EdgeContract {
            from_node: from_node.into(),
            from_port: from_port.into(),
            to_node: to_node.into(),
            to_port: to_port.into(),
            proof: CompatibilityProof::default(),
            kind: EdgeKind::TypedDataFlow,
            chain_of_custody: None,
            mutually_exclusive_group: None,
        }
    }

    /// A registered produced-output File entity, as
    /// `register_produced_output_tables` leaves it.
    fn registered_output_file(rel: &str) -> Value {
        json!({
            "@id": rel,
            "@type": ["File", "Dataset"],
            "name": rel,
            "encodingFormat": "text/tab-separated-values",
        })
    }

    /// A stage's retrospective production `CreateAction` over `rel`, seeded with
    /// the `used` array the DECLARED-graph registrar would have written.
    fn production_action(task: &str, rel: &str, used: &[&str]) -> Value {
        json!({
            "@id": format!("#action/{rel}"),
            "@type": ["CreateAction", "prov:Activity"],
            "name": format!("Production of {rel} by stage '{task}'."),
            "instrument": {"@id": format!("#step-{task}")},
            "result": {"@id": rel},
            "object": used.iter().map(|id| json!({"@id": id})).collect::<Vec<_>>(),
        })
    }

    /// The `used` (`CreateAction.object`) `@id`s of the action producing `rel`.
    fn used_ids(metadata: &Value, rel: &str) -> Vec<String> {
        metadata["@graph"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["@id"] == format!("#action/{rel}"))
            .unwrap_or_else(|| panic!("CreateAction for {rel} present"))["object"]
            .as_array()
            .expect("CreateAction.object is an array")
            .iter()
            .map(|o| {
                assert_eq!(
                    o.as_object().map(|m| m.len()),
                    Some(1),
                    "a `used` element must stay a bare {{@id}} reference, got {o:?}"
                );
                o["@id"]
                    .as_str()
                    .expect("used element carries @id")
                    .to_string()
            })
            .collect()
    }

    /// The `ecaax:provenanceStatus` recorded for `object_id` on `task`'s `used`
    /// list, resolved through the action's `ecaax:objectProvenance` references
    /// (which MUST be bare `@id` refs to real `@graph` nodes).
    fn used_status(metadata: &Value, rel: &str, object_id: &str) -> String {
        let graph = metadata["@graph"].as_array().unwrap();
        let action = graph
            .iter()
            .find(|e| e["@id"] == format!("#action/{rel}"))
            .unwrap_or_else(|| panic!("CreateAction for {rel} present"));
        let refs = action["ecaax:objectProvenance"]
            .as_array()
            .expect("action carries an objectProvenance reference array");
        for r in refs {
            assert_eq!(
                r.as_object().map(|m| m.len()),
                Some(1),
                "objectProvenance element must be a bare {{@id}} reference, got {r:?}"
            );
            let node = graph
                .iter()
                .find(|e| e["@id"] == r["@id"])
                .unwrap_or_else(|| panic!("objectProvenance ref {r:?} resolves to a @graph node"));
            if node["object"]["@id"] == object_id {
                return node["ecaax:provenanceStatus"]
                    .as_str()
                    .expect("provenance node carries a status")
                    .to_string();
            }
        }
        panic!("no objectProvenance node marks `used` entry {object_id}");
    }

    /// A task that READ a file its declared producer never registered as a
    /// production output (it is not in the producer's `task_outputs`) must still
    /// see that file in its `CreateAction.object` — marked observed — because
    /// the read is recorded evidence and the file is a registered `@graph`
    /// entity. Before the rebuild `object` mirrored the declared graph only, so
    /// the read was invisible.
    #[test]
    fn create_action_object_includes_observed_reads() {
        let edges = vec![declared_edge(
            "quantification",
            "count_matrix",
            "differential_expression",
            "raw_counts",
        )];
        let mut metadata = graph_with_parameter_connections(&edges);
        let de_out = "runtime/outputs/differential_expression/de_results.tsv";
        let counts = "runtime/outputs/quantification/count_matrix.tsv";
        // A registered sibling artifact of the SAME producer that is NOT the
        // result of any CreateAction — i.e. absent from `task_outputs`.
        let index = "runtime/outputs/quantification/matrices_index.tsv";
        {
            let graph = metadata["@graph"].as_array_mut().unwrap();
            graph.push(registered_output_file(counts));
            graph.push(registered_output_file(index));
            graph.push(production_action("quantification", counts, &[]));
            graph.push(production_action(
                "differential_expression",
                de_out,
                &[counts],
            ));
        }
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: index.into(),
        }];

        reconcile_ro_crate_edges(&mut metadata, &edges, &reads);

        let used = used_ids(&metadata, de_out);
        assert!(
            used.iter().any(|u| u == index),
            "the observed read must appear in CreateAction.object; got {used:?}"
        );
        assert_eq!(
            used_status(&metadata, de_out, index),
            "ecaax:observed",
            "an entry backed by a recorded read is marked observed"
        );
    }

    /// A declared input no read corroborates stays in `used` (we never drop a
    /// declared data flow on silence) but is marked `declaredOnly`, so a
    /// reviewer can tell a compile-time belief from recorded evidence.
    #[test]
    fn declared_but_unread_input_is_marked_declared_only() {
        let edges = vec![
            declared_edge(
                "quantification",
                "count_matrix",
                "differential_expression",
                "raw_counts",
            ),
            declared_edge(
                "metadata_prep",
                "design",
                "differential_expression",
                "design",
            ),
        ];
        let mut metadata = graph_with_parameter_connections(&edges);
        let de_out = "runtime/outputs/differential_expression/de_results.tsv";
        let counts = "runtime/outputs/quantification/count_matrix.tsv";
        let design = "runtime/outputs/metadata_prep/design.tsv";
        {
            let graph = metadata["@graph"].as_array_mut().unwrap();
            graph.push(registered_output_file(counts));
            graph.push(registered_output_file(design));
            graph.push(production_action("quantification", counts, &[]));
            graph.push(production_action("metadata_prep", design, &[]));
            graph.push(production_action(
                "differential_expression",
                de_out,
                &[counts, design],
            ));
        }
        // Only the counts input was actually read.
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: counts.into(),
        }];

        reconcile_ro_crate_edges(&mut metadata, &edges, &reads);

        let used = used_ids(&metadata, de_out);
        assert!(
            used.iter().any(|u| u == design),
            "a declared-but-unread input is kept, never silently dropped; got {used:?}"
        );
        assert_eq!(used_status(&metadata, de_out, design), "ecaax:declaredOnly");
        assert_eq!(used_status(&metadata, de_out, counts), "ecaax:observed");
    }

    /// The end-state the deposit audit demands: for a simple task, `used` and
    /// `reads.jsonl` agree EXACTLY — no read omitted from the action, no action
    /// entry absent from the reads.
    #[test]
    fn object_and_reads_reconcile_exactly_for_a_simple_task() {
        let edges = vec![declared_edge(
            "quantification",
            "count_matrix",
            "differential_expression",
            "raw_counts",
        )];
        let mut metadata = graph_with_parameter_connections(&edges);
        let de_out = "runtime/outputs/differential_expression/de_results.tsv";
        let counts = "runtime/outputs/quantification/count_matrix.tsv";
        {
            let graph = metadata["@graph"].as_array_mut().unwrap();
            graph.push(registered_output_file(counts));
            graph.push(production_action("quantification", counts, &[]));
            // The registrar wrote the ABSTRACT step fallback even though the
            // producer did register a file — the stale shape the rebuild
            // collapses.
            graph.push(production_action(
                "differential_expression",
                de_out,
                &["#step-quantification"],
            ));
        }
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: counts.into(),
        }];

        reconcile_ro_crate_edges(&mut metadata, &edges, &reads);

        let used = used_ids(&metadata, de_out);
        let read_paths: Vec<String> = reads.iter().map(|r| r.path.clone()).collect();
        assert_eq!(
            used, read_paths,
            "`used` must reconcile 1:1 with the observed reads — no omissions, no extras"
        );
        assert!(
            !used.iter().any(|u| u.starts_with("#step-")),
            "the abstract step fallback must not survive a producer that registered a file: {used:?}"
        );
        assert_eq!(used_status(&metadata, de_out, counts), "ecaax:observed");
    }

    /// A divergent read the consuming task's declared `read_allowance` sanctions
    /// is marked `allowanceCovered` on the `used` entry — distinguishable from
    /// both an ordinary observed read and an uncorroborated declared input.
    #[test]
    fn allowance_covered_read_is_marked_on_the_used_entry() {
        let edges = vec![declared_edge(
            "quantification",
            "count_matrix",
            "final_reporting",
            "counts",
        )];
        let mut metadata = graph_with_parameter_connections(&edges);
        let report = "runtime/outputs/final_reporting/summary.tsv";
        let counts = "runtime/outputs/quantification/count_matrix.tsv";
        // Read by final_reporting but produced by a stage it declares no edge
        // to — divergent, and sanctioned by the allowance below.
        let literature = "runtime/outputs/review_prior_work/evidence.tsv";
        {
            let graph = metadata["@graph"].as_array_mut().unwrap();
            graph.push(registered_output_file(counts));
            graph.push(registered_output_file(literature));
            graph.push(production_action("quantification", counts, &[]));
            graph.push(production_action("review_prior_work", literature, &[]));
            graph.push(production_action("final_reporting", report, &[counts]));
        }
        let reads = vec![
            crate::provenance::ObservedRead {
                task_id: "final_reporting".into(),
                declared_port: Some("counts".into()),
                path: counts.into(),
            },
            crate::provenance::ObservedRead {
                task_id: "final_reporting".into(),
                declared_port: None,
                path: literature.into(),
            },
        ];
        let mut allowances = std::collections::BTreeMap::new();
        allowances.insert(
            "final_reporting".to_string(),
            vec![crate::atom::ReadAllowance {
                scope: crate::atom::ReadAllowanceScope::AnyUpstreamStage,
                rationale: "dashboard aggregation".into(),
            }],
        );

        reconcile_ro_crate_edges_with_allowances(&mut metadata, &edges, &reads, &allowances);

        let used = used_ids(&metadata, report);
        assert!(used.iter().any(|u| u == literature), "got {used:?}");
        assert_eq!(
            used_status(&metadata, report, literature),
            "ecaax:allowanceCovered"
        );
        assert_eq!(used_status(&metadata, report, counts), "ecaax:observed");
    }

    /// The rebuild is exactly idempotent: a second reconcile over the same
    /// inputs leaves the same `used` list and mints no duplicate provenance
    /// node.
    #[test]
    fn used_rebuild_is_idempotent() {
        let edges = vec![declared_edge(
            "quantification",
            "count_matrix",
            "differential_expression",
            "raw_counts",
        )];
        let mut metadata = graph_with_parameter_connections(&edges);
        let de_out = "runtime/outputs/differential_expression/de_results.tsv";
        let counts = "runtime/outputs/quantification/count_matrix.tsv";
        {
            let graph = metadata["@graph"].as_array_mut().unwrap();
            graph.push(registered_output_file(counts));
            graph.push(production_action("quantification", counts, &[]));
            graph.push(production_action(
                "differential_expression",
                de_out,
                &[counts],
            ));
        }
        let reads = vec![crate::provenance::ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: counts.into(),
        }];

        reconcile_ro_crate_edges(&mut metadata, &edges, &reads);
        let first = metadata.clone();
        reconcile_ro_crate_edges(&mut metadata, &edges, &reads);

        assert_eq!(
            metadata, first,
            "a repeated reconcile must converge on the identical graph"
        );
        let node_id = format!("#object-provenance/differential_expression/{counts}");
        assert_eq!(
            metadata["@graph"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["@id"] == node_id)
                .count(),
            1,
            "exactly one provenance node per (task, used entry)"
        );
    }
}
