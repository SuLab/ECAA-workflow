//! Forward / backward / meet-in-the-middle planner over the typed
//! `WorkflowDag` IR.
//!
//! `plan()` drives `super::forward_search` + `super::backward_search` +
//! `super::meet_in_middle` directly and treats the archetype match as
//! one of two seeds, not a hard fast-path. What ships:
//!
//! 1. **Archetype seed** — `archetype_reg.find_match(...)` produces a
//!    candidate when a project archetype matches the goal triple
//!    (`edam_data` + `edam_format` + `project_class`, plus optional
//!    modality/kind disambiguators). Lifted into a typed `WorkflowDag`
//!    via `lift_to_workflow_dag` and tagged `source = "archetype"`.
//! 2. **Search seed** — forward reachability from
//!    `intent.available_data`, backward decomposition from
//!    `intent.desired_outputs`, met in the middle via the
//!    compatibility engine's proof. Tagged `source = "search"`.
//! 3. **Score + rank** per the design's 16-component tuple — production
//!    trust counts, unresolved assumptions, risky/total adapter counts
//!    (now via typed `AdapterRegistry` lookup, not id-prefix string
//!    matching), reproducibility/explainability, etc.
//! 4. **Top-K ranked alternatives** when `max_alternatives > 0`. Both
//!    seeds are returned as separate alternatives so the SME can
//!    compare archetype-driven vs search-driven candidates.
//! 5. **WorkflowDag → CompositionResult lowering** for the dispatch
//!    path (`compose_v4_dispatch_full`) so v4 produces an executable
//!    composition.

use std::collections::{BTreeMap, BTreeSet};

use crate::archetype_registry::ArchetypeRegistry;
use crate::assumption_policy::{
    AssumptionPolicyTable, DefectClass as PolicyDefectClass, PolicyPrivacyClass,
};
use crate::atom::AtomDefinition;
use crate::atom_registry::AtomRegistry;
use crate::compatibility::engine::{
    CompatibilityEngine, CompatibilityResult, DeterministicCompatibilityEngine,
    PlanningContext as CompatibilityContext,
};
use crate::policy_context::PolicyContext;
use crate::sandbox_policy::{check_generated_code_node, SandboxPolicy};
use crate::sandbox_refusal_category::SandboxRefusalCategory;
use crate::workflow_contracts::evidence::{Assumption, AssumptionResolution, AssumptionSource};
use crate::workflow_contracts::outcome::BlockerContext;
use crate::workflow_contracts::port::PortPrivacyClass;
use crate::workflow_contracts::refusal_kind::RefusalKind;
use crate::workflow_contracts::unblock_path::{ProjectedOutcome, UnblockPath};

use super::policy_gate;
use crate::composer::{
    aggregate_resources, resolve_task_container, AtomSelectionRationale, ComposedAtom,
    CompositionError, CompositionResult, SelectionReason,
};
use crate::goal_spec::GoalSpec;
use crate::ids::StageId;
use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
use crate::workflow_contracts::evidence::AssumptionLedger;
use crate::workflow_contracts::lifecycle::LifecycleState;
use crate::workflow_contracts::outcome::{ComposeOutcome, GapReport, ValidationReport};
use crate::workflow_contracts::port::PortContract;
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

use super::scoring::{ScoringTuple, ScoringValue};
use super::{PlannerResult, PlanningContext, RankedAlternative};

/// Build a planning context from the existing v3 inputs. Used by
/// `compose_v4_dispatch_full` to bridge `(GoalSpec, AtomRegistry)` → v4.
///
/// Also lifts `goal.edam_data` / `goal.edam_format`
/// into a single `DesiredOutput` entry on the intent so the new
/// forward/backward/meet-in-the-middle search has a goal to walk
/// backward from. Without this, `backward_search` returns an empty
/// requirement set and the planner would fall back to the
/// archetype-only seed.
///
/// Thin wrapper over
/// [`planning_context_for_goal_with_intake`] that omits
/// modality / project_class / available_data. The dispatch entry
/// point uses the richer helper so the v4 forward search has a
/// frontier to walk from; this wrapper stays for back-compat with
/// historical callers and the placeholder `compose()`.
pub fn planning_context_for_goal(intent_id: impl Into<String>, goal: &GoalSpec) -> PlanningContext {
    planning_context_for_goal_with_intake(intent_id, goal, None, None, &[])
}

/// Build a planning context that threads the
/// dispatcher's `modality`, `project_class`, and intake-derived
/// `available_data` into the resulting [`WorkflowIntent`].
///
/// The v4 forward / backward / meet-in-the-middle planner reads:
///
/// - `intent.desired_outputs` — drives `backward_search`. Lifted from
///   `goal.edam_data` + `goal.edam_format` (one [`DesiredOutput`] per
///   goal triple).
/// - `intent.available_data` — drives `forward_search`. Caller-supplied
///   typed contracts; today the dispatcher seeds best-effort fixtures
///   based on modality so the search has a frontier to walk.
/// - `intent.modality` — disambiguates archetype matches when the
///   archetype catalog declares modality hints.
/// - `intent.project_class` — surfaces in audit logs and feeds the
///   archetype matcher's project-class disambiguator.
///
/// Without these fields populated, the v4 planner returns
/// `ComposeOutcome::PartialDag` with "no producer atom in registry
/// produces <goal>" because both search modules walk an empty intent.
pub fn planning_context_for_goal_with_intake(
    intent_id: impl Into<String>,
    goal: &GoalSpec,
    target_modality: Option<&str>,
    project_class: Option<&str>,
    available_data: &[crate::workflow_contracts::data_product::DataProductContract],
) -> PlanningContext {
    planning_context_for_goal_with_modalities(
        intent_id,
        goal,
        target_modality,
        &[],
        project_class,
        available_data,
    )
}

/// Sibling of [`planning_context_for_goal_with_intake`]
/// that accepts a slice of *additional* modalities (those beyond the
/// primary `target_modality`). Cross-omics scenarios route through this
/// helper so the planner can attempt a cross-omics archetype match
/// (set-equality on `cross_omics_modalities`) before falling through to
/// single-modality matching.
///
/// The full modality set the SME requested is
/// `[target_modality]` ++ `additional_modalities`; the planner uses the
/// concatenation when calling
/// `archetype_reg.find_match_cross_omics(...)`.
///
/// Pre-Task-I the v4 planner only saw the primary modality, so
/// cross-omics archetypes (filtered out by
/// `find_match_with_evidence_modality_kind`) were never seeded and v4
/// emitted a single bare-name pipeline instead of v2's
/// dual-namespaced parallel-pipeline shape (see
/// `tests/conversation-fixtures/fixtures/v4-parity/cross-omics/v4-emission/TRIAGE.md`).
pub fn planning_context_for_goal_with_modalities(
    intent_id: impl Into<String>,
    goal: &GoalSpec,
    target_modality: Option<&str>,
    additional_modalities: &[&str],
    project_class: Option<&str>,
    available_data: &[crate::workflow_contracts::data_product::DataProductContract],
) -> PlanningContext {
    let goal_label = match &goal.edam_format {
        Some(f) => format!("{} ({})", goal.edam_data, f),
        None => goal.edam_data.clone(),
    };
    let desired_label = goal
        .source_prose
        .clone()
        .unwrap_or_else(|| goal_label.clone());
    let desired_outputs = if !goal.edam_data.is_empty() {
        vec![crate::workflow_contracts::workflow_intent::DesiredOutput {
            label: desired_label,
            edam_data: Some(goal.edam_data.clone()),
            edam_format: goal.edam_format.clone(),
            human_readable: false,
        }]
    } else {
        Vec::new()
    };
    let intent = crate::workflow_contracts::workflow_intent::WorkflowIntent {
        id: intent_id.into(),
        schema_version: crate::migration::current_workflow_intent_version(),
        goal: goal.source_prose.clone().unwrap_or(goal_label),
        modality: target_modality.map(str::to_string),
        project_class: project_class.map(str::to_string),
        available_data: available_data.to_vec(),
        desired_outputs,
        ..Default::default()
    };
    let mut ctx = PlanningContext::new(intent);
    ctx.additional_modalities = additional_modalities
        .iter()
        .map(|s| s.to_string())
        .collect();
    ctx
}

