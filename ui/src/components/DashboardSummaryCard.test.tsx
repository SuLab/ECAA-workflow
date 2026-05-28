// summary. Component does NOT auto-fetch; the SME clicks Generate /
// Refresh. Tests verify the initial empty state, the POST on click,
// and the rendered summary.

import { afterEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import DashboardSummaryCard from './DashboardSummaryCard'

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

describe('DashboardSummaryCard', () => {
  it('renders nothing when sessionId is null', () => {
    const { container } = render(<DashboardSummaryCard sessionId={null} />)
    expect(container.firstChild).toBeNull()
  })

  it('renders a Generate summary button before any summary is fetched', () => {
    render(<DashboardSummaryCard sessionId="s1" />)
    expect(screen.getByLabelText('Analysis summary')).toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: 'Generate summary' }),
    ).toBeInTheDocument()
  })

  it('clicking the button POSTs to /dashboard/summary and renders the summary', async () => {
    const fetchMock = mockFetch([
      jsonResponse(200, {
        summary: 'A three paragraph summary.',
        model: 'haiku-4.5',
        cached: false,
      }),
    ])
    render(<DashboardSummaryCard sessionId="s1" />)
    fireEvent.click(screen.getByRole('button', { name: 'Generate summary' }))
    await waitFor(() =>
      expect(screen.getByText('A three paragraph summary.')).toBeInTheDocument(),
    )
    expect(fetchMock!.mock.calls[0]![0]).toBe('/api/v1/chat/session/s1/dashboard/summary')
    expect(fetchMock!.mock.calls[0]![1].method).toBe('POST')
    // After a fetch lands the button relabels to "Refresh summary".
    expect(
      screen.getByRole('button', { name: 'Refresh summary' }),
    ).toBeInTheDocument()
  })
})
