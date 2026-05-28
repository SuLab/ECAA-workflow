import React, { memo, useEffect, useRef, useState } from 'react'
import type { Turn, ProjectClass, SessionMode, CheckpointMode } from '../types'
import type { LiteratureContext } from '../types/LiteratureContext'
import type { EntityKind } from '../types/EntityKind'
import { FetchError, jsonFetch } from '../api/_fetch'
import { LiteratureContextCard } from './LiteratureContextCard'
import BranchFromHereCard from './BranchFromHereCard'
import ConfirmationTurnCard from './ConfirmationTurnCard'
import QuickReplyRow from './QuickReplyRow'
import ToolCallStatusPill from './ToolCallStatusPill'
import { Z } from '../lib/z-index'

// ── LitEntityButton ──────────────────────────────────────────────────────────
// Renders an inline pill for a `<lit-entity name="X" kind="Y" />` tag found
// in the assistant's markdown. On click, fetches literature context and opens
// a floating LiteratureContextCard popover.

interface LitEntityButtonProps {
  name: string
  kind: EntityKind
  sessionId: string
}

export const LitEntityButton: React.FC<LitEntityButtonProps> = ({
  name,
  kind,
  sessionId,
}) => {
  const [open, setOpen] = useState(false)
  const [ctx, setCtx] = useState<LiteratureContext | null>(null)
  const [error, setError] = useState<string | null>(null)
  const wrapperRef = useRef<HTMLSpanElement | null>(null)
  const popoverRef = useRef<HTMLDivElement | null>(null)

  // The popover is an inline anchored panel, not a full-screen modal.
  // Using the shared <Dialog> primitive would force a centered backdrop
  // and tear the lit-entity citation out of its inline context, so we
  // instead wire the two a11y affordances <Dialog> provides that this
  // popover lacked: Escape-to-dismiss and outside-click-to-dismiss.
  // role="dialog" is retained (vs the more semantically-accurate
  // role="region") so existing `getByRole('dialog')` queries in
  // AssistantTurnCard.litEntity.test.tsx keep matching.
  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault()
        setOpen(false)
      }
    }
    const onDocMouseDown = (e: MouseEvent) => {
      if (wrapperRef.current && !wrapperRef.current.contains(e.target as Node)) {
        setOpen(false)
      }
    }
    window.addEventListener('keydown', onKey)
    document.addEventListener('mousedown', onDocMouseDown)
    return () => {
      window.removeEventListener('keydown', onKey)
      document.removeEventListener('mousedown', onDocMouseDown)
    }
  }, [open])

  const handleClick = async () => {
    if (open) {
      setOpen(false)
      return
    }
    setOpen(true)
    setError(null)
    if (ctx) return // already fetched
    try {
      const url = `/api/chat/session/${encodeURIComponent(sessionId)}/literature-context?entity=${encodeURIComponent(name)}&entity_kind=${encodeURIComponent(kind)}`
      const data = await jsonFetch<LiteratureContext>(url)
      setCtx(data)
    } catch (e) {
      if (e instanceof FetchError) {
        if (e.status === 404) {
          setError('No literature atoms ran in this session.')
        } else if (e.status === 409) {
          setError('Package not yet emitted.')
        } else {
          setError(`Server error: ${e.status}`)
        }
      } else {
        setError(
          `Network error: ${e instanceof Error ? e.message : String(e)}`,
        )
      }
    }
  }

  return (
    <span ref={wrapperRef} style={{ position: 'relative', display: 'inline-block' }}>
      <button
        type="button"
        onClick={handleClick}
        aria-label={`Show literature context for ${name}`}
        style={{
          border: '1px dashed #888',
          padding: '0 4px',
          background: 'none',
          cursor: 'pointer',
          font: 'inherit',
          color: 'inherit',
          borderRadius: 2,
        }}
      >
        {name}
      </button>
      {open && (
        <div
          ref={popoverRef}
          role="dialog"
          aria-label={`Literature context for ${name}`}
          style={{
            position: 'absolute',
            top: '100%',
            left: 0,
            zIndex: Z.TOP,
            marginTop: 4,
            boxShadow: '0 4px 12px rgba(0,0,0,0.15)',
            background: 'white',
          }}
        >
          <div style={{ position: 'absolute', top: 4, right: 4 }}>
            <button
              type="button"
              onClick={() => setOpen(false)}
              aria-label="Close literature context"
              style={{
                border: 'none',
                background: 'none',
                cursor: 'pointer',
                fontSize: 16,
              }}
            >
              ✕
            </button>
          </div>
          {error && (
            <p
              style={{
                color: '#a8202e',
                padding: 12,
                margin: 0,
                fontSize: 12,
              }}
            >
              {error}
            </p>
          )}
          {!error && !ctx && (
            <p
              style={{ color: '#666', padding: 12, margin: 0, fontSize: 12 }}
            >
              Loading…
            </p>
          )}
          {ctx && <LiteratureContextCard ctx={ctx} />}
        </div>
      )}
    </span>
  )
}

