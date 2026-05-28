// Verify the client-provided `user_turn_id` flows end-to-end so the
// 60s reconciliation poll dedupes the optimistic user turn against the
// server-canonical user turn via `mergeBy(turn_id)`.
//
// The client mints the id (`crypto.randomUUID()`), passes it via
// `userTurnId`, the server uses it for the persisted Turn, and the
// reconciliation path collapses to one bubble. If the server minted a
// fresh UUID instead, the optimistic UUID and the persisted UUID would
// never match: after a poll tick the transcript fetch would APPEND the
// server-side user turn rather than merge with the local optimistic
// append, leaving duplicate user-turn bubbles.

import { renderHook, act, waitFor } from '@testing-library/react'
import { describe, expect, it, vi, beforeEach } from 'vitest'
import { useConversation } from './useConversation'
import * as chatClient from '../api/chatClient'

describe('useConversation user-turn dedup', () => {
  beforeEach(() => {
    vi.restoreAllMocks()
    // Reset URL so start() takes the fresh-session path rather than
    // attaching to a `?session=` query.
    if (typeof window !== 'undefined') {
      window.history.replaceState(null, '', '/')
    }
  })

  it('60s reconciliation poll does not duplicate the user turn when client + server share user_turn_id', async () => {
    const sessionId = 'sess-1'
    const userText = 'hello'
    // Server-side createChatSession returns a Greeting only
    vi.spyOn(chatClient, 'createChatSession').mockResolvedValue({
      session_id: sessionId,
      greeting: {
        turn_id: 'greet-1',
        role: 'assistant',
        content: 'Hi!',
        intent: null,
        tool_calls: [],
        quick_replies: [],
        confirmation_card: null,
        timestamp: new Date().toISOString(),
      },
    } as any)
    // sendChatTurn must echo back an assistant turn AND use the
    // client-provided user_turn_id for the server-side user turn.
    // We capture the client-supplied user_turn_id and stash it for
    // the later transcript mock.
    let capturedUserTurnId: string | undefined
    vi.spyOn(chatClient, 'sendChatTurn').mockImplementation(
      async (_sid: string, _msg: string, opts?: { userTurnId?: string }) => {
        capturedUserTurnId = opts?.userTurnId
        return {
          turn_id: 'assist-1',
          role: 'assistant',
          content: 'reply',
          intent: null,
          tool_calls: [],
          quick_replies: [],
          confirmation_card: null,
          timestamp: new Date().toISOString(),
        } as any
      },
    )
    // refreshState / loadDag noops
    vi.spyOn(chatClient, 'getChatState').mockResolvedValue({
      session_id: sessionId,
      state: { kind: 'intake_followup' },
      user_confirmed: false,
      last_activity: new Date().toISOString(),
      task_count: 0,
      progress: { completed: 0, ready: 0, blocked: 0, pending: 0 },
    } as any)
    vi.spyOn(chatClient, 'getChatDag').mockResolvedValue(null as any)

    const { result } = renderHook(() => useConversation())
    await act(async () => {
      await result.current.start()
    })
    await waitFor(() => expect(result.current.sessionId).toBe(sessionId))
    await act(async () => {
      await result.current.sendTurn(userText)
    })

    expect(capturedUserTurnId).toBeTruthy()
    // Now simulate the 60s reconciliation poll arriving with the
    // canonical transcript: greeting + user turn (SAME id) + assist.
    vi.spyOn(chatClient, 'getChatTranscript').mockResolvedValue([
      {
        turn_id: 'greet-1',
        role: 'assistant',
        content: 'Hi!',
      },
      {
        turn_id: capturedUserTurnId!,
        role: 'user',
        content: userText,
      },
      {
        turn_id: 'assist-1',
        role: 'assistant',
        content: 'reply',
      },
    ] as any)
    await act(async () => {
      await result.current.resyncAll()
    })
    const userTurns = result.current.turns.filter((t) => t.role === 'user')
    expect(userTurns).toHaveLength(1)
    expect(userTurns[0]!.turn_id).toBe(capturedUserTurnId)
  })
})
