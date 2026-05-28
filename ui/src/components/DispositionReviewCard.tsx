// Rendered below the chat timeline (next to BlockerCard) when the
// session has at least one pending disposition. The SME sees "the
// agent proposes N changes based on <task>" and can Apply all /
// Reject / review per-action.
//
// Mirrors the BlockerCard's structural pattern: a `section` with an
// inline picker, an `useAsync` busy/error state, and buttons that call
// the REST endpoints + fire the parent `onDone` callback on success so
// the host can refetch state.

import { useMemo, useState } from 'react'
import { useCancelableEffect } from '../hooks/useCancelableFetch'
import {
  applyDisposition,
  rejectDisposition,
  type ApplyDispositionResponseWire,
  type DispositionBodyWire,
  type DispositionListEntryWire,
} from '../api/chatClient'
import { useAsync } from '../hooks/useAsync'
import { stageIdToLabel } from '../lib/stageLabels'
import { relativeTime } from '../lib/time'
import type { DispositionAction } from '../types'

interface Props {
  sessionId: string
  /// The disposition's in-memory metadata from `GET /dispositions`.
  /// Drives the status-based render branch (pending / applied /
  /// partial / rejected).
  entry: DispositionListEntryWire
  /// Full body (when already loaded). Optional — the card fetches on
  /// mount when absent. Host pre-loads during list hydration to skip
  /// the round-trip.
  body?: DispositionBodyWire
  /// Refresh the parent state after a successful apply or reject.
  /// Typically `loadDispositions()` + `refreshSessionState()`.
  onDone?: () => void | Promise<void>
  /// Disable all controls (e.g. when another turn is in flight).
  disabled?: boolean
}

/**
 * Pretty a single action entry for the card preview. Narrows the wire
 * union via discriminant and uses `_exhaustive: never` so a new variant
 * fails `tsc --noEmit` until both this switch and the apply-loop server
 * side are updated together.
 */
function describeAction(action: DispositionAction): {
  summary: string
  target: string
  rationale?: string
} {
  switch (action.kind) {
    case 'amend_method':
      return {
        summary: `Amend ${stageIdToLabel(action.target_stage)} → ${action.new_method}`,
        target: action.target_stage,
        rationale: action.rationale,
      }
    case 'rerun':
      return {
        summary: `Re-run ${stageIdToLabel(action.target_stage)}`,
        target: action.target_stage,
        rationale: action.reason,
      }
    case 'invalidate_slice': {
      const count = action.stages_explicit.length
      return {
        summary:
          count > 0
            ? `Invalidate ${count} downstream stage${count === 1 ? '' : 's'} from ${stageIdToLabel(action.from_stage)}`
            : `Invalidate forward slice from ${stageIdToLabel(action.from_stage)}`,
        target: action.from_stage,
      }
    }
    case 'preserve_pin':
      return {
        summary: `Preserve SME pin on ${stageIdToLabel(action.target_stage)} = ${action.method}`,
        target: action.target_stage,
      }
  }
  // Exhaustiveness: tsc fails when a new DispositionAction variant
  // is added upstream and lands in ui/src/types/Action.ts via `make types`.
  const _exhaustive: never = action
  void _exhaustive
  return { summary: 'Unknown action', target: '-' }
}

