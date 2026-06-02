//! Public-API dispatch entry points for the composer.
//!
//! Houses the `compose_*` entry points that callers (CLI `intake`,
//! the conversation crate's `try_build_via_composer`, and the
//! integration-test corpus) drive through. v4 (the proof-carrying
//! planner) is the only composer; all entry points funnel into
//! `compose_with_modalities_full` → `compose_v4_dispatch_full`.
//!
//! Routing:
//!
//! - `compose` is the zero-config entry point — delegates to
//!   `compose_with_modality(None)`.
//! - `compose_with_modality` lifts the optional modality slot to a
//!   single-element slice and calls `compose_with_modalities_full`,
//!   returning the lowered `CompositionResult`.
//! - `compose_with_modalities` is the multi-modality sibling; v4
//!   discovers cross-omics archetypes via the same
//!   `ArchetypeRegistry::find_match_cross_omics`.
//! - `compose_with_modalities_full` is the proof-carrying entry:
//!   returns the full `ComposerOutput` (composition + WorkflowDag +
//!   ranked alternatives + ComposeOutcome + per-node policy decisions)
//!   so the conversation crate can persist v4 sidecars at emit time.
//!   It also handles the atypical-shape / out-of-catalog fall-through
//!   to `generic_omics`.
//!
//! Internal helpers (`pub(super)`):
//!
//! - `compose_v4_dispatch_full` is the canonical v4 entry point;
//!   both single-modality and multi-modality dispatchers route
//!   here for v4 and either keep or discard sidecar fields based on
//!   their own return shape.
//!
//! Module-private helpers:
//!
//! - `collect_policy_decisions` reads the v4 planner's policy gate
//!   output and projects it onto the persisted `PolicyDecisionRecord`
//!   shape.
//! - `format_check_kind_str` renders a `PolicyCheckKind` for the
//!   persisted record's `kind` field.
//! - `seed_available_data_for_modalities` synthesizes a best-effort
//!   `WorkflowIntent.available_data` seed for the v4 forward search
//!   until a dataset profiler can thread real intake-derived
//!   contracts in.

use super::multi_modal::unique_modalities;
use super::validation::validate_composition;
use super::{ComposerOutput, CompositionError, CompositionResult, PolicyDecisionRecord};
use crate::archetype_registry::ArchetypeRegistry;
use crate::atom_registry::AtomRegistry;
use crate::goal_spec::GoalSpec;

/// Returns true when the classifier's modality + goal pairing is
/// atypical-enough that the modality archetype is likely missing
/// universal terminals (`raw_qc` + `generic_summary`). Triggered when
/// the goal's `kind` modifier names a flex-shape analysis (survival,
/// strain-SNP, scATAC-only) that doesn't map to a specific modality
/// archetype's atom set.
///
/// Used by `compose_with_version_and_modality` to override the modality
/// archetype with `generic_omics` so the universal terminals are
/// always present on atypical-shape emits.
fn requires_generic_fallthrough(goal: &GoalSpec, target_modality: Option<&str>) -> bool {
    const FLEX_KINDS: &[&str] = &[
        "survival_analysis",
        "cox_proportional_hazards",
        "kaplan_meier",
        "strain_resolution",
        "strain_snp",
        "scatac_only",
        // Catch-all for catalog-absent modalities the classifier's
        // out-of-catalog signature scan tagged (mass cytometry, MR,
        // cryo-EM, single-cell methylation, Slide-seq, CODEX, …).
        // Routes the prompt to `generic_omics` instead of letting it
        // misroute to the nearest keyword-similar archetype.
        "out_of_catalog",
    ];
    if let Some(kind) = goal.modifiers.get("kind") {
        if FLEX_KINDS.contains(&kind.as_str()) {
            return true;
        }
    }
    // Heuristic: scATAC-only (no companion modality, scATAC primary)
    // routes to single_cell_rnaseq archetype today but the SME wants
    // ATAC-specific outputs. Detect by checking modality_hint.
    if let Some(m) = target_modality {
        if m == "scatac_only" {
            return true;
        }
    }
    false
}

