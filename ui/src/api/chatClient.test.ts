import { afterEach, describe, expect, it, vi } from 'vitest'
import { postBranch, unblockChatSession } from './chatClient'

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('postBranch', () => {
  it('normalizes the server branched_session_id response for UI navigation', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ branched_session_id: 'child-123' }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
      ),
    )

    await expect(
      postBranch('parent-123', { rationale: 'try downstream branch', taskId: 'normalisation' }),
    ).resolves.toEqual({ session_id: 'child-123' })

    const call = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0]!
    expect(call[0]).toBe('/api/v1/chat/session/parent-123/branch')
    expect(JSON.parse(call[1].body)).toEqual({
      rationale: 'try downstream branch',
      task_id: 'normalisation',
    })
  })
})

describe('unblockChatSession', () => {
  it('sends task-scoped retry guidance to the unblock endpoint', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }))
    vi.stubGlobal('fetch', fetchMock)

    await unblockChatSession('session-123', {
      rationale: 'Retry with two independently verified pathway sources.',
      taskId: 'survey_method_landscape',
    })

    const call = fetchMock.mock.calls[0]!
    expect(call[0]).toBe('/api/v1/chat/session/session-123/unblock')
    expect(JSON.parse(call[1].body)).toEqual({
      resolution: null,
      rationale: 'Retry with two independently verified pathway sources.',
      task_id: 'survey_method_landscape',
    })
  })
})
