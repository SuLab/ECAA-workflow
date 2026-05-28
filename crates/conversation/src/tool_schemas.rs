//! JSON schema emission and user-facing status copy for the closed
//! 15-tool vocabulary. Split out from `tools.rs` so the dispatch layer
//! stays focused on argument validation, session mutation, and audit
//! logging.
//!
//! The `Tool` enum + its `name()` / `is_mutation()` methods + `dispatch_one`
//! and `dispatch_batch` intentionally stay in `tools.rs` — that is the
//! closed-vocabulary invariant. Splitting them would obscure the closedness
//! without buying anything.
//!
//! What lives here:
//! * [`tool_status_line`] — the pre-written copy the UI shows in the
//!   `ToolCallStatusPill` while a tool call is in flight.
//! * [`tool_schemas`] — the static JSON schema list emitted into the
//!   system prompt as the LLM tool vocabulary and used for pre-dispatch
//!   argument validation.

use crate::session::SessionState;
use crate::tools::{BatchableTool, HighImpactTool, Tool};

/// §3.1 — tools that are only meaningful in post-emission states. The
/// `PreconditionFailure` checks inside each tool's dispatch implementation
/// already reject these when called from an earlier state; by filtering
/// them out of the schema block entirely, we also save ~2–2.5 KB of
/// cached prefix on every early-state request.
const POST_EMIT_ONLY_TOOLS: &[&str] = &[
    "amend_stage_method",
    "select_sensitivity_winner",
    "rerun_task",
    "branch_session",
    "start_execution",
];

/// §3.1 — `emit_package` is valid once the session has reached
/// `ReadyToEmit` or later. We keep it in earlier states only when
/// Blocked (recovery flexibility); all other pre-emit states drop it.
const EMIT_ONLY_TOOL: &str = "emit_package";

/// §3.1 — tools that only make sense during pre-emit intake. Once the
/// session has reached `ReadyToEmit` (SME has confirmed the plan),
/// these become noise: the SME isn't going to re-classify, re-set a
/// field they already locked in, or be re-asked for a summary
/// confirmation. Filtering them out at the schema level cuts the
/// `ReadyToEmit`/`Emitting`/`Emitted`/`Amending` tool inventory from
/// 18 → 13. `Blocked` keeps these in scope so a session that blocked
/// during intake can still recover via re-classification.
const INTAKE_ONLY_TOOLS: &[&str] = &[
    "classify_intake",
    "set_intake_field",
    "set_intake_method",
    "append_intake_prose",
    "set_intake_excluded_atoms",
    "set_intake_modality",
    "propose_summary_confirmation",
    // Anthropic's strict tool-schema compiler still hit
    // "Schema is too complex for compilation" when ReadyToEmit kept
    // `list_atoms` (5 properties, two with multi-word descriptions
    // and an enum). The atom-catalog lookup is only meaningful
    // pre-emit (to seed `propose_hypothesized_node` or
    // `amend_stage_method`), so dropping it from ReadyToEmit/Emitting
    // keeps the inventory at 5 — under the compile budget — and is
    // semantically a no-op: post-confirm the LLM should be calling
    // emit_package, not browsing the atom registry.
    "list_atoms",
];

/// Tools that make sense ONLY after `emit_package` has succeeded —
/// reviewing task results, amending stages, branching the session,
/// kicking off execution, or proposing new pipeline nodes/renderers
/// based on observed outputs. ReadyToEmit/Emitting drop these because
/// (a) they're inapplicable until the package exists on disk and
/// (b) loading their schemas into the ReadyToEmit prompt block pushed
/// the cumulative schema size past Anthropic's compilation budget
/// ("Schema is too complex for compilation"), which silently failed
/// Every emit_package follow-up turn (root-cause).
const POST_EMIT_REVIEW_TOOLS: &[&str] = &[
    "amend_stage_method",
    "select_sensitivity_winner",
    "rerun_task",
    "branch_session",
    "start_execution",
    "propose_hypothesized_node",
    "propose_hypothesized_renderer",
    "get_task_result",
    // Literature context is only meaningful when a package exists with
    // literature atom outputs present.
    "get_literature_context",
];

/// Pre-written user-facing status copy for each tool. Used by the
/// `ToolCallStatusPill`.
pub fn tool_status_line(tool: &Tool) -> &'static str {
    match tool {
        Tool::Batchable(BatchableTool::ClassifyIntake { .. }) => {
            "Checking the plan against your description…"
        }
        Tool::Batchable(BatchableTool::GetTaxonomyInfo { .. }) => "Looking up analysis details…",
        Tool::Batchable(BatchableTool::GetSessionState) => "Reviewing the current plan…",
        Tool::Batchable(BatchableTool::GetClassificationEvidence) => {
            "Gathering what I understood so far…"
        }
        Tool::Batchable(BatchableTool::GetTaskResult { .. }) => "Looking up the task result…",
        Tool::Batchable(BatchableTool::GetLiteratureContext { .. }) => {
            "Looking up literature context for that entity…"
        }
        Tool::Batchable(BatchableTool::SetIntakeField { .. }) => {
            "Updating the plan with what you just said…"
        }
        Tool::Batchable(BatchableTool::SetIntakeMethod { .. }) => "Recording your method choice…",
        Tool::Batchable(BatchableTool::AppendIntakeProse { .. }) => {
            "Adding your latest details to the plan…"
        }
        Tool::Batchable(BatchableTool::SetIntakeExcludedAtoms { .. }) => {
            "Pruning excluded pipeline steps from the plan…"
        }
        Tool::Batchable(BatchableTool::SetIntakeModality { .. }) => {
            "Switching the analysis modality to your selection…"
        }
        Tool::HighImpact(HighImpactTool::AmendStageMethod { .. }) => {
            "Amending the method for that stage…"
        }
        Tool::HighImpact(HighImpactTool::SelectSensitivityWinner { .. }) => {
            "Recording the SME's sensitivity choice…"
        }
        Tool::HighImpact(HighImpactTool::RerunTask { .. }) => "Rerunning that task…",
        Tool::HighImpact(HighImpactTool::BranchSession { .. }) => {
            "Forking a branch from this session…"
        }
        Tool::HighImpact(HighImpactTool::EmitPackage { .. }) => "Writing the package to disk…",
        Tool::HighImpact(HighImpactTool::StartExecution { .. }) => "Kicking off execution…",
        Tool::Batchable(BatchableTool::ProposeSummaryConfirmation { .. }) => {
            "Preparing a summary for you to confirm…"
        }
        Tool::Batchable(BatchableTool::ProposeQuickReplies { .. }) => {
            "Drafting quick-reply options…"
        }
        Tool::HighImpact(HighImpactTool::ProposeHypothesizedNode { .. }) => {
            "Proposing a new pipeline step for review…"
        }
        Tool::HighImpact(HighImpactTool::ProposeHypothesizedRenderer { .. }) => {
            "Proposing a custom plot…"
        }
        Tool::Batchable(BatchableTool::ListAtoms { .. }) => "Checking the atom catalog…",
    }
}

