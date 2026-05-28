// Interactive dashboard. Fetches /api/chat/session/:id/dashboard/index
// to learn which stages/views have data, renders a stage-chip selector,
// and dispatches to a per-view-id React component that knows how to
// render the payload. Native SVG + React state for zoom/pan so the
// initial bundle stays small (no Plotly dependency at P3 foundation).

import { useEffect, useMemo, useState } from 'react'
import { useCancelableEffect } from '../../hooks/useCancelableFetch'
import {
  getDashboardIndex,
  type DashboardIndexWire as DashboardIndex,
  type DashboardViewWire as DashboardView,
} from '../../api/chatClient'
import { jsonFetch } from '../../api/_fetch'
import { DASHBOARD_INDEX_POLL_MS } from '../../lib/polling'
import { formatInteger } from '../../lib/format'
import {
  SCATTER_PLOT_WIDTH, SCATTER_PLOT_HEIGHT,
  SCATTER_AXIS_PADDING_X, SCATTER_AXIS_PADDING_Y,
  SCATTER_ZOOM_MIN, SCATTER_ZOOM_MAX,
  SCATTER_WHEEL_ZOOM_FACTOR,
  COORD_EPSILON,
  SCATTER_POINT_RADIUS_DIVISOR,
  SCATTER_FILL_OPACITY_BG, SCATTER_FILL_OPACITY_FG,
  MAX_CATEGORICAL_LEGEND_ENTRIES,
} from '../../lib/dashboardConstants'
import DashboardSummaryCard from '../DashboardSummaryCard'

interface Props {
  sessionId: string | null
}

