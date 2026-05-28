/**
 * High-level chat interactions and expectations.
 *
 * `Chat` wraps the composer / confirmation card / blocker / tabs so a
 * beat's assertions read as plain prose: `await chat.sendUserMessage("…");
 * await chat.expect.stateBadge("intake_followup")`.
 */

import { expect, type Locator, type Page } from '@playwright/test'
import { sel } from './selectors'
import type { SessionStateKind, TabKind } from './types'

export class Chat {
  readonly expect: ChatExpectations

  constructor(public readonly page: Page) {
    this.expect = new ChatExpectations(page)
  }

  /** Type into the composer and hit Enter. */
  async sendUserMessage(text: string): Promise<void> {
    const composer = this.page.locator(sel.composer)
    await composer.waitFor({ state: 'visible' })
    // Focus first so the auto-focus effect doesn't fight us.
    await composer.click()
    await composer.fill(text)
    await composer.press('Enter')
    // Wait for the user bubble to land — it's appended synchronously by
    // useConversation.sendTurn before the server round-trip. Filter on
    // a short prefix of the message rather than the full string:
    // long SME replies (several hundred chars) span multiple DOM
    // text nodes after React hydration, and Playwright's hasText
    // substring match can time out against the wrapped rendering.
    // A 40-char prefix is unique-enough per turn in live specs and
    // matches cleanly regardless of wrapping.
    const prefix = text.slice(0, 40)
    const userBubble = this.page
      .locator(sel.userBubble)
      .filter({ hasText: prefix })
    await userBubble.first().waitFor({ state: 'visible', timeout: 15_000 })
  }

  /**
   * Wait for an assistant turn.
   *
   * - With `textContains`: wait for any bubble whose content contains the
   *  substring. Use this after `sendUserMessage` to wait for the specific
   *  response to a beat.
   * - Without: wait for at least one assistant bubble to be visible. Use
   *  this once right after `page.goto` to wait for the greeting.
   */
  async waitForAssistant(opts?: {
    textContains?: string
    timeout?: number
  }): Promise<string> {
    const timeout = opts?.timeout ?? 10_000
    const bubbles = this.page.locator(sel.assistantBubble)
    if (opts?.textContains) {
      const match = bubbles.filter({ hasText: opts.textContains }).last()
      await match.waitFor({ state: 'visible', timeout })
      return (await match.textContent()) ?? ''
    }
    await bubbles.first().waitFor({ state: 'visible', timeout })
    return (await bubbles.last().textContent()) ?? ''
  }

  /** Click Confirm in the confirmation card. */
  async clickConfirm(): Promise<void> {
    const btn = this.page.locator(sel.confirmButton)
    await btn.waitFor({ state: 'visible' })
    await btn.click()
  }

  /** Click Revise in the confirmation card. */
  async clickReject(): Promise<void> {
    const btn = this.page.locator(sel.rejectButton)
    await btn.waitFor({ state: 'visible' })
    await btn.click()
  }

  /** Click I've addressed this in the BlockerCard. */
  async clickUnblock(): Promise<void> {
    const btn = this.page.locator(sel.blockerUnblockButton)
    await btn.waitFor({ state: 'visible' })
    await btn.click()
  }

  /** Click a quick-reply chip by visible text. */
  async clickQuickReply(label: string): Promise<void> {
    await this.page.locator(sel.quickReplyButton(label)).first().click()
  }

  /** Click a tab in the State Inspector. */
  async openTab(tab: TabKind): Promise<void> {
    await this.page.locator(sel.inspectorTab(tab)).click()
  }

  /** Click a mobile view tab. Desktop breakpoint has no such toggle. */
  async mobileShow(view: 'chat' | 'state'): Promise<void> {
    // force: true skips Playwright's hit-test pre-flight. Under
    // mobile-chrome's emulated-touch context the actionability check
    // intermittently reports the parent <div role="tablist"> as the
    // hit target instead of the <button role="tab"> child, even
    // though elementsFromPoint at the click center returns the
    // button. The visible/enabled/stable checks have already passed;
    // bypassing the hit-test makes the touch tap consistent across
    // chromium / firefox / mobile-chrome.
    const tab = this.page.locator(sel.mobileTab(view))
    await tab.waitFor({ state: 'visible' })
    for (let attempt = 0; attempt < 3; attempt += 1) {
      await tab.click({ force: true })
      try {
        await expect(tab).toHaveAttribute('aria-selected', 'true', {
          timeout: 1_000,
        })
        return
      } catch (e) {
        if (attempt === 2) throw e
      }
    }
  }