/// §3.1 — return only the tool schemas valid in the given session
/// state. Applies a dispatch-mirroring filter so the cacheable prefix
/// stays tight in early-session states (Greeting / Intake /
/// IntakeFollowup) without sacrificing correctness: any tool that's
/// never valid in `state` is dropped here; any state transition that
/// brings tools into/out of scope causes a one-time cache-write at the
/// transition boundary, which is acceptable.
pub fn tool_schemas_for_state(state: &SessionState) -> Vec<serde_json::Value> {
    let all = tool_schemas();

    // `Blocked` recovery: previously this returned the full 18-tool
    // inventory under "recovery flexibility", but that tipped Anthropic's
    // tool-schema-compilation budget past its limit and every recovery
    // turn 500'd with "Schema is too complex for compilation" —
    // turning a transient block into a permanent dead session
    // (root-cause: P07 ATAC-RNA cross-omics intake →
    // emit_package "no DAG built" → Blocked → all 18 tools → 400).
    // Keep the intake-mutation + read-only set so the LLM can still
    // re-classify, append prose, or surface a corrected confirmation;
    // drop the post-emit-review tools whose schemas dominate the
    // compilation cost. Recovery from a post-emission block still has
    // emit_package + amend_stage_method + read-only via the
    // intentional inclusion below.
    if matches!(state, SessionState::Blocked { .. }) {
        // `select_sensitivity_winner` is the SME's recovery affordance
        // for `Blocked { kind: AwaitingSmeSelection }`; the handler's
        // precondition (`tools/sensitivity.rs`) requires exactly that
        // state. Keep it visible to the LLM when the resolved blocker
        // is AwaitingSmeSelection, drop it otherwise. The other three
        // tools (`propose_hypothesized_*`, `branch_session`) genuinely
        // have no role inside Blocked recovery, so they stay filtered.
        let awaiting_sme_selection = matches!(
            state.resolved_blocker(),
            Some((
                scripps_workflow_core::blocker::BlockerKind::AwaitingSmeSelection { .. },
                _,
            ))
        );
        return all
            .into_iter()
            .filter(|s| {
                let name = s["name"].as_str().unwrap_or("");
                if name == "select_sensitivity_winner" {
                    return awaiting_sme_selection;
                }
                // Trim the bulkiest review tools that are not load-bearing
                // for recovery. emit_package and amend_stage_method stay
                // so post-emit blocks can still recover.
                !matches!(
                    name,
                    "propose_hypothesized_node"
                        | "propose_hypothesized_renderer"
                        | "branch_session"
                )
            })
            .collect();
    }

    let early = matches!(
        state,
        SessionState::Greeting
            | SessionState::Intake
            | SessionState::IntakeFollowup
            | SessionState::PendingConfirmation { .. }
    );
    let emit_allowed = matches!(
        state,
        SessionState::ReadyToEmit
            | SessionState::Emitting
            | SessionState::Emitted
            | SessionState::Amending { .. }
    );
    // Root-cause: ReadyToEmit + Emitting must keep a tight
    // tool surface — the LLM's only job is to call emit_package, and
    // loading the 7 post-emit-review tool schemas alongside it tipped
    // the cumulative tool-schema payload past Anthropic's compilation
    // budget. The review tools come back as soon as the package
    // exists (Emitted / Amending).
    let pre_emit_or_emitting = matches!(state, SessionState::ReadyToEmit | SessionState::Emitting);

    all.into_iter()
        .filter(|s| {
            let name = s["name"].as_str().unwrap_or("");
            if early && POST_EMIT_ONLY_TOOLS.contains(&name) {
                return false;
            }
            if !emit_allowed && name == EMIT_ONLY_TOOL {
                return false;
            }
            // §3.1 — drop intake-only tools once the session is at
            // ReadyToEmit or later. SME has confirmed; re-classifying
            // / re-setting intake fields / re-asking for summary
            // confirmation is noise. Keeps the ReadyToEmit tool
            // inventory under Anthropic's schema-compilation budget.
            if emit_allowed && INTAKE_ONLY_TOOLS.contains(&name) {
                return false;
            }
            if pre_emit_or_emitting && POST_EMIT_REVIEW_TOOLS.contains(&name) {
                return false;
            }
            true
        })
        .collect()
}

/// Recursively remove `maxLength` keys from a JSON-schema fragment.
/// Anthropic's strict tool-schema compiler rejects schemas that carry
/// `maxLength` ("Schema is too complex for compilation"); server-side
/// length enforcement on the deserialized tool input is the canonical
/// guard. See [`MAX_FREETEXT_LEN`] for the policy constant.
fn strip_max_length(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("maxLength");
            for (_, v) in map.iter_mut() {
                strip_max_length(v);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                strip_max_length(v);
            }
        }
        _ => {}
    }
}