/// Entry point. Today's v1 composition pipeline:
///
/// 1. Score every archetype against the goal via
///    `ArchetypeRegistry::find_match`.
/// 2. If exactly one wins (or top wins by > 5% over runner-up),
///    proceed.
/// 3. If ≥ 2 tie at the top, return TieRequiresSmeDecision.
/// 4. If none match, return NoArchetypeMatch (the full impl would
///    fall through to backward-chain here).
/// 5. Resolve the archetype's atoms via the atom registry.
/// 6. Apply per-call wiring overrides (alias → stage_id;
///    depends_on overrides).
/// 7. Run the v1 exclusion-consistency check.
/// 8. Return CompositionResult.
///
/// Determinism: every collection is BTreeMap-ordered + atoms emit
/// in the archetype's declared order. Two calls with identical
/// inputs produce byte-identical CompositionResult.
pub fn compose(
    goal: &GoalSpec,
    project_class: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
) -> Result<CompositionResult, CompositionError> {
    // `compose()`
    // now routes to v4 (proof-carrying semantic). The previous v2
    // (archetype-fast-path) default is retired; archetype matching
    // survives as a v4 *seed* candidate via
    // `composer_v4::planner::try_archetype_seed`, so callers that
    // had a unique archetype winner under v2 still land on the same
    // composition under v4.
    compose_with_modality(goal, project_class, atom_reg, archetype_reg, None)
}

/// Single-modality compose entry. Delegates to [`compose_with_modalities_full`]
/// and returns its lowered `CompositionResult` (v4 is the only composer).
pub fn compose_with_modality(
    goal: &GoalSpec,
    project_class: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
    target_modality: Option<&str>,
) -> Result<CompositionResult, CompositionError> {
    let modalities: Vec<&str> = target_modality.into_iter().collect();
    compose_with_modalities_full(
        goal,
        project_class,
        atom_reg,
        archetype_reg,
        &modalities,
        None,
        None,
        None,
    )
    .map(|out| out.composition)
}

/// Multi-modality compose entry. Delegates to [`compose_with_modalities_full`]
/// and returns its lowered `CompositionResult`. v4 discovers cross-omics
/// archetypes via the same `ArchetypeRegistry::find_match_cross_omics`.
pub fn compose_with_modalities(
    goal: &GoalSpec,
    project_class: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
    target_modalities: &[&str],
) -> Result<CompositionResult, CompositionError> {
    compose_with_modalities_full(
        goal,
        project_class,
        atom_reg,
        archetype_reg,
        target_modalities,
        None,
        None,
        None,
    )
    .map(|out| out.composition)
}

