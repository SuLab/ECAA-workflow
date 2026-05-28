import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import { sel } from '../helpers/selectors'
import type { Beat } from '../helpers/types'

/**
 * Multi-transition BlockerCard rendering regression.
 *
 * When the harness fires `task_blocked` for two consecutive `discover_*`
 * tasks (e.g. `discover_differential_expression` then
 * `discover_normalisation`), the SECOND BlockerCard must render even
 * though the session's state discriminant doesn't change between the
 * two events — the state machine appends in-place to
 * `state.blockers` while staying on `Blocked`. Before the fix, the SSE
 * `state_advanced` handler only triggered `refreshCurrentState` (a
 * refetch), which could race the next broadcast and leave the second
 * blocker invisible until the SME refreshed manually or hit the API
 * directly. The fix forwards the SSE payload's authoritative
 * `new_state` to a synchronous `applyStateAdvanced` so the second
 * BlockerCard renders without waiting on the refetch.
 *
 * The test drives two state_advanced SSE events for the same `blocked`
 * discriminant — first with one blocker, then with two — and asserts
 * the BlockerCard count grows from 1 to 2 in lock-step. This covers
 * BOTH the "in-place append while still blocked" path (the bug) AND
 * the legacy "Blocked → Emitted → Blocked again" path that the round
 * 5 regression also exercises.
 */

const blockerForTask = (taskId: string, label: string) => ({
  blocker_id: `bid-${taskId}`,
  task_id: taskId,
  kind: {
    kind: 'awaiting_sme_approval',
    stage_id: taskId,
    top_candidate: 'deseq2',
    runner_ups: ['edger', 'limma_voom'],
  },
  message: `Task ${taskId} blocked: Awaiting SME approval for ${label}. Top candidate: deseq2 (score 4.85). Runner-ups: edger (4.30), limma_voom (4.20). Full decision: runtime/outputs/${taskId}/decision.json`,
  recovery_hint:
    'Review the candidates and accept the agent’s top pick (recommended) or override to a runner-up.',
})

const intakeBeat: Beat = {
  user: 'Run a bulk RNA-seq differential expression analysis with DESeq2.',
  assistant: {
    content:
      'Plan confirmed — the package is emitted and execution started. The discover_* tasks will block one at a time for your approval.',
  },
  state: {
    kind: 'blocked',
    reason: 'Task discover_differential_expression blocked: Awaiting SME approval',
    recovery_hint: 'Review the candidates.',
    blockers: [blockerForTask('discover_differential_expression', 'differential_expression')],
  },
}

test.describe('BlockerCard renders for every awaiting_sme_approval transition', () => {
  test('second blocker appended to state.blockers renders its own BlockerCard', async ({
    page,
  }) => {
    await withMockBackend(
      page,
      { beats: [intakeBeat] },
      async (handle) => {
        await page.goto('/')
        const chat = new Chat(page)
        await chat.waitForAssistant()

        await chat.sendUserMessage(intakeBeat.user)
        await chat.waitForAssistant({ textContains: 'Plan confirmed' })

        // First BlockerCard — verifies the baseline still works.
        await expect(page.locator(sel.blockerCard)).toHaveCount(1)
        await expect(page.locator(sel.blockerCard).first()).toContainText(
          'differential_expression',
        )

        // Simulate the harness firing task_blocked for a SECOND
        // discover_* task while the first is still pending SME accept.
        // The server appends to state.blockers in place — session
        // discriminant stays on `blocked` — and broadcasts a fresh
        // state_advanced. Before the fix, the SSE handler only
        // triggered a refetch that could race the next broadcast and
        // miss the second blocker.
        //
        // Production race: the server's `/state` endpoint has its own
        // `reconciled_progress_cache` (chat_routes/sessions.rs:365)
        // that returns stale `blocked_tasks` for one round-trip after
        // a fresh task_blocked. To force the regression deterministically
        // here we KEEP the mock's /state snapshot stale (still returning
        // the single-blocker shape) and only push the fresh
        // state_advanced SSE. The fix MUST use the SSE payload's
        // new_state to render the second card — without it, the
        // refetch returns the stale single-blocker shape and the
        // second BlockerCard never appears.
        const twoBlockers = {
          kind: 'blocked' as const,
          reason:
            'Task discover_normalisation blocked: Awaiting SME approval',
          recovery_hint: 'Review the candidates.',
          blockers: [
            blockerForTask(
              'discover_differential_expression',
              'differential_expression',
            ),
            blockerForTask('discover_normalisation', 'normalisation'),
          ],
        }
        // Deliberately DO NOT call `handle.setState(twoBlockers)` —
        // the mock /state stays on the single-blocker shape so the
        // refetch-only path returns stale data. The fix's
        // applyStateAdvanced path bypasses the refetch race by
        // applying the SSE payload's authoritative new_state directly.
        await handle.pushSseEvent({
          type: 'state_advanced',
          new_state: twoBlockers,
        })

        // The second BlockerCard must render in lock-step with the SSE
        // event — the test fails if `applyStateAdvanced` did not apply
        // the payload synchronously and the UI just refetched the
        // stale /state snapshot.
        await expect(page.locator(sel.blockerCard)).toHaveCount(2, {
          timeout: 5_000,
        })
        await expect(page.locator(sel.blockerCard).nth(1)).toContainText(
          'normalisation',
        )
      },
    )
  })

  test('Blocked → Emitted → Blocked re-renders BlockerCard for the new blocker', async ({
    page,
  }) => {
    // Variant of the bug where the SME unblocks the first blocker (state
    // → Emitted) and the harness then posts a SECOND task_blocked for a
    // different task (state → Blocked with a fresh single-entry
    // blockers array). The legacy refetch path covered this case in
    // theory but races could lose the second card; the new
    // applyStateAdvanced path is unambiguous.
    await withMockBackend(
      page,
      {
        beats: [intakeBeat],
        unblockTarget: { kind: 'emitted' },
      },
      async (handle) => {
        await page.goto('/')
        const chat = new Chat(page)
        await chat.waitForAssistant()

        await chat.sendUserMessage(intakeBeat.user)
        await chat.waitForAssistant({ textContains: 'Plan confirmed' })

        await expect(page.locator(sel.blockerCard)).toHaveCount(1)
        await expect(page.locator(sel.blockerCard).first()).toContainText(
          'differential_expression',
        )

        // SME clicks Accept on the first blocker. mockBackend.unblock
        // flips state to `emitted` and broadcasts state_advanced.
        await chat.clickUnblock()
        await expect(page.locator(sel.blockerCard)).toHaveCount(0)

        // Harness posts task_blocked for the NEXT discover_* task.
        const secondOnly = {
          kind: 'blocked' as const,
          reason: 'Task discover_normalisation blocked: Awaiting SME approval',
          recovery_hint: 'Review the candidates.',
          blockers: [
            blockerForTask('discover_normalisation', 'normalisation'),
          ],
        }
        handle.setState(secondOnly)
        await handle.pushSseEvent({
          type: 'state_advanced',
          new_state: secondOnly,
        })

        // BlockerCard for the SECOND blocker must render — this is the
        // exact failure mode from the round-5 reproduction
        // (sessions/7e553817-...). Without applyStateAdvanced the
        // refetch could race and the SME would see the harness-progress
        // synthetic "paused: Awaiting SME approval" prose but no
        // BlockerCard button to click.
        await expect(page.locator(sel.blockerCard)).toHaveCount(1, {
          timeout: 5_000,
        })
        await expect(page.locator(sel.blockerCard).first()).toContainText(
          'normalisation',
        )
      },
    )
  })
})
