// Chat session client for the LLM-mediated conversation surface.
// Mirrors the Rust chat_routes wire types in crates/server/src/chat_routes.rs.
//
// Type-export policy (R-13):
//
// Every type re-exported from this module falls into one of two buckets:
//
//   (a) ts-rs re-export — the type has a `#[derive(TS)]` Rust counterpart
//       and the generated definition under `../types/` is the single
//       source of truth. We `import type` and `export type` so call
//       sites can continue `import { Foo } from '../api/chatClient'`
//       without churn while the shape stays in lockstep with Rust.
//
//   (b) Hand-typed — local DTO not derived from ts-rs. Falls into one
//       of the sub-cases below; each hand-defined interface in this
//       file is tagged inline with the matching reason:
//         * HTTP-DTO — request / response shape with no Rust struct
//           (e.g. `CreateSessionRequest`, `AutoTitleResponse`).
//         * Wire — server-emitted variant that intentionally drifts
//           from the core type (e.g. `HarnessEventWire` carries
//           camelCase `taskId` after the UI's snake-to-camel
//           transform — different shape from the Rust `HarnessEvent`).
//         * Agent-on-disk — JSON shape the UI reads but which the server
//           passes through unmodified (`AgentBlockerJson`,
//           `DispositionBodyWire`).
//         * Metrics-mirror — hand-mirrored from
//           `crates/conversation/src/metrics/` per its docstring;
//           intentionally not ts-rs because the metrics surface mixes
//           u64 fields with optional maps that don't round-trip cleanly.
//         * Config-blob — static configuration data fetched once and
//           cached (e.g. `StageDescription`).
//         * UI-row — state-inspector tab row / view-model composition.
//
// When promoting a hand-typed shape to ts-rs, drop the hand interface,
// add a re-export to the barrel below, and update the inline tag.
//
// TODO(R-13): the following hand-typed shapes have direct Rust
// counterparts that could be migrated once the consumer code stops
// relying on loose property access:
//   * `DecisionsResponse.decisions[]` -> tighten to `DecisionRecord[]`
//     after `DecisionsTab.tsx` switches to a per-variant switch on
//     `record.decision.kind` (today it does `record.decision[field]`).
//   * `ExecutionStatus.status` -> narrowed string union here vs
//     `ExecutionStatusResponse.status: string` in the ts-rs binding;
//     promote once the harness state-machine guarantees the union
//     server-side (today it's just convention).
//   * `DispositionListEntryWire` / `DispositionBodyWire` /
//     `ApplyDispositionResponseWire` -> share fields with the ts-rs
//     `Disposition` but carry the wire-flavored `path` / status
//     metadata the server attaches on listing. Promote after the
//     server splits the listing DTO into a separate ts-rs type.

import type {
  AssistantIntent,
  BlockerEntry,
  CheckpointMode,
  ConfirmationCard,
  CrossVersionReport,
  DAG,
  DecisionActor,
  ProgressSummary,
  RubricScore,
  Session,
  SessionMode,
  Turn,
  UserInput,
} from '../types'
import type { AgentCodeRecord } from '../types/AgentCodeRecord'
import type { BranchInput } from '../types/BranchInput'
import type { ClaimVerificationReport } from '../types/ClaimVerificationReport'
import type { DecisionRecord } from '../types/DecisionRecord'
import type { RemoteExecutionInfo } from '../types/RemoteExecutionInfo'
import type { SessionStateSnapshot } from '../types/SessionStateSnapshot'
import type { TierResult } from '../types/TierResult'
import type { ValueAddMetricsResponse } from '../types/ValueAddMetricsResponse'
import type { AdjudicationQueueEntry } from '../types/AdjudicationQueueEntry'
import type { RepairProposal } from '../types/RepairProposal'
import type { HypothesizedProposal } from '../types/HypothesizedProposal'
import type { SendTurnRequest } from '../types/SendTurnRequest'
import type { DispositionStatus } from '../types/DispositionStatus'
import {
  ApiClientError,
  FetchError,
  isApiError,
  jsonFetch,
  jsonFetchOrNull,
  jsonFetchRaw,
  voidFetch,
} from './_fetch'
import type { ApiErrorBody } from './_fetch'

// Re-export the typed error surface so chatClient call sites can do
// `import { ApiClientError, isApiError } from '../api/chatClient'`
// without reaching past the module boundary. Consumer code branches:
//   try { await postAmendMethod(...) }
//   catch (e) {
//     if (e instanceof ApiClientError && e.code === 'precondition_failure') {
//       toast('Confirm the session first.')
//     } else throw e
//   }
export { ApiClientError, FetchError, isApiError, jsonFetchRaw }
export type { ApiErrorBody }

// ── ts-rs re-exports (R-13) ────────────────────────────────────────────
// Generated under `../types/` and re-exported here so call sites that
// already `import { X } from '../api/chatClient'` keep working after
// migration. Drop any duplicate `export interface X` body below this
// block when promoting more shapes.
export type {
  AgentCodeRecord,
  AssistantIntent,
  BlockerEntry,
  BranchInput,
  ConfirmationCard,
  DecisionActor,
  DecisionRecord,
  ProgressSummary,
  RubricScore,
  SendTurnRequest,
  Session,
  SessionStateSnapshot,
  TierResult,
  UserInput,
  ValueAddMetricsResponse,
}

/**
 * Construct a session- or task-scoped chat API URL. Encodes both ids
 * to handle session ids carrying URI-significant characters (the
 * server-side path-jail enforces the same rule on the receive end).
 *
 *  SessionUrl('abc-123', 'state') -> /api/chat/session/abc-123/state
 *  SessionUrl('abc-123', 'verify', 'task_4') -> /api/chat/session/abc-123/task/task_4/verify
 */
export const sessionUrl = (
  sessionId: string,
  verb: string,
  taskId?: string,
): string =>
  taskId
    ? `/api/chat/session/${encodeURIComponent(sessionId)}/task/${encodeURIComponent(taskId)}/${verb}`
    : `/api/chat/session/${encodeURIComponent(sessionId)}/${verb}`

// HTTP-DTO. POST /api/chat/session request body.
export interface CreateSessionRequest {
  careful_mode?: boolean
}

// HTTP-DTO. POST /api/chat/session response body.
export interface CreateSessionResponse {
  session_id: string
  greeting: Turn
}

// ProgressSummary + SessionStateSnapshot live in the ts-rs re-export
// block at the top of this file. Generated SessionStateSnapshot
// tightens `title?: string` to `title: string | null` (closes the
// undefined-vs-null drift the chat_routes.rs schema actually emits)
// and promotes `parent_session_id` + `blocked_tasks` from optional to
// required-with-null-default — call sites already use `?.` / `??` so
// the runtime behaviour is unchanged. Test fixtures that built
// snapshots without these fields need the three null/empty defaults
// (handled in the sibling-agent test pass).

// HTTP-DTO. §3.15 — chat config endpoint response shape.
export interface ChatConfig {
  auto_title_enabled: boolean
  auto_title_min_turns: number
}

export async function getChatConfig(): Promise<ChatConfig> {
  return jsonFetch('/api/chat/config')
}

/** Response shape for POST /api/chat/session/:id/auto-title. */
export interface AutoTitleResponse {
  title: string
  from_cache: boolean
}

export async function autoTitleSession(
  sessionId: string,
): Promise<AutoTitleResponse> {
  return jsonFetch(sessionUrl(sessionId, 'auto-title'), {
    method: 'POST',
  })
}

/** POST /api/chat/session/:id/explain response shape. */
export interface ExplainResponse {
  explanation: string
  model: string
  cached: boolean
}

/**
 * Plain-language rewrite of a technical snippet. Billed against the
 * session's side-call cost bucket. Cached for an hour server-side so
 * a repeat click doesn't re-bill; a lightweight client-side promise
 * cache (capped at 50 entries, FIFO eviction) avoids the round-trip
 * entirely when the same text + context lands twice in one tab.
 */
const _explainCache = new Map<string, Promise<ExplainResponse>>()
const EXPLAIN_CACHE_CAP = 50

export async function postExplain(
  sessionId: string,
  text: string,
  context?: string,
): Promise<ExplainResponse> {
  const key = `${sessionId}|${context ?? ''}|${text}`
  const hit = _explainCache.get(key)
  if (hit) return hit
  const promise = jsonFetch<ExplainResponse>(
    sessionUrl(sessionId, 'explain'),
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ text, context: context ?? null }),
    },
  )
  // Insert before await so concurrent callers share the promise.
  _explainCache.set(key, promise)
  if (_explainCache.size > EXPLAIN_CACHE_CAP) {
    const oldest = _explainCache.keys().next().value
    if (oldest !== undefined) _explainCache.delete(oldest)
  }
  // If the request errors, drop the cached failure so a retry can happen.
  promise.catch(() => _explainCache.delete(key))
  return promise
}

export interface BudgetResponse {
  budget_usd: number | null
  budget_set_by: string | null
  budget_set_at: string | null
}

/** Attach an SME-authored note to a task. */
export async function postTaskNote(
  sessionId: string,
  taskId: string,
  body: string,
  author?: string,
): Promise<void> {
  await voidFetch(
    sessionUrl(sessionId, 'note', taskId),
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ body, author: author ?? null }),
    },
  )
}

export interface DashboardSummaryResponse {
  summary: string
  model: string
  cached: boolean
}

/** Narrative dashboard summary. */
export async function postDashboardSummary(
  sessionId: string,
): Promise<DashboardSummaryResponse> {
  return jsonFetch(
    sessionUrl(sessionId, 'dashboard/summary'),
    { method: 'POST' },
  )
}

export interface CrossVersionTablePair {
  table_name: string
  current: { mime: string; body: string } | null
  parent: { mime: string; body: string } | null
}

