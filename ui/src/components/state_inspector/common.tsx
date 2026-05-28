import type { ReactNode } from 'react'

/**
 * Shared "empty-state" pane reused across every state-inspector tab.
 * Kept here so per-tab files render a placeholder without depending on
 * the parent component.
 */
export function PlaceholderPane({ children }: { children: ReactNode }): JSX.Element {
  return (
    <div
      style={{
        flex: 1,
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        color: 'var(--color-text-faint)',
        fontSize: '0.85rem',
        textAlign: 'center',
        padding: '1.5rem',
        maxWidth: 380,
        margin: '0 auto',
        lineHeight: 1.55,
      }}
    >
      {children}
    </div>
  )
}

/**
 * Color helpers for the Jobs tab's harness progress rows. Keyed on the
 * string event kind the harness posts (task_started / task_completed /
 * ...). Values are CSS var() strings so the same helpers work in both
 * themes.
 */
export function backgroundForKind(kind: string): string {
  switch (kind) {
    case 'task_started':
      return 'var(--color-info-bg)'
    case 'task_completed':
    case 'execution_finished':
      return 'var(--color-success-bg)'
    case 'task_failed':
      return 'var(--color-danger-bg)'
    case 'task_blocked':
      return 'var(--color-warning-bg)'
    default:
      return 'var(--color-surface-0)'
  }
}

export function borderForKind(kind: string): string {
  switch (kind) {
    case 'task_started':
      return 'var(--color-info-border)'
    case 'task_completed':
    case 'execution_finished':
      return 'var(--color-success-border)'
    case 'task_failed':
      return 'var(--color-danger-border)'
    case 'task_blocked':
      return 'var(--color-warning-border)'
    default:
      return 'var(--color-border-default)'
  }
}

export function textForKind(kind: string): string {
  switch (kind) {
    case 'task_started':
      return 'var(--color-info-fg)'
    case 'task_completed':
    case 'execution_finished':
      return 'var(--color-success-fg)'
    case 'task_failed':
      return 'var(--color-danger-fg)'
    case 'task_blocked':
      return 'var(--color-warning-fg)'
    default:
      return 'var(--color-text-secondary)'
  }
}

export function labelForKind(kind: string): string {
  switch (kind) {
    case 'task_started':
      return 'Started'
    case 'task_completed':
      return 'Done'
    case 'task_failed':
      return 'Failed'
    case 'task_blocked':
      return 'Blocked'
    case 'execution_finished':
      return 'Finished'
    default:
      return kind
  }
}

export { METRICS_POLL_MS } from '../../lib/polling'
