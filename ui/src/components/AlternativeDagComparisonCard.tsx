/**
 * `AlternativeDagComparisonCard` UI component.
 *
 * Surfaces the v4 planner's top-K ranked alternatives. Each row
 * shows the alternative's compact summary, score breakdown, node
 * count, adapter counts, and unresolved-assumption count. SMEs use
 * the card to pick among "RNA-seq DE via DESeq2 with two
 * adapters" vs "RNA-seq DE via edgeR with no adapters" without
 * needing to render every DAG.
 *
 * The first alternative is the planner's recommendation; later rows
 * are valid runners-up. Selecting an alternative dispatches through
 * the existing `set_intake_method` / `select_sensitivity_winner`
 * paths so the lineage trail records the SME's choice.
 */

export interface AlternativeSummary {
  dag_id: string
  summary: string
  node_count: number
  edge_count: number
  total_adapters: number
  risky_adapters: number
  unresolved_assumptions: number
  reproducibility_score: number
}

interface Props {
  alternatives: AlternativeSummary[]
  selected_dag_id?: string | null
  onSelect?: (dag_id: string) => void
}

export default function AlternativeDagComparisonCard({
  alternatives,
  selected_dag_id,
  onSelect,
}: Props) {
  if (alternatives.length === 0) {
    return (
      <div role="region" aria-label="Alternative compositions" style={emptyStyle}>
        Only one composition produced — no alternatives to compare.
      </div>
    )
  }
  return (
    <div role="region" aria-label="Alternative compositions" style={cardStyle}>
      <h4 style={headingStyle}>
        {alternatives.length} alternative{alternatives.length === 1 ? '' : 's'}
      </h4>
      <ul style={listStyle}>
        {alternatives.map((alt, idx) => {
          const isRecommended = idx === 0
          const isSelected = selected_dag_id === alt.dag_id
          return (
            <li
              key={`${alt.dag_id}-${idx}`}
              style={{
                ...rowStyle,
                borderLeft: isSelected
                  ? '3px solid #1f6f3a'
                  : isRecommended
                    ? '3px solid #3a7a4f'
                    : '3px solid #5a5a5a',
                background: isSelected
                  ? 'var(--color-surface-1)'
                  : 'var(--color-surface-muted)',
              }}
            >
              <div style={{ flex: 1 }}>
                <div style={titleRow}>
                  <strong>{isRecommended ? '★ Recommended' : `Alt ${idx + 1}`}</strong>
                  <code style={dagIdStyle}>{alt.dag_id}</code>
                  {isSelected && <span style={selectedChipStyle}>Selected</span>}
                </div>
                <div style={summaryRow}>{alt.summary}</div>
                <div style={statsRow}>
                  <Stat label="Nodes" value={alt.node_count} />
                  <Stat label="Edges" value={alt.edge_count} />
                  <Stat
                    label="Adapters"
                    value={alt.total_adapters}
                    detail={
                      alt.risky_adapters > 0
                        ? `${alt.risky_adapters} risky`
                        : undefined
                    }
                    color={alt.risky_adapters > 0 ? 'var(--color-danger-accent)' : undefined}
                  />
                  <Stat
                    label="Assumptions"
                    value={alt.unresolved_assumptions}
                    color={alt.unresolved_assumptions > 0 ? 'var(--color-warning-accent)' : undefined}
                  />
                  <Stat
                    label="Repro"
                    value={alt.reproducibility_score}
                    detail="/10"
                    color={alt.reproducibility_score >= 8 ? 'var(--color-success-accent)' : 'var(--color-warning-accent)'}
                  />
                </div>
              </div>
              {onSelect && !isSelected && (
                <button onClick={() => onSelect(alt.dag_id)} style={selectStyle}>
                  Select
                </button>
              )}
            </li>
          )
        })}
      </ul>
      <p style={legendStyle}>
        The recommended alternative scores best across the planner's
        16-component tuple (trust, risk, repro, etc.). Runners-up
        remain valid; pick a different alternative when the SME has
        domain knowledge the planner lacks (e.g. a runner-up uses an
        in-house validated pipeline).
      </p>
    </div>
  )
}

function Stat({
  label,
  value,
  detail,
  color,
}: {
  label: string
  value: number
  detail?: string
  color?: string
}) {
  return (
    <span style={statBlockStyle}>
      <span style={{ ...statNumStyle, color: color ?? 'var(--color-text-primary)' }}>
        {value}
        {detail && <span style={statDetailStyle}>{detail}</span>}
      </span>
      <span style={statLabelStyle}>{label}</span>
    </span>
  )
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
  gap: '0.5rem',
  padding: '0.5rem 0.6rem',
  marginBottom: '0.4rem',
  borderRadius: '0.3rem',
}
const titleRow: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: '0.3rem',
}
const dagIdStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  fontSize: '0.7rem',
  color: 'var(--color-text-muted)',
}
const selectedChipStyle: React.CSSProperties = {
  background: 'var(--color-success-accent)',
  color: '#fff',
  padding: '0.05rem 0.3rem',
  borderRadius: '0.3rem',
  fontSize: '0.7rem',
}
const summaryRow: React.CSSProperties = {
  fontSize: '0.74rem',
  color: 'var(--color-text-secondary)',
  marginTop: '0.2rem',
}
const statsRow: React.CSSProperties = {
  display: 'flex',
  gap: '0.6rem',
  marginTop: '0.3rem',
  flexWrap: 'wrap',
}
const statBlockStyle: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  alignItems: 'flex-start',
  fontSize: '0.7rem',
}
const statNumStyle: React.CSSProperties = {
  fontWeight: 700,
  fontSize: '0.78rem',
}
const statDetailStyle: React.CSSProperties = {
  marginLeft: '0.15rem',
  fontWeight: 400,
  fontSize: '0.65rem',
  color: 'var(--color-text-muted)',
}
const statLabelStyle: React.CSSProperties = {
  color: 'var(--color-text-muted)',
}
const selectStyle: React.CSSProperties = {
  alignSelf: 'center',
  padding: '0.3rem 0.7rem',
  fontSize: '0.74rem',
  background: 'var(--color-success-accent)',
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