const STYLES = {
  section: {
    marginTop: '0.75rem',
    padding: '0.85rem 1rem',
    background: 'var(--color-info-bg)',
    border: '1px solid #93c5fd',
    borderLeft: '4px solid #2563eb',
    borderRadius: 8,
  } as const,
  appliedSection: {
    marginTop: '0.75rem',
    padding: '0.6rem 0.85rem',
    background: 'var(--color-surface-2)',
    border: '1px solid var(--color-border-default)',
    borderRadius: 8,
    color: 'var(--color-text-faint)',
    fontSize: '0.8rem',
  } as const,
  heading: {
    margin: 0,
    marginBottom: 6,
    fontSize: '0.9rem',
    color: 'var(--color-info-fg)',
  } as const,
  interpretation: {
    margin: 0,
    marginBottom: 8,
    fontSize: '0.82rem',
    color: 'var(--color-info-fg)',
    fontStyle: 'italic' as const,
    lineHeight: 1.5,
  } as const,
  actionRow: {
    padding: '0.5rem 0.6rem',
    borderTop: '1px solid var(--color-border-default)',
    display: 'flex',
    flexDirection: 'column' as const,
    gap: '0.25rem',
  } as const,
  actionSummary: {
    fontWeight: 600,
    fontSize: '0.83rem',
    color: 'var(--color-text-primary)',
  } as const,
  actionRationale: {
    fontSize: '0.76rem',
    color: 'var(--color-text-faint)',
    fontStyle: 'italic' as const,
  } as const,
  buttonRow: {
    display: 'flex',
    gap: '0.4rem',
    marginTop: '0.75rem',
    flexWrap: 'wrap' as const,
  } as const,
  primaryButton: {
    padding: '0.45rem 0.9rem',
    background: 'var(--color-info-accent, #2563eb)',
    color: 'var(--color-text-on-accent, #fff)',
    border: 'none',
    borderRadius: 6,
    cursor: 'pointer',
    fontSize: '0.8rem',
    fontWeight: 600,
  } as const,
  secondaryButton: {
    padding: '0.45rem 0.9rem',
    background: 'transparent',
    color: 'var(--color-info-fg)',
    border: '1px solid #93c5fd',
    borderRadius: 6,
    cursor: 'pointer',
    fontSize: '0.8rem',
    fontWeight: 500,
  } as const,
  errorText: {
    marginTop: '0.5rem',
    fontSize: '0.76rem',
    color: 'var(--color-danger-fg)',
  } as const,
  errorList: {
    margin: 0,
    paddingLeft: '1.1rem',
    fontSize: '0.76rem',
    color: 'var(--color-danger-fg)',
  } as const,
}

function formatTimestamp(ts: string | undefined): string {
  if (!ts) return ''
  try {
    return relativeTime(ts)
  } catch {
    return ts
  }
}

