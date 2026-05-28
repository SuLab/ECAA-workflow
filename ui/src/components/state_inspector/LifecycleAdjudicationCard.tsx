/**
 * V3 `LifecycleAdjudicationCard` UI component (design §7).
 *
 * Renders the session-scoped adjudication queue for the six
 * non-monotonic lifecycle edges (same-user contradiction, cross-user
 * conflict, upstream invalidation, forbidden waiver, verifier-
 * discovered unresolvability, production-node revocation).
 *
 * Each open entry surfaces:
 *  - The transition kind (chip)
 *  - A human summary of the transition
 *  - A Resolve action that POSTs `/api/chat/session/:id/adjudication/:entry_id/resolve`
 *  with `{decided_by, decision}` from a small form
 *
 * Resolved entries render dimmed with the decided_by + decision shown.
 */

import { useState } from 'react'

export interface LifecycleTransition {
  kind:
    | 'same_user_contradiction'
    | 'cross_user_conflict'
    | 'upstream_invalidation'
    | 'forbidden_waiver_attempt'
    | 'verifier_unresolvability'
    | 'production_node_revocation'
  actor?: string
  actor_a?: string
  actor_b?: string
  assumption_id?: string
  prior_record_id?: string
  new_record_id?: string
  records?: string[]
  invalidating_change?: string
  affected_downstream?: string[]
  policy_rule_id?: string
  verifier?: string
  reason?: string
  node_id?: string
  prior_state?: string
  affected_dags?: string[]
}

export type AdjudicationStatus =
  | { kind: 'open' }
  | { kind: 'resolved'; decided_by: string; decision: string; decided_at: string }
  | { kind: 'deferred_to_operator'; reason: string }

export interface AdjudicationQueueEntry {
  id: string
  created_at: string
  transition: LifecycleTransition
  status: AdjudicationStatus
}

interface Props {
  entries: AdjudicationQueueEntry[]
  onResolve?: (entryId: string, decidedBy: string, decision: string) => void
}

export default function LifecycleAdjudicationCard({ entries, onResolve }: Props) {
  if (entries.length === 0) {
    return (
      <div role="region" aria-label="Lifecycle adjudication queue" style={emptyStyle}>
        No lifecycle adjudication entries queued.
      </div>
    )
  }
  return (
    <div role="region" aria-label="Lifecycle adjudication queue" style={cardStyle}>
      <h4 style={headingStyle}>
        {entries.length} lifecycle adjudication{entries.length === 1 ? '' : 's'}
      </h4>
      <ul style={listStyle}>
        {entries.map((e) => (
          <AdjudicationRow key={e.id} entry={e} onResolve={onResolve} />
        ))}
      </ul>
      <p style={legendStyle}>
        Non-monotonic lifecycle edges (contradictions, forbidden
        waivers, production revocations) require explicit resolution
        before the workflow can advance.
      </p>
    </div>
  )
}

function AdjudicationRow({
  entry,
  onResolve,
}: {
  entry: AdjudicationQueueEntry
  onResolve?: (entryId: string, decidedBy: string, decision: string) => void
}) {
  const [decidedBy, setDecidedBy] = useState('')
  const [decision, setDecision] = useState('')
  const isOpen = entry.status.kind === 'open'
  const accent = transitionColor(entry.transition.kind)
  return (
    <li
      style={{
        ...rowStyle,
        borderLeft: `3px solid ${accent}`,
        opacity: isOpen ? 1 : 0.65,
      }}
    >
      <div style={{ flex: 1 }}>
        <div style={titleStyle}>
          <TransitionChip kind={entry.transition.kind} />
          <code style={codeStyle}>{entry.id}</code>
        </div>
        <div style={summaryStyle}>{transitionSummary(entry.transition)}</div>
        {entry.status.kind === 'resolved' && (
          <div style={resolvedStyle}>
            Resolved by <strong>{entry.status.decided_by}</strong>:{' '}
            {entry.status.decision}
          </div>
        )}
        {entry.status.kind === 'deferred_to_operator' && (
          <div style={resolvedStyle}>
            Deferred to operator: {entry.status.reason}
          </div>
        )}
        {isOpen && onResolve && (
          <div style={formStyle}>
            <input
              type="text"
              placeholder="decided_by"
              value={decidedBy}
              onChange={(e) => setDecidedBy(e.target.value)}
              style={inputStyle}
              aria-label="Decided by"
            />
            <input
              type="text"
              placeholder="decision"
              value={decision}
              onChange={(e) => setDecision(e.target.value)}
              style={inputStyle}
              aria-label="Decision narrative"
            />
            <button
              onClick={() => onResolve(entry.id, decidedBy, decision)}
              disabled={!decidedBy.trim() || !decision.trim()}
              style={resolveStyle}
            >
              Resolve
            </button>
          </div>
        )}
      </div>
    </li>
  )
}

