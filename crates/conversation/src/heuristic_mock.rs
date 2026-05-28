//! Heuristic (non-scripted) `LlmBackend` for fixture-driven tests
//! that want to exercise the actual tool-selection decision pipeline
//! rather than replay a hard-coded tape.
//!
//! The default [`MockLlmBackend`](crate::MockLlmBackend) is a strict
//! tape recorder: every fixture spells out the exact `TurnResponse`
//! sequence the backend should emit, which means the tool dispatcher,
//! the alone-in-turn enforcement, and the state-machine reactions all
//! run, but the *choice* of which tool to invoke next is never under
//! test — a fixture can pass while the live LLM is making
//! catastrophically wrong picks, so long as the scripted picks land
//! the session at `expected_final_state`.
//!
//! `HeuristicMockBackend` closes that gap with the smallest viable
//! oracle: a handful of state + content rules drive tool selection.
//! It is a *first-draft* foundation, not a complete substitute for
//! a real LLM — further tiers widen the confidence model, ambiguity
//! detection, refusal logic, batching, and error injection.
//!
//! # Decision Table
//!
//! Each row is one rule the [`HeuristicMockBackend::decide`] dispatch
//! consults in order. The Tier column reflects the incremental expansion
//! of the oracle: Tier 0 covers the foundation rules and Tiers 1–5 widen
//! the oracle's reach. The Helper column names the sibling
//! module the rule's body delegates to (Tiers 1–5) or `(inline)` for
//! the foundation rules that live in `decide` itself.
//!
//! | Tier | Signal | Action | Helper |
//! |---|---|---|---|
//! | 0 | Conversation empty | Greeting text | (inline) |
//! | 1 | Confidence < 0.3 | `propose_quick_replies` | `heuristic_confidence` |
//! | 2 | SME requested method recommendation | Refusal response | `heuristic_refusal` |
//! | 3 | `with_batch_strategy(BatchedReadOnly)` | Batched tool response | `heuristic_batch` |
//! | 4 | `with_failure_at(tool, reason)` | Synthetic failing `tool_use` | `heuristic_failure` |
//! | 5 | Cross-omics conjunction present | `propose_quick_replies` for archetype | `heuristic_cross_omics` |
//! | 0 | First user prose, no `classify_intake` yet | `classify_intake` | (inline) |
//! | 0 | `classify_intake` ran, no mutation yet | `append_intake_prose` | (inline) |
//! | 0 | Mutation observed, no summary yet | `propose_summary_confirmation` | (inline) |
//! | 0 | Summary observed + SME-confirm inferred | `emit_package` (alone-in-turn) | (inline) |
//! | 0 | `emit_package` ran | Closing text | (inline) |
//! | 0 | Fallback | `(ack)` text (ends loop) | (inline) |
//!
//! The Tier 0 foundation rules deliberately do not consult `Session`
//! directly — the backend trait only exposes the `TurnRequest`, so the
//! heuristic reads `request.conversation` for SME prose and
//! `request.tool_exchange` for the already-issued tool-call ledger.
//! The state-machine progress signal for the confirmation step is
//! inferred from a `tool_result` containing a
//! `pending_confirmation_cleared` marker that the fixture runner
//! injects between the `propose_summary` turn and the post-confirm
//! turn; that marker is the only piece of out-of-band glue.
//!
//! Each Tier 1–5 rule lives in a sibling file (`heuristic_confidence`,
//! `heuristic_refusal`, `heuristic_batch`, `heuristic_failure`,
//! `heuristic_cross_omics`) so adding a new branch doesn't widen this
//! module's surface area. The corresponding fixtures live in
//! `tests/conversation-fixtures/fixtures/` under the `heuristic_`
//! prefix (`heuristic_low_confidence`, `heuristic_method_*`,
//! `heuristic_batch_*`, `heuristic_failure_*`, `heuristic_multi_modality_*`).

use crate::anthropic::{LlmBackend, StopReason, TurnRequest, TurnResponse, Usage};
use crate::heuristic_batch::{build_batched_response, BatchStrategy};
use crate::heuristic_confidence::{estimate_confidence, ConfidenceEstimate};
use crate::heuristic_failure::FailureInjection;
use crate::heuristic_refusal::{detect_method_mention, MethodMention};
use crate::tools::{BatchableTool, HighImpactTool, Tool};
use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

