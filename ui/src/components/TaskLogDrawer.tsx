import { useEffect, useMemo, useRef, useState } from 'react'
import { artifactUrl } from '../api/chatClient'
import { FIGURES_POLL_MS, LOG_POLL_MS } from '../lib/polling'
import { Z } from '../lib/z-index'
import { Dialog } from './primitives/Dialog'

interface Props {
  sessionId: string | null
  taskId: string | null
  onClose: () => void
}

const MAX_BYTES = 128 * 1024

type DrawerTab = 'log' | 'figures'

interface FiguresManifest {
  stage_id: string
  written: Record<string, string>
  skipped: Record<string, string>
  errors: Record<string, string>
}

/**
 * Right-side drawer with two tabs:
 *  - Log: tails `runtime/outputs/<task_id>/progress.log` every 2 s
 *  - Figures: polls `runtime/outputs/<task_id>/figures/manifest.json`
 *  every 5 s and renders each listed PNG as a thumbnail; click opens
 *  a lightbox. Supports Escape + arrow-key navigation.
 *
 * Both tabs use the existing `/api/chat/session/:id/artifacts/*path` route
 * — no new server endpoint required. The figures manifest is written by
 * the shared plotting library (`runtime/plotting/core.generate()`); the
 * agent is instructed to call it before marking any task with a non-empty
 * `required_figures` list as completed.
 */
export default function TaskLogDrawer({ sessionId, taskId, onClose }: Props) {
  const [tab, setTab] = useState<DrawerTab>('log')
  if (!taskId || !sessionId) return null

  return (
    <aside
      role="complementary"
      aria-label={`Progress log for task ${taskId}`}
      data-testid="task-log-drawer"
      style={{
        position: 'absolute',
        top: 0,
        right: 0,
        width: 520,
        maxWidth: '85vw',
        height: '100%',
        background: 'var(--color-chrome-bg-elevated)',
        color: 'var(--color-chrome-fg-muted)',
        boxShadow: '-2px 0 10px rgba(0,0,0,0.25)',
        display: 'flex',
        flexDirection: 'column',
        zIndex: Z.STICKY_BANNER,
        borderLeft: '1px solid var(--color-chrome-border-strong)',
      }}
    >
      <DrawerHeader
        taskId={taskId}
        tab={tab}
        onTabChange={setTab}
        onClose={onClose}
      />
      {tab === 'log' && <LogTab sessionId={sessionId} taskId={taskId} />}
      {tab === 'figures' && <FiguresTab sessionId={sessionId} taskId={taskId} />}
    </aside>
  )
}

function DrawerHeader({
  taskId,
  tab,
  onTabChange,
  onClose,
}: {
  taskId: string | null
  tab: DrawerTab
  onTabChange: (t: DrawerTab) => void
  onClose: () => void
}) {
  return (
    <header
      style={{
        display: 'flex',
        justifyContent: 'space-between',
        alignItems: 'center',
        padding: '0.6rem 0.9rem',
        borderBottom: '1px solid var(--color-chrome-border-strong)',
        background: 'var(--color-chrome-bg-elevated)',
        flexShrink: 0,
      }}
    >
      <div style={{ display: 'flex', alignItems: 'center', gap: '0.75rem' }}>
        <div>
          <div style={{ fontSize: '0.7rem', color: 'var(--color-text-faint)' }}>task</div>
          <div
            style={{
              fontFamily: 'ui-monospace, monospace',
              fontSize: '0.85rem',
              fontWeight: 600,
            }}
          >
            {taskId ?? '-'}
          </div>
        </div>
        <div
          role="tablist"
          aria-label="Task drawer tabs"
          style={{ display: 'flex', gap: '0.2rem' }}
        >
          {(['log', 'figures'] as DrawerTab[]).map((t) => (
            <button
              key={t}
              type="button"
              role="tab"
              aria-selected={tab === t}
              aria-controls={`task-drawer-panel-${t}`}
              id={`task-drawer-tab-${t}`}
              onClick={() => onTabChange(t)}
              style={{
                background: tab === t ? 'var(--color-text-secondary)' : 'transparent',
                color: tab === t ? 'var(--color-border-subtle)' : 'var(--color-text-faint)',
                border: '1px solid #475569',
                borderRadius: 4,
                padding: '3px 10px',
                fontSize: '0.72rem',
                cursor: 'pointer',
                textTransform: 'capitalize',
              }}
            >
              {t}
            </button>
          ))}
        </div>
      </div>
      <button
        onClick={onClose}
        aria-label="Close log drawer"
        style={{
          background: 'transparent',
          border: '1px solid #475569',
          color: 'var(--color-chrome-fg-muted)',
          borderRadius: 4,
          padding: '2px 8px',
          fontSize: '0.72rem',
          cursor: 'pointer',
        }}
      >
        close
      </button>
    </header>
  )
}

