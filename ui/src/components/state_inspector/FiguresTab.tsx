import { useEffect, useMemo, useState } from 'react'
import { artifactUrl } from '../../api/chatClient'
import { jsonFetch } from '../../api/_fetch'
import { FIGURES_POLL_MS } from '../../lib/polling'
import type { DAG } from '../../types'

interface Props {
  sessionId: string | null
  dag: DAG | null
}

interface FiguresManifestShape {
  stage_id: string
  written: Record<string, string>
  skipped: Record<string, string>
  errors: Record<string, string>
}

interface StageFigures {
  taskId: string
  manifest: FiguresManifestShape
}

/**
 * Normalize the four known manifest schemas the agent's R-script
 * renderers produce. Agents writing custom R scripts (fallback when
 * lib/plotting/stages/* can't run for lack of ggplot2/jsonlite in a
 * network=none container) emit one of:
 *
 *  1. `written: {id: "figures/<id>.png"}` — the canonical schema from
 *     `lib/plotting/core.py::FigureManifest`. Path is a string.
 *  2. `written: {id: {png: "figures/<id>.png", pdf: "..."}}` — value is
 *     an object with format-specific paths.
 *  3. `figures: {id: {figure_id, png, pdf, width_in, height_in}}` —
 *     `figures` key instead of `written`; richer per-figure metadata.
 *  4. `figures: [{figure_id, png, pdf, status}]` — array of figure
 *     objects keyed by an inner `figure_id` field. Common in the
 *     R-script renderer time_series_decompose ships; before this
 *     normalizer learned the shape, Object.entries treated array
 *     indices 0,1,2,... as figure_ids and the figures were invisible
 *     (mislabeled) in the UI.
 *
 * Without normalization the FiguresTab silently dropped tasks #2 and
 * #3 (Object.entries(written) yielded `[Object Object]` for the path,
 * or the manifest was filtered out by the `written != null` guard).
 */
function normalizeManifest(raw: unknown): FiguresManifestShape | null {
  if (!raw || typeof raw !== 'object') return null
  const m = raw as Record<string, unknown>
  const stage_id = typeof m.stage_id === 'string' ? m.stage_id : ''
  const skipped =
    m.skipped && typeof m.skipped === 'object'
      ? (m.skipped as Record<string, string>)
      : {}
  const errors =
    m.errors && typeof m.errors === 'object'
      ? (m.errors as Record<string, string>)
      : {}

  const extractPng = (val: unknown): string | null => {
    if (typeof val === 'string') return val
    if (val && typeof val === 'object') {
      const obj = val as Record<string, unknown>
      if (typeof obj.png === 'string') return obj.png
      // Last resort: pick any string value (covers `{path: "..."}` shapes)
      for (const v of Object.values(obj)) {
        if (typeof v === 'string' && /\.(png|jpg|jpeg|svg)$/i.test(v)) return v
      }
    }
    return null
  }

  // Schema 4: `figures: [{figure_id, png, pdf, status}]`. Project the
  // array into a dict keyed by the inner figure_id field, dropping
  // entries without a usable id. Done before the dict-shape path so
  // arrays don't get treated as Records (which would yield index keys
  // 0,1,2... instead of real figure_ids).
  if (Array.isArray(m.figures)) {
    const written: Record<string, string> = {}
    for (const entry of m.figures) {
      if (!entry || typeof entry !== 'object') continue
      const e = entry as Record<string, unknown>
      const id = typeof e.figure_id === 'string' ? e.figure_id : null
      if (!id) continue
      const path = extractPng(e)
      if (path) written[id] = path
    }
    if (Object.keys(written).length === 0) return null
    return { stage_id, written, skipped, errors }
  }

  const source =
    m.written && typeof m.written === 'object'
      ? (m.written as Record<string, unknown>)
      : m.figures && typeof m.figures === 'object'
        ? (m.figures as Record<string, unknown>)
        : null

  if (!source) return null
  const written: Record<string, string> = {}
  for (const [id, val] of Object.entries(source)) {
    const path = extractPng(val)
    if (path) written[id] = path
  }
  if (Object.keys(written).length === 0) return null
  return { stage_id, written, skipped, errors }
}

