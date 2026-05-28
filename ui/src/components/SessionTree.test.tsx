import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import SessionTree from './SessionTree'

type ChildFixture = {
  session_id: string
  created_at: string
  lineage: {
    parent_session_id: string
    branched_at: string
    branched_from_turn_index: number | null
  } | null
  state_kind: string
}

function mockFetchWithChildren(children: ChildFixture[]) {
  const impl = vi.fn((_input: RequestInfo | URL) =>
    Promise.resolve({
      ok: true,
      status: 200,
      json: () => Promise.resolve(children),
      text: () => Promise.resolve(''),
    } as unknown as Response),
  )
  vi.stubGlobal('fetch', impl)
  return impl
}

beforeEach(() => {
  vi.useRealTimers()
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('SessionTree', () => {
  it('renders nothing when currentSessionId is null', () => {
    mockFetchWithChildren([])
    const { container } = render(
      <SessionTree currentSessionId={null} onSelectSession={vi.fn()} />,
    )
    expect(container.firstChild).toBeNull()
  })

  it('renders only the current-session pill when fetch returns no children', async () => {
    mockFetchWithChildren([])
    render(
      <SessionTree
        currentSessionId="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        onSelectSession={vi.fn()}
      />,
    )
    // Current pill is always rendered immediately.
    expect(screen.getByText('aaaaaaaa')).toBeInTheDocument()
    expect(screen.getByText('Root')).toBeInTheDocument()
    // Once the empty-children fetch resolves, the "branching creates
    // siblings" hint appears.
    await waitFor(() =>
      expect(
        screen.getByText(/Branching creates siblings/i),
      ).toBeInTheDocument(),
    )
  })

  it('renders child pills when fetch returns children', async () => {
    const branchedAt = new Date(Date.now() - 2 * 60 * 60 * 1000).toISOString()
    mockFetchWithChildren([
      {
        session_id: '11111111-2222-3333-4444-555555555555',
        created_at: branchedAt,
        lineage: {
          parent_session_id: 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
          branched_at: branchedAt,
          branched_from_turn_index: 3,
        },
        state_kind: 'intake',
      },
    ])
    render(
      <SessionTree
        currentSessionId="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        onSelectSession={vi.fn()}
      />,
    )
    await waitFor(() =>
      expect(screen.getByText('11111111')).toBeInTheDocument(),
    )
    expect(screen.getByText('Branch')).toBeInTheDocument()
  })

  it('clicking a non-current pill fires onSelectSession with the right id', async () => {
    const branchedAt = new Date(Date.now() - 5 * 60 * 1000).toISOString()
    mockFetchWithChildren([
      {
        session_id: '11111111-2222-3333-4444-555555555555',
        created_at: branchedAt,
        lineage: {
          parent_session_id: 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
          branched_at: branchedAt,
          branched_from_turn_index: null,
        },
        state_kind: 'intake',
      },
    ])
    const onSelectSession = vi.fn()
    render(
      <SessionTree
        currentSessionId="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        onSelectSession={onSelectSession}
      />,
    )
    const user = userEvent.setup()
    const childPill = await screen.findByText('11111111')
    await user.click(childPill.closest('button') as HTMLButtonElement)
    expect(onSelectSession).toHaveBeenCalledWith(
      '11111111-2222-3333-4444-555555555555',
    )
  })

  it('clicking the current-session pill does not fire onSelectSession', async () => {
    mockFetchWithChildren([])
    const onSelectSession = vi.fn()
    render(
      <SessionTree
        currentSessionId="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        onSelectSession={onSelectSession}
      />,
    )
    const user = userEvent.setup()
    const currentPill = screen.getByText('aaaaaaaa').closest('button') as HTMLButtonElement
    await user.click(currentPill)
    expect(onSelectSession).not.toHaveBeenCalled()
  })

  it('marks the current pill with aria-current="page" and others with no aria-current', async () => {
    const branchedAt = new Date(Date.now() - 90 * 1000).toISOString()
    mockFetchWithChildren([
      {
        session_id: '11111111-2222-3333-4444-555555555555',
        created_at: branchedAt,
        lineage: {
          parent_session_id: 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
          branched_at: branchedAt,
          branched_from_turn_index: 1,
        },
        state_kind: 'intake',
      },
    ])
    render(
      <SessionTree
        currentSessionId="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        onSelectSession={vi.fn()}
      />,
    )
    const currentPill = screen
      .getByText('aaaaaaaa')
      .closest('button') as HTMLButtonElement
    expect(currentPill.getAttribute('aria-current')).toBe('page')
    const childPill = (await screen.findByText('11111111')).closest(
      'button',
    ) as HTMLButtonElement
    expect(childPill.getAttribute('aria-current')).toBeNull()
  })

  it('renders branched_at as a relative time string', async () => {
    // 2 hours ago → "2h ago"
    const branchedAt = new Date(Date.now() - 2 * 60 * 60 * 1000).toISOString()
    mockFetchWithChildren([
      {
        session_id: '11111111-2222-3333-4444-555555555555',
        created_at: branchedAt,
        lineage: {
          parent_session_id: 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
          branched_at: branchedAt,
          branched_from_turn_index: 2,
        },
        state_kind: 'intake',
      },
    ])
    render(
      <SessionTree
        currentSessionId="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        onSelectSession={vi.fn()}
      />,
    )
    await waitFor(() => expect(screen.getByText('2 hours ago')).toBeInTheDocument())
  })

  it('has <nav aria-label="Session tree"> landmark', async () => {
    mockFetchWithChildren([])
    render(
      <SessionTree
        currentSessionId="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        onSelectSession={vi.fn()}
      />,
    )
    expect(
      screen.getByRole('navigation', { name: /session tree/i }),
    ).toBeInTheDocument()
    // Let the fetch effect settle so React doesn't warn about an
    // unwrapped state update after test teardown.
    await waitFor(() =>
      expect(
        screen.getByText(/Branching creates siblings/i),
      ).toBeInTheDocument(),
    )
  })

  it('walks the parent chain and renders Root → Parent → Current', async () => {
    // Mock both endpoints:
    //   GET /api/chat/sessions?parent=<branch>      → []  (no further children)
    //   GET /api/chat/session/<parent>/state        → {parent_session_id: <grand>}
    //   GET /api/chat/session/<grand>/state         → {parent_session_id: null}
    const branchId = 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb'
    const parentId = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa'
    const grandparentId = '99999999-9999-9999-9999-999999999999'
    const impl = vi.fn((input: RequestInfo | URL) => {
      const url = typeof input === 'string' ? input : input.toString()
      let body: unknown = []
      if (url.includes(`/session/${parentId}/state`)) {
        body = { state: { kind: 'emitted' }, parent_session_id: grandparentId }
      } else if (url.includes(`/session/${grandparentId}/state`)) {
        body = { state: { kind: 'emitted' }, parent_session_id: null }
      } else if (url.includes('/sessions?parent=')) {
        body = []
      }
      return Promise.resolve({
        ok: true,
        status: 200,
        json: () => Promise.resolve(body),
        text: () => Promise.resolve(''),
      } as unknown as Response)
    })
    vi.stubGlobal('fetch', impl)

    render(
      <SessionTree
        currentSessionId={branchId}
        parentSessionId={parentId}
        onSelectSession={vi.fn()}
      />,
    )

    // Once the walk resolves, the grandparent (root) and parent appear as pills.
    await waitFor(() =>
      expect(screen.getByText(grandparentId.slice(0, 8))).toBeInTheDocument(),
    )
    await waitFor(() =>
      expect(screen.getByText(parentId.slice(0, 8))).toBeInTheDocument(),
    )
    // Current branch still rendered.
    expect(screen.getByText(branchId.slice(0, 8))).toBeInTheDocument()
    // Root label should be on the oldest ancestor.
    expect(screen.getAllByText('Root').length).toBeGreaterThanOrEqual(1)
  })

  it('caps the ancestor walk at MAX_ANCESTOR_HOPS and bails on cycles', async () => {
    // Server returns a self-cycle (parent points back to itself) — the
    // walk must terminate, not loop forever.
    const branchId = 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb'
    const cyclicParent = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa'
    const impl = vi.fn((input: RequestInfo | URL) => {
      const url = typeof input === 'string' ? input : input.toString()
      let body: unknown = []
      if (url.includes(`/session/${cyclicParent}/state`)) {
        body = { state: { kind: 'emitted' }, parent_session_id: cyclicParent }
      } else if (url.includes('/sessions?parent=')) {
        body = []
      }
      return Promise.resolve({
        ok: true,
        status: 200,
        json: () => Promise.resolve(body),
        text: () => Promise.resolve(''),
      } as unknown as Response)
    })
    vi.stubGlobal('fetch', impl)

    render(
      <SessionTree
        currentSessionId={branchId}
        parentSessionId={cyclicParent}
        onSelectSession={vi.fn()}
      />,
    )

    // The cyclic parent appears once; the walk bails on the second sighting.
    await waitFor(() =>
      expect(screen.getAllByText(cyclicParent.slice(0, 8)).length).toBe(1),
    )
  })
})
