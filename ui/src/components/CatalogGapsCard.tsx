// Surfaces affordance_fallbacks from the /metrics payload in the Performance
// tab. Each row is a (semantic_type, primitive, count) triple — how many
// times the affordance resolver fell back to a generic structural primitive
// across this session. The "Describe a plot" CTA per row opens the
// RendererProposalCard so the SME can file a renderer proposal inline.
//
// Returns null when fallbacks is empty so the metrics tab stays uncluttered
// for sessions where all renderers are registered.

import type { AffordanceFallbackSummary } from '../api/chatClient'

interface Props {
  fallbacks: AffordanceFallbackSummary[]
  /**
   * Called when the SME clicks "Describe a plot" for a gap row.
   * The handler is responsible for opening the RendererProposalCard;
   * the CatalogGapsCard itself doesn't mount it — it stays stateless so
   * the metrics tab can decide how to surface the modal.
   */
  onSuggestRenderer(semanticType: string, primitive: string): void
}

export function CatalogGapsCard({ fallbacks, onSuggestRenderer }: Props): JSX.Element | null {
  if (fallbacks.length === 0) return null

  return (
    <section
      aria-label="Catalog gaps"
      data-metric-block="catalog-gaps"
      style={{
        marginTop: '1rem',
        padding: '0.75rem',
        background: 'var(--color-surface-0)',
        border: '1px solid var(--color-border-default)',
        borderRadius: '0.25rem',
      }}
    >
      <div
        style={{
          fontSize: '0.83rem',
          fontWeight: 600,
          color: 'var(--color-text-primary)',
          marginBottom: '0.35rem',
        }}
      >
        Catalog gaps
      </div>
      <p
        style={{
          margin: '0 0 0.55rem',
          fontSize: '0.78rem',
          color: 'var(--color-text-secondary)',
        }}
      >
        Type-specific renderers pending for these results:
      </p>
      <table
        aria-label="Catalog gaps by result type"
        style={{ width: '100%', borderCollapse: 'collapse', fontSize: '0.78rem' }}
      >
        <thead>
          <tr style={{ borderBottom: '1px solid var(--color-border-strong)' }}>
            <th
              style={{
                textAlign: 'left',
                padding: '0.3rem 0.5rem 0.3rem 0',
                fontWeight: 600,
                color: 'var(--color-text-secondary)',
              }}
            >
              Result type
            </th>
            <th
              style={{
                textAlign: 'left',
                padding: '0.3rem 0.5rem',
                fontWeight: 600,
                color: 'var(--color-text-secondary)',
              }}
            >
              Generic plot used
            </th>
            <th
              style={{
                textAlign: 'right',
                padding: '0.3rem 0.5rem',
                fontWeight: 600,
                color: 'var(--color-text-secondary)',
              }}
            >
              Uses
            </th>
            <th style={{ padding: '0.3rem 0' }} />
          </tr>
        </thead>
        <tbody>
          {fallbacks.map((row) => (
            <tr
              key={`${row.semantic_type}::${row.primitive}`}
              data-gap-row="true"
              data-semantic-type={row.semantic_type}
              style={{ borderBottom: '1px solid var(--color-border-subtle)' }}
            >
              <td
                style={{
                  padding: '0.3rem 0.5rem 0.3rem 0',
                  color: 'var(--color-text-primary)',
                }}
              >
                <code
                  style={{
                    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
                    fontSize: '0.72rem',
                    background: 'var(--color-surface-2)',
                    padding: '1px 4px',
                    borderRadius: 3,
                  }}
                >
                  {row.semantic_type}
                </code>
              </td>
              <td
                style={{
                  padding: '0.3rem 0.5rem',
                  color: 'var(--color-text-secondary)',
                  fontFamily: 'ui-monospace, monospace',
                  fontSize: '0.75rem',
                }}
              >
                {row.primitive}
              </td>
              <td
                style={{
                  padding: '0.3rem 0.5rem',
                  textAlign: 'right',
                  color: 'var(--color-text-secondary)',
                  fontFamily: 'ui-monospace, monospace',
                }}
              >
                {row.count}
              </td>
              <td style={{ padding: '0.3rem 0', textAlign: 'right' }}>
                <button
                  type="button"
                  onClick={() => onSuggestRenderer(row.semantic_type, row.primitive)}
                  style={{
                    padding: '0.2rem 0.55rem',
                    fontSize: '0.72rem',
                    fontWeight: 600,
                    background: 'transparent',
                    color: 'var(--color-info-fg, #1d4ed8)',
                    border: '1px solid var(--color-info-border, #bfdbfe)',
                    borderRadius: 4,
                    cursor: 'pointer',
                    whiteSpace: 'nowrap',
                  }}
                >
                  Describe a plot
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  )
}
