import { useState } from 'react'
import { useCancelableEffect } from '../hooks/useCancelableFetch'
import { getCrossVersionDiff } from '../api/chatClient'
import type { CrossVersionReport, RowClassification } from '../types'
import { Z } from '../lib/z-index'

interface Props {
  sessionId: string
  /**
   * When true, the card fetches the cross-version diff for this
   * session on mount. When false, the card is invisible — callers can
   * open/close by flipping the flag.
   */
  open: boolean
  onClose?: () => void
  /**
   * Entity names whose claims were demoted to post_hoc via the
   * claim-verifier × confirmatory-deviation lineage. Each matching
   * row renders a `demoted (post-hoc)` tag so reviewers can see at a
   * glance which discordances are explained by an SME-authorized SAP
   * deviation rather than method drift. Caller passes the set built
   * from `payload.verification.verdicts`.
   */
  demotedEntities?: ReadonlySet<string>
}

/**
 * Stable string key for a RowClassification. The base classifier has
 * 6 string variants plus `numerics_incomplete` which is an internally-
 * tagged object — we collapse it to a string key here for display
 * (the specific which_missing payload is rendered separately when
 * needed).
 */
function classificationKey(c: RowClassification): string {
  return typeof c === 'string' ? c : 'numerics_incomplete'
}

const CLASS_COLORS: Record<string, string> = {
  robust: 'var(--color-success-accent)',
  concordant: 'var(--color-warning-accent)',
  discordant: 'var(--color-danger-accent)',
  new_in_child: 'var(--color-accent)',
  dropped_in_parent: 'var(--color-text-muted)',
  entity_missing: 'var(--color-text-faint)',
  numerics_incomplete: 'var(--color-text-faint)',
}

/**
 * Cross-version diff drill-down. Renders the full `CrossVersionReport`
 * with per-table counts and a sortable per-row listing. Invoked from
 * `ResultReviewTurnCard` via an "Open diff" button.
 */
