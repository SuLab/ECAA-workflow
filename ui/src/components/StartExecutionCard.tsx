// Persistent header strip in the Jobs (Progress) tab. Renders the
// full execution control surface keyed on the polled
// `/api/chat/session/:id/execution` status, with a parallel poll of
// `/state.progress` so the start-row label correctly distinguishes
// "fresh start" from "resume after reboot/stop":
//
// no execution + 0 done → "Start execution"
// No execution + N done → "Resume execution" ← reboot survival path:
// the in-memory exec
// handle is gone but
// WORKFLOW.json on disk
// shows progress, so
// clicking spawns a
// harness that picks
// up at the next ready
// task.
// Running → Pause · Stop · Force-kill
// Pausing/paused → Cancel pause/Resume · Stop · Force-kill
// Stopping → "Stopping…" + Force-kill (escape hatch)
// Exited code=0 → "Resume execution" ← Stop preserves state:
// in-flight task was reset
// to Ready, so resume
// dispatches it again.
// Exited code≠0 → "Resume from last checkpoint"
//
// Force-kill always shows a confirmation modal — it's the only
// destructive action; everything else (start/pause/resume/stop) is
// idempotent and recoverable.

import { useEffect, useState } from 'react'
import type { ExecutionStatus, SessionStateSnapshot } from '../api/chatClient'
import {
  getChatState,
  getExecution,
  killExecution,
  pauseExecution,
  resumeExecution,
  startExecution,
  stopExecution,
} from '../api/chatClient'
import { useAsync } from '../hooks/useAsync'
import { EXECUTION_POLL_MS } from '../lib/polling'
import { relativeTime } from '../lib/time'
import { Dialog } from './primitives/Dialog'

interface Props {
  sessionId: string | null
  /** Is the session in state.kind === 'emitted'? */
  emitted: boolean
}

const STATUS_PILL: Record<
  NonNullable<ExecutionStatus['status']>,
  { label: string; bg: string; fg: string }
> = {
  running: {
    label: 'Running',
    bg: 'var(--color-success-bg)',
    fg: 'var(--color-success-fg)',
  },
  pausing: {
    label: 'Pausing…',
    bg: 'var(--color-warning-bg)',
    fg: 'var(--color-warning-fg)',
  },
  paused: {
    label: 'Paused',
    bg: 'var(--color-warning-bg)',
    fg: 'var(--color-warning-fg)',
  },
  stopping: {
    label: 'Stopping…',
    bg: 'var(--color-warning-bg)',
    fg: 'var(--color-warning-fg)',
  },
  exited: {
    label: 'Exited',
    bg: 'var(--color-info-bg)',
    fg: 'var(--color-info-fg)',
  },
}

const BTN_BASE: React.CSSProperties = {
  padding: '6px 12px',
  fontSize: '0.82rem',
  border: '1px solid var(--color-border-default)',
  borderRadius: 4,
  cursor: 'pointer',
  fontWeight: 600,
  background: 'var(--color-surface-1)',
  color: 'var(--color-text-primary)',
}
const BTN_PRIMARY: React.CSSProperties = {
  ...BTN_BASE,
  background: 'var(--color-accent)',
  color: 'var(--color-text-on-accent)',
  border: 'none',
}
const BTN_DANGER: React.CSSProperties = {
  ...BTN_BASE,
  borderColor: 'var(--color-danger-fg)',
  color: 'var(--color-danger-fg)',
}

