// Package-level aggregate safety banner.
// Shows the worst-case SafetyLevel across all tasks in the package +
// per-level counts. Each count pill is a button that emits a filter
// request to the parent (typically routed into the Audit tab in the
// State Inspector); the parent decides how to surface the filter.
//
// Reads only the SafetySummary type emitted by ts-rs from
// crates/core/src/atom_safety.rs. The summary is computed server-side
// on emit and threaded onto session state, or client-side from the
// per-task SafetyPolicy values in `dag.tasks` when the server hasn't
// sent it yet — both paths produce the same shape.

import type { SafetyLevel } from '../types/SafetyLevel'
import type { SafetySummary } from '../types/SafetySummary'

const LEVEL_LABEL: Record<SafetyLevel, string> = {
  safe: 'Safe',
  network: 'Network',
  compute: 'Compute',
  exec: 'Exec',
}

// Severity ordering for the banner background tint. The banner takes
// the color of the worst-case level so the SME's eye is drawn to
// Exec-bearing packages first.
const TINT_BG: Record<SafetyLevel, string> = {
  safe: 'var(--color-success-bg)',
  network: 'var(--color-info-bg)',
  compute: 'var(--color-success-bg)',
  exec: 'var(--color-warning-bg)',
}

const TINT_BORDER: Record<SafetyLevel, string> = {
  safe: 'var(--color-success-border)',
  network: 'var(--color-info-border)',
  compute: 'var(--color-success-border)',
  exec: 'var(--color-warning-border)',
}

const TINT_FG: Record<SafetyLevel, string> = {
  safe: 'var(--color-success-fg)',
  network: 'var(--color-info-fg)',
  compute: 'var(--color-success-fg)',
  exec: 'var(--color-warning-fg)',
}

interface Props {
  summary: SafetySummary
  onFilterByLevel: (level: SafetyLevel) => void
}

export default function PackageSafetyBanner({
  summary,
  onFilterByLevel,
}: Props): JSX.Element {
  const worst = summary.worst_case_level
  // Ordered Exec → Network → Compute → Safe so the highest-severity
  // count is closest to the worst-case label. Matches the design doc's
  // visual hierarchy.
  const orderedLevels: SafetyLevel[] = ['exec', 'network', 'compute', 'safe']

  return (
    <div
      role="status"
      aria-label={`Package safety: worst-case ${LEVEL_LABEL[worst]}`}
      className="package-safety-banner"
      style={{
        padding: '0.45rem 0.85rem',
        margin: '0.4rem 0.75rem',
        background: TINT_BG[worst],
        border: `1px solid ${TINT_BORDER[worst]}`,
        borderRadius: 6,
        color: TINT_FG[worst],
        fontSize: '0.78rem',
        display: 'flex',
        alignItems: 'center',
        flexWrap: 'wrap',
        gap: 8,
      }}
    >
      <span>
        Package safety: worst-case <strong>{LEVEL_LABEL[worst]}</strong>
      </span>
      <span aria-hidden style={{ opacity: 0.6 }}>
        ·
      </span>
      {orderedLevels.map((level) => {
        const count = summary.level_counts[level]
        return (
          <button
            key={level}
            type="button"
            onClick={() => onFilterByLevel(level)}
            className="safety-banner-pill"
            data-safety-pill={level}
            aria-label={`${count} ${LEVEL_LABEL[level]} — filter`}
            style={{
              padding: '2px 8px',
              borderRadius: 4,
              fontSize: '0.72rem',
              fontWeight: 600,
              background: 'var(--color-surface-1)',
              border: `1px solid ${TINT_BORDER[worst]}`,
              color: TINT_FG[worst],
              cursor: 'pointer',
            }}
          >
            {count} {LEVEL_LABEL[level]}
          </button>
        )
      })}
    </div>
  )
}
