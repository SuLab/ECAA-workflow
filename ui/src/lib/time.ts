// Plain-language time formatting + per-task ETA helpers.
//
// relativeTime(iso) produces "just now" / "2 minutes ago" / "yesterday at
// 14:05" / "Apr 22 at 09:15" depending on distance from now. useRelativeTime
// hook wraps it with a 60-second tick so live cards update without prop-
// drilling the current clock. etaFromHistory estimates remaining wall-
// clock from SessionMetrics.per_task_agent historical task durations
// grouped by stage class.

import { useEffect, useState } from 'react'
import { RELATIVE_TIME_TICK_MS } from './polling'

const MS_MIN = 60_000
const MS_HOUR = 60 * MS_MIN
const MS_DAY = 24 * MS_HOUR

/**
 * Format an ISO-8601 timestamp as plain-language relative time.
 *
 * - < 30s: "just now"
 * - < 60m: "N minutes ago"
 * - < 24h: "N hours ago"
 * - yesterday: "yesterday at HH:MM"
 * - < 7d: "Mon at HH:MM"
 * - otherwise: "MMM DD at HH:MM"
 *
 * Future timestamps (clock drift, scheduled events) render as "in N …".
 * Returns "—" on parse failure so callers can render confidently.
 */
export function relativeTime(iso: string, now: number = Date.now()): string {
  const t = new Date(iso).getTime()
  if (Number.isNaN(t)) return '—'
  const diffMs = now - t
  const isFuture = diffMs < 0
  const abs = Math.abs(diffMs)

  if (abs < 30_000) return isFuture ? 'soon' : 'just now'
  if (abs < MS_HOUR) {
    const mins = Math.max(1, Math.floor(abs / MS_MIN))
    return isFuture
      ? `in ${mins} minute${mins === 1 ? '' : 's'}`
      : `${mins} minute${mins === 1 ? '' : 's'} ago`
  }
  if (abs < MS_DAY) {
    const hours = Math.max(1, Math.floor(abs / MS_HOUR))
    return isFuture
      ? `in ${hours} hour${hours === 1 ? '' : 's'}`
      : `${hours} hour${hours === 1 ? '' : 's'} ago`
  }

  const d = new Date(t)
  const hh = d.getHours().toString().padStart(2, '0')
  const mm = d.getMinutes().toString().padStart(2, '0')
  const nowDate = new Date(now)

  const dayDiff = daysBetween(nowDate, d)
  if (!isFuture && dayDiff === 1) return `yesterday at ${hh}:${mm}`
  if (!isFuture && dayDiff < 7) {
    const weekday = d.toLocaleDateString(undefined, { weekday: 'short' })
    return `${weekday} at ${hh}:${mm}`
  }
  const month = d.toLocaleDateString(undefined, { month: 'short' })
  const day = d.getDate().toString()
  return `${month} ${day} at ${hh}:${mm}`
}

function daysBetween(a: Date, b: Date): number {
  const startA = Date.UTC(a.getFullYear(), a.getMonth(), a.getDate())
  const startB = Date.UTC(b.getFullYear(), b.getMonth(), b.getDate())
  return Math.floor((startA - startB) / MS_DAY)
}

/**
 * Hook that re-renders every 60s so relative-time strings tick forward
 * without the caller having to wire a timer. Returns the formatted
 * string; call site drops it directly into JSX.
 */
export function useRelativeTime(iso: string | null | undefined): string {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), RELATIVE_TIME_TICK_MS)
    return () => clearInterval(id)
  }, [])
  if (!iso) return ''
  return relativeTime(iso, now)
}

/**
 * Format an absolute duration (seconds) as "X min" / "Xh Ym" for the
 * ETA label.
 */
export function formatDuration(seconds: number): string {
  const s = Math.max(0, Math.round(seconds))
  if (s < 60) return `${s}s`
  const m = Math.round(s / 60)
  if (m < 60) return `${m} min`
  const h = Math.floor(m / 60)
  const rem = m % 60
  return rem === 0 ? `${h}h` : `${h}h ${rem}m`
}

interface PerTaskTiming {
  /** Stage class of the task (from PerTaskAgentSnapshot.stage_class). */
  stage_class?: string | null
  /** Wall-clock seconds the task consumed. */
  elapsed_secs?: number | null
}

/**
 * Estimate remaining wall-clock for an in-flight task from historical
 * per-task-class completion times. Returns `null` when there's no
 * stage-class match or fewer than 2 historical completions. Call site
 * can fall back to the generic "in progress" label.
 *
 * The `confidence` field is a hand-rolled heuristic: "low" with 2 prior
 * completions, "med" with 3-4, "high" with 5+. Clamped downward by the
 * median absolute deviation — a highly dispersed per-class history
 * earns only "low" regardless of sample count.
 */
export function etaFromHistory(
  startedAt: string | null,
  stageClass: string | null | undefined,
  history: readonly PerTaskTiming[],
  now: number = Date.now(),
): { eta_mins: number; confidence: 'low' | 'med' | 'high' } | null {
  if (!startedAt || !stageClass) return null
  const matching = history
    .filter((t) => t.stage_class === stageClass && typeof t.elapsed_secs === 'number')
    .map((t) => t.elapsed_secs as number)
    .filter((s) => s > 0)
  if (matching.length < 2) return null
  matching.sort((a, b) => a - b)
  const mid = matching.length >> 1
  // With matching.length >= 2 the mid-index reads are always
  // in-bounds; `?? 0` keeps tsc happy under noUncheckedIndexedAccess
  // without changing behavior.
  const median =
    matching.length % 2 === 0
      ? ((matching[mid - 1] ?? 0) + (matching[mid] ?? 0)) / 2
      : (matching[mid] ?? 0)

  const startedMs = new Date(startedAt).getTime()
  if (Number.isNaN(startedMs)) return null
  const elapsedSecs = Math.max(0, (now - startedMs) / 1000)
  const remainingSecs = Math.max(0, median - elapsedSecs)

  const deviations = matching.map((s) => Math.abs(s - median))
  deviations.sort((a, b) => a - b)
  const mad = deviations[deviations.length >> 1] ?? 0
  const dispersion = mad / Math.max(1, median)

  let confidence: 'low' | 'med' | 'high'
  if (matching.length >= 5 && dispersion < 0.2) confidence = 'high'
  else if (matching.length >= 3 && dispersion < 0.4) confidence = 'med'
  else confidence = 'low'

  return { eta_mins: Math.max(0, Math.round(remainingSecs / 60)), confidence }
}
