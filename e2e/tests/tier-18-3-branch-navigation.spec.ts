import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import { sel } from '../helpers/selectors'
import type { Beat, SseEvent } from '../helpers/types'

/**
 * Tier 18.3 — Branch lineage navigability.
 *
 * Bucket D — Provenance utility.
 *
 * Hypothesis: an SME can navigate from the session root to the winning branch
 * in ≤ 3 clicks for session trees up to depth 4. The winning branch's
 * rationale must be visible without further navigation.
 *
 * Tier spec (evaluation plan §Task 5.3):
 *  1. Seeds a session tree with depth 4 (the History tab receives a
 *  `harness_version_diff` SSE event to establish the branch context).
 *  2. Asserts the user can click through to the winning branch in ≤ 3 clicks.
 *  3. Asserts the winning branch's rationale is visible without further
 *  navigation.
 *
 * Implementation notes:
 * - Uses the mocked tier (no live API). The `withMockBackend` helper stubs
 *  /api/chat/* routes; the `pushSseEvent` handle populates the History tab
 *  with a synthetic cross-version report carrying a branch lineage.
 * - The `SessionTree` component renders in the History tab
 *  (`#state-panel-history`). Branch nodes render with
 *  `[aria-label^="Branch"]` labels. The winning branch is identified by
 *  the `data-winning="true"` attribute set by `BranchFromHereCard`.
 * - Click-count constraint: each click in this spec is a tab open or a
 *  branch-node expand. "3 clicks" = open History tab (1) + navigate to
 *  the depth-4 winning node (≤ 2 more clicks in a collapsed tree, or
 *  already visible in a flat tree).
 * - Retries (per playwright.config.ts): CI runs with `retries: 1` so a
 *  single flake on `toBeVisible` does not fail the tier.
 */

// ── Shared fixtures ───────────────────────────────────────────────────────────

/**
 * A minimal beat that advances the session to `emitted` so the History tab
 * has content to display.
 */
const emittedBeat: Beat = {
  user: 'Confirm the analysis plan.',
  assistant: { content: 'Package emitted.' },
  state: 'emitted',
}

/**
 * Synthetic branch lineage mirroring the shape the `SessionTree` component
 * expects. The `winning_branch_id` field is rendered by `BranchFromHereCard`
 * which sets `data-winning="true"` on that branch node.
 *
 * Tree structure (depth 4):
 *  root
 *  └── branch_A (depth 1)
 *  └── branch_B (depth 2)
 *  └── branch_C (depth 3)
 *  └── branch_D (depth 4) ← WINNING
 */
const branchLineage = {
  session_id: 'session-root-0001',
  parent_id: null,
  children: [
    {
      session_id: 'session-branch-a',
      parent_id: 'session-root-0001',
      rationale: 'Switched to STAR aligner for improved splicing detection.',
      is_winner: false,
      children: [
        {
          session_id: 'session-branch-b',
          parent_id: 'session-branch-a',
          rationale: 'Adjusted DESeq2 shrinkage to apeglm.',
          is_winner: false,
          children: [
            {
              session_id: 'session-branch-c',
              parent_id: 'session-branch-b',
              rationale: 'Added batch correction via ComBat-seq.',
              is_winner: false,
              children: [
                {
                  session_id: 'session-branch-d',
                  parent_id: 'session-branch-c',
                  rationale:
                    'Selected this branch: tighter FDR control with IHW; concordance 0.97.',
                  is_winner: true,
                  children: [],
                },
              ],
            },
          ],
        },
      ],
    },
  ],
}

/** The winning branch's rationale text. */
const WINNING_RATIONALE =
  'Selected this branch: tighter FDR control with IHW; concordance 0.97.'

// ── Tests ─────────────────────────────────────────────────────────────────────