function LogTab({ sessionId, taskId }: { sessionId: string; taskId: string }) {
  const [text, setText] = useState<string>('')
  const [error, setError] = useState<string | null>(null)
  const [autoscroll, setAutoscroll] = useState(true)
  const scrollRef = useRef<HTMLPreElement | null>(null)

  useEffect(() => {
    if (!sessionId || !taskId) return
    let cancelled = false
    const fetchOnce = async () => {
      try {
        const url = artifactUrl(
          sessionId,
          `runtime/outputs/${encodeURIComponent(taskId)}/progress.log`,
        )
        const res = await fetch(url) // allow-bare-fetch: artifactUrl carries share-token
        if (!res.ok) {
          if (res.status === 404 && !cancelled) {
            setText('')
            setError(null)
          } else if (!cancelled) {
            setError(`HTTP ${res.status}`)
          }
          return
        }
        const body = await res.text()
        if (cancelled) return
        const trimmed =
          body.length > MAX_BYTES ? body.slice(body.length - MAX_BYTES) : body
        setText(trimmed)
        setError(null)
      } catch (e) {
        if (!cancelled) setError((e as Error).message)
      }
    }
    void fetchOnce()
    const id = window.setInterval(fetchOnce, LOG_POLL_MS)
    return () => {
      cancelled = true
      window.clearInterval(id)
    }
  }, [sessionId, taskId])

  useEffect(() => {
    if (autoscroll && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight
    }
  }, [text, autoscroll])

  return (
    <div
      role="tabpanel"
      id="task-drawer-panel-log"
      aria-labelledby="task-drawer-tab-log"
      style={{
        flex: 1,
        minHeight: 0,
        display: 'flex',
        flexDirection: 'column',
      }}
    >
      <div
        style={{
          padding: '0.3rem 0.9rem',
          borderBottom: '1px solid var(--color-chrome-border-strong)',
          background: 'var(--color-chrome-bg-elevated)',
          flexShrink: 0,
        }}
      >
        <label
          style={{
            fontSize: '0.7rem',
            color: 'var(--color-text-faint)',
            display: 'flex',
            alignItems: 'center',
            gap: 4,
            cursor: 'pointer',
          }}
        >
          <input
            type="checkbox"
            checked={autoscroll}
            onChange={(e) => setAutoscroll(e.target.checked)}
            aria-label="Autoscroll"
          />
          autoscroll
        </label>
      </div>
      <pre
        ref={scrollRef}
        data-testid="task-log-body"
        style={{
          flex: 1,
          margin: 0,
          padding: '0.75rem 0.9rem',
          overflow: 'auto',
          fontFamily: 'ui-monospace, monospace',
          fontSize: '0.78rem',
          lineHeight: 1.45,
          whiteSpace: 'pre-wrap',
          wordBreak: 'break-word',
        }}
      >
        {error ? (
          <span style={{ color: 'var(--color-danger-border)' }}>[error: {error}]</span>
        ) : text.length === 0 ? (
          <span style={{ color: 'var(--color-text-muted)', fontStyle: 'italic' }}>
            No progress log yet — the agent writes to runtime/outputs/
            {taskId}/progress.log as it works.
          </span>
        ) : (
          text
        )}
      </pre>
    </div>
  )
}