/// Stateless tool-selection oracle that picks a `TurnResponse` from
/// the request context rather than a scripted tape. See module docs
/// for the decision table. Always-safe to construct; carries no
/// configuration today (knobs land alongside the confidence-model
/// phase in the roadmap).
pub struct HeuristicMockBackend {
    /// Optional confirm-bypass override. When `true` the backend
    /// treats every turn after the first `propose_summary_confirmation`
    /// as confirmed (skips the SME-click rule). Tests that want to
    /// exercise the rejection path leave this `false` (the default).
    auto_confirm: bool,
    /// Phase-3 batching strategy. Default ([`BatchStrategy::Single`])
    /// preserves the one-tool-per-response shape. Non-default values
    /// route the decision table's chosen tool through
    /// [`build_batched_response`] so the dispatcher's alone-in-turn
    /// enforcement runs against a non-scripted backend.
    batch_strategy: BatchStrategy,
    /// Optional one-shot failure injection. When set, the heuristic's
    /// next decision that would emit a tool matching
    /// `FailureInjection::tool_name` instead emits a synthetic tool
    /// call the deterministic dispatcher rejects, surfacing a
    /// `ToolError::ValidationFailure` in the tool ledger. The
    /// injection's latch flips after the first match, so subsequent
    /// turns fall back to the normal decision table.
    failure_injection: Option<FailureInjection>,
}

impl Default for HeuristicMockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl HeuristicMockBackend {
    /// Create a default-configured `HeuristicMockBackend`.
    pub fn new() -> Self {
        Self {
            auto_confirm: false,
            batch_strategy: BatchStrategy::default(),
            failure_injection: None,
        }
    }

    /// Builder toggle for `auto_confirm`. Leaves the production
    /// confirmation gate in place: the heuristic still requires the
    /// fixture runner to fire `service.confirm(session_id)` so the
    /// `ConfirmationToken` latch is set; the toggle only changes how
    /// the heuristic *infers* "the SME has confirmed" from the prior
    /// tool ledger.
    pub fn with_auto_confirm(mut self, on: bool) -> Self {
        self.auto_confirm = on;
        self
    }

    /// Builder toggle for the batching strategy. Default
    /// ([`BatchStrategy::Single`]) preserves the one-tool-per-response
    /// shape; non-default values route the decision table's chosen
    /// tool through [`build_batched_response`] so the dispatcher's
    /// alone-in-turn enforcement runs against a non-scripted backend.
    pub fn with_batch_strategy(mut self, strategy: BatchStrategy) -> Self {
        self.batch_strategy = strategy;
        self
    }

    /// Wrap a chosen tool in a `TurnResponse`, consulting
    /// `self.batch_strategy` AND `self.failure_injection`. The failure
    /// injection takes precedence: if it fires, the synthetic failing
    /// tool call is emitted (bypassing batching). Otherwise:
    /// `BatchStrategy::Single` (the default) emits exactly one
    /// `tool_use`; non-default strategies fan the chosen tool into a
    /// multi-tool batch via [`build_batched_response`] so the
    /// dispatcher's alone-in-turn enforcement is exercised.
    fn tool_response(&self, tool: Tool) -> TurnResponse {
        if let Some(fi) = &self.failure_injection {
            if fi.check_and_fire(tool.name()).is_some() {
                return tool_use(fi.synthetic_failing_tool_call());
            }
        }
        match &self.batch_strategy {
            BatchStrategy::Single => tool_use(tool),
            other => build_batched_response(other, tool),
        }
    }

    /// Install a one-shot failure injection. The next heuristic decision
    /// that would dispatch a tool whose name matches `tool_name` instead
    /// emits a synthetic call the deterministic dispatcher rejects with
    /// `ToolError::ValidationFailure`. After the injection has fired,
    /// subsequent decisions fall back to the normal table — fixtures
    /// that want repeated failures must install a fresh backend.
    ///
    /// See `crates/conversation/src/heuristic_failure.rs`.
    pub fn with_failure_at(
        mut self,
        tool_name: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        self.failure_injection = Some(FailureInjection::new(tool_name, reason));
        self
    }

