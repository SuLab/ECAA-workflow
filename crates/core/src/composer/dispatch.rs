//! Public-API dispatch entry points for the composer.
//!
//! Houses the five `compose_*` overloads that callers (CLI `intake`,
//! the conversation crate's `try_build_via_composer`, eval-baselines,
//! and the integration-test corpus) drive through, plus the three
//! v4-specific internal helpers (`collect_policy_decisions`,
//! `format_check_kind_str`, `seed_available_data_for_modalities`).
//!
//! Routing precedence is preserved verbatim:
//!
//! - `compose` is the zero-config entry point — delegates to
//!   `compose_with_version` with `composer_version = 4`.
//! - `compose_with_version` lifts the optional modality slot to
//!   `compose_with_version_and_modality`.
//! - `compose_with_version_and_modality` routes v4 through
//!   `compose_v4_dispatch_full` (single-modality slice) and v1/v2/v3
//!   through the legacy archetype fast-path with backward-chain
//!   fallback.
//! - `compose_with_version_and_modalities` adds cross-omics archetype
//!   matching for multi-modality requests, falling back to the
//!   generic multi-branch synthesizer when no cross-omics archetype
//!   set-equals the request.
//! - `compose_with_version_and_modalities_full` is the proof-carrying
//!   variant: returns the full `ComposerOutput` (composition +
//!   WorkflowDag + ranked alternatives + ComposeOutcome +
//!   per-node policy decisions) so the conversation crate can
//!   persist v4 sidecars at emit time.
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

use super::backward_chain::backward_chain_compose;
use super::inheritance::resolve_inheritance;
use super::multi_modal::{synthesize_generic_multi_modal_composition, unique_modalities};
use super::validation::validate_composition;
use super::{
    aggregate_resources, apply_atom_ref_overrides, merged_depends_on, resolve_task_container,
    AtomSelectionRationale, ComposedAtom, ComposerOutput, CompositionError, CompositionResult,
    PolicyDecisionRecord, SelectionReason,
};
use crate::archetype::ArchetypeDefinition;
use crate::archetype_registry::ArchetypeRegistry;
use crate::atom_registry::AtomRegistry;
use crate::goal_spec::GoalSpec;
use crate::ids::StageId;
use std::collections::BTreeMap;

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

/// Build a `CompositionResult` from a single archetype-in-hand, mirroring
/// the legacy archetype fast-path body of `compose_with_version_and_modality`.
///
/// Extracted so the generic-omics confidence fall-through can route through
/// the same atom-resolution + rationale-synthesis logic as the normal path.
/// Callers are responsible for invoking `validate_composition` on the
/// returned result.
fn resolve_archetype_to_composition(
    archetype: &ArchetypeDefinition,
    goal: &GoalSpec,
    atom_reg: &AtomRegistry,
    _archetype_reg: &ArchetypeRegistry,
) -> Result<CompositionResult, CompositionError> {
    let mut composed: Vec<ComposedAtom> = Vec::with_capacity(archetype.atoms.len());
    for aref in &archetype.atoms {
        let atom =
            atom_reg
                .get(aref.atom_id.as_str())
                .ok_or_else(|| CompositionError::UnknownAtom {
                    archetype_id: archetype.id.clone(),
                    atom_id: aref.atom_id.clone(),
                })?;
        let container = resolve_task_container(atom, None, None);
        composed.push(ComposedAtom {
            stage_id: aref
                .alias
                .as_deref()
                .map(StageId::from)
                .unwrap_or_else(|| StageId::from(aref.atom_id.as_str())),
            atom: apply_atom_ref_overrides(atom, aref),
            depends_on: merged_depends_on(atom, aref),
            required: aref.required,
            bindings: Vec::new(),
            container,
        });
    }
    let resource_estimate = aggregate_resources(&composed);
    // The fall-through path has no archetype-match score; emit zero so
    // rationale records still serialize cleanly.
    let synthesized_score: u32 = 0;
    let atom_rationales: BTreeMap<String, AtomSelectionRationale> = composed
        .iter()
        .map(|c| {
            (
                c.stage_id.to_string(),
                AtomSelectionRationale {
                    stage_id: c.stage_id.clone(),
                    atom_id: c.atom.id.clone().into(),
                    reason: SelectionReason::ArchetypeRequired,
                    score: synthesized_score,
                    explanation: format!(
                        "Archetype {} declares {} as required.",
                        archetype.id, c.atom.id
                    ),
                },
            )
        })
        .collect();
    Ok(CompositionResult {
        matched_archetype: Some(archetype.id.clone()),
        match_score: synthesized_score,
        atoms: composed,
        goal: goal.clone(),
        rationale: archetype.sme_summary.clone(),
        atom_rationales,
        resource_estimate,
    })
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
    compose_with_version(goal, project_class, atom_reg, archetype_reg, 4)
}