/// V4 dispatch returning the full proof-carrying bundle.
///
/// The single canonical v4 dispatch entry point. Returns a
/// `ComposerOutput` carrying the legacy `CompositionResult` plus the
/// typed `WorkflowDag`, ranked alternatives, `ComposeOutcome`, and
/// per-node policy decisions so the chat session can cache them and
/// the emit pipeline can persist sidecars (`runtime/proofs.jsonl`,
/// `runtime/assumptions.jsonl`, `runtime/policy-decisions.jsonl`).
///
/// Both single-modality (`compose_with_version_and_modality`) and
/// multi-modality (`compose_with_version_and_modalities_full`)
/// dispatchers route here for v4. The single-modality entry point
/// extracts only `.composition` (legacy CompositionResult) and
/// discards v4-only fields; conversation crate callers should use
/// the `_full` entry point to preserve them.
///
/// **`target_modalities` is threaded into the
/// `PlanningContext.intent` so the v4 forward / backward search has
/// the modality, project class, and modality-derived available data
/// it needs to walk the registry. Without this seed, the planner
/// runs on an empty intent and returns `PartialDag` for every
/// dispatch.**
#[allow(clippy::too_many_arguments)]
pub(crate) fn compose_v4_dispatch_full(
    goal: &GoalSpec,
    project_class: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
    target_modalities: &[&str],
    policy_ctx: Option<&crate::policy_context::PolicyContext>,
    // R1/R2 closure (closure-residuals plan Task 1.4) — opaque
    // observation sink + session id threaded into the composer's
    // PlanningContext so the v4 planner can lower them onto the engine
    // `CompatibilityContext` at per-atom `prove()` call sites.
    opaque_sink: Option<
        std::sync::Arc<dyn crate::compatibility::engine::OpaqueObservationSink + Send + Sync>,
    >,
    opaque_session_id: Option<&str>,
    // SME/intake-requested methods keyed by bare discover axis. Empty for
    // bare/eval/test callers (byte-identical emit); populated from the CLI
    // classifier (`methods_specified`) or chat `set_intake_method` so the
    // discover-companion synthesis can stamp `spec_preferred_methods`.
    preferred_methods: &crate::preferred_methods::PreferredMethods,
) -> Result<ComposerOutput, CompositionError> {
    use crate::composer_v4;

    // Build the typed PlanningContext from the
    // dispatcher's args:
    //
    // - `intent.modality` ← primary modality (first in the slice).
    // - `intent.project_class` ← project class string.
    // - `intent.available_data` ← best-effort seed derived from the
    // modality. A future dataset profiler will replace this with
    // real intake-derived contracts; until then the seed gives the
    // forward search a frontier to walk from. The seed mirrors
    // `scenario_available_data` in the parity-corpus regenerator.
    // - `intent.desired_outputs` ← built from `goal.edam_data` +
    // `goal.edam_format` by the helper.
    let primary_modality = target_modalities.first().copied();
    let additional_modalities: Vec<&str> = target_modalities.iter().skip(1).copied().collect();

    // Bare-modality goal synthesis. When the caller passes
    // a goal whose `edam_data` matches no archetype's `goal_data` AND
    // a primary archetype for the requested modality exists, rewrite
    // the goal to the archetype's effective `(goal_data, goal_format)`
    // pair before planning. This is the v4 closer of the bare-modality
    // gap that previously routed through the legacy taxonomy build:
    // SME prose like "single cell scRNA-seq from human IVD samples
    // with 10x Chromium" classifies to a modality but no goal phrase,
    // and the conversation crate either infers from the modality
    // archetype or passes a placeholder goal. Either way, we land on
    // the archetype's canonical goal so `validate_composition` and
    // `lower_dag_to_composition_result` agree.
    //
    // Cross-omics intake (`additional_modalities` non-empty) is handled
    // separately by `try_cross_omics_archetype_seed`; this rewrite
    // only fires for single-modality intake.
    //
    // Bug #9 — additionally captures the rewrite-selected
    // archetype so we can override the dispatcher's incoming
    // `project_class` when the modality-specific archetype lives under
    // a different project_class. Closes the GWAS false-positive case
    // where "Phase 3 EUR" (1000 Genomes reference panel) trips the
    // clinical_trial classifier but the `gwas_coloc` archetype is
    // registered under `project_class: bioinformatics`. Without the
    // override the downstream planner kept `project_class:
    // clinical_trial` and matched `clinical_trial_analysis` instead.
    // Two-pass lookup so a high-confidence modality classifier wins over
    // a softer project-class classifier when they disagree. Pass 1 looks
    // for an archetype that matches both signals (e.g. a clinical-trial
    // GWAS archetype, if one existed). Pass 2 falls back to ANY archetype
    // matching the requested modality, regardless of project class — this
    // is the GWAS+clinical-trial case where `gwas_coloc` lives under
    // `project_class: bioinformatics` but the classifier saw "Phase 3 EUR"
    // and routed to clinical_trial. Without the fallback, pass 1 returns
    // the modality-less `clinical_trial_analysis` (a project-class
    // default) and the planner builds the wrong DAG.
    //
    // `generic_omics` is excluded from the modality-wins fallback: it is
    // the sentinel "modality not confidently classified" value, so the
    // project-class signal carries the real information (e.g. a generic
    // intake under `project_class: clinical_trial` should still build
    // the clinical_trial_analysis archetype, not a bioinformatics
    // generic_omics catch-all).
    let primary_archetype_for_modality = if additional_modalities.is_empty() {
        primary_modality.and_then(|modality| {
            let pass1 = archetype_reg.find_primary_for_modality(modality, project_class);
            if modality == "generic_omics" {
                // Don't let the generic sentinel override the project
                // class — return pass 1 unmodified.
                return pass1;
            }
            pass1
                // Reject the project-class-only fallback when the
                // returned archetype's modality_hint doesn't match
                // the requested modality. That case means pass 2 of
                // find_primary_for_modality returned a modality-less
                // project-class default; the any-project lookup below
                // is the correct answer when the modality is the
                // higher-confidence signal.
                .filter(|a| a.modality_hint.as_deref() == Some(modality))
                .or_else(|| archetype_reg.find_primary_for_modality_hint_any_project(modality))
        })
    } else {
        None
    };
    let mut effective_project_class: String = project_class.to_string();
    if let Some(primary) = primary_archetype_for_modality.as_ref() {
        if primary.project_class != effective_project_class {
            effective_project_class = primary.project_class.clone();
        }
    }
    let effective_goal: GoalSpec = if additional_modalities.is_empty() {
        if let Some(modality) = primary_modality {
            // Bare-modality detection: no archetype's `goal_data`
            // exact-or-subtype-matched the input goal. Modality-only
            // and project_class-only score components are NOT goal
            // signal — they're partial matches that don't indicate
            // the input goal's `edam_data` actually corresponds to
            // any catalog archetype's output. Inspect the score
            // breakdown rather than `is_empty()`.
            let matches = archetype_reg.find_match_with_evidence_modality_kind(
                &goal.edam_data,
                goal.edam_format.as_deref(),
                effective_project_class.as_str(),
                Some(modality),
                goal.modifiers.get("kind").map(|s| s.as_str()),
            );
            // The rewrite is modality-specific (we rewrite to the
            // primary archetype of the requested modality), so the
            // reachability check must be modality-specific too:
            // restrict the check to archetypes whose
            // `modality_match > 0`. Otherwise a bulk-RNA-seq DE
            // archetype matching `data:0951` would suppress the
            // rewrite for an ATAC-seq SME whose archetype catalog has
            // no DE shape, and the planner would error with
            // `GoalUnreachable { goal: data:0951 ... }`.
            let any_goal_data_match = matches.iter().any(|m| {
                m.evidence.modality_match > 0
                    && (m.evidence.goal_data_exact > 0 || m.evidence.goal_data_subtype > 0)
            });
            if !any_goal_data_match {
                if let Some(primary) = primary_archetype_for_modality.as_ref() {
                    let mut synthesized = goal.clone();
                    synthesized.edam_data = primary.goal_data.clone();
                    synthesized.edam_format = primary.goal_format.clone();
                    if let Some(kind) = &primary.goal_kind_hint {
                        synthesized
                            .modifiers
                            .insert("kind".to_string(), kind.clone());
                    }
                    synthesized
                } else {
                    goal.clone()
                }
            } else {
                goal.clone()
            }
        } else {
            goal.clone()
        }
    } else {
        goal.clone()
    };

    // Project-class-aware seed. Clinical-trial and
    // time-series project classes drive `data_import`-rooted pipelines
    // whose input port is a `ecaax:dataset_descriptor` (SME-supplied
    // tabular/CDISC), not paired-end FASTQ. Seeding with FASTQ for
    // these classes caused the forward search in the v4 planner to
    // fail to bridge `data:2044` into the `data_import` input
    // contract → `GoalUnreachable { goal: "data:0951 (format:3475)" }`.
    // The seed key respects the post-rewrite `effective_project_class`
    // so bug-#9 rerouted scenarios (GWAS misrouted to clinical_trial)
    // get the right seed shape.
    let available_data = if matches!(
        effective_project_class.as_str(),
        "clinical_trial" | "time_series_forecast"
    ) {
        vec![
            crate::workflow_contracts::data_product::DataProductContract::sample_dataset_descriptor(
            ),
        ]
    } else {
        seed_available_data_for_modalities(target_modalities)
    };

    // Thread the full modality slice (primary +
    // additional) into the PlanningContext. When two or more modalities
    // are requested, the v4 planner attempts a cross-omics archetype
    // match (set-equality on `cross_omics_modalities`) before falling
    // through to single-modality matching. Without this, cross-omics
    // scenarios silently degenerate to a single-modality bare-name
    // pipeline because the single-modality matcher
    // (`find_match_with_evidence_modality_kind`) explicitly excludes
    // archetypes carrying `cross_omics_modalities`.
    let mut ctx = composer_v4::planning_context_for_goal_with_modalities(
        format!(
            "v4_{}_{}",
            effective_project_class, effective_goal.edam_data
        ),
        &effective_goal,
        primary_modality,
        &additional_modalities,
        Some(effective_project_class.as_str()),
        &available_data,
    );
    // R1/R2 closure — surface the opaque-observation sink + session id
    // onto the composer-level PlanningContext so `composer_v4::plan`
    // (and the underlying forward/backward/meet-in-middle search modules)
    // can lower them onto every engine `prove()` call site.
    ctx.opaque_observation_sink = opaque_sink;
    ctx.opaque_session_id = opaque_session_id.map(String::from);
    ctx.preferred_methods = preferred_methods.clone();
    // Session-scope every verifier-substrate row recorded for the rest
    // of this dispatch (the engine + planner rows fired inside `plan()`
    // below, plus the policy-gate rows fired by `collect_policy_decisions`)
    // into the calling session's bucket so two server sessions composing
    // concurrently never interleave their decisions into one shared
    // buffer. Bare callers (CLI `intake`, eval, tests) pass no session id
    // and so record into the unscoped default bucket exactly as before.
    // The named binding keeps the RAII guard alive until the function
    // returns from any of its match arms, restoring the thread's previous
    // ambient session on drop.
    let _substrate_scope = opaque_session_id.map(crate::decision_substrate::enter_session);
    let result = composer_v4::plan(
        &ctx,
        &effective_goal,
        effective_project_class.as_str(),
        atom_reg,
        archetype_reg,
    );
    let alternatives = result.alternatives.clone();
    let outcome = result.primary.clone();

    // Evaluate per-node policy gate before classifying the outcome
    // shape. We collect decisions for any DAG-bearing outcome so the
    // SME can audit which policies the composition cleared.
    let policy_decisions = collect_policy_decisions(policy_ctx, &outcome);

    match outcome.clone() {
        crate::workflow_contracts::outcome::ComposeOutcome::ValidatedExecutableDag {
            dag, ..
        } => {
            let mut composition =
                composer_v4::lower_dag_to_composition_result(&dag, atom_reg, &effective_goal)?;
            validate_composition(&composition, atom_reg)?;
            composition.matched_archetype = composition
                .matched_archetype
                .or_else(|| Some(format!("v4:{}", effective_goal.edam_data)));
            Ok(ComposerOutput {
                composition,
                workflow_dag: Some(dag),
                compose_outcome: Some(outcome),
                ranked_alternatives: alternatives,
                policy_decisions,
            })
        }
        crate::workflow_contracts::outcome::ComposeOutcome::DraftDag { dag, blockers, .. } => {
            let lowered =
                composer_v4::lower_dag_to_composition_result(&dag, atom_reg, &effective_goal);
            let summary = format!(
                "v4 planner returned DraftDag with {} blocker(s)",
                blockers.len()
            );
            match lowered {
                Ok(composition) => {
                    validate_composition(&composition, atom_reg)?;
                    Err(CompositionError::ComposerV4OutcomeNotExecutable {
                        outcome_kind: "DraftDag".into(),
                        summary,
                        gaps: blockers.iter().map(|b| b.statement.clone()).collect(),
                    })
                }
                Err(_) => Err(CompositionError::ComposerV4OutcomeNotExecutable {
                    outcome_kind: "DraftDag".into(),
                    summary,
                    gaps: blockers.iter().map(|b| b.statement.clone()).collect(),
                }),
            }
        }
        crate::workflow_contracts::outcome::ComposeOutcome::PartialDag {
            unresolved_gaps, ..
        } => Err(CompositionError::ComposerV4OutcomeNotExecutable {
            outcome_kind: "PartialDag".into(),
            summary: format!(
                "v4 planner returned PartialDag with {} unresolved gap(s)",
                unresolved_gaps.len()
            ),
            gaps: unresolved_gaps
                .iter()
                .map(|g| g.statement.clone())
                .collect(),
        }),
        crate::workflow_contracts::outcome::ComposeOutcome::NovelNodeSpec {
            node,
            required_work,
        } => Err(CompositionError::ComposerV4OutcomeNotExecutable {
            outcome_kind: "NovelNodeSpec".into(),
            summary: format!(
                "v4 planner proposed a hypothesized node ({}) requiring {} validation \
                 obligation(s) before promotion",
                node.id,
                required_work.len()
            ),
            gaps: required_work.iter().map(|o| o.statement.clone()).collect(),
        }),
        crate::workflow_contracts::outcome::ComposeOutcome::Refusal { report } => {
            Err(CompositionError::ComposerV4OutcomeNotExecutable {
                outcome_kind: "Refusal".into(),
                summary: report.statement.clone(),
                gaps: report.references.clone(),
            })
        }
    }
}

