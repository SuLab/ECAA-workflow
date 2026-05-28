// new caps to /api/v1/chat/session/:id/budget. Tests build a minimal
// SessionMetrics-shaped object inline — the component only reads five
// fields from it.

import { afterEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import BudgetRow from './BudgetRow'
import type { SessionMetrics } from '../api/chatClient'

function mockFetch(responses: Array<Response>) {
  const mock = vi.fn()
  for (const r of responses) mock.mockResolvedValueOnce(r)
  ;(globalThis as unknown as { fetch: typeof fetch }).fetch = mock as unknown as typeof fetch
  return mock
}

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

// Minimal SessionMetrics — only the fields BudgetRow reads. Cast through
// `unknown` so we don't have to populate the 40+ unrelated fields.
function metrics(patch: Partial<SessionMetrics>): SessionMetrics {
  return patch as unknown as SessionMetrics
}

afterEach(() => {
  vi.restoreAllMocks()
})

describe('BudgetRow', () => {
  it('returns null when metrics is null', () => {
    const { container } = render(
      <BudgetRow metrics={null} sessionId="s1" />,
    )
    expect(container.firstChild).toBeNull()
  })

  it('renders "No cap set" when budget_usd is null', () => {
    render(
      <BudgetRow
        metrics={metrics({ budget_usd: null, total_cost_usd: 0.42 })}
        sessionId="s1"
      />,
    )
    expect(screen.getByText(/No cap set/)).toBeInTheDocument()
    expect(screen.getByLabelText('Session budget')).toBeInTheDocument()
  })

  it('renders "$5.00 / $10.00 (50%)" when a cap is set', () => {
    render(
      <BudgetRow
        metrics={metrics({
          budget_usd: 10,
          total_cost_usd: 5,
          budget_used_pct: 0.5,
          budget_state: 'ok',
          projected_finish_usd: 6,
        })}
        sessionId="s1"
      />,
    )
    expect(screen.getByText(/\$5\.00 \/ \$10\.00/)).toBeInTheDocument()
    expect(screen.getByText(/50%/)).toBeInTheDocument()
  })

  it('Edit → Save posts {usd: 12} to /api/v1/chat/session/s1/budget', async () => {
    const onChanged = vi.fn()
    const fetchMock = mockFetch([
      jsonResponse(200, { budget_usd: 12, budget_set_by: null, budget_set_at: null }),
    ])
    render(
      <BudgetRow
        metrics={metrics({
          budget_usd: 10,
          total_cost_usd: 5,
          budget_used_pct: 0.5,
          budget_state: 'ok',
        })}
        sessionId="s1"
        onChanged={onChanged}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: 'Edit' }))
    const input = screen.getByRole('spinbutton')
    fireEvent.change(input, { target: { value: '12' } })
    fireEvent.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(onChanged).toHaveBeenCalled())

    const call = fetchMock.mock.calls[0]
    expect(call![0]).toBe('/api/v1/chat/session/s1/budget')
    expect(call![1].method).toBe('POST')
    const body = JSON.parse(call![1].body as string)
    expect(body.usd).toBe(12)
  })

  it('when budget_state is exceeded, Session budget container renders', () => {
    render(
      <BudgetRow
        metrics={metrics({
          budget_usd: 10,
          total_cost_usd: 11,
          budget_used_pct: 1.1,
          budget_state: 'exceeded',
        })}
        sessionId="s1"
      />,
    )
    expect(screen.getByLabelText('Session budget')).toBeInTheDocument()
  })
})
