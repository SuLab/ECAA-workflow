import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import { sel } from '../helpers/selectors'
import type { Beat } from '../helpers/types'

/**
 * 06 — Error recovery
 *
 * Two failure modes:
 *  - `infra_error` SSE event → `InfraErrorBanner` with a Dismiss button.
 *  Clicking Dismiss hides the banner. The conversation stays usable.
 *  - `/turn` POST returning a 5xx → `useConversation` captures the error
 *  and renders a red alert box in the conversation pane. Retrying with
 *  the same or a new message recovers.
 */

const beat: Beat = {
  user: 'I want to analyze some data.',
  assistant: { content: 'What kind of data?' },
  state: 'intake_followup',
}

test.describe('Error recovery', () => {
  test('infra_error SSE shows banner, Dismiss hides it', async ({ page }) => {
    await withMockBackend(page, { beats: [beat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await handle.pushSseEvent({
        type: 'infra_error',
        reason: 'api_unreachable',
        user_copy: 'Cannot reach the assistant right now — please try again.',
      })

      // Banner uses the canonical COPY map for the "api_unreachable"
      // reason — title "The assistant is temporarily unavailable".
      const banner = page.locator(sel.infraBanner).first()
      await expect(banner).toBeVisible()
      await expect(banner).toContainText('temporarily unavailable')

      // Dismiss the banner via the accessible button.
      await page.locator(sel.infraDismissButton).click()
      await expect(page.locator(sel.infraBanner)).toHaveCount(0)
    })
})

  test('infra_error with unknown reason falls back to user_copy text', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [beat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await handle.pushSseEvent({
        type: 'infra_error',
        reason: 'unknown_reason_code',
        user_copy: 'Something specific went wrong — here is the user-facing copy.',
      })

      const banner = page.locator(sel.infraBanner).first()
      await expect(banner).toBeVisible()
      await expect(banner).toContainText('Something specific went wrong')
    })
})

  test('/turn POST 500 surfaces a red error alert in the conversation pane', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [beat, beat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      handle.failNextTurn(500)
      // Send a message — the mock will fail with 500 on this /turn POST.
      // useConversation catches the error and sets conv.error, which
      // renders a red alert via role="alert" in ConversationPane.
      const composer = page.locator(sel.composer)
      await composer.fill(beat.user)
      await composer.press('Enter')

      // Wait for the conversation pane error alert. It's distinct from
      // the blocker card (different aria-label) and from InfraErrorBanner
      // (no aria-live assertive on the wrapper).
      await expect(
        page.locator('role=alert').filter({ hasText: /500|mock \/turn failure/ }),
      ).toBeVisible({ timeout: 5000 })
    })
})

  test('retry after /turn failure succeeds and the conversation continues', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [beat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      handle.failNextTurn(500)

      await page.locator(sel.composer).fill(beat.user)
      await page.locator(sel.composer).press('Enter')
      // Wait for the error alert.
      await expect(
        page.locator('role=alert').filter({ hasText: /500|mock \/turn failure/ }),
      ).toBeVisible({ timeout: 5000 })

      // Retry — next /turn call will succeed with the beat's canned response.
      // useConversation.sendTurn reads `sending` first; since it flipped back
      // to false in the finally block after the 500, we can send again.
      await chat.sendUserMessage(beat.user)
      await chat.waitForAssistant({ textContains: 'What kind of data' })
    })
})
})