export default function StartExecutionCard({ sessionId, emitted }: Props) {
  const [execStatus, setExecStatus] = useState<ExecutionStatus | null>(null)
  const [confirmKill, setConfirmKill] = useState(false)
  // Session progress snapshot (completed/ready/blocked/pending counts)
  // — drives the "fresh start vs resume" label switch. Polled
  // alongside /execution so the start-row label updates at the same
  // cadence as the status pill.
  const [progress, setProgress] = useState<
    SessionStateSnapshot['progress'] | null
  >(null)
  const { busy, error, run } = useAsync()

  // Poll /execution + /state.progress while emitted. Refreshes on the
  // session id so a branched session picks up its own (absent)
  // execution state and progress snapshot.
  useEffect(() => {
    if (!sessionId || !emitted) return
    let cancelled = false
    const tick = async () => {
      const [execRes, stateRes] = await Promise.allSettled([
        getExecution(sessionId),
        getChatState(sessionId),
      ])
      if (cancelled) return
      if (execRes.status === 'fulfilled') setExecStatus(execRes.value)
      if (stateRes.status === 'fulfilled') {
        setProgress(stateRes.value.progress ?? null)
      }
    }
    void tick()
    const id = window.setInterval(tick, EXECUTION_POLL_MS)
    return () => {
      cancelled = true
      window.clearInterval(id)
    }
  }, [sessionId, emitted])

  if (!emitted || !sessionId) return null

  const status = execStatus?.status ?? null
  const refresh = async () => {
    if (!sessionId) return
    try {
      const s = await getExecution(sessionId)
      setExecStatus(s)
    } catch {
      /* poll continues anyway */
    }
  }

  const onStart = async () => {
    const resp = await run(() => startExecution(sessionId, {}))
    if (resp) setExecStatus(resp)
  }
  const onPause = async () => {
    await run(() => pauseExecution(sessionId))
    await refresh()
  }
  const onResume = async () => {
    await run(() => resumeExecution(sessionId))
    await refresh()
  }
  const onStop = async () => {
    await run(() => stopExecution(sessionId))
    await refresh()
  }
  const onKill = async () => {
    setConfirmKill(false)
    await run(() => killExecution(sessionId))
    await refresh()
  }

  const showStartRow = status === null || status === 'exited'
  // Distinguish "fresh start" from "resume" using the on-disk progress
  // snapshot (survives server reboots — the in-memory exec handle
  // does not). Any completed task means clicking Start will pick up
  // from where the harness left off, NOT restart from scratch — so
  // surface that explicitly in the button label.
  const completedCount = progress?.completed ?? 0
  const totalCount =
    (progress?.completed ?? 0) +
    (progress?.ready ?? 0) +
    (progress?.blocked ?? 0) +
    (progress?.pending ?? 0)
  const hasProgress = completedCount > 0
  const restartLabel = (() => {
    if (status === 'exited' && (execStatus?.exit_code ?? 0) !== 0) {
      return 'Resume from last checkpoint'
    }
    if (status === 'exited' || hasProgress) {
      // Stop-then-start (exit_code 0) OR fresh server with prior
      // progress on disk (post-reboot path) both resume cleanly from
      // WORKFLOW.json's checkpointed state.
      return 'Resume execution'
    }
    return 'Start execution'
  })()
  const startBlurb = (() => {
    if (hasProgress) {
      return `${completedCount} of ${totalCount} tasks already complete — clicking Resume picks up at the next ready task.`
    }
    if (status === 'exited') {
      return (execStatus?.exit_code ?? 0) === 0
        ? 'The harness checkpointed cleanly. Resume picks up where it left off.'
        : 'The harness exited unexpectedly. Resume retries from the last completed task.'
    }
    return 'Kick off the workflow against the emitted package.'
  })()

  const pill = status ? STATUS_PILL[status] : null

  return (
    <div
      data-testid="start-execution-card"
      style={{
        padding: '0.75rem 1rem',
        borderBottom: '1px solid var(--color-border-default)',
        background: 'var(--color-surface-1)',
        color: 'var(--color-text-primary)',
        display: 'flex',
        flexDirection: 'column',
        gap: '0.5rem',
      }}
    >
      <div
        style={{ display: 'flex', alignItems: 'center', gap: '0.75rem', flexWrap: 'wrap' }}
      >
        <strong style={{ fontSize: '0.88rem', color: 'var(--color-text-primary)' }}>
          Execution
        </strong>
        {pill && (
          <span
            aria-label="execution status"
            data-testid="exec-status-pill"
            style={{
              display: 'inline-flex',
              alignItems: 'center',
              gap: 4,
              padding: '2px 8px',
              fontSize: '0.7rem',
              background: pill.bg,
              color: pill.fg,
              borderRadius: 999,
              fontWeight: 600,
            }}
          >
            {pill.label}
            {status === 'running' && execStatus && ` · pid ${execStatus.pid}`}
            {status === 'exited' && execStatus && ` · code ${execStatus.exit_code ?? '–'}`}
          </span>
        )}
      </div>

      {/* Action row — keyed on status */}
      <div style={{ display: 'flex', alignItems: 'center', gap: '0.5rem', flexWrap: 'wrap' }}>
        {showStartRow && (
          <>
            <span style={{ flex: 1, fontSize: '0.78rem', color: 'var(--color-text-muted)' }}>
              {startBlurb}
            </span>
            <button
              onClick={onStart}
              disabled={busy}
              aria-label={
                restartLabel === 'Start execution'
                  ? 'Start execution'
                  : 'Resume execution'
              }
              data-testid="exec-start-btn"
              style={{ ...BTN_PRIMARY, cursor: busy ? 'wait' : 'pointer' }}
            >
              {busy
                ? restartLabel === 'Start execution'
                  ? 'Starting…'
                  : 'Resuming…'
                : restartLabel}
            </button>
          </>
        )}

        {status === 'running' && (
          <>
            <button
              onClick={onPause}
              disabled={busy}
              aria-label="Pause execution"
              data-testid="exec-pause-btn"
              style={{ ...BTN_BASE, cursor: busy ? 'wait' : 'pointer' }}
            >
              Pause
            </button>
            <button
              onClick={onStop}
              disabled={busy}
              aria-label="Stop execution safely (preserves checkpoint for resume)"
              data-testid="exec-stop-btn"
              title="Safe stop: harness finishes the current task, marks any in-flight task back to Ready, then exits. Click Resume afterward to pick up where it left off."
              style={{ ...BTN_BASE, cursor: busy ? 'wait' : 'pointer' }}
            >
              Stop
            </button>
            <button
              onClick={() => setConfirmKill(true)}
              disabled={busy}
              aria-label="Force-kill execution"
              data-testid="exec-kill-btn"
              style={{ ...BTN_DANGER, cursor: busy ? 'wait' : 'pointer' }}
            >
              Force kill
            </button>
          </>
        )}

        {(status === 'paused' || status === 'pausing') && (
          <>
            {status === 'pausing' && (
              <span
                style={{ flex: 1, fontSize: '0.78rem', color: 'var(--color-text-muted)' }}
              >
                Pause requested — harness will idle at the next iteration boundary.
                Cancel with Resume.
              </span>
            )}
            <button
              onClick={onResume}
              disabled={busy}
              aria-label={status === 'pausing' ? 'Cancel pause' : 'Resume execution'}
              data-testid="exec-resume-btn"
              style={{ ...BTN_PRIMARY, cursor: busy ? 'wait' : 'pointer' }}
            >
              {status === 'pausing' ? 'Cancel pause' : 'Resume'}
            </button>
            <button
              onClick={onStop}
              disabled={busy}
              aria-label="Stop execution safely (preserves checkpoint for resume)"
              data-testid="exec-stop-btn"
              title="Safe stop: harness finishes the current task, marks any in-flight task back to Ready, then exits. Click Resume afterward to pick up where it left off."
              style={{ ...BTN_BASE, cursor: busy ? 'wait' : 'pointer' }}
            >
              Stop
            </button>
            <button
              onClick={() => setConfirmKill(true)}
              disabled={busy}
              aria-label="Force-kill execution"
              data-testid="exec-kill-btn"
              style={{ ...BTN_DANGER, cursor: busy ? 'wait' : 'pointer' }}
            >
              Force kill
            </button>
          </>
        )}

        {status === 'stopping' && (
          <>
            <span style={{ flex: 1, fontSize: '0.78rem', color: 'var(--color-text-muted)' }}>
              Harness is finishing the in-flight task and cleaning up. Restart
              afterward to pick up where it left off.
            </span>
            <button
              onClick={() => setConfirmKill(true)}
              disabled={busy}
              aria-label="Force-kill execution (escape hatch)"
              data-testid="exec-kill-btn"
              style={{ ...BTN_DANGER, cursor: busy ? 'wait' : 'pointer' }}
            >
              Force kill
            </button>
          </>
        )}
      </div>

      {error && (
        <div
          role="alert"
          style={{
            padding: '0.4rem 0.6rem',
            background: 'var(--color-danger-bg)',
            color: 'var(--color-danger-fg)',
            fontSize: '0.75rem',
            borderRadius: 4,
          }}
        >
          {error}
        </div>
      )}

      {execStatus && (
        <div
          style={{
            fontSize: '0.72rem',
            color: 'var(--color-text-muted)',
            fontFamily: 'ui-monospace, monospace',
          }}
        >
          {execStatus.agent_command} · started{' '}
          <span title={new Date(execStatus.started_at).toLocaleString()}>
            {relativeTime(execStatus.started_at)}
          </span>
          {execStatus.paused_at && (
            <>
              {' · paused '}
              <span title={new Date(execStatus.paused_at).toLocaleString()}>
                {relativeTime(execStatus.paused_at)}
              </span>
            </>
          )}
          {execStatus.stop_requested_at && (
            <>
              {' · stop requested '}
              <span title={new Date(execStatus.stop_requested_at).toLocaleString()}>
                {relativeTime(execStatus.stop_requested_at)}
              </span>
            </>
          )}
        </div>
      )}

      {confirmKill && (
        <Dialog
          onClose={() => setConfirmKill(false)}
          ariaLabel="Confirm force-kill"
          // Force-kill is destructive — disable outside-click dismiss
          // so a misclick on the backdrop doesn't accidentally cancel
          // the confirmation and leave the SME thinking "I clicked but
          // nothing happened" when they meant to confirm.
          closeOnOutsideClick={false}
          contentStyle={{
            padding: '0.6rem 0.75rem',
            border: '1px solid var(--color-danger-fg)',
            background: 'var(--color-danger-bg)',
            color: 'var(--color-danger-fg)',
            borderRadius: 4,
            fontSize: '0.78rem',
            display: 'flex',
            flexDirection: 'column',
            gap: '0.5rem',
            maxWidth: 480,
          }}
        >
          <div data-testid="exec-kill-confirm" style={{ display: 'contents' }}>
            <strong>Force-kill execution?</strong>
            <span>
              This will SIGTERM the harness and the entire agent + claude
              subtree. WORKFLOW.json will be left in whatever state the harness
              last wrote. Use Stop instead for a clean shutdown.
            </span>
            <div style={{ display: 'flex', gap: '0.5rem' }}>
              <button
                onClick={onKill}
                disabled={busy}
                data-testid="exec-kill-confirm-yes"
                style={{ ...BTN_DANGER, background: 'var(--color-danger-fg)', color: 'var(--color-text-on-accent)' }}
              >
                Yes, force kill
              </button>
              <button
                onClick={() => setConfirmKill(false)}
                disabled={busy}
                data-testid="exec-kill-confirm-no"
                style={BTN_BASE}
              >
                Cancel
              </button>
            </div>
          </div>
        </Dialog>
      )}
    </div>
  )
}
