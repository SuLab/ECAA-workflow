import { useEffect, useRef, useState } from 'react'
import { connectChatStream, type ChatSseEvent } from '../api/chatStream'
import { getHarnessEventsBacklog, type ArtifactRef } from '../api/chatClient'
import type { SessionState, Turn } from '../types'
import { notifyBlocker } from '../lib/notifications'

export interface ToolCallPill {
  toolName: string
  statusLine: string
}

export interface InfraError {
  reason: string
  userCopy: string
}

export interface HarnessProgressLine {
  /// Stable client-side id stamped at SSE-receive time. Survives the
  /// drop-oldest buffer trim so React keys don't shift across renders.
  id: string
  kind: string
  taskId: string
  status: string
  detail: string
  /// When the harness ran the task on a remote backend, the SSE event
  /// carries the backend + instance id + instance type so the Jobs tab
  /// can render a backend badge and sizing chip.
  remote?: {
    backend: string
    instanceId: string
    instanceType: string
  } | null
}

export interface StallSignalEvent {
  taskId: string
  signal: unknown
  suggestedAction: 'resize' | 'retry' | 'abort'
}

export interface PilotLifecycle {
  status: 'started' | 'complete' | 'skipped' | null
  report: unknown | null
  skipReason: string | null
}

/// Cap on the client-side harness-progress event buffer. A session
/// running a long AWS job can easily accumulate thousands of events;
/// rendering them all in JobsFeed would stutter scroll. Drop-oldest
/// past the cap, and surface the dropped count in the Metrics tab
/// (paired with the server-side `batch_dropped_events`).
const MAX_HARNESS_PROGRESS_EVENTS = 500

interface UseSseChatEvents {
  toolCallPill: ToolCallPill | null
  infraError: InfraError | null
  harnessProgress: HarnessProgressLine[]
  /// Count of harness-progress events the client dropped (oldest-first)
  /// because the buffer hit MAX_HARNESS_PROGRESS_EVENTS, *plus* the
  /// drop counts reported by the server via `resync_required`. Metrics
  /// tab surfaces this as a warning.
  harnessProgressDropped: number
  /// Task ids the harness reported as reviewable (completed + artifacts
  /// attached). ResultReviewTurnCard uses this to mark a card
  /// reviewable without polling.
  reviewableTasks: Set<string>
  /// Latest artifact list per reviewable task, keyed by task id.
  /// Populated from `task_completed_reviewable` events.
  reviewableArtifacts: Record<string, ArtifactRef[]>
  /// Pilot lifecycle (started/complete/skipped) + the latest PilotReport
  /// JSON when complete. Metrics tab reads this to render the Pilot row.
  pilot: PilotLifecycle
  /// Latest stall signals, keyed by task id (latest signal per task
  /// wins). Jobs tab reads this to render stall chips.
  stallSignals: Record<string, StallSignalEvent>
  /// Latest cross-version concordance report for the current session,
  /// or null when none has been produced. History tab reads this to
  /// render the per-hop timeline.
  crossVersionReport: unknown | null
  /// Backend info from the harness's startup diagnostic event. Null
  /// until the first `executor_selected` event lands. Progress tab
  /// renders a header row when present.
  executorInfo: ExecutorInfo | null
  /// Rolling POST-health snapshot from the harness. Null until the
  /// first `progress_client_health` event lands.
  progressHealth: ProgressHealth | null
  /// Most recent verified orphan-reap sweep. Null until the first
  /// sweep completes.
  orphanReap: OrphanReap | null
  /// Heartbeat-stall advisories, keyed by task id (latest per task
  /// wins). Progress tab renders a chip.
  heartbeatStalls: Record<string, HeartbeatStall>
  /// Live state for hypothesized-node proposals keyed
  /// by `proposal_id`. Populated by the 4 `proposal_*` SSE events.
  /// HypothesizedProposalCard reads this to drive the gate chips and
  /// terminal-state collapse.
  proposalEvents: Record<string, ProposalEventState>
  clearInfraError: () => void
}