export function DashboardPane({ sessionId }: Props) {
  const [index, setIndex] = useState<DashboardIndex | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [selectedStage, setSelectedStage] = useState<string | null>(null)
  const [selectedView, setSelectedView] = useState<string | null>(null)
  // Cross-stage brush — a category name (cluster/celltype/etc.) that
  // every ScatterView highlights when rendering its own categorical
  // coloring. Kept here so switching between stages keeps the brush
  // context; cleared with the "Clear brush" button on the active
  // scatter.
  const [brush, setBrush] = useState<string | null>(null)

  // Poll the index so newly-completed stages surface without a manual reload.
  // Gated on document.visibilityState so a backgrounded tab doesn't poll.
  useEffect(() => {
    if (!sessionId) return
    let cancelled = false
    const fetchOnce = async () => {
      try {
        const body = await getDashboardIndex(sessionId)
        if (!cancelled) {
          setIndex(body)
          setError(null)
        }
      } catch (e) {
        if (!cancelled) setError((e as Error).message)
      }
    }
    void fetchOnce()
    const tick = () => {
      if (document.visibilityState !== 'visible') return
      void fetchOnce()
    }
    const id = window.setInterval(tick, DASHBOARD_INDEX_POLL_MS)
    return () => {
      cancelled = true
      window.clearInterval(id)
    }
  }, [sessionId])

  // Auto-select the first stage + view once the index arrives.
  useEffect(() => {
    if (!index || index.stages.length === 0) return
    if (!selectedStage || !index.stages.some((s) => s.stage_id === selectedStage)) {
      const first = index.stages[0]
      if (!first) return
      setSelectedStage(first.stage_id)
      setSelectedView(first.views[0]?.view_id ?? null)
    }
  }, [index, selectedStage])

  const activeStage = useMemo(
    () => index?.stages.find((s) => s.stage_id === selectedStage) ?? null,
    [index, selectedStage],
  )
  const activeView = useMemo(
    () => activeStage?.views.find((v) => v.view_id === selectedView) ?? null,
    [activeStage, selectedView],
  )

  if (!sessionId) return <EmptyState text="Start a session to see dashboard views." />

  return (
    <div
      data-testid="state-dashboard-pane"
      style={{
        flex: 1,
        minHeight: 0,
        display: 'flex',
        flexDirection: 'column',
        background: 'var(--color-surface-0)',
      }}
    >
      <DashboardSummaryCard sessionId={sessionId} />
      <div
        style={{
          padding: '0.5rem 0.75rem',
          borderBottom: '1px solid var(--color-border-default)',
          background: 'var(--color-surface-1)',
          flexShrink: 0,
        }}
      >
        {error && (
          <div style={{ color: 'var(--color-danger-fg)', fontSize: '0.78rem', marginBottom: 4 }}>
            {error}
          </div>
        )}
        {!index ? (
          <div style={{ fontSize: '0.82rem', color: 'var(--color-text-muted)', fontStyle: 'italic' }}>
            Loading dashboard index…
          </div>
        ) : index.stages.length === 0 ? (
          <div style={{ fontSize: '0.82rem', color: 'var(--color-text-muted)', fontStyle: 'italic' }}>
            No completed stages have interactive views yet. View data lands as
            compute stages finish.
          </div>
        ) : (
          <>
            <div
              role="tablist"
              aria-label="Dashboard stages"
              style={{
                display: 'flex',
                flexWrap: 'wrap',
                gap: '0.3rem',
                marginBottom: 6,
              }}
            >
              {index.stages.map((s) => (
                <button
                  key={s.stage_id}
                  type="button"
                  role="tab"
                  aria-selected={s.stage_id === selectedStage}
                  data-stage-id={s.stage_id}
                  onClick={() => {
                    setSelectedStage(s.stage_id)
                    setSelectedView(s.views[0]?.view_id ?? null)
                  }}
                  style={{
                    padding: '0.25rem 0.6rem',
                    fontSize: '0.72rem',
                    background:
                      s.stage_id === selectedStage
                        ? 'var(--color-accent)'
                        : 'var(--color-surface-3)',
                    color:
                      s.stage_id === selectedStage
                        ? 'var(--color-accent-fg)'
                        : 'var(--color-text-secondary)',
                    border: 'none',
                    borderRadius: 4,
                    cursor: 'pointer',
                    fontFamily: 'ui-monospace, monospace',
                  }}
                >
                  {s.stage_id}
                </button>
              ))}
            </div>
            {activeStage && activeStage.views.length > 1 && (
              <div
                role="radiogroup"
                aria-label="Dashboard views"
                style={{
                  display: 'flex',
                  gap: '0.3rem',
                  flexWrap: 'wrap',
                }}
              >
                {activeStage.views.map((v) => (
                  <button
                    key={v.view_id}
                    type="button"
                    role="radio"
                    aria-checked={v.view_id === selectedView}
                    data-view-id={v.view_id}
                    onClick={() => setSelectedView(v.view_id)}
                    style={{
                      padding: '0.2rem 0.55rem',
                      fontSize: '0.68rem',
                      background:
                        v.view_id === selectedView
                          ? 'var(--color-button-primary-bg)'
                          : 'transparent',
                      color:
                        v.view_id === selectedView
                          ? 'var(--color-button-primary-fg)'
                          : 'var(--color-text-primary)',
                      border: '1px solid var(--color-border-strong)',
                      borderRadius: 4,
                      cursor: 'pointer',
                    }}
                  >
                    {v.view_id}
                  </button>
                ))}
              </div>
            )}
          </>
        )}
      </div>
      <div
        style={{
          flex: 1,
          minHeight: 0,
          overflow: 'auto',
          padding: '0.75rem',
        }}
      >
        {activeView ? (
          <ViewPanel
            key={`${activeStage?.stage_id}/${activeView.view_id}`}
            view={activeView}
            stageId={activeStage!.stage_id}
            brush={brush}
            onBrush={setBrush}
          />
        ) : (
          <div style={{ color: 'var(--color-text-faint)', fontSize: '0.8rem' }}>
            Select a stage + view to render.
          </div>
        )}
      </div>
    </div>
  )
}

function EmptyState({ text }: { text: string }) {
  return (
    <div
      data-testid="state-dashboard-pane"
      style={{
        padding: '1.5rem',
        color: 'var(--color-text-muted)',
        fontStyle: 'italic',
        fontSize: '0.85rem',
      }}
    >
      {text}
    </div>
  )
}

