// ⌘K / Ctrl+K command palette. Fuzzy-match index over tasks, tabs,
// recent decisions, and nav actions. Keyboard-only navigation (arrow
// keys to move, Enter to activate, Escape to close). Wraps the
// existing URL-hash deep-link contract for task navigation.

import { useEffect, useMemo, useRef, useState } from 'react'
import { getChatDag } from '../api/chatClient'
import type { DAG } from '../types'
import { Z } from '../lib/z-index'
import { Dialog } from './primitives/Dialog'

// DAG cache lifetime. Refetch when the open-toggle elapses this many
// ms since the cached DAG's `at` timestamp (or when the sessionId
// changes). Keeps a rapid-fire ⌘K open-close from re-issuing GETs.
const DAG_CACHE_TTL_MS = 60_000

export interface Command {
  id: string
  label: string
  /** Optional secondary text shown muted after the label. */
  hint?: string
  /** Category badge shown on the left. */
  category: 'Task' | 'Tab' | 'Action' | 'Session' | 'Composition'
  perform: () => void
}

interface Props {
  sessionId: string | null
}

// Module-level helpers — closure-free so they're reference-stable
// across CommandPalette renders and don't churn the `commands` useMemo
// on every keystroke.
function jumpToTask(taskId: string) {
  window.location.hash = `task=${encodeURIComponent(taskId)}`
}
function switchTab(tab: string) {
  window.dispatchEvent(new CustomEvent('ecaax:switch-tab', { detail: { tab } }))
}

