import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  getLlmAvailability,
  getMethodLandscape,
  getProposals,
  getTaskResult,
  listDispositions,
  postBranch,
  postRerun,
  postSmeSelection,
  startSessionFromIntent,
  type DispositionListEntryWire,
  type LlmAvailability,
  type MethodLandscape,
  type TaskResultPayload,
  type WorkflowIntent,
} from '../api/chatClient'
import { useEventsContext, useSessionContext } from '../hooks/contexts'
import { useStreamingText } from '../state/StreamingTextContext'
import { useCancelableEffect } from '../hooks/useCancelableFetch'
import type { HypothesizedProposal } from '../types/HypothesizedProposal'
import BlockerCard from './BlockerCard'
import ChatComposer from './ChatComposer'
import ChatTimeline from './ChatTimeline'
import DispositionReviewCard from './DispositionReviewCard'
import CompositionOutcomeBanner from './CompositionOutcomeBanner'
import EnsembleReviewTurnCard from './EnsembleReviewTurnCard'
import HypothesizedProposalCard from './HypothesizedProposalCard'
import InfraErrorBanner from './InfraErrorBanner'
import {
  MethodOptionsCard,
  axisFromStageId,
  mapLandscapeToOptions,
  type MethodOption,
} from './MethodOptionsCard'
import PackageSafetyBanner from './PackageSafetyBanner'
import PendingInputHintsCard from './PendingInputHintsCard'
import ResultReviewTurnCard, {
  type ResultReviewPayload,
  type ResultStatus,
} from './ResultReviewTurnCard'
import SensitivityComparisonCard from './SensitivityComparisonCard'
import StartExecutionPanel from './StartExecutionPanel'
import StructuredIntakeForm from './StructuredIntakeForm'
import StuckTasksBanner from './StuckTasksBanner'
import StillThinkingIndicator from './StillThinkingIndicator'
import type { SafetyLevel } from '../types/SafetyLevel'
import type { SafetySummary } from '../types/SafetySummary'
import { CardContainer } from './primitives/CardContainer'

// Feature flags for the three orphan cards being mounted (R1.8). All
// default to `true`; QA can tighten the trigger conditions later and
// flip these per surface. Keeping them as module-level constants instead
// of env-driven toggles so the prod bundle has zero runtime branching
// cost — Vite tree-shakes the `false` branch.
const ENABLE_BRANCH_FROM_HERE_CARD = true
const ENABLE_RESULT_REVIEW_CARD = true
const ENABLE_SENSITIVITY_COMPARISON_CARD = true
// Literature-grounded method-discovery picker. Triggers on the same
// AwaitingSmeSelection blocker as the sensitivity card, but only for
// `discover_*` stage ids — those route to MethodOptionsCard instead.
const ENABLE_METHOD_OPTIONS_CARD = true
// In-chat ensemble summary card. Self-suppresses (returns null) when no
// ensemble ran for this session, so leaving this unconditionally on
// post-emit is cheap.
const ENABLE_ENSEMBLE_REVIEW_CARD = true

interface ConversationPaneProps {
  /**
   * Switches `StateInspectorPane`'s controlled tab to `'ensemble'` (the
   * Robustness tab) when the SME clicks the ensemble card's "View
   * robustness details" button. Optional — tests that mount
   * `ConversationPane` standalone omit it, which simply suppresses the
   * ensemble card (it has nowhere useful to send the click).
   */
  onOpenRobustnessTab?: () => void
}

