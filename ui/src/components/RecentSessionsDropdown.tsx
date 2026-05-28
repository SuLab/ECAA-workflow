// Title-bar "Recent ▼" picker. Lists up to 20 most-recently-active
// sessions across all roots and branches so an SME can jump back into a
// workflow they navigated away from. Clicking a row drives
// `switchToSession`, which pushes ?session=<id> so a refresh keeps the
// chosen workflow loaded. The "+ New session" item at the top resets to
// a fresh greeting via the `onNewSession` callback.

import { useCallback, useEffect, useRef, useState } from 'react'
import {
  getRecentSessions,
  type RecentSessionSummary,
} from '../api/chatClient'
import { useTitleBarPolling } from '../hooks/useTitleBarPolling'
import { RECENT_SESSIONS_POLL_MS } from '../lib/polling'
import { relativeTime } from '../lib/time'
import { Z } from '../lib/z-index'

interface Props {
  currentSessionId: string | null
  onSelect: (sessionId: string) => void
  onNewSession: () => void
}

function stateBadgeColor(kind: string): { bg: string; fg: string } {
  switch (kind) {
    case 'blocked':
      return { bg: 'var(--color-warning-bg)', fg: 'var(--color-warning-fg)' }
    case 'emitted':
    case 'amending':
      return { bg: 'var(--color-success-bg)', fg: 'var(--color-success-fg)' }
    case 'emitting':
    case 'ready_to_emit':
      return { bg: 'var(--color-accent-soft)', fg: 'var(--color-accent-fg)' }
    case 'pending_confirmation':
      return { bg: 'var(--color-info-bg)', fg: 'var(--color-info-fg)' }
    default:
      return {
        bg: 'var(--color-chrome-bg-elevated)',
        fg: 'var(--color-chrome-fg-muted)',
      }
  }
}

// Faithful logical-state label. Earlier this function lied for `emitted`,
// returning "Running" — that conflated session state with harness
// liveness. Harness liveness is now reported separately via
// executionBadge() (powered by the new `execution_status` field).
function stateBadgeLabel(kind: string): string {
  switch (kind) {
    case 'greeting':
      return 'Greeting'
    case 'intake':
      return 'Intake'
    case 'intake_followup':
      return 'Intake'
    case 'pending_confirmation':
      return 'Awaiting confirm'
    case 'ready_to_emit':
      return 'Ready'
    case 'emitting':
      return 'Emitting'
    case 'emitted':
      return 'Emitted'
    case 'amending':
      return 'Amending'
    case 'blocked':
      return 'Blocked'
    default:
      return kind
  }
}

// Harness liveness badge. Returns null for the common "no badge needed"
// states (idle / exited / older clients without the field) so the row
// stays uncluttered when nothing is actively running.
function executionBadge(
  s: RecentSessionSummary,
): { label: string; bg: string; fg: string } | null {
  if (s.execution_status === 'running') {
    return {
      label: 'Running',
      bg: 'var(--color-success-bg)',
      fg: 'var(--color-success-fg)',
    }
  }
  return null
}

