// WS-D7 (L9) — SSE reconnect dedup edge.
//
// The pre-WS-D code reset lastSeq to 0 on reconnect and skipped the gap
// check on the first post-reconnect event (lastSeq === 0), so a server
// that resumed at a seq FAR above the pre-reconnect high-water mark
// (events dropped during the disconnect) only emitted the onopen
// `dropped: 0` floor — never a resync reflecting the real gap. This test
// pins the hardened behavior: a high first-seq after a RECONNECT fires a
// resync_required carrying the accurate dropped count, while a first-ever
// high seq (mid-stream join) and a server counter reset (low first-seq)
// do NOT spuriously over-fire.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ChatSseEvent } from './chatStream'
import { connectChatStream } from './chatStream'

class FakeEventSource {
  url: string
  readyState = 0
  onopen: (() => void) | null = null
  onmessage: ((msg: MessageEvent) => void) | null = null
  onerror: (() => void) | null = null
  closed = false
  static last: FakeEventSource | null = null

  constructor(url: string) {
    this.url = url
    FakeEventSource.last = this
  }
  close() {
    this.closed = true
  }
}

const original = (globalThis as unknown as { EventSource: unknown }).EventSource

beforeEach(() => {
  (globalThis as unknown as { EventSource: unknown }).EventSource =
    FakeEventSource as unknown
  FakeEventSource.last = null
})

afterEach(() => {
  (globalThis as unknown as { EventSource: unknown }).EventSource = original
})

function msg(seq: number, text: string): MessageEvent {
  return new MessageEvent('message', {
    data: JSON.stringify({ seq, type: 'assistant_token_delta', text }),
  })
}

describe('chatStream reconnect dedup (WS-D L9)', () => {
  it('fires resync_required with the real dropped count on a high first-seq after reconnect', () => {
    const onEvent = vi.fn<(e: ChatSseEvent) => void>()
    connectChatStream('sess-wsd-1', onEvent)
    const es = FakeEventSource.last!

    // Initial connect + a small run of events (lastSeq advances to 3).
    es.onopen?.()
    es.onmessage?.(msg(1, 'a'))
    es.onmessage?.(msg(2, 'b'))
    es.onmessage?.(msg(3, 'c'))

    // Drop + auto-reconnect; the server resumes at seq 50, meaning a large
    // run (4..49) was missed while we were disconnected.
    es.onerror?.()
    es.onopen?.()
    es.onmessage?.(msg(50, 'resumed'))

    const resyncs = onEvent.mock.calls
      .map((c) => c[0])
      .filter((e) => e.type === 'resync_required')

    // At least one resync must carry the accurate gap (50 - 3 - 1 = 46).
    expect(resyncs.some((e) => 'dropped' in e && e.dropped === 46)).toBe(true)

    // The resumed event itself is still forwarded.
    expect(onEvent).toHaveBeenCalledWith({
      seq: 50,
      type: 'assistant_token_delta',
      text: 'resumed',
    })
  })

  it('does NOT fire a gap resync when the server counter resets low after reconnect', () => {
    const onEvent = vi.fn<(e: ChatSseEvent) => void>()
    connectChatStream('sess-wsd-2', onEvent)
    const es = FakeEventSource.last!

    es.onopen?.()
    es.onmessage?.(msg(10, 'before'))

    es.onerror?.()
    es.onopen?.()
    es.onmessage?.(msg(1, 'after')) // server counter reset: 1 < 10, not a gap

    const gapResyncs = onEvent.mock.calls
      .map((c) => c[0])
      .filter((e) => e.type === 'resync_required' && 'dropped' in e && e.dropped > 0)

    // Only the onopen floor (dropped: 0) may fire — never a positive-gap
    // resync, because a low resumed seq is a counter reset, not a drop.
    expect(gapResyncs).toHaveLength(0)
    expect(onEvent).toHaveBeenCalledWith({
      seq: 1,
      type: 'assistant_token_delta',
      text: 'after',
    })
  })

  it('does NOT fire any resync on a high first-ever seq (mid-stream join, no prior connection)', () => {
    const onEvent = vi.fn<(e: ChatSseEvent) => void>()
    connectChatStream('sess-wsd-3', onEvent)
    const es = FakeEventSource.last!

    // First-ever connection: a high seq means we joined mid-stream, not a
    // drop. No resync should fire.
    es.onopen?.()
    es.onmessage?.(msg(42, 'mid'))

    const resyncs = onEvent.mock.calls
      .map((c) => c[0])
      .filter((e) => e.type === 'resync_required')
    expect(resyncs).toHaveLength(0)
    expect(onEvent).toHaveBeenCalledWith({
      seq: 42,
      type: 'assistant_token_delta',
      text: 'mid',
    })
  })
})