function FiguresTab({
  sessionId,
  taskId,
}: {
  sessionId: string
  taskId: string
}) {
  const [manifest, setManifest] = useState<FiguresManifest | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [lightbox, setLightbox] = useState<string | null>(null)

  useEffect(() => {
    if (!sessionId || !taskId) return
    // discover_* and validate_* never write figures; skip the poll to
    // avoid 404 noise in the console for those drawers.
    if (taskId.startsWith('discover_') || taskId.startsWith('validate_')) {
      setManifest(null)
      setError(null)
      return
    }
    let cancelled = false
    const fetchOnce = async () => {
      try {
        const url = artifactUrl(
          sessionId,
          `runtime/outputs/${encodeURIComponent(taskId)}/figures/manifest.json`,
        )
        const res = await fetch(url) // allow-bare-fetch: artifactUrl carries share-token
        if (!res.ok) {
          if (res.status === 404 && !cancelled) {
            setManifest(null)
            setError(null)
          } else if (!cancelled) {
            setError(`HTTP ${res.status}`)
          }
          return
        }
        const body = (await res.json()) as FiguresManifest
        if (!cancelled) {
          setManifest(body)
          setError(null)
        }
      } catch (e) {
        if (!cancelled) setError((e as Error).message)
      }
    }
    void fetchOnce()
    const id = window.setInterval(fetchOnce, FIGURES_POLL_MS)
    return () => {
      cancelled = true
      window.clearInterval(id)
    }
  }, [sessionId, taskId])

  const entries = useMemo(() => {
    if (!manifest) return []
    return Object.entries(manifest.written).map(([id, absPath]) => ({
      id,
      // `absPath` is the full filesystem path from the manifest. Strip
      // everything up to the `runtime/` segment to build a URL the
      // /artifacts route can serve. Falls back to id-based path.
      url: deriveArtifactUrl(sessionId, taskId, id, absPath),
    }))
  }, [manifest, sessionId, taskId])

  // Keyboard navigation for lightbox (Escape closes, arrows move)
  useEffect(() => {
    if (!lightbox) return
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        setLightbox(null)
      } else if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
        const ids = entries.map((x) => x.id)
        const cur = ids.indexOf(lightbox)
        if (cur < 0) return
        const next =
          e.key === 'ArrowRight'
            ? (cur + 1) % ids.length
            : (cur - 1 + ids.length) % ids.length
        setLightbox(ids[next]!)
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [lightbox, entries])

  return (
    <div
      role="tabpanel"
      id="task-drawer-panel-figures"
      aria-labelledby="task-drawer-tab-figures"
      data-testid="task-figures-panel"
      style={{
        flex: 1,
        minHeight: 0,
        display: 'flex',
        flexDirection: 'column',
      }}
    >
      {error ? (
        <div style={{ padding: '1rem', color: 'var(--color-danger-border)', fontSize: '0.78rem' }}>
          [error: {error}]
        </div>
      ) : !manifest ? (
        <div
          style={{
            padding: '1rem',
            color: 'var(--color-text-muted)',
            fontStyle: 'italic',
            fontSize: '0.82rem',
          }}
        >
          No figures yet — produced once the stage's compute script calls{' '}
          <code>runtime.plotting.core.generate()</code>.
        </div>
      ) : entries.length === 0 ? (
        <div style={{ padding: '1rem', fontSize: '0.82rem' }}>
          <div style={{ color: 'var(--color-border-strong)', marginBottom: '0.5rem' }}>
            No figures written yet for <code>{manifest.stage_id}</code>.
          </div>
          {Object.keys(manifest.skipped).length > 0 && (
            <SkipList
              title="Skipped"
              entries={manifest.skipped}
              color="var(--color-warning-border)"
            />
          )}
          {Object.keys(manifest.errors).length > 0 && (
            <SkipList
              title="Errors"
              entries={manifest.errors}
              color="var(--color-danger-border)"
            />
          )}
        </div>
      ) : (
        <div
          style={{
            flex: 1,
            overflowY: 'auto',
            padding: '0.75rem',
            display: 'grid',
            gridTemplateColumns: 'repeat(auto-fill, minmax(180px, 1fr))',
            gap: '0.6rem',
            alignContent: 'start',
          }}
        >
          {entries.map(({ id, url }) => (
            <figure
              key={id}
              style={{ margin: 0, display: 'flex', flexDirection: 'column' }}
            >
              <button
                type="button"
                onClick={() => setLightbox(id)}
                aria-label={`Open figure ${id}`}
                style={{
                  padding: 0,
                  background: 'var(--color-chrome-bg-elevated)',
                  border: '1px solid var(--color-chrome-border-strong)',
                  borderRadius: 4,
                  cursor: 'zoom-in',
                  overflow: 'hidden',
                }}
              >
                <img
                  src={url}
                  alt={id}
                  loading="lazy"
                  style={{
                    width: '100%',
                    height: 'auto',
                    display: 'block',
                    background: 'var(--color-surface-1)',
                  }}
                />
              </button>
              <figcaption
                style={{
                  fontFamily: 'ui-monospace, monospace',
                  fontSize: '0.7rem',
                  marginTop: 3,
                  color: 'var(--color-border-strong)',
                  wordBreak: 'break-all',
                }}
              >
                {id}
              </figcaption>
            </figure>
          ))}
        </div>
      )}
      {lightbox && (
        <Lightbox
          onClose={() => setLightbox(null)}
          figureId={lightbox}
          url={entries.find((x) => x.id === lightbox)?.url ?? ''}
        />
      )}
    </div>
  )
}

