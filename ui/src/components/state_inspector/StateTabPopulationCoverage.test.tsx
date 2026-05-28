// V3+v4 residuals closure StateTab Population-coverage mount test.
//
// Confirms PopulationCoverageCard is rendered above the raw-state JSON
// when a sessionId is provided. The card's own fetch path is mocked at
// the global `fetch` level so we don't take a real network hop.

import { render, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import * as chatClient from '../../api/chatClient'
import { StateTab } from './StateTab'
import type { SessionStateSnapshot } from '../../api/chatClient'

const sampleState = {
  session_id: 's1',
  state: { kind: 'emitted' } as unknown as SessionStateSnapshot['state'],
  user_confirmed: true,
  last_activity: '2026-05-11T12:00:00Z',
  task_count: 4,
  progress: { completed: 1, ready: 2, blocked: 0, pending: 1 },
} as SessionStateSnapshot

beforeEach(() => {
  // PopulationCoverageCard fetches `/population-coverage` on mount.
  // Stub it with the empty (no-workflow) shape so the card renders
  // without error.
  vi.stubGlobal(
    'fetch',
    vi.fn(async () =>
      new Response(JSON.stringify({ workflow_id: null, statement: null }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }),
    ),
  )
  vi.spyOn(chatClient, 'getComposeOutcome').mockResolvedValue(null)
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('StateTab population coverage', () => {
  it('renders the PopulationCoverageCard when sessionId is provided', async () => {
    const { findByLabelText } = render(
      <StateTab state={sampleState} sessionId="s1" />,
    )
    await waitFor(async () => {
      expect(await findByLabelText(/Population coverage/i)).toBeInTheDocument()
    })
  })

  it('does NOT render the card when sessionId is null (legacy)', () => {
    const { queryByLabelText, container } = render(
      <StateTab state={sampleState} sessionId={null} />,
    )
    expect(queryByLabelText(/Population coverage/i)).toBeNull()
    // The raw-state JSON is still rendered so the debug view continues
    // to work.
    expect(container.textContent).toContain('"session_id": "s1"')
  })

  it('still renders the raw state JSON when sessionId is provided', async () => {
    const { container, findByLabelText } = render(
      <StateTab state={sampleState} sessionId="s1" />,
    )
    await findByLabelText(/Population coverage/i)
    expect(container.textContent).toContain('"task_count": 4')
  })
})
