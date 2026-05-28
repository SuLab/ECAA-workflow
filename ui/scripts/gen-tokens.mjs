#!/usr/bin/env node
// Single-source theme-token generator. Reads
// `ui/src/styles/tokens.json` (the only hand-edited token-VALUE
// surface) and emits `tokens.ts` (TypeScript record + interface) +
// `tokens.css` (`:root` light + `:root[data-theme=dark]` blocks plus
// the static footer).
//
// Run via:
//   npm run gen-tokens   (from ui/)
//   make types           (from repo root; types target invokes this)
//
// `scripts/check-tokens-in-sync.sh` runs this script and asserts the
// emitted files match HEAD; the CI gate fails on drift so a manual
// edit to tokens.ts or tokens.css gets caught at review time.
//
// The expected output is byte-identical to the hand-curated 2026-05-05
// baseline. Group order, comment placement, and blank-line layout are
// preserved by mirroring the baseline's structure verbatim — adding a
// new token requires extending TOKEN_GROUPS + (if applicable)
// TS_GROUP_HEADERS / CSS_LIGHT_GROUP_HEADERS / CSS_DARK_GROUP_HEADERS.

import { promises as fs } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const STYLES_DIR = path.join(__dirname, '..', 'src', 'styles')
const SOURCE = path.join(STYLES_DIR, 'tokens.json')
const TS_OUT = path.join(STYLES_DIR, 'tokens.ts')
const CSS_OUT = path.join(STYLES_DIR, 'tokens.css')

// Token groups — order matters; tokens emit in this order with a blank
// line between groups in the TypeScript output. CSS layout is governed
// separately by CSS_LIGHT_GROUP_HEADERS / CSS_DARK_GROUP_HEADERS.
const TOKEN_GROUPS = [
  ['surface0', 'surface1', 'surface2', 'surface3', 'surfaceMuted', 'surfaceDanger'],
  [
    'chromeBg',
    'chromeBgElevated',
    'chromeFg',
    'chromeFgMuted',
    'chromeFgFaint',
    'chromeFgAccent',
    'chromeBorder',
    'chromeBorderStrong',
  ],
  [
    'textPrimary',
    'textSecondary',
    'textMuted',
    'textFaint',
    'textOnAccent',
    'textOnDark',
    'textLink',
    'textCode',
    'textDefault',
  ],
  ['borderSubtle', 'borderFaint', 'borderDefault', 'borderStrong', 'grid'],
  [
    'accent',
    'accentHover',
    'accentFg',
    'accentBg',
    'accentMutedBg',
    'accentMutedBorder',
    'accentSoft',
    'focusRing',
  ],
  ['buttonPrimaryBg', 'buttonPrimaryBgHover', 'buttonPrimaryFg'],
  ['userBubbleBg', 'userBubbleFg'],
  ['warningBg', 'warningBorder', 'warningAccent', 'warningFg', 'warningMuted'],
  ['infoBg', 'infoBorder', 'infoAccent', 'infoFg'],
  ['successBg', 'successBorder', 'successAccent', 'successFg', 'successMuted'],
  ['dangerBg', 'dangerBorder', 'dangerAccent', 'dangerFg', 'dangerMuted'],
  ['branchBg', 'branchBorder', 'branchAccent', 'branchFg'],
  ['neutralBg', 'neutralBorder', 'neutralAccent', 'neutralFg'],
  ['chart'],
  ['shadowSm', 'shadowMd', 'shadowLg'],
]

// Tokens skipped from the TS interface ThemeTokens. shadow* tokens are
// CSS-only (consumed via var(--shadow-*)); they appear in tokens.css
// but never in tokens.ts. The light/dark TypeScript records skip them
// outright so their absence from the interface doesn't surface.
const TS_SKIP_TOKENS = new Set(['shadowSm', 'shadowMd', 'shadowLg'])

// CSS group headers — keyed by the first token in each TOKEN_GROUPS
// row. `null` = no header for that group. Light + dark have different
// header rules: the light :root has every header; the dark block
// inherits the light layout structurally but suppresses every header
// except the chart-palette callout (which carries a dark-specific
// caption) — matches the 2026-05-05 baseline tokens.css verbatim.
const CSS_LIGHT_GROUP_HEADERS = {
  surface0: '/* Primary surfaces (by depth) */',
  chromeBg:
    '/* Chrome — always-dark areas (title bar, tooltips, dark panels) */',
  textPrimary: '/* Text on standard surfaces */',
  borderSubtle: '/* Borders and rules */',
  accent: '/* Primary accent (interactive) */',
  buttonPrimaryBg:
    '/* Primary "dark" button (inverts in dark mode to accent) */',
  userBubbleBg: '/* User message bubble — distinct from assistant bubble */',
  warningBg:
    '/* Semantic palettes — mirror CARD_PALETTES layout (bg / border / accent / fg) */',
  chart: '/* Chart palette (matplotlib defaults, light-friendly) */',
  shadowSm: '/* Elevation */',
}
const CSS_DARK_GROUP_HEADERS = {
  chart: '/* Chart palette — higher-luminance, colorblind-safer on dark */',
}