/// Forward / backward / meet-in-the-middle planner. Returns a typed
/// `PlannerResult` carrying the primary outcome plus ranked
/// alternatives. The planner drives its dedicated search modules
/// directly and does not delegate to the v2 archetype or v3
/// backward-chain composers.
///
/// Algorithm:
///
/// 1. **Archetype seed.** When `archetype_reg.find_match(...)` returns
///    a candidate, lift its scaffold into a typed `WorkflowDag` and
///    push it as a candidate alternative tagged `source = "archetype"`.
/// 2. **Search seed.** Run [`super::forward_search::forward_search`]
///    over the intent's `available_data` and
///    [`super::backward_search::backward_search`] over the intent's
///    `desired_outputs`, then meet in the middle via
///    [`super::meet_in_middle::meet_in_the_middle`]. Connected /
///    partially-connected results become a `source = "search"`
///    alternative.
/// 3. **Score + rank.** Each alternative is scored by the design's
///    16-component tuple ([`score_dag`]); the slate is sorted (lower
///    is better) and truncated to `ctx.max_alternatives`.
/// 4. **Outcome selection.** The primary alternative's score +
///    structure flow through [`classify_outcome_with_sandbox`] and
///    [`classify_outcome_with_policy`] to produce the final
///    `ComposeOutcome`. When no alternatives are produced, the
///    planner emits a typed `PartialDag` with a gap pointing at the
///    goal data type.
pub fn plan(
    ctx: &PlanningContext,
    goal: &GoalSpec,
    project_class: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
) -> PlannerResult {
    let mut alternatives: Vec<RankedAlternative> = Vec::new();

    // Seed 1 — archetype match (treat as v4 candidate subgraph). Use
    // `find_match_with_modality_and_kind` so the matcher behaves the
    // same as the production archetype selector (modality + kind
    // disambiguators apply).
    //
    // Capture the archetype's match evidence so we
    // can detect a "definitive canonical match" downstream and bias
    // scoring toward the archetype seed. Without this, scrnaseq's
    // `single_cell_de` archetype (which carries the load-bearing goal
    // atoms `cell_type_annotation` + `differential_expression`) loses
    // to the search seed on raw node count even though it represents
    // the canonical pipeline pattern for the requested modality+goal.
    let mut archetype_seed = try_archetype_seed(goal, project_class, atom_reg, archetype_reg, ctx);
    // Proposal C fallback: when neither the single-modality nor the
    // cross-omics archetype matcher produced a seed, fall through to a
    // pure backward type-directed A* search over the atom catalog. A
    // synthesized chain becomes an ad-hoc "archetype" tagged
    // `adhoc_<goal_iri>` so the rest of the planner lifts it like any
    // other seed. Default available inputs: FASTQ (`data:2044`). Future
    // work threads `IntakeFacts.input_kinds` in here so non-FASTQ
    // intake (CDISC tabular, MTBLS metabolomics, etc.) gets the right
    // starting frontier.
    if archetype_seed.is_none() {
        archetype_seed = try_backward_search_fallback(goal, atom_reg);
    }
    let archetype_definitive = archetype_seed
        .as_ref()
        .map(|seed| is_definitive_archetype_match(&seed.evidence))
        .unwrap_or(false);
    if let Some(seed) = archetype_seed {
        let mut dag = lift_to_workflow_dag(&seed.composition, ctx, goal);
        // Synthesize `validate_*` companions for
        // result-producing atoms. Mirrors v2's `emit_stage` post-pass
        // so v4 emissions reach parity with the v2 baseline.
        super::companion_synthesis::synthesize_validate_companions(&mut dag, atom_reg);
        // Closure Phase B.3 — synthesize `discover_<axis>` companions
        // for any atom in the lifted DAG that signals runtime method
        // discovery (`method_choice.deferred_to` or
        // `attributes.candidate_tools`). Mirrors v2's `emit_stage`
        // Discovery-task wrapper so `set_intake_method(<stage>,...)`
        // and the `IntakeFollowup` state-trigger fire against v4 DAGs.
        // The function appends both the discover_X node AND its
        // `discover_X → X` `EdgeContract` so the lowering pass folds
        // the dependency into `Task.depends_on`. Without the edge,
        // every `discover_*` lowers as an orphan in `WORKFLOW.json`.
        super::discover_companion_synthesis::synthesize_discover_companions(&mut dag, atom_reg);
        // Wire stranded analytical atoms into the reporting terminal.
        // Some archetype shapes (multi-omics integrator slots,
        // cross-omics compose: branches, optional reporting consumers)
        // leave load-bearing atoms with only their validate_* companion
        // as downstream consumer. Without this pass the atom's output
        // exists in the package but never flows into the SME report.
        // Runs AFTER validate-companion synthesis so validators are
        // visible as terminal nodes and don't count as a reporting
        // path on their own.
        super::reporting_consumer_synthesis::wire_dangling_analytical_atoms_to_reporting(&mut dag);
        let score = score_dag(&dag, ctx);
        let summary = summarize_dag(&dag, &score);
        alternatives.push(RankedAlternative {
            dag,
            score,
            summary,
            source: "archetype".into(),
        });
    }

    // Seed 2 — forward / backward / meet-in-the-middle search over
    // the typed atom registry. Bounds come from `ctx`
    // (`max_branches`, `max_depth`); the search is replayable in
    // determinism tests because each module's iteration order is
    // sorted at every step (BTreeMap-keyed).
    //
    // R1/R2 closure — route through the `_with_opaque_observation`
    // siblings so each `engine.prove()` call site sees the planning
    // context's sink + session id. When unset (bare callers, tests),
    // the search behaves identically to the 4-arg form.
    let forward = super::forward_search::forward_search_with_opaque_observation(
        &ctx.intent,
        atom_reg,
        ctx.max_depth,
        ctx.max_branches,
        ctx.opaque_observation_sink.clone(),
        ctx.opaque_session_id.as_deref(),
    );
    let backward = super::backward_search::backward_search_with_opaque_observation(
        &ctx.intent,
        atom_reg,
        ctx.max_depth,
        ctx.max_branches,
        ctx.opaque_observation_sink.clone(),
        ctx.opaque_session_id.as_deref(),
    );
    let meet = super::meet_in_middle::meet_in_the_middle_with_opaque_observation(
        &forward,
        &backward,
        atom_reg,
        ctx.opaque_observation_sink.clone(),
        ctx.opaque_session_id.as_deref(),
    );
    // v4 P5 — snapshot the structured `repair_gaps` BEFORE matching so
    // the repair-strategy registry sees them once we drop into Phase
    // 8.5 wiring below.
    let repair_gaps_snapshot: Vec<crate::repair::proposal::RepairGap> = match &meet {
        super::meet_in_middle::MeetResult::Connected { repair_gaps, .. }
        | super::meet_in_middle::MeetResult::PartiallyConnected { repair_gaps, .. }
        | super::meet_in_middle::MeetResult::Disconnected { repair_gaps, .. } => {
            repair_gaps.clone()
        }
    };
    match meet {
        super::meet_in_middle::MeetResult::Connected { mut dag, .. }
        | super::meet_in_middle::MeetResult::PartiallyConnected { mut dag, .. } => {
            // Stamp the search result with the planning context's id
            // so the lowering pass produces the same DAG id as the
            // archetype path.
            dag.id = format!("composed:{}", ctx.intent.id);
            // Synthesize `validate_*` companions
            // for result-producing atoms after meet-in-the-middle
            // assembled the core DAG. Mirrors v2's `emit_stage`
            // post-pass so v4 emissions reach parity with the v2
            // baseline (validate_<id> companion per non-validation /
            // non-discovery / non-adapter operation atom).
            super::companion_synthesis::synthesize_validate_companions(&mut dag, atom_reg);
            // Closure Phase B.3 — synthesize `discover_<axis>`
            // companions analogous to v2's `emit_stage` discovery-
            // task wrapper. Atoms with `method_choice.deferred_to` or
            // `attributes.candidate_tools` get a companion node so
            // `set_intake_method(<stage>,...)` and the
            // `IntakeFollowup` state-trigger fire against v4 DAGs.
            // The function appends both the discover_X node AND its
            // `discover_X → X` `EdgeContract` so the lowering pass
            // folds the dependency into `Task.depends_on`. Without
            // the edge, every `discover_*` lowers as an orphan in
            // `WORKFLOW.json`.
            super::discover_companion_synthesis::synthesize_discover_companions(&mut dag, atom_reg);
            // Wire stranded analytical atoms into the reporting
            // terminal — see the archetype-seed branch above for the
            // full rationale. Runs at parity with the archetype-seed
            // path so search-driven DAGs also reach the strand-free
            // contract.
            super::reporting_consumer_synthesis::wire_dangling_analytical_atoms_to_reporting(
                &mut dag,
            );
            let mut score = score_dag(&dag, ctx);
            // When an archetype seed presents a
            // definitive canonical match (modality_hint + goal_data +
            // goal_format all matched), prefer that seed as primary.
            // The search seed is the discovery helper for cases where
            // no archetype claims the goal+modality; when one does,
            // search competing on raw node count would silently drop
            // load-bearing canonical atoms (e.g. scrnaseq
            // `cell_type_annotation` + `differential_expression`)
            // because their outputs don't unify with the goal's
            // `data:3917` (count matrix) port shape — they output
            // `data:3736` / `data:0951`. Tag the search alternative
            // with a +1000 scientific_appropriateness penalty so the
            // archetype seed wins ranking and surfaces as primary
            // while search remains in the comparison slate.
            if archetype_definitive {
                score.scientific_appropriateness_penalty = score
                    .scientific_appropriateness_penalty
                    .saturating_add(1000);
            }
            let summary = summarize_dag(&dag, &score);
            alternatives.push(RankedAlternative {
                dag,
                score,
                summary,
                source: "search".into(),
            });
        }
        super::meet_in_middle::MeetResult::Disconnected {
            gaps,
            repair_gaps: _,
        } => {
            // Search produced nothing. Drop — the archetype seed (if
            // any) carries the planner's output. We deliberately do
            // not log here: `core` is the synchronous compiler crate
            // and pulling in `tracing` would violate the no-tokio /
            // no-async-runtime architectural rule. The gap detail is
            // retrievable by the caller via direct
            // `meet_in_the_middle` invocation (the dispatcher in
            // `compose_v4_dispatch_full` synthesizes its own
            // `PartialDag` gap report when alternatives is empty).
            let _ = gaps;
        }
    }

    // Consult the repair-strategy registry for every structured gap
    // surfaced by `meet_in_the_middle` (both Disconnected and
    // PartiallyConnected meets). Every proposal emits
    // `VerifierDecision::RepairProposed` to the substrate; only
    // `LowAutoAttempt` proposals are eligible for auto-application,
    // and even then only when `ctx.repair_registry` is wired.
    // `MediumUserGated` + `HighCredentialedReview` proposals are
    // emitted but NEVER applied here — accept/reject is the SME's
    // explicit decision through the server endpoints.
    //
    // `LowAutoAttempt` proposals actually splice the modification
    // into the search-derived DAG via
    // `dag_mutation::apply_dag_modification`, then re-rank the
    // alternatives. Failures emit `RepairRejected { reason }` so the
    // substrate captures the apply-time failure mode.
    if let Some(registry) = ctx.repair_registry.as_ref() {
        for gap in &repair_gaps_snapshot {
            let proposals = registry.propose(gap, ctx);
            for proposal in proposals {
                crate::decision_substrate::record(
                    crate::decision_substrate::VerifierDecision::RepairProposed {
                        id: crate::decision_substrate::stable_id(
                            "repair_proposed",
                            &proposal.id,
                            &proposal.gap_id,
                        ),
                        timestamp: crate::decision_substrate::timestamp(),
                        gap_id: proposal.gap_id.clone(),
                        strategy: proposal.strategy_id.clone(),
                        risk_class: format!("{:?}", proposal.risk_class),
                        proposal_payload: serde_json::to_string(&proposal).unwrap_or_default(),
                    },
                );
                // F20 invariant — only proposals whose risk class is at
                // or below the auto-attempt threshold may mutate the
                // DAG. v3+v4 residuals actually splice the
                // mutation into the search alternative's DAG so safe
                // mechanical repairs (gzip decompression, sort/index)
                // flow end-to-end without an SME click.
                //
                // `MediumUserGated` + `HighCredentialedReview` skip
                // this branch entirely; their proposals remain in
                // substrate as `RepairProposed` only, awaiting SME
                // accept via the `/repair/:proposal_id/accept` route.
                if proposal.risk_class <= ctx.auto_attempt_risk_threshold {
                    // Target the search alternative (gaps come from
                    // `meet_in_the_middle`, which only runs on the
                    // search seed). If no search alternative exists,
                    // emit `RepairRejected` and move on.
                    let search_idx = alternatives.iter().position(|a| a.source == "search");
                    match search_idx {
                        Some(idx) => {
                            let payload_json =
                                serde_json::to_string(&proposal.modification).unwrap_or_default();
                            match super::dag_mutation::apply_dag_modification(
                                &mut alternatives[idx].dag,
                                &proposal.modification,
                            ) {
                                Ok(()) => {
                                    crate::decision_substrate::record(
                                        crate::decision_substrate::VerifierDecision::RepairAccepted {
                                            id: crate::decision_substrate::stable_id(
                                                "repair_accepted",
                                                &proposal.id,
                                                "auto",
                                            ),
                                            timestamp: crate::decision_substrate::timestamp(),
                                            proposal_id: proposal.id.clone(),
                                            acceptor: "planner_auto_apply".into(),
                                            credentials: Vec::new(),
                                            attempt_kind:
                                                crate::decision_substrate::AttemptKind::Auto,
                                            applied_modification: Some(payload_json),
                                        },
                                    );
                                }
                                Err(e) => {
                                    crate::decision_substrate::record(
                                        crate::decision_substrate::VerifierDecision::RepairRejected {
                                            id: crate::decision_substrate::stable_id(
                                                "repair_rejected",
                                                &proposal.id,
                                                "auto_apply_failed",
                                            ),
                                            timestamp: crate::decision_substrate::timestamp(),
                                            proposal_id: proposal.id.clone(),
                                            reason: format!("auto-apply failed: {e}"),
                                        },
                                    );
                                }
                            }
                        }
                        None => {
                            crate::decision_substrate::record(
                                crate::decision_substrate::VerifierDecision::RepairRejected {
                                    id: crate::decision_substrate::stable_id(
                                        "repair_rejected",
                                        &proposal.id,
                                        "no_search_alternative",
                                    ),
                                    timestamp: crate::decision_substrate::timestamp(),
                                    proposal_id: proposal.id.clone(),
                                    reason:
                                        "auto-apply skipped: no search-derived alternative to mutate"
                                            .to_string(),
                                },
                            );
                        }
                    }
                }
            }
        }
    }

    if alternatives.is_empty() {
        // Neither path produced a composition. Surface as PartialDag
        // with a typed gap pointing at the goal data type.
        let dag = WorkflowDag {
            id: format!("composed:{}", ctx.intent.id),
            ..Default::default()
        };
        return PlannerResult {
            primary: ComposeOutcome::PartialDag {
                dag,
                unresolved_gaps: vec![GapReport {
                    id: "no_producer_for_goal".into(),
                    statement: format!(
                        "no producer atom in registry produces {} (no archetype matched and \
                         search produced no connected DAG)",
                        goal.edam_data
                    ),
                    missing_port: Some(goal.edam_data.clone()),
                    suggestions: vec![
                        "Add an atom whose edam_data matches the goal".into(),
                        "Adjust the goal's data type or format to match an existing producer"
                            .into(),
                    ],
                }],
            },
            alternatives: Vec::new(),
        };
    }

    // Sort alternatives by scoring tuple (lower is better), then by
    // source (archetype < search) as a stable tie-break. We do NOT
    // dedupe by node-set: two alternatives with identical structure
    // but distinct provenance (archetype vs search) remain visible to
    // the SME so they can pick the path matching their mental model.
    alternatives.sort_by(|a, b| {
        a.score
            .cmp(&b.score)
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.dag.id.cmp(&b.dag.id))
    });

    // v4 P2 / F18 — emit one `AlternativeRanked` row per ranked
    // alternative so the verifier substrate captures the slate's
    // composition. Recorded BEFORE truncation so the substrate
    // surfaces every alternative the planner considered, even when
    // `ctx.max_alternatives` is 1.
    for (rank, alt) in alternatives.iter().enumerate() {
        crate::decision_substrate::record(
            crate::decision_substrate::VerifierDecision::AlternativeRanked {
                id: crate::decision_substrate::stable_id("alt", &alt.dag.id, &alt.source),
                timestamp: crate::decision_substrate::timestamp(),
                dag_id: alt.dag.id.clone(),
                rank: rank as u32,
                source: alt.source.clone(),
                score_summary: alt.summary.clone(),
            },
        );
    }

    let primary_dag = alternatives[0].dag.clone();
    let primary_score = alternatives[0].score.clone();

    // Determine outcome shape from the primary's score + content.
    // Apply the sandbox policy's GeneratedCode refusal sweep
    // before presenting the composition outcome to the caller.
    let effective_sandbox = ctx
        .sandbox_policy
        .clone()
        .unwrap_or_else(SandboxPolicy::default_strict);
    let outcome =
        classify_outcome_with_sandbox(&primary_dag, &primary_score, &effective_sandbox, ctx);

    let max_alts = ctx.max_alternatives.max(1) as usize;
    alternatives.truncate(max_alts);

    PlannerResult {
        primary: outcome,
        alternatives,
    }
}

/// Archetype seed paired with the match evidence
/// that produced it. The evidence is needed by the planner so it can
/// detect a "definitive canonical match" (modality_hint + goal_data +
/// goal_format all fired) and bias scoring toward the archetype seed.
struct ArchetypeSeed {
    composition: CompositionResult,
    evidence: crate::archetype_registry::ScoreEvidence,
}

/// Predicate over `ScoreEvidence`. Returns `true`
/// when the archetype represents the canonical pipeline for the
/// requested goal+modality+format triple. The three load-bearing
/// components must all be set:
///
/// - `goal_data_exact > 0` (archetype.goal_data == target.edam_data),
/// - `goal_format_match > 0` (archetype.goal_format == target.edam_format),
/// - `modality_match > 0` (archetype.modality_hint == intent.modality).
///
/// When all three fire, the archetype is **the** canonical pattern for
/// the requested analysis — search-driven discovery should not override
/// it. Subtype-only goal_data matches (e.g. `goal_data_subtype > 0`
/// without exact) don't qualify because the archetype is a partial
/// match in that case; the SME's intent might genuinely be a different
/// kind of analysis where search-driven discovery is appropriate.
fn is_definitive_archetype_match(evidence: &crate::archetype_registry::ScoreEvidence) -> bool {
    // Modality_hint match alone is enough to make an archetype
    // definitive. Requiring goal_data + goal_format + modality together
    // would drop the archetype's definitive bonus whenever the
    // classifier infers a goal from method keywords (e.g. "DESeq2" →
    // data:0951 DE-result) but the archetype declares a modality-natural
    // goal (e.g. atac_seq_peaks → data:1255 peaks). The search-side
    // alternative — which pulls atoms from every modality — then wins
    // ranking and yields cross-modality pollution (e.g. a
    // single_cell_rnaseq package that includes proteomics + metagenomics
    // + time-series atoms). Modality match is the strongest signal of
    // "right kind of analysis"; treat it as definitive on its own.
    evidence.modality_match > 0
}

