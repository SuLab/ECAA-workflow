// the session has tripped the 75% / 100% soft budget state. Polls
// /api/chat/session/:id/metrics every 15s; tests stub fetch and wait
// for the first poll to land.

import { afterEach, describe, expect, it, vi } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import BudgetChip from './BudgetChip'

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

afterEach(() => {
  vi.restoreAllMocks()
})

describe('BudgetChip', () => {
  it('renders nothing when sessionId is null', () => {
    const { container } = render(<BudgetChip sessionId={null} />)
    expect(container.firstChild).toBeNull()
  })

  it('renders nothing when budget_state is ok', async () => {
    mockFetch([jsonResponse(200, { budget_state: 'ok', budget_used_pct: 0.1 })])
    const { container } = render(<BudgetChip sessionId="s1" />)
    // Wait for the initial load to fire; the chip should stay empty.
    await waitFor(() => {
      expect((globalThis.fetch as unknown as { mock: { calls: unknown[] } }).mock.calls.length).toBeGreaterThan(0)
    })
    expect(container.firstChild).toBeNull()
  })

  it('renders "75% of budget" on warn + carries role=status aria-live=polite', async () => {
    mockFetch([jsonResponse(200, { budget_state: 'warn', budget_used_pct: 0.75 })])
    render(<BudgetChip sessionId="s1" />)
    await waitFor(() =>
      expect(screen.getByText('75% of budget')).toBeInTheDocument(),
    )
    const chip = screen.getByRole('status')
    expect(chip).toHaveAttribute('aria-live', 'polite')
  })

  it('renders "Over budget" when budget_state is exceeded', async () => {
    mockFetch([jsonResponse(200, { budget_state: 'exceeded', budget_used_pct: 1.12 })])
    render(<BudgetChip sessionId="s1" />)
    await waitFor(() =>
      expect(screen.getByText('Over budget')).toBeInTheDocument(),
    )
  })
})
