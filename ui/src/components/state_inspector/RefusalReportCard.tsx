/**
 * `RefusalReportCard` (state-inspector variant).
 *
 * The `ui/src/components/RefusalReportCard.tsx` component is retained
 * for back-compat with the legacy session view; this card is the
 * v4-aware surface that renders the typed `RefusalKind` enum + the
 * `unblock_paths` array inline as `UnblockPathCard` instances.
 *
 * Hosted by `CompositionTab` when the active outcome is a refusal.
 */

import React from 'react'
import UnblockPathCard from './UnblockPathCard'
import type { RefusalReport } from '../../types/RefusalReport'

interface Props {
  sessionId: string
  report: RefusalReport
  onDispatched: () => void
  onBranch?: () => void
  onAmendPolicy?: () => void
}

export default function RefusalReportCard({
  sessionId,
  report,
  onDispatched,
  onBranch,
  onAmendPolicy,
}: Props) {
  // `report.kind` may be either the legacy free-text string (pre-Phase-4
  // sessions on disk) or the new typed `RefusalKind` JSON shape
  // ({ kind: "..." } or { kind: "sandbox_refused", category: "..." }).
  // The display label normalizer handles both.
  const kindLabel = normalizeKindLabel(report.kind as unknown)
  const paths = report.unblock_paths
  const hasPaths = paths.length > 0

  return (
    <div role="region" aria-label="Composition refused" style={cardStyle}>
      <header style={headerStyle}>
        <h4 style={headingStyle}>Composition refused</h4>
        <KindChip kind={kindLabel} />
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

      {hasPaths && (
        <div style={unblockBlock}>
          <strong style={unblockHeading}>Recovery paths</strong>
          {paths.map((p, i) => (
            <UnblockPathCard
              key={i}
              sessionId={sessionId}
              refusalId={report.id}
              pathIndex={i}
              path={p}
              onDispatched={onDispatched}
            />
          ))}
        </div>
      )}

      {!hasPaths && (
        <p style={legendStyle}>
          This refusal is unconditional hard policy. The only recovery
          is to branch the session and adjust upstream constraints.
        </p>
      )}

      <div style={buttonRow}>
        {onAmendPolicy && (
          <button
            type="button"
            onClick={onAmendPolicy}
            style={secondaryStyle}
          >
            Review policy
          </button>
        )}
        {onBranch && (
          <button type="button" onClick={onBranch} style={primaryStyle}>
            Branch session
          </button>
        )}
      </div>
    </div>
  )
}

/**
 * Normalize the `kind` field across the pre-Phase-4 string shape and
 * the new typed-enum shape. Returns a human-readable label suitable
 * for the kind-chip header.
 */
function normalizeKindLabel(kind: unknown): string {
  if (typeof kind === 'string') {
    return kind.replace(/_/g, ' ')
  }
  if (
    kind &&
    typeof kind === 'object' &&
    'kind' in (kind as Record<string, unknown>) &&
    typeof (kind as { kind: unknown }).kind === 'string'
  ) {
    const k = (kind as { kind: string }).kind
    if (
      k === 'sandbox_refused' &&
      'category' in (kind as Record<string, unknown>)
    ) {
      const cat = (kind as { category: string }).category
      return `sandbox refused (${cat.replace(/_/g, ' ')})`
    }
    return k.replace(/_/g, ' ')
  }
  return 'unknown'
}

function KindChip({ kind }: { kind: string }) {
  return <span style={chipStyle}>{kind}</span>
}

const cardStyle: React.CSSProperties = {
  padding: '0.75rem 1rem',
  fontSize: '0.78rem',
  background: 'var(--color-surface-muted, #f6f6f6)',
  border: '2px solid var(--color-danger-fg, #a83e2f)',
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
const chipStyle: React.CSSProperties = {
  background: 'var(--color-danger-fg, #a83e2f)',
  color: '#fff',
  padding: '0.1rem 0.5rem',
  borderRadius: '0.3rem',
  fontSize: '0.74rem',
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
  background: 'var(--color-surface-1, #fff)',
  padding: '0.05rem 0.3rem',
  borderRadius: '0.2rem',
}
const unblockBlock: React.CSSProperties = {
  marginTop: '0.55rem',
}
const unblockHeading: React.CSSProperties = {
  display: 'block',
  fontSize: '0.74rem',
  marginBottom: '0.3rem',
}
const legendStyle: React.CSSProperties = {
  margin: '0.6rem 0 0',
  fontSize: '0.72rem',
  color: 'var(--color-text-muted, #666)',
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
  background: 'var(--color-success-accent, #2e7d32)',
  color: '#fff',
  border: 'none',
  borderRadius: '0.3rem',
  cursor: 'pointer',
}
const secondaryStyle: React.CSSProperties = {
  padding: '0.35rem 0.8rem',
  fontSize: '0.74rem',
  background: 'var(--color-surface-1, #fff)',
  color: 'var(--color-text-primary)',
  border: '1px solid var(--color-border-subtle, #ccc)',
  borderRadius: '0.3rem',
  cursor: 'pointer',
}
