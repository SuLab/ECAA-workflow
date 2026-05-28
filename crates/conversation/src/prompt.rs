//! System prompt assembly with prompt-cache markers.

use crate::session::Session;
use ecaa_workflow_core::project_class::ProjectClass;

/// Confirmation-card total character budget. The LLM is instructed in
/// `prompt_role.txt` to keep `card_body` within this; the validator below
/// (and in `tools/hypothesized_renderer`) rejects exceeding outputs.
/// Drift between prompt + validator causes silent rejection loops — both
/// must reference this single constant.
///
/// Step-3 path choice (magic-numbers Task B2): the prompt template lives
/// in `prompt_role.txt` (loaded via `include_str!`), not a Rust string
/// literal — `format!` interpolation would require placeholder tokens in
/// the text file, fragmenting reader-visible content from effective
/// contents. The plan explicitly allows leaving the literals in the
/// template and fixing the validator side. The
/// `prompt_role_pins_confirmation_card_budgets` test below now consumes
/// these constants, so any change to either requires a paired prompt
/// update — closing the drift loop without prompt-template surgery.
pub const CONFIRMATION_CARD_TOTAL_MAX_CHARS: usize = 800;

/// Claim-boundary sub-section budget. Paired with the total budget above.
pub const CONFIRMATION_CARD_CLAIM_BOUNDARY_MAX_CHARS: usize = 200;

/// One block in the assembled system prompt, with optional
/// Anthropic prompt-cache metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemPromptBlock {
    /// Text content of this prompt block.
    pub text: String,
    /// True if this block should carry `cache_control: ephemeral`.
    pub cache: bool,
}

const ROLE_AND_STYLE: &str = include_str!("prompt_role.txt");

// Per-class prompt blocks, embedded via `include_str!` so they ship
// with the binary and live in the crate's source tree (ops-friendly,
// matches prompt_role.txt pattern).
const CLINICAL_TRIAL_CLASS_PROMPT: &str = include_str!("project_class_prompts/clinical_trial.txt");
const TIME_SERIES_CLASS_PROMPT: &str =
    include_str!("project_class_prompts/time_series_forecast.txt");

/// Returns the per-class prompt block, or `None` for bioinformatics
/// (base `prompt_role.txt` is already bio-framed; emitting an empty
/// block would burn a cache breakpoint for zero model-visible gain).
pub(crate) fn class_prompt_for(class: ProjectClass) -> Option<&'static str> {
    match class {
        ProjectClass::Bioinformatics => None,
        ProjectClass::ClinicalTrial => Some(CLINICAL_TRIAL_CLASS_PROMPT),
        ProjectClass::TimeSeriesForecast => Some(TIME_SERIES_CLASS_PROMPT),
    }
}

/// At or past
/// this streak count, `build_system_prompt` appends the convergence
/// nudge so the LLM stops looping clarifying questions and surfaces
/// `propose_summary_confirmation`. Tracked on
/// `Session::intake_followup_streak` (bumped/reset in
/// `Session::note_turn_end_intake_followup`, called once per
/// `tool_loop::run_tool_loop` exit).
pub const INTAKE_FOLLOWUP_CONVERGENCE_THRESHOLD: u32 = 4;

