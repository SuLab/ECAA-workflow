/**
 * `NovelNodeSpecCard` UI component.
 *
 * Surfaces the v4 planner's `NovelNodeSpec` outcome — a hypothesized
 * node the LLM proposed via `propose_hypothesized_node` that needs
 * validation evidence before it can execute. The card renders the
 * proposed node's contract (id, intent, declared inputs/outputs,
 * parent ontology terms) plus the list of validation obligations
 * that must pass before promotion to `ProductionNode`.
 *
 * SMEs use this card to evaluate "the LLM thinks we need a new step
 * — should we accept it?" without leaving the chat surface.
 */

export interface NovelNodeSpec {
  node_id: string
  intent: string
  proposed_parent_terms: string[]
  declared_inputs: string[]
  declared_outputs: string[]
  declared_assumptions: string[]
  declared_failure_modes: string[]
  validation_obligations: string[]
  llm_rationale?: string
}

interface Props {
  spec: NovelNodeSpec
  onAccept?: (node_id: string) => void
  onReject?: (node_id: string) => void
}

export default function NovelNodeSpecCard({ spec, onAccept, onReject }: Props) {
  return (
    <div role="region" aria-label="Novel node specification" style={cardStyle}>
      <header style={headerStyle}>
        <h4 style={headingStyle}>
          Proposed new task: <code style={codeStyle}>{spec.node_id}</code>
        </h4>
        <span style={statusChipStyle}>HypothesizedNode</span>
      </header>
      <p style={intentStyle}>{spec.intent}</p>
      {spec.llm_rationale && (
        <Section title="LLM's rationale">
          <p style={paraStyle}>{spec.llm_rationale}</p>
        </Section>
      )}
      <Section title="Proposed parent ontology terms">
        <ChipList items={spec.proposed_parent_terms} />
      </Section>
      <Section title="Declared inputs">
        <ChipList items={spec.declared_inputs} />
      </Section>
      <Section title="Declared outputs">
        <ChipList items={spec.declared_outputs} />
      </Section>
      {spec.declared_assumptions.length > 0 && (
        <Section title="Declared assumptions">
          <ul style={listStyle}>
            {spec.declared_assumptions.map((a, i) => (
              <li key={i}>{a}</li>
            ))}
          </ul>
        </Section>
      )}
      {spec.declared_failure_modes.length > 0 && (
        <Section title="Declared failure modes">
          <ul style={listStyle}>
            {spec.declared_failure_modes.map((f, i) => (
              <li key={i}>{f}</li>
            ))}
          </ul>
        </Section>
      )}
      <Section title="Validation obligations">
        <ul style={listStyle}>
          {spec.validation_obligations.map((o) => (
            <li key={o}>
              <code style={codeStyle}>{o}</code>
            </li>
          ))}
        </ul>
      </Section>
      <p style={legendStyle}>
        This task hasn't run before. The harness will validate every
        obligation above before allowing the node to execute in
        production. While the node is HypothesizedNode, only draft
        compositions can include it.
      </p>
      <div style={buttonRowStyle}>
        {onReject && (
          <button onClick={() => onReject(spec.node_id)} style={rejectStyle}>
            Reject proposal
          </button>
        )}
        {onAccept && (
          <button onClick={() => onAccept(spec.node_id)} style={acceptStyle}>
            Accept as draft
          </button>
        )}
      </div>
    </div>
  )
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div style={{ marginTop: '0.55rem' }}>
      <strong style={sectionTitle}>{title}</strong>
      {children}
    </div>
  )
}

function ChipList({ items }: { items: string[] }) {
  if (items.length === 0) {
    return <em style={{ fontSize: '0.74rem', color: 'var(--color-text-muted)' }}>none declared</em>
  }
  return (
    <div style={{ display: 'flex', flexWrap: 'wrap', gap: '0.3rem', marginTop: '0.2rem' }}>
      {items.map((it) => (
        <code key={it} style={codeStyle}>
          {it}
        </code>
      ))}
    </div>
  )
}

const cardStyle: React.CSSProperties = {
  padding: '0.75rem 1rem',
  fontSize: '0.78rem',
  background: 'var(--color-surface-muted)',
  border: '1px solid #a83e2f',
  borderRadius: '0.4rem',
}
const headerStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'space-between',
  gap: '0.5rem',
  marginBottom: '0.2rem',
}
const headingStyle: React.CSSProperties = {
  margin: 0,
  fontSize: '0.85rem',
  fontWeight: 600,
}
const statusChipStyle: React.CSSProperties = {
  background: 'var(--color-danger-accent)',
  color: '#fff',
  padding: '0.1rem 0.5rem',
  borderRadius: '0.3rem',
  fontSize: '0.7rem',
}
const intentStyle: React.CSSProperties = {
  margin: '0.3rem 0',
  fontSize: '0.78rem',
  color: 'var(--color-text-secondary)',
}
const sectionTitle: React.CSSProperties = {
  display: 'block',
  fontSize: '0.74rem',
  marginBottom: '0.2rem',
}
const codeStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  background: 'var(--color-surface-1)',
  padding: '0.05rem 0.3rem',
  borderRadius: '0.2rem',
  fontSize: '0.72rem',
}
const paraStyle: React.CSSProperties = {
  margin: 0,
  fontSize: '0.74rem',
  color: 'var(--color-text-secondary)',
}
const listStyle: React.CSSProperties = {
  margin: '0.2rem 0 0',
  paddingLeft: '1.1rem',
  fontSize: '0.74rem',
}
const legendStyle: React.CSSProperties = {
  margin: '0.6rem 0',
  fontSize: '0.72rem',
  color: 'var(--color-text-muted)',
  fontStyle: 'italic',
}
const buttonRowStyle: React.CSSProperties = {
  display: 'flex',
  gap: '0.4rem',
  justifyContent: 'flex-end',
  marginTop: '0.5rem',
}
const acceptStyle: React.CSSProperties = {
  padding: '0.3rem 0.8rem',
  fontSize: '0.74rem',
  background: 'var(--color-success-accent)',
  color: '#fff',
  border: 'none',
  borderRadius: '0.3rem',
  cursor: 'pointer',
}
const rejectStyle: React.CSSProperties = {
  padding: '0.3rem 0.8rem',
  fontSize: '0.74rem',
  background: 'var(--color-danger-accent)',
  color: '#fff',
  border: 'none',
  borderRadius: '0.3rem',
  cursor: 'pointer',
}
