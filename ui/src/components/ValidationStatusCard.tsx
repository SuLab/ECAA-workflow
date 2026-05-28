/**
 * `ValidationStatusCard` UI component.
 *
 * Surfaces validation outcomes for one task. Each row shows
 * an obligation id (e.g. `p_value_in_unit_interval`,
 * `gene_id_in_annotation`) and the typed outcome (passed / failed
 * with message / errored with reason / unimplemented).
 *
 * Failures escalate to `BlockerKind::ValidationFailed` via the
 * existing claim-verifier path; this card is the SME's read-only
 * surface into "what did the harness check?"
 *
 * Distinguishes "internally executable / contract-consistent" from
 * "scientifically validated" — design §22 invariant.
 */

export interface ValidationRow {
  obligation_id: string
  outcome: 'passed' | 'failed' | 'errored' | 'unimplemented'
  message?: string
}

interface Props {
  task_id: string
  rows: ValidationRow[]
}

export default function ValidationStatusCard({ task_id, rows }: Props) {
  if (rows.length === 0) {
    return (
      <div role="region" aria-label="Validation status" style={emptyStyle}>
        No validators ran for this task. The composition is
        executable but has no domain-evidence backing — interpret
        scientific conclusions with caution.
      </div>
    )
  }
  const passed = rows.filter((r) => r.outcome === 'passed').length
  const failed = rows.filter((r) => r.outcome === 'failed').length
  const errored = rows.filter((r) => r.outcome === 'errored').length
  const unimpl = rows.filter((r) => r.outcome === 'unimplemented').length

  const overallColor = failed > 0 ? 'var(--color-danger-accent)' : errored > 0 ? 'var(--color-warning-accent)' : 'var(--color-success-accent)'
  const overallLabel = failed > 0 ? 'Failed' : errored > 0 ? 'Partial' : 'Passed'

  return (
    <div role="region" aria-label="Validation status" style={cardStyle}>
      <header style={headerStyle}>
        <h4 style={headingStyle}>Validation status — task {task_id}</h4>
        <span style={{ ...overallChipStyle, background: overallColor }}>
          {overallLabel}
        </span>
      </header>
      <div style={summaryStyle}>
        <Stat label="Passed" value={passed} color="#1f6f3a" />
        <Stat label="Failed" value={failed} color="#a83e2f" />
        <Stat label="Errored" value={errored} color="#9f7d2a" />
        <Stat label="Unimplemented" value={unimpl} color="#5a5a5a" />
      </div>
      <ul style={listStyle}>
        {rows.map((r) => (
          <li
            key={r.obligation_id}
            style={{
              ...rowStyle,
              borderLeft: `3px solid ${outcomeColor(r.outcome)}`,
            }}
          >
            <code style={codeStyle}>{r.obligation_id}</code>
            <OutcomeChip outcome={r.outcome} />
            {r.message && (
              <span style={{ flex: 1, fontSize: '0.74rem', color: 'var(--color-text-muted)' }}>
                {r.message}
              </span>
            )}
          </li>
        ))}
      </ul>
      <p style={legendStyle}>
        Validators check internal contract consistency (e.g. p-values
        in [0,1], gene ids in annotation). They do <em>not</em>
        substitute for domain expert review of biological validity.
      </p>
    </div>
  )
}

function Stat({
  label,
  value,
  color,
}: {
  label: string
  value: number
  color: string
}) {
  return (
    <div style={{ display: 'flex', gap: '0.3rem', alignItems: 'center' }}>
      <span style={{ ...statNumStyle, color }}>{value}</span>
      <span style={statLabelStyle}>{label}</span>
    </div>
  )
}

function OutcomeChip({ outcome }: { outcome: ValidationRow['outcome'] }) {
  return (
    <span
      style={{
        background: outcomeColor(outcome),
        color: '#fff',
        padding: '0.05rem 0.4rem',
        borderRadius: '0.3rem',
        fontSize: '0.7rem',
      }}
    >
      {outcome}
    </span>
  )
}

function outcomeColor(o: ValidationRow['outcome']): string {
  switch (o) {
    case 'passed':
      return 'var(--color-success-accent)'
    case 'failed':
      return 'var(--color-danger-accent)'
    case 'errored':
      return 'var(--color-warning-accent)'
    case 'unimplemented':
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
const headerStyle: React.CSSProperties = {
  display: 'flex',
  justifyContent: 'space-between',
  alignItems: 'center',
  marginBottom: '0.4rem',
}
const headingStyle: React.CSSProperties = {
  margin: 0,
  fontSize: '0.8rem',
  fontWeight: 600,
}
const overallChipStyle: React.CSSProperties = {
  color: '#fff',
  padding: '0.1rem 0.5rem',
  borderRadius: '0.3rem',
  fontSize: '0.74rem',
}
const summaryStyle: React.CSSProperties = {
  display: 'flex',
  gap: '1rem',
  marginBottom: '0.5rem',
}
const statNumStyle: React.CSSProperties = {
  fontWeight: 700,
  fontSize: '0.85rem',
}
const statLabelStyle: React.CSSProperties = {
  fontSize: '0.72rem',
  color: 'var(--color-text-muted)',
}
const listStyle: React.CSSProperties = { margin: 0, padding: 0, listStyle: 'none' }
const rowStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: '0.4rem',
  padding: '0.3rem 0.5rem',
  marginBottom: '0.2rem',
  background: 'var(--color-surface-1)',
  borderRadius: '0.3rem',
}
const codeStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  fontSize: '0.74rem',
  minWidth: '11rem',
}
const legendStyle: React.CSSProperties = {
  margin: '0.4rem 0 0',
  fontSize: '0.72rem',
  color: 'var(--color-text-muted)',
  fontStyle: 'italic',
}
