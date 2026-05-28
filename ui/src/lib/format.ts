// Shared number-formatting helpers. The codebase scattered
// `$${n.toFixed(2)}` and ad-hoc toLocaleString() calls across every
// metrics, dashboard, and banner surface; this module centralizes both
// so downstream tweaks (locale, currency symbol, decimal precision)
// land in one place.
//
// `formatUSD` is intentionally locale-agnostic — the UI is English-only
// and we want byte-stable test snapshots. Pass `precision: 0` for
// whole-dollar displays (e.g. budgets) and the default `precision: 2`
// for cost rollups.

interface FormatUsdOpts {
  /** Decimal places to render. Defaults to 2. */
  precision?: number
}

/** Format a number as a USD currency string. Returns e.g. `$12.34`. */
export function formatUSD(n: number, opts: FormatUsdOpts = {}): string {
  const precision = opts.precision ?? 2
  if (!Number.isFinite(n)) {
    return `$${(0).toFixed(precision)}`
  }
  return `$${n.toFixed(precision)}`
}

/**
 * Format an integer with locale-aware thousands separators
 * (`toLocaleString()` default). Returns e.g. `1,234,567`.
 */
export function formatInteger(n: number): string {
  if (!Number.isFinite(n)) return '0'
  // Round to nearest integer so callers can pass through float
  // accumulators (e.g. counts derived from filter/reduce) without
  // worrying about precision.
  return Math.round(n).toLocaleString()
}