    /// Pick the next `TurnResponse` based on the prior conversation +
    /// tool exchange. Pure; no I/O.
    fn decide(&self, request: &TurnRequest) -> TurnResponse {
        // `tool_exchange` is the per-turn ledger of tools fired during
        // the current `send_turn` (resets between user turns).
        // `Turn.tool_calls` on the committed conversation carries the
        // session-wide history. Track both: the per-turn list tells us
        // what we already did in this loop pass; the historical list
        // tells us state from prior turns.
        let current_turn_calls = collect_tool_names_from_exchange(&request.tool_exchange);
        let history_calls = collect_tool_names_from_conversation(&request.conversation);
        let mut tool_names_called = history_calls.clone();
        tool_names_called.extend(current_turn_calls.iter().cloned());
        let latest_user_prose = latest_user_prose(&request.conversation);

        // Rule 1: greeting acknowledgement — no SME prose yet.
        if latest_user_prose.is_none() {
            return assistant_text(
                "Hello — describe your bioinformatics project when you're ready.",
            );
        }
        let prose = latest_user_prose.unwrap();

        // Rule 6: `emit_package` already fired — closing text only.
        if tool_names_called.iter().any(|n| n == "emit_package") {
            return assistant_text(
                "Package is written. The execution agent will pick up from here.",
            );
        }

        // Rule 5: post-confirmation emission.
        //
        // Emission requires the SME to click Confirm between turns;
        // the heuristic can't directly observe that click, so it infers
        // confirmation from the turn-boundary signal: `propose_summary
        // _confirmation` fired in a prior committed turn, NOT in the
        // current `tool_exchange`. That implies the prior turn ended
        // cleanly and a fresh user-driven turn is now driving the
        // post-summary follow-up — which only happens after the SME
        // clicked Confirm (Reject rewrites the summary and re-loops).
        //
        // The `auto_confirm` toggle and `pending_confirmation_cleared`
        // marker are escape hatches for tests that want to drive
        // emission without a second user turn.
        let summary_proposed_history = history_calls
            .iter()
            .any(|n| n == "propose_summary_confirmation");
        let summary_proposed_current = current_turn_calls
            .iter()
            .any(|n| n == "propose_summary_confirmation");
        let summary_proposed = summary_proposed_history || summary_proposed_current;
        // `auto_confirm` short-circuit must require `summary_proposed_history`,
        // not `summary_proposed`. Firing `emit_package` in the same loop
        // iteration as `propose_summary_confirmation` lands before any SME
        // click can mint a `ConfirmationToken`, so emit refuses on
        // `is_confirmed()==false`. Waiting for a prior-turn `propose_summary
        // _confirmation` guarantees the confirmation gate has been crossed.
        let confirmation_observed = (summary_proposed_history
            && (!summary_proposed_current || self.auto_confirm))
            || pending_confirmation_cleared(&request.tool_exchange);
        let _ = summary_proposed;
        if confirmation_observed {
            eprintln!(
                "[heuristic] emit_package fired (summary_proposed_history={} summary_proposed_current={} auto_confirm={} pending_cleared={})",
                summary_proposed_history,
                summary_proposed_current,
                self.auto_confirm,
                pending_confirmation_cleared(&request.tool_exchange)
            );
            return self.tool_response(Tool::HighImpact(HighImpactTool::EmitPackage {
                output_dir: None,
            }));
        }
        eprintln!(
            "[heuristic] decide: prose={:?} tool_names_called={:?} summary_proposed_history={} summary_proposed_current={}",
            prose.chars().take(40).collect::<String>(), tool_names_called, summary_proposed_history, summary_proposed_current
        );

        // Rule 2a: cross-omics routing.
        //
        // Fires BEFORE the confidence-band routing so prose that
        // carries an explicit cross-omics conjunction signal (e.g.
        // "joint analysis of X and Y") goes through the
        // `propose_quick_replies` archetype-confirmation round instead
        // of the mid-band classify→evidence path. The conjunction
        // signal is the load-bearing gate — semicolon / "also" prose
        // that just mentions two modalities does NOT match
        // (regression guard: fixture 66).
        //
        // Gated on `!classified && !cross_omics_proposed` so the
        // quick-replies card is shown once per session; subsequent
        // user turns fall through to the rest of the decision table.
        let classified_check = tool_names_called.iter().any(|n| n == "classify_intake");
        let cross_omics_proposed_check = tool_names_called
            .iter()
            .any(|n| n == "propose_quick_replies");
        if !classified_check && !cross_omics_proposed_check {
            if let Some(signal) = crate::heuristic_cross_omics::detect_cross_omics(prose) {
                if signal.modalities.len() >= 2 {
                    return tool_use(Tool::Batchable(
                        BatchableTool::ProposeQuickReplies {
                            question: format!(
                                "I see {} mentioned together. Should I plan a cross-omics joint analysis, or treat them as separate analyses?",
                                signal.modalities.join(" + ")
                            ),
                            options: vec![
                                "Yes, joint cross-omics analysis".to_string(),
                                "No, treat each modality separately".to_string(),
                            ],
                        },
                    ));
                }
            }
        }

        // Confidence-driven branching. Runs the keyword-table oracle on the
        // SME prose and forks the happy-path:
        //   confidence < 0.3  → propose_quick_replies (ask SME to pick a
        //                        modality before classification).
        //   < 0.7              → classify_intake then immediately
        //                        get_classification_evidence before any
        //                        mutation, so the SME-facing transcript
        //                        carries the rationale alongside the pick.
        //   ≥ 0.7              → fall through to the rule 2/3/4 happy path.
        // Both branches are loop-guarded against their own prior calls so
        // the dispatcher doesn't pin in an infinite quick-reply / evidence
        // cycle when the SME elaboration still lands in the same band.
        //
        // SKIPPED once cross-omics quick-replies already fired and the
        // SME's affirmation turn arrived (any new user turn after
        // propose_quick_replies counts as elaboration). The heuristic
        // then falls through to the regular classify→mutate→propose
        // happy-path so the cross-omics fan-out can reach emit_package.
        let confidence_estimate = estimate_confidence(prose);
        let classified = tool_names_called.iter().any(|n| n == "classify_intake");
        let already_proposed_quick_replies = tool_names_called
            .iter()
            .any(|n| n == "propose_quick_replies");
        let already_fetched_evidence = tool_names_called
            .iter()
            .any(|n| n == "get_classification_evidence");
        // Cross-omics gate: skip confidence-band routing only when
        // `propose_quick_replies` fired in a PRIOR committed turn (the
        // SME's affirmation is now driving). Within the same tool-loop
        // turn that just emitted quick-replies, the band's own
        // terminator must still run so the turn ends cleanly instead
        // of falling through to the happy path. Distinguishing
        // history- vs current-turn calls is the load-bearing
        // disambiguator (fixtures 58/59/65).
        let cross_omics_already_proposed =
            history_calls.iter().any(|n| n == "propose_quick_replies");
        // Low-confidence branch is additionally gated on `!classified` so
        // a keyword-free SME acknowledgement (e.g. `"(confirmed — please
        // continue)"`) on a post-classify turn does NOT re-route through
        // quick-replies (fixture 66). The mid-band branch is NOT
        // `!classified`-gated because it deliberately keeps firing
        // within the same turn after classify_intake to deliver
        // get_classification_evidence and the clarifying-ask terminator.
        if !classified && !cross_omics_already_proposed && confidence_estimate.confidence < 0.3 {
            if !already_proposed_quick_replies {
                return propose_quick_replies_response(&confidence_estimate);
            }
            // Quick-replies card is already out; end the turn cleanly
            // and wait for the SME's pick (the follow-up user turn will
            // re-enter decide() with the disambiguated prose).
            return assistant_text(
                "Pick the modality that best matches — I'll continue once you choose.",
            );
        }
        if !cross_omics_already_proposed && confidence_estimate.confidence < 0.7 {
            if !classified {
                return tool_use(Tool::Batchable(BatchableTool::ClassifyIntake {
                    prose: prose.to_string(),
                }));
            }
            if !already_fetched_evidence {
                return classify_then_fetch_evidence_response();
            }
            // Mid-band turn-ender: classify + evidence both fired this
            // user turn, so end the assistant turn with a clarifying
            // ask rather than continuing into mutation. The next SME
            // elaboration drives the follow-up; this prevents the
            // dispatcher from pinning into AppendIntakeProse →
            // ProposeSummaryConfirmation on still-ambiguous input.
            return assistant_text(
                "I see signals for more than one modality — could you say which one is primary, \
                 or whether this is a joint cross-omics analysis?",
            );
        }

        // Rule 4: propose summary once the classifier + mutation pass have run.
        let mutation_observed = tool_names_called
            .iter()
            .any(|n| n == "set_intake_field" || n == "append_intake_prose");

        // Neutrality / refusal arm. Sits between the
        // classify+mutation pass and the summary proposal so the
        // DAG has been built (set_intake_method validates against
        // `discover_<stage>` task ids) and the assistant text
        // refusal still lands in the same intake_followup window
        // the SME would see. Fires at most once per session: the
        // SmeNamed branch's `set_intake_method` call shows up in
        // `tool_names_called` on the next turn; the
        // SmeRequestedRecommendation branch ends the loop with an
        // alone-in-turn assistant text so it can't recurse.
        let method_arm_already_fired = tool_names_called.iter().any(|n| n == "set_intake_method");
        if classified && mutation_observed && !summary_proposed && !method_arm_already_fired {
            match detect_method_mention(prose) {
                MethodMention::SmeNamed { method } => {
                    return set_intake_method_response(&method);
                }
                MethodMention::SmeRequestedRecommendation { tool_category } => {
                    return refusal_response(&tool_category);
                }
                MethodMention::None => {}
            }
        }

        if classified && mutation_observed && !summary_proposed {
            return self.tool_response(Tool::Batchable(
                BatchableTool::ProposeSummaryConfirmation {
                    summary_markdown: synthesize_summary(prose),
                },
            ));
        }

        // Rule 3: intake-mutation pass after classification.
        if classified && !mutation_observed {
            return self.tool_response(Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: prose.to_string(),
            }));
        }

        // Rule 2: first SME prose → classify.
        if !classified {
            return self.tool_response(Tool::Batchable(BatchableTool::ClassifyIntake {
                prose: prose.to_string(),
            }));
        }

        // Rule 7: fallback — short acknowledgement so the tool loop ends.
        assistant_text("(ack)")
    }
}

