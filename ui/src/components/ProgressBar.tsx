import type { DAG } from '../types'

interface Props {
  dag: DAG
}

export default function ProgressBar({ dag }: Props) {
  const tasks = Object.values(dag.tasks).filter(Boolean)
  const total = tasks.length
  const completed = tasks.filter(t => t!.state.status === 'completed').length
  const failed    = tasks.filter(t => t!.state.status === 'failed').length
  const running   = tasks.filter(t => t!.state.status === 'running').length
  const pct = total > 0 ? Math.round((completed / total) * 100) : 0
  const failPct = total > 0 ? Math.round((failed / total) * 100) : 0

  return (
    <div style={{
      padding: '0.35rem 0.75rem', background: 'var(--color-surface-0)',
      borderBottom: '1px solid var(--color-border-default)', flexShrink: 0,
    }}>
      <div style={{
        display: 'flex', justifyContent: 'space-between',
        fontSize: '0.72rem', color: 'var(--color-text-muted)', marginBottom: 3,
      }}>
        <span>
          {completed}/{total} completed
          {running > 0 && (
            <span style={{ color: 'var(--color-warning-accent)' }}> · {running} running</span>
          )}
          {failed > 0 && (
            <span style={{ color: 'var(--color-danger-accent)' }}> · {failed} failed</span>
          )}
        </span>
        <span style={{ fontWeight: 600, color: 'var(--color-text-secondary)' }}>{pct}%</span>
      </div>
      <div style={{
        height: 4, background: 'var(--color-surface-3)',
        borderRadius: 2, display: 'flex', overflow: 'hidden',
      }}>
        <div
          style={{
            width: `${pct}%`,
            background: 'var(--color-success-accent)',
            transition: 'width 0.3s',
          }}
        />
        {failPct > 0 && (
          <div style={{ width: `${failPct}%`, background: 'var(--color-danger-accent)' }} />
        )}
      </div>
    </div>
  )
}
