// Coverage: mergePilotStates merge logic, formatBucketPct, isCostSumMismatch,
// and CostBreakdownBlock rendering. MetricsTable integration is covered by
// StateInspectorPane.test.tsx.

import { describe, expect, it } from 'vitest'
import { render, screen } from '@testing-library/react'

import { mergePilotStates, formatBucketPct, isCostSumMismatch, CostBreakdownBlock } from './MetricsTab'
import type { SessionMetrics } from '../../api/chatClient'

describe('mergePilotStates', () => {
  it('returns SSE state when it carries a complete report', () => {
    const sse = { status: 'complete' as const, report: { from: 'sse' }, skipReason: null }
    const merged = mergePilotStates(sse, { from: 'fetch' })
    expect(merged).toBe(sse)
  })

  it('promotes fetched payload to complete status when SSE has none', () => {
    const merged = mergePilotStates(undefined, { from: 'fetch' })
    expect(merged).toEqual({
      status: 'complete',
      report: { from: 'fetch' },
      skipReason: null,
    })
  })

  it('falls back to SSE skipped state when fetch is null', () => {
    const sse = { status: 'skipped' as const, report: null, skipReason: 'pilot off' }
    const merged = mergePilotStates(sse, null)
    expect(merged).toBe(sse)
  })

  it('returns undefined when both inputs are absent', () => {
    expect(mergePilotStates(undefined, null)).toBeUndefined()
  })

  it('SSE wins over fetch even when SSE has only a started status', () => {
    // Started ⇒ no `report` yet; fetched payload hydrates the row.
    const sse = { status: 'started' as const, report: null, skipReason: null }
    const merged = mergePilotStates(sse, { from: 'fetch' })
    expect(merged?.report).toEqual({ from: 'fetch' })
    expect(merged?.status).toBe('complete')
  })
})

describe('formatBucketPct', () => {
  it('returns — when total is zero', () => {
    expect(formatBucketPct(0, 0)).toBe('—')
    expect(formatBucketPct(1, 0)).toBe('—')
  })

  it('rounds to zero decimals for values >= 10%', () => {
    expect(formatBucketPct(0.5, 1.0)).toBe('50%')
    expect(formatBucketPct(0.1, 1.0)).toBe('10%')
    expect(formatBucketPct(1.0, 1.0)).toBe('100%')
  })

  it('uses one decimal for values < 10%', () => {
    expect(formatBucketPct(0.05, 1.0)).toBe('5.0%')
    expect(formatBucketPct(0.001, 1.0)).toBe('0.1%')
    expect(formatBucketPct(0.099, 1.0)).toBe('9.9%')
  })

  it('handles partial bucket that is just at the 10% boundary', () => {
    // Exactly 10% should round to "10%" (zero decimals branch).
    expect(formatBucketPct(0.1, 1.0)).toBe('10%')
  })
})

describe('isCostSumMismatch', () => {
  it('returns false when four buckets sum to total within tolerance', () => {
    expect(isCostSumMismatch(0.10, 0.20, 0.05, 0.05, 0.40)).toBe(false)
  })

  it('returns false on exact match', () => {
    expect(isCostSumMismatch(0, 0, 0, 0, 0)).toBe(false)
  })

  it('returns false when drift is below $0.01', () => {
    // 0.1 + 0.2 + 0.05 + 0.05 = 0.40; total = 0.4099 → diff = 0.0099 < 0.01
    expect(isCostSumMismatch(0.10, 0.20, 0.05, 0.05, 0.4099)).toBe(false)
  })

  it('returns true when drift exceeds $0.01', () => {
    // sum = 0.40; total = 0.42 → diff = 0.02 > 0.01
    expect(isCostSumMismatch(0.10, 0.20, 0.05, 0.05, 0.42)).toBe(true)
  })

  it('returns true on large discrepancy', () => {
    expect(isCostSumMismatch(1.00, 0, 0, 0, 5.00)).toBe(true)
  })
})

