import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, test, vi, beforeEach } from 'vitest'
import { LitEntityButton } from './AssistantTurnCard'

// Mirror the mockFetch helper pattern from BlockerCard.test.tsx.
function mockFetch(
  responses: Response[],
) {
  const mock = vi.fn()
  for (const r of responses) mock.mockResolvedValueOnce(r)
  ;(globalThis as unknown as { fetch: typeof fetch }).fetch =
    mock as unknown as typeof fetch
  return mock
}

function jsonResponse(body: unknown, init: ResponseInit = {}): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'Content-Type': 'application/json' },
    ...init,
  })
}

const greenCtx = {
  entity: 'ACAN',
  entity_kind: 'gene',
  prior_rows: [
    {
      entity: 'ACAN',
      entity_kind: 'gene',
      pmid: '28123456',
      evidence_quote: 'ACAN reduction',
      source_kind: 'pmc_oa_full_text',
      source_hash: 'sha256:abc',
      redistributable: true,
    },
  ],
  finding_rows: [],
  source_artifacts: [],
  source_scope: 'pmc_oa',
}

describe('LitEntityButton', () => {
  beforeEach(() => {
    (globalThis as unknown as { fetch: typeof fetch }).fetch = vi.fn()
  })

  test('renders the entity name as a button', () => {
    render(<LitEntityButton name="ACAN" kind="gene" sessionId="s1" />)
    expect(
      screen.getByRole('button', { name: /Show literature context for ACAN/i }),
    ).toBeInTheDocument()
  })

  test('clicking fetches and opens the card', async () => {
    mockFetch([jsonResponse(greenCtx)])
    const user = userEvent.setup()
    render(<LitEntityButton name="ACAN" kind="gene" sessionId="s1" />)
    await user.click(
      screen.getByRole('button', { name: /Show literature context for ACAN/i }),
    )
    await waitFor(() => {
      expect(screen.getByText('28123456')).toBeInTheDocument()
    })
    // Dialog should be present
    expect(
      screen.getByRole('dialog', { name: /Literature context for ACAN/i }),
    ).toBeInTheDocument()
  })

  test('shows error on 404 (no literature atoms ran)', async () => {
    mockFetch([new Response('', { status: 404, statusText: 'Not Found' })])
    const user = userEvent.setup()
    render(<LitEntityButton name="ACAN" kind="gene" sessionId="s1" />)
    await user.click(
      screen.getByRole('button', { name: /Show literature context for ACAN/i }),
    )
    await waitFor(() => {
      expect(screen.getByText(/No literature atoms ran/i)).toBeInTheDocument()
    })
  })

  test('shows error on 409 (package not yet emitted)', async () => {
    mockFetch([new Response('', { status: 409, statusText: 'Conflict' })])
    const user = userEvent.setup()
    render(<LitEntityButton name="ACAN" kind="gene" sessionId="s1" />)
    await user.click(
      screen.getByRole('button', { name: /Show literature context for ACAN/i }),
    )
    await waitFor(() => {
      expect(screen.getByText(/Package not yet emitted/i)).toBeInTheDocument()
    })
  })

  test('clicking a second time closes the popover', async () => {
    mockFetch([jsonResponse(greenCtx)])
    const user = userEvent.setup()
    render(<LitEntityButton name="ACAN" kind="gene" sessionId="s1" />)
    const btn = screen.getByRole('button', {
      name: /Show literature context for ACAN/i,
    })
    await user.click(btn)
    await waitFor(() =>
      expect(screen.getByText('28123456')).toBeInTheDocument(),
    )
    // Second click closes
    await user.click(btn)
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  test('close button dismisses the popover', async () => {
    mockFetch([jsonResponse(greenCtx)])
    const user = userEvent.setup()
    render(<LitEntityButton name="ACAN" kind="gene" sessionId="s1" />)
    await user.click(
      screen.getByRole('button', { name: /Show literature context for ACAN/i }),
    )
    await waitFor(() =>
      expect(screen.getByText('28123456')).toBeInTheDocument(),
    )
    await user.click(
      screen.getByRole('button', { name: /Close literature context/i }),
    )
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  test('does not re-fetch when popover is opened a second time', async () => {
    const fetchMock = mockFetch([jsonResponse(greenCtx)])
    const user = userEvent.setup()
    render(<LitEntityButton name="ACAN" kind="gene" sessionId="s1" />)
    const btn = screen.getByRole('button', {
      name: /Show literature context for ACAN/i,
    })
    // First open
    await user.click(btn)
    await waitFor(() =>
      expect(screen.getByText('28123456')).toBeInTheDocument(),
    )
    // Close
    await user.click(btn)
    // Reopen
    await user.click(btn)
    // Fetch was called exactly once
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })
})