function ViewPanel({
  view,
  stageId,
  brush,
  onBrush,
}: {
  view: DashboardView
  stageId: string
  brush: string | null
  onBrush: (value: string | null) => void
}) {
  const [payload, setPayload] = useState<unknown>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  useCancelableEffect(async ({ signal, cancelled }) => {
    setLoading(true)
    setError(null)
    try {
      const body = await jsonFetch<unknown>(view.data_url, { signal })
      if (!cancelled()) {
        setPayload(body)
        setLoading(false)
      }
    } catch (e) {
      if (!cancelled()) {
        setError(e instanceof Error ? e.message : String(e))
        setLoading(false)
      }
    }
  }, [view.data_url])

  if (loading)
    return (
      <div style={{ fontSize: '0.82rem', color: 'var(--color-text-muted)' }}>Loading view data…</div>
    )
  if (error) return <div style={{ color: 'var(--color-danger-fg)' }}>{error}</div>
  if (!payload) return null

  const typed = payload as {
    data?: { runs?: ScatterRun[]; comparisons?: VolcanoComparison[] }
  }
  const data = typed.data ?? {}

  // Dispatch on view_id. Unknown views fall back to a JSON dump so the
  // SME sees the raw payload instead of a blank panel.
  if (
    view.view_id === 'embedding_scatter' ||
    view.view_id === 'umap_by_cluster' ||
    view.view_id === 'umap_by_celltype' ||
    view.view_id === 'pseudotime_scatter' ||
    view.view_id === 'mean_variance'
  ) {
    return (
      <ScatterView
        stageId={stageId}
        viewId={view.view_id}
        runs={data.runs ?? []}
        brush={brush}
        onBrush={onBrush}
      />
    )
  }
  if (view.view_id === 'volcano') {
    return <VolcanoView comparisons={data.comparisons ?? []} />
  }
  return <RawJsonView payload={payload} />
}

interface ScatterRun {
  id: string
  n_points: number
  n_total: number
  x: number[]
  y: number[]
  cluster?: string[]
}

function ScatterView({
  stageId,
  viewId,
  runs,
  brush,
  onBrush,
}: {
  stageId: string
  viewId: string
  runs: ScatterRun[]
  brush: string | null
  onBrush: (value: string | null) => void
}) {
  const [activeRun, setActiveRun] = useState<string>(runs[0]?.id ?? '')
  useEffect(() => {
    if (runs.length && !runs.some((r) => r.id === activeRun)) {
      setActiveRun(runs[0]?.id ?? '')
    }
  }, [runs, activeRun])
  const run = runs.find((r) => r.id === activeRun)
  if (!run)
    return (
      <div style={{ fontSize: '0.82rem', color: 'var(--color-text-muted)' }}>No runs to display.</div>
    )

  return (
    <div data-testid="dashboard-scatter-view" data-stage-id={stageId} data-view-id={viewId}>
      {runs.length > 1 && (
        <div style={{ marginBottom: 8, display: 'flex', gap: '0.25rem' }}>
          {runs.map((r) => (
            <button
              key={r.id}
              type="button"
              onClick={() => setActiveRun(r.id)}
              aria-pressed={r.id === activeRun}
              style={{
                padding: '0.15rem 0.5rem',
                fontSize: '0.68rem',
                background:
                  r.id === activeRun
                    ? 'var(--color-button-primary-bg)'
                    : 'var(--color-surface-3)',
                color:
                  r.id === activeRun
                    ? 'var(--color-button-primary-fg)'
                    : 'var(--color-text-primary)',
                border: 'none',
                borderRadius: 4,
                cursor: 'pointer',
              }}
            >
              {r.id}
            </button>
          ))}
        </div>
      )}
      <div style={{ fontSize: '0.72rem', color: 'var(--color-text-muted)', marginBottom: 4 }}>
        {formatInteger(run.n_points)} of {formatInteger(run.n_total)} points
        {run.n_points < run.n_total ? ' (subsampled)' : ''}
      </div>
      <ScatterSvg
        xs={run.x}
        ys={run.y}
        categories={run.cluster}
        brush={brush}
        onBrush={onBrush}
      />
    </div>
  )
}

