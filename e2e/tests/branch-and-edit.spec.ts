import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import type { Beat } from '../helpers/types'

/**
 * Branch-and-edit — mocked-tier coverage (Phase 4 UI).
 *
 * Exercises the TaskDetailDrawer "Branch & edit" flow end-to-end:
 *  1. Emit a package whose DAG has a completed `align` task backed by an
 *     atom that declares an `aligner` enum + a `min_mapq` integer.
 *  2. Open the drawer for `align` via the `#task=` deep-link the PlanTab
 *     honours (the DagCanvas is canvas-rendered, so no DOM node to click).
 *  3. Open the branch-to-edit modal, stage a parameter change through the
 *     structured TaskParameterEditor (a RadioRow enum + a number input).
 *  4. Submit and assert the POST /branch body carries the task-scoped
 *     `edits` payload with the staged parameter overrides.
 *
 * The full Docker e2e runs separately; this spec is the mocked contract
 * check for the branch-edits wire shape and the SPA (no-reload) submit.
 */

const dagWithTask = {
  version: '1',
  workflow_id: 'wf-branch-edit',
  tasks: {
    align: {
      kind: 'computation',
      state: { status: 'completed', result: { ok: true } },
      depends_on: [],
      assignee: 'agent',
      description: 'Read alignment',
      spec: { stage_class: 'alignment' },
      source_atom_id: 'alignment',
    },
    quantify: {
      kind: 'computation',
      state: { status: 'completed', result: { ok: true } },
      depends_on: ['align'],
      assignee: 'agent',
      description: 'Quantify expression',
      spec: { stage_class: 'quantification' },
      source_atom_id: 'quantification',
    },
  },
}

const beats: Beat[] = [
  {
    user: 'Run a bulk RNA-seq DE analysis.',
    assistant: { content: 'Plan emitted with an alignment task.' },
    state: 'emitted',
    dag: dagWithTask,
    // The SSE state_advanced makes the client refetch the DAG (a plain
    // /turn does not), so the PlanTab has tasks to deep-link into.
    sse: [{ type: 'state_advanced', new_state: { kind: 'emitted' } }],
  },
]

test.describe('Branch & edit (Phase 4)', () => {
  test('branch modal sends the staged parameter overrides in the edits payload', async ({
    page,
  }) => {
    const capturedBranchBodies: unknown[] = []

    await withMockBackend(page, { beats }, async () => {
      // GET the editable parameter schema for the aligner atom.
      await page.route(
        /\/api\/(?:v1\/)?chat\/session\/[^/]+\/task\/[^/]+\/parameters$/,
        async (route) => {
          if (route.request().method() !== 'GET') return route.fallback()
          await route.fulfill({
            status: 200,
            contentType: 'application/json',
            body: JSON.stringify({
              task_id: 'align',
              source_atom_id: 'alignment',
              parameters: [
                {
                  name: 'aligner',
                  type: 'enum',
                  required: false,
                  allowed_values: ['star', 'hisat2'],
                  examples: [],
                },
                {
                  name: 'min_mapq',
                  type: 'integer',
                  required: false,
                  default: 10,
                  allowed_values: [],
                  examples: [],
                },
              ],
              current_overrides: {},
              current_method: null,
            }),
          })
        },
      )

      // Blast-radius preview shown inside the branch modal.
      await page.route(
        /\/api\/(?:v1\/)?chat\/session\/[^/]+\/task\/[^/]+\/impact-preview$/,
        async (route) => {
          if (route.request().method() !== 'POST') return route.fallback()
          await route.fulfill({
            status: 200,
            contentType: 'application/json',
            body: JSON.stringify({
              target_task_id: 'align',
              invalidated_tasks: [
                {
                  task_id: 'quantify',
                  description: 'Quantify expression',
                  est_cost_usd_min: 0.1,
                  est_cost_usd_max: 0.5,
                  cost_source: 'coarse_default',
                },
              ],
              invalidated_count: 1,
              est_cost_usd_min: 0.1,
              est_cost_usd_max: 0.5,
            }),
          })
        },
      )

      // Capture the branch POST and return a child session id.
      await page.route(
        /\/api\/(?:v1\/)?chat\/session\/[^/]+\/branch$/,
        async (route) => {
          if (route.request().method() !== 'POST') return route.fallback()
          try {
            capturedBranchBodies.push(
              JSON.parse(route.request().postData() ?? '{}'),
            )
          } catch {
            capturedBranchBodies.push(null)
          }
          await route.fulfill({
            status: 200,
            contentType: 'application/json',
            body: JSON.stringify({ session_id: 'child-branch-edit' }),
          })
        },
      )

      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()
      await chat.sendUserMessage(beats[0].user)
      await chat.waitForAssistant({ textContains: 'emitted' })

      // Open the TaskDetailDrawer for `align` via the deep-link hash.
      await page.evaluate(() => {
        window.location.hash = '#task=align'
      })
      const drawer = page.getByTestId('task-detail-drawer')
      await drawer.waitFor({ state: 'visible', timeout: 10_000 })

      // Open the branch-to-edit modal.
      await page.getByRole('button', { name: /branch & edit/i }).click()

      // The structured editor loads from the mocked GET /parameters.
      const hisat2 = page.getByRole('radio', { name: 'hisat2' })
      await hisat2.waitFor({ state: 'visible', timeout: 10_000 })
      await hisat2.check()
      await page.locator('#param-min_mapq').fill('30')

      // Rationale is prefilled by openBranchModal, so the button enables.
      await page.getByRole('button', { name: /create branch/i }).click()

      await expect
        .poll(() => capturedBranchBodies.length, { timeout: 5_000 })
        .toBeGreaterThan(0)

      const body = capturedBranchBodies[0] as {
        task_id?: string
        edits?: {
          method?: string | null
          parameters?: Record<string, unknown>
          validation_bounds?: unknown[]
        }
      }
      expect(body.task_id).toBe('align')
      expect(body.edits).toBeTruthy()
      expect(body.edits?.parameters?.aligner).toBe('hisat2')
      expect(body.edits?.parameters?.min_mapq).toBe(30)
      expect(body.edits?.validation_bounds).toEqual([])

      // SPA nav: the URL swaps to the child session without a full reload.
      await expect
        .poll(
          () => new URL(page.url()).searchParams.get('session'),
          { timeout: 5_000 },
        )
        .toBe('child-branch-edit')
    })
  })
})
