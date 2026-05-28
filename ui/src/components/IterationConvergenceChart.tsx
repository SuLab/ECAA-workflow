// Convergence-trajectory chart for iterate-until tasks.
//
// Renders the per-iteration metric trail produced by an `iterate_check_<id>`
// task as a small SVG line chart: x-axis is iteration number (1..N), y-axis
// is the convergence metric. Optional threshold reference line (dashed) is
// drawn when the agent passes the convergence threshold; the converged-at
// iteration is marked with a filled circle so the SME can see "this is
// where the loop stopped".
//
// Pure SVG + React — no chart-lib dependency, matches the DashboardTab
// inline-SVG idiom (P3 foundation discipline).

import { type CSSProperties, useId, useMemo } from 'react'

export interface IterationConvergencePoint {
  iter: number
  metric: number
}

export interface Props {
  /** Per-iteration metric trail. The agent writes this from
   *  `runtime/outputs/<id>/progress.log` when it marks the
   *  `iterate_check_<id>` task Completed (S10.4 contract). */
  trail: IterationConvergencePoint[]
  /** Convergence threshold from the atom's `iterate.convergence.threshold`.
   *  When present, drawn as a horizontal dashed reference line so the SME
   *  can see how far each iteration sat from the rule. */
  threshold?: number
  /** CEL convergence operator (`<`, `<=`, `>`, `>=`). Drives the threshold
   *  line label only — the chart is operator-agnostic. */
  operator?: '<' | '<=' | '>' | '>='
  /** Iteration index (1-based) at which the convergence rule fired. The
   *  chart marks this point with a filled circle. Optional — non-converging
   *  trails (max_iterations hit) still render without the marker. */
  convergedAtIter?: number
  /** Compact mode squeezes the chart to ~140×56 for inline-card use; full
   *  mode is ~280×120. Defaults to full. */
  compact?: boolean
  /** Optional aria-describedby override; default is the auto-generated
   *  summary line below the chart. */
  style?: CSSProperties
}

interface Geometry {
  width: number
  height: number
  padLeft: number
  padRight: number
  padTop: number
  padBottom: number
  innerW: number
  innerH: number
}

function computeGeometry(compact: boolean): Geometry {
  const width = compact ? 220 : 320
  const height = compact ? 72 : 132
  const padLeft = 30
  const padRight = 8
  const padTop = 10
  const padBottom = 18
  return {
    width,
    height,
    padLeft,
    padRight,
    padTop,
    padBottom,
    innerW: width - padLeft - padRight,
    innerH: height - padTop - padBottom,
  }
}

interface Scales {
  xMin: number
  xMax: number
  yMin: number
  yMax: number
  xToPx: (x: number) => number
  yToPx: (y: number) => number
}

function computeScales(
  trail: IterationConvergencePoint[],
  threshold: number | undefined,
  geom: Geometry,
): Scales {
  const xMin = 1
  const xMax = Math.max(2, ...trail.map((p) => p.iter))
  const metrics = trail.map((p) => p.metric)
  // Pad the y-axis ~5% beyond the observed range so the trail line
  // never sits flush against the top/bottom edge. Threshold (when
  // present) participates in the range so its dashed line stays
  // visible even when the trail collapsed to one side of it.
  const ys = threshold !== undefined ? [...metrics, threshold] : metrics
  let yMin = Math.min(...ys)
  let yMax = Math.max(...ys)
  if (yMin === yMax) {
    // Degenerate: every point is the same value. Inflate by 1 in both
    // directions so the line renders mid-chart, not on the bottom edge.
    yMin -= 1
    yMax += 1
  } else {
    const span = yMax - yMin
    yMin -= span * 0.08
    yMax += span * 0.08
  }
  const xToPx = (x: number) =>
    geom.padLeft + ((x - xMin) / Math.max(1, xMax - xMin)) * geom.innerW
  const yToPx = (y: number) =>
    geom.padTop + (1 - (y - yMin) / (yMax - yMin)) * geom.innerH
  return { xMin, xMax, yMin, yMax, xToPx, yToPx }
}

function formatNumber(n: number): string {
  // Match the agent's progress.log convention: at most 4 sig-figs so a
  // 0.0123456 metric reads "0.01235" not "0.0123456" eating axis labels.
  if (Number.isInteger(n)) return String(n)
  if (Math.abs(n) >= 100) return n.toFixed(0)
  if (Math.abs(n) >= 1) return n.toFixed(2)
  return n.toFixed(4)
}