export interface ShareTokenDescriptor {
  token: string
  expires_at: string | null
  created_at: string
  scope: 'read_only'
}

/** Issue a new read-only share token. */
export async function createShareToken(
  sessionId: string,
  expiresInHours?: number | null,
): Promise<ShareTokenDescriptor> {
  return jsonFetch(
    sessionUrl(sessionId, 'share-token'),
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ expires_in_hours: expiresInHours ?? null }),
    },
  )
}

export async function revokeShareToken(
  sessionId: string,
  token: string,
): Promise<void> {
  await voidFetch(
    `${sessionUrl(sessionId, 'share-token')}/${encodeURIComponent(token)}`,
    { method: 'DELETE' },
  )
}

export async function listShareTokens(
  sessionId: string,
): Promise<ShareTokenDescriptor[]> {
  return jsonFetch(
    sessionUrl(sessionId, 'share-tokens'),
  )
}

/** Parent + current table contents for cross-version diff. */
export async function getCrossVersionDiffTablePair(
  sessionId: string,
  tableName: string,
): Promise<CrossVersionTablePair> {
  return jsonFetch(
    `${sessionUrl(sessionId, 'cross-version-diff/tables')}/${encodeURIComponent(tableName)}`,
  )
}

/**
 * Fetch the parsed install-log entries
 * for a session's emitted package. Each row mirrors the install-proxy
 * JSONL format: `atom_id`, `package`, `registry`, `timestamp`, plus
 * any forward-compatible fields the proxy adds. Returns an empty
 * array (server-side 200) when no package has been emitted, no
 * install-log was written, or every line is malformed — the AuditTab
 * renders a placeholder rather than an error in that case.
 */
export interface InstallLogResponse {
  entries: Array<Record<string, unknown>>
}

export async function getInstallLog(
  sessionId: string,
): Promise<InstallLogResponse> {
  return jsonFetch(
    sessionUrl(sessionId, 'install-log'),
  )
}

/** Set / clear the session-level budget cap. */
export async function postBudget(
  sessionId: string,
  usd: number | null,
  author?: string,
): Promise<BudgetResponse> {
  return jsonFetch(
    sessionUrl(sessionId, 'budget'),
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ usd, author: author ?? null }),
    },
  )
}

/// Mirrors crates/conversation/src/metrics.rs::SessionMetrics.
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
  // Back-compat mirrors: equal to `per_model_turns["sonnet_4_6"]` and
  // `per_model_turns["opus_4_6"]` respectively. Retained so the UI
  // doesn't need a behaviour change at the same time the Rust refactor
  // lands; new UI code should read `per_model_turns` directly.
  sonnet_turns: number
  opus_turns: number
  // Per-model turn count / cost, keyed by ModelId serde name
  // (`sonnet_4_6`, `opus_4_6`, `haiku_4_5`, …). Absent when that model
  // never served a turn in this session.
  per_model_turns: Record<string, number>
  per_model_cost_usd: Record<string, number>
  // Compute-usage telemetry from harness progress events carrying a
  // `remote` executor. `total_instance_seconds` is the sum over every
  // completed task; `instance_type_seconds` breaks that total down by
  // EC2 instance type.
  total_instance_seconds: number
  instance_type_seconds: Record<string, number>
  // Bumped when the harness realized more compute was needed than the
  // sizing baseline; the executor resized up per
  // ECAA_AWS_HIGH_WATER_POLICY.
  high_water_exceeded_count: number
  // Estimated Anthropic API spend for this session in USD, priced per
  // model using 5-min ephemeral cache rates (see metrics.rs::pricing).
  // `chat_cost_usd + agent_cost_usd === total_cost_usd`.
  total_cost_usd: number
  // Chat-only: LLM turns driven by ConversationService::send_turn.
  chat_cost_usd: number
  // Agent-only: task execution LLM spend recorded when the harness
  // forwards agent-usage blocks from the agent-claude.sh wrapper.
  // Zero when the agent script hasn't been instrumented.
  agent_cost_usd: number
  // Per-model agent spend breakdown, same key shape as
  // per_model_cost_usd (`sonnet_4_6`, `opus_4_6`, `haiku_4_5`, …).
  per_model_agent_cost_usd: Record<string, number>
  // Scorer-only: rubric-scorer API spend recorded when an operator
  // triggers POST /api/chat/session/:id/score. Zero on sessions that
  // have never been scored. `chat + agent + scorer === total`.
  scorer_cost_usd: number
  // Per-model scorer spend breakdown. In practice only `sonnet_4_6`
  // appears (the scorer pins Sonnet at the call site), but the map
  // shape matches the other cost buckets.
  per_model_scorer_cost_usd: Record<string, number>
  // Side-call spend: cheap, read-only LLM hops routed through
  // ModelPolicy::for_side_call (Haiku 4.5). First caller is session
  // auto-titling. `chat + agent + scorer + side_call === total`.
  // Optional for legacy snapshots that pre-date the side-call bucket.
  side_call_cost_usd?: number
  // Per-model side-call spend breakdown. Usually only `haiku_4_5`.
  per_model_side_call_cost_usd?: Record<string, number>
  sonnet_cost_usd: number
  opus_cost_usd: number
  // ECAA_AGENT_BILLING at snapshot time — "subscription" (default;
  // per-task agent runs bill the Max/Pro plan, real charge = $0) or
  // "api" (forwarded API key, real per-token charges). Under
  // subscription, `agent_cost_usd` is NOTIONAL — the Claude Code CLI's
  // self-reported cost representing what the tokens would have cost
  // on the API. The UI renders a "notional" badge in subscription mode
  // so the figure isn't mistaken for real spend.
  agent_billing_mode?: string
  // F6 — per-surface token split. Each of the four cost surfaces (chat /
  // agent / scorer / side_call) carries its own input / output /
  // cache_read / cache_creation sub-totals. The aggregate `total_*`
  // fields above remain as grand totals (sum across all four surfaces).
  per_surface_tokens?: PerSurfaceTokens
  // F1 — per-task agent cost breakdown. One entry per agent-run task,
  // sorted by cost descending at serialize time. Empty for sessions
  // that haven't run any agent tasks yet, or pre-F1 sidecars.
  per_task_agent?: PerTaskAgentSnapshot[]
  // F2 — per-tool-name call counter. Bucketed at the service-layer
  // tool-dispatch boundary by Tool::name(). Empty until at least one
  // tool has been dispatched. Aggregate sum equals tool_call_count.
  tool_calls_by_name?: Record<string, number>
  // F3 — wall-clock seconds since session.created_at. Zero on a
  // fresh session or when the snapshot was taken via a path that
  // doesn't have the Session loaded (test-only). UI uses this to
  // derive per-hour burn rates.
  session_duration_seconds?: number
  // F5 — total dollar savings from prompt caching, summed across all
  // four surfaces. cache_read_tokens × (input_rate − cache_read_rate).
  cache_savings_usd?: number
  // F5 — per-surface savings split. Only nonzero entries appear.
  per_surface_cache_savings_usd?: Record<string, number>
  // §4.5 — fraction of billed input tokens served from Anthropic's
  // prompt cache. Healthy values after warm-up are ≥ 0.6; near-zero
  // values on a multi-turn session signal silent cache invalidation.
  // Zero on a fresh session (no data yet).
  cache_hit_ratio: number
  // §3.2/§4.3 — per-turn tool-loop iteration-count histogram. Keyed
  // by the iteration count used, value is how many turns used exactly
  // that many iterations.
  tool_loop_iterations_histogram: Record<string, number>
  // §3.3/§4.2 — Opus escalation count per reason (careful_mode,
  // blocked, low_confidence). Empty when every turn ran on Sonnet.
  opus_escalation_reasons: Record<string, number>
  // §S7.11 — composer performance counters. The composer is
  // synchronous + sub-millisecond per call today; aggregate sums
  // surface in the Performance tab so operators see when the
  // backward-chain path is climbing toward the planned p99 < 500ms
  // budget at ≤ 200 atoms. Zero on a session that hasn't hit the
  // composer-driven emit path yet (legacy intake→build_dag path
  // doesn't increment).
  composer_runs?: number
  composer_total_duration_ms?: number
  composer_atoms_considered?: number
  composer_backtracks?: number
  composer_exclusion_hits?: number
  // §3.7 — session-wide token budget / used / remaining. May be
  // absent on older server builds; UI should treat `undefined` as
  // "budget unlimited".
  session_token_budget?: number | null
  /** Session-level USD budget cap. */
  budget_usd?: number | null
  /** 0.0..=1.0+ — fraction of cap consumed by total_cost_usd. */
  budget_used_pct?: number | null
  /** 'ok' | 'warn' | 'exceeded'. */
  budget_state?: string | null
  /** Projected remaining cost for unfinished tasks. */
  projected_remaining_usd?: number
  /** total_cost_usd + projected_remaining_usd. */
  projected_finish_usd?: number
  // Affordance fallback telemetry. Each entry is a
  // (semantic_type, primitive, count) triple recording how many times a
  // structural-fallback generic renderer was used in this session, to
  // surface catalog gaps in the Performance tab.
  affordance_fallbacks?: AffordanceFallbackSummary[]
}

/// Hand-mirrored from crates/conversation/src/metrics.rs::AffordanceFallbackSummary.
/// Not ts-rs derived (u32 is safe as number; mirrors the prose in metrics.rs).
export interface AffordanceFallbackSummary {
  semantic_type: string
  primitive: string
  count: number
}

export interface PerSurfaceTokens {
  chat: TokenBucket
  agent: TokenBucket
  scorer: TokenBucket
  side_call: TokenBucket
}

export interface TokenBucket {
  input_tokens: number
  output_tokens: number
  cache_read_tokens: number
  cache_creation_tokens: number
}

