import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import { sel } from '../helpers/selectors'
import type { Beat } from '../helpers/types'

/**
 * 07 — Blocker recovery
 *
 * When SessionState is blocked, ConversationPane renders BlockerCard
 * with the reason, recovery_hint, and an "I've addressed this — continue"
 * button. Clicking the button POSTs /unblock, the mock advances the
 * state, and the card disappears.
 */

const intakeBeat: Beat = {
  user: 'I want to analyze MIMIC-IV ICU data for sepsis prediction.',
  assistant: {
    content:
      'MIMIC-IV is credentialed-access on PhysioNet — I need you to confirm you have the DUA before we continue.',
  },
  state: {
    kind: 'blocked',
    reason:
      'MIMIC-IV is credentialed-access. Please confirm you have PhysioNet DUA and CITI training.',
    recovery_hint:
      'Once you have confirmed access, click the button below to continue.',
  },
}

test.describe('Blocker recovery', () => {
  test('BlockerCard renders with reason and recovery hint when state is blocked', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [intakeBeat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.sendUserMessage(intakeBeat.user)
      await chat.waitForAssistant({
        textContains: intakeBeat.assistant.content.slice(0, 20),
      })

      await chat.expect.blockerVisible()
      await expect(page.locator(sel.blockerCard)).toContainText(
        'credentialed-access',
      )
      await expect(page.locator(sel.blockerCard)).toContainText(
        'Once you have confirmed access',
      )
      await chat.expect.stateBadge('blocked')
    })
})

  test('clicking unblock button POSTs /unblock and dismisses the card', async ({
    page,
  }) => {
    await withMockBackend(page, {
      beats: [intakeBeat],
      unblockTarget: { kind: 'intake_followup' },
    }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.sendUserMessage(intakeBeat.user)
      await chat.waitForAssistant({
        textContains: intakeBeat.assistant.content.slice(0, 20),
      })

      await chat.expect.blockerVisible()

      await chat.clickUnblock()

      // After unblock, useConversation.unblock() triggers refreshState which
      // hits the mock /state endpoint, now returning intake_followup.
      await chat.expect.stateBadge('intake_followup')
      await chat.expect.blockerHidden()
    })
})

  test('BlockerCard has role=alert for screen-reader announcement', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [intakeBeat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.sendUserMessage(intakeBeat.user)
      await chat.waitForAssistant()

      const card = page.locator(sel.blockerCard)
      await expect(card).toHaveAttribute('role', 'alert')
      // Title varies by BlockerKind. Legacy shape (no blocker_kind)
      // falls through to the generic title; match by prefix so the
      // assertion is kind-agnostic.
      await expect(card).toHaveAttribute(
        'aria-label',
        /^Conversation blocked — /,
      )
    })
})
})
