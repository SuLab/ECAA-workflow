// Renders a compact affordance-status pill for a figure resolved by the
// plot-affordance registry. Five variants mirror the five `PlotAffordance`
// discriminant kinds: registered (Validated), inherited_via_ontology
// (Inherited from …), structural_fallback (Generic), generated_sandboxed
// (Generated), and deferred (No automatic plot).
//
// The badge is read-only (cursor: default). Tooltip text is attached via
// title for inherited and structural_fallback variants where the rationale
// or warning carries useful detail.

import type { PlotAffordance } from '../types/PlotAffordance'

const STYLES = {
  base: {
    display: 'inline-block',
    padding: '2px 8px',
    borderRadius: '4px',
    fontSize: '11px',
    fontWeight: 600,
    textTransform: 'uppercase' as const,
    letterSpacing: '0.04em',
    cursor: 'default',
  },
  // All bg/fg pairs target WCAG AA ≥ 4.5:1 contrast against white and
  // against the `var(--color-surface-1)` inspector background.
  validated: {
    background: '#1f5f3a',
    color: '#e8f5ee',
  },
  inherited: {
    background: '#2c3e7a',
    color: '#e6ebf7',
  },
  generic: {
    background: '#a05a16',
    color: '#fff5e6',
  },
  generated: {
    background: '#5d2d8e',
    color: '#f0e6f7',
  },
  deferred: {
    background: '#5a5a5a',
    color: '#e6e6e6',
  },
}

export function AffordanceBadge({
  affordance,
}: {
  affordance: PlotAffordance
}): JSX.Element {
  const merge = (variantStyle: React.CSSProperties) => ({
    ...STYLES.base,
    ...variantStyle,
  })

  switch (affordance.kind) {
    case 'registered':
      return (
        <span
          style={merge(STYLES.validated)}
          aria-label="Validated renderer"
        >
          Validated
        </span>
      )
    case 'inherited_via_ontology':
      return (
        <span
          style={merge(STYLES.inherited)}
          title={affordance.proof.rationale}
          aria-label={`Inherited from ${affordance.parent_term}`}
        >
          Inherited from {affordance.parent_term}
        </span>
      )
    case 'structural_fallback':
      return (
        <span
          style={merge(STYLES.generic)}
          title={affordance.warning}
          aria-label={`Generic plot using ${affordance.primitive}`}
        >
          Generic ({affordance.primitive})
        </span>
      )
    case 'generated_sandboxed':
      return (
        <span
          style={merge(STYLES.generated)}
          aria-label={`Generated renderer ${affordance.review_status}`}
        >
          Generated · {affordance.review_status}
        </span>
      )
    case 'deferred':
      return (
        <span style={merge(STYLES.deferred)} aria-label="No automatic plot">
          No automatic plot
        </span>
      )
  }
}