// ── lit-entity preprocessing ──────────────────────────────────────────────────
// Transforms `<lit-entity name="X" kind="Y" />` occurrences in the raw
// markdown string into a sentinel token `\x00LITENT:X:kind\x00` before
// the string is passed to the whitespace-preserving renderer. The text
// renderer then splits on those sentinels and returns LitEntityButton nodes.

const LIT_ENTITY_TAG_RE = /<lit-entity\s+name="([^"]*)"\s+kind="([^"]*)"\s*\/?>/g

function preprocessLitEntities(raw: string): string {
  return raw.replace(LIT_ENTITY_TAG_RE, (_, name, kind) => {
    // NUL is safe as a delimiter because markdown text never contains it.
    return `\x00LITENT:${name}:${kind}\x00`
  })
}

// Splits a text node at sentinel tokens and returns a mixed array of plain
// strings and LitEntityButton elements.
function renderTextWithLitEntities(
  text: string,
  sessionId: string,
): React.ReactNode {
  // eslint-disable-next-line no-control-regex -- null-byte delimiters are the LitEntity wire protocol
  const parts = text.split(/(\x00LITENT:[^\x00]+\x00)/)
  if (parts.length === 1) return text
  return parts.map((part, i) => {
    // eslint-disable-next-line no-control-regex -- null-byte delimiters are the LitEntity wire protocol
    const m = /^\x00LITENT:([^:]+):([^\x00]+)\x00$/.exec(part)
    if (m) {
      return (
        <LitEntityButton
          key={i}
          name={m[1]!}
          kind={m[2] as EntityKind}
          sessionId={sessionId}
        />
      )
    }
    return <React.Fragment key={i}>{part}</React.Fragment>
  })
}

// ── AssistantTurnCard ─────────────────────────────────────────────────────────

interface Props {
  turn: Turn
  isLatest: boolean
  /** True iff this turn is the most-recent assistant turn carrying a
   *  `confirmation_card`. Only the latest card renders Accept/Revise
   *  actions; historical cards (from prior intake iterations that
   *  surfaced a card and were superseded) render their summary text
   *  but no interactive buttons. */
  isLatestConfirmationCard?: boolean
  pillStatusLine: string | null
  /** Optional mode + checkpoint overrides from the confirmation
   *  dropdowns. `undefined` preserves the legacy confirm shape. */
  onConfirm: (opts?: { mode?: SessionMode; checkpointMode?: CheckpointMode }) => void | Promise<void>
  onReject: () => void | Promise<void>
  onQuickReply: (option: string) => void | Promise<void>
  /** Drives the dropdown's force-explicit behavior for
   *  ClinicalTrial. Defaults to Bioinformatics if unknown. */
  projectClass?: ProjectClass
  /** When set, `<lit-entity name="X" kind="Y" />` spans in the assistant
   *  content are rendered as interactive LitEntityButton pills that fetch
   *  literature context on click. */
  sessionId?: string
  /** When provided AND `isLatest` is true, render an inline
   *  `BranchFromHereCard` footer affordance under the message body. The
   *  parent supplies the dispatcher (typically a wrapper around
   *  `postBranch` from chatClient). Absent / undefined suppresses the
   *  affordance — used today only after the session has emitted at
   *  least one package, so the SME has something concrete to branch
   *  from. */
  onBranch?: (rationale?: string) => void | Promise<void>
}

function AssistantTurnCardImpl({
  turn,
  isLatest,
  isLatestConfirmationCard,
  pillStatusLine,
  onConfirm,
  onReject,
  onQuickReply,
  projectClass,
  sessionId,
  onBranch,
}: Props) {
  // Preprocess lit-entity tags once per content string. If there are no
  // sentinel tokens, `preprocessed === turn.content` and the fast path
  // (plain string render) avoids any splitting overhead.
  const preprocessed = sessionId
    ? preprocessLitEntities(turn.content)
    : turn.content

  const hasSentinels = sessionId && preprocessed !== turn.content

  return (
    <article
      aria-label="Assistant message"
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
        {hasSentinels
          ? renderTextWithLitEntities(preprocessed, sessionId!)
          : turn.content}
      </div>
      {turn.quick_replies.length > 0 && (
        <QuickReplyRow options={turn.quick_replies} onPick={onQuickReply} />
      )}
      {turn.confirmation_card && isLatestConfirmationCard !== false && (
        <ConfirmationTurnCard
          card={turn.confirmation_card}
          onConfirm={onConfirm}
          onReject={onReject}
          projectClass={projectClass}
        />
      )}
      {isLatest && pillStatusLine && <ToolCallStatusPill statusLine={pillStatusLine} />}
      {isLatest && onBranch && (
        <div style={{ marginTop: '0.6rem' }}>
          <BranchFromHereCard onBranch={onBranch} />
        </div>
      )}
    </article>
  )
}

// React.memo'd because ChatTimeline re-renders on every streaming
// token; turn_id is a stable key, so the memo comparator's default
// (shallow prop equality) is correct — the only props that change
// per-render are `isLatest` and `pillStatusLine`, which are genuine
// reasons to re-render.
export default memo(AssistantTurnCardImpl)