export default function CrossVersionDiffCard({
  sessionId,
  open,
  onClose,
  demotedEntities,
}: Props) {
  const [report, setReport] = useState<CrossVersionReport | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const [sortBy, setSortBy] = useState<'classification' | 'entity'>('classification')

  useCancelableEffect(async ({ cancelled }) => {
    if (!open) return
    try {
      const body = await getCrossVersionDiff(sessionId)
      if (cancelled()) return
      if (body === null) setErr('No diff yet')
      else setReport(body)
    } catch (e) {
      if (!cancelled()) setErr((e as Error).message)
    }
  }, [sessionId, open])

  if (!open) return null

  return (
    <section
      role="dialog"
      aria-label="Cross-version diff"
      style={{
        position: 'fixed',
        inset: 0,
        background: 'rgba(17, 24, 39, 0.65)',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        zIndex: Z.TOAST,
      }}
    >
      <div
        style={{
          maxWidth: '72rem',
          width: '90vw',
          maxHeight: '85vh',
          overflow: 'auto',
          background: 'var(--color-surface-1)',
          borderRadius: 8,
          padding: '1.25rem 1.5rem',
          boxShadow: '0 10px 40px rgba(0,0,0,0.3)',
        }}
      >
        <header
          style={{
            display: 'flex',
            justifyContent: 'space-between',
            alignItems: 'baseline',
            marginBottom: '0.75rem',
          }}
        >
          <h2 style={{ margin: 0, fontSize: '1.1rem' }}>
            Cross-version diff
          </h2>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            style={{
              border: 'none',
              background: 'transparent',
              cursor: 'pointer',
              fontSize: '0.9rem',
              color: 'var(--color-text-muted)',
            }}
          >
            Close
          </button>
        </header>
        {err && (
          <p role="alert" style={{ color: 'var(--color-danger-accent)', fontSize: '0.85rem' }}>
            {err}
          </p>
        )}
        {report && (
          <>
            <p style={{ margin: '0 0 0.75rem', fontSize: '0.85rem', color: 'var(--color-text-primary)' }}>
              Comparing <code style={{ fontFamily: 'ui-monospace, monospace' }}>{report.parent_package}</code>{' '}
              → <code style={{ fontFamily: 'ui-monospace, monospace' }}>{report.child_package}</code>.{' '}
              Overall concordance:{' '}
              <strong>{(report.overall_concordance * 100).toFixed(1)}%</strong>
            </p>
            {report.tables.map((t) => (
              <details
                key={t.table_name}
                open
                style={{
                  border: '1px solid #e5e7eb',
                  borderRadius: 6,
                  padding: '0.5rem 0.75rem',
                  marginBottom: '0.75rem',
                }}
              >
                <summary style={{ cursor: 'pointer', fontSize: '0.9rem', fontWeight: 600 }}>
                  {t.table_name} — robust {t.n_robust} · concordant{' '}
                  {t.n_concordant} · discordant {t.n_discordant}
                  {t.pearson_r != null && (
                    <span style={{ color: 'var(--color-text-muted)', fontWeight: 400 }}>
                      {' '}
                      (Pearson r = {t.pearson_r.toFixed(3)})
                    </span>
                  )}
                </summary>
                <div
                  style={{
                    display: 'flex',
                    gap: '0.5rem',
                    alignItems: 'center',
                    margin: '0.5rem 0',
                    fontSize: '0.78rem',
                  }}
                >
                  Sort:
                  <button
                    type="button"
                    onClick={() => setSortBy('classification')}
                    aria-pressed={sortBy === 'classification'}
                    style={{
                      padding: '2px 8px',
                      border: `1px solid ${sortBy === 'classification' ? 'var(--color-text-primary)' : 'var(--color-border-strong)'}`,
                      borderRadius: 4,
                      background: sortBy === 'classification' ? 'var(--color-text-primary)' : 'var(--color-surface-1)',
                      color: sortBy === 'classification' ? 'var(--color-surface-1)' : 'var(--color-text-primary)',
                      cursor: 'pointer',
                    }}
                  >
                    by classification
                  </button>
                  <button
                    type="button"
                    onClick={() => setSortBy('entity')}
                    aria-pressed={sortBy === 'entity'}
                    style={{
                      padding: '2px 8px',
                      border: `1px solid ${sortBy === 'entity' ? 'var(--color-text-primary)' : 'var(--color-border-strong)'}`,
                      borderRadius: 4,
                      background: sortBy === 'entity' ? 'var(--color-text-primary)' : 'var(--color-surface-1)',
                      color: sortBy === 'entity' ? 'var(--color-surface-1)' : 'var(--color-text-primary)',
                      cursor: 'pointer',
                    }}
                  >
                    by entity
                  </button>
                </div>
                <table
                  style={{
                    width: '100%',
                    borderCollapse: 'collapse',
                    fontSize: '0.78rem',
                  }}
                >
                  <thead>
                    <tr style={{ textAlign: 'left', color: 'var(--color-text-muted)' }}>
                      <th style={{ padding: '4px 6px' }}>Entity</th>
                      <th style={{ padding: '4px 6px' }}>Class</th>
                      <th style={{ padding: '4px 6px' }}>Parent Δ</th>
                      <th style={{ padding: '4px 6px' }}>Child Δ</th>
                      <th style={{ padding: '4px 6px' }}>Parent p</th>
                      <th style={{ padding: '4px 6px' }}>Parent padj</th>
                      <th style={{ padding: '4px 6px' }}>Child p</th>
                      <th style={{ padding: '4px 6px' }}>Child padj</th>
                    </tr>
                  </thead>
                  <tbody>
                    {[...t.rows]
                      .sort((a, b) => {
                        if (sortBy === 'entity') return a.entity.localeCompare(b.entity)
                        // Discordant first, then concordant, then robust, then new/dropped, then missing/incomplete.
                        const rank: Record<string, number> = {
                          discordant: 0,
                          concordant: 1,
                          robust: 2,
                          new_in_child: 3,
                          dropped_in_parent: 4,
                          entity_missing: 5,
                          numerics_incomplete: 6,
                        }
                        return (
                          (rank[classificationKey(a.classification)] ?? 99) -
                          (rank[classificationKey(b.classification)] ?? 99)
                        )
                      })
                      .slice(0, 300)
                      .map((r) => (
                        <tr
                          key={r.entity}
                          data-classification={classificationKey(r.classification)}
                          style={{ borderTop: '1px solid #f3f4f6' }}
                        >
                          <td
                            style={{
                              padding: '4px 6px',
                              fontFamily: 'ui-monospace, monospace',
                            }}
                          >
                            {r.entity}
                          </td>
                          <td style={{ padding: '4px 6px' }}>
                            <span
                              style={{
                                color: CLASS_COLORS[classificationKey(r.classification)],
                                fontWeight: 600,
                              }}
                            >
                              {classificationKey(r.classification).replace(/_/g, ' ')}
                            </span>
                            {demotedEntities?.has(r.entity) && (
                              // Inline tag marking rows whose
                              // underlying claim was demoted to
                              // post_hoc via the confirmatory-deviation
                              // lineage.
                              <span
                                aria-label="claim demoted to post-hoc"
                                data-demoted-claim-strength="post_hoc"
                                style={{
                                  marginLeft: '0.4rem',
                                  padding: '0 0.35rem',
                                  fontSize: '0.65rem',
                                  fontWeight: 600,
                                  color: 'var(--color-warning-fg)',
                                  background: 'var(--color-warning-bg)',
                                  border: '1px solid #fbbf24',
                                  borderRadius: 999,
                                }}
                              >
                                demoted (post-hoc)
                              </span>
                            )}
                          </td>
                          <td style={{ padding: '4px 6px' }}>
                            {fmt(r.parent_effect)}
                          </td>
                          <td style={{ padding: '4px 6px' }}>
                            {fmt(r.child_effect)}
                          </td>
                          <td style={{ padding: '4px 6px' }}>
                            {fmt(r.parent_pvalue_raw)}
                          </td>
                          <td style={{ padding: '4px 6px' }}>
                            {fmt(r.parent_pvalue_adjusted)}
                          </td>
                          <td style={{ padding: '4px 6px' }}>
                            {fmt(r.child_pvalue_raw)}
                          </td>
                          <td style={{ padding: '4px 6px' }}>
                            {fmt(r.child_pvalue_adjusted)}
                          </td>
                        </tr>
                      ))}
                  </tbody>
                </table>
                {t.rows.length > 300 && (
                  <p style={{ fontSize: '0.72rem', color: 'var(--color-text-muted)', marginTop: '0.5rem' }}>
                    Showing the top 300 of {t.rows.length} rows. Fetch
                    the per-table CSV for the full list.
                  </p>
                )}
              </details>
            ))}
          </>
        )}
      </div>
    </section>
  )
}

function fmt(v: number | null | undefined): string {
  if (v == null) return '—'
  if (Math.abs(v) < 0.001) return v.toExponential(2)
  return v.toFixed(3)
}
