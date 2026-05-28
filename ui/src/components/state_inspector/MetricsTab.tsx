import { useEffect, useMemo, useState } from 'react'
import {
  scoreChatSession,
  getValueAddMetrics,
  RUBRIC_MAX_TOTAL,
  RUBRIC_PASS_THRESHOLD,
  type PerTaskAgentSnapshot,
  type RubricScore,
  type SessionMetrics,
  type TokenBucket,
  type AffordanceFallbackSummary,
  type TierResult,
  type ValueAddMetricsResponse,
} from '../../api/chatClient'
import type { PilotLifecycle, ProgressHealth } from '../../hooks/useSseChatEvents'
import BudgetRow from '../BudgetRow'
import { METRICS_POLL_MS, PlaceholderPane } from './common'
import { CatalogGapsCard } from '../CatalogGapsCard'
import { RendererProposalCard } from '../RendererProposalCard'
import { formatUSD, formatInteger } from '../../lib/format'

type ScoringState =
  | { kind: 'idle' }
  | { kind: 'running' }
  | { kind: 'done'; score: RubricScore }
  | { kind: 'error'; message: string }

/**
 * Exported for direct Vitest coverage; the parent Pane component
 * renders it as the metrics tab's body.
 */
export function MetricsTable({
  metrics,
  pilot,
  sessionId,
  progressHealth,
  fetchedAt,
}: {
  metrics: SessionMetrics | null
  pilot?: PilotLifecycle
  /**
   * Required for the "Score transcript" button. Null before the session
   * has started or absent in legacy test fixtures; the button renders
   * disabled when missing.
   */
  sessionId?: string | null
  /**
   * Rolling POST-health snapshot. When
   * `failedPosts / totalPosts > 0.05` the table surfaces a
   * "Progress events lost" row in red.
   */
  progressHealth?: ProgressHealth | null
  /**
   * Wall-clock timestamp of the last successful metrics fetch. Surfaced
   * in the cost breakdown block so operators can tell how stale the
   * numbers are between 4s polls.
   */
  fetchedAt?: Date | null
}): JSX.Element {
  const [scoring, setScoring] = useState<ScoringState>({ kind: 'idle' })
  const [valueAdd, setValueAdd] = useState<ValueAddMetricsResponse | null>(null)

  // Poll /value-add-metrics every 4s (same cadence as the main metrics poll).
  // Errors are swallowed so a missing file or pre-emit session shows the empty
  // state rather than crashing the tab. Polling is gated on
  // document.visibilityState so a backgrounded tab doesn't burn quota.
  useEffect(() => {
    if (!sessionId) return
    let cancelled = false
    const poll = async () => {
      try {
        const r = await getValueAddMetrics(sessionId)
        if (!cancelled) setValueAdd(r)
      } catch {
        // tile renders empty state on error
      }
    }
    void poll()
    const tick = () => {
      if (document.visibilityState !== 'visible') return
      void poll()
    }
    const t = setInterval(tick, METRICS_POLL_MS)
    return () => {
      cancelled = true
      clearInterval(t)
    }
  }, [sessionId])

  // Memoize `rows` and `instanceTypeEntries` so the table body only
  // rebuilds when the metrics snapshot's reference changes. MetricsTab
  // re-renders on a 4s interval + on streaming token
  // updates; without memoization Object.entries + sort fires each time.
  const rows = useMemo<Array<[string, string]>>(() => {
    if (!metrics) return []
    // F3 — derived burn rates. Only render the rows once we have a
    // meaningful interval (>60s) — otherwise a 5-second-old session
    // shows wildly inflated $/hr figures.
    const dur = metrics.session_duration_seconds ?? 0
    const burnRows: Array<[string, string]> =
      dur > 60
        ? [
            ['Session duration', formatDuration(dur)],
            [
              'Chat burn',
              `${formatUSD(ratePerHour(metrics.chat_cost_usd ?? 0, dur), { precision: 4 })}/hr`,
            ],
            [
              'Agent burn',
              `${formatUSD(ratePerHour(metrics.agent_cost_usd ?? 0, dur), { precision: 4 })}/hr`,
            ],
            [
              'Input-token burn',
              `${formatTokensPerHour(metrics.total_input_tokens, dur)}/hr`,
            ],
          ]
        : []
    return [
      ['Turns', metrics.turn_count.toString()],
      ['Tool calls', metrics.tool_call_count.toString()],
      ['Sonnet turns', metrics.sonnet_turns.toString()],
      ['Opus turns', metrics.opus_turns.toString()],
      ...burnRows,
      ['Mean turn latency', `${metrics.mean_turn_ms} ms`],
      ['p50 turn latency', `${metrics.p50_turn_ms} ms`],
      ['p95 turn latency', `${metrics.p95_turn_ms} ms`],
      ['p99 turn latency', `${metrics.p99_turn_ms} ms`],
      ['Max turn latency', `${metrics.max_turn_ms} ms`],
      ['Input tokens', formatInteger(metrics.total_input_tokens)],
      ['Output tokens', formatInteger(metrics.total_output_tokens)],
      ['Cache reads', formatInteger(metrics.cache_read_tokens)],
      ['Cache creation', formatInteger(metrics.cache_creation_tokens)],
      // §4.5 — cache hit ratio. Surfaced as a percentage with a warning
      // glyph when it dips below 50% on a warm session (10+ turns).
      // F5 — append the dollar savings inline when nonzero so the value
      // of caching is visible alongside the ratio.
      [
        'Cache hit ratio',
        formatCacheHitRatio(metrics.cache_hit_ratio, metrics.turn_count) +
          formatCacheSavingsSuffix(metrics.cache_savings_usd ?? 0),
      ],
    ]
    // Cost rows are handled by <CostBreakdownBlock> below — keeps the
    // per-bucket + per-model nested structure distinct from the flat
    // token/latency rows above.
  }, [metrics])

  // §3.3/§4.2 — Opus escalation rows. Only render when at least one
  // escalation fired — Sonnet-only sessions shouldn't grow the table.
  const opusEscalationEntries = useMemo(() => {
    const r = metrics?.opus_escalation_reasons
    if (!r) return []
    return Object.entries(r).sort(([a], [b]) => a.localeCompare(b))
  }, [metrics])

  // §4.4 — session input / budget progress. Suppressed when the server
  // side budget is null (SWFC_SESSION_TOKEN_BUDGET=0) or absent (legacy
  // server build that doesn't emit the field yet). The Rust side's
  // read_session_token_budget() agrees with the tool-loop's
  // check_budget soft-block, so the bar position here reflects the
  // same threshold the session will actually soft-block against.
  const budget = useMemo(() => {
    if (!metrics) return null
    const cap = metrics.session_token_budget
    if (cap == null || cap <= 0) return null
    const used = metrics.total_input_tokens
    const ratio = Math.max(0, Math.min(1, used / cap))
    // Thresholds aligned with the a11y palette (WCAG AA ≥ 4.5:1).
    let tone: 'ok' | 'warn' | 'crit' = 'ok'
    if (ratio >= 0.9) tone = 'crit'
    else if (ratio >= 0.6) tone = 'warn'
    return { used, cap, ratio, tone }
  }, [metrics])

  // §3.2/§4.3 — Iteration histogram (p50/max). Only render when at
  // least one turn has been recorded.
  const iterationSummary = useMemo<string | null>(() => {
    const h = metrics?.tool_loop_iterations_histogram
    if (!h || Object.keys(h).length === 0) return null
    const counts = Object.entries(h)
      .map(([k, v]) => [Number(k), v] as const)
      .sort(([a], [b]) => a - b)
    const total = counts.reduce((acc, [, v]) => acc + v, 0)
    if (total === 0) return null
    let cum = 0
    let p50 = counts[0]?.[0] ?? 0
    for (const [iters, n] of counts) {
      cum += n
      if (cum >= total / 2) {
        p50 = iters
        break
      }
    }
    const max = counts[counts.length - 1]?.[0] ?? 0
    return `p50 ${p50} · max ${max}`
  }, [metrics])

  const instanceTypeEntries = useMemo(() => {
    if (!metrics) return []
    return Object.entries(metrics.instance_type_seconds)
      .filter(([, secs]) => secs > 0)
      .sort(([a], [b]) => a.localeCompare(b))
  }, [metrics])

  // Per-model breakdown rows. Skipped when only Sonnet + Opus are
  // represented (the flat sonnet_turns / opus_turns rows already cover
  // that case) so the table doesn't grow for the common case. When
  // Haiku or a future model joins the session, extra rows render here.
  // `opus_4_7` is collapsed into the legacy `opus_*` flat mirrors on
  // the server (they aggregate across Opus variants), so the
  // per-model row would double-count if we rendered it again — filter
  // it out here alongside the flat-mirrored variants.
  const extraModelEntries = useMemo(() => {
    if (!metrics?.per_model_cost_usd) return []
    const KNOWN_FLAT = new Set(['sonnet_4_6', 'opus_4_6', 'opus_4_7'])
    return Object.entries(metrics.per_model_cost_usd)
      .filter(([key]) => !KNOWN_FLAT.has(key))
      .sort(([a], [b]) => a.localeCompare(b))
  }, [metrics])

  if (!metrics) {
    return (
      <PlaceholderPane>
        Metrics will appear here once you've sent your first turn. Updates
        every {METRICS_POLL_MS / 1000}s while this tab is open.
      </PlaceholderPane>
    )
  }
  const hasInstanceSeconds = metrics.total_instance_seconds > 0
  const hasHighWaterCount = metrics.high_water_exceeded_count > 0
  const canScore = !!sessionId && metrics.turn_count > 0 && scoring.kind !== 'running'
  const onScoreClick = async () => {
    if (!sessionId) return
    setScoring({ kind: 'running' })
    try {
      const score = await scoreChatSession(sessionId)
      setScoring({ kind: 'done', score })
    } catch (e) {
      setScoring({
        kind: 'error',
        message: e instanceof Error ? e.message : String(e),
      })
    }
  }
  return (
    <div
      style={{
        flex: 1,
        overflowY: 'auto',
        padding: '1rem',
        background: 'var(--color-surface-1)',
      }}
    >
      <div
        style={{
          display: 'flex',
          justifyContent: 'flex-end',
          alignItems: 'center',
          gap: '0.5rem',
          marginBottom: '0.5rem',
        }}
      >
        {scoring.kind === 'running' && (
          <span style={{ fontSize: '0.78rem', color: 'var(--color-text-muted)' }}>Scoring…</span>
        )}
        {scoring.kind === 'error' && (
          <span style={{ fontSize: '0.78rem', color: 'var(--color-danger-fg)' }}>
            {scoring.message}
          </span>
        )}
        <button
          type="button"
          onClick={onScoreClick}
          disabled={!canScore}
          aria-label="Score transcript"
          style={{
            padding: '0.35rem 0.75rem',
            fontSize: '0.78rem',
            background: canScore
              ? 'var(--color-button-primary-bg)'
              : 'var(--color-border-strong)',
            color: canScore
              ? 'var(--color-button-primary-fg)'
              : 'var(--color-text-muted)',
            border: 'none',
            borderRadius: '0.25rem',
            cursor: canScore ? 'pointer' : 'not-allowed',
          }}
        >
          Score transcript
        </button>
      </div>
      <BudgetRow metrics={metrics} sessionId={sessionId ?? null} />
      <table
        aria-label="Per-session metrics"
        style={{
          width: '100%',
          borderCollapse: 'collapse',
          fontSize: '0.83rem',
        }}
      >
        <tbody>
          {rows.map(([label, value]) => (
            <tr key={label} style={{ borderBottom: '1px solid var(--color-border-subtle)' }}>
              <th
                scope="row"
                style={{
                  textAlign: 'left',
                  padding: '0.45rem 0.5rem 0.45rem 0',
                  fontWeight: 500,
                  color: 'var(--color-text-secondary)',
                }}
              >
                {label}
              </th>
              <td
                style={{
                  textAlign: 'right',
                  padding: '0.45rem 0',
                  fontFamily: 'ui-monospace, monospace',
                  color: 'var(--color-text-primary)',
                }}
              >
                {value}
              </td>
            </tr>
          ))}
          {budget && (
            <tr
              key="session-token-budget"
              data-metric-row="session_token_budget"
              style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
            >
              <th
                scope="row"
                style={{
                  textAlign: 'left',
                  padding: '0.45rem 0.5rem 0.45rem 0',
                  fontWeight: 500,
                  color: 'var(--color-text-secondary)',
                }}
              >
                Session token budget
              </th>
              <td
                style={{
                  textAlign: 'right',
                  padding: '0.45rem 0',
                  fontFamily: 'ui-monospace, monospace',
                  color: 'var(--color-text-primary)',
                }}
              >
                <BudgetProgressBar
                  used={budget.used}
                  cap={budget.cap}
                  ratio={budget.ratio}
                  tone={budget.tone}
                />
              </td>
            </tr>
          )}
          {extraModelEntries.map(([modelKey, cost]) => {
            const turns = metrics.per_model_turns?.[modelKey] ?? 0
            return (
              <tr
                key={`model-${modelKey}`}
                data-metric-row="per_model"
                data-model-key={modelKey}
                style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
              >
                <th
                  scope="row"
                  style={{
                    textAlign: 'left',
                    padding: '0.45rem 0.5rem 0.45rem 0',
                    fontWeight: 500,
                    color: 'var(--color-text-secondary)',
                  }}
                >
                  {modelKey} · {turns} turn{turns === 1 ? '' : 's'}
                </th>
                <td
                  style={{
                    textAlign: 'right',
                    padding: '0.45rem 0',
                    fontFamily: 'ui-monospace, monospace',
                    color: 'var(--color-text-primary)',
                  }}
                >
                  {formatUSD(cost, { precision: 4 })}
                </td>
              </tr>
            )
          })}
          {hasInstanceSeconds && (
            <tr
              key="instance-hours"
              data-metric-row="instance_hours"
              style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
            >
              <th
                scope="row"
                style={{
                  textAlign: 'left',
                  padding: '0.45rem 0.5rem 0.45rem 0',
                  fontWeight: 500,
                  color: 'var(--color-text-secondary)',
                }}
              >
                Instance hours
              </th>
              <td
                style={{
                  textAlign: 'right',
                  padding: '0.45rem 0',
                  fontFamily: 'ui-monospace, monospace',
                  color: 'var(--color-text-primary)',
                }}
              >
                {(metrics.total_instance_seconds / 3600).toFixed(2)}
              </td>
            </tr>
          )}
          {hasInstanceSeconds &&
            instanceTypeEntries.map(([instanceType, secs]) => (
              <tr
                key={`instance-type-${instanceType}`}
                data-metric-row="instance_type"
                data-instance-type={instanceType}
                style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
              >
                <th
                  scope="row"
                  style={{
                    textAlign: 'left',
                    padding: '0.35rem 0.5rem 0.35rem 1rem',
                    fontWeight: 400,
                    color: 'var(--color-text-muted)',
                    fontSize: '0.78rem',
                  }}
                >
                  {instanceType}
                </th>
                <td
                  style={{
                    textAlign: 'right',
                    padding: '0.35rem 0',
                    fontFamily: 'ui-monospace, monospace',
                    color: 'var(--color-text-secondary)',
                    fontSize: '0.78rem',
                  }}
                >
                  {(secs / 3600).toFixed(2)} h
                </td>
              </tr>
            ))}
          {iterationSummary && (
            <tr
              key="iteration-histogram"
              data-metric-row="tool_loop_iterations"
              style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
            >
              <th
                scope="row"
                style={{
                  textAlign: 'left',
                  padding: '0.45rem 0.5rem 0.45rem 0',
                  fontWeight: 500,
                  color: 'var(--color-text-secondary)',
                }}
              >
                Tool-loop iterations
              </th>
              <td
                style={{
                  textAlign: 'right',
                  padding: '0.45rem 0',
                  fontFamily: 'ui-monospace, monospace',
                  color: 'var(--color-text-primary)',
                }}
              >
                {iterationSummary}
              </td>
            </tr>
          )}
          {opusEscalationEntries.map(([reason, count]) => (
            <tr
              key={`opus-escalation-${reason}`}
              data-metric-row="opus_escalation"
              data-escalation-reason={reason}
              style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
            >
              <th
                scope="row"
                style={{
                  textAlign: 'left',
                  padding: '0.35rem 0.5rem 0.35rem 1rem',
                  fontWeight: 400,
                  color: 'var(--color-text-muted)',
                  fontSize: '0.78rem',
                }}
              >
                Opus · {reason}
              </th>
              <td
                style={{
                  textAlign: 'right',
                  padding: '0.35rem 0',
                  fontFamily: 'ui-monospace, monospace',
                  color: 'var(--color-text-secondary)',
                  fontSize: '0.78rem',
                }}
              >
                {count}
              </td>
            </tr>
          ))}
          {hasHighWaterCount && (
            <tr
              key="high-water-resizes"
              data-metric-row="high_water_resizes"
              style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
            >
              <th
                scope="row"
                style={{
                  textAlign: 'left',
                  padding: '0.45rem 0.5rem 0.45rem 0',
                  fontWeight: 500,
                  color: 'var(--color-text-secondary)',
                }}
              >
                High-water resizes
              </th>
              <td
                style={{
                  textAlign: 'right',
                  padding: '0.45rem 0',
                  fontFamily: 'ui-monospace, monospace',
                  color: 'var(--color-text-primary)',
                }}
              >
                {metrics.high_water_exceeded_count.toString()}
              </td>
            </tr>
          )}
          {/* Plan §S7.11 — composer performance row. Hidden when no
              composer-driven emit has fired yet. Displays as
              "N runs · M ms total · P atoms · Q backtracks · R excl
              hits" so the operator can spot when the backward-chain
              path is climbing toward the planned p99 < 500ms budget. */}
          {(metrics.composer_runs ?? 0) > 0 && (
            <tr
              key="composer"
              data-metric-row="composer"
              style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
            >
              <th
                scope="row"
                style={{
                  textAlign: 'left',
                  padding: '0.45rem 0.5rem 0.45rem 0',
                  fontWeight: 500,
                  color: 'var(--color-text-secondary)',
                }}
              >
                Composer
              </th>
              <td
                style={{
                  textAlign: 'right',
                  padding: '0.45rem 0',
                  fontFamily: 'ui-monospace, monospace',
                  color: 'var(--color-text-primary)',
                  fontSize: '0.78rem',
                }}
              >
                {metrics.composer_runs} runs ·{' '}
                {(metrics.composer_total_duration_ms ?? 0).toString()} ms ·{' '}
                {(metrics.composer_atoms_considered ?? 0).toString()} atoms
                {(metrics.composer_backtracks ?? 0) > 0 &&
                  ` · ${metrics.composer_backtracks} backtracks`}
                {(metrics.composer_exclusion_hits ?? 0) > 0 &&
                  ` · ${metrics.composer_exclusion_hits} excl hits`}
              </td>
            </tr>
          )}
          {pilot && pilot.status && (
            <tr
              key="pilot"
              data-metric-row="pilot"
              style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
            >
              <th
                scope="row"
                style={{
                  textAlign: 'left',
                  padding: '0.45rem 0.5rem 0.45rem 0',
                  fontWeight: 500,
                  color: 'var(--color-text-secondary)',
                }}
              >
                Pilot
              </th>
              <td
                style={{
                  textAlign: 'right',
                  padding: '0.45rem 0',
                  fontFamily: 'ui-monospace, monospace',
                  color: 'var(--color-text-primary)',
                }}
              >
                {pilot.status === 'complete' ? (
                  <PilotSummary report={pilot.report} />
                ) : pilot.status === 'skipped' ? (
                  <span style={{ fontStyle: 'italic', color: 'var(--color-text-muted)' }}>
                    skipped — {pilot.skipReason ?? 'disabled'}
                  </span>
                ) : (
                  <span style={{ fontStyle: 'italic', color: 'var(--color-info-accent)' }}>running…</span>
                )}
              </td>
            </tr>
          )}
          {progressHealth && progressHealth.totalPosts > 0 && (() => {
            const ratio =
              progressHealth.failedPosts / progressHealth.totalPosts
            const degraded = ratio > 0.05
            return (
              <tr
                key="progress-events-lost"
                data-metric-row="progress_events_lost"
                style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
              >
                <th
                  scope="row"
                  style={{
                    textAlign: 'left',
                    padding: '0.45rem 0.5rem 0.45rem 0',
                    fontWeight: 500,
                    color: degraded ? 'var(--color-danger-fg)' : 'var(--color-text-secondary)',
                  }}
                >
                  Progress events lost
                </th>
                <td
                  style={{
                    textAlign: 'right',
                    padding: '0.45rem 0',
                    fontFamily: 'ui-monospace, monospace',
                    color: degraded ? 'var(--color-danger-fg)' : 'var(--color-text-primary)',
                  }}
                >
                  {progressHealth.failedPosts}/{progressHealth.totalPosts}
                  {' '}
                  <span style={{ color: 'var(--color-text-muted)' }}>
                    (attempts {progressHealth.totalAttempts})
                  </span>
                </td>
              </tr>
            )
          })()}
        </tbody>
      </table>
      <CostBreakdownBlock metrics={metrics} fetchedAt={fetchedAt} />
      <StageClassRollupBlock perTask={metrics.per_task_agent ?? []} />
      <PerTaskBreakdownBlock perTask={metrics.per_task_agent ?? []} />
      <ToolCallsBlock byName={metrics.tool_calls_by_name ?? {}} />
      {scoring.kind === 'done' && <RubricScoreBlock score={scoring.score} />}
      <AffordanceFallbacksSection
        fallbacks={metrics.affordance_fallbacks ?? []}
        sessionId={sessionId ?? null}
      />
      <ValueAddTile valueAdd={valueAdd} />
      <p
        style={{
          marginTop: '1rem',
          fontSize: '0.72rem',
          color: 'var(--color-text-faint)',
          fontStyle: 'italic',
          lineHeight: 1.5,
        }}
      >
        Token totals are 0 in mock-backed test sessions because
        MockLlmBackend doesn't populate Usage. Live sessions against the
        Anthropic API surface real values.
      </p>
    </div>
  )
}