/// Archetype-seed helper. Mirrors the v3 archetype fast-path's
/// disambiguation rules (modality + kind) but stops short of falling
/// back to backward-chain — the search seed in `plan()` covers that
/// path with a typed proof-carrying DAG. Returns `None` when:
///
/// 1. No archetype matches the goal.
/// 2. The match is in a 5%-tie window (caller treats as "search-only"
///    rather than picking arbitrarily — the SME-facing tie-breaking
///    card surfaces in the v3 path; here we silently skip and let the
///    search result take over).
/// 3. Atom resolution fails (returns `None` rather than propagating
///    `UnknownAtom` — the planner's job is to produce a slate, not to
///    fail hard on archetype config issues).
fn try_archetype_seed(
    goal: &GoalSpec,
    project_class: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
    ctx: &PlanningContext,
) -> Option<ArchetypeSeed> {
    let target_modality = ctx.intent.modality.as_deref();
    let target_kind = goal.modifiers.get("kind").map(|s| s.as_str());

    // When the SME requested two or more modalities, prefer a
    // cross-omics archetype match (set-equality on
    // `cross_omics_modalities`) over single-modality matching. The
    // cross-omics archetypes encode the parallel-pipeline scaffold
    // (namespaced via `alias` per archetype atom) that single-modality
    // matching can't reproduce: e.g. `cross_omics_rnaseq_proteomics`
    // builds two `data_acquisition → ... → differential_expression`
    // chains aliased as `rnaseq_*` / `proteomics_*` joined at
    // `cross_omics_thematic_comparison`. Without this seed the v4
    // planner would consult only the single-modality matcher (which
    // excludes archetypes with `cross_omics_modalities`) and emit a
    // single bare-name pipeline.
    // N-way intent override: when goal.modifiers["n_way_intent"] is
    // set (rebuild_dag detects ≥3 distinct modality nouns in a
    // comma-list), call try_cross_omics_archetype_seed even when
    // classifier surfaced only a single modality. The seed function
    // uses the n_way path internally (subset matching) so a 3-way
    // archetype like cross_omics_rnaseq_atac_chip can match against
    // [bulk_rnaseq] alone — the SME's prose evidence outweighs the
    // classifier's under-counting.
    let n_way_signal = goal.modifiers.contains_key("n_way_intent");
    if !ctx.additional_modalities.is_empty() || n_way_signal {
        if let Some(seed) = try_cross_omics_archetype_seed(
            goal,
            project_class,
            target_modality,
            &ctx.additional_modalities,
            target_kind,
            atom_reg,
            archetype_reg,
        ) {
            return Some(seed);
        }
    }

    let matches = archetype_reg.find_match_with_evidence_modality_kind(
        &goal.edam_data,
        goal.edam_format.as_deref(),
        project_class,
        target_modality,
        target_kind,
    );
    if matches.is_empty() {
        return None;
    }
    let top_score = matches[0].evidence.total;
    let top_evidence = matches[0].evidence;
    let top_modality_match = matches[0].evidence.modality_match;
    let tie_threshold = (top_score as f32 * 0.95).floor() as u32;
    // The tie window only considers archetypes with
    // the SAME modality_match level as the top match. After we
    // sort modality_match-first, a top atac_seq_peaks entry
    // (modality_match=2, total=3) shouldn't tie with any
    // bulk_rnaseq_de that happens to share the goal triple but
    // doesn't match atac_seq's modality (modality_match=0,
    // total=6 just on goal_data + format + project_class).
    let close: Vec<_> = matches
        .iter()
        .filter(|m| {
            m.evidence.modality_match == top_modality_match && m.evidence.total >= tie_threshold
        })
        .collect();
    if close.len() > 1 {
        // Tied archetypes — skip the seed; the search path produces a
        // single typed DAG without forcing the SME to pick between
        // close-call archetypes here.
        return None;
    }
    let mut archetype = matches[0].archetype;
    let mut effective_evidence = top_evidence;
    let mut effective_score = top_score;

    // Remediation task 2 — defense in depth against the
    // bare-keyword false-positive class (paper-01..paper-10 hit this
    // via the "prespecified" trigger; task 1 narrowed the keyword;
    // this catches any future bare keyword leak). When `project_class`
    // routes to a project-class archetype (e.g. `clinical_trial_analysis`)
    // but the classified `modality` is a sequencing modality (e.g.
    // `bulk_rnaseq`) whose canonical pipeline starts from FASTQ, the
    // project-class archetype's first atom (`data_import`, CDISC tabular
    // only) cannot feed the modality's downstream stages. Demote to the
    // modality-canonical archetype (`<modality>_de`) when the input
    // shapes are incompatible.
    //
    // The check is intentionally narrow: only fires when (a) the
    // matcher's `modality_match` component is 0 (the picked archetype's
    // `modality_hint` does NOT cover the classified modality), (b) the
    // first-atom output set of the modality-canonical archetype is NOT
    // a subset of the picked archetype's first-atom output set, AND
    // (c) the modality-canonical archetype's `goal_data` matches the
    // requested goal IRI (so we don't trade a wrong-shape archetype
    // for a wrong-goal archetype — e.g. catch-all `generic_omics`
    // whose `goal_data=data:0006` would fail to satisfy a DE goal
    // `data:0951`). If the project-class archetype IS compatible
    // (e.g. clinical-trial bio data via ADaM bridge — same first-atom
    // shape as the modality canonical), this branch is a no-op.
    if let Some(modality_id) = ctx.intent.modality.as_deref() {
        if effective_evidence.modality_match == 0
            && !arch_inputs_compatible_with_modality(
                archetype,
                modality_id,
                atom_reg,
                archetype_reg,
            )
        {
            if let Some(fallback) =
                archetype_reg.find_primary_for_modality(modality_id, "bioinformatics")
            {
                let re_matches = archetype_reg.find_match_with_evidence_modality_kind(
                    &goal.edam_data,
                    goal.edam_format.as_deref(),
                    &fallback.project_class,
                    Some(modality_id),
                    target_kind,
                );
                let fallback_match = re_matches.iter().find(|m| m.archetype.id == fallback.id);
                let fallback_matches_goal = fallback_match
                    .map(|m| m.evidence.goal_data_exact > 0 || m.evidence.goal_data_subtype > 0)
                    .unwrap_or(false);
                if fallback.id != archetype.id && fallback_matches_goal {
                    tracing::warn!(
                        archetype = %archetype.id,
                        fallback = %fallback.id,
                        modality = %modality_id,
                        project_class = %project_class,
                        "archetype-modality input collision; demoting project-class archetype to modality fallback"
                    );
                    archetype = fallback;
                    if let Some(rm) = fallback_match {
                        effective_evidence = rm.evidence;
                        effective_score = rm.evidence.total;
                    }
                }
            }
        }
    }

    // Weak-match → generic_omics demotion was added in 40c95251 to
    // handle flex/novel-method scenarios (cytof, MR, etc.). Reverted
    // because it caused canonical scrnaseq scenarios to demote to
    // generic_omics, dropping the clustering/dim-reduction/integration
    // atoms (failed `v4_dispatch_synthesizes_validate_clustering`). A
    // future revision needs to distinguish "weak match with no goal
    // signal but goal_data ontology HIT" vs "weak match with no
    // ontology hits"; the current goal_data_signal == 0 check is too
    // coarse. Tracked as follow-up; flex/novel-method scenarios are
    // still rescued by the cross-omics degraded-fallback +
    // single-modality retry paths upstream.

    // Slot expansion: if the archetype declares a slot manifest,
    // resolve its value from goal.modifiers OR goal.source_prose,
    // then expand the atom list with the slot value's extra_atoms.
    // Falls back to base atoms only when the archetype has no slots.
    let expanded_atoms: Vec<crate::archetype::ArchetypeAtomRef> =
        if let Some(slots) = &archetype.slots {
            let prose_for_slot = goal
                .modifiers
                .get(&slots.slot_name)
                .cloned()
                .unwrap_or_else(|| {
                    // Fallback to source_prose + any modifier value scan.
                    let mut buf = goal.source_prose.clone().unwrap_or_default();
                    for v in goal.modifiers.values() {
                        buf.push(' ');
                        buf.push_str(v);
                    }
                    buf
                });
            let chosen_id = crate::archetype_slots::resolve_slot_value(slots, &prose_for_slot);
            crate::archetype_slots::expand_atoms(&archetype.atoms, slots, &chosen_id)
        } else {
            archetype.atoms.clone()
        };

    // Resolve atoms + apply wiring. This mirrors the v3 archetype
    // path's atom resolution but stays inline so the planner doesn't
    // re-enter the full `compose_with_version_and_modality` fallback
    // chain.
    let mut composed: Vec<ComposedAtom> = Vec::with_capacity(expanded_atoms.len());
    for aref in &expanded_atoms {
        let atom = atom_reg.get(aref.atom_id.as_str())?;
        let container = resolve_task_container(atom, None, None);
        composed.push(ComposedAtom {
            stage_id: aref
                .alias
                .as_deref()
                .map(StageId::from)
                .unwrap_or_else(|| StageId::from(aref.atom_id.as_str())),
            atom: crate::composer::apply_atom_ref_overrides(atom, aref),
            depends_on: crate::composer::merged_depends_on(atom, aref),
            required: aref.required,
            bindings: Vec::new(),
            container,
        });
    }

    let resource_estimate = aggregate_resources(&composed);
    let atom_rationales: BTreeMap<String, AtomSelectionRationale> = composed
        .iter()
        .map(|c| {
            (
                c.stage_id.to_string(),
                AtomSelectionRationale {
                    stage_id: c.stage_id.clone(),
                    atom_id: c.atom.id.as_str().into(),
                    reason: SelectionReason::ArchetypeRequired,
                    score: effective_score,
                    explanation: format!(
                        "Archetype {} declares {} as required.",
                        archetype.id, c.atom.id
                    ),
                },
            )
        })
        .collect();
    Some(ArchetypeSeed {
        composition: CompositionResult {
            matched_archetype: Some(archetype.id.clone()),
            match_score: effective_score,
            atoms: composed,
            goal: goal.clone(),
            rationale: archetype.sme_summary.clone(),
            atom_rationales,
            resource_estimate,
        },
        evidence: effective_evidence,
    })
}

/// Proposal C — final fallback for [`plan`] when neither the
/// single-modality nor cross-omics archetype matcher seeded a
/// composition. Runs a pure backward type-directed A* search over the
/// atom catalog (`composer_v4::backward_search::search_backward`) and,
/// on success, synthesizes an ad-hoc `ArchetypeSeed` whose
/// `matched_archetype` is tagged `adhoc_<goal_iri>`. Atoms are linked
/// linearly (each atom depends only on the previous one in the chain)
/// so the lift-to-DAG pass produces a valid topology. Available inputs
/// default to FASTQ (`data:2044`); the search bound is depth 10.
///
/// Returns `None` when the search produces no chain — the caller falls
/// through to the search seed and (if that also fails) the
/// `PartialDag` no-producer-for-goal path.
fn try_backward_search_fallback(goal: &GoalSpec, atom_reg: &AtomRegistry) -> Option<ArchetypeSeed> {
    use crate::composer_v4::backward_search::{search_backward, BackwardSearchInput};

    // Default seed: FASTQ. Future work threads `IntakeFacts.input_kinds`
    // in here for non-FASTQ intake (CDISC tabular, MTBLS, mass spec).
    let available_inputs = vec!["data:2044".to_string()];
    let chain = search_backward(BackwardSearchInput {
        goal_data: goal.edam_data.clone(),
        goal_format: goal.edam_format.clone(),
        available_inputs,
        atom_registry: atom_reg,
        max_depth: 10,
    })?;

    // Synthesize a linear-dependency composition. Each atom depends on
    // the previous atom in the chain (intake-first); the first atom
    // has no in-chain dependency.
    let composed: Vec<ComposedAtom> = chain
        .iter()
        .enumerate()
        .map(|(i, ra)| {
            let depends_on = if i == 0 {
                Vec::new()
            } else {
                vec![chain[i - 1].atom.id.clone()]
            };
            let container = resolve_task_container(ra.atom, None, None);
            ComposedAtom {
                stage_id: ra.atom.id.clone().into(),
                atom: ra.atom.clone(),
                depends_on,
                required: true,
                bindings: Vec::new(),
                container,
            }
        })
        .collect();

    tracing::warn!(
        goal = %goal.edam_data,
        chain_len = chain.len(),
        "backward_search_fallback_synthesized_adhoc_chain",
    );

    Some(ArchetypeSeed {
        composition: CompositionResult {
            matched_archetype: Some(format!("adhoc_{}", goal.edam_data.replace(':', "_"))),
            match_score: 0,
            atoms: composed,
            goal: goal.clone(),
            rationale: "Synthesized via backward type-directed A* search".into(),
            atom_rationales: BTreeMap::new(),
            resource_estimate: crate::composer::ResourceEstimate::default(),
        },
        evidence: Default::default(),
    })
}

