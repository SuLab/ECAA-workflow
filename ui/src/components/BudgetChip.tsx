// Tiny header chip that surfaces budget state when it trips 75% / 100%.
// Polls the metrics endpoint every 15s (same cadence as Performance tab
// but from the header so the SME sees it even on the Chat tab).

import { useEffect, useState } from 'react'
import { getChatMetrics, type SessionMetrics } from '../api/chatClient'
import { useTitleBarPolling } from '../hooks/useTitleBarPolling'
import { BUDGET_POLL_MS } from '../lib/polling'

interface Props {
  sessionId: string | null
}

export default function BudgetChip({ sessionId }: Props): JSX.Element | null {
  const [metrics, setMetrics] = useState<SessionMetrics | null>(null)

  // Mount-time initial fetch; periodic refresh runs on the shared
  // title-bar tick (`useTitleBarPolling`).
  useEffect(() => {
    if (!sessionId) {
      setMetrics(null)
      return
    }
    let cancelled = false
    getChatMetrics(sessionId)
      .then((m) => {
        if (!cancelled) setMetrics(m)
      })
      .catch(() => {
        // Swallow; header chip degrades to hidden.
      })
    return () => {
      cancelled = true
    }
  }, [sessionId])

  useTitleBarPolling({
    cadenceMs: BUDGET_POLL_MS,
    enabled: sessionId != null,
    onTick: () => {
      if (!sessionId) return
      void getChatMetrics(sessionId).then(setMetrics).catch(() => {})
    },
  })

  if (!metrics) return null
  const state = metrics.budget_state
  if (state !== 'warn' && state !== 'exceeded') return null
  const pct = Math.round((metrics.budget_used_pct ?? 0) * 100)
  const label = state === 'exceeded' ? 'Over budget' : `${pct}% of budget`

  const tone =
    state === 'exceeded'
      ? {
          bg: 'var(--color-danger-bg)',
          fg: 'var(--color-danger-fg)',
          border: 'var(--color-danger-border)',
        }
      : {
          bg: 'var(--color-warning-bg)',
          fg: 'var(--color-warning-fg)',
          border: 'var(--color-warning-border)',
        }

  return (
    <span
      role="status"
      aria-live="polite"
      style={{
        display: 'inline-flex',
        alignItems: 'center',
        gap: '0.3rem',
        padding: '0.2rem 0.55rem',
        marginLeft: '0.4rem',
        background: tone.bg,
        color: tone.fg,
        border: `1px solid ${tone.border}`,
        borderRadius: 999,
        fontSize: '0.7rem',
        fontWeight: 600,
      }}
    >
      <span aria-hidden>💰</span>
      {label}
    </span>
  )
}