/// Recursively insert `"additionalProperties": false` on every object-typed
/// sub-schema that doesn't already declare it. Anthropic's tool-schema
/// validation (rolled out 2026-05) rejects `tools[i].custom` when an
/// object-typed schema omits the field. Doing this at emit time keeps the
/// per-tool literals readable and makes the constraint apply uniformly to
/// nested objects too (e.g. a future tool with `properties.foo` typed as
/// `object`).
fn inject_additional_properties_false(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            let is_object_schema = matches!(
                map.get("type"),
                Some(serde_json::Value::String(s)) if s == "object"
            );
            if is_object_schema && !map.contains_key("additionalProperties") {
                map.insert(
                    "additionalProperties".to_string(),
                    serde_json::Value::Bool(false),
                );
            }
            for (_, v) in map.iter_mut() {
                inject_additional_properties_false(v);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                inject_additional_properties_false(v);
            }
        }
        _ => {}
    }
}

/// Grant v19 §Authentication of Key Resources — D4 — the tool-schema
/// version pinned into `runtime/model-policy.json`. Bumped any time a
/// tool's input schema, output shape, or alone-in-turn discipline
/// changes in a way reviewers should notice. Today's value is `1`;
/// CLAUDE.md `Tool::COUNT` already pins the variant count, so this
/// constant only covers shape-of-schema changes that the variant count
/// can't detect.
pub const SCHEMA_VERSION: u32 = 1;

/// Hard cap on free-text tool-argument strings.
/// Anthropic's strict-schema rollout accepts `maxLength` constraints
/// and forwards them to the model, but the more important reason is
/// server-side: bounded inputs cap the audit-log row size, the JSON
/// body the LLM dispatcher hands to a downstream side-call (e.g. the
/// remediation proposer), and the prompt-cache fingerprint cost when
/// a tool result is echoed back into a subsequent turn.
///
/// 16,384 chars ≈ 4,096 tokens at the GPT-style 4-char average, which
/// is large enough that no legitimate SME-typed prose comes close
/// while still bounding the worst-case load.
pub const MAX_FREETEXT_LEN: u32 = 16_384;

/// Static JSON schemas for all 20 tools. Use `tool_schemas_for_state`
/// to get the state-filtered subset used in live conversation; use
/// this full list for validation, introspection, and tests.
pub fn tool_schemas() -> Vec<serde_json::Value> {
    let mut schemas = raw_tool_schemas();
    for s in &mut schemas {
        if let Some(input_schema) = s.get_mut("input_schema") {
            inject_additional_properties_false(input_schema);
            // Anthropic's strict tool-schema compiler (2026-05 rollout)
            // rejects `maxLength` on string properties with a
            // "Schema is too complex for compilation" error. We keep
            // the constants for server-side length enforcement (see
            // `MAX_FREETEXT_LEN`) but strip them from the wire schema
            // before send. Idempotent — running twice is a no-op.
            strip_max_length(input_schema);
        }
    }
    schemas
}