export default function IterationConvergenceChart({
  trail,
  threshold,
  operator,
  convergedAtIter,
  compact = false,
  style,
}: Props): JSX.Element | null {
  const geom = computeGeometry(compact)
  const titleId = useId()
  const summaryId = useId()

  const scales = useMemo(() => computeScales(trail, threshold, geom), [trail, threshold, geom])

  if (trail.length < 1) return null

  // SVG path string for the trail polyline.
  const path = trail
    .map((p, i) => {
      const cmd = i === 0 ? 'M' : 'L'
      return `${cmd}${scales.xToPx(p.iter).toFixed(2)},${scales.yToPx(p.metric).toFixed(2)}`
    })
    .join(' ')

  const lastMetric = trail[trail.length - 1]?.metric
  const lastIter = trail[trail.length - 1]?.iter
  const summary = (() => {
    if (convergedAtIter != null && lastMetric != null) {
      return `Converged at iter ${convergedAtIter} (metric ${formatNumber(lastMetric)}).`
    }
    if (lastIter != null && lastMetric != null) {
      return `${trail.length} iterations; last metric ${formatNumber(lastMetric)} at iter ${lastIter}.`
    }
    return `${trail.length} iterations recorded.`
  })()

  return (
    <figure
      data-iter-chart="true"
      role="figure"
      aria-labelledby={titleId}
      aria-describedby={summaryId}
      style={{
        margin: 0,
        marginTop: '0.6rem',
        padding: '0.5rem 0.6rem 0.4rem',
        background: 'var(--color-surface-0)',
        border: '1px solid var(--color-border-default)',
        borderRadius: 6,
        ...style,
      }}
    >
      <figcaption
        id={titleId}
        style={{
          fontSize: '0.7rem',
          fontWeight: 600,
          textTransform: 'uppercase',
          letterSpacing: '0.04em',
          color: 'var(--color-text-secondary)',
          marginBottom: 4,
        }}
      >
        Convergence trajectory
      </figcaption>
      <svg
        width={geom.width}
        height={geom.height}
        viewBox={`0 0 ${geom.width} ${geom.height}`}
        role="img"
        aria-label={summary}
        style={{ display: 'block' }}
      >
        {/* Y-axis baseline + min/max labels. */}
        <line
          x1={geom.padLeft}
          y1={geom.padTop}
          x2={geom.padLeft}
          y2={geom.height - geom.padBottom}
          stroke="var(--color-border-default)"
          strokeWidth={1}
        />
        <line
          x1={geom.padLeft}
          y1={geom.height - geom.padBottom}
          x2={geom.width - geom.padRight}
          y2={geom.height - geom.padBottom}
          stroke="var(--color-border-default)"
          strokeWidth={1}
        />
        <text
          x={geom.padLeft - 4}
          y={geom.padTop + 4}
          fontSize={9}
          textAnchor="end"
          fill="var(--color-text-secondary)"
        >
          {formatNumber(scales.yMax)}
        </text>
        <text
          x={geom.padLeft - 4}
          y={geom.height - geom.padBottom}
          fontSize={9}
          textAnchor="end"
          fill="var(--color-text-secondary)"
        >
          {formatNumber(scales.yMin)}
        </text>
        <text
          x={geom.padLeft}
          y={geom.height - 4}
          fontSize={9}
          textAnchor="start"
          fill="var(--color-text-secondary)"
        >
          iter 1
        </text>
        <text
          x={geom.width - geom.padRight}
          y={geom.height - 4}
          fontSize={9}
          textAnchor="end"
          fill="var(--color-text-secondary)"
        >
          iter {scales.xMax}
        </text>
        {/* Threshold reference line (dashed). */}
        {threshold !== undefined && (
          <g data-threshold-line="true">
            <line
              x1={geom.padLeft}
              y1={scales.yToPx(threshold)}
              x2={geom.width - geom.padRight}
              y2={scales.yToPx(threshold)}
              stroke="var(--color-warning-accent)"
              strokeDasharray="3 3"
              strokeWidth={1}
            />
            <text
              x={geom.width - geom.padRight - 2}
              y={scales.yToPx(threshold) - 2}
              fontSize={8}
              textAnchor="end"
              fill="var(--color-warning-fg)"
            >
              {operator ?? ''} {formatNumber(threshold)}
            </text>
          </g>
        )}
        {/* Trail polyline. */}
        <path
          d={path}
          fill="none"
          stroke="var(--color-info-accent)"
          strokeWidth={1.5}
          strokeLinejoin="round"
          strokeLinecap="round"
          data-trail="true"
        />
        {/* Per-iteration markers. Subtle — keeps the line legible. */}
        {trail.map((p) => (
          <circle
            key={`pt-${p.iter}`}
            cx={scales.xToPx(p.iter)}
            cy={scales.yToPx(p.metric)}
            r={1.8}
            fill="var(--color-info-accent)"
            data-iter-point={p.iter}
          />
        ))}
        {/* Converged-at marker. Filled circle on the converging iter so
            the SME sees the stopping point even when the trail kept
            running for a settle window. */}
        {convergedAtIter !== undefined &&
          (() => {
            const pt = trail.find((p) => p.iter === convergedAtIter)
            if (!pt) return null
            return (
              <circle
                data-converged-marker="true"
                cx={scales.xToPx(pt.iter)}
                cy={scales.yToPx(pt.metric)}
                r={3.5}
                fill="var(--color-success-accent)"
                stroke="var(--color-surface-0)"
                strokeWidth={1.5}
              />
            )
          })()}
      </svg>
      <p
        id={summaryId}
        style={{
          margin: '0.25rem 0 0',
          fontSize: '0.7rem',
          color: 'var(--color-text-secondary)',
        }}
      >
        {summary}
      </p>
    </figure>
  )
}

