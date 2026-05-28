import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import type { MockHypothesizedProposal, ProposalLifecycle } from '../helpers/types'

/**
 * proposal-card — HypothesizedProposalCard mocked-tier coverage
 *
 * Exercises the full proposal lifecycle on the UI:
 *
 *  1. `proposal_received` SSE triggers a REST refetch (via
 *  `proposalEvents` key churn in useSseChatEvents) and mounts the
 *  card.
 *  2. `proposal_gate_advanced` SSE flips a chip in place without a
 *  REST roundtrip — the chip status resolves off the overlay
 *  first (see chipFor() in HypothesizedProposalCard.tsx).
 *  3. Approve → POST /signoff → `proposal_promoted` SSE collapses
 *  the card to a one-line "Promoted as <task_node_id>" status.
 *  4. Reject → POST /reject (with rationale body) → `proposal_rejected`
 *  SSE collapses the card to "Rejected — <rationale>".
 *  5. Blocked lifecycle renders the inline alert with the typed
 *  blocker-reason copy (validator_failed in this case).
 */

function makeProposal(
  overrides: Partial<MockHypothesizedProposal> = {},
): MockHypothesizedProposal {
  return {
    id: 'proposal-abc123def456',
    node_id: 'doublet_score',
    intent: 'Score per-cell doublet probability',
    parent_terms: ['edam:operation_3198', 'swfc:doublet_detection'],
    assumptions: ['Counts matrix is raw, not normalized.'],
    failure_modes: ['False positives on neighbouring multiplets.'],
    validation_tests: ['cell_count_matches', 'no_negative_counts'],
    llm_rationale:
      'Doublet scores filter heterotypic artifacts before clustering.',
    lifecycle: { kind: 'awaiting_signoff' } satisfies ProposalLifecycle,
    gate_outcomes: [
      {
        gate: 'validator',
        passed: true,
        details: ['cell_count_matches: ok', 'no_negative_counts: ok'],
        recorded_at: 1700000010,
      },
      {
        gate: 'sandbox',
        passed: true,
        details: ['static-analysis: ok', 'sandbox-run: ok'],
        recorded_at: 1700000020,
      },
    ],
    created_at: 1700000000,
    last_transition_at: 1700000020,
    ...overrides,
  }
}

const PROPOSAL_ID = 'proposal-abc123def456'
const NODE_ID = 'doublet_score'