/** One entry in `SessionMetrics.per_task_agent`. */
export interface PerTaskAgentSnapshot {
  task_id: string
  model: string
  stage_class?: string
  input_tokens: number
  output_tokens: number
  cache_read_tokens: number
  cache_creation_tokens: number
  cost_usd: number
  /** Wall-clock seconds consumed. */
  elapsed_secs?: number | null
}

export async function createChatSession(
  req: CreateSessionRequest = {},
): Promise<CreateSessionResponse> {
  return jsonFetch('/api/chat/session', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(req),
  })
}

/**
 * v3 P10 — three-state availability surface. Mirrors
 * `crates/core/src/llm_availability.rs::LlmAvailability`. The ts-rs
 * binding will overwrite this with the canonical generated type on the
 * next `make types`; the local declaration here keeps the UI build
 * green until then.
 */
export type LlmAvailability =
  | { kind: 'available' }
  | { kind: 'unavailable'; reason: string; retry_after_seconds: number | null }
  | { kind: 'disabled'; reason: string; set_by: string }

/**
 * v3 P10 — `GET /api/chat/llm-availability` polled by the UI at mount
 * time. When the result is anything other than `{ kind: "available" }`,
 * the conversational chat surface yields to the
 * `StructuredIntakeForm` MVP fallback.
 */
export async function getLlmAvailability(): Promise<LlmAvailability> {
  return jsonFetch('/api/chat/llm-availability')
}

/**
 * v3 P10 — the structured-intent shape captured by
 * `StructuredIntakeForm` when the LLM is `Disabled` / `Unavailable`.
 * Matches `WorkflowIntent` on the React side.
 */
export interface WorkflowIntent {
  goal: string
  modality: string
  organism: string
  desired_outputs: string
  uncertainties: string
}

/**
 * v3 P10 — start a new session from a structured intent without
 * requiring an LLM turn. The server records the intent as deterministic
 * intake prose, runs the classifier/DAG builder, and returns the
 * newly-created session.
 */
export async function startSessionFromIntent(
  intent: WorkflowIntent,
): Promise<CreateSessionResponse> {
  return jsonFetch('/api/chat/session/from-intent', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(intent),
  })
}

export async function sendChatTurn(
  sessionId: string,
  message: string,
  opts?: { userTurnId?: string },
): Promise<Turn> {
  return jsonFetch(sessionUrl(sessionId, 'turn'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      message,
      ...(opts?.userTurnId ? { user_turn_id: opts.userTurnId } : {}),
    } as SendTurnRequest),
  })
}

// The three poll/refresh fetches accept an optional
// `signal?: AbortSignal`. useConversation threads a per-wave
// AbortController through these so that switching sessions or
// re-firing the 60s reconciliation poll cancels the prior wave's
// in-flight requests — closing the late-resolver race where a stale
// transcript would overwrite a fresh session's state.
export async function getChatState(
  sessionId: string,
  opts?: { signal?: AbortSignal },
): Promise<SessionStateSnapshot> {
  return jsonFetch(sessionUrl(sessionId, 'state'), opts)
}

export async function getChatTranscript(
  sessionId: string,
  opts?: { signal?: AbortSignal },
): Promise<Turn[]> {
  return jsonFetch(sessionUrl(sessionId, 'transcript'), opts)
}

export async function confirmChatSession(
  sessionId: string,
  opts?: {
    rationale?: string
    mode?: SessionMode
    checkpointMode?: CheckpointMode
  },
): Promise<void> {
  // Optional mode + checkpoint_mode flow through so the
  // ConfirmationTurnCard dropdowns can lock the session's discipline
  // on the first confirm. Empty body preserves the legacy shape for
  // callers that don't pass the options.
  const body: Record<string, unknown> = {}
  if (opts?.rationale !== undefined) body.rationale = opts.rationale
  if (opts?.mode !== undefined) body.mode = opts.mode
  if (opts?.checkpointMode !== undefined) body.checkpoint_mode = opts.checkpointMode
  return voidFetch(sessionUrl(sessionId, 'confirm'), {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  })
}

export async function rejectChatSession(sessionId: string): Promise<void> {
  return voidFetch(sessionUrl(sessionId, 'reject'), { method: 'POST' })
}

export async function unblockChatSession(
  sessionId: string,
  opts?: { resolution?: 'resize' | 'retry' | 'abort'; rationale?: string },
): Promise<void> {
  const body =
    opts && (opts.resolution || opts.rationale)
      ? JSON.stringify({
          resolution: opts.resolution ?? null,
          rationale: opts.rationale ?? null,
        })
      : undefined
  return voidFetch(sessionUrl(sessionId, 'unblock'), {
    method: 'POST',
    headers: body ? { 'Content-Type': 'application/json' } : undefined,
    body,
  })
}

/// UI-driven execution start + status endpoints.
/// Mirrors crates/server/src/chat_routes.rs ExecutionStatusResponse.
export interface ExecutionStatus {
  pid: number
  pgid: number
  started_at: string
  package_dir: string
  agent_command: string
  /// `running` | `pausing` | `paused` | `stopping` | `exited`. See the
  /// harness state-machine doc on the server's ExecutionHandle.
  /// `pausing` is the in-flight window after /pause was called but
  /// before the harness has observed the sentinel and written the
  ///.harness-paused ack — the UI treats it identically to `paused`
  /// so the SME can always back out via Resume.
  status: 'running' | 'pausing' | 'paused' | 'stopping' | 'exited'
  exit_code?: number
  paused_at?: string
  stop_requested_at?: string
}

export interface StartExecutionRequest {
  agent_path?: string
  max_iterations?: number
}

export async function startExecution(
  sessionId: string,
  req: StartExecutionRequest = {},
): Promise<ExecutionStatus> {
  return jsonFetch(sessionUrl(sessionId, 'start-execution'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(req),
  })
}

export async function getExecution(
  sessionId: string,
): Promise<ExecutionStatus | null> {
  return jsonFetchOrNull(sessionUrl(sessionId, 'execution'))
}

/// Cooperative pause — server sets pause_requested + writes
/// runtime/.harness-pause; harness self-suspends at top of next iter.
/// Resume with `resumeExecution`.
export async function pauseExecution(sessionId: string): Promise<void> {
  return voidFetch(sessionUrl(sessionId, 'execution/pause'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: '{}',
  })
}

export async function resumeExecution(sessionId: string): Promise<void> {
  return voidFetch(sessionUrl(sessionId, 'execution/resume'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: '{}',
  })
}

/// Cooperative graceful stop — harness aborts in-flight agent, marks
/// the in-flight task back to `ready` in WORKFLOW.json, archives its
/// WAL line, exits 0. Restart afterward via `startExecution`.
export async function stopExecution(sessionId: string): Promise<void> {
  return voidFetch(sessionUrl(sessionId, 'execution/stop'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: '{}',
  })
}

/// Hard kill — SIGTERM the harness's pgid, taking down the entire
/// agent + claude subtree atomically. No state cleanup. Use only when
/// `stopExecution` doesn't take effect (harness hung). Confirm via
/// modal before calling.
export async function killExecution(sessionId: string): Promise<void> {
  return voidFetch(sessionUrl(sessionId, 'execution/kill'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: '{}',
  })
}

/// Discovery-approval blocker affordances.

export interface DiscoveryDecision {
  task_id: string
  top_candidate: string
  runner_ups?: string[]
  scores?: Record<string, number>
  rationale?: string
  auto_picked?: boolean
}

/// Read a task's decision.json artifact. Returns null on 404 so the
/// BlockerCard can fall back to plain-text reason display.
export async function getTaskDecision(
  sessionId: string,
  taskId: string,
): Promise<DiscoveryDecision | null> {
  return jsonFetchOrNull(
    `${sessionUrl(sessionId, 'artifacts/runtime/outputs')}/${encodeURIComponent(taskId)}/decision.json`,
  )
}

/// Structured blocker surface. Reads
/// runtime/outputs/<task>/blocker.json via the dedicated endpoint
/// (not the generic artifact path) so the server can normalise the
/// response shape + return `{blocker: null}` for "the agent hasn't
/// yet structured this block" rather than a 404.
export interface AgentBlockerJson {
  blocker_kind?: string
  decision_points_for_sme?: Array<{
    id: string
    question: string
    options?: Array<{
      id: string
      label?: string
      risk?: string
      consequence?: string
    }>
    default_if_unanswered?: string | null
  }>
  sme_pinned_method?: string
  missing_capability?: string
  recommended_substitute?: string
  summary?: string
  /// Single-method recommendation (e.g., "multi_repo_processed_matrices_python")
  /// or a hybrid sentinel like "hybrid:<a>+<b>" when multiple methods are
  /// needed to cover all cohort accessions. The hybrid case ALSO carries
  /// `top_candidate_components` with the per-method coverage breakdown.
  top_candidate?: string
  /// When the agent's recommended method is a hybrid (one method has
  /// partial accession coverage and another covers the rest), this
  /// array enumerates the components: each entry is a single method_id
  /// that contributes coverage. The BlockerCard prefers this over
  /// `decision.json.top_candidate` because decision.json forces a
  /// single top_candidate even when it has < 100% coverage.
  top_candidate_components?: Array<{
    method_id: string
    score?: number
    covers_accessions?: string[]
  }>
  /// When set, BlockerCard renders a "Retry: <label>" button that
  /// POSTs to /task/:task_id/rerun-script. The path must live under
  /// the task's scripts/ subdir (server enforces).
  recoverable_action?: {
    kind: 'rerun_script'
    /// Path relative to runtime/outputs/<task_id>/scripts/.
    rel_path: string
    /// Button label shown to the SME.
    label?: string
    /// One-line explanation of what the rerun does, rendered next to
    /// the button.
    description?: string
  }
  [key: string]: unknown
}

