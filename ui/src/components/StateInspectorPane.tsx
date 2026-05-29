import { Suspense, useEffect, useState } from 'react'
import { useCancelableEffect } from '../hooks/useCancelableFetch'
import type { SessionMetrics } from '../api/chatClient'
import {
  getChatMetrics,
  getComposeOutcome,
  getPilot,
  type ComposeOutcomePayload,
} from '../api/chatClient'
import { PILOT_POLL_MS } from '../lib/polling'
import { useEventsContext, useSessionContext } from '../hooks/contexts'
import StartExecutionCard from './StartExecutionCard'
import TaskLogDrawer from './TaskLogDrawer'
import RunningTasksPanel from './RunningTasksPanel'
import {
  DocumentsPane,
  JobsFeed,
  ClaimsTab,
  METRICS_POLL_MS,
  MetricsTable,
  PlaceholderPane,
  TABS,
  VerifierDecisionsTab,
  type Tab,
  mergePilotStates,
} from './state_inspector'
import {
  LazyCompareTab,
  LazyCompositionTab,
  LazyDashboardPane,
  LazyDecisionsTab,
  LazyFiguresPane,
  LazyHistoryPane,
  LazyInputsTab,
  LazyJobsFeed,
  LazyMetricsTable,
  LazyPlanTab,
  LazyRepairsTab,
  LazyStateTab,
} from './state_inspector/lazy'

// Re-export the per-tab surfaces so the existing Vitest suite (which
// imports DocumentsPane/JobsFeed/MetricsTable from this file) keeps
// working unchanged. New code should prefer `components/state_inspector`
// as the canonical path.
export { DocumentsPane, JobsFeed, MetricsTable, mergePilotStates }

/**
 * The Pane pulls its sessionId / state / harness progress / pilot /
 * stall / cross-version props from context rather than from App.tsx.
 * App is a layout shell.
 */
