import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import { sel } from '../helpers/selectors'
import type { Beat, SseEvent } from '../helpers/types'

/**
 * 09 — Harness progress feed
 *
 * When the harness posts /progress events, the server broadcasts them
 * as SSE `harness_progress` payloads. `useSseChatEvents` accumulates
 * them and `StateInspectorPane` renders the Jobs tab with a live feed.
 *
 * First-ever event auto-switches the active tab to Jobs; the badge
 * counter increments on every new event; each kind renders with a
 * distinct background color and label.
 */

const beat: Beat = {
  user: 'Analyze some data.',
  assistant: { content: 'Ok, starting.' },
  state: 'emitted',
}

const progressEvents: SseEvent[] = [
  {
    type: 'harness_progress',
    kind: 'task_started',
    task_id: 'discover_datasets',
    status: 'running',
    detail: 'Looking up candidate datasets from GEO and ENA',
  },
  {
    type: 'harness_progress',
    kind: 'task_completed',
    task_id: 'discover_datasets',
    status: 'done',
    detail: 'Found 5 candidate datasets',
  },
  {
    type: 'harness_progress',
    kind: 'task_started',
    task_id: 'validate_ingest',
    status: 'running',
    detail: 'Validating ingest for the selected cohort',
  },
  {
    type: 'harness_progress',
    kind: 'task_failed',
    task_id: 'validate_ingest',
    status: 'error',
    detail: 'Ingest validation failed — schema mismatch',
  },
  {
    type: 'harness_progress',
    kind: 'task_blocked',
    task_id: 'align',
    status: 'blocked',
    detail: 'Blocked on SME input for reference genome',
  },
]

test.describe('Harness progress feed', () => {
  test('first harness_progress event auto-switches active tab to Jobs', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [beat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      // Plan tab is active initially.
      await expect(page.locator(sel.inspectorTab('plan'))).toHaveAttribute(
        'aria-selected',
        'true',
      )

      // Push first progress event.
      await handle.pushSseEvent(progressEvents[0])

      // Auto-switch to Jobs tab.
      await expect(page.locator(sel.inspectorTab('jobs'))).toHaveAttribute(
        'aria-selected',
        'true',
      )
    })
})

  test('Jobs badge increments on each new progress event', async ({ page }) => {
    await withMockBackend(page, { beats: [beat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      for (let i = 0; i < progressEvents.length; i += 1) {
        await handle.pushSseEvent(progressEvents[i])
        await chat.expect.jobsBadgeCount(i + 1)
      }

      // Final count matches the total pushed.
      await chat.expect.jobsBadgeCount(progressEvents.length)
    })
})

  test('Jobs feed renders an item per event with kind-specific label', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [beat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await handle.pushSseEvents(progressEvents)

      const feed = page.locator(sel.jobsFeed)
      await expect(feed).toBeVisible()

      // The labels used by StateInspectorPane's labelForKind.
      const expectedLabels = ['STARTED', 'DONE', 'STARTED', 'FAILED', 'BLOCKED']
      for (const label of expectedLabels) {
        // Multiple of some labels; just check the label count on screen.
        const matching = feed.locator(`text=${label}`)
        expect(await matching.count()).toBeGreaterThan(0)
      }

      // Task ids render as inline code.
      await expect(feed.locator('code').first()).toContainText('discover_datasets')
    })
})

  test('Jobs feed has role=log for screen reader announcements', async ({ page }) => {
    await withMockBackend(page, { beats: [beat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await handle.pushSseEvent(progressEvents[0])

      const feed = page.locator(sel.jobsFeed)
      await expect(feed).toHaveAttribute('role', 'log')
      await expect(feed).toHaveAttribute('aria-live', 'polite')
    })
})
})
