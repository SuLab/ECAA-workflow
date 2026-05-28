import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import { sel } from '../helpers/selectors'
import type { Beat, SessionMetrics } from '../helpers/types'

/**
 * 10 — Metrics tab
 *
 * StateInspectorPane's Metrics tab polls /metrics every 4 s while visible.
 * The rendered table must include per-session counters (turns, tool calls,
 * Sonnet/Opus split, latency percentiles, token totals). When /metrics
 * returns 404, the placeholder prose is shown instead.
 */

const beat: Beat = {
  user: 'Run the experiment.',
  assistant: { content: 'Running.' },
  state: 'intake_followup',
}

const metrics: SessionMetrics = {
  turn_count: 3,
  tool_call_count: 7,
  total_input_tokens: 5432,
  total_output_tokens: 2109,
  cache_read_tokens: 1234,
  cache_creation_tokens: 5678,
  p50_turn_ms: 321,
  p95_turn_ms: 987,
  p99_turn_ms: 1234,
  mean_turn_ms: 421,
  max_turn_ms: 1500,
  sonnet_turns: 2,
  opus_turns: 1,
  total_instance_seconds: 0,
  instance_type_seconds: {},
  high_water_exceeded_count: 0,
}

test.describe('Metrics tab', () => {
  test('populated metrics render a table with labeled rows', async ({ page }) => {
    await withMockBackend(page, { beats: [beat], metrics }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.openTab('metrics')

      const table = page.locator(sel.metricsTable)
      await expect(table).toBeVisible()
      await expect(table).toContainText('Turns')
      await expect(table).toContainText('3')
      await expect(table).toContainText('Tool calls')
      await expect(table).toContainText('7')
      await expect(table).toContainText('Sonnet turns')
      await expect(table).toContainText('Opus turns')
      await expect(table).toContainText('Mean turn latency')
      await expect(table).toContainText('421 ms')
      await expect(table).toContainText('p95 turn latency')
      await expect(table).toContainText('987 ms')
    })
})

  test('empty metrics (404) show the placeholder prose', async ({ page }) => {
    await withMockBackend(page, { beats: [beat], metrics: null }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.openTab('metrics')

      await expect(page.locator(sel.metricsTable)).toHaveCount(0)
      await expect(
        page.locator(sel.inspectorPanel('metrics')),
      ).toContainText('Metrics will appear here')
    })
})

  test('metrics updates when handle.setMetrics changes the snapshot', async ({
    page,
  }) => {
    await withMockBackend(page, {
      beats: [beat],
      metrics: { ...metrics, turn_count: 1 },
    }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.openTab('metrics')
      await expect(page.locator(sel.metricsTable)).toContainText('1')

      // Flip the mock and wait for the next poll tick (~4 s).
      handle.setMetrics({ ...metrics, turn_count: 42 })
      await expect(page.locator(sel.metricsTable)).toContainText('42', {
        timeout: 6000,
      })
    })
})
})
