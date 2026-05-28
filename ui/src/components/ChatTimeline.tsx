import React, { useMemo, useRef } from 'react'
import { Virtuoso, type VirtuosoHandle } from 'react-virtuoso'
import type { CheckpointMode, ProjectClass, SessionMode, Turn } from '../types'
import AssistantTurnCard from './AssistantTurnCard'
import UserTurnCard from './UserTurnCard'

interface Props {
  turns: Turn[]
  pillStatusLine: string | null
  /// Buffered text from streaming `assistant_token_delta` SSE events.
  /// When non-empty, render an in-flight assistant turn at the bottom
  /// of the log so tokens arrive live instead of waiting for the full
  /// turn.
  streamingText?: string
  onConfirm: (opts?: { mode?: SessionMode; checkpointMode?: CheckpointMode }) => void | Promise<void>
  onReject: () => void | Promise<void>
  onQuickReply: (option: string) => void | Promise<void>
  /** Threaded to the ConfirmationTurnCard so it can force explicit
   *  mode selection for ClinicalTrial sessions. */
  projectClass?: ProjectClass
  /** When set, threads through to AssistantTurnCard so `<lit-entity>`
   *  spans in assistant messages render as interactive pills. */
  sessionId?: string | null
  /** When provided, threaded to the latest assistant turn so it can
   *  render an inline "Branch from here" affordance. Suppressed when
   *  undefined (the default — pre-emission sessions have nothing
   *  meaningful to branch from). */
  onBranch?: (rationale?: string) => void | Promise<void>
}

// Sentinel turn_id we splice onto the end of the rendered list when an
// in-flight streamed assistant bubble is showing. Picked from a
// reserved-ish keyspace so it can never collide with a real turn_id
// (which are UUID-shaped).
const STREAMING_ROW_KEY = '__streaming__'

function ChatTimeline({
  turns,
  pillStatusLine,
  streamingText,
  onConfirm,
  onReject,
  onQuickReply,
  projectClass,
  sessionId,
  onBranch,
}: Props) {
  const virtuosoRef = useRef<VirtuosoHandle>(null)

  // The assistant turn that should host the live status pill is the most
  // recent assistant turn — the LLM is the actor for any in-flight tool call.
  const lastAssistantIdx = useMemo(() => {
    for (let i = turns.length - 1; i >= 0; i -= 1) {
      if (turns![i]!.role === 'assistant') return i
    }
    return -1
  }, [turns])

  // The latest assistant turn carrying `confirmation_card`. Older cards
  // are historical — they should render their summary text but their
  // Accept/Revise buttons must NOT be actionable, otherwise multi-turn
  // intake flows render duplicate Accept buttons (the methylation
  // Multi-turn case from the audit).
  const lastConfirmationCardIdx = useMemo(() => {
    for (let i = turns.length - 1; i >= 0; i -= 1) {
      if (turns![i]!.role === 'assistant' && turns![i]!.confirmation_card) return i
    }
    return -1
  }, [turns])

  // Memoize the filtered list + precomputed real-index map so the
  // visible array identity only changes when turns do, and the per-turn
  // index lookup is O(1) instead of O(N) via indexOf.
  const { visible, indexMap } = useMemo(() => {
    const visible: Turn[] = []
    const indexMap = new Map<string, number>()
    for (let i = 0; i < turns.length; i += 1) {
      const t = turns[i]
      if (t!.role !== 'system') {
        visible.push(t!)
        indexMap.set(t!.turn_id, i)
      }
    }
    return { visible, indexMap }
  }, [turns])

  // Suppress the in-flight bubble when the most recent turn is already an
  // assistant turn whose content matches the buffer — covers the moment
  // between the final delta and the resetStreamingText() call.
  const showStreaming =
    !!streamingText &&
    streamingText.length > 0 &&
    !(visible.length > 0 &&
      visible![visible.length - 1]!.role === 'assistant' &&
      visible![visible.length - 1]!.content.startsWith(streamingText))

  // Empty-state takes the simple non-virtualized rendering path. Avoids
  // creating a 0-row Virtuoso instance just to layer a centered placeholder.
  if (visible.length === 0 && !showStreaming) {
    return (
      <div
        role="log"
        aria-live="polite"
        aria-relevant="additions text"
        style={{
          flex: 1,
          overflowY: 'auto',
          padding: '0.75rem',
          display: 'flex',
          flexDirection: 'column',
          gap: '0.6rem',
          background: 'var(--color-surface-1)',
        }}
      >
        <div
          style={{
            color: 'var(--color-text-faint)',
            fontSize: '0.83rem',
            margin: 'auto',
            textAlign: 'center',
            maxWidth: 320,
          }}
        >
          Tell me about the analysis you're planning. I'll work through it with you.
        </div>
      </div>
    )
  }

  // Render index = visible.length when streaming, else visible.length - 1.
  // We compose a stable lookup table that maps the renderer's `index` to
  // either a real Turn or the in-flight streaming bubble sentinel.
  const itemKeys: string[] = []
  for (const t of visible) itemKeys.push(t.turn_id)
  if (showStreaming) itemKeys.push(STREAMING_ROW_KEY)

  return (
    <div
      role="log"
      aria-live="polite"
      aria-relevant="additions text"
      style={{
        flex: 1,
        minHeight: 0,
        display: 'flex',
        flexDirection: 'column',
        background: 'var(--color-surface-1)',
      }}
    >
      <Virtuoso
        ref={virtuosoRef}
        data={itemKeys}
        // `followOutput` strategy: force-scroll on every append regardless
        // of where the user currently is. The string form `"smooth"` only
        // fires when the user is near the bottom, so a new SSE-delivered
        // assistant turn could land off-screen. The function form
        // disables Virtuoso's "stick to scroll position" heuristic and
        // always animates to the new tail. The user can scroll up freely;
        // the next append re-snaps to bottom.
        followOutput={() => 'smooth'}
        // Anchor on turn_id (or the streaming sentinel) so reorders /
        // mid-list mutations from the server reuse the right row.
        computeItemKey={(_index, key) => key}
        increaseViewportBy={{ top: 400, bottom: 600 }}
        style={{ flex: 1, padding: '0.75rem' }}
        itemContent={(_index, key) => {
          if (key === STREAMING_ROW_KEY) {
            return (
              <div style={{ paddingBottom: '0.6rem' }}>
                <InFlightAssistantBubble text={streamingText!} pillStatusLine={pillStatusLine} />
              </div>
            )
          }
          const realIndex = indexMap.get(key) ?? -1
          const turn = realIndex >= 0 ? turns[realIndex] : undefined
          // Virtuoso requires a non-null return; a 1px spacer covers
          // the unreachable "row keyed but not in turns[]" branch.
          if (!turn) return <div style={{ height: 1 }} />
          if (turn.role === 'user') {
            return (
              <div style={{ paddingBottom: '0.6rem' }}>
                <UserTurnCard turn={turn} />
              </div>
            )
          }
          return (
            <div style={{ paddingBottom: '0.6rem' }}>
              <AssistantTurnCard
                turn={turn}
                isLatest={realIndex === lastAssistantIdx && !showStreaming}
                isLatestConfirmationCard={realIndex === lastConfirmationCardIdx}
                pillStatusLine={
                  realIndex === lastAssistantIdx && !showStreaming
                    ? pillStatusLine
                    : null
                }
                onConfirm={onConfirm}
                onReject={onReject}
                onQuickReply={onQuickReply}
                projectClass={projectClass}
                sessionId={sessionId ?? undefined}
                onBranch={
                  realIndex === lastAssistantIdx && !showStreaming
                    ? onBranch
                    : undefined
                }
              />
            </div>
          )
        }}
      />
    </div>
  )
}

