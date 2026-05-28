// Session budget burn bar + edit affordance. Rendered at the top of
// the Performance/Metrics tab when the session carries a budget cap,
// or with an "Add budget" call-to-action when it doesn't.

import { useState } from 'react'
import { postBudget, type SessionMetrics } from '../api/chatClient'
import { formatUSD } from '../lib/format'

interface Props {
  metrics: SessionMetrics | null
  sessionId: string | null
  onChanged?: () => void
}

export default function BudgetRow({ metrics, sessionId, onChanged }: Props): JSX.Element | null {
  const [editing, setEditing] = useState(false)
  const [value, setValue] = useState<string>('')
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  if (!metrics) return null
  const cap = metrics.budget_usd ?? null
  const used = metrics.total_cost_usd ?? 0
  const pct = metrics.budget_used_pct ?? (cap ? used / cap : null)
  const state = metrics.budget_state ?? null
  const projectedFinish = metrics.projected_finish_usd ?? used
  const willExceed = cap != null && projectedFinish > cap

  const save = async (newCap: number | null) => {
    if (!sessionId) return
    setSaving(true)
    setError(null)
    try {
      await postBudget(sessionId, newCap)
      onChanged?.()
      setEditing(false)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setSaving(false)
    }
  }

  const tone = stateTone(state)

  return (
    <div
      aria-label="Session budget"
      style={{
        border: `1px solid ${tone.border}`,
        background: tone.bg,
        borderRadius: 6,
        padding: '0.6rem 0.8rem',
        marginBottom: '0.75rem',
      }}
    >
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: '0.6rem',
          fontSize: '0.85rem',
        }}
      >
        <strong style={{ color: 'var(--color-text-primary)' }}>Budget</strong>
        {cap === null ? (
          <span style={{ color: 'var(--color-text-secondary)' }}>
            No cap set — {formatUSD(used)} spent
          </span>
        ) : (
          <span style={{ color: 'var(--color-text-secondary)' }}>
            {formatUSD(used)} / {formatUSD(cap)}
            {pct != null && (
              <> ({Math.round(pct * 100)}%)</>
            )}
          </span>
        )}
        <div style={{ flex: 1 }} />
        {!editing && (
          <button
            type="button"
            onClick={() => {
              setEditing(true)
              setValue(cap != null ? cap.toFixed(2) : '')
            }}
            style={btnStyle}
          >
            {cap === null ? 'Set budget' : 'Edit'}
          </button>
        )}
      </div>
      {cap !== null && (
        <div
          role="progressbar"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={Math.round(Math.min(1, pct ?? 0) * 100)}
          style={{
            marginTop: 6,
            height: 8,
            background: 'var(--color-surface-2)',
            borderRadius: 999,
            overflow: 'hidden',
          }}
        >
          <div
            style={{
              height: '100%',
              width: `${Math.min(100, Math.max(0, (pct ?? 0) * 100))}%`,
              background: tone.fill,
            }}
          />
        </div>
      )}
      {cap !== null && (
        <div
          style={{
            marginTop: 4,
            fontSize: '0.75rem',
            color: willExceed ? 'var(--color-danger-accent)' : 'var(--color-text-muted)',
          }}
        >
          Estimated cost to finish: {formatUSD(projectedFinish)}
          {willExceed && ' — projected over budget'}
        </div>
      )}
      {editing && (
        <div
          style={{
            marginTop: 8,
            display: 'flex',
            alignItems: 'center',
            gap: 8,
            fontSize: '0.8rem',
          }}
        >
          <label>
            $
            <input
              type="number"
              inputMode="decimal"
              step="0.50"
              min="0"
              value={value}
              onChange={(e) => setValue(e.target.value)}
              style={{
                marginLeft: 4,
                padding: '0.2rem 0.4rem',
                border: '1px solid var(--color-border-default)',
                borderRadius: 4,
                width: 90,
              }}
            />
          </label>
          <button
            type="button"
            disabled={saving}
            onClick={() => {
              const parsed = parseFloat(value)
              void save(Number.isFinite(parsed) && parsed > 0 ? parsed : null)
            }}
            style={btnPrimary}
          >
            {saving ? 'Saving…' : 'Save'}
          </button>
          <button
            type="button"
            disabled={saving}
            onClick={() => void save(null)}
            style={btnStyle}
          >
            Clear
          </button>
          <button
            type="button"
            disabled={saving}
            onClick={() => {
              setEditing(false)
              setError(null)
            }}
            style={btnStyle}
          >
            Cancel
          </button>
          {error && (
            <span role="alert" style={{ color: 'var(--color-danger-accent)' }}>
              {error}
            </span>
          )}
        </div>
      )}
    </div>
  )
}

function stateTone(state: string | null | undefined): {
  fill: string
  border: string
  bg: string
} {
  switch (state) {
    case 'exceeded':
      return {
        fill: 'var(--color-danger-accent)',
        border: 'var(--color-danger-border)',
        bg: 'var(--color-danger-bg)',
      }
    case 'warn':
      return {
        fill: 'var(--color-warning-accent)',
        border: 'var(--color-warning-border)',
        bg: 'var(--color-warning-bg)',
      }
    default:
      return {
        fill: 'var(--color-success-accent)',
        border: 'var(--color-border-default)',
        bg: 'var(--color-surface-1)',
      }
  }
}

const btnStyle: React.CSSProperties = {
  padding: '0.25rem 0.6rem',
  fontSize: '0.75rem',
  border: '1px solid var(--color-border-default)',
  background: 'var(--color-surface-0)',
  color: 'var(--color-text-secondary)',
  borderRadius: 4,
  cursor: 'pointer',
}

const btnPrimary: React.CSSProperties = {
  ...btnStyle,
  background: 'var(--color-accent)',
  color: 'var(--color-accent-fg)',
  border: '1px solid var(--color-accent)',
  fontWeight: 600,
}