export default function RecentSessionsDropdown({
  currentSessionId,
  onSelect,
  onNewSession,
}: Props) {
  const [open, setOpen] = useState(false)
  const [sessions, setSessions] = useState<RecentSessionSummary[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const containerRef = useRef<HTMLDivElement>(null)
  const buttonRef = useRef<HTMLButtonElement>(null)

  const load = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      const data = await getRecentSessions(20)
      setSessions(data)
    } catch (e) {
      setError(String(e))
    } finally {
      setLoading(false)
    }
  }, [])

  // Mount the open-dropdown initial fetch. The 30s periodic refresh
  // joins the shared title-bar tick (`useTitleBarPolling`).
  useEffect(() => {
    if (!open) return
    void load()
  }, [open, load])

  useTitleBarPolling({
    cadenceMs: RECENT_SESSIONS_POLL_MS,
    enabled: open,
    onTick: load,
  })

  // Close on outside click / Escape so the panel doesn't trap focus or
  // hide other chrome when a user moves on.
  useEffect(() => {
    if (!open) return
    const onDocClick = (e: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(e.target as Node)
      ) {
        setOpen(false)
      }
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        setOpen(false)
        buttonRef.current?.focus()
      }
    }
    document.addEventListener('mousedown', onDocClick)
    document.addEventListener('keydown', onKey)
    return () => {
      document.removeEventListener('mousedown', onDocClick)
      document.removeEventListener('keydown', onKey)
    }
  }, [open])

  const handleSelect = useCallback(
    (id: string) => {
      setOpen(false)
      if (id === currentSessionId) return
      onSelect(id)
    },
    [currentSessionId, onSelect],
  )

  const handleNew = useCallback(() => {
    setOpen(false)
    onNewSession()
  }, [onNewSession])

  return (
    <div ref={containerRef} style={{ position: 'relative' }}>
      <button
        ref={buttonRef}
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label="Open recent sessions"
        title="Recent sessions"
        onClick={() => setOpen((prev) => !prev)}
        style={{
          background: 'transparent',
          border: '1px solid var(--color-chrome-border-strong)',
          color: 'var(--color-chrome-fg-muted)',
          padding: '0.3rem 0.55rem',
          borderRadius: 4,
          cursor: 'pointer',
          fontSize: '0.72rem',
          marginRight: '0.35rem',
          display: 'inline-flex',
          alignItems: 'center',
          gap: '0.3rem',
        }}
        data-testid="recent-sessions-button"
      >
        Recent
        <span aria-hidden style={{ fontSize: '0.6rem', lineHeight: 1 }}>
          ▼
        </span>
      </button>
      {open && (
        <div
          role="menu"
          aria-label="Recent sessions"
          data-testid="recent-sessions-panel"
          style={{
            position: 'absolute',
            top: 'calc(100% + 4px)',
            right: 0,
            minWidth: 320,
            maxWidth: 480,
            maxHeight: 420,
            overflowY: 'auto',
            background: 'var(--color-chrome-bg)',
            border: '1px solid var(--color-chrome-border-strong)',
            borderRadius: 6,
            boxShadow: '0 6px 18px rgba(0,0,0,0.35)',
            zIndex: Z.DROPDOWN,
          }}
        >
          <button
            type="button"
            role="menuitem"
            onClick={handleNew}
            data-testid="recent-sessions-new"
            style={{
              display: 'block',
              width: '100%',
              textAlign: 'left',
              padding: '0.55rem 0.75rem',
              background: 'transparent',
              border: 'none',
              borderBottom: '1px solid var(--color-chrome-border)',
              color: 'var(--color-accent-fg)',
              cursor: 'pointer',
              fontSize: '0.78rem',
              fontWeight: 600,
            }}
          >
            + New session
          </button>
          {loading && sessions.length === 0 && (
            <div
              style={{
                padding: '0.6rem 0.75rem',
                fontSize: '0.72rem',
                color: 'var(--color-chrome-fg-faint)',
              }}
            >
              Loading…
            </div>
          )}
          {error && (
            <div
              role="alert"
              style={{
                padding: '0.6rem 0.75rem',
                fontSize: '0.72rem',
                color: 'var(--color-warning-fg)',
              }}
            >
              {error}
            </div>
          )}
          {!loading && !error && sessions.length === 0 && (
            <div
              style={{
                padding: '0.6rem 0.75rem',
                fontSize: '0.72rem',
                color: 'var(--color-chrome-fg-faint)',
              }}
            >
              No previous sessions.
            </div>
          )}
          {sessions.map((s) => {
            const isCurrent = s.session_id === currentSessionId
            const badge = stateBadgeColor(s.state_kind)
            const exec = executionBadge(s)
            const titleLine =
              s.title?.trim() ||
              (s.project_class && s.project_class !== 'Generic'
                ? s.project_class
                : 'Untitled session')
            return (
              <button
                key={s.session_id}
                type="button"
                role="menuitem"
                onClick={() => handleSelect(s.session_id)}
                data-testid={`recent-sessions-item-${s.session_id}`}
                aria-current={isCurrent ? 'true' : undefined}
                style={{
                  display: 'block',
                  width: '100%',
                  textAlign: 'left',
                  padding: '0.55rem 0.75rem',
                  background: isCurrent
                    ? 'var(--color-chrome-bg-elevated)'
                    : 'transparent',
                  border: 'none',
                  borderBottom: '1px solid var(--color-chrome-border)',
                  color: 'var(--color-chrome-fg)',
                  cursor: isCurrent ? 'default' : 'pointer',
                  fontSize: '0.75rem',
                  fontWeight: isCurrent ? 600 : 400,
                }}
              >
                <div
                  style={{
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'space-between',
                    gap: '0.5rem',
                  }}
                >
                  <span
                    style={{
                      flex: 1,
                      overflow: 'hidden',
                      textOverflow: 'ellipsis',
                      whiteSpace: 'nowrap',
                    }}
                  >
                    {titleLine}
                  </span>
                  <span
                    style={{
                      display: 'inline-flex',
                      gap: '0.25rem',
                      flexShrink: 0,
                    }}
                  >
                    <span
                      style={{
                        fontSize: '0.65rem',
                        padding: '0.1rem 0.4rem',
                        background: badge.bg,
                        color: badge.fg,
                        borderRadius: 999,
                      }}
                    >
                      {stateBadgeLabel(s.state_kind)}
                    </span>
                    {exec && (
                      <span
                        data-testid="exec-status-pill"
                        style={{
                          fontSize: '0.65rem',
                          padding: '0.1rem 0.4rem',
                          background: exec.bg,
                          color: exec.fg,
                          borderRadius: 999,
                        }}
                      >
                        {exec.label}
                      </span>
                    )}
                  </span>
                </div>
                <div
                  style={{
                    display: 'flex',
                    gap: '0.6rem',
                    fontSize: '0.65rem',
                    color: 'var(--color-chrome-fg-faint)',
                    marginTop: '0.2rem',
                  }}
                >
                  <code>{s.session_id.slice(0, 8)}</code>
                  <span>{relativeTime(s.last_activity)}</span>
                  <span>{s.n_turns} turns</span>
                  {s.parent_id && <span>↳ branch</span>}
                </div>
              </button>
            )
          })}
        </div>
      )}
    </div>
  )
}
