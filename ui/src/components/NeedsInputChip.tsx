/**
 * "Needs your input" persistent chip in the app header.
 *
 * Renders when the session has ≥1 blocked task so the SME knows a
 * decision is outstanding regardless of which tab they're currently
 * on. Clicking the chip deep-links to the first blocked task via the
 * `#task=<id>` URL hash, which the Plan tab's PlanTab component reads
 * on mount / hash-change to open the TaskDetailDrawer on that node.
 *
 * Hidden entirely when `blockedTasks` is empty so the normal header
 * stays uncluttered on healthy sessions.
 */

interface Props {
  blockedTasks: string[]
  /** Called with the first blocked task id when the chip is clicked. */
  onJump: (taskId: string) => void
}

export default function NeedsInputChip({
  blockedTasks,
  onJump,
}: Props): JSX.Element | null {
  if (blockedTasks.length === 0) return null
  const count = blockedTasks.length
  const label =
    count === 1
      ? 'Needs your input (1 step)'
      : `Needs your input (${count} steps)`
  return (
    <button
      type="button"
      data-testid="needs-input-chip"
      aria-live="polite"
      aria-label={`${count} step${count === 1 ? '' : 's'} awaiting your decision. Click to jump to the first blocked step.`}
      onClick={() => onJump(blockedTasks[0]!)}
      style={{
        display: 'inline-flex',
        alignItems: 'center',
        gap: '0.35rem',
        padding: '0.35rem 0.75rem',
        marginLeft: '0.5rem',
        background: 'var(--color-warning-bg)',
        color: 'var(--color-warning-fg)',
        border: '1px solid var(--color-warning-border)',
        borderRadius: 999,
        cursor: 'pointer',
        fontSize: '0.76rem',
        fontWeight: 600,
      }}
    >
      <span aria-hidden>⚠</span>
      <span>{label}</span>
    </button>
  )
}
