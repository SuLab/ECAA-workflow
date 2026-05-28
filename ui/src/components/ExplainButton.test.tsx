// Tests use `sessionIdOverride` so the component works outside a
// SessionProvider.

import { afterEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import ExplainButton from './ExplainButton'

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

describe('ExplainButton', () => {
  it('returns null for empty text', () => {
    const { container } = render(
      <ExplainButton text="" sessionIdOverride="s1" />,
    )
    expect(container.firstChild).toBeNull()
  })

  it('renders a button with aria-label "Explain this in plain language"', () => {
    render(<ExplainButton text="CPM normalization" sessionIdOverride="s1" />)
    expect(
      screen.getByLabelText('Explain this in plain language'),
    ).toBeInTheDocument()
  })

  it('clicking opens the popover and POSTs to /explain', async () => {
    const fetchMock = mockFetch([
      jsonResponse(200, {
        explanation: 'Counts-per-million, a per-sample scaling.',
        model: 'haiku-4.5',
        cached: false,
      }),
    ])
    render(
      <ExplainButton text="CPM normalization" sessionIdOverride="s1" context="method" />,
    )
    fireEvent.click(screen.getByLabelText('Explain this in plain language'))
    await waitFor(() =>
      expect(
        screen.getByText('Counts-per-million, a per-sample scaling.'),
      ).toBeInTheDocument(),
    )
    expect(fetchMock!.mock.calls[0]![0]).toBe('/api/v1/chat/session/s1/explain')
    const body = JSON.parse(fetchMock!.mock.calls[0]![1].body as string)
    expect(body.text).toBe('CPM normalization')
    expect(body.context).toBe('method')
  })

  it('"Try again" re-fires POST with prior explanation in context', async () => {
    const fetchMock = mockFetch([
      jsonResponse(200, {
        explanation: 'First rewrite.',
        model: 'haiku-4.5',
        cached: false,
      }),
      jsonResponse(200, {
        explanation: 'Second rewrite.',
        model: 'haiku-4.5',
        cached: false,
      }),
    ])
    render(
      <ExplainButton text="log2FC" sessionIdOverride="s1" context="narrative" />,
    )
    fireEvent.click(screen.getByLabelText('Explain this in plain language'))
    await waitFor(() => expect(screen.getByText('First rewrite.')).toBeInTheDocument())
    fireEvent.click(screen.getByText('That was unclear — try again'))
    await waitFor(() => expect(screen.getByText('Second rewrite.')).toBeInTheDocument())
    const secondBody = JSON.parse(fetchMock!.mock.calls[1]![1].body as string)
    expect(secondBody.context).toContain('First rewrite.')
    expect(secondBody.context).toContain('narrative')
  })
})
