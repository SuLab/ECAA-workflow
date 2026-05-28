// on mount and lists the tables + figures in the report. Tests cover
// the no-session placeholder, the 404 empty state, and the happy path
// where the report renders at least one table name.

import { afterEach, describe, expect, it, vi } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import { CompareTab } from './CompareTab'

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

describe('CompareTab', () => {
  it('renders the start-a-session placeholder when sessionId is null', () => {
    render(<CompareTab sessionId={null} />)
    expect(
      screen.getByText(/Start a session to compare results/),
    ).toBeInTheDocument()
  })

  it('fetches /cross-version-diff on mount', async () => {
    const fetchMock = mockFetch([jsonResponse(200, { tables: [], figures: [] })])
    render(<CompareTab sessionId="s1" />)
    await waitFor(() => expect(fetchMock).toHaveBeenCalled())
    expect(fetchMock!.mock.calls[0]![0]).toBe(
      '/api/v1/chat/session/s1/cross-version-diff',
    )
  })

  it('renders the no-parent-package message on 404', async () => {
    mockFetch([jsonResponse(404, 'no parent lineage')])
    render(<CompareTab sessionId="s1" />)
    await waitFor(() =>
      expect(
        screen.getByText(/no parent package to compare against/),
      ).toBeInTheDocument(),
    )
  })

  it('renders a table name when the report has tables', async () => {
    mockFetch([
      jsonResponse(200, {
        tables: [{ name: 'de_genes.tsv', concordance: 0.92 }],
        figures: [],
      }),
    ])
    render(<CompareTab sessionId="s1" />)
    await waitFor(() =>
      expect(screen.getByText('de_genes.tsv')).toBeInTheDocument(),
    )
  })
})
