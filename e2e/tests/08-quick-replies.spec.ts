import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import { sel } from '../helpers/selectors'
import type { Beat } from '../helpers/types'

/**
 * 08 — Quick replies
 *
 * When the assistant returns `quick_replies: [...]`, the QuickReplyRow
 * renders chip buttons. Clicking a chip sends that exact text as a user
 * turn, and the row disappears once the next turn arrives because it
 * only renders on the latest assistant turn's response.
 */

const beatWithQuickReplies: Beat = {
  user: 'I want to analyze some RNA-seq data.',
  assistant: {
    content: 'Which organism are you working with?',
    quick_replies: ['Human', 'Mouse', 'Other organism'],
  },
}

const mouseFollowup: Beat = {
  user: 'Mouse',
  assistant: {
    content: 'Mouse RNA-seq recorded. What tissue?',
  },
  state: 'intake_followup',
}

test.describe('Quick replies', () => {
  test('assistant quick_replies render as chip buttons on the latest turn', async ({
    page,
  }) => {
    await withMockBackend(page, {
      beats: [beatWithQuickReplies, mouseFollowup],
    }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.sendUserMessage(beatWithQuickReplies.user)
      await chat.waitForAssistant({
        textContains: beatWithQuickReplies.assistant.content.slice(0, 20),
      })

      for (const option of beatWithQuickReplies.assistant.quick_replies!) {
        await expect(page.locator(sel.quickReplyButton(option))).toBeVisible()
      }
    })
})

  test('clicking a quick-reply chip sends the exact label as a user turn', async ({
    page,
  }) => {
    await withMockBackend(page, {
      beats: [beatWithQuickReplies, mouseFollowup],
    }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.sendUserMessage(beatWithQuickReplies.user)
      await chat.waitForAssistant({
        textContains: beatWithQuickReplies.assistant.content.slice(0, 20),
      })

      await chat.clickQuickReply('Mouse')
      // Verify the /turn POST body carried 'Mouse' as the message.
      const posts = handle.recordedTurnMessages()
      expect(posts).toContain('Mouse')
      // Verify a user bubble with 'Mouse' appears.
      await expect(
        page.locator(sel.userBubble).filter({ hasText: 'Mouse' }),
      ).toBeVisible()
      // Wait for the follow-up assistant response.
      await chat.waitForAssistant({
        textContains: mouseFollowup.assistant.content.slice(0, 20),
      })
    })
})
})
