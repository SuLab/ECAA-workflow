/**
 * `RefusalReportCard` UI component.
 *
 * Surfaces a `ComposeOutcome::Refusal` — when the v4 planner refused
 * to produce any composition (clinical policy violation, hard
 * compatibility refusal, missing safety evidence). The card carries
 * a refusal kind, statement, references to the offending nodes /
 * policy IDs, and a recovery affordance per kind.
 *
 * Distinct from `CompositionInfeasibleCard`, which surfaces "the
 * planner ran but found no chain"; this card surfaces "the planner
 * refused to even try because policy / safety blocks it."
 */

export interface RefusalReport {
  id: string
  kind: 'policy' | 'safety' | 'archetype_inconsistent' | 'v4_planner_refused' | string
  statement: string
  references: string[]
}

interface Props {
  report: RefusalReport
  onBranch?: () => void
  onAmendPolicy?: () => void
}

export default function RefusalReportCard({ report, onBranch, onAmendPolicy }: Props) {
  return (
    <div role="region" aria-label="Composition refused" style={cardStyle}>
      <header style={headerStyle}>
        <h4 style={headingStyle}>Composition refused</h4>
        <KindChip kind={report.kind} />
      </header>
      <p style={statementStyle}>{report.statement}</p>
      {report.references.length > 0 && (
        <div style={refsBlock}>
          <strong style={refsHeading}>References</strong>
          <ul style={refsList}>
            {report.references.map((r, i) => (
              <li key={i}>
                <code style={codeStyle}>{r}</code>
              </li>
            ))}
          </ul>
        </div>
      )}
      <p style={legendStyle}>
        {recoveryHint(report.kind)}
      </p>
      <div style={buttonRow}>
        {onAmendPolicy && (
          <button onClick={onAmendPolicy} style={secondaryStyle}>
            Review policy
          </button>
        )}
        {onBranch && (
          <button onClick={onBranch} style={primaryStyle}>
            Branch session
          </button>
        )}
      </div>
    </div>
  )
}

function KindChip({ kind }: { kind: string }) {
  const color =
    kind === 'policy' || kind === 'v4_planner_refused'
      ? 'var(--color-danger-fg)'
      : kind === 'safety'
        ? 'var(--color-danger-accent)'
        : 'var(--color-warning-accent)'
  return (
    <span
      style={{
        background: color,
        color: '#fff',
        padding: '0.1rem 0.5rem',
        borderRadius: '0.3rem',
        fontSize: '0.74rem',
      }}
    >
      {kind.replace(/_/g, ' ')}
    </span>
  )
}

function recoveryHint(kind: string): string {
  if (kind === 'policy') {
    return 'A clinical / regulated policy bundle blocked the composition. Either branch the session and relax the policy, or amend the upstream nodes to satisfy the policy (pinned containers, validated nodes, no generated code, etc.).'
  }
  if (kind === 'safety') {
    return 'A generated-code or unsafe adapter was inserted that the active sandbox policy refuses. Review the offending node and either accept the safety evidence or remove the upstream that introduced it.'
  }
  if (kind === 'archetype_inconsistent') {
    return "The matched archetype's scaffold is internally inconsistent (cycle, exclusion conflict, inheritance issue). The archetype YAML needs review by the operator who maintains config/archetypes/."
  }
  return 'Branch the session and inspect the planner trace. The refusal references list points at the offending nodes / policy ids.'
}

const cardStyle: React.CSSProperties = {
  padding: '0.75rem 1rem',
  fontSize: '0.78rem',
  background: 'var(--color-surface-muted)',
  border: '2px solid #a83e2f',
  borderRadius: '0.4rem',
}
const headerStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'space-between',
  marginBottom: '0.4rem',
}
const headingStyle: React.CSSProperties = {
  margin: 0,
  fontSize: '0.85rem',
  fontWeight: 600,
}
const statementStyle: React.CSSProperties = {
  margin: '0.2rem 0',
  fontSize: '0.78rem',
  color: 'var(--color-text-primary)',
  lineHeight: 1.45,
}
const refsBlock: React.CSSProperties = {
  marginTop: '0.55rem',
}
const refsHeading: React.CSSProperties = {
  display: 'block',
  fontSize: '0.74rem',
  marginBottom: '0.2rem',
}
const refsList: React.CSSProperties = {
  margin: 0,
  paddingLeft: '1.1rem',
  fontSize: '0.74rem',
}
const codeStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  background: 'var(--color-surface-1)',
  padding: '0.05rem 0.3rem',
  borderRadius: '0.2rem',
}
const legendStyle: React.CSSProperties = {
  margin: '0.6rem 0 0',
  fontSize: '0.72rem',
  color: 'var(--color-text-muted)',
  fontStyle: 'italic',
}
const buttonRow: React.CSSProperties = {
  display: 'flex',
  gap: '0.4rem',
  justifyContent: 'flex-end',
  marginTop: '0.55rem',
}
const primaryStyle: React.CSSProperties = {
  padding: '0.35rem 0.8rem',
  fontSize: '0.74rem',
  background: 'var(--color-success-accent)',
  color: '#fff',
  border: 'none',
  borderRadius: '0.3rem',
  cursor: 'pointer',
}
const secondaryStyle: React.CSSProperties = {
  padding: '0.35rem 0.8rem',
  fontSize: '0.74rem',
  background: 'var(--color-surface-1)',
  color: 'var(--color-text-primary)',
  border: '1px solid var(--color-border-subtle)',
  borderRadius: '0.3rem',
  cursor: 'pointer',
}
