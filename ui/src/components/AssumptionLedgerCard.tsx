/**
 * `AssumptionLedgerCard` UI component.
 *
 * Surfaces unresolved assumptions from `runtime/assumptions.jsonl`.
 * Each entry pairs a free-text statement with the source (LLM
 * inferred / lossy adapter / unresolved ontology mapping / policy
 * exception), the affected node ids, and the risk class.
 *
 * SMEs can confirm or reject each assumption inline; the card
 * dispatches the resolution back through the existing
 * `record_decision` path which writes a
 * `DecisionType::AssumptionResolved` to the audit log. SMEs can
 * also supply an optional rationale before confirming/rejecting
 * (revealed by clicking the row's "Add rationale" link); the
 * rationale flows onto the `DecisionRecord` for handoff continuity.
 */

import { useState } from 'react'
import { ChainOfCustodyPanel, type ChainOfCustody } from './EdgeProofDrawer'

export interface Assumption {
  id: string
  statement: string
  source: string
  affects_nodes: string[]
  risk: string
  resolution: 'unresolved' | 'confirmed' | 'rejected'
  /** v3 P5 — populated when the assumption carries suppressed
   *  content. Rendered inline via `ChainOfCustodyPanel`. */
  chain_of_custody?: ChainOfCustody
}

interface Props {
  assumptions: Assumption[]
  /** `rationale` is an optional free-text note the SME
   *  can attach via the inline textarea revealed by "Add rationale". */
  onResolve?: (
    id: string,
    resolution: 'confirmed' | 'rejected',
    rationale?: string,
  ) => void
}