function deriveArtifactUrl(
  sessionId: string,
  taskId: string,
  figureId: string,
  absPath: string,
): string {
  const marker = '/runtime/'
  const i = absPath.indexOf(marker)
  if (i >= 0) {
    const relative = absPath.slice(i + 1) // drop leading slash, keep 'runtime/...'
    return artifactUrl(sessionId, relative)
  }
  return artifactUrl(
    sessionId,
    `runtime/outputs/${encodeURIComponent(taskId)}/figures/${encodeURIComponent(figureId)}.png`,
  )
}

function SkipList({
  title,
  entries,
  color,
}: {
  title: string
  entries: Record<string, string>
  color: string
}) {
  return (
    <div style={{ marginTop: 8 }}>
      <div style={{ fontWeight: 600, color, fontSize: '0.72rem' }}>{title}</div>
      <ul
        style={{
          listStyle: 'none',
          paddingLeft: 0,
          marginTop: 4,
          marginBottom: 0,
        }}
      >
        {Object.entries(entries).map(([id, reason]) => (
          <li key={id} style={{ fontSize: '0.72rem', marginBottom: 2 }}>
            <code style={{ color: 'var(--color-border-subtle)' }}>{id}</code>:{' '}
            <span style={{ color: 'var(--color-text-faint)' }}>{reason}</span>
          </li>
        ))}
      </ul>
    </div>
  )
}

function Lightbox({
  figureId,
  url,
  onClose,
}: {
  figureId: string
  url: string
  onClose: () => void
}) {
  // Backdrop click anywhere outside the figure should close — Dialog's
  // `closeOnOutsideClick` (default true) handles that. The custom
  // styling here replicates the pre-Dialog look: full-screen 85%-dark
  // backdrop, centered figure, "zoom-out" cursor on the backdrop only.
  return (
    <Dialog
      onClose={onClose}
      ariaLabel={`Figure: ${figureId}`}
      zIndex={Z.PALETTE}
      backdropStyle={{
        background: 'rgba(15, 23, 42, 0.85)',
        flexDirection: 'column',
        padding: '2rem',
        cursor: 'zoom-out',
      }}
      contentStyle={{
        background: 'transparent',
        border: 'none',
        borderRadius: 0,
        padding: 0,
        boxShadow: 'none',
        display: 'flex',
        flexDirection: 'column',
        alignItems: 'center',
        cursor: 'default',
      }}
    >
      <div data-testid="figure-lightbox" style={{ display: 'contents' }}>
        <img
          src={url}
          alt={figureId}
          style={{
            maxWidth: '95vw',
            maxHeight: '85vh',
            background: 'var(--color-surface-1)',
            borderRadius: 6,
            boxShadow: '0 6px 30px rgba(0,0,0,0.5)',
          }}
        />
        <div
          style={{
            marginTop: 12,
            color: 'var(--color-chrome-fg-muted)',
            display: 'flex',
            gap: 12,
            alignItems: 'center',
          }}
        >
          <span
            style={{
              fontFamily: 'ui-monospace, monospace',
              fontSize: '0.8rem',
            }}
          >
            {figureId}
          </span>
          <a
            href={url}
            download={`${figureId}.png`}
            style={{
              fontSize: '0.75rem',
              color: 'var(--color-accent)',
              textDecoration: 'underline',
              cursor: 'pointer',
            }}
          >
            download
          </a>
          <button
            type="button"
            onClick={onClose}
            style={{
              fontSize: '0.72rem',
              background: 'transparent',
              color: 'var(--color-chrome-fg-muted)',
              border: '1px solid #475569',
              borderRadius: 4,
              padding: '2px 10px',
              cursor: 'pointer',
            }}
          >
            close (Esc)
          </button>
        </div>
      </div>
    </Dialog>
  )
}
