// Static color-contrast audit.
//
// jsdom doesn't implement canvas getContext(), so axe-core's
// `color-contrast` rule is disabled in `axe.test.tsx` (it would
// emit a noisy false-positive). This test computes WCAG 2.1
// contrast ratios directly from `tokens.ts` and asserts the
// load-bearing foreground/background pairs hit AA (4.5:1 for
// body text, 3:1 for large text and UI chrome). The pairs are
// the same ones the accessibility audit doc spot-checked
// out-of-band; the static test pulls those checks into the CI
// surface so contrast drift fails on the PR rather than on the
// next manual audit.
//
// WCAG 2.1 AA targets:
// - Normal text: ≥ 4.5:1
// - Large text (18pt+ or 14pt+ bold): ≥ 3:1
// - Non-text UI components + active states: ≥ 3:1
//
// Reference: https://www.w3.org/WAI/WCAG21/Understanding/contrast-minimum.html

import { describe, expect, it } from 'vitest'
import { THEMES, type ThemeTokens } from '../styles/tokens'

/** Parse a `#RRGGBB` hex literal into [r, g, b] in 0..1. */
function parseHex(hex: string): [number, number, number] {
  const clean = hex.replace(/^#/, '')
  if (clean.length !== 6) {
    throw new Error(`expected #RRGGBB hex, got: ${hex}`)
  }
  const r = parseInt(clean.slice(0, 2), 16) / 255
  const g = parseInt(clean.slice(2, 4), 16) / 255
  const b = parseInt(clean.slice(4, 6), 16) / 255
  return [r, g, b]
}

/** Convert a 0..1 sRGB channel to linear. */
function linearize(c: number): number {
  return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4)
}

/** WCAG 2.1 relative luminance from a hex color string. */
function luminance(hex: string): number {
  const [r, g, b] = parseHex(hex).map(linearize)
  // Coefficients per WCAG 2.x spec.
  return 0.2126 * r! + 0.7152 * g! + 0.0722 * b!
}

/** WCAG 2.1 contrast ratio between two hex colors. Returns ratio
 *  in the conventional `lighter / darker` order (always >= 1). */
function contrastRatio(fg: string, bg: string): number {
  const a = luminance(fg)
  const b = luminance(bg)
  const lighter = Math.max(a, b)
  const darker = Math.min(a, b)
  return (lighter + 0.05) / (darker + 0.05)
}

interface Pair {
  /** Stable label for the failure message. */
  name: string
  /** Foreground token name. */
  fg: keyof ThemeTokens
  /** Background token name. */
  bg: keyof ThemeTokens
  /** WCAG threshold. 4.5 for normal-text, 3.0 for large/UI. */
  min: 4.5 | 3.0
}

/** Foreground / background pairs that must hit WCAG AA in BOTH
 *  light and dark themes. Pulled from the accessibility audit doc's
 *  load-bearing list; new pairs land here when a component picks
 *  a non-default fg/bg combination. */
const PAIRS: Pair[] = [
  // Body text on every surface tier.
  { name: 'textPrimary on surface0', fg: 'textPrimary', bg: 'surface0', min: 4.5 },
  { name: 'textPrimary on surface1', fg: 'textPrimary', bg: 'surface1', min: 4.5 },
  { name: 'textPrimary on surface2', fg: 'textPrimary', bg: 'surface2', min: 4.5 },
  { name: 'textPrimary on chromeBg', fg: 'chromeFg', bg: 'chromeBg', min: 4.5 },
  // Secondary / muted body text.
  { name: 'textSecondary on surface0', fg: 'textSecondary', bg: 'surface0', min: 4.5 },
  { name: 'textMuted on surface0', fg: 'textMuted', bg: 'surface0', min: 4.5 },
  // Accent button — text on accent fill must be readable. AA for
  // normal text (the buttons carry "Accept"/"Confirm" labels).
  { name: 'accentFg on accent', fg: 'accentFg', bg: 'accent', min: 4.5 },
  // Border vs surface — non-text UI threshold (3.0). Borders are
  // load-bearing for keyboard-focus indicators and form-field
  // boundaries; the AA non-text threshold applies.
  { name: 'borderStrong on surface0', fg: 'borderStrong', bg: 'surface0', min: 3.0 },
  // Code text on the inline code chip.
  { name: 'textCode on surface0', fg: 'textCode', bg: 'surface0', min: 4.5 },
]

describe('a11y — color-contrast (Plan §S11.7)', () => {
  for (const mode of ['light', 'dark'] as const) {
    describe(`${mode} theme`, () => {
      const theme = THEMES[mode]
      for (const pair of PAIRS) {
        it(`${pair.name} hits WCAG AA (≥${pair.min.toFixed(1)}:1)`, () => {
          const fg = theme[pair.fg] as string
          const bg = theme[pair.bg] as string
          const ratio = contrastRatio(fg, bg)
          expect(ratio).toBeGreaterThanOrEqual(pair.min)
        })
      }
    })
  }

  it('the contrast computation matches a known WCAG reference', () => {
    // Black on white: should be exactly 21:1 by the WCAG formula.
    expect(contrastRatio('#000000', '#ffffff')).toBeCloseTo(21, 1)
    // White on white: 1:1.
    expect(contrastRatio('#ffffff', '#ffffff')).toBeCloseTo(1, 4)
  })
})
