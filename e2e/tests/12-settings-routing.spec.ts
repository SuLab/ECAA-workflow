import { expect, test } from '@playwright/test'
import { withMockBackend } from '../helpers/withMockBackend'

/**
 * 12 — Settings URL routing (PR-G).
 *
 * Settings is reachable two ways:
 *  - Click the gear icon in the title bar.
 *  - Load `/?view=settings` directly (bookmark / share-link).
 *
 * The query-param contract makes the second form work.
 */

test.describe('Settings URL routing', () => {
  test('loading /?view=settings on a fresh page renders the Settings page', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [] }, async () => {
      await page.goto('/?view=settings')
      // The Settings page renders an h1 with the literal text "Settings"
      // and a "← Back to chat" button in its body. The title-bar gear
      // also flips its aria-label to "Back to chat" while in settings
      // mode, so the body button is matched on its visible-text form
      // (with the leading arrow) to disambiguate.
      await expect(
        page.getByRole('heading', { name: 'Settings' }),
      ).toBeVisible()
      await expect(
        page.getByRole('button', { name: '← Back to chat' }),
      ).toBeVisible()
    })
  })

  test('clicking "Back to chat" returns to the chat surface and clears the query', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [] }, async () => {
      await page.goto('/?view=settings')
      await page.getByRole('button', { name: '← Back to chat' }).click()
      // Composer is the canonical anchor for "we're back on chat".
      await expect(page.locator('[aria-label="Message"]')).toBeVisible()
      // ?view=settings should be cleared.
      await expect(page).not.toHaveURL(/[?&]view=settings/)
    })
  })

  test('command-palette "Open settings" navigates to the settings page', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [] }, async () => {
      await page.goto('/')
      // CommandPalette installs its global Ctrl+K listener inside a
      // useEffect that runs after first paint. Wait for the greeting
      // to be visible so we know React has hydrated and the listener
      // is wired before pressing Ctrl+K — otherwise the keypress
      // arrives at the composer textarea (which is autofocused) and
      // the palette never opens.
      await page
        .locator('[aria-label="Assistant message"]')
        .first()
        .waitFor({ state: 'visible' })
      await page.keyboard.press('Control+K')
      await page.keyboard.type('settings')
      await page.keyboard.press('Enter')
      await expect(
        page.getByRole('heading', { name: 'Settings' }),
      ).toBeVisible()
    })
  })
})
