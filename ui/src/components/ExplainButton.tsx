// Inline "?" button that asks Haiku to rewrite a technical snippet in
// plain language. Popover anchors under the button; spinner while the
// side-call is in flight; "Try another rewrite" refires with the prior
// explanation appended to the context so Haiku avoids repeating.

import { useContext, useEffect, useRef, useState } from 'react'
import { postExplain } from '../api/chatClient'
import { SessionContext } from '../hooks/contexts'
import { Z } from '../lib/z-index'

interface Props {
  text: string
  /** Optional register hint like "blocker reason", "method", "narrative". */
  context?: string
  /** Override the session id (for error banners that live outside the session provider). */
  sessionIdOverride?: string | null
}

export default function ExplainButton({
  text,
  context,
  sessionIdOverride,
}: Props): JSX.Element | null {
  // Degrade gracefully when no SessionProvider is present (unit tests
  // mounted without App wrapping): the button just does nothing
  // without a session id.
  const conv = useContext(SessionContext)
  const sessionId = sessionIdOverride ?? conv?.sessionId ?? null
  const [open, setOpen] = useState(false)
  const [loading, setLoading] = useState(false)
  const [explanation, setExplanation] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const ref = useRef<HTMLSpanElement>(null)

  const load = async (retry: boolean) => {
    if (!sessionId || !text) return
    setLoading(true)
    setError(null)
    try {
      const ctx = retry && explanation
        ? `${context ?? ''} (previous rewrite: "${explanation}" — produce a different wording)`
        : context
      const res = await postExplain(sessionId, text, ctx)
      setExplanation(res.explanation)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    if (!open) return
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false)
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false)
    }
    document.addEventListener('mousedown', onClick)
    document.addEventListener('keydown', onKey)
    return () => {
      document.removeEventListener('mousedown', onClick)
      document.removeEventListener('keydown', onKey)
    }
  }, [open])

  if (!text || !text.trim()) return null

  return (
    <span ref={ref} style={{ position: 'relative', display: 'inline-block' }}>
      <button
        type="button"
        aria-label="Explain this in plain language"
        title="Explain in plain language"
        onClick={() => {
          const next = !open
          setOpen(next)
          if (next && explanation === null) {
            void load(false)
          }
        }}
        style={{
          background: 'transparent',
          border: '1px solid var(--color-border-default)',
          color: 'var(--color-text-muted)',
          borderRadius: '50%',
          width: 18,
          height: 18,
          fontSize: '0.7rem',
          cursor: 'pointer',
          padding: 0,
          marginLeft: 4,
          verticalAlign: 'middle',
          lineHeight: 1,
        }}
      >
        ?
      </button>
      {open && (
        <div
          role="dialog"
          aria-label="Plain-language explanation"
          style={{
            position: 'absolute',
            zIndex: Z.ANCHORED_POPOVER,
            top: 'calc(100% + 6px)',
            left: 0,
            width: 360,
            maxWidth: '80vw',
            padding: '0.7rem 0.8rem',
            background: 'var(--color-surface-0)',
            border: '1px solid var(--color-border-default)',
            borderRadius: 6,
            boxShadow: '0 6px 20px rgba(0,0,0,0.15)',
            fontSize: '0.85rem',
            color: 'var(--color-text-primary)',
          }}
        >
          {loading && <div aria-live="polite">Generating a plain-language rewrite…</div>}
          {!loading && error && (
            <div role="alert" style={{ color: 'var(--color-danger-accent)' }}>
              {error}
            </div>
          )}
          {!loading && !error && explanation && (
            <>
              <div style={{ whiteSpace: 'pre-wrap' }}>{explanation}</div>
              <div style={{ marginTop: 8, display: 'flex', gap: 8 }}>
                <button
                  type="button"
                  onClick={() => void load(true)}
                  style={{
                    fontSize: '0.72rem',
                    padding: '0.2rem 0.55rem',
                    borderRadius: 4,
                    border: '1px solid var(--color-border-default)',
                    background: 'var(--color-surface-1)',
                    color: 'var(--color-text-secondary)',
                    cursor: 'pointer',
                  }}
                >
                  That was unclear — try again
                </button>
              </div>
            </>
          )}
        </div>
      )}
    </span>
  )
}
