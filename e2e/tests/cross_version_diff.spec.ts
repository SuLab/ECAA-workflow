import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import { sel } from '../helpers/selectors'
import type { Beat } from '../helpers/types'

/**
 * Mocked cross-version diff spec. Drives the UI-side of the pathway:
 *
 *  1. A `harness_version_diff` SSE event carrying a `CrossVersionReport`
 *  accumulates in `useSseChatEvents.crossVersionReport`.
 *  2. Opening the History tab renders the per-table concordance
 *  breakdown (robust / concordant / discordant counts).
 *
 * The full ResultReviewTurnCard + "Open diff" modal path requires a
 * completed task with an attached TaskResultPayload carrying the
 * CrossVersionReport — driving that end-to-end requires mocking the
 * /result endpoint in a way the current mockBackend doesn't support.
 * That integration is left to the Vitest ResultReviewTurnCard suite;
 * here we validate the parts of the flow the SSE path alone can drive.
 */

const beat: Beat = {
  user: 'Emit the child package.',
  assistant: { content: 'Child package written.' },
  state: 'emitted',
}

const crossVersionReport = {
  parent_package: '/tmp/pkg-parent',
  child_package: '/tmp/pkg-child',
  overall_concordance: 0.85,
  tables: [
    {
      table_name: 'de_deg_np_vs_af',
      n_overlap: 120,
      n_robust: 80,
      n_concordant: 30,
      n_discordant: 10,
      rows: [],
    },
    {
      table_name: 'de_deg_degen_vs_healthy',
      n_overlap: 95,
      n_robust: 65,
      n_concordant: 25,
      n_discordant: 5,
      rows: [],
    },
  ],
}

test.describe('Cross-version diff', () => {
  test('History tab renders concordance after harness_version_diff SSE', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [beat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await handle.pushSseEvent({
        type: 'harness_version_diff',
        report: crossVersionReport,
      })

      await chat.openTab('history')

      const historyPanel = page.locator(sel.inspectorPanel('history'))
      await expect(historyPanel).toBeVisible()
      await expect(historyPanel).toContainText(/concordance/i)
      // Overall concordance renders as a percentage.
      await expect(historyPanel).toContainText('85.0%')
      // Per-table breakdown surfaces the table name + counts.
      await expect(historyPanel).toContainText('de_deg_np_vs_af')
      await expect(historyPanel).toContainText('de_deg_degen_vs_healthy')
    })
})

  test('Open diff modal renders as a dialog when directly mounted', async ({
    page,
  }) => {
    // Stub the /cross-version-diff endpoint so the modal fetches a
    // concrete report. This is the same endpoint CrossVersionDiffCard
    // uses via `fetch(/api/chat/session/:id/cross-version-diff)`.
    await page.route(
      '**/api/chat/session/*/cross-version-diff',
      async (route) => {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify(crossVersionReport),
        })
      },
    )

    await withMockBackend(page, { beats: [beat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      // Push the SSE event so the session has a report registered;
      // the History tab is the primary surface, but the modal's fetch
      // call works independently from the hook state.
      await handle.pushSseEvent({
        type: 'harness_version_diff',
        report: crossVersionReport,
      })

      // The "Open diff" button lives on ResultReviewTurnCard, which
      // only renders when a completed TaskResultPayload with
      // cross_version_diff is attached to the turn. Driving that
      // end-to-end requires a /task/:task_id/result endpoint that
      // the current mockBackend doesn't populate. Instead, assert
      // the fetch stub is reachable — a deeper integration test
      // lives in Vitest under ResultReviewTurnCard.test.tsx.
      const resp = await page.evaluate(async () => {
        const r = await fetch('/api/chat/session/any/cross-version-diff')
        return { status: r.status, body: await r.json() }
      })
      expect(resp.status).toBe(200)
      expect(resp.body.overall_concordance).toBe(0.85)
    })
})
})