/// Canonical attempts shape per CLAUDE.md: each entry is exactly
/// `{ method, result }` strings. Lives on `task.state.record.attempts`
/// in WORKFLOW.json; the server merges it into the blocker response so
/// the BlockerCard can show "Tried so far" without a second roundtrip.
export interface BlockerAttempt {
  method: string
  result: string
}

export interface TaskBlockerPayload {
  blocker: AgentBlockerJson | null
  attempts: BlockerAttempt[]
}

export async function getTaskBlocker(
  sessionId: string,
  taskId: string,
): Promise<AgentBlockerJson | null> {
  const body = await jsonFetchOrNull<TaskBlockerPayload>(
    sessionUrl(sessionId, 'blocker', taskId),
  )
  return body?.blocker ?? null
}

/// Returns blocker + attempts in one call. Use this from the
/// BlockerCard when both pieces are needed.
export async function getTaskBlockerPayload(
  sessionId: string,
  taskId: string,
): Promise<TaskBlockerPayload> {
  const body = await jsonFetchOrNull<TaskBlockerPayload>(
    sessionUrl(sessionId, 'blocker', taskId),
  )
  return body ?? { blocker: null, attempts: [] }
}

export interface SmeDecisionAnswer {
  id: string
  chosen: string
  rationale?: string
}

/// POST the SME's structured-blocker answers + fire the auto-relaunch
/// hook server-side. Records one `AppliedStructuredDecision` per
/// answer in the session's decision log. Throws on non-2xx so callers
/// can distinguish 4xx (validation) from 5xx (server) without losing
/// the body text.
export async function postSmeDecisions(
  sessionId: string,
  taskId: string,
  decisions: SmeDecisionAnswer[],
  rationale?: string,
): Promise<void> {
  await voidFetch(
    sessionUrl(sessionId, 'sme-decisions', taskId),
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        task_id: taskId,
        decisions,
        rationale: rationale ?? null,
      }),
    },
  )
}

/// Writes the SME's candidate choice to the package so the resuming
/// agent picks it up. Use when the SME selected a runner-up instead
/// of the top candidate. Throws on non-2xx.
export async function postSmeSelection(
  sessionId: string,
  taskId: string,
  chosen: string,
): Promise<void> {
  await voidFetch(
    sessionUrl(sessionId, 'sme-selection', taskId),
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ chosen }),
    },
  )
}

/// Session-scoped "auto-approve all future discoveries" flag. Writes
/// a marker file the agents check before emitting a discovery blocker.
/// Throws on non-2xx.
export async function setAutoApproveDiscoveries(
  sessionId: string,
): Promise<void> {
  await voidFetch(
    sessionUrl(sessionId, 'auto-approve-discoveries'),
    { method: 'POST' },
  )
}

/// Atom-safety-policy add a package to the session-scoped
/// override of an atom's `runtime_packages` list. The harness's
/// `enforce_safety_policy` gate refused dispatch when the atom requested
/// a package the policy hadn't allowed; this endpoint widens the
/// allowlist so the SME can retry without amending the upstream atom
/// definition. Task 4.7 lands the server-side handler; until then a
/// non-2xx response surfaces in the calling component's error state.
export async function postAddRuntimePackage(
  sessionId: string,
  atomId: string,
  pkg: string,
  registry: string,
): Promise<void> {
  await voidFetch(
    `${sessionUrl(sessionId, 'atom')}/${encodeURIComponent(atomId)}/add-runtime-package`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ package: pkg, registry }),
    },
  )
}

/// Fetch per-session metrics. Returns null if the server has no metrics
/// for this session (e.g. before the first turn lands).
export async function getChatMetrics(sessionId: string): Promise<SessionMetrics | null> {
  return jsonFetchOrNull(sessionUrl(sessionId, 'metrics'))
}

// `RubricScore` re-exported from the top-of-file ts-rs barrel.

/// Maximum possible total (9 dimensions × 2). Mirrors
/// `RubricScore::MAX_TOTAL` in Rust.
export const RUBRIC_MAX_TOTAL = 18

/// Rubric pass threshold used by the nightly gate. Mirrors
/// `RubricScore::PASS_THRESHOLD` in Rust.
export const RUBRIC_PASS_THRESHOLD = 14

/// Run the rubric scorer against the session's current transcript.
/// Spend is billed into the session's `scorer_cost_usd` bucket so the
/// Performance tab updates on the next poll.
export async function scoreChatSession(sessionId: string): Promise<RubricScore> {
  return jsonFetch(sessionUrl(sessionId, 'score'), { method: 'POST' })
}

/// Fetch the session's built DAG. Returns null if no DAG has been
/// built yet (e.g. before the first append_intake_prose).
///
/// Accepts an optional `signal?: AbortSignal` so the
/// 60s reconciliation poll + session-switch wave in useConversation
/// can cancel a prior, still-in-flight DAG fetch.
export async function getChatDag(
  sessionId: string,
  opts?: { signal?: AbortSignal },
): Promise<DAG | null> {
  return jsonFetchOrNull(sessionUrl(sessionId, 'dag'), opts)
}

// Per-task result + artifact surface. Mirrors the Rust wire types in
// chat_routes.rs (ArtifactRef + the JSON body from `get_task_result`).

export interface ArtifactRef {
  name: string
  relative_path: string
  size_bytes: number
  mime_type: string
}

export type TaskResultStatus =
  | 'completed'
  | 'failed'
  | 'blocked'
  | 'pending'
  | 'ready'
  | 'running'

export interface TaskResultPayload {
  task_id: string
  status: TaskResultStatus
  description: string
  // Serialized `scripps_workflow_core::dag::TaskKind`.
  kind: unknown
  artifacts: ArtifactRef[]
  // Present when status === 'completed'.
  result?: unknown
  // Present when status === 'failed'.
  reason?: string
  // Present when status === 'blocked'.
  record?: unknown
  // Per-task LLM-generated code capture. Present only when the agent
  // has run and written runtime/outputs/<task_id>/agent-code.json.
  agent_code?: AgentCodeRecord | null
  // Claim verification rollup. Present only when the package's
  // interpretation policy declares a `verifiableEntities` block AND
  // the task has a narrative artifact. Null otherwise — the server
  // populates either from a fast-path sidecar at
  // `runtime/verification-reports/<task_id>.json` or by running
  // `claim_extractor` + `claim_verifier` live on the blocking pool.
  verification?: ClaimVerificationReport | null
}


export async function getTaskResult(
  sessionId: string,
  taskId: string,
): Promise<TaskResultPayload> {
  return jsonFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/task/${encodeURIComponent(taskId)}/result`,
  )
}

// Companion POST endpoint. Re-runs claim_extractor + claim_verifier
// and transitions the session to `Blocked { ValidationFailed }` if any
// mismatch is found. Idempotent — calling it on a session already
// Blocked with the same task just returns the fresh report without
// double-transitioning.
export interface VerifyTaskResponse {
  report: ClaimVerificationReport | null
  reason?: string
}

export async function verifyTask(
  sessionId: string,
  taskId: string,
): Promise<VerifyTaskResponse> {
  return jsonFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/task/${encodeURIComponent(taskId)}/verify`,
    { method: 'POST' },
  )
}

/// Build a URL the browser can GET/<img src> for an artifact served out
/// of the session's emitted package. Forwards `?share_token=` when the
/// page was opened with one — `<img src>` and `<a href>` can't carry the
/// X-Share-Token header that `_fetch.ts` uses, so the query param is the
/// only way read-only viewers see artifacts. Server accepts either per
/// `crates/server/src/read_only.rs:30-37`.
export function artifactUrl(sessionId: string, path: string): string {
  const base = `/api/chat/session/${encodeURIComponent(sessionId)}/artifacts/${path.replace(/^\/+/, '')}`
  if (typeof window === 'undefined') return base
  const tok = new URLSearchParams(window.location.search).get('share_token')
  return tok ? `${base}?share_token=${encodeURIComponent(tok)}` : base
}

// ── Task-detail drawer endpoints ───────────────────────────────────────

/// Plain-English stage description shared across sessions on a server.
/// Served from config/stage-descriptions.yaml — static, safe to cache
/// for the lifetime of the tab.
export interface StageDescription {
  sme_friendly_name: string
  short: string
  long?: string
  example_inputs?: string
  example_outputs?: string
}

export interface StageDescriptionsResponse {
  version: number
  stages: Record<string, StageDescription>
}

// Module-level cache: stage-descriptions is a static config blob keyed
// to the server build, so caching for the lifetime of the tab is safe
// per the docstring at line 700-701. The promise is reused so concurrent
// callers de-duplicate the fetch.
let _stageDescriptionsPromise: Promise<StageDescriptionsResponse> | null = null
export function getStageDescriptions(): Promise<StageDescriptionsResponse> {
  if (!_stageDescriptionsPromise) {
    _stageDescriptionsPromise = jsonFetch('/api/chat/stage-descriptions')
  }
  return _stageDescriptionsPromise
}

/// Paginated progress.log tail for one task. Pass `sinceLine` from the
/// prior response's `next_since_line` to fetch only new lines on each
/// poll (the drawer ticks every 2 s while the task is running).
export interface ProgressLogResponse {
  lines: string[]
  total_lines: number
  next_since_line: number
  truncated: boolean
}

export async function getProgressLog(
  sessionId: string,
  taskId: string,
  sinceLine = 0,
): Promise<ProgressLogResponse> {
  const url = `/api/chat/session/${encodeURIComponent(
    sessionId,
  )}/task/${encodeURIComponent(taskId)}/progress-log?since_line=${sinceLine}`
  return jsonFetch(url)
}

