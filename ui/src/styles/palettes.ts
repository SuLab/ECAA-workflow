// Shared palette tokens consumed by CardContainer and any other surface
// that would otherwise hardcode hex literals. Each entry has four roles:
// `bg` (card background), `border` (subtle edge), `accent` (prominent
// left-border / button background), `fg` (title text color). Consumers
// pass `palette="warning"` etc. to CardContainer; direct access via
// `CARD_PALETTES.warning.accent` is fine for buttons/chips inside the
// card.
//
// Values resolve at paint time from the CSS variables defined in
//./tokens.css. Swapping `data-theme` on <html> flips the whole palette
// without re-rendering components.

export const CARD_PALETTES = {
  warning: {
    bg: 'var(--color-warning-bg)',
    border: 'var(--color-warning-border)',
    accent: 'var(--color-warning-accent)',
    fg: 'var(--color-warning-fg)',
  },
  info: {
    bg: 'var(--color-info-bg)',
    border: 'var(--color-info-border)',
    accent: 'var(--color-info-accent)',
    fg: 'var(--color-info-fg)',
  },
  success: {
    bg: 'var(--color-success-bg)',
    border: 'var(--color-success-border)',
    accent: 'var(--color-success-accent)',
    fg: 'var(--color-success-fg)',
  },
  danger: {
    bg: 'var(--color-danger-bg)',
    border: 'var(--color-danger-border)',
    accent: 'var(--color-danger-accent)',
    fg: 'var(--color-danger-fg)',
  },
  branch: {
    bg: 'var(--color-branch-bg)',
    border: 'var(--color-branch-border)',
    accent: 'var(--color-branch-accent)',
    fg: 'var(--color-branch-fg)',
  },
  neutral: {
    bg: 'var(--color-neutral-bg)',
    border: 'var(--color-neutral-border)',
    accent: 'var(--color-neutral-accent)',
    fg: 'var(--color-neutral-fg)',
  },
} as const

export type CardPalette = keyof typeof CARD_PALETTES
