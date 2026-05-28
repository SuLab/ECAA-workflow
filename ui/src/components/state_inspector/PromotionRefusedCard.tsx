// V3+v4 residuals closure PromotionRefusedCard.
//
// Renders the `RefusalKind::PromotionRefused` case of a `RefusalReport`.
// The Rust variant is constructed via
// `RefusalReport::promotion_refused(non_promotable_node_ids,
// missing_summary, unblock_paths)`, which serializes to:
//
// {
// "id": "promotion_refused",
// "kind": { "kind": "promotion_refused" },
// "statement": "N node(s) failed the validation × lifecycle promotion grid:...",
// "references": ["<non_promotable_node_id>",...],
// "unblock_paths": [<UnblockPath>,...]
// }
//
// `references` carries the per-node ids the planner refused; the
// `unblock_paths` enumerate the actionable EscalateToReviewer routes the
// SME can dispatch to recover (one per missing-credential class on the
// failed grid entries). The card surfaces both lists so the SME knows
// which nodes are blocked AND who to escalate to.

import type { RefusalReport } from '../../types/RefusalReport'
import type { UnblockPath } from '../../types/UnblockPath'

interface Props {
  refusal: RefusalReport
}

export function PromotionRefusedCard({ refusal }: Props): JSX.Element | null {
  // Defensive: only render for the typed PromotionRefused kind. Caller
  // is expected to gate on `refusal.kind.kind === 'promotion_refused'`,
  // but the runtime guard means a misrouted refusal won't crash the UI.
  if (refusal.kind.kind !== 'promotion_refused') return null

  const nonPromotableNodes = refusal.references ?? []
  const escalations = (refusal.unblock_paths ?? []).filter(
    (p): p is Extract<UnblockPath, { kind: 'escalate_to_reviewer' }> =>
      p.kind === 'escalate_to_reviewer',
  )
  const otherPaths = (refusal.unblock_paths ?? []).filter(
    (p) => p.kind !== 'escalate_to_reviewer',
  )

  return (
    <section
      role="region"
      aria-label="Promotion refused"
      style={cardStyle}
    >
      <header style={headerStyle}>
        <h3 style={headingStyle}>Promotion refused</h3>
        <span style={badgeStyle}>validation × lifecycle</span>
      </header>
      <p style={statementStyle}>{refusal.statement}</p>

      {nonPromotableNodes.length > 0 && (
        <div style={sectionStyle}>
          <h4 style={subHeadingStyle}>
            Nodes that failed the promotion grid
          </h4>
          <ul style={nodeListStyle}>
            {nonPromotableNodes.map((nodeId) => (
              <li key={nodeId}>
                <code style={codeStyle}>{nodeId}</code>
              </li>
            ))}
          </ul>
        </div>
      )}

      {escalations.length > 0 && (
        <div style={sectionStyle}>
          <h4 style={subHeadingStyle}>Required approvals</h4>
          <ul style={escalationListStyle}>
            {escalations.map((p, i) => (
              <li key={`${p.reviewer_class}-${i}`} style={escalationItemStyle}>
                <strong>{p.reviewer_class}</strong>
                {p.required_artifacts.length > 0 && (
                  <span style={artifactListStyle}>
                    {' '}
                    — needs:{' '}
                    {p.required_artifacts.map((a, j) => (
                      <code key={a} style={codeStyle}>
                        {a}
                        {j < p.required_artifacts.length - 1 ? ', ' : ''}
                      </code>
                    ))}
                  </span>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}

      {otherPaths.length > 0 && (
        <div style={sectionStyle}>
          <h4 style={subHeadingStyle}>Other recovery affordances</h4>
          <ul style={pathListStyle}>
            {otherPaths.map((p, i) => (
              <li key={`${p.kind}-${i}`}>
                <code style={codeStyle}>{p.kind.replace(/_/g, ' ')}</code>
              </li>
            ))}
          </ul>
        </div>
      )}

      {refusal.unblock_paths.length === 0 && (
        <p style={noPathsStyle}>
          No recovery affordances available — branch the session to
          explore alternatives.
        </p>
      )}
    </section>
  )
}

export default PromotionRefusedCard

const cardStyle: React.CSSProperties = {
  border: '1px solid var(--color-danger-fg, #d77)',
  background: 'var(--color-surface-danger, #fef0f0)',
  padding: '12px',
  borderRadius: '4px',
  fontSize: '0.86rem',
  lineHeight: 1.45,
}
const headerStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'space-between',
  gap: '0.5rem',
  marginBottom: '0.4rem',
}
const headingStyle: React.CSSProperties = {
  margin: 0,
  fontSize: '1rem',
  fontWeight: 600,
  color: 'var(--color-danger-fg, #b71c1c)',
}
const badgeStyle: React.CSSProperties = {
  fontSize: '0.7rem',
  padding: '0.15rem 0.45rem',
  borderRadius: '0.25rem',
  background: 'var(--color-danger-muted, #ffcdd2)',
  color: 'var(--color-danger-fg, #b71c1c)',
  fontFamily: 'ui-monospace, monospace',
}
const statementStyle: React.CSSProperties = {
  margin: '0.3rem 0 0.6rem',
}
const sectionStyle: React.CSSProperties = {
  marginTop: '0.5rem',
}
const subHeadingStyle: React.CSSProperties = {
  margin: '0 0 0.25rem',
  fontSize: '0.82rem',
  fontWeight: 600,
}
const nodeListStyle: React.CSSProperties = {
  margin: 0,
  paddingLeft: '1.2rem',
  fontSize: '0.78rem',
}
const escalationListStyle: React.CSSProperties = {
  margin: 0,
  paddingLeft: '1.2rem',
  fontSize: '0.82rem',
}
const escalationItemStyle: React.CSSProperties = {
  marginBottom: '0.2rem',
}
const artifactListStyle: React.CSSProperties = {
  color: 'var(--color-text-muted, #555)',
  fontSize: '0.78rem',
}
const pathListStyle: React.CSSProperties = {
  margin: 0,
  paddingLeft: '1.2rem',
  fontSize: '0.78rem',
}
const codeStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  background: 'var(--color-surface-1, #fff)',
  padding: '0.05rem 0.3rem',
  borderRadius: '0.2rem',
  fontSize: '0.76rem',
}
const noPathsStyle: React.CSSProperties = {
  margin: '0.4rem 0 0',
  color: 'var(--color-text-muted, #555)',
  fontStyle: 'italic',
  fontSize: '0.8rem',
}