export default function ConversationPane({
  onOpenRobustnessTab,
}: ConversationPaneProps = {}) {
  const conv = useSessionContext()
  const sse = useEventsContext()
  // Streaming text + reset live in `<StreamingTextProvider>` instead of
  // `useSseChatEvents` so the rAF-coalesced delta commits re-render
  // only `ConversationPane` (and the memoized `ChatTimeline` underneath
  // when the text prop actually shifts) — not the full `App.tsx`
  // sibling tree (`StateInspectorPane`, etc.).
  const streaming = useStreamingText()

  // v3 P10 — three-state LLM availability. Polled once at mount; when
  // anything other than `available`, the MVP structured-intake form
  // replaces the conversational composer. Defaults to `available` so
  // the chat surface still renders if the endpoint is slow or fails.
  const [availability, setAvailability] = useState<LlmAvailability>({
    kind: 'available',
  })
  useCancelableEffect(async ({ cancelled }) => {
    try {
      const av = await getLlmAvailability()
      if (!cancelled()) setAvailability(av)
    } catch {
      // Non-fatal — assume `Available` and let the live backend
      // surface a typed error on the first turn if it actually is
      // down. The form fallback is for the policy-driven case.
    }
  }, [])

  const handleStructuredSubmit = useCallback(
    async (intent: WorkflowIntent) => {
      const session = await startSessionFromIntent(intent)
      // Attach the page-level session context to the deterministic
      // session the fallback just created. Without the query param,
      // useConversation would create a fresh blank session on reload.
      if (typeof window !== 'undefined') {
        const url = new URL(window.location.href)
        url.searchParams.set('session', session.session_id)
        window.location.assign(url.toString())
      }
    },
    [],
  )

  // When a turn finishes (sending → false) drop the in-flight streaming
  // buffer so the canonical AssistantTurnCard takes over from the
  // InFlightAssistantBubble.
  const sendingRef = useRef(conv.sending)
  useEffect(() => {
    if (sendingRef.current && !conv.sending) {
      streaming.reset()
    }
    sendingRef.current = conv.sending
  }, [conv.sending, streaming])

  // Pull pending + recently-applied dispositions so the card can
  // render inline. Poll on session change + after each turn (the
  // transcript-poll interval is too slow to catch an agent-emitted
  // disposition mid-turn).
  const [dispositions, setDispositions] = useState<DispositionListEntryWire[]>(
    [],
  )
  const loadDispositions = useCallback(async () => {
    if (!conv.sessionId) return
    try {
      const res = await listDispositions(conv.sessionId)
      setDispositions(res.dispositions)
    } catch {
      // Non-fatal; the next poll will retry.
    }
  }, [conv.sessionId])

  useEffect(() => {
    loadDispositions()
  }, [loadDispositions, conv.state?.state.kind, conv.turns.length])

  // Pull the session's hypothesized-node proposals so the
  // card list can render. Re-fetch when a new proposal lands via SSE
  // (`proposalEvents` keys grow) or when the session pivots to a new
  // id. Live chip / terminal state still flows through the SSE
  // overlay; the REST snapshot supplies the authoritative
  // intent / rationale / gate_outcomes shape the card decorates.
  const [proposals, setProposals] = useState<HypothesizedProposal[]>([])
  const proposalEventKeys = useMemo(
    () => Object.keys(sse.proposalEvents).sort().join(','),
    [sse.proposalEvents],
  )
  const loadProposals = useCallback(async () => {
    if (!conv.sessionId) return
    try {
      const list = await getProposals(conv.sessionId)
      setProposals(list)
    } catch {
      // Non-fatal; the next SSE event will retrigger the fetch.
    }
  }, [conv.sessionId])
  useEffect(() => {
    loadProposals()
    // Also refetch on every committed turn (`conv.turns.length`) — the
    // SSE proposal_received event fires from inside dispatch BEFORE
    // `send_turn` merges the tool-loop mutations into the SessionStore,
    // so a fetch triggered by proposalEventKeys alone can race ahead
    // of the persistence merge and silently see an empty proposals
    // list. The committed-turn trigger guarantees a refetch lands
    // AFTER the merge. Mirrors the dispositions loader pattern above.
  }, [loadProposals, proposalEventKeys, conv.turns.length])
  // Filter to non-rejected proposals so the card list stays scoped to
  // actionable + recently-promoted entries. Rejected entries collapse
  // out of the chat scroll immediately; promoted entries collapse to
  // a one-line confirmation rendered by the card itself.
  const visibleProposals = useMemo(
    () => proposals.filter((p) => p.lifecycle.kind !== 'rejected'),
    [proposals],
  )

  // Derive the package safety summary
  // client-side from each task's SafetyPolicy. Server-side
  // `safety_summary` on session state can replace this once wired; the
  // shapes are identical so the banner doesn't need to change. We only
  // emit the banner once a package has been emitted (dag is populated).
  const safetySummary = useMemo<SafetySummary | null>(() => {
    const tasks = conv.dag?.tasks
    if (!tasks) return null
    const taskEntries = Object.values(tasks).filter(
      (t): t is NonNullable<typeof t> => !!t,
    )
    if (taskEntries.length === 0) return null
    const counts = { safe: 0, network: 0, compute: 0, exec: 0 }
    let worst: SafetyLevel = 'safe'
    const rank: Record<SafetyLevel, number> = {
      safe: 0,
      network: 1,
      compute: 2,
      exec: 3,
    }
    let seenAny = false
    for (const t of taskEntries) {
      // task.safety may be absent on pre-A.S6 emitted packages; skip
      // gracefully so older sessions don't crash the banner.
      const level = t.safety?.level
      if (!level) continue
      seenAny = true
      counts[level] += 1
      if (rank[level] > rank[worst]) worst = level
    }
    if (!seenAny) return null
    return { worst_case_level: worst, level_counts: counts }
  }, [conv.dag])

  const handleFilterByLevel = useCallback((_level: SafetyLevel) => {
    // The filter is consumed by the Audit tab. Without a global
    // filter-bus yet, this is a no-op stub so the banner can render
    // and respond to clicks. Wiring to the StateInspectorPane filter
    // is left for the audit-tab follow-up.
  }, [])

  const blockedState = conv.state?.state.kind === 'blocked' ? conv.state.state : null
  const blockers =
    blockedState != null
      ? (blockedState.blockers && blockedState.blockers.length > 0
          ? blockedState.blockers.map((entry) => ({
              key: entry.blocker_id,
              reason: entry.message,
              recoveryHint: entry.recovery_hint ?? blockedState.recovery_hint,
              blockerKind: entry.kind,
              taskId: entry.task_id === '_session' ? null : entry.task_id,
            }))
          : [
              {
                key: 'legacy-blocker',
                reason: blockedState.reason,
                recoveryHint: blockedState.recovery_hint,
                blockerKind: blockedState.blocker_kind ?? null,
                taskId: null,
              },
            ])
      : []

  // Pending disposition upstream of the current blocker? Surfaces the
  // §6.1 "related disposition" hint on BlockerCard so the SME knows
  // the remediation is upstream. First pending wins (disposition order
  // is usually newest → oldest anyway).
  const pendingDisposition = dispositions.find(
    (d) => d.status === 'pending' || d.status === 'partial',
  )

  // Stable reference for the memo'd AssistantTurnCard — inline arrows
  // here would defeat React.memo on every parent render.
  const onQuickReply = useCallback(
    (opt: string) => conv.sendTurn(opt),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- dep on .sendTurn method ref is intentional; full conv object would re-run on every render
    [conv.sendTurn],
  )

  // Drop a retained optional stage the confirmation card surfaced.
  // Routes through the same conversation loop quick-replies use: a
  // deterministic natural-language instruction the LLM resolves into a
  // `set_intake_excluded_atoms` call, which prunes the stage and rebuilds
  // the DAG. (No dedicated REST endpoint exists for exclusion today; it
  // is a conversational tool, so this is the established mechanism — see
  // crates/conversation/src/tools/intake.rs::set_intake_excluded_atoms.)
  const onRemoveOptionalStage = useCallback(
    (stageId: string) =>
      conv.sendTurn(
        `Drop the optional stage "${stageId}" from the plan — exclude it before we emit.`,
      ),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- dep on .sendTurn method ref is intentional; full conv object would re-run on every render
    [conv.sendTurn],
  )

  // Whether the session has produced at least one emitted package.
  // Today's gating: only offer the "Branch from here" affordance once
  // the SME has something concrete to fork — pre-emission sessions
  // have nothing meaningful behind them.
  // `blocked` is intentionally excluded: the server-side `branch_session`
  // tool refuses from Blocked state, so showing the affordance there
  // would always result in a server error (see crates/conversation/src/tools/branch.rs).
  const stateKind = conv.state?.state.kind ?? null
  const hasEmittedAtLeastOnce =
    stateKind === 'emitted' ||
    stateKind === 'amending' ||
    !!conv.state?.parent_session_id

  // Non-blocking warning surfaced when a session-scoped branch reports
  // inherited artifacts it couldn't carry into the child. Mirrors the
  // TaskDetailDrawer's warning; the inline card navigates immediately so
  // we surface the note as a dismissible banner that persists across the
  // SPA session swap.
  const [branchWarning, setBranchWarning] = useState<string | null>(null)

  // Branch handler: dispatch to /branch and route the browser to the
  // child session. Mirrors the TaskDetailDrawer's submit-branch flow
  // but without the modal — the BranchFromHereCard owns the rationale
  // capture inline.
  const onBranch = useCallback(
    async (rationale?: string) => {
      if (!conv.sessionId) return
      try {
        const body = await postBranch(conv.sessionId, { rationale: rationale || undefined })
        const childId = body.session_id
        // The child ALREADY exists in the session tree; if some inherited
        // artifacts couldn't be copied, surface a non-blocking warning
        // (the missing results just re-run in the branch).
        const missing = body.artifacts_missing
        setBranchWarning(
          missing && missing.length > 0
            ? `Branch created. Some prior results couldn't be carried over and will re-run: ${missing.join(', ')}`
            : null,
        )
        // SPA navigation into the child session — pushState-syncs
        // `?session=` and swaps the transcript/state/dag in place so the
        // SME keeps the chat scroll instead of a full-page reload.
        await conv.switchToSession(childId)
      } catch (e) {
        // Surface the error on the conversation banner so the SME
        // sees it without having to open the console.
        // eslint-disable-next-line no-console
        console.error('Branch failed', e)
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- dep on .sessionId/.switchToSession method refs is intentional; full conv object would re-run on every render
    [conv.sessionId, conv.switchToSession],
  )

  // Result review: load full TaskResultPayload bodies for each task
  // the harness flagged as reviewable. The SSE event already supplied
  // the artifact list, but the card wants the typed result/reason/
  // record body which only the REST endpoint returns. Fetch lazily as
  // each reviewable task id surfaces and cache by task id.
  const [taskResults, setTaskResults] = useState<
    Record<string, TaskResultPayload>
  >({})
  const reviewableKeys = useMemo(
    () => Array.from(sse.reviewableTasks).sort().join(','),
    [sse.reviewableTasks],
  )
  useEffect(() => {
    if (!conv.sessionId) return
    if (!ENABLE_RESULT_REVIEW_CARD) return
    let cancelled = false
    void (async () => {
      for (const taskId of sse.reviewableTasks) {
        if (taskResults[taskId]) continue
        try {
          const payload = await getTaskResult(conv.sessionId!, taskId)
          if (cancelled) return
          setTaskResults((prev) => ({ ...prev, [taskId]: payload }))
        } catch {
          // Non-fatal — the next reviewable event for this task will
          // retrigger the fetch.
        }
      }
    })()
    return () => {
      cancelled = true
    }
  }, [conv.sessionId, reviewableKeys, sse.reviewableTasks, taskResults])

  // Rerun handler — call the deterministic `/rerun` REST endpoint
  // directly. The server-side handler is gated on session state, emits
  // the typed `RerunTask` DecisionRecord at mutation time, and
  // auto-relaunches the harness — no LLM round-trip required. The chat
  // `sendTurn` path is kept as a fallback for when the session id is
  // missing or the REST call fails (the LLM then routes the intent to
  // the `rerun_task` high-impact tool).
  const onRerunTask = useCallback(
    async (taskId: string, reason?: string) => {
      const trimmed = reason?.trim() || undefined
      if (conv.sessionId) {
        try {
          await postRerun(conv.sessionId, taskId, trimmed)
          return
        } catch {
          // Fall through to the LLM path on REST failure.
        }
      }
      const text = trimmed
        ? `Please rerun task ${taskId}. Reason: ${trimmed}`
        : `Please rerun task ${taskId}.`
      return conv.sendTurn(text)
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- dep on .sessionId/.sendTurn method refs is intentional; full conv object would re-run on every render
    [conv.sessionId, conv.sendTurn],
  )

  // Narrow the wire-level TaskResultStatus to the inline card's
  // 3-state `ResultStatus`. Statuses other than completed/failed/
  // blocked don't surface in the reviewable stream (the harness only
  // emits `task_completed_reviewable` once the task lands in one of
  // those terminal states), so the fallback path here is defensive.
  const narrowStatus = (s: TaskResultPayload['status']): ResultStatus | null => {
    if (s === 'completed' || s === 'failed' || s === 'blocked') return s
    return null
  }

  // Detect a sensitivity-selection blocker — the trigger condition for
  // the inline `SensitivityComparisonCard`. The blocker carries
  // `stage_id` + `candidates` and the BlockerCard already renders a
  // generic Unblock affordance; the SensitivityComparisonCard adds
  // the radio-group picker so the SME can pick a winner inline. On
  // submit we POST directly to the deterministic `/sme-selection`
  // endpoint, which emits the typed `SelectSensitivityWinner`
  // DecisionRecord server-side at mutation time and resumes the DAG.
  const sensitivityBlocker = useMemo(() => {
    if (!blockedState) return null
    // Prefer the first blocker entry whose kind is awaiting_sme_selection;
    // fall back to the legacy top-level blocker_kind.
    for (const entry of blockedState.blockers ?? []) {
      if (entry.kind?.kind === 'awaiting_sme_selection') {
        return entry.kind
      }
    }
    if (blockedState.blocker_kind?.kind === 'awaiting_sme_selection') {
      return blockedState.blocker_kind
    }
    return null
  }, [blockedState])

  // A `discover_*` selection blocker routes to the literature-grounded
  // MethodOptionsCard instead of the sensitivity picker. Both consume
  // the same AwaitingSmeSelection blocker, so split on the stage id to
  // keep them mutually exclusive (no double render).
  const isDiscoverBlocker =
    sensitivityBlocker?.stage_id?.startsWith('discover_') ?? false

  // The `survey_method_landscape` agent task emits a per-axis
  // method-landscape artifact (candidate methods + literature-anchored
  // evidence + deterministic support scores). Fetch it when a
  // `discover_*` selection blocker is active so the card can show real
  // evidence/scores. Null until it arrives (or if the survey hasn't
  // produced it) — the placeholder mapping below covers that case.
  const [methodLandscape, setMethodLandscape] =
    useState<MethodLandscape | null>(null)
  useCancelableEffect(
    async ({ signal, cancelled }) => {
      if (!ENABLE_METHOD_OPTIONS_CARD) return
      if (!conv.sessionId || !isDiscoverBlocker) {
        setMethodLandscape(null)
        return
      }
      try {
        const landscape = await getMethodLandscape(conv.sessionId, { signal })
        if (cancelled()) return
        setMethodLandscape(landscape)
      } catch {
        // Non-fatal — fall back to bare candidate names. A later blocker
        // refresh retriggers this fetch.
        if (!cancelled()) setMethodLandscape(null)
      }
    },
    [conv.sessionId, isDiscoverBlocker],
  )

  // Map the blocker's candidates into the MethodOptionsCard shape.
  // Preferred: the survey artifact, keyed by the `discover_`-stripped
  // axis, carrying real evidence + deterministic support scores.
  // Fallback (artifact missing/unfetchable or axis absent): the bare
  // candidate names the blocker carries, rank-ordered with a descending
  // placeholder score so rank-1 still reads highest. Never crashes.
  const methodOptions = useMemo<MethodOption[]>(() => {
    if (!sensitivityBlocker) return []
    if (isDiscoverBlocker) {
      const axis = axisFromStageId(sensitivityBlocker.stage_id)
      const fromLandscape = mapLandscapeToOptions(methodLandscape, axis)
      if (fromLandscape && fromLandscape.length > 0) return fromLandscape
    }
    return sensitivityBlocker.candidates.map((method, i) => ({
      method,
      // Server hands candidates back rank-ordered; surface a descending
      // placeholder score so rank-1 reads highest until real scores wire in.
      score: sensitivityBlocker.candidates.length - i,
      literatureEligible: false,
      evidence: [],
    }))
  }, [sensitivityBlocker, isDiscoverBlocker, methodLandscape])

  const onSelectDiscoverMethod = useCallback(
    async (method: string, rationale?: string) => {
      const stageId = sensitivityBlocker?.stage_id
      if (conv.sessionId && stageId) {
        try {
          await postSmeSelection(conv.sessionId, stageId, method)
          return
        } catch {
          // Fall through to the LLM path on REST failure.
        }
      }
      const text = rationale
        ? `Use ${method} for ${stageId}. Rationale: ${rationale}`
        : `Use ${method} for ${stageId}.`
      await conv.sendTurn(text)
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- dep on .sessionId/.sendTurn method refs is intentional; full conv object would re-run on every render
    [conv.sessionId, conv.sendTurn, sensitivityBlocker],
  )

  const onSelectSensitivityWinner = useCallback(
    async (winner: string, rationale?: string) => {
      // The AwaitingSmeSelection blocker exposes `stage_id` (NOT
      // `task_id`); the /sme-selection route is keyed by that id.
      const taskId = sensitivityBlocker?.stage_id
      if (conv.sessionId && taskId) {
        try {
          // Deterministic, server-gated: emits SelectSensitivityWinner
          // at mutation time + writes the sme-selection sidecars +
          // resumes the DAG + auto-relaunches the harness.
          await postSmeSelection(conv.sessionId, taskId, winner)
          return
        } catch {
          // Fall through to the LLM path on REST failure.
        }
      }
      const text = rationale
        ? `Selected ${winner} as the winner. Rationale: ${rationale}`
        : `Selected ${winner} as the winner.`
      await conv.sendTurn(text)
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- dep on .sessionId/.sendTurn method refs is intentional; full conv object would re-run on every render
    [conv.sessionId, conv.sendTurn, sensitivityBlocker],
  )

  // v3 P10 — when the LLM is `Disabled` or `Unavailable`, swap the
  // chat surface for the MVP structured-intake form. The chat
  // transcript would be empty anyway (no LLM to mediate); the form
  // routes the SME's intent through the deterministic compiler.
  // Exception: if the session is already Blocked (e.g. by claim
  // verification's ValidationFailed transition), render the BlockerCard
  // stack ABOVE the form so the SME can still act on the blocker. The
  // recovery affordances (Re-verify, Accept-and-continue, amend) do
  // not require the LLM — they are server-side POSTs the chat surface
  // only wraps. Without this branch, an offline / no-API-key operator
  // sees the MISMATCH badge in the Claims tab but has no in-pane
  // recovery handle for the blocker.
  if (availability.kind !== 'available') {
    const isDisabled = availability.kind === 'disabled'
    const reason =
      'reason' in availability ? availability.reason : 'unknown'
    return (
      <div
        style={{
          display: 'flex',
          flexDirection: 'column',
          height: '100%',
          padding: '1rem',
          gap: '0.85rem',
          background: 'var(--color-surface-1)',
          borderRight: '1px solid var(--color-border-default)',
          overflowY: 'auto',
        }}
      >
        <CardContainer
          palette={isDisabled ? 'warning' : 'danger'}
          role="status"
          ariaLive="polite"
          title={
            isDisabled
              ? 'Chat assistant disabled'
              : 'Chat assistant temporarily unavailable'
          }
          style={{
            padding: '0.6rem 0.85rem',
            color: isDisabled
              ? 'var(--color-warning-fg)'
              : 'var(--color-danger-fg)',
            fontSize: '0.85rem',
            borderLeft: `1px solid ${
              isDisabled
                ? 'var(--color-warning-border)'
                : 'var(--color-danger-border)'
            }`,
          }}
        >
          <div style={{ fontSize: '0.78rem', marginTop: '0.2rem' }}>
            {reason}. You can still start a workflow by filling out the form
            below.
          </div>
        </CardContainer>
        {blockers.length > 0 && (
          <div style={{ padding: '0.25rem 0' }}>
            {blockers.map((blocked) => (
              <BlockerCard
                key={blocked.key}
                reason={blocked.reason}
                recoveryHint={blocked.recoveryHint}
                onUnblock={conv.unblock}
                blockerKind={blocked.blockerKind ?? null}
                sessionId={conv.sessionId}
                taskId={blocked.taskId}
                relatedDispositionPath={pendingDisposition?.path ?? null}
                relatedDispositionTaskId={pendingDisposition?.task_id ?? null}
              />
            ))}
          </div>
        )}
        <StructuredIntakeForm onSubmit={handleStructuredSubmit} />
      </div>
    )
  }

  return (
    <div
      style={{
        display: 'flex',
        flexDirection: 'column',
        height: '100%',
        background: 'var(--color-surface-1)',
        borderRight: '1px solid var(--color-border-default)',
      }}
    >
      <StuckTasksBanner sessionId={conv.sessionId} />
      <CompositionOutcomeBanner
        sessionId={conv.sessionId}
        refreshKey={conv.state?.last_activity}
      />
      {safetySummary && (
        <PackageSafetyBanner
          summary={safetySummary}
          onFilterByLevel={handleFilterByLevel}
        />
      )}
      {sse.infraError && (
        <InfraErrorBanner
          error={sse.infraError}
          onDismiss={sse.clearInfraError}
          sessionId={conv.sessionId}
        />
      )}
      {conv.staleSources.size > 0 && (
        <CardContainer
          palette="warning"
          role="status"
          ariaLive="polite"
          style={{
            padding: '0.4rem 0.85rem',
            margin: '0.4rem 0.75rem',
            color: 'var(--color-warning-fg)',
            fontSize: '0.76rem',
            // Inline-banner chrome: skip the 4px accent.
            borderLeft: '1px solid var(--color-warning-border)',
          }}
        >
          Some data may be out of date — retrying…
        </CardContainer>
      )}
      {conv.error && (
        <CardContainer
          palette="danger"
          role="alert"
          style={{
            padding: '0.6rem 0.85rem',
            margin: '0.5rem 0.75rem',
            color: 'var(--color-danger-fg)',
            fontSize: '0.8rem',
            borderLeft: '1px solid var(--color-danger-border)',
          }}
        >
          {conv.error}
        </CardContainer>
      )}
      {branchWarning && (
        <CardContainer
          palette="warning"
          role="status"
          ariaLive="polite"
          style={{
            display: 'flex',
            alignItems: 'flex-start',
            gap: 8,
            padding: '0.6rem 0.85rem',
            margin: '0.5rem 0.75rem',
            color: 'var(--color-warning-fg)',
            fontSize: '0.8rem',
            borderLeft: '1px solid var(--color-warning-border)',
          }}
        >
          <span style={{ flex: 1 }}>{branchWarning}</span>
          <button
            type="button"
            aria-label="Dismiss"
            onClick={() => setBranchWarning(null)}
            style={{
              border: 'none',
              background: 'transparent',
              color: 'inherit',
              cursor: 'pointer',
              fontSize: '1rem',
              lineHeight: 1,
              padding: 0,
            }}
          >
            ×
          </button>
        </CardContainer>
      )}
      {/* R1.8: BranchFromHereCard is now wired through ChatTimeline →
         AssistantTurnCard as an inline footer affordance on the latest
         assistant turn, gated on a prior emission. The legacy modal
         entry point in TaskDetailDrawer still works for task-scoped
         branching. */}
      <ChatTimeline
        turns={conv.turns}
        pillStatusLine={sse.toolCallPill?.statusLine ?? null}
        streamingText={streaming.text}
        onConfirm={conv.confirm}
        onReject={conv.reject}
        onQuickReply={onQuickReply}
        sessionId={conv.sessionId}
        onBranch={
          ENABLE_BRANCH_FROM_HERE_CARD && hasEmittedAtLeastOnce
            ? onBranch
            : undefined
        }
        onRemoveOptionalStage={onRemoveOptionalStage}
      />
      {conv.stillThinking && (
        <StillThinkingIndicator
          stage={conv.thinkingStage}
          onCancel={conv.cancelTurn}
        />
      )}
      {dispositions.length > 0 && conv.sessionId && (
        <div style={{ padding: '0.5rem 0.75rem' }}>
          {dispositions.map((d) => (
            <DispositionReviewCard
              key={d.path}
              sessionId={conv.sessionId!}
              entry={d}
              onDone={loadDispositions}
            />
          ))}
        </div>
      )}
      {visibleProposals.length > 0 && conv.sessionId && (
        <div style={{ padding: '0.5rem 0.75rem' }}>
          {visibleProposals.map((p) => (
            <HypothesizedProposalCard
              key={p.id}
              sessionId={conv.sessionId!}
              proposal={p}
              liveOverlay={sse.proposalEvents[p.id] ?? null}
              onPromoted={loadProposals}
              onRejected={loadProposals}
            />
          ))}
        </div>
      )}
      {ENABLE_METHOD_OPTIONS_CARD && sensitivityBlocker && isDiscoverBlocker && (
        <div style={{ padding: '0.5rem 0.75rem' }}>
          <MethodOptionsCard
            stage={sensitivityBlocker.stage_id}
            options={methodOptions}
            onSelect={onSelectDiscoverMethod}
          />
        </div>
      )}
      {ENABLE_SENSITIVITY_COMPARISON_CARD &&
        sensitivityBlocker &&
        !isDiscoverBlocker && (
          <div style={{ padding: '0.5rem 0.75rem' }}>
            <SensitivityComparisonCard
              stage={sensitivityBlocker.stage_id}
              candidates={sensitivityBlocker.candidates}
              onSelect={onSelectSensitivityWinner}
            />
          </div>
        )}
      {ENABLE_RESULT_REVIEW_CARD && conv.sessionId && (
        <div style={{ padding: '0.5rem 0.75rem' }}>
          {Array.from(sse.reviewableTasks).map((taskId) => {
            const payload = taskResults[taskId]
            if (!payload) return null
            const status = narrowStatus(payload.status)
            if (!status) return null
            const reviewPayload: ResultReviewPayload = {
              task_id: payload.task_id,
              status,
              description: payload.description,
              kind: payload.kind as ResultReviewPayload['kind'],
              result: payload.result,
              reason: payload.reason,
              record: payload.record,
            }
            return (
              <ResultReviewTurnCard
                key={taskId}
                payload={reviewPayload}
                onRerun={onRerunTask}
                sessionId={conv.sessionId}
              />
            )
          })}
        </div>
      )}
      {ENABLE_ENSEMBLE_REVIEW_CARD &&
        conv.sessionId &&
        hasEmittedAtLeastOnce &&
        onOpenRobustnessTab && (
          <div style={{ padding: '0.5rem 0.75rem' }}>
            <EnsembleReviewTurnCard
              sessionId={conv.sessionId}
              onOpenRobustnessTab={onOpenRobustnessTab}
            />
          </div>
        )}
      {blockers.length > 0 && (
        <div style={{ padding: '0.5rem 0.75rem' }}>
          {blockers.map((blocked) => (
            <BlockerCard
              key={blocked.key}
              reason={blocked.reason}
              recoveryHint={blocked.recoveryHint}
              onUnblock={conv.unblock}
              blockerKind={blocked.blockerKind ?? null}
              sessionId={conv.sessionId}
              taskId={blocked.taskId}
              relatedDispositionPath={pendingDisposition?.path ?? null}
              relatedDispositionTaskId={pendingDisposition?.task_id ?? null}
            />
          ))}
        </div>
      )}
      {conv.sessionId &&
        (conv.state?.pending_input_hints?.length ?? 0) > 0 && (
          <PendingInputHintsCard
            sessionId={conv.sessionId}
            hints={conv.state?.pending_input_hints ?? []}
            onRegistered={conv.refreshCurrentState}
          />
        )}
      {conv.sessionId && conv.state?.state && (
        <StartExecutionPanel
          sessionId={conv.sessionId}
          sessionState={conv.state.state}
          executionRunning={conv.executionRunning}
          progress={conv.state.progress}
          onStart={conv.startExecutionAction}
        />
      )}
      <ChatComposer
        onSend={conv.sendTurn}
        disabled={conv.sending || !conv.sessionId}
      />
    </div>
  )
}
