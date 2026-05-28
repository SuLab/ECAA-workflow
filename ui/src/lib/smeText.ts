// SME-visible string sanitizer.
//
// Applied at the boundary where agent-provided prose (reason strings,
// recovery hints, narrative turns) lands in a card. Internal vocabulary
// and runtime-path fragments that leak into those strings get translated or
// stripped before rendering so the SME never sees `discover_*` / `validate_*`
// IDs, "harness"/"executor" jargon, or `runtime/outputs/...` paths.
//
// UI-owned hardcoded strings (card labels, button copy, fixed prose) should
// be written correctly in the first place and do NOT need to pass through
// this sanitizer — the linter in `ui/src/__tests__/sme-copy-linter.test.tsx`
// asserts they are clean as authored.

import { stageIdToLabel } from './stageLabels'

// Case-insensitive replacements for executor-layer / UI-chrome vocabulary
// that has no SME-readable analog. Matched on word boundaries so we don't
// turn "hardware" into "sysstemware" or similar.
//
// Replacements are passed through `preserveCase` so a sentence-initial
// "Harness" → "System" instead of "system" — keeps grammar clean when
// SME-visible prose starts with one of these words.
const WORD_REPLACEMENTS: Array<[RegExp, string]> = [
  [/\bharness\b/gi, 'system'],
  [/\bexecutor\b/gi, 'system'],
  [/\btool call(s)?\b/gi, 'action$1'],
  [/\btool_call(s)?\b/gi, 'action$1'],
  [/\bemit history\b/gi, 'history'],
  [/\bJobs tab\b/g, 'progress panel'],
  [/\bState tab\b/g, 'status panel'],
]

function preserveCase(match: string, replacement: string): string {
  if (!match || !replacement) return replacement
  // If the original starts with an uppercase letter, capitalize the
  // replacement so sentence-initial replacements don't lowercase mid-text.
  const first = match.charCodeAt(0)
  if (first >= 65 && first <= 90 /* A-Z */) {
    return replacement.charAt(0).toUpperCase() + replacement.slice(1)
  }
  return replacement
}

// Stage-ID prefix pattern — catches `discover_X`, `validate_X`, `select_X`
// appearing in otherwise free-form prose and routes them through
// stageIdToLabel so the SME sees "Normalization" instead of
// "discover_normalization". Kept separate from WORD_REPLACEMENTS because
// the replacement is computed, not a literal.
const STAGE_ID_PATTERN = /\b(discover|validate|select)_[a-z][a-z0-9_]*/g

// Runtime-path fragments the SME should never see. `runtime/` and
// `results/tables/` are internal paths; we mention the artifact by purpose
// rather than path. Matches a whole path-like token (no whitespace).
const RUNTIME_PATH_PATTERN = /\b(runtime|results)\/[^\s)]+/g

/**
 * Translate internal vocabulary in a free-form SME-visible string into
 * plain English. Safe to call on any agent-provided prose; idempotent
 * (running it twice produces the same output as running it once).
 */
export function sanitizeForSme(text: string): string {
  if (!text) return text
  let out = text
  // Pass 1: translate stage-IDs (needs the computed replacement).
  out = out.replace(STAGE_ID_PATTERN, (match) => stageIdToLabel(match))
  // Pass 2: strip runtime-path fragments.
  out = out.replace(RUNTIME_PATH_PATTERN, 'the result file')
  // Pass 3: word-level replacements. Capitalization preservation
  // applies only to case-insensitive patterns (`/i` flag) — those
  // capture both `harness` and `Harness`, and we want `Harness` →
  // `System` not `system` so sentence-initial prose stays grammatical.
  // Case-sensitive patterns (`Jobs tab`, `State tab`) are intentional
  // proper-noun forms whose replacement is authored as-intended.
  for (const [pattern, replacement] of WORD_REPLACEMENTS) {
    if (pattern.flags.includes('i')) {
      out = out.replace(pattern, (match, ...groups) => {
        let resolved = replacement
        for (let i = 0; i < groups.length - 2; i++) {
          const group = groups[i] ?? ''
          resolved = resolved.replace(new RegExp(`\\$${i + 1}`, 'g'), group)
        }
        return preserveCase(match, resolved)
      })
    } else {
      out = out.replace(pattern, replacement)
    }
  }
  return out
}
