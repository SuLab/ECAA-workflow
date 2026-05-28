import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import type { Beat } from '../helpers/types'

/**
 * BranchFromHereCard — mocked-tier wiring coverage (M1.1)
 *
 * Verifies the inline branch affordance in the conversation flow:
 *  1. The card appears once the session has emitted at least one package.
 *  2. Clicking "Branch from here" expands the rationale panel.
 *  3. Submitting the form POSTs to /branch and captures the rationale.
 *  4. The card is absent during pre-emission states (intake_followup).
 *  5. The card is absent when the session is Blocked (server refuses
 *     branch_session from Blocked; the UI withholds onBranch accordingly).
 *
 * The branch endpoint is intercepted via page.route so we can assert the
 * POST body without needing a live server.
 */

// ── Beats ───────────────────────────────────────────────────────────────────

const emittedBeats: Beat[] = [
  {
    user: 'I want to run a bulk RNA-seq DE analysis on human liver.',
    assistant: {
      content: 'Understood — bulk RNA-seq DE on human liver. Please confirm the plan.',
      confirmation_card: {
        summary_markdown: '**Bulk RNA-seq DE on human liver**\n\n- Organism: Human\n- Tissue: Liver',
      },
    },
    state: 'pending_confirmation',
  },
  {
    user: '(confirmed)',
    assistant: {
      content: 'Package emitted. Your analysis plan is ready for execution.',
    },
    state: 'emitted',
  },
]

const intakeBeat: Beat = {
  user: 'I want to run an analysis.',
  assistant: {
    content: 'Tell me more about your dataset.',
  },
  state: 'intake_followup',
}

// ── Tests ────────────────────────────────────────────────────────────────────

test.describe('BranchFromHereCard — inline conversation affordance', () => {
  test('branch card appears in the emitted state after package emission', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: emittedBeats }, async () => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      // Drive through the beats to reach emitted state.
      await chat.sendUserMessage(emittedBeats[0].user)
      await chat.waitForAssistant({ textContains: 'confirm' })
      await chat.sendUserMessage(emittedBeats[1].user)
      await chat.waitForAssistant({ textContains: 'emitted' })

      // The BranchFromHereCard should be visible inside the latest
      // assistant turn card.
      const branchRegion = page.getByRole('region', {
        name: 'Branch from this session',
      })
      await expect(branchRegion).toBeVisible({ timeout: 10_000 })
      await expect(
        page.getByRole('button', { name: /branch from here/i }),
      ).toBeVisible()
    })
  })

  test('clicking "Branch from here" expands the rationale panel', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: emittedBeats }, async () => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.sendUserMessage(emittedBeats[0].user)
      await chat.waitForAssistant({ textContains: 'confirm' })
      await chat.sendUserMessage(emittedBeats[1].user)
      await chat.waitForAssistant({ textContains: 'emitted' })

      await page.getByRole('button', { name: /branch from here/i }).click()

      // Panel should expand to show the rationale textarea.
      await expect(
        page.getByRole('textbox', { name: /optional branching rationale/i }),
      ).toBeVisible({ timeout: 5_000 })
      await expect(
        page.getByRole('button', { name: /create branch/i }),
      ).toBeVisible()
    })
  })

  test('submitting the form POSTs to /branch with the rationale body', async ({
    page,
  }) => {
    const capturedBodies: unknown[] = []

    await withMockBackend(page, { beats: emittedBeats }, async () => {
      // Register the branch interceptor INSIDE withMockBackend so it runs
      // before the mock-backend's /session/*/** catch-all in Playwright's
      // LIFO route stack. The pattern uses a broad suffix match so it
      // catches the UUID-keyed URL regardless of session id.
      await page.route(/\/api\/(?:v1\/)?chat\/session\/[^/]+\/branch/, async (route) => {
        if (route.request().method() !== 'POST') return route.fallback()
        try {
          capturedBodies.push(JSON.parse(route.request().postData() ?? '{}'))
        } catch {
          capturedBodies.push(null)
        }
        // Return a fake child session id. The ConversationPane will then
        // navigate to window.location.href — in the test environment that
        // navigation is a no-op (the page stays put), which is fine.
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({ session_id: 'child-session-123' }),
        })
      })

      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.sendUserMessage(emittedBeats[0].user)
      await chat.waitForAssistant({ textContains: 'confirm' })
      await chat.sendUserMessage(emittedBeats[1].user)
      await chat.waitForAssistant({ textContains: 'emitted' })

      await page.getByRole('button', { name: /branch from here/i }).click()

      const textarea = page.getByRole('textbox', {
        name: /optional branching rationale/i,
      })
      await textarea.waitFor({ state: 'visible' })
      await textarea.fill('Try a stricter FDR threshold')

      await page.getByRole('button', { name: /create branch/i }).click()

      // Assert the POST was captured with the expected rationale.
      await expect
        .poll(() => capturedBodies.length, { timeout: 5_000 })
        .toBeGreaterThan(0)

      const body = capturedBodies[0] as { rationale?: string }
      expect(body.rationale).toBe('Try a stricter FDR threshold')
    })
  })

  test('branch card is absent during intake (pre-emission)', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [intakeBeat] }, async () => {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      await chat.sendUserMessage(intakeBeat.user)
      await chat.waitForAssistant({ textContains: 'dataset' })

      // No branch affordance during intake — nothing has been emitted yet.
      await expect(
        page.getByRole('button', { name: /branch from here/i }),
      ).toHaveCount(0)
    })
  })

  test('branch card is absent when the session is Blocked', async ({
    page,
  }) => {
    const blockedState = {
      kind: 'blocked' as const,
      reason: 'Validation failed for stage de_analysis.',
      recovery_hint: 'Review the task result and retry.',
    }
    const blockedBeat: Beat = {
      user: 'Continue.',
      assistant: {
        content: 'The session is blocked — please resolve the issue.',
      },
      // Push a state_advanced SSE so the UI transitions to Blocked in real
      // time (mirrors what the real server broadcasts). The beat.state field
      // also updates the mocked /state endpoint so the refreshState call
      // that follows POST /turn returns the blocked snapshot consistently.
      sse: [{ type: 'state_advanced', new_state: blockedState }],
      state: blockedState,
    }

    await withMockBackend(
      page,
      { beats: [blockedBeat] },
      async () => {
        await page.goto('/')
        const chat = new Chat(page)
        await chat.waitForAssistant()

        await chat.sendUserMessage(blockedBeat.user)
        await chat.waitForAssistant({ textContains: 'blocked' })

        // Positive assertion: the BlockerCard must be visible, proving the
        // session genuinely transitioned to Blocked. Without this the test
        // would pass vacuously even if the state transition never happened
        // (the branch button is also absent in greeting state).
        await expect(
          page.getByRole('alert', { name: /conversation blocked/i }),
        ).toBeVisible({ timeout: 5_000 })

        // The UI withholds onBranch from AssistantTurnCard when stateKind
        // is 'blocked' — branch_session is refused server-side from that
        // state, so the affordance is suppressed.
        // This assertion fails if 'blocked' is added back to hasEmittedAtLeastOnce
        // in ConversationPane.tsx because the branch button would then appear.
        await expect(
          page.getByRole('button', { name: /branch from here/i }),
        ).toHaveCount(0)
      },
    )
  })
})