export default function StateInspectorPane() {
  const conv = useSessionContext()
  const sse = useEventsContext()
  const sessionId = conv.sessionId
  const state = conv.state
  // `sending` flips synchronously when the SME clicks Accept/Reject/Unblock,
  // before the server has finished /confirm + try_auto_emit_after_confirm
  // and before the next refreshState lands the new `state.state.kind`. Use
  // it to give the badge an immediate "updating…" state so an Accept click
  // doesn't leave the badge frozen at `pending confirmation` for 1–2 s
  // while the round-trip plays out. See #14.
  const sending = conv.sending
  // DAG ownership is hoisted into useConversation so resyncAll
  // (visibilitychange, SSE resync_required) and refreshCurrentState
  // (state_advanced + terminal harness_progress events) keep it fresh
  // in lockstep with `state` and `turns`. Fetching DAG locally on a
  // dep array of `[sessionId, state.state.kind, hp.length]` would
  // leave it stale when SSE missed a transition while the tab was
  // hidden — the TaskDetailDrawer's BlockerCard would then fail to
  // surface for a freshly-blocked task because it read a stale
  // `state.status`.
  const dag = conv.dag
  const harnessProgress = sse.harnessProgress
  const pilot = sse.pilot
  const stallSignals = sse.stallSignals
  const crossVersionReport = sse.crossVersionReport
  const executorInfo = sse.executorInfo
  const heartbeatStalls = sse.heartbeatStalls
  const progressHealth = sse.progressHealth
  const orphanReap = sse.orphanReap
  const [tab, setTab] = useState<Tab>('plan')
  const [metrics, setMetrics] = useState<SessionMetrics | null>(null)
  const [metricsAt, setMetricsAt] = useState<Date | null>(null)
  // Pre-fetch the typed compose outcome at pane level so
  // the Composition tab's badge can render before the SME clicks
  // the tab. Refreshes when last_activity changes (any SME mutation
  // bumps it). Returns null on legacy v1/v2/v3 sessions (the
  // endpoint is a 204) and on errors — both render no badge.
  const [composeOutcome, setComposeOutcome] =
    useState<ComposeOutcomePayload | null>(null)
  useCancelableEffect(async ({ cancelled }) => {
    if (!sessionId) {
      setComposeOutcome(null)
      return
    }
    try {
      const result = await getComposeOutcome(sessionId)
      if (!cancelled()) setComposeOutcome(result)
    } catch {
      if (!cancelled()) setComposeOutcome(null)
    }
  }, [sessionId, state?.last_activity])
  const [fetchedPilotReport, setFetchedPilotReport] = useState<unknown | null>(null)
  // Right-side drawer polls progress.log when the user clicks a task
  // in the Jobs feed. Auto-opens for the latest running task the first
  // time a task_started event arrives.
  const [openLogTaskId, setOpenLogTaskId] = useState<string | null>(null)
  const [autoOpenedOnce, setAutoOpenedOnce] = useState(false)
  useEffect(() => {
    if (autoOpenedOnce) return
    for (let i = harnessProgress.length - 1; i >= 0; i--) {
      const e = harnessProgress[i]
      if (e!.kind === 'task_started' && e!.taskId) {
        setOpenLogTaskId(e!.taskId)
        setAutoOpenedOnce(true)
        break
      }
    }
  }, [harnessProgress, autoOpenedOnce])

  // Fetch pilot report while the Metrics tab is visible. 30s cadence
  // (looser than metrics) because pilot data is static once written.
  // Gated on document.visibilityState so a backgrounded browser tab
  // doesn't poll either.
  useEffect(() => {
    if (!sessionId || tab !== 'metrics') return
    let cancelled = false
    const tick = async () => {
      try {
        const body = await getPilot(sessionId)
        if (!cancelled) {
          setFetchedPilotReport(body)
          conv.markFresh('pilot')
        }
      } catch {
        if (!cancelled) conv.markStale('pilot')
      }
    }
    void tick()
    const gatedTick = () => {
      if (document.visibilityState !== 'visible') return
      void tick()
    }
    const interval = window.setInterval(gatedTick, PILOT_POLL_MS)
    return () => {
      cancelled = true
      window.clearInterval(interval)
    }
  }, [sessionId, tab, conv])

  // Poll metrics only while that tab is visible. Also gated on
  // document.visibilityState so a backgrounded browser tab doesn't poll.
  useEffect(() => {
    if (!sessionId || tab !== 'metrics') return
    let cancelled = false
    const tick = async () => {
      try {
        const m = await getChatMetrics(sessionId)
        if (!cancelled) {
          setMetrics(m)
          setMetricsAt(new Date())
          conv.markFresh('metrics')
        }
      } catch {
        if (!cancelled) conv.markStale('metrics')
      }
    }
    void tick()
    const gatedTick = () => {
      if (document.visibilityState !== 'visible') return
      void tick()
    }
    const interval = window.setInterval(gatedTick, METRICS_POLL_MS)
    return () => {
      cancelled = true
      window.clearInterval(interval)
    }
  }, [sessionId, tab, conv])

  // Once harness progress events start arriving, switch to Jobs the
  // first time so the SME sees execution come alive.
  const [autoSwitched, setAutoSwitched] = useState(false)
  const [autoSwitchAnnouncement, setAutoSwitchAnnouncement] = useState('')
  useEffect(() => {
    if (!autoSwitched && harnessProgress.length > 0) {
      setTab('jobs')
      setAutoSwitched(true)
      setAutoSwitchAnnouncement(
        'Execution started — the Progress tab now shows live updates.',
      )
    }
  }, [harnessProgress.length, autoSwitched])

  // Listen for the CommandPalette's tab-switch event so a palette
  // Enter maps to setTab here without prop drilling. Only switch to a
  // known tab id so malformed events are silently ignored.
  useEffect(() => {
    const handler = (ev: Event) => {
      const ce = ev as CustomEvent<{ tab: string }>
      const requested = ce.detail?.tab
      if (requested && (TABS as readonly { id: string }[]).some((t) => t.id === requested)) {
        setTab(requested as Tab)
      }
    }
    window.addEventListener('ecaax:switch-tab', handler)
    return () => window.removeEventListener('ecaax:switch-tab', handler)
  }, [])

  // While a SME-triggered mutation is in flight (Accept / Revise / Unblock
  // / send message), show "updating…" instead of the stale `state.state.kind`
  // so the badge reflects the SME's click immediately rather than the
  // pre-click state. The real state lands as soon as refreshState resolves
  // (typically <1 s) or the SSE state_advanced event fires.
  const stateKindLabel = sending ? 'updating' : (state?.state.kind ?? 'idle')

  // Dispatch the active tab body. Each case consumes the unique slice
  // of local state it needs; tsc enforces the switch is exhaustive
  // over `Tab` so adding a new TAB entry forces an arm here.
  const renderActive = (): JSX.Element => {
    switch (tab) {
      case 'plan':
        return (
          <LazyPlanTab
            dag={dag}
            sessionId={sessionId}
            parentSessionId={state?.parent_session_id ?? null}
          />
        )
      case 'state':
        return <LazyStateTab state={state} sessionId={sessionId} />
      case 'documents':
        return <DocumentsPane state={state} sessionId={sessionId} />
      case 'jobs':
        return (
          <div
            style={{
              flex: 1,
              minHeight: 0,
              display: 'flex',
              flexDirection: 'column',
            }}
          >
            <StartExecutionCard
              sessionId={sessionId}
              emitted={
                state?.state.kind === 'emitted' ||
                state?.state.kind === 'blocked' ||
                state?.state.kind === 'amending'
              }
            />
            <div
              style={{
                flex: 1,
                minHeight: 0,
                position: 'relative',
                display: 'flex',
                flexDirection: 'column',
              }}
            >
              <RunningTasksPanel sessionId={sessionId} />
              <LazyJobsFeed
                events={harnessProgress}
                onOpenTaskLog={(id) => setOpenLogTaskId(id)}
                stallSignals={stallSignals ?? {}}
                executorInfo={executorInfo}
                heartbeatStalls={heartbeatStalls ?? {}}
                orphanReap={orphanReap}
              />
              <TaskLogDrawer
                sessionId={sessionId}
                taskId={openLogTaskId}
                onClose={() => setOpenLogTaskId(null)}
              />
            </div>
          </div>
        )
      case 'metrics':
        return (
          <LazyMetricsTable
            metrics={metrics}
            pilot={mergePilotStates(pilot, fetchedPilotReport)}
            sessionId={sessionId}
            progressHealth={progressHealth}
            fetchedAt={metricsAt}
          />
        )
      case 'figures':
        return <LazyFiguresPane sessionId={sessionId} dag={dag} />
      case 'dashboard':
        return <LazyDashboardPane sessionId={sessionId} />
      case 'decisions':
        return (
          <LazyDecisionsTab
            sessionId={sessionId}
            refreshKey={state?.state.kind ?? 'idle'}
            dag={dag}
            onJumpToTask={(id) => {
              setTab('plan')
              window.location.hash = `task=${encodeURIComponent(id)}`
            }}
          />
        )
      case 'history':
        return <LazyHistoryPane crossVersionReport={crossVersionReport ?? null} />
      case 'compare':
        return <LazyCompareTab sessionId={sessionId} />
      case 'inputs':
        return <LazyInputsTab sessionId={sessionId} />
      case 'composition':
        // Use last_activity (changes on every SME
        // mutation: assumption-resolve, adapter-decision,
        // novel-node-decision, refusal-acknowledge, policy-bundle
        // change) so the tab refetches whenever the typed outcome
        // could have shifted. state.kind alone is too coarse:
        // resolving an assumption keeps the session in Emitted
        // and would bypass the refresh.
        return (
          <LazyCompositionTab
            sessionId={sessionId}
            refreshKey={`${state?.state.kind ?? 'idle'}:${state?.last_activity ?? ''}`}
          />
        )
      case 'repairs':
        // V3+v4 residuals closure pending repair-strategy
        // proposals produced by the planner's gap-closure pipeline.
        // The hook inside RepairsTab polls every 4s.
        return <LazyRepairsTab sessionId={sessionId} />
      case 'verifier_decisions':
        // v4 P2 / F18 — typed verifier decision substrate. The tab body
        // lives outside the lazy bundle (no asynchrony needed) so we
        // render the eager export here.
        return <VerifierDecisionsTab sessionId={sessionId} />
      case 'claims':
        // Runtime claim verification rollup across all completed tasks.
        // Distinct from `verifier_decisions` above — that surfaces the
        // v4 composer's compile-time port-unification trace; this one
        // runs `claim_extractor` + `claim_verifier` on per-task
        // narratives against result tables.
        return <ClaimsTab sessionId={sessionId} dag={dag} />
      default: {
        // F5 Gate C — compile-time exhaustiveness fence on `Tab`. Adding
        // a new `Tab` variant to `state_inspector/index.ts::Tab` without
        // wiring an arm above produces a tsc error here:
        // `Type 'X' is not assignable to type 'never'.`
        // The runtime branch is unreachable because every prior arm
        // returns; the `throw` exists so the function's return type
        // remains `JSX.Element` rather than `JSX.Element | undefined`.
        const _exhaustive: never = tab
        throw new Error(`unhandled StateInspector tab: ${String(_exhaustive)}`)
      }
    }
  }

  return (
    <div
      style={{
        display: 'flex',
        flexDirection: 'column',
        height: '100%',
        background: 'var(--color-surface-0)',
      }}
    >
      <div
        style={{
          display: 'flex',
          gap: '0.25rem',
          padding: '0.5rem 0.5rem 0',
          borderBottom: '1px solid var(--color-border-default)',
          background: 'var(--color-surface-1)',
          flexShrink: 0,
        }}
      >
        <div
          role="tablist"
          aria-label="State inspector"
          style={{ display: 'flex', gap: '0.25rem' }}
        >
          {TABS.filter((t) => {
            // Compare tab only shows when the session has a parent to
            // compare against. Hide for pure root sessions so the
            // tablist doesn't get cluttered.
            if (t.id === 'compare') {
              return Boolean(state?.parent_session_id)
            }
            return true
          }).map((t) => (
            <button
              key={t.id}
              type="button"
              role="tab"
              aria-selected={tab === t.id}
              aria-controls={`state-panel-${t.id}`}
              id={`state-tab-${t.id}`}
              onClick={() => setTab(t.id)}
              style={{
                padding: '0.45rem 0.85rem',
                background: tab === t.id ? 'var(--color-surface-1)' : 'transparent',
                border: 'none',
                borderBottom:
                  tab === t.id
                    ? '2px solid var(--color-accent)'
                    : '2px solid transparent',
                color: tab === t.id ? 'var(--color-text-primary)' : 'var(--color-text-muted)',
                cursor: 'pointer',
                fontSize: '0.82rem',
                fontWeight: 600,
              }}
            >
              {t.label}
              {t.id === 'jobs' && harnessProgress.length > 0 && (
                <span
                  aria-label={`${harnessProgress.length} progress events`}
                  style={{
                    marginLeft: 6,
                    background: 'var(--color-accent)',
                    color: 'var(--color-accent-fg)',
                    borderRadius: 999,
                    padding: '0 6px',
                    fontSize: '0.65rem',
                  }}
                >
                  {harnessProgress.length}
                </span>
              )}
              {t.id === 'composition' &&
                composeOutcome !== null &&
                compositionBadgeCount(composeOutcome) > 0 && (
                  <span
                    aria-label={`${compositionBadgeCount(composeOutcome)} unresolved composition issue${compositionBadgeCount(composeOutcome) === 1 ? '' : 's'}`}
                    style={{
                      marginLeft: 6,
                      background:
                        composeOutcome.variant === 'refusal'
                          ? 'var(--color-danger-accent)'
                          : 'var(--color-warning-accent)',
                      color: '#fff',
                      borderRadius: 999,
                      padding: '0 6px',
                      fontSize: '0.65rem',
                    }}
                  >
                    {compositionBadgeCount(composeOutcome)}
                  </span>
                )}
            </button>
          ))}
        </div>
        <div style={{ flex: 1 }} />
        <div
          aria-label="Session state"
          style={{
            alignSelf: 'center',
            marginRight: '0.5rem',
            padding: '0.2rem 0.55rem',
            background: 'var(--color-info-bg)',
            color: 'var(--color-info-fg)',
            border: '1px solid var(--color-info-border)',
            borderRadius: 999,
            fontSize: '0.72rem',
            fontWeight: 600,
            textTransform: 'capitalize',
          }}
        >
          {stateKindLabel.replace(/_/g, ' ')}
        </div>
      </div>

      <div
        aria-live="polite"
        aria-atomic="true"
        style={{
          position: 'absolute',
          width: 1,
          height: 1,
          padding: 0,
          margin: -1,
          overflow: 'hidden',
          clip: 'rect(0,0,0,0)',
          whiteSpace: 'nowrap',
          border: 0,
        }}
      >
        {autoSwitchAnnouncement}
      </div>

      <div
        role="tabpanel"
        id={`state-panel-${tab}`}
        aria-labelledby={`state-tab-${tab}`}
        style={{
          flex: 1,
          minHeight: 0,
          display: 'flex',
          flexDirection: 'column',
          overflow: 'hidden',
        }}
      >
        <Suspense fallback={<PlaceholderPane>Loading…</PlaceholderPane>}>
          {renderActive()}
        </Suspense>
      </div>
    </div>
  )
}

/**
 * Derive a count for the Composition tab badge.
 *
 * Returns the number of items the SME should pay attention to:
 *  - Refusal outcome → 1 (the refusal itself; treated as
 *  critical so the badge color flips to danger).
 *  - NovelNodeSpec outcome → 1 (the proposed node awaiting review).
 *  - DraftDag outcome → blocker count (each is a remaining gate).
 *  - PartialDag outcome → unresolved gap count.
 *  - ValidatedExecutableDag with unresolved assumptions → assumption count.
 *
 * Returns 0 when the outcome is `validated_executable_dag` AND
 * assumption_count is zero — nothing for the SME to act on.
 */
function compositionBadgeCount(outcome: ComposeOutcomePayload): number {
  switch (outcome.variant) {
    case 'refusal':
      return 1
    case 'novel_node_spec':
      return 1
    case 'draft_dag':
      return Math.max(outcome.blockers?.length ?? 0, outcome.assumption_count)
    case 'partial_dag':
      return outcome.unresolved_gaps?.length ?? 0
    case 'validated_executable_dag':
      return outcome.assumption_count
    default:
      return 0
  }
}