fn raw_tool_schemas() -> Vec<serde_json::Value> {
    use serde_json::json;
    vec![
        json!({
            "name": "classify_intake",
            "description": "Run the deterministic keyword classifier over a prose description of the experiment. Returns modality, confidence, EDAM terms, organisms, and accessions. Read-only — does not save the prose or advance the session. To persist the user's description and build the DAG, call append_intake_prose instead.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "prose": { "type": "string", "maxLength": MAX_FREETEXT_LEN, "description": "Free-text description of the experiment" }
                },
                "required": ["prose"]
            }
        }),
        json!({
            "name": "get_taxonomy_info",
            "description": "Load taxonomy details for a given modality id. Returns stage list, claim boundary, policies, and intake_hints.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "modality_id": { "type": "string" }
                },
                "required": ["modality_id"]
            }
        }),
        json!({
            "name": "list_atoms",
            "description": "Read-only summary of the atom catalog (config/stage-atoms/*.yaml). Call this BEFORE proposing a new atom via propose_hypothesized_node to avoid duplicating existing capability, and before choosing methods for amend_stage_method to confirm the tool exists in candidate_tools. Returns id, role, modality_hint, edam_operation/data/format, depends_on, candidate_tools, and method_choice target. Sub-millisecond — call freely.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "modality": {
                        "type": "string",
                        "description": "Optional. Filter to atoms whose modality_hint matches exactly (e.g. 'bulk_rnaseq', 'long_read_rnaseq', 'single_cell_rnaseq')."
                    },
                    "role": {
                        "type": "string",
                        "enum": ["operation", "discovery", "validation", "aggregator", "sizing", "selection", "calibration", "pilot", "adversarial", "monitor"],
                        "description": "Optional. Filter to atoms with the given role. 'operation' = data-producing step; 'discovery' = runtime method-choice wrapper (discover_*); 'validation' = post-check (validate_*); 'aggregator' = fan-in barrier; 'sizing'/'pilot' = resource-projection atoms; 'selection' = candidate-set picker; 'calibration' = parameter fitter; 'adversarial' = robustness probe; 'monitor' = metric side-channel."
                    },
                    "has_method_choice": {
                        "type": "boolean",
                        "description": "Optional. true = only atoms whose tool choice is deferred to a discovery wrapper (look at the returned method_choice_deferred_to field for the wrapper id). false = atoms with concrete or unspecified method."
                    },
                    "produces_edam_data": {
                        "type": "string",
                        "description": "Optional. Filter to atoms whose primary output edam_data IRI matches. e.g. 'data:0951' for effect-size + p-value tables."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Optional cap on returned rows. Default 100; hard-capped at 500 (server-enforced; values outside [1, 500] are clamped). The full catalog is ~73 atoms today."
                    }
                }
            }
        }),
        json!({
            "name": "get_session_state",
            "description": "Returns current intake methods, DAG snapshot, unresolved discovery tasks, and readiness summary.",
            "input_schema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "get_classification_evidence",
            "description": "Returns the specific keyword hits, organism matches, and accession matches that drove the latest classification. Use before summarizing back to the SME.",
            "input_schema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "get_task_result",
            "description": "Fetch the structured result for a completed task — method, status, key outputs, metrics, completion time. Call this when the SME asks to see what a task produced.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" }
                },
                "required": ["task_id"]
            }
        }),
        json!({
            "name": "get_literature_context",
            "description": "Retrieve PMID-anchored literature rows for a named entity from the session's emitted package. Read-only; no live PubMed call. Returns prior_rows (from review_prior_work) and finding_rows (from contextualize_findings_with_literature) matching the entity. Requires an emitted package with literature atoms.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "entity": {
                        "type": "string",
                        "description": "Entity name (gene symbol, pathway name, variant rsID, cell type name, etc.)"
                    },
                    "entity_kind": {
                        "type": "string",
                        "enum": ["gene", "region", "variant", "pathway", "cell_type"],
                        "description": "Optional filter by entity kind. Omit to match all kinds."
                    }
                },
                "required": ["entity"]
            }
        }),
        json!({
            "name": "set_intake_field",
            "description": "Set a structured field on a stage's intake resolution. Rebuilds the DAG and returns a state delta. `value` accepts a string (e.g. organism, accession id), boolean, number, or array of strings (e.g. excluded_methods).",
            "input_schema": {
                "type": "object",
                "properties": {
                    "stage": { "type": "string" },
                    "field": { "type": "string" },
                    "value": {
                        "description": "Field value. Pass primitives directly (string/number/boolean) or an array of strings (e.g. excluded_methods). For richer structures (e.g. an array of per-study metadata objects), JSON-encode the structure as a string — the dispatcher will parse it back.",
                        "type": "string"
                    }
                },
                "required": ["stage", "field", "value"]
            }
        }),
        json!({
            "name": "set_intake_method",
            "description": "Record an SME-named method for a given stage. Use only when the SME explicitly named a method on their own initiative — never as a methodology recommendation from the LLM.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "stage": { "type": "string" },
                    "method_prose": { "type": "string", "maxLength": MAX_FREETEXT_LEN }
                },
                "required": ["stage", "method_prose"]
            }
        }),
        json!({
            "name": "append_intake_prose",
            "description": "Append new SME-provided prose to the running intake context and re-classify. Returns whether the modality changed.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "prose": { "type": "string", "maxLength": MAX_FREETEXT_LEN }
                },
                "required": ["prose"]
            }
        }),
        json!({
            "name": "set_intake_excluded_atoms",
            "description": "Prune upstream pipeline atoms the SME has explicitly opted out of (sub-archetype small-task). Call when (a) input is past a pipeline stage: count matrix → skip ['sequence_trimming','alignment','quantification']; BAM/CRAM → skip ['sequence_trimming','alignment']; processed Seurat/AnnData → also skip ['qc_preprocessing','normalisation']; (b) SME says 'skip X' for a specific atom. Never include protected atoms: raw_qc, reporting, final_reporting, generic_summary. Discover/validate companions of excluded atoms are auto-pruned. Downstream atoms outside this set that lose all upstreams are REWIRED to data_acquisition (not dropped) — so cell-level QC steps still run when the user has Cell Ranger output, etc. Use list_atoms first to confirm atom ids.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "atom_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Atom ids to exclude (e.g. ['sequence_trimming','alignment','quantification'])."
                    }
                },
                "required": ["atom_ids"]
            }
        }),
        json!({
            "name": "set_intake_modality",
            "description": "Override the classifier's modality choice. Call ONLY after the SME has explicitly confirmed a modality the classifier under-ranked — typically when classify_intake returned tie_candidates and the SME picked one via propose_quick_replies, or when the SME corrects a misclassification in plain prose. Rewrites session.classification.modality, clears the archetype snapshot, and rebuilds the DAG so the composer reseeds against the SME-confirmed modality. Refuses unknown modality ids and refuses when no classification exists yet (call append_intake_prose first).",
            "input_schema": {
                "type": "object",
                "properties": {
                    "modality_id": {
                        "type": "string",
                        "description": "Modality id from config/modalities/*.yaml (e.g. 'spatial_transcriptomics', 'bulk_rnaseq', 'single_cell_rnaseq'). Must match a registered manifest id exactly."
                    }
                },
                "required": ["modality_id"]
            }
        }),
        json!({
            "name": "amend_stage_method",
            "description": "Post-emission, swap the recorded method for a stage. Only valid in Emitted. Invalidates the downstream DAG slice and routes back to ReadyToEmit for a fresh emit. Alone-in-turn. Pass `rationale` for prespecified stages — logged as PostHocDeviation; required when the stage was prespecified.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "stage": { "type": "string" },
                    "method_prose": { "type": "string", "maxLength": MAX_FREETEXT_LEN },
                    "rationale": { "type": "string", "maxLength": MAX_FREETEXT_LEN }
                },
                "required": ["stage", "method_prose"]
            }
        }),
        json!({
            "name": "select_sensitivity_winner",
            "description": "Record the SME's choice of winning variant for a sensitivity_comparison stage. Only valid when the session is Blocked with kind AwaitingSmeSelection. Records the winner in intake_methods, invalidates the downstream DAG slice, and routes the session back so the LLM can propose a fresh summary confirmation. Alone-in-turn.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "stage": { "type": "string" },
                    "winner": { "type": "string" },
                    "rationale": { "type": "string", "maxLength": MAX_FREETEXT_LEN }
                },
                "required": ["stage", "winner"]
            }
        }),
        json!({
            "name": "rerun_task",
            "description": "Rerun a completed task with the same method. Thin wrapper on amend_stage_method — invalidates the downstream slice and routes back to ReadyToEmit. Use when the SME wants a fresh result from the same method (e.g., inputs drifted) rather than a method swap. Alone-in-turn.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" },
                    "reason": { "type": "string", "maxLength": MAX_FREETEXT_LEN }
                },
                "required": ["task_id"]
            }
        }),
        json!({
            "name": "branch_session",
            "description": "Fork the current session into a new branched session that inherits intake state but has its own audit log + emit history. Use when the SME wants to explore an alternative analysis without losing the current one. Alone-in-turn.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "rationale": { "type": "string", "maxLength": MAX_FREETEXT_LEN }
                }
            }
        }),
        // C-10 / `output_dir` is intentionally
        // schema-less. The LLM cannot influence the emit location; the
        // server resolves the path under `$SWFC_PACKAGE_ROOT` or
        // `~/.scripps-workflow/packages` via `default_package_root`.
        // `ignore_llm_output_dir` (in tools/mod.rs) silently drops any
        // value the model sneaks in despite the schema.
        json!({
            "name": "emit_package",
            "description": "Emit the final package. The server auto-assigns a writable directory under the central package root — call this with NO arguments. Preconditions: a taxonomy is loaded, a DAG is built, and user_confirmed is true. Must be the only tool call in its turn.",
            "input_schema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }),
        json!({
            "name": "start_execution",
            "description": "Kick off workflow execution against the emitted package. Only valid in Emitted state. Alone-in-turn. Call when the SME explicitly confirms they want execution to start ('yes, go', 'start it now'). The harness runs detached and streams progress through the Jobs tab; the SME can unblock/amend mid-run via the same chat surface. The agent script is server-controlled; the LLM cannot choose it.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "max_iterations": {
                        "type": "integer",
                        "description": "Cap on harness iterations. Omit to use the server default (20)."
                    }
                },
                "required": []
            }
        }),
        json!({
            "name": "propose_summary_confirmation",
            "description": "Render a confirmation card to the user with a plain-language summary. Advances session state to PendingConfirmation.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "summary_markdown": { "type": "string", "maxLength": MAX_FREETEXT_LEN }
                },
                "required": ["summary_markdown"]
            }
        }),
        json!({
            "name": "propose_quick_replies",
            "description": "Offer the user a small set of quick-reply chips for a clarification question.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "question": { "type": "string", "maxLength": MAX_FREETEXT_LEN },
                    "options": {
                        "type": "array",
                        "items": { "type": "string", "maxLength": MAX_FREETEXT_LEN }
                    }
                },
                "required": ["question", "options"]
            }
        }),
        json!({
            "name": "propose_hypothesized_node",
            "description": "Propose a new pipeline node when the SME asks for a capability the registry doesn't satisfy. Alone-in-turn. The node is recorded as Hypothesized/Unverified and cannot execute until validators + sandbox + promotion authority approve. Only use when no existing atom or archetype fits.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "proposed_id": {
                        "type": "string",
                        "description": "snake_case identifier for the new capability (e.g. `doublet_score`)."
                    },
                    "intent": {
                        "type": "string",
                        "maxLength": MAX_FREETEXT_LEN,
                        "description": "One sentence describing what the proposed node does."
                    },
                    "parent_terms": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "EDAM / swfc parent terms (e.g. data:2603) so the compatibility engine can subsume."
                    },
                    "llm_rationale": {
                        "type": "string",
                        "maxLength": MAX_FREETEXT_LEN,
                        "description": "Free-text summary of the SME's justification from prior turns."
                    },
                    "assumptions": {
                        "type": "array",
                        "items": { "type": "string", "maxLength": MAX_FREETEXT_LEN },
                        "description": "Declared assumptions the SME confirms before promotion."
                    },
                    "failure_modes": {
                        "type": "array",
                        "items": { "type": "string", "maxLength": MAX_FREETEXT_LEN },
                        "description": "Failure modes the validators must cover."
                    },
                    "validation_tests": {
                        "type": "array",
                        "items": { "type": "string", "maxLength": MAX_FREETEXT_LEN },
                        "description": "Minimal validation test ids (e.g. `p_value_in_unit_interval`)."
                    },
                    "upstream_atom_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Atom-ids the proposed node depends on (e.g. ['normalisation'] or ['differential_expression']). Becomes the new node's depends_on on promotion. Without this the promoted node would be an orphan with no input data. Use list_atoms first to confirm the upstream atom ids exist."
                    }
                },
                "required": ["proposed_id", "intent", "parent_terms", "llm_rationale"]
            }
        }),
        json!({
            "name": "propose_hypothesized_renderer",
            "description": "Propose a preferred renderer when a figure resolved via structural fallback (badge shows 'Generic'). Alone-in-turn. Recorded as Hypothesized; will not replace the fallback until promotion evidence accumulates. Call once when the SME describes a preferred plot; do not invent parent terms the registry doesn't expose.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "target_semantic_type": {
                        "type": "string",
                        "description": "SemanticType IRI of the output port the preferred renderer addresses (e.g. `swfc:my_custom_output`). The `EDAM:` namespace is reserved and will be rejected."
                    },
                    "proposed_parent_terms": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Registered parent-term SemanticType IRIs the proposed renderer inherits from. Use only terms surfaced in the affordance proof for this figure or terms the SME explicitly named that are registered in the catalog."
                    },
                    "proposed_figure_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Figure ids the preferred renderer would produce (e.g. `volcano`, `ridge_plot`). None may shadow an existing registered figure id."
                    },
                    "sme_intent": {
                        "type": "string",
                        "maxLength": MAX_FREETEXT_LEN,
                        "description": "LLM-summarized description of the SME's preferred renderer from prior turns. ≤ 800 chars."
                    },
                    "primitive_basis": {
                        "type": "string",
                        "description": "The structural primitive id the SME is upgrading from, if known (e.g. `__structural_matrix_overview`). Omit when the fallback primitive is unknown."
                    }
                },
                "required": ["target_semantic_type", "proposed_parent_terms", "proposed_figure_ids", "sme_intent"]
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_status_lines_exhaust_variants() {
        // S16.4 — single source of truth for the variant inventory lives
        // in `Tool::all_variants_for_tests`; this test confirms every
        // variant has a non-empty user-facing status line.
        for v in Tool::all_variants_for_tests() {
            let line = tool_status_line(&v);
            assert!(
                !line.is_empty(),
                "tool_status_line returned empty string for variant {}",
                v.name(),
            );
        }
    }

    #[test]
    fn tool_schemas_count_is_twenty() {
        // The literature-atom plan added get_literature_context to
        // the closed Tool vocabulary, bringing the total from 18 to 19.
        // Test-suite-remediation plan added `list_atoms`
        // (read-only catalog inspection) bringing it to 20. RCA F8
        // added `set_intake_modality` (SME-confirmed disambiguation)
        // bringing it to 22. Any further additions require a plan
        // amendment per CLAUDE.md.
        assert_eq!(tool_schemas().len(), 22);
    }

    /// Runtime baseline for `Tool::COUNT` (strum `EnumCount`). The
    /// compile-time `strum::EnumCount` guarantee already prevents the
    /// enum from silently gaining variants without a recompile, but a
    /// runtime assertion here makes the drift visible in CI test output
    /// alongside the CLAUDE.md prose count and the `tool_schemas` length
    /// check above. All three should be updated together when a tool is
    /// added. If this fires, update CLAUDE.md prose + this baseline +
    /// `tool_schemas_count_is_twenty` in the same commit.
    #[test]
    fn tool_count_baseline_is_twenty() {
        assert_eq!(
            Tool::COUNT,
            22,
            "Tool::COUNT drifted; update CLAUDE.md prose + this baseline together"
        );
    }

    /// Names_match_enum: the JSON-schema name set and the
    /// `Tool` enum's `name()` set must be exactly equal. This catches a
    /// drift mode the audit flagged: the two surfaces are written by
    /// hand from the same closed vocabulary, and a partial PR can rename
    /// one side without the other. Cross-checking here turns that into a
    /// CI-blocking signal.
    #[test]
    fn names_match_enum() {
        use std::collections::BTreeSet;
        let schema_names: BTreeSet<String> = tool_schemas()
            .iter()
            .map(|s| {
                s["name"]
                    .as_str()
                    .expect("schema name is a string")
                    .to_string()
            })
            .collect();
        let enum_names: BTreeSet<String> = Tool::all_variants_for_tests()
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        assert_eq!(
            schema_names,
            enum_names,
            "tool_schemas() names and Tool::name() set must match exactly. \
             Missing from enum: {:?}; missing from schemas: {:?}",
            schema_names.difference(&enum_names).collect::<Vec<_>>(),
            enum_names.difference(&schema_names).collect::<Vec<_>>(),
        );
    }

    /// `Tool::all_variants_for_tests()` is the source of
    /// truth for "what variants exist." Cross-checks against
    /// `tool_schemas()` (which is also the LLM-visible vocabulary) so
    /// a missed sample variant becomes a CI failure rather than silent
    /// gap.
    #[test]
    fn all_variants_for_tests_covers_every_schema() {
        assert_eq!(
            Tool::all_variants_for_tests().len(),
            tool_schemas().len(),
            "Tool::all_variants_for_tests() must enumerate every variant \
             in tool_schemas(). Missing one is a compile error in the \
             match in Tool::all_variants_for_tests; missing one here is \
             this assertion."
        );
    }

    #[test]
    fn early_state_hides_post_emit_tools() {
        // §3.1 — Greeting / Intake / IntakeFollowup sessions should
        // not see amend / rerun / sensitivity / branch / start_execution
        // nor emit_package. Regression guard for the progressive-
        // disclosure filter.
        let early_states = [
            SessionState::Greeting,
            SessionState::Intake,
            SessionState::IntakeFollowup,
        ];
        for state in &early_states {
            let schemas = tool_schemas_for_state(state);
            let names: Vec<&str> = schemas
                .iter()
                .map(|s| s["name"].as_str().unwrap())
                .collect();
            for name in POST_EMIT_ONLY_TOOLS {
                assert!(
                    !names.contains(name),
                    "state {:?} must not carry tool {} in its schema block",
                    state,
                    name
                );
            }
            assert!(
                !names.contains(&EMIT_ONLY_TOOL),
                "state {:?} must not carry {} in its schema block",
                state,
                EMIT_ONLY_TOOL
            );
            // Read-only + intake-mutation tools must still be present.
            assert!(names.contains(&"classify_intake"));
            assert!(names.contains(&"append_intake_prose"));
            assert!(names.contains(&"propose_summary_confirmation"));
        }
    }

    #[test]
    fn emit_only_available_from_ready_state_onwards() {
        // emit_package is valid starting from ReadyToEmit. Emitted,
        // Amending also include it (e.g. a failed emit can retry).
        for state in [
            SessionState::ReadyToEmit,
            SessionState::Emitted,
            SessionState::Amending {
                target_stage: "x".into(),
                invalidated_tasks: vec![],
            },
        ] {
            let schemas = tool_schemas_for_state(&state);
            let names: Vec<&str> = schemas
                .iter()
                .map(|s| s["name"].as_str().unwrap())
                .collect();
            assert!(
                names.contains(&EMIT_ONLY_TOOL),
                "state {:?} must carry emit_package",
                state
            );
        }
    }

    #[test]
    fn ready_to_emit_drops_intake_only_tools() {
        // §3.1 — once the SME has confirmed and the session is in
        // ReadyToEmit / Emitting / Emitted / Amending, intake-only
        // tools (classify_intake, set_intake_field, set_intake_method,
        // append_intake_prose, propose_summary_confirmation) are
        // dropped. ReadyToEmit/Emitting additionally drop the
        // post-emit-review tools (amend, rerun, branch, propose_*,
        // start_execution, get_task_result) so the LLM's payload
        // stays under Anthropic's schema-compilation budget; Emitted
        // and Amending get them back.
        let post_intake_states = [
            SessionState::ReadyToEmit,
            SessionState::Emitting,
            SessionState::Emitted,
            SessionState::Amending {
                target_stage: "x".into(),
                invalidated_tasks: vec![],
            },
        ];
        for state in &post_intake_states {
            let schemas = tool_schemas_for_state(state);
            let names: Vec<&str> = schemas
                .iter()
                .map(|s| s["name"].as_str().unwrap())
                .collect();
            for name in INTAKE_ONLY_TOOLS {
                assert!(
                    !names.contains(name),
                    "state {state:?} must not carry intake-only tool {name}"
                );
            }
            assert!(names.contains(&"emit_package"));
            assert!(names.contains(&"get_session_state"));
            // Post-emit-review tools land only after emit_package has run.
            let post_review =
                matches!(state, SessionState::Emitted | SessionState::Amending { .. });
            if post_review {
                assert!(names.contains(&"amend_stage_method"));
            } else {
                assert!(
                    !names.contains(&"amend_stage_method"),
                    "state {state:?} must drop amend_stage_method until package exists"
                );
            }
        }
    }

    #[test]
    fn ready_to_emit_inventory_size() {
        // ReadyToEmit / Emitting expose the tools needed to call
        // emit_package + the read-only fleet (`get_taxonomy_info`,
        // `get_session_state`, `get_classification_evidence`)
        // + `propose_quick_replies` — 5 total. Loading the post-emit-review
        // schemas alongside pushed the cumulative tool payload over
        // Anthropic's compilation budget, silently failing every emit
        // Follow-up turn (root-caused). The post-fix
        // remediation: (1) `strip_max_length` drops the `maxLength`
        // string constraints that the strict compiler rejects, and
        // (2) `list_atoms` moves into `INTAKE_ONLY_TOOLS` so its 5-property
        // schema stops billing the ReadyToEmit budget — the atom-catalog
        // lookup is only meaningful pre-emit.
        let schemas = tool_schemas_for_state(&SessionState::ReadyToEmit);
        assert_eq!(
            schemas.len(),
            5,
            "ReadyToEmit must expose exactly 5 tools (emit_package + read-only fleet + propose_quick_replies); \
             changes require re-checking the schema-too-complex risk"
        );
    }

    /// Regression for Anthropic's 2026-05 strict-schema rollout: empty
    /// schemas (`{}`) that accept any JSON value are rejected with HTTP
    /// 400 ("Empty schema ({}) that accepts any JSON value is not
    /// supported. Please specify a concrete type."). Walks every property
    /// schema and panics if it sees an empty object.
    ///
    /// Allowed exception: `properties: {}` and `required: []` are valid
    /// container shapes (object with no properties), not "any-value"
    /// schemas — the walker only fires on schemas that *would be* the
    /// schema for a value.
    #[test]
    fn no_empty_value_schemas() {
        fn check(value: &serde_json::Value, path: &str) {
            if let serde_json::Value::Object(map) = value {
                if let Some(serde_json::Value::Object(props)) = map.get("properties") {
                    for (k, v) in props {
                        if let serde_json::Value::Object(prop_map) = v {
                            assert!(
                                !prop_map.is_empty(),
                                "empty schema at {path}.properties.{k} — \
                                 specify a concrete type (string/number/boolean/array/object/oneOf)."
                            );
                        }
                        check(v, &format!("{path}.properties.{k}"));
                    }
                }
                // Recurse through other schema-bearing keys.
                for key in [
                    "items", "oneOf", "anyOf", "allOf", "not", "if", "then", "else",
                ] {
                    if let Some(v) = map.get(key) {
                        check(v, &format!("{path}.{key}"));
                    }
                }
            } else if let serde_json::Value::Array(arr) = value {
                for (i, v) in arr.iter().enumerate() {
                    check(v, &format!("{path}[{i}]"));
                }
            }
        }
        for s in tool_schemas() {
            let name = s["name"].as_str().unwrap_or("<anon>");
            check(&s["input_schema"], &format!("{name}.input_schema"));
        }
    }

    /// Regression for Anthropic's 2026-05 strict-schema rollout: every
    /// object-typed input_schema (and any nested object schemas) must
    /// declare `additionalProperties: false` or the API rejects the
    /// request with HTTP 400.
    #[test]
    fn every_object_schema_has_additional_properties_false() {
        fn check(value: &serde_json::Value, path: &str) {
            match value {
                serde_json::Value::Object(map) => {
                    if matches!(map.get("type"), Some(serde_json::Value::String(s)) if s == "object")
                    {
                        match map.get("additionalProperties") {
                            Some(serde_json::Value::Bool(false)) => {}
                            other => panic!(
                                "object schema at {path} must have \
                                 additionalProperties: false (got {:?})",
                                other
                            ),
                        }
                    }
                    for (k, v) in map.iter() {
                        check(v, &format!("{path}.{k}"));
                    }
                }
                serde_json::Value::Array(arr) => {
                    for (i, v) in arr.iter().enumerate() {
                        check(v, &format!("{path}[{i}]"));
                    }
                }
                _ => {}
            }
        }
        for s in tool_schemas() {
            let name = s["name"].as_str().unwrap_or("<anon>");
            check(&s["input_schema"], &format!("{name}.input_schema"));
        }
    }

    /// Every free-text tool argument carries a
    /// `maxLength` constraint matching `MAX_FREETEXT_LEN` at the
    /// raw-schema layer. The public `tool_schemas()` helper strips
    /// these before send (Anthropic's strict compiler rejects them
    /// with "Schema is too complex"), but the raw constant is kept
    /// for server-side enforcement on the deserialized tool input.
    /// The inventory below is the canonical "what counts as free-text"
    /// list — identifier-like fields (`task_id`, `stage`, `winner`,
    /// `proposed_id`, EDAM IRIs, etc.) are deliberately excluded
    /// because they're bounded by their own format requirements.
    ///
    /// If you add a new free-text field, add it here too. The test
    /// then fails until you also add the raw-schema `maxLength`.
    #[test]
    fn free_text_fields_carry_max_length() {
        let raw = raw_tool_schemas();
        let by_name: std::collections::HashMap<&str, &serde_json::Value> = raw
            .iter()
            .map(|s| (s["name"].as_str().unwrap(), s))
            .collect();

        // (tool_name, dotted-property-path-from-input_schema.properties)
        let free_text_fields: &[(&str, &str)] = &[
            ("classify_intake", "prose"),
            ("append_intake_prose", "prose"),
            ("set_intake_method", "method_prose"),
            ("amend_stage_method", "method_prose"),
            ("amend_stage_method", "rationale"),
            ("select_sensitivity_winner", "rationale"),
            ("rerun_task", "reason"),
            ("branch_session", "rationale"),
            ("propose_summary_confirmation", "summary_markdown"),
            ("propose_quick_replies", "question"),
            ("propose_hypothesized_node", "intent"),
            ("propose_hypothesized_node", "llm_rationale"),
            ("propose_hypothesized_renderer", "sme_intent"),
        ];
        for (tool, field) in free_text_fields {
            let tool_schema = by_name
                .get(tool)
                .unwrap_or_else(|| panic!("tool {tool} missing"));
            let cap = tool_schema["input_schema"]["properties"][field]["maxLength"]
                .as_u64()
                .unwrap_or_else(|| {
                    panic!("tool {tool} field {field} missing raw-schema maxLength constraint")
                });
            assert_eq!(
                cap, MAX_FREETEXT_LEN as u64,
                "tool {tool} field {field} maxLength must equal MAX_FREETEXT_LEN ({MAX_FREETEXT_LEN})",
            );
        }
    }

    /// The on-the-wire `tool_schemas()` output must NOT contain
    /// `maxLength` anywhere — Anthropic's strict tool-schema compiler
    /// rejects schemas that carry it. `strip_max_length` is the guard.
    #[test]
    fn tool_schemas_strip_max_length_for_wire() {
        fn assert_no_max_length(v: &serde_json::Value, path: &str) {
            match v {
                serde_json::Value::Object(map) => {
                    assert!(
                        !map.contains_key("maxLength"),
                        "tool_schemas() leaked maxLength at {path}",
                    );
                    for (k, child) in map.iter() {
                        assert_no_max_length(child, &format!("{path}.{k}"));
                    }
                }
                serde_json::Value::Array(arr) => {
                    for (i, child) in arr.iter().enumerate() {
                        assert_no_max_length(child, &format!("{path}[{i}]"));
                    }
                }
                _ => {}
            }
        }
        for s in tool_schemas() {
            let name = s["name"].as_str().unwrap_or("<anon>");
            assert_no_max_length(&s, name);
        }
    }

    #[test]
    fn blocked_keeps_recovery_tools() {
        // Recovery from a blocked state keeps most tools visible.
        // Four bulky review tools are dropped to stay under Anthropic's
        // Schema-compilation budget (root-caused): those are
        // propose_hypothesized_node, propose_hypothesized_renderer,
        // branch_session, and select_sensitivity_winner.
        let blocked = SessionState::Blocked {
            blockers: vec![],
            reason: "x".into(),
            recovery_hint: "y".into(),
            blocker_kind: None,
            context: None,
        };
        const BLOCKED_DROPPED: usize = 4; // see filter in tool_schemas_for_state
        let schemas = tool_schemas_for_state(&blocked);
        assert_eq!(schemas.len(), tool_schemas().len() - BLOCKED_DROPPED);
        // Verify the four dropped tools are absent.
        let names: Vec<&str> = schemas
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        for dropped in &[
            "propose_hypothesized_node",
            "propose_hypothesized_renderer",
            "branch_session",
            "select_sensitivity_winner",
        ] {
            assert!(
                !names.contains(dropped),
                "blocked state must not carry {dropped} (exceeds Anthropic schema-compilation budget)"
            );
        }
        // Load-bearing recovery tools must still be present.
        assert!(names.contains(&"emit_package"));
        assert!(names.contains(&"amend_stage_method"));
        assert!(names.contains(&"get_session_state"));
    }

    #[test]
    fn excluded_atom_guidance_never_suggests_protected_raw_qc() {
        let prompt = include_str!("prompt_role.txt");
        for forbidden in [
            "count matrix supplied → `[\"raw_qc\"",
            "BAM/CRAM supplied → `[\"raw_qc\"",
        ] {
            assert!(
                !prompt.contains(forbidden),
                "prompt guidance must not recommend excluding protected raw_qc: {forbidden}"
            );
        }

        let schema = raw_tool_schemas()
            .into_iter()
            .find(|schema| schema["name"] == "set_intake_excluded_atoms")
            .expect("set_intake_excluded_atoms schema missing");
        let description = schema["description"].as_str().unwrap_or_default();
        assert!(
            !description.contains("skip ['raw_qc'"),
            "set_intake_excluded_atoms tool description must not recommend excluding protected raw_qc"
        );

        let atom_ids_description = schema["input_schema"]["properties"]["atom_ids"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(
            !atom_ids_description.contains("['raw_qc'"),
            "atom_ids input description must not use protected raw_qc in exclusion examples"
        );
    }
}
