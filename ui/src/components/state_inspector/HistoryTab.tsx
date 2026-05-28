import { PlaceholderPane } from './common'

/**
 * Shows the cross-version diff parent → child hop with per-table
 * counts when the session has a diff; falls back to a placeholder
 * for root sessions.
 */
export function HistoryPane({
  crossVersionReport,
}: {
  crossVersionReport: unknown
}): JSX.Element {
  if (!crossVersionReport || typeof crossVersionReport !== 'object') {
    return (
      <PlaceholderPane>
        Past conversations and emitted packages will be listed here.
      </PlaceholderPane>
    )
  }
  const report = crossVersionReport as {
    parent_package?: string
    child_package?: string
    overall_concordance?: number
    tables?: Array<{
      table_name: string
      n_robust: number
      n_concordant: number
      n_discordant: number
    }>
  }
  const tables = report.tables ?? []
  return (
    <div
      aria-label="Cross-version history"
      style={{
        flex: 1,
        overflowY: 'auto',
        padding: '1rem',
        background: 'var(--color-surface-1)',
        color: 'var(--color-text-primary)',
      }}
    >
      <h3 style={{ margin: '0 0 0.5rem', fontSize: '0.95rem' }}>
        Cross-version diff
      </h3>
      <p style={{ margin: '0 0 0.75rem', fontSize: '0.8rem', color: 'var(--color-text-secondary)' }}>
        Overall concordance:{' '}
        <strong>
          {report.overall_concordance != null
            ? `${(report.overall_concordance * 100).toFixed(1)}%`
            : '—'}
        </strong>
      </p>
      <code style={{ fontSize: '0.74rem', color: 'var(--color-text-muted)' }}>
        {report.parent_package} → {report.child_package}
      </code>
      <ul
        style={{
          listStyle: 'none',
          padding: 0,
          margin: '0.75rem 0 0',
          display: 'flex',
          flexDirection: 'column',
          gap: '0.4rem',
        }}
      >
        {tables.map((t) => (
          <li
            key={t.table_name}
            data-history-table={t.table_name}
            style={{
              display: 'flex',
              justifyContent: 'space-between',
              padding: '0.4rem 0.55rem',
              border: '1px solid var(--color-border-default)',
              borderRadius: 4,
              fontSize: '0.78rem',
            }}
          >
            <span style={{ fontFamily: 'ui-monospace, monospace', color: 'var(--color-text-primary)' }}>
              {t.table_name}
            </span>
            <span>
              <span style={{ color: 'var(--color-success-accent)' }}>{t.n_robust} robust</span>
              {' · '}
              <span style={{ color: 'var(--color-warning-accent)' }}>{t.n_concordant} concordant</span>
              {' · '}
              <span style={{ color: 'var(--color-danger-accent)' }}>{t.n_discordant} discordant</span>
            </span>
          </li>
        ))}
      </ul>
    </div>
  )
}