  /** Return all current assistant bubble texts joined — useful for forbidden-text checks. */
  async allAssistantText(): Promise<string> {
    const bubbles = this.page.locator(sel.assistantBubble)
    const count = await bubbles.count()
    const texts: string[] = []
    for (let i = 0; i < count; i += 1) {
      const t = (await bubbles.nth(i).textContent()) ?? ''
      texts.push(t)
    }
    return texts.join(' ')
  }

  latestAssistant(): Locator {
    return this.page.locator(sel.assistantBubble).last()
  }
}

function escapeRegex(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

export class ChatExpectations {
  constructor(private readonly page: Page) {}

  async stateBadge(kind: SessionStateKind): Promise<void> {
    // Use toHaveText (not toBeVisible) so the check works on mobile where
    // the State Inspector pane has display:none while the chat tab is
    // active. The element is still in the DOM and its text still reflects
    // the current state; visibility would require switching tabs.
    const badge = this.page.locator(sel.stateBadge)
    const label = kind.replace(/_/g, ' ')
    await expect(badge).toHaveText(new RegExp(label, 'i'))
  }

  async confirmationCardVisible(): Promise<void> {
    await expect(this.page.locator(sel.confirmationCard)).toBeVisible()
  }

  async confirmationCardHidden(): Promise<void> {
    await expect(this.page.locator(sel.confirmationCard)).toHaveCount(0)
  }

  async confirmationCardContains(text: string): Promise<void> {
    await expect(this.page.locator(sel.confirmationCard)).toContainText(text)
  }

  async blockerVisible(): Promise<void> {
    await expect(this.page.locator(sel.blockerCard)).toBeVisible()
  }

  async blockerHidden(): Promise<void> {
    await expect(this.page.locator(sel.blockerCard)).toHaveCount(0)
  }

  async infraBannerVisible(): Promise<void> {
    // The infra banner is a role=alert that is NOT the blocker (blocker
    // has aria-label). Match on the aria-live=assertive disambiguator.
    await expect(this.page.locator(sel.infraBanner).first()).toBeVisible()
  }

  async toolPillStatus(line: string | null): Promise<void> {
    const pill = this.page.locator(sel.toolPillInLatestAssistant)
    if (line === null) {
      await expect(pill).toHaveCount(0)
    } else {
      await expect(pill).toBeVisible()
      await expect(pill).toContainText(line)
    }
  }

  async stillThinkingVisible(): Promise<void> {
    await expect(this.page.locator(sel.stillThinking)).toBeVisible()
  }

  async stillThinkingHidden(): Promise<void> {
    await expect(this.page.locator(sel.stillThinking)).toHaveCount(0)
  }

  async messageContains(text: string): Promise<void> {
    // Case-insensitive substring match — scenario YAML authors shouldn't
    // need to worry about whether the assistant capitalizes "Platform-
    // aware" or "platform-aware". Use a regex for the match.
    const re = new RegExp(escapeRegex(text), 'i')
    await expect(
      this.page.locator(sel.assistantBubble).last(),
    ).toContainText(re)
  }

  async noAssistantMessageContains(text: string): Promise<void> {
    // Case-insensitive forbidden-text guard — "Leiden" and "leiden"
    // should both trip the check. Snapshot the locator in one browser
    // round-trip so the assertion does not race chat-log virtualization
    // between a count() call and nth(i).textContent().
    const needle = text.toLowerCase()
    const texts = await this.page.locator(sel.assistantBubble).allTextContents()
    for (let i = 0; i < texts.length; i += 1) {
      const t = texts[i]
      if (t.toLowerCase().includes(needle)) {
        throw new Error(
          `forbidden text "${text}" appeared in assistant message ${i}: ${t.slice(
            0,
            120,
          )}`,
        )
      }
    }
  }

  async jobsBadgeCount(n: number): Promise<void> {
    if (n === 0) {
      await expect(this.page.locator('[aria-label$=" progress events"]')).toHaveCount(0)
      return
    }
    await expect(this.page.locator(sel.jobsBadge(n))).toBeVisible()
  }

  async activeTab(kind: TabKind): Promise<void> {
    await expect(this.page.locator(sel.inspectorTab(kind))).toHaveAttribute(
      'aria-selected',
      'true',
    )
  }

  async userBubbleContains(text: string): Promise<void> {
    const user = this.page.locator(sel.userBubble).filter({ hasText: text })
    await expect(user.first()).toBeVisible()
  }
}