// Tokens that share a single header (drawn from the FIRST token in
// the first group of the run); subsequent tokens in the run get a
// blank-line separator but no header. Layout mirrors the
// 2026-05-05 baseline tokens.css verbatim.
const SHARED_HEADER_RUNS = [
  ['warningBg', 'infoBg', 'successBg', 'dangerBg', 'branchBg', 'neutralBg'],
]

const PER_THEME_BORDER_STRONG_CSS_COMMENT = {
  light: '/* slate-500 hits WCAG AA 3:1 vs surface0. */',
  dark:
    '/* slate-500 hits WCAG AA 3:1 vs surface0 in dark too. */',
}

function isSharedHeaderContinuation(firstTokenInGroup) {
  for (const run of SHARED_HEADER_RUNS) {
    const idx = run.indexOf(firstTokenInGroup)
    if (idx > 0) return true
  }
  return false
}

// camelCase → kebab-case for a CSS variable suffix. Insert a hyphen
// between a letter and a following digit too, so `surface0` becomes
// `surface-0` to match the baseline `--color-surface-0` shape.
function kebab(camel) {
  return camel
    .replace(/([a-z])([A-Z])/g, '$1-$2')
    .replace(/([a-zA-Z])(\d)/g, '$1-$2')
    .toLowerCase()
}

// CSS custom-property name. Color tokens use the `--color-*` prefix;
// shadow tokens use `--shadow-*` (matches the baseline).
function cssVarName(camel) {
  if (camel.startsWith('shadow')) {
    return '--shadow-' + camel.slice('shadow'.length).toLowerCase()
  }
  return '--color-' + kebab(camel)
}

// ─── tokens.ts rendering ──────────────────────────────────────────────────

function renderTsInterface() {
  const lines = ['export interface ThemeTokens {']
  for (let g = 0; g < TOKEN_GROUPS.length; g++) {
    const visible = TOKEN_GROUPS[g].filter((k) => !TS_SKIP_TOKENS.has(k))
    if (visible.length === 0) continue
    if (g > 0) lines.push('')
    for (const k of visible) {
      if (k === 'chart') {
        lines.push(
          '  chart: readonly [string, string, string, string, string, string, string, string, string, string]',
        )
      } else {
        lines.push(`  ${k}: string`)
      }
    }
  }
  lines.push('}')
  return lines.join('\n')
}

function renderTsRecord(themeName, tokens) {
  const upper = themeName.toUpperCase()
  const lines = [`const ${upper}: ThemeTokens = {`]
  for (let g = 0; g < TOKEN_GROUPS.length; g++) {
    const visible = TOKEN_GROUPS[g].filter((k) => !TS_SKIP_TOKENS.has(k))
    if (visible.length === 0) continue
    if (g > 0) lines.push('')
    for (const key of visible) {
      const value = tokens[key]
      if (value === undefined) {
        throw new Error(
          `tokens.json[${themeName}] missing token "${key}" (declared in TOKEN_GROUPS)`,
        )
      }
      if (key === 'borderStrong') {
        if (themeName === 'light') {
          lines.push(
            '  // slate-500 #64748b (~4.7:1 vs surface0) so form-field outlines +',
          )
          lines.push(
            '  // active focus indicators hit WCAG AA 3:1 for non-text UI',
          )
          lines.push('  // components.')
        } else {
          lines.push(
            '  // slate-500 #64748b (~3.9:1 vs surface0) so form-field outlines +',
          )
          lines.push(
            '  // active focus indicators hit WCAG AA 3:1 for non-text UI',
          )
          lines.push('  // components.')
        }
      }
      if (key === 'chart' && Array.isArray(value)) {
        const head = value.slice(0, 5).map((c) => `'${c}'`).join(', ')
        const tail = value.slice(5, 10).map((c) => `'${c}'`).join(', ')
        lines.push('  chart: [')
        lines.push(`    ${head},`)
        lines.push(`    ${tail},`)
        lines.push('  ],')
      } else {
        lines.push(`  ${key}: '${value}',`)
      }
    }
  }
  lines.push('}')
  return lines.join('\n')
}

function renderTs(json) {
  return [
    '// JS-side mirror of tokens.css. Inline styles should prefer the',
    '// `var(--color-*)` form — reach for this record only when you need a',
    '// runtime color value (ReactFlow props, canvas drawing, chart libs',
    "// that don't consume CSS variables). Generated by",
    '// `ui/scripts/gen-tokens.mjs` from `ui/src/styles/tokens.json`',
    '// — do NOT edit by hand. `scripts/check-no-hex-in-ui.sh`',
    '// enforces that no other file in ui/src/ introduces hex literals.',
    '',
    "export type ThemeMode = 'light' | 'dark'",
    '',
    renderTsInterface(),
    '',
    renderTsRecord('light', json.light),
    '',
    renderTsRecord('dark', json.dark),
    '',
    'export const THEMES: Readonly<Record<ThemeMode, ThemeTokens>> = { light: LIGHT, dark: DARK }',
    '',
  ].join('\n')
}

