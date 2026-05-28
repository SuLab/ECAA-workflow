import { afterEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import RecentSessionsDropdown from './RecentSessionsDropdown'
import * as chatClient from '../api/chatClient'

const sample = (
  overrides: Partial<chatClient.RecentSessionSummary> = {},
): chatClient.RecentSessionSummary => ({
  session_id: '11111111-2222-3333-4444-555555555555',
  title: 'IVD reanalysis',
  created_at: new Date(Date.now() - 60_000).toISOString(),
  last_activity: new Date(Date.now() - 60_000).toISOString(),
  state_kind: 'emitted',
  // Default to idle so test bodies must opt in to "running" — matches
  // the new contract where execution_status is the only signal that
  // licenses the "Running" pill.
  execution_status: 'idle',
  parent_id: null,
  n_turns: 7,
  project_class: 'SingleCellRnaseq',
  ...overrides,
})

describe('RecentSessionsDropdown', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders just the trigger button until opened', () => {
    render(
      <RecentSessionsDropdown
        currentSessionId="s-current"
        onSelect={() => {}}
        onNewSession={() => {}}
      />,
    )
    expect(screen.getByTestId('recent-sessions-button')).toBeInTheDocument()
    expect(screen.queryByTestId('recent-sessions-panel')).toBeNull()
  })

  it('lists sessions when opened and surfaces title + state badge', async () => {
    vi.spyOn(chatClient, 'getRecentSessions').mockResolvedValue([
      sample({
        title: 'IVD reanalysis',
        state_kind: 'emitted',
        execution_status: 'running',
      }),
      sample({
        session_id: 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
        title: null,
        state_kind: 'blocked',
        n_turns: 12,
      }),
    ])
    render(
      <RecentSessionsDropdown
        currentSessionId="s-current"
        onSelect={() => {}}
        onNewSession={() => {}}
      />,
    )
    fireEvent.click(screen.getByTestId('recent-sessions-button'))
    await waitFor(() => {
      expect(screen.getByTestId('recent-sessions-panel')).toBeInTheDocument()
    })
    expect(screen.getByText('IVD reanalysis')).toBeInTheDocument()
    // Untitled fallback derives from project_class.
    expect(screen.getByText('SingleCellRnaseq')).toBeInTheDocument()
    // The first row is `emitted` with a live harness — both pills render.
    expect(screen.getByText('Emitted')).toBeInTheDocument()
    expect(screen.getByText('Running')).toBeInTheDocument()
    expect(screen.getByText('Blocked')).toBeInTheDocument()
  })

  it('does not show the Running pill on emitted sessions with idle execution', async () => {
    vi.spyOn(chatClient, 'getRecentSessions').mockResolvedValue([
      sample({
        title: 'Idle workflow',
        state_kind: 'emitted',
        execution_status: 'idle',
      }),
    ])
    render(
      <RecentSessionsDropdown
        currentSessionId="s-current"
        onSelect={() => {}}
        onNewSession={() => {}}
      />,
    )
    fireEvent.click(screen.getByTestId('recent-sessions-button'))
    await waitFor(() =>
      expect(screen.getByTestId('recent-sessions-panel')).toBeInTheDocument(),
    )
    // Faithful state pill still renders.
    expect(screen.getByText('Emitted')).toBeInTheDocument()
    // No execution pill — the row should NOT carry a Running badge just
    // because the package was emitted at some point.
    expect(screen.queryByTestId('exec-status-pill')).toBeNull()
    expect(screen.queryByText('Running')).toBeNull()
  })

  it('renders the Running execution pill alongside the state pill when execution is live', async () => {
    vi.spyOn(chatClient, 'getRecentSessions').mockResolvedValue([
      sample({
        title: 'Live workflow',
        state_kind: 'blocked',
        execution_status: 'running',
      }),
    ])
    render(
      <RecentSessionsDropdown
        currentSessionId="s-current"
        onSelect={() => {}}
        onNewSession={() => {}}
      />,
    )
    fireEvent.click(screen.getByTestId('recent-sessions-button'))
    await waitFor(() =>
      expect(screen.getByTestId('recent-sessions-panel')).toBeInTheDocument(),
    )
    expect(screen.getByText('Blocked')).toBeInTheDocument()
    const execPill = screen.getByTestId('exec-status-pill')
    expect(execPill.textContent).toBe('Running')
  })

  it('treats exited execution as not-running (no execution pill)', async () => {
    vi.spyOn(chatClient, 'getRecentSessions').mockResolvedValue([
      sample({
        title: 'Crashed workflow',
        state_kind: 'emitted',
        execution_status: 'exited',
      }),
    ])
    render(
      <RecentSessionsDropdown
        currentSessionId="s-current"
        onSelect={() => {}}
        onNewSession={() => {}}
      />,
    )
    fireEvent.click(screen.getByTestId('recent-sessions-button'))
    await waitFor(() =>
      expect(screen.getByTestId('recent-sessions-panel')).toBeInTheDocument(),
    )
    expect(screen.getByText('Emitted')).toBeInTheDocument()
    expect(screen.queryByTestId('exec-status-pill')).toBeNull()
  })

  it('invokes onSelect when a non-current row is clicked and closes the panel', async () => {
    vi.spyOn(chatClient, 'getRecentSessions').mockResolvedValue([
      sample({
        session_id: '11111111-2222-3333-4444-555555555555',
        title: 'Pick me',
      }),
    ])
    const onSelect = vi.fn()
    render(
      <RecentSessionsDropdown
        currentSessionId="s-current"
        onSelect={onSelect}
        onNewSession={() => {}}
      />,
    )
    fireEvent.click(screen.getByTestId('recent-sessions-button'))
    await waitFor(() => {
      expect(
        screen.getByTestId(
          'recent-sessions-item-11111111-2222-3333-4444-555555555555',
        ),
      ).toBeInTheDocument()
    })
    fireEvent.click(
      screen.getByTestId(
        'recent-sessions-item-11111111-2222-3333-4444-555555555555',
      ),
    )
    expect(onSelect).toHaveBeenCalledWith(
      '11111111-2222-3333-4444-555555555555',
    )
    expect(screen.queryByTestId('recent-sessions-panel')).toBeNull()
  })

  it('does not call onSelect when the current session is clicked', async () => {
    vi.spyOn(chatClient, 'getRecentSessions').mockResolvedValue([
      sample({ session_id: 's-current', title: 'Current' }),
    ])
    const onSelect = vi.fn()
    render(
      <RecentSessionsDropdown
        currentSessionId="s-current"
        onSelect={onSelect}
        onNewSession={() => {}}
      />,
    )
    fireEvent.click(screen.getByTestId('recent-sessions-button'))
    await waitFor(() =>
      expect(
        screen.getByTestId('recent-sessions-item-s-current'),
      ).toBeInTheDocument(),
    )
    fireEvent.click(screen.getByTestId('recent-sessions-item-s-current'))
    expect(onSelect).not.toHaveBeenCalled()
  })

  it('triggers onNewSession when the "+ New session" item is clicked', async () => {
    vi.spyOn(chatClient, 'getRecentSessions').mockResolvedValue([])
    const onNewSession = vi.fn()
    render(
      <RecentSessionsDropdown
        currentSessionId="s-current"
        onSelect={() => {}}
        onNewSession={onNewSession}
      />,
    )
    fireEvent.click(screen.getByTestId('recent-sessions-button'))
    await waitFor(() =>
      expect(screen.getByTestId('recent-sessions-new')).toBeInTheDocument(),
    )
    fireEvent.click(screen.getByTestId('recent-sessions-new'))
    expect(onNewSession).toHaveBeenCalledTimes(1)
  })
})