/**
 * Type-guard + parse helper. The agent writes the trail into the
 * iterate_check task's result JSON; this normalises any ad-hoc shape
 * (`[{iter, metric}]` or `[{iteration, value}]` or `[[iter, metric]]`)
 * into the canonical Array<{iter, metric}>. Returns null when the
 * result doesn't carry a recognisable trail.
 */
export function extractIterationTrail(
  result: unknown,
): IterationConvergencePoint[] | null {
  if (!result || typeof result !== 'object') return null
  const raw = (result as Record<string, unknown>).metric_trail
  if (!Array.isArray(raw)) return null
  const out: IterationConvergencePoint[] = []
  for (const item of raw) {
    if (Array.isArray(item) && item.length >= 2) {
      const iter = Number(item[0])
      const metric = Number(item[1])
      if (Number.isFinite(iter) && Number.isFinite(metric)) {
        out.push({ iter, metric })
      }
      continue
    }
    if (item && typeof item === 'object') {
      const obj = item as Record<string, unknown>
      const iter = Number(obj.iter ?? obj.iteration ?? obj.n)
      const metric = Number(obj.metric ?? obj.value ?? obj.m)
      if (Number.isFinite(iter) && Number.isFinite(metric)) {
        out.push({ iter, metric })
      }
    }
  }
  // Sort by iter so out-of-order writes still chart correctly.
  out.sort((a, b) => a.iter - b.iter)
  return out.length > 0 ? out : null
}

/**
 * Canonical iterate-result shape parsed from a completed iterate_check
 * task's result JSON. Used by ResultReviewTurnCard to thread the
 * convergence chart in alongside the JSON dump.
 */
export interface ParsedIterateResult {
  trail: IterationConvergencePoint[]
  threshold?: number
  operator?: '<' | '<=' | '>' | '>='
  convergedAtIter?: number
}

export function parseIterateResult(result: unknown): ParsedIterateResult | null {
  const trail = extractIterationTrail(result)
  if (!trail) return null
  if (!result || typeof result !== 'object') return { trail }
  const obj = result as Record<string, unknown>
  const threshold =
    typeof obj.threshold === 'number'
      ? obj.threshold
      : typeof obj.convergence_threshold === 'number'
        ? obj.convergence_threshold
        : undefined
  const operatorRaw = obj.operator ?? obj.convergence_operator
  const operator =
    operatorRaw === '<' ||
    operatorRaw === '<=' ||
    operatorRaw === '>' ||
    operatorRaw === '>='
      ? operatorRaw
      : undefined
  const convergedAtIter =
    typeof obj.converged_at === 'number'
      ? obj.converged_at
      : typeof obj.converged_at_iter === 'number'
        ? obj.converged_at_iter
        : undefined
  return { trail, threshold, operator, convergedAtIter }
}