/// Version-aware compose entry. `composer_version`
/// determines routing:
///
/// - **1** (legacy): archetype-first with backward-chain fallback.
///   Today's `compose()` default. Effectively identical to v2 for
///   our v1 atom library; the version distinction is a migration
///   rail (DEC Q3.5).
/// - **2** (archetype-fast-path; default): archetype-first with
///   backward-chain fallback. Same routing as `compose()`.
/// - **3** (backward-chain forced): skip the archetype fast-path
///   entirely and route through `backward_chain_compose`. Honored
///   when ECAA_COMPOSER=backward-chain is set at session creation
///   (read by `Session::new` at session-creation time and pinned
///   on `Session.composer_version` so amendments stay on the same
///   path through to re-emit).
///
/// Sessions persisted with `composer_version` other than 1/2/3
/// fall back to the v2 default.
pub fn compose_with_version(
    goal: &GoalSpec,
    project_class: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
    composer_version: u32,
) -> Result<CompositionResult, CompositionError> {
    compose_with_version_and_modality(
        goal,
        project_class,
        atom_reg,
        archetype_reg,
        composer_version,
        None,
    )
}

/// Tie-fix variant — same as `compose_with_version` but also
/// accepts the classifier's modality so `find_match_with_modality`
/// adds the +2 modality-hint disambiguator. Resolves DE-shaped 3-way
/// ties (bulk_rnaseq_de + long_read_rnaseq + metagenomics_taxonomic
/// all producing `data:0951 / format:3475` would tie at score 6
/// without this).
///
/// Also reads `goal.modifiers.kind` and passes it as the goal-kind
/// disambiguator (resolves proteomics DDA-vs-DIA tie at score 8).
/// The kind lookup is automatic; no extra parameter required.
///
/// The dispatch table preserves `composer_version` 1 / 2 / 3 paths
/// so persisted sessions whose `Session::composer_version` is pinned
/// at 1, 2, or 3 keep producing the same DAG shape they did
/// originally.
pub fn compose_with_version_and_modality(
    goal: &GoalSpec,
    project_class: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
    composer_version: u32,
    target_modality: Option<&str>,
) -> Result<CompositionResult, CompositionError> {
    // Atypical-shape fall-through: prompts whose `kind` modifier names
    // a flex-shape analysis (survival, strain-SNP, scATAC-only) bypass
    // the modality archetype and route to `generic_omics` so the
    // universal `raw_qc` + `generic_summary` terminals are always
    // present. Without this, modality archetypes emit their own
    // domain-specific terminals (`reporting`, `final_reporting`) that
    // don't satisfy the blinded-corpus universal-terminal contract.
    if requires_generic_fallthrough(goal, target_modality) {
        if let Some(generic) = archetype_reg.get("generic_omics") {
            // Override the goal's edam_data to the wildcard (empty
            // string) — generic_omics is the "no specific committed
            // shape" archetype, so the SME's goal_pattern-derived
            // data class (e.g. survival's data:0951) doesn't apply.
            // The validation.rs wildcard short-circuit (RCA cluster D
            // defense-in-depth) accepts empty edam_data without the
            // any_reaches goal-shape check.
            let mut wildcarded = goal.clone();
            wildcarded.edam_data = String::new();
            wildcarded.edam_format = None;
            let result =
                resolve_archetype_to_composition(generic, &wildcarded, atom_reg, archetype_reg)?;
            validate_composition(&result, atom_reg)?;
            return Ok(result);
        }
    }

    // The v3
    // (`backward-chain`) entry point is retired. Sessions persisted
    // with composer_version=3 fall through to v2 routing here
    // (archetype fast-path with backward-chain fallback when no
    // archetype matches) — same observable behavior the v3-forced
    // path delivered when the catalog had no archetype winner. The
    // backward-chain *algorithm* (`backward_chain_compose` below)
    // stays alive as the v2 archetype-empty fallback.
    // composer_version=4 routes through the proof-carrying v4
    // planner. We dispatch through
    // `compose_v4_dispatch_full` so the underlying call shares one
    // implementation; this entry point's signature returns the
    // legacy `CompositionResult` so v4 metadata (WorkflowDag,
    // ranked alternatives, ComposeOutcome, policy decisions) is
    // discarded here. Callers that need the full bundle must use
    // `compose_with_version_and_modalities_full` instead. Returns
    // `ComposerV4OutcomeNotExecutable` for non-Validated outcomes
    // (DraftDag / PartialDag / NovelNodeSpec / Refusal). Callers
    // that want the legacy DAG fallback should set composer_version<=3.
    if composer_version == 4 {
        // Single-modality entry point lifts the
        // optional `target_modality` into a single-element slice
        // (or empty when None) so the v4 dispatch sees the same
        // modality information the multi-modality entry point
        // does. Empty slice ⇒ the dispatcher's seeded available_data
        // helper picks the modality-agnostic FASTQ fallback.
        let modalities: Vec<&str> = target_modality.into_iter().collect();
        return compose_v4_dispatch_full(
            goal,
            project_class,
            atom_reg,
            archetype_reg,
            &modalities,
            None,
            // R1/R2 closure — this singular dispatch path discards v4
            // sidecar metadata, so opaque-sink wiring is unused here.
            None,
            None,
        )
        .map(|out| out.composition);
    }
    // The v1
    // *surface* (CLI `--taxonomy` flag, `ECAA_COMPOSER=legacy`
    // alias) is retired, but the v1 *routing* (archetype fast-path
    // with backward-chain fallback) stays alive here so persisted
    // sessions with `composer_version` pinned at 1 keep building
    // the same DAG shape. The legacy taxonomy YAMLs under
    // `config/stage-taxonomies/` remain on disk for the v4 soak
    // window; deletion is gated on v4 producing a DAG for prose
    // without an explicit goal phrase.
    let target_kind = goal.modifiers.get("kind").map(|s| s.as_str());
    let matches = archetype_reg.find_match_with_modality_and_kind(
        &goal.edam_data,
        goal.edam_format.as_deref(),
        project_class,
        target_modality,
        target_kind,
    );

    let result = if matches.is_empty() {
        // No archetype; fall through to backward-chain.
        backward_chain_compose(goal, atom_reg, project_class)?
    } else {
        let top_score = matches[0].1;
        // 5% tie-surfacing.
        let tie_threshold = (top_score as f32 * 0.95).floor() as u32;
        let close: Vec<&ArchetypeDefinition> = matches
            .iter()
            .filter(|(_, s)| *s >= tie_threshold)
            .map(|(a, _)| *a)
            .collect();
        if close.len() > 1 {
            return Err(CompositionError::TieRequiresSmeDecision {
                candidates: close.iter().map(|a| a.id.clone()).collect(),
                score: top_score,
            });
        }
        let archetype = matches[0].0;

        // Resolve atoms + apply wiring.
        let mut composed: Vec<ComposedAtom> = Vec::with_capacity(archetype.atoms.len());
        for aref in &archetype.atoms {
            let atom = atom_reg.get(aref.atom_id.as_str()).ok_or_else(|| {
                CompositionError::UnknownAtom {
                    archetype_id: archetype.id.clone(),
                    atom_id: aref.atom_id.clone(),
                }
            })?;
            // Resolve container at compose time using the
            // atom > archetype > profile > host precedence.
            // archetype-level + profile-level slots are None today;
            // they'll get populated when S15.21 / S15.23 land.
            let container = resolve_task_container(atom, None, None);
            composed.push(ComposedAtom {
                stage_id: aref
                    .alias
                    .as_deref()
                    .map(StageId::from)
                    .unwrap_or_else(|| StageId::from(aref.atom_id.as_str())),
                atom: apply_atom_ref_overrides(atom, aref),
                depends_on: merged_depends_on(atom, aref),
                required: aref.required,
                bindings: Vec::new(),
                container,
            });
        }

        let resource_estimate = aggregate_resources(&composed);
        // Synthesize per-atom rationales for the
        // archetype fast path. Every atom in an archetype scaffold
        // is either required (the default) or an optional slot-fill
        // win; today we don't carry slot-candidate ids through this
        // path so optional atoms get the same `ArchetypeRequired`
        // shape with the score that the archetype scored against
        // the goal. The richer `OptionalSlotFilled` variant is
        // reserved for the slot-fill resolver (S6.11) once it
        // surfaces candidates here.
        let atom_rationales: BTreeMap<String, AtomSelectionRationale> = composed
            .iter()
            .map(|c| {
                (
                    c.stage_id.to_string(),
                    AtomSelectionRationale {
                        stage_id: c.stage_id.clone(),
                        atom_id: c.atom.id.clone().into(),
                        reason: SelectionReason::ArchetypeRequired,
                        score: top_score,
                        explanation: format!(
                            "Archetype {} declares {} as required.",
                            archetype.id, c.atom.id
                        ),
                    },
                )
            })
            .collect();
        CompositionResult {
            matched_archetype: Some(archetype.id.clone()),
            match_score: top_score,
            atoms: composed,
            goal: goal.clone(),
            rationale: archetype.sme_summary.clone(),
            atom_rationales,
            resource_estimate,
        }
    };

    // Six-item formal validation. Both fast-path and
    // backward-chain results funnel through here.
    validate_composition(&result, atom_reg)?;
    Ok(result)
}

