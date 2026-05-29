// MetricsTable compute-usage rendering tests. All three row categories
// (instance hours, per-type sub-rows, high-water resizes) hide themselves
// when their counter is zero so the table stays quiet on local-only
// sessions.

import { describe, expect, it } from 'vitest'
import { render, screen } from '@testing-library/react'
import axe from 'axe-core'

import { DocumentsPane, JobsFeed, MetricsTable } from './StateInspectorPane'
import type { SessionMetrics, SessionStateSnapshot } from '../api/chatClient'
import type {
  HarnessProgressLine,
  StallSignalEvent,
} from '../hooks/useSseChatEvents'

function makeMetrics(overrides: Partial<SessionMetrics> = {}): SessionMetrics {
  return {
    turn_count: 1,
    tool_call_count: 0,
    total_input_tokens: 0,
    total_output_tokens: 0,
    cache_read_tokens: 0,
    cache_creation_tokens: 0,
    p50_turn_ms: 0,
    p95_turn_ms: 0,
    p99_turn_ms: 0,
    mean_turn_ms: 0,
    max_turn_ms: 0,
    sonnet_turns: 1,
    opus_turns: 0,
    per_model_turns: {},
    per_model_cost_usd: {},
    total_instance_seconds: 0,
    instance_type_seconds: {},
    high_water_exceeded_count: 0,
    total_cost_usd: 0,
    chat_cost_usd: 0,
    agent_cost_usd: 0,
    per_model_agent_cost_usd: {},
    scorer_cost_usd: 0,
    per_model_scorer_cost_usd: {},
    sonnet_cost_usd: 0,
    opus_cost_usd: 0,
    cache_hit_ratio: 0,
    tool_loop_iterations_histogram: {},
    opus_escalation_reasons: {},
    ...overrides,
  }
}

