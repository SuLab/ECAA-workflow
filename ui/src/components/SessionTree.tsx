// Top-nav pill row that shows the full lineage chain — Root → … → current
// session → children. Walks up via getChatState().parent_session_id until
// it finds null (the true root), then fetches direct children via
// GET /api/chat/sessions?parent=<id>. Each hop is a separate fetch but
// they're cached for the current `currentSessionId` so the cost is paid
// only when the user navigates between sessions.

import { useEffect, useState } from 'react'
import {
  getChatState,
  listChildSessions,
  type ChildSummaryWire,
} from '../api/chatClient'
import { useTitleBarPolling } from '../hooks/useTitleBarPolling'
import { SESSION_TREE_POLL_MS } from '../lib/polling'
import { relativeTime } from '../lib/time'

interface Props {
  currentSessionId: string | null
  /// Set when this session is itself a branch — drives the "Branch" vs
  /// "Root" label on the current pill so the title-bar agrees with the
  /// "branched session" chip in PlanTab.
  parentSessionId?: string | null
  onSelectSession: (id: string) => void
}

/**
 * Cap the parent walk so a corrupted lineage cycle (theoretical — server
 * rejects creates that would loop, but be defensive) can't peg the title
 * bar in a fetch loop.
 */
const MAX_ANCESTOR_HOPS = 20

interface Ancestor {
  id: string
}

async function walkAncestors(
  firstParentId: string,
  signal: AbortSignal,
): Promise<Ancestor[]> {
  const chain: Ancestor[] = []
  const seen = new Set<string>()
  let cursor: string | null = firstParentId
  while (cursor && chain.length < MAX_ANCESTOR_HOPS) {
    if (seen.has(cursor)) break // defensive cycle guard
    seen.add(cursor)
    chain.push({ id: cursor })
    try {
      const snap = await getChatState(cursor, { signal })
      cursor = snap.parent_session_id ?? null
    } catch {
      // Network / server error: stop the walk and render what we have.
      // Non-fatal because the title bar's job is awareness, not enforcement.
      break
    }
  }
  return chain
}

