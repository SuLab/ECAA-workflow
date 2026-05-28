import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react'

interface StreamingTextValue {
  text: string
  append: (chunk: string) => void
  reset: () => void
}

interface StreamingTextControls {
  append: (chunk: string) => void
  reset: () => void
}

const StreamingTextContextInternal = createContext<StreamingTextValue | null>(null)
/// Stable-identity callbacks context. Held separately from the text
/// context so the `SseChatEventsBridge` can pull `append` / `reset`
/// without re-rendering on every committed streaming frame (which would
/// invalidate the `<EventsProvider>` value object on every frame).
const StreamingTextControlsInternal = createContext<StreamingTextControls | null>(null)

/**
 * Provides a dedicated React context for in-flight LLM streaming text.
 * Components that don't render the streaming bubble do NOT subscribe to
 * this context, so an `assistant_token_delta` SSE event triggers only
 * the in-flight bubble's re-render, not the full `ConversationPane` tree.
 *
 * Internally, deltas are buffered in a ref and committed via
 * requestAnimationFrame so the bubble updates at ~60fps regardless of
 * delta arrival rate.
 */
export function StreamingTextProvider({ children }: { children: React.ReactNode }) {
  const [text, setText] = useState('')
  const bufferRef = useRef('')
  const rafRef = useRef<number | null>(null)

  const append = useCallback((chunk: string) => {
    bufferRef.current += chunk
    if (rafRef.current !== null) return
    rafRef.current = window.requestAnimationFrame(() => {
      rafRef.current = null
      setText(bufferRef.current)
    })
  }, [])

  const reset = useCallback(() => {
    if (rafRef.current !== null) {
      window.cancelAnimationFrame(rafRef.current)
      rafRef.current = null
    }
    bufferRef.current = ''
    setText('')
  }, [])

  useEffect(() => () => {
    if (rafRef.current !== null) window.cancelAnimationFrame(rafRef.current)
  }, [])

  // Controls value is identity-stable across renders (callbacks are
  // useCallback'd with empty deps). Consuming the controls context
  // therefore does NOT trigger re-render on streaming frames.
  const controls = useMemo<StreamingTextControls>(
    () => ({ append, reset }),
    [append, reset],
  )
  // Text value re-creates on every frame; consumers (the in-flight
  // bubble) re-render — that's the intended path.
  const value = useMemo<StreamingTextValue>(
    () => ({ text, append, reset }),
    [text, append, reset],
  )

  return (
    <StreamingTextControlsInternal.Provider value={controls}>
      <StreamingTextContextInternal.Provider value={value}>
        {children}
      </StreamingTextContextInternal.Provider>
    </StreamingTextControlsInternal.Provider>
  )
}

export function useStreamingText(): StreamingTextValue {
  const v = useContext(StreamingTextContextInternal)
  if (!v) {
    throw new Error('useStreamingText must be used inside <StreamingTextProvider>')
  }
  return v
}

/// Stable-identity append / reset only — does NOT re-render on streaming
/// frames. Use this for callers that only need to write into the
/// streaming buffer (e.g. the SSE delta handler).
export function useStreamingTextControls(): StreamingTextControls {
  const v = useContext(StreamingTextControlsInternal)
  if (!v) {
    throw new Error(
      'useStreamingTextControls must be used inside <StreamingTextProvider>',
    )
  }
  return v
}