export default function CommandPalette({ sessionId }: Props): JSX.Element | null {
  const [dag, setDag] = useState<DAG | null>(null)
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const [highlight, setHighlight] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)

  // Cache the last fetched DAG per session for 60s. Without this,
  // every ⌘K open re-issued the GET — tasks rarely change inside a
  // single browsing minute, and the palette is most commonly used in
  // bursts (open, type, close, reopen) where the round-trip latency
  // shows up as flicker.
  const dagCacheRef = useRef<{ sessionId: string; at: number; dag: DAG | null }>({
    sessionId: '',
    at: 0,
    dag: null,
  })

  // Global toggle shortcut (⌘K / Ctrl+K). Escape and Tab focus-trap
  // handling are owned by the <Dialog> primitive; this effect owns
  // only the open/close toggle.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey
      if (mod && (e.key === 'k' || e.key === 'K')) {
        e.preventDefault()
        setOpen((o) => !o)
      }
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [])

  useEffect(() => {
    if (!open) return undefined
    setHighlight(0)
    setQuery('')
    // Focus the input after paint.
    const id = requestAnimationFrame(() => {
      inputRef.current?.focus()
    })
    if (!sessionId) return () => cancelAnimationFrame(id)
    // Cache lookup. Fresh = same session AND within the TTL window.
    const cache = dagCacheRef.current
    const fresh =
      cache.sessionId === sessionId && Date.now() - cache.at < DAG_CACHE_TTL_MS
    if (fresh) {
      setDag(cache.dag)
      return () => cancelAnimationFrame(id)
    }
    let cancelled = false
    void (async () => {
      try {
        const d = await getChatDag(sessionId)
        if (cancelled) return
        dagCacheRef.current = { sessionId, at: Date.now(), dag: d }
        setDag(d)
      } catch {
        // ignore — palette degrades to tabs-only.
      }
    })()
    return () => {
      cancelled = true
      cancelAnimationFrame(id)
    }
  }, [open, sessionId])

  const commands = useMemo<Command[]>(() => {
    const list: Command[] = []
    if (dag) {
      for (const [taskId, task] of Object.entries(dag.tasks)) {
        if (!task) continue
        list.push({
          id: `task:${taskId}`,
          label: task.description ?? taskId,
          hint: taskId,
          category: 'Task',
          perform: () => jumpToTask(taskId),
        })
      }
    }
    const tabs = [
      ['plan', 'Plan'],
      ['composition', 'Composition'],
      ['state', 'Status'],
      ['documents', 'Documents'],
      ['jobs', 'Progress'],
      ['metrics', 'Performance'],
      ['figures', 'Figures'],
      ['dashboard', 'Dashboard'],
      ['decisions', 'Decisions'],
      ['history', 'History'],
      ['compare', 'Compare'],
    ] as const
    for (const [id, label] of tabs) {
      list.push({
        id: `tab:${id}`,
        label: `Open ${label}`,
        hint: 'tab',
        category: 'Tab',
        perform: () => switchTab(id),
      })
    }
    list.push({
      id: 'nav:settings',
      label: 'Open settings',
      hint: 'gear',
      category: 'Action',
      perform: () => {
        window.dispatchEvent(new CustomEvent('ecaax:open-settings'))
      },
    })
    // Quick-jump shortcuts to the Composition tab's
    // common surfaces. All resolve to the same tab switch but with
    // different aliases so the SME can search "edge proofs" /
    // "assumption ledger" / "ranked alternatives" and find them.
    list.push({
      id: 'phase15:edge-proofs',
      label: 'Show edge proofs',
      hint: 'why does this edge exist',
      category: 'Composition',
      perform: () => switchTab('composition'),
    })
    list.push({
      id: 'phase15:assumptions',
      label: 'Show assumption ledger',
      hint: 'unresolved assumptions',
      category: 'Composition',
      perform: () => switchTab('composition'),
    })
    list.push({
      id: 'phase15:alternatives',
      label: 'Show ranked alternatives',
      hint: 'choose composition variant',
      category: 'Composition',
      perform: () => switchTab('composition'),
    })
    list.push({
      id: 'phase15:adapter-warnings',
      label: 'Show adapter warnings',
      hint: 'lossy / risky adapter review',
      category: 'Composition',
      perform: () => switchTab('composition'),
    })
    list.push({
      id: 'phase15:validation',
      label: 'Show validation status',
      hint: 'validators that ran on tasks',
      category: 'Composition',
      perform: () => switchTab('composition'),
    })
    return list
  }, [dag])

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase()
    if (!q) return commands
    // Trivial fuzzy match: each char in q must appear in order in the
    // label or hint. Score by earliest match + shorter label.
    const scored = commands
      .map((c) => {
        const hay = (c.label + ' ' + (c.hint ?? '') + ' ' + c.category).toLowerCase()
        let score = 0
        let idx = 0
        for (const ch of q) {
          const pos = hay.indexOf(ch, idx)
          if (pos < 0) return null
          score += pos - idx
          idx = pos + 1
        }
        return [c, score + hay.length / 100] as const
      })
      .filter((x): x is readonly [Command, number] => x !== null)
    scored.sort(([, a], [, b]) => a - b)
    return scored.map(([c]) => c).slice(0, 25)
  }, [commands, query])

  useEffect(() => {
    if (highlight >= filtered.length) setHighlight(Math.max(0, filtered.length - 1))
  }, [filtered, highlight])

  if (!open) return null

  const run = (c: Command) => {
    c.perform()
    setOpen(false)
  }

  return (
    <Dialog
      onClose={() => setOpen(false)}
      ariaLabel="Command palette"
      zIndex={Z.PALETTE}
      backdropStyle={{
        background: 'rgba(0,0,0,0.28)',
        alignItems: 'flex-start',
        paddingTop: '18vh',
      }}
      contentStyle={{
        width: 520,
        maxWidth: '92vw',
        padding: 0,
        boxShadow: '0 24px 64px rgba(0,0,0,0.3)',
        overflow: 'hidden',
      }}
    >
        <input
          ref={inputRef}
          type="text"
          placeholder="Type to search tasks, tabs, actions…"
          value={query}
          aria-controls="command-palette-results"
          aria-activedescendant={filtered[highlight]?.id ?? undefined}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            // Arrow / Enter — palette-specific. Escape is handled by
            // the parent <Dialog>'s window-level keydown listener.
            if (e.key === 'ArrowDown') {
              e.preventDefault()
              setHighlight((h) => Math.min(filtered.length - 1, h + 1))
            } else if (e.key === 'ArrowUp') {
              e.preventDefault()
              setHighlight((h) => Math.max(0, h - 1))
            } else if (e.key === 'Enter') {
              e.preventDefault()
              const c = filtered[highlight]
              if (c) run(c)
            }
          }}
          aria-label="Command input"
          style={{
            width: '100%',
            padding: '0.8rem 1rem',
            border: 'none',
            borderBottom: '1px solid var(--color-border-default)',
            fontSize: '0.95rem',
            color: 'var(--color-text-primary)',
            background: 'transparent',
            outline: 'none',
          }}
        />
        <ul
          id="command-palette-results"
          role="listbox"
          aria-label="Command results"
          style={{
            listStyle: 'none',
            margin: 0,
            padding: 0,
            maxHeight: 360,
            overflowY: 'auto',
          }}
        >
          {filtered.length === 0 ? (
            <li
              style={{
                padding: '0.8rem 1rem',
                color: 'var(--color-text-muted)',
                fontSize: '0.85rem',
              }}
            >
              Nothing matches.
            </li>
          ) : (
            filtered.map((c, i) => (
              <li
                key={c.id}
                id={c.id}
                role="option"
                aria-selected={i === highlight}
                onMouseDown={(e) => {
                  e.preventDefault()
                  run(c)
                }}
                onMouseEnter={() => setHighlight(i)}
                style={{
                  padding: '0.5rem 1rem',
                  background: i === highlight ? 'var(--color-surface-1)' : 'transparent',
                  display: 'flex',
                  alignItems: 'center',
                  gap: '0.6rem',
                  cursor: 'pointer',
                  fontSize: '0.85rem',
                }}
              >
                <span
                  style={{
                    fontSize: '0.65rem',
                    padding: '0.1rem 0.4rem',
                    borderRadius: 4,
                    background: 'var(--color-surface-2)',
                    color: 'var(--color-text-muted)',
                    textTransform: 'uppercase',
                    letterSpacing: '0.05em',
                  }}
                >
                  {c.category}
                </span>
                <span style={{ color: 'var(--color-text-primary)' }}>{c.label}</span>
                {c.hint && (
                  <span
                    style={{
                      marginLeft: 'auto',
                      color: 'var(--color-text-muted)',
                      fontSize: '0.72rem',
                      fontFamily: 'ui-monospace, monospace',
                    }}
                  >
                    {c.hint}
                  </span>
                )}
              </li>
            ))
          )}
        </ul>
    </Dialog>
  )
}