/// Assemble the ordered list of system-prompt blocks for a session.
/// Static blocks are marked `cache: true` to benefit from prompt caching.
pub fn build_system_prompt(session: &Session) -> Vec<SystemPromptBlock> {
    // Tool names + descriptions are emitted only via the Anthropic
    // API's `tools` request field (see `client.rs`), not duplicated
    // into a system-prompt list. A duplicated "AVAILABLE TOOLS: - name
    // — description" block would be redundant with `tools` and eat
    // ~500 bytes of cacheable prefix for no model-visible gain.
    let mut blocks = Vec::with_capacity(4);

    blocks.push(SystemPromptBlock {
        text: ROLE_AND_STYLE.to_string(),
        cache: true,
    });

    // The per-class block is cached (stable per project_class), placed
    // *after* the base role block so the cache prefix stays stable
    // across sessions of the same class. Bio sessions emit zero
    // class-block delta.
    if let Some(class_text) = class_prompt_for(session.project_class) {
        blocks.push(SystemPromptBlock {
            text: class_text.to_string(),
            cache: true,
        });
    }

    if let Some(tax) = &session.taxonomy {
        // Earlier revisions of this block left the taxonomy uncached,
        // reasoning that the §3.8 conversation-tail marker extends the
        // cacheable prefix past it. That reasoning was wrong for the
        // cross-turn case: the uncached `format_session_state` block
        // below changes every turn, so the prefix-up-to-conversation-
        // tail never matches a prior turn's cache. Only the role and
        // (optional) class markers hit cross-turn. Without a dedicated
        // marker here, the taxonomy block (5–20 KB for large taxonomies
        // like gwas-coloc) bills at full input rate on the first
        // iteration of every new user turn.
        //
        // For bio sessions we have a free marker slot (no class block),
        // so flipping cache: true gives us role + taxonomy + conv-tail
        // + tool_exchange-tail = 4 markers — exactly at Anthropic's 4-
        // breakpoint cap. For non-bio sessions the class block already
        // occupies a marker; adding a taxonomy marker would push us to
        // 5 and the API would 400 (enforced in client.rs's debug-
        // assert). Non-bio taxonomies are also smaller (clinical-
        // trial-analysis.yaml, time-series-forecast.yaml), so the lost
        // cross-turn savings are modest.
        let cache_taxonomy = matches!(session.project_class, ProjectClass::Bioinformatics);
        blocks.push(SystemPromptBlock {
            text: format_taxonomy_info(tax),
            cache: cache_taxonomy,
        });
    }

    blocks.push(SystemPromptBlock {
        text: format_session_state(session),
        cache: false,
    });

    // convergence nudge after `INTAKE_FOLLOWUP_CONVERGENCE_THRESHOLD`
    // consecutive `IntakeFollowup` turns. The LLM has no internal
    // trigger to surface `propose_summary_confirmation`; without an
    // explicit SME signal it can loop in `IntakeFollowup` indefinitely
    // (variant-calling, single-cell, edge-case-multiomics, paper-07,
    // paper-10 all hit this).
    //
    // Placed AFTER the uncached session-state block so it shares the
    // uncached suffix — toggling the nudge on/off across turns does
    // not invalidate the cacheable prefix (role + class + taxonomy).
    // The deterministic gates on confirmation/emit are unchanged; this
    // is a UX convergence signal, not an LLM bypass.
    if session.intake_followup_streak >= INTAKE_FOLLOWUP_CONVERGENCE_THRESHOLD {
        blocks.push(SystemPromptBlock {
            text: String::from(
                "## CONVERGENCE NUDGE\n\
                 The SME has answered four consecutive clarifying questions on this \
                 intake. Unless a HARD blocker remains (data missing, contract \
                 violation, contradictory constraints), you have enough detail to \
                 surface the package summary. Call propose_summary_confirmation on \
                 the next turn — do not ask another question. If a hard blocker \
                 remains, state it explicitly and call propose_summary_confirmation \
                 anyway with the blocker flagged.\n",
            ),
            cache: false,
        });
    }

    blocks
}

fn format_taxonomy_info(tax: &ecaa_workflow_core::taxonomy::StageTaxonomy) -> String {
    let mut out = String::from("LOADED TAXONOMY:\n");
    out.push_str(&format!("  id: {}\n", tax.id));
    out.push_str(&format!("  domain: {}\n", tax.domain));
    out.push_str(&format!("  description: {}\n", tax.description));
    if let Some(cb) = &tax.claim_boundary {
        out.push_str(&format!("  claim_boundary: {}\n", cb));
    }
    out.push_str(&format!("  stages ({}):\n", tax.stages.len()));
    for stage in &tax.stages {
        // §3.6 — flag-gated slimming. When ECAA_SLIM_TAXONOMY=1, emit
        // only the stage id (+ claim boundary if any, above) instead
        // of the per-stage description. The LLM still has
        // `get_taxonomy_info` available for detail lookups when a
        // specific stage matters. Typical savings: 5–20 KB per
        // session (one-time cache write; amortized on cache reads).
        if slim_taxonomy_enabled() {
            out.push_str(&format!("    - {}\n", stage.id));
        } else {
            out.push_str(&format!(
                "    - {}: {}\n",
                stage.id,
                stage.description.lines().next().unwrap_or("")
            ));
        }
    }
    out
}

