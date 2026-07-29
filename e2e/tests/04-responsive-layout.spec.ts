import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withBeats, withMockBackend } from '../helpers/withMockBackend'
import { sel } from '../helpers/selectors'
import type { Beat } from '../helpers/types'

/**
 * 04 — Responsive layout
 *
 * Desktop (≥1024 px) renders a split pane: ConversationPane (left) +
 * StateInspectorPane (right). Below 1024 px both panes exist in the DOM
 * but only one is visible at a time, toggled by the "Mobile view
 * switcher" tablist in the title bar.
 *
 * Resize triggers include:
 *  - desktop → mobile: mobile-toggle appears, both panes still mounted,
 *  chat tab is active first.
 *  - mobile → desktop: mobile-toggle disappears, split pane returns.
 */

const beat: Beat = {
  user: 'I want to analyze RNA-seq data.',
  assistant: { content: 'Got it — RNA-seq. Organism?' },
  state: 'intake_followup',
}

test.describe('Responsive layout', () => {
  test('desktop viewport (1280x800) shows split pane with both wrappers visible', async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1280, height: 800 })
    await withBeats(page, [beat], async () => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      // Mobile toggle must NOT be present.
      await expect(page.locator(sel.mobileTablist)).toHaveCount(0)
      // Both the chat log and the state inspector must be visible.
      await expect(page.locator(sel.chatLog)).toBeVisible()
      await expect(page.locator(sel.inspectorTablist)).toBeVisible()
    })
  })

  test('mobile viewport (390x844) shows tablist with Chat active by default', async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 })
    await withMockBackend(page, { beats: [beat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await expect(page.locator(sel.mobileTablist)).toBeVisible()
      await expect(page.locator(sel.mobileTab('chat'))).toHaveAttribute(
        'aria-selected',
        'true',
      )
      await expect(page.locator(sel.mobileTab('state'))).toHaveAttribute(
        'aria-selected',
        'false',
      )

      // Chat pane is visible, state pane wrapper is in DOM but hidden.
      await expect(page.locator(sel.chatPaneWrapper)).toBeVisible()
      await expect(page.locator(sel.statePaneWrapper)).toBeHidden()
    })
  })

  test('mobile view switcher toggles chat ↔ state', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 })
    await withMockBackend(page, { beats: [beat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.mobileShow('state')
      await expect(page.locator(sel.statePaneWrapper)).toBeVisible()
      await expect(page.locator(sel.chatPaneWrapper)).toBeHidden()
      // The state inspector tabs are now visible.
      await expect(page.locator(sel.inspectorTablist)).toBeVisible()

      await chat.mobileShow('chat')
      await expect(page.locator(sel.chatPaneWrapper)).toBeVisible()
      await expect(page.locator(sel.statePaneWrapper)).toBeHidden()
    })
  })

  test('desktop → mobile resize surfaces the tablist', async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 })
    await withMockBackend(page, { beats: [beat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      // Split pane, no tablist.
      await expect(page.locator(sel.mobileTablist)).toHaveCount(0)

      // Shrink to mobile.
      await page.setViewportSize({ width: 390, height: 844 })
      await expect(page.locator(sel.mobileTablist)).toBeVisible()
      // Chat tab is active by default after a return-to-mobile.
      await expect(page.locator(sel.mobileTab('chat'))).toHaveAttribute(
        'aria-selected',
        'true',
      )
    })
  })

  test('mobile → desktop resize hides the tablist and restores split pane', async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 })
    await withMockBackend(page, { beats: [beat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()
      await expect(page.locator(sel.mobileTablist)).toBeVisible()

      await page.setViewportSize({ width: 1280, height: 800 })
      await expect(page.locator(sel.mobileTablist)).toHaveCount(0)
      await expect(page.locator(sel.chatLog)).toBeVisible()
      await expect(page.locator(sel.inspectorTablist)).toBeVisible()
    })
  })

  test('mobile view tab click moves focus into the newly-visible pane', async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 })
    await withMockBackend(page, { beats: [beat] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.mobileShow('state')
      // Focus should move into the state pane wrapper (tabindex=-1 wrapper).
      const focusedLabel = await page.evaluate(() => {
        const el = document.activeElement as HTMLElement | null
        return el?.getAttribute('aria-label') ?? null
      })
      expect(focusedLabel).toBe('State inspector pane')
    })
  })

  test('mobile title-bar actions remain inside the viewport', async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 })
    await withMockBackend(page, { beats: [beat] }, async () => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      const titleBar = page.getByTestId('app-title-bar')
      const actions = [
        { role: 'button' as const, name: 'Open recent sessions' },
        { role: 'button' as const, name: 'Open a package' },
        { role: 'button' as const, name: 'Share session' },
        { role: 'button' as const, name: 'Accessibility settings' },
        { role: 'button' as const, name: 'Switch to dark theme' },
        { role: 'button' as const, name: 'Open Settings' },
        { role: 'tab' as const, name: 'Chat' },
        { role: 'tab' as const, name: 'View plan' },
      ]

      for (const { role, name } of actions) {
        const action = titleBar.getByRole(role, { name })
        await expect(action).toBeVisible()
        const box = await action.boundingBox()
        expect(box, `${name} should have a layout box`).not.toBeNull()
        expect(
          box!.x,
          `${name} should not be clipped on the left`,
        ).toBeGreaterThanOrEqual(0)
        expect(
          box!.x + box!.width,
          `${name} should not be clipped on the right`,
        ).toBeLessThanOrEqual(390)
      }

      const composer = page.getByRole('textbox', { name: 'Message' })
      const composerBox = await composer.boundingBox()
      expect(composerBox, 'Message composer should have a layout box').not.toBeNull()
      expect(
        composerBox!.y + composerBox!.height,
        'Message composer should not be clipped below the viewport',
      ).toBeLessThanOrEqual(844)
    })
  })
})