export default function AssumptionLedgerCard({ assumptions, onResolve }: Props) {
  // Per-assumption rationale buffer + open/closed state for the
  // inline textarea. Keyed by assumption id so each row tracks
  // independently (the SME can have multiple textareas open at
  // once if they want to compose carefully).
  const [rationales, setRationales] = useState<Record<string, string>>({})
  const [openRationales, setOpenRationales] = useState<Set<string>>(new Set())
  const toggleRationale = (id: string) => {
    setOpenRationales((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }
  if (assumptions.length === 0) {
    return (
      <div role="region" aria-label="Assumption ledger" style={emptyStyle}>
        No assumptions recorded — the composition has no
        assumption-mediated edges or all assumptions are resolved.
      </div>
    )
  }
  const unresolved = assumptions.filter((a) => a.resolution === 'unresolved')
  return (
    <div role="region" aria-label="Assumption ledger" style={cardStyle}>
      <h4 style={headingStyle}>
        Assumption ledger ({unresolved.length} unresolved /{' '}
        {assumptions.length} total)
      </h4>
      <ul style={listStyle}>
        {assumptions.map((a) => (
          <li
            key={a.id}
            style={{
              ...rowStyle,
              opacity: a.resolution === 'rejected' ? 0.5 : 1,
              borderLeft: `3px solid ${riskColor(a.risk)}`,
            }}
          >
            <div style={{ flex: 1 }}>
              <div style={{ fontWeight: 600, fontSize: '0.78rem' }}>
                {a.statement}
              </div>
              <div style={detailRowStyle}>
                <span>Source: {prettySource(a.source)}</span>
                <RiskChip risk={a.risk} />
                {a.affects_nodes.length > 0 && (
                  <span style={{ color: 'var(--color-text-muted)' }}>
                    Affects: {a.affects_nodes.slice(0, 3).join(', ')}
                    {a.affects_nodes.length > 3 ? ` +${a.affects_nodes.length - 3}` : ''}
                  </span>
                )}
              </div>
              {a.chain_of_custody && (
                <div style={custodyInlineStyle}>
                  <div style={custodyHeadingStyle}>Chain of custody</div>
                  <ChainOfCustodyPanel custody={a.chain_of_custody} />
                </div>
              )}
            </div>
            {a.resolution === 'unresolved' && onResolve && (
              <div
                style={{
                  display: 'flex',
                  flexDirection: 'column',
                  gap: '0.3rem',
                  alignItems: 'flex-end',
                }}
              >
                <div style={{ display: 'flex', gap: '0.3rem' }}>
                  <button
                    onClick={() =>
                      onResolve(a.id, 'confirmed', rationales[a.id]?.trim() || undefined)
                    }
                    style={confirmStyle}
                  >
                    Confirm
                  </button>
                  <button
                    onClick={() =>
                      onResolve(a.id, 'rejected', rationales[a.id]?.trim() || undefined)
                    }
                    style={rejectStyle}
                  >
                    Reject
                  </button>
                </div>
                <button
                  onClick={() => toggleRationale(a.id)}
                  style={rationaleToggleStyle}
                  type="button"
                >
                  {openRationales.has(a.id)
                    ? 'Hide rationale'
                    : '+ Add rationale (optional)'}
                </button>
                {openRationales.has(a.id) && (
                  <textarea
                    value={rationales[a.id] ?? ''}
                    onChange={(e) =>
                      setRationales((prev) => ({
                        ...prev,
                        [a.id]: e.target.value,
                      }))
                    }
                    placeholder="Why are you confirming/rejecting this assumption?"
                    aria-label={`Rationale for assumption ${a.id}`}
                    rows={2}
                    style={rationaleTextareaStyle}
                  />
                )}
              </div>
            )}
            {a.resolution !== 'unresolved' && (
              <span style={resolvedStyle}>
                {a.resolution === 'confirmed' ? 'Confirmed' : 'Rejected'}
              </span>
            )}
          </li>
        ))}
      </ul>
      <p style={legendStyle}>
        Unresolved assumptions block transition to
        ValidatedExecutableDag. Confirm an assumption when you're
        comfortable with the inference; reject it to remove the
        affected edge from the composition.
      </p>
    </div>
  )
}

function prettySource(s: string): string {
  // Mirrors the Rust `AssumptionSource` enum's snake_case rename
  // form. The server flattens the tagged-enum object to a bare
  // string before send (see `flatten_assumption_value` in
  // `chat_routes/compose.rs`).
  if (s === 'llm_inferred') return 'LLM inferred'
  if (s === 'lossy_adapter') return 'Lossy adapter'
  if (s === 'ontology_mapping_unresolved')
    return 'Unresolved ontology mapping'
  if (s === 'policy_exception') return 'Policy exception'
  if (s === 'sme_accepted') return 'SME accepted'
  if (s === 'profiler_degraded') return 'Profiler degraded'
  return s.replace(/_/g, ' ')
}

function RiskChip({ risk }: { risk: string }) {
  return (
    <span
      style={{
        background: riskColor(risk),
        color: '#fff',
        padding: '0.05rem 0.4rem',
        borderRadius: '0.3rem',
        fontSize: '0.7rem',
      }}
    >
      {risk}
    </span>
  )
}

function riskColor(risk: string): string {
  switch (risk) {
    case 'clinical':
      return 'var(--color-danger-fg)'
    case 'high':
      return 'var(--color-danger-accent)'
    case 'moderate':
      return 'var(--color-warning-accent)'
    case 'low':
      return 'var(--color-success-fg)'
    default:
      return 'var(--color-text-muted)'
  }
}

const cardStyle: React.CSSProperties = {
  padding: '0.6rem 0.85rem',
  fontSize: '0.78rem',
  background: 'var(--color-surface-muted)',
  border: '1px solid var(--color-border-subtle)',
  borderRadius: '0.4rem',
}
const emptyStyle: React.CSSProperties = {
  padding: '0.6rem 0.85rem',
  fontSize: '0.78rem',
  color: 'var(--color-text-muted)',
  fontStyle: 'italic',
}
const headingStyle: React.CSSProperties = {
  margin: '0 0 0.4rem',
  fontSize: '0.8rem',
  fontWeight: 600,
}
const listStyle: React.CSSProperties = { margin: 0, padding: 0, listStyle: 'none' }
const rowStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'flex-start',
  gap: '0.5rem',
  padding: '0.4rem 0.6rem',
  marginBottom: '0.3rem',
  background: 'var(--color-surface-1)',
  borderRadius: '0.3rem',
}
const detailRowStyle: React.CSSProperties = {
  display: 'flex',
  gap: '0.6rem',
  fontSize: '0.72rem',
  color: 'var(--color-text-muted)',
  marginTop: '0.25rem',
  flexWrap: 'wrap',
}
const confirmStyle: React.CSSProperties = {
  padding: '0.3rem 0.6rem',
  fontSize: '0.72rem',
  background: 'var(--color-success-accent)',
  color: '#fff',
  border: 'none',
  borderRadius: '0.3rem',
  cursor: 'pointer',
}
const rejectStyle: React.CSSProperties = {
  padding: '0.3rem 0.6rem',
  fontSize: '0.72rem',
  background: 'var(--color-danger-accent)',
  color: '#fff',
  border: 'none',
  borderRadius: '0.3rem',
  cursor: 'pointer',
}
const resolvedStyle: React.CSSProperties = {
  fontSize: '0.72rem',
  color: 'var(--color-text-muted)',
  fontStyle: 'italic',
}
const legendStyle: React.CSSProperties = {
  margin: '0.4rem 0 0',
  fontSize: '0.72rem',
  color: 'var(--color-text-muted)',
  fontStyle: 'italic',
}
const rationaleToggleStyle: React.CSSProperties = {
  background: 'transparent',
  border: 'none',
  color: 'var(--color-text-muted)',
  fontSize: '0.7rem',
  cursor: 'pointer',
  padding: 0,
  textDecoration: 'underline',
}
const rationaleTextareaStyle: React.CSSProperties = {
  width: '100%',
  minWidth: '14rem',
  fontSize: '0.74rem',
  padding: '0.3rem 0.4rem',
  borderRadius: '0.3rem',
  border: '1px solid var(--color-border-subtle)',
  background: 'var(--color-surface-0)',
  color: 'var(--color-text-primary)',
  fontFamily: 'inherit',
  resize: 'vertical',
}
const custodyInlineStyle: React.CSSProperties = {
  marginTop: '0.4rem',
  padding: '0.35rem 0.5rem',
  borderRadius: '0.3rem',
  background: 'var(--color-surface-muted)',
  borderLeft: '3px solid var(--color-danger-accent)',
}
const custodyHeadingStyle: React.CSSProperties = {
  fontWeight: 600,
  fontSize: '0.72rem',
  marginBottom: '0.2rem',
}
