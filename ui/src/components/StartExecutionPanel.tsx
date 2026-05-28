import { useEffect, useState } from 'react'
import type { ProgressSummary, SessionState } from '../types'
import { CardContainer } from './primitives/CardContainer'

interface Props {
  sessionId: string
  /// Current session state. The panel only renders when
  /// `state.kind === 'emitted'`; every other state collapses to null.
  sessionState: SessionState
  /// Server-state derived flag from useConversation's /execution poll.
  /// True for any of running | pausing | paused | stopping; false for
  /// exited or no handle. The panel hides while running so the SME
  /// can't double-start.
  executionRunning: boolean
  /// Aggregated task counts from the session snapshot. Used to hide
  /// the chat-side affordance once the DAG is fully executed and to
  /// label branch/partial runs as resumes.
  progress?: ProgressSummary | null
  /// Fires the actual POST /api/chat/session/:id/start-execution.
  /// Optimistically flips `executionRunning` in the parent before the
  /// next /execution poll picks up the real status.
  onStart: () => Promise<void>
}

/**
 * Inline post-emission affordance shown above the chat composer in
 * `ConversationPane`. The legacy surface required the SME to type "kick
 * off execution" into chat after the package emitted; this panel makes
 * the action a single click gated entirely on deterministic server
 * state (no LLM-mediated propose_quick_replies suggestion). Hides as
 * soon as either `sessionState.kind` leaves `emitted` or the harness
 * starts running.
 */
export default function StartExecutionPanel({
  sessionId: _sessionId,
  sessionState,
  executionRunning,
  progress,
  onStart,
}: Props): JSX.Element | null {
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!executionRunning) setPending(false)
  }, [executionRunning])

  const completed = progress?.completed ?? 0
  const total =
    (progress?.completed ?? 0) +
    (progress?.ready ?? 0) +
    (progress?.blocked ?? 0) +
    (progress?.pending ?? 0)
  const allComplete = total > 0 && completed >= total
  const hasProgress = completed > 0
  const actionLabel = hasProgress ? 'Resume Execution' : 'Start Execution'
  const ariaLabel = hasProgress ? 'Resume execution' : 'Start execution'

  if (sessionState.kind !== 'emitted' || executionRunning || allComplete) {
    return null
  }

  const handleClick = async () => {
    if (pending) return
    setPending(true)
    setError(null)
    try {
      await onStart()
      // Parent flips executionRunning → true via the optimistic update
      // in startExecutionAction. The next render will hide this panel.
    } catch (e) {
      setError(String(e))
      setPending(false)
    }
  }

  return (
    <CardContainer
      palette="neutral"
      ariaLabel="Start execution"
      style={{
        margin: '0.5rem 0.75rem',
        padding: '0.7rem 0.9rem',
        background: 'var(--color-surface-muted)',
        border: '1px solid var(--color-border-default)',
        borderLeft: '4px solid var(--color-accent)',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'space-between',
        gap: '0.75rem',
        flexWrap: 'wrap',
      }}
    >
      <div style={{ display: 'flex', flexDirection: 'column', gap: '0.2rem' }}>
        <span
          style={{
            fontSize: '0.83rem',
            fontWeight: 600,
            color: 'var(--color-text-primary)',
          }}
        >
          {hasProgress
            ? `${completed} of ${total} tasks already complete. Ready to resume execution.`
            : 'Package emitted. Ready to start execution.'}
        </span>
        {error && (
          <span
            role="alert"
            style={{
              fontSize: '0.76rem',
              color: 'var(--color-danger-fg)',
            }}
          >
            {error}
          </span>
        )}
      </div>
      <button
        type="button"
        onClick={() => {
          void handleClick()
        }}
        disabled={pending}
        aria-label={
          pending
            ? hasProgress
              ? 'Resuming execution'
              : 'Starting execution'
            : ariaLabel
        }
        data-testid="exec-start-btn-inline"
        style={{
          padding: '0.45rem 0.9rem',
          background: 'var(--color-accent)',
          color: 'var(--color-text-on-accent)',
          border: 'none',
          borderRadius: 6,
          cursor: pending ? 'not-allowed' : 'pointer',
          fontSize: '0.8rem',
          fontWeight: 600,
          opacity: pending ? 0.6 : 1,
        }}
      >
        {pending ? (hasProgress ? 'Resuming…' : 'Starting…') : actionLabel}
      </button>
    </CardContainer>
  )
}