#[async_trait]
impl LlmBackend for HeuristicMockBackend {
    async fn send_turn(&self, request: TurnRequest) -> Result<TurnResponse> {
        Ok(self.decide(&request))
    }

    async fn send_turn_streaming(
        &self,
        request: TurnRequest,
        on_delta: crate::anthropic::delta_sink::DeltaSink,
    ) -> Result<TurnResponse> {
        let resp = self.decide(&request);
        if !resp.assistant_content.is_empty() {
            on_delta(&resp.assistant_content);
        }
        Ok(resp)
    }

    async fn count_tokens(&self, _request: &TurnRequest) -> Result<Option<u32>> {
        Ok(Some(1))
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn assistant_text(s: &str) -> TurnResponse {
    TurnResponse {
        assistant_content: s.into(),
        tool_uses: vec![],
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
        request_metadata: Default::default(),
    }
}

fn tool_use(tool: Tool) -> TurnResponse {
    TurnResponse {
        assistant_content: String::new(),
        tool_uses: vec![(Uuid::new_v4(), tool)],
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
        request_metadata: Default::default(),
    }
}

// Confidence-band helpers ------------------------------------------------

/// Build a `propose_quick_replies` `TurnResponse` from a sub-0.3
/// confidence estimate. The question + options strings are synthesized
/// from the `ambiguous_modalities` list when present (the SME may have
/// hit multiple under-confident modalities) or fall back to a canned
/// "what modality" prompt when no modality keyword fired at all.
fn propose_quick_replies_response(estimate: &ConfidenceEstimate) -> TurnResponse {
    let (question, options) = if estimate.ambiguous_modalities.is_empty() {
        (
            "I couldn't tell which modality you have. Which best matches?".to_string(),
            vec![
                "scRNA-seq".to_string(),
                "Bulk RNA-seq".to_string(),
                "ATAC-seq".to_string(),
                "ChIP-seq".to_string(),
                "Whole-genome sequencing".to_string(),
                "Proteomics".to_string(),
                "Other / not sure".to_string(),
            ],
        )
    } else {
        (
            "Which modality should we prioritize?".to_string(),
            estimate.ambiguous_modalities.clone(),
        )
    };
    tool_use(Tool::Batchable(BatchableTool::ProposeQuickReplies {
        question,
        options,
    }))
}

/// Build a `get_classification_evidence` `TurnResponse` for the
/// mid-band (0.3 ≤ confidence < 0.7) path. The tool is read-only +
/// argument-free; the heuristic fires it right after `classify_intake`
/// so the SME-facing transcript carries the classification rationale
/// before any intake-mutation lands.
fn classify_then_fetch_evidence_response() -> TurnResponse {
    tool_use(Tool::Batchable(BatchableTool::GetClassificationEvidence))
}

// Neutrality / refusal helpers ------------------------------------------

/// Build a `set_intake_method` tool-call response for an SME-named
/// method. The stage defaults to `alignment` for aligner-family
/// methods and `differential_expression` for stats-family — but
/// the SME-signal gate at `tools::intake::set_intake_method` will
/// refuse the call regardless until the UI flips
/// `sme_method_signals.named` for that stage. The heuristic
/// returns the call anyway so the fixture can assert the
/// neutrality-arm decision was made (the refused call still appears
/// in `tool_call_log` with `is_error=true`).
fn set_intake_method_response(method: &str) -> TurnResponse {
    let stage = stage_for_method(method);
    tool_use(Tool::Batchable(BatchableTool::SetIntakeMethod {
        stage,
        method_prose: method.to_string(),
    }))
}

/// Map a known method token to the canonical `discover_<stage>`
/// stem it pins. Keeps the heuristic side aligned with the
/// builder's intake-method resolution so the SME-signal gate's
/// downstream validation can find the stage at all.
fn stage_for_method(method: &str) -> String {
    match method.to_ascii_uppercase().as_str() {
        "STAR" | "HISAT2" | "BOWTIE" | "BOWTIE2" | "BWA" | "BWA-MEM" | "MINIMAP2" | "SALMON"
        | "KALLISTO" | "TOPHAT" => "alignment".into(),
        "DESEQ2" | "EDGER" | "LIMMA" | "LIMMA-VOOM" => "differential_expression".into(),
        "COMBAT" | "HARMONY" | "MNN" | "SCVI" | "SCANORAMA" => "batch_correction".into(),
        "MAGIC" | "SCNORM" | "TMM" | "RPKM" | "FPKM" | "TPM" => "normalization".into(),
        "SEURAT" | "SCANPY" | "LEIDEN" | "LOUVAIN" | "PHENOGRAPH" => "clustering".into(),
        "GATK" | "FREEBAYES" | "DEEPVARIANT" | "STRELKA2" => "variant_calling".into(),
        "MACS2" | "MACS3" | "HOMER" | "PEAKCHIP" | "GENRICH" => "peak_calling".into(),
        _ => "alignment".into(),
    }
}

/// Refusal text mirroring the `prompt_role.txt` lines 487-507
/// neutrality contract. The fixture asserts on the verbatim
/// "the execution agent" anchor so any drift away from the
/// role-spec phrasing surfaces as a fixture failure rather than
/// silently shipping a recommendation.
fn refusal_response(tool_category: &str) -> TurnResponse {
    let category = if tool_category.trim().is_empty() {
        "method"
    } else {
        tool_category
    };
    assistant_text(&format!(
        "I won't recommend a specific {category} — that's the kind of judgment \
         the execution agent makes with the data in hand at runtime. If you \
         want to pin a particular choice, name it and I'll carry it through; \
         otherwise the execution agent picks based on properties of the actual \
         data and will check back if anything looks unusual."
    ))
}

/// Walk the latest user-role turn's `content` from the conversation
/// slice. Returns `None` when no user turn is present (e.g. the very
/// first send_turn fires on greeting boot).
fn latest_user_prose(conversation: &[crate::session::Turn]) -> Option<&str> {
    use crate::session::TurnRole;
    conversation
        .iter()
        .rev()
        .find(|t| matches!(t.role, TurnRole::User))
        .map(|t| t.content.as_str())
}

/// Pull every `tool_use.name` block out of the `tool_exchange`
/// ledger. Order-preserving so callers can pick the latest or check
/// for prior occurrences.
fn collect_tool_names_from_exchange(tool_exchange: &[serde_json::Value]) -> Vec<String> {
    let mut out = Vec::new();
    for entry in tool_exchange {
        let Some(role) = entry.get("role").and_then(|v| v.as_str()) else {
            continue;
        };
        if role != "assistant" {
            continue;
        }
        let Some(content) = entry.get("content").and_then(|v| v.as_array()) else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                if let Some(name) = block.get("name").and_then(|v| v.as_str()) {
                    out.push(name.to_string());
                }
            }
        }
    }
    out
}