/**
 * Top-level Figures tab — cross-stage timeline gallery. For every task
 * in the DAG, attempt to poll its `figures/manifest.json`; tasks that
 * return a non-empty `written` map become sections in the gallery.
 * Clicking a thumbnail opens it in a new tab for a full-resolution
 * view.
 *
 * The component used to filter on the declarative
 * `task.spec.required_figures` field, but the v4 composer doesn't yet
 * populate that field for every plot-producing stage — figures rendered
 * on disk would never appear in the UI. Now the manifest itself is the
 * source of truth: if a stage wrote `figures/manifest.json` with at
 * least one entry, it shows up.
 *
 * Ordering follows DAG topological position (task order from
 * WORKFLOW.json is already dependency-sorted by the builder).
 */
export function FiguresPane({ sessionId, dag }: Props) {
  // Probe only tasks that could plausibly have written figures —
  // `running` / `completed` / `blocked` / `failed`. Tasks still in
  // `pending` / `ready` cannot have a `figures/manifest.json` on
  // disk yet, so polling them generates a 404 storm in the browser
  // console (one per task per cycle on long DAGs). The poll loop
  // below further filters to those whose manifest fetch succeeds
  // with at least one written figure. Tasks that transition into a
  // probeable state mid-session are picked up on the next cycle
  // because the memo re-derives whenever `dag` changes.
  const probedTaskIds = useMemo(() => {
    if (!dag) return [] as string[]
    return Object.entries(dag.tasks)
      .filter(([id, task]) => {
        if (!task) return false
        // discover_* and validate_* tasks never write figures by design
        // (the builder excludes them from required_figures). Skip them
        // to avoid the 404 storm seen on completed DAGs (one poll per
        // task per cycle).
        if (id.startsWith('discover_') || id.startsWith('validate_')) return false
        const status = task.state?.status
        return (
          status === 'running' ||
          status === 'completed' ||
          status === 'blocked' ||
          status === 'failed'
        )
      })
      .map(([id]) => id)
  }, [dag])

  const [byTask, setByTask] = useState<Record<string, FiguresManifestShape | null>>({})

  // Derive a stable string key from the task ids so the poll doesn't
  // re-arm on every render (the useMemo result is a fresh array
  // reference even when its contents are unchanged).
  const tasksKey = probedTaskIds.join(',')

  useEffect(() => {
    if (!sessionId || probedTaskIds.length === 0) return
    const controller = new AbortController()

    const fetchAll = async () => {
      const next: Record<string, FiguresManifestShape | null> = {}
      await Promise.all(
        probedTaskIds.map(async (tid) => {
          try {
            const url = artifactUrl(
              sessionId,
              `runtime/outputs/${encodeURIComponent(tid)}/figures/manifest.json`,
            )
            // 404 → null (task hasn't emitted figures), non-OK other
            // statuses also → null so we don't surface fetch errors
            // as broken thumbnails. The artifact endpoint returns 404
            // for missing files; jsonFetch surfaces that as a throw
            // which the catch below normalizes.
            const raw = await jsonFetch<unknown>(url, {
              signal: controller.signal,
            })
            next[tid] = normalizeManifest(raw)
          } catch {
            next[tid] = null
          }
        }),
      )
      if (!controller.signal.aborted) setByTask(next)
    }
    void fetchAll()
    // Gate polling on tab visibility so we don't burn quota on a
    // backgrounded tab.
    const tick = () => {
      if (document.visibilityState !== 'visible') return
      void fetchAll()
    }
    const id = window.setInterval(tick, FIGURES_POLL_MS)
    return () => {
      controller.abort()
      window.clearInterval(id)
    }
    // tasksKey is the stable hash of probedTaskIds; depending on the
    // array itself would re-arm on every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId, tasksKey])

  const sections: StageFigures[] = probedTaskIds
    .map((tid) => ({ taskId: tid, manifest: byTask[tid] }))
    .filter(
      (x): x is StageFigures =>
        x.manifest !== null &&
        x.manifest !== undefined &&
        // Defensive: agents have been known to write manifest.json with
        // a non-canonical shape (e.g. `figures: [...]` instead of the
        // expected `written: {...}` map). When `written` is missing or
        // null, treat the manifest as empty rather than crashing the
        // whole StateInspectorPane via Object.keys(null).
        x.manifest.written != null &&
        typeof x.manifest.written === 'object' &&
        Object.keys(x.manifest.written).length > 0,
    )

  if (!sessionId) {
    return <EmptyState text="Start a session to see figures." />
  }
  if (probedTaskIds.length === 0) {
    return <EmptyState text="No workflow tasks yet — figures appear as stages complete." />
  }
  if (sections.length === 0) {
    // Determine whether we're still probing or have confirmed-no
    // results. While Object.keys(byTask).length < probedTaskIds.length
    // the poll is still in flight.
    const stillProbing = Object.keys(byTask).length < probedTaskIds.length
    return (
      <EmptyState
        text={
          stillProbing
            ? `Probing ${probedTaskIds.length} task${probedTaskIds.length === 1 ? '' : 's'} for figures…`
            : 'No figures produced yet — they appear here as compute stages complete.'
        }
      />
    )
  }

  return (
    <div
      data-testid="state-figures-pane"
      style={{
        flex: 1,
        overflowY: 'auto',
        padding: '0.75rem 0.9rem',
        display: 'flex',
        flexDirection: 'column',
        gap: '1rem',
      }}
    >
      {sections.map(({ taskId, manifest }) => (
        <StageSection
          key={taskId}
          taskId={taskId}
          manifest={manifest}
          sessionId={sessionId}
        />
      ))}
    </div>
  )
}

function StageSection({
  taskId,
  manifest,
  sessionId,
}: {
  taskId: string
  manifest: FiguresManifestShape
  sessionId: string
}) {
  const entries = Object.entries(manifest.written)
  return (
    <section
      aria-label={`Figures for ${taskId}`}
      data-stage-id={taskId}
      style={{
        background: 'var(--color-surface-1)',
        border: '1px solid var(--color-border-default)',
        borderRadius: 6,
        padding: '0.6rem 0.75rem',
      }}
    >
      <header
        style={{
          display: 'flex',
          justifyContent: 'space-between',
          alignItems: 'baseline',
          marginBottom: '0.45rem',
        }}
      >
        <strong
          style={{
            fontFamily: 'ui-monospace, monospace',
            fontSize: '0.85rem',
            color: 'var(--color-text-primary)',
          }}
        >
          {taskId}
        </strong>
        <span style={{ fontSize: '0.7rem', color: 'var(--color-text-muted)' }}>
          {entries.length} figure{entries.length === 1 ? '' : 's'}
        </span>
      </header>
      <div
        style={{
          display: 'grid',
          gridTemplateColumns: 'repeat(auto-fill, minmax(150px, 1fr))',
          gap: '0.45rem',
        }}
      >
        {entries.map(([id, absPath]) => {
          const url = deriveFigureUrl(sessionId, taskId, id, absPath)
          return (
            <a
              key={id}
              href={url}
              target="_blank"
              rel="noreferrer"
              title={id}
              data-figure-id={id}
              style={{
                display: 'block',
                background: 'var(--color-surface-0)',
                border: '1px solid var(--color-border-strong)',
                borderRadius: 4,
                overflow: 'hidden',
              }}
            >
              <img
                src={url}
                alt={id}
                loading="lazy"
                style={{
                  width: '100%',
                  height: 110,
                  objectFit: 'contain',
                  display: 'block',
                  background: '#ffffff',
                }}
              />
              <div
                style={{
                  fontSize: '0.68rem',
                  fontFamily: 'ui-monospace, monospace',
                  padding: '2px 4px',
                  color: 'var(--color-text-secondary)',
                  borderTop: '1px solid var(--color-border-strong)',
                  background: 'var(--color-surface-2)',
                  whiteSpace: 'nowrap',
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                }}
              >
                {id}
              </div>
            </a>
          )
        })}
      </div>
    </section>
  )
}

function EmptyState({ text }: { text: string }) {
  return (
    <div
      data-testid="state-figures-pane"
      style={{
        padding: '1.5rem',
        color: 'var(--color-text-muted)',
        fontSize: '0.85rem',
        fontStyle: 'italic',
      }}
    >
      {text}
    </div>
  )
}

function deriveFigureUrl(
  sessionId: string,
  taskId: string,
  figureId: string,
  absPath: string,
): string {
  const marker = '/runtime/'
  const i = absPath.indexOf(marker)
  if (i >= 0) {
    const relative = absPath.slice(i + 1)
    return artifactUrl(sessionId, relative)
  }
  return artifactUrl(
    sessionId,
    `runtime/outputs/${encodeURIComponent(taskId)}/figures/${encodeURIComponent(figureId)}.png`,
  )
}
