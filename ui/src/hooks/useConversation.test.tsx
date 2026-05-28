// Race-condition tests for session-switching. Late-resolving fetches
// from a prior session must not overwrite the current session's state,
// and the 60s poll must merge results instead of clobbering optimistic
// local turns.
//
// Lives alongside useConversation.test.ts; that file covers the
// happy-path lifecycle, this one drives the session-switch + abort
// behaviour.

import { renderHook, act } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { useConversation } from './useConversation'
import * as chatClient from '../api/chatClient'
import type { SessionStateSnapshot } from '../api/chatClient'
import type { Turn } from '../types'

function makeTurn(role: 'user' | 'assistant' | 'system', content: string): Turn {
  return {
    turn_id: `turn-${role}-${content}`,
    role,
    content,
    intent: null,
    tool_calls: [],
    quick_replies: [],
    confirmation_card: null,
    timestamp: '2026-05-13T00:00:00Z',
  }
}

function makeState(sessionId: string): SessionStateSnapshot {
  return {
    session_id: sessionId,
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    state: { kind: 'greeting' } as any,
    user_confirmed: false,
    last_activity: '2026-05-13T00:00:00Z',
    task_count: 0,
    progress: { completed: 0, ready: 0, blocked: 0, pending: 0 },
  }
}

/** Resolve after `ms` ms — used to let pending promises drain. */
function delay(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms))
}

beforeEach(() => {
  // Reset the URL between tests without rebuilding the location object
  // (Object.defineProperty(window.location,...) breaks jsdom's
  // pushState same-origin check that switchToSession relies on).
  if (typeof window !== 'undefined') {
    window.history.replaceState(null, '', '/')
  }
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('useConversation – session-switch race (Phase 8 / RC-7)', () => {
  const SESS_A = '11111111-aaaa-bbbb-cccc-000000000001'
  const SESS_B = '22222222-aaaa-bbbb-cccc-000000000002'

  it('does not apply session A transcript after switching to session B', async () => {
    // Both sessions exist server-side; the test simulates A's fetch
    // landing *after* the user has already switched to B.
    const sessionATranscript = [makeTurn('assistant', 'A-greeting'), makeTurn('user', 'A-hello')]
    const sessionBTranscript = [makeTurn('assistant', 'B-greeting'), makeTurn('user', 'B-hello')]

    vi.spyOn(chatClient, 'createChatSession').mockResolvedValue({
      session_id: SESS_A,
      greeting: makeTurn('assistant', 'A-greeting'),
    })

    // A's transcript fetch is slow; B's resolves immediately. The
    // slow A fetch must NOT overwrite turns once B is the current
    // session.
    const transcriptSpy = vi
      .spyOn(chatClient, 'getChatTranscript')
      .mockImplementation(async (id) => {
        if (id === SESS_A) {
          await delay(80)
          return sessionATranscript
        }
        return sessionBTranscript
      })

    vi.spyOn(chatClient, 'getChatState').mockImplementation(async (id) =>
      makeState(id),
    )
    vi.spyOn(chatClient, 'getChatDag').mockResolvedValue(null)

    const { result } = renderHook(() => useConversation())
    await act(async () => {
      await result.current.start()
    })
    // We're attached to sess-A; turns has just the greeting at this
    // point because start() didn't fetch the transcript on the
    // fresh-session path.
    expect(result.current.sessionId).toBe(SESS_A)

    // Kick off the slow A transcript fetch (poll-style), then
    // immediately switch to B. Don't await A.
    await act(async () => {
      // The 60s poll path is where transcript-fetches that can race
      // with switching live. Trigger a manual resyncAll to drive the
      // race in a way the test can observe deterministically.
      const aFetch = result.current.resyncAll()
      // Switch sessions before A's fetch resolves.
      await result.current.switchToSession(SESS_B)
      // Wait long enough for A's delayed fetch to land.
      await delay(120)
      await aFetch.catch(() => {})
    })

    // Final session is B; A's late transcript must not have leaked
    // into the displayed turns.
    expect(result.current.sessionId).toBe(SESS_B)
    // The displayed turns must be B's, not A's.
    expect(result.current.turns.map((t) => t.content)).toEqual(
      sessionBTranscript.map((t) => t.content),
    )
    // Sanity: A's transcript fetch did fire.
    expect(transcriptSpy).toHaveBeenCalledWith(SESS_A, expect.anything())
    expect(transcriptSpy).toHaveBeenCalledWith(SESS_B, expect.anything())
  })

  it('60s poll merges into existing turns rather than replacing', async () => {
    vi.useFakeTimers()
    try {
      vi.spyOn(chatClient, 'createChatSession').mockResolvedValue({
        session_id: 'sess-merge',
        greeting: makeTurn('assistant', 'greeting'),
      })
      vi.spyOn(chatClient, 'getChatState').mockResolvedValue(makeState('sess-merge'))
      vi.spyOn(chatClient, 'getChatDag').mockResolvedValue(null)

      // The server-side transcript that the 60s poll returns. Note: it
      // does NOT yet include the optimistic user turn the SME just
      // submitted (server hasn't seen it yet, or the response was
      // captured before persist).
      const serverTranscript = [makeTurn('assistant', 'greeting')]
      vi.spyOn(chatClient, 'getChatTranscript').mockResolvedValue(serverTranscript)

      const { result } = renderHook(() => useConversation())
      await act(async () => {
        await result.current.start()
      })

      // SME types into the composer; appendTurn fires before the
      // server response arrives. (Simulating an optimistic local
      // user-turn append.)
      const optimisticUserTurn = makeTurn('user', 'optimistic-user-msg')
      act(() => {
        result.current.appendTurn(optimisticUserTurn)
      })
      expect(result.current.turns).toHaveLength(2)

      // Now fire the 60s poll. Without mergeBy this REPLACES turns
      // with serverTranscript and the optimistic user turn vanishes.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(60_000)
      })

      // After the poll, the optimistic user turn must still be
      // present.
      const contents = result.current.turns.map((t) => t.content)
      expect(contents).toContain('optimistic-user-msg')
      expect(contents).toContain('greeting')
    } finally {
      vi.useRealTimers()
    }
  })
})