/**
 * Renders the nine-dimension rubric score + total + pass glyph returned
 * by POST /api/chat/session/:id/score. The cost is recorded in the
 * session's `scorer_cost_usd` bucket and surfaces in the main table
 * on the next metrics poll.
 */
function RubricScoreBlock({ score }: { score: RubricScore }): JSX.Element {
  const total =
    score.naturalness +
    score.continuity +
    score.one_question +
    score.method_neutrality +
    score.claim_boundary +
    score.tool_efficiency +
    score.confirmation +
    score.recovery +
    score.hardware_awareness
  const passed = total >= RUBRIC_PASS_THRESHOLD
  const dimensions: Array<[string, number]> = [
    ['Naturalness', score.naturalness],
    ['Continuity', score.continuity],
    ['One question', score.one_question],
    ['Method neutrality', score.method_neutrality],
    ['Claim boundary', score.claim_boundary],
    ['Tool efficiency', score.tool_efficiency],
    ['Confirmation', score.confirmation],
    ['Recovery', score.recovery],
    ['Hardware awareness', score.hardware_awareness],
  ]
  return (
    <div
      data-metric-row="rubric_score"
      style={{
        marginTop: '1rem',
        padding: '0.75rem',
        background: 'var(--color-surface-0)',
        border: '1px solid var(--color-border-default)',
        borderRadius: '0.25rem',
      }}
    >
      <div
        style={{
          display: 'flex',
          justifyContent: 'space-between',
          alignItems: 'baseline',
          marginBottom: '0.5rem',
        }}
      >
        <span style={{ fontSize: '0.83rem', fontWeight: 600, color: 'var(--color-text-primary)' }}>
          Rubric score
        </span>
        <span
          style={{
            fontSize: '0.83rem',
            fontFamily: 'ui-monospace, monospace',
            color: passed ? 'var(--color-success-fg)' : 'var(--color-warning-accent)',
          }}
        >
          {total} / {RUBRIC_MAX_TOTAL} {passed ? '✓' : '⚠'}
        </span>
      </div>
      <table
        aria-label="Rubric score dimensions"
        style={{ width: '100%', borderCollapse: 'collapse', fontSize: '0.78rem' }}
      >
        <tbody>
          {dimensions.map(([label, value]) => (
            <tr key={label} style={{ borderBottom: '1px solid var(--color-border-default)' }}>
              <th
                scope="row"
                style={{
                  textAlign: 'left',
                  padding: '0.3rem 0.5rem 0.3rem 0',
                  fontWeight: 400,
                  color: 'var(--color-text-secondary)',
                }}
              >
                {label}
              </th>
              <td
                style={{
                  textAlign: 'right',
                  padding: '0.3rem 0',
                  fontFamily: 'ui-monospace, monospace',
                  color: 'var(--color-text-primary)',
                }}
              >
                {value} / 2
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

/**
 * Merge SSE-provided pilot state with the Metrics tab's `/pilot` fetch.
 * SSE wins when it has a report (more recent); the fetched payload
 * hydrates the row on page refresh when SSE hasn't fired yet.
 */
export function mergePilotStates(
  sse: PilotLifecycle | undefined,
  fetched: unknown | null,
): PilotLifecycle | undefined {
  if (sse && sse.status === 'complete' && sse.report) return sse
  if (fetched) return { status: 'complete', report: fetched, skipReason: null }
  return sse
}

/**
 * §4.4 — Inline progress bar for `total_input_tokens` against
 * `session_token_budget`. Renders as a right-aligned cell in the main
 * metrics table: a thin colored bar above a text row showing the
 * absolute numbers and percentage.
 *
 * Color thresholds (matching the a11y palette — all
 * foreground/background pairs clear WCAG AA contrast on white):
 *  - < 60% green (#15803d fill on #dcfce7 track)
 *  - 60–90% amber (#b45309 fill on #fef3c7 track)
 *  - ≥ 90% red (#b91c1c fill on #fee2e2 track) + "approaching ceiling"
 *
 * Accessibility: the outer wrapper is `role="progressbar"` with
 * aria-valuenow/max/min + a human-readable aria-valuetext so the
 * percentage reads cleanly under a screen reader.
 */
export function BudgetProgressBar({
  used,
  cap,
  ratio,
  tone,
}: {
  used: number
  cap: number
  ratio: number
  tone: 'ok' | 'warn' | 'crit'
}): JSX.Element {
  const fillColor =
    tone === 'crit'
      ? 'var(--color-danger-accent)'
      : tone === 'warn'
        ? 'var(--color-warning-accent)'
        : 'var(--color-success-accent)'
  const trackColor =
    tone === 'crit'
      ? 'var(--color-danger-bg)'
      : tone === 'warn'
        ? 'var(--color-warning-bg)'
        : 'var(--color-success-bg)'
  const pct = Math.round(ratio * 100)
  const valueText = `${formatInteger(used)} of ${formatInteger(cap)} input tokens (${pct}%)`
  return (
    <span
      style={{
        display: 'inline-flex',
        flexDirection: 'column',
        alignItems: 'flex-end',
        gap: '0.2rem',
        minWidth: '12rem',
      }}
      role="progressbar"
      aria-valuenow={used}
      aria-valuemin={0}
      aria-valuemax={cap}
      aria-valuetext={valueText}
      aria-label="Session input token budget"
      data-tone={tone}
    >
      <span
        aria-hidden="true"
        style={{
          position: 'relative',
          width: '100%',
          height: '0.45rem',
          background: trackColor,
          borderRadius: '0.2rem',
          overflow: 'hidden',
        }}
      >
        <span
          style={{
            position: 'absolute',
            inset: '0 auto 0 0',
            width: `${pct}%`,
            background: fillColor,
            transition: 'width 180ms ease-out',
          }}
        />
      </span>
      <span style={{ fontSize: '0.72rem', color: 'var(--color-text-secondary)' }}>
        {formatInteger(used)} / {formatInteger(cap)} ({pct}%)
        {tone === 'crit' && (
          <span style={{ color: 'var(--color-danger-fg)', marginLeft: '0.35rem' }}>
            ⚠ approaching ceiling
          </span>
        )}
      </span>
    </span>
  )
}

/**
 * Tolerance (in USD) within which four-bucket sum == total_cost_usd is
 * considered exact. Floating-point summation of four independent f64
 * round-trips through JSON can accumulate ~$0.0001 of error; $0.01 is a
 * generous but practical threshold that catches real drift without
 * false-firing on representation noise.
 */
const COST_SUM_TOLERANCE_USD = 0.01

/**
 * Format a fractional percentage as "X%" with one decimal when < 10%,
 * else zero decimals. Returns "—" when the total is zero so callers
 * never show "NaN%".
 */
export function formatBucketPct(bucketCost: number, total: number): string {
  if (total <= 0) return '—'
  const pct = (bucketCost / total) * 100
  return pct < 10 ? `${pct.toFixed(1)}%` : `${Math.round(pct)}%`
}

/**
 * Returns true when the sum of the four cost buckets differs from
 * `total_cost_usd` by more than COST_SUM_TOLERANCE_USD. Used to surface
 * a warning badge in the cost breakdown block.
 */
export function isCostSumMismatch(
  chat: number,
  agent: number,
  scorer: number,
  sideCall: number,
  total: number,
): boolean {
  const bucketSum = chat + agent + scorer + sideCall
  return Math.abs(bucketSum - total) > COST_SUM_TOLERANCE_USD
}

/**
 * Cost-breakdown block. Every Anthropic dollar flows through ONE of
 * four buckets — chat (UX shim),
 * agent (per-task), scorer (operator-triggered), side-call (Haiku hops
 * like auto-title). Each bucket shows its total + an indented row per
 * model that contributed. The agent row carries a billing-mode badge
 * — "notional" under subscription mode (the Claude Code CLI reports a
 * `total_cost_usd` in its JSON even when the subscription, not the API,
 * actually paid for it), "real" under api mode.
 */
export function CostBreakdownBlock({
  metrics,
  fetchedAt,
}: {
  metrics: SessionMetrics
  fetchedAt?: Date | null
}): JSX.Element {
  const chat = metrics.chat_cost_usd ?? metrics.total_cost_usd ?? 0
  const agent = metrics.agent_cost_usd ?? 0
  const scorer = metrics.scorer_cost_usd ?? 0
  const sideCall = metrics.side_call_cost_usd ?? 0
  const total = metrics.total_cost_usd ?? chat + agent + scorer + sideCall
  const billingMode = metrics.agent_billing_mode ?? 'subscription'
  const agentNotional = billingMode === 'subscription' && agent > 0

  const sumMismatch = isCostSumMismatch(chat, agent, scorer, sideCall, total)

  // Per-model breakdowns per bucket. We filter to nonzero entries so a
  // $0 bucket collapses to just the header. Chat uses the combined
  // `per_model_cost_usd` (which already excludes agent+scorer because
  // only chat turns feed it server-side).
  const chatByModel = nonZeroEntries(metrics.per_model_cost_usd, metrics.per_model_turns, 'turn')
  const agentByModel = nonZeroEntries(metrics.per_model_agent_cost_usd, undefined, 'task')
  const scorerByModel = nonZeroEntries(metrics.per_model_scorer_cost_usd, undefined, 'call')
  const sideCallByModel = nonZeroEntries(
    metrics.per_model_side_call_cost_usd,
    undefined,
    'call',
  )

  return (
    <section
      data-metric-block="cost-breakdown"
      aria-label="Cost breakdown"
      style={{
        marginTop: '1rem',
        padding: '0.75rem',
        background: 'var(--color-surface-0)',
        border: '1px solid var(--color-border-default)',
        borderRadius: '0.25rem',
      }}
    >
      <div
        style={{
          display: 'flex',
          justifyContent: 'space-between',
          alignItems: 'baseline',
          marginBottom: '0.5rem',
        }}
      >
        <span
          style={{
            fontSize: '0.83rem',
            fontWeight: 600,
            color: 'var(--color-text-primary)',
          }}
        >
          Cost breakdown
        </span>
        {fetchedAt && (
          <span
            style={{
              fontSize: '0.68rem',
              color: 'var(--color-text-faint)',
              fontStyle: 'italic',
            }}
          >
            fetched {fetchedAt.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })}
          </span>
        )}
      </div>
      <table
        aria-label="Cost breakdown by bucket"
        style={{ width: '100%', borderCollapse: 'collapse', fontSize: '0.83rem' }}
      >
        <tbody>
          <CostBucketRow
            label="Chat (Sonnet/Opus turns)"
            cost={chat}
            pct={formatBucketPct(chat, total)}
            ariaLabel={`Chat cost: ${formatUSD(chat, { precision: 4 })}, ${formatBucketPct(chat, total)} of total`}
          />
          <TokenSurfaceRow
            bucket="chat"
            tokens={metrics.per_surface_tokens?.chat}
          />
          {chatByModel.map((r) => (
            <CostModelSubRow key={`chat-${r.modelKey}`} bucket="chat" {...r} />
          ))}
          <CostBucketRow
            label="Agent (task execution, claude CLI)"
            cost={agent}
            pct={formatBucketPct(agent, total)}
            ariaLabel={`Agent cost: ${formatUSD(agent, { precision: 4 })}, ${formatBucketPct(agent, total)} of total`}
            badge={
              agentNotional
                ? { text: 'notional', tone: 'info' }
                : agent > 0
                  ? { text: 'real (api)', tone: 'warn' }
                  : undefined
            }
          />
          <TokenSurfaceRow
            bucket="agent"
            tokens={metrics.per_surface_tokens?.agent}
          />
          {agentByModel.map((r) => (
            <CostModelSubRow key={`agent-${r.modelKey}`} bucket="agent" {...r} />
          ))}
          <CostBucketRow
            label="Scorer (rubric LLM-judge)"
            cost={scorer}
            pct={formatBucketPct(scorer, total)}
            ariaLabel={`Scorer cost: ${formatUSD(scorer, { precision: 4 })}, ${formatBucketPct(scorer, total)} of total`}
          />
          <TokenSurfaceRow
            bucket="scorer"
            tokens={metrics.per_surface_tokens?.scorer}
          />
          {scorerByModel.map((r) => (
            <CostModelSubRow key={`scorer-${r.modelKey}`} bucket="scorer" {...r} />
          ))}
          <CostBucketRow
            label="Side-calls (auto-title, remediation)"
            cost={sideCall}
            pct={formatBucketPct(sideCall, total)}
            ariaLabel={`Side-call cost: ${formatUSD(sideCall, { precision: 4 })}, ${formatBucketPct(sideCall, total)} of total`}
          />
          <TokenSurfaceRow
            bucket="side_call"
            tokens={metrics.per_surface_tokens?.side_call}
          />
          {sideCallByModel.map((r) => (
            <CostModelSubRow key={`sidecall-${r.modelKey}`} bucket="side_call" {...r} />
          ))}
          <tr
            data-metric-row="cost-total"
            style={{ borderTop: '2px solid var(--color-border-strong)' }}
          >
            <th
              scope="row"
              style={{
                textAlign: 'left',
                padding: '0.5rem 0.5rem 0.5rem 0',
                fontWeight: 600,
                color: 'var(--color-text-primary)',
              }}
            >
              Total
              {agentNotional && (
                <span style={{ fontWeight: 400, fontSize: '0.72rem', color: 'var(--color-text-muted)' }}>
                  {' '}(includes notional agent cost)
                </span>
              )}
            </th>
            <td
              style={{
                textAlign: 'right',
                padding: '0.5rem 0',
                fontFamily: 'ui-monospace, monospace',
                fontWeight: 600,
                color: 'var(--color-text-primary)',
              }}
            >
              {formatUSD(total, { precision: 4 })}
              {sumMismatch && (
                <span
                  data-cost-sum-mismatch="true"
                  role="img"
                  aria-label="Cost bucket sum does not match total"
                  title={`Bucket sum differs from reported total by more than $${COST_SUM_TOLERANCE_USD.toFixed(2)}`}
                  style={{
                    marginLeft: '0.4rem',
                    color: 'var(--color-warning-accent)',
                    fontSize: '0.8em',
                  }}
                >
                  ⚠
                </span>
              )}
            </td>
          </tr>
        </tbody>
      </table>
    </section>
  )
}

/**
 * Per-stage-class agent cost rollup. Derived entirely from F1's
 * `per_task_agent` snapshot data: groups task costs by their
 * `stage_class` field (populated server-side from the DAG). Hides
 * when no tasks carry a `stage_class` (e.g. legacy taxonomy or fresh
 * session). Useful for spotting which phase of the pipeline ate the
 * spend (preprocessing vs DE vs interpretation).
 */
export function StageClassRollupBlock({
  perTask,
}: {
  perTask: PerTaskAgentSnapshot[]
}): JSX.Element | null {
  const tagged = perTask.filter((t) => t.stage_class)
  if (tagged.length === 0) return null
  const byStage = tagged.reduce<Record<string, number>>((acc, t) => {
    const k = t.stage_class!
    acc[k] = (acc[k] ?? 0) + t.cost_usd
    return acc
  }, {})
  const entries = Object.entries(byStage).sort(([, a], [, b]) => b - a)
  return (
    <section
      data-metric-block="stage-class-rollup"
      aria-label="Agent cost by stage class"
      style={{
        marginTop: '1rem',
        padding: '0.75rem',
        background: 'var(--color-surface-0)',
        border: '1px solid var(--color-border-default)',
        borderRadius: '0.25rem',
      }}
    >
      <div
        style={{
          fontSize: '0.83rem',
          fontWeight: 600,
          color: 'var(--color-text-primary)',
          marginBottom: '0.5rem',
        }}
      >
        Agent cost by stage class
      </div>
      <table
        aria-label="Agent cost rollup by stage class"
        style={{ width: '100%', borderCollapse: 'collapse', fontSize: '0.78rem' }}
      >
        <tbody>
          {entries.map(([stage, cost]) => (
            <tr
              key={stage}
              data-metric-row="stage-class"
              data-stage-class={stage}
              style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
            >
              <th
                scope="row"
                style={{
                  textAlign: 'left',
                  padding: '0.3rem 0.5rem 0.3rem 0',
                  fontWeight: 400,
                  color: 'var(--color-text-secondary)',
                  fontFamily: 'ui-monospace, monospace',
                }}
              >
                {stage}
              </th>
              <td
                style={{
                  textAlign: 'right',
                  padding: '0.3rem 0',
                  color: 'var(--color-text-primary)',
                  fontFamily: 'ui-monospace, monospace',
                }}
              >
                {formatUSD(cost, { precision: 4 })}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  )
}

/**
 * Tool-call breakdown by name. Surfaces the per-tool-name
 * counter `tool_calls_by_name` so operators can spot runaway tool
 * loops (e.g. the LLM calling `classify_intake` 20 times in one turn).
 * Sorted by count descending. Hides entirely when the map is empty.
 */
export function ToolCallsBlock({
  byName,
}: {
  byName: Record<string, number>
}): JSX.Element | null {
  const entries = Object.entries(byName)
    .filter(([, n]) => n > 0)
    .sort(([a, ax], [b, bx]) => bx - ax || a.localeCompare(b))
  if (entries.length === 0) return null
  return (
    <section
      data-metric-block="tool-calls-by-name"
      aria-label="Tool calls by name"
      style={{
        marginTop: '1rem',
        padding: '0.75rem',
        background: 'var(--color-surface-0)',
        border: '1px solid var(--color-border-default)',
        borderRadius: '0.25rem',
      }}
    >
      <div
        style={{
          fontSize: '0.83rem',
          fontWeight: 600,
          color: 'var(--color-text-primary)',
          marginBottom: '0.5rem',
        }}
      >
        Tool calls by name
      </div>
      <table
        aria-label="Tool calls by name table"
        style={{ width: '100%', borderCollapse: 'collapse', fontSize: '0.78rem' }}
      >
        <tbody>
          {entries.map(([name, count]) => (
            <tr
              key={name}
              data-metric-row="tool-call"
              data-tool-name={name}
              style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
            >
              <th
                scope="row"
                style={{
                  textAlign: 'left',
                  padding: '0.3rem 0.5rem 0.3rem 0',
                  fontWeight: 400,
                  color: 'var(--color-text-secondary)',
                  fontFamily: 'ui-monospace, monospace',
                }}
              >
                {name}
              </th>
              <td
                style={{
                  textAlign: 'right',
                  padding: '0.3rem 0',
                  color: 'var(--color-text-primary)',
                  fontFamily: 'ui-monospace, monospace',
                }}
              >
                {count}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  )
}

/**
 * Per-task agent cost breakdown. Renders a table of every task
 * the agent has reported usage for, sorted by cost descending (the
 * snapshot already arrives sorted; we render in receive order). Hides
 * entirely when no agent tasks have completed yet (e.g. pre-emit chat).
 *
 * Columns: task id · model · output tokens · cache reads · cost. Cost
 * is the dominant operator-relevant figure so it sits last (right-
 * aligned) in line with the cost-breakdown convention. Stage class
 * (F4) renders as a small chip after the task id when present.
 */
export function PerTaskBreakdownBlock({
  perTask,
}: {
  perTask: PerTaskAgentSnapshot[]
}): JSX.Element | null {
  if (perTask.length === 0) return null
  const fmtTokens = (n: number) => {
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(n >= 10_000_000 ? 0 : 1)}M`
    if (n >= 1_000) return `${(n / 1_000).toFixed(n >= 10_000 ? 0 : 1)}k`
    return n.toString()
  }
  return (
    <section
      data-metric-block="per-task-agent"
      aria-label="Per-task agent cost breakdown"
      style={{
        marginTop: '1rem',
        padding: '0.75rem',
        background: 'var(--color-surface-0)',
        border: '1px solid var(--color-border-default)',
        borderRadius: '0.25rem',
      }}
    >
      <div
        style={{
          fontSize: '0.83rem',
          fontWeight: 600,
          color: 'var(--color-text-primary)',
          marginBottom: '0.5rem',
        }}
      >
        Per-task agent cost
      </div>
      <table
        aria-label="Agent cost per task"
        style={{ width: '100%', borderCollapse: 'collapse', fontSize: '0.78rem' }}
      >
        <thead>
          <tr style={{ borderBottom: '1px solid var(--color-border-strong)' }}>
            <th
              style={{
                textAlign: 'left',
                padding: '0.3rem 0.5rem 0.3rem 0',
                fontWeight: 600,
                color: 'var(--color-text-secondary)',
              }}
            >
              Task
            </th>
            <th
              style={{
                textAlign: 'left',
                padding: '0.3rem 0.5rem',
                fontWeight: 600,
                color: 'var(--color-text-secondary)',
              }}
            >
              Model
            </th>
            <th
              style={{
                textAlign: 'right',
                padding: '0.3rem 0.5rem',
                fontWeight: 600,
                color: 'var(--color-text-secondary)',
              }}
            >
              Out
            </th>
            <th
              style={{
                textAlign: 'right',
                padding: '0.3rem 0.5rem',
                fontWeight: 600,
                color: 'var(--color-text-secondary)',
              }}
            >
              Cache R
            </th>
            <th
              style={{
                textAlign: 'right',
                padding: '0.3rem 0',
                fontWeight: 600,
                color: 'var(--color-text-secondary)',
              }}
            >
              Cost
            </th>
          </tr>
        </thead>
        <tbody>
          {perTask.map((t) => (
            <tr
              key={t.task_id}
              data-metric-row="per-task-agent"
              data-task-id={t.task_id}
              style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
            >
              <td
                style={{
                  textAlign: 'left',
                  padding: '0.25rem 0.5rem 0.25rem 0',
                  color: 'var(--color-text-primary)',
                  fontFamily: 'ui-monospace, monospace',
                }}
              >
                {t.task_id}
                {t.stage_class && (
                  <span
                    data-stage-class={t.stage_class}
                    style={{
                      marginLeft: '0.4rem',
                      padding: '0.05rem 0.3rem',
                      fontSize: '0.65rem',
                      borderRadius: '0.2rem',
                      background: 'var(--color-surface-3)',
                      color: 'var(--color-text-secondary)',
                      fontFamily: 'inherit',
                    }}
                  >
                    {t.stage_class}
                  </span>
                )}
              </td>
              <td
                style={{
                  textAlign: 'left',
                  padding: '0.25rem 0.5rem',
                  color: 'var(--color-text-secondary)',
                  fontFamily: 'ui-monospace, monospace',
                }}
              >
                {t.model}
              </td>
              <td
                style={{
                  textAlign: 'right',
                  padding: '0.25rem 0.5rem',
                  color: 'var(--color-text-secondary)',
                  fontFamily: 'ui-monospace, monospace',
                }}
              >
                {fmtTokens(t.output_tokens)}
              </td>
              <td
                style={{
                  textAlign: 'right',
                  padding: '0.25rem 0.5rem',
                  color: 'var(--color-text-secondary)',
                  fontFamily: 'ui-monospace, monospace',
                }}
              >
                {fmtTokens(t.cache_read_tokens)}
              </td>
              <td
                style={{
                  textAlign: 'right',
                  padding: '0.25rem 0',
                  color: 'var(--color-text-primary)',
                  fontWeight: 500,
                  fontFamily: 'ui-monospace, monospace',
                }}
              >
                {formatUSD(t.cost_usd, { precision: 4 })}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  )
}

interface ModelRow {
  modelKey: string
  cost: number
  unitCount?: number
  unitLabel: string
}

function nonZeroEntries(
  costMap: Record<string, number> | undefined,
  unitMap: Record<string, number> | undefined,
  unitLabel: string,
): ModelRow[] {
  if (!costMap) return []
  return Object.entries(costMap)
    .filter(([, cost]) => cost > 0)
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([modelKey, cost]) => ({
      modelKey,
      cost,
      unitCount: unitMap?.[modelKey],
      unitLabel,
    }))
}

function CostBucketRow({
  label,
  cost,
  pct,
  ariaLabel,
  badge,
}: {
  label: string
  cost: number
  /** Pre-formatted percent-of-total string, e.g. "34%" or "3.2%". */
  pct?: string
  /** Accessible description for screen readers. */
  ariaLabel?: string
  badge?: { text: string; tone: 'info' | 'warn' }
}): JSX.Element {
  return (
    <tr
      data-metric-row="cost-bucket"
      style={{ borderBottom: '1px solid var(--color-border-default)' }}
      aria-label={ariaLabel}
    >
      <th
        scope="row"
        style={{
          textAlign: 'left',
          padding: '0.4rem 0.5rem 0.4rem 0',
          fontWeight: 500,
          color: 'var(--color-text-secondary)',
        }}
      >
        {label}
        {badge && (
          <span
            data-cost-badge={badge.text}
            style={{
              marginLeft: '0.5rem',
              padding: '0.05rem 0.4rem',
              fontSize: '0.68rem',
              fontWeight: 600,
              borderRadius: '0.2rem',
              textTransform: 'uppercase',
              letterSpacing: '0.02em',
              background:
                badge.tone === 'info'
                  ? 'var(--color-info-bg)'
                  : 'var(--color-warning-bg)',
              color:
                badge.tone === 'info'
                  ? 'var(--color-info-fg)'
                  : 'var(--color-warning-fg)',
            }}
          >
            {badge.text}
          </span>
        )}
      </th>
      <td
        style={{
          textAlign: 'right',
          padding: '0.4rem 0',
          fontFamily: 'ui-monospace, monospace',
          color: 'var(--color-text-primary)',
        }}
      >
        {formatUSD(cost, { precision: 4 })}
        {pct && (
          <span
            data-cost-pct="true"
            style={{
              marginLeft: '0.5rem',
              fontSize: '0.78rem',
              color: 'var(--color-text-muted)',
            }}
          >
            {pct}
          </span>
        )}
      </td>
    </tr>
  )
}

/**
 * Compact one-line token-count row under a cost bucket header. Renders
 * `in 3.8k · out 15.2k · cr 128k · cw 28k` when any of the four counts
 * is nonzero; hides entirely on an empty bucket. Uses k/M shortening so
 * the line fits inside the narrow Performance pane on a 1024-wide split.
 */
function TokenSurfaceRow({
  bucket,
  tokens,
}: {
  bucket: string
  tokens?: TokenBucket
}): JSX.Element | null {
  if (!tokens) return null
  const { input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens } = tokens
  const anyNonZero =
    input_tokens > 0 ||
    output_tokens > 0 ||
    cache_read_tokens > 0 ||
    cache_creation_tokens > 0
  if (!anyNonZero) return null
  const fmt = (n: number) => {
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(n >= 10_000_000 ? 0 : 1)}M`
    if (n >= 1_000) return `${(n / 1_000).toFixed(n >= 10_000 ? 0 : 1)}k`
    return n.toString()
  }
  return (
    <tr
      data-metric-row="surface-tokens"
      data-cost-bucket={bucket}
      style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
    >
      <th
        scope="row"
        colSpan={2}
        style={{
          textAlign: 'left',
          padding: '0.15rem 0 0.3rem 1.25rem',
          fontWeight: 400,
          color: 'var(--color-text-muted)',
          fontSize: '0.72rem',
          fontFamily: 'ui-monospace, monospace',
        }}
      >
        in {fmt(input_tokens)} · out {fmt(output_tokens)} · cr{' '}
        {fmt(cache_read_tokens)} · cw {fmt(cache_creation_tokens)}
      </th>
    </tr>
  )
}

function CostModelSubRow({
  bucket,
  modelKey,
  cost,
  unitCount,
  unitLabel,
}: ModelRow & { bucket: string }): JSX.Element {
  const label = unitCount != null
    ? `${modelKey} · ${unitCount} ${unitLabel}${unitCount === 1 ? '' : 's'}`
    : modelKey
  return (
    <tr
      data-metric-row="cost-model"
      data-cost-bucket={bucket}
      data-model-key={modelKey}
      style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
    >
      <th
        scope="row"
        style={{
          textAlign: 'left',
          padding: '0.3rem 0.5rem 0.3rem 1.25rem',
          fontWeight: 400,
          color: 'var(--color-text-muted)',
          fontSize: '0.78rem',
        }}
      >
        {label}
      </th>
      <td
        style={{
          textAlign: 'right',
          padding: '0.3rem 0',
          fontFamily: 'ui-monospace, monospace',
          color: 'var(--color-text-secondary)',
          fontSize: '0.78rem',
        }}
      >
        {formatUSD(cost, { precision: 4 })}
      </td>
    </tr>
  )
}

/**
 * F3 helper — convert a cumulative cost into a per-hour rate based on
 * elapsed session-duration seconds. Returns 0 when duration is too
 * short to project (caller already gates on > 60s).
 */
function ratePerHour(cost: number, durationSeconds: number): number {
  if (durationSeconds <= 0) return 0
  return cost * (3600 / durationSeconds)
}

/**
 * F3 helper — humanize an elapsed-seconds duration as `Xm Ys` or
 * `Xh Ym`. Used in the Session-duration row.
 */
function formatDuration(secs: number): string {
  if (secs < 60) return `${secs}s`
  const m = Math.floor(secs / 60)
  if (m < 60) return `${m}m ${secs % 60}s`
  const h = Math.floor(m / 60)
  return `${h}h ${m % 60}m`
}

/**
 * F3 helper — humanize tokens-per-hour as e.g. `~450k` or `~1.2M`.
 * Used in the Input-token-burn row.
 */
function formatTokensPerHour(totalInput: number, durationSeconds: number): string {
  if (durationSeconds <= 0) return '0'
  const perHour = totalInput * (3600 / durationSeconds)
  if (perHour >= 1_000_000) return `~${(perHour / 1_000_000).toFixed(1)}M`
  if (perHour >= 1_000) return `~${Math.round(perHour / 1_000)}k`
  return `~${Math.round(perHour)}`
}

/**
 * Render the cache savings as an inline suffix to the
 * `Cache hit ratio` row. Returns an empty string when there are no
 * savings to report (no cache reads yet), so the ratio cell stays
 * uncluttered on fresh sessions.
 */
function formatCacheSavingsSuffix(savings: number): string {
  if (savings <= 0) return ''
  return `  (saved ${formatUSD(savings, { precision: 4 })} vs uncached)`
}

/**
 * §4.5 — Render the cache hit ratio as a percentage. Flags visibly low
 * ratios on warm sessions (10+ turns) where caching should have kicked
 * in but didn't — the characteristic silent-cache-invalidation pattern.
 */
function formatCacheHitRatio(ratio: number, turnCount: number): string {
  if (!Number.isFinite(ratio) || ratio <= 0) {
    return turnCount === 0 ? '— (no turns yet)' : '0% ⚠'
  }
  const pct = (ratio * 100).toFixed(0)
  if (turnCount >= 10 && ratio < 0.5) {
    return `${pct}% ⚠ (cache may be fragmenting)`
  }
  return `${pct}%`
}

function PilotSummary({ report }: { report: unknown }): JSX.Element {
  if (!report || typeof report !== 'object') {
    return <span style={{ fontStyle: 'italic', color: 'var(--color-text-muted)' }}>complete</span>
  }
  const r = report as {
    measurements?: unknown[]
    confidence?: number
    projected_requirements?: Record<string, unknown>
  }
  const n = Array.isArray(r.measurements) ? r.measurements.length : 0
  const conf = typeof r.confidence === 'number' ? r.confidence : null
  const projected = r.projected_requirements ?? {}
  const projectedCount = typeof projected === 'object' ? Object.keys(projected).length : 0
  return (
    <span>
      {n} measurement{n === 1 ? '' : 's'}
      {conf !== null ? ` · confidence ${(conf * 100).toFixed(0)}%` : ''}
      {projectedCount > 0
        ? ` · ${projectedCount} stage projection${projectedCount === 1 ? '' : 's'}`
        : ''}
    </span>
  )
}

/**
 * Affordance fallback section in the Performance tab.
 * Renders the `CatalogGapsCard` when there are any structural-fallback
 * entries in the session metrics. The "Describe a plot" CTA per gap row
 * opens a `RendererProposalCard` in an inline overlay.
 *
 * `sessionId` is needed to POST the renderer proposal. When absent the
 * Propose button surfaces but is effectively disabled (the API call will
 * fail with a 404; the error appears inline in the proposal form).
 */
function AffordanceFallbacksSection({
  fallbacks,
  sessionId,
}: {
  fallbacks: AffordanceFallbackSummary[]
  sessionId: string | null
}): JSX.Element | null {
  const [proposalTarget, setProposalTarget] = useState<{
    semanticType: string
    primitive: string
  } | null>(null)

  if (fallbacks.length === 0) return null

  return (
    <>
      <CatalogGapsCard
        fallbacks={fallbacks}
        onSuggestRenderer={(semanticType, primitive) =>
          setProposalTarget({ semanticType, primitive })
        }
      />
      {proposalTarget && (
        <div
          role="dialog"
          aria-label="Describe a preferred plot overlay"
          style={{
            marginTop: '0.75rem',
          }}
        >
          <RendererProposalCard
            sessionId={sessionId ?? ''}
            targetSemanticType={proposalTarget.semanticType}
            primitiveBasis={proposalTarget.primitive}
            availableParentTerms={[]}
            onAccepted={(_proposalId) => setProposalTarget(null)}
            onCancel={() => setProposalTarget(null)}
          />
        </div>
      )}
    </>
  )
}

/**
 * Value-add evaluation scorecard tile. Renders the latest per-tier
 * results from `runtime/value-add-metrics.jsonl` polled every 4s by
 * `MetricsTable`. Shows an empty state prompt when no data is available
 * (pre-emit session or file not yet written).
 *
 * Columns: Tier · Bucket · Score · Threshold · Pass
 * Pass column uses "PASS" / "FAIL" text (no glyphs per CLAUDE.md).
 */
export function ValueAddTile({
  valueAdd,
}: {
  valueAdd: ValueAddMetricsResponse | null
}): JSX.Element {
  return (
    <section
      data-metric-block="value-add"
      aria-label="Value-add evaluation scorecard"
      style={{
        marginTop: '1rem',
        padding: '0.75rem',
        background: 'var(--color-surface-0)',
        border: '1px solid var(--color-border-default)',
        borderRadius: '0.25rem',
      }}
    >
      <div
        style={{
          fontSize: '0.83rem',
          fontWeight: 600,
          color: 'var(--color-text-primary)',
          marginBottom: '0.5rem',
        }}
      >
        Value-add evaluation
      </div>
      {valueAdd === null ? (
        <p
          style={{
            fontSize: '0.78rem',
            color: 'var(--color-text-muted)',
            fontStyle: 'italic',
            margin: 0,
          }}
        >
          No value-add metrics yet — run{' '}
          <code
            style={{
              fontFamily: 'ui-monospace, monospace',
              fontSize: '0.75rem',
              background: 'var(--color-surface-2)',
              padding: '0.1rem 0.25rem',
              borderRadius: '0.2rem',
            }}
          >
            make value-add-eval-all
          </code>{' '}
          after a session.
        </p>
      ) : valueAdd.tier_results.length === 0 ? (
        <p
          style={{
            fontSize: '0.78rem',
            color: 'var(--color-text-muted)',
            fontStyle: 'italic',
            margin: 0,
          }}
        >
          Metrics file exists but contains no tier rows yet.
        </p>
      ) : (
        <table
          aria-label="Value-add tier scorecard"
          style={{ width: '100%', borderCollapse: 'collapse', fontSize: '0.78rem' }}
        >
          <thead>
            <tr style={{ borderBottom: '1px solid var(--color-border-strong)' }}>
              <th
                style={{
                  textAlign: 'left',
                  padding: '0.3rem 0.5rem 0.3rem 0',
                  fontWeight: 600,
                  color: 'var(--color-text-secondary)',
                }}
              >
                Tier
              </th>
              <th
                style={{
                  textAlign: 'left',
                  padding: '0.3rem 0.5rem',
                  fontWeight: 600,
                  color: 'var(--color-text-secondary)',
                }}
              >
                Bucket
              </th>
              <th
                style={{
                  textAlign: 'right',
                  padding: '0.3rem 0.5rem',
                  fontWeight: 600,
                  color: 'var(--color-text-secondary)',
                }}
              >
                Score
              </th>
              <th
                style={{
                  textAlign: 'right',
                  padding: '0.3rem 0.5rem',
                  fontWeight: 600,
                  color: 'var(--color-text-secondary)',
                }}
              >
                Threshold
              </th>
              <th
                style={{
                  textAlign: 'right',
                  padding: '0.3rem 0',
                  fontWeight: 600,
                  color: 'var(--color-text-secondary)',
                }}
              >
                Pass
              </th>
            </tr>
          </thead>
          <tbody>
            {valueAdd.tier_results.map((t: TierResult) => (
              <tr
                key={t.tier}
                data-metric-row="value-add-tier"
                data-tier={t.tier}
                data-bucket={t.bucket}
                style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
              >
                <td
                  style={{
                    textAlign: 'left',
                    padding: '0.25rem 0.5rem 0.25rem 0',
                    color: 'var(--color-text-primary)',
                    fontFamily: 'ui-monospace, monospace',
                  }}
                >
                  {t.tier}
                </td>
                <td
                  style={{
                    textAlign: 'left',
                    padding: '0.25rem 0.5rem',
                    color: 'var(--color-text-secondary)',
                    fontFamily: 'ui-monospace, monospace',
                  }}
                >
                  {t.bucket}
                </td>
                <td
                  style={{
                    textAlign: 'right',
                    padding: '0.25rem 0.5rem',
                    color: 'var(--color-text-primary)',
                    fontFamily: 'ui-monospace, monospace',
                  }}
                >
                  {t.score.toFixed(3)}
                </td>
                <td
                  style={{
                    textAlign: 'right',
                    padding: '0.25rem 0.5rem',
                    color: 'var(--color-text-secondary)',
                    fontFamily: 'ui-monospace, monospace',
                  }}
                >
                  {t.threshold.toFixed(3)}
                </td>
                <td
                  style={{
                    textAlign: 'right',
                    padding: '0.25rem 0',
                    fontWeight: 600,
                    fontFamily: 'ui-monospace, monospace',
                    color: t.passed
                      ? 'var(--color-success-fg)'
                      : 'var(--color-danger-fg)',
                  }}
                >
                  {t.passed ? 'PASS' : 'FAIL'}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  )
}
