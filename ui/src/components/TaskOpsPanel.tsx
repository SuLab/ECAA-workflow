/**
 * TaskOpsPanel — surfaces per-task operational artifacts that aren't
 * tracked by WORKFLOW.json itself. Designed for the SME's "what just
 * happened?" loop when a task looks busy but isn't transitioning.
 *
 * Three sections:
 *  1. **Status sentinels** — `*.OK` / `*.FAILED` / `*.PENDING` files
 *  written by long-running wrapper scripts (e.g. Seurat install,
 *  integration runs). Coloured chips with mtime + body excerpt.
 *  2. **Logs** — every `.log`/`.jsonl`/`.txt` under the task's output
 *  dir. Click to tail in a 2 s polling viewer.
 *  3. **Scripts** — agent-generated `.R`/`.py`/`.sh` under the
 *  `scripts/` subdir. Click to view read-only.
 *
 * All endpoints are $HOME/package-jailed server-side. The tail viewer
 * shares the polling cadence with the main progress.log tab so a
 * moderately busy task stays under one HTTP/s of polling pressure.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  artifactUrl,
  getTaskLogTail,
  getTaskStatusSentinels,
  listTaskLogs,
  listTaskScripts,
  type ProgressLogResponse,
  type StatusSentinel,
  type TaskFileEntry,
} from '../api/chatClient'
import { LOG_POLL_MS, TASK_OPS_POLL_MS } from '../lib/polling'
import { relativeTime } from '../lib/time'

interface Props {
  sessionId: string
  taskId: string
  /** When `running`, sentinels + logs poll every 30 s for fresh output.
   *  Otherwise we fetch once on mount. */
  isRunning: boolean
}

const SENTINEL_COLOR: Record<string, { bg: string; fg: string; label: string }> = {
  ok: {
    bg: 'var(--color-success-bg)',
    fg: 'var(--color-success-fg)',
    label: 'OK',
  },
  failed: {
    bg: 'var(--color-danger-bg)',
    fg: 'var(--color-danger-fg)',
    label: 'FAILED',
  },
  pending: {
    bg: 'var(--color-warning-bg)',
    fg: 'var(--color-warning-fg)',
    label: 'PENDING',
  },
  status_file: {
    bg: 'var(--color-info-bg)',
    fg: 'var(--color-info-fg, #1d4ed8)',
    label: 'STATUS',
  },
}

export default function TaskOpsPanel({ sessionId, taskId, isRunning }: Props): JSX.Element {
  const [sentinels, setSentinels] = useState<StatusSentinel[]>([])
  const [logFiles, setLogFiles] = useState<TaskFileEntry[]>([])
  const [scriptFiles, setScriptFiles] = useState<TaskFileEntry[]>([])
  const [selectedLog, setSelectedLog] = useState<string | null>(null)
  const [selectedScript, setSelectedScript] = useState<string | null>(null)

  const refresh = useCallback(async () => {
    try {
      const [s, l, sc] = await Promise.all([
        getTaskStatusSentinels(sessionId, taskId),
        listTaskLogs(sessionId, taskId),
        listTaskScripts(sessionId, taskId),
      ])
      setSentinels(s.sentinels)
      setLogFiles(l.files)
      setScriptFiles(sc.files)
    } catch {
      /* non-fatal — partial render */
    }
  }, [sessionId, taskId])

  useEffect(() => {
    void refresh()
    if (!isRunning) return
    const id = setInterval(() => void refresh(), TASK_OPS_POLL_MS)
    return () => clearInterval(id)
  }, [refresh, isRunning])

  const hasAnything =
    sentinels.length > 0 || logFiles.length > 0 || scriptFiles.length > 0

  if (!hasAnything) return <></>

  return (
    <section
      data-testid="task-ops-panel"
      aria-label="Task operations"
      style={{
        marginTop: '0.6rem',
        padding: '0.7rem 0.85rem',
        background: 'var(--color-surface-1)',
        border: '1px solid var(--color-border-default)',
        borderRadius: 6,
        display: 'flex',
        flexDirection: 'column',
        gap: '0.7rem',
      }}
    >
      {sentinels.length > 0 && <SentinelChips sentinels={sentinels} />}
      {logFiles.length > 0 && (
        <FileList
          title="Logs"
          files={logFiles}
          selected={selectedLog}
          onSelect={(p) => {
            setSelectedScript(null)
            setSelectedLog(p)
          }}
        />
      )}
      {selectedLog && (
        <LogTail
          sessionId={sessionId}
          taskId={taskId}
          relPath={selectedLog}
          isRunning={isRunning}
          onClose={() => setSelectedLog(null)}
        />
      )}
      {scriptFiles.length > 0 && (
        <FileList
          title="Scripts"
          files={scriptFiles}
          selected={selectedScript}
          onSelect={(p) => {
            setSelectedLog(null)
            setSelectedScript(p)
          }}
        />
      )}
      {selectedScript && (
        <ScriptViewer
          sessionId={sessionId}
          relPath={selectedScript}
          taskId={taskId}
          onClose={() => setSelectedScript(null)}
        />
      )}
    </section>
  )
}