export default function DispositionReviewCard({
  sessionId,
  entry,
  body: bodyProp,
  onDone,
  disabled,
}: Props): JSX.Element | null {
  const [body, setBody] = useState<DispositionBodyWire | null>(bodyProp ?? null)
  const [outcome, setOutcome] = useState<ApplyDispositionResponseWire | null>(null)
  const [rationale, setRationale] = useState('')
  const { busy: applying, error: applyError, run } = useAsync()

  // Hydrate the body lazily on mount when the host didn't pre-load.
  useCancelableEffect(async ({ cancelled }) => {
    if (body) return
    try {
      const full = await (
        await import('../api/chatClient')
      ).getDisposition(sessionId, entry.path)
      if (!cancelled() && full) {
        setBody(full)
      }
    } catch {
      // Non-fatal; host-level error banner catches it.
    }
  }, [sessionId, entry.path, body])

  const actions = useMemo(() => body?.actions ?? [], [body])

  const headingId = `disposition-${entry.path.replace(/\//g, '-')}`

  // Applied / partial / rejected branch — collapsed summary row.
  if (entry.status === 'applied' || entry.status === 'rejected') {
    const label =
      entry.status === 'applied'
        ? '✓ Applied'
        : '✗ Rejected'
    return (
      <section
        role="status"
        data-disposition-path={entry.path}
        data-disposition-status={entry.status}
        style={STYLES.appliedSection}
      >
        <strong>{label}</strong> disposition from{' '}
        <span title={entry.task_id}>{stageIdToLabel(entry.task_id)}</span>
        {body?.status_updated_at && (
          <>
            {' '}on {formatTimestamp(body.status_updated_at)}
          </>
        )}
        {entry.status === 'applied' && (outcome?.invalidated_tasks?.length ?? 0) > 0 && (
          <>
            {' — '}invalidated {outcome!.invalidated_tasks.length} downstream stage
            {outcome!.invalidated_tasks.length === 1 ? '' : 's'}.
          </>
        )}
      </section>
    )
  }

  // Pending / partial branch — interactive card.
  const isPartial = entry.status === 'partial'
  return (
    <section
      role="region"
      aria-labelledby={headingId}
      data-disposition-path={entry.path}
      data-disposition-status={entry.status}
      style={STYLES.section}
    >
      <h3 id={headingId} style={STYLES.heading}>
        {isPartial
          ? `Retry ${entry.action_count} change${entry.action_count === 1 ? '' : 's'} from ${stageIdToLabel(entry.task_id)}`
          : `The agent proposes ${entry.action_count} change${entry.action_count === 1 ? '' : 's'} from ${stageIdToLabel(entry.task_id)}`}
      </h3>

      {(body?.authoritative_interpretation || entry.authoritative_interpretation) && (
        <p style={STYLES.interpretation}>
          {body?.authoritative_interpretation ?? entry.authoritative_interpretation}
        </p>
      )}

      {actions.length === 0 ? (
        <p style={STYLES.interpretation}>
          This disposition has no applicable actions. It may have been
          written by an older agent format — you can dismiss it with
          Reject.
        </p>
      ) : (
        actions.map((a, idx) => {
          // Narrow at the boundary: `actions` comes from
          // `DispositionBodyWire` (hand-maintained `[key: string]:
          // unknown` + `kind: string`). The exhaustive switch in
          // describeAction operates on the typed `DispositionAction`.
          const d = describeAction(a as unknown as DispositionAction)
          return (
            <div
              key={idx}
              data-action-index={idx}
              data-action-kind={a.kind}
              style={STYLES.actionRow}
            >
              <div style={STYLES.actionSummary}>
                {idx + 1}. {d.summary}
              </div>
              {d.rationale && (
                <div style={STYLES.actionRationale}>
                  Rationale: {d.rationale}
                </div>
              )}
            </div>
          )
        })
      )}

      <label
        style={{
          display: 'block',
          marginTop: '0.75rem',
          fontSize: '0.74rem',
          color: 'var(--color-info-fg)',
          fontWeight: 500,
        }}
      >
        Reason (optional, saved to the audit log)
        <textarea
          data-testid="disposition-rationale"
          value={rationale}
          onChange={(e) => setRationale(e.target.value)}
          rows={2}
          placeholder="A short note on why you accept or reject this plan."
          style={{
            width: '100%',
            marginTop: 4,
            padding: '0.4rem 0.5rem',
            borderRadius: 4,
            border: '1px solid #93c5fd',
            fontSize: '0.78rem',
            fontFamily: 'inherit',
            background: 'var(--color-info-bg)',
            resize: 'vertical',
            boxSizing: 'border-box',
          }}
        />
      </label>

      {(applyError || (outcome?.errors?.length ?? 0) > 0) && (
        <div style={STYLES.errorText}>
          {applyError && <p style={STYLES.errorText}>{applyError}</p>}
          {outcome && outcome.errors.length > 0 && (
            <ul style={STYLES.errorList}>
              {outcome.errors.map((e) => (
                <li key={`${e.action_index}-${e.action_kind}`}>
                  Action {e.action_index + 1} ({e.action_kind} on{' '}
                  {stageIdToLabel(e.target_stage)}): {e.reason}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      <div style={STYLES.buttonRow}>
        <button
          type="button"
          data-testid="disposition-apply-all"
          disabled={disabled || applying || actions.length === 0}
          style={{
            ...STYLES.primaryButton,
            opacity: disabled || applying || actions.length === 0 ? 0.6 : 1,
          }}
          onClick={async () => {
            const result = await run(() =>
              applyDisposition(sessionId, entry.path, {
                rationale: rationale.trim() || undefined,
              }),
            )
            if (result) {
              setOutcome(result)
              if (result.status === 'applied' || result.status === 'partial') {
                await onDone?.()
              }
            }
          }}
        >
          {applying
            ? 'Applying…'
            : isPartial
              ? `Retry ${actions.length} change${actions.length === 1 ? '' : 's'}`
              : `Apply all ${actions.length} change${actions.length === 1 ? '' : 's'}`}
        </button>
        <button
          type="button"
          data-testid="disposition-reject"
          disabled={disabled || applying}
          style={{
            ...STYLES.secondaryButton,
            opacity: disabled || applying ? 0.6 : 1,
          }}
          onClick={async () => {
            await run(() =>
              rejectDisposition(
                sessionId,
                entry.path,
                rationale.trim() || undefined,
              ),
            )
            await onDone?.()
          }}
        >
          Reject
        </button>
      </div>
    </section>
  )
}