function TransitionChip({ kind }: { kind: LifecycleTransition['kind'] }) {
  return (
    <span
      style={{
        background: transitionColor(kind),
        color: '#fff',
        padding: '0.05rem 0.4rem',
        borderRadius: '0.3rem',
        fontSize: '0.7rem',
      }}
    >
      {kind.replace(/_/g, ' ')}
    </span>
  )
}

function transitionColor(kind: LifecycleTransition['kind']): string {
  switch (kind) {
    case 'same_user_contradiction':
    case 'cross_user_conflict':
      return 'var(--color-warning-accent)'
    case 'forbidden_waiver_attempt':
    case 'production_node_revocation':
      return 'var(--color-danger-accent)'
    case 'verifier_unresolvability':
      return 'var(--color-danger-fg)'
    case 'upstream_invalidation':
      return 'var(--color-info-accent)'
  }
}

function transitionSummary(t: LifecycleTransition): string {
  switch (t.kind) {
    case 'same_user_contradiction':
      return `${t.actor ?? 'actor'} authored opposite resolutions on assumption ${t.assumption_id}`
    case 'cross_user_conflict':
      return `${t.actor_a ?? 'a'} and ${t.actor_b ?? 'b'} disagree on assumption ${t.assumption_id}`
    case 'upstream_invalidation':
      return `assumption ${t.assumption_id} invalidated: ${t.invalidating_change ?? ''}`
    case 'forbidden_waiver_attempt':
      return `${t.actor ?? 'actor'} attempted to waive blocking policy ${t.policy_rule_id} on ${t.assumption_id}`
    case 'verifier_unresolvability':
      return `${t.verifier ?? 'verifier'} flagged ${t.assumption_id} as unresolvable: ${t.reason ?? ''}`
    case 'production_node_revocation':
      return `${t.node_id ?? 'node'} demoted from ${t.prior_state ?? 'production'}: ${t.reason ?? ''}`
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
  padding: '0.5rem 0.6rem',
  marginBottom: '0.3rem',
  background: 'var(--color-surface-1)',
  borderRadius: '0.3rem',
}
const titleStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: '0.3rem',
  flexWrap: 'wrap',
}
const codeStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  background: 'var(--color-surface-muted)',
  padding: '0.05rem 0.3rem',
  borderRadius: '0.2rem',
  fontSize: '0.74rem',
}
const summaryStyle: React.CSSProperties = {
  fontSize: '0.74rem',
  color: 'var(--color-text-secondary)',
  marginTop: '0.3rem',
}
const resolvedStyle: React.CSSProperties = {
  fontSize: '0.72rem',
  color: 'var(--color-text-muted)',
  fontStyle: 'italic',
  marginTop: '0.3rem',
}
const formStyle: React.CSSProperties = {
  display: 'flex',
  gap: '0.3rem',
  flexWrap: 'wrap',
  marginTop: '0.4rem',
}
const inputStyle: React.CSSProperties = {
  padding: '0.25rem 0.4rem',
  fontSize: '0.72rem',
  border: '1px solid var(--color-border-subtle)',
  borderRadius: '0.25rem',
  background: 'var(--color-surface-muted)',
  color: 'inherit',
}
const resolveStyle: React.CSSProperties = {
  padding: '0.3rem 0.6rem',
  fontSize: '0.72rem',
  background: 'var(--color-info-accent)',
  color: '#fff',
  border: 'none',
  borderRadius: '0.3rem',
  cursor: 'pointer',
}
const legendStyle: React.CSSProperties = {
  margin: '0.4rem 0 0',
  fontSize: '0.72rem',
  color: 'var(--color-text-muted)',
  fontStyle: 'italic',
}