/// Multi-modality compose entry point.
///
/// `target_modalities` is the SME's full requested modality set,
/// usually `&[primary, additional…]` derived from
/// `ClassificationResult.{modality, additional_modalities}`. The
/// dispatch contract:
///
/// - **0 or 1 modality** → delegate to the single-modality entry
///   point `compose_with_version_and_modality`. The function works
///   as a drop-in replacement for callers that don't yet thread the
///   additional modalities through.
/// - **≥2 modalities, composer_version != 3** → try
///   `find_match_cross_omics` first. If a cross-omics archetype's
///   `cross_omics_modalities` set-equals the request, use it (top-1
///   among candidates; no tie surfacing today since at most one
///   cross-omics archetype is registered per modality combo).
///   Otherwise synthesize a generic multi-branch DAG by composing the
///   best single-modality archetype for every requested modality,
///   namespace-prefixing every branch, and joining them through a
///   modality-agnostic reporting tail. This preserves every requested
///   analysis branch without requiring a bespoke archetype file for
///   each modality combination.
/// - **≥2 modalities, any non-v4 version** → cross-omics archetype
///   matching with a generic multi-branch synthesis fallback when
///   no cross-omics archetype is registered. The v3 backward-chain
///   entry point is retired; the dispatch table has no "force
///   backward-chain across modalities" branch.
pub fn compose_with_version_and_modalities(
    goal: &GoalSpec,
    project_class: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
    composer_version: u32,
    target_modalities: &[&str],
) -> Result<CompositionResult, CompositionError> {
    let modalities = unique_modalities(target_modalities);

    // Single (or zero) modality — delegate to the legacy entry point.
    if modalities.len() < 2 {
        return compose_with_version_and_modality(
            goal,
            project_class,
            atom_reg,
            archetype_reg,
            composer_version,
            modalities.first().copied(),
        );
    }

    let primary = modalities[0];

    // The v3
    // backward-chain entry point was retired; sessions persisted
    // with composer_version=3 now route through the v2 cross-omics
    // path below (archetype-first, with backward-chain fallback per
    // single-modality). The v3-specific multi-modality warning is
    // gone — there is no "force backward-chain" path to opt into
    // anymore.

    // Cross-omics archetype lookup — set-equality on
    // `cross_omics_modalities`.
    let target_kind = goal.modifiers.get("kind").map(|s| s.as_str());
    // Legacy (v2) dispatch hasn't surfaced n-way intent through the
    // GoalSpec; route conservatively (no superset fallback for
    // 2-modality input). v4 dispatch threads the proper flag via
    // try_cross_omics_archetype_seed.
    let n_way = goal
        .source_prose
        .as_deref()
        .map(crate::classify::is_n_way_intent)
        .unwrap_or(false);
    let mut cross_matches = archetype_reg.find_match_cross_omics(
        &goal.edam_data,
        goal.edam_format.as_deref(),
        project_class,
        &modalities,
        target_kind,
        n_way,
        goal.source_prose.as_deref().unwrap_or(""),
    );

    if cross_matches.is_empty() {
        eprintln!(
            "[composer] no cross-omics archetype matches the SME's modality set \
             (primary={}, additional={:?}); synthesizing a generic multi-branch DAG.",
            primary,
            &modalities[1..]
        );
        return synthesize_generic_multi_modal_composition(
            goal,
            project_class,
            atom_reg,
            archetype_reg,
            &modalities,
        );
    }

    // Integration-method discriminator hoist. Multiple cross-omics
    // archetypes can share the same modality set (e.g. DIABLO / MOFA /
    // SNF / generic for RNA + proteomics). EDAM-data scoring alone
    // can't distinguish them — they all consume the same data types.
    // When the SME's goal prose names a specific integrator, hoist the
    // matching archetype to the top so the LLM doesn't silently route
    // to whichever variant sorts first alphabetically.
    //
    // The discriminator scans goal.source_prose + every goal.modifiers
    // value for known integration-method substrings, then promotes any
    // candidate archetype whose id contains that method token.
    // Deterministic: case-folded substring match; no scoring noise.
    {
        let mut prose_lower = goal
            .source_prose
            .as_deref()
            .map(str::to_lowercase)
            .unwrap_or_default();
        for v in goal.modifiers.values() {
            prose_lower.push(' ');
            prose_lower.push_str(&v.to_lowercase());
        }
        const METHOD_TOKENS: &[&str] = &[
            "diablo",
            "spls-da",
            "mixomics",
            "mofa",
            "factor decomposition",
            "snf",
            "similarity network fusion",
            "wnn",
            "weighted nearest neighbor",
            "share seq",
            "shareseq",
            "multiome",
        ];
        for token in METHOD_TOKENS {
            if !prose_lower.contains(token) {
                continue;
            }
            // Normalize token for archetype-id matching (strip spaces,
            // hyphens; the archetype id uses snake_case).
            let id_form = token.replace([' ', '-'], "_");
            if let Some(hoist_idx) = cross_matches
                .iter()
                .position(|(a, _)| a.id.contains(&id_form))
            {
                if hoist_idx != 0 {
                    let entry = cross_matches.remove(hoist_idx);
                    cross_matches.insert(0, entry);
                }
                break;
            }
        }
    }

    // We have at least one cross-omics archetype. Top-1 wins after the
    // integration-method hoist above; tie surfacing for cross-omics is
    // reserved for a future iteration when the catalog has multiple
    // cross-omics archetypes per modality combo that ALSO share the
    // integration method (currently not a real case).
    let archetype = cross_matches[0].0;
    let top_score = cross_matches[0].1;

    // Flatten the archetype's `compose:` inheritance
    // before composition. Archetypes that don't use `compose:` get
    // their atom list back unchanged (lineage is empty); archetypes
    // that DO use `compose:` get the inherited atoms prepended /
    // appended / substituted with the rewriting rules applied.
    let flat = resolve_inheritance(archetype, archetype_reg)?;

    let mut composed: Vec<ComposedAtom> = Vec::with_capacity(flat.atoms.len());
    for aref in &flat.atoms {
        let atom =
            atom_reg
                .get(aref.atom_id.as_str())
                .ok_or_else(|| CompositionError::UnknownAtom {
                    archetype_id: archetype.id.clone(),
                    atom_id: aref.atom_id.clone(),
                })?;
        let container = resolve_task_container(atom, None, None);
        composed.push(ComposedAtom {
            stage_id: aref
                .alias
                .as_deref()
                .map(StageId::from)
                .unwrap_or_else(|| StageId::from(aref.atom_id.as_str())),
            atom: apply_atom_ref_overrides(atom, aref),
            depends_on: merged_depends_on(atom, aref),
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
                    atom_id: c.atom.id.clone().into(),
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
    let result = CompositionResult {
        matched_archetype: Some(archetype.id.clone()),
        match_score: top_score,
        atoms: composed,
        goal: goal.clone(),
        rationale: archetype.sme_summary.clone(),
        atom_rationales,
        resource_estimate,
    };

    validate_composition(&result, atom_reg)?;
    Ok(result)
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
pub fn compose_with_version_and_modalities_full(
    goal: &GoalSpec,
    project_class: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
    composer_version: u32,
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
) -> Result<ComposerOutput, CompositionError> {
    if composer_version == 4 {
        // v4 dispatch already takes the policy context; for
        // multi-modality v4 (cross-omics) we route through the same
        // entry — the v4 planner discovers cross-omics archetypes
        // through the same archetype registry the legacy path uses.
        // Thread the full modality slice so the
        // PlanningContext.intent has primary modality + project class
        // populated.
        return compose_v4_dispatch_full(
            goal,
            project_class,
            atom_reg,
            archetype_reg,
            target_modalities,
            policy_ctx,
            opaque_sink,
            opaque_session_id,
        );
    }
    let composition = compose_with_version_and_modalities(
        goal,
        project_class,
        atom_reg,
        archetype_reg,
        composer_version,
        target_modalities,
    )?;
    Ok(ComposerOutput::legacy(composition))
}