/// Collect `PolicyDecisionRecord`s from the v4 planner's
/// per-node policy gate so the emit pipeline can persist them to
/// `runtime/policy-decisions.jsonl`.
fn collect_policy_decisions(
    policy_ctx: Option<&crate::policy_context::PolicyContext>,
    outcome: &crate::workflow_contracts::outcome::ComposeOutcome,
) -> Vec<PolicyDecisionRecord> {
    let Some(policy) = policy_ctx else {
        return Vec::new();
    };
    let dag_opt = match outcome {
        crate::workflow_contracts::outcome::ComposeOutcome::ValidatedExecutableDag {
            dag, ..
        }
        | crate::workflow_contracts::outcome::ComposeOutcome::DraftDag { dag, .. }
        | crate::workflow_contracts::outcome::ComposeOutcome::PartialDag { dag, .. } => Some(dag),
        _ => None,
    };
    let Some(dag) = dag_opt else {
        return Vec::new();
    };
    let eval = crate::composer_v4::policy_gate::evaluate(policy, dag);
    let mut decisions: Vec<PolicyDecisionRecord> = Vec::new();
    for v in eval.violations {
        decisions.push(PolicyDecisionRecord {
            bundle_id: "active_bundle".into(),
            kind: format_check_kind_str(&v.check_kind),
            node_id: Some(v.node_id),
            statement: v.statement,
            blocking: v.blocking,
            chain_of_custody: None,
        });
    }
    for rec in eval.recorded_decisions {
        // `recorded_decisions` entries arrive as `"<bundle>: <kind>"`
        // strings — split into typed fields for the persisted record.
        let (bundle_id, kind) = rec.split_once(": ").unwrap_or(("active_bundle", &rec));
        decisions.push(PolicyDecisionRecord {
            bundle_id: bundle_id.to_string(),
            kind: kind.to_string(),
            node_id: None,
            statement: rec.clone(),
            blocking: false,
            chain_of_custody: None,
        });
    }
    decisions.sort_by(|a, b| {
        a.node_id
            .cmp(&b.node_id)
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.bundle_id.cmp(&b.bundle_id))
    });
    decisions
}

