import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import { sel } from '../helpers/selectors'
import type { Beat } from '../helpers/types'

/**
 * 05 — Tool-call feedback (pill, streaming bubble, still-thinking)
 *
 * Covers three distinct visual indicators that fire during an in-flight
 * assistant turn:
 *
 *  - `ToolCallStatusPill`: appears inside the latest assistant turn when
 *  a `tool_call_started` SSE arrives; disappears on `tool_call_finished`.
 *  - `InFlightAssistantBubble`: appears at the bottom of the log while
 *  `assistant_token_delta` events accumulate; is replaced by the
 *  canonical `AssistantTurnCard` when the final Turn lands.
 *  - `StillThinkingIndicator`: appears after 8 s of any in-flight turn
 *  regardless of whether the tool pill is up. Distinct from the pill.
 */

const beatWithToolCalls: Beat = {
  user: 'Please classify my experiment: scRNA-seq human IVD.',
  assistant: {
    content: 'Got it — classified as single_cell_rnaseq for human IVD.',
  },
  state: 'intake_followup',
  sse: [
    {
      type: 'tool_call_started',
      tool_name: 'classify_intake',
      status_line: 'Checking the plan against your description…',
    },
    { type: 'tool_call_finished', tool_name: 'classify_intake' },
  ],
}

test.describe('Tool-call feedback', () => {
  test('tool_call_started shows the pill, tool_call_finished removes it', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [beatWithToolCalls] }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      // Push the started event without a beat turn, so the pill shows up
      // without racing the /turn response.
      await handle.pushSseEvent({
        type: 'tool_call_started',
        tool_name: 'classify_intake',
        status_line: 'Checking the plan against your description…',
      })

      // The pill is attached to the LATEST assistant turn (the greeting
      // until we send something). Match by parent assistant bubble.
      await expect(
        page.locator(sel.toolPillInLatestAssistant),
      ).toContainText('Checking the plan')

      await handle.pushSseEvent({
        type: 'tool_call_finished',
        tool_name: 'classify_intake',
      })
      await expect(page.locator(sel.toolPillInLatestAssistant)).toHaveCount(0)
    })
})

  test('assistant_token_delta events render streaming bubble with caret', async ({
    page,
  }) => {
    // Use a delayed turn so the streaming bubble has time to render before
    // the canonical Turn arrives.
    const beat: Beat = {
      user: 'stream me a response',
      assistant: { content: 'Streamed final content.' },
    }
    await withMockBackend(page, {
      beats: [beat],
      delayTurn: { callIndex: 1, delayMs: 2000 },
    }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      // Kick off the turn — the mock will delay 2s before resolving.
      void chat.sendUserMessage(beat.user)

      // Push streaming chunks while the /turn promise is pending.
      await page.waitForTimeout(200)
      await handle.pushSseEvent({ type: 'assistant_token_delta', text: 'Stre' })
      await handle.pushSseEvent({ type: 'assistant_token_delta', text: 'aming ' })
      await handle.pushSseEvent({ type: 'assistant_token_delta', text: 'chunk' })

      // The streaming bubble should render with the accumulated text.
      const streaming = page.locator(sel.streamingBubble)
      await expect(streaming).toBeVisible({ timeout: 3000 })
      await expect(streaming).toContainText('Streaming chunk')

      // After the real Turn lands, the canonical bubble takes over and
      // the streaming bubble is removed.
      await chat.waitForAssistant({ textContains: 'Streamed final content' })
      await expect(page.locator(sel.streamingBubble)).toHaveCount(0)
    })
})

  test('Still thinking indicator appears after 8 s of in-flight turn', async ({
    page,
  }) => {
    const beat: Beat = {
      user: 'slow turn',
      assistant: { content: 'That took a while, but here is the answer.' },
    }
    await withMockBackend(page, {
      beats: [beat],
      delayTurn: { callIndex: 1, delayMs: 9500 },
    }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.sendUserMessage(beat.user)

      // Still thinking indicator should appear after 8s. Give it a small
      // margin for CI timing variability.
      await expect(page.locator(sel.stillThinking)).toBeVisible({
        timeout: 12_000,
      })

      // Once the turn lands, the indicator clears.
      await chat.waitForAssistant({
        textContains: 'That took a while',
        timeout: 15_000,
      })
      await expect(page.locator(sel.stillThinking)).toHaveCount(0)
    })
})
})