describe('MetricsTable — compute-usage rows', () => {
  it('hides Instance hours row entirely when total_instance_seconds is 0', () => {
    render(<MetricsTable metrics={makeMetrics()} />)
    // No "Instance hours" label should be in the DOM.
    expect(screen.queryByText('Instance hours')).toBeNull()
    // And no instance-hours data row marker should be rendered.
    expect(
      document.querySelector('[data-metric-row="instance_hours"]'),
    ).toBeNull()
  })

  it('renders Instance hours + sub-rows when total_instance_seconds > 0', () => {
    // 7200 s total (2 h), split evenly between two instance types.
    render(
      <MetricsTable
        metrics={makeMetrics({
          total_instance_seconds: 7200,
          instance_type_seconds: {
            'r6i.4xlarge': 3600,
            'g6.xlarge': 3600,
          },
        })}
      />,
    )

    // Aggregate row.
    const instanceHoursRow = document.querySelector(
      '[data-metric-row="instance_hours"]',
    )
    expect(instanceHoursRow).not.toBeNull()
    expect(instanceHoursRow?.textContent).toContain('Instance hours')
    expect(instanceHoursRow?.textContent).toContain('2.00')

    // Per-type sub-rows — both must be present.
    const typeRows = document.querySelectorAll(
      '[data-metric-row="instance_type"]',
    )
    expect(typeRows.length).toBe(2)

    // Sub-rows must be alphabetically sorted by instance type:
    // g6.xlarge < r6i.4xlarge.
    expect(typeRows![0]!.getAttribute('data-instance-type')).toBe('g6.xlarge')
    expect(typeRows![1]!.getAttribute('data-instance-type')).toBe('r6i.4xlarge')
    expect(typeRows![0]!.textContent).toContain('g6.xlarge')
    expect(typeRows![0]!.textContent).toContain('1.00 h')
    expect(typeRows![1]!.textContent).toContain('r6i.4xlarge')
    expect(typeRows![1]!.textContent).toContain('1.00 h')
  })

  it('renders chat + agent cost rows alongside total cost', () => {
    // Bundle D: chat LLM spend and agent execution spend show up as
    // separate lines so the SME can see where the money went. Both
    // are always rendered (even when zero) — the total sits below.
    render(
      <MetricsTable
        metrics={makeMetrics({
          chat_cost_usd: 0.5,
          agent_cost_usd: 1.25,
          total_cost_usd: 1.75,
        })}
      />,
    )
    // Cost rows now live inside the CostBreakdownBlock section, each
    // bucket as its own row, with a Total row underneath.
    expect(screen.getByText('Chat (Sonnet/Opus turns)')).toBeInTheDocument()
    expect(screen.getByText('$0.5000')).toBeInTheDocument()
    expect(screen.getByText('Agent (task execution, claude CLI)')).toBeInTheDocument()
    expect(screen.getByText('$1.2500')).toBeInTheDocument()
    // Total row carries the $1.7500 figure; scope to the cost-total row
    // so a future row with the same dollar value can't match.
    const totalRow = document.querySelector('[data-metric-row="cost-total"]')
    expect(totalRow).not.toBeNull()
    expect(totalRow?.textContent).toContain('$1.7500')
  })

  it('hides per-model breakdown rows when per_model_cost_usd only has Sonnet/Opus', () => {
    // Sonnet + Opus are already surfaced by the flat sonnet_turns /
    // opus_turns rows, so the per-model breakdown should only render
    // *additional* models (Haiku, future variants). Passing just the
    // known flat keys must not produce any per_model rows.
    render(
      <MetricsTable
        metrics={makeMetrics({
          per_model_turns: { sonnet_4_6: 2, opus_4_6: 1 },
          per_model_cost_usd: { sonnet_4_6: 0.5, opus_4_6: 1.2 },
        })}
      />,
    )
    expect(
      document.querySelectorAll('[data-metric-row="per_model"]').length,
    ).toBe(0)
  })

  it('renders a per-model row for each non-Sonnet-non-Opus model that served a turn', () => {
    // A session that routed Haiku (budget path) gets an extra row with
    // the key + turn count + cost. Keys are alphabetised so the render
    // order is stable for screenshot tests and axe scans.
    render(
      <MetricsTable
        metrics={makeMetrics({
          per_model_turns: { sonnet_4_6: 3, haiku_4_5: 2 },
          per_model_cost_usd: { sonnet_4_6: 0.9, haiku_4_5: 0.0042 },
        })}
      />,
    )
    const rows = document.querySelectorAll('[data-metric-row="per_model"]')
    expect(rows.length).toBe(1)
    expect(rows![0]!.getAttribute('data-model-key')).toBe('haiku_4_5')
    expect(rows![0]!.textContent).toContain('haiku_4_5')
    expect(rows![0]!.textContent).toContain('2 turns')
    expect(rows![0]!.textContent).toContain('$0.0042')
  })

  it('hides High-water resizes row entirely when count is 0', () => {
    render(<MetricsTable metrics={makeMetrics()} />)
    expect(screen.queryByText('High-water resizes')).toBeNull()
    expect(
      document.querySelector('[data-metric-row="high_water_resizes"]'),
    ).toBeNull()
  })

  it('renders High-water resizes row with the count when > 0', () => {
    render(
      <MetricsTable metrics={makeMetrics({ high_water_exceeded_count: 3 })} />,
    )
    const row = document.querySelector(
      '[data-metric-row="high_water_resizes"]',
    )
    expect(row).not.toBeNull()
    expect(row?.textContent).toContain('High-water resizes')
    expect(row?.textContent).toContain('3')
  })

  it('renders the placeholder when metrics is null', () => {
    render(<MetricsTable metrics={null} />)
    expect(screen.getByText(/Metrics will appear here/i)).toBeInTheDocument()
  })

  it('preserves accessibility contract: table has aria-label and <th scope="row">', async () => {
    // Render with a realistic mix of compute and per-turn metrics so
    // the axe scan covers every new row variant introduced by this PR.
    const { container } = render(
      <MetricsTable
        metrics={makeMetrics({
          turn_count: 4,
          tool_call_count: 7,
          mean_turn_ms: 312,
          total_instance_seconds: 7200,
          instance_type_seconds: {
            'r6i.4xlarge': 3600,
            'g6.xlarge': 3600,
          },
          high_water_exceeded_count: 2,
        })}
      />,
    )

    // The aria-label on the table survives all row-structure edits.
    const table = screen.getByRole('table', { name: /per-session metrics/i })
    expect(table).toBeInTheDocument()

    // Every new data row keeps its <th scope="row"> header cell.
    const instanceHoursHeader = container.querySelector(
      '[data-metric-row="instance_hours"] th[scope="row"]',
    )
    expect(instanceHoursHeader?.textContent).toBe('Instance hours')
    const perTypeHeaders = container.querySelectorAll(
      '[data-metric-row="instance_type"] th[scope="row"]',
    )
    expect(perTypeHeaders.length).toBe(2)
    const highWaterHeader = container.querySelector(
      '[data-metric-row="high_water_resizes"] th[scope="row"]',
    )
    expect(highWaterHeader?.textContent).toBe('High-water resizes')

    // Full axe-core WCAG 2.1 AA pass so the new rows don't introduce
    // regressions (color-contrast disabled — jsdom limitation, mirrors
    // the policy in src/test/axe.test.tsx).
    const results = await axe.run(container, {
      runOnly: {
        type: 'tag',
        values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'],
      },
      rules: { 'color-contrast': { enabled: false } },
    })
    expect(results.violations).toEqual([])
  })
})