function SentinelChips({ sentinels }: { sentinels: StatusSentinel[] }): JSX.Element {
  return (
    <div
      data-testid="status-sentinel-chips"
      style={{
        display: 'flex',
        flexWrap: 'wrap',
        gap: '0.4rem',
        alignItems: 'center',
      }}
    >
      <span
        style={{
          fontSize: '0.74rem',
          color: 'var(--color-text-secondary)',
          fontWeight: 500,
        }}
      >
        Wrapper status:
      </span>
      {sentinels.map((s) => {
        const palette = SENTINEL_COLOR[s.kind] ?? SENTINEL_COLOR.status_file
        const tooltip = s.body
          ? `${s.name}\n\n${s.body}`
          : `${s.name} — written ${relativeTime(new Date(s.mtime_unix * 1000).toISOString())}`
        return (
          <span
            key={s.name}
            data-sentinel-kind={s.kind}
            title={tooltip}
            style={{
              display: 'inline-flex',
              alignItems: 'center',
              gap: '0.3rem',
              padding: '2px 8px',
              borderRadius: 11,
              background: palette!.bg,
              color: palette!.fg,
              fontSize: '0.7rem',
              fontWeight: 600,
              lineHeight: 1.4,
            }}
          >
            <span>{palette!.label}</span>
            <span style={{ fontWeight: 400, opacity: 0.85 }}>{s.name}</span>
            <span
              style={{
                fontWeight: 400,
                opacity: 0.7,
                fontSize: '0.65rem',
                fontFamily: 'ui-monospace, monospace',
              }}
            >
              {relativeTime(new Date(s.mtime_unix * 1000).toISOString())}
            </span>
          </span>
        )
      })}
    </div>
  )
}

function FileList({
  title,
  files,
  selected,
  onSelect,
}: {
  title: string
  files: TaskFileEntry[]
  selected: string | null
  onSelect: (relPath: string) => void
}): JSX.Element {
  return (
    <div>
      <div
        style={{
          fontSize: '0.74rem',
          color: 'var(--color-text-secondary)',
          fontWeight: 600,
          marginBottom: '0.25rem',
        }}
      >
        {title}
      </div>
      <ul
        style={{
          listStyle: 'none',
          padding: 0,
          margin: 0,
          display: 'flex',
          flexDirection: 'column',
          gap: 2,
        }}
      >
        {files.map((f) => {
          const isSelected = f.rel_path === selected
          return (
            <li key={f.rel_path}>
              <button
                type="button"
                onClick={() => onSelect(f.rel_path)}
                style={{
                  width: '100%',
                  textAlign: 'left',
                  padding: '4px 8px',
                  background: isSelected
                    ? 'var(--color-info-bg, #eff6ff)'
                    : 'transparent',
                  border: '1px solid',
                  borderColor: isSelected
                    ? 'var(--color-info-border, #bfdbfe)'
                    : 'transparent',
                  borderRadius: 3,
                  cursor: 'pointer',
                  display: 'flex',
                  gap: '0.5rem',
                  alignItems: 'baseline',
                  fontSize: '0.74rem',
                  color: 'var(--color-text-primary)',
                  fontFamily: 'ui-monospace, monospace',
                }}
              >
                <span style={{ flex: '1 1 auto' }}>{f.rel_path}</span>
                <span
                  style={{
                    flex: '0 0 auto',
                    color: 'var(--color-text-muted)',
                    fontSize: '0.66rem',
                  }}
                >
                  {formatBytes(f.size_bytes)} · {relativeTime(new Date(f.mtime_unix * 1000).toISOString())}
                </span>
              </button>
            </li>
          )
        })}
      </ul>
    </div>
  )
}