fn format_check_kind_str(kind: &crate::policy_context::PolicyCheckKind) -> String {
    use crate::policy_context::PolicyCheckKind;
    match kind {
        PolicyCheckKind::NoScientificallyRiskyAdapters => "no_scientifically_risky_adapters",
        PolicyCheckKind::NoPolicyRestrictedAdapters => "no_policy_restricted_adapters",
        PolicyCheckKind::NoPrivacyWidening => "no_privacy_widening",
        PolicyCheckKind::AuditTrailRequired => "audit_trail_required",
        PolicyCheckKind::HumanSignoffRequired => "human_signoff_required",
        PolicyCheckKind::ValidatedNodesOnly => "validated_nodes_only",
        PolicyCheckKind::RequirePinnedContainers => "require_pinned_containers",
        PolicyCheckKind::NoGeneratedCode => "no_generated_code",
        PolicyCheckKind::NoNetwork => "no_network",
        PolicyCheckKind::PinnedReferenceDataOnly => "pinned_reference_data_only",
        PolicyCheckKind::SiteLocal => "site_local",
    }
    .to_string()
}

/// Modality-aware best-effort seed for
/// `WorkflowIntent.available_data` used by `compose_v4_dispatch_full`
/// when no real intake-profiler contracts are threaded in.
///
/// The v4 forward search needs at least one [`DataProductContract`] to
/// walk the registry from. Once a dataset profiler exists, the
/// dispatch caller will pass typed contracts derived from real
/// intake artifacts; until then, this helper synthesizes a paired-end
/// FASTQ shape (`data:2044` / `format:1930`) that unifies with every
/// modality whose pipeline starts at sequencer reads (bulk-rnaseq,
/// scrnaseq, variant-calling, chip-seq, atac-seq, long-read-rnaseq).
///
/// Cross-omics and project-class scenarios (proteomics, time-series,
/// clinical-trial) get the same seed today. Their pipelines may not
/// directly consume FASTQ, but the archetype registry's cross-omics
/// match takes precedence over forward search in those cases — the
/// seed acts only as a non-empty fallback so the planner never starts
/// from an empty frontier.
///
/// Mirrors `scenario_available_data` in
/// `crates/core/tests/composer_v4_parity_corpus.rs::emit_v4`.
fn seed_available_data_for_modalities(
    target_modalities: &[&str],
) -> Vec<crate::workflow_contracts::data_product::DataProductContract> {
    use crate::workflow_contracts::data_product::DataProductContract;
    if target_modalities.is_empty() {
        return vec![DataProductContract::sample_paired_fastq()];
    }
    let mut data = Vec::with_capacity(target_modalities.len());
    for modality in target_modalities {
        match *modality {
            "bulk_rnaseq" | "single_cell_rnaseq" | "variant_calling" | "chip_seq" | "atac_seq"
            | "long_read_rnaseq" => {
                data.push(DataProductContract::sample_paired_fastq());
            }
            _ => {
                // Proteomics + project-class scenarios share the same
                // FASTQ-shaped fallback seed — see the doc-comment for
                // why this is acceptable today.
                data.push(DataProductContract::sample_paired_fastq());
            }
        }
    }
    if data.is_empty() {
        data.push(DataProductContract::sample_paired_fastq());
    }
    data
}

