// V3+v4 residuals closure Repairs tab body.
//
// Renders one `RepairProposalCard` per pending proposal returned by
// `useRepairProposals`. Empty state surfaces the "No pending repair
// proposals." placeholder so the SME knows the surface is healthy
// rather than broken. The hook handles the 4s poll loop.

import { useRepairProposals } from '../../hooks/useRepairProposals'
import { RepairProposalCard } from './RepairProposalCard'

interface Props {
  sessionId: string | null
}

export function RepairsTab({ sessionId }: Props): JSX.Element {
  const { proposals, error, loading, accept, reject, refresh } =
    useRepairProposals(sessionId)

  if (!sessionId) {
    return (
      <div style={emptyStyle} aria-label="No session">
        No session selected.
      </div>
    )
  }

  if (loading && proposals.length === 0) {
    return (
      <div style={emptyStyle} aria-label="Loading repair proposals">
        Loading repair proposals…
      </div>
    )
  }

  if (proposals.length === 0) {
    return (
      <div
        style={emptyStyle}
        role="status"
        aria-label="No pending repair proposals"
      >
        No pending repair proposals.
        {error && (
          <div style={errorStyle}>Failed to load repair proposals: {error}</div>
        )}
      </div>
    )
  }

  return (
    <div
      style={containerStyle}
      role="region"
      aria-label="Pending repair proposals"
    >
      {error && (
        <div style={errorStyle}>Failed to refresh repair proposals: {error}</div>
      )}
      {proposals.map((p) => (
        <RepairProposalCard
          key={p.id}
          sessionId={sessionId}
          proposal={p}
          onAccept={async (id, creds) => {
            await accept(id, creds)
            await refresh()
          }}
          onReject={async (id, reason) => {
            await reject(id, reason)
            await refresh()
          }}
        />
      ))}
    </div>
  )
}

const containerStyle: React.CSSProperties = {
  padding: '12px',
  display: 'flex',
  flexDirection: 'column',
  gap: '12px',
  overflow: 'auto',
  height: '100%',
}

const emptyStyle: React.CSSProperties = {
  padding: '12px',
  color: 'var(--color-text-muted, #888)',
  fontSize: '0.88rem',
}

const errorStyle: React.CSSProperties = {
  fontSize: '0.78rem',
  color: 'var(--color-danger-fg, #b71c1c)',
  marginBottom: '8px',
}
