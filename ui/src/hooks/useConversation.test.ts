// Unit tests for the central session-lifecycle hook.
// Coverage: start (auto-create + ?session attach + ?share_token guard),
// sendTurn (optimistic + error path), confirm (auto-follow-up turn),
// appendTurn idempotence, switchToSession, staleness flagging.

import { renderHook, act, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { useConversation } from './useConversation'
import * as chatClient from '../api/chatClient'
import type { Turn } from '../types'

function makeTurn(role: 'user' | 'assistant' | 'system', content: string): Turn {
  return {
    turn_id: `turn-${role}-${content.slice(0, 6)}`,
    role,
    content,
    intent: null,
    tool_calls: [],
    quick_replies: [],
    confirmation_card: null,
    timestamp: '2026-04-27T00:00:00Z',
  }
}

beforeEach(() => {
  // Default URL — fresh session creation path.
  Object.defineProperty(window, 'location', {
    writable: true,
    value: { ...window.location, search: '', href: 'http://localhost/' } as Location,
  })
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('useConversation', () => {
  it('start() creates a fresh session when no ?session= is present', async () => {
    vi.spyOn(chatClient, 'createChatSession').mockResolvedValue({
      session_id: 'sess-A',
      greeting: makeTurn('assistant', 'hello'),
    })
    vi.spyOn(chatClient, 'getChatState').mockResolvedValue({
      session_id: 'sess-A',
      state: { kind: 'greeting' } as any,
      user_confirmed: false,
      last_activity: '2026-04-27T00:00:00Z',
      task_count: 0,
      progress: { completed: 0, ready: 0, blocked: 0, pending: 0 },
      title: null,
      parent_session_id: null,
      blocked_tasks: [], pending_input_hints: [],
    })

    const { result } = renderHook(() => useConversation())
    await act(async () => {
      await result.current.start()
    })

    expect(result.current.sessionId).toBe('sess-A')
    expect(result.current.turns).toHaveLength(1)
    expect(result.current.turns[0]!.content).toBe('hello')
  })

  it('start() attaches to ?session=<uuid> and hydrates transcript', async () => {
    Object.defineProperty(window, 'location', {
      writable: true,
      value: {
        ...window.location,
        search: '?session=11111111-2222-3333-4444-555555555555',
        href: 'http://localhost/?session=11111111-2222-3333-4444-555555555555',
      } as Location,
    })
    const transcript = [makeTurn('assistant', 'old greeting'), makeTurn('user', 'hi')]
    vi.spyOn(chatClient, 'getChatTranscript').mockResolvedValue(transcript)
    vi.spyOn(chatClient, 'getChatState').mockResolvedValue({
      session_id: '11111111-2222-3333-4444-555555555555',
      state: { kind: 'intake' } as any,
      user_confirmed: false,
      last_activity: '2026-04-27T00:00:00Z',
      task_count: 0,
      progress: { completed: 0, ready: 0, blocked: 0, pending: 0 },
      title: null,
      parent_session_id: null,
      blocked_tasks: [], pending_input_hints: [],
    })

    const { result } = renderHook(() => useConversation())
    await act(async () => {
      await result.current.start()
    })

    expect(result.current.sessionId).toBe('11111111-2222-3333-4444-555555555555')
    expect(result.current.turns).toHaveLength(2)
  })

  it('start() with ?share_token= and no ?session= sets a missing-session error', async () => {
    Object.defineProperty(window, 'location', {
      writable: true,
      value: {
        ...window.location,
        search: '?share_token=abc',
        href: 'http://localhost/?share_token=abc',
      } as Location,
    })

    const { result } = renderHook(() => useConversation())
    await act(async () => {
      await result.current.start()
    })

    expect(result.current.error).toMatch(/missing session id/i)
    expect(result.current.sessionId).toBeNull()
  })

  it('appendTurn() is idempotent on turn_id', () => {
    const { result } = renderHook(() => useConversation())
    const t = makeTurn('user', 'first')
    act(() => {
      result.current.appendTurn(t)
      result.current.appendTurn(t)
    })
    expect(result.current.turns).toHaveLength(1)
  })

  it('60s reconciliation poll fetches BOTH transcript and state', async () => {
    vi.useFakeTimers()
    try {
      vi.spyOn(chatClient, 'createChatSession').mockResolvedValue({
        session_id: 'sess-poll',
        greeting: makeTurn('assistant', 'hi'),
      })
      const initialState = {
        session_id: 'sess-poll',
        state: { kind: 'greeting' } as any,
        user_confirmed: false,
        last_activity: '2026-04-27T00:00:00Z',
        task_count: 0,
        progress: { completed: 0, ready: 0, blocked: 0, pending: 0 },
        title: null,
        parent_session_id: null,
        blocked_tasks: [], pending_input_hints: [],
      }
      const stateSpy = vi
        .spyOn(chatClient, 'getChatState')
        .mockResolvedValue(initialState)
      const transcriptSpy = vi
        .spyOn(chatClient, 'getChatTranscript')
        .mockResolvedValue([makeTurn('assistant', 'hi'), makeTurn('user', 'hello')])

      const { result } = renderHook(() => useConversation())
      await act(async () => {
        await result.current.start()
      })
      // Reset call counts to count only poll-driven fetches.
      stateSpy.mockClear()
      transcriptSpy.mockClear()

      // Now mid-conversation the server flips Emitted → Blocked
      // *during* an SSE drop. The next reconciliation tick should pick
      // it up without any user action — transcript AND state.
      const blockedState = {
        ...initialState,
        state: {
          kind: 'blocked',
          reason: 'awaiting SME',
          recovery_hint: '',
          blocker_kind: null,
          context: null,
        } as any,
      }
      stateSpy.mockResolvedValue(blockedState)

      await act(async () => {
        await vi.advanceTimersByTimeAsync(60_000)
      })

      // Both endpoints fetched on the same tick.
      expect(transcriptSpy).toHaveBeenCalledTimes(1)
      expect(stateSpy).toHaveBeenCalledTimes(1)
      // The blocker is now visible to the UI without a refresh.
      expect(result.current.state?.state.kind).toBe('blocked')
    } finally {
      vi.useRealTimers()
    }
  })

  it('staleSources tracks failed refreshState calls and clears on next success', async () => {
    vi.spyOn(chatClient, 'createChatSession').mockResolvedValue({
      session_id: 'sess-B',
      greeting: makeTurn('assistant', 'hi'),
    })
    const healthyState = {
      session_id: 'sess-B',
      state: { kind: 'greeting' } as any,
      user_confirmed: false,
      last_activity: '2026-04-27T00:00:00Z',
      task_count: 0,
      progress: { completed: 0, ready: 0, blocked: 0, pending: 0 },
      title: null,
      parent_session_id: null,
      blocked_tasks: [], pending_input_hints: [],
    }
    const stateSpy = vi
      .spyOn(chatClient, 'getChatState')
      .mockResolvedValueOnce(healthyState)

    const { result } = renderHook(() => useConversation())
    await act(async () => {
      await result.current.start()
    })
    await waitFor(() => {
      expect(result.current.sessionId).toBe('sess-B')
    })

    stateSpy.mockReset()
    stateSpy.mockRejectedValueOnce(new Error('boom'))
    await act(async () => {
      await result.current.refreshCurrentState()
    })

    await waitFor(() => {
      expect(result.current.staleSources.has('state')).toBe(true)
    })

    stateSpy.mockReset()
    stateSpy.mockResolvedValueOnce(healthyState)
    await act(async () => {
      await result.current.refreshCurrentState()
    })

    await waitFor(() => {
      expect(result.current.staleSources.has('state')).toBe(false)
    })
    expect(stateSpy).toHaveBeenCalledTimes(1)
  })

  // Synchronous busy-ref gating prevents double-click / rapid-Enter from
  // firing /confirm and /turn POSTs twice. setState alone is insufficient
  // because the next render-cycle event can fire before React commits the
  // state update — only useRef flipped BEFORE the await provides the
  // synchronous gate.
  it('double-click on confirm sends only one POST /confirm', async () => {
    vi.spyOn(chatClient, 'createChatSession').mockResolvedValue({
      session_id: 'sess-confirm',
      greeting: makeTurn('assistant', 'hi'),
    })
    vi.spyOn(chatClient, 'getChatState').mockResolvedValue({
      session_id: 'sess-confirm',
      state: { kind: 'greeting' } as any,
      user_confirmed: false,
      last_activity: '2026-04-27T00:00:00Z',
      task_count: 0,
      progress: { completed: 0, ready: 0, blocked: 0, pending: 0 },
      title: null,
      parent_session_id: null,
      blocked_tasks: [], pending_input_hints: [],
    })
    // Slow the confirm POST so the second click lands while the first
    // is still in flight — that's the race the fix is supposed to close.
    const confirmSpy = vi
      .spyOn(chatClient, 'confirmChatSession')
      .mockImplementation(
        () => new Promise<void>((resolve) => setTimeout(resolve, 50)),
      )
    vi.spyOn(chatClient, 'sendChatTurn').mockResolvedValue(
      makeTurn('assistant', 'continuing'),
    )

    const { result } = renderHook(() => useConversation())
    await act(async () => {
      await result.current.start()
    })

    await act(async () => {
      // Both calls fire BEFORE the first await on confirmChatSession
      // resolves — the synchronous useRef gate must already be set
      // when the second invocation enters confirm().
      const p1 = result.current.confirm()
      const p2 = result.current.confirm()
      await Promise.all([p1, p2])
    })

    expect(confirmSpy).toHaveBeenCalledTimes(1)
  })

  it('rapid sendTurn calls only POST /turn once', async () => {
    vi.spyOn(chatClient, 'createChatSession').mockResolvedValue({
      session_id: 'sess-rapid',
      greeting: makeTurn('assistant', 'hi'),
    })
    vi.spyOn(chatClient, 'getChatState').mockResolvedValue({
      session_id: 'sess-rapid',
      state: { kind: 'greeting' } as any,
      user_confirmed: false,
      last_activity: '2026-04-27T00:00:00Z',
      task_count: 0,
      progress: { completed: 0, ready: 0, blocked: 0, pending: 0 },
      title: null,
      parent_session_id: null,
      blocked_tasks: [], pending_input_hints: [],
    })
    // Slow the turn POST so the second invocation races with the first.
    const turnSpy = vi.spyOn(chatClient, 'sendChatTurn').mockImplementation(
      () =>
        new Promise((resolve) =>
          setTimeout(() => resolve(makeTurn('assistant', 'reply')), 50),
        ),
    )

    const { result } = renderHook(() => useConversation())
    await act(async () => {
      await result.current.start()
    })

    await act(async () => {
      const p1 = result.current.sendTurn('hello')
      const p2 = result.current.sendTurn('hello')
      await Promise.all([p1, p2])
    })

    expect(turnSpy).toHaveBeenCalledTimes(1)
  })

  it('reject and unblock guard against double-click', async () => {
    vi.spyOn(chatClient, 'createChatSession').mockResolvedValue({
      session_id: 'sess-other',
      greeting: makeTurn('assistant', 'hi'),
    })
    vi.spyOn(chatClient, 'getChatState').mockResolvedValue({
      session_id: 'sess-other',
      state: { kind: 'greeting' } as any,
      user_confirmed: false,
      last_activity: '2026-04-27T00:00:00Z',
      task_count: 0,
      progress: { completed: 0, ready: 0, blocked: 0, pending: 0 },
      title: null,
      parent_session_id: null,
      blocked_tasks: [], pending_input_hints: [],
    })
    const rejectSpy = vi
      .spyOn(chatClient, 'rejectChatSession')
      .mockImplementation(
        () => new Promise<void>((resolve) => setTimeout(resolve, 50)),
      )
    const unblockSpy = vi
      .spyOn(chatClient, 'unblockChatSession')
      .mockImplementation(
        () => new Promise<void>((resolve) => setTimeout(resolve, 50)),
      )

    const { result } = renderHook(() => useConversation())
    await act(async () => {
      await result.current.start()
    })

    await act(async () => {
      const r1 = result.current.reject()
      const r2 = result.current.reject()
      await Promise.all([r1, r2])
    })
    expect(rejectSpy).toHaveBeenCalledTimes(1)

    await act(async () => {
      const u1 = result.current.unblock('retry')
      const u2 = result.current.unblock('retry')
      await Promise.all([u1, u2])
    })
    expect(unblockSpy).toHaveBeenCalledTimes(1)
  })

  it('a slow late-finishing turn does not clear stillThinking for a fresh turn', async () => {
    vi.useFakeTimers()
    try {
      vi.spyOn(chatClient, 'createChatSession').mockResolvedValue({
        session_id: 'sess-token',
        greeting: makeTurn('assistant', 'hi'),
      })
      vi.spyOn(chatClient, 'getChatState').mockResolvedValue({
        session_id: 'sess-token',
        state: { kind: 'greeting' } as any,
        user_confirmed: false,
        last_activity: '2026-04-27T00:00:00Z',
        task_count: 0,
        progress: { completed: 0, ready: 0, blocked: 0, pending: 0 },
        title: null,
        parent_session_id: null,
        blocked_tasks: [], pending_input_hints: [],
      })

      // First turn settles immediately; second turn is slow. The token
      // gate guarantees that each turn's finally block only clears the
      // still-thinking timer when ITS token is the current one — so a
      // late-resolving sendTurn from a prior wave can't pre-clear the
      // current wave's pending timer. (Closes M1 cross-turn timer leak.)
      const turnSpy = vi
        .spyOn(chatClient, 'sendChatTurn')
        .mockResolvedValueOnce(makeTurn('assistant', 'fast'))
        .mockImplementationOnce(
          () =>
            new Promise((resolve) =>
              setTimeout(() => resolve(makeTurn('assistant', 'slow')), 30_000),
            ),
        )

      const { result } = renderHook(() => useConversation())
      await act(async () => {
        await result.current.start()
      })

      // First turn — finishes quickly, finally clears its own timer.
      await act(async () => {
        await result.current.sendTurn('first')
      })

      // Second turn — fire it and advance past the 8s still-thinking
      // threshold without awaiting. stillThinking should flip true
      // because the second turn's timer is still live.
      await act(async () => {
        void result.current.sendTurn('second')
        await vi.advanceTimersByTimeAsync(9_000)
      })

      expect(result.current.stillThinking).toBe(true)
      expect(turnSpy).toHaveBeenCalledTimes(2)
    } finally {
      vi.useRealTimers()
    }
  })
})