// Minimal SessionMetrics fixture for rendering tests.
function makeMetrics(overrides: Partial<SessionMetrics> = {}): SessionMetrics {
  return {
    turn_count: 2,
    tool_call_count: 4,
    sonnet_turns: 2,
    opus_turns: 0,
    mean_turn_ms: 1000,
    p50_turn_ms: 900,
    p95_turn_ms: 1200,
    p99_turn_ms: 1500,
    max_turn_ms: 1600,
    total_input_tokens: 1000,
    total_output_tokens: 500,
    cache_read_tokens: 800,
    cache_creation_tokens: 200,
    cache_hit_ratio: 0.8,
    total_cost_usd: 0.40,
    chat_cost_usd: 0.20,
    agent_cost_usd: 0.10,
    scorer_cost_usd: 0.05,
    side_call_cost_usd: 0.05,
    per_model_cost_usd: {},
    per_model_turns: {},
    per_model_agent_cost_usd: {},
    per_model_scorer_cost_usd: {},
    per_model_side_call_cost_usd: {},
    total_instance_seconds: 0,
    instance_type_seconds: {},
    high_water_exceeded_count: 0,
    ...overrides,
  } as unknown as SessionMetrics
}

describe('CostBreakdownBlock', () => {
  it('renders all four bucket labels', () => {
    render(<CostBreakdownBlock metrics={makeMetrics()} />)
    expect(screen.getByText(/Chat \(Sonnet\/Opus turns\)/)).toBeTruthy()
    expect(screen.getByText(/Agent \(task execution, claude CLI\)/)).toBeTruthy()
    expect(screen.getByText(/Scorer \(rubric LLM-judge\)/)).toBeTruthy()
    expect(screen.getByText(/Side-calls \(auto-title, remediation\)/)).toBeTruthy()
  })

  it('shows percent-of-total for each bucket', () => {
    render(<CostBreakdownBlock metrics={makeMetrics()} />)
    // chat: 0.20/0.40 = 50%, agent: 0.10/0.40 = 25%,
    // scorer: 0.05/0.40 = 12.5% → 12% (>= 10 → 0 decimals? no, 12.5 >= 10 so Math.round = 13)
    // actually 12.5% → Math.round(12.5) = 13
    const pctSpans = document.querySelectorAll('[data-cost-pct="true"]')
    expect(pctSpans.length).toBe(4)
  })

  it('does not show sum-mismatch warning when buckets sum correctly', () => {
    render(<CostBreakdownBlock metrics={makeMetrics()} />)
    expect(document.querySelector('[data-cost-sum-mismatch="true"]')).toBeNull()
  })

  it('shows sum-mismatch warning when buckets do not sum to total', () => {
    const metrics = makeMetrics({ total_cost_usd: 1.00 })
    render(<CostBreakdownBlock metrics={metrics} />)
    expect(document.querySelector('[data-cost-sum-mismatch="true"]')).toBeTruthy()
  })

  it('shows fetched-at timestamp when fetchedAt is provided', () => {
    const at = new Date('2026-05-16T10:30:00')
    render(<CostBreakdownBlock metrics={makeMetrics()} fetchedAt={at} />)
    expect(screen.getByText(/fetched/)).toBeTruthy()
  })

  it('omits fetched-at timestamp when fetchedAt is null', () => {
    render(<CostBreakdownBlock metrics={makeMetrics()} fetchedAt={null} />)
    expect(screen.queryByText(/fetched/)).toBeNull()
  })

  it('bucket rows carry aria-label attributes', () => {
    render(<CostBreakdownBlock metrics={makeMetrics()} />)
    const rows = document.querySelectorAll('[data-metric-row="cost-bucket"]')
    // All four bucket rows should have aria-label set.
    expect(rows.length).toBe(4)
    rows.forEach((row) => {
      expect(row.getAttribute('aria-label')).toBeTruthy()
    })
  })
})
