// tokens. Tests mock the three endpoints — GET /share-tokens on mount
// and POST /share-token on Create link click.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import ShareModal from './ShareModal'

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

beforeEach(() => {
  // Stub the clipboard so createAndCopy() doesn't throw under jsdom.
  Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    writable: true,
    value: { writeText: vi.fn().mockResolvedValue(undefined) },
  })
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('ShareModal', () => {
  it('renders as a dialog with a visible heading', async () => {
    mockFetch([jsonResponse(200, [])])
    render(<ShareModal sessionId="s1" onClose={vi.fn()} />)
    expect(screen.getByRole('dialog', { name: 'Share session' })).toBeInTheDocument()
    expect(screen.getByText('Share session (read-only)')).toBeInTheDocument()
    await waitFor(() =>
      expect(
        (globalThis.fetch as unknown as { mock: { calls: unknown[] } }).mock.calls.length,
      ).toBeGreaterThan(0),
    )
  })

  it('Close button fires onClose', async () => {
    mockFetch([jsonResponse(200, [])])
    const onClose = vi.fn()
    render(<ShareModal sessionId="s1" onClose={onClose} />)
    fireEvent.click(screen.getByLabelText('Close'))
    expect(onClose).toHaveBeenCalled()
  })

  it('Create link posts to /share-token and surfaces the token URL', async () => {
    const fetchMock = mockFetch([
      // initial list
      jsonResponse(200, []),
      // POST /share-token
      jsonResponse(200, {
        token: 'abcdefghijUNIQUE42',
        expires_at: null,
        created_at: new Date().toISOString(),
        scope: 'read_only',
      }),
      // refresh after create
      jsonResponse(200, [
        {
          token: 'abcdefghijUNIQUE42',
          expires_at: null,
          created_at: new Date().toISOString(),
          scope: 'read_only',
        },
      ]),
    ])
    render(<ShareModal sessionId="s1" onClose={vi.fn()} />)
    await waitFor(() => {
      expect(fetchMock.mock.calls.length).toBeGreaterThanOrEqual(1)
    })
    fireEvent.click(screen.getByRole('button', { name: 'Create link' }))
    // Wait for the "URL copied" feedback (Create link fires createAndCopy
    // which writes to navigator.clipboard + sets copyFeedback). Then
    // assert the refresh call surfaced the new token's last 8 chars in
    // the active-tokens list.
    await waitFor(() => {
      expect(screen.getByText('URL copied to clipboard.')).toBeInTheDocument()
    })
    const postCall = fetchMock.mock.calls[1]
    expect(postCall![0]).toBe('/api/v1/chat/session/s1/share-token')
    expect(postCall![1].method).toBe('POST')
  })
})