/// Walk the session's committed conversation turns and lift every
/// `ToolCallRecord.tool_name` so the heuristic sees tool calls from
/// prior user-turn cycles (the per-turn `tool_exchange` ledger resets
/// each `send_turn`).
fn collect_tool_names_from_conversation(conversation: &[crate::session::Turn]) -> Vec<String> {
    let mut out = Vec::new();
    for turn in conversation {
        for rec in &turn.tool_calls {
            out.push(rec.tool_name.clone());
        }
    }
    out
}

/// Detect the `pending_confirmation_cleared` marker the fixture
/// runner injects into a `tool_result` between the propose-summary
/// turn and the post-confirm turn. Matched against the raw JSON
/// payload so any nested shape works (the fixture writer doesn't
/// have to mirror Anthropic's exact block layout).
fn pending_confirmation_cleared(tool_exchange: &[serde_json::Value]) -> bool {
    for entry in tool_exchange {
        let serialized = entry.to_string();
        if serialized.contains("pending_confirmation_cleared") {
            return true;
        }
    }
    false
}

/// Produce a deterministic summary markdown block from the SME's
/// latest prose. Truncates to 240 chars to keep the summary card
/// from running away on long pastes.
fn synthesize_summary(prose: &str) -> String {
    let trimmed = prose.trim();
    let snippet = if trimmed.len() > 240 {
        let mut cut = 240;
        while !trimmed.is_char_boundary(cut) {
            cut -= 1;
        }
        &trimmed[..cut]
    } else {
        trimmed
    };
    format!(
        "Here's what I have so far:\n\n{}\n\nClick Accept when this looks right.",
        snippet
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_policy::ModelId;
    use crate::session::{ToolCallRecord, Turn};
    use std::sync::Arc;

    fn empty_request() -> TurnRequest {
        TurnRequest {
            system_prompt: vec![],
            conversation: Arc::new(vec![]),
            tool_schemas: vec![],
            model: ModelId::Sonnet46,
            temperature: 0.4,
            max_tokens: 1024,
            tool_exchange: vec![],
            tool_choice: None,
        }
    }

    fn request_with_user(prose: &str) -> TurnRequest {
        let mut req = empty_request();
        req.conversation = Arc::new(vec![Turn::user(prose.to_string())]);
        req
    }

    fn append_assistant_tool_use(req: &mut TurnRequest, name: &str) {
        req.tool_exchange.push(serde_json::json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": Uuid::new_v4().to_string(),
                "name": name,
                "input": {},
            }],
        }));
        req.tool_exchange.push(serde_json::json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "x",
                "content": "{}",
                "is_error": false,
            }],
        }));
    }

    #[tokio::test]
    async fn empty_request_returns_greeting_text() {
        let backend = HeuristicMockBackend::new();
        let resp = backend.send_turn(empty_request()).await.unwrap();
        assert!(resp.tool_uses.is_empty());
        assert!(resp.assistant_content.to_lowercase().contains("hello"));
    }

    #[tokio::test]
    async fn first_user_prose_triggers_classify_intake() {
        let backend = HeuristicMockBackend::new();
        let resp = backend
            .send_turn(request_with_user("scRNA-seq from IVD"))
            .await
            .unwrap();
        assert_eq!(resp.tool_uses.len(), 1);
        match &resp.tool_uses[0].1 {
            Tool::Batchable(BatchableTool::ClassifyIntake { prose }) => {
                assert!(prose.contains("IVD"));
            }
            other => panic!("expected ClassifyIntake, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn post_classify_triggers_append_prose() {
        let backend = HeuristicMockBackend::new();
        let mut req = request_with_user("scRNA-seq from IVD");
        append_assistant_tool_use(&mut req, "classify_intake");
        let resp = backend.send_turn(req).await.unwrap();
        assert_eq!(resp.tool_uses.len(), 1);
        assert!(matches!(
            &resp.tool_uses[0].1,
            Tool::Batchable(BatchableTool::AppendIntakeProse { .. })
        ));
    }

    #[tokio::test]
    async fn post_mutation_triggers_summary() {
        let backend = HeuristicMockBackend::new();
        let mut req = request_with_user("scRNA-seq from IVD");
        append_assistant_tool_use(&mut req, "classify_intake");
        append_assistant_tool_use(&mut req, "append_intake_prose");
        let resp = backend.send_turn(req).await.unwrap();
        assert_eq!(resp.tool_uses.len(), 1);
        assert!(matches!(
            &resp.tool_uses[0].1,
            Tool::Batchable(BatchableTool::ProposeSummaryConfirmation { .. })
        ));
    }

    #[tokio::test]
    async fn auto_confirm_short_circuits_to_emit() {
        // `auto_confirm` short-circuit fires only when
        // `propose_summary_confirmation` appears in a PRIOR committed
        // turn (i.e. via `request.conversation`'s `tool_calls`, not the
        // current `tool_exchange`). Same-iteration firing was the
        // bug that left a session in PendingConfirmation without a minted
        // ConfirmationToken; `is_confirmed()` would have returned false
        // and `emit_package` would have refused with PreconditionFailure.
        // Putting summary in HISTORY guarantees the SME's confirm click
        // (server-side `confirm_with_modes`) has minted the token.
        let backend = HeuristicMockBackend::new().with_auto_confirm(true);
        let mut req = request_with_user("scRNA-seq from IVD");
        let mut prior = Turn::assistant("(prior assistant turn)");
        for name in [
            "classify_intake",
            "append_intake_prose",
            "propose_summary_confirmation",
        ] {
            prior.tool_calls.push(ToolCallRecord {
                turn_id: prior.turn_id,
                tool_name: name.to_string(),
                args: serde_json::json!({}),
                result: serde_json::json!({}),
                is_error: false,
                model: "test".to_string(),
                timestamp: chrono::Utc::now(),
            });
        }
        let mut conv = (*req.conversation).clone();
        conv.push(prior);
        req.conversation = Arc::new(conv);
        let resp = backend.send_turn(req).await.unwrap();
        assert_eq!(resp.tool_uses.len(), 1);
        assert!(matches!(
            &resp.tool_uses[0].1,
            Tool::HighImpact(HighImpactTool::EmitPackage { .. })
        ));
    }

    #[tokio::test]
    async fn confirmation_marker_advances_to_emit() {
        let backend = HeuristicMockBackend::new();
        let mut req = request_with_user("scRNA-seq from IVD");
        append_assistant_tool_use(&mut req, "classify_intake");
        append_assistant_tool_use(&mut req, "append_intake_prose");
        append_assistant_tool_use(&mut req, "propose_summary_confirmation");
        // Inject the SME-click marker the fixture runner writes
        // between the propose-summary turn and the post-confirm turn.
        req.tool_exchange.push(serde_json::json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "marker",
                "content": "{\"pending_confirmation_cleared\": true}",
                "is_error": false,
            }],
        }));
        let resp = backend.send_turn(req).await.unwrap();
        assert!(matches!(
            &resp.tool_uses[0].1,
            Tool::HighImpact(HighImpactTool::EmitPackage { .. })
        ));
    }

    #[tokio::test]
    async fn post_emit_returns_closing_text() {
        let backend = HeuristicMockBackend::new();
        let mut req = request_with_user("scRNA-seq from IVD");
        append_assistant_tool_use(&mut req, "classify_intake");
        append_assistant_tool_use(&mut req, "append_intake_prose");
        append_assistant_tool_use(&mut req, "propose_summary_confirmation");
        append_assistant_tool_use(&mut req, "emit_package");
        let resp = backend.send_turn(req).await.unwrap();
        assert!(resp.tool_uses.is_empty());
        assert!(!resp.assistant_content.is_empty());
    }

    #[tokio::test]
    async fn streaming_path_fires_delta_for_text() {
        let backend = HeuristicMockBackend::new();
        let captured = Arc::new(std::sync::Mutex::new(String::new()));
        let cap = captured.clone();
        let sink: crate::anthropic::delta_sink::DeltaSink =
            Arc::new(move |s: &str| cap.lock().unwrap().push_str(s));
        let _ = backend
            .send_turn_streaming(empty_request(), sink)
            .await
            .unwrap();
        assert!(!captured.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn count_tokens_returns_some() {
        let backend = HeuristicMockBackend::new();
        let n = backend.count_tokens(&empty_request()).await.unwrap();
        assert!(n.is_some());
    }
}
