// Coverage: shows "Loading…" before state lands; renders the snapshot
// as pretty-printed JSON once present.

import { render } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { StateTab } from './StateTab'
import type { SessionStateSnapshot } from '../../api/chatClient'

describe('StateTab', () => {
  it('renders Loading… when state is null', () => {
    const { getByText } = render(<StateTab state={null} />)
    expect(getByText('Loading…')).toBeInTheDocument()
  })

  it('serializes the snapshot to formatted JSON', () => {
    const state = {
      session_id: 's1',
      state: { kind: 'emitted' } as unknown as SessionStateSnapshot['state'],
      user_confirmed: true,
      last_activity: '2026-04-27T00:00:00Z',
      task_count: 3,
      progress: { completed: 1, ready: 2, blocked: 0, pending: 0 },
      title: null,
      parent_session_id: null,
      blocked_tasks: [], pending_input_hints: [],
    }
    const { container } = render(<StateTab state={state} />)
    const text = container.textContent ?? ''
    expect(text).toContain('"session_id": "s1"')
    expect(text).toContain('"task_count": 3')
  })
})
