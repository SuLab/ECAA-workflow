/**
 * Fake EventSource installed into the page via addInitScript.
 *
 * Why not stream SSE through `route.fulfill`? Playwright's route.fulfill
 * sends the response body as a single unit — there's no way to hold a
 * streaming response open and push events mid-test. The cleanest
 * alternative is to replace window.EventSource entirely before any page
 * script runs. The fake EventSource exposes a test-side push API via
 * `window.__pushSseEvent(sessionId, payload)` that tests call with
 * page.evaluate().
 *
 * The script below runs in the browser context. It's intentionally written
 * as a standalone function so Playwright can serialize it into an init
 * script. No imports, no closures over the test-side scope.
 */

/**
 * This is the body of the init script. Playwright serializes this function
 * and runs it in every new document before any page script.
 */
export function installFakeEventSource(): void {
  // Guard against re-install on SPA navigation.
  if ((window as unknown as { __scrippsFakeEsInstalled?: boolean }).__scrippsFakeEsInstalled) {
    return
  }
  ;(window as unknown as { __scrippsFakeEsInstalled: boolean }).__scrippsFakeEsInstalled = true

  type Payload = Record<string, unknown>
  interface MockInstance {
    sessionId: string
    readyState: number
    onopen: ((ev: Event) => void) | null
    onmessage: ((ev: MessageEvent) => void) | null
    onerror: ((ev: Event) => void) | null
    dispatch: (data: string) => void
    close: () => void
  }

  const queued: Record<string, string[]> = {}
  const live: Record<string, MockInstance[]> = {}

  const parseSessionId = (url: string): string | null => {
    const m = url.match(/\/api\/(?:v1\/)?chat\/session\/([^/]+)\/events/)
    return m ? m[1] : null
  }

  class FakeEventSource extends EventTarget implements MockInstance {
    public static readonly CONNECTING = 0
    public static readonly OPEN = 1
    public static readonly CLOSED = 2

    public readonly CONNECTING = 0
    public readonly OPEN = 1
    public readonly CLOSED = 2

    public readyState = 0
    public onopen: ((ev: Event) => void) | null = null
    public onmessage: ((ev: MessageEvent) => void) | null = null
    public onerror: ((ev: Event) => void) | null = null
    public readonly url: string
    public readonly withCredentials = false
    public readonly sessionId: string

    constructor(url: string | URL) {
      super()
      this.url = typeof url === 'string' ? url : url.toString()
      this.sessionId = parseSessionId(this.url) ?? '_unknown_'
      if (!live[this.sessionId]) live[this.sessionId] = []
      live[this.sessionId].push(this)
      // Open on a microtask so consumers can attach handlers first.
      Promise.resolve().then(() => {
        if (this.readyState === this.CLOSED) return
        this.readyState = this.OPEN
        const openEv = new Event('open')
        this.onopen?.(openEv)
        this.dispatchEvent(openEv)
        // Drain any events queued before the consumer opened the stream.
        const q = queued[this.sessionId] ?? []
        for (const data of q) this.dispatch(data)
        queued[this.sessionId] = []
      })
    }

    dispatch(data: string): void {
      if (this.readyState !== this.OPEN) return
      const ev = new MessageEvent('message', { data })
      this.onmessage?.(ev)
      this.dispatchEvent(ev)
    }

    close(): void {
      this.readyState = this.CLOSED
      const arr = live[this.sessionId]
      if (arr) {
        const ix = arr.indexOf(this)
        if (ix >= 0) arr.splice(ix, 1)
      }
    }
  }

  ;(window as unknown as { EventSource: typeof FakeEventSource }).EventSource =
    FakeEventSource

  ;(window as unknown as {
    __pushSseEvent: (sid: string, payload: Payload) => void
  }).__pushSseEvent = (sid: string, payload: Payload) => {
    const data = JSON.stringify(payload)
    const instances = live[sid]
    if (instances && instances.length > 0) {
      for (const inst of instances) inst.dispatch(data)
    } else {
      if (!queued[sid]) queued[sid] = []
      queued[sid].push(data)
    }
  }

  ;(window as unknown as {
    __resetSse: () => void
  }).__resetSse = () => {
    for (const k of Object.keys(queued)) delete queued[k]
    for (const k of Object.keys(live)) {
      for (const inst of live[k]) inst.close()
      delete live[k]
    }
  }
}

/**
 * String form of the init script — Playwright's addInitScript takes either
 * a function or a { content: string } form. We use the string form so the
 * install happens even when page-side TypeScript transpilation isn't
 * available in the browser context.
 */
export const FAKE_EVENT_SOURCE_INIT_SCRIPT = `(${installFakeEventSource.toString()})()`
