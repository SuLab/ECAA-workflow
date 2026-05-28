/**
 * Per-task progress panel — one card per task currently in
 * TaskState::Running. Polls /active-tasks every 2s; the panel
 * re-derives the list every poll, so a task that transitions Running
 * → Completed/Blocked/Failed simply falls out of the response and the
 * card auto-removes from the UI without explicit dismount logic.
 *
 * Each card surfaces: friendly stage name, elapsed time (live ticker
 * driven by setInterval, not by polling — saves a re-fetch every 1s),
 * heartbeat freshness dot (green <60s, amber 60-300s, red >300s),
 * last non-empty line of progress.log, and a progress bar that's:
 *  - determinate (N / total) when figures-produced or
 *  expected-artifacts can be counted on disk
 *  - indeterminate (CSS shimmer) otherwise — most accurate signal
 *  for unknowable %
 *
 * Renders nothing when no tasks are running so the parent layout
 * collapses cleanly.
 */

import { useEffect, useRef, useState } from 'react'
import {
  getActiveTasks,
  type ActiveTaskSummary,
} from '../api/chatClient'
import { ACTIVE_TASKS_POLL_MS, ELAPSED_TIME_TICK_MS } from '../lib/polling'

interface Props {
  sessionId: string | null
}

export default function RunningTasksPanel({ sessionId }: Props): JSX.Element | null {
  const [tasks, setTasks] = useState<ActiveTaskSummary[]>([])
  const [error, setError] = useState<string | null>(null)
  // Live ticker — bumps every 1s so the elapsed-time readouts advance
  // without a re-fetch. Independent of the 2s poll cadence.
  const [tick, setTick] = useState(0)
  const cancelledRef = useRef(false)

  useEffect(() => {
    cancelledRef.current = false
    if (!sessionId) {
      setTasks([])
      return () => {
        cancelledRef.current = true
      }
    }
    const load = async () => {
      try {
        const list = await getActiveTasks(sessionId)
        if (!cancelledRef.current) {
          setTasks(list)
          setError(null)
        }
      } catch (e) {
        if (!cancelledRef.current) {
          setError(e instanceof Error ? e.message : String(e))
        }
      }
    }
    void load()
    const poll = setInterval(load, ACTIVE_TASKS_POLL_MS)
    return () => {
      cancelledRef.current = true
      clearInterval(poll)
    }
  }, [sessionId])

  useEffect(() => {
    const t = setInterval(() => setTick((n) => n + 1), ELAPSED_TIME_TICK_MS)
    return () => clearInterval(t)
  }, [])

  if (!sessionId) return null
  if (tasks.length === 0 && !error) return null

  return (
    <section
      role="region"
      aria-label="Active tasks"
      data-testid="running-tasks-panel"
      style={{
        padding: '0.5rem 0.75rem',
        borderBottom: '1px solid var(--color-border-default)',
        background: 'var(--color-surface-1)',
        display: 'flex',
        flexDirection: 'column',
        gap: '0.4rem',
      }}
    >
      <header
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: '0.5rem',
          fontSize: '0.74rem',
          fontWeight: 600,
          color: 'var(--color-text-secondary)',
          textTransform: 'uppercase',
          letterSpacing: '0.04em',
        }}
      >
        <span>Active tasks</span>
        <span
          aria-label={`${tasks.length} running`}
          style={{
            background: 'var(--color-success-accent)',
            color: 'var(--color-text-on-accent)',
            borderRadius: 999,
            padding: '0 0.4rem',
            fontSize: '0.65rem',
          }}
        >
          {tasks.length}
        </span>
        {error && (
          <span style={{ color: 'var(--color-danger-fg)', fontSize: '0.7rem' }}>
            poll error: {error.slice(0, 80)}
          </span>
        )}
      </header>
      {tasks.map((t) => (
        <ActiveTaskCard key={t.task_id} task={t} tickSeed={tick} />
      ))}
    </section>
  )
}

