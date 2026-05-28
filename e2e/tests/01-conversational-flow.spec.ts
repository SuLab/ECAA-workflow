import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withBeats } from '../helpers/withMockBackend'
import { sel } from '../helpers/selectors'
import type { Beat } from '../helpers/types'

/**
 * 01 — Conversational flow
 *
 * Exercises the composer, greeting render, multi-turn timeline, auto-focus,
 * Enter vs Shift+Enter, draft persistence across re-render, whitespace-only
 * Send disabled, and scroll-to-latest behavior. No confirmation card in
 * this spec — see 02.
 */

const beats: Beat[] = [
  {
    user: 'I want to do a bulk RNA-seq differential expression analysis.',
    assistant: {
      content: 'Got it — bulk RNA-seq DE. What organism and tissue are you working with?',
    },
    state: 'intake_followup',
    expect: { stateBadge: 'intake_followup' },
  },
  {
    user: 'Human liver, comparing treated vs untreated.',
    assistant: {
      content: 'Understood — human liver, treated vs untreated contrast. Do you have specific datasets in mind?',
    },
    expect: { visibleText: ['human liver'] },
  },
  {
    user: 'GSE12345 and GSE67890.',
    assistant: {
      content: 'Recorded those two accessions. What governance applies — internal, preprint, or publication?',
    },
    expect: { visibleText: ['two accessions'] },
  },
]

test.describe('Conversational flow', () => {
  test('greeting renders on mount before any user input', async ({ page }) => {
    await withBeats(page, beats, async () => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()
      const greeting = await chat.latestAssistant().textContent()
      expect(greeting).toBeTruthy()
      expect(greeting!.length).toBeGreaterThan(10)
    })
  })

  test('composer auto-focuses on mount so SME can type immediately', async ({
    page,
  }) => {
    await withBeats(page, beats, async () => {
      await page.goto('/')
      // Wait for the app to settle and the auto-focus effect to run.
      await page.locator(sel.composer).waitFor({ state: 'visible' })
      const focused = await page.evaluate(() => {
        const el = document.activeElement as HTMLElement | null
        return el?.getAttribute('aria-label') ?? null
      })
      expect(focused).toBe('Message')
    })
  })

  test('Enter submits, Shift+Enter inserts a newline', async ({ page }) => {
    await withBeats(page, beats, async () => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      // Shift+Enter inserts a newline without submitting.
      const composer = page.locator(sel.composer)
      await composer.click()
      await composer.type('line one')
      await composer.press('Shift+Enter')
      await composer.type('line two')
      const draftValue = await composer.inputValue()
      expect(draftValue).toBe('line one\nline two')
      // Still no user bubble — Shift+Enter did not submit.
      await expect(page.locator(sel.userBubble)).toHaveCount(0)

      // Clear and submit with a plain Enter.
      await composer.fill('')
      await chat.sendUserMessage(beats[0].user)
      await expect(
        page.locator(sel.userBubble).filter({ hasText: beats[0].user }),
      ).toBeVisible()
    })
  })

  test('whitespace-only text cannot be submitted', async ({ page }) => {
    await withBeats(page, beats, async () => {
      await page.goto('/')
      await page.locator(sel.composer).waitFor({ state: 'visible' })

      const composer = page.locator(sel.composer)
      const sendButton = page.locator(sel.sendButton)

      // Empty — disabled.
      await expect(sendButton).toBeDisabled()
      // Whitespace only — still disabled.
      await composer.fill('   \n  \t ')
      await expect(sendButton).toBeDisabled()
      // With content — enabled.
      await composer.fill('real content')
      await expect(sendButton).toBeEnabled()
    })
  })

  test('multi-turn timeline renders user + assistant bubbles in order', async ({
    page,
  }) => {
    await withBeats(page, beats, async () => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      for (const beat of beats) {
        await chat.sendUserMessage(beat.user)
        await chat.waitForAssistant({
          textContains: beat.assistant.content.slice(0, 20),
        })
      }

      // 3 user bubbles + 1 greeting + 3 assistant replies = at least 6 bubbles
      const userCount = await page.locator(sel.userBubble).count()
      expect(userCount).toBe(3)
      const assistantCount = await page.locator(sel.assistantBubble).count()
      expect(assistantCount).toBeGreaterThanOrEqual(4) // greeting + 3 replies
    })
  })

  test('scroll follows new turns to the bottom of the log', async ({ page }) => {
    // Use a bigger beat list to force overflow.
    const longBeats: Beat[] = Array.from({ length: 8 }, (_, i) => ({
      user: `Turn ${i + 1}: telling the assistant about dataset ${i + 1}.`,
      assistant: {
        content: `Noted — dataset ${i + 1} added to the plan. What's next?`,
      },
    }))
    await withBeats(page, longBeats, async () => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      for (const beat of longBeats) {
        await chat.sendUserMessage(beat.user)
        await chat.waitForAssistant({
          textContains: beat.assistant.content.slice(0, 20),
        })
      }

      // The latest assistant bubble must be in view. Use Playwright's
      // built-in toBeInViewport so the assertion auto-waits on the
      // 500ms debounced smooth-scroll in ChatTimeline (one-shot
      // bounding-box checks raced the debounce on slow CI).
      const latest = chat.latestAssistant()
      await expect(latest).toBeInViewport()
    })
  })
})