function ScatterSvg({
  xs,
  ys,
  categories,
  brush,
  onBrush,
}: {
  xs: number[]
  ys: number[]
  categories?: string[]
  brush?: string | null
  onBrush?: (value: string | null) => void
}) {
  const W = SCATTER_PLOT_WIDTH
  const H = SCATTER_PLOT_HEIGHT
  const pad = SCATTER_AXIS_PADDING_X
  // Zoom-pan state: viewport offset + scale applied via a nested <g>.
  const [viewport, setViewport] = useState({ tx: 0, ty: 0, scale: 1 })
  const [dragState, setDragState] = useState<{
    startX: number
    startY: number
    origTx: number
    origTy: number
  } | null>(null)
  const [hovered, setHovered] = useState<number | null>(null)
  if (xs.length === 0)
    return (
      <div
        style={{
          padding: '1rem',
          color: 'var(--color-text-muted)',
          fontSize: '0.85rem',
          fontStyle: 'italic',
          textAlign: 'center',
        }}
      >
        No points available for this figure yet.
      </div>
    )
  const minX = Math.min(...xs)
  const maxX = Math.max(...xs)
  const minY = Math.min(...ys)
  const maxY = Math.max(...ys)
  const sx = (x: number) =>
    pad + ((x - minX) / Math.max(maxX - minX, COORD_EPSILON)) * (W - 2 * pad)
  const sy = (y: number) =>
    H - pad - ((y - minY) / Math.max(maxY - minY, COORD_EPSILON)) * (H - 2 * pad)
  // Categorical palette for scatter points. Consumed as raw hex strings
  // because SVG presentation attributes (fill="…") don't resolve
  // var(--…) like inline CSS does. Kept in sync with the light variant
  // of the `--color-chart-*` tokens in tokens.css; dark-mode plots use
  // the same hex values for now.
  const palette = [
    '#1f77b4', '#ff7f0e', '#2ca02c', '#d62728', '#9467bd', '#8c564b',
    '#e377c2', '#7f7f7f', '#bcbd22', '#17becf', '#a6cee3', '#fb9a99',
  ]
  const uniques = categories
    ? Array.from(new Set(categories)).sort()
    : null
  const colorFor = (i: number): string => {
    if (!categories || !uniques) return '#334155'
    const idx = uniques.indexOf(categories[i] ?? '')
    return palette[idx % palette.length] ?? '#334155'
  }
  const handleWheel = (e: React.WheelEvent<SVGSVGElement>) => {
    e.preventDefault()
    const delta = -e.deltaY * SCATTER_WHEEL_ZOOM_FACTOR
    const next = Math.max(SCATTER_ZOOM_MIN, Math.min(SCATTER_ZOOM_MAX, viewport.scale * (1 + delta)))
    setViewport((v) => ({ ...v, scale: next }))
  }
  const handleMouseDown = (e: React.MouseEvent<SVGSVGElement>) => {
    setDragState({
      startX: e.clientX,
      startY: e.clientY,
      origTx: viewport.tx,
      origTy: viewport.ty,
    })
  }
  const handleMouseMove = (e: React.MouseEvent<SVGSVGElement>) => {
    if (!dragState) return
    setViewport((v) => ({
      ...v,
      tx: dragState.origTx + (e.clientX - dragState.startX),
      ty: dragState.origTy + (e.clientY - dragState.startY),
    }))
  }
  const handleMouseUp = () => setDragState(null)
  const resetView = () => setViewport({ tx: 0, ty: 0, scale: 1 })
  const radius = SCATTER_POINT_RADIUS_DIVISOR / viewport.scale
  return (
    <div>
      <div
        style={{
          display: 'flex',
          justifyContent: 'space-between',
          alignItems: 'center',
          marginBottom: 4,
        }}
      >
        <div style={{ fontSize: '0.68rem', color: 'var(--color-text-muted)' }}>
          scroll-to-zoom · drag-to-pan
          {hovered !== null && categories ? (
            <span style={{ marginLeft: 8, color: 'var(--color-text-primary)' }}>
              → {categories[hovered]}
            </span>
          ) : null}
        </div>
        <div style={{ display: 'flex', gap: 6 }}>
          {brush && onBrush && (
            <button
              type="button"
              onClick={() => onBrush(null)}
              style={{
                fontSize: '0.7rem',
                background: 'var(--color-warning-bg)',
                border: '1px solid var(--color-warning-border)',
                color: 'var(--color-warning-fg)',
                borderRadius: 4,
                padding: '1px 8px',
                cursor: 'pointer',
              }}
            >
              Clear brush: {brush}
            </button>
          )}
          <button
            type="button"
            onClick={resetView}
            disabled={viewport.scale === 1 && viewport.tx === 0 && viewport.ty === 0}
            style={{
              fontSize: '0.7rem',
              background: 'var(--color-surface-2)',
              border: '1px solid var(--color-border-strong)',
              color: 'var(--color-text-primary)',
              borderRadius: 4,
              padding: '1px 8px',
              cursor: 'pointer',
            }}
          >
            Reset view
          </button>
        </div>
      </div>
      <svg
        role="img"
        aria-label="Scatter plot"
        viewBox={`0 0 ${W} ${H}`}
        onWheel={handleWheel}
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
        onMouseLeave={handleMouseUp}
        data-testid="scatter-svg"
        style={{
          width: '100%',
          background: 'var(--color-surface-1)',
          border: '1px solid var(--color-border-strong)',
          borderRadius: 4,
          maxHeight: 520,
          cursor: dragState ? 'grabbing' : 'grab',
          userSelect: 'none',
        }}
      >
        <rect
          x={pad}
          y={pad}
          width={W - 2 * pad}
          height={H - 2 * pad}
          fill="transparent"
          stroke="currentColor"
          strokeOpacity={0.15}
        />
        <g
          transform={`translate(${viewport.tx},${viewport.ty}) scale(${viewport.scale})`}
        >
          {xs.map((x, i) => {
            const category = categories?.[i] ?? ''
            const isBrushed = brush && category === brush
            const isDimmed = brush && !isBrushed
            const r = isBrushed ? radius * 2.2 : radius
            return (
              <circle
                key={i}
                cx={sx(x)}
                cy={sy(ys[i] ?? 0)}
                r={r}
                fill={colorFor(i)}
                fillOpacity={isDimmed ? SCATTER_FILL_OPACITY_BG : SCATTER_FILL_OPACITY_FG}
                onMouseEnter={() => setHovered(i)}
                onMouseLeave={() => setHovered(null)}
              />
            )
          })}
        </g>
      </svg>
      {uniques && uniques.length <= MAX_CATEGORICAL_LEGEND_ENTRIES && (
        <div
          style={{
            display: 'flex',
            flexWrap: 'wrap',
            gap: '0.35rem',
            fontSize: '0.68rem',
            marginTop: 6,
          }}
        >
          {uniques.map((cat, i) => {
            const isActive = brush === cat
            return (
              <button
                key={cat}
                type="button"
                onClick={() => onBrush?.(isActive ? null : cat)}
                data-brush-target={cat}
                aria-pressed={isActive}
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: 3,
                  padding: '0 5px',
                  border: `1px solid ${isActive ? 'var(--color-text-primary)' : 'transparent'}`,
                  borderRadius: 3,
                  background: isActive ? 'var(--color-warning-bg)' : 'transparent',
                  color: 'var(--color-text-primary)',
                  cursor: onBrush ? 'pointer' : 'default',
                  fontSize: '0.68rem',
                  fontFamily: 'inherit',
                }}
              >
                <span
                  style={{
                    width: 10,
                    height: 10,
                    background: palette[i % palette.length],
                    display: 'inline-block',
                    borderRadius: 2,
                  }}
                />
                {cat}
              </button>
            )
          })}
        </div>
      )}
    </div>
  )
}

