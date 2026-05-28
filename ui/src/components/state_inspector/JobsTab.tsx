import { Virtuoso } from 'react-virtuoso'
import type {
  ExecutorInfo,
  HarnessProgressLine,
  HeartbeatStall,
  OrphanReap,
  StallSignalEvent,
} from '../../hooks/useSseChatEvents'
import {
  PlaceholderPane,
  backgroundForKind,
  borderForKind,
  labelForKind,
  textForKind,
} from './common'

/**
 * JobsFeed is exported so the Vitest suite can render it directly.
 */
export function JobsFeed({
  events,
  onOpenTaskLog,
  stallSignals,
  executorInfo,
  heartbeatStalls,
  orphanReap,
}: {
  events: HarnessProgressLine[]
  onOpenTaskLog?: (taskId: string) => void
  stallSignals?: Record<string, StallSignalEvent>
  /// Backend info from the harness startup event. Renders as a header
  /// row above the feed when present.
  executorInfo?: ExecutorInfo | null
  /// Per-task heartbeat stall chips.
  heartbeatStalls?: Record<string, HeartbeatStall>
  /// Most recent orphan-reap outcome; surfaced as an advisory banner
  /// below the executor header.
  orphanReap?: OrphanReap | null
}): JSX.Element {
  const header = executorInfo ? (
    <div
      data-testid="executor-header"
      aria-label="Active executor backend"
      style={{
        padding: '0.5rem 0.75rem',
        background: 'var(--color-chrome-bg)',
        color: 'var(--color-chrome-fg)',
        borderBottom: '1px solid var(--color-chrome-border)',
        fontSize: '0.72rem',
        display: 'flex',
        gap: '0.65rem',
        alignItems: 'center',
        flexWrap: 'wrap',
      }}
    >
      <span
        data-backend-header={executorInfo.name}
        style={{
          fontWeight: 700,
          textTransform: 'uppercase',
          letterSpacing: '0.05em',
          color: 'var(--color-chrome-fg-accent)',
        }}
      >
        {executorInfo.name}
      </span>
      {executorInfo.instanceType && (
        <span
          style={{
            fontFamily:
              'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
            color: 'var(--color-chrome-fg-accent)',
          }}
        >
          {executorInfo.instanceType}
        </span>
      )}
      <span style={{ color: 'var(--color-chrome-fg-faint)' }}>
        cpu budget {executorInfo.cpuBudget}
      </span>
      <span style={{ color: 'var(--color-chrome-fg-faint)' }}>
        gpu budget {executorInfo.gpuBudget}
      </span>
      <span style={{ flex: 1 }} />
      <span
        title={`harness ${executorInfo.harnessVersion}  ·  env ${executorInfo.envMode}`}
        style={{ color: 'var(--color-chrome-fg-faint)' }}
      >
        v{executorInfo.harnessVersion}
      </span>
    </div>
  ) : null

  const orphanBanner =
    orphanReap && orphanReap.unverifiedIds.length > 0 ? (
      <div
        role="alert"
        data-testid="orphan-reap-banner"
        style={{
          padding: '0.4rem 0.75rem',
          background: 'var(--color-warning-bg)',
          color: 'var(--color-warning-fg)',
          borderBottom: '1px solid var(--color-warning-border)',
          fontSize: '0.72rem',
        }}
      >
        Orphan reap: {orphanReap.verifiedCount}/{orphanReap.candidateCount}{' '}
        verified terminated. {orphanReap.unverifiedIds.length} instance
        {orphanReap.unverifiedIds.length === 1 ? '' : 's'} did not converge:{' '}
        <code>{orphanReap.unverifiedIds.join(', ')}</code>
      </div>
    ) : null

  if (events.length === 0) {
    return (
      <>
        {header}
        {orphanBanner}
        <PlaceholderPane>
          Execution jobs and progress lines from the harness appear here once
          execution begins.
        </PlaceholderPane>
      </>
    )
  }

  // C24 virtualization: the harness event feed grows unbounded for a
  // long-running session (one row per started/completed/failed/blocked
  // event, plus per-second stall signals). Render through Virtuoso so
  // only the visible window mounts; rows keyed on the stable event id
  // (e.id) so DOM nodes are reused on append, not recreated.
  const renderRow = (_idx: number, e: HarnessProgressLine) => (
    <div
      key={e.id}
      data-event-id={e.id}
      data-testid="harness-event-row"
      role={e.taskId && onOpenTaskLog ? 'button' : undefined}
      tabIndex={e.taskId && onOpenTaskLog ? 0 : undefined}
      aria-label={
        e.taskId && onOpenTaskLog
          ? `Open progress log for ${e.taskId}`
          : undefined
      }
      onClick={() => {
        if (e.taskId && onOpenTaskLog) onOpenTaskLog(e.taskId)
      }}
      onKeyDown={(ev) => {
        if (
          e.taskId &&
          onOpenTaskLog &&
          (ev.key === 'Enter' || ev.key === ' ')
        ) {
          ev.preventDefault()
          onOpenTaskLog(e.taskId)
        }
      }}
      style={{
        display: 'flex',
        gap: '0.6rem',
        padding: '0.5rem 0.7rem',
        marginBottom: '0.4rem',
        background: backgroundForKind(e.kind),
        border: `1px solid ${borderForKind(e.kind)}`,
        borderRadius: 6,
        fontSize: '0.8rem',
        cursor: e.taskId && onOpenTaskLog ? 'pointer' : 'default',
      }}
    >
      <span
        aria-hidden="true"
        style={{
          fontSize: '0.7rem',
          fontWeight: 700,
          color: textForKind(e.kind),
          textTransform: 'uppercase',
          minWidth: 64,
          flexShrink: 0,
        }}
      >
        {labelForKind(e.kind)}
      </span>
      <span style={{ color: 'var(--color-text-primary)', flex: 1 }}>
        {e.taskId && (
          <code style={{ marginRight: 6, color: 'var(--color-text-secondary)', fontSize: '0.75rem' }}>
            {e.taskId}
          </code>
        )}
        {e.detail}
        {e.taskId && onOpenTaskLog && (
          <button
            type="button"
            onClick={(ev) => {
              ev.stopPropagation()
              onOpenTaskLog(e.taskId!)
            }}
            aria-label={`View progress log for ${e.taskId}`}
            style={{
              marginLeft: 6,
              padding: '1px 6px',
              fontSize: '0.68rem',
              background: 'var(--color-chrome-bg-elevated)',
              color: 'var(--color-chrome-fg)',
              border: '1px solid var(--color-chrome-border-strong)',
              borderRadius: 3,
              cursor: 'pointer',
              fontFamily: 'ui-monospace, monospace',
              verticalAlign: 'baseline',
            }}
          >
            view log
          </button>
        )}
        {stallSignals && e.taskId && stallSignals[e.taskId] && (
          <span
            data-stall-chip={e.taskId}
            title="Stall signal observed"
            style={{
              display: 'inline-block',
              marginLeft: 8,
              padding: '0.05rem 0.4rem',
              fontSize: '0.65rem',
              fontWeight: 700,
              color: 'var(--color-warning-fg)',
              background: 'var(--color-warning-bg)',
              border: '1px solid var(--color-warning-border)',
              borderRadius: 4,
              textTransform: 'uppercase',
              letterSpacing: '0.04em',
            }}
          >
            ⚠ stall
          </span>
        )}
        {heartbeatStalls && e.taskId && heartbeatStalls[e.taskId] && (
          <span
            data-heartbeat-chip={e.taskId}
            title={`Heartbeat ${heartbeatStalls[e.taskId]!.ageSecs}s old`}
            style={{
              display: 'inline-block',
              marginLeft: 8,
              padding: '0.05rem 0.4rem',
              fontSize: '0.65rem',
              fontWeight: 700,
              color: 'var(--color-danger-fg)',
              background: 'var(--color-danger-bg)',
              border: '1px solid var(--color-danger-border)',
              borderRadius: 4,
              textTransform: 'uppercase',
              letterSpacing: '0.04em',
            }}
          >
            ⚠ heartbeat
          </span>
        )}
        {e.remote && (
          <>
            <span
              data-backend-badge={e.remote.backend}
              title={`Instance ${e.remote.instanceId}`}
              style={{
                display: 'inline-block',
                marginLeft: 8,
                padding: '0.05rem 0.4rem',
                fontSize: '0.65rem',
                fontWeight: 700,
                color: 'var(--color-accent-fg)',
                background: 'var(--color-accent)',
                borderRadius: 4,
                textTransform: 'uppercase',
                letterSpacing: '0.04em',
              }}
            >
              {e.remote.backend}
            </span>
            <span
              data-sizing-chip={e.remote.instanceType}
              style={{
                display: 'inline-block',
                marginLeft: 4,
                padding: '0.05rem 0.4rem',
                fontSize: '0.7rem',
                color: 'var(--color-info-fg)',
                background: 'var(--color-info-bg)',
                border: '1px solid var(--color-info-border)',
                borderRadius: 4,
                fontFamily:
                  'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
              }}
            >
              {e.remote.instanceType}
            </span>
          </>
        )}
      </span>
    </div>
  )

  return (
    <>
      {header}
      {orphanBanner}
      <div
        role="log"
        aria-live="polite"
        aria-label="Harness progress feed"
        style={{
          flex: 1,
          padding: '0.75rem',
          background: 'var(--color-surface-1)',
          minHeight: 0,
        }}
      >
        <Virtuoso
          data={events}
          itemContent={renderRow}
          computeItemKey={(_idx, e) => e.id}
          initialItemCount={Math.min(events.length, 50)}
          style={{ height: '100%' }}
        />
      </div>
    </>
  )
}
