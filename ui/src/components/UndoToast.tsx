// Bottom-right toast that surfaces a 30s undo opportunity after a
// reversible mutation (amendment, rerun, branch). Keyboard-dismissable
// with Escape; countdown ring shows time remaining.

import { useEffect, useState } from 'react'
import { UNDO_WINDOW_MS, useUndoStack } from '../hooks/useUndoStack'
import { UNDO_TOAST_TICK_MS } from '../lib/polling'
import { Z } from '../lib/z-index'

export default function UndoToast(): JSX.Element | null {
  const { token, clear } = useUndoStack()
  const [now, setNow] = useState(() => Date.now())
  const [undoing, setUndoing] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!token) return
    const id = window.setInterval(() => setNow(Date.now()), UNDO_TOAST_TICK_MS)
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') clear()
    }
    document.addEventListener('keydown', onKey)
    return () => {
      window.clearInterval(id)
      document.removeEventListener('keydown', onKey)
    }
  }, [token, clear])

  if (!token) return null
  const remainingMs = Math.max(0, UNDO_WINDOW_MS - (now - token.createdAt))
  const remainingSec = Math.ceil(remainingMs / 1000)
  const progress = remainingMs / UNDO_WINDOW_MS

  return (
    <div
      role="status"
      aria-live="polite"
      style={{
        position: 'fixed',
        bottom: 20,
        right: 20,
        background: 'var(--color-surface-0)',
        border: '1px solid var(--color-border-default)',
        borderRadius: 8,
        padding: '0.7rem 0.9rem',
        display: 'flex',
        alignItems: 'center',
        gap: '0.7rem',
        boxShadow: '0 8px 28px rgba(0,0,0,0.15)',
        zIndex: Z.TOAST,
        minWidth: 260,
      }}
    >
      <CountdownRing progress={progress} label={`${remainingSec}s`} />
      <div style={{ flex: 1, fontSize: '0.85rem' }}>
        <div style={{ fontWeight: 600, color: 'var(--color-text-primary)' }}>
          {token.label}
        </div>
        {error && (
          <div style={{ color: 'var(--color-danger-accent)', marginTop: 2 }}>
            {error}
          </div>
        )}
      </div>
      <button
        type="button"
        disabled={undoing}
        onClick={async () => {
          setUndoing(true)
          setError(null)
          try {
            await token.undo()
            clear()
          } catch (err) {
            setError(err instanceof Error ? err.message : String(err))
          } finally {
            setUndoing(false)
          }
        }}
        style={{
          padding: '0.35rem 0.7rem',
          borderRadius: 4,
          border: '1px solid var(--color-accent)',
          background: 'var(--color-accent)',
          color: 'var(--color-accent-fg)',
          fontSize: '0.8rem',
          fontWeight: 600,
          cursor: undoing ? 'progress' : 'pointer',
        }}
      >
        {undoing ? 'Undoing…' : 'Undo'}
      </button>
      <button
        type="button"
        aria-label="Dismiss"
        onClick={clear}
        style={{
          background: 'transparent',
          border: 'none',
          color: 'var(--color-text-muted)',
          cursor: 'pointer',
          fontSize: '1.1rem',
          lineHeight: 1,
          padding: '0 0.1rem',
        }}
      >
        ×
      </button>
    </div>
  )
}

function CountdownRing({ progress, label }: { progress: number; label: string }) {
  const r = 14
  const circ = 2 * Math.PI * r
  const offset = circ * (1 - Math.max(0, Math.min(1, progress)))
  return (
    <svg width={34} height={34} aria-hidden style={{ display: 'block' }}>
      <circle
        cx={17}
        cy={17}
        r={r}
        fill="none"
        stroke="var(--color-border-default)"
        strokeWidth={2}
      />
      <circle
        cx={17}
        cy={17}
        r={r}
        fill="none"
        stroke="var(--color-accent)"
        strokeWidth={2}
        strokeDasharray={`${circ} ${circ}`}
        strokeDashoffset={offset}
        transform="rotate(-90 17 17)"
      />
      <text
        x={17}
        y={19}
        textAnchor="middle"
        fontSize="9"
        fill="var(--color-text-secondary)"
      >
        {label}
      </text>
    </svg>
  )
}