/// Remediation task 2 — input-shape compatibility check for
/// the project-class → modality archetype fallback. Returns `true` when
/// the archetype's first-atom output set is compatible with the
/// modality's canonical first-atom output set (the modality-canonical's
/// outputs are a subset of, or share the distinctive sequencing/data
/// type with, the candidate's first-atom outputs).
///
/// Returns `true` (the safe default — don't demote) when:
/// - `modality_id` has no canonical archetype in the registry
///   (`find_primary_for_modality` returns `None`),
/// - the candidate archetype has no atoms,
/// - the modality canonical's first atom isn't loadable from the atom
///   registry,
/// - the candidate's first-atom id matches the canonical's (same
///   pipeline start → compatible by construction),
/// - the candidate's first-atom outputs already cover the
///   modality-canonical's first-atom outputs (compatible by subset).
///
/// Returns `false` (incompatible — demotion candidate) only when both
/// archetypes resolve, their first atoms differ in id, AND the
/// modality canonical's first-atom output IRIs include at least one
/// sequencing/data type that the candidate's first-atom outputs do
/// not — i.e. the candidate's first stage cannot feed the modality's
/// downstream pipeline because the data shape is missing.
///
/// Mirrors the plan §Task 2 step 2.3 "first-atom inputs
/// incompatible" principle but uses output-side comparison because
/// both first-atom inputs are typically `data:2531`-parented
/// descriptors that share parent shape — the discriminator is whether
/// the FIRST atom produces the modality's distinctive sequencing
/// artifact (`data:2044` for bulk_rnaseq, `data:2914` for generic_omics,
/// etc).
fn arch_inputs_compatible_with_modality(
    candidate: &crate::archetype::ArchetypeDefinition,
    modality_id: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
) -> bool {
    let Some(canonical) = archetype_reg.find_primary_for_modality(modality_id, "bioinformatics")
    else {
        return true;
    };
    if std::ptr::eq(canonical, candidate) || canonical.id == candidate.id {
        return true;
    }
    let candidate_first = candidate
        .atoms
        .first()
        .and_then(|a| atom_reg.get(a.atom_id.as_str()));
    let canonical_first = canonical
        .atoms
        .first()
        .and_then(|a| atom_reg.get(a.atom_id.as_str()));
    let (Some(candidate_atom), Some(canonical_atom)) = (candidate_first, canonical_first) else {
        return true;
    };
    if candidate_atom.id == canonical_atom.id {
        return true;
    }
    let candidate_outputs: BTreeSet<String> = candidate_atom
        .outputs
        .iter()
        .map(|p| p.semantic_type.stable_id())
        .collect();
    let canonical_outputs: BTreeSet<String> = canonical_atom
        .outputs
        .iter()
        .map(|p| p.semantic_type.stable_id())
        .collect();
    canonical_outputs.is_subset(&candidate_outputs)
}

/// Cross-omics archetype seed. Mirrors the v3
/// `compose_with_version_and_modalities` cross-omics branch:
///
/// 1. Build the full modality set (`primary` ++ `additional`).
/// 2. Call `archetype_reg.find_match_cross_omics(...)`. The matcher
///    requires set-equality on the archetype's `cross_omics_modalities`
///    field, so a 2-way request `[bulk_rnaseq, proteomics]` matches
///    `cross_omics_rnaseq_proteomics` but does NOT match a 3-way
///    `[rnaseq, proteomics, atac]` archetype (intentional — preventing
///    silent modality drop).
/// 3. Lift the top-scored archetype's atoms with their `alias`-honored
///    stage ids so namespaced parallel-pipeline scaffolds (`rnaseq_*`,
///    `proteomics_*`) materialize correctly.
///
/// Cross-omics matches synthesize a `ScoreEvidence` with
/// `modality_match = 2` so `is_definitive_archetype_match` fires and
/// the caller's archetype-primacy bias prevents the search seed from
/// silently outranking the canonical scaffold on raw node count.
fn try_cross_omics_archetype_seed(
    goal: &GoalSpec,
    project_class: &str,
    primary_modality: Option<&str>,
    additional_modalities: &[String],
    target_kind: Option<&str>,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
) -> Option<ArchetypeSeed> {
    // Assemble the full modality set. Skip when primary is missing —
    // cross-omics requires at least 2 modalities by definition.
    let primary = primary_modality?;
    let mut full_modalities: Vec<&str> = Vec::with_capacity(1 + additional_modalities.len());
    full_modalities.push(primary);
    for m in additional_modalities {
        if !full_modalities.contains(&m.as_str()) {
            full_modalities.push(m.as_str());
        }
    }
    // Relax the size guard when n_way_intent is set on goal.modifiers.
    // SMEs writing "RNA-seq, ATAC-seq, and ChIP-seq" without an
    // n-way conjunction often have the classifier surface only one
    // or two modalities while the prose clearly names three. The
    // subset matcher in find_match_cross_omics (also gated on
    // n_way_intent) handles partial-modality cases by searching for
    // superset archetypes. Returning early at size < 2 prevented
    // the subset matcher from running at all.
    let n_way_signal = goal.modifiers.contains_key("n_way_intent");
    if full_modalities.len() < 2 && !n_way_signal {
        return None;
    }

    // Reconstruct prose for n-way intent detection from
    // goal.source_prose + every modifier value. The dispatch path
    // hasn't been given the raw intake_prose; this is the next-best
    // signal. Without n_way_intent, the matcher refuses to
    // superset-match a 2-modality request against a 3-modality
    // archetype (prevents orphan-branch injection).
    let mut prose = goal.source_prose.clone().unwrap_or_default();
    for v in goal.modifiers.values() {
        prose.push(' ');
        prose.push_str(v);
    }
    let n_way = n_way_signal || crate::classify::is_n_way_intent(&prose);

    // Exact modality coverage is stronger than the classifier's branch
    // goal phrase. In tri-omics prose the classifier may choose the
    // ChIP "peak calling" goal even though the SME explicitly named
    // RNA-seq + ATAC + ChIP. If we require the cross-omics archetype's
    // `goal_data` to score against that branch goal before seeding,
    // the planner falls through to the search alternative and emits a
    // generic single-branch DAG. Prefer the exact cross-omics scaffold
    // whenever the requested modality set exactly matches one catalog
    // archetype for this project class; its own metadata supplies the
    // branch-specific task set and wiring.
    let requested: std::collections::BTreeSet<&str> = full_modalities.iter().copied().collect();
    let exact_cross = archetype_reg
        .iter()
        .map(|(_, archetype)| archetype)
        .filter(|archetype| {
            archetype.project_class == project_class && !archetype.cross_omics_modalities.is_empty()
        })
        .filter(|archetype| {
            let have: std::collections::BTreeSet<&str> = archetype
                .cross_omics_modalities
                .iter()
                .map(String::as_str)
                .collect();
            have == requested
        })
        .min_by(|a, b| a.id.cmp(&b.id));

    let (archetype, top_score) = if let Some(archetype) = exact_cross {
        // This is a structural match, not a branch-goal match. Use a
        // stable non-zero score for rationale display; ranking still
        // comes from the synthetic ScoreEvidence below.
        (archetype, 10)
    } else {
        let matches = archetype_reg.find_match_cross_omics(
            &goal.edam_data,
            goal.edam_format.as_deref(),
            project_class,
            &full_modalities,
            target_kind,
            n_way,
            &prose,
        );
        if matches.is_empty() {
            return None;
        }
        // Slot-filling refactor: integrator discrimination now happens at
        // atom-expansion time inside this function via SlotManifest +
        // expand_atoms, not by hoisting one archetype variant over others;
        // there is only one archetype per modality pair after consolidation.
        matches[0]
    };

    // Flatten archetype inheritance (compose: directives) before slot
    // expansion. For 3-way cross-omics archetypes
    // (cross_omics_rnaseq_atac_chip, cross_omics_variant_rnaseq_chipseq,
    // etc.), the archetype.atoms field declares only the cross-omics-
    // specific atoms; the per-modality branch atoms come from compose:
    // pulling in bulk_rnaseq_de + atac_seq_peaks + chip_seq_peaks with
    // id_prefix rewriting. Without this flattening, the planner
    // composed only the 3 cross-omics own atoms and the rest fell
    // through to a fallback path that lost the aliases.
    let flattened = crate::composer::resolve_inheritance(archetype, archetype_reg)
        .map_err(|e| tracing::warn!(?e, "resolve_inheritance failed"))
        .ok()?;

    // Slot expansion: if the archetype declares a slot manifest,
    // resolve its value from goal.modifiers OR goal.source_prose,
    // then expand the atom list with the slot value's extra_atoms.
    // Falls back to flattened atoms only when the archetype has no
    // slots.
    let expanded_atoms: Vec<crate::archetype::ArchetypeAtomRef> =
        if let Some(slots) = &archetype.slots {
            let prose_for_slot = goal
                .modifiers
                .get(&slots.slot_name)
                .cloned()
                .unwrap_or_else(|| {
                    // Fallback to source_prose + any modifier value scan.
                    let mut buf = goal.source_prose.clone().unwrap_or_default();
                    for v in goal.modifiers.values() {
                        buf.push(' ');
                        buf.push_str(v);
                    }
                    buf
                });
            let chosen_id = crate::archetype_slots::resolve_slot_value(slots, &prose_for_slot);
            crate::archetype_slots::expand_atoms(&flattened.atoms, slots, &chosen_id)
        } else {
            flattened.atoms.clone()
        };

    let mut composed: Vec<ComposedAtom> = Vec::with_capacity(expanded_atoms.len());
    for aref in &expanded_atoms {
        let atom = atom_reg.get(aref.atom_id.as_str())?;
        let container = resolve_task_container(atom, None, None);
        composed.push(ComposedAtom {
            stage_id: aref
                .alias
                .as_deref()
                .map(StageId::from)
                .unwrap_or_else(|| StageId::from(aref.atom_id.as_str())),
            atom: crate::composer::apply_atom_ref_overrides(atom, aref),
            depends_on: crate::composer::merged_depends_on(atom, aref),
            required: aref.required,
            bindings: Vec::new(),
            container,
        });
    }

    let resource_estimate = aggregate_resources(&composed);
    let atom_rationales: BTreeMap<String, AtomSelectionRationale> = composed
        .iter()
        .map(|c| {
            (
                c.stage_id.to_string(),
                AtomSelectionRationale {
                    stage_id: c.stage_id.clone(),
                    atom_id: c.atom.id.as_str().into(),
                    reason: SelectionReason::ArchetypeRequired,
                    score: top_score,
                    explanation: format!(
                        "Cross-omics archetype {} declares {} as required (alias: {}).",
                        archetype.id, c.atom.id, c.stage_id
                    ),
                },
            )
        })
        .collect();

    // Build a synthetic `ScoreEvidence` flagging the cross-omics
    // match path. The matcher (`find_match_cross_omics`) returns only
    // a `(archetype, total_score)` tuple, but the caller needs
    // evidence to drive the archetype-primacy bias. We synthesize a
    // record with the same goal_data / format / project_class
    // components as the matcher's `score_archetype_full` call, plus
    // `modality_match = 2` so `is_definitive_archetype_match` fires
    // for cross-omics seeds (set-equality on `cross_omics_modalities`
    // is the strongest possible modality signal).
    let mut evidence = crate::archetype_registry::ScoreEvidence::default();
    if archetype.goal_data == goal.edam_data {
        evidence.goal_data_exact = 3;
    }
    if let (Some(want), Some(got)) = (
        goal.edam_format.as_deref(),
        archetype.goal_format.as_deref(),
    ) {
        if want == got {
            evidence.goal_format_match = 2;
        }
    }
    evidence.project_class_match = 1;
    if let (Some(want), Some(got)) = (target_kind, archetype.goal_kind_hint.as_deref()) {
        if want == got {
            evidence.goal_kind_match = 2;
        }
    }
    evidence.modality_match = 2;
    evidence.total = evidence.goal_data_exact
        + evidence.goal_data_subtype
        + evidence.goal_format_match
        + evidence.project_class_match
        + evidence.modality_match
        + evidence.goal_kind_match;

    Some(ArchetypeSeed {
        composition: CompositionResult {
            matched_archetype: Some(archetype.id.clone()),
            match_score: top_score,
            atoms: composed,
            goal: goal.clone(),
            rationale: archetype.sme_summary.clone(),
            atom_rationales,
            resource_estimate,
        },
        evidence,
    })
}

