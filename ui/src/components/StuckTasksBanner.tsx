/**
 * StuckTasksBanner — proactive surfacing of tasks that are alive (fresh
 * heartbeat) but not making progress. Two patterns count as stuck:
 *
 * 1. **`*.FAILED` sentinel + no transition.** A wrapper script wrote
 *  `integration_status.FAILED` (or any `*.FAILED`), but the task
 *  is still in `Running` state because the agent hasn't yet
 *  written a `state.patch.json`. Common when a sub-script crashes
 *  and the agent loop hasn't yet observed it.
 *
 * 2. **No `state.patch.json` after 2+ min of fresh heartbeats.** The
 *  agent's bash touch loop is alive but the agent itself isn't
 *  producing a transition. Indicator that the agent is stuck
 *  reading or in an LLM hop without making progress.
 *
 * Banner offers three actions per stuck task: open the task drawer
 * (to inspect logs), force re-block with a follow-up question, and
 * force-kill + redispatch (lift the running flag, restart). Initial
 * version provides "Open task drawer" only — kill/reblock land in a
 * follow-up wave when the server endpoints exist.
 */

import { useEffect, useState } from 'react'
import { getStuckTasks, type StuckTaskInfo } from '../api/chatClient'
import { STUCK_TASKS_POLL_MS } from '../lib/polling'
import { CardContainer } from './primitives/CardContainer'

interface Props {
  sessionId: string | null
  /** Optional: if provided, the task id renders as a button that
   *  triggers `onOpenTask`. Otherwise the id is shown as a plain
   *  code chip — used when the banner lives in a pane that doesn't
   *  host the TaskDetailDrawer (e.g. ConversationPane). */
  onOpenTask?: (taskId: string) => void
}

export default function StuckTasksBanner({ sessionId, onOpenTask }: Props): JSX.Element {
  const [stuck, setStuck] = useState<StuckTaskInfo[]>([])

  useEffect(() => {
    if (!sessionId) {
      setStuck([])
      return
    }
    let alive = true
    const tick = async () => {
      try {
        const res = await getStuckTasks(sessionId)
        if (alive) setStuck(res.stuck)
      } catch {
        /* non-fatal */
      }
    }
    void tick()
    const id = setInterval(() => void tick(), STUCK_TASKS_POLL_MS)
    return () => {
      alive = false
      clearInterval(id)
    }
  }, [sessionId])

  if (stuck.length === 0) return <></>

  return (
    <CardContainer
      palette="warning"
      role="alert"
      ariaLabel="Tasks alive but not making progress"
      dataAttrs={{ 'data-testid': 'stuck-tasks-banner' }}
      style={{ margin: '0.4rem 0.75rem', padding: '0.6rem 0.85rem' }}
      title={
        stuck.length === 1
          ? '1 task is alive but not making progress'
          : `${stuck.length} tasks are alive but not making progress`
      }
    >
      <ul
        style={{
          listStyle: 'none',
          padding: 0,
          margin: 0,
          display: 'flex',
          flexDirection: 'column',
          gap: '0.3rem',
        }}
      >
        {stuck.map((s) => (
          <li
            key={s.task_id}
            style={{
              fontSize: '0.74rem',
              color: 'var(--color-text-primary)',
              display: 'flex',
              gap: '0.45rem',
              alignItems: 'center',
            }}
          >
            {onOpenTask ? (
              <button
                type="button"
                onClick={() => onOpenTask(s.task_id)}
                style={{
                  padding: '2px 8px',
                  background: 'transparent',
                  border: '1px solid var(--color-warning-border)',
                  borderRadius: 3,
                  cursor: 'pointer',
                  fontSize: '0.72rem',
                  fontFamily: 'ui-monospace, monospace',
                  color: 'var(--color-warning-fg)',
                }}
              >
                {s.task_id}
              </button>
            ) : (
              <code
                style={{
                  padding: '2px 8px',
                  border: '1px solid var(--color-warning-border)',
                  borderRadius: 3,
                  fontSize: '0.72rem',
                  fontFamily: 'ui-monospace, monospace',
                  color: 'var(--color-warning-fg)',
                  background: 'var(--color-surface-1)',
                }}
              >
                {s.task_id}
              </code>
            )}
            <span style={{ flex: '1 1 auto', color: 'var(--color-warning-fg)' }}>
              {s.reason}
            </span>
            {s.failing_sentinel && (
              <code
                title={`Sentinel file written by the wrapper: ${s.failing_sentinel}`}
                style={{
                  fontFamily: 'ui-monospace, monospace',
                  fontSize: '0.65rem',
                  color: 'var(--color-danger-fg)',
                  background: 'var(--color-danger-bg)',
                  padding: '1px 6px',
                  borderRadius: 3,
                }}
              >
                {s.failing_sentinel}
              </code>
            )}
          </li>
        ))}
      </ul>
    </CardContainer>
  )
}
