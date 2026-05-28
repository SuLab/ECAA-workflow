import { useCallback, useState } from 'react'

/**
 * Wraps the `busy / error / try-catch-finally` triad that every
 * interactive turn card repeats. Callers invoke `run(fn)` and the hook
 * tracks the async lifecycle:
 *
 *  const { busy, error, run } = useAsync()
 *  await run(() => postChatAction(...))
 *
 * `error` is a string (toStringified Error.message) so cards can render
 * it inline without an `instanceof Error` check at each call site.
 */
export interface AsyncState {
  /** True while the most recent `run` is in flight. */
  busy: boolean
  /** Error message from the most recent `run`, or null when idle / ok. */
  error: string | null
  /** Execute `fn`; captures busy / error lifecycle. */
  run: <T>(fn: () => Promise<T>) => Promise<T | undefined>
  /** Clear the error state without firing a new run. */
  clearError: () => void
}

export function useAsync(): AsyncState {
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const run = useCallback(async <T>(fn: () => Promise<T>): Promise<T | undefined> => {
    setBusy(true)
    setError(null)
    try {
      return await fn()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      return undefined
    } finally {
      setBusy(false)
    }
  }, [])

  const clearError = useCallback(() => setError(null), [])

  return { busy, error, run, clearError }
}
