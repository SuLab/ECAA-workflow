/**
 * `AcceptedNodeList` UI component.
 *
 * Renders the v4 planner's accepted-node list as a compact table. SMEs
 * use it to read "what will run" without opening the full DAG canvas.
 * Each row carries the node id, lifecycle state, and trust level so
 * the SME can spot Production-grade vs Contracted nodes at a glance.
 *
 * Pulls from the typed `WorkflowDag.nodes` shape produced by the v4
 * planner. Empty list → "no nodes yet — composition not run." The
 * card is read-only; clicking a node opens the existing
 * `TaskDetailDrawer` via the parent.
 */

export interface AcceptedNode {
  id: string
  human_name: string
  lifecycle_state: string
  trust_level: string
  intent: string
}

interface Props {
  nodes: AcceptedNode[]
  onSelect?: (id: string) => void
}

export default function AcceptedNodeList({ nodes, onSelect }: Props) {
  if (nodes.length === 0) {
    return (
      <div
        role="region"
        aria-label="Accepted nodes"
        style={{
          padding: '0.6rem 0.85rem',
          fontSize: '0.78rem',
          color: 'var(--color-text-muted)',
          fontStyle: 'italic',
        }}
      >
        No nodes accepted yet — composition has not run.
      </div>
    )
  }
  return (
    <div role="region" aria-label="Accepted nodes" style={cardStyle}>
      <h4 style={headingStyle}>{nodes.length} accepted node{nodes.length === 1 ? '' : 's'}</h4>
      <ul style={listStyle}>
        {nodes.map((n) => (
          <li
            key={n.id}
            style={rowStyle}
            onClick={onSelect ? () => onSelect(n.id) : undefined}
            role={onSelect ? 'button' : undefined}
            tabIndex={onSelect ? 0 : undefined}
          >
            <span style={idStyle}>{n.id}</span>
            <LifecycleChip lifecycle={n.lifecycle_state} />
            <TrustChip trust={n.trust_level} />
            <span style={intentStyle}>{n.intent}</span>
          </li>
        ))}
      </ul>
      <p style={legendStyle}>
        Lifecycle indicates how validated the node's contract is; Trust
        indicates whether a curated maintainer has reviewed it.
      </p>
    </div>
  )
}

function LifecycleChip({ lifecycle }: { lifecycle: string }) {
  const color = lifecycleColor(lifecycle)
  return (
    <span style={{ ...chipStyle, background: color, color: '#fff' }}>
      {lifecycle.replace(/_/g, ' ')}
    </span>
  )
}

function TrustChip({ trust }: { trust: string }) {
  const ok = trust === 'reviewed' || trust === 'production'
  return (
    <span
      style={{
        ...chipStyle,
        background: ok ? 'var(--color-success-accent)' : 'var(--color-warning-fg)',
        color: '#fff',
      }}
    >
      {trust.replace(/_/g, ' ')}
    </span>
  )
}

function lifecycleColor(s: string): string {
  switch (s) {
    case 'production':
      return 'var(--color-success-accent)'
    case 'benchmark_validated':
      return 'var(--color-success-fg)'
    case 'locally_validated':
      return 'var(--color-warning-accent)'
    case 'implemented':
      return 'var(--color-warning-fg)'
    case 'contracted':
      return 'var(--color-text-muted)'
    case 'hypothesized':
      return 'var(--color-danger-accent)'
    case 'deprecated':
      return '#a31e1e'
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
const headingStyle: React.CSSProperties = {
  margin: '0 0 0.4rem',
  fontSize: '0.8rem',
  fontWeight: 600,
}
const listStyle: React.CSSProperties = { margin: 0, padding: 0, listStyle: 'none' }
const rowStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: '0.4rem',
  padding: '0.25rem 0',
  borderBottom: '1px solid var(--color-border-subtle)',
  cursor: 'pointer',
}
const idStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  fontSize: '0.74rem',
  minWidth: '7rem',
}
const intentStyle: React.CSSProperties = {
  flex: 1,
  fontSize: '0.74rem',
  color: 'var(--color-text-muted)',
  whiteSpace: 'nowrap',
  overflow: 'hidden',
  textOverflow: 'ellipsis',
}
const chipStyle: React.CSSProperties = {
  fontSize: '0.7rem',
  padding: '0.05rem 0.4rem',
  borderRadius: '0.4rem',
  whiteSpace: 'nowrap',
}
const legendStyle: React.CSSProperties = {
  margin: '0.4rem 0 0',
  fontSize: '0.72rem',
  color: 'var(--color-text-muted)',
  fontStyle: 'italic',
}
