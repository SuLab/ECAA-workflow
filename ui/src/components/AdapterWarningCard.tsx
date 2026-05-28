/**
 * `AdapterWarningCard` UI component.
 *
 * Surfaces inserted adapters whose safety class is not `Lossless`.
 * SMEs see one row per inserted adapter with the adapter id, class,
 * safety tier, the affected edge, and the rationale (e.g.
 * "GRCh37→GRCh38 liftover; ~1% of regions require manual review").
 *
 * Risky / policy-restricted adapters block production status until
 * the SME explicitly confirms; the card surfaces a confirm/reject
 * affordance per row.
 */

export interface AdapterWarning {
  adapter_id: string
  adapter_class: string
  safety: 'lossy_declared' | 'scientifically_risky' | 'policy_restricted'
  affected_edge: string
  rationale: string
  resolution: 'unresolved' | 'confirmed' | 'rejected'
}

interface Props {
  adapters: AdapterWarning[]
  onConfirm?: (adapterId: string) => void
  onReject?: (adapterId: string) => void
}

export default function AdapterWarningCard({ adapters, onConfirm, onReject }: Props) {
  if (adapters.length === 0) {
    return (
      <div role="region" aria-label="Adapter warnings" style={emptyStyle}>
        No risky adapters inserted — every edge held under
        lossless-only adapter policy.
      </div>
    )
  }
  return (
    <div role="region" aria-label="Adapter warnings" style={cardStyle}>
      <h4 style={headingStyle}>
        {adapters.length} adapter warning{adapters.length === 1 ? '' : 's'}
      </h4>
      <ul style={listStyle}>
        {adapters.map((a) => (
          <li
            key={a.adapter_id}
            style={{
              ...rowStyle,
              borderLeft: `3px solid ${safetyColor(a.safety)}`,
              opacity: a.resolution === 'rejected' ? 0.5 : 1,
            }}
          >
            <div style={{ flex: 1 }}>
              <div style={titleStyle}>
                <code style={codeStyle}>{a.adapter_id}</code>
                <SafetyChip safety={a.safety} />
                <span style={classStyle}>{a.adapter_class}</span>
              </div>
              <div style={edgeStyle}>Edge: {a.affected_edge}</div>
              <div style={rationaleStyle}>{a.rationale}</div>
            </div>
            {a.resolution === 'unresolved' && (
              <div style={{ display: 'flex', gap: '0.3rem' }}>
                {onConfirm && (
                  <button
                    onClick={() => onConfirm(a.adapter_id)}
                    style={confirmStyle}
                  >
                    Confirm
                  </button>
                )}
                {onReject && (
                  <button
                    onClick={() => onReject(a.adapter_id)}
                    style={rejectStyle}
                  >
                    Reject
                  </button>
                )}
              </div>
            )}
            {a.resolution !== 'unresolved' && (
              <span style={resolvedStyle}>
                {a.resolution === 'confirmed' ? '✓ Confirmed' : '✗ Rejected'}
              </span>
            )}
          </li>
        ))}
      </ul>
      <p style={legendStyle}>
        Lossy / risky adapters change the data in ways that can affect
        biological conclusions. Confirm only when you're comfortable
        with the documented loss; rejection drops the adapter and
        forces the planner to re-route.
      </p>
    </div>
  )
}

function SafetyChip({ safety }: { safety: AdapterWarning['safety'] }) {
  return (
    <span
      style={{
        background: safetyColor(safety),
        color: '#fff',
        padding: '0.05rem 0.4rem',
        borderRadius: '0.3rem',
        fontSize: '0.7rem',
        marginLeft: '0.3rem',
      }}
    >
      {safety.replace(/_/g, ' ')}
    </span>
  )
}

function safetyColor(s: AdapterWarning['safety']): string {
  switch (s) {
    case 'lossy_declared':
      return 'var(--color-warning-accent)'
    case 'scientifically_risky':
      return 'var(--color-danger-accent)'
    case 'policy_restricted':
      return 'var(--color-danger-fg)'
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
const classStyle: React.CSSProperties = {
  fontSize: '0.72rem',
  color: 'var(--color-text-muted)',
}
const edgeStyle: React.CSSProperties = {
  fontSize: '0.72rem',
  color: 'var(--color-text-muted)',
  marginTop: '0.2rem',
  fontFamily: 'ui-monospace, monospace',
}
const rationaleStyle: React.CSSProperties = {
  fontSize: '0.74rem',
  color: 'var(--color-text-secondary)',
  marginTop: '0.3rem',
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