/// Options accepted by `useSseChatEvents`. Exported so callers can
/// build the options object out-of-line (e.g. when threading a context
/// value such as `appendStreamingText` from `<StreamingTextProvider>`).
export interface UseSseChatEventsOptions {
  /// Fired on every `state_advanced` SSE event. The consumer applies
  /// `newState` to local state immediately (the SSE payload already
  /// carries the full server-authoritative `SessionState`, so the
  /// BlockerCard / NeedsInputChip / etc. can re-render without
  /// waiting on a refetch). `newState` is undefined when the SSE
  /// payload omitted it (forward-compatibility) and the consumer
  /// should fall back to refetching. Called for EVERY transition,
  /// including `Blocked → Blocked` (in-place blocker queue append)
  /// where the discriminant doesn't change but `state.blockers`
  /// grows — without forwarding the payload, two consecutive
  /// `task_blocked` events for different tasks would race the
  /// refetch and the second BlockerCard could fail to render.
  onStateAdvanced?: (newState?: SessionState) => void | Promise<void>
  /// Fired when the server emits a `resync_required` event (broadcast
  /// channel dropped events on this subscriber). The consumer should
  /// refetch state + transcript + DAG in parallel to re-converge.
  /// The callback receives the server-reported drop count for
  /// telemetry.
  onResyncRequired?: (dropped: number) => void | Promise<void>
  /// Fired when the server commits a new Turn. UIs append it locally
  /// instead of waiting for the 60s reconciliation poll. Idempotent
  /// on turn_id; the consumer drops duplicates.
  onTurnAppended?: (turn: Turn) => void
  /// Called for each `assistant_token_delta` SSE event. The consumer
  /// owns the buffer (e.g. `<StreamingTextProvider>` rAF-coalesces
  /// chunks into a context-scoped state slice so only the in-flight
  /// bubble re-renders).
  appendStreamingText?: (chunk: string) => void
  /// Reset the streaming buffer once the final non-streaming Turn has
  /// been appended to the transcript so the in-flight bubble disappears.
  /// Symmetric with `appendStreamingText`; the consumer owns the state.
  resetStreamingText?: () => void
}

export interface ExecutorInfo {
  name: string
  cpuBudget: number
  gpuBudget: number
  instanceType: string | null
  harnessVersion: string
  envMode: string
}

export interface ProgressHealth {
  totalPosts: number
  failedPosts: number
  totalAttempts: number
  lastError: string
  lastSuccessAt: string
}

export interface OrphanReap {
  candidateCount: number
  verifiedCount: number
  unverifiedIds: string[]
  policy: string
}

export interface HeartbeatStall {
  taskId: string
  ageSecs: number
}

/**
 * Per-proposal live state derived from the SSE event
 * stream. Keyed by `proposal_id` in `proposalEvents`. The card reads
 * `latestGate` to drive the chip strip; `terminal` flips on
 * promoted/rejected so the card can collapse.
 */
export interface ProposalEventState {
  proposalId: string
  nodeId: string
  /** Per-gate outcome: undefined = not yet fired; true = pass; false = fail. */
  validator: boolean | undefined
  sandbox: boolean | undefined
  /** `promoted | rejected` once terminal; null while in-flight. */
  terminal: 'promoted' | 'rejected' | null
  /** Set when the proposal materialized into a TaskNode. */
  promotedTaskNodeId: string | null
  /** SME rationale captured on reject, if any. */
  rejectRationale: string | null
}