/// Status-sentinel files written by long-running scripts inside a task's
/// output directory (e.g. `integration_status.OK`, `*.FAILED`,
/// `install_status`). Surfaces them so the SME sees an out-of-band
/// failure even when WORKFLOW.json hasn't yet transitioned.
export type StatusSentinelKind = 'ok' | 'failed' | 'pending' | 'status_file'

export interface StatusSentinel {
  name: string
  kind: StatusSentinelKind
  /** Unix epoch seconds (0 if filesystem mtime unreadable). */
  mtime_unix: number
  /** First 512 chars of file contents, trimmed. May be empty. */
  body: string
}

export interface StatusSentinelsResponse {
  sentinels: StatusSentinel[]
}

export async function getTaskStatusSentinels(
  sessionId: string,
  taskId: string,
): Promise<StatusSentinelsResponse> {
  const url = `/api/chat/session/${encodeURIComponent(
    sessionId,
  )}/task/${encodeURIComponent(taskId)}/status-sentinels`
  return jsonFetch(url)
}

/// Generalised file tail used by the per-task Logs tab. Same shape as
/// progress-log but accepts a relative path (jailed to the task's
/// output directory by the server).
export async function getTaskLogTail(
  sessionId: string,
  taskId: string,
  relPath: string,
  sinceLine = 0,
): Promise<ProgressLogResponse> {
  const params = new URLSearchParams({
    path: relPath,
    since_line: String(sinceLine),
  })
  const url = `/api/chat/session/${encodeURIComponent(
    sessionId,
  )}/task/${encodeURIComponent(taskId)}/log-tail?${params.toString()}`
  return jsonFetch(url)
}

/// File entry under a task's output directory, used by the Logs +
/// Scripts tabs to populate selectable lists.
export interface TaskFileEntry {
  /** Path relative to the task's output dir (e.g. "scripts/01_install.R" or "integration_run.log"). */
  rel_path: string
  size_bytes: number
  mtime_unix: number
}

export interface TaskFileListResponse {
  files: TaskFileEntry[]
}

export async function listTaskLogs(
  sessionId: string,
  taskId: string,
): Promise<TaskFileListResponse> {
  const url = `/api/chat/session/${encodeURIComponent(
    sessionId,
  )}/task/${encodeURIComponent(taskId)}/logs`
  return jsonFetch(url)
}

export async function listTaskScripts(
  sessionId: string,
  taskId: string,
): Promise<TaskFileListResponse> {
  const url = `/api/chat/session/${encodeURIComponent(
    sessionId,
  )}/task/${encodeURIComponent(taskId)}/scripts`
  return jsonFetch(url)
}

export interface RerunScriptResponse {
  pid: number | null
  message: string
}

export async function postRerunScript(
  sessionId: string,
  taskId: string,
  relPath: string,
): Promise<RerunScriptResponse> {
  return jsonFetch<RerunScriptResponse>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/task/${encodeURIComponent(taskId)}/rerun-script`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ rel_path: relPath }),
    },
  )
}

export interface StuckTaskInfo {
  task_id: string
  reason: string
  /// One of "failed_sentinel_no_transition", "no_patch_after_heartbeat".
  kind: string
  last_heartbeat_unix: number
  failing_sentinel: string | null
}

export interface StuckTasksResponse {
  stuck: StuckTaskInfo[]
}

export async function getStuckTasks(
  sessionId: string,
): Promise<StuckTasksResponse> {
  const url = `/api/chat/session/${encodeURIComponent(sessionId)}/stuck-tasks`
  return jsonFetch(url)
}

/// Per-running-task summary returned by /active-tasks. The Rust-side
/// type lives in `crates/server/src/chat_routes/tasks.rs`; tagging
/// here is `serde(rename_all = "snake_case")` so `progress.kind` is
/// `"determinate"` or `"indeterminate"` on the wire. The UI panel
/// re-derives the list every 2s; tasks that transition Running →
/// Completed/Blocked/Failed simply fall out of the response and the
/// corresponding card auto-removes.
export type ActiveTaskProgress =
  | { kind: 'determinate'; completed: number; total: number; unit: string }
  | { kind: 'indeterminate'; eta_min_secs: number | null; eta_max_secs: number | null }

export interface ActiveTaskSummary {
  task_id: string
  stage_class: string
  friendly_name: string
  started_at: string
  elapsed_secs: number
  heartbeat_age_secs: number | null
  last_progress_line: string | null
  progress: ActiveTaskProgress
}

export async function getActiveTasks(
  sessionId: string,
): Promise<ActiveTaskSummary[]> {
  const url = `/api/chat/session/${encodeURIComponent(sessionId)}/active-tasks`
  const body = await jsonFetch<{ active_tasks: ActiveTaskSummary[] }>(url)
  return body.active_tasks ?? []
}

/// Title-bar "Recent ▼" dropdown row. Returned by
/// `GET /api/chat/sessions/recent?limit=N`. Sorted newest-first by
/// `last_activity` so the dropdown surfaces the workflow the SME most
/// recently touched at the top.
///
/// `state_kind` is the logical session state (`emitted`, `blocked`, …);
/// `execution_status` is the harness liveness signal that comes from the
/// same source `/execution` consumes — the two are independent. Render
/// them as separate pills so an SME can tell `Emitted` (package exists,
/// no harness running) apart from `Emitted` + `Running` (harness
/// actively working). Older clients that don't send the field default
/// to "idle" in the UI.
export interface RecentSessionSummary {
  session_id: string
  title: string | null
  created_at: string
  last_activity: string
  state_kind: string
  execution_status: 'running' | 'exited' | 'idle'
  parent_id: string | null
  n_turns: number
  project_class: string
}

export async function getRecentSessions(
  limit = 20,
): Promise<RecentSessionSummary[]> {
  const url = `/api/chat/sessions/recent?limit=${encodeURIComponent(String(limit))}`
  return jsonFetch<RecentSessionSummary[]>(url)
}

/// Impact preview for a proposed amend or rerun. Pure GET-style call
/// (POST only because it carries an optional body). Never mutates.
export interface ImpactPreviewEntry {
  task_id: string
  description: string
  stage_class?: string
  current_status?: string
  est_cost_usd_min: number
  est_cost_usd_max: number
  cost_source: 'prior_run' | 'stage_median' | 'coarse_default'
}

export interface ImpactPreviewResponse {
  target_task_id: string
  proposed_method?: string
  invalidated_tasks: ImpactPreviewEntry[]
  invalidated_count: number
  est_cost_usd_min: number
  est_cost_usd_max: number
}

export async function postImpactPreview(
  sessionId: string,
  taskId: string,
  proposedMethod?: string,
): Promise<ImpactPreviewResponse> {
  const url = `/api/chat/session/${encodeURIComponent(
    sessionId,
  )}/task/${encodeURIComponent(taskId)}/impact-preview`
  return jsonFetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ proposed_method: proposedMethod ?? null }),
  })
}

export interface AmendMethodResponse {
  task_id: string
  invalidated_tasks: string[]
  /**
   * The method prose the stage carried *before* this amendment
   * applied. Empty string when the stage had no prior method. Drives
   * the Undo toast's revert button.
   */
  prior_method_prose?: string
}

/// Submit an amendment. Fires server auto-relaunch when the resulting
/// session state has ready work. Throws on 400 (session not Emitted,
/// empty method_prose, missing rationale in confirmatory mode, etc.);
/// the drawer catches and surfaces the message inline.
export async function postAmendMethod(
  sessionId: string,
  taskId: string,
  methodProse: string,
  rationale?: string,
): Promise<AmendMethodResponse> {
  const url = `/api/chat/session/${encodeURIComponent(
    sessionId,
  )}/task/${encodeURIComponent(taskId)}/amend-method`
  return jsonFetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      method_prose: methodProse,
      rationale: rationale ?? null,
    }),
  })
}

/// Reverse a just-applied amendment by re-applying the prior prose.
/// Records a `DecisionType::UndoneAmendment` entry server-side.
export async function postUndoAmendment(
  sessionId: string,
  taskId: string,
  revertedProse: string,
): Promise<AmendMethodResponse> {
  const url = `/api/chat/session/${encodeURIComponent(
    sessionId,
  )}/task/${encodeURIComponent(taskId)}/undo-amendment`
  return jsonFetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      reverted_prose: revertedProse,
    }),
  })
}

export interface RerunResponse {
  task_id: string
  invalidated_tasks: string[]
}

export async function postRerun(
  sessionId: string,
  taskId: string,
  reason?: string,
): Promise<RerunResponse> {
  const url = `/api/chat/session/${encodeURIComponent(
    sessionId,
  )}/task/${encodeURIComponent(taskId)}/rerun`
  return jsonFetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ reason: reason ?? null }),
  })
}

// HTTP-DTO wrapping a list of decisions. The inner shape is
// structurally the ts-rs `DecisionRecord` (re-exported above) but uses
// a loose `Record<string, unknown> & { kind: string }` for `decision`
// so the DecisionsTab can do open-world `record.decision[field]`
// bracket access without explicit narrowing per `DecisionType`
// variant. Promote to `DecisionRecord[]` only after the DecisionsTab
// switches to a per-variant switch on `record.decision.kind`.
export interface DecisionsResponse {
  session_id: string
  decisions: Array<{
    timestamp: string
    session_id: string
    decision: Record<string, unknown> & { kind: string }
    rationale?: string
    actor: DecisionActor
  }>
}

export async function getDecisions(
  sessionId: string,
  filter?: string,
): Promise<DecisionsResponse> {
  const q = filter ? `?filter=${encodeURIComponent(filter)}` : ''
  return jsonFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/decisions${q}`,
  )
}