/// Composer dispatch returning a full
/// `ComposerOutput` (composition + v4 sidecar data).
///
/// Sibling of `compose_with_version_and_modalities`. v1/v2/v3 paths
/// wrap their `CompositionResult` via `ComposerOutput::legacy`; v4
/// paths return the full bundle so the conversation crate can route
/// through `build_dag_from_workflow_dag` and persist proof-carrying
/// sidecars at emit time.
#[allow(clippy::too_many_arguments)]
pub fn compose_with_modalities_full(
    goal: &GoalSpec,
    project_class: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
    target_modalities: &[&str],
    policy_ctx: Option<&crate::policy_context::PolicyContext>,
    opaque_sink: Option<
        std::sync::Arc<dyn crate::compatibility::engine::OpaqueObservationSink + Send + Sync>,
    >,
    opaque_session_id: Option<&str>,
) -> Result<ComposerOutput, CompositionError> {
    // Thin delegator: bare callers (CLI build/preview, conformance + composer
    // tests) carry no requested methods → empty preferred set → byte-identical
    // emit. The preferred-aware capture sites (CLI intake, chat rebuild_dag)
    // call `compose_with_modalities_full_pref` directly with the captured set.
    compose_with_modalities_full_pref(
        goal,
        project_class,
        atom_reg,
        archetype_reg,
        target_modalities,
        policy_ctx,
        opaque_sink,
        opaque_session_id,
        &crate::preferred_methods::PreferredMethods::new(),
    )
}