// DocumentsPane — post-emit artifact listing

function makeSnapshot(overrides: Partial<SessionStateSnapshot> = {}): SessionStateSnapshot {
  return {
    session_id: '00000000-0000-0000-0000-000000000000',
    state: { kind: 'greeting' },
    user_confirmed: false,
    last_activity: new Date().toISOString(),
    task_count: 0,
    progress: { completed: 0, ready: 0, blocked: 0, pending: 0 },
    title: null,
    parent_session_id: null,
    blocked_tasks: [], pending_input_hints: [],
    ...overrides,
  }
}

describe('DocumentsPane — post-emit artifact listing', () => {
  it('renders the placeholder when the session has not emitted', () => {
    render(<DocumentsPane state={makeSnapshot()} sessionId={null} />)
    expect(
      screen.getByText(/documents.*appear here after confirmation/i),
    ).toBeInTheDocument()
    expect(screen.queryByLabelText('Package artifacts')).toBeNull()
  })

  it('renders the placeholder when emitted but no path is set', () => {
    render(
      <DocumentsPane
        state={makeSnapshot({
          state: { kind: 'emitted' },
          user_confirmed: true,
          emitted_package_path: undefined,
        })}
        sessionId="test-session"
      />,
    )
    expect(
      screen.getByText(/documents.*appear here after confirmation/i),
    ).toBeInTheDocument()
  })

  it('lists the four canonical artifacts and the package path when emitted', () => {
    const pkgDir = '/home/alan/.ecaa-workflow/packages/abc-bulk_rnaseq-20260417T120000'
    render(
      <DocumentsPane
        state={makeSnapshot({
          state: { kind: 'emitted' },
          user_confirmed: true,
          emitted_package_path: pkgDir,
        })}
        sessionId="test-session"
      />,
    )
    const artifactList = screen.getByLabelText('Package artifacts')
    expect(artifactList).toBeInTheDocument()
    expect(artifactList.textContent).toContain('WORKFLOW.json')
    expect(artifactList.textContent).toContain('PROMPT.md')
    expect(artifactList.textContent).toContain('CONTEXT.md')
    expect(artifactList.textContent).toContain('ro-crate-metadata.json')
    expect(screen.getByLabelText('Package directory').textContent).toBe(pkgDir)
  })
})

// Pilot row + JobsFeed stall chip

describe('MetricsTable — Pilot row', () => {
  it('renders Pilot row when pilot has status=complete + report', () => {
    render(
      <MetricsTable
        metrics={makeMetrics()}
        pilot={{
          status: 'complete',
          report: {
            measurements: [
              { task_id: 't1', peak_rss_mb: 500 },
              { task_id: 't2', peak_rss_mb: 700 },
              { task_id: 't3', peak_rss_mb: 900 },
            ],
            confidence: 0.82,
            projected_requirements: { align: {} },
          },
          skipReason: null,
        }}
      />,
    )
    const pilotRow = document.querySelector('[data-metric-row="pilot"]')
    expect(pilotRow).not.toBeNull()
    // Label cell.
    expect(pilotRow?.textContent).toContain('Pilot')
    // PilotSummary renders "3 measurements · confidence 82%" etc.
    expect(pilotRow?.textContent).toContain('3 measurement')
    expect(pilotRow?.textContent).toContain('82%')
  })

  it('renders nothing for Pilot when pilot prop is undefined', () => {
    render(<MetricsTable metrics={makeMetrics()} />)
    expect(document.querySelector('[data-metric-row="pilot"]')).toBeNull()
    expect(screen.queryByText(/^Pilot$/)).toBeNull()
  })

  it('renders nothing for Pilot when pilot.status is null (not yet started)', () => {
    render(
      <MetricsTable
        metrics={makeMetrics()}
        pilot={{ status: null, report: null, skipReason: null }}
      />,
    )
    expect(document.querySelector('[data-metric-row="pilot"]')).toBeNull()
  })
})