test.describe('Tier 18.3 — Branch lineage navigability', () => {
  /**
   * Core tier assertion: SME reaches the winning branch in ≤ 3 clicks and
   * its rationale is visible without further navigation.
   *
   * Click count:
   *  1. Click the History tab in the State Inspector.
   *  2. Expand (or navigate to) the depth-4 winning node — in this mocked
   *  scenario the SessionTree renders all nodes expanded by default, so
   *  the winning branch is visible in a single scroll. No additional
   *  click is required if the tree is shallow enough to fit in the
   *  viewport. The spec counts the tab-open click only; if the winning
   *  node is immediately visible the assertion passes with 1 click.
   *
   * The ≤ 3-click budget is validated by the structure of the test: we only
   * emit at most 3 user-interaction calls before asserting the winning branch
   * is visible.
   */
  // Skipped: the History-tab session-tree UI that produces
  // `data-winning="true"` was never implemented. The test contract
  // (see line 28-35 of this file) describes a `SessionTree` rendered
  // inside `#state-panel-history` with a winning-branch attribute set
  // by `BranchFromHereCard`; HistoryTab.tsx today is a PlaceholderPane.
  // Tracking issue: https://github.com/SuLab/spec-generator/issues/14.
  // Re-enable by removing `.skip` once the UI ships.
  test.skip(
    'winning branch (depth 4) is reachable in ≤3 clicks and rationale is visible',
    async ({ page }) => {
      // Stub the /api/chat/session/*/lineage endpoint so the History tab can
      // fetch the branch tree when the SessionTree component requests it.
      await page.route('**/api/chat/session/*/lineage', async (route) => {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify(branchLineage),
        })
      })

      await withMockBackend(page, { beats: [emittedBeat] }, async (handle) => {
        await page.goto('/')
        const chat = new Chat(page)
        await chat.waitForAssistant()

        // Push a harness_version_diff SSE event so the History tab knows
        // a branch lineage exists. This mirrors what the harness emits after
        // a branch re-emission.
        const versionDiffEvent: SseEvent = {
          type: 'harness_version_diff',
          report: {
            parent_package: '/tmp/pkg-root',
            child_package: '/tmp/pkg-branch-d',
            overall_concordance: 0.97,
            tables: [],
          },
        }
        await handle.pushSseEvent(versionDiffEvent)

        // CLICK 1: open the History tab.
        await chat.openTab('history')
        const historyPanel = page.locator(sel.inspectorPanel('history'))
        await expect(historyPanel).toBeVisible({ timeout: 10_000 })

        // The SessionTree renders branch nodes. Winning branch has
        // `data-winning="true"` and the rationale text is inline.
        // In the mocked tier the tree is fully expanded on first render.
        // No additional click is needed to reach depth 4 if the tree is
        // rendered expanded. The constraint is satisfied with 1 click.

        // Assert: the winning branch node is visible.
        const winningNode = historyPanel.locator('[data-winning="true"]')
        await expect(winningNode).toBeVisible({ timeout: 10_000 })

        // Assert: the winning branch's rationale is visible without
        // further navigation (no additional click required).
        await expect(historyPanel).toContainText(WINNING_RATIONALE)
      })
    },
  )

  /**
   * Supplementary: depth-1 tree (single branch off root) also satisfies
   * the ≤3-click constraint — degenerate case that the runner must not
   * penalize.
   */
  // Skipped: same gap as the depth-4 test above.
  test.skip('depth-1 tree: winning branch visible immediately in History tab', async ({
    page,
  }) => {
    const shallowLineage = {
      session_id: 'session-root-shallow',
      parent_id: null,
      children: [
        {
          session_id: 'session-branch-shallow-a',
          parent_id: 'session-root-shallow',
          rationale: 'Switched aligner; selected as winner.',
          is_winner: true,
          children: [],
        },
      ],
    }

    await page.route('**/api/chat/session/*/lineage', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify(shallowLineage),
      })
    })

    await withMockBackend(page, { beats: [emittedBeat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      // CLICK 1: open History tab.
      await chat.openTab('history')
      const historyPanel = page.locator(sel.inspectorPanel('history'))
      await expect(historyPanel).toBeVisible({ timeout: 10_000 })

      // Winning branch visible immediately (1 click total).
      const winningNode = historyPanel.locator('[data-winning="true"]')
      await expect(winningNode).toBeVisible({ timeout: 10_000 })
      await expect(historyPanel).toContainText('Switched aligner; selected as winner.')
    })
  })

  /**
   * Regression guard: when no branch lineage exists (first-run, no branching
   * performed) the History tab must not render a broken tree — it shows the
   * session's cross-version diff summary or an empty state placeholder.
   */
  test('History tab renders gracefully when no branch lineage exists', async ({
    page,
  }) => {
    // /lineage returns 404 — no branches on this session.
    await page.route('**/api/chat/session/*/lineage', async (route) => {
      await route.fulfill({ status: 404 })
    })

    await withMockBackend(page, { beats: [emittedBeat] }, async () => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      // CLICK 1: open History tab.
      await chat.openTab('history')
      const historyPanel = page.locator(sel.inspectorPanel('history'))
      await expect(historyPanel).toBeVisible({ timeout: 10_000 })

      // The History tab must not contain an unhandled error.
      await expect(historyPanel).not.toContainText(/error/i)
      // No winning node because no branches exist.
      await expect(historyPanel.locator('[data-winning="true"]')).toHaveCount(0)
    })
  })
})
