// Narrative dashboard summary card rendered at the top of the
// Dashboard tab. Fires a Haiku side-call on demand; caches on the
// server so a repeat click is free.

import { useState } from 'react'
import { postDashboardSummary } from '../api/chatClient'

interface Props {
  sessionId: string | null
}

export default function DashboardSummaryCard({ sessionId }: Props): JSX.Element | null {
  const [summary, setSummary] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [cached, setCached] = useState<boolean | null>(null)

  const fetchSummary = async () => {
    if (!sessionId) return
    setLoading(true)
    setError(null)
    try {
      const res = await postDashboardSummary(sessionId)
      setSummary(res.summary)
      setCached(res.cached)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setLoading(false)
    }
  }

  if (!sessionId) return null

  return (
    <div
      aria-label="Analysis summary"
      style={{
        padding: '0.7rem 0.9rem',
        borderBottom: '1px solid var(--color-border-default)',
        background: 'var(--color-surface-1)',
      }}
    >
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: '0.5rem',
          marginBottom: summary ? '0.5rem' : 0,
        }}
      >
        <strong style={{ fontSize: '0.85rem' }}>Summary</strong>
        <div style={{ flex: 1 }} />
        {cached !== null && (
          <span style={{ fontSize: '0.7rem', color: 'var(--color-text-muted)' }}>
            {cached ? 'cached' : 'fresh'}
          </span>
        )}
        <button
          type="button"
          disabled={loading}
          onClick={() => void fetchSummary()}
          style={{
            padding: '0.25rem 0.7rem',
            fontSize: '0.75rem',
            border: '1px solid var(--color-accent)',
            background: 'var(--color-accent)',
            color: 'var(--color-accent-fg)',
            borderRadius: 4,
            cursor: loading ? 'progress' : 'pointer',
            fontWeight: 600,
          }}
        >
          {loading
            ? 'Generating…'
            : summary
              ? 'Refresh summary'
              : 'Generate summary'}
        </button>
      </div>
      {error && (
        <div role="alert" style={{ color: 'var(--color-danger-accent)', fontSize: '0.8rem' }}>
          {error}
        </div>
      )}
      {summary && (
        <div
          style={{
            fontSize: '0.85rem',
            color: 'var(--color-text-primary)',
            whiteSpace: 'pre-wrap',
            lineHeight: 1.55,
          }}
        >
          {summary}
        </div>
      )}
    </div>
  )
}