// ─── tokens.css rendering ─────────────────────────────────────────────────

function renderCssBlock(themeName, tokens, headers) {
  const lines = []
  for (let g = 0; g < TOKEN_GROUPS.length; g++) {
    const group = TOKEN_GROUPS[g]
    const firstKey = group[0]
    const continuation = isSharedHeaderContinuation(firstKey)
    const header = headers[firstKey]
    if (g > 0) lines.push('')
    if (header && !continuation) {
      lines.push(`  ${header}`)
    }
    for (const key of group) {
      const value = tokens[key]
      if (value === undefined) continue
      if (key === 'borderStrong') {
        lines.push(`  ${PER_THEME_BORDER_STRONG_CSS_COMMENT[themeName]}`)
      }
      if (key === 'chart' && Array.isArray(value)) {
        for (let i = 0; i < value.length; i++) {
          lines.push(`  --color-chart-${i + 1}: ${value[i]};`)
        }
      } else {
        lines.push(`  ${cssVarName(key)}: ${value};`)
      }
    }
  }
  return lines.join('\n')
}

const CSS_HEADER = `/*
 * Design-token source of truth. Every color the UI renders resolves to a
 * var(--color-*) defined here. Two blocks: :root = light, :root[data-theme=dark]
 * = dark. Swapping happens via the data-theme attribute on <html>, set by the
 * FOUC script in index.html and kept in sync by useTheme().
 *
 * Component code references these via \`style={{ background: 'var(--color-bg-surface)' }}\`.
 * JS-side consumers (ReactFlow props, chart palettes) import the parallel
 * record from ./tokens.ts — both files are regenerated together.
 *
 * Generated by ui/scripts/gen-tokens.mjs from ui/src/styles/tokens.json
 * — do NOT edit by hand; \`scripts/check-tokens-in-sync.sh\`
 * fails CI on drift.
 */`

const CSS_FOOTER = `/* Document defaults. The inline <style> block in index.html sets
 * matching values for initial paint so there's no flash before this
 * stylesheet hits the cache. The transition smooths theme-toggle
 * clicks; the reduced-motion media query in index.html disables it
 * for users who opt out. */
html, body {
  background: var(--color-surface-0);
  color: var(--color-text-primary);
  transition: background-color 150ms ease, color 150ms ease;
}

:focus-visible {
  outline: 2px solid var(--color-focus-ring);
  outline-offset: 2px;
}

/* Never print a dark document. */
@media print {
  :root,
  :root[data-theme="dark"] {
    color-scheme: light;
    --color-surface-0: #ffffff;
    --color-surface-1: #ffffff;
    --color-text-primary: #000000;
  }
}

/*
 * DAG canvas cursor affordances.
 *
 * Task nodes signal "clickable" (pointer); empty canvas signals
 * "movable" (move arrows). Combined with \`nopan nodrag\` className on
 * TaskCard's inner button + e.stopPropagation() on pointerdown, this
 * stops the click→pan promotion entirely.
 *
 * Handles (the small connection dots React Flow renders at the
 * top/bottom of every node) are explicit no-pan/no-drag activation
 * targets in TaskCard. They keep pointer events so clicks that land on
 * a handle open the drawer instead of falling through to the pane.
 */
.react-flow__node,
.react-flow__node * {
  cursor: pointer !important;
}
.react-flow__pane,
.react-flow__pane.draggable {
  cursor: move !important;
}
.react-flow__pane.dragging {
  cursor: move !important;
}
.react-flow__handle {
  pointer-events: auto !important;
  cursor: pointer !important;
}
`

function renderCss(json) {
  return [
    CSS_HEADER,
    '',
    ':root {',
    '  color-scheme: light;',
    '',
    renderCssBlock('light', json.light, CSS_LIGHT_GROUP_HEADERS),
    '}',
    '',
    ':root[data-theme="dark"] {',
    '  color-scheme: dark;',
    '',
    renderCssBlock('dark', json.dark, CSS_DARK_GROUP_HEADERS),
    '}',
    '',
    CSS_FOOTER,
  ].join('\n')
}

// ─── main ────────────────────────────────────────────────────────────────

async function main() {
  const raw = await fs.readFile(SOURCE, 'utf8')
  const json = JSON.parse(raw)
  for (const theme of ['light', 'dark']) {
    if (!json[theme] || typeof json[theme] !== 'object') {
      throw new Error(`tokens.json missing required "${theme}" block`)
    }
  }
  await fs.writeFile(TS_OUT, renderTs(json))
  await fs.writeFile(CSS_OUT, renderCss(json))
  // eslint-disable-next-line no-console
  console.log(`tokens written: ${TS_OUT}, ${CSS_OUT}`)
}

main().catch((err) => {
  // eslint-disable-next-line no-console
  console.error(err)
  process.exit(1)
})