/// Lift a v3 `CompositionResult` into the typed `WorkflowDag` shape.
/// Each `ComposedAtom` becomes a `TaskNode` (via `from_atom`); each
/// `depends_on` entry becomes an `EdgeContract` carrying a
/// `CompatibilityProof` produced by the `CompatibilityEngine`.
///
/// When an atom declares rich `inputs:` / `outputs:` (the typed-port
/// shape), use those directly on the lifted `TaskNode` instead of the
/// `from_atom` synthesis path that projects `atom.edam_data` into a
/// single placeholder port. The synthesis path otherwise emits a
/// placeholder shape that doesn't match the rich-port declarations
/// the YAML carries, leaving every archetype-lifted edge with a
/// `SemanticTypeMismatch` warning that flips
/// `score.required_contract_unsatisfied` to `Reject`. Using the rich
/// ports directly lets the archetype seed score cleanly and surface
/// as primary when its match evidence is definitive.
pub fn lift_to_workflow_dag(
    result: &CompositionResult,
    ctx: &PlanningContext,
    _goal: &GoalSpec,
) -> WorkflowDag {
    let mut nodes: Vec<TaskNode> = Vec::with_capacity(result.atoms.len());
    let mut node_ports: BTreeMap<String, (Vec<PortContract>, Vec<PortContract>)> = BTreeMap::new();
    for c in &result.atoms {
        let mut node = TaskNode::from_atom(&c.atom);
        node.id = c.stage_id.to_string();
        node.machine_name = c.stage_id.to_string();
        // Prefer the atom's rich port specs over the
        // legacy `edam_data`-synthesized placeholders. The rich ports
        // are what the meet-in-the-middle search uses; the archetype
        // lift path needs to use the same shapes so its edges' proofs
        // reflect real port-typed compatibility, not the legacy
        // single-port fallback.
        if !c.atom.inputs.is_empty() {
            node.inputs = c.atom.inputs.clone();
        }
        if !c.atom.outputs.is_empty() {
            node.outputs = c.atom.outputs.clone();
        }
        // Stage id remembered separately so re-lowering is byte-stable.
        node.attributes.insert(
            "stage_id".into(),
            serde_json::Value::String(c.stage_id.to_string()),
        );
        // Also remember the underlying atom id so
        // the lowering pass (`lower_dag_to_composition_result`) can
        // recover the registry atom even when the stage_id has been
        // aliased (e.g. cross-omics archetypes alias `differential_
        // expression` to `rnaseq_differential_expression` and
        // `proteomics_differential_abundance`). Without this, the
        // lowering's `atom_reg.iter().find(a.id == node.id)` falls
        // through to the placeholder atom (empty outputs), and
        // `validate_composition` returns `GoalUnreachable` because
        // none of the placeholder outputs match the goal.
        node.attributes.insert(
            "atom_id".into(),
            serde_json::Value::String(c.atom.id.clone()),
        );
        // The figures contract (per scripts/agent-prompts/task-execution.md
        // §Figures) needs `required_figures` + `plot_stage_id` to reach
        // the agent through `task-spec.json`. Without these the agent has
        // no way to know what figures to render and skips the plotting
        // step entirely. Stash them on attributes so the lowering pass
        // (`lower_task` in backend_emitters/workflow_json.rs) can fold
        // them into `Task.spec`.
        if !c.atom.required_figures.is_empty() {
            node.attributes.insert(
                "required_figures".into(),
                serde_json::Value::Array(
                    c.atom
                        .required_figures
                        .iter()
                        .map(|s| serde_json::Value::String(s.clone()))
                        .collect(),
                ),
            );
        }
        if let Some(plot_stage_id) = &c.atom.plot_stage_id {
            node.attributes.insert(
                "plot_stage_id".into(),
                serde_json::Value::String(plot_stage_id.clone()),
            );
        }
        if !c.atom.expected_artifacts.is_empty() {
            node.attributes.insert(
                "expected_artifacts".into(),
                serde_json::Value::Array(
                    c.atom
                        .expected_artifacts
                        .iter()
                        .map(|s| serde_json::Value::String(s.clone()))
                        .collect(),
                ),
            );
        }
        // Resolve container per the standard precedence.
        if let Some(container) = &c.container {
            node.implementation =
                crate::workflow_contracts::implementation::Implementation::ContainerCommand {
                    image: crate::workflow_contracts::implementation::OciImageRef {
                        image: container.image.clone(),
                        tag: container.tag.clone(),
                        digest: container.digest.clone(),
                        arch: if container.arch.is_empty() {
                            vec!["amd64".into()]
                        } else {
                            container.arch.clone()
                        },
                        gpu: container.gpu_required,
                    },
                    command_template: vec![],
                };
        }
        node_ports.insert(
            c.stage_id.to_string(),
            (node.inputs.clone(), node.outputs.clone()),
        );
        nodes.push(node);
    }

    // Build edges. For each consumer's depends_on, the producer is
    // the upstream stage.
    //
    // Find the best (producer-output, consumer-input)
    // port pair via the compatibility engine instead of blindly pairing
    // `outputs.first()` with `inputs.first()`. With rich multi-port
    // atoms (e.g. `data_acquisition` outputs both `raw_reads` AND
    // `cohort_manifest`; `cell_type_annotation` outputs cell-type
    // labels but DE's actual input is normalized_counts from upstream
    // `normalisation`), the legacy first-port pairing surfaced fake
    // `SemanticTypeMismatch` warnings on most archetype edges. The
    // archetype's `depends_on` is a workflow ordering relation, not a
    // data-flow relation — but at the typed-DAG level we need to
    // expose the data-flow shape, so we pick the compatible port pair
    // when one exists. When no producer→consumer port pair proves
    // Compatible / CompatibleWithAdapters, fall back to the first
    // ports (carrying the proof's warnings forward) so the lifted DAG
    // still has an edge slot for the SME's review surface.
    let engine = DeterministicCompatibilityEngine::new();
    let mut edges: Vec<EdgeContract> = Vec::new();
    let stage_ids: BTreeSet<&str> = result.atoms.iter().map(|c| c.stage_id.as_str()).collect();
    for c in &result.atoms {
        for dep in &c.depends_on {
            // Resolve dep id → stage id (atom id might match either
            // a stage_id or a sibling atom id when archetype aliases
            // are used).
            let producer_stage = if stage_ids.contains(dep.as_str()) {
                dep.clone()
            } else if let Some(sibling) = result.atoms.iter().find(|x| x.atom.id == *dep) {
                sibling.stage_id.to_string()
            } else {
                continue; // intake-supplied input
            };

            let (producer_outputs, _producer_inputs_unused) = node_ports
                .get(&producer_stage)
                .map(|(i, o)| (o.clone(), i.clone()))
                .unwrap_or_default();
            let (consumer_inputs, _consumer_outputs_unused) = node_ports
                .get(c.stage_id.as_str())
                .map(|(i, o)| (i.clone(), o.clone()))
                .unwrap_or_default();

            let (producer_port, consumer_port, proof) =
                pick_best_port_pair(&engine, &producer_outputs, &consumer_inputs);

            edges.push(EdgeContract {
                from_node: producer_stage,
                from_port: producer_port.name.clone(),
                to_node: c.stage_id.to_string(),
                to_port: consumer_port.name.clone(),
                proof,
                chain_of_custody: None,
            });
        }
    }

    // Stable edge order (sorted by from→to).
    edges.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.from_port.cmp(&b.from_port))
            .then_with(|| a.to_node.cmp(&b.to_node))
            .then_with(|| a.to_port.cmp(&b.to_port))
    });

    // Record a RegistryDefault assumption for every atom added without
    // an explicit SME configuration override. "Default taken" is the
    // atom's primary output semantic type when authored, or "registry_default"
    // for atoms whose output list is empty (intake/acquisition stages).
    //
    // Resolution is `Accepted` — not `Unresolved` — because registry-default
    // atoms are authoritative best-practice selections from the archetype or
    // search path, not pending SME decisions. Marking them `Unresolved` would
    // cause `score_dag` to count them and degrade the outcome from
    // `ValidatedExecutableDag` to `DraftDag` with 0 blockers, breaking the
    // `tier-8-3` compose-twice byte-reproducibility gate and preventing any
    // intake from succeeding. The ledger entry still exists for auditability;
    // only the resolution state is changed to reflect that no action is needed.
    let mut assumptions = AssumptionLedger::default();
    for c in &result.atoms {
        let default_taken = c
            .atom
            .outputs
            .first()
            .map(|p| p.semantic_type.stable_id())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "registry_default".to_string());
        assumptions.push(Assumption {
            id: format!("registry_default:{}", c.stage_id),
            statement: format!(
                "Atom {} added via registry default; no user-specified configuration override.",
                c.stage_id
            ),
            source: AssumptionSource::RegistryDefault {
                atom_id: c.atom.id.as_str().into(),
                default_taken,
            },
            affects_nodes: vec![c.stage_id.to_string()],
            risk: crate::workflow_contracts::evidence::RiskClass::Negligible,
            resolution: crate::workflow_contracts::evidence::AssumptionResolution::Accepted {
                rationale: "Registry-default atom selection; no SME override required.".into(),
            },
            chain_of_custody: None,
        });
    }

    WorkflowDag {
        id: format!("composed:{}", ctx.intent.id),
        nodes,
        edges,
        assumptions,
        source_template: result.matched_archetype.clone(),
    }
}