function LogTail({
  sessionId,
  taskId,
  relPath,
  isRunning,
  onClose,
}: {
  sessionId: string
  taskId: string
  relPath: string
  isRunning: boolean
  onClose: () => void
}): JSX.Element {
  const [lines, setLines] = useState<string[]>([])
  const [truncated, setTruncated] = useState(false)
  const sinceRef = useRef(0)
  const stickToBottom = useRef(true)
  const containerRef = useRef<HTMLDivElement | null>(null)

  // Reset state on path change.
  useEffect(() => {
    setLines([])
    setTruncated(false)
    sinceRef.current = 0
  }, [relPath])

  useEffect(() => {
    let alive = true
    const tick = async () => {
      try {
        const res: ProgressLogResponse = await getTaskLogTail(
          sessionId,
          taskId,
          relPath,
          sinceRef.current,
        )
        if (!alive) return
        if (res.lines.length > 0) {
          // P1-159: cap the accumulator at 5000 lines so a long-running
          // task that emits 200 lines/minute over 12 h doesn't grow the
          // component-state buffer to 144 k strings (and the tab DOM
          // along with it). When the cap clips, the visible tail still
          // shows the most-recent activity — the byte-cap on the
          // server side already drops old lines off the front of the
          // file, and the UI mirrors that contract.
          setLines((prev) => {
            const next = [...prev, ...res.lines]
            const MAX_LINES = 5000
            return next.length > MAX_LINES ? next.slice(-MAX_LINES) : next
          })
        }
        sinceRef.current = res.next_since_line
        setTruncated(res.truncated)
      } catch {
        /* non-fatal */
      }
    }
    void tick()
    if (!isRunning) return
    const id = setInterval(() => void tick(), LOG_POLL_MS)
    return () => {
      alive = false
      clearInterval(id)
    }
  }, [sessionId, taskId, relPath, isRunning])

  useEffect(() => {
    const el = containerRef.current
    if (!el || !stickToBottom.current) return
    el.scrollTop = el.scrollHeight
  }, [lines])

  return (
    <div
      style={{
        border: '1px solid var(--color-border-default)',
        borderRadius: 4,
        background: 'var(--color-surface-2, #0b1020)',
        color: 'var(--color-text-on-dark, #d1d5db)',
      }}
    >
      <div
        style={{
          padding: '4px 8px',
          display: 'flex',
          gap: '0.5rem',
          alignItems: 'center',
          background: 'var(--color-surface-3, #111827)',
          borderBottom: '1px solid var(--color-border-default)',
          fontSize: '0.72rem',
        }}
      >
        <span
          style={{
            fontFamily: 'ui-monospace, monospace',
            color: 'var(--color-text-on-dark, #d1d5db)',
          }}
        >
          {relPath}
        </span>
        {truncated && (
          <span
            style={{
              fontSize: '0.66rem',
              color: 'var(--color-warning-accent)',
              fontWeight: 600,
            }}
          >
            (recent lines only)
          </span>
        )}
        <button
          type="button"
          onClick={onClose}
          style={{
            marginLeft: 'auto',
            background: 'transparent',
            border: 'none',
            color: 'var(--color-text-on-dark, #d1d5db)',
            cursor: 'pointer',
            fontSize: '0.74rem',
          }}
        >
          Close
        </button>
      </div>
      <div
        ref={containerRef}
        onScroll={(e) => {
          const el = e.currentTarget
          stickToBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40
        }}
        style={{
          maxHeight: '300px',
          overflowY: 'auto',
          padding: '0.4rem 0.6rem',
          fontFamily: 'ui-monospace, monospace',
          fontSize: '0.7rem',
          lineHeight: 1.45,
          whiteSpace: 'pre-wrap',
        }}
      >
        {lines.length === 0
          ? 'no output yet…'
          : lines.map((l, i) => <div key={i}>{l}</div>)}
      </div>
    </div>
  )
}

function ScriptViewer({
  sessionId,
  relPath,
  taskId,
  onClose,
}: {
  sessionId: string
  relPath: string
  taskId: string
  onClose: () => void
}): JSX.Element {
  const [body, setBody] = useState<string | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const url = useMemo(
    () => artifactUrl(sessionId, `runtime/outputs/${taskId}/scripts/${relPath}`),
    [sessionId, taskId, relPath],
  )
  useEffect(() => {
    let alive = true
    setBody(null)
    setErr(null)
    fetch(url) // allow-bare-fetch: artifactUrl carries share-token
      .then(async (r) => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`)
        const text = await r.text()
        if (alive) setBody(text)
      })
      .catch((e) => {
        if (alive) setErr((e as Error).message)
      })
    return () => {
      alive = false
    }
  }, [url])
  return (
    <div
      style={{
        border: '1px solid var(--color-border-default)',
        borderRadius: 4,
        background: 'var(--color-surface-2, #0b1020)',
        color: 'var(--color-text-on-dark, #d1d5db)',
      }}
    >
      <div
        style={{
          padding: '4px 8px',
          display: 'flex',
          gap: '0.5rem',
          alignItems: 'center',
          background: 'var(--color-surface-3, #111827)',
          borderBottom: '1px solid var(--color-border-default)',
          fontSize: '0.72rem',
        }}
      >
        <span style={{ fontFamily: 'ui-monospace, monospace' }}>scripts/{relPath}</span>
        <button
          type="button"
          onClick={onClose}
          style={{
            marginLeft: 'auto',
            background: 'transparent',
            border: 'none',
            color: 'var(--color-text-on-dark, #d1d5db)',
            cursor: 'pointer',
            fontSize: '0.74rem',
          }}
        >
          Close
        </button>
      </div>
      <pre
        style={{
          margin: 0,
          maxHeight: '420px',
          overflow: 'auto',
          padding: '0.5rem 0.7rem',
          fontFamily: 'ui-monospace, monospace',
          fontSize: '0.7rem',
          lineHeight: 1.45,
          whiteSpace: 'pre',
        }}
      >
        {err ? `error loading script: ${err}` : (body ?? 'loading…')}
      </pre>
    </div>
  )
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  return `${(n / 1024 / 1024).toFixed(1)} MB`
}
