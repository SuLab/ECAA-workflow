// SME-copy linter gate.
//
// Renders each SME-facing component with canonical props and asserts the
// rendered HTML contains none of the forbidden tokens. Adding a new SME-
// facing component means: 1) add it to CANONICAL_RENDERS below with a
// representative props fixture; 2) if the component surfaces a user-visible
// string that legitimately contains one of the forbidden substrings,
// thread it through `ui/src/lib/smeText.ts::sanitizeForSme` before render.
//
// This linter scans the DOM after render, not the component source. It
// catches agent-provided prose leaks as well as hardcoded authoring
// Mistakes — the two ways jargon was leaking before the sanitizer.

import { describe, expect, it } from 'vitest'
import { render } from '@testing-library/react'
import type { ReactElement } from 'react'

import ConfirmationTurnCard from '../components/ConfirmationTurnCard'
import BranchFromHereCard from '../components/BranchFromHereCard'
import SensitivityComparisonCard from '../components/SensitivityComparisonCard'
import ResultReviewTurnCard, {
  type ResultReviewPayload,
} from '../components/ResultReviewTurnCard'
import BlockerCard from '../components/BlockerCard'
import type { BlockerKind } from '../types'
import { SessionTestWrapper } from '../test/sessionTestHelpers'

// ── Forbidden-token inventory ────────────────────────────────────────────
//
// Substrings that must not appear in the rendered SME-visible HTML. Each
// row is [label, regex]; the label is used for a human-readable failure
// message. We use word-boundary regexes where loose matches would produce
// false positives (e.g. "harness" the word vs. a mention inside a
// data-attribute name), and literal matches where uniqueness is already
// guaranteed (e.g. "Jobs tab" is always a jargon leak in SME-visible
// prose).

interface ForbiddenToken {
  label: string
  pattern: RegExp
}

const FORBIDDEN: ForbiddenToken[] = [
  { label: 'discover_* stage-ID prefix', pattern: /\bdiscover_[a-z][a-z0-9_]*\b/ },
  { label: 'validate_* stage-ID prefix', pattern: /\bvalidate_[a-z][a-z0-9_]*\b/ },
  {
    // select_ is also an English verb when written as "select a method" —
    // allow it unless followed by a stage-name-like token.
    label: 'select_* stage-ID prefix',
    pattern: /\bselect_[a-z][a-z0-9_]*\b/,
  },
  { label: '"harness" (executor jargon)', pattern: /\bharness\b/i },
  { label: '"executor" (executor jargon)', pattern: /\bexecutor\b/i },
  { label: '"emit history" (internal vocabulary)', pattern: /\bemit history\b/i },
  { label: '"tool call" / "tool_call"', pattern: /\btool[\s_]call(s)?\b/i },
  { label: '"Jobs tab" (UI chrome name)', pattern: /\bJobs tab\b/ },
  { label: '"State tab" (UI chrome name)', pattern: /\bState tab\b/ },
  { label: '"Metrics tab" (UI chrome name)', pattern: /\bMetrics tab\b/ },
  { label: '"discovery decisions" (deprecated SME phrasing)', pattern: /\bdiscovery decisions\b/i },
  { label: 'runtime/ path fragment', pattern: /\bruntime\/[A-Za-z0-9._/-]+/ },
  { label: 'results/tables/ path fragment', pattern: /\bresults\/tables\/[A-Za-z0-9._/-]+/ },
]

// Pull the rendered text out of the DOM. We strip data-attribute values
// (components intentionally expose raw IDs via `data-task-id`, `data-stage-id`
// title-attr tooltips so e2e selectors can still anchor on them), because
// SMEs don't see those — only the inner-HTML text matters.
function smeVisibleText(container: HTMLElement): string {
  return container.textContent ?? ''
}

interface CanonicalRender {
  name: string
  render: () => ReactElement
}

// ── Canonical props per component ────────────────────────────────────────
//
// Each render is intentionally stressful: the prop fixtures include stage
// IDs, runtime paths, and executor vocabulary in the free-form fields.
// A clean component must sanitize all of them away before rendering.

const baseResult: ResultReviewPayload = {
  task_id: 'discover_normalization',
  status: 'failed',
  description: 'Normalization method selection',
  kind: { type: 'computation' } as never,
  reason:
    'The harness ran into trouble on validate_qc; see runtime/outputs/discover_normalization/decision.json for the decision.',
}

const blockerKindDataShape: BlockerKind = {
  kind: 'data_shape_mismatch',
  task_id: 'discover_normalization',
  details: 'The harness could not align validate_qc outputs.',
} as never

const CANONICAL_RENDERS: CanonicalRender[] = [
  {
    name: 'ConfirmationTurnCard',
    render: () => (
      <SessionTestWrapper>
        <ConfirmationTurnCard
          card={{ summary_markdown: 'Here\'s the plan for 47 single-cell libraries.', summary_hash: 'a'.repeat(64) }}
          onConfirm={() => {}}
          onReject={() => {}}
        />
      </SessionTestWrapper>
    ),
  },
  {
    name: 'BranchFromHereCard',
    render: () => <BranchFromHereCard onBranch={() => {}} />,
  },
  {
    name: 'SensitivityComparisonCard (methods mode, stressful stage id)',
    render: () => (
      <SensitivityComparisonCard
        stage="discover_batch_correction"
        candidates={['harmony', 'scvi', 'scanorama']}
        onSelect={() => {}}
      />
    ),
  },
  {
    name: 'SensitivityComparisonCard (empty-state)',
    render: () => (
      <SensitivityComparisonCard
        stage="discover_integration"
        candidates={[]}
        onSelect={() => {}}
      />
    ),
  },
  {
    name: 'ResultReviewTurnCard (failure with jargon-rich reason)',
    render: () => <ResultReviewTurnCard payload={baseResult} />,
  },
  {
    name: 'BlockerCard (data-shape with jargon-rich reason)',
    render: () => (
      <BlockerCard
        reason="The harness reported validate_qc failed; see runtime/outputs/validate_qc/log.jsonl"
        recoveryHint="Re-run the executor after fixing the input shape."
        onUnblock={() => {}}
        blockerKind={blockerKindDataShape}
      />
    ),
  },
]

describe('SME-copy linter — rendered HTML is free of internal vocabulary', () => {
  for (const { name, render: makeElement } of CANONICAL_RENDERS) {
    for (const { label, pattern } of FORBIDDEN) {
      it(`${name} — no ${label}`, () => {
        const { container, unmount } = render(makeElement())
        const text = smeVisibleText(container)
        const match = text.match(pattern)
        if (match) {
          // Include a helpful snippet (up to 200 chars of surrounding
          // context) so a failure points at the offending string directly.
          const idx = text.indexOf(match[0])
          const start = Math.max(0, idx - 60)
          const end = Math.min(text.length, idx + match[0].length + 60)
          const snippet = text.slice(start, end)
          throw new Error(
            `Forbidden token ${label} found in ${name}:\n  match: ${JSON.stringify(match[0])}\n  context: …${snippet}…`,
          )
        }
        unmount()
        expect(match).toBeNull()
      })
    }
  }
})
