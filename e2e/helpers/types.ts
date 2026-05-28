/**
 * Local mirrors of the Rust/ts-rs types the mock backend returns.
 *
 * These are intentionally hand-maintained in the e2e package rather than
 * imported from ui/src/types. The e2e suite is a black-box consumer of the
 * chat REST contract — keeping the types local means the suite doesn't
 * break when ui/src/types churns from a ts-rs regeneration, and makes the
 * beat YAML format self-documenting.
 *
 * Source of truth:
 *  - crates/conversation/src/session.rs (Turn, SessionState, ConfirmationCard)
 *  - crates/server/src/chat_routes.rs (SessionStateSnapshot, SsePayload)
 */

export type SessionStateKind =
  | 'greeting'
  | 'intake'
  | 'intake_followup'
  | 'pending_confirmation'
  | 'ready_to_emit'
  | 'emitting'
  | 'emitted'
  | 'blocked'

/**
 * Hand-mirrored from `ui/src/types/BlockerEntry.ts`. Carries enough
 * fields for the BlockerCard renderer to dispatch (task_id +
 * blocker_id + kind + message). Tests use `blockers: [...]` on the
 * `blocked` state variant to verify that multiple concurrent blockers
 * each render their own BlockerCard. The legacy single-blocker shape
 * (`reason` + `recovery_hint` + `blocker_kind` without `blockers`)
 * still works for round-1 fixtures that haven't migrated.
 */
export interface BlockerEntry {
  blocker_id: string
  task_id: string
  /** Shape echoes `BlockerKind` from crates/core/src/blocker.rs. */
  kind: unknown
  message: string
  recovery_hint?: string
  at?: string
}

export type SessionState =
  | { kind: 'greeting' }
  | { kind: 'intake' }
  | { kind: 'intake_followup' }
  | { kind: 'pending_confirmation' }
  | { kind: 'ready_to_emit' }
  | { kind: 'emitting' }
  | { kind: 'emitted' }
  | {
      kind: 'blocked'
      reason: string
      recovery_hint: string
      /**
       * Typed blocker taxonomy. Not every test sets it; when absent
       * the UI falls back to the generic blocker copy. Shape is a
       * black-box echo of `BlockerKind` from crates/core/src/blocker.rs.
       */
      blocker_kind?: unknown
      /**
       * Queue of active blockers. The UI prefers this over the legacy
       * single-blocker fields when present and non-empty — each entry
       * renders its own BlockerCard. Mirrors `SessionState::Blocked`
       * in crates/conversation/src/session.rs.
       */
      blockers?: BlockerEntry[]
    }

export type TurnRole = 'user' | 'assistant' | 'system'

export interface ConfirmationCard {
  summary_markdown: string
}

export interface ToolCallRecord {
  tool_name: string
  args: unknown
  result: unknown
  started_at: string
  finished_at: string
}

export interface Turn {
  turn_id: string
  role: TurnRole
  content: string
  intent: unknown | null
  tool_calls: ToolCallRecord[]
  quick_replies: string[]
  confirmation_card: ConfirmationCard | null
  timestamp: string
}

export interface ProgressSummary {
  completed: number
  ready: number
  blocked: number
  pending: number
}

export interface SessionStateSnapshot {
  session_id: string
  state: SessionState
  user_confirmed: boolean
  last_activity: string
  task_count: number
  progress: ProgressSummary
  /**
   * Absolute path to the emitted package, once `emit_package` has fired.
   * Absent until the session reaches `emitted`. Live-tier tests read this
   * after confirmation instead of routing `output_dir` through LLM prose.
   */
  emitted_package_path?: string
}

export interface SessionMetrics {
  turn_count: number
  tool_call_count: number
  total_input_tokens: number
  total_output_tokens: number
  cache_read_tokens: number
  cache_creation_tokens: number
  p50_turn_ms: number
  p95_turn_ms: number
  p99_turn_ms: number
  mean_turn_ms: number
  max_turn_ms: number
  sonnet_turns: number
  opus_turns: number
  // compute-usage telemetry from harness progress
  // events carrying a `remote` executor. Required on the wire; the
  // mock backend defaults them to zero/empty when a test doesn't
  // specify remote execution.
  total_instance_seconds: number
  instance_type_seconds: Record<string, number>
  high_water_exceeded_count: number
}