test.describe('HypothesizedProposalCard — mocked tier', () => {
  test('card mounts on proposal_received SSE with Approve enabled', async ({
    page,
  }) => {
    await withMockBackend(
      page,
      { beats: [], proposals: [] },
      async (handle) => {
        await page.goto('/')
        const chat = new Chat(page)
        await chat.waitForAssistant()

        // No proposals yet — no card mounted.
        await expect(
          page.locator(`[data-proposal-id="${PROPOSAL_ID}"]`),
        ).toHaveCount(0)

        // Server materializes the proposal record server-side, then
        // broadcasts `proposal_received` on the SSE channel. Mirror
        // that order: seed the REST response BEFORE the SSE fires so
        // the refetch triggered by the proposalEvents key churn picks
        // up the new entry.
        handle.setProposals([makeProposal()])
        await handle.pushSseEvent({
          type: 'proposal_received',
          proposal_id: PROPOSAL_ID,
          node_id: NODE_ID,
        })

        const card = page.locator(`[data-proposal-id="${PROPOSAL_ID}"]`)
        await expect(card).toBeVisible()
        await expect(card).toContainText(NODE_ID)

        // Card is in the active-state section (awaiting_signoff), not
        // the collapsed terminal pill.
        await expect(card).toHaveAttribute('data-proposal-state', 'awaiting_signoff')

        // Approve button enabled because lifecycle is awaiting_signoff.
        const approve = page.getByTestId('proposal-approve')
        await expect(approve).toBeEnabled()
        await expect(approve).toHaveAccessibleName(
          `Approve and promote node ${NODE_ID}`,
        )
      },
    )
  })

  test('proposal_gate_advanced SSE flips validator + sandbox chips to passed', async ({
    page,
  }) => {
    await withMockBackend(
      page,
      {
        beats: [],
        // Seed with the proposal sitting in pending_validation so the
        // validator chip starts as `running` and we can watch the SSE
        // overlay promote each gate in turn.
        proposals: [
          makeProposal({
            lifecycle: { kind: 'pending_validation' },
            gate_outcomes: [],
          }),
        ],
      },
      async (handle) => {
        await page.goto('/')
        const chat = new Chat(page)
        await chat.waitForAssistant()

        // Push proposal_received to bump the loadProposals refetch and
        // settle the overlay registry. (The card would mount off the
        // initial loadProposals on mount alone, but emitting the event
        // matches the server's real broadcast ordering.)
        await handle.pushSseEvent({
          type: 'proposal_received',
          proposal_id: PROPOSAL_ID,
          node_id: NODE_ID,
        })

        const card = page.locator(`[data-proposal-id="${PROPOSAL_ID}"]`)
        await expect(card).toBeVisible()

        const validatorChip = page.getByTestId('proposal-chip-validator')
        const sandboxChip = page.getByTestId('proposal-chip-sandbox')

        // Initial state: validator running, sandbox pending.
        await expect(validatorChip).toHaveAttribute('data-chip-status', 'running')
        await expect(sandboxChip).toHaveAttribute('data-chip-status', 'pending')

        // Validator passes → SSE overlay flips the chip status to
        // passed without waiting for a REST refetch.
        await handle.pushSseEvent({
          type: 'proposal_gate_advanced',
          proposal_id: PROPOSAL_ID,
          gate: 'validator',
          passed: true,
        })
        await expect(validatorChip).toHaveAttribute('data-chip-status', 'passed')

        // Sandbox passes → same overlay-driven flip on the sandbox chip.
        await handle.pushSseEvent({
          type: 'proposal_gate_advanced',
          proposal_id: PROPOSAL_ID,
          gate: 'sandbox',
          passed: true,
        })
        await expect(sandboxChip).toHaveAttribute('data-chip-status', 'passed')
      },
    )
  })

  test('Approve click POSTs /signoff and proposal_promoted SSE collapses the card', async ({
    page,
  }) => {
    await withMockBackend(
      page,
      {
        beats: [],
        proposals: [makeProposal({ lifecycle: { kind: 'awaiting_signoff' } })],
      },
      async (handle) => {
        await page.goto('/')
        const chat = new Chat(page)
        await chat.waitForAssistant()

        // Mount the card (proposal seeded at install time; the SSE
        // matches the server's real broadcast).
        await handle.pushSseEvent({
          type: 'proposal_received',
          proposal_id: PROPOSAL_ID,
          node_id: NODE_ID,
        })

        const card = page.locator(`[data-proposal-id="${PROPOSAL_ID}"]`)
        await expect(card).toBeVisible()

        // Set up the request capture BEFORE the click so the await on
        // the request promise can never race the click. Pair it with
        // waitForResponse so the assertion only fires AFTER the mock
        // route handler has pushed onto recordedProposalActions
        // (waitForRequest resolves on request initiation, which races
        // the route handler under parallel-worker load).
        const signoffUrlMatches = (url: string) =>
          new URL(url).pathname.match(
            new RegExp(
              `/api/(?:v1/)?chat/session/${handle.sessionId}/proposal/${PROPOSAL_ID}/signoff$`,
            ),
          ) !== null
        const signoffRequestP = page.waitForRequest(
          (req) => req.method() === 'POST' && signoffUrlMatches(req.url()),
        )
        const signoffResponseP = page.waitForResponse(
          (res) => signoffUrlMatches(res.url()),
        )

        await page.getByTestId('proposal-approve').click()

        const signoffReq = await signoffRequestP
        // Body shape: { sme_initials: null } (defaulted by the card
        // when the SME skips initials). Verify the shape rather than a
        // string match so a future tweak to the default value still
        // passes.
        const body = signoffReq.postDataJSON() as { sme_initials: string | null }
        expect(body).toHaveProperty('sme_initials')
        await signoffResponseP

        // Mirror the assertion against the mock's recorded actions —
        // belt-and-suspenders so a regression that drops the
        // waitForRequest event (e.g. a route re-registration) still
        // fails the assertion downstream.
        await expect
          .poll(() => handle.recordedProposalActions())
          .toContainEqual({
            verb: 'signoff',
            proposalId: PROPOSAL_ID,
            body: expect.objectContaining({ sme_initials: null }),
          })

        // Now broadcast the promoted event the server would emit after
        // materialization succeeds. The card collapses to the
        // role=status "Promoted as <task_node_id>" pill.
        await handle.pushSseEvent({
          type: 'proposal_promoted',
          proposal_id: PROPOSAL_ID,
          task_node_id: 'doublet_score',
        })

        const collapsed = page.locator(
          `[data-proposal-id="${PROPOSAL_ID}"][data-proposal-state="promoted"]`,
        )
        await expect(collapsed).toBeVisible()
        await expect(collapsed).toContainText('Promoted')
        await expect(collapsed).toContainText('doublet_score')
      },
    )
  })

  test('Reject click POSTs /reject with rationale and collapses to rejected pill', async ({
    page,
  }) => {
    await withMockBackend(
      page,
      {
        beats: [],
        proposals: [makeProposal({ lifecycle: { kind: 'awaiting_signoff' } })],
      },
      async (handle) => {
        await page.goto('/')
        const chat = new Chat(page)
        await chat.waitForAssistant()

        await handle.pushSseEvent({
          type: 'proposal_received',
          proposal_id: PROPOSAL_ID,
          node_id: NODE_ID,
        })

        const card = page.locator(`[data-proposal-id="${PROPOSAL_ID}"]`)
        await expect(card).toBeVisible()

        // First click on Reject just reveals the rationale textarea —
        // it doesn't post anything yet.
        await page.getByTestId('proposal-reject').click()
        const textarea = page.getByTestId('reject-rationale')
        await expect(textarea).toBeVisible()

        const rationale =
          'Doublet detection is out of scope for this round of QC.'
        await textarea.fill(rationale)

        // Now capture the POST that the Confirm reject click fires.
        // Pair waitForRequest with waitForResponse so the mock-side
        // assertion only runs after the route handler has pushed onto
        // recordedProposalActions (waitForRequest alone races the
        // handler under parallel-worker load — same pattern as the
        // signoff path above).
        const rejectUrlMatches = (url: string) =>
          new URL(url).pathname.match(
            new RegExp(
              `/api/(?:v1/)?chat/session/${handle.sessionId}/proposal/${PROPOSAL_ID}/reject$`,
            ),
          ) !== null
        const rejectRequestP = page.waitForRequest(
          (req) => req.method() === 'POST' && rejectUrlMatches(req.url()),
        )
        const rejectResponseP = page.waitForResponse(
          (res) => rejectUrlMatches(res.url()),
        )

        await page.getByTestId('proposal-reject-confirm').click()

        const rejectReq = await rejectRequestP
        const body = rejectReq.postDataJSON() as { rationale: string | null }
        expect(body.rationale).toBe(rationale)
        await rejectResponseP

        // Mock-side cross-check.
        await expect
          .poll(() => handle.recordedProposalActions())
          .toContainEqual({
            verb: 'reject',
            proposalId: PROPOSAL_ID,
            body: { rationale },
          })

        // Server side would now broadcast proposal_rejected — the card
        // collapses to the role=status "Rejected — <rationale>" line.
        await handle.pushSseEvent({
          type: 'proposal_rejected',
          proposal_id: PROPOSAL_ID,
          rationale,
        })

        const collapsed = page.locator(
          `[data-proposal-id="${PROPOSAL_ID}"][data-proposal-state="rejected"]`,
        )
        await expect(collapsed).toBeVisible()
        await expect(collapsed).toContainText('Rejected')
        await expect(collapsed).toContainText(rationale)
      },
    )
  })

  test('Blocked lifecycle renders the typed blocker-reason alert', async ({
    page,
  }) => {
    const failures = ['cell_count_matches', 'no_negative_counts']
    await withMockBackend(
      page,
      {
        beats: [],
        proposals: [
          makeProposal({
            lifecycle: {
              kind: 'blocked',
              reason: { kind: 'validator_failed', failures },
            },
            // Mirror the gate timeline a real validator_failed would
            // produce: failed validator outcome, no sandbox row.
            gate_outcomes: [
              {
                gate: 'validator',
                passed: false,
                details: failures.map((f) => `${f}: failed`),
                recorded_at: 1700000010,
              },
            ],
          }),
        ],
      },
      async (handle) => {
        await page.goto('/')
        const chat = new Chat(page)
        await chat.waitForAssistant()

        await handle.pushSseEvent({
          type: 'proposal_received',
          proposal_id: PROPOSAL_ID,
          node_id: NODE_ID,
        })

        const card = page.locator(`[data-proposal-id="${PROPOSAL_ID}"]`)
        await expect(card).toBeVisible()
        await expect(card).toHaveAttribute('data-proposal-state', 'blocked')

        const alert = page.getByTestId('proposal-blocked-alert')
        await expect(alert).toBeVisible()
        await expect(alert).toHaveAttribute(
          'data-blocker-reason',
          'validator_failed',
        )
        // The blocker-reason copy renders the failure list inline.
        for (const f of failures) {
          await expect(alert).toContainText(f)
        }
        // Validator chip stays failed — sandbox / sme_signoff stay
        // pending — covers chipFor()'s blocked-lifecycle branch.
        await expect(page.getByTestId('proposal-chip-validator')).toHaveAttribute(
          'data-chip-status',
          'failed',
        )
      },
    )
  })
})
