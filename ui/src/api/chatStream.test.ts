// Unit tests for connectChatStream's reconnect-detection behavior.
// jsdom doesn't ship a native EventSource so we stub it on globalThis
// for these tests; the stub captures the constructed instance and its
// handlers so the test can drive onopen/onmessage/onerror manually.

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

describe('connectChatStream — reconnect detection', () => {
  it('does NOT fire resync_required on the first onopen (initial subscribe)', () => {
    const onEvent = vi.fn<(e: ChatSseEvent) => void>()
    connectChatStream('sess-1', onEvent)
    const es = FakeEventSource.last!
    expect(es.url).toBe('/api/v1/chat/session/sess-1/events')

    es.onopen?.()
    expect(onEvent).not.toHaveBeenCalled()
  })

  it('fires synthetic resync_required on the second+ onopen (reconnect)', () => {
    const onEvent = vi.fn<(e: ChatSseEvent) => void>()
    connectChatStream('sess-2', onEvent)
    const es = FakeEventSource.last!

    es.onopen?.() // initial
    expect(onEvent).not.toHaveBeenCalled()

    // simulated drop + browser auto-reconnect
    es.onerror?.()
    es.onopen?.()
    expect(onEvent).toHaveBeenCalledTimes(1)
    expect(onEvent).toHaveBeenCalledWith({ type: 'resync_required', dropped: 0 })

    // a third reconnect fires another synthetic
    es.onerror?.()
    es.onopen?.()
    expect(onEvent).toHaveBeenCalledTimes(2)
  })

  it('parses incoming messages and forwards them to onEvent', () => {
    const onEvent = vi.fn<(e: ChatSseEvent) => void>()
    connectChatStream('sess-3', onEvent)
    const es = FakeEventSource.last!

    es.onmessage?.(
      new MessageEvent('message', {
        data: JSON.stringify({
          seq: 1,
          type: 'state_advanced',
          new_state: { kind: 'emitted' },
        }),
      }),
    )
    expect(onEvent).toHaveBeenCalledWith({
      seq: 1,
      type: 'state_advanced',
      new_state: { kind: 'emitted' },
    })
  })

  it('drops duplicate or out-of-order sequenced messages', () => {
    const onEvent = vi.fn<(e: ChatSseEvent) => void>()
    connectChatStream('sess-seq', onEvent)
    const es = FakeEventSource.last!

    es.onmessage?.(
      new MessageEvent('message', {
        data: JSON.stringify({ seq: 2, type: 'assistant_token_delta', text: 'new' }),
      }),
    )
    es.onmessage?.(
      new MessageEvent('message', {
        data: JSON.stringify({ seq: 2, type: 'assistant_token_delta', text: 'dupe' }),
      }),
    )
    es.onmessage?.(
      new MessageEvent('message', {
        data: JSON.stringify({ seq: 1, type: 'assistant_token_delta', text: 'old' }),
      }),
    )
    es.onmessage?.(
      new MessageEvent('message', {
        data: JSON.stringify({ seq: 3, type: 'assistant_token_delta', text: 'next' }),
      }),
    )

    expect(onEvent).toHaveBeenCalledTimes(2)
    expect(onEvent).toHaveBeenNthCalledWith(1, {
      seq: 2,
      type: 'assistant_token_delta',
      text: 'new',
    })
    expect(onEvent).toHaveBeenNthCalledWith(2, {
      seq: 3,
      type: 'assistant_token_delta',
      text: 'next',
    })
  })

  it('resets the sequence gate after reconnect', () => {
    const onEvent = vi.fn<(e: ChatSseEvent) => void>()
    connectChatStream('sess-reset', onEvent)
    const es = FakeEventSource.last!

    es.onopen?.()
    es.onmessage?.(
      new MessageEvent('message', {
        data: JSON.stringify({ seq: 10, type: 'assistant_token_delta', text: 'before' }),
      }),
    )
    es.onerror?.()
    es.onopen?.()
    es.onmessage?.(
      new MessageEvent('message', {
        data: JSON.stringify({ seq: 1, type: 'assistant_token_delta', text: 'after' }),
      }),
    )

    expect(onEvent).toHaveBeenCalledWith({ type: 'resync_required', dropped: 0 })
    expect(onEvent).toHaveBeenCalledWith({
      seq: 1,
      type: 'assistant_token_delta',
      text: 'after',
    })
  })

  it('drops malformed JSON without throwing', () => {
    const onEvent = vi.fn<(e: ChatSseEvent) => void>()
    connectChatStream('sess-4', onEvent)
    const es = FakeEventSource.last!

    expect(() =>
      es.onmessage?.(new MessageEvent('message', { data: 'not-json' })),
    ).not.toThrow()
    expect(onEvent).not.toHaveBeenCalled()
  })

  it('returned disconnect closes the underlying EventSource', () => {
    const onEvent = vi.fn<(e: ChatSseEvent) => void>()
    const disconnect = connectChatStream('sess-5', onEvent)
    const es = FakeEventSource.last!

    expect(es.closed).toBe(false)
    disconnect()
    expect(es.closed).toBe(true)
  })
})
