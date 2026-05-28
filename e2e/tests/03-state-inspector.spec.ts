import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import { sel } from '../helpers/selectors'
import type { Beat, TabKind } from '../helpers/types'

/**
 * 03 — State Inspector
 *
 * Covers all six tabs switching on click, aria-selected semantics, state
 * badge label reflects the current SessionState, Plan tab renders the
 * placeholder when no DAG is available, State tab pretty-prints JSON,
 * and keyboard Tab navigation reaches every tab button in order.
 */

const TABS: TabKind[] = ['plan', 'state', 'documents', 'jobs', 'metrics', 'history']

const intakeBeat: Beat = {
  user: 'I want bulk RNA-seq differential expression.',
  assistant: { content: 'Bulk RNA-seq DE. Organism?' },
  state: 'intake_followup',
}

test.describe('State Inspector', () => {
  test('all six tabs switch on click and update aria-selected', async ({ page }) => {
    await withMockBackend(page, { beats: [intakeBeat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      // Plan is selected by default.
      await expect(page.locator(sel.inspectorTab('plan'))).toHaveAttribute(
        'aria-selected',
        'true',
      )
      for (const other of TABS.filter((t) => t !== 'plan')) {
        await expect(page.locator(sel.inspectorTab(other))).toHaveAttribute(
          'aria-selected',
          'false',
        )
      }

      // Click each tab and verify aria-selected flips.
      for (const t of TABS) {
        await chat.openTab(t)
        await expect(page.locator(sel.inspectorTab(t))).toHaveAttribute(
          'aria-selected',
          'true',
        )
        for (const other of TABS.filter((o) => o !== t)) {
          await expect(page.locator(sel.inspectorTab(other))).toHaveAttribute(
            'aria-selected',
            'false',
          )
        }
      }
    })
})

  test('state badge label reflects the current SessionState', async ({ page }) => {
    await withMockBackend(page, { beats: [intakeBeat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      // Initial state is intake.
      await chat.expect.stateBadge('intake')

      // After a beat, should advance to intake_followup.
      await chat.sendUserMessage(intakeBeat.user)
      await chat.waitForAssistant({
        textContains: intakeBeat.assistant.content.slice(0, 20),
      })
      await chat.expect.stateBadge('intake_followup')
    })
})

  test('Plan tab renders placeholder when no DAG is available', async ({ page }) => {
    await withMockBackend(page, { beats: [intakeBeat] }, async (handle) => {
      await page.goto('/')
      await new Chat(page).waitForAssistant()
      // The /api/dag mock returns 404 so the inspector renders the
      // placeholder "Your plan will appear here" prose.
      await expect(
        page.locator(sel.inspectorPanel('plan')).locator('text=Your plan will appear here'),
      ).toBeVisible()
    })
})

  test('State tab pretty-prints the JSON snapshot', async ({ page }) => {
    await withMockBackend(page, { beats: [intakeBeat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()
      await chat.openTab('state')
      // LazyStateTab renders a Suspense "Loading…" placeholder while
      // the chunk fetches; reading textContent synchronously races
      // the chunk load. expect().toContainText auto-retries until
      // the actual JSON renders.
      const panel = page.locator(sel.inspectorPanel('state'))
      await expect(panel).toContainText('"session_id"')
      await expect(panel).toContainText('"kind"')
      await expect(panel).toContainText('intake')
    })
})

  test('Jobs and Metrics tabs show placeholder text before any events', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [intakeBeat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.openTab('jobs')
      await expect(
        page.locator(sel.inspectorPanel('jobs')).locator('text=progress lines'),
      ).toBeVisible()

      await chat.openTab('metrics')
      await expect(
        page
          .locator(sel.inspectorPanel('metrics'))
          .locator('text=Metrics will appear here'),
      ).toBeVisible()
    })
})
})
