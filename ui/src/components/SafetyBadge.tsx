// Compact safety-level chip for atoms / tasks.
// Maps each SafetyLevel discriminant to a token from the shared palette
// (see styles/palettes.ts). The tooltip exposes the full SafetyPolicy
// shape so SMEs can see why an atom/task is `compute` (or `exec`) at a
// glance without opening the task drawer.
//
// Reuse policy: the four token families (success / info / warning) are
// the same ones TaskCard's status palette draws from — this keeps the
// safety-badge visual language consistent with the running/blocked
// status palette already in use across the dag canvas.

import type { SafetyPolicy } from '../types/SafetyPolicy'

const LEVEL_TOKEN: Record<SafetyPolicy['level'], string> = {
  // Safe == no side effects: muted success (subtle green) — present but
  // visually unobtrusive on dense Status / DAG views.
  safe: 'status-success-muted',
  // Network == egress-only side effects: info / blue family.
  network: 'status-info',
  // Compute == in-package side effects only: stronger success accent
  // (the canonical "this is the normal case for analysis tasks").
  compute: 'status-success',
  // Exec == arbitrary code execution: warning (yellow/orange) so it
  // visibly stands out wherever it appears.
  exec: 'status-warning',
}

const LEVEL_LABEL: Record<SafetyPolicy['level'], string> = {
  safe: 'Safe',
  network: 'Network',
  compute: 'Compute',
  exec: 'Exec',
}

// Maps the token name onto resolved CSS variables. Inline-style
// approach matches the rest of the components/ folder (see CLAUDE.md UI
// section — "No CSS-in-JS runtime"). The status-success-muted token
// falls back through the existing success palette tinted via a partial
// transparency overlay.
const TOKEN_STYLE: Record<string, { bg: string; fg: string; border: string }> =
  {
    'status-success-muted': {
      bg: 'var(--color-success-muted, var(--color-success-bg))',
      fg: 'var(--color-success-fg)',
      border: 'var(--color-success-border)',
    },
    'status-info': {
      bg: 'var(--color-info-bg)',
      fg: 'var(--color-info-fg)',
      border: 'var(--color-info-border)',
    },
    'status-success': {
      bg: 'var(--color-success-bg)',
      fg: 'var(--color-success-fg)',
      border: 'var(--color-success-border)',
    },
    'status-warning': {
      bg: 'var(--color-warning-bg)',
      fg: 'var(--color-warning-fg)',
      border: 'var(--color-warning-border)',
    },
  }

export default function SafetyBadge({
  safety,
}: {
  safety: SafetyPolicy
}): JSX.Element {
  const token = LEVEL_TOKEN[safety.level]
  const label = LEVEL_LABEL[safety.level]
  const palette = TOKEN_STYLE[token] ?? TOKEN_STYLE['status-success-muted']

  // The tooltip carries the deterministic policy shape so an SME with
  // mouse hover (or AT querying the title attribute) can audit the
  // atom-safety policy without opening the task drawer.
  const networkSummary =
    safety.network.kind === 'none'
      ? `none [${safety.network.allowlist.join(', ')}]`
      : safety.network.kind
  const tooltip = [
    `level: ${safety.level}`,
    `network: ${networkSummary}`,
    `code_execution: ${safety.code_execution}`,
    `sandbox: ${safety.sandbox}`,
    `provisioning: ${safety.provisioning}`,
  ].join('\n')

  return (
    <span
      aria-label={`safety: ${label.toLowerCase()}`}
      title={tooltip}
      className={`safety-badge safety-badge--${token}`}
      data-safety-level={safety.level}
      style={{
        display: 'inline-block',
        padding: '1px 6px',
        borderRadius: 4,
        fontSize: '0.62rem',
        fontWeight: 600,
        textTransform: 'uppercase',
        letterSpacing: '0.04em',
        background: palette!.bg,
        color: palette!.fg,
        border: `1px solid ${palette!.border}`,
        lineHeight: 1.35,
        whiteSpace: 'nowrap',
        cursor: 'default',
      }}
    >
      {label}
    </span>
  )
}