/**
 * SSE payloads pushed onto the fake EventSource. Mirrors
 * crates/server/src/chat_routes.rs::SsePayload.
 */
export type SseEvent =
  | { type: 'tool_call_started'; tool_name: string; status_line: string }
  | { type: 'tool_call_finished'; tool_name: string }
  | { type: 'assistant_token_delta'; text: string }
  | { type: 'state_advanced'; new_state: SessionState }
  | {
      type: 'harness_progress'
      kind: string
      task_id: string
      status: string
      detail: string
    }
  | { type: 'infra_error'; reason: string; user_copy: string }
  // pilot lifecycle, stall signals, cross-version diff.
  // Mirrors the Rust SsePayload variants in
  // crates/server/src/chat_routes.rs. Tests typically feed mocked
  // fixtures of these payloads in via `handle.pushSseEvent` so the
  // UI-side state accumulation path lights up without the full server.
  | { type: 'harness_sizing_pilot_started' }
  | { type: 'harness_sizing_pilot_complete'; report: unknown }
  | { type: 'harness_sizing_pilot_skipped'; reason: string }
  | {
      type: 'harness_stall_detected'
      task_id: string
      signal: unknown
      suggested_action: 'resize' | 'retry' | 'abort'
    }
  | {
      type: 'harness_resize_recommended'
      task_id: string
      from_instance_type: string
      to_instance_type: string
    }
  | { type: 'harness_version_diff'; report: unknown }
  // Hypothesized-node proposal lifecycle. Mirrors the four
  // `proposal_*` variants in crates/server/src/chat_routes.rs::SsePayload
  // and the UI-side ChatSseEvent in ui/src/api/chatStream.ts.
  | {
      type: 'proposal_received'
      proposal_id: string
      node_id: string
    }
  | {
      type: 'proposal_gate_advanced'
      proposal_id: string
      gate: 'validator' | 'sandbox' | 'sme_signoff'
      passed: boolean
    }
  | {
      type: 'proposal_promoted'
      proposal_id: string
      task_node_id: string
    }
  | {
      type: 'proposal_rejected'
      proposal_id: string
      rationale: string | null
    }

// ── Proposal fixtures ─────────────────────────────────────────────────────
//
// Hand-mirrored from `ui/src/types/HypothesizedProposal.ts` (ts-rs
// generated). The e2e suite is a black-box consumer of the REST contract,
// so we keep a local shape rather than importing the ui bindings. Drift
// guards: changes to the ts-rs Rust struct require a matching tweak here.

export type ProposalGateName = 'validator' | 'sandbox' | 'sme_signoff'

export type ProposalLifecycle =
  | { kind: 'pending_validation' }
  | { kind: 'pending_sandbox' }
  | { kind: 'awaiting_signoff' }
  | { kind: 'promoted'; task_node_id: string }
  | { kind: 'blocked'; reason: ProposalBlockerReason }
  | { kind: 'rejected'; rationale?: string }

export type ProposalBlockerReason =
  | { kind: 'validator_failed'; failures: string[] }
  | { kind: 'sandbox_refused'; refusals: Array<{ kind: string; [k: string]: unknown }> }
  | { kind: 'materialization_failed'; reason: string }
  | { kind: 'sme_rejected'; rationale?: string }

export interface ProposalGateOutcome {
  gate: ProposalGateName
  passed: boolean
  details: string[]
  /** Unix-epoch seconds — serialized as a number on the wire. */
  recorded_at: number
}

export interface MockHypothesizedProposal {
  id: string
  node_id: string
  intent: string
  parent_terms: string[]
  assumptions: string[]
  failure_modes: string[]
  validation_tests: string[]
  llm_rationale: string
  lifecycle: ProposalLifecycle
  gate_outcomes: ProposalGateOutcome[]
  /** Unix-epoch seconds — serialized as a number on the wire. */
  created_at: number
  /** Unix-epoch seconds — serialized as a number on the wire. */
  last_transition_at: number
}

/**
 * Captured POST body from a `/signoff` or `/reject` call. The mock
 * backend stamps the proposal id + verb so assertions can target a
 * specific click without parsing the URL by hand.
 */
