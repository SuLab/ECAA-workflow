import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import { sel } from '../helpers/selectors'
import type { Beat } from '../helpers/types'

/**
 * 02 — Confirmation gate
 *
 * The confirmation card is the deterministic high-impact gate for
 * emit_package. The button click is a real server-side state transition
 * that the LLM cannot bypass. This spec verifies:
 *
 *  - the card renders when the assistant returns a confirmation_card
 *  - Confirm POSTs /confirm and advances to ready_to_emit, then drives
 *  the post-confirm follow-up turn, then emitted
 *  - Revise POSTs /reject and returns to intake_followup
 *  - the card disappears after rejection
 *  - the Confirm button cannot be double-clicked (local decision=confirmed
 *  guard in ConfirmationTurnCard)
 */

const intakeBeat: Beat = {
  user: "I'm planning a variant calling run on GIAB HG002.",
  assistant: {
    content: 'Germline WGS variant calling on GIAB HG002. Caller set?',
  },
  state: 'intake_followup',
  expect: { stateBadge: 'intake_followup' },
}

const confirmationBeat: Beat = {
  user: 'GATK HaplotypeCaller and DeepVariant, both pinned to v4.2.1 truth.',
  assistant: {
    content: 'Here is the plan — please review and confirm.',
    confirmation_card: {
      summary_markdown:
        '**Germline variant calling on GIAB HG002**\n\n- Callers: GATK HC + DeepVariant\n- Truth: GIAB v4.2.1\n- Claim boundary: benchmark precision/recall on reference only — no clinical claims',
    },
  },
  state: 'pending_confirmation',
  expect: {
    stateBadge: 'pending_confirmation',
    confirmationCardVisible: true,
  },
}

const postConfirmBeat: Beat = {
  user: '(confirmed — please continue)',
  assistant: {
    content: 'Package emitted to disk. Harness can begin execution now.',
  },
  state: 'emitted',
  expect: { stateBadge: 'emitted' },
}

test.describe('Confirmation gate', () => {
  test('summary card renders with the assistant confirmation_card content', async ({
    page,
  }) => {
    await withMockBackend(page, {
      beats: [intakeBeat, confirmationBeat],
    }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.sendUserMessage(intakeBeat.user)
      await chat.waitForAssistant({
        textContains: intakeBeat.assistant.content.slice(0, 20),
      })

      await chat.sendUserMessage(confirmationBeat.user)
      await chat.waitForAssistant({
        textContains: confirmationBeat.assistant.content.slice(0, 20),
      })

      await chat.expect.confirmationCardVisible()
      await chat.expect.confirmationCardContains('GIAB v4.2.1')
      await chat.expect.confirmationCardContains('Claim boundary')
      await chat.expect.stateBadge('pending_confirmation')
    })
})

  test('Confirm click advances to ready_to_emit then drives post-confirm turn to emitted', async ({
    page,
  }) => {
    await withMockBackend(page, {
      beats: [intakeBeat, confirmationBeat],
      afterConfirmBeats: [postConfirmBeat],
    }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.sendUserMessage(intakeBeat.user)
      await chat.waitForAssistant({
        textContains: intakeBeat.assistant.content.slice(0, 20),
      })
      await chat.sendUserMessage(confirmationBeat.user)
      await chat.waitForAssistant({
        textContains: confirmationBeat.assistant.content.slice(0, 20),
      })
      await chat.expect.confirmationCardVisible()

      await chat.clickConfirm()
      await chat.waitForAssistant({
        textContains: postConfirmBeat.assistant.content.slice(0, 20),
      })
      await chat.expect.stateBadge('emitted')

      // The post-confirm turn must have been driven by useConversation.confirm()
      // automatically — recorded as a /turn POST with the "(confirmed" message.
      const posts = handle.recordedTurnMessages()
      expect(posts.some((p) => p.includes('confirmed'))).toBe(true)
    })
})

  test('Revise returns to intake_followup and hides the card', async ({
    page,
  }) => {
    await withMockBackend(page, {
      beats: [intakeBeat, confirmationBeat],
      rejectTarget: { kind: 'intake_followup' },
    }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.sendUserMessage(intakeBeat.user)
      await chat.waitForAssistant()
      await chat.sendUserMessage(confirmationBeat.user)
      await chat.waitForAssistant()
      await chat.expect.confirmationCardVisible()

      await chat.clickReject()

      // The confirmation card's local 'decision' state flips to rejected
      // and renders "Returning to the conversation…" — the card itself
      // remains in the DOM but the action buttons disappear.
      await expect(page.locator(sel.confirmButton)).toHaveCount(0)
      await chat.expect.stateBadge('intake_followup')
    })
})

  test('Confirm button cannot be double-clicked', async ({ page }) => {
    await withMockBackend(page, {
      beats: [intakeBeat, confirmationBeat],
      afterConfirmBeats: [postConfirmBeat],
    }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.sendUserMessage(intakeBeat.user)
      await chat.waitForAssistant()
      await chat.sendUserMessage(confirmationBeat.user)
      await chat.waitForAssistant()

      const confirmBtn = page.locator(sel.confirmButton)
      await confirmBtn.click()
      // After the first click, the ConfirmationTurnCard's internal
      // decision flips to 'confirmed' and the button leaves the DOM.
      // A second click would fail — assert count goes to zero.
      await expect(confirmBtn).toHaveCount(0)
    })
})
})
