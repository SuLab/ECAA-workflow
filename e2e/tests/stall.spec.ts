import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import { sel } from '../helpers/selectors'
import type { Beat, SessionState } from '../helpers/types'

/**
 * Mocked stall-recovery spec. Exercises the UI-side of the stall pathway:
 *
 *  1. When `SessionState.state` is `blocked` with a typed
 *  `blocker_kind.kind === 'stalled'`, `BlockerCard` renders three
 *  resolution buttons (Resize / Retry / Abort) each carrying a
 *  `data-resolution=` attribute.
 *  2. Clicking Resize posts to `/unblock` with
 *  `{resolution: "resize"}`. Verified via a `page.route`
 *  interceptor because the default mock backend treats /unblock
 *  as a fire-and-forget no-content.
 *  3. An injected `harness_stall_detected` SSE event lands as a
 *  `[data-stall-chip]` span in the Jobs tab.
 */

const stalledState: SessionState = {
  kind: 'blocked',
  reason:
    'Task align_sample_03 has been running with near-zero CPU for 30 minutes.',
  recovery_hint: 'Consider resizing or retrying.',
  blocker_kind: {
    kind: 'stalled',
    task_id: 'align_sample_03',
    signal: {
      kind: 'cpu_starvation',
      avg_cpu_pct: 1.8,
      window_mins: 30,
    },
    suggested_action: 'resize',
  },
}

const stallBeat: Beat = {
  user: 'What is the task status?',
  assistant: { content: 'Task align_sample_03 looks stalled — see the blocker.' },
  state: stalledState,
}

test.describe('Stall recovery', () => {
  test('BlockerCard renders Resize/Retry/Abort buttons for stalled variant', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [stallBeat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()
      await chat.sendUserMessage(stallBeat.user)
      await chat.waitForAssistant({ textContains: 'stalled' })

      await chat.expect.blockerVisible()
      const card = page.locator(sel.blockerCard)
      await expect(card).toBeVisible()

      // All three resolution buttons are always offered.
      await expect(card.locator('[data-resolution="resize"]')).toBeVisible()
      await expect(card.locator('[data-resolution="retry"]')).toBeVisible()
      await expect(card.locator('[data-resolution="abort"]')).toBeVisible()

      // The suggested_action is highlighted as default.
      await expect(card.locator('[data-resolution="resize"]')).toHaveAttribute(
        'data-default-resolution',
        'true',
      )
    })
})

  test('clicking Resize POSTs /unblock with resolution=resize', async ({ page }) => {
    // Intercept /unblock specifically so we can capture the posted body.
    // The mockBackend's default /unblock handler returns 204 no content;
    // we pre-empt it with a more specific route. Playwright runs handlers
    // most-recently-registered first, so this MUST be registered after
    // withMockBackend installs its catch-all — otherwise the catch-all
    // wins and our interceptor never sees the request.
    const unblockBodies: string[] = []

    await withMockBackend(page, {
      beats: [stallBeat],
      unblockTarget: { kind: 'intake_followup' },
    }, async (handle) => {
      await page.route(/\/api\/(?:v1\/)?chat\/session\/[^/]+\/unblock$/, async (route) => {
        const body = route.request().postData()
        if (body) unblockBodies.push(body)
        await route.fulfill({ status: 204, body: '' })
      })
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()
      await chat.sendUserMessage(stallBeat.user)
      await chat.waitForAssistant({ textContains: 'stalled' })

      await chat.expect.blockerVisible()
      await page.locator(sel.blockerCard).locator('[data-resolution="resize"]').click()

      // At least one /unblock body should carry the resize resolution.
      await expect
        .poll(() => unblockBodies.length, { timeout: 3000 })
        .toBeGreaterThan(0)
      const found = unblockBodies.some((b) => {
        try {
          const j = JSON.parse(b) as { resolution?: string | null }
          return j.resolution === 'resize'
        } catch {
          return false
        }
      })
      expect(found).toBe(true)
    })
})

  test('Jobs tab renders stall chip after harness_stall_detected SSE', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [stallBeat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      // Emit a harness_progress for the stalled task so it has a card in
      // the Jobs feed — the chip renders inline next to the task's card.
      await handle.pushSseEvent({
        type: 'harness_progress',
        kind: 'task_started',
        task_id: 'align_sample_03',
        status: 'running',
        detail: 'Aligning sample 03',
      })

      // Emit the stall signal itself — the UI stores this keyed by
      // task_id and renders the chip in the Jobs tab.
      await handle.pushSseEvent({
        type: 'harness_stall_detected',
        task_id: 'align_sample_03',
        signal: {
          kind: 'cpu_starvation',
          avg_cpu_pct: 1.8,
          window_mins: 30,
        },
        suggested_action: 'resize',
      })

      await chat.openTab('jobs')
      const chip = page.locator('[data-stall-chip="align_sample_03"]')
      await expect(chip).toBeVisible({ timeout: 5000 })
      await expect(chip).toContainText(/stall/i)
    })
})
})