export default function SessionTree({ currentSessionId, parentSessionId, onSelectSession }: Props) {
  const [children, setChildren] = useState<ChildSummaryWire[]>([])
  // Ancestors are ordered nearest-first ([parent, grandparent, ..., root]).
  // The render flips to root-first so the tree reads left→right in lineage
  // order: Root … Grandparent … Parent … Current … Child1 Child2.
  const [ancestors, setAncestors] = useState<Ancestor[]>([])

  // Mount-time initial fetch + clear on session-id change. The
  // periodic refresh is delegated to the shared title-bar tick
  // (`useTitleBarPolling`) below so all four title-bar chips share a
  // single setInterval instead of fanning out four independent ones.
  useEffect(() => {
    if (!currentSessionId) {
      setChildren([])
      setAncestors([])
      return
    }
    const controller = new AbortController()
    let cancelled = false
    const load = async () => {
      try {
        const data = await listChildSessions(currentSessionId)
        if (!cancelled) setChildren(data)
      } catch {
        // Non-fatal — tree collapses to current-only on the children half.
      }
      if (parentSessionId) {
        const chain = await walkAncestors(parentSessionId, controller.signal)
        if (!cancelled) setAncestors(chain)
      } else {
        if (!cancelled) setAncestors([])
      }
    }
    void load()
    return () => {
      cancelled = true
      controller.abort()
    }
  }, [currentSessionId, parentSessionId])

  // Poll every 15s so a branch created from another tab / from the
  // task drawer's "Explore in a branch" / from any other path
  // surfaces in the title-bar pill row without a manual refresh.
  // Visibility-gated by the shared hook: hidden tabs don't poll, but
  // refocus triggers an immediate catch-up tick.
  useTitleBarPolling({
    cadenceMs: SESSION_TREE_POLL_MS,
    enabled: currentSessionId != null,
    onTick: () => {
      if (!currentSessionId) return
      void listChildSessions(currentSessionId).then(setChildren).catch(() => {
        // Non-fatal — keep the last-known children list.
      })
    },
  })

  if (!currentSessionId) return null

  const short = (id: string) => id.slice(0, 8)
  // Ancestors are stored nearest-first; reverse so the leftmost pill is
  // the true root and the rightmost ancestor pill is the direct parent.
  const ancestorPills = [...ancestors].reverse().map((a, idx, arr) => ({
    id: a.id,
    isCurrent: false,
    // The first (leftmost, reversed-position 0) is the root; everything
    // between is an intermediate "Branch" hop.
    label: (idx === 0 ? 'Root' : 'Branch') as 'Root' | 'Branch',
    when: null as string | null,
    // Tag the immediate parent so the connector arrow renders without it.
    isImmediateParent: idx === arr.length - 1,
  }))
  const currentPill = {
    id: currentSessionId,
    isCurrent: true,
    label: (parentSessionId ? 'Branch' : 'Root') as 'Root' | 'Branch',
    when: null as string | null,
    isImmediateParent: false,
  }
  const childPills = children.map((c) => ({
    id: c.session_id,
    isCurrent: false,
    label: 'Branch' as const,
    when: c.lineage?.branched_at ?? c.created_at,
    isImmediateParent: false,
  }))
  const pills = [...ancestorPills, currentPill, ...childPills]

  return (
    <nav
      aria-label="Session tree"
      style={{ display: 'flex', gap: '0.35rem', alignItems: 'center', flexWrap: 'wrap' }}
    >
      {pills.map((p) => {
        const active = p.isCurrent
        return (
          <button
            key={p.id}
            type="button"
            aria-current={active ? 'page' : undefined}
            onClick={() => {
              if (!active) onSelectSession(p.id)
            }}
            style={{
              display: 'inline-flex',
              alignItems: 'center',
              gap: '0.35rem',
              padding: '0.25rem 0.55rem',
              background: active ? 'var(--color-accent)' : 'transparent',
              color: active
                ? 'var(--color-accent-fg)'
                : 'var(--color-chrome-fg-muted)',
              border: active
                ? '1px solid var(--color-accent)'
                : '1px solid var(--color-chrome-border-strong)',
              borderRadius: 999,
              cursor: active ? 'default' : 'pointer',
              fontSize: '0.72rem',
              fontWeight: 600,
              fontFamily: 'inherit',
            }}
          >
            <span
              aria-hidden="true"
              style={{
                fontSize: '0.62rem',
                fontWeight: 700,
                padding: '0 0.35rem',
                borderRadius: 4,
                background: active
                  ? 'rgba(255,255,255,0.2)'
                  : 'var(--color-chrome-bg-elevated)',
                color: active
                  ? 'var(--color-accent-fg)'
                  : 'var(--color-chrome-fg-faint)',
                letterSpacing: '0.03em',
                textTransform: 'uppercase',
              }}
            >
              {p.label}
            </span>
            <code
              style={{
                color: active
                  ? 'var(--color-accent-fg)'
                  : 'var(--color-chrome-fg-accent)',
                fontSize: '0.72rem',
              }}
            >
              {short(p.id)}
            </code>
            {p.when && (
              <span
                style={{
                  color: active
                    ? 'rgba(255,255,255,0.75)'
                    : 'var(--color-chrome-fg-faint)',
                  fontSize: '0.66rem',
                }}
              >
                {relativeTime(p.when)}
              </span>
            )}
          </button>
        )
      })}
      {children.length === 0 && (
        <span
          style={{
            color: 'var(--color-chrome-fg-faint)',
            fontSize: '0.68rem',
            fontStyle: 'italic',
            pointerEvents: 'none',
          }}
        >
          Branching creates siblings you can switch between here.
        </span>
      )}
    </nav>
  )
}
