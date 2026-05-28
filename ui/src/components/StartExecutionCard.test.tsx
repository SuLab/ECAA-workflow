// Tests mock fetch and verify rendering + POST body + status rendering
// for the two endpoints the card exercises
// (POST /api/v1/chat/session/:id/start-execution and
// GET /api/v1/chat/session/:id/execution). The happy path with a real
// harness + SSE flow is covered by the live Playwright spec.

import { describe, expect, it, vi, afterEach } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import StartExecutionCard from './StartExecutionCard'

function mockFetch(responses: Array<Response | Promise<Response>>) {
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

function emptyResponse(status: number): Response {
  return new Response(null, { status })
}

/// Default /state response — fresh session with zero progress.
/// The card now polls /state alongside /execution so each tick
/// consumes TWO mocks; helper packages the standard "nothing done
/// yet" snapshot.
function freshStateResponse(): Response {
  return jsonResponse(200, {
    session_id: 's1',
    state: { kind: 'emitted' },
    user_confirmed: true,
    last_activity: '2026-04-29T00:00:00Z',
    task_count: 0,
    progress: { completed: 0, ready: 0, blocked: 0, pending: 0 },
  })
}

/// Variant: progress snapshot showing N completed tasks (for the
/// "resume after reboot" path where in-memory execution handle is
/// gone but on-disk progress survives).
function progressStateResponse(completed: number, total = 43): Response {
  return jsonResponse(200, {
    session_id: 's1',
    state: { kind: 'emitted' },
    user_confirmed: true,
    last_activity: '2026-04-29T00:00:00Z',
    task_count: total,
    progress: {
      completed,
      ready: 1,
      blocked: 0,
      pending: total - completed - 1,
    },
  })
}

describe('StartExecutionCard', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders nothing when session is not emitted', () => {
    const { container } = render(
      <StartExecutionCard sessionId="s1" emitted={false} />,
    )
    expect(container.firstChild).toBeNull()
  })

  it('renders nothing when sessionId is null', () => {
    const { container } = render(
      <StartExecutionCard sessionId={null} emitted={true} />,
    )
    expect(container.firstChild).toBeNull()
  })

  it('shows the Start button when emitted and no execution tracked', async () => {
    mockFetch([jsonResponse(404, 'no execution for session'), freshStateResponse()])
    render(<StartExecutionCard sessionId="s1" emitted={true} />)
    await waitFor(() =>
      expect(screen.getByLabelText('Start execution')).toBeInTheDocument(),
    )
    // Agent path is server-resolved from env; the user surface does
    // not expose it.
    expect(screen.queryByLabelText('Agent script path')).toBeNull()
  })

  it('POSTs to /start-execution with an empty body (server picks agent)', async () => {
    const poll404 = jsonResponse(404, 'no execution for session')
    const startOk = jsonResponse(200, {
      pid: 4242,
      started_at: '2026-04-17T13:40:00Z',
      package_dir: '/tmp/pkg',
      agent_command: 'scripts/agent-claude.sh',
      status: 'running',
    })
    // Mock order: poll fires getExecution + getChatState in parallel
    // (Promise.allSettled), then the click fires startExecution.
    const fetchMock = mockFetch([poll404, freshStateResponse(), startOk])

    render(<StartExecutionCard sessionId="s1" emitted={true} />)
    await waitFor(() =>
      expect(screen.getByLabelText('Start execution')).toBeInTheDocument(),
    )

    fireEvent.click(screen.getByLabelText('Start execution'))

    await waitFor(() =>
      expect(screen.getByLabelText('execution status').textContent).toContain(
        'Running · pid 4242',
      ),
    )

    // The start POST is the third fetch (index 2): poll-exec, poll-state, start.
    const call = fetchMock.mock.calls[2]
    expect(call![0]).toBe('/api/v1/chat/session/s1/start-execution')
    expect(call![1].method).toBe('POST')
    const body = JSON.parse(call![1].body as string)
    // Empty body — server resolves agent_path + max_iterations from env.
    expect(body.agent_path).toBeUndefined()
    expect(body.max_iterations).toBeUndefined()
  })

  it('renders the Running pill and pid when /execution returns running', async () => {
    mockFetch([
      jsonResponse(200, {
        pid: 1234,
        started_at: '2026-04-17T13:40:00Z',
        package_dir: '/tmp/pkg',
        agent_command: 'scripts/agent-mock-blocker.sh',
        status: 'running',
      }),
      freshStateResponse(),
    ])
    render(<StartExecutionCard sessionId="s1" emitted={true} />)
    await waitFor(() =>
      expect(screen.getByLabelText('execution status').textContent).toContain(
        'Running · pid 1234',
      ),
    )
    // While running, the Start button is hidden.
    expect(screen.queryByLabelText('Start execution')).toBeNull()
  })

  it('renders the Exited pill + Resume button when /execution returns exited', async () => {
    // Post-exit the button is labeled "Resume execution" (not
    // "Restart") because the harness's safe-stop / clean-exit path
    // checkpoints state. exit_code=0 means resume picks up where the
    // last completed task left off.
    mockFetch([
      jsonResponse(200, {
        pid: 9999,
        started_at: '2026-04-17T13:40:00Z',
        package_dir: '/tmp/pkg',
        agent_command: 'scripts/agent-mock-blocker.sh',
        status: 'exited',
        exit_code: 0,
      }),
      freshStateResponse(),
    ])
    render(<StartExecutionCard sessionId="s1" emitted={true} />)
    await waitFor(() =>
      expect(screen.getByLabelText('execution status').textContent).toContain(
        'Exited · code 0',
      ),
    )
    expect(screen.getByLabelText('Resume execution')).toBeInTheDocument()
    expect(screen.getByText('Resume execution')).toBeInTheDocument()
  })

  it('shows a user-visible error when the server rejects the start', async () => {
    const poll404 = jsonResponse(404, 'no execution for session')
    const startFail = jsonResponse(400, 'session has no emitted package')
    mockFetch([poll404, freshStateResponse(), startFail])

    render(<StartExecutionCard sessionId="s1" emitted={true} />)
    await waitFor(() =>
      expect(screen.getByLabelText('Start execution')).toBeInTheDocument(),
    )
    fireEvent.click(screen.getByLabelText('Start execution'))
    await waitFor(() =>
      expect(screen.getByRole('alert').textContent).toContain(
        'session has no emitted package',
      ),
    )
  })

  it('labels the Start button "Resume execution" when /state shows prior progress', async () => {
    // Regression: after a server reboot or manual stop, the
    // in-memory ExecutionHandle is gone (/execution returns 404) BUT
    // WORKFLOW.json on disk shows partial progress. Clicking Start
    // spawns a harness that resumes from the last completed task.
    // The button label must signal that — "Start execution" misleads
    // SMEs into thinking the run is restarting from scratch.
    mockFetch([
      jsonResponse(404, 'no execution for session'),
      progressStateResponse(7, 43), // 7 of 43 tasks already done
    ])
    render(<StartExecutionCard sessionId="s1" emitted={true} />)
    await waitFor(() =>
      expect(screen.getByLabelText('Resume execution')).toBeInTheDocument(),
    )
    expect(screen.getByText('Resume execution')).toBeInTheDocument()
    // Hint surfaces the on-disk progress so the SME knows what
    // "Resume" picks up from.
    expect(
      screen.getByText(/7 of 43 tasks already complete/i),
    ).toBeInTheDocument()
  })

  describe('button cluster keyed on status', () => {
    it('shows Pause + Stop + Force-kill while running', async () => {
      mockFetch([
        jsonResponse(200, {
          pid: 1234,
          pgid: 1234,
          started_at: '2026-04-17T13:40:00Z',
          package_dir: '/tmp/pkg',
          agent_command: 'scripts/agent-claude.sh',
          status: 'running',
        }),
        freshStateResponse(),
      ])
      render(<StartExecutionCard sessionId="s1" emitted={true} />)
      await waitFor(() =>
        expect(screen.getByLabelText('Pause execution')).toBeInTheDocument(),
      )
      expect(screen.getByLabelText('Stop execution safely (preserves checkpoint for resume)')).toBeInTheDocument()
      expect(screen.getByLabelText('Force-kill execution')).toBeInTheDocument()
      expect(screen.queryByLabelText('Resume execution')).toBeNull()
    })

    it('shows "Cancel pause" + Stop + Force-kill while pausing (ack not yet observed)', async () => {
      // Regression: when /pause sets pause_requested but the harness
      // is mid-iteration and hasn't observed the sentinel yet, server
      // reports status="pausing" — UI must render the same control
      // cluster as paused so the SME can back out.
      mockFetch([
        jsonResponse(200, {
          pid: 1234,
          pgid: 1234,
          started_at: '2026-04-17T13:40:00Z',
          paused_at: '2026-04-17T13:42:00Z',
          package_dir: '/tmp/pkg',
          agent_command: 'scripts/agent-claude.sh',
          status: 'pausing',
        }),
        freshStateResponse(),
      ])
      render(<StartExecutionCard sessionId="s1" emitted={true} />)
      await waitFor(() =>
        expect(screen.getByLabelText('Cancel pause')).toBeInTheDocument(),
      )
      expect(screen.getByText('Cancel pause')).toBeInTheDocument()
      expect(screen.getByLabelText('Stop execution safely (preserves checkpoint for resume)')).toBeInTheDocument()
      expect(screen.getByLabelText('Force-kill execution')).toBeInTheDocument()
      // No "Resume execution" label while merely pausing — that label
      // appears only when the harness has ack'd.
      expect(screen.queryByLabelText('Resume execution')).toBeNull()
    })

    it('Cancel pause POSTs /resume', async () => {
      const poll = jsonResponse(200, {
        pid: 1234,
        pgid: 1234,
        started_at: '2026-04-17T13:40:00Z',
        paused_at: '2026-04-17T13:42:00Z',
        package_dir: '/tmp/pkg',
        agent_command: 'scripts/agent-claude.sh',
        status: 'pausing',
      })
      const ok = emptyResponse(204)
      const refresh = jsonResponse(200, {
        pid: 1234,
        pgid: 1234,
        started_at: '2026-04-17T13:40:00Z',
        package_dir: '/tmp/pkg',
        agent_command: 'scripts/agent-claude.sh',
        status: 'running',
      })
      // poll-exec, poll-state, click→resume POST, refresh-exec, refresh-state
      const fetchMock = mockFetch([
        poll,
        freshStateResponse(),
        ok,
        refresh,
        freshStateResponse(),
      ])

      render(<StartExecutionCard sessionId="s1" emitted={true} />)
      await waitFor(() =>
        expect(screen.getByLabelText('Cancel pause')).toBeInTheDocument(),
      )
      fireEvent.click(screen.getByLabelText('Cancel pause'))
      await waitFor(() =>
        expect(fetchMock!.mock.calls[2]![0]).toBe(
          '/api/v1/chat/session/s1/execution/resume',
        ),
      )
    })

    it('shows Resume + Stop + Force-kill while paused', async () => {
      mockFetch([
        jsonResponse(200, {
          pid: 1234,
          pgid: 1234,
          started_at: '2026-04-17T13:40:00Z',
          paused_at: '2026-04-17T13:42:00Z',
          package_dir: '/tmp/pkg',
          agent_command: 'scripts/agent-claude.sh',
          status: 'paused',
        }),
        freshStateResponse(),
      ])
      render(<StartExecutionCard sessionId="s1" emitted={true} />)
      await waitFor(() =>
        expect(screen.getByLabelText('Resume execution')).toBeInTheDocument(),
      )
      expect(screen.getByLabelText('Stop execution safely (preserves checkpoint for resume)')).toBeInTheDocument()
      expect(screen.getByLabelText('Force-kill execution')).toBeInTheDocument()
      expect(screen.queryByLabelText('Pause execution')).toBeNull()
    })

    it('shows only Force-kill (escape hatch) while stopping', async () => {
      mockFetch([
        jsonResponse(200, {
          pid: 1234,
          pgid: 1234,
          started_at: '2026-04-17T13:40:00Z',
          stop_requested_at: '2026-04-17T13:45:00Z',
          package_dir: '/tmp/pkg',
          agent_command: 'scripts/agent-claude.sh',
          status: 'stopping',
        }),
        freshStateResponse(),
      ])
      render(<StartExecutionCard sessionId="s1" emitted={true} />)
      await waitFor(() =>
        expect(
          screen.getByLabelText('Force-kill execution (escape hatch)'),
        ).toBeInTheDocument(),
      )
      expect(screen.queryByLabelText('Stop execution safely (preserves checkpoint for resume)')).toBeNull()
      expect(screen.queryByLabelText('Pause execution')).toBeNull()
      expect(screen.queryByLabelText('Resume execution')).toBeNull()
    })

    it("renders 'Resume from last checkpoint' for a non-zero exit", async () => {
      mockFetch([
        jsonResponse(200, {
          pid: 9999,
          pgid: 9999,
          started_at: '2026-04-17T13:40:00Z',
          package_dir: '/tmp/pkg',
          agent_command: 'scripts/agent-claude.sh',
          status: 'exited',
          exit_code: 137,
        }),
        freshStateResponse(),
      ])
      render(<StartExecutionCard sessionId="s1" emitted={true} />)
      await waitFor(() =>
        expect(
          screen.getByLabelText('Resume execution').textContent,
        ).toContain('Resume from last checkpoint'),
      )
    })
  })

  describe('control endpoints', () => {
    it('POSTs /pause when Pause is clicked', async () => {
      const poll = jsonResponse(200, {
        pid: 1234,
        pgid: 1234,
        started_at: '2026-04-17T13:40:00Z',
        package_dir: '/tmp/pkg',
        agent_command: 'scripts/agent-claude.sh',
        status: 'running',
      })
      const pauseOk = emptyResponse(204)
      const refresh = jsonResponse(200, {
        pid: 1234,
        pgid: 1234,
        started_at: '2026-04-17T13:40:00Z',
        paused_at: '2026-04-17T13:42:00Z',
        package_dir: '/tmp/pkg',
        agent_command: 'scripts/agent-claude.sh',
        status: 'paused',
      })
      // Mock order: poll-exec, poll-state, click→pause POST, refresh-exec, refresh-state.
      const fetchMock = mockFetch([
        poll,
        freshStateResponse(),
        pauseOk,
        refresh,
        freshStateResponse(),
      ])

      render(<StartExecutionCard sessionId="s1" emitted={true} />)
      await waitFor(() =>
        expect(screen.getByLabelText('Pause execution')).toBeInTheDocument(),
      )
      fireEvent.click(screen.getByLabelText('Pause execution'))
      await waitFor(() =>
        expect(fetchMock!.mock.calls[2]![0]).toBe(
          '/api/v1/chat/session/s1/execution/pause',
        ),
      )
      expect(fetchMock!.mock.calls[2]![1].method).toBe('POST')
    })

    it('POSTs /resume when Resume is clicked', async () => {
      const poll = jsonResponse(200, {
        pid: 1234,
        pgid: 1234,
        started_at: '2026-04-17T13:40:00Z',
        paused_at: '2026-04-17T13:42:00Z',
        package_dir: '/tmp/pkg',
        agent_command: 'scripts/agent-claude.sh',
        status: 'paused',
      })
      const ok = emptyResponse(204)
      const refresh = jsonResponse(200, {
        pid: 1234,
        pgid: 1234,
        started_at: '2026-04-17T13:40:00Z',
        package_dir: '/tmp/pkg',
        agent_command: 'scripts/agent-claude.sh',
        status: 'running',
      })
      const fetchMock = mockFetch([
        poll,
        freshStateResponse(),
        ok,
        refresh,
        freshStateResponse(),
      ])

      render(<StartExecutionCard sessionId="s1" emitted={true} />)
      await waitFor(() =>
        expect(screen.getByLabelText('Resume execution')).toBeInTheDocument(),
      )
      fireEvent.click(screen.getByLabelText('Resume execution'))
      await waitFor(() =>
        expect(fetchMock!.mock.calls[2]![0]).toBe(
          '/api/v1/chat/session/s1/execution/resume',
        ),
      )
    })

    it('POSTs /stop when Stop is clicked', async () => {
      const poll = jsonResponse(200, {
        pid: 1234,
        pgid: 1234,
        started_at: '2026-04-17T13:40:00Z',
        package_dir: '/tmp/pkg',
        agent_command: 'scripts/agent-claude.sh',
        status: 'running',
      })
      const ok = emptyResponse(204)
      const refresh = jsonResponse(200, {
        pid: 1234,
        pgid: 1234,
        started_at: '2026-04-17T13:40:00Z',
        stop_requested_at: '2026-04-17T13:45:00Z',
        package_dir: '/tmp/pkg',
        agent_command: 'scripts/agent-claude.sh',
        status: 'stopping',
      })
      const fetchMock = mockFetch([
        poll,
        freshStateResponse(),
        ok,
        refresh,
        freshStateResponse(),
      ])

      render(<StartExecutionCard sessionId="s1" emitted={true} />)
      await waitFor(() =>
        expect(
          screen.getByLabelText('Stop execution safely (preserves checkpoint for resume)'),
        ).toBeInTheDocument(),
      )
      fireEvent.click(screen.getByLabelText('Stop execution safely (preserves checkpoint for resume)'))
      await waitFor(() =>
        expect(fetchMock!.mock.calls[2]![0]).toBe(
          '/api/v1/chat/session/s1/execution/stop',
        ),
      )
    })
  })

  describe('force-kill confirmation modal', () => {
    it('opens the confirmation when Force-kill is clicked', async () => {
      mockFetch([
        jsonResponse(200, {
          pid: 1234,
          pgid: 1234,
          started_at: '2026-04-17T13:40:00Z',
          package_dir: '/tmp/pkg',
          agent_command: 'scripts/agent-claude.sh',
          status: 'running',
        }),
        freshStateResponse(),
      ])
      render(<StartExecutionCard sessionId="s1" emitted={true} />)
      await waitFor(() =>
        expect(
          screen.getByLabelText('Force-kill execution'),
        ).toBeInTheDocument(),
      )
      // No dialog yet.
      expect(screen.queryByRole('dialog')).toBeNull()
      fireEvent.click(screen.getByLabelText('Force-kill execution'))
      expect(
        screen.getByRole('dialog', { name: /confirm force-kill/i }),
      ).toBeInTheDocument()
    })

    it('closes the dialog without firing on Cancel', async () => {
      const fetchMock = mockFetch([
        jsonResponse(200, {
          pid: 1234,
          pgid: 1234,
          started_at: '2026-04-17T13:40:00Z',
          package_dir: '/tmp/pkg',
          agent_command: 'scripts/agent-claude.sh',
          status: 'running',
        }),
        freshStateResponse(),
      ])
      render(<StartExecutionCard sessionId="s1" emitted={true} />)
      await waitFor(() =>
        expect(
          screen.getByLabelText('Force-kill execution'),
        ).toBeInTheDocument(),
      )
      fireEvent.click(screen.getByLabelText('Force-kill execution'))
      fireEvent.click(screen.getByText('Cancel'))
      expect(screen.queryByRole('dialog')).toBeNull()
      // Only the initial poll happened (exec + state) — no kill.
      expect(fetchMock.mock.calls.length).toBe(2)
    })

    it('POSTs /kill on Yes, force kill', async () => {
      const poll = jsonResponse(200, {
        pid: 1234,
        pgid: 1234,
        started_at: '2026-04-17T13:40:00Z',
        package_dir: '/tmp/pkg',
        agent_command: 'scripts/agent-claude.sh',
        status: 'running',
      })
      const ok = emptyResponse(204)
      const refresh = jsonResponse(200, {
        pid: 1234,
        pgid: 1234,
        started_at: '2026-04-17T13:40:00Z',
        package_dir: '/tmp/pkg',
        agent_command: 'scripts/agent-claude.sh',
        status: 'exited',
        exit_code: 137,
      })
      const fetchMock = mockFetch([
        poll,
        freshStateResponse(),
        ok,
        refresh,
        freshStateResponse(),
      ])

      render(<StartExecutionCard sessionId="s1" emitted={true} />)
      await waitFor(() =>
        expect(
          screen.getByLabelText('Force-kill execution'),
        ).toBeInTheDocument(),
      )
      fireEvent.click(screen.getByLabelText('Force-kill execution'))
      fireEvent.click(screen.getByText('Yes, force kill'))
      await waitFor(() =>
        expect(fetchMock!.mock.calls[2]![0]).toBe(
          '/api/v1/chat/session/s1/execution/kill',
        ),
      )
      // Dialog dismisses after submit.
      await waitFor(() =>
        expect(screen.queryByRole('dialog')).toBeNull(),
      )
    })
  })
})