export interface RecordedProposalAction {
  verb: 'signoff' | 'reject'
  proposalId: string
  /** Raw POST body as the client sent it. */
  body: unknown
}

// ── Beat / scenario schema ────────────────────────────────────────────────

export type TabKind = 'plan' | 'state' | 'documents' | 'jobs' | 'metrics' | 'history'

export interface AssistantResponse {
  content: string
  quick_replies?: string[]
  confirmation_card?: ConfirmationCard
  /** Optional tool_calls to attach to the Turn (purely cosmetic for now). */
  tool_calls?: ToolCallRecord[]
}

export interface BeatExpect {
  /** SessionStateSnapshot.state.kind the badge should read after the beat. */
  stateBadge?: SessionStateKind
  /** Tool-call status pill line. null means the pill must NOT be visible. */
  pillStatusLine?: string | null
  /** Substrings that must appear in the latest assistant turn's content. */
  visibleText?: string[]
  /** Substrings that must NOT appear anywhere in the assistant content. */
  forbiddenText?: string[]
  /** task_count the mocked /state should return. */
  planTaskCount?: number
  /** Active tab in the State Inspector. */
  activeTab?: TabKind
  /** Whether a confirmation card should be visible on the latest assistant turn. */
  confirmationCardVisible?: boolean
  /** Whether a BlockerCard should be visible. */
  blockerVisible?: boolean
  /** Whether an InfraErrorBanner should be visible. */
  infraBannerVisible?: boolean
  /** Number of items the Jobs badge should show. */
  jobsBadgeCount?: number
}

export interface Beat {
  /** Short, SME-style message typed into the composer. */
  user: string
  /** Canned assistant response for this beat. */
  assistant: AssistantResponse
  /** SSE events to push between the user submit and the assistant arrival. */
  sse?: SseEvent[]
  /** State the mocked /state should return after this beat. */
  state?: SessionStateKind | SessionState
  /** Optional canned DAG for mocked Plan/TaskDrawer tests. */
  dag?: unknown
  /** Assertions to run after the assistant turn renders. */
  expect?: BeatExpect
}

export interface ScenarioFinale {
  /** 'confirm' clicks Accept; 'reject' clicks Revise; 'none' skips. */
  confirmAction?: 'confirm' | 'reject' | 'none'
  /** Beats to drive after the finale action (e.g. the post-confirm turn). */
  afterConfirm?: Beat[]
}

export interface Scenario {
  name: string
  modality: string
  beats: Beat[]
  finale?: ScenarioFinale
  /** Live-tier configuration — drives the real server + real Anthropic API. */
  live?: LiveConfig
}

// ── Live-tier schema ──────────────────────────────────────────────────────

export interface LiveConfig {
  /**
   * Dense SME prose sent as the first user message. Should contain all
   * the domain context the system needs to classify, build a DAG, and
   * reach a confirmable state in one or two turns. `{pkgDir}` is
   * replaced at runtime with a temp directory path.
   */
  intake_prose: string
  /**
   * Optional follow-up replies keyed by regex triggers. If the assistant
   * asks a question matching a trigger, the corresponding reply is sent.
   */
  followups?: LiveFollowup[]
  /** Assertions run after the intake conversation stabilizes. */
  assertions: LiveAssertions
}

export interface LiveFollowup {
  /** Regex pattern tested against the latest assistant bubble text. */
  trigger: string
  /** Message to type into the composer if the trigger matches. */
  reply: string
}

export interface LiveAssertions {
  /** State badge must not be 'greeting' after the intake turn. */
  stateNotGreeting?: boolean
  /** Substrings that must NOT appear in any assistant bubble. */
  forbiddenText?: string[]
  /** POST /confirm directly instead of waiting for a UI confirmation card. */
  directConfirm?: boolean
  /** Verify package artifacts on disk after emission. */
  verifyPackage?: boolean
  /** Expected state.kind after the entire flow completes. */
  expectedFinalState?: string
  /** Minimum number of turns in the transcript. */
  minTranscriptTurns?: number
  /**
   * Substrings at least ONE of which must appear in the final assistant
   * bubble. Used for refusal scenarios (e.g. explicit-only taxonomies
   * like gwas-coloc where the LLM correctly refuses to emit) to verify
   * the refusal is present and clearly worded.
   */
  refusalTextContains?: string[]
}
