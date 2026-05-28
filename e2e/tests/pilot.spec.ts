import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import { sel } from '../helpers/selectors'
import type { Beat } from '../helpers/types'

/**
 * Mocked pilot row in the Metrics tab. When the server broadcasts
 * `harness_sizing_pilot_complete` with a PilotReport payload,
 * `useSseChatEvents` stores it as `pilot.report` and the Metrics tab
 * renders a row labeled "Pilot" whose second cell is a `PilotSummary`
 * showing measurement + confidence counts. This spec injects a synthetic
 * complete event via the fake EventSource and asserts the row lands.
 */

const beat: Beat = {
  user: 'Plan an analysis.',
  assistant: { content: 'Planning.' },
  state: 'emitted',
}

test.describe('Pilot row in Metrics tab', () => {
  test('renders pilot projection row when harness_sizing_pilot_complete fires', async ({
    page,
  }) => {
    // Populate metrics with a minimal payload so the MetricsTable
    // renders at all (an empty metrics returns 404 and hides the table
    // entirely). Pilot row is rendered by the same table once
    // sse.pilot.status === 'complete'.
    await withMockBackend(page, {
      beats: [beat],
      metrics: {
        turn_count: 1,
        tool_call_count: 1,
        total_input_tokens: 0,
        total_output_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        p50_turn_ms: 0,
        p95_turn_ms: 0,
        p99_turn_ms: 0,
        mean_turn_ms: 0,
        max_turn_ms: 0,
        sonnet_turns: 1,
        opus_turns: 0,
        total_instance_seconds: 0,
        instance_type_seconds: {},
        high_water_exceeded_count: 0,
      },
    }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.openTab('metrics')
      await expect(page.locator(sel.metricsTable)).toBeVisible()

      // Emit the harness_sizing_pilot_complete event with a minimal
      // report shape. The UI's `PilotSummary` reads `measurements`
      // (length) and `confidence` (0..1); anything else is ignored.
      await handle.pushSseEvent({
        type: 'harness_sizing_pilot_complete',
        report: {
          measurements: [
            { task_id: 'align_t1', peak_rss_mb: 512, wall_time_secs: 12 },
            { task_id: 'align_t2', peak_rss_mb: 640, wall_time_secs: 15 },
            { task_id: 'align_t3', peak_rss_mb: 580, wall_time_secs: 13 },
          ],
          confidence: 0.82,
          projected_requirements: {
            alignment_quantification: { vcpus: 8, memory_gb: 32, storage_gb: 100 },
          },
        },
      })

      const pilotRow = page.locator('tr[data-metric-row="pilot"]')
      await expect(pilotRow).toBeVisible()
      await expect(pilotRow).toContainText('Pilot')
      await expect(pilotRow).toContainText('3 measurements')
      await expect(pilotRow).toContainText('confidence 82%')
    })
})

  test('renders "skipped" summary when harness_sizing_pilot_skipped fires', async ({
    page,
  }) => {
    await withMockBackend(page, {
      beats: [beat],
      metrics: {
        turn_count: 1,
        tool_call_count: 1,
        total_input_tokens: 0,
        total_output_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        p50_turn_ms: 0,
        p95_turn_ms: 0,
        p99_turn_ms: 0,
        mean_turn_ms: 0,
        max_turn_ms: 0,
        sonnet_turns: 1,
        opus_turns: 0,
        total_instance_seconds: 0,
        instance_type_seconds: {},
        high_water_exceeded_count: 0,
      },
    }, async (handle) => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()
      await chat.openTab('metrics')

      await handle.pushSseEvent({
        type: 'harness_sizing_pilot_skipped',
        reason: 'disabled',
      })

      const pilotRow = page.locator('tr[data-metric-row="pilot"]')
      await expect(pilotRow).toBeVisible()
      await expect(pilotRow).toContainText('skipped')
    })
})
})
