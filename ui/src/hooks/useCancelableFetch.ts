// useCancelableFetch — small wrapper around the manual `cancelled` flag
// pattern that's repeated across ~20+ effects in the codebase. Bundles
// the cancel signal + abortable fetch wrapper so callers can drop the
// boilerplate.
//
// Migrate incrementally: existing manual-cancel effects keep working,
// but new code should prefer this hook so unmount during an in-flight
// request actually aborts the network call instead of just suppressing
// the result.

import { useEffect, useRef } from 'react'

interface CancelableContext {
  /** Mirror of AbortController.signal — pass into the request options. */
  readonly signal: AbortSignal
  /** True after the effect has unmounted. Use as a guard before setState. */
  readonly cancelled: () => boolean
}

/**
 * Run an async effect that can be cancelled on unmount via AbortController.
 * The factory receives a CancelableContext exposing the signal + a
 * cancelled() guard. Promise rejections caused by abort are silently
 * swallowed; other errors propagate to the caller's catch.
 *
 * Example:
 *
 *  useCancelableEffect(async ({ signal, cancelled }) => {
 *  const r = await jsonFetch(url, { signal })
 *  if (cancelled()) return
 *  setData(r)
 *  }, [url])
 */
export function useCancelableEffect(
  factory: (ctx: CancelableContext) => Promise<void>,
  deps: ReadonlyArray<unknown>,
): void {
  const factoryRef = useRef(factory)
  factoryRef.current = factory

  useEffect(() => {
    const controller = new AbortController()
    let isCancelled = false
    const ctx: CancelableContext = {
      signal: controller.signal,
      cancelled: () => isCancelled,
    }
    void factoryRef.current(ctx).catch((err) => {
      if (err instanceof DOMException && err.name === 'AbortError') return
      // Other errors flow through the caller's own try/catch inside the
      // factory; this catch only exists to swallow abort-on-unmount.
      throw err
    })
    return () => {
      isCancelled = true
      controller.abort()
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps)
}