/// Find the best (producer-output, consumer-input)
/// port pair via the compatibility engine. Multi-port atoms are common
/// (e.g. `data_acquisition` emits both raw FASTQ reads AND a cohort
/// manifest; `cell_type_annotation`
/// emits typed celltype labels but the workflow ordering edge into
/// `differential_expression` is for *workflow ordering* — DE actually
/// consumes normalized counts from the upstream `normalisation` atom).
///
/// Strategy:
///
/// 1. Iterate every (output, input) pair and call `engine.prove`.
/// 2. Return the first `Compatible` pair (lossless proof).
/// 3. If no lossless pair, return the first `CompatibleWithAdapters`
///    pair (lossy proof; risky-adapter audit handled by the scorer).
/// 4. If no proof succeeds, fall back to the first-output/first-input
///    pair carrying the engine's diagnostic warning. This mirrors the
///    legacy single-port behavior so the lifted DAG still has an edge
///    slot for the SME's review surface even when the depends_on
///    relation doesn't reflect a data-flow port pairing.
///
/// Iteration order is deterministic (input then output index, sorted)
/// so the lift is byte-stable.
fn pick_best_port_pair(
    engine: &DeterministicCompatibilityEngine,
    producer_outputs: &[PortContract],
    consumer_inputs: &[PortContract],
) -> (PortContract, PortContract, CompatibilityProof) {
    let cctx = CompatibilityContext::default();
    let mut adapter_fallback: Option<(PortContract, PortContract, CompatibilityProof)> = None;
    for in_port in consumer_inputs {
        for out_port in producer_outputs {
            match engine.prove(out_port, in_port, &cctx) {
                CompatibilityResult::Compatible(proof) => {
                    return (out_port.clone(), in_port.clone(), proof);
                }
                CompatibilityResult::CompatibleWithAdapters {
                    mut proof,
                    adapters,
                } => {
                    if adapter_fallback.is_none() {
                        for a in &adapters {
                            proof.inserted_adapter_node_ids.push(a.id.clone());
                        }
                        adapter_fallback = Some((out_port.clone(), in_port.clone(), proof));
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(triple) = adapter_fallback {
        return triple;
    }
    // No compatible pair found — treat the depends_on as a workflow
    // ordering relation rather than a port-typed data-flow edge. This
    // is the common case for archetype-declared edges that exist for
    // sequencing reasons rather than data plumbing (e.g.
    // `differential_expression depends_on cell_type_annotation` in
    // `single_cell_de` is "DE runs after cell-type calling"; DE's
    // actual data input is the normalized counts from upstream
    // `normalisation`, satisfied via that atom's own depends_on edge).
    //
    // Surface the pair with the first-output / first-input names plus
    // a *non-`incompatible`* warning so `score_dag`'s
    // `required_contract_unsatisfied` check (which keys on the literal
    // substring "incompatible") doesn't escalate to `Reject`. The
    // archetype lift's depends_on is the canonical authoring surface
    // for ordering; we shouldn't reject the entire composition because
    // a workflow-ordering edge doesn't unify port types.
    let producer_port = producer_outputs.first().cloned().unwrap_or_default();
    let consumer_port = consumer_inputs.first().cloned().unwrap_or_default();
    let producer_type = {
        let stable = producer_port.semantic_type.stable_id();
        if stable.is_empty() {
            "ecaax:workflow_ordering".to_string()
        } else {
            stable
        }
    };
    let proof = CompatibilityProof {
        producer_type,
        consumer_type: consumer_port.semantic_type.stable_id(),
        warnings: vec![
            "workflow_ordering_edge: archetype depends_on without port-typed \
                        data flow"
                .into(),
        ],
        ..Default::default()
    };
    (producer_port, consumer_port, proof)
}

/// Score a `WorkflowDag` per the design's 16-component tuple.
fn score_dag(dag: &WorkflowDag, ctx: &PlanningContext) -> ScoringTuple {
    let mut s = ScoringTuple::default();
    // 1. Hard policy violation — slot is wired via PolicyContext.
    // Default is Pass.
    // 2. Required-contract-unsatisfied: every edge must have a
    // producer_type set.
    let any_unsat = dag.edges.iter().any(|e| {
        e.proof.producer_type.is_empty()
            || e.proof.warnings.iter().any(|w| w.contains("incompatible"))
    });
    if any_unsat {
        s.required_contract_unsatisfied = ScoringValue::Reject;
    }
    // 3. User-constraint violation — read intent.constraints. Default
    // is Pass.
    // 4. Scientific appropriateness penalty.
    s.scientific_appropriateness_penalty = 0;
    // 5. Untrusted node count (lower-is-better).
    let production_count: u32 = dag
        .nodes
        .iter()
        .filter(|n| matches!(n.lifecycle_state, LifecycleState::Production))
        .count() as u32;
    let total_nodes = dag.nodes.len() as u32;
    s.untrusted_node_count = total_nodes.saturating_sub(production_count);
    // 6. Unresolved blocking-assumption count.
    s.unresolved_assumptions = dag
        .assumptions
        .entries
        .iter()
        .filter(|a| {
            matches!(
                a.resolution,
                crate::workflow_contracts::evidence::AssumptionResolution::Unresolved
            )
        })
        .count() as u32;
    // 7. Risky adapter count + 8. Total adapter count.
    //
    // Typed adapter lookup via `AdapterRegistry`
    // (replacing the prior id-prefix heuristic). Both `dag.nodes`
    // (adapters lifted into the DAG by the meet path) and every
    // edge's `proof.inserted_adapter_node_ids` (the lift-from-
    // CompositionResult path) are scanned, deduped by id so an
    // adapter that appears as both a node AND a proof entry is
    // counted once. Risky = `AdapterSafety::ScientificallyRisky` or
    // `PolicyRestricted`.
    let adapter_reg = crate::adapter_registry::AdapterRegistry::with_starters();
    let mut adapter_ids: BTreeSet<&str> = BTreeSet::new();
    for n in &dag.nodes {
        if adapter_reg.get(&n.id).is_some() {
            adapter_ids.insert(n.id.as_str());
        }
    }
    for edge in &dag.edges {
        for adapter_id in &edge.proof.inserted_adapter_node_ids {
            if adapter_reg.get(adapter_id).is_some() {
                adapter_ids.insert(adapter_id.as_str());
            }
        }
    }
    let total_adapters = adapter_ids.len() as u32;
    let risky = adapter_ids
        .iter()
        .filter_map(|id| adapter_reg.get(id))
        .filter(|spec| {
            matches!(
                spec.safety,
                crate::adapter_registry::AdapterSafety::ScientificallyRisky
                    | crate::adapter_registry::AdapterSafety::PolicyRestricted
            )
        })
        .count() as u32;
    s.risky_adapter_count = risky;
    s.total_adapter_count = total_adapters;
    // 9. Validation coverage penalty: fewer validators per node = more
    // penalty.
    let total_validators: u32 = dag.nodes.iter().map(|n| n.validators.len() as u32).sum();
    s.validation_coverage_penalty = total_nodes.saturating_sub(total_validators.min(total_nodes));
    // 10. Evidence quality penalty: nodes with empty evidence count.
    s.evidence_quality_penalty = dag
        .nodes
        .iter()
        .filter(|n| n.evidence.passed_validators.is_empty() && n.evidence.benchmarks.is_empty())
        .count() as u32;
    // 11. Reproducibility: penalize nodes lacking pinned container
    // digests.
    s.reproducibility_penalty = dag
        .nodes
        .iter()
        .filter(|n| match &n.implementation {
            crate::workflow_contracts::implementation::Implementation::ContainerCommand {
                image,
                ..
            } => image.digest.is_empty(),
            _ => true,
        })
        .count() as u32;
    // 12. Explainability: penalize Opaque semantic types.
    s.explainability_penalty = dag
        .nodes
        .iter()
        .flat_map(|n| n.inputs.iter().chain(n.outputs.iter()))
        .filter(|p| {
            matches!(
                p.semantic_type,
                crate::workflow_contracts::semantic_type::SemanticType::Opaque { .. }
            )
        })
        .count() as u32;
    // 13. Backend availability — single-emitter path always available.
    s.backend_availability_penalty = 0;
    // 14. Runtime cost estimate.
    s.runtime_cost_estimate = dag.nodes.len() as u32;
    // 15. Data movement cost — coarse estimate via edge count.
    s.data_movement_cost = dag.edges.len() as u32;
    // 16. Stable lexical tie-breaker.
    let mut ids: Vec<String> = dag.nodes.iter().map(|n| n.id.clone()).collect();
    ids.sort();
    s.stable_lexical_id = ids.join(":");
    // Planning context's policy mode could flip hard_policy_violation
    // when wired; today the computation ships and the policy decisions
    // are wired in composer_v4::policy_gate.
    let _ = ctx;
    s
}

/// Same as the simple classify helper but also sweeps every
/// `GeneratedCode` node through the `SandboxPolicy` refusal check before
/// the composition is accepted as a `ValidatedExecutableDag`. Any node
/// that fails the check produces a `Refusal` so the planner doesn't lower
/// an unsafe node into the dispatch queue.
///
/// Refusals are recorded as structured `policy_decisions` strings on the
/// compatibility proof of any edge that touches the refused node, and
/// surfaced in the `RefusalReport.references` list (one entry per
/// refused node id) so the conversation layer can address them.
fn classify_outcome_with_sandbox(
    dag: &WorkflowDag,
    score: &ScoringTuple,
    sandbox_policy: &SandboxPolicy,
    ctx: &PlanningContext,
) -> ComposeOutcome {
    // Run the sandbox refusal sweep over every GeneratedCode node.
    let mut refused_nodes: Vec<String> = Vec::new();
    let mut refusal_reasons: Vec<String> = Vec::new();
    for node in &dag.nodes {
        let refusals = check_generated_code_node(node, sandbox_policy);
        if !refusals.is_empty() {
            refused_nodes.push(node.id.clone());
            for r in &refusals {
                refusal_reasons.push(format!("sandbox:{}:{:?}", node.id, r));
            }
        }
    }

    if !refused_nodes.is_empty() {
        // One or more GeneratedCode nodes failed the sandbox policy check.
        // Downgrade to Refusal so the SME sees the issue before any
        // dispatch work begins.
        //
        // v4 P4 / F21 — `SandboxRefused { category }` carries the v4 P7
        // category axis. Pick the first refusal's category as the
        // primary; the reasons list still enumerates every refusal so
        // the UI can surface the full set. `SupplyChain` is the
        // conservative default when no nodes surfaced a categorized
        // refusal (shouldn't happen given the sweep above produced at
        // least one entry per node).
        let primary_category = dag
            .nodes
            .iter()
            .find(|n| refused_nodes.contains(&n.id))
            .and_then(|n| {
                check_generated_code_node(n, sandbox_policy)
                    .first()
                    .map(|r| r.category())
            })
            .unwrap_or(SandboxRefusalCategory::SupplyChain);
        let kind = RefusalKind::SandboxRefused {
            category: primary_category,
        };
        let unblock_paths = synthesize_unblock_paths(dag, ctx, &kind);
        return ComposeOutcome::Refusal {
            report: crate::workflow_contracts::outcome::RefusalReport {
                id: "sandbox_generated_code_refusal".into(),
                kind,
                statement: format!(
                    "{} GeneratedCode node(s) refused under sandbox policy: {}",
                    refused_nodes.len(),
                    refusal_reasons.join("; ")
                ),
                references: refused_nodes,
                unblock_paths,
            },
        };
    }

    // No sandbox refusals — proceed with the normal policy + scoring logic.
    classify_outcome_with_policy(dag, score, &PolicyContext::empty(), ctx)
}

/// Same as `classify_outcome` but consults the per-node policy gate
/// and the assumption-policy table (F12).
///
/// Blocking policy violations escalate to `Refusal`; recorded
/// decisions surface as DraftDag warnings.
///
/// The assumption-policy lookup is a **discrete labelled section** so
/// the verifier substrate can wrap each `is_blocking` call with
/// `decision_substrate::record(VerifierDecision::AssumptionPolicyConsulted {... })`
/// without re-architecting the function.
pub fn classify_outcome_with_policy(
    dag: &WorkflowDag,
    score: &ScoringTuple,
    policy: &PolicyContext,
    ctx: &PlanningContext,
) -> ComposeOutcome {
    let policy_eval = policy_gate::evaluate(policy, dag);

    if policy_eval.has_blocking_violations() {
        // Per-node policy violations are clinical-tier refusals
        // (CompilerCi / DownstreamPolicy bundles). These are surfaced
        // with `ClinicalGateFailed` so the SME's UI dispatches the
        // clinical-recovery card (review policy + branch session).
        let kind = RefusalKind::ClinicalGateFailed;
        let unblock_paths = synthesize_unblock_paths(dag, ctx, &kind);
        return ComposeOutcome::Refusal {
            report: crate::workflow_contracts::outcome::RefusalReport {
                id: "policy_refusal".into(),
                kind,
                statement: format!(
                    "{} per-node policy violation(s): {}",
                    policy_eval.violations.len(),
                    policy_eval
                        .violations
                        .iter()
                        .take(3)
                        .map(|v| v.statement.clone())
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
                references: policy_eval
                    .violations
                    .iter()
                    .map(|v| v.node_id.clone())
                    .collect(),
                unblock_paths,
            },
        };
    }
    if matches!(score.hard_policy_violation, ScoringValue::Reject) {
        // The scoring tuple's hard-policy gate is the unconditional
        // refusal axis: the only recovery is branching the session
        // and relaxing the policy, so `HardPolicyViolation` (the one
        // kind that permits empty unblock_paths) is appropriate.
        let kind = RefusalKind::HardPolicyViolation;
        let unblock_paths = synthesize_unblock_paths(dag, ctx, &kind);
        return ComposeOutcome::Refusal {
            report: crate::workflow_contracts::outcome::RefusalReport {
                id: "policy_refusal".into(),
                kind,
                statement: "hard policy gate refused composition".into(),
                references: Vec::new(),
                unblock_paths,
            },
        };
    }
    if matches!(score.required_contract_unsatisfied, ScoringValue::Reject) {
        return ComposeOutcome::PartialDag {
            dag: dag.clone(),
            unresolved_gaps: vec![GapReport {
                id: "incompatible_edge".into(),
                statement: "one or more edges failed compatibility proving".into(),
                missing_port: None,
                suggestions: vec![
                    "Add an adapter, or change the producer to match the consumer's port shape"
                        .into(),
                ],
            }],
        };
    }
    // ---- v3 P3 + v4 P3 F11/F19 — promotion-gate refusal -----------
    //
    // Closes F11 (production-only refusal) + F19 (config-driven
    // promotion grid; ad-hoc promotion logic in code forbidden).
    //
    // For every node in the candidate DAG, consult the v4 P3 grid
    // (`PlanningContext.promotion_gate` ← `config/promotion-gate-policy.yaml`).
    // When at least one node fails the grid for its declared
    // `lifecycle_state`, refuse the lift with
    // `RefusalKind::PromotionRefused` — `unblock_paths` enumerates
    // the per-credential recovery affordances synthesized via
    // `synthesize_unblock_paths`.
    //
    // When `ctx.promotion_gate` is `None` (back-compat), every node's
    // `consult_promotion_gate` short-circuits to `Allow`; the F11 block
    // emits no refusal and the legacy path runs unchanged.
    //
    // This block is intentionally discrete so v4 P2's verifier
    // substrate captures every `PromotionGateConsulted` row via
    // `policy_gate::consult_promotion_gate` (one row per (node, target)
    // pair).
    if ctx.promotion_gate.is_some() {
        let mut non_promotable: Vec<String> = Vec::new();
        let mut summaries: Vec<String> = Vec::new();
        for node in &dag.nodes {
            let target = node.lifecycle_state;
            let decision = policy_gate::consult_promotion_gate(node, target, ctx);
            if let crate::promotion_gate_policy::PromotionDecision::Deny {
                missing_classes,
                missing_approvals,
            } = decision
            {
                non_promotable.push(node.id.clone());
                let mut parts: Vec<String> = Vec::new();
                if !missing_classes.is_empty() {
                    parts.push(format!("missing classes: {}", missing_classes.join(", ")));
                }
                if !missing_approvals.is_empty() {
                    parts.push(format!(
                        "missing approvals: {}",
                        missing_approvals.join(", ")
                    ));
                }
                summaries.push(format!(
                    "node `{}` -> {}: {}",
                    node.id,
                    target.canonical_name(),
                    parts.join("; ")
                ));
            }
        }
        if !non_promotable.is_empty() {
            let kind = RefusalKind::PromotionRefused;
            let unblock_paths = synthesize_unblock_paths(dag, ctx, &kind);
            let missing_summary = summaries.join(" | ");
            return ComposeOutcome::Refusal {
                report: crate::workflow_contracts::outcome::RefusalReport::promotion_refused(
                    non_promotable,
                    missing_summary,
                    unblock_paths,
                ),
            };
        }
    }
    // ---- End v3 P3 + v4 P3 F11/F19 promotion-gate refusal --------

    // ---- v3 P9 §11.X — population-coverage gate -------------------
    //
    // Runs AFTER F11 promotion-refusal (v3 P3∪v4 P3) and BEFORE the
    // assumption-policy lookup. The check fires when:
    // 1. an active `PolicyBundle` declares this session clinical
    // (signalled by `ValidatedNodesOnly`, the canonical clinical
    // gate check), AND
    // 2. `WorkflowIntent.sample_cohort` is set, AND
    // 3. `population_coverage_dir` is set in the planning context.
    //
    // For each source archetype id in the composed DAG, the gate
    // loads `<dir>/<archetype_id>.yaml`. When the file exists and the
    // sample cohort isn't covered AND no waiver in scope, the gate
    // emits a typed `RefusalKind::PopulationOutOfCoverage` outcome.
    //
    // Framing constraint (v3 §11.X): this gate runs against the
    // workflow's coverage statement, not the user's identity. The
    // sample's population is metadata about workflow applicability,
    // not access-control on who's allowed to dispatch.
    if let Some(coverage_dir) = ctx.population_coverage_dir.as_ref() {
        if let Some(sample_cohort) = ctx.intent.sample_cohort.clone() {
            let clinical_session =
                policy.requires_check(crate::policy_context::PolicyCheckKind::ValidatedNodesOnly);
            if clinical_session {
                if let Some(refusal) =
                    check_population_coverage(coverage_dir, dag, &sample_cohort, policy)
                {
                    return ComposeOutcome::Refusal { report: refusal };
                }
            }
        }
    }
    // ---- End v3 P9 §11.X population-coverage gate -----------------

    // ---- v3 assumption-policy table consult --------------
    //
    // Closes F12. When the planning context carries a loaded
    // `AssumptionPolicyTable`, each unresolved (non-waived) assumption
    // is keyed by `(defect_class × privacy_class)` and looked up in the
    // table. A `Blocking` resolution emits a `DraftDag` with a typed
    // `BlockerContext::policy_blocked` payload so the SME's recovery
    // surface (BlockerCard) knows it's a policy block, not a generic
    // scoring penalty.
    //
    // This block is intentionally discrete so v4 P2's verifier
    // substrate can wrap `assumption_policy.is_blocking(...)` with
    // `decision_substrate::record(VerifierDecision::AssumptionPolicyConsulted {.. })`
    // without re-architecting `classify_outcome_with_policy`.
    if let Some(assumption_policy) = ctx.assumption_policy.as_ref() {
        let privacy = privacy_class_of_dag(dag);
        let mut blockers: Vec<BlockerContext> = Vec::new();
        for assumption in &dag.assumptions.entries {
            if is_waived(assumption) {
                continue;
            }
            if !matches!(assumption.resolution, AssumptionResolution::Unresolved) {
                continue;
            }
            let defect = defect_class_of(assumption);
            let blocking = assumption_policy.is_blocking(defect, privacy);
            // v4 P2 / F18 — record every policy-table consult so the
            // substrate captures the (defect × privacy) → resolution
            // mapping the planner used. `rule_id` is a deterministic
            // string built from the lookup key so replay tests can
            // cross-reference the same row across re-emissions.
            let resolution = if blocking { "blocking" } else { "non_blocking" };
            let rule_id = format!("assumption_policy:{defect:?}:{privacy:?}");
            crate::decision_substrate::record(
                crate::decision_substrate::VerifierDecision::AssumptionPolicyConsulted {
                    id: crate::decision_substrate::stable_id(
                        "policy",
                        &format!("{defect:?}"),
                        &format!("{privacy:?}"),
                    ),
                    timestamp: crate::decision_substrate::timestamp(),
                    defect_class: format!("{defect:?}"),
                    privacy_class: format!("{privacy:?}"),
                    resolution: resolution.to_string(),
                    rule_id,
                },
            );
            if blocking {
                blockers.push(BlockerContext {
                    id: format!("policy_blocked:{}", assumption.id),
                    kind: "policy_blocked".into(),
                    statement: format!(
                        "Assumption {} is blocking per policy table (defect={:?}, privacy={:?})",
                        assumption.id, defect, privacy
                    ),
                    suggested_action: Some("waive_assumption_with_credentials".into()),
                    affected_nodes: assumption.affects_nodes.clone(),
                });
            }
        }
        if !blockers.is_empty() {
            return ComposeOutcome::DraftDag {
                dag: dag.clone(),
                assumptions: dag.assumptions.clone(),
                blockers,
            };
        }
    }
    // ---- End assumption-policy table consult ----------

    if score.unresolved_assumptions > 0 || score.risky_adapter_count > 0 {
        return ComposeOutcome::DraftDag {
            dag: dag.clone(),
            assumptions: dag.assumptions.clone(),
            blockers: Vec::new(),
        };
    }
    ComposeOutcome::ValidatedExecutableDag {
        dag: dag.clone(),
        report: ValidationReport::default(),
    }
}

/// Map an `AssumptionSource` to a defect class for the
/// assumption-policy table lookup. The substrate emission records
/// each call so the lookup mapping is replayable.
///
/// Mappings:
/// - `OntologyMappingUnresolved` → `OntologyMappingUnresolved`
/// - `LossyAdapter` → `LossyAdapterDefault`
/// - `ProfilerDegraded` → `SampleMetadataMissing` (the canonical
///   profiler-degraded reason: missing metadata to infer the field)
/// - `SmeAccepted` → `SampleMetadataMissing` (SME accepted a default
///   in lieu of authoritative metadata)
/// - `LlmInferred` → `SampleMetadataMissing` (LLM filled in a gap
///   from intake context)
/// - `PolicyException` → `PolicyRestrictedAdapter` (explicit
///   exception => restricted-adapter equivalent for the table)
///
/// Conservative default for unmapped sources: `SampleMetadataMissing`
/// — the mapping can be tuned per source as the audit-log corpus
/// accumulates.
fn defect_class_of(a: &Assumption) -> PolicyDefectClass {
    match &a.source {
        AssumptionSource::OntologyMappingUnresolved { .. } => {
            PolicyDefectClass::OntologyMappingUnresolved
        }
        AssumptionSource::LossyAdapter { .. } => PolicyDefectClass::LossyAdapterDefault,
        AssumptionSource::ProfilerDegraded { .. } => PolicyDefectClass::SampleMetadataMissing,
        AssumptionSource::SmeAccepted { .. } => PolicyDefectClass::SampleMetadataMissing,
        AssumptionSource::LlmInferred { .. } => PolicyDefectClass::SampleMetadataMissing,
        AssumptionSource::PolicyException { .. } => PolicyDefectClass::PolicyRestrictedAdapter,
        // Registry defaults, adapter insertions, and seed heuristics are
        // informational; map to the nearest existing defect class so the
        // policy-table consult still fires for any unresolved entry.
        AssumptionSource::RegistryDefault { .. } => PolicyDefectClass::SampleMetadataMissing,
        AssumptionSource::OntologyAdapterInserted { .. } => PolicyDefectClass::LossyAdapterDefault,
        AssumptionSource::SeedHeuristic { .. } => PolicyDefectClass::SampleMetadataMissing,
    }
}

/// V3 derive the policy-table privacy class for a DAG.
/// Picks the most restrictive port-level privacy class across every
/// node's inputs/outputs, then maps the in-tree `PortPrivacyClass`
/// taxonomy onto the assumption-policy's coarser
/// `(phi | controlled_access | research | public)` taxonomy.
///
/// In-tree → policy-table mapping:
/// - `Phi` → `Phi`
/// - `Restricted` → `ControlledAccess` (closest analog)
/// - `Sensitive` → `Research` (sensitive but not regulated)
/// - `Internal` → `Research`
/// - `Public` → `Public`
fn privacy_class_of_dag(dag: &WorkflowDag) -> PolicyPrivacyClass {
    let mut worst = PolicyPrivacyClass::Public;
    let rank = |c: PolicyPrivacyClass| {
        // Lower index = more restrictive. We want the most-restrictive
        // (smallest index) to win.
        match c {
            PolicyPrivacyClass::Phi => 0,
            PolicyPrivacyClass::ControlledAccess => 1,
            PolicyPrivacyClass::Research => 2,
            PolicyPrivacyClass::Public => 3,
        }
    };
    for node in &dag.nodes {
        for port in node.inputs.iter().chain(node.outputs.iter()) {
            let mapped = match port.privacy_class {
                PortPrivacyClass::Phi => PolicyPrivacyClass::Phi,
                PortPrivacyClass::Restricted => PolicyPrivacyClass::ControlledAccess,
                PortPrivacyClass::Sensitive => PolicyPrivacyClass::Research,
                PortPrivacyClass::Internal => PolicyPrivacyClass::Research,
                PortPrivacyClass::Public => PolicyPrivacyClass::Public,
            };
            if rank(mapped) < rank(worst) {
                worst = mapped;
            }
        }
    }
    worst
}

/// V3 an assumption is "waived" if its resolution is the
/// `WaivedWithRisk` variant. The policy-table consult must not block
/// on a waived assumption; the waiver itself is the recorded
/// resolution path.
fn is_waived(a: &Assumption) -> bool {
    matches!(a.resolution, AssumptionResolution::WaivedWithRisk { .. })
}

/// Synthesize the unblock-paths vector for a refusal of the given
/// `RefusalKind`. Centralized so every `ComposeOutcome::Refusal`
/// emission site populates a deterministic recovery affordance set.
///
/// Sources:
/// - **Unresolved assumptions** → `UnblockPath::ResolveAssumption`
///   (one per assumption tagged blocking by the policy table, or all
///   of them when no policy table is loaded).
/// - **Policy-table rules with `waivable_with_credentials` resolution
///   matching the refusal context** → `UnblockPath::Waiver`.
/// - **Repair strategies** → `UnblockPath::AttemptRepair` is wired
///   via the repair-strategy registry.
///
/// Hard-policy kinds get an empty vec by design — there's no
/// actionable recovery short of branching the session.
pub(crate) fn synthesize_unblock_paths(
    dag: &WorkflowDag,
    ctx: &PlanningContext,
    kind: &RefusalKind,
) -> Vec<UnblockPath> {
    if kind.permits_no_unblock_paths() {
        return Vec::new();
    }

    let mut paths: Vec<UnblockPath> = Vec::new();

    // 1. ResolveAssumption — one path per unresolved (non-waived)
    // assumption flagged blocking by the policy table.
    let privacy = privacy_class_of_dag(dag);
    for assumption in &dag.assumptions.entries {
        if is_waived(assumption) {
            continue;
        }
        if !matches!(assumption.resolution, AssumptionResolution::Unresolved) {
            continue;
        }
        let defect = defect_class_of(assumption);
        let is_blocking = ctx
            .assumption_policy
            .as_ref()
            .map(|table| table.is_blocking(defect, privacy))
            .unwrap_or(true);
        if !is_blocking {
            continue;
        }
        paths.push(UnblockPath::ResolveAssumption {
            assumption_id: assumption.id.clone(),
            suggested_resolution: Some(assumption.statement.clone()),
            target_outcome: ProjectedOutcome::DraftDag,
        });
    }

    // 2. Waiver — one path per policy-table entry with
    // `waivable_with_credentials` matching the DAG's privacy class.
    if let Some(table) = ctx.assumption_policy.as_ref() {
        for entry in table.iter() {
            if !matches!(
                entry.resolution,
                crate::assumption_policy::ResolutionPolicy::WaivableWithCredentials
            ) {
                continue;
            }
            if entry.privacy_class != privacy {
                continue;
            }
            paths.push(UnblockPath::Waiver {
                rule_id: format!(
                    "assumption_policy:{:?}:{:?}",
                    entry.defect_class, entry.privacy_class
                ),
                required_credentials: entry.required_credentials.clone(),
                target_outcome: ProjectedOutcome::DraftDag,
            });
        }
    }

    // 3. Per-kind synthesized paths.
    match kind {
        RefusalKind::GoalUnderspecified => {
            paths.push(UnblockPath::SupplyMissingMetadata {
                field: "goal".into(),
                suggested_value: None,
                target_outcome: ProjectedOutcome::DraftDag,
            });
        }
        RefusalKind::PopulationOutOfCoverage {
            suggested_waiver_authority,
            ..
        } => {
            // v3 P9 §11.X — two recovery affordances: (1) amend the
            // sample-cohort metadata if it was wrong, (2) escalate to
            // the clinical lead for a `PopulationWaiver`.
            paths.push(UnblockPath::SupplyMissingMetadata {
                field: "population".into(),
                suggested_value: None,
                target_outcome: ProjectedOutcome::DraftDag,
            });
            paths.push(UnblockPath::EscalateToReviewer {
                reviewer_class: suggested_waiver_authority.clone(),
                required_artifacts: vec!["population_waiver_rationale".into()],
                target_outcome: ProjectedOutcome::DraftDag,
            });
        }
        RefusalKind::LicenseMissing => {
            paths.push(UnblockPath::SupplyMissingMetadata {
                field: "license_token".into(),
                suggested_value: None,
                target_outcome: ProjectedOutcome::DraftDag,
            });
        }
        RefusalKind::ClinicalGateFailed => {
            paths.push(UnblockPath::EscalateToReviewer {
                reviewer_class: "clinical_lead".into(),
                required_artifacts: vec!["IRB_approval".into(), "validated_reference".into()],
                target_outcome: ProjectedOutcome::DraftDag,
            });
        }
        RefusalKind::SemanticLossNotAuthorized => {
            paths.push(UnblockPath::EscalateToReviewer {
                reviewer_class: "domain_expert".into(),
                required_artifacts: vec!["semantic_loss_acknowledgement".into()],
                target_outcome: ProjectedOutcome::DraftDag,
            });
        }
        RefusalKind::SandboxRefused { .. } => {
            paths.push(UnblockPath::EscalateToReviewer {
                reviewer_class: "validation_engineer".into(),
                required_artifacts: vec!["sandbox_review_record".into()],
                target_outcome: ProjectedOutcome::DraftDag,
            });
        }
        RefusalKind::UncategorizedBlocker => {
            paths.push(UnblockPath::EscalateToReviewer {
                reviewer_class: "bioinformatics_lead".into(),
                required_artifacts: vec!["refusal_review".into()],
                target_outcome: ProjectedOutcome::DraftDag,
            });
        }
        RefusalKind::PromotionRefused => {
            // v3 P3 / v4 P3 F11 / F19 — promotion-grid refusal default
            // unblock path. `RefusalReport::promotion_refused()` populates
            // the per-credential approval rows directly; this arm is a
            // safety-net for callers that constructed the report by hand.
            paths.push(UnblockPath::EscalateToReviewer {
                reviewer_class: "domain_expert".into(),
                required_artifacts: vec!["promotion_approval".into()],
                target_outcome: ProjectedOutcome::DraftDag,
            });
        }
        RefusalKind::HardPolicyViolation
        | RefusalKind::PhiLeakBlocked
        | RefusalKind::PrivacyViolation => {}
    }

    // F21 fallback: every non-hard kind MUST carry at least one path.
    if paths.is_empty() {
        paths.push(UnblockPath::EscalateToReviewer {
            reviewer_class: "bioinformatics_lead".into(),
            required_artifacts: vec!["refusal_review".into()],
            target_outcome: ProjectedOutcome::DraftDag,
        });
    }

    paths
}

/// v3 P9 §11.X — run the population-coverage gate against the composed
/// DAG. Iterates source archetypes (deduped, in stable order), loads
/// `<coverage_dir>/<archetype_id>.yaml` for each, and emits a typed
/// `PopulationOutOfCoverage` refusal the first time the SME's sample
/// cohort fails to match a workflow's validated set AND no in-scope
/// waiver covers the gap.
///
/// Returns `None` when every archetype either lacks a coverage statement
/// (file missing — fall through to the rest of the planner) or covers
/// the sample cohort, or when a waiver in any active bundle covers the
/// workflow.
///
/// The "source archetype ids" today come from `WorkflowDag.source_template`
/// (the planner records the matched archetype id there when an archetype
/// seed produces the lifted DAG). Search-derived DAGs that have no
/// archetype seed return `None` — the gate short-circuits with no
/// coverage check, since there's no validated workflow to compare
/// against.
fn check_population_coverage(
    coverage_dir: &std::path::Path,
    dag: &WorkflowDag,
    sample_cohort: &crate::population_coverage::CohortDescriptor,
    policy: &PolicyContext,
) -> Option<crate::workflow_contracts::outcome::RefusalReport> {
    // De-duplicated archetype ids in stable insertion order.
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut archetype_ids: Vec<String> = Vec::new();
    if let Some(t) = dag.source_template.as_ref() {
        if seen.insert(t.clone()) {
            archetype_ids.push(t.clone());
        }
    }
    if archetype_ids.is_empty() {
        return None;
    }
    for archetype_id in &archetype_ids {
        let path = coverage_dir.join(format!("{archetype_id}.yaml"));
        let stmt =
            match crate::population_coverage::PopulationCoverageStatement::load_from_path(&path) {
                Ok(s) => s,
                Err(_) => continue, // missing / unreadable: no gate applies
            };
        if stmt.covers(sample_cohort) {
            continue;
        }
        if has_active_population_waiver(policy, archetype_id) {
            continue;
        }
        // Refusal: build the typed report with both unblock paths.
        return Some(
            crate::workflow_contracts::outcome::RefusalReport::population_out_of_coverage(
                archetype_id.clone(),
                sample_cohort.label.clone(),
                stmt.validated_cohorts
                    .iter()
                    .map(|c| c.label.clone())
                    .collect(),
                "clinical_lead",
            ),
        );
    }
    None
}

/// v3 P9 §11.X — true iff at least one active `PolicyBundle` carries a
/// `PopulationWaiver` for the given workflow id.
fn has_active_population_waiver(policy: &PolicyContext, workflow_id: &str) -> bool {
    policy.bundles.values().any(|b| {
        b.population_waivers
            .iter()
            .any(|w| w.workflow_id == workflow_id)
    })
}

/// v3 P9 §11.X — populate `ctx.population_coverage_dir` from a caller-
/// provided path. Idempotent: if the directory doesn't exist the
/// context is returned unchanged. The planner then short-circuits the
/// population-coverage gate to "no check."
pub fn planning_context_with_population_coverage(
    mut ctx: PlanningContext,
    population_coverage_dir: impl Into<std::path::PathBuf>,
) -> PlanningContext {
    let dir = population_coverage_dir.into();
    if dir.is_dir() {
        ctx.population_coverage_dir = Some(dir);
    }
    ctx
}

/// V3 helper to load a [`PlanningContext`] with an
/// assumption-policy table from `config_dir/assumption-policy.yaml`.
/// Idempotent; returns the context unchanged when the file doesn't
/// exist or fails to parse (the planner falls back to the legacy
/// "any unresolved blocks Production" behaviour in that case).
///
/// Wired by callers that build a `PlanningContext` at the dispatch
/// entry point: `compose_v4_dispatch_full` in `crates/core/src/composer.rs`,
/// the conversation layer's `try_build_via_composer`, etc.
pub fn planning_context_with_assumption_policy(
    mut ctx: PlanningContext,
    config_dir: &std::path::Path,
) -> PlanningContext {
    let path = config_dir.join("assumption-policy.yaml");
    if let Ok(table) = AssumptionPolicyTable::load_from_path(&path) {
        ctx.assumption_policy = Some(std::sync::Arc::new(table));
    }
    ctx
}

/// V4 populate `ctx.ontology_scope` from
/// `config/modality-ontology-coverage.yaml`.
///
/// Missing / malformed config is a soft failure: the planner falls
/// back to legacy behaviour (no scope checks). Wired by callers that
/// build a `PlanningContext` at the dispatch entry point.
pub fn planning_context_with_ontology_scope(
    mut ctx: PlanningContext,
    config_dir: &std::path::Path,
) -> PlanningContext {
    let path = config_dir.join("modality-ontology-coverage.yaml");
    if let Ok(matrix) = crate::ontology_scope::OntologyScopeMatrix::load_from_path(&path) {
        ctx.ontology_scope = Some(matrix);
    }
    ctx
}

/// One-line summary used in the UI alternative-comparison card.
fn summarize_dag(dag: &WorkflowDag, score: &ScoringTuple) -> String {
    format!(
        "{} nodes, {} edges, {} adapters ({} risky), {} unresolved assumptions",
        dag.nodes.len(),
        dag.edges.len(),
        score.total_adapter_count,
        score.risky_adapter_count,
        score.unresolved_assumptions
    )
}

/// Lower a `WorkflowDag` back into a `CompositionResult` so the
/// existing dispatch path (`compose_v4_dispatch_full`) can return an
/// executable composition. The lowering is the inverse of
/// `lift_to_workflow_dag` for `Task`-bearing fields; sidecar fields
/// (proofs, assumptions) are dropped in this direction (they survive
/// in `WorkflowDag` itself for the downstream emitter).
pub fn lower_dag_to_composition_result(
    dag: &WorkflowDag,
    atom_reg: &AtomRegistry,
    goal: &GoalSpec,
) -> Result<CompositionResult, CompositionError> {
    let mut atoms: Vec<ComposedAtom> = Vec::with_capacity(dag.nodes.len());
    let mut atom_rationales: BTreeMap<String, AtomSelectionRationale> = BTreeMap::new();
    for (step, node) in dag.nodes.iter().enumerate() {
        let stage_id = node
            .attributes
            .get("stage_id")
            .and_then(|v| v.as_str())
            .unwrap_or(&node.id)
            .to_string();
        // Honor the lift-pass-recorded `atom_id`
        // attribute first so namespaced archetypes (cross-omics)
        // recover their underlying registry atom even when the
        // stage_id has been aliased. Falls through to the legacy
        // by-stage_id / by-node.id lookup when the attribute isn't
        // present (search-seed and historical paths don't set it).
        let preserved_atom_id = node
            .attributes
            .get("atom_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let atom_id = preserved_atom_id
            .as_ref()
            .filter(|id| atom_reg.get(id).is_some())
            .cloned()
            .or_else(|| {
                atom_reg
                    .iter()
                    .find(|(_, a)| a.id == node.id || a.id == stage_id)
                    .map(|(_, a)| a.id.clone())
            })
            .unwrap_or_else(|| node.id.clone());
        let atom = atom_reg
            .get(&atom_id)
            .cloned()
            .unwrap_or_else(|| placeholder_atom(node));
        // depends_on: gather producers from edges.to_node == stage_id.
        let depends_on: Vec<String> = dag
            .edges
            .iter()
            .filter(|e| e.to_node == stage_id || e.to_node == node.id)
            .map(|e| e.from_node.clone())
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect();
        let container = resolve_task_container(&atom, None, None);
        atoms.push(ComposedAtom {
            stage_id: stage_id.clone().into(),
            atom: atom.clone(),
            depends_on,
            required: true,
            bindings: Vec::new(),
            container,
        });
        atom_rationales.insert(
            stage_id.clone(),
            AtomSelectionRationale {
                stage_id: StageId::from(stage_id.as_str()),
                atom_id: atom.id.as_str().into(),
                reason: SelectionReason::BackwardChainGoalProducer {
                    target_edam: goal.edam_data.clone(),
                    step: step as u32,
                },
                score: 0,
                explanation: format!(
                    "v4 planner included {} (step {}) to produce {} for the goal.",
                    atom.id, step, goal.edam_data
                ),
            },
        );
    }
    let resource_estimate = aggregate_resources(&atoms);
    Ok(CompositionResult {
        matched_archetype: dag.source_template.clone(),
        match_score: 0,
        atoms,
        goal: goal.clone(),
        rationale: format!(
            "v4 planner composed {} via {} atom(s); proof-carrying edges in WorkflowDag.",
            goal.edam_data,
            dag.nodes.len(),
        ),
        atom_rationales,
        resource_estimate,
    })
}

/// Construct a placeholder `AtomDefinition` for a TaskNode that has
/// no atom registry entry. Used by the lowering pass when the v4
/// planner inserted a hypothesized or generated node that wasn't in
/// the YAML library.
fn placeholder_atom(node: &TaskNode) -> AtomDefinition {
    AtomDefinition {
        id: node.id.clone(),
        version: node.version.render(),
        role: crate::atom::AtomRole::Operation,
        discovery_kind: None,
        description: node.intent.clone(),
        edam_operation: "ecaax:hypothesized".into(),
        edam_data: None,
        edam_format: None,
        assignee: crate::atom::AtomAssignee::Agent,
        depends_on: Vec::new(),
        excludes: Vec::new(),
        attributes: BTreeMap::new(),
        joint_with: Vec::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        method_choice: None,
        resource_profile: None,
        preferred_container: None,
        claim_boundary: None,
        iterate: None,
        condition: None,
        required_figures: Vec::new(),
        plot_stage_id: None,
        figure_exempt: None,
        expected_artifacts: Vec::new(),
        required_artifacts: Vec::new(),
        validators: Vec::new(),
        runtime_packages: crate::runtime_prereqs::RuntimePrereqs::default(),
        safety: crate::atom::SafetyPolicy::default(),
    }
}

/// Inverse of `score_dag` for ranking purposes. Public so external
/// rankers can recompute when DAG mutates.
pub fn rescore_dag(dag: &WorkflowDag, ctx: &PlanningContext) -> ScoringTuple {
    score_dag(dag, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archetype_registry::ArchetypeRegistry;
    use crate::atom_registry::AtomRegistry;

    fn empty_atom_registry() -> AtomRegistry {
        AtomRegistry::default()
    }

    fn empty_archetype_registry() -> ArchetypeRegistry {
        ArchetypeRegistry::default()
    }

    fn simple_goal() -> GoalSpec {
        GoalSpec {
            edam_data: "data:3917".into(),
            edam_format: Some("format:3475".into()),
            modifiers: BTreeMap::new(),
            source_prose: Some("count matrix".into()),
            confidence: 0.8,
        }
    }

    #[test]
    fn empty_registry_returns_partial_dag() {
        let ctx = planning_context_for_goal("test", &simple_goal());
        let result = plan(
            &ctx,
            &simple_goal(),
            "research",
            &empty_atom_registry(),
            &empty_archetype_registry(),
        );
        assert!(matches!(result.primary, ComposeOutcome::PartialDag { .. }));
        assert!(result.alternatives.is_empty());
    }

    #[test]
    fn rescore_dag_is_pure() {
        let ctx = planning_context_for_goal("test", &simple_goal());
        let dag = WorkflowDag::default();
        let s1 = rescore_dag(&dag, &ctx);
        let s2 = rescore_dag(&dag, &ctx);
        assert_eq!(s1, s2);
    }

    /// Planner must refuse a DAG that contains a `GeneratedCode`
    /// node failing the sandbox policy check (Unreviewed + default_strict
    /// requires static analysis ⇒ `StaticAnalysisRequired` refusal).
    #[test]
    fn planner_refuses_generated_code_under_strict_policy() {
        use crate::sandbox_policy::SandboxPolicy;
        use crate::workflow_contracts::implementation::{Implementation, ReviewStatus};

        let sandbox = SandboxPolicy::default_strict();

        // Build a minimal DAG with a single unreviewed GeneratedCode node.
        let mut unreviewed =
            crate::workflow_contracts::task_node::TaskNode::skeleton("gen_node", "Generated");
        unreviewed.implementation = Implementation::GeneratedCode {
            repository_ref: "git@example.com/repo".into(),
            review_status: ReviewStatus::Unreviewed,
            artifact_digest: None,
        };

        let dag = WorkflowDag {
            id: "test_gen".into(),
            nodes: vec![unreviewed],
            edges: Vec::new(),
            assumptions: Default::default(),
            source_template: None,
        };

        let score = ScoringTuple::default();
        let ctx = PlanningContext::default();
        let outcome = classify_outcome_with_sandbox(&dag, &score, &sandbox, &ctx);

        // The planner must refuse the composition.
        assert!(
            matches!(outcome, ComposeOutcome::Refusal { .. }),
            "expected Refusal for GeneratedCode node failing sandbox policy, got {:?}",
            outcome
        );

        // The refusal report must reference the refused node.
        if let ComposeOutcome::Refusal { report } = &outcome {
            assert!(
                report.references.contains(&"gen_node".to_string()),
                "refusal report must reference the refused node id, got: {:?}",
                report.references
            );
            // v4 P4 / F21 — kind is now `RefusalKind::SandboxRefused`.
            assert!(
                matches!(report.kind, RefusalKind::SandboxRefused { .. }),
                "expected SandboxRefused kind, got {:?}",
                report.kind
            );
            // F21 invariant: synthesized unblock_paths must be populated.
            assert!(
                !report.unblock_paths.is_empty(),
                "sandbox refusal must carry at least one unblock_path, got: {:?}",
                report.unblock_paths
            );
            assert!(report.validate().is_ok());
        }
    }

    /// Planner must NOT refuse a DAG with a `GeneratedCode` node
    /// that passes the sandbox policy (HumanReviewed = best case).
    #[test]
    fn planner_accepts_human_reviewed_generated_code() {
        use crate::sandbox_policy::SandboxPolicy;
        use crate::workflow_contracts::implementation::{Implementation, ReviewStatus};

        let sandbox = SandboxPolicy::default_strict();

        let mut reviewed =
            crate::workflow_contracts::task_node::TaskNode::skeleton("gen_ok", "Generated");
        reviewed.implementation = Implementation::GeneratedCode {
            repository_ref: "git@example.com/repo".into(),
            review_status: ReviewStatus::HumanReviewed,
            artifact_digest: Some("sha256:abc".into()),
        };

        let dag = WorkflowDag {
            id: "test_gen_ok".into(),
            nodes: vec![reviewed],
            edges: Vec::new(),
            assumptions: Default::default(),
            source_template: None,
        };

        let score = ScoringTuple::default();
        let ctx = PlanningContext::default();
        let outcome = classify_outcome_with_sandbox(&dag, &score, &sandbox, &ctx);

        // HumanReviewed passes — outcome should NOT be a Refusal.
        assert!(
            !matches!(outcome, ComposeOutcome::Refusal { .. }),
            "HumanReviewed GeneratedCode must NOT be refused, got {:?}",
            outcome
        );
    }
}