export function useSseChatEvents(
  sessionId: string | null,
  opts?: UseSseChatEventsOptions,
): UseSseChatEvents {
  const [toolCallPill, setToolCallPill] = useState<ToolCallPill | null>(null)
  const [infraError, setInfraError] = useState<InfraError | null>(null)
  const [harnessProgress, setHarnessProgress] = useState<HarnessProgressLine[]>([])
  const [harnessProgressDropped, setHarnessProgressDropped] = useState<number>(0)
  const [reviewableTasks, setReviewableTasks] = useState<Set<string>>(() => new Set())
  const [reviewableArtifacts, setReviewableArtifacts] = useState<
    Record<string, ArtifactRef[]>
  >({})
  const [pilot, setPilot] = useState<PilotLifecycle>({
    status: null,
    report: null,
    skipReason: null,
  })
  const [stallSignals, setStallSignals] = useState<Record<string, StallSignalEvent>>({})
  const [crossVersionReport, setCrossVersionReport] = useState<unknown | null>(null)
  const [executorInfo, setExecutorInfo] = useState<ExecutorInfo | null>(null)
  const [progressHealth, setProgressHealth] = useState<ProgressHealth | null>(null)
  const [orphanReap, setOrphanReap] = useState<OrphanReap | null>(null)
  const [heartbeatStalls, setHeartbeatStalls] = useState<Record<string, HeartbeatStall>>({})
  const [proposalEvents, setProposalEvents] = useState<Record<string, ProposalEventState>>(
    {},
  )

  // `opts` is captured in a ref so the SSE subscription effect's dep
  // array can stay `[sessionId]`. Without this, every parent re-render
  // that produces a fresh `opts` object identity (the common React
  // pattern — defining callbacks inline) tore down and re-opened the
  // EventSource, which a) lost a turn of buffered SSE events on every
  // reconnect, and b) double-counted any event in flight at teardown.
  // Capturing in a ref lets callback identity churn without churning the
  // subscription; the handlers read `optsRef.current` at fire time so
  // the LATEST callback is always invoked.
  const optsRef = useRef(opts)
  optsRef.current = opts

  useEffect(() => {
    if (!sessionId) return
    let cancelled = false

    // Hydrate the harness-progress backlog from the server before the
    // SSE stream picks up new events. Without this, a page reload
    // wipes every prior event out of the Progress tab even though the
    // server has them persisted in session.harness_events. Fetch once
    // per sessionId change; the live SSE merge below de-dupes any
    // overlap between the backlog and the first SSE events that land
    // after the hydrate.
    void (async () => {
      try {
        const body = await getHarnessEventsBacklog(sessionId)
        if (cancelled) return
        const lines: HarnessProgressLine[] = body.events.map((e) => {
          // `e.remote` is now typed as
          // `RemoteExecutionWire | null` upstream (chatClient.ts), so
          // the prior `as any` cast is unnecessary. Field-name
          // bridging (`instance_id` → `instanceId`) stays inline since
          // the wire shape uses snake_case while the UI normalizes to
          // camelCase.
          const r = e.remote
          return {
            id: crypto.randomUUID(),
            kind: e.kind,
            taskId: e.taskId,
            status: e.status,
            detail: e.detail,
            remote:
              r != null
                ? {
                    backend: r.backend,
                    instanceId: r.instance_id,
                    instanceType: r.instance_type,
                  }
                : null,
          }
        })
        setHarnessProgress((prev) => {
          // SSE may have already pushed a few events before the fetch
          // resolved; merge by a stable dedup key so the same event
          // isn't rendered twice on reconnect.
          const seen = new Set<string>()
          const keyOf = (l: HarnessProgressLine) =>
            `${l.kind}|${l.taskId}|${l.status}|${l.detail}`
          const out: HarnessProgressLine[] = []
          for (const l of lines) {
            const k = keyOf(l)
            if (seen.has(k)) continue
            seen.add(k)
            out.push(l)
          }
          for (const l of prev) {
            const k = keyOf(l)
            if (seen.has(k)) continue
            seen.add(k)
            out.push(l)
          }
          // Preserve the drop-oldest cap established by the SSE path.
          if (out.length > MAX_HARNESS_PROGRESS_EVENTS) {
            const excess = out.length - MAX_HARNESS_PROGRESS_EVENTS
            setHarnessProgressDropped((d) => d + excess)
            return out.slice(excess)
          }
          return out
        })
      } catch {
        // Non-fatal: the SSE stream will still deliver live events;
        // the backlog just won't pre-populate.
      }
    })()

    // SSE handler registry. Each entry receives the narrowed event and
    // the shared setters. Mapped over `ChatSseEvent['type']` so tsc
    // fails when a Rust-side SsePayload variant is added without a
    // matching handler here.
    type Setters = {
      setToolCallPill: typeof setToolCallPill
      setInfraError: typeof setInfraError
      setHarnessProgress: typeof setHarnessProgress
      setHarnessProgressDropped: typeof setHarnessProgressDropped
      setReviewableTasks: typeof setReviewableTasks
      setReviewableArtifacts: typeof setReviewableArtifacts
      setPilot: typeof setPilot
      setStallSignals: typeof setStallSignals
      setCrossVersionReport: typeof setCrossVersionReport
      setExecutorInfo: typeof setExecutorInfo
      setProgressHealth: typeof setProgressHealth
      setOrphanReap: typeof setOrphanReap
      setHeartbeatStalls: typeof setHeartbeatStalls
      setProposalEvents: typeof setProposalEvents
      onStateAdvanced?: (newState?: SessionState) => void | Promise<void>
      onResyncRequired?: (dropped: number) => void | Promise<void>
      onTurnAppended?: (turn: Turn) => void
      appendStreamingText?: (chunk: string) => void
    }
    type Handler<K extends ChatSseEvent['type']> = (
      event: Extract<ChatSseEvent, { type: K }>,
      setters: Setters,
    ) => void
    type HandlerMap = { [K in ChatSseEvent['type']]: Handler<K> }

    const HANDLERS: HandlerMap = {
      tool_call_started: (e, s) =>
        s.setToolCallPill({ toolName: e.tool_name, statusLine: e.status_line }),
      tool_call_finished: (e, s) =>
        s.setToolCallPill((current) =>
          current?.toolName === e.tool_name ? null : current,
        ),
      state_advanced: (e, s) => {
        // Forward the SSE payload's authoritative new_state to the
        // consumer so it applies the state immediately AND triggers a
        // refetch for derived data (DAG, etc.). The discriminant
        // alone can match the prior state (Blocked → Blocked when a
        // second task_blocked appends to the existing blocker queue),
        // so consumers that only refetch race the next broadcast and
        // can miss the second BlockerCard. Forwarding the payload
        // gives the UI an unambiguous "here is the new state right
        // now" signal even when the refetch races.
        if (s.onStateAdvanced) void s.onStateAdvanced(e.new_state)
        // Browser notification: when the session transitions to
        // `blocked` and the tab isn't visible, ping the OS so the SME
        // knows they need to come back. Permission is opt-in — if the
        // SME hasn't granted it we fall back to title-bar blink
        // (handled inside notifyBlocker).
        if (
          e.new_state?.kind === 'blocked' &&
          typeof document !== 'undefined' &&
          document.visibilityState === 'hidden'
        ) {
          notifyBlocker({
            title: 'ECAA-workflow needs input',
            body:
              (e.new_state as { reason?: string }).reason ??
              'A task is waiting on your decision.',
          })
        }
      },
      infra_error: (e, s) => s.setInfraError({ reason: e.reason, userCopy: e.user_copy }),
      harness_progress: (e, s) => {
        s.setHarnessProgress((prev) => {
          const next: HarnessProgressLine = {
            id: crypto.randomUUID(),
            kind: e.kind,
            taskId: e.task_id,
            status: e.status,
            detail: e.detail,
            remote: e.remote
              ? {
                  backend: e.remote.backend,
                  instanceId: e.remote.instance_id,
                  instanceType: e.remote.instance_type,
                }
              : null,
          }
          // Cap the buffer at MAX_HARNESS_PROGRESS_EVENTS with
          // drop-oldest. Bump the client-side drop counter so the
          // Metrics tab can surface "silent drops" alongside the
          // server-side `batch_dropped_events`.
          if (prev.length >= MAX_HARNESS_PROGRESS_EVENTS) {
            const excess = prev.length - MAX_HARNESS_PROGRESS_EVENTS + 1
            s.setHarnessProgressDropped((d) => d + excess)
            return [...prev.slice(excess), next]
          }
          return [...prev, next]
        })
        // The server only broadcasts state_advanced on transitions
        // INTO Blocked. When a task transitions OUT of Blocked
        // (task_completed / task_failed after SME selection), the
        // session.dag updates server-side but no state_advanced
        // event fires, leaving the client's `blocked_tasks` and
        // `progress` snapshot stale. Refresh `/state` on every
        // terminal task event so the NeedsInputChip + Plan-tab
        // counters stay accurate.
        if (
          (e.kind === 'task_completed' ||
            e.kind === 'task_failed' ||
            e.kind === 'task_started') &&
          s.onStateAdvanced
        ) {
          void s.onStateAdvanced()
        }
      },
      assistant_token_delta: (e, s) => {
        // Forward the streamed chunk to the consumer-owned buffer
        // (typically `<StreamingTextProvider>`'s rAF-coalesced append),
        // so only components subscribed to that context re-render — not
        // the full `ConversationPane` tree.
        if (s.appendStreamingText) s.appendStreamingText(e.text)
      },
      task_completed_reviewable: (e, s) => {
        // Expose both the set of reviewable task ids and the artifact
        // list keyed by task id.
        s.setReviewableTasks((prev) => {
          if (prev.has(e.task_id)) return prev
          const next = new Set(prev)
          next.add(e.task_id)
          return next
        })
        s.setReviewableArtifacts((prev) => ({ ...prev, [e.task_id]: e.artifacts }))
      },
      harness_sizing_pilot_started: (_e, s) =>
        s.setPilot({ status: 'started', report: null, skipReason: null }),
      harness_sizing_pilot_complete: (e, s) =>
        s.setPilot({ status: 'complete', report: e.report, skipReason: null }),
      harness_sizing_pilot_skipped: (e, s) =>
        s.setPilot({ status: 'skipped', report: null, skipReason: e.reason }),
      harness_stall_detected: (e, s) =>
        s.setStallSignals((prev) => ({
          ...prev,
          [e.task_id]: {
            taskId: e.task_id,
            signal: e.signal,
            suggestedAction: e.suggested_action,
          },
        })),
      harness_resize_recommended: (e, s) =>
        // Surfaces as a progress line so the Jobs tab can render it
        // alongside other events.
        s.setHarnessProgress((prev) => [
          ...prev,
          {
            id: crypto.randomUUID(),
            kind: 'resize_recommended',
            taskId: e.task_id,
            status: 'advisory',
            detail: `resize ${e.from_instance_type} → ${e.to_instance_type}`,
          },
        ]),
      harness_version_diff: (e, s) => s.setCrossVersionReport(e.report),
      // No-op surface handler — the amended package is reflected
      // through state_advanced + transcript poll; we only need to
      // register the variant so the exhaustiveness check passes.
      package_amended: () => {},
      // Server dropped events on our subscriber. Bump the drop counter
      // and fire onResyncRequired so the consumer refetches state +
      // transcript + DAG in parallel.
      resync_required: (e, s) => {
        s.setHarnessProgressDropped((d) => d + e.dropped)
        if (s.onResyncRequired) void s.onResyncRequired(e.dropped)
      },
      dashboard_summary_failed: (e, s) =>
        s.setInfraError({
          reason: 'dashboard_summary_failed',
          userCopy: `Dashboard summary failed for task ${e.task_id}: ${e.reason}`,
        }),
      // Server committed a new Turn. Forward it to the consumer for
      // in-place append (idempotent on turn_id there).
      turn_appended: (e, s) => {
        if (s.onTurnAppended) s.onTurnAppended(e.turn)
      },
      // Harness startup diagnostic.
      harness_executor_selected: (e, s) =>
        s.setExecutorInfo({
          name: e.name,
          cpuBudget: e.cpu_budget,
          gpuBudget: e.gpu_budget,
          instanceType: e.instance_type,
          harnessVersion: e.harness_version,
          envMode: e.env_mode,
        }),
      // Rolling POST-health snapshot.
      harness_progress_health: (e, s) =>
        s.setProgressHealth({
          totalPosts: e.total_posts,
          failedPosts: e.failed_posts,
          totalAttempts: e.total_attempts,
          lastError: e.last_error,
          lastSuccessAt: e.last_success_at,
        }),
      // Verified AWS orphan-reap sweep.
      harness_orphans_reaped: (e, s) =>
        s.setOrphanReap({
          candidateCount: e.candidate_count,
          verifiedCount: e.verified_count,
          unverifiedIds: e.unverified_ids,
          policy: e.policy,
        }),
      // Per-task heartbeat stall chip.
      harness_heartbeat_stalled: (e, s) =>
        s.setHeartbeatStalls((prev) => ({
          ...prev,
          [e.task_id]: { taskId: e.task_id, ageSecs: e.age_secs },
        })),
      // Proposal lifecycle events. Seed an entry on
      // `proposal_received`; merge gate outcomes on
      // `proposal_gate_advanced`; flip `terminal` on promoted /
      // rejected so the card collapses.
      proposal_received: (e, s) =>
        s.setProposalEvents((prev) => {
          // Idempotent on `proposal_id`: a duplicate event for an
          // already-seen proposal (e.g. after `resync_required`)
          // preserves the existing gate state instead of resetting.
          if (prev[e.proposal_id]) return prev
          return {
            ...prev,
            [e.proposal_id]: {
              proposalId: e.proposal_id,
              nodeId: e.node_id,
              validator: undefined,
              sandbox: undefined,
              terminal: null,
              promotedTaskNodeId: null,
              rejectRationale: null,
            },
          }
        }),
      proposal_gate_advanced: (e, s) =>
        s.setProposalEvents((prev) => {
          const entry = prev[e.proposal_id]
          // Defensive: gate_advanced should always follow a
          // `proposal_received`, but on resync the latter may not
          // have arrived yet. Synthesize a minimal entry so the
          // chip still renders.
          const base = entry ?? {
            proposalId: e.proposal_id,
            nodeId: '',
            validator: undefined,
            sandbox: undefined,
            terminal: null,
            promotedTaskNodeId: null,
            rejectRationale: null,
          }
          const next = { ...base }
          if (e.gate === 'validator') next.validator = e.passed
          else if (e.gate === 'sandbox') next.sandbox = e.passed
          // The `sme_signoff` gate fires from the signoff endpoint
          // via ProposalPromoted; nothing to record here.
          return { ...prev, [e.proposal_id]: next }
        }),
      proposal_promoted: (e, s) =>
        s.setProposalEvents((prev) => {
          const base = prev[e.proposal_id]
          if (!base) {
            // Resync ordering: synthesize a terminal entry so the
            // SME sees the result.
            return {
              ...prev,
              [e.proposal_id]: {
                proposalId: e.proposal_id,
                nodeId: '',
                validator: undefined,
                sandbox: undefined,
                terminal: 'promoted',
                promotedTaskNodeId: e.task_node_id,
                rejectRationale: null,
              },
            }
          }
          return {
            ...prev,
            [e.proposal_id]: {
              ...base,
              terminal: 'promoted',
              promotedTaskNodeId: e.task_node_id,
            },
          }
        }),
      proposal_rejected: (e, s) =>
        s.setProposalEvents((prev) => {
          const base = prev[e.proposal_id]
          if (!base) {
            return {
              ...prev,
              [e.proposal_id]: {
                proposalId: e.proposal_id,
                nodeId: '',
                validator: undefined,
                sandbox: undefined,
                terminal: 'rejected',
                promotedTaskNodeId: null,
                rejectRationale: e.rationale,
              },
            }
          }
          return {
            ...prev,
            [e.proposal_id]: {
              ...base,
              terminal: 'rejected',
              rejectRationale: e.rationale,
            },
          }
        }),
    }

    // The callback fields read `optsRef.current.*` at dispatch time
    // (not subscription time), so a parent that re-renders with new
    // `onStateAdvanced` / `onResyncRequired` / `onTurnAppended`
    // identities still hits the LATEST callback without re-creating the
    // EventSource. The setter fields are stable from useState so they
    // bind freely here.
    const onEvent = (e: ChatSseEvent) => {
      const setters: Setters = {
        setToolCallPill,
        setInfraError,
        setHarnessProgress,
        setHarnessProgressDropped,
        setReviewableTasks,
        setReviewableArtifacts,
        setPilot,
        setStallSignals,
        setCrossVersionReport,
        setExecutorInfo,
        setProgressHealth,
        setOrphanReap,
        setHeartbeatStalls,
        setProposalEvents,
        onStateAdvanced: optsRef.current?.onStateAdvanced,
        onResyncRequired: optsRef.current?.onResyncRequired,
        onTurnAppended: optsRef.current?.onTurnAppended,
        appendStreamingText: optsRef.current?.appendStreamingText,
      }
      const handler = HANDLERS[e.type] as Handler<typeof e.type> | undefined
      if (handler) {
        handler(e, setters)
      } else {
        console.warn('[useSseChatEvents] no handler for SSE type', e.type, e)
      }
    }
    const disconnect = connectChatStream(sessionId, onEvent)
    return () => {
      cancelled = true
      disconnect()
    }
  }, [sessionId])

  return {
    toolCallPill,
    infraError,
    harnessProgress,
    harnessProgressDropped,
    reviewableTasks,
    reviewableArtifacts,
    pilot,
    stallSignals,
    crossVersionReport,
    executorInfo,
    progressHealth,
    orphanReap,
    heartbeatStalls,
    proposalEvents,
    clearInfraError: () => setInfraError(null),
  }
}