fn slim_taxonomy_enabled() -> bool {
    ecaa_workflow_core::env_helpers::env_bool("ECAA_SLIM_TAXONOMY")
}

fn format_session_state(session: &Session) -> String {
    let mut out = String::from("CURRENT SESSION STATE:\n");
    out.push_str(&format!("  state: {:?}\n", session.state));
    // `is_confirmed()` is a three-way check (token present + pending
    // emission set + summary hash matches). The LLM only needs the
    // boolean projection here so the prompt formatter stays unchanged
    // on the wire; the underlying gate refuses emit when the plan
    // summary drifts after confirmation.
    out.push_str(&format!("  user_confirmed: {}\n", session.is_confirmed()));
    out.push_str(&format!(
        "  project_class: {}\n",
        session.project_class.as_str()
    ));
    if let Some(c) = &session.classification {
        out.push_str(&format!(
            "  modality: {} ({:.0}% — {})\n",
            c.modality,
            c.confidence * 100.0,
            c.confidence_label
        ));
    } else {
        out.push_str("  modality: (unclassified)\n");
    }
    if !session.intake_methods.0.is_empty() {
        out.push_str(&format!(
            "  intake_methods: {} resolved\n",
            session.intake_methods.0.len()
        ));
    }
    if let Some(d) = &session.dag {
        let (completed, ready, blocked, pending) = d.progress();
        out.push_str(&format!(
            "  dag: {} tasks ({} completed, {} ready, {} blocked, {} pending)\n",
            d.tasks.len(),
            completed,
            ready,
            blocked,
            pending
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
    // `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
    // edition because the env table is not thread-safe). All call sites
    // are single-threaded test setup/teardown; the bounded waiver is
    // scoped to this `mod tests` block.
    #![allow(unsafe_code)]
    use super::*;

    #[test]
    fn role_block_is_present_and_cached() {
        let session = Session::new(false);
        let blocks = build_system_prompt(&session);
        assert!(blocks.len() >= 2);
        assert!(blocks[0].cache);
        assert!(blocks[0].text.contains("expert operator assistant"));
    }

    #[test]
    fn schema_list_block_not_rendered() {
        // §3.11: the old "AVAILABLE TOOLS:\n\n- name — description" block
        // was redundant with the Anthropic API `tools` field. Regression
        // guard: assert that characteristic list header no longer
        // appears in any system-prompt block. Tool names are still
        // allowed to appear in prose (rules about tool use), just not
        // in a schema dump.
        let session = Session::new(false);
        let blocks = build_system_prompt(&session);
        for block in &blocks {
            assert!(
                !block.text.contains("AVAILABLE TOOLS:"),
                "system prompt block still carries the dedup'd tool schema list"
            );
        }
    }

    #[test]
    fn session_state_block_is_uncached() {
        let session = Session::new(false);
        let blocks = build_system_prompt(&session);
        let last = blocks.last().unwrap();
        assert!(!last.cache);
        assert!(last.text.contains("CURRENT SESSION STATE"));
    }

    #[test]
    fn no_taxonomy_yields_two_blocks() {
        let session = Session::new(false);
        let blocks = build_system_prompt(&session);
        // §3.11: no taxonomy → 2 blocks (role, state).
        // §8.B.4: bio is the default project class; class block is None
        // for bio, so this stays at 2.
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn non_bio_class_emits_cached_class_block() {
        // §8.B.4: ClinicalTrial adds one additional cached block after
        // the base role block. No taxonomy → 3 blocks total (role,
        // class, state).
        let mut session = Session::new(false);
        session.project_class = ProjectClass::ClinicalTrial;
        let blocks = build_system_prompt(&session);
        assert_eq!(blocks.len(), 3);
        assert!(blocks[0].cache);
        assert!(blocks[1].cache);
        assert!(blocks[1].text.contains("Statistical Analysis Plan"));
        assert!(blocks[1].text.contains("VOCABULARY vs. RECOMMENDATION"));
    }

    #[test]
    fn time_series_class_emits_time_series_block() {
        let mut session = Session::new(false);
        session.project_class = ProjectClass::TimeSeriesForecast;
        let blocks = build_system_prompt(&session);
        assert_eq!(blocks.len(), 3);
        assert!(blocks[1].text.contains("ARIMA"));
        assert!(blocks[1].text.contains("stationarity"));
    }

    #[test]
    fn session_state_surfaces_project_class() {
        let mut session = Session::new(false);
        session.project_class = ProjectClass::ClinicalTrial;
        let blocks = build_system_prompt(&session);
        let state_block = blocks.last().unwrap();
        assert!(state_block.text.contains("project_class: clinical_trial"));
    }

    #[test]
    fn class_prompt_for_bio_is_none() {
        assert!(class_prompt_for(ProjectClass::Bioinformatics).is_none());
        assert!(class_prompt_for(ProjectClass::ClinicalTrial).is_some());
        assert!(class_prompt_for(ProjectClass::TimeSeriesForecast).is_some());
    }

    #[test]
    fn deterministic_assembly_for_same_input() {
        let session = Session::new(false);
        let a = build_system_prompt(&session);
        let b = build_system_prompt(&session);
        assert_eq!(a, b);
    }

    /// Phase B4 — synthesize a `StageTaxonomy` metadata holder
    /// without touching disk. Pre-B4 these tests loaded
    /// `config/stage-taxonomies/single-cell.yaml`; the YAMLs are
    /// gone, so the test now constructs the minimal metadata shape
    /// the prompt-block path consumes.
    fn synthetic_single_cell_taxonomy() -> ecaa_workflow_core::taxonomy::StageTaxonomy {
        ecaa_workflow_core::taxonomy::StageTaxonomy {
            id: "single_cell".into(),
            domain: "computational biology".into(),
            description: "single-cell RNA-seq composition (synthesized for prompt tests)".into(),
            policies: None,
            claim_boundary: Some(
                "Do not claim biological causality; statistical associations only.".into(),
            ),
            validation_contract_ref: None,
            project_class: None,
            preferred_container: None,
            runtime_baseline: Default::default(),
            stages: Vec::new(),
        }
    }

    fn synthetic_clinical_trial_taxonomy() -> ecaa_workflow_core::taxonomy::StageTaxonomy {
        ecaa_workflow_core::taxonomy::StageTaxonomy {
            id: "clinical_trial".into(),
            domain: "clinical research".into(),
            description: "clinical trial analysis (synthesized for prompt tests)".into(),
            policies: None,
            claim_boundary: None,
            validation_contract_ref: None,
            project_class: Some(ProjectClass::ClinicalTrial),
            preferred_container: None,
            runtime_baseline: Default::default(),
            stages: Vec::new(),
        }
    }

    #[test]
    fn bio_taxonomy_block_is_cached() {
        // A bio session with a loaded taxonomy must cache the taxonomy
        // block so its bytes cache-read cross-turn instead of billing
        // at full input rate. Regression guard.
        let tax = synthetic_single_cell_taxonomy();
        let mut session = Session::new(false);
        session.taxonomy = Some(tax);
        assert_eq!(session.project_class, ProjectClass::Bioinformatics);

        let blocks = build_system_prompt(&session);
        // role, taxonomy, session_state — 3 blocks for bio w/ tax
        assert_eq!(blocks.len(), 3);
        assert!(blocks[0].cache, "role must be cached");
        assert!(blocks[1].cache, "taxonomy must be cached for bio");
        assert!(blocks[1].text.starts_with("LOADED TAXONOMY:"));
        assert!(!blocks[2].cache, "session state stays uncached");
    }

    #[test]
    fn non_bio_taxonomy_block_is_not_cached() {
        // Marker-budget invariant: non-bio sessions already spend a
        // marker on the class block (role + class + conv-tail +
        // tool_exchange-tail = 4, at the cap). Caching a non-bio
        // taxonomy block would push to 5 and Anthropic would 400.
        let tax = synthetic_clinical_trial_taxonomy();
        let mut session = Session::new(false);
        session.project_class = ProjectClass::ClinicalTrial;
        session.taxonomy = Some(tax);

        let blocks = build_system_prompt(&session);
        // role, class, taxonomy, session_state — 4 blocks for non-bio w/ tax
        assert_eq!(blocks.len(), 4);
        assert!(blocks[0].cache, "role must be cached");
        assert!(blocks[1].cache, "class must be cached");
        assert!(
            !blocks[2].cache,
            "non-bio taxonomy MUST NOT be cached (would exceed 4-marker budget)"
        );
        assert!(!blocks[3].cache, "session state stays uncached");
    }

    #[test]
    fn slim_taxonomy_drops_per_stage_descriptions() {
        // §3.6 — flag-gated. When enabled, stage lines carry only the
        // stage id (no description), shrinking the taxonomy block.
        // Post-B4 the metadata holder carries no stages so the test
        // synthesizes a couple of `StageSpec`s explicitly.
        use ecaa_workflow_core::taxonomy::{DiscoveryRequirement, StageCardinality, StageSpec};
        let mut tax = synthetic_single_cell_taxonomy();
        tax.stages = vec![
            StageSpec {
                id: "alignment".into(),
                class: "operation".into(),
                discovery: DiscoveryRequirement::None,
                depends_on: vec![],
                assignee: None,
                description: "Align reads against the reference genome.".into(),
                role: Default::default(),
                cardinality: StageCardinality::default(),
                expansion_source: None,
                expansion_instructions: None,
                condition: None,
                edam_operation: None,
                method_prose: None,
                variants: vec![],
                resource_class: None,
                requires_sme_review: None,
                required_figures: vec![],
                plot_stage_id: None,
                figure_exempt: None,
                expected_artifacts: vec![],
                spec_preferred_methods: std::collections::BTreeMap::new(),
                claim_boundary: None,
                checkpoint_level: None,
                required_artifacts: vec![],
                validators: vec![],
            },
            StageSpec {
                id: "differential_expression".into(),
                class: "operation".into(),
                discovery: DiscoveryRequirement::None,
                depends_on: vec!["alignment".into()],
                assignee: None,
                description: "Compute differential expression across conditions.".into(),
                role: Default::default(),
                cardinality: StageCardinality::default(),
                expansion_source: None,
                expansion_instructions: None,
                condition: None,
                edam_operation: None,
                method_prose: None,
                variants: vec![],
                resource_class: None,
                requires_sme_review: None,
                required_figures: vec![],
                plot_stage_id: None,
                figure_exempt: None,
                expected_artifacts: vec![],
                spec_preferred_methods: std::collections::BTreeMap::new(),
                claim_boundary: None,
                checkpoint_level: None,
                required_artifacts: vec![],
                validators: vec![],
            },
        ];

        unsafe { std::env::remove_var("ECAA_SLIM_TAXONOMY") };
        let full = format_taxonomy_info(&tax);
        unsafe { std::env::set_var("ECAA_SLIM_TAXONOMY", "1") };
        let slim = format_taxonomy_info(&tax);
        unsafe { std::env::remove_var("ECAA_SLIM_TAXONOMY") };

        assert!(
            slim.len() < full.len(),
            "slim ({}) should be shorter than full ({})",
            slim.len(),
            full.len()
        );
        // Slim output keeps stage ids but drops per-stage descriptions.
        assert!(slim.contains("stages ("));
    }

    /// R10/R11 confirmation-card length budget regression.
    ///
    /// The prompt's HARD LIMITS block declares two numeric budgets:
    /// 1. "Total length: ≤ 800 characters"
    /// 2. "claim_boundary section: ≤ 200 characters"
    ///
    /// Drift in either limit (silently bumping it up via prompt edits
    /// without a paired update to the BAD examples / scorer rubric)
    /// would weaken the per-card discipline the auto-confirm path
    /// relies on. This test pins both numbers as the canonical strings
    /// the prompt has to keep saying. Keep this in lockstep with
    /// `runner/scorer_prompt.txt`'s claim-boundary scoring rubric.
    #[test]
    fn prompt_role_pins_confirmation_card_budgets() {
        let total_needle = format!(
            "Total length: ≤ {} characters",
            CONFIRMATION_CARD_TOTAL_MAX_CHARS
        );
        assert!(
            ROLE_AND_STYLE.contains(&total_needle),
            "prompt_role.txt no longer pins the {}-char total-card budget; \
             update this test if the budget legitimately changed (also touch \
             runner/scorer_prompt.txt) — drift is the bug this test guards",
            CONFIRMATION_CARD_TOTAL_MAX_CHARS
        );
        let cb_needle = format!(
            "claim_boundary section: ≤ {} characters",
            CONFIRMATION_CARD_CLAIM_BOUNDARY_MAX_CHARS
        );
        assert!(
            ROLE_AND_STYLE.contains(&cb_needle),
            "prompt_role.txt no longer pins the {}-char claim_boundary \
             budget; same rules as above re: legitimate change",
            CONFIRMATION_CARD_CLAIM_BOUNDARY_MAX_CHARS
        );
    }

    /// Every worked confirmation-card example in
    /// `prompt_role.txt` must obey the limits the prompt itself
    /// declares (≤800 chars total, ≤200 chars claim_boundary). When
    /// a contributor edits a worked example past the budget without
    /// noticing, this test fails — surfacing the drift in the
    /// example-vs-rule pair before the model trains on inconsistent
    /// guidance.
    ///
    /// Worked examples are tagged with `Worked example A — `,
    /// `Worked example B — `, `Worked example C — ` headers and
    /// terminate at the next blank-line-followed-by-non-indented-text
    /// boundary or the next `Worked example ` header.
    #[test]
    fn prompt_role_worked_examples_obey_their_own_budgets() {
        let mut starts: Vec<usize> = ROLE_AND_STYLE
            .match_indices("Worked example ")
            .map(|(i, _)| i)
            .collect();
        starts.push(ROLE_AND_STYLE.len());
        assert!(
            starts.len() >= 4, // at least 3 examples + the synthetic end
            "expected ≥3 worked examples in prompt_role.txt; found {}",
            starts.len() - 1
        );
        for window in starts.windows(2) {
            let body = &ROLE_AND_STYLE[window[0]..window[1]];
            // Each worked example contains an indented `## ` headline
            // followed by the card body up to the next blank-line +
            // non-indented-text marker. We look for the ## headline
            // line and grab the lines below it that are indented (the
            // worked-card body uses 2-space indent throughout).
            let card_body: String = body
                .lines()
                .skip_while(|l| !l.trim_start().starts_with("##"))
                .take_while(|l| l.starts_with("  ") || l.is_empty())
                .map(|l| l.trim_start().to_string())
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
            assert!(
                card_body.chars().count() <= CONFIRMATION_CARD_TOTAL_MAX_CHARS,
                "worked-example card body exceeds the {}-char total budget \
                 the prompt declares: {} chars in:\n{}",
                CONFIRMATION_CARD_TOTAL_MAX_CHARS,
                card_body.chars().count(),
                card_body
            );
            // The claim_boundary is the paragraph between the headline
            // line and the first bullet (`- `). Empty paragraphs (e.g.
            // the trailing `Confirm to lock in or correct any.` line)
            // don't count.
            let cb: String = card_body
                .lines()
                .skip_while(|l| !l.starts_with("##"))
                .skip(1) // headline
                .skip_while(|l| l.is_empty())
                .take_while(|l| !l.starts_with("- "))
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                cb.chars().count() <= CONFIRMATION_CARD_CLAIM_BOUNDARY_MAX_CHARS,
                "worked-example claim_boundary exceeds the {}-char budget \
                 the prompt declares: {} chars in:\n{}",
                CONFIRMATION_CARD_CLAIM_BOUNDARY_MAX_CHARS,
                cb.chars().count(),
                cb
            );
        }
    }
}