describe('JobsFeed — stall chip', () => {
  const progressEvent: HarnessProgressLine = {
    id: 'evt-1',
    kind: 'task_started',
    taskId: 'align-1',
    status: 'running',
    detail: 'alignment task',
    remote: null,
  }

  it('renders a stall chip when stallSignals carries an entry matching the event task id', () => {
    const stallSignals: Record<string, StallSignalEvent> = {
      'align-1': {
        taskId: 'align-1',
        signal: { kind: 'memory_pressure', pct: 93.4, window_mins: 5 },
        suggestedAction: 'resize',
      },
    }
    render(
      <JobsFeed
        events={[progressEvent]}
        stallSignals={stallSignals}
      />,
    )
    const chip = document.querySelector('[data-stall-chip="align-1"]')
    expect(chip).not.toBeNull()
    expect(chip?.textContent?.toLowerCase()).toContain('stall')
  })

  it('omits the stall chip when no stall signal targets the event task id', () => {
    render(
      <JobsFeed
        events={[progressEvent]}
        stallSignals={{
          'other-task': {
            taskId: 'other-task',
            signal: { kind: 'memory_pressure', pct: 92, window_mins: 5 },
            suggestedAction: 'resize',
          },
        }}
      />,
    )
    expect(document.querySelector('[data-stall-chip="align-1"]')).toBeNull()
  })

  it('renders without stall chips when stallSignals prop is omitted', () => {
    render(<JobsFeed events={[progressEvent]} />)
    expect(document.querySelector('[data-stall-chip]')).toBeNull()
  })
})

// Session input / budget progress bar. Renders only when the server
// surfaces session_token_budget; older builds /
// ECAA_SESSION_TOKEN_BUDGET=0 suppress the row entirely.
describe('MetricsTable — session token budget progress bar', () => {
  it('hides the budget row when session_token_budget is absent (legacy server)', () => {
    // Legacy metrics snapshots don't carry the field — the row must not render.
    render(<MetricsTable metrics={makeMetrics()} />)
    expect(
      document.querySelector('[data-metric-row="session_token_budget"]'),
    ).toBeNull()
  })

  it('hides the budget row when session_token_budget is null (budget disabled)', () => {
    render(
      <MetricsTable metrics={makeMetrics({ session_token_budget: null })} />,
    )
    expect(
      document.querySelector('[data-metric-row="session_token_budget"]'),
    ).toBeNull()
  })

  it('renders green tone with correct numbers below the 60% threshold', () => {
    render(
      <MetricsTable
        metrics={makeMetrics({
          total_input_tokens: 100_000,
          session_token_budget: 500_000,
        })}
      />,
    )
    const row = document.querySelector(
      '[data-metric-row="session_token_budget"]',
    )
    expect(row).not.toBeNull()
    expect(row?.textContent).toContain('Session token budget')
    expect(row?.textContent).toContain('100,000')
    expect(row?.textContent).toContain('500,000')
    expect(row?.textContent).toContain('(20%)')
    const bar = row?.querySelector('[role="progressbar"]')
    expect(bar?.getAttribute('data-tone')).toBe('ok')
    expect(bar?.getAttribute('aria-valuenow')).toBe('100000')
    expect(bar?.getAttribute('aria-valuemax')).toBe('500000')
  })

  it('flips to amber tone between 60% and 90%', () => {
    render(
      <MetricsTable
        metrics={makeMetrics({
          total_input_tokens: 375_000,
          session_token_budget: 500_000,
        })}
      />,
    )
    const bar = document.querySelector(
      '[data-metric-row="session_token_budget"] [role="progressbar"]',
    )
    expect(bar?.getAttribute('data-tone')).toBe('warn')
    // No critical-threshold warning yet.
    expect(bar?.textContent).not.toContain('approaching ceiling')
  })

  it('flips to red tone and surfaces warning text at ≥90%', () => {
    render(
      <MetricsTable
        metrics={makeMetrics({
          total_input_tokens: 475_000,
          session_token_budget: 500_000,
        })}
      />,
    )
    const bar = document.querySelector(
      '[data-metric-row="session_token_budget"] [role="progressbar"]',
    )
    expect(bar?.getAttribute('data-tone')).toBe('crit')
    expect(bar?.textContent).toContain('approaching ceiling')
  })

  it('clamps the progressbar fill at 100% when used exceeds cap', () => {
    // The tool loop should have soft-blocked by the time this happens,
    // but the UI must degrade gracefully rather than render
    // `width: 140%` or negative aria values.
    render(
      <MetricsTable
        metrics={makeMetrics({
          total_input_tokens: 700_000,
          session_token_budget: 500_000,
        })}
      />,
    )
    const bar = document.querySelector(
      '[data-metric-row="session_token_budget"] [role="progressbar"]',
    )
    expect(bar?.getAttribute('data-tone')).toBe('crit')
    expect(bar?.textContent).toContain('(100%)')
  })

  it('is accessible — axe passes with a budget row rendered', async () => {
    const { container } = render(
      <MetricsTable
        metrics={makeMetrics({
          total_input_tokens: 300_000,
          session_token_budget: 500_000,
        })}
      />,
    )
    const results = await axe.run(container)
    expect(results.violations).toEqual([])
  })
})