// ── Disposition auto-apply ──────────────────────────────────────────────
//
// Agent-authored `runtime/outputs/<task>/sme_disposition.json` files surface
// through these routes.

// Alias the generated `DispositionStatus` (ts-rs from
// `crates/core::disposition::DispositionStatus`) so chatClient call
// sites that imported `DispositionStatusWire` keep working without
// re-thread to the canonical name.
export type DispositionStatusWire = DispositionStatus

export interface DispositionListEntryWire {
  path: string
  task_id: string
  status: DispositionStatusWire
  schema_version: number
  action_count: number
  created_at?: string
  authoritative_interpretation?: string
}

export interface DispositionListResponseWire {
  session_id: string
  dispositions: DispositionListEntryWire[]
}

export async function listDispositions(
  sessionId: string,
): Promise<DispositionListResponseWire> {
  return jsonFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/dispositions`,
  )
}

/**
 * The normalized disposition body. Structurally matches
 * `crates/core/bindings/Disposition.ts` but written out by hand so the
 * UI isn't forced to regenerate types when the Rust side adds
 * passthrough fields.
 */
export interface DispositionBodyWire {
  schema_version: number
  task_id: string
  created_at?: string
  authoritative_interpretation?: string
  actions: Array<Record<string, unknown> & { kind: string }>
  auto_apply: boolean
  status: DispositionStatusWire
  status_updated_at?: string
  [key: string]: unknown
}

export async function getDisposition(
  sessionId: string,
  path: string,
): Promise<DispositionBodyWire | null> {
  // The server wildcard is `/view/*path` — path segments preserved.
  return jsonFetchOrNull(
    `/api/chat/session/${encodeURIComponent(sessionId)}/dispositions/view/${path.replace(/^\/+/, '')}`,
  )
}

export interface ApplyDispositionResponseWire {
  applied: number
  failed: number
  invalidated_tasks: string[]
  status: DispositionStatusWire
  errors: Array<{
    action_index: number
    action_kind: string
    target_stage: string
    reason: string
  }>
}

export async function applyDisposition(
  sessionId: string,
  path: string,
  opts?: { rationale?: string; dryRun?: boolean },
): Promise<ApplyDispositionResponseWire> {
  const url = `/api/chat/session/${encodeURIComponent(
    sessionId,
  )}/dispositions/apply/${path.replace(/^\/+/, '')}`
  const body: Record<string, unknown> = {}
  if (opts?.rationale !== undefined) body.rationale = opts.rationale
  if (opts?.dryRun) body.dry_run = true
  return jsonFetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
}

export async function applyDispositionAction(
  sessionId: string,
  path: string,
  actionIndex: number,
  rationale?: string,
): Promise<{
  applied: number
  action_index: number
  invalidated_tasks: string[]
  status: DispositionStatusWire
}> {
  const url = `/api/chat/session/${encodeURIComponent(
    sessionId,
  )}/dispositions/apply-one/${path.replace(/^\/+/, '')}`
  return jsonFetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      action_index: actionIndex,
      rationale: rationale ?? null,
    }),
  })
}

export async function rejectDisposition(
  sessionId: string,
  path: string,
  rationale?: string,
): Promise<{ status: DispositionStatusWire }> {
  const url = `/api/chat/session/${encodeURIComponent(
    sessionId,
  )}/dispositions/reject/${path.replace(/^\/+/, '')}`
  return jsonFetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ rationale: rationale ?? null }),
  })
}

export async function scanDispositions(
  sessionId: string,
): Promise<{ session_id: string; dispositions_found: number }> {
  return jsonFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/dispositions/scan`,
    { method: 'POST' },
  )
}

// ── Endpoints previously called via inline fetch() ────────────────────
// These wrappers route through _fetch.ts so X-Share-Token forwarding +
// error normalization apply uniformly.

export interface ChildSummaryWire {
  session_id: string
  created_at: string
  lineage: {
    parent_session_id: string
    branched_at: string
    branched_from_turn_index?: number | null
  } | null
  state_kind: string
}

export async function listChildSessions(
  parentId: string,
): Promise<ChildSummaryWire[]> {
  return jsonFetch(`/api/chat/sessions?parent=${encodeURIComponent(parentId)}`)
}

/** Pilot sizing report. Returns null until the harness has produced one. */
export async function getPilot(sessionId: string): Promise<unknown | null> {
  return jsonFetchOrNull(
    `/api/chat/session/${encodeURIComponent(sessionId)}/pilot`,
  )
}

export interface DashboardViewWire {
  view_id: string
  data_url: string
}
export interface DashboardStageWire {
  stage_id: string
  description: string
  views: DashboardViewWire[]
}
export interface DashboardIndexWire {
  session_id: string
  stages: DashboardStageWire[]
}

export async function getDashboardIndex(
  sessionId: string,
): Promise<DashboardIndexWire> {
  return jsonFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/dashboard/index`,
  )
}

/** Top-level cross-version diff report. Throws on 404 (no parent). */
export async function getCrossVersionDiff(
  sessionId: string,
): Promise<CrossVersionReport | null> {
  return jsonFetchOrNull(
    `/api/chat/session/${encodeURIComponent(sessionId)}/cross-version-diff`,
  )
}

/**
 * Typed remote-execution shape on harness backlog events.
 * Mirrors `crates/server/src/chat_routes/wire_types.rs::RemoteExecutionInfoWire`
 * which is structurally identical to the core
 * `RemoteExecutionInfo` ts-rs binding — alias so the `remote` field
 * round-trips without an `as any` cast at the consumer site
 * (useSseChatEvents.ts:187 used to do this).
 */
export type RemoteExecutionWire = RemoteExecutionInfo

// Wire variant: server emits `task_id` (snake_case); the
// SSE-handling layer in `useSseChatEvents.ts` re-keys to camelCase
// `taskId` before storing, which is what every UI consumer reads.
// Intentionally NOT the ts-rs `HarnessEvent` (which is snake_case).
export interface HarnessEventWire {
  kind: string
  taskId: string
  status: string
  detail: string
  remote?: RemoteExecutionWire | null
  timestamp: string
}
export interface HarnessEventsResponse {
  events: HarnessEventWire[]
}

export async function getHarnessEventsBacklog(
  sessionId: string,
): Promise<HarnessEventsResponse> {
  return jsonFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/harness-events`,
  )
}

interface BranchResponseWire {
  session_id?: string
  branched_session_id?: string
}

export async function postBranch(
  sessionId: string,
  opts: { rationale?: string; taskId?: string },
): Promise<{ session_id: string }> {
  const body: Record<string, string> = {}
  if (opts.rationale) body.rationale = opts.rationale
  if (opts.taskId) body.task_id = opts.taskId
  const response = await jsonFetch<BranchResponseWire>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/branch`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    },
  )
  const childId = response.session_id ?? response.branched_session_id
  if (!childId) {
    throw new Error('branch response did not include a child session id')
  }
  return { session_id: childId }
}

// ── SME data inputs ─────────────────────────────────────────────────
// Powers the Inputs inspector tab. Two registration paths today:
// - registerInputPath: SME points the server at an existing
// directory (server walks + hashes; allowlist via ECAA_INPUT_ROOTS).
// - uploadInputFile (phase E): chunked browser upload.
// Both return the updated Session.inputs list so the tab can re-render.
// `UserInput` re-exported from the top-of-file ts-rs barrel.

export async function listInputs(sessionId: string): Promise<UserInput[]> {
  return jsonFetch<UserInput[]>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/inputs`,
  )
}

export interface RegisterInputPathBody {
  path: string
  label?: string
}

/// Registers a server-local directory path as a session input. Returns
/// the new full inputs list. The server canonicalizes the path, walks
/// the tree, computes per-file size + sha256, and persists. 400 on
/// allowlist / non-existent / non-dir errors with a human-readable
/// message in the response body — surface that text in the UI rather
/// than a generic "Failed".
export async function registerInputPath(
  sessionId: string,
  body: RegisterInputPathBody,
): Promise<UserInput[]> {
  return jsonFetch<UserInput[]>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/inputs/path`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    },
  )
}

/// Removes an input registration from the session. Does NOT delete the
/// underlying files (those are the SME's data; for uploaded_files, a
/// later GC pass cleans up un-referenced uploads).
export async function deleteInput(
  sessionId: string,
  inputId: string,
): Promise<UserInput[]> {
  return jsonFetch<UserInput[]>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/inputs/${encodeURIComponent(inputId)}`,
    { method: 'DELETE' },
  )
}

const UPLOAD_CHUNK_BYTES = 8 * 1024 * 1024 // 8 MiB matches server-recommended chunk size

async function sha256Hex(file: Blob): Promise<string> {
  // crypto.subtle is available in all modern browsers; fall back path
  // (incremental hash via a Worker) is added if a target browser ever
  // lacks it. arrayBuffer() materializes the file in RAM — fine for
  // single-cell matrix uploads of up to a few GB on typical hardware.
  const buf = await file.arrayBuffer()
  const digest = await crypto.subtle.digest('SHA-256', buf)
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}

function genUploadToken(): string {
  // 16 hex chars from crypto.getRandomValues — no '/' or '.' so the
  // server's regex (alphanumeric + '-') is happy.
  const arr = new Uint8Array(8)
  crypto.getRandomValues(arr)
  return Array.from(arr)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}

