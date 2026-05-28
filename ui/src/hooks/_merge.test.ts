// Unit tests for the `mergeBy` helper used by `useConversation`'s
// 60s reconciliation poll.
//
// The contract is field-level reconciliation: for every item present
// in both lists, spread server fields over the local ones. An
// append-only merge would drop server-side mutations to items already
// in `local` — e.g. when the server fills in `tool_calls` / `intent` /
// `confirmation_card` AFTER the assistant turn first arrived (a slow
// side-call resolving and updating the persisted turn).

import { describe, expect, it } from 'vitest'
import { mergeBy } from './_merge'

describe('mergeBy', () => {
  it('preserves local-only items', () => {
    const local = [{ turn_id: '1', content: 'a' }]
    const remote: { turn_id: string; content: string }[] = []
    expect(mergeBy(local, remote, 'turn_id')).toEqual(local)
  })

  it('uses remote order when remote has new items', () => {
    // Remote is the server's persisted chronology and is canonical.
    // Local-only items (here: turn 1) are kept after remote order so
    // the persisted sequence renders chronologically.
    const local = [{ turn_id: '1', content: 'a' }]
    const remote = [{ turn_id: '2', content: 'b' }]
    expect(mergeBy(local, remote, 'turn_id')).toEqual([
      { turn_id: '2', content: 'b' },
      { turn_id: '1', content: 'a' },
    ])
  })

  it('does not duplicate items present in both lists (key match)', () => {
    const local = [{ turn_id: '1', content: 'a' }]
    const remote = [{ turn_id: '1', content: 'a' }]
    const merged = mergeBy(local, remote, 'turn_id')
    expect(merged).toHaveLength(1)
  })

  it('merges server-side mutations to existing items', () => {
    // The key regression test: the server has filled in tool_calls and
    // Intent AFTER the assistant turn first. The append-only
    // mergeBy would drop these mutations because the turn_id was
    // already in `local`.
    const local = [
      { turn_id: '1', tool_calls: [] as Array<{ name: string }>, intent: null as string | null },
    ]
    const remote = [
      {
        turn_id: '1',
        tool_calls: [{ name: 'foo' }],
        intent: 'bar' as string | null,
      },
    ]
    const merged = mergeBy(local, remote, 'turn_id')
    expect(merged).toHaveLength(1)
    expect(merged[0]!.tool_calls).toEqual([{ name: 'foo' }])
    expect(merged[0]!.intent).toBe('bar')
  })

  it('preserves local order when both lists share a prefix', () => {
    const local = [
      { turn_id: '1', content: 'a' },
      { turn_id: '2', content: 'b-local' },
    ]
    const remote = [
      { turn_id: '1', content: 'a' },
      { turn_id: '2', content: 'b-server' },
      { turn_id: '3', content: 'c' },
    ]
    const merged = mergeBy(local, remote, 'turn_id')
    expect(merged.map((m) => m.turn_id)).toEqual(['1', '2', '3'])
    // Server is source of truth on the shared item.
    expect(merged[1]!.content).toBe('b-server')
  })

  it('handles empty local', () => {
    const local: Array<{ turn_id: string }> = []
    const remote = [{ turn_id: '1' }, { turn_id: '2' }]
    expect(mergeBy(local, remote, 'turn_id')).toEqual(remote)
  })

  it('handles empty remote', () => {
    const local = [{ turn_id: '1' }, { turn_id: '2' }]
    const remote: Array<{ turn_id: string }> = []
    expect(mergeBy(local, remote, 'turn_id')).toEqual(local)
  })

  it('uses remote order when SSE delivered an assistant turn before the user turn lands locally', () => {
    // The chat-ordering regression. SSE `turn_appended` raced ahead of
    // the persisted user turn into local state (sibling-tab POST, test
    // harness fetch, harness-as-SME injection). The append-end rule
    // shoved the missing user turn after the assistant reply that
    // answered it — user sees "Hi greeting → AI clarification → User
    // prompt" instead of "Hi → User prompt → AI clarification."
    const local = [
      { turn_id: 'greeting', role: 'assistant', content: 'Hi!' },
      { turn_id: 'asst-reply', role: 'assistant', content: 'Got it.' },
    ]
    const remote = [
      { turn_id: 'greeting', role: 'assistant', content: 'Hi!' },
      { turn_id: 'user-msg', role: 'user', content: 'bulk RNA-seq…' },
      { turn_id: 'asst-reply', role: 'assistant', content: 'Got it.' },
    ]
    const merged = mergeBy(local, remote, 'turn_id')
    expect(merged.map((m) => m.turn_id)).toEqual([
      'greeting',
      'user-msg',
      'asst-reply',
    ])
  })

  it('keeps the user-bubble optimistic append at the end while server still processing', () => {
    // The other direction. The SME just typed; the optimistic append
    // is in local but not yet on the server. Local-only items must
    // survive (they're appended after the canonical remote
    // chronology in their original local order).
    const local = [
      { turn_id: 'greeting' },
      { turn_id: 'optimistic-user', _local_only: true },
    ]
    const remote = [{ turn_id: 'greeting' }]
    const merged = mergeBy(local, remote, 'turn_id')
    expect(merged.map((m) => m.turn_id)).toEqual([
      'greeting',
      'optimistic-user',
    ])
  })
})