/// As [`compose_with_modalities_full`], plus the SME/intake-requested
/// methods (keyed by bare discover axis) that the discover-companion
/// synthesis stamps onto discover task specs. The two capture sources —
/// the CLI classifier and chat `set_intake_method` — fold into one
/// [`crate::preferred_methods::PreferredMethods`] before calling here.
#[allow(clippy::too_many_arguments)]
pub fn compose_with_modalities_full_pref(
    goal: &GoalSpec,
    project_class: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
    target_modalities: &[&str],
    policy_ctx: Option<&crate::policy_context::PolicyContext>,
    // R1/R2 closure (closure-residuals plan Task 1.4) — optional
    // cross-session opaque-type observation sink + session id. When set,
    // the v4 planner threads them into the engine `PlanningContext` so
    // Opaque-type observations attribute to the right session and node.
    // Bare callers (CLI `intake`, eval-baselines, tests) pass `None,
    // None` and preserve existing log-only behavior; the conversation
    // crate's `try_build_via_composer` constructs the concrete sink from
    // `ECAA_CHAT_SESSIONS_DIR` and threads `session.id` through.
    opaque_sink: Option<
        std::sync::Arc<dyn crate::compatibility::engine::OpaqueObservationSink + Send + Sync>,
    >,
    opaque_session_id: Option<&str>,
    preferred_methods: &crate::preferred_methods::PreferredMethods,
) -> Result<ComposerOutput, CompositionError> {
    // Atypical-shape fall-through. The v4 dispatch jumps straight to
    // `compose_v4_dispatch_full` for the normal path, so flex-shape /
    // out-of-catalog prompts (CyTOF, Mendelian randomization, cryo-EM,
    // …) would misroute to the nearest keyword-similar archetype and
    // emit forbidden domain atoms unless we force them to `generic_omics`
    // here first.
    let is_out_of_catalog = goal
        .modifiers
        .get("kind")
        .map(|k| k == "out_of_catalog")
        .unwrap_or(false);
    // clinical_trial / time_series_forecast are covered project classes
    // with dedicated archetypes (clinical_trial_analysis,
    // time_series_forecast) that already route to generic_omics with the
    // right richer atom set — a clinical mortality trial naming
    // Kaplan-Meier / Cox must NOT be hijacked to the bare generic_omics
    // scaffold. Restrict the fall-through to the bioinformatics class.
    let covered_project_class =
        project_class == "clinical_trial" || project_class == "time_series_forecast";
    // `out_of_catalog` fires regardless of how many modality companions
    // the keyword scorer surfaced: a scATAC-only prompt is mis-detected
    // as RNA+ATAC cross-omics, and that spurious companion is exactly the
    // misroute being suppressed. Other flex kinds stay single-modality.
    let generic_fallthrough = !covered_project_class
        && (unique_modalities(target_modalities).len() < 2 || is_out_of_catalog)
        && requires_generic_fallthrough(goal, target_modalities.first().copied());

    if generic_fallthrough {
        // Route the fall-through THROUGH the v4 planner with the
        // modality forced to `generic_omics`. The conversation session
        // model is workflow_dag-centric — `Session::current_dag` reads
        // `session.workflow_dag`, and promoted hypothesized-node
        // proposals are injected onto that typed `WorkflowDag`. Producing
        // a v4 `WorkflowDag` here keeps both the CLI and the
        // conversation/server emit paths working off the same
        // authoritative structure. edam_data is wildcarded —
        // generic_omics is the no-committed-shape archetype.
        let mut wildcarded = goal.clone();
        wildcarded.edam_data = String::new();
        wildcarded.edam_format = None;
        return compose_v4_dispatch_full(
            &wildcarded,
            project_class,
            atom_reg,
            archetype_reg,
            &["generic_omics"],
            policy_ctx,
            opaque_sink,
            opaque_session_id,
            preferred_methods,
        );
    }
    // v4 dispatch already takes the policy context; for multi-modality
    // (cross-omics) we route through the same entry — the v4 planner
    // discovers cross-omics archetypes through the same archetype
    // registry. Thread the full modality slice so the
    // PlanningContext.intent has primary modality + project class
    // populated.
    compose_v4_dispatch_full(
        goal,
        project_class,
        atom_reg,
        archetype_reg,
        target_modalities,
        policy_ctx,
        opaque_sink,
        opaque_session_id,
        preferred_methods,
    )
}