/// Stream `file` to the server as a sequence of 8 MiB chunks, then
/// returns the per-file completion payload. The caller usually invokes
/// this once per file in a batch and then calls `finalizeUpload(token)`
/// to register them all as a single UserInput.
///
/// `onProgress` fires after each chunk completes, so the UI can render
/// a progress bar without instrumenting fetch internals.
export async function uploadInputFile(
  sessionId: string,
  file: File,
  uploadToken: string = genUploadToken(),
  onProgress?: (bytesTransferred: number, totalBytes: number) => void,
): Promise<{ status: string; sha256?: string; final_path?: string }> {
  const total = file.size
  const sha = await sha256Hex(file)
  let offset = 0
  while (offset < total) {
    const end = Math.min(offset + UPLOAD_CHUNK_BYTES, total)
    const chunk = file.slice(offset, end)
    const headers: Record<string, string> = {
      'Upload-Token': uploadToken,
      'Content-Range': `bytes ${offset}-${end - 1}/${total}`,
    }
    if (offset === 0) headers['Upload-Filename'] = file.name
    if (end === total) headers['Upload-Sha256'] = sha
    // Body is a binary Blob, NOT JSON — but jsonFetch's only requirement
    // on the request body is that it's a valid BodyInit. Response is
    // JSON, so jsonFetch is the right call. Auth + share-token
    // decoration applies uniformly.
    const body = await jsonFetch<{
      status: string
      received_bytes?: number
      total_bytes?: number
      sha256?: string
      final_path?: string
    }>(
      `/api/chat/session/${encodeURIComponent(sessionId)}/inputs/upload`,
      {
        method: 'POST',
        headers,
        body: chunk,
      },
    )
    onProgress?.(end, total)
    if (body.status === 'complete') {
      return body
    }
    offset = end
  }
  throw new Error('upload loop exited without final chunk acknowledgement')
}

export { genUploadToken }

/// Closes a multi-file upload batch by walking the per-token dir and
/// registering the completed files as a single `UserInput` of
/// `kind: uploaded_files`. Returns the new input or null if no files
/// completed.
export async function finalizeUpload(
  sessionId: string,
  uploadToken: string,
): Promise<UserInput | null> {
  return jsonFetch<UserInput>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/inputs/upload/${encodeURIComponent(uploadToken)}/finalize`,
    { method: 'POST' },
  )
}

// `Session` re-exported from the top-of-file ts-rs barrel (used by tests
// as a debug surface only).

// ── Remediation surface ────────────────────────────────────────────────────
// Maps the GET / POST endpoints in
// `crates/server/src/chat_routes/remediation.rs`. Drives the
// `RemediationSuggestionList` BlockerCard variant.

import type {
  RemediationSuggestion,
  ToolErrorEnvelope,
} from '../types'

export interface RemediationSuggestionsResponse {
  envelope: ToolErrorEnvelope
  suggestions: RemediationSuggestion[]
  attempts_consumed: number
  /** True when a fresh proposer call ran for this fetch. */
  regenerated: boolean
}

export interface ApplyRemediationResponse {
  suggestion_id: string
  tool_binding:
    | 'rerun_task'
    | 'amend_stage_method'
    | 'set_intake_field'
    | 'rerun_upstream_task'
    | 'operator_action'
    | 'manual_only'
  outcome: 'applied' | 'guidance_only'
  message: string
  overrides_path: string
}

/** Fetch the typed remediation envelope + ranked suggestions for a failed task. */
export async function getRemediationSuggestions(
  sessionId: string,
  taskId: string,
): Promise<RemediationSuggestionsResponse | null> {
  return jsonFetchOrNull<RemediationSuggestionsResponse>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/task/${encodeURIComponent(taskId)}/remediation-suggestions`,
  )
}

// ── Flexible plotting — renderer proposal ────────────────────────────────────
// Wraps POST /api/chat/session/:id/tool/propose_hypothesized_renderer.
// This direct endpoint is used by the RendererProposalCard form in the
// ResultReviewTurnCard so the SME can describe a preferred plot for a
// figure that resolved via StructuralFallback, without having to type
// in free text and wait for the LLM to invoke the tool.

export interface ProposeHypothesizedRendererArgs {
  sessionId: string
  targetSemanticType: string
  proposedParentTerms: string[]
  proposedFigureIds: string[]
  smeIntent: string
  primitiveBasis: string | null
}

export type ProposeHypothesizedRendererResult =
  | { outcome: 'proposal_accepted'; proposal_id: string }
  | { outcome: 'proposal_rejected'; reason: string }

export async function proposeHypothesizedRenderer(
  args: ProposeHypothesizedRendererArgs,
): Promise<ProposeHypothesizedRendererResult> {
  return jsonFetch<ProposeHypothesizedRendererResult>(
    `/api/chat/session/${encodeURIComponent(args.sessionId)}/tool/propose_hypothesized_renderer`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        target_semantic_type: args.targetSemanticType,
        proposed_parent_terms: args.proposedParentTerms,
        proposed_figure_ids: args.proposedFigureIds,
        sme_intent: args.smeIntent,
        primitive_basis: args.primitiveBasis,
      }),
    },
  )
}

/** Apply a previously-fetched suggestion. */
export async function applyRemediation(
  sessionId: string,
  taskId: string,
  suggestionId: string,
  rationale?: string,
): Promise<ApplyRemediationResponse> {
  return jsonFetch<ApplyRemediationResponse>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/task/${encodeURIComponent(taskId)}/apply-remediation`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ suggestion_id: suggestionId, rationale }),
    },
  )
}

// =====================================================================
// Composition / Proofs / Assumptions / Alternatives
// =====================================================================

/** One row in the AcceptedNodeList card. */
export interface AcceptedNodeRow {
  id: string
  human_name: string
  lifecycle_state: string
  trust_level: string
  intent: string
}

/**
 * Typed `ComposeOutcome` rendered for the Composition tab. Mirrors the
 * server's `ComposeOutcomeResponse` (chat_routes/compose.rs).
 */
export interface ComposeOutcomePayload {
  variant:
    | 'validated_executable_dag'
    | 'draft_dag'
    | 'partial_dag'
    | 'novel_node_spec'
    | 'refusal'
  summary: string
  node_count: number
  edge_count: number
  assumption_count: number
  accepted_nodes: AcceptedNodeRow[]
  refusal?: {
    id?: string
    kind?: string
    statement?: string
    references?: string[]
  } | null
  novel_node_spec?: {
    node_id: string
    intent: string
    proposed_parent_terms: string[]
    declared_inputs: string[]
    declared_outputs: string[]
    declared_assumptions: string[]
    declared_failure_modes: string[]
    validation_obligations: string[]
    llm_rationale?: string
  } | null
  blockers?: unknown[]
  unresolved_gaps?: unknown[]
}

/** GET /api/chat/session/:id/compose-outcome */
export async function getComposeOutcome(
  sessionId: string,
): Promise<ComposeOutcomePayload | null> {
  return jsonFetchOrNull<ComposeOutcomePayload>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/compose-outcome`,
  )
}

export interface AlternativeSummary {
  dag_id: string
  summary: string
  node_count: number
  edge_count: number
  total_adapters: number
  risky_adapters: number
  unresolved_assumptions: number
  reproducibility_score: number
}

export interface AlternativesResponse {
  alternatives: AlternativeSummary[]
}

/** GET /api/chat/session/:id/compose-alternatives */
export async function getComposeAlternatives(
  sessionId: string,
): Promise<AlternativesResponse> {
  const result = await jsonFetchOrNull<AlternativesResponse>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/compose-alternatives`,
  )
  return result ?? { alternatives: [] }
}

export interface ProofsResponse {
  proofs: unknown[]
}

/** GET /api/chat/session/:id/proofs */
export async function getProofs(sessionId: string): Promise<ProofsResponse> {
  const result = await jsonFetchOrNull<ProofsResponse>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/proofs`,
  )
  return result ?? { proofs: [] }
}

export interface AssumptionRow {
  id: string
  statement: string
  source: string
  affects_nodes: string[]
  risk: string
  resolution: 'unresolved' | 'confirmed' | 'rejected' | string
}

export interface AssumptionsResponse {
  assumptions: { entries: AssumptionRow[] }
}

/** GET /api/chat/session/:id/assumptions */
export async function getAssumptions(
  sessionId: string,
): Promise<AssumptionsResponse> {
  const result = await jsonFetchOrNull<AssumptionsResponse>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/assumptions`,
  )
  return result ?? { assumptions: { entries: [] } }
}

export interface PolicyDecisionRow {
  bundle_id: string
  kind: string
  node_id?: string | null
  statement: string
  blocking: boolean
}

export interface PolicyDecisionsResponse {
  decisions: PolicyDecisionRow[]
}

/** GET /api/chat/session/:id/policy-decisions */
export async function getPolicyDecisions(
  sessionId: string,
): Promise<PolicyDecisionsResponse> {
  const result = await jsonFetchOrNull<PolicyDecisionsResponse>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/policy-decisions`,
  )
  return result ?? { decisions: [] }
}

export interface ValidationReportRow {
  task_id: string
  obligation_id: string
  outcome: string
}

export interface ValidationReportsResponse {
  reports: ValidationReportRow[]
}

/** GET /api/chat/session/:id/validation-reports */
export async function getValidationReports(
  sessionId: string,
): Promise<ValidationReportsResponse> {
  const result = await jsonFetchOrNull<ValidationReportsResponse>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/validation-reports`,
  )
  return result ?? { reports: [] }
}

export interface PolicyBundleResponse {
  active_bundle: string | null
}

/**
 * POST /api/chat/session/:id/policy-bundle — activate or clear the
 * session's active policy bundle (clinical_trial / phi_strict).
 * Pass `null` (or undefined) to clear.
 */
export async function setPolicyBundle(
  sessionId: string,
  bundleId: string | null,
): Promise<PolicyBundleResponse> {
  return jsonFetch<PolicyBundleResponse>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/policy-bundle`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ bundle_id: bundleId }),
    },
  )
}

/**
 * POST /api/chat/session/:id/assumption-resolve — record an SME
 * resolution against an unresolved assumption. The server appends
 * a `DecisionType::AssumptionResolved` to the audit log AND
 * updates the in-memory cached `WorkflowDag`'s assumption.
 */
