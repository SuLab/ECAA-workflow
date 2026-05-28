/**
 * `RejectedCandidateList` UI component.
 *
 * Counterpart to `AcceptedNodeList`: shows the candidate atoms the v4
 * planner *considered* but ruled out, with the reason for each
 * rejection. SMEs use this to answer "why didn't $favorite_atom run?"
 * without opening the planner internals.
 *
 * Reasons surface from the typed `IneligibleReason` enum in core
 * (`composition_infeasible` blocker payload). When the planner ran
 * cleanly with no rejections, the component renders an empty-state
 * placeholder.
 */

export interface RejectedCandidate {
  atom_id: string
  reason: string
  reason_detail?: string
}

interface Props {
  candidates: RejectedCandidate[]
}

export default function RejectedCandidateList({ candidates }: Props) {
  if (candidates.length === 0) {
    return (
      <div
        role="region"
        aria-label="Rejected candidates"
        style={emptyStyle}
      >
        No candidate atoms were rejected — every considered node made
        it into the composition.
      </div>
    )
  }
  return (
    <div role="region" aria-label="Rejected candidates" style={cardStyle}>
      <h4 style={headingStyle}>
        {candidates.length} rejected candidate{candidates.length === 1 ? '' : 's'}
      </h4>
      <ul style={listStyle}>
        {candidates.map((c, i) => (
          <li key={`${c.atom_id}-${i}`} style={rowStyle}>
            <span style={idStyle}>{c.atom_id}</span>
            <span style={reasonStyle}>{prettyReason(c.reason)}</span>
            {c.reason_detail && (
              <span style={detailStyle}>{c.reason_detail}</span>
            )}
          </li>
        ))}
      </ul>
      <p style={legendStyle}>
        Each candidate failed at least one planner gate
        (compatibility / policy / lifecycle). Branch the session and
        edit the relevant atom YAML or policy bundle to revisit a
        rejection.
      </p>
    </div>
  )
}

function prettyReason(s: string): string {
  switch (s) {
    case 'incompatible_input':
      return 'Input port type mismatch'
    case 'incompatible_output':
      return 'Output port type mismatch'
    case 'lifecycle_too_early':
      return 'Node lifecycle below policy threshold'
    case 'policy_violation':
      return 'Policy refused'
    case 'duplicate_producer':
      return 'Duplicate producer for required input'
    case 'unfilled_slot':
      return 'No upstream producer + no intake field'
    default:
      return s.replace(/_/g, ' ')
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
  alignItems: 'baseline',
  gap: '0.5rem',
  padding: '0.3rem 0',
  borderBottom: '1px solid var(--color-border-subtle)',
}
const idStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  fontSize: '0.74rem',
  fontWeight: 600,
  minWidth: '7rem',
}
const reasonStyle: React.CSSProperties = {
  fontSize: '0.74rem',
  color: 'var(--color-text-secondary)',
}
const detailStyle: React.CSSProperties = {
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