export default React.memo(ChatTimeline)

/// Renders the in-progress streaming text as an assistant-style bubble
/// with a trailing caret so readers see more text is coming. When the
/// final non-streaming Turn arrives, resetStreamingText() from the
/// parent removes this bubble in favour of the canonical
/// AssistantTurnCard.
function InFlightAssistantBubble({
  text,
  pillStatusLine,
}: {
  text: string
  pillStatusLine: string | null
}) {
  return (
    <article
      aria-label="Assistant message (streaming)"
      style={{
        alignSelf: 'flex-start',
        maxWidth: '92%',
        padding: '0.7rem 0.9rem',
        background: 'var(--color-surface-2)',
        color: 'var(--color-text-primary)',
        borderRadius: 12,
        borderTopLeftRadius: 4,
        fontSize: '0.85rem',
        lineHeight: 1.55,
      }}
    >
      <div style={{ whiteSpace: 'pre-wrap' }}>
        {text}
        <span
          aria-hidden="true"
          style={{
            display: 'inline-block',
            width: 8,
            height: '1.1em',
            verticalAlign: 'text-bottom',
            background: 'var(--color-text-primary)',
            marginLeft: 2,
            animation: 'scrippsPulse 1s ease-in-out infinite',
          }}
        />
      </div>
      {pillStatusLine && (
        <div
          role="status"
          aria-live="polite"
          style={{
            marginTop: '0.5rem',
            display: 'inline-flex',
            alignItems: 'center',
            gap: '0.5rem',
            padding: '0.3rem 0.6rem',
            background: 'var(--color-info-bg)',
            border: '1px solid var(--color-info-border)',
            borderRadius: 999,
            fontSize: '0.78rem',
            color: 'var(--color-info-fg)',
          }}
        >
          <span
            aria-hidden="true"
            style={{
              width: 10,
              height: 10,
              borderRadius: '50%',
              background: 'var(--color-info-accent)',
              animation: 'scrippsPulse 1.2s ease-in-out infinite',
              flexShrink: 0,
            }}
          />
          <span>{pillStatusLine}</span>
        </div>
      )}
    </article>
  )
}
