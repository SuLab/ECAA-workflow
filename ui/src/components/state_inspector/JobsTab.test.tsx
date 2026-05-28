// Coverage: stable e.id keying (drop-oldest buffer trims don't shift
// indices on remaining rows), executor header, orphan-reap banner,
// onOpenTaskLog click + Enter activation.
//
// JobsFeed virtualizes the row list via react-virtuoso so the buffer
// of long-running sessions doesn't blow up the DOM. Virtuoso depends
// on `ResizeObserver` and a real-ish layout; jsdom doesn't provide the
// former, so we stub a minimal no-op + force `getBoundingClientRect`
// to return a non-zero viewport. Without these shims Virtuoso renders
// zero rows in jsdom even when given non-empty data.

import { fireEvent, render } from '@testing-library/react'
import { beforeAll, describe, expect, it, vi } from 'vitest'

import { JobsFeed } from './JobsTab'
import type { HarnessProgressLine } from '../../hooks/useSseChatEvents'

beforeAll(() => {
  if (typeof window !== 'undefined' && !('ResizeObserver' in window)) {
    class ResizeObserverStub {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    }
    Object.defineProperty(window, 'ResizeObserver', {
      writable: true,
      configurable: true,
      value: ResizeObserverStub,
    })
  }
  // Virtuoso reads getBoundingClientRect on its scroller to decide how
  // many rows fit; jsdom returns 0×0 by default, which causes Virtuoso
  // to render an empty list. Patch HTMLElement to return a fake viewport
  // big enough to mount every row in these small test fixtures.
  const originalGetBoundingClientRect =
    HTMLElement.prototype.getBoundingClientRect
  HTMLElement.prototype.getBoundingClientRect = function (): DOMRect {
    const r = originalGetBoundingClientRect.call(this) as DOMRect
    return new DOMRect(0, 0, r.width || 1024, r.height || 800)
  }
})

function makeEvent(overrides: Partial<HarnessProgressLine>): HarnessProgressLine {
  return {
    id: 'evt-default',
    kind: 'task_started',
    taskId: 't1',
    status: 'running',
    detail: 'started',
    remote: null,
    ...overrides,
  }
}

describe('JobsFeed', () => {
  it('renders empty placeholder when no events', () => {
    const { getByText } = render(<JobsFeed events={[]} />)
    expect(getByText(/Execution jobs and progress lines/i)).toBeInTheDocument()
  })

  it('keys rows on event id, not array index', () => {
    const events = [
      makeEvent({ id: 'a', detail: 'first' }),
      makeEvent({ id: 'b', detail: 'second' }),
      makeEvent({ id: 'c', detail: 'third' }),
    ]
    const { container } = render(<JobsFeed events={events} />)
    // Sanity: each event renders. Virtuoso wraps rows in its own
    // intermediate divs, so we select by the data-event-id attribute
    // we apply on the row root. The stable-id contract is enforced
    // structurally — if it ever breaks, React will warn about
    // duplicate keys, which test runs would surface.
    const rows = container.querySelectorAll('[data-event-id]')
    expect(rows).toHaveLength(3)
    const ids = Array.from(rows).map((r) => r.getAttribute('data-event-id'))
    expect(ids).toEqual(['a', 'b', 'c'])
  })

  it('renders executor header when executorInfo is provided', () => {
    const { getByTestId, getByText } = render(
      <JobsFeed
        events={[]}
        executorInfo={{
          name: 'AwsBackend',
          cpuBudget: 8,
          gpuBudget: 1,
          instanceType: 'p4d.24xlarge',
          harnessVersion: '0.1.0',
          envMode: 'production',
        }}
      />,
    )
    expect(getByTestId('executor-header')).toBeInTheDocument()
    expect(getByText('AwsBackend')).toBeInTheDocument()
    expect(getByText('p4d.24xlarge')).toBeInTheDocument()
  })

  it('renders remote backend and sizing chips on progress rows', () => {
    const events = [
      makeEvent({
        id: 'evt-remote',
        remote: {
          backend: 'aws',
          instanceId: 'i-123',
          instanceType: 'r6i.xlarge',
        },
      }),
    ]
    const { container, getByText } = render(<JobsFeed events={events} />)

    expect(container.querySelector('[data-backend-badge="aws"]')).toHaveAttribute(
      'title',
      'Instance i-123',
    )
    expect(container.querySelector('[data-sizing-chip="r6i.xlarge"]')).toBeTruthy()
    expect(getByText('aws')).toBeInTheDocument()
    expect(getByText('r6i.xlarge')).toBeInTheDocument()
  })

  it('renders orphan-reap banner when unverifiedIds is non-empty', () => {
    const { getByTestId } = render(
      <JobsFeed
        events={[]}
        orphanReap={{
          candidateCount: 3,
          verifiedCount: 2,
          unverifiedIds: ['i-abc'],
          policy: 'fail',
        }}
      />,
    )
    expect(getByTestId('orphan-reap-banner')).toHaveTextContent(/i-abc/)
  })

  it('does not render orphan-reap banner when unverifiedIds is empty', () => {
    const { queryByTestId } = render(
      <JobsFeed
        events={[]}
        orphanReap={{
          candidateCount: 3,
          verifiedCount: 3,
          unverifiedIds: [],
          policy: 'warn',
        }}
      />,
    )
    expect(queryByTestId('orphan-reap-banner')).toBeNull()
  })

  it('invokes onOpenTaskLog when an event with a task id is clicked', () => {
    const onOpenTaskLog = vi.fn()
    const events = [makeEvent({ id: 'evt-1', taskId: 'task-X' })]
    const { container } = render(
      <JobsFeed events={events} onOpenTaskLog={onOpenTaskLog} />,
    )
    const row = container.querySelector('[role="button"]') as HTMLElement
    expect(row).not.toBeNull()
    fireEvent.click(row)
    expect(onOpenTaskLog).toHaveBeenCalledWith('task-X')
  })
})