function ActiveTaskCard({
  task,
  tickSeed: _tickSeed,
}: {
  task: ActiveTaskSummary
  // The tick triggers a re-render so elapsed-time math runs again.
  // Underscore-prefixed param intentionally — we only need the
  // re-render side-effect, not the value.
  tickSeed: number
}): JSX.Element {
  // Server returned elapsed_secs at fetch time; advance it locally
  // by (now - lastFetch) so the readout doesn't visibly stall
  // between polls.
  const now = Math.floor(Date.now() / 1_000)
  const startedDt = Math.floor(new Date(task.started_at).getTime() / 1_000)
  const liveElapsed = Math.max(0, now - startedDt)
  return (
    <article
      role="status"
      aria-live="polite"
      aria-label={`${task.friendly_name} in progress`}
      data-testid="active-task-card"
      data-task-id={task.task_id}
      style={{
        background: 'var(--color-surface-0)',
        border: '1px solid var(--color-border-default)',
        borderRadius: 6,
        padding: '0.55rem 0.7rem',
        display: 'flex',
        flexDirection: 'column',
        gap: '0.35rem',
      }}
    >
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: '0.5rem',
          fontSize: '0.82rem',
        }}
      >
        <HeartbeatDot ageSecs={task.heartbeat_age_secs} />
        <strong style={{ flex: 1, color: 'var(--color-text-primary)' }}>
          {task.friendly_name}
        </strong>
        <code
          style={{
            fontFamily: 'ui-monospace, monospace',
            fontSize: '0.7rem',
            color: 'var(--color-text-muted)',
          }}
        >
          {task.task_id}
        </code>
        <ElapsedReadout secs={liveElapsed} />
      </div>
      <ProgressBar progress={task.progress} />
      {task.last_progress_line && (
        <div
          style={{
            fontFamily: 'ui-monospace, monospace',
            fontSize: '0.7rem',
            color: 'var(--color-text-muted)',
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            whiteSpace: 'nowrap',
          }}
          title={task.last_progress_line}
        >
          {task.last_progress_line}
        </div>
      )}
    </article>
  )
}

function HeartbeatDot({ ageSecs }: { ageSecs: number | null }): JSX.Element {
  // None → starting (grey). <60s → green. 60-300s → amber.
  // >300s → red (matches ECAA_TASK_HEARTBEAT_STALL_SECS default).
  const { color, label } = (() => {
    if (ageSecs === null)
      return { color: 'var(--color-text-faint)', label: 'starting' }
    if (ageSecs < 60)
      return { color: 'var(--color-success-accent)', label: `heartbeat ${ageSecs}s ago` }
    if (ageSecs < 300)
      return { color: 'var(--color-warning-accent)', label: `heartbeat ${ageSecs}s ago` }
    return {
      color: 'var(--color-danger-accent)',
      label: `heartbeat stale (${ageSecs}s ago)`,
    }
  })()
  return (
    <span
      role="img"
      aria-label={label}
      title={label}
      style={{
        width: 10,
        height: 10,
        borderRadius: 999,
        background: color,
        display: 'inline-block',
        flexShrink: 0,
      }}
    />
  )
}

function ElapsedReadout({ secs }: { secs: number }): JSX.Element {
  const m = Math.floor(secs / 60)
  const s = secs % 60
  const text = m > 0 ? `${m}m ${String(s).padStart(2, '0')}s` : `${s}s`
  return (
    <time
      aria-label={`${secs} seconds elapsed`}
      style={{
        fontFamily: 'ui-monospace, monospace',
        fontSize: '0.74rem',
        color: 'var(--color-text-secondary)',
        whiteSpace: 'nowrap',
      }}
    >
      {text}
    </time>
  )
}

function ProgressBar({
  progress,
}: {
  progress: ActiveTaskSummary['progress']
}): JSX.Element {
  if (progress.kind === 'determinate') {
    const pct = progress.total > 0 ? (progress.completed / progress.total) * 100 : 0
    return (
      <div
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={progress.total}
        aria-valuenow={progress.completed}
        aria-label={`${progress.completed} of ${progress.total} ${progress.unit}`}
        style={{
          position: 'relative',
          height: 8,
          background: 'var(--color-border-subtle)',
          borderRadius: 999,
          overflow: 'hidden',
        }}
      >
        <div
          style={{
            position: 'absolute',
            left: 0,
            top: 0,
            bottom: 0,
            width: `${pct}%`,
            background: 'var(--color-success-accent)',
            transition: 'width 200ms ease',
          }}
        />
        <div
          style={{
            position: 'absolute',
            right: 6,
            top: -2,
            fontSize: '0.65rem',
            color: 'var(--color-text-muted)',
          }}
        >
          {progress.completed}/{progress.total} {progress.unit}
        </div>
      </div>
    )
  }
  // Indeterminate — CSS shimmer.
  return (
    <div
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-label="Task in progress (indeterminate)"
      data-indeterminate="true"
      style={{
        position: 'relative',
        height: 8,
        background: 'var(--color-border-subtle)',
        borderRadius: 999,
        overflow: 'hidden',
      }}
    >
      <span
        style={{
          position: 'absolute',
          left: 0,
          top: 0,
          bottom: 0,
          width: '40%',
          background:
            'linear-gradient(90deg, transparent 0%, var(--color-accent) 50%, transparent 100%)',
          animation: 'rt-shimmer 1.4s linear infinite',
        }}
      />
      <style>{`
        @keyframes rt-shimmer {
          0% { transform: translateX(-100%); }
          100% { transform: translateX(250%); }
        }
      `}</style>
    </div>
  )
}
