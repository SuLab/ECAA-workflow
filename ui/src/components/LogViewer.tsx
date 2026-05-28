import { useEffect, useRef } from 'react'

interface Props {
  logs: Record<string, unknown>[]
}

function formatEntry(entry: Record<string, unknown>): string {
  const ts = typeof entry.timestamp === 'string'
    ? entry.timestamp.slice(11, 19)  // HH:MM:SS
    : ''
  const type = typeof entry.type === 'string' ? entry.type : ''
  const task = typeof entry.task_id === 'string' ? entry.task_id : ''
  const msg  = typeof entry.message === 'string' ? entry.message : ''
  const status = typeof entry.status === 'string' ? entry.status : ''

  const parts = [ts, type, task, status || msg].filter(Boolean)
  return parts.join('  ')
}

function entryColor(entry: Record<string, unknown>): string {
  // LogViewer is an always-dark "terminal" surface — picked in the
  // dark-mode plan so log text stays legible under either theme.
  // Colors render on `--color-chrome-bg-elevated`, which is dark in
  // both themes, so we use presentation-accent variants that work on
  // dark regardless of `data-theme`.
  const t = String(entry.type ?? '')
  const s = String(entry.status ?? '')
  if (t === 'waiting_for_sme') return 'var(--color-warning-accent)'
  if (s === 'completed' || t === 'task_completed') return 'var(--color-success-accent)'
  if (s === 'failed' || t === 'task_failed') return 'var(--color-danger-accent)'
  if (s === 'running' || t === 'task_started') return 'var(--color-info-accent)'
  return 'var(--color-chrome-fg-muted)'
}

export default function LogViewer({ logs }: Props) {
  const bottomRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [logs])

  return (
    <div style={{
      height: '100%', overflowY: 'auto',
      background: 'var(--color-chrome-bg-elevated)', fontFamily: 'monospace',
      padding: '0.5rem 0.75rem',
    }}>
      {logs.length === 0 && (
        <div style={{ color: 'var(--color-chrome-fg-faint)', fontSize: '0.78rem', paddingTop: '0.5rem' }}>
          Waiting for execution log…
        </div>
      )}
      {logs.map((entry, i) => (
        <div key={i} style={{
          fontSize: '0.76rem', lineHeight: 1.6,
          color: entryColor(entry),
          borderBottom: '1px solid rgba(255,255,255,0.03)',
          padding: '1px 0',
        }}>
          {formatEntry(entry)}
        </div>
      ))}
      <div ref={bottomRef} />
    </div>
  )
}
