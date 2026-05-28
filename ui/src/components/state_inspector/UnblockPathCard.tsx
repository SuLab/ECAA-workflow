/**
 * `UnblockPathCard` UI component.
 *
 * Renders one `UnblockPath` synthesized for an active refusal. Branches
 * on the path's `kind` to render the right form/affordance, then POSTs
 * to `/api/chat/session/:id/refusal/:refusal_id/dispatch` when the SME
 * submits.
 *
 * Hosted by `RefusalReportCard`, which renders one card per path on
 * a refusal report.
 */

import React, { useState } from 'react'
import type { UnblockPath } from '../../types/UnblockPath'

interface Props {
  sessionId: string
  refusalId: string
  pathIndex: number
  path: UnblockPath
  onDispatched: () => void
}

export function UnblockPathCard({
  sessionId,
  refusalId,
  pathIndex,
  path,
  onDispatched,
}: Props) {
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [inputValue, setInputValue] = useState<string>('')

  async function dispatch(payload: Record<string, unknown>) {
    setBusy(true)
    setError(null)
    try {
      const res = await fetch( // allow-bare-fetch: 501-branching needed for "not yet wired" affordances
        `/api/chat/session/${sessionId}/refusal/${refusalId}/dispatch`,
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ path_index: pathIndex, payload }),
        },
      )
      if (res.status === 501) {
        setError('This recovery path is not yet wired (Phase 5 follow-up).')
        return
      }
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`)
      }
      onDispatched()
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(false)
    }
  }

  const projection = projectedOutcomeLabel(path)

  return (
    <div style={cardStyle}>
      <header style={headerStyle}>
        <strong style={kindStyle}>{kindLabel(path)}</strong>
        <span style={projectionStyle}>
          projected outcome:{' '}
          <code style={codeStyle}>{projection}</code>
        </span>
      </header>

      {renderBody(path, inputValue, setInputValue, dispatch, busy)}

      {error && <div style={errorStyle}>{error}</div>}
    </div>
  )
}

/** Friendly label per variant. */
function kindLabel(path: UnblockPath): string {
  switch (path.kind) {
    case 'resolve_assumption':
      return 'Resolve assumption'
    case 'waiver':
      return 'Apply a waiver'
    case 'attempt_repair':
      return 'Attempt automated repair'
    case 'supply_missing_metadata':
      return 'Supply missing metadata'
    case 'escalate_to_reviewer':
      return 'Escalate to reviewer'
  }
  // Exhaustiveness: tsc fails when a new UnblockPath variant
  // is added upstream and lands in ui/src/types/UnblockPath.ts via `make types`.
  const _exhaustive: never = path
  void _exhaustive
  return ''
}

/** Surface the projected outcome label so the SME knows what to expect. */
function projectedOutcomeLabel(path: UnblockPath): string {
  return path.target_outcome
}

/**
 * Per-variant body: form fields the SME fills in, plus the dispatch
 * button. Each branch builds the payload it cares about and passes it
 * to the shared `dispatch` callback.
 */
function renderBody(
  path: UnblockPath,
  inputValue: string,
  setInputValue: (v: string) => void,
  dispatch: (payload: Record<string, unknown>) => void,
  busy: boolean,
): React.ReactNode {
  if (path.kind === 'resolve_assumption') {
    return (
      <div style={bodyStyle}>
        <div style={fieldRowStyle}>
          <label style={labelStyle}>Assumption</label>
          <code style={codeStyle}>{path.assumption_id}</code>
        </div>
        {path.suggested_resolution && (
          <div style={hintStyle}>
            Suggested:{' '}
            <em>{path.suggested_resolution}</em>
          </div>
        )}
        <div style={fieldRowStyle}>
          <label htmlFor={`resolution-${path.assumption_id}`} style={labelStyle}>
            Resolution value
          </label>
          <input
            id={`resolution-${path.assumption_id}`}
            type="text"
            value={inputValue}
            onChange={(e) => setInputValue(e.target.value)}
            placeholder={path.suggested_resolution ?? ''}
            style={inputStyle}
            disabled={busy}
          />
        </div>
        <button
          type="button"
          disabled={busy || inputValue.trim().length === 0}
          onClick={() => dispatch({ resolution: inputValue.trim() })}
          style={primaryButton}
        >
          {busy ? 'Resolving…' : 'Resolve'}
        </button>
      </div>
    )
  }

  if (path.kind === 'waiver') {
    return (
      <div style={bodyStyle}>
        <div style={fieldRowStyle}>
          <label style={labelStyle}>Rule</label>
          <code style={codeStyle}>{path.rule_id}</code>
        </div>
        <div style={fieldRowStyle}>
          <label style={labelStyle}>Required credentials</label>
          <ul style={chipListStyle}>
            {path.required_credentials.map((c) => (
              <li key={c} style={chipStyle}>
                {c}
              </li>
            ))}
          </ul>
        </div>
        <button
          type="button"
          disabled={busy}
          onClick={() =>
            dispatch({ credentials_supplied: path.required_credentials })
          }
          style={primaryButton}
        >
          {busy ? 'Waiving…' : 'Apply waiver'}
        </button>
      </div>
    )
  }

  if (path.kind === 'attempt_repair') {
    return (
      <div style={bodyStyle}>
        <div style={fieldRowStyle}>
          <label style={labelStyle}>Strategy</label>
          <code style={codeStyle}>{path.strategy_id}</code>
        </div>
        <div style={fieldRowStyle}>
          <label style={labelStyle}>Targeting gap</label>
          <code style={codeStyle}>{path.gap_id}</code>
        </div>
        <button
          type="button"
          disabled={busy}
          onClick={() => dispatch({})}
          style={primaryButton}
        >
          {busy ? 'Repairing…' : 'Run repair'}
        </button>
      </div>
    )
  }

  if (path.kind === 'supply_missing_metadata') {
    return (
      <div style={bodyStyle}>
        <div style={fieldRowStyle}>
          <label style={labelStyle}>Field</label>
          <code style={codeStyle}>{path.field}</code>
        </div>
        {path.suggested_value && (
          <div style={hintStyle}>
            Suggested: <em>{path.suggested_value}</em>
          </div>
        )}
        <div style={fieldRowStyle}>
          <label htmlFor={`field-${path.field}`} style={labelStyle}>
            Value
          </label>
          <input
            id={`field-${path.field}`}
            type="text"
            value={inputValue}
            onChange={(e) => setInputValue(e.target.value)}
            placeholder={path.suggested_value ?? ''}
            style={inputStyle}
            disabled={busy}
          />
        </div>
        <button
          type="button"
          disabled={busy || inputValue.trim().length === 0}
          onClick={() => dispatch({ value: inputValue.trim() })}
          style={primaryButton}
        >
          {busy ? 'Submitting…' : 'Submit'}
        </button>
      </div>
    )
  }

  if (path.kind === 'escalate_to_reviewer') {
    return (
      <div style={bodyStyle}>
        <div style={fieldRowStyle}>
          <label style={labelStyle}>Reviewer class</label>
          <code style={codeStyle}>{path.reviewer_class}</code>
        </div>
        {path.required_artifacts.length > 0 && (
          <div style={fieldRowStyle}>
            <label style={labelStyle}>Required artifacts</label>
            <ul style={chipListStyle}>
              {path.required_artifacts.map((a) => (
                <li key={a} style={chipStyle}>
                  {a}
                </li>
              ))}
            </ul>
          </div>
        )}
        <button
          type="button"
          disabled={busy}
          onClick={() => dispatch({})}
          style={primaryButton}
        >
          {busy ? 'Escalating…' : 'Escalate'}
        </button>
      </div>
    )
  }

  return null
}

const cardStyle: React.CSSProperties = {
  padding: '0.55rem 0.75rem',
  border: '1px solid var(--color-border-subtle, #ccc)',
  borderRadius: '0.35rem',
  background: 'var(--color-surface-1, #f7f7f7)',
  marginTop: '0.5rem',
  fontSize: '0.78rem',
}
const headerStyle: React.CSSProperties = {
  display: 'flex',
  justifyContent: 'space-between',
  alignItems: 'baseline',
  marginBottom: '0.45rem',
}
const kindStyle: React.CSSProperties = {
  fontWeight: 600,
}
const projectionStyle: React.CSSProperties = {
  fontSize: '0.7rem',
  color: 'var(--color-text-muted, #666)',
}
const bodyStyle: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  gap: '0.35rem',
}
const fieldRowStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: '0.45rem',
  flexWrap: 'wrap',
}
const labelStyle: React.CSSProperties = {
  minWidth: '7rem',
  fontWeight: 500,
  fontSize: '0.72rem',
}
const inputStyle: React.CSSProperties = {
  flex: 1,
  minWidth: '12rem',
  padding: '0.25rem 0.4rem',
  fontSize: '0.74rem',
  border: '1px solid var(--color-border-subtle, #ccc)',
  borderRadius: '0.25rem',
}
const hintStyle: React.CSSProperties = {
  fontSize: '0.7rem',
  color: 'var(--color-text-muted, #666)',
  fontStyle: 'italic',
}
const codeStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  background: 'var(--color-surface-2, #fff)',
  padding: '0.05rem 0.3rem',
  borderRadius: '0.2rem',
  fontSize: '0.72rem',
}
const chipListStyle: React.CSSProperties = {
  display: 'flex',
  flexWrap: 'wrap',
  gap: '0.25rem',
  listStyle: 'none',
  margin: 0,
  padding: 0,
}
const chipStyle: React.CSSProperties = {
  fontSize: '0.7rem',
  background: 'var(--color-surface-muted, #eaeaea)',
  padding: '0.1rem 0.4rem',
  borderRadius: '0.25rem',
  fontFamily: 'ui-monospace, monospace',
}
const primaryButton: React.CSSProperties = {
  alignSelf: 'flex-start',
  padding: '0.3rem 0.65rem',
  fontSize: '0.72rem',
  background: 'var(--color-success-accent, #2e7d32)',
  color: '#fff',
  border: 'none',
  borderRadius: '0.25rem',
  cursor: 'pointer',
}
const errorStyle: React.CSSProperties = {
  marginTop: '0.4rem',
  fontSize: '0.7rem',
  color: 'var(--color-danger-fg, #b71c1c)',
}

export default UnblockPathCard