interface VolcanoComparison {
  id: string
  n_total: number
  n_significant: number
  points: {
    log2fc: number[]
    neg_log10_p: number[]
    significant: boolean[]
    labeled: boolean[]
    feature: string[]
  }
}

function VolcanoView({ comparisons }: { comparisons: VolcanoComparison[] }) {
  const [active, setActive] = useState<string>(comparisons[0]?.id ?? '')
  useEffect(() => {
    if (comparisons.length && !comparisons.some((c) => c.id === active)) {
      setActive(comparisons[0]?.id ?? '')
    }
  }, [comparisons, active])
  const cmp = comparisons.find((c) => c.id === active)
  if (!cmp)
    return (
      <div style={{ fontSize: '0.82rem', color: 'var(--color-text-muted)' }}>No comparisons.</div>
    )

  const W = SCATTER_PLOT_WIDTH
  const H = SCATTER_PLOT_HEIGHT
  const pad = SCATTER_AXIS_PADDING_Y
  const xs = cmp.points.log2fc
  const ys = cmp.points.neg_log10_p
  const labels = cmp.points.feature
  const sig = cmp.points.significant
  const lbl = cmp.points.labeled
  const minX = Math.min(...xs)
  const maxX = Math.max(...xs)
  const minY = Math.min(...ys, 0)
  const maxY = Math.max(...ys)
  const sx = (x: number) =>
    pad + ((x - minX) / Math.max(maxX - minX, COORD_EPSILON)) * (W - 2 * pad)
  const sy = (y: number) =>
    H - pad - ((y - minY) / Math.max(maxY - minY, COORD_EPSILON)) * (H - 2 * pad)
  return (
    <div data-testid="dashboard-volcano-view">
      {comparisons.length > 1 && (
        <div style={{ marginBottom: 8, display: 'flex', gap: '0.25rem' }}>
          {comparisons.map((c) => (
            <button
              key={c.id}
              type="button"
              onClick={() => setActive(c.id)}
              aria-pressed={c.id === active}
              style={{
                padding: '0.15rem 0.5rem',
                fontSize: '0.68rem',
                background:
                  c.id === active
                    ? 'var(--color-button-primary-bg)'
                    : 'var(--color-surface-3)',
                color:
                  c.id === active
                    ? 'var(--color-button-primary-fg)'
                    : 'var(--color-text-primary)',
                border: 'none',
                borderRadius: 4,
                cursor: 'pointer',
              }}
            >
              {c.id}
            </button>
          ))}
        </div>
      )}
      <div style={{ fontSize: '0.72rem', color: 'var(--color-text-muted)', marginBottom: 4 }}>
        {formatInteger(cmp.n_significant)} significant of{' '}
        {formatInteger(cmp.n_total)} total
      </div>
      <svg
        role="img"
        aria-label={`Volcano plot ${cmp.id}`}
        viewBox={`0 0 ${W} ${H}`}
        style={{
          width: '100%',
          background: 'var(--color-surface-1)',
          border: '1px solid var(--color-border-strong)',
          borderRadius: 4,
          maxHeight: 520,
          color: 'var(--color-text-primary)',
        }}
      >
        {xs.map((x, i) => (
          <circle
            key={i}
            cx={sx(x)}
            cy={sy(ys[i] ?? 0)}
            r={2}
            fill={sig[i] ? '#dc2626' : '#94a3b8'}
            fillOpacity={sig[i] ? 0.8 : 0.4}
          />
        ))}
        {xs.map((x, i) =>
          lbl[i] ? (
            <text
              key={`lbl-${i}`}
              x={sx(x) + 4}
              y={sy(ys[i] ?? 0) - 3}
              fontSize="9"
              fill="currentColor"
            >
              {labels[i]}
            </text>
          ) : null,
        )}
        <line
          x1={sx(1)}
          y1={pad}
          x2={sx(1)}
          y2={H - pad}
          stroke="currentColor"
          strokeOpacity={0.3}
          strokeDasharray="3,3"
        />
        <line
          x1={sx(-1)}
          y1={pad}
          x2={sx(-1)}
          y2={H - pad}
          stroke="currentColor"
          strokeOpacity={0.3}
          strokeDasharray="3,3"
        />
        <line
          x1={pad}
          y1={sy(1.3)}
          x2={W - pad}
          y2={sy(1.3)}
          stroke="currentColor"
          strokeOpacity={0.3}
          strokeDasharray="3,3"
        />
      </svg>
    </div>
  )
}

function RawJsonView({ payload }: { payload: unknown }) {
  return (
    <pre
      data-testid="dashboard-raw-json"
      style={{
        fontSize: '0.72rem',
        background: 'var(--color-surface-1)',
        border: '1px solid var(--color-border-default)',
        color: 'var(--color-text-primary)',
        borderRadius: 4,
        padding: '0.6rem',
        margin: 0,
        maxHeight: 500,
        overflow: 'auto',
      }}
    >
      {JSON.stringify(payload, null, 2)}
    </pre>
  )
}