export async function resolveAssumption(
  sessionId: string,
  assumptionId: string,
  resolution: 'confirmed' | 'rejected',
  rationale?: string,
): Promise<void> {
  return voidFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/assumption-resolve`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        assumption_id: assumptionId,
        resolution,
        rationale,
      }),
    },
  )
}

/**
 * POST /api/chat/session/:id/adapter-decision — record an SME
 * confirm/reject decision against an inserted adapter. Persists
 * to the decision log via `DecisionType::AdapterDecisionRecorded`.
 */
export async function recordAdapterDecision(
  sessionId: string,
  adapterId: string,
  decision: 'confirmed' | 'rejected',
  safety?: string,
): Promise<void> {
  return voidFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/adapter-decision`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        adapter_id: adapterId,
        decision,
        safety,
      }),
    },
  )
}

/**
 * POST /api/chat/session/:id/novel-node-decision — record an SME
 * accept/reject decision against a NovelNodeSpec outcome. Persists
 * to the decision log via `DecisionType::NovelNodeDecisionRecorded`.
 */
export async function recordNovelNodeDecision(
  sessionId: string,
  nodeId: string,
  decision: 'accepted_as_draft' | 'rejected',
): Promise<void> {
  return voidFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/novel-node-decision`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        node_id: nodeId,
        decision,
      }),
    },
  )
}

/**
 * POST /api/chat/session/:id/refusal-acknowledge — record the SME's
 * chosen recovery affordance after a Refusal composition outcome.
 * Persists to the decision log via `DecisionType::RefusalAcknowledged`.
 */
export async function acknowledgeRefusal(
  sessionId: string,
  refusalId: string,
  recovery: 'branch' | 'amend_policy' | 'dismiss',
): Promise<void> {
  return voidFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/refusal-acknowledge`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        refusal_id: refusalId,
        recovery,
      }),
    },
  )
}

// =====================================================================
// V3+v4 residuals closure repair / adjudication / graduation
// wire helpers consumed by the new state-inspector tabs.
// =====================================================================

/**
 * GET /api/chat/session/:id/repair/pending — list every pending
 * `RepairProposal` produced by the planner's repair-strategy pipeline.
 * Returns an empty array on 404 or empty substrate.
 */
export async function fetchRepairProposals(
  sessionId: string,
): Promise<RepairProposal[]> {
  const result = await jsonFetchOrNull<RepairProposal[]>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/repair/pending`,
  )
  return result ?? []
}

/**
 * POST /api/chat/session/:id/repair/:proposal_id/accept — record SME
 * acceptance. The server validates `credentials` against the proposal's
 * `required_credentials`; mismatched chains return 403.
 */
export async function acceptRepair(
  sessionId: string,
  proposalId: string,
  credentials: string[] = [],
  rationale?: string,
  opts?: { signal?: AbortSignal },
): Promise<void> {
  // Uses voidFetch so X-Share-Token header injection happens for
  // read-only viewers and an AbortSignal can be threaded through from
  // the caller. The body shape is unchanged.
  return voidFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/repair/${encodeURIComponent(
      proposalId,
    )}/accept`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ credentials, rationale }),
      signal: opts?.signal,
    },
  )
}

/**
 * POST /api/chat/session/:id/repair/:proposal_id/reject — record SME
 * rejection. Reason must be non-empty (server returns 400 otherwise).
 */
export async function rejectRepair(
  sessionId: string,
  proposalId: string,
  reason: string,
  opts?: { signal?: AbortSignal },
): Promise<void> {
  // Uses voidFetch so X-Share-Token header injection happens for
  // read-only viewers and an AbortSignal can be threaded through.
  return voidFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/repair/${encodeURIComponent(
      proposalId,
    )}/reject`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ reason }),
      signal: opts?.signal,
    },
  )
}

/**
 * GET /api/chat/session/:id/adjudication — list lifecycle-adjudication
 * queue entries. Empty array on 404.
 */
export async function fetchAdjudicationQueue(
  sessionId: string,
): Promise<AdjudicationQueueEntry[]> {
  const result = await jsonFetchOrNull<AdjudicationQueueEntry[]>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/adjudication`,
  )
  return result ?? []
}

/**
 * POST /api/chat/session/:id/adjudication/:entry_id/resolve — record
 * a resolution for a queue entry. Returns 204 on success.
 */
export async function resolveAdjudication(
  sessionId: string,
  entryId: string,
  decidedBy: string,
  decision: string,
): Promise<void> {
  return voidFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/adjudication/${encodeURIComponent(
      entryId,
    )}/resolve`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ decided_by: decidedBy, decision }),
    },
  )
}

/** Wire shape for `GET.../graduation/candidates`. */
export interface GraduationCandidatesPayload {
  thresholds: {
    min_usage_count: number
    min_unique_sessions: number
    min_success_rate: number
  }
  candidates: Array<{
    iri: string
    label: string
    usage_count: number
    unique_sessions: number
    success_rate: number
    graduation_target_ontology: string
  }>
}

/**
 * GET /api/chat/session/:id/graduation/candidates — list every
 * LocalExtension that has crossed graduation thresholds. Returns a
 * zero-candidate payload when 404 or no extensions qualify.
 */
export async function fetchGraduationCandidates(
  sessionId: string,
): Promise<GraduationCandidatesPayload> {
  const result = await jsonFetchOrNull<GraduationCandidatesPayload>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/graduation/candidates`,
  )
  return (
    result ?? {
      thresholds: {
        min_usage_count: 0,
        min_unique_sessions: 0,
        min_success_rate: 0,
      },
      candidates: [],
    }
  )
}

/**
 * POST /api/chat/session/:id/graduation/:iri/annotate — record an
 * upstream-submission annotation against a graduation candidate.
 */
export async function annotateGraduationCandidate(
  sessionId: string,
  iri: string,
  annotatedBy: string,
  submissionRef: string,
  rationale: string,
): Promise<void> {
  return voidFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/graduation/${encodeURIComponent(
      iri,
    )}/annotate`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        annotated_by: annotatedBy,
        submission_ref: submissionRef,
        rationale,
      }),
    },
  )
}

// `TierResult` + `ValueAddMetricsResponse` re-exported from the
// top-of-file ts-rs barrel so callers can keep importing them from
// `'../api/chatClient'`.

/**
 * GET /api/chat/session/:id/value-add-metrics
 *
 * Returns the latest per-tier value-add scorecard aggregated from
 * `runtime/value-add-metrics.jsonl`. Returns null when the session has
 * not emitted yet or the file does not exist (the server returns 200 +
 * null body in those cases).
 */
export async function getValueAddMetrics(
  sessionId: string,
): Promise<ValueAddMetricsResponse | null> {
  return jsonFetchOrNull(
    `/api/chat/session/${encodeURIComponent(sessionId)}/value-add-metrics`,
  )
}

/**
 * Proposal-lifecycle REST surface. The 4 methods below
 * shadow the routes mounted at
 * `crates/server/src/chat_routes/proposal.rs`. SSE updates flow via
 * the matching `proposal_*` event variants on `ChatSseEvent`; the
 * REST surface is used for initial load + button clicks.
 */

/**
 * GET /api/chat/session/:id/proposals — all proposals on the session,
 * sorted by `created_at` ascending. Server returns an empty array for
 * a session with no proposals, and 404 only for an unknown session id.
 */
export async function getProposals(
  sessionId: string,
): Promise<HypothesizedProposal[]> {
  const result = await jsonFetchOrNull<HypothesizedProposal[]>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/proposals`,
  )
  return result ?? []
}

/**
 * GET /api/chat/session/:id/proposal/:proposal_id — one full proposal.
 * Returns null when the proposal id is unknown so the caller can show
 * an empty state instead of throwing.
 */
export async function getProposal(
  sessionId: string,
  proposalId: string,
): Promise<HypothesizedProposal | null> {
  return jsonFetchOrNull<HypothesizedProposal>(
    `/api/chat/session/${encodeURIComponent(sessionId)}/proposal/${encodeURIComponent(
      proposalId,
    )}`,
  )
}

/**
 * POST /api/chat/session/:id/proposal/:proposal_id/signoff — SME
 * approve. Server materializes a TaskNode and splices into the DAG;
 * the UI receives a `proposal_promoted` SSE event when the broadcast
 * lands. Throws on non-204 (e.g. 409 when the proposal isn't in
 * `awaiting_signoff`).
 */
export async function signoffProposal(
  sessionId: string,
  proposalId: string,
  smeInitials?: string,
  opts?: { signal?: AbortSignal },
): Promise<void> {
  // Uses voidFetch so X-Share-Token header injection happens for
  // read-only viewers and an AbortSignal can be threaded through.
  // The body shape (with `sme_initials` defaulting to null) is unchanged.
  return voidFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/proposal/${encodeURIComponent(
      proposalId,
    )}/signoff`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ sme_initials: smeInitials ?? null }),
      signal: opts?.signal,
    },
  )
}

/**
 * POST /api/chat/session/:id/proposal/:proposal_id/reject — SME
 * reject. Throws on non-204 (409 when the proposal is already
 * terminal).
 */
export async function rejectProposal(
  sessionId: string,
  proposalId: string,
  rationale?: string,
  opts?: { signal?: AbortSignal },
): Promise<void> {
  // Uses voidFetch so X-Share-Token header injection happens for
  // read-only viewers and an AbortSignal can be threaded through.
  return voidFetch(
    `/api/chat/session/${encodeURIComponent(sessionId)}/proposal/${encodeURIComponent(
      proposalId,
    )}/reject`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ rationale: rationale ?? null }),
      signal: opts?.signal,
    },
  )
}
